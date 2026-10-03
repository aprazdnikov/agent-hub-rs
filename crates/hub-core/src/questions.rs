//! Pending agent questions answered with buttons or a free-text message in the same topic.

use std::collections::BTreeSet;

use crate::domain::{Denied, Question, Selection, TopicKey};
use crate::escape::escape;
use crate::ids::hex;
use crate::render::truncate;

pub const CALLBACK_PREFIX: &str = "qa";
const SUBMIT: &str = "done";
const DECLINE: &str = "no";
pub const DECLINED_BY_USER: &str = "Пользователь отказался отвечать на вопрос";
pub const BUTTON_TEXT_LIMIT: usize = 60;
// Keeps the whole question inside one Telegram message.
pub const QUESTION_TEXT_LIMIT: usize = 2000;
pub const DESCRIPTION_TEXT_LIMIT: usize = 300;
pub const ANSWER_TEXT_LIMIT: usize = 500;
// Multi-select answers use the separator the agent CLI expects.
pub const ANSWER_SEPARATOR: &str = ", ";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QuestionId(String);

impl QuestionId {
    #[must_use]
    pub fn from_random(bytes: [u8; 8]) -> Self {
        Self(hex(&bytes))
    }

    fn parse(raw: &str) -> Option<Self> {
        (!raw.is_empty()).then(|| Self(raw.to_owned()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionAction {
    Pick(usize),
    Submit,
    Decline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionPress {
    pub id: QuestionId,
    pub action: QuestionAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Text(String),
    Declined(Denied),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub text: String,
    pub data: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Press<R> {
    Accepted {
        responder: R,
        answer: Answer,
    },
    SelectionChanged {
        question: Question,
        selected: BTreeSet<usize>,
    },
    NothingSelected,
    /// The question is gone (answered, timed out, stopped) or the button does not fit it.
    Stale,
}

struct Pending<R> {
    id: QuestionId,
    key: TopicKey,
    question: Question,
    selected: BTreeSet<usize>,
    responder: R,
}

enum Step {
    Resolve(Answer),
    Changed,
    NothingSelected,
    Stale,
}

/// Open questions in the order they were asked; `R` delivers the answer back to the asker.
pub struct QuestionRegistry<R> {
    pending: Vec<Pending<R>>,
}

impl<R> Default for QuestionRegistry<R> {
    fn default() -> Self {
        Self { pending: Vec::new() }
    }
}

impl<R> QuestionRegistry<R> {
    pub fn open(&mut self, id: QuestionId, key: TopicKey, question: Question, responder: R) {
        self.pending.push(Pending { id, key, question, selected: BTreeSet::new(), responder });
    }

    pub fn close(&mut self, id: &QuestionId) {
        self.pending.retain(|pending| pending.id != *id);
    }

    pub fn press(&mut self, press: &QuestionPress) -> Press<R> {
        let Some(position) = self.pending.iter().position(|pending| pending.id == press.id) else {
            return Press::Stale;
        };
        let Some(pending) = self.pending.get_mut(position) else {
            return Press::Stale;
        };
        match step(pending, press.action) {
            Step::Resolve(answer) => {
                let pending = self.pending.remove(position);
                Press::Accepted { responder: pending.responder, answer }
            }
            Step::Changed => Press::SelectionChanged {
                question: pending.question.clone(),
                selected: pending.selected.clone(),
            },
            Step::NothingSelected => Press::NothingSelected,
            Step::Stale => Press::Stale,
        }
    }

    /// Answer the topic's oldest open question with free text.
    pub fn reply(&mut self, key: TopicKey, text: &str) -> Option<(R, Answer)> {
        let position = self.pending.iter().position(|pending| pending.key == key)?;
        let pending = self.pending.remove(position);
        Some((pending.responder, Answer::Text(text.to_owned())))
    }
}

fn step<R>(pending: &mut Pending<R>, action: QuestionAction) -> Step {
    let options = pending.question.options();
    match (action, pending.question.selection()) {
        (QuestionAction::Pick(index), _) if index >= options.len() => Step::Stale,
        (QuestionAction::Pick(index), Selection::Multiple) => {
            if !pending.selected.remove(&index) {
                pending.selected.insert(index);
            }
            Step::Changed
        }
        (QuestionAction::Pick(index), Selection::Single) => options
            .get(index)
            .map_or(Step::Stale, |option| Step::Resolve(Answer::Text(option.label.clone()))),
        (QuestionAction::Submit, Selection::Single) => Step::Stale,
        (QuestionAction::Submit, Selection::Multiple) if pending.selected.is_empty() => {
            Step::NothingSelected
        }
        (QuestionAction::Submit, Selection::Multiple) => {
            let labels = pending
                .selected
                .iter()
                .filter_map(|index| options.get(*index))
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>();
            Step::Resolve(Answer::Text(labels.join(ANSWER_SEPARATOR)))
        }
        (QuestionAction::Decline, _) => {
            Step::Resolve(Answer::Declined(Denied::new(DECLINED_BY_USER)))
        }
    }
}

#[must_use]
pub fn callback_data(id: &QuestionId, action: QuestionAction) -> String {
    let code = match action {
        QuestionAction::Pick(index) => index.to_string(),
        QuestionAction::Submit => SUBMIT.to_owned(),
        QuestionAction::Decline => DECLINE.to_owned(),
    };
    format!("{CALLBACK_PREFIX}:{}:{code}", id.0)
}

#[must_use]
pub fn parse_callback_data(data: &str) -> Option<QuestionPress> {
    match data.split(':').collect::<Vec<_>>().as_slice() {
        [prefix, id, code] if *prefix == CALLBACK_PREFIX => {
            Some(QuestionPress { id: QuestionId::parse(id)?, action: parse_action(code)? })
        }
        _ => None,
    }
}

fn parse_action(code: &str) -> Option<QuestionAction> {
    match code {
        SUBMIT => Some(QuestionAction::Submit),
        DECLINE => Some(QuestionAction::Decline),
        _ if !code.is_empty() && code.bytes().all(|byte| byte.is_ascii_digit()) => {
            code.parse().ok().map(QuestionAction::Pick)
        }
        _ => None,
    }
}

#[must_use]
pub fn keyboard(
    id: &QuestionId,
    question: &Question,
    selected: &BTreeSet<usize>,
) -> Vec<Vec<Button>> {
    let button = |text: String, action| Button { text, data: callback_data(id, action) };
    let mut rows: Vec<Vec<Button>> = question
        .options()
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let label = truncate(&option.label, BUTTON_TEXT_LIMIT);
            let text = match question.selection() {
                Selection::Single => label,
                Selection::Multiple => {
                    format!("{} {label}", if selected.contains(&index) { "☑" } else { "☐" })
                }
            };
            vec![button(text, QuestionAction::Pick(index))]
        })
        .collect();
    let decline = button("❌ Не отвечать".to_owned(), QuestionAction::Decline);
    rows.push(match question.selection() {
        Selection::Single => vec![decline],
        Selection::Multiple => {
            vec![button("✅ Готово".to_owned(), QuestionAction::Submit), decline]
        }
    });
    rows
}

#[must_use]
pub fn question_html(question: &Question) -> String {
    let title = if question.header().trim().is_empty() {
        "❓ ".to_owned()
    } else {
        format!("❓ <b>{}</b>\n", escape(question.header()))
    };
    let options = question
        .options()
        .iter()
        .map(|option| {
            let description = if option.description.trim().is_empty() {
                String::new()
            } else {
                format!(" — {}", escape(&truncate(&option.description, DESCRIPTION_TEXT_LIMIT)))
            };
            format!("• <b>{}</b>{description}", escape(&option.label))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let hint = match question.selection() {
        Selection::Single => "Выберите вариант или напишите свой ответ сообщением.",
        Selection::Multiple => {
            "Отметьте варианты и нажмите «Готово» или напишите свой ответ сообщением."
        }
    };
    let text = escape(&truncate(question.text(), QUESTION_TEXT_LIMIT));
    [format!("{title}{text}"), options, format!("<i>{hint}</i>")]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[must_use]
pub fn answer_line(answer: &Answer) -> String {
    match answer {
        Answer::Text(text) => format!("💬 {}", escape(&truncate(text, ANSWER_TEXT_LIMIT))),
        Answer::Declined(_) => "❌ Без ответа".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use rstest::rstest;

    use super::*;
    use crate::domain::{ChatId, QuestionOption, Selection, ThreadId};

    const KEY: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
    const OTHER_KEY: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(8) };

    fn options() -> Vec<QuestionOption> {
        vec![
            QuestionOption {
                label: "Кратко".to_owned(), description: "Только суть".to_owned()
            },
            QuestionOption { label: "Подробно".to_owned(), description: String::new() },
        ]
    }

    fn single() -> Question {
        Question::new("Какой формат?".to_owned(), "Формат".to_owned(), options(), Selection::Single)
            .unwrap()
    }

    fn multi() -> Question {
        Question::new("Какие разделы?".to_owned(), String::new(), options(), Selection::Multiple)
            .unwrap()
    }

    fn id() -> QuestionId {
        QuestionId::from_random([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef])
    }

    fn press(action: QuestionAction) -> QuestionPress {
        QuestionPress { id: id(), action }
    }

    fn selected(indices: &[usize]) -> BTreeSet<usize> {
        indices.iter().copied().collect()
    }

    #[rstest]
    #[case(QuestionAction::Pick(0))]
    #[case(QuestionAction::Pick(11))]
    #[case(QuestionAction::Submit)]
    #[case(QuestionAction::Decline)]
    fn callback_data_round_trips(#[case] action: QuestionAction) {
        let data = callback_data(&id(), action);
        assert!(data.len() <= 64, "Telegram callback_data limit");
        assert_eq!(parse_callback_data(&data), Some(press(action)));
    }

    #[rstest]
    #[case("")]
    #[case("qa")]
    #[case("qa:x:")]
    #[case("qa:x:maybe")]
    #[case("qa:x:-1")]
    #[case("ap:x:0")]
    #[case("qa:x:0:1")]
    #[case("qa::0")]
    #[case("qa:x:²")]
    fn malformed_callback_data_is_rejected(#[case] data: &str) {
        assert_eq!(parse_callback_data(data), None);
    }

    #[test]
    fn single_choice_pick_answers_with_label() {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, single(), "r");

        assert_eq!(
            registry.press(&press(QuestionAction::Pick(1))),
            Press::Accepted { responder: "r", answer: Answer::Text("Подробно".to_owned()) }
        );
        assert_eq!(registry.press(&press(QuestionAction::Pick(0))), Press::Stale);
    }

    #[test]
    fn multi_choice_toggles_then_submits_in_option_order() {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, multi(), "r");

        assert_eq!(
            registry.press(&press(QuestionAction::Pick(1))),
            Press::SelectionChanged { question: multi(), selected: selected(&[1]) }
        );
        assert_eq!(
            registry.press(&press(QuestionAction::Pick(0))),
            Press::SelectionChanged { question: multi(), selected: selected(&[0, 1]) }
        );
        assert_eq!(
            registry.press(&press(QuestionAction::Pick(1))),
            Press::SelectionChanged { question: multi(), selected: selected(&[0]) }
        );
        registry.press(&press(QuestionAction::Pick(1)));

        assert_eq!(
            registry.press(&press(QuestionAction::Submit)),
            Press::Accepted {
                responder: "r", answer: Answer::Text("Кратко, Подробно".to_owned())
            }
        );
    }

    #[test]
    fn multi_choice_submit_requires_selection() {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, multi(), ());
        assert_eq!(registry.press(&press(QuestionAction::Submit)), Press::NothingSelected);
    }

    #[rstest]
    #[case(QuestionAction::Pick(2))]
    #[case(QuestionAction::Submit)]
    fn invalid_press_for_single_choice_is_stale(#[case] action: QuestionAction) {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, single(), ());
        assert_eq!(registry.press(&press(action)), Press::Stale);
        assert!(registry.reply(KEY, "still open").is_some());
    }

    #[test]
    fn decline_denies() {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, single(), ());
        assert_eq!(
            registry.press(&press(QuestionAction::Decline)),
            Press::Accepted {
                responder: (),
                answer: Answer::Declined(Denied::new(DECLINED_BY_USER))
            }
        );
    }

    #[test]
    fn text_reply_answers_pending_question_of_its_topic_only() {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, single(), "r");

        assert_eq!(registry.reply(OTHER_KEY, "чужой"), None);
        assert_eq!(
            registry.reply(KEY, "свой вариант"),
            Some(("r", Answer::Text("свой вариант".to_owned())))
        );
        assert_eq!(registry.reply(KEY, "ещё"), None);
    }

    #[test]
    fn reply_answers_the_oldest_question_first() {
        let mut registry = QuestionRegistry::default();
        registry.open(QuestionId::from_random([1; 8]), KEY, single(), "first");
        registry.open(QuestionId::from_random([2; 8]), KEY, single(), "second");

        assert_eq!(registry.reply(KEY, "a").map(|(responder, _)| responder), Some("first"));
    }

    #[test]
    fn closed_question_is_stale() {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, single(), ());
        registry.close(&id());

        assert_eq!(registry.press(&press(QuestionAction::Pick(0))), Press::Stale);
        assert_eq!(registry.reply(KEY, "x"), None);
    }

    fn button(text: &str, action: QuestionAction) -> Button {
        Button { text: text.to_owned(), data: callback_data(&id(), action) }
    }

    #[test]
    fn single_choice_keyboard_has_options_and_decline() {
        assert_eq!(
            keyboard(&id(), &single(), &BTreeSet::new()),
            vec![
                vec![button("Кратко", QuestionAction::Pick(0))],
                vec![button("Подробно", QuestionAction::Pick(1))],
                vec![button("❌ Не отвечать", QuestionAction::Decline)],
            ]
        );
    }

    #[test]
    fn multi_choice_keyboard_marks_selection_and_submits() {
        assert_eq!(
            keyboard(&id(), &multi(), &selected(&[1])),
            vec![
                vec![button("☐ Кратко", QuestionAction::Pick(0))],
                vec![button("☑ Подробно", QuestionAction::Pick(1))],
                vec![
                    button("✅ Готово", QuestionAction::Submit),
                    button("❌ Не отвечать", QuestionAction::Decline),
                ],
            ]
        );
    }

    #[test]
    fn question_html_escapes_and_lists_descriptions() {
        let question =
            Question::new("a < b?".to_owned(), "H&M".to_owned(), options(), Selection::Single)
                .unwrap();
        assert_eq!(
            question_html(&question),
            "❓ <b>H&amp;M</b>\na &lt; b?\n\n• <b>Кратко</b> — Только суть\n• <b>Подробно</b>\n\n\
             <i>Выберите вариант или напишите свой ответ сообщением.</i>"
        );
    }

    #[test]
    fn question_html_without_header() {
        let html = question_html(&multi());
        assert!(html.starts_with("❓ Какие разделы?\n\n"));
        assert!(html.ends_with(
            "<i>Отметьте варианты и нажмите «Готово» или напишите свой ответ сообщением.</i>"
        ));
    }

    #[test]
    fn answer_line_escapes_and_truncates() {
        assert_eq!(answer_line(&Answer::Text("a<b".to_owned())), "💬 a&lt;b");
        assert_eq!(answer_line(&Answer::Declined(Denied::new("x"))), "❌ Без ответа");
        assert_eq!(
            answer_line(&Answer::Text("я".repeat(600))).chars().count(),
            "💬 ".chars().count() + ANSWER_TEXT_LIMIT
        );
    }
}
