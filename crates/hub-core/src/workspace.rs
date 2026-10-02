//! Working-directory candidates; containment and existence are checked by the shell.

use std::path::{Path, PathBuf};

#[must_use]
pub fn expand_home(raw: &str, home: &Path) -> PathBuf {
    match raw.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with(['/', '\\']) => {
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        Some(_) | None => PathBuf::from(raw),
    }
}

/// `raw` absolute, `~`-prefixed or relative to `root`; blank means `root`.
#[must_use]
pub fn candidate(root: &Path, home: &Path, raw: Option<&str>) -> PathBuf {
    match raw.map(str::trim).filter(|raw| !raw.is_empty()) {
        None => root.to_path_buf(),
        Some(raw) => {
            let path = expand_home(raw, home);
            if path.is_absolute() { path } else { root.join(path) }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" })
    }

    fn home() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\Users\me" } else { "/home/me" })
    }

    #[test]
    fn missing_path_means_root() {
        assert_eq!(candidate(&root(), &home(), None), root());
        assert_eq!(candidate(&root(), &home(), Some("  ")), root());
    }

    #[test]
    fn relative_path_is_under_root() {
        assert_eq!(candidate(&root(), &home(), Some("project/sub")), root().join("project/sub"));
    }

    #[test]
    fn absolute_path_is_kept() {
        let inside = root().join("project");
        assert_eq!(candidate(&root(), &home(), inside.to_str()), inside);
    }

    #[test]
    fn tilde_expands_to_home() {
        assert_eq!(expand_home("~", &home()), home());
        assert_eq!(expand_home("~/p", &home()), home().join("p"));
        assert_eq!(expand_home("~other/p", &home()), PathBuf::from("~other/p"));
    }
}
