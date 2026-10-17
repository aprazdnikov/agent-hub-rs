//! Subscription limits the agents report, as plain data for the `/limits` card.
//!
//! Strings come from the agents' answers: parsers pass them through `clean_label`, and
//! `render::render_limits` escapes them.

use chrono::{DateTime, Utc};

use crate::domain::BackendKind;
use crate::render::truncate;

/// Longest plan, window name or refusal taken from an agent's answer.
pub const LABEL_LIMIT: usize = 32;
/// Most windows kept per backend, so the card always fits one Telegram message.
pub const WINDOW_LIMIT: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WindowKind {
    FiveHour,
    Week,
    /// A weekly window of one model, e.g. `Opus`.
    WeekModel(String),
    /// Usage beyond the plan, paid separately.
    Extra,
    /// A window shown under the given name, e.g. Codex `основное`.
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageWindow {
    pub kind: WindowKind,
    /// 0–100.
    pub percent: u8,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaSource {
    /// Just asked the agent.
    Fresh,
    /// The last values live sessions reported, as of `at`.
    Cached { at: DateTime<Utc> },
    /// The agent's protocol has no subscription limits.
    NotReported,
    /// Nothing to show; the reason is shown instead.
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaReport {
    pub backend: BackendKind,
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub source: QuotaSource,
}

impl QuotaReport {
    #[must_use]
    pub fn fresh(backend: BackendKind, plan: Option<String>, windows: Vec<UsageWindow>) -> Self {
        let windows = windows.into_iter().take(WINDOW_LIMIT).collect();
        Self { backend, plan, windows, source: QuotaSource::Fresh }
    }

    #[must_use]
    pub fn not_reported(backend: BackendKind) -> Self {
        Self { backend, plan: None, windows: Vec::new(), source: QuotaSource::NotReported }
    }

    #[must_use]
    pub fn unavailable(backend: BackendKind, reason: impl Into<String>) -> Self {
        let source = QuotaSource::Unavailable(reason.into());
        Self { backend, plan: None, windows: Vec::new(), source }
    }
}

/// A rolling update from a live session: the windows it names replace the cached ones, a
/// missing plan keeps the cached plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaUpdate {
    pub backend: BackendKind,
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
}

/// Agent text fit for a card: no control characters, trimmed, at most `LABEL_LIMIT` chars.
#[must_use]
pub fn clean_label(raw: &str) -> Option<String> {
    let printable: String = raw.chars().filter(|c| !c.is_control()).collect();
    let trimmed = printable.trim();
    (!trimmed.is_empty()).then(|| truncate(trimmed, LABEL_LIMIT))
}

/// A percentage clamped to 0–100 and rounded; `None` for NaN and infinities.
#[must_use]
pub fn percent(value: f64) -> Option<u8> {
    if !value.is_finite() {
        return None;
    }
    let rounded = value.clamp(0.0, 100.0).round();
    // No float-to-int cast: the lints deny them, and 101 steps are nothing.
    (0..=100_u8).find(|step| f64::from(*step) >= rounded)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain("Max", Some("Max"))]
    #[case::trimmed("  Fable  ", Some("Fable"))]
    #[case::controls("Op\u{1b}[31mus\n", Some("Op[31mus"))]
    #[case::empty(" \t ", None)]
    #[case::long("абвгдежзийклмнопрстуфхцчшщъыьэюя-ещё", Some("абвгдежзийклмнопрстуфхцчшщъыьэю…"))]
    fn labels_are_cleaned_and_bounded(#[case] raw: &str, #[case] expected: Option<&str>) {
        assert_eq!(clean_label(raw).as_deref(), expected);
    }

    #[rstest]
    #[case(0.0, Some(0))]
    #[case(62.4, Some(62))]
    #[case(99.5, Some(100))]
    #[case(-3.0, Some(0))]
    #[case(250.0, Some(100))]
    #[case(f64::NAN, None)]
    #[case(f64::INFINITY, None)]
    fn percent_is_clamped_and_rounded(#[case] value: f64, #[case] expected: Option<u8>) {
        assert_eq!(percent(value), expected);
    }

    #[test]
    fn a_fresh_report_keeps_at_most_eight_windows() {
        let windows = (0..12)
            .map(|i| UsageWindow {
                kind: WindowKind::Other(format!("w{i}")),
                percent: 1,
                resets_at: None,
            })
            .collect();
        let report = QuotaReport::fresh(BackendKind::Claude, None, windows);
        assert_eq!(report.windows.len(), WINDOW_LIMIT);
        assert_eq!(report.source, QuotaSource::Fresh);
    }
}
