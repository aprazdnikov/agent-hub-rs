//! Filesystem checks for working directories and for files crossing the chat boundary.
//! Symlinks are resolved before every containment check, so a link cannot lead outside.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use hub_core::attachments::{GITIGNORE, UPLOADS_DIR};
use hub_core::domain::AbsolutePath;
use hub_core::workspace::{candidate, expand_home};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CwdError {
    #[error("корень рабочих директорий {} недоступен", path.display())]
    Root {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} не существует", .0.display())]
    Missing(PathBuf),
    #[error("{} вне корня рабочих директорий {}", path.display(), root.display())]
    Outside { path: PathBuf, root: PathBuf },
    #[error("{} — не директория", .0.display())]
    NotDirectory(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("{raw}: файл вне рабочей директории {}", cwd.display())]
    Outside { raw: String, cwd: PathBuf },
    #[error("{0}: файл не найден")]
    Missing(String),
    #[error("{raw}: {} МБ, Telegram принимает от бота не больше {} МБ", size / MIB, limit / MIB)]
    TooLarge { raw: String, size: u64, limit: u64 },
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("{} ведёт за пределы {}", directory.display(), cwd.display())]
    Outside { directory: PathBuf, cwd: PathBuf },
    #[error("{} уже существует", .0.display())]
    Exists(PathBuf),
    #[error("Не удалось сохранить вложение: {0}")]
    Io(#[from] io::Error),
}

/// The configured root in canonical form, so containment checks compare like with like.
pub fn workspace_root(path: &Path) -> Result<AbsolutePath, CwdError> {
    let canonical = dunce::canonicalize(path)
        .map_err(|source| CwdError::Root { path: path.to_path_buf(), source })?;
    directory(canonical)
}

/// `raw` — absolute, `~`-prefixed or relative to `root`; blank means `root`.
pub fn resolve_cwd(
    root: &AbsolutePath,
    home: &Path,
    raw: Option<&str>,
) -> Result<AbsolutePath, CwdError> {
    let path = candidate(root.as_path(), home, raw);
    let resolved = dunce::canonicalize(&path).map_err(|_| CwdError::Missing(path))?;
    if !resolved.starts_with(root.as_path()) {
        return Err(CwdError::Outside { path: resolved, root: root.as_path().to_path_buf() });
    }
    directory(resolved)
}

/// Whether a stored working directory is still inside the (possibly changed) root.
#[must_use]
pub fn inside(root: &AbsolutePath, cwd: &AbsolutePath) -> bool {
    dunce::canonicalize(cwd.as_path()).is_ok_and(|real| real.starts_with(root.as_path()))
}

fn directory(path: PathBuf) -> Result<AbsolutePath, CwdError> {
    if !path.is_dir() {
        return Err(CwdError::NotDirectory(path));
    }
    let shown = path.clone();
    AbsolutePath::new(path).ok_or(CwdError::NotDirectory(shown))
}

/// A file the agent wants to send: only a regular file inside `cwd` qualifies.
pub fn outgoing_path(
    cwd: &AbsolutePath,
    home: &Path,
    raw: &str,
    limit: u64,
) -> Result<PathBuf, FileError> {
    let missing = || FileError::Missing(raw.to_owned());
    let root = dunce::canonicalize(cwd.as_path()).map_err(|_| missing())?;
    let path = dunce::canonicalize(root.join(expand_home(raw, home))).map_err(|_| missing())?;
    if !path.starts_with(&root) {
        return Err(FileError::Outside { raw: raw.to_owned(), cwd: root });
    }
    let metadata = fs::metadata(&path).map_err(|_| missing())?;
    if !metadata.is_file() {
        return Err(missing());
    }
    if metadata.len() > limit {
        return Err(FileError::TooLarge { raw: raw.to_owned(), size: metadata.len(), limit });
    }
    Ok(path)
}

/// Makes `target` writable without letting a symlink redirect the write out of `cwd`.
pub fn prepare_upload(cwd: &AbsolutePath, target: &Path) -> Result<(), UploadError> {
    let root = dunce::canonicalize(cwd.as_path())?;
    let directory = target.parent().unwrap_or(cwd.as_path());
    let outside = || UploadError::Outside { directory: directory.to_path_buf(), cwd: root.clone() };
    let relative = directory.strip_prefix(cwd.as_path()).map_err(|_| outside())?;
    // One level at a time: a planted link must not receive even an empty directory.
    relative.components().try_fold(root.clone(), |parent, part| match part {
        Component::Normal(name) => enter(parent.join(name)).and_then(|entered| match entered {
            Entry::Directory(path) => Ok(path),
            Entry::Other => Err(outside()),
        }),
        Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
            Err(outside())
        }
    })?;
    let real = dunce::canonicalize(directory)?;
    if !real.starts_with(&root) {
        return Err(outside());
    }
    // Names are unique per message, so an existing entry was planted, not uploaded.
    if fs::symlink_metadata(target).is_ok() {
        return Err(UploadError::Exists(target.to_path_buf()));
    }
    let [hub, _uploads] = UPLOADS_DIR;
    let ignore = cwd.as_path().join(hub).join(".gitignore");
    if fs::symlink_metadata(&ignore).is_err() {
        fs::write(&ignore, GITIGNORE)?;
    }
    Ok(())
}

enum Entry {
    Directory(PathBuf),
    Other,
}

/// Creates `path` unless it exists; anything but a real directory there is refused.
fn enter(path: PathBuf) -> Result<Entry, UploadError> {
    match fs::create_dir(&path) {
        Ok(()) => {}
        // `create_dir` does not follow a link in the last component; the check below sees it.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    // `symlink_metadata` reports a link (or a Windows junction) as not a directory.
    Ok(if fs::symlink_metadata(&path)?.is_dir() { Entry::Directory(path) } else { Entry::Other })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use hub_core::attachments::upload_path;
    use hub_core::domain::MessageId;
    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;

    fn home() -> PathBuf {
        std::env::temp_dir()
    }

    /// A root with `project/sub` and a regular file `file.txt`.
    fn workspace() -> (TempDir, AbsolutePath) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("project").join("sub")).unwrap();
        fs::write(dir.path().join("file.txt"), "x").unwrap();
        let root = workspace_root(dir.path()).unwrap();
        (dir, root)
    }

    #[test]
    fn missing_path_means_root() {
        let (_dir, root) = workspace();
        assert_eq!(resolve_cwd(&root, &home(), None).unwrap(), root);
        assert_eq!(resolve_cwd(&root, &home(), Some("  ")).unwrap(), root);
    }

    #[test]
    fn relative_path_is_under_root() {
        let (_dir, root) = workspace();
        let expected = root.as_path().join("project").join("sub");
        assert_eq!(resolve_cwd(&root, &home(), Some("project/sub")).unwrap().as_path(), expected);
    }

    #[test]
    fn absolute_path_inside_root_is_accepted() {
        let (_dir, root) = workspace();
        let inside = root.as_path().join("project");
        let resolved = resolve_cwd(&root, &home(), inside.to_str()).unwrap();
        assert_eq!(resolved.as_path(), inside);
    }

    #[rstest]
    #[case("..")]
    #[case("../..")]
    #[case("/")]
    #[case("project/../../")]
    fn escape_from_root_is_rejected(#[case] raw: &str) {
        let (_dir, root) = workspace();
        assert!(matches!(resolve_cwd(&root, &home(), Some(raw)), Err(CwdError::Outside { .. })));
    }

    #[rstest]
    #[case("missing")]
    #[case("file.txt")]
    fn non_directory_is_rejected(#[case] raw: &str) {
        let (_dir, root) = workspace();
        assert!(resolve_cwd(&root, &home(), Some(raw)).is_err());
    }

    #[test]
    fn missing_root_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(workspace_root(&dir.path().join("gone")), Err(CwdError::Root { .. })));
    }

    #[test]
    fn stored_cwd_outside_a_new_root_is_detected() {
        let (_dir, root) = workspace();
        let other = tempfile::tempdir().unwrap();
        let elsewhere = workspace_root(other.path()).unwrap();
        let project = resolve_cwd(&root, &home(), Some("project")).unwrap();
        assert!(inside(&root, &project));
        assert!(!inside(&elsewhere, &project));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        let (_dir, root) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.as_path().join("link")).unwrap();
        assert!(matches!(resolve_cwd(&root, &home(), Some("link")), Err(CwdError::Outside { .. })));
    }

    #[test]
    fn outgoing_path_resolves_relative_to_cwd() {
        let (_dir, root) = workspace();
        fs::create_dir(root.as_path().join("out")).unwrap();
        fs::write(root.as_path().join("out").join("r.pdf"), "%PDF").unwrap();
        let expected = root.as_path().join("out").join("r.pdf");
        assert_eq!(outgoing_path(&root, &home(), "out/r.pdf", 10).unwrap(), expected);
        let absolute = expected.to_str().unwrap().to_owned();
        assert_eq!(outgoing_path(&root, &home(), &absolute, 10).unwrap(), expected);
    }

    #[rstest]
    #[case("missing.pdf")]
    #[case("project")]
    #[case("../secret")]
    fn outgoing_path_rejects_unsendable(#[case] raw: &str) {
        let parent = tempfile::tempdir().unwrap();
        fs::create_dir_all(parent.path().join("cwd").join("project")).unwrap();
        fs::write(parent.path().join("secret"), "x").unwrap();
        let cwd = workspace_root(&parent.path().join("cwd")).unwrap();
        assert!(outgoing_path(&cwd, &home(), raw, 10).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn outgoing_symlink_out_of_cwd_is_rejected() {
        let parent = tempfile::tempdir().unwrap();
        fs::create_dir(parent.path().join("cwd")).unwrap();
        fs::write(parent.path().join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(
            parent.path().join("secret"),
            parent.path().join("cwd").join("link"),
        )
        .unwrap();
        let cwd = workspace_root(&parent.path().join("cwd")).unwrap();
        assert!(matches!(outgoing_path(&cwd, &home(), "link", 10), Err(FileError::Outside { .. })));
    }

    #[test]
    fn outgoing_path_rejects_oversized() {
        let (_dir, root) = workspace();
        fs::write(root.as_path().join("big.bin"), vec![0_u8; 11]).unwrap();
        assert!(matches!(
            outgoing_path(&root, &home(), "big.bin", 10),
            Err(FileError::TooLarge { size: 11, limit: 10, .. })
        ));
    }

    #[test]
    fn prepare_upload_creates_ignored_directory() {
        let (_dir, root) = workspace();
        prepare_upload(&root, &upload_path(root.as_path(), MessageId(1), Some("a"))).unwrap();
        assert!(root.as_path().join(".agent-hub").join("uploads").is_dir());
        assert_eq!(
            fs::read_to_string(root.as_path().join(".agent-hub").join(".gitignore")).unwrap(),
            "*\n"
        );
    }

    #[test]
    fn prepare_upload_refuses_existing_target() {
        let (_dir, root) = workspace();
        let target = upload_path(root.as_path(), MessageId(5), Some("a.txt"));
        prepare_upload(&root, &target).unwrap();
        fs::write(&target, "planted").unwrap();
        assert!(matches!(prepare_upload(&root, &target), Err(UploadError::Exists(_))));
    }

    #[cfg(unix)]
    #[test]
    fn prepare_upload_rejects_symlink_escape() {
        let (_dir, root) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.as_path().join(".agent-hub")).unwrap();
        let target = upload_path(root.as_path(), MessageId(1), Some("a"));
        assert!(matches!(prepare_upload(&root, &target), Err(UploadError::Outside { .. })));
        assert!(!outside.path().join("uploads").exists());
    }

    #[cfg(unix)]
    #[test]
    fn planted_upload_target_is_refused() {
        let (_dir, root) = workspace();
        let target = upload_path(root.as_path(), MessageId(5), Some("a.txt"));
        prepare_upload(&root, &target).unwrap();
        fs::write(root.as_path().join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(root.as_path().join("secret"), &target).unwrap();
        assert!(matches!(prepare_upload(&root, &target), Err(UploadError::Exists(_))));
    }
}
