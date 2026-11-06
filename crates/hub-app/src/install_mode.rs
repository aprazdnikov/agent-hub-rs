//! Determines whether automatic self-replacement is appropriate.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Portable,
    Manual,
}

#[derive(Debug, Clone, Copy)]
pub enum Platform {
    Linux,
    Windows,
    Mac,
    Other,
}

#[must_use]
pub fn detect(executable: &Path, platform: Platform) -> Mode {
    match platform {
        Platform::Linux
            if executable == Path::new("/usr/bin/agent-hub")
                || executable == Path::new("/bin/agent-hub")
                || executable == Path::new("/usr/bin/agent-hub (deleted)")
                || executable == Path::new("/bin/agent-hub (deleted)") =>
        {
            Mode::Manual
        }
        Platform::Windows => {
            let Some(parent) = executable.parent() else { return Mode::Manual };
            match std::fs::symlink_metadata(parent.join("agent-hub.install-kind")) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Mode::Portable,
                Ok(_) | Err(_) => Mode::Manual,
            }
        }
        Platform::Mac => {
            let bundle = executable.parent().and_then(Path::parent).and_then(Path::parent);
            let macos = executable.parent().and_then(Path::file_name);
            let contents = executable.parent().and_then(Path::parent).and_then(Path::file_name);
            if macos
                .and_then(std::ffi::OsStr::to_str)
                .is_some_and(|s| s.eq_ignore_ascii_case("MacOS"))
                && contents
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case("Contents"))
                && bundle
                    .and_then(Path::extension)
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|s| s.eq_ignore_ascii_case("app"))
            {
                Mode::Manual
            } else {
                Mode::Portable
            }
        }
        Platform::Linux | Platform::Other => Mode::Portable,
    }
}

#[must_use]
pub fn resolved(executable: &Path, platform: Platform) -> Mode {
    std::fs::canonicalize(executable).map_or(Mode::Manual, |path| detect(&path, platform))
}

#[must_use]
pub fn current() -> Mode {
    let platform = if cfg!(target_os = "linux") {
        Platform::Linux
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::Mac
    } else {
        Platform::Other
    };
    std::env::current_exe().map_or(Mode::Manual, |path| resolved(&path, platform))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_binary_remains_automatically_updatable() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect(&dir.path().join("agent-hub"), Platform::Linux), Mode::Portable);
    }

    #[test]
    fn nsis_marker_is_scoped_to_the_executable_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("agent-hub.install-kind"), "nsis\n").unwrap();
        assert_eq!(detect(&dir.path().join("agent-hub.exe"), Platform::Windows), Mode::Manual);
        assert_eq!(
            detect(&dir.path().join("elsewhere/agent-hub.exe"), Platform::Windows),
            Mode::Portable
        );
    }

    #[test]
    fn malformed_or_unreadable_marker_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("agent-hub.install-kind");
        std::fs::write(&marker, "unknown").unwrap();
        assert_eq!(detect(&dir.path().join("agent-hub.exe"), Platform::Windows), Mode::Manual);
        std::fs::remove_file(&marker).unwrap();
        std::fs::create_dir(&marker).unwrap();
        assert_eq!(detect(&dir.path().join("agent-hub.exe"), Platform::Windows), Mode::Manual);
    }

    #[test]
    fn system_package_and_macos_bundle_cannot_self_replace() {
        assert_eq!(
            detect(std::path::Path::new("/usr/bin/agent-hub"), Platform::Linux),
            Mode::Manual
        );
        assert_eq!(
            detect(std::path::Path::new("/usr/bin/agent-hub (deleted)"), Platform::Linux),
            Mode::Manual
        );
        assert_eq!(
            detect(
                std::path::Path::new(
                    "/Users/a/Applications/agent-hub.app/Contents/MacOS/agent-hub"
                ),
                Platform::Mac
            ),
            Mode::Manual
        );
        assert_eq!(
            detect(
                std::path::Path::new(
                    "/Users/a/Applications/AGENT-HUB.APP/contents/macos/agent-hub"
                ),
                Platform::Mac
            ),
            Mode::Manual
        );
        assert_eq!(
            detect(std::path::Path::new("/tmp/Contents/MacOS/agent-hub"), Platform::Mac),
            Mode::Portable
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_resolve_to_installed_binary_before_detection() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("x.app/Contents/MacOS");
        std::fs::create_dir_all(&bundle).unwrap();
        let real = bundle.join("agent-hub");
        std::fs::write(&real, "binary").unwrap();
        let link = dir.path().join("portable-looking");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(resolved(&link, Platform::Mac), Mode::Manual);
        std::fs::write(bundle.join("agent-hub.install-kind"), "nsis").unwrap();
        assert_eq!(resolved(&link, Platform::Windows), Mode::Manual);
        std::fs::remove_file(real).unwrap();
        assert_eq!(resolved(&link, Platform::Windows), Mode::Manual);
    }
}
