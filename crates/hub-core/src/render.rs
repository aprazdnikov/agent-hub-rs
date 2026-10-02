use std::num::NonZeroUsize;
use std::time::Duration;

use rust_decimal::Decimal;

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
pub fn format_finished(turns: u32, cost: Option<Decimal>, background: usize) -> String {
    let cost = cost.map_or_else(String::new, |cost| {
        // Banker's rounding, as Python's Decimal.quantize, then always two decimals.
        let mut cents = cost.round_dp(2);
        cents.rescale(2);
        format!(" · ${cents}")
    });
    let pending = if background == 0 {
        String::new()
    } else {
        format!(" · ⏳ в фоне задач: {background}, пришлю результат")
    };
    format!("✅ Готово · ходов: {turns}{cost}{pending}")
}

#[must_use]
pub fn format_abandoned(tasks: &[String], timeout: Duration) -> String {
    let listing = tasks.iter().map(|task| format!("• {task}")).collect::<Vec<_>>().join("\n");
    format!(
        "⌛ Фоновые задачи не завершились за {} мин, сессия закрыта:\n{listing}",
        timeout.as_secs() / 60
    )
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use rstest::rstest;
    use rust_decimal::Decimal;

    use super::*;

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

    #[rstest]
    #[case(None, "✅ Готово · ходов: 3")]
    #[case(Some("0.1234"), "✅ Готово · ходов: 3 · $0.12")]
    #[case(Some("2"), "✅ Готово · ходов: 3 · $2.00")]
    #[case(Some("0.125"), "✅ Готово · ходов: 3 · $0.12")]
    fn finished_line(#[case] cost: Option<&str>, #[case] expected: &str) {
        let cost = cost.map(|raw| raw.parse::<Decimal>().unwrap());
        assert_eq!(format_finished(3, cost, 0), expected);
    }

    #[test]
    fn finished_mentions_running_background_tasks() {
        assert_eq!(
            format_finished(3, None, 2),
            "✅ Готово · ходов: 3 · ⏳ в фоне задач: 2, пришлю результат"
        );
    }

    #[test]
    fn abandoned_lists_tasks() {
        let tasks = ["sleep 600".to_owned(), "npm test".to_owned()];
        assert_eq!(
            format_abandoned(&tasks, Duration::from_mins(30)),
            "⌛ Фоновые задачи не завершились за 30 мин, сессия закрыта:\n• sleep 600\n• npm test"
        );
    }
}
