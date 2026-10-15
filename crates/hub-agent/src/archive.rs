//! Moving one agent session's own history into a snapshot and back.
//!
//! Archives block (plain `std::fs`, child processes): call them from `spawn_blocking`.
//! Only what a backend lists is copied. Every component below a trusted root is checked
//! with `symlink_metadata`: a link anywhere on the way is refused, never followed.
//! Destination directories are made one component at a time, files with `create_new`, so
//! nothing in the agent's storage is overwritten; snapshot files are private.
//!
//! Residual risk: a process of the same user that writes into these `0700` directories can
//! swap a checked component for a link between the check and the `open`/`remove_file` that
//! follows it (TOCTOU). `O_NOFOLLOW` needs `libc`/`unsafe`, which the workspace forbids, so the
//! final destination file is guarded by `create_new` and a source by the check right before
//! its `open`.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, PathError, RelPath};

const ID_LIMIT: usize = 128;
const TREE_DEPTH: usize = 16;
const TREE_ENTRIES: usize = 100_000;

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
            tracing::warn!(%dir, "agent file with an unusable name skipped");
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
/// whole on failure, so a partial copy is not cleaned up here.
pub fn export_files(root: &Path, files: &[RelPath], dest: &Path) -> Result<(), ArchiveError> {
    files.iter().try_for_each(|file| {
        let mut from = open_under(root, file)?;
        let mut out = create_under(dest, file)?;
        io::copy(&mut from, &mut out)?;
        out.sync_all()?;
        Ok(())
    })
}

/// Writes every `(from, to)` pair — `from` under `src`, `to` under `dest` — through `write`,
/// which gets the opened, checked source. Each `to` is created anew; on the first failure every
/// file created so far is removed, so a failed import leaves the agent's storage as it was.
/// Missing parent directories are created and may stay behind empty.
pub fn place_all(
    src: &Path,
    dest: &Path,
    pairs: &[(RelPath, RelPath)],
    mut write: impl FnMut(&RelPath, File, &mut File) -> io::Result<()>,
) -> Result<(), ArchiveError> {
    let mut created: Vec<PathBuf> = Vec::new();
    for (from, to) in pairs {
        if let Err(error) = place(src, from, dest, to, &mut write) {
            for path in created.iter().rev() {
                remove_placed(path, "restored file left behind");
            }
            return Err(error);
        }
        created.push(to.under(dest));
    }
    Ok(())
}

fn place(
    src: &Path,
    from: &RelPath,
    dest: &Path,
    to: &RelPath,
    write: &mut impl FnMut(&RelPath, File, &mut File) -> io::Result<()>,
) -> Result<(), ArchiveError> {
    let source = open_under(src, from)?;
    let mut out = create_under(dest, to)?;
    let written = write(from, source, &mut out).and_then(|()| out.sync_all());
    drop(out);
    written.map_err(|error| {
        remove_placed(&to.under(dest), "half-written file left behind");
        error.into()
    })
}

/// Rollback is best effort: the original error is what the caller reports, a file that could
/// not be removed is logged by path (no content).
fn remove_placed(path: &Path, what: &'static str) {
    if let Err(cleanup) = fs::remove_file(path) {
        tracing::warn!(path = %path.display(), error = %cleanup, "{what}");
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
