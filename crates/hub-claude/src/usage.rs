//! Subscription limits from the Claude CLI: the `get_usage` control reply and
//! `rate_limit_event`. Both are untrusted: strings pass `Label`, numbers `Percent`.

use chrono::{DateTime, Utc};
use hub_core::domain::BackendKind;
use hub_core::quota::{Label, Percent, QuotaReport, QuotaUpdate, UsageWindow, WindowKind};
use serde::Deserialize;
use serde_json::Value;

use crate::wire::RateLimitInfo;

pub const NO_SUBSCRIPTION: &str = "вход не через подписку claude.ai — лимитов подписки нет";
const OAUTH_APPS: &str = "7 дней, приложения";
const OVERAGE_INCLUDED: &str = "7 дней, включённые модели";
/// `resetsAt` above this is in milliseconds: in seconds it stays below until the year 5138.
const MILLIS_FROM: i64 = 100_000_000_000;

/// The unit of `utilization` in `rate_limit_event`, which the SDK leaves undocumented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    Fraction,
    Percent,
}

/// What the live check found (task 1 of the plan); `get_usage` is 0–100 by the SDK types.
pub const EVENT_SCALE: Scale = Scale::Fraction;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    #[error("Claude отклонил запрос лимитов: {0}")]
    Rejected(String),
    #[error("непонятный ответ Claude о лимитах")]
    Shape,
}

impl ProbeError {
    /// What goes to the log: never the agent's text.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Rejected(_) => "rejected",
            Self::Shape => "shape",
        }
    }
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
    display_name: String,
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct Extra {
    #[serde(default)]
    is_enabled: bool,
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
        let kind = WindowKind::WeekModel(Label::new(&display_name)?);
        usage_window(kind, utilization, resets_at.as_deref())
    });
    let extra = extra_usage
        .filter(|extra| extra.is_enabled)
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
pub fn event_update(info: &RateLimitInfo, scale: Scale) -> Option<QuotaUpdate> {
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
    let value = match scale {
        Scale::Fraction => raw * 100.0,
        Scale::Percent => raw,
    };
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
    #[case::fraction(json!({"status": "allowed", "rateLimitType": "five_hour", "utilization": 0.62, "resetsAt": 1_792_160_000}), Scale::Fraction, Some(update(WindowKind::FiveHour, 62, Some("2026-10-16T14:13:20Z"))))]
    #[case::percent(json!({"status": "allowed_warning", "rateLimitType": "seven_day", "utilization": 81}), Scale::Percent, Some(update(WindowKind::Week, 81, None)))]
    #[case::millis(json!({"rateLimitType": "seven_day_opus", "utilization": 0.5, "resetsAt": 1_792_160_000_000_i64}), Scale::Fraction, Some(update(model("Opus"), 50, Some("2026-10-16T14:13:20Z"))))]
    #[case::fractional_reset(json!({"rateLimitType": "overage", "utilization": 0.1, "resetsAt": 1.5}), Scale::Fraction, Some(update(WindowKind::Extra, 10, None)))]
    #[case::status_only(json!({"status": "allowed"}), Scale::Fraction, None)]
    #[case::unknown_window(json!({"rateLimitType": "one_day", "utilization": 0.1}), Scale::Fraction, None)]
    #[case::bad_utilization(json!({"rateLimitType": "five_hour", "utilization": "high"}), Scale::Fraction, None)]
    fn rate_limit_events_update_one_window(
        #[case] raw: Value,
        #[case] scale: Scale,
        #[case] expected: Option<QuotaUpdate>,
    ) {
        assert_eq!(event_update(&info(raw), scale), expected);
    }
}
