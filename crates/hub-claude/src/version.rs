//! Which Claude CLI to run and whether it speaks the protocol this hub expects.

use std::path::{Path, PathBuf};

use hub_agent::cli::{self, VersionError};
pub use hub_agent::cli::Version;

/// The protocol features this hub relies on (task messages, result origin, replayed prompts)
/// were verified against this version.
pub const MIN_VERSION: Version = Version { major: 2, minor: 1, patch: 280 };
const CLI_NAME: &str = "claude";

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("Claude Code не найден: укажите путь в настройках или установите `claude` в PATH")]
    Missing,
    #[error(transparent)]
    Version(#[from] VersionError),
    #[error("непонятный ответ `claude --version`: {0}")]
    Unrecognized(String),
    #[error("Claude Code {found} слишком старый, нужен {} или новее", MIN_VERSION)]
    TooOld { found: Version },
}

/// `2.1.287 (Claude Code)` → 2.1.287.
#[must_use]
pub fn parse_version(output: &str) -> Option<Version> {
    output.split_whitespace().next().and_then(Version::parse)
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
        assert!(matches!(
            check(&path).await,
            Err(CliError::Version(VersionError::Spawn { .. }))
        ));
    }
}
