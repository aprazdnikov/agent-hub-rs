use crate::common::Fixture;
use hub_hermes::version::{CliError, Version, check};
use rstest::rstest;
use serde_json::json;

#[tokio::test]
async fn checks_acp_version_and_dependency_imports() {
    let fixture = Fixture::new("version");
    let result = check(&fixture.cli).await;
    assert_eq!(result.unwrap(), Version { major: 0, minor: 21, patch: 5 });
    let args: Vec<_> =
        fixture.records().iter().filter_map(|record| record.get("argv").cloned()).collect();
    assert_eq!(args, vec![json!(["acp", "--version"]), json!(["acp", "--check"])]);
}

#[rstest]
#[case("version_error", "--version")]
#[case("dependency_error", "--check")]
#[tokio::test]
async fn unsuccessful_probe_status_is_never_reported_as_ready(
    #[case] case: &str,
    #[case] expected: &str,
) {
    let fixture = Fixture::new(case);
    assert!(
        matches!(check(&fixture.cli).await, Err(CliError::Probe { probe, .. }) if probe == expected)
    );
}

#[tokio::test]
async fn old_version_does_not_run_dependency_probe() {
    let fixture = Fixture::new("old_version");
    assert!(matches!(check(&fixture.cli).await, Err(CliError::TooOld { .. })));
    assert_eq!(fixture.records().len(), 1);
}

#[tokio::test]
async fn dependency_probe_has_a_bounded_deadline() {
    let fixture = Fixture::new("dependency_timeout");
    let result = tokio::time::timeout(std::time::Duration::from_secs(15), check(&fixture.cli))
        .await
        .unwrap();
    assert!(matches!(result, Err(CliError::Probe { probe: "--check", .. })));
}

#[tokio::test]
async fn missing_binary_preserves_spawn_error() {
    let path = std::path::Path::new("/definitely-missing-hermes-acp-binary");
    assert!(matches!(
        check(path).await,
        Err(CliError::Version(hub_agent::cli::VersionError::Spawn { .. }))
    ));
}
