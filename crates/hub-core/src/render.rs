use std::fmt::Display;
use std::num::NonZeroUsize;
use std::time::Duration;

use chrono::{DateTime, Datelike, TimeZone, Utc, Weekday};
use rust_decimal::Decimal;

use crate::domain::{BackendKind, Finished, Usage};
use crate::escape::escape;
use crate::quota::{QuotaReport, QuotaSource, UsageWindow, WindowKind};

// `NonZeroUsize::new(..).unwrap()` is not allowed by the lints; 1 + 4095 is const and total.
pub const TELEGRAM_TEXT_LIMIT: NonZeroUsize = NonZeroUsize::MIN.saturating_add(4095);

#[must_use]
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// Byte offset of the `n`-th character, or the end of `text`.
fn char_offset(text: &str, n: usize) -> usize {
    text.char_indices().nth(n).map_or(text.len(), |(offset, _)| offset)
}

/// Split `text` into chunks of at most `limit` chars, preferring line boundaries.
#[must_use]
pub fn split_message(text: &str, limit: NonZeroUsize) -> Vec<String> {
    let limit = limit.get();
    let mut chunks = Vec::new();
    let mut rest = text.trim();
    while char_len(rest) > limit {
        let (window, _) = rest.split_at(char_offset(rest, limit + 1));
        let cut = match window.rfind('\n') {
            Some(newline) if newline > 0 => newline,
            Some(_) | None => char_offset(rest, limit),
        };
        let (head, tail) = rest.split_at(cut);
        chunks.push(head.trim_end().to_owned());
        rest = tail.trim_start_matches('\n');
    }
    if !rest.is_empty() {
        chunks.push(rest.to_owned());
    }
    chunks
}

#[must_use]
pub fn truncate(text: &str, limit: usize) -> String {
    if char_len(text) <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[must_use]
pub fn format_finished(finished: &Finished) -> String {
    let usage = match &finished.usage {
        Usage::Claude { turns, cost } => format!(" · ходов: {turns}{}", cost_text(*cost)),
        Usage::Codex { tokens } | Usage::Qwen { tokens } | Usage::Hermes { tokens } => tokens
            .map_or_else(String::new, |tokens| {
                format!(" · токенов в сессии: {}", group_thousands(tokens))
            }),
    };
    let pending = if finished.background == 0 {
        String::new()
    } else {
        format!(" · ⏳ в фоне задач: {}, пришлю результат", finished.background)
    };
    format!("✅ Готово{usage}{pending}")
}

fn cost_text(cost: Option<Decimal>) -> String {
    cost.map_or_else(String::new, |cost| {
        // Banker's rounding, as Python's Decimal.quantize, then always two decimals.
        let mut cents = cost.round_dp(2);
        cents.rescale(2);
        format!(" · ${cents}")
    })
}

/// `12345` → `12 345`, as Python's `f"{n:_}".replace("_", " ")`.
fn group_thousands(number: u64) -> String {
    let digits = number.to_string();
    let len = digits.len();
    digits.chars().enumerate().fold(
        String::with_capacity(len + len / 3),
        |mut out, (index, digit)| {
            if index > 0 && (len - index).is_multiple_of(3) {
                out.push(' ');
            }
            out.push(digit);
            out
        },
    )
}

#[must_use]
pub fn format_abandoned(tasks: &[String], timeout: Duration) -> String {
    let listing = tasks.iter().map(|task| format!("• {task}")).collect::<Vec<_>>().join("\n");
    format!(
        "⌛ Фоновые задачи не завершились за {} мин, сессия закрыта:\n{listing}",
        timeout.as_secs() / 60
    )
}

pub const LIMITS_TITLE: &str = "📊 <b>Лимиты подписок</b>";
const NOT_REPORTED: &str = "квоты подписки агент не сообщает";
const NO_AGENTS: &str = "Ни один CLI агента не найден";
const NO_DATA: &str = "  нет данных";
const BAR_CELLS: usize = 10;

/// The `/limits` card as Telegram HTML, times in `tz`; every string from the agents escaped.
#[must_use]
pub fn render_limits<Tz>(reports: &[QuotaReport], now: DateTime<Utc>, tz: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let now = now.with_timezone(tz);
    let header = format!("{LIMITS_TITLE} · {}", now.format("%d.%m %H:%M"));
    let silent: Vec<&str> = reports
        .iter()
        .filter(|report| matches!(report.source, QuotaSource::NotReported))
        .map(|report| backend_title(report.backend))
        .collect();
    let blocks = reports
        .iter()
        .filter(|report| !matches!(report.source, QuotaSource::NotReported))
        .flat_map(|report| report_lines(report, &now));
    let tail = match (reports.is_empty(), silent.is_empty()) {
        (true, _) => Some(NO_AGENTS.to_owned()),
        (false, true) => None,
        (false, false) => Some(format!("{} — {NOT_REPORTED}", silent.join(", "))),
    };
    std::iter::once(header).chain(blocks).chain(tail).collect::<Vec<_>>().join("\n")
}

fn report_lines<Tz>(report: &QuotaReport, now: &DateTime<Tz>) -> Vec<String>
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let plan = report
        .plan
        .as_ref()
        .map_or_else(String::new, |plan| format!(" ({})", escape(plan.as_str())));
    let head = format!("<b>{}</b>{plan}", backend_title(report.backend));
    let head = match &report.source {
        QuotaSource::Unavailable(reason) => {
            return vec![format!("{head} ⚠️ {}", escape(reason.as_str()))];
        }
        QuotaSource::Cached { at } => {
            format!("{head} (на {})", moment(&at.with_timezone(&now.timezone()), now))
        }
        QuotaSource::Fresh | QuotaSource::NotReported => head,
    };
    let windows = report.windows();
    if windows.is_empty() {
        return vec![head, NO_DATA.to_owned()];
    }
    let width =
        windows.iter().map(|window| label(&window.kind).chars().count()).max().unwrap_or_default();
    std::iter::once(head)
        .chain(windows.iter().map(|window| window_line(window, width, now)))
        .collect()
}

fn window_line<Tz>(window: &UsageWindow, width: usize, now: &DateTime<Tz>) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let percent = window.percent.get();
    let filled = usize::from(percent.saturating_add(5) / 10).min(BAR_CELLS);
    let bar = format!("{}{}", "▓".repeat(filled), "░".repeat(BAR_CELLS - filled));
    let name = label(&window.kind);
    let pad = " ".repeat(width.saturating_sub(name.chars().count()));
    let reset = window.resets_at.map_or_else(String::new, |at| {
        format!(" · сброс {}", reset_text(&at.with_timezone(&now.timezone()), now))
    });
    format!("<code>  {}{pad} {bar} {percent:>2}%</code>{reset}", escape(&name))
}

fn reset_text<Tz>(at: &DateTime<Tz>, now: &DateTime<Tz>) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    match at.date_naive().signed_duration_since(now.date_naive()).num_days() {
        0 => format!("в {}", at.format("%H:%M")),
        1..=6 => format!("{} {}", weekday(at.weekday()), at.format("%H:%M")),
        _ => at.format("%d.%m %H:%M").to_string(),
    }
}

fn moment<Tz>(at: &DateTime<Tz>, now: &DateTime<Tz>) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let pattern = if at.date_naive() == now.date_naive() { "%H:%M" } else { "%d.%m %H:%M" };
    at.format(pattern).to_string()
}

const fn weekday(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "пн",
        Weekday::Tue => "вт",
        Weekday::Wed => "ср",
        Weekday::Thu => "чт",
        Weekday::Fri => "пт",
        Weekday::Sat => "сб",
        Weekday::Sun => "вс",
    }
}

const fn backend_title(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Claude => "Claude",
        BackendKind::Codex => "Codex",
        BackendKind::Qwen => "Qwen",
        BackendKind::Hermes => "Hermes",
    }
}

fn label(kind: &WindowKind) -> String {
    match kind {
        WindowKind::FiveHour => "5 часов".to_owned(),
        WindowKind::Week => "7 дней".to_owned(),
        WindowKind::WeekModel(model) => format!("7 дней {}", model.as_str()),
        WindowKind::Extra => "сверх подписки".to_owned(),
        WindowKind::Other(name) => name.as_str().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use chrono::FixedOffset;
    use rstest::rstest;
    use rust_decimal::Decimal;

    use super::*;
    use crate::domain::SessionId;
    use crate::quota::{Label, Percent};

    fn limit(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(split_message("  hello  ", TELEGRAM_TEXT_LIMIT), ["hello"]);
    }

    #[test]
    fn empty_text_has_no_chunks() {
        assert!(split_message("   ", TELEGRAM_TEXT_LIMIT).is_empty());
    }

    #[test]
    fn long_text_splits_on_line_boundary() {
        assert_eq!(split_message("aaaa\nbbbb\ncccc", limit(9)), ["aaaa\nbbbb", "cccc"]);
    }

    #[test]
    fn line_longer_than_limit_is_hard_split() {
        assert_eq!(split_message("abcdefgh", limit(3)), ["abc", "def", "gh"]);
    }

    #[test]
    fn every_chunk_respects_limit() {
        let text = (1..60).map(|n| "x".repeat(n)).collect::<Vec<_>>().join("\n");
        let chunks = split_message(&text, limit(50));
        assert!(chunks.iter().all(|chunk| char_len(chunk) <= 50));
        assert_eq!(chunks.concat().replace('\n', ""), text.replace('\n', ""));
    }

    #[test]
    fn split_message_respects_char_boundaries() {
        assert_eq!(split_message("абвгдеё", limit(3)), ["абв", "где", "ё"]);
    }

    #[test]
    fn truncate_marks_cut() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(truncate("абвгд", 3), "аб…");
    }

    fn claude(turns: u32, cost: Option<&str>, background: usize) -> Finished {
        Finished {
            session: SessionId::parse("s").unwrap(),
            usage: Usage::Claude { turns, cost: cost.map(|raw| raw.parse::<Decimal>().unwrap()) },
            background,
        }
    }

    fn codex(tokens: Option<u64>) -> Finished {
        Finished {
            session: SessionId::parse("t").unwrap(),
            usage: Usage::Codex { tokens },
            background: 0,
        }
    }

    #[rstest]
    #[case(None, "✅ Готово · ходов: 3")]
    #[case(Some("0.1234"), "✅ Готово · ходов: 3 · $0.12")]
    #[case(Some("2"), "✅ Готово · ходов: 3 · $2.00")]
    #[case(Some("0.125"), "✅ Готово · ходов: 3 · $0.12")]
    fn claude_finished_line(#[case] cost: Option<&str>, #[case] expected: &str) {
        assert_eq!(format_finished(&claude(3, cost, 0)), expected);
    }

    #[test]
    fn finished_mentions_running_background_tasks() {
        assert_eq!(
            format_finished(&claude(3, None, 2)),
            "✅ Готово · ходов: 3 · ⏳ в фоне задач: 2, пришлю результат"
        );
    }

    #[rstest]
    #[case(None, "✅ Готово")]
    #[case(Some(12_345), "✅ Готово · токенов в сессии: 12 345")]
    fn qwen_finished_line(#[case] tokens: Option<u64>, #[case] expected: &str) {
        let finished = Finished {
            session: SessionId::parse("q").unwrap(),
            usage: Usage::Qwen { tokens },
            background: 0,
        };
        assert_eq!(format_finished(&finished), expected);
    }

    #[rstest]
    #[case(None, 0, "✅ Готово")]
    #[case(Some(0), 0, "✅ Готово · токенов в сессии: 0")]
    #[case(Some(12_345), 0, "✅ Готово · токенов в сессии: 12 345")]
    #[case(None, 2, "✅ Готово · ⏳ в фоне задач: 2, пришлю результат")]
    fn hermes_finished_line(
        #[case] tokens: Option<u64>,
        #[case] background: usize,
        #[case] expected: &str,
    ) {
        let finished = Finished {
            session: SessionId::parse("h").unwrap(),
            usage: Usage::Hermes { tokens },
            background,
        };
        assert_eq!(format_finished(&finished), expected);
    }

    #[rstest]
    #[case(None, "✅ Готово")]
    #[case(Some(0), "✅ Готово · токенов в сессии: 0")]
    #[case(Some(999), "✅ Готово · токенов в сессии: 999")]
    #[case(Some(12_345), "✅ Готово · токенов в сессии: 12 345")]
    #[case(Some(1_000_000), "✅ Готово · токенов в сессии: 1 000 000")]
    fn codex_finished_line(#[case] tokens: Option<u64>, #[case] expected: &str) {
        assert_eq!(format_finished(&codex(tokens)), expected);
    }

    #[test]
    fn abandoned_lists_tasks() {
        let tasks = ["sleep 600".to_owned(), "npm test".to_owned()];
        assert_eq!(
            format_abandoned(&tasks, Duration::from_mins(30)),
            "⌛ Фоновые задачи не завершились за 30 мин, сессия закрыта:\n• sleep 600\n• npm test"
        );
    }

    fn at(raw: &str) -> DateTime<Utc> {
        raw.parse().unwrap()
    }

    fn msk() -> FixedOffset {
        FixedOffset::east_opt(3 * 3600).unwrap()
    }

    fn label(raw: &str) -> Label {
        Label::new(raw).unwrap()
    }

    fn window(kind: WindowKind, percent: i64, resets_at: Option<&str>) -> UsageWindow {
        UsageWindow { kind, percent: Percent::from_int(percent), resets_at: resets_at.map(at) }
    }

    fn card(reports: &[QuotaReport]) -> String {
        render_limits(reports, at("2026-10-16T11:05:00Z"), &msk())
    }

    fn other(name: &str) -> WindowKind {
        WindowKind::Other(label(name))
    }

    #[test]
    fn the_spec_card_is_rendered() {
        let claude = QuotaReport::fresh(
            BackendKind::Claude,
            Some(label("Max")),
            vec![
                window(WindowKind::FiveHour, 62, Some("2026-10-16T14:30:00Z")),
                window(WindowKind::Week, 18, Some("2026-10-19T06:00:00Z")),
                window(WindowKind::WeekModel(label("Opus")), 9, None),
            ],
        );
        let codex = QuotaReport::fresh(
            BackendKind::Codex,
            Some(label("ChatGPT Plus")),
            vec![
                window(other("основное"), 41, Some("2026-10-16T13:10:00Z")),
                window(other("недельное"), 12, Some("2026-10-22T08:00:00Z")),
            ],
        );
        let reports = [
            claude,
            codex,
            QuotaReport::not_reported(BackendKind::Qwen),
            QuotaReport::not_reported(BackendKind::Hermes),
        ];
        assert_eq!(
            card(&reports),
            [
                "📊 <b>Лимиты подписок</b> · 16.10 14:05",
                "<b>Claude</b> (Max)",
                "<code>  5 часов     ▓▓▓▓▓▓░░░░ 62%</code> · сброс в 17:30",
                "<code>  7 дней      ▓▓░░░░░░░░ 18%</code> · сброс пн 09:00",
                "<code>  7 дней Opus ▓░░░░░░░░░  9%</code>",
                "<b>Codex</b> (ChatGPT Plus)",
                "<code>  основное  ▓▓▓▓░░░░░░ 41%</code> · сброс в 16:10",
                "<code>  недельное ▓░░░░░░░░░ 12%</code> · сброс чт 11:00",
                "Qwen, Hermes — квоты подписки агент не сообщает",
            ]
            .join("\n")
        );
    }

    #[rstest]
    #[case::empty(0, "<code>  7 дней ░░░░░░░░░░  0%</code>")]
    #[case::rounds_down(4, "<code>  7 дней ░░░░░░░░░░  4%</code>")]
    #[case::rounds_up(5, "<code>  7 дней ▓░░░░░░░░░  5%</code>")]
    #[case::almost(95, "<code>  7 дней ▓▓▓▓▓▓▓▓▓▓ 95%</code>")]
    #[case::full(100, "<code>  7 дней ▓▓▓▓▓▓▓▓▓▓ 100%</code>")]
    #[case::clamped(250, "<code>  7 дней ▓▓▓▓▓▓▓▓▓▓ 100%</code>")]
    fn the_bar_has_ten_cells_and_percent_is_clamped(#[case] percent: i64, #[case] line: &str) {
        let report = QuotaReport::fresh(
            BackendKind::Claude,
            None,
            vec![window(WindowKind::Week, percent, None)],
        );
        assert_eq!(card(&[report]).lines().nth(2), Some(line));
    }

    #[rstest]
    #[case::today("2026-10-16T20:59:00Z", " · сброс в 23:59")]
    #[case::tomorrow_local("2026-10-16T21:00:00Z", " · сброс сб 00:00")]
    #[case::within_a_week("2026-10-22T08:00:00Z", " · сброс чт 11:00")]
    #[case::a_week_on("2026-10-23T08:00:00Z", " · сброс 23.10 11:00")]
    #[case::past("2026-10-15T08:00:00Z", " · сброс 15.10 11:00")]
    fn resets_are_shown_in_local_time(#[case] resets_at: &str, #[case] suffix: &str) {
        let report = QuotaReport::fresh(
            BackendKind::Claude,
            None,
            vec![window(WindowKind::FiveHour, 50, Some(resets_at))],
        );
        let text = card(&[report]);
        let line = text.lines().nth(2).unwrap();
        assert!(line.ends_with(&format!("</code>{suffix}")), "{line}");
    }

    #[rstest]
    #[case::same_day("2026-10-16T11:02:00Z", "<b>Claude</b> (Max) (на 14:02)")]
    #[case::yesterday("2026-10-15T20:00:00Z", "<b>Claude</b> (Max) (на 15.10 23:00)")]
    fn cached_data_is_dated(#[case] cached: &str, #[case] head: &str) {
        let report = QuotaReport::cached(
            BackendKind::Claude,
            Some(label("Max")),
            vec![window(WindowKind::FiveHour, 10, None)],
            at(cached),
        );
        assert_eq!(card(&[report]).lines().nth(1), Some(head));
    }

    #[test]
    fn unavailable_and_empty_reports_say_why() {
        let reports = [
            QuotaReport::unavailable(
                BackendKind::Codex,
                "вход по API-ключу — лимитов подписки нет",
            ),
            QuotaReport::fresh(BackendKind::Claude, Some(label("Pro")), Vec::new()),
        ];
        assert_eq!(
            card(&reports),
            "📊 <b>Лимиты подписок</b> · 16.10 14:05\n\
             <b>Codex</b> ⚠️ вход по API-ключу — лимитов подписки нет\n\
             <b>Claude</b> (Pro)\n  нет данных"
        );
        assert_eq!(
            card(&[]),
            "📊 <b>Лимиты подписок</b> · 16.10 14:05\nНи один CLI агента не найден"
        );
    }

    #[test]
    fn agent_strings_are_escaped() {
        let reports = [
            QuotaReport::fresh(
                BackendKind::Claude,
                Some(label("<b>&")),
                vec![window(other("<i>x</i>"), 10, None)],
            ),
            QuotaReport::unavailable(BackendKind::Codex, "a<script>"),
        ];
        let text = card(&reports);
        assert!(text.contains("<b>Claude</b> (&lt;b&gt;&amp;)"), "{text}");
        assert!(text.contains("<code>  &lt;i&gt;x&lt;/i&gt; ▓░"), "{text}");
        assert!(text.contains("⚠️ a&lt;script&gt;"), "{text}");
        assert!(!text.contains("<i>") && !text.contains("<script>"));
    }

    #[test]
    fn the_header_uses_the_local_date() {
        let text = render_limits(&[], at("2026-10-16T22:30:00Z"), &msk());
        assert!(text.starts_with("📊 <b>Лимиты подписок</b> · 17.10 01:30"), "{text}");
    }
}
