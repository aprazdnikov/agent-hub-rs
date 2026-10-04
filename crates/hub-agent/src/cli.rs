//! Locating an agent CLI and asking it for its version.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// `2.1.287` → 2.1.287; a fourth component means it is not a version.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        let mut parts = token.split('.').map(str::parse::<u32>);
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) => {
                Some(Self { major, minor, patch })
            }
            _ => None,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    #[error("не удалось запустить {}", path.display())]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("`{} --version` не ответил за {} с", path.display(), VERSION_TIMEOUT.as_secs())]
    Timeout { path: PathBuf },
}

#[must_use]
pub fn locate(name: &str, configured: Option<&Path>) -> Option<PathBuf> {
    match configured {
        Some(path) => Some(path.to_path_buf()),
        None => which::which(name).ok(),
    }
}

pub async fn version_output(cli: &Path) -> Result<String, VersionError> {
    let mut command = Command::new(cli);
    command.arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    hide_window(&mut command);
    let output = tokio::time::timeout(VERSION_TIMEOUT, command.output())
        .await
        .map_err(|_| VersionError::Timeout { path: cli.to_path_buf() })?
        .map_err(|source| VersionError::Spawn { path: cli.to_path_buf(), source })?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Without it every CLI process flashes a console window under the desktop app.
#[cfg(windows)]
pub fn hide_window(command: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("2.1.287", Some(Version { major: 2, minor: 1, patch: 287 }))]
    #[case("0.160.0", Some(Version { major: 0, minor: 160, patch: 0 }))]
    #[case("", None)]
    #[case("2.1", None)]
    #[case("2.1.x", None)]
    #[case("2.1.287.4", None)]
    fn version_tokens_are_parsed(#[case] token: &str, #[case] expected: Option<Version>) {
        assert_eq!(Version::parse(token), expected);
    }

    #[test]
    fn versions_compare_numerically_and_print_dotted() {
        let old = Version { major: 0, minor: 159, patch: 9 };
        let new = Version { major: 0, minor: 160, patch: 0 };
        assert!(old < new);
        assert_eq!(new.to_string(), "0.160.0");
    }

    #[test]
    fn configured_path_is_used_as_is() {
        let path = std::env::temp_dir().join("agent-custom");
        assert_eq!(locate("agent", Some(&path)), Some(path));
    }

    #[test]
    fn unknown_name_is_not_found() {
        assert_eq!(locate("definitely-not-an-agent-cli", None), None);
    }

    #[tokio::test]
    async fn missing_binary_is_a_spawn_error() {
        let path = std::env::temp_dir().join("definitely-not-an-agent-binary");
        assert!(matches!(version_output(&path).await, Err(VersionError::Spawn { .. })));
    }
}
