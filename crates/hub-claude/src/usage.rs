//! Subscription limits from the Claude CLI: the `get_usage` control reply and
//! `rate_limit_event`. Both are untrusted: strings pass `Label`, numbers `Percent`.

use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use hub_agent::cli::hide_window;
use hub_core::domain::BackendKind;
use hub_core::quota::{Label, Percent, QuotaReport, QuotaUpdate, UsageWindow, WindowKind};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

use crate::outgoing::{self, ENTRYPOINT};
use crate::wire::RateLimitInfo;

pub const NO_SUBSCRIPTION: &str = "вход не через подписку claude.ai — лимитов подписки нет";
const OAUTH_APPS: &str = "7 дней, приложения";
const OVERAGE_INCLUDED: &str = "7 дней, включённые модели";
/// `resetsAt` above this is in milliseconds: in seconds it stays below until the year 5138.
const MILLIS_FROM: i64 = 100_000_000_000;

/// How the probe asks; the live check (task 1 of the plan) picks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handshake {
    /// `get_usage` right away.
    Direct,
    /// A minimal `initialize` first, as SDK hosts do.
    InitializeFirst,
    /// The CLI does not answer without a prompt: `/limits` uses the cache only.
    CacheOnly,
}

pub const HANDSHAKE: Handshake = Handshake::InitializeFirst;

/// No prompt, no settings, no MCP servers, no hooks: the run only answers control requests.
pub const PROBE_ARGS: [&str; 7] = [
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--setting-sources=",
    "--strict-mcp-config",
];
/// Below the hub's 20 s outer timeout, which also has to cover the 5 s shutdown.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);
// After stdin closes the CLI exits on its own; this bounds the wait before killing it.
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const USAGE_REQUEST: &str = "hub-usage";
const INITIALIZE_REQUEST: &str = "hub-usage-initialize";
const LINE_LIMIT: usize = 1024 * 1024;
const LINES_LIMIT: usize = 64;
const PROBE_DIR_PREFIX: &str = "agent-hub-usage-";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    #[error("Claude отклонил запрос лимитов: {0}")]
    Rejected(String),
    #[error("непонятный ответ Claude о лимитах")]
    Shape,
    #[error("проба лимитов Claude отключена: CLI не отвечает без сообщения")]
    Disabled,
    #[error("не удалось запустить Claude: {0}")]
    Spawn(io::ErrorKind),
    #[error("Claude не ответил о лимитах за {} с", ANSWER_TIMEOUT.as_secs())]
    Timeout,
    #[error("Claude завершился, не ответив о лимитах")]
    NoAnswer,
    #[error("ответ Claude о лимитах длиннее {} КиБ", LINE_LIMIT / 1024)]
    TooLarge,
    #[error("ошибка обмена с Claude: {0}")]
    Io(io::ErrorKind),
}

impl ProbeError {
    /// What goes to the log: never the agent's text.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Rejected(_) => "rejected",
            Self::Shape => "shape",
            Self::Disabled => "disabled",
            Self::Spawn(_) => "spawn",
            Self::Timeout => "timeout",
            Self::NoAnswer => "no_answer",
            Self::TooLarge => "too_large",
            Self::Io(_) => "io",
        }
    }
}

/// The control requests of one probe; `None` when the probe is off.
#[must_use]
pub fn requests(handshake: Handshake) -> Option<Vec<Value>> {
    let usage = outgoing::control_request(
        USAGE_REQUEST,
        json!({"subtype": "get_usage", "skip_behaviors": true}),
    );
    match handshake {
        Handshake::Direct => Some(vec![usage]),
        Handshake::InitializeFirst => {
            Some(vec![outgoing::control_request(INITIALIZE_REQUEST, outgoing::initialize()), usage])
        }
        Handshake::CacheOnly => None,
    }
}

/// Asks a short-lived CLI for the plan limits. It never gets a prompt, so it starts no turn
/// and writes no session; its stderr is dropped, so nothing of it reaches the log.
///
/// # Errors
/// `Disabled` when the probe is off, `Spawn` when its directory or the CLI cannot be made,
/// the exchange's errors otherwise.
pub async fn probe(cli: &Path) -> Result<QuotaReport, ProbeError> {
    let requests = requests(HANDSHAKE).ok_or(ProbeError::Disabled)?;
    // Not the shared temp dir: nobody else can plant `.claude/` or `.mcp.json` in this one.
    let workdir = private_dir().map_err(|error| ProbeError::Spawn(error.kind()))?;
    let answer = run(cli, workdir.path(), &requests).await;
    // Only now: the child has exited or been killed, so nothing runs in the directory.
    if let Err(error) = workdir.close() {
        tracing::debug!(error = ?error.kind(), "claude usage probe directory not removed");
    }
    answer
}

async fn run(cli: &Path, workdir: &Path, requests: &[Value]) -> Result<QuotaReport, ProbeError> {
    let mut child =
        probe_command(cli, workdir).spawn().map_err(|error| ProbeError::Spawn(error.kind()))?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(ProbeError::Io(io::ErrorKind::BrokenPipe));
    };
    let answer = exchange(stdout, stdin, requests, ANSWER_TIMEOUT).await;
    if tokio::time::timeout(EXIT_TIMEOUT, child.wait()).await.is_err()
        && let Err(error) = child.kill().await
    {
        tracing::warn!(error = ?error.kind(), "claude usage probe could not be killed");
    }
    answer
}

/// A fresh directory only this user can enter (0700 on Unix; on Windows the per-user temp
/// dir already is private); it is removed when dropped.
fn private_dir() -> io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(PROBE_DIR_PREFIX);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    builder.tempdir()
}

fn probe_command(cli: &Path, workdir: &Path) -> Command {
    let mut command = Command::new(cli);
    command
        .args(PROBE_ARGS)
        .current_dir(workdir)
        .env(ENTRYPOINT.0, ENTRYPOINT.1)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    hide_window(&mut command);
    command
}

/// Writes `requests`, reads at most `LINES_LIMIT` lines of at most `LINE_LIMIT` bytes for the
/// usage answer within `wait`, then closes `writer` whatever happened, so the CLI sees EOF.
///
/// # Errors
/// `Timeout`, `NoAnswer`, `TooLarge`, `Io`, or the reply's own error.
pub async fn exchange<R, W>(
    reader: R,
    mut writer: W,
    requests: &[Value],
    wait: Duration,
) -> Result<QuotaReport, ProbeError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let asked = async {
        for request in requests {
            let line = format!("{request}\n");
            writer
                .write_all(line.as_bytes())
                .await
                .map_err(|error| ProbeError::Io(error.kind()))?;
        }
        writer.flush().await.map_err(|error| ProbeError::Io(error.kind()))?;
        let mut lines =
            FramedRead::new(reader, LinesCodec::new_with_max_length(LINE_LIMIT)).take(LINES_LIMIT);
        while let Some(line) = lines.next().await {
            let line = line.map_err(|error| match error {
                LinesCodecError::MaxLineLengthExceeded => ProbeError::TooLarge,
                LinesCodecError::Io(error) => ProbeError::Io(error.kind()),
            })?;
            if let Some(reply) = usage_reply(&line) {
                return parse_reply(&reply);
            }
        }
        Err(ProbeError::NoAnswer)
    };
    let answer = tokio::time::timeout(wait, asked).await.unwrap_or(Err(ProbeError::Timeout));
    if let Err(error) = writer.shutdown().await {
        tracing::debug!(error = ?error.kind(), "claude usage probe stdin shutdown failed");
    }
    // Tokio's `ChildStdin::poll_shutdown` does not close the pipe (on Unix it is a no-op): the
    // CLI sees EOF only once the writer is dropped, here.
    drop(writer);
    answer
}

/// The `response` of the `control_response` that answers the usage request; other lines and
/// lines that are not JSON are skipped.
fn usage_reply(line: &str) -> Option<Value> {
    let mut message: Value = serde_json::from_str(line).ok()?;
    let answers = message.get("type").and_then(Value::as_str) == Some("control_response")
        && message.pointer("/response/request_id").and_then(Value::as_str) == Some(USAGE_REQUEST);
    if !answers {
        return None;
    }
    message.get_mut("response").map(Value::take)
}

#[derive(Deserialize)]
struct UsageBody {
    #[serde(default)]
    subscription_type: Option<String>,
    #[serde(default)]
    rate_limits_available: Option<bool>,
    #[serde(default)]
    rate_limits: Option<RateLimits>,
}

#[derive(Deserialize)]
struct RateLimits {
    #[serde(default)]
    five_hour: Option<Window>,
    #[serde(default)]
    seven_day: Option<Window>,
    #[serde(default)]
    seven_day_opus: Option<Window>,
    #[serde(default)]
    seven_day_sonnet: Option<Window>,
    #[serde(default)]
    seven_day_oauth_apps: Option<Window>,
    #[serde(default)]
    model_scoped: Option<Vec<ModelWindow>>,
    #[serde(default)]
    extra_usage: Option<Extra>,
}

#[derive(Deserialize)]
struct Window {
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct ModelWindow {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct Extra {
    #[serde(default)]
    is_enabled: Option<bool>,
    #[serde(default)]
    utilization: Option<f64>,
}

/// The reply to `get_usage`: the `response` object of its `control_response` line.
///
/// # Errors
/// `Rejected` when the CLI answered with an error, `Shape` when the reply is not a usage report.
pub fn parse_reply(reply: &Value) -> Result<QuotaReport, ProbeError> {
    match reply.get("subtype").and_then(Value::as_str) {
        Some("success") => {}
        Some("error") => {
            let error = reply
                .get("error")
                .and_then(Value::as_str)
                .and_then(Label::new)
                .map(|label| label.as_str().to_owned());
            return Err(ProbeError::Rejected(error.unwrap_or_default()));
        }
        Some(_) | None => return Err(ProbeError::Shape),
    }
    let body = reply.get("response").cloned().unwrap_or(Value::Null);
    let UsageBody { subscription_type, rate_limits_available, rate_limits } =
        serde_json::from_value(body).map_err(|_| ProbeError::Shape)?;
    match (rate_limits_available, rate_limits) {
        (Some(false), _) => Ok(QuotaReport::unavailable(BackendKind::Claude, NO_SUBSCRIPTION)),
        (Some(true) | None, None) => Err(ProbeError::Shape),
        (Some(true) | None, Some(limits)) => {
            let plan = subscription_type.as_deref().and_then(plan_name);
            Ok(QuotaReport::fresh(BackendKind::Claude, plan, windows(limits)))
        }
    }
}

fn windows(limits: RateLimits) -> Vec<UsageWindow> {
    let RateLimits {
        five_hour,
        seven_day,
        seven_day_opus,
        seven_day_sonnet,
        seven_day_oauth_apps,
        model_scoped,
        extra_usage,
    } = limits;
    let named = [
        (Some(WindowKind::FiveHour), five_hour),
        (Some(WindowKind::Week), seven_day),
        (Label::new("Opus").map(WindowKind::WeekModel), seven_day_opus),
        (Label::new("Sonnet").map(WindowKind::WeekModel), seven_day_sonnet),
        (Label::new(OAUTH_APPS).map(WindowKind::Other), seven_day_oauth_apps),
    ]
    .into_iter()
    .filter_map(|(kind, window)| {
        let Window { utilization, resets_at } = window?;
        usage_window(kind?, utilization, resets_at.as_deref())
    });
    let models = model_scoped.unwrap_or_default().into_iter().filter_map(|model| {
        let ModelWindow { display_name, utilization, resets_at } = model;
        let kind = WindowKind::WeekModel(Label::new(display_name.as_deref()?)?);
        usage_window(kind, utilization, resets_at.as_deref())
    });
    let extra = extra_usage
        .filter(|extra| extra.is_enabled == Some(true))
        .and_then(|extra| usage_window(WindowKind::Extra, extra.utilization, None));
    named.chain(models).chain(extra).collect()
}

fn usage_window(
    kind: WindowKind,
    utilization: Option<f64>,
    resets_at: Option<&str>,
) -> Option<UsageWindow> {
    let percent = Percent::new(utilization?)?;
    Some(UsageWindow { kind, percent, resets_at: resets_at.and_then(iso_time) })
}

fn iso_time(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw).ok().map(|time| time.with_timezone(&Utc))
}

/// `max` → `Max`.
fn plan_name(raw: &str) -> Option<Label> {
    let mut chars = raw.trim().chars();
    let first = chars.next()?;
    Label::new(&first.to_uppercase().chain(chars).collect::<String>())
}

/// The one window a `rate_limit_event` reports; `None` when it names no known window or no
/// usage (CLI 2.1.28x sends only `status`, see the fixtures).
#[must_use]
pub fn event_update(info: &RateLimitInfo) -> Option<QuotaUpdate> {
    let kind = match info.rate_limit_type.as_ref()?.as_str()? {
        "five_hour" => WindowKind::FiveHour,
        "seven_day" => WindowKind::Week,
        "seven_day_opus" => WindowKind::WeekModel(Label::new("Opus")?),
        "seven_day_sonnet" => WindowKind::WeekModel(Label::new("Sonnet")?),
        "seven_day_overage_included" => WindowKind::Other(Label::new(OVERAGE_INCLUDED)?),
        "overage" => WindowKind::Extra,
        _ => return None,
    };
    let raw = info.utilization.as_ref()?.as_f64()?;
    // The SDK leaves the unit undocumented: up to 1 is a fraction, above it percent. Exactly 1
    // reads as 100 %, never as 1 %.
    let value = if raw <= 1.0 { raw * 100.0 } else { raw };
    let resets_at = info.resets_at.as_ref().and_then(Value::as_i64).and_then(unix_time);
    let window = UsageWindow { kind, percent: Percent::new(value)?, resets_at };
    Some(QuotaUpdate::new(BackendKind::Claude, None, vec![window]))
}

fn unix_time(raw: i64) -> Option<DateTime<Utc>> {
    if raw > MILLIS_FROM {
        DateTime::from_timestamp_millis(raw)
    } else {
        DateTime::from_timestamp(raw, 0)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    const RECORDED: &str = include_str!("../tests/fixtures/get_usage.json");

    fn at(raw: &str) -> DateTime<Utc> {
        raw.parse().unwrap()
    }

    fn success(body: &Value) -> Value {
        json!({"subtype": "success", "request_id": "hub-usage", "response": body})
    }

    fn label(raw: &str) -> Label {
        Label::new(raw).unwrap()
    }

    fn model(raw: &str) -> WindowKind {
        WindowKind::WeekModel(label(raw))
    }

    fn window(kind: WindowKind, percent: i64, resets_at: Option<&str>) -> UsageWindow {
        UsageWindow { kind, percent: Percent::from_int(percent), resets_at: resets_at.map(at) }
    }

    fn fresh(plan: Option<&str>, windows: Vec<UsageWindow>) -> QuotaReport {
        QuotaReport::fresh(BackendKind::Claude, plan.map(label), windows)
    }

    #[test]
    fn the_recorded_reply_becomes_a_report() {
        let body: Value = serde_json::from_str(RECORDED).unwrap();
        assert_eq!(
            parse_reply(&success(&body)),
            Ok(fresh(
                Some("Max"),
                vec![
                    window(WindowKind::FiveHour, 62, Some("2026-10-16T14:30:00Z")),
                    window(WindowKind::Week, 18, Some("2026-10-19T06:00:00Z")),
                    window(model("Opus"), 9, None),
                    window(model("Fable"), 33, Some("2026-10-19T06:00:00Z")),
                ],
            ))
        );
    }

    #[rstest]
    #[case::api_key(
        json!({"subscription_type": null, "rate_limits_available": false, "rate_limits": null}),
        Ok(QuotaReport::unavailable(BackendKind::Claude, NO_SUBSCRIPTION))
    )]
    #[case::fields_missing(
        json!({"rate_limits": {"five_hour": {"utilization": 7.5}}}),
        Ok(fresh(None, vec![window(WindowKind::FiveHour, 8, None)]))
    )]
    #[case::all_windows_null(
        json!({"subscription_type": "pro", "rate_limits_available": true, "rate_limits": {"five_hour": null, "seven_day": {"utilization": null, "resets_at": null}}}),
        Ok(fresh(Some("Pro"), Vec::new()))
    )]
    #[case::bad_time_is_dropped(
        json!({"rate_limits": {"seven_day": {"utilization": 120, "resets_at": "tomorrow"}}}),
        Ok(fresh(None, vec![window(WindowKind::Week, 100, None)]))
    )]
    #[case::extra_usage_when_enabled(
        json!({"rate_limits": {"extra_usage": {"is_enabled": true, "utilization": 40.0}}}),
        Ok(fresh(None, vec![window(WindowKind::Extra, 40, None)]))
    )]
    #[case::null_model_name_skips_the_model(
        json!({"rate_limits": {"model_scoped": [{"display_name": null, "utilization": 5}, {"display_name": "Fable", "utilization": 6}]}}),
        Ok(fresh(None, vec![window(model("Fable"), 6, None)]))
    )]
    #[case::null_extra_flag_skips_extra_usage(
        json!({"rate_limits": {"five_hour": {"utilization": 3}, "extra_usage": {"is_enabled": null, "utilization": 40.0}}}),
        Ok(fresh(None, vec![window(WindowKind::FiveHour, 3, None)]))
    )]
    #[case::wrong_type(json!({"rate_limits": {"five_hour": {"utilization": "a lot"}}}), Err(ProbeError::Shape))]
    #[case::no_limits(json!({"rate_limits_available": true, "rate_limits": null}), Err(ProbeError::Shape))]
    #[case::not_an_object(json!("usage"), Err(ProbeError::Shape))]
    fn replies_are_read_tolerantly(
        #[case] body: Value,
        #[case] expected: Result<QuotaReport, ProbeError>,
    ) {
        assert_eq!(parse_reply(&success(&body)), expected);
    }

    #[rstest]
    #[case::error(
        json!({"subtype": "error", "request_id": "hub-usage", "error": "Unsupported control request subtype: get_usage\n"}),
        Err(ProbeError::Rejected("Unsupported control request sub…".to_owned()))
    )]
    #[case::unknown_subtype(json!({"subtype": "pending", "request_id": "hub-usage"}), Err(ProbeError::Shape))]
    fn refusals_are_errors(
        #[case] reply: Value,
        #[case] expected: Result<QuotaReport, ProbeError>,
    ) {
        assert_eq!(parse_reply(&reply), expected);
    }

    #[test]
    fn many_model_windows_are_capped() {
        let models: Vec<Value> = (0..20)
            .map(|i| json!({"display_name": format!("m{i}"), "utilization": 1, "resets_at": null}))
            .collect();
        let report =
            parse_reply(&success(&json!({"rate_limits": {"model_scoped": models}}))).unwrap();
        assert_eq!(report.windows().len(), hub_core::quota::WINDOW_LIMIT);
    }

    fn info(value: Value) -> RateLimitInfo {
        serde_json::from_value(value).unwrap()
    }

    fn update(kind: WindowKind, percent: i64, resets_at: Option<&str>) -> QuotaUpdate {
        QuotaUpdate::new(BackendKind::Claude, None, vec![window(kind, percent, resets_at)])
    }

    #[rstest]
    #[case::fraction(json!({"status": "allowed", "rateLimitType": "five_hour", "utilization": 0.62, "resetsAt": 1_792_160_000}), Some(update(WindowKind::FiveHour, 62, Some("2026-10-16T14:13:20Z"))))]
    #[case::percent(json!({"status": "allowed_warning", "rateLimitType": "seven_day", "utilization": 81}), Some(update(WindowKind::Week, 81, None)))]
    #[case::millis(json!({"rateLimitType": "seven_day_opus", "utilization": 0.5, "resetsAt": 1_792_160_000_000_i64}), Some(update(model("Opus"), 50, Some("2026-10-16T14:13:20Z"))))]
    #[case::fractional_reset(json!({"rateLimitType": "overage", "utilization": 0.1, "resetsAt": 1.5}), Some(update(WindowKind::Extra, 10, None)))]
    #[case::status_only(json!({"status": "allowed"}), None)]
    #[case::unknown_window(json!({"rateLimitType": "one_day", "utilization": 0.1}), None)]
    #[case::bad_utilization(json!({"rateLimitType": "five_hour", "utilization": "high"}), None)]
    fn rate_limit_events_update_one_window(
        #[case] raw: Value,
        #[case] expected: Option<QuotaUpdate>,
    ) {
        assert_eq!(event_update(&info(raw)), expected);
    }

    #[rstest]
    #[case::fraction(0.62, 62)]
    #[case::percent(62.0, 62)]
    #[case::whole_fraction(1.0, 100)]
    #[case::zero(0.0, 0)]
    #[case::just_above_one(1.5, 2)]
    fn event_utilization_is_read_in_either_unit(#[case] utilization: f64, #[case] percent: i64) {
        let raw = json!({"rateLimitType": "five_hour", "utilization": utilization});
        assert_eq!(event_update(&info(raw)), Some(update(WindowKind::FiveHour, percent, None)));
    }

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

    const WAIT: Duration = Duration::from_secs(5);

    fn usage_request() -> Value {
        json!({"type": "control_request", "request_id": "hub-usage",
               "request": {"subtype": "get_usage", "skip_behaviors": true}})
    }

    fn answer(body: &Value) -> String {
        json!({"type": "control_response",
               "response": {"subtype": "success", "request_id": "hub-usage", "response": body}})
        .to_string()
    }

    enum Then {
        /// Keep stdout open: the probe must stop on its own.
        Wait,
        /// Close stdout, as an exiting CLI does.
        Exit,
    }

    /// The CLI side: reads `expect` request lines, writes `lines`, then checks that the probe
    /// closes stdin.
    async fn fake_cli(
        theirs: DuplexStream,
        expect: usize,
        lines: Vec<String>,
        then: Then,
    ) -> Vec<Value> {
        let (read, mut write) = tokio::io::split(theirs);
        let mut input = BufReader::new(read).lines();
        let mut seen = Vec::new();
        for _ in 0..expect {
            let line = input.next_line().await.unwrap().unwrap();
            seen.push(serde_json::from_str(&line).unwrap());
        }
        for line in lines {
            write.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        }
        match then {
            Then::Wait => {}
            Then::Exit => write.shutdown().await.unwrap(),
        }
        assert_eq!(input.next_line().await.unwrap(), None, "the probe must close stdin");
        seen
    }

    async fn run(
        handshake: Handshake,
        lines: Vec<String>,
        then: Then,
        wait: Duration,
    ) -> (Result<QuotaReport, ProbeError>, Vec<Value>) {
        let requests = requests(handshake).unwrap();
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        let cli = tokio::spawn(fake_cli(theirs, requests.len(), lines, then));
        let (read, write) = tokio::io::split(ours);
        let outcome = exchange(read, write, &requests, wait).await;
        (outcome, cli.await.unwrap())
    }

    fn recorded() -> Value {
        serde_json::from_str(RECORDED).unwrap()
    }

    #[tokio::test]
    async fn initialize_first_then_the_answer_among_other_lines() {
        let lines = vec![
            json!({"type": "control_response", "response": {"subtype": "success", "request_id": "hub-usage-initialize", "response": {}}}).to_string(),
            "not json".to_owned(),
            json!({"type": "system", "subtype": "status"}).to_string(),
            answer(&recorded()),
        ];
        let (outcome, seen) = run(Handshake::InitializeFirst, lines, Then::Wait, WAIT).await;
        let report = outcome.unwrap();
        assert_eq!(report.plan.as_ref().map(Label::as_str), Some("Max"));
        assert_eq!(
            seen,
            [
                json!({"type": "control_request", "request_id": "hub-usage-initialize",
                       "request": {"subtype": "initialize", "hooks": null}}),
                usage_request(),
            ]
        );
    }

    #[tokio::test]
    async fn direct_sends_only_get_usage_and_never_a_prompt() {
        let (outcome, seen) =
            run(Handshake::Direct, vec![answer(&recorded())], Then::Wait, WAIT).await;
        assert!(outcome.is_ok());
        assert_eq!(seen, [usage_request()]);
    }

    #[test]
    fn cache_only_runs_no_cli() {
        assert_eq!(requests(Handshake::CacheOnly), None);
    }

    #[tokio::test]
    async fn an_unknown_shape_is_an_error() {
        let lines = vec![answer(&json!({"rate_limits": 5}))];
        let (outcome, _seen) = run(Handshake::Direct, lines, Then::Wait, WAIT).await;
        assert_eq!(outcome, Err(ProbeError::Shape));
    }

    #[tokio::test]
    async fn silence_times_out_and_closes_stdin() {
        let (outcome, _seen) =
            run(Handshake::Direct, Vec::new(), Then::Wait, Duration::from_millis(100)).await;
        assert_eq!(outcome, Err(ProbeError::Timeout));
    }

    #[tokio::test]
    async fn exit_without_an_answer() {
        let lines =
            vec![json!({"type": "system", "subtype": "init", "session_id": "s"}).to_string()];
        let (outcome, _seen) = run(Handshake::Direct, lines, Then::Exit, WAIT).await;
        assert_eq!(outcome, Err(ProbeError::NoAnswer));
    }

    #[tokio::test]
    async fn too_many_lines_without_an_answer() {
        let lines = vec![json!({"type": "system", "subtype": "status"}).to_string(); 65];
        let (outcome, _seen) = run(Handshake::Direct, lines, Then::Wait, WAIT).await;
        assert_eq!(outcome, Err(ProbeError::NoAnswer));
    }

    #[tokio::test]
    async fn a_huge_line_is_refused() {
        let lines = vec!["x".repeat(1024 * 1024 + 1)];
        let (outcome, _seen) = run(Handshake::Direct, lines, Then::Exit, WAIT).await;
        assert_eq!(outcome, Err(ProbeError::TooLarge));
    }

    #[test]
    fn the_probe_loads_no_settings_and_no_mcp_servers() {
        assert!(PROBE_ARGS.contains(&"--setting-sources="));
        assert!(PROBE_ARGS.contains(&"--strict-mcp-config"));
        assert!(
            !PROBE_ARGS.iter().any(|arg| arg.starts_with("--resume") || *arg == "--mcp-config")
        );
    }

    #[test]
    fn the_probe_runs_in_its_own_fresh_directory() {
        let first = private_dir().unwrap();
        let second = private_dir().unwrap();
        assert_ne!(first.path(), second.path());
        assert!(first.path().is_dir());
        assert!(first.path().starts_with(std::env::temp_dir()));
        assert_ne!(first.path(), std::env::temp_dir());

        let command = probe_command(Path::new("claude"), first.path());
        let command = command.as_std();
        assert_eq!(command.get_current_dir(), Some(first.path()));
        assert_eq!(command.get_args().collect::<Vec<_>>(), PROBE_ARGS);

        let path = first.path().to_owned();
        drop(first);
        assert!(!path.exists(), "the directory goes away with the probe");
    }

    #[cfg(unix)]
    #[test]
    fn the_probe_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = private_dir().unwrap();
        let mode = std::fs::metadata(dir.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[tokio::test]
    async fn a_missing_cli_is_a_spawn_error() {
        let outcome = probe(&std::env::temp_dir().join("definitely-not-claude")).await;
        // `Disabled` if the live check turned the probe off.
        assert!(matches!(outcome, Err(ProbeError::Spawn(_) | ProbeError::Disabled)), "{outcome:?}");
    }
}
