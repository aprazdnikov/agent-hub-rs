//! Which Qwen Code CLI to run and whether it speaks the ACP this hub was verified against.

use std::path::{Path, PathBuf};

pub use hub_agent::cli::Version;
use hub_agent::cli::{self, VersionError};

/// The ACP wire shapes this hub speaks were checked against qwen-code at this version.
pub const MIN_VERSION: Version = Version { major: 0, minor: 25, patch: 0 };
const CLI_NAME: &str = "qwen";

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("Qwen Code не найден: укажите путь в настройках или установите `qwen` в PATH")]
    Missing,
    #[error(transparent)]
    Version(#[from] VersionError),
    #[error("непонятный ответ `qwen --version`: {0}")]
    Unrecognized(String),
    #[error("Qwen Code {found} слишком старый, нужен {} или новее", MIN_VERSION)]
    TooOld { found: Version },
}

/// `0.25.0` (yargs prints the bare version) → 0.25.0.
#[must_use]
pub fn parse_version(output: &str) -> Option<Version> {
    output.split_whitespace().find_map(Version::parse)
}

pub fn locate(configured: Option<&Path>) -> Result<PathBuf, CliError> {
    cli::locate(CLI_NAME, configured).ok_or(CliError::Missing)
}

pub async fn check(path: &Path) -> Result<Version, CliError> {
    let text = cli::version_output(path).await?;
    let found =
        parse_version(&text).ok_or_else(|| CliError::Unrecognized(text.trim().to_owned()))?;
    if found < MIN_VERSION { Err(CliError::TooOld { found }) } else { Ok(found) }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("0.25.0\n", Some(Version { major: 0, minor: 25, patch: 0 }))]
    #[case("qwen 0.26.1", Some(Version { major: 0, minor: 26, patch: 1 }))]
    #[case("", None)]
    #[case("unknown", None)]
    #[case("0.25", None)]
    fn versions_are_parsed(#[case] output: &str, #[case] expected: Option<Version>) {
        assert_eq!(parse_version(output), expected);
    }

    #[test]
    fn minimum_is_the_verified_protocol() {
        assert_eq!(MIN_VERSION.to_string(), "0.25.0");
        assert!(Version { major: 0, minor: 24, patch: 99 } < MIN_VERSION);
    }

    #[test]
    fn configured_path_is_used_as_is() {
        // npm installs `qwen.cmd` on Windows; it must be run as found.
        let path = std::env::temp_dir().join("qwen.cmd");
        assert_eq!(locate(Some(&path)).unwrap(), path);
    }

    #[tokio::test]
    async fn missing_binary_is_a_spawn_error() {
        let path = std::env::temp_dir().join("definitely-not-qwen-binary");
        assert!(matches!(check(&path).await, Err(CliError::Version(VersionError::Spawn { .. }))));
    }
}
