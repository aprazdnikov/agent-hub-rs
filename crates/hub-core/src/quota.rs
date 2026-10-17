//! Subscription limits the agents report, as plain data for the `/limits` card.
//!
//! Strings come from the agents' answers: parsers pass them through `clean_label`, and
//! `render::render_limits` escapes them.

use chrono::{DateTime, Utc};

use crate::domain::BackendKind;
use crate::render::truncate;

/// Longest plan, window name or refusal taken from an agent's answer.
pub const LABEL_LIMIT: usize = 32;
/// Longest refusal text taken from an agent's answer.
pub const REASON_LIMIT: usize = 160;
/// Most windows kept per backend, so the card always fits one Telegram message.
pub const WINDOW_LIMIT: usize = 8;

/// Shown when an agent's refusal text is empty once cleaned.
const UNKNOWN_REASON: &str = "причина не указана";

/// Agent text fit for a card: no control characters, trimmed, non-empty, at most
/// `LABEL_LIMIT` chars.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Label(String);

impl Label {
    /// `None` when nothing printable is left.
    #[must_use]
    pub fn new(raw: &str) -> Option<Self> {
        Self::bounded(raw, LABEL_LIMIT)
    }

    fn bounded(raw: &str, limit: usize) -> Option<Self> {
        let printable: String = raw.chars().filter(|c| !c.is_control()).collect();
        let trimmed = printable.trim();
        (!trimmed.is_empty()).then(|| Self(truncate(trimmed, limit)))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A usage percentage, always within 0–100.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Percent(u8);

impl Percent {
    /// Clamped to 0–100 and rounded; `None` for NaN and infinities.
    #[must_use]
    pub fn new(raw: f64) -> Option<Self> {
        if !raw.is_finite() {
            return None;
        }
        let rounded = raw.clamp(0.0, 100.0).round();
        // No float-to-int cast: the lints deny them, and 101 steps are nothing.
        (0..=100_u8).find(|step| f64::from(*step) >= rounded).map(Self)
    }

    /// An integer percentage clamped to 0–100.
    #[must_use]
    pub fn from_int(raw: i64) -> Self {
        Self(u8::try_from(raw.clamp(0, 100)).unwrap_or(100))
    }

    #[must_use]
    pub fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WindowKind {
    FiveHour,
    Week,
    /// A weekly window of one model, e.g. `Opus`.
    WeekModel(Label),
    /// Usage beyond the plan, paid separately.
    Extra,
    /// A window shown under the given name, e.g. Codex `основное`.
    Other(Label),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageWindow {
    pub kind: WindowKind,
    pub percent: Percent,
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
    Unavailable(Label),
}

fn capped(windows: Vec<UsageWindow>) -> Vec<UsageWindow> {
    windows.into_iter().take(WINDOW_LIMIT).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaReport {
    pub backend: BackendKind,
    pub plan: Option<Label>,
    windows: Vec<UsageWindow>,
    pub source: QuotaSource,
}

impl QuotaReport {
    #[must_use]
    pub fn fresh(backend: BackendKind, plan: Option<Label>, windows: Vec<UsageWindow>) -> Self {
        Self { backend, plan, windows: capped(windows), source: QuotaSource::Fresh }
    }

    /// The last values live sessions reported, as of `at`.
    #[must_use]
    pub fn cached(
        backend: BackendKind,
        plan: Option<Label>,
        windows: Vec<UsageWindow>,
        at: DateTime<Utc>,
    ) -> Self {
        Self { backend, plan, windows: capped(windows), source: QuotaSource::Cached { at } }
    }

    #[must_use]
    pub fn not_reported(backend: BackendKind) -> Self {
        Self { backend, plan: None, windows: Vec::new(), source: QuotaSource::NotReported }
    }

    /// `reason` is cleaned like any agent text and bounded by `REASON_LIMIT`.
    #[must_use]
    pub fn unavailable(backend: BackendKind, reason: &str) -> Self {
        let label = Label::bounded(reason, REASON_LIMIT)
            .unwrap_or_else(|| Label(UNKNOWN_REASON.to_owned()));
        Self { backend, plan: None, windows: Vec::new(), source: QuotaSource::Unavailable(label) }
    }

    /// At most `WINDOW_LIMIT` windows.
    #[must_use]
    pub fn windows(&self) -> &[UsageWindow] {
        &self.windows
    }
}

/// A rolling update from a live session: the windows it names replace the cached ones, a
/// missing plan keeps the cached plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaUpdate {
    pub backend: BackendKind,
    pub plan: Option<Label>,
    windows: Vec<UsageWindow>,
}

impl QuotaUpdate {
    #[must_use]
    pub fn new(backend: BackendKind, plan: Option<Label>, windows: Vec<UsageWindow>) -> Self {
        Self { backend, plan, windows: capped(windows) }
    }

    /// At most `WINDOW_LIMIT` windows.
    #[must_use]
    pub fn windows(&self) -> &[UsageWindow] {
        &self.windows
    }
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
        assert_eq!(Label::new(raw).as_ref().map(Label::as_str), expected);
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
        assert_eq!(Percent::new(value).map(Percent::get), expected);
    }

    #[rstest]
    #[case(-5, 0)]
    #[case(0, 0)]
    #[case(42, 42)]
    #[case(100, 100)]
    #[case(1_000, 100)]
    #[case(i64::MAX, 100)]
    #[case(i64::MIN, 0)]
    fn integer_percent_is_clamped(#[case] raw: i64, #[case] expected: u8) {
        assert_eq!(Percent::from_int(raw).get(), expected);
    }

    fn window(name: &str) -> UsageWindow {
        UsageWindow {
            kind: WindowKind::Other(Label::new(name).unwrap()),
            percent: Percent::from_int(1),
            resets_at: None,
        }
    }

    fn many() -> Vec<UsageWindow> {
        (0..12).map(|i| window(&format!("w{i}"))).collect()
    }

    #[test]
    fn a_fresh_report_keeps_at_most_eight_windows() {
        let report = QuotaReport::fresh(BackendKind::Claude, None, many());
        assert_eq!(report.windows().len(), WINDOW_LIMIT);
        assert_eq!(report.source, QuotaSource::Fresh);
    }

    #[test]
    fn a_cached_report_keeps_at_most_eight_windows() {
        let at = DateTime::<Utc>::UNIX_EPOCH;
        let report = QuotaReport::cached(BackendKind::Claude, None, many(), at);
        assert_eq!(report.windows().len(), WINDOW_LIMIT);
        assert_eq!(report.source, QuotaSource::Cached { at });
    }

    #[test]
    fn an_update_keeps_at_most_eight_windows() {
        let update = QuotaUpdate::new(BackendKind::Claude, None, many());
        assert_eq!(update.windows().len(), WINDOW_LIMIT);
    }

    #[test]
    fn a_refusal_is_cleaned_and_bounded() {
        let raw = format!("\u{1b}[31m{}\n", "я".repeat(200));
        let report = QuotaReport::unavailable(BackendKind::Codex, &raw);
        let QuotaSource::Unavailable(reason) = report.source else {
            panic!("expected Unavailable");
        };
        assert_eq!(reason.as_str().chars().count(), REASON_LIMIT);
        assert!(!reason.as_str().chars().any(char::is_control));
    }

    #[test]
    fn a_spec_length_refusal_survives_whole() {
        let raw = "вход по API-ключу — лимитов подписки нет";
        let report = QuotaReport::unavailable(BackendKind::Codex, raw);
        assert_eq!(report.source, QuotaSource::Unavailable(Label(raw.to_owned())));
    }

    #[test]
    fn an_empty_refusal_gets_the_fallback_text() {
        let report = QuotaReport::unavailable(BackendKind::Codex, " \n ");
        assert_eq!(report.source, QuotaSource::Unavailable(Label(UNKNOWN_REASON.to_owned())));
    }

    #[test]
    fn an_empty_refusal_gets_a_fixed_text() {
        let report = QuotaReport::unavailable(BackendKind::Codex, " \u{7}\n");
        assert_eq!(report.source, QuotaSource::Unavailable(Label(UNKNOWN_REASON.to_owned())));
    }
}
