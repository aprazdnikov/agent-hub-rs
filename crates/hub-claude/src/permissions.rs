//! `can_use_tool` decisions: clarifying questions go to the human as questions, the rest as
//! approvals; a question input that cannot be parsed still reaches the human as an approval.

use hub_core::domain::{Decision, Question, QuestionAnswer, QuestionsOutcome, ToolRequest};
use hub_core::render::truncate;
use serde_json::{Map, Value, json};

use hub_agent::tools::{TOOL_SUMMARY_LIMIT, parse_questions};

use crate::outgoing::object;

pub const ASK_USER_QUESTION: &str = "AskUserQuestion";
pub const SEND_FILE_TOOL: &str = "mcp__agent-hub__send_file";

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Allow,
    Ask(Vec<Question>),
    Request(ToolRequest),
}

#[must_use]
pub fn route(tool: &str, input: &Value) -> Route {
    if tool == SEND_FILE_TOOL {
        // Only reaches the user's own chat, which already sees everything the agent prints.
        return Route::Allow;
    }
    if tool == ASK_USER_QUESTION
        && let Some(questions) = parse_questions(input)
    {
        return Route::Ask(questions);
    }
    Route::Request(ToolRequest {
        tool: tool.to_owned(),
        summary: summarize_tool_input(tool, input),
    })
}

/// One human-readable line for the most common tools.
#[must_use]
pub fn summarize_tool_input(tool: &str, input: &Value) -> String {
    let key = match tool {
        "Bash" => Some("command"),
        "Read" | "Write" | "Edit" | "MultiEdit" => Some("file_path"),
        "NotebookEdit" => Some("notebook_path"),
        "Glob" | "Grep" => Some("pattern"),
        "WebFetch" => Some("url"),
        "WebSearch" => Some("query"),
        _ => None,
    };
    let text = key
        .and_then(|key| input.get(key))
        .and_then(Value::as_str)
        .map_or_else(|| input.to_string(), str::to_owned);
    truncate(&text, TOOL_SUMMARY_LIMIT)
}

#[must_use]
pub fn allow(input: Value) -> Value {
    object([("behavior", Value::from("allow")), ("updatedInput", input)])
}

#[must_use]
pub fn deny(reason: &str) -> Value {
    json!({"behavior": "deny", "message": reason})
}

#[must_use]
pub fn decided(input: Value, decision: Decision) -> Value {
    match decision {
        Decision::Allowed => allow(input),
        Decision::Denied(denied) => deny(&denied.reason),
    }
}

/// The CLI reads the answers from the tool input and hands them to the model.
#[must_use]
pub fn answered(input: Value, outcome: QuestionsOutcome) -> Value {
    match outcome {
        QuestionsOutcome::Answered(answers) => allow(with_answers(input, &answers)),
        QuestionsOutcome::Denied(denied) => deny(&denied.reason),
    }
}

fn with_answers(input: Value, answers: &[QuestionAnswer]) -> Value {
    let Value::Object(mut object) = input else {
        return input;
    };
    let answers: Map<String, Value> = answers
        .iter()
        .map(|answer| (answer.question.clone(), Value::String(answer.answer.clone())))
        .collect();
    object.insert("answers".to_owned(), Value::Object(answers));
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use hub_core::domain::{Denied, QuestionOption, Selection};
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    fn ask_input() -> Value {
        json!({"questions": [{
            "question": "Какой формат?",
            "header": "Формат",
            "options": [
                {"label": "Кратко", "description": "Только суть"},
                {"label": "Подробно", "description": "С примерами"}
            ],
            "multiSelect": false
        }]})
    }

    fn ask_question() -> Question {
        Question::new(
            "Какой формат?".to_owned(),
            "Формат".to_owned(),
            vec![
                QuestionOption {
                    label: "Кратко".to_owned(),
                    description: "Только суть".to_owned(),
                },
                QuestionOption {
                    label: "Подробно".to_owned(),
                    description: "С примерами".to_owned(),
                },
            ],
            Selection::Single,
        )
        .unwrap()
    }

    #[test]
    fn questions_route_to_ask() {
        assert_eq!(route(ASK_USER_QUESTION, &ask_input()), Route::Ask(vec![ask_question()]));
    }

    #[test]
    fn malformed_question_falls_back_to_approval() {
        assert_eq!(
            route(ASK_USER_QUESTION, &json!({"questions": []})),
            Route::Request(ToolRequest {
                tool: ASK_USER_QUESTION.to_owned(),
                summary: r#"{"questions":[]}"#.to_owned()
            })
        );
    }

    #[test]
    fn send_file_is_allowed_without_asking() {
        assert_eq!(route(SEND_FILE_TOOL, &json!({"path": "a.pdf"})), Route::Allow);
    }

    #[test]
    fn other_tools_go_through_approval() {
        assert_eq!(
            route("Bash", &json!({"command": "ls"})),
            Route::Request(ToolRequest { tool: "Bash".to_owned(), summary: "ls".to_owned() })
        );
    }

    #[rstest]
    #[case("Read", json!({"file_path": "/a/b.py", "limit": 10}), "/a/b.py")]
    #[case("NotebookEdit", json!({"notebook_path": "n.ipynb"}), "n.ipynb")]
    #[case("WebSearch", json!({"query": "rust"}), "rust")]
    #[case("mcp__x", json!({"q": "привет"}), r#"{"q":"привет"}"#)]
    #[case("Bash", json!({"command": 1}), r#"{"command":1}"#)]
    fn tool_input_summaries(#[case] tool: &str, #[case] input: Value, #[case] expected: &str) {
        assert_eq!(summarize_tool_input(tool, &input), expected);
    }

    #[test]
    fn summary_is_truncated() {
        let input = json!({"command": "x".repeat(TOOL_SUMMARY_LIMIT * 2)});
        assert_eq!(summarize_tool_input("Bash", &input).chars().count(), TOOL_SUMMARY_LIMIT);
    }

    #[test]
    fn answers_are_returned_as_tool_input() {
        let outcome = QuestionsOutcome::Answered(vec![QuestionAnswer {
            question: "Какой формат?".to_owned(),
            answer: "Кратко".to_owned(),
        }]);
        let mut expected = ask_input();
        expected["answers"] = json!({"Какой формат?": "Кратко"});
        assert_eq!(
            answered(ask_input(), outcome),
            json!({"behavior": "allow", "updatedInput": expected})
        );
    }

    #[test]
    fn declined_questions_deny_the_tool() {
        assert_eq!(
            answered(ask_input(), QuestionsOutcome::Denied(Denied::new("нет"))),
            json!({"behavior": "deny", "message": "нет"})
        );
    }

    #[test]
    fn decisions_map_to_behaviors() {
        let input = json!({"command": "ls"});
        assert_eq!(
            decided(input.clone(), Decision::Allowed),
            json!({"behavior": "allow", "updatedInput": {"command": "ls"}})
        );
        assert_eq!(
            decided(input, Decision::Denied(Denied::new("нет"))),
            json!({"behavior": "deny", "message": "нет"})
        );
    }
}
