//! Moving one agent session's own history into a snapshot and back.
//!
//! Archives block (plain `std::fs`, child processes): call them from `spawn_blocking`.
//! Only what a backend lists is copied. Every component below a trusted root is checked
//! with `symlink_metadata`: a link anywhere on the way is refused, never followed.
//! Destination directories are made one component at a time, files with `create_new` (agent
//! files under a temporary name, then hard-linked to the final one), so nothing in the agent's
//! storage is overwritten; snapshot files are private.
//!
//! Residual risk: a process of the same user that writes into these `0700` directories can
//! swap a checked component for a link between the check and the `open`/`remove_file` that
//! follows it (TOCTOU). `O_NOFOLLOW` needs `libc`/`unsafe`, which the workspace forbids, so the
//! final destination file is guarded by `create_new`/`hard_link` (neither follows a link in the
//! last component) and a source by the check right before its `open`.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};

use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, FILES_LIMIT, PathError, RelPath};

const ID_LIMIT: usize = 128;
const TREE_DEPTH: usize = 16;
/// Bounds a walk, not a snapshot: Codex scans every thread's rollouts under `sessions/`, far
/// more than one snapshot may hold; `export_files` applies the snapshot's own `FILES_LIMIT`.
const TREE_ENTRIES: usize = 100_000;

/// A file in the snapshot and where it goes in the agent's storage.
pub type Pair = (RelPath, RelPath);

/// What `export` put into the snapshot's `agent/` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    pub files: Vec<RelPath>,
    pub data: BackendData,
    pub agent_version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("История сессии не найдена у агента")]
    NotFound,
    #[error("У агента несколько копий этой сессии — оставьте одну")]
    Ambiguous,
    #[error("Сессия в архиве Codex: выполните `codex unarchive {0}`")]
    Archived(String),
    #[error("В Qwen уже есть сессия {id} для каталога {cwd}")]
    Occupied { id: String, cwd: String },
    #[error("«{0}»: путь идёт через символьную ссылку — снимки по ссылкам не ходят")]
    Link(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("Файлы агента недоступны: {0}")]
    Io(#[from] io::Error),
    #[error("Данные снимка повреждены: {0}")]
    Malformed(String),
}

/// A path that cannot be a `RelPath` came from the snapshot or the agent's storage.
impl From<PathError> for ArchiveError {
    fn from(error: PathError) -> Self {
        Self::Malformed(error.to_string())
    }
}

/// One backend's way to copy a session's history out of its storage and back.
pub trait SessionArchive: Send + Sync {
    /// Copies the session's files into `dest` (the snapshot's empty `agent/`).
    fn export(
        &self,
        session: &SessionId,
        cwd: &AbsolutePath,
        dest: &Path,
    ) -> Result<Exported, ArchiveError>;

    /// Whether the agent can already resume `session` in `cwd`; then nothing is imported.
    fn present(
        &self,
        session: &SessionId,
        cwd: &AbsolutePath,
        data: &BackendData,
    ) -> Result<bool, ArchiveError>;

    /// Puts `files` from `src` (the snapshot's `agent/`) back for `cwd`; all or nothing.
    fn import(
        &self,
        session: &SessionId,
        data: &BackendData,
        files: &[RelPath],
        src: &Path,
        cwd: &AbsolutePath,
    ) -> Result<(), ArchiveError>;
}

/// What a path below a trusted root is, found without following any link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entry {
    Missing,
    File,
    Dir,
    /// The last component itself is a link; callers skip or refuse it.
    Link,
    /// Neither file, directory nor link (a socket, a device, a Windows junction).
    Other,
}

/// The id as a file-name stem; agents mint UUID-like ids, anything else could leave a directory.
pub fn file_stem(session: &SessionId) -> Result<&str, ArchiveError> {
    let id = session.as_str();
    let safe = id.len() <= ID_LIMIT
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if safe {
        Ok(id)
    } else {
        Err(ArchiveError::Malformed("id сессии нельзя использовать как имя файла".to_owned()))
    }
}

/// A builder for owner-only (`0700`) directories on Unix.
#[cfg(unix)]
fn dir_builder() -> DirBuilder {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder
}

/// Windows ACLs already follow the user profile; nothing to narrow.
#[cfg(not(unix))]
fn dir_builder() -> DirBuilder {
    DirBuilder::new()
}

/// Creates `path` and its missing parents; new directories are `0700` on Unix. Only for
/// directories the hub owns (the snapshot root, staging, the Hermes work directory): it follows
/// links like any `create_dir_all`.
pub fn private_dirs(path: &Path) -> io::Result<()> {
    dir_builder().recursive(true).create(path)
}

/// A new file — never an existing one or a link — readable only by the owner on Unix.
pub fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// `0700` on an existing directory the hub owns.
#[cfg(unix)]
pub fn restrict_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Windows ACLs already follow the user profile; nothing to narrow.
#[cfg(not(unix))]
pub fn restrict_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn classify(path: &Path) -> io::Result<Entry> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            let kind = metadata.file_type();
            Ok(if kind.is_symlink() {
                Entry::Link
            } else if kind.is_file() {
                Entry::File
            } else if kind.is_dir() {
                Entry::Dir
            } else {
                Entry::Other
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Entry::Missing),
        Err(error) => Err(error),
    }
}

/// The refusal for a parent component that is not a real directory: a link is `Link`, anything
/// else that exists (a file, a socket) is `Malformed`.
fn not_a_dir(rel: &RelPath, part: &Path, entry: Entry) -> ArchiveError {
    match entry {
        Entry::Link => ArchiveError::Link(rel.to_string()),
        Entry::Missing | Entry::File | Entry::Dir | Entry::Other => {
            ArchiveError::Malformed(format!("{rel}: «{}» не каталог", part.display()))
        }
    }
}

/// Walks `rel` from `root` component by component. A parent that is a link is refused
/// (`Link`), one that is not a directory too (`Malformed`); a missing parent means the whole
/// path is `Missing`.
pub fn entry_under(root: &Path, rel: &RelPath) -> Result<Entry, ArchiveError> {
    let parts: Vec<&str> = rel.parts().collect();
    let Some((last, parents)) = parts.split_last() else {
        return Ok(Entry::Missing);
    };
    let mut path = root.to_path_buf();
    for part in parents {
        path.push(part);
        match classify(&path)? {
            Entry::Missing => return Ok(Entry::Missing),
            Entry::Dir => {}
            entry @ (Entry::File | Entry::Link | Entry::Other) => {
                return Err(not_a_dir(rel, Path::new(part), entry));
            }
        }
    }
    path.push(last);
    Ok(classify(&path)?)
}

/// Opens `root/rel` for reading, right after checking every component.
pub fn open_under(root: &Path, rel: &RelPath) -> Result<File, ArchiveError> {
    match entry_under(root, rel)? {
        Entry::File => Ok(File::open(rel.under(root))?),
        Entry::Link => Err(ArchiveError::Link(rel.to_string())),
        Entry::Missing | Entry::Dir | Entry::Other => {
            Err(ArchiveError::Malformed(format!("{rel}: не обычный файл")))
        }
    }
}

/// Creates the parents of `root/rel` one component at a time (an existing component must be a
/// real directory), then the file with `create_new`, so neither a planted link nor an existing
/// file is followed or overwritten.
pub fn create_under(root: &Path, rel: &RelPath) -> Result<File, ArchiveError> {
    let parts: Vec<&str> = rel.parts().collect();
    let Some((last, parents)) = parts.split_last() else {
        return Err(ArchiveError::Malformed("пустой путь".to_owned()));
    };
    let mut path = root.to_path_buf();
    for part in parents {
        path.push(part);
        // `mkdir` never follows a link in the last component: a planted link is `AlreadyExists`
        // and then refused by the check below.
        match dir_builder().create(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        match classify(&path)? {
            Entry::Dir => {}
            Entry::Missing => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("{rel}: каталог исчез во время создания"),
                )
                .into());
            }
            entry @ (Entry::File | Entry::Link | Entry::Other) => {
                return Err(not_a_dir(rel, Path::new(part), entry));
            }
        }
    }
    path.push(last);
    Ok(private_file(&path)?)
}

/// Every regular file under `root/dir` as paths relative to `root`, sorted. Links, special
/// files and names that are not valid `RelPath`s inside the tree are skipped; a missing or
/// linked `dir` is empty; a link *above* `dir` is refused.
pub fn collect_tree(root: &Path, dir: &RelPath) -> Result<Vec<RelPath>, ArchiveError> {
    let mut found = Vec::new();
    match entry_under(root, dir)? {
        Entry::Dir => walk(root, dir, 0, &mut found)?,
        Entry::Missing | Entry::File | Entry::Link | Entry::Other => {}
    }
    found.sort();
    Ok(found)
}

fn walk(
    root: &Path,
    dir: &RelPath,
    depth: usize,
    found: &mut Vec<RelPath>,
) -> Result<(), ArchiveError> {
    if depth > TREE_DEPTH {
        return Err(ArchiveError::Malformed(format!("{dir}: вложенность больше {TREE_DEPTH}")));
    }
    for entry in fs::read_dir(dir.under(root))? {
        let entry = entry?;
        let child = entry
            .file_name()
            .to_str()
            .and_then(|name| RelPath::parse(&format!("{dir}/{name}")).ok());
        let Some(child) = child else {
            tracing::warn!("agent file with an unusable name skipped");
            continue;
        };
        // `DirEntry::file_type` does not follow links.
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(root, &child, depth + 1, found)?;
        } else if kind.is_file() {
            found.push(child);
            if found.len() > TREE_ENTRIES {
                return Err(ArchiveError::Malformed(format!(
                    "{dir}: больше {TREE_ENTRIES} файлов"
                )));
            }
        }
    }
    Ok(())
}

/// Copies `files` from `root` into `dest` under the same relative paths, private and checked
/// component by component on both sides. `dest` is a staging directory the caller removes as a
/// whole on failure, so a partial copy is not cleaned up here. More files than a manifest holds
/// (`FILES_LIMIT`) are refused before anything is copied.
pub fn export_files(root: &Path, files: &[RelPath], dest: &Path) -> Result<(), ArchiveError> {
    if files.len() > FILES_LIMIT {
        return Err(ArchiveError::Malformed(format!(
            "у сессии больше {FILES_LIMIT} файлов — снимок их не вместит"
        )));
    }
    files.iter().try_for_each(|file| {
        let mut from = open_under(root, file)?;
        let mut out = create_under(dest, file)?;
        io::copy(&mut from, &mut out)?;
        out.sync_all()?;
        Ok(())
    })
}

/// Writes every `(from, to)` pair — `from` under `src`, `to` under `dest` — through `write`,
/// which gets the opened, checked source. Pairs are placed in order, except that the pair whose
/// `to` is `presence` — the file by which the agent finds the session — goes last, so an import
/// cut short leaves a session the agent does not see and a repeated `/restore` imports anew.
///
/// Each file is written under a temporary name in its target directory (`create_new`, `0600`,
/// synced) and then hard-linked to its final name, which fails if that name exists: a crash
/// leaves at most a temporary file, never a partial file under the final name. A file system
/// without hard links (`Unsupported`) falls back to writing the final name with `create_new`.
/// On the first failure every file created so far is removed, so a failed import leaves the
/// agent's storage as it was. Missing parent directories are created and may stay behind empty;
/// a temporary file left by a crash stays behind too: it is not ours to tell apart from the
/// agent's own files later, and the agent ignores it.
///
/// A retry after such a crash finds the files placed before it: a regular file that already
/// holds exactly the bytes to be written is kept (and is not removed on rollback, this run did
/// not create it); different bytes, a link, or an existing `presence` file are refused.
pub fn place_all(
    src: &Path,
    dest: &Path,
    pairs: &[Pair],
    presence: &RelPath,
    mut write: impl FnMut(&RelPath, File, &mut File) -> io::Result<()>,
) -> Result<(), ArchiveError> {
    let (last, first): (Vec<&Pair>, Vec<&Pair>) = pairs.iter().partition(|(_, to)| to == presence);
    if last.is_empty() {
        return Err(ArchiveError::Malformed(format!("{presence}: нет среди файлов снимка")));
    }
    let ordered = first
        .into_iter()
        .map(|pair| (pair, Existing::KeepIfSame))
        .chain(last.into_iter().map(|pair| (pair, Existing::Refuse)));
    let mut created: Vec<PathBuf> = Vec::new();
    for ((from, to), existing) in ordered {
        let target = Target { dest, to, existing };
        if let Err(error) = place(src, from, &target, &mut write, &mut created) {
            remove_placed(created.iter().rev(), "restored files left behind");
            return Err(error);
        }
    }
    Ok(())
}

/// `to` with its last component replaced by a fresh temporary name: the same directory, so the
/// hard link stays on one file system, and a name no agent reads as a session file.
fn temporary_beside(to: &RelPath) -> Result<RelPath, ArchiveError> {
    let name = format!(".agent-hub-{}.tmp", uuid::Uuid::new_v4().simple());
    let parts: Vec<&str> = to.parts().collect();
    let parents = parts.split_last().map_or(&[][..], |(_, parents)| parents);
    let all: Vec<&str> = parents.iter().copied().chain([name.as_str()]).collect();
    Ok(RelPath::from_parts(&all)?)
}

/// What placing does when the final name is already taken.
#[derive(Debug, Clone, Copy)]
enum Existing {
    /// The file the agent finds the session by: if it is there, the session already is.
    Refuse,
    /// Left by an import cut short: kept when it is a regular file with the same bytes.
    KeepIfSame,
}

/// Where one file goes.
struct Target<'a> {
    dest: &'a Path,
    to: &'a RelPath,
    existing: Existing,
}

/// Places one file; whatever it leaves in `dest` is pushed to `created`, a failure removes what
/// it wrote itself.
fn place(
    src: &Path,
    from: &RelPath,
    target: &Target<'_>,
    write: &mut impl FnMut(&RelPath, File, &mut File) -> io::Result<()>,
    created: &mut Vec<PathBuf>,
) -> Result<(), ArchiveError> {
    let Target { dest, to, existing: _ } = *target;
    let source = open_under(src, from)?;
    let temporary = temporary_beside(to)?;
    let mut out = create_under(dest, &temporary)?;
    let temporary = temporary.under(dest);
    let written = write(from, source, &mut out).and_then(|()| out.sync_all());
    drop(out);
    let linked = written.map_err(ArchiveError::from).and_then(|()| {
        match fs::hard_link(&temporary, to.under(dest)) {
            Ok(()) => Ok(Placed::New),
            Err(error) if error.kind() == io::ErrorKind::Unsupported => {
                copy_new(&temporary, dest, to)
                    .map(|()| Placed::New)
                    .or_else(|error| taken(error, target, &temporary))
            }
            Err(error) => taken(error.into(), target, &temporary),
        }
    });
    match linked {
        Ok(Placed::New) => created.push(to.under(dest)),
        Ok(Placed::Kept) => {}
        Err(error) => {
            remove_placed([temporary].iter(), "half-written file left behind");
            return Err(error);
        }
    }
    if let Err(error) = fs::remove_file(&temporary) {
        // The final file is complete; the leftover is a second name for it, removed on rollback.
        tracing::warn!(error = ?error.kind(), "temporary restore file left behind");
        created.push(temporary);
    }
    Ok(())
}

/// Whether this run created the final file or found it already in place.
#[derive(Debug, Clone, Copy)]
enum Placed {
    New,
    Kept,
}

/// The final name is taken (`error`): kept only if `target` allows it and it is a regular file
/// holding exactly the bytes of `temporary`; anything else is the original error.
fn taken(
    error: ArchiveError,
    target: &Target<'_>,
    temporary: &Path,
) -> Result<Placed, ArchiveError> {
    let Target { dest, to, existing } = *target;
    let exists =
        matches!(&error, ArchiveError::Io(io) if io.kind() == io::ErrorKind::AlreadyExists);
    match existing {
        Existing::KeepIfSame if exists => {}
        Existing::KeepIfSame | Existing::Refuse => return Err(error),
    }
    if entry_under(dest, to)? == Entry::File && same_bytes(temporary, &to.under(dest))? {
        Ok(Placed::Kept)
    } else {
        Err(error)
    }
}

fn same_bytes(left: &Path, right: &Path) -> io::Result<bool> {
    let (left, right) = (File::open(left)?, File::open(right)?);
    if left.metadata()?.len() != right.metadata()?.len() {
        return Ok(false);
    }
    for (left, right) in BufReader::new(left).bytes().zip(BufReader::new(right).bytes()) {
        if left? != right? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The fallback without hard links: the written temporary file copied to `to` with
/// `create_new`; a partial copy is removed.
fn copy_new(temporary: &Path, dest: &Path, to: &RelPath) -> Result<(), ArchiveError> {
    let mut from = File::open(temporary)?;
    let mut out = create_under(dest, to)?;
    let copied = io::copy(&mut from, &mut out).and_then(|_| out.sync_all());
    drop(out);
    copied.map_err(|error| {
        remove_placed([to.under(dest)].iter(), "half-written file left behind");
        error.into()
    })
}

/// Rollback is best effort: the original error is what the caller reports. Files that could not
/// be removed are logged by count and io error kind only — agent-storage paths encode the user's
/// working directory and stay out of the logs.
fn remove_placed<'a>(paths: impl Iterator<Item = &'a PathBuf>, what: &'static str) {
    let kinds: Vec<io::ErrorKind> =
        paths.filter_map(|path| fs::remove_file(path).err().map(|error| error.kind())).collect();
    if !kinds.is_empty() {
        tracing::warn!(left = kinds.len(), errors = ?kinds, "{what}");
    }
}

/// The plain writer for [`place_all`]: the bytes as they are.
pub fn copy_plain(_rel: &RelPath, mut from: File, out: &mut File) -> io::Result<()> {
    io::copy(&mut from, out).map(drop)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::uuid("0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0", true)]
    #[case::hermes("20261009_153012_ab12cd", true)]
    #[case::traversal("../../etc/passwd", false)]
    #[case::separator("a/b", false)]
    #[case::dot("a.b", false)]
    #[case::too_long(&"a".repeat(129), false)]
    #[case::longest(&"a".repeat(128), true)]
    fn session_ids_must_be_safe_file_stems(#[case] raw: &str, #[case] safe: bool) {
        let id = SessionId::parse(raw).unwrap();
        assert_eq!(file_stem(&id).is_ok(), safe);
    }
}
