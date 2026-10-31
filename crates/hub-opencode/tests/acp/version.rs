use crate::common::Fixture;
use hub_opencode::version::{CliError, Version, check};
use rstest::rstest;
use serde_json::json;

#[tokio::test]
async fn checks_acp_version_without_starting_a_session() {
    let fixture = Fixture::new("version");
    let result = check(&fixture.cli).await;
    assert_eq!(result.unwrap(), Version { major: 1, minor: 19, patch: 5 });
    let args: Vec<_> =
        fixture.records().iter().filter_map(|record| record.get("argv").cloned()).collect();
    assert_eq!(args, vec![json!(["acp", "--version"])]);
}

#[tokio::test]
async fn unsuccessful_probe_status_is_never_reported_as_ready() {
    let fixture = Fixture::new("version_error");
    assert!(
        matches!(check(&fixture.cli).await, Err(CliError::Probe { probe, .. }) if probe == "--version")
    );
}

#[tokio::test]
async fn old_version_is_refused() {
    let fixture = Fixture::new("old_version");
    assert!(matches!(check(&fixture.cli).await, Err(CliError::TooOld { .. })));
    assert_eq!(fixture.records().len(), 1);
}

#[rstest]
#[case("1.19.5\n", Some(Version { major: 1, minor: 19, patch: 5 }))]
#[case("1.20.0-rc.1\n", None)]
#[case("opencode 1.19.7\n", Some(Version { major: 1, minor: 19, patch: 7 }))]
#[tokio::test]
async fn probe_output_is_parsed_strictly(#[case] output: &str, #[case] expected: Option<Version>) {
    assert_eq!(hub_opencode::version::parse_version(output), expected);
}

#[tokio::test]
async fn missing_binary_preserves_spawn_error() {
    let path = std::path::Path::new("/definitely-missing-opencode-acp-binary");
    assert!(matches!(
        check(path).await,
        Err(CliError::Version(hub_agent::cli::VersionError::Spawn { .. }))
    ));
}
