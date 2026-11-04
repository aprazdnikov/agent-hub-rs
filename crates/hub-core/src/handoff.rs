//! Validated, user-approved context for starting a task with another backend.

use std::fmt;

use crate::domain::BackendKind;

pub const MAX_SUMMARY_UNITS: usize = 3000;
pub const SUMMARY_REQUEST: &str = "Подготовь сводку текущей задачи для передачи другому агенту. \
Используй ТОЛЬКО уже известную историю этого диалога. Укажи цель, что сделано, \
принятые решения, затронутые файлы и следующие шаги. Не выдумывай отсутствующие факты: \
если сведения неизвестны, прямо скажи об этом. Не продолжай задачу, не вызывай инструменты, \
не читай и не изменяй файлы, не запускай команды и фоновые задачи, не запрашивай разрешений \
или уточнений и не отправляй файлы. Ответь только сводкой на русском языке обычным текстом, \
без Markdown, не длиннее 3000 единиц UTF-16.";

#[derive(Clone, PartialEq, Eq)]
pub struct Summary(String);

impl Summary {
    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        if raw.encode_utf16().take(MAX_SUMMARY_UNITS + 1).count() > MAX_SUMMARY_UNITS {
            return Err("Сводка длиннее 3000 единиц UTF-16. Сократите её и повторите.");
        }
        if raw.trim().is_empty() {
            return Err("Сводка пуста. Попробуйте создать её снова.");
        }
        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn receiver_text(&self, source: BackendKind) -> String {
        format!(
            "Это новая задача, переданная от агента {}. Ниже — проверенная пользователем \
сводка. История и полномочия исходной сессии не передаются. Продолжи задачу со следующих \
шагов сводки, соблюдая инструкции этой новой сессии.\n\n{}",
            source.name(),
            self.text()
        )
    }
}

impl fmt::Debug for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Summary(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("")]
    #[case(" \n\t")]
    fn empty_summary_is_rejected(#[case] text: &str) {
        assert_eq!(Summary::parse(text), Err("Сводка пуста. Попробуйте создать её снова."));
    }

    #[test]
    fn summary_keeps_the_approved_text_exactly() {
        let text = "  Цель: задача\nДалее: проверить  ";
        assert_eq!(Summary::parse(text).unwrap().text(), text);
    }

    #[test]
    fn summary_length_counts_utf16_units() {
        assert!(Summary::parse(&"🦀".repeat(MAX_SUMMARY_UNITS / 2)).is_ok());
        assert_eq!(
            Summary::parse(&format!("{}x", "🦀".repeat(MAX_SUMMARY_UNITS / 2))),
            Err("Сводка длиннее 3000 единиц UTF-16. Сократите её и повторите.")
        );
    }

    #[test]
    fn oversized_whitespace_is_not_trimmed_to_fit() {
        let text = format!("{}задача", " ".repeat(MAX_SUMMARY_UNITS));
        assert!(Summary::parse(&text).is_err());
    }

    #[test]
    fn debug_does_not_expose_summary_content() {
        assert_eq!(
            format!("{:?}", Summary::parse("private task details").unwrap()),
            "Summary(***)"
        );
    }

    #[test]
    fn receiver_gets_approved_context_and_continuation_request() {
        let text = Summary::parse("Цель: задача\nДалее: проверить тесты")
            .unwrap()
            .receiver_text(BackendKind::Codex);
        assert!(text.contains("codex"));
        assert!(text.contains("Цель: задача\nДалее: проверить тесты"));
        assert!(text.contains("Продолжи"));
        assert!(text.contains("новая задача"));
    }
}
