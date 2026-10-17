//! Subscription limits from Codex: `account/rateLimits/read`, its sparse
//! `account/rateLimits/updated`, and a short app-server run that asks for them. The probe
//! never logs in with the configured key, never opens a thread and never logs the server's
//! stderr or answers.

use std::io;
use std::path::Path;

use chrono::DateTime;
use hub_agent::rpc::{RpcClient, RpcError};
use hub_core::domain::BackendKind;
use hub_core::quota::{Label, Percent, QuotaReport, QuotaUpdate, UsageWindow, WindowKind};
use serde_json::{Value, json};

use crate::backend::{AppServer, Stderr, handshake, refusing};
use crate::protocol::{Call, CodexAuth};

pub const API_KEY_LOGIN: &str = "вход по API-ключу — лимитов подписки нет";
pub const NOT_LOGGED_IN: &str = "Codex не авторизован — лимитов подписки нет";
// The spec's card names the windows by position, not by duration.
const PRIMARY: &str = "основное";
const SECONDARY: &str = "недельное";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    #[error("не удалось запустить Codex app-server: {0}")]
    Spawn(io::ErrorKind),
    #[error("Codex app-server: {0}")]
    Rpc(#[from] RpcError),
    #[error("непонятный ответ Codex о лимитах")]
    Shape,
}

impl ProbeError {
    /// What goes to the log: never the server's text.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Spawn(_) => "spawn",
            Self::Rpc(RpcError::Remote { .. }) => "remote",
            Self::Rpc(RpcError::Closed) => "closed",
            Self::Rpc(RpcError::Protocol(_)) => "protocol",
            Self::Rpc(RpcError::Timeout { .. }) => "timeout",
            Self::Shape => "shape",
        }
    }
}

/// Asks a short-lived app-server for the plan limits; its stderr is dropped.
///
/// # Errors
/// `Spawn` when the CLI does not start, `Rpc` when the exchange fails, `Shape` when the
/// answer is not a rate-limits read.
pub async fn probe(cli: &Path) -> Result<QuotaReport, ProbeError> {
    let server = AppServer::spawn(cli, refusing(), Stderr::Discard)
        .map_err(|error| ProbeError::Spawn(error.kind()))?;
    let report = read(&server.client).await;
    server.stop().await;
    report
}

/// The handshake, then the limits; a login other than `ChatGPT` has none, which is an
/// answer, not an error.
///
/// # Errors
/// `Rpc` when the exchange fails, `Shape` when the answer is not a rate-limits read.
pub async fn read(client: &RpcClient) -> Result<QuotaReport, ProbeError> {
    match handshake(client).await? {
        CodexAuth::ChatGpt => {}
        CodexAuth::Missing => {
            return Ok(QuotaReport::unavailable(BackendKind::Codex, NOT_LOGGED_IN));
        }
        CodexAuth::ApiKey | CodexAuth::Other | CodexAuth::NotRequired => {
            return Ok(QuotaReport::unavailable(BackendKind::Codex, API_KEY_LOGIN));
        }
    }
    let params = json!({"excludeResetCreditDetails": true});
    let result = client.request(Call::RateLimitsRead.method(), params).await?;
    let (plan, windows) = parse_snapshot(result.get("rateLimits").ok_or(ProbeError::Shape)?)?;
    Ok(QuotaReport::fresh(BackendKind::Codex, plan, windows))
}

/// A sparse `account/rateLimits/updated` as a session update; `None` when it names nothing.
#[must_use]
pub fn update_from(params: &Value) -> Option<QuotaUpdate> {
    // A malformed rolling update is skipped: the next one, or a probe, brings the values.
    let (plan, windows) = parse_snapshot(params.get("rateLimits")?).ok()?;
    (plan.is_some() || !windows.is_empty())
        .then(|| QuotaUpdate::new(BackendKind::Codex, plan, windows))
}

/// A `RateLimitSnapshot`: the plan and the windows it names; `null` or absent ones are left out.
///
/// # Errors
/// `Shape` when the snapshot is not an object or a window has no numeric `usedPercent`.
pub fn parse_snapshot(snapshot: &Value) -> Result<(Option<Label>, Vec<UsageWindow>), ProbeError> {
    if !snapshot.is_object() {
        return Err(ProbeError::Shape);
    }
    let windows = [(PRIMARY, "primary"), (SECONDARY, "secondary")]
        .into_iter()
        .filter_map(|(name, key)| match snapshot.get(key) {
            None | Some(Value::Null) => None,
            Some(window) => Some(parse_window(name, window)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let plan = snapshot.get("planType").and_then(Value::as_str).and_then(plan_name);
    Ok((plan, windows))
}

fn parse_window(name: &str, window: &Value) -> Result<UsageWindow, ProbeError> {
    let used = window.get("usedPercent").ok_or(ProbeError::Shape)?;
    let percent = match used.as_i64() {
        Some(whole) => Percent::from_int(whole),
        None => used.as_f64().and_then(Percent::new).ok_or(ProbeError::Shape)?,
    };
    let kind = WindowKind::Other(Label::new(name).ok_or(ProbeError::Shape)?);
    let resets_at = window
        .get("resetsAt")
        .and_then(Value::as_i64)
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0));
    Ok(UsageWindow { kind, percent, resets_at })
}

/// `plus` → `ChatGPT Plus`; an unknown plan keeps its name.
fn plan_name(raw: &str) -> Option<Label> {
    let known = match raw {
        "free" => "Free",
        "go" => "Go",
        "plus" => "Plus",
        "pro" => "Pro",
        "prolite" => "Pro Lite",
        "promax" => "Pro Max",
        "team" => "Team",
        "business" => "Business",
        "enterprise" => "Enterprise",
        "edu" => "Edu",
        other => other,
    };
    Label::new(&format!("ChatGPT {}", known.trim()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use hub_agent::rpc::RpcError;
    use hub_core::domain::BackendKind;
    use hub_core::quota::{Label, Percent, QuotaReport, QuotaUpdate, UsageWindow, WindowKind};
    use rstest::rstest;
    use serde_json::{Value, json};

    use super::*;
    use crate::testing::{Peer, pair, refusing};

    const RECORDED: &str = include_str!("../tests/fixtures/rate_limits_read.json");

    fn label(raw: &str) -> Label {
        Label::new(raw).unwrap()
    }

    fn window(name: &str, percent: i64, resets_at: Option<&str>) -> UsageWindow {
        UsageWindow {
            kind: WindowKind::Other(label(name)),
            percent: Percent::from_int(percent),
            resets_at: resets_at.map(|raw| raw.parse().unwrap()),
        }
    }

    fn recorded() -> Value {
        serde_json::from_str(RECORDED).unwrap()
    }

    #[test]
    fn the_recorded_read_becomes_a_report() {
        let snapshot = recorded().get("rateLimits").cloned().unwrap();
        assert_eq!(
            parse_snapshot(&snapshot),
            Ok((
                Some(label("ChatGPT Plus")),
                vec![
                    window("основное", 41, Some("2026-10-16T13:10:00Z")),
                    window("недельное", 12, Some("2026-10-22T08:00:00Z")),
                ]
            ))
        );
    }

    #[rstest]
    #[case::sparse(json!({"primary": null, "secondary": {"usedPercent": 13}, "planType": null}), Ok((None, vec![window("недельное", 13, None)])))]
    #[case::float_percent(json!({"primary": {"usedPercent": 41.6, "resetsAt": null}}), Ok((None, vec![window("основное", 42, None)])))]
    #[case::clamped(json!({"primary": {"usedPercent": 150}, "secondary": {"usedPercent": -5}}), Ok((None, vec![window("основное", 100, None), window("недельное", 0, None)])))]
    #[case::pro_lite(json!({"planType": "prolite"}), Ok((Some(label("ChatGPT Pro Lite")), Vec::new())))]
    #[case::unknown_plan(json!({"planType": "weird_plan"}), Ok((Some(label("ChatGPT weird_plan")), Vec::new())))]
    #[case::long_plan_is_bounded(json!({"planType": "self_serve_business_usage_based"}), Ok((Label::new("ChatGPT self_serve_business_usage_based"), Vec::new())))]
    #[case::bad_percent(json!({"primary": {"usedPercent": "x"}}), Err(ProbeError::Shape))]
    #[case::not_an_object(json!([1]), Err(ProbeError::Shape))]
    fn snapshots_are_read_tolerantly(
        #[case] snapshot: Value,
        #[case] expected: Result<(Option<Label>, Vec<UsageWindow>), ProbeError>,
    ) {
        assert_eq!(parse_snapshot(&snapshot), expected);
    }

    #[rstest]
    #[case::one_window(json!({"rateLimits": {"secondary": {"usedPercent": 20}}}), Some(QuotaUpdate::new(BackendKind::Codex, None, vec![window("недельное", 20, None)])))]
    #[case::nothing(json!({"rateLimits": {"primary": null, "planType": null}}), None)]
    #[case::malformed(json!({"rateLimits": 7}), None)]
    #[case::missing(json!({}), None)]
    fn rolling_updates_carry_what_they_name(
        #[case] params: Value,
        #[case] expected: Option<QuotaUpdate>,
    ) {
        assert_eq!(update_from(&params), expected);
    }

    /// Answers each request with the next reply (`Err` is a JSON-RPC error); once they run
    /// out it stays silent. Returns every method the client sent and the params it sent them with.
    async fn server(
        mut peer: Peer,
        replies: Vec<Result<Value, &'static str>>,
    ) -> Vec<(String, Value)> {
        let mut seen = Vec::new();
        let mut replies = replies.into_iter();
        while let Some(message) = peer.read().await {
            let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
            seen.push((method.to_owned(), message.get("params").cloned().unwrap_or(Value::Null)));
            let Some(id) = message.get("id").cloned() else { continue };
            match replies.next() {
                Some(Ok(result)) => peer.write(json!({"id": id, "result": result})).await,
                Some(Err(message)) => {
                    peer.write(json!({"id": id, "error": {"code": -32000, "message": message}}))
                        .await;
                }
                None => {}
            }
        }
        seen
    }

    async fn probe_over_pair(
        replies: Vec<Result<Value, &'static str>>,
        timeout: Duration,
    ) -> (Result<QuotaReport, ProbeError>, Vec<(String, Value)>) {
        let (connection, peer) = pair(refusing(), timeout);
        let server = tokio::spawn(server(peer, replies));
        let report = read(&connection.client).await;
        connection.client.close();
        (report, server.await.unwrap())
    }

    fn methods(seen: &[(String, Value)]) -> Vec<&str> {
        seen.iter().map(|(method, _)| method.as_str()).collect()
    }

    #[tokio::test]
    async fn a_chatgpt_login_reads_the_limits_without_side_effects() {
        let replies = vec![
            Ok(json!({})),
            Ok(json!({"account": {"type": "chatgpt"}, "requiresOpenaiAuth": true})),
            Ok(recorded()),
        ];
        let (report, seen) = probe_over_pair(replies, Duration::from_secs(5)).await;
        assert_eq!(report.map(|report| report.plan), Ok(Some(label("ChatGPT Plus"))));
        assert_eq!(
            methods(&seen),
            ["initialize", "initialized", "account/read", "account/rateLimits/read"]
        );
        assert_eq!(
            seen.last().map(|(_, params)| params),
            Some(&json!({"excludeResetCreditDetails": true}))
        );
    }

    #[rstest]
    #[case::api_key(json!({"account": {"type": "apiKey"}}), API_KEY_LOGIN)]
    #[case::not_required(json!({"account": null, "requiresOpenaiAuth": false}), API_KEY_LOGIN)]
    #[case::logged_out(json!({"account": null, "requiresOpenaiAuth": true}), NOT_LOGGED_IN)]
    #[tokio::test]
    async fn without_chatgpt_there_are_no_limits_and_no_login(
        #[case] account: Value,
        #[case] reason: &str,
    ) {
        let (report, seen) =
            probe_over_pair(vec![Ok(json!({})), Ok(account)], Duration::from_secs(5)).await;
        assert_eq!(report, Ok(QuotaReport::unavailable(BackendKind::Codex, reason)));
        assert_eq!(methods(&seen), ["initialize", "initialized", "account/read"]);
    }

    #[tokio::test]
    async fn a_refused_read_is_an_error() {
        let replies = vec![
            Ok(json!({})),
            Ok(json!({"account": {"type": "chatgpt"}})),
            Err("usage unavailable"),
        ];
        let (report, _seen) = probe_over_pair(replies, Duration::from_secs(5)).await;
        assert_eq!(
            report,
            Err(ProbeError::Rpc(RpcError::Remote {
                code: -32000,
                message: "usage unavailable".to_owned()
            }))
        );
    }

    #[tokio::test]
    async fn a_silent_read_times_out() {
        let replies = vec![Ok(json!({})), Ok(json!({"account": {"type": "chatgpt"}}))];
        let (report, _seen) = probe_over_pair(replies, Duration::from_millis(100)).await;
        assert!(matches!(report, Err(ProbeError::Rpc(RpcError::Timeout { .. }))), "{report:?}");
    }

    #[tokio::test]
    async fn a_read_without_rate_limits_is_a_shape_error() {
        let replies =
            vec![Ok(json!({})), Ok(json!({"account": {"type": "chatgpt"}})), Ok(json!({}))];
        let (report, _seen) = probe_over_pair(replies, Duration::from_secs(5)).await;
        assert_eq!(report, Err(ProbeError::Shape));
    }

    #[tokio::test]
    async fn a_missing_binary_is_a_spawn_error() {
        let outcome = probe(Path::new("definitely-not-codex-binary")).await;
        assert!(matches!(outcome, Err(ProbeError::Spawn(_))), "{outcome:?}");
    }
}
