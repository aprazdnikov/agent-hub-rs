//! Locating and checking the Hermes ACP adapter.

pub use hub_agent::cli::Version;

use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

use hub_agent::cli::{VersionError, hide_window};
use tokio::process::Command;

/// ACP shapes verified against Hermes 0.21.5 (derived build 9108.g61b7f95).
pub const MIN_VERSION: Version = Version { major: 0, minor: 21, patch: 5 };
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("Hermes не найден: укажите путь в настройках или установите `hermes` в PATH")]
    Missing,
    #[error(transparent)]
    Version(#[from] VersionError),
    #[error("непонятный ответ `hermes acp --version`: {0}")]
    Unrecognized(String),
    #[error("Hermes {found} слишком старый, нужен {} или новее", MIN_VERSION)]
    TooOld { found: Version },
    #[error("`hermes acp {probe}`: {message}")]
    Probe { probe: &'static str, message: String },
}

/// Checks the ACP version and imports without constructing an agent or calling a model.
pub async fn check(path: &Path) -> Result<Version, CliError> {
    let output = probe(path, "--version").await?;
    let text = String::from_utf8_lossy(&output.stdout);
    let found = parse_version(&text)
        .ok_or_else(|| CliError::Unrecognized(hub_core::render::truncate(text.trim(), 256)))?;
    if found < MIN_VERSION {
        return Err(CliError::TooOld { found });
    }
    probe(path, "--check").await?;
    Ok(found)
}

async fn probe(path: &Path, flag: &'static str) -> Result<Output, CliError> {
    let mut command = Command::new(path);
    command.args(["acp", flag]).stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    hide_window(&mut command);
    let output = tokio::time::timeout(PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| CliError::Probe {
            probe: flag, message: "нет ответа за 10 с".to_owned()
        })?
        .map_err(|source| VersionError::Spawn { path: path.to_path_buf(), source })?;
    if !output.status.success() {
        return Err(CliError::Probe {
            probe: flag,
            message: format!("процесс завершился: {}", output.status),
        });
    }
    Ok(output)
}

pub fn locate(configured: Option<&Path>) -> Result<PathBuf, CliError> {
    hub_agent::cli::locate("hermes", configured).ok_or(CliError::Missing)
}

#[must_use]
pub fn parse_version(output: &str) -> Option<Version> {
    output.split_whitespace().find_map(|token| {
        let base = match token.split_once('+') {
            Some((base, metadata))
                if metadata.split('.').all(|part| {
                    !part.is_empty()
                        && part.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                }) =>
            {
                base
            }
            Some(_) => return None,
            None => token,
        };
        Version::parse(base)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("0.21.5", Some(Version { major: 0, minor: 21, patch: 5 }))]
    #[case("0.21.6+build.23", Some(Version { major: 0, minor: 21, patch: 6 }))]
    #[case("Hermes ACP 0.22.0", Some(Version { major: 0, minor: 22, patch: 0 }))]
    #[case("0.21.5+", None)]
    #[case("0.21.5+bad..metadata", None)]
    #[case("0.21.5+build/23", None)]
    #[case("0.21.5.4", None)]
    #[case("0.21", None)]
    #[case("unknown", None)]
    fn versions_are_parsed_without_accepting_malformed_tokens(
        #[case] output: &str,
        #[case] expected: Option<Version>,
    ) {
        assert_eq!(parse_version(output), expected);
    }

    #[test]
    fn configured_path_is_preserved() {
        let path = std::path::Path::new("/custom/bin/hermes.cmd");
        assert_eq!(locate(Some(path)).ok().as_deref(), Some(path));
    }

    #[test]
    fn parses_derived_acp_version() {
        assert_eq!(
            parse_version("0.21.5+9108.g61b7f95\n"),
            Some(Version { major: 0, minor: 21, patch: 5 })
        );
    }
}
