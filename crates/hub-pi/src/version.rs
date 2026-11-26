//! Locating and checking the `Pi` CLI, and what its own auth store says about a provider.

pub use hub_agent::cli::Version;

use hub_agent::cli::{VersionError, hide_window};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

/// RPC shapes verified against `Pi` 1.6.0 (`pi --mode rpc` over stdio).
pub const MIN_VERSION: Version = Version { major: 1, minor: 6, patch: 0 };
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("Pi не найден: укажите путь в настройках или установите `pi` в PATH")]
    Missing,
    #[error(transparent)]
    Version(#[from] VersionError),
    #[error("непонятный ответ `pi --version`: {0}")]
    Unrecognized(String),
    #[error("Pi {found} слишком старый, нужен {} или новее", MIN_VERSION)]
    TooOld { found: Version },
    #[error("`pi {probe}`: {message}")]
    Probe { probe: &'static str, message: String },
}

/// Reads `pi --version`. The build prints a bare semver line; anything else is refused, so a
/// wrapper script that answers with help text cannot pass as a supported agent.
pub async fn check(path: &Path) -> Result<Version, CliError> {
    let output = probe(path, "--version", &["--version"]).await?;
    let text = String::from_utf8_lossy(&output.stdout);
    let found = parse_version(&text)
        .ok_or_else(|| CliError::Unrecognized(hub_core::render::truncate(text.trim(), 256)))?;
    if found < MIN_VERSION {
        return Err(CliError::TooOld { found });
    }
    Ok(found)
}

/// Whether the provider's credentials already sit in the user's own `~/.pi/agent/auth.json`.
///
/// `pi auth check` exits non-zero when a provider is not ready, so a failure is an answer and
/// not an error; only a missing or unreadable probe is one. The hub never reads that file.
pub async fn auth_state(path: &Path, provider: &str) -> Result<bool, CliError> {
    let args = ["auth", "check", "--provider", provider, "--json"];
    let output = probe(path, "auth check", &args).await?;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_auth(&text)
        .ok_or_else(|| CliError::Unrecognized(hub_core::render::truncate(text.trim(), 256)))
}

#[derive(Deserialize)]
struct AuthJson {
    status: String,
}

fn parse_auth(output: &str) -> Option<bool> {
    serde_json::from_str::<AuthJson>(output.trim()).ok().map(|auth| auth.status == "ready")
}

async fn probe(
    path: &Path,
    probe: &'static str,
    args: &[&str],
) -> Result<std::process::Output, CliError> {
    let mut command = Command::new(path);
    command.args(args).stdin(std::process::Stdio::null()).kill_on_drop(true);
    hide_window(&mut command);
    let output = tokio::time::timeout(PROBE_TIMEOUT, command.output())
        .await
        .map_err(|_| CliError::Probe {
            probe, message: "нет ответа за 10 с".to_owned()
        })?
        .map_err(|source| VersionError::Spawn { path: path.to_path_buf(), source })?;
    // `auth check` reports a missing login as a non-zero exit; the caller reads the JSON.
    if !output.status.success() && output.stdout.is_empty() {
        return Err(CliError::Probe {
            probe,
            message: format!("процесс завершился: {}", output.status),
        });
    }
    Ok(output)
}

pub fn locate(configured: Option<&Path>) -> Result<PathBuf, CliError> {
    hub_agent::cli::locate("pi", configured).ok_or(CliError::Missing)
}

#[must_use]
pub fn parse_version(output: &str) -> Option<Version> {
    output.split_whitespace().find_map(Version::parse)
}

/// The provider part of a `provider/model` model id. Pi's `--provider` needs it separately,
/// and a bare model name means the agent's own default provider.
#[must_use]
pub fn provider_of(model: &str) -> Option<&str> {
    model.split_once('/').map(|(provider, _)| provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("1.6.0", Some(Version { major: 1, minor: 6, patch: 0 }))]
    #[case("1.12.3", Some(Version { major: 1, minor: 12, patch: 3 }))]
    #[case("pi 1.7.0", Some(Version { major: 1, minor: 7, patch: 0 }))]
    #[case("1.6.0-beta", None)]
    #[case("1.6", None)]
    #[case("", None)]
    #[case("unknown", None)]
    fn versions_are_parsed(#[case] output: &str, #[case] expected: Option<Version>) {
        assert_eq!(parse_version(output), expected);
    }

    #[rstest]
    #[case(r#"{"status":"ready","provider":"qwen-token-plan","authType":"api_key"}"#, Some(true))]
    #[case(
        r#"{"status":"not_ready","provider":"openai-codex","reason":"credentials_not_configured"}"#,
        Some(false)
    )]
    #[case("", None)]
    #[case("ready", None)]
    fn auth_answers_are_read(#[case] output: &str, #[case] expected: Option<bool>) {
        assert_eq!(parse_auth(output), expected);
    }

    #[rstest]
    #[case("openai-codex/gpt-5.4", Some("openai-codex"))]
    #[case("qwen3.8-max", None)]
    fn provider_is_the_part_before_the_slash(#[case] model: &str, #[case] expected: Option<&str>) {
        assert_eq!(provider_of(model), expected);
    }

    #[test]
    fn configured_path_is_preserved() {
        let path = Path::new(if cfg!(windows) { r"C:\bin\pi.cmd" } else { "/custom/bin/pi" });
        assert_eq!(locate(Some(path)).ok().as_deref(), Some(path));
    }

    #[tokio::test]
    async fn a_missing_binary_is_reported_as_such() {
        let path = std::env::temp_dir().join("definitely-not-a-pi-binary");
        assert!(matches!(check(&path).await, Err(CliError::Version(VersionError::Spawn { .. }))));
    }
}
