//! Which agent CLI to run and whether it speaks the protocol this hub expects.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::process::hide_window;

/// The protocol features this hub relies on (task messages, result origin, replayed prompts)
/// were verified against this version.
pub const MIN_VERSION: Version = Version { major: 2, minor: 1, patch: 280 };
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const CLI_NAME: &str = "claude";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("Claude Code не найден: укажите путь в настройках или установите `claude` в PATH")]
    Missing,
    #[error("не удалось запустить {}", path.display())]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("`{} --version` не ответил за {} с", path.display(), VERSION_TIMEOUT.as_secs())]
    Timeout { path: PathBuf },
    #[error("непонятный ответ `claude --version`: {0}")]
    Unrecognized(String),
    #[error("Claude Code {found} слишком старый, нужен {} или новее", MIN_VERSION)]
    TooOld { found: Version },
}

/// `2.1.287 (Claude Code)` → 2.1.287.
#[must_use]
pub fn parse_version(output: &str) -> Option<Version> {
    let token = output.split_whitespace().next()?;
    let mut parts = token.split('.').map(str::parse::<u32>);
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) => {
            Some(Version { major, minor, patch })
        }
        _ => None,
    }
}

pub fn locate(configured: Option<&Path>) -> Result<PathBuf, CliError> {
    match configured {
        Some(path) => Ok(path.to_path_buf()),
        None => which::which(CLI_NAME).map_err(|_| CliError::Missing),
    }
}

pub async fn check(cli: &Path) -> Result<Version, CliError> {
    let mut command = Command::new(cli);
    command.arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    hide_window(&mut command);
    let output = tokio::time::timeout(VERSION_TIMEOUT, command.output())
        .await
        .map_err(|_| CliError::Timeout { path: cli.to_path_buf() })?
        .map_err(|source| CliError::Spawn { path: cli.to_path_buf(), source })?;
    let text = String::from_utf8_lossy(&output.stdout);
    let found =
        parse_version(&text).ok_or_else(|| CliError::Unrecognized(text.trim().to_owned()))?;
    if found < MIN_VERSION { Err(CliError::TooOld { found }) } else { Ok(found) }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("2.1.287 (Claude Code)\n", Some(Version { major: 2, minor: 1, patch: 287 }))]
    #[case("2.1.280", Some(Version { major: 2, minor: 1, patch: 280 }))]
    #[case("", None)]
    #[case("Claude Code 2.1.287", None)]
    #[case("2.1", None)]
    #[case("2.1.x", None)]
    #[case("2.1.287.4", None)]
    fn versions_are_parsed(#[case] output: &str, #[case] expected: Option<Version>) {
        assert_eq!(parse_version(output), expected);
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(Version { major: 2, minor: 1, patch: 1000 } > MIN_VERSION);
        assert!(Version { major: 2, minor: 1, patch: 279 } < MIN_VERSION);
        assert_eq!(MIN_VERSION.to_string(), "2.1.280");
    }

    #[test]
    fn configured_path_is_used_as_is() {
        let path = std::env::temp_dir().join("claude-custom");
        assert_eq!(locate(Some(&path)).unwrap(), path);
    }

    #[tokio::test]
    async fn missing_binary_is_a_spawn_error() {
        let path = std::env::temp_dir().join("definitely-not-claude-binary");
        assert!(matches!(check(&path).await, Err(CliError::Spawn { .. })));
    }
}
