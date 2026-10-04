//! Which Codex CLI to run and whether it speaks the app-server protocol this hub expects.

use std::path::{Path, PathBuf};

pub use hub_agent::cli::Version;
use hub_agent::cli::{self, VersionError};

/// The app-server protocol this hub speaks was verified against this version.
pub const MIN_VERSION: Version = Version { major: 0, minor: 160, patch: 0 };
const CLI_NAME: &str = "codex";

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("Codex не найден: укажите путь в настройках или установите `codex` в PATH")]
    Missing,
    #[error(transparent)]
    Version(#[from] VersionError),
    #[error("непонятный ответ `codex --version`: {0}")]
    Unrecognized(String),
    #[error("Codex {found} слишком старый, нужен {} или новее", MIN_VERSION)]
    TooOld { found: Version },
}

/// `codex-cli 0.160.0` → 0.160.0.
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
    #[case("codex-cli 0.160.0\n", Some(Version { major: 0, minor: 160, patch: 0 }))]
    #[case("codex-cli 1.2.3", Some(Version { major: 1, minor: 2, patch: 3 }))]
    #[case("", None)]
    #[case("codex-cli", None)]
    #[case("codex-cli 0.160", None)]
    fn versions_are_parsed(#[case] output: &str, #[case] expected: Option<Version>) {
        assert_eq!(parse_version(output), expected);
    }

    #[test]
    fn minimum_is_the_verified_protocol() {
        assert_eq!(MIN_VERSION.to_string(), "0.160.0");
        assert!(Version { major: 0, minor: 159, patch: 99 } < MIN_VERSION);
    }

    #[test]
    fn configured_path_is_used_as_is() {
        // A `codex.cmd` from npm on Windows is found by `which` and must be run as found.
        let path = std::env::temp_dir().join("codex.cmd");
        assert_eq!(locate(Some(&path)).unwrap(), path);
    }

    #[tokio::test]
    async fn missing_binary_is_a_spawn_error() {
        let path = std::env::temp_dir().join("definitely-not-codex-binary");
        assert!(matches!(check(&path).await, Err(CliError::Version(VersionError::Spawn { .. }))));
    }
}
