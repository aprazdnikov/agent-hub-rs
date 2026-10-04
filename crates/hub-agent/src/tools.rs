//! Tools agent-hub itself gives an agent, and parsing of their inputs.

use hub_core::domain::{FileDelivery, OutgoingFile, Question, QuestionOption, Selection};
use serde_json::{Value, json};

use crate::channel::UserChannel;

pub const SEND_FILE: &str = "send_file";
pub const ASK_USER: &str = "ask_user";
pub const TOOL_SUMMARY_LIMIT: usize = 600;

pub const SEND_FILE_DESCRIPTION: &str = "Send a file to the user in their Telegram chat. The user \
    only sees your text replies, so use this whenever they ask for a file or a file is the \
    natural result (a PDF report, an archive, an image, a CSV export). `path` is absolute or \
    relative to the working directory and must stay inside it; `caption` is optional text shown \
    under the file.";
pub const ASK_USER_DESCRIPTION: &str = "Ask the user clarifying questions in their Telegram chat \
    and wait for the answers. Use it only when an answer would materially change the work. Each \
    question has a short header and up to four options the user picks with a button; the user \
    may also reply with free text. Set multiSelect to let the user pick several options.";
pub const ASK_USER_SHAPE: &str = "questions must be a non-empty list of \
    {question, header, options: [{label, description}], multiSelect}";
const SEND_FILE_SHAPE: &str = "path must be a non-empty string, caption a string";

#[must_use]
pub fn send_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"path": {"type": "string"}, "caption": {"type": "string"}},
        "required": ["path"],
    })
}

#[must_use]
pub fn ask_user_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "questions": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        "question": {"type": "string"},
                        "header": {"type": "string"},
                        "options": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "label": {"type": "string"},
                                    "description": {"type": "string"},
                                },
                                "required": ["label"],
                            },
                        },
                        "multiSelect": {"type": "boolean"},
                    },
                    "required": ["question"],
                },
            }
        },
        "required": ["questions"],
    })
}

/// What a hub tool returns to the agent; an error is something the agent can react to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolResult {
    Success(String),
    Error(String),
}

pub fn parse_send_file(arguments: &Value) -> Result<OutgoingFile, String> {
    let path = arguments.get("path").and_then(Value::as_str).filter(|path| !path.trim().is_empty());
    let caption = match arguments.get("caption") {
        None => Some(""),
        Some(caption) => caption.as_str(),
    };
    match (path, caption) {
        (Some(path), Some(caption)) => {
            Ok(OutgoingFile { path: path.to_owned(), caption: caption.to_owned() })
        }
        (Some(_) | None, Some(_) | None) => Err(SEND_FILE_SHAPE.to_owned()),
    }
}

#[must_use]
pub fn delivery_result(path: &str, delivery: FileDelivery) -> ToolResult {
    match delivery {
        FileDelivery::Delivered => {
            ToolResult::Success(format!("Файл {path} отправлен пользователю"))
        }
        FileDelivery::Denied(denied) => ToolResult::Error(denied.reason),
    }
}

pub async fn deliver_file(arguments: &Value, channel: &dyn UserChannel) -> ToolResult {
    match parse_send_file(arguments) {
        Err(reason) => ToolResult::Error(reason),
        Ok(file) => {
            let path = file.path.clone();
            delivery_result(&path, channel.send_file(file).await)
        }
    }
}

#[must_use]
pub fn parse_questions(input: &Value) -> Option<Vec<Question>> {
    let raw = input.get("questions")?.as_array()?;
    if raw.is_empty() {
        return None;
    }
    raw.iter().map(parse_question).collect()
}

fn parse_question(raw: &Value) -> Option<Question> {
    let object = raw.as_object()?;
    let text = object.get("question")?.as_str()?;
    let header = match object.get("header") {
        None => "",
        Some(header) => header.as_str()?,
    };
    let options = match object.get("options") {
        None => Vec::new(),
        Some(options) => options.as_array()?.iter().map(parse_option).collect::<Option<_>>()?,
    };
    let selection = match object.get("multiSelect") {
        Some(Value::Bool(true)) => Selection::Multiple,
        Some(_) | None => Selection::Single,
    };
    Question::new(text.to_owned(), header.to_owned(), options, selection)
}

#[must_use]
pub fn parse_option(raw: &Value) -> Option<QuestionOption> {
    let object = raw.as_object()?;
    let label = object.get("label")?.as_str().filter(|label| !label.trim().is_empty())?;
    let description = match object.get("description") {
        None => "",
        Some(description) => description.as_str()?,
    };
    Some(QuestionOption { label: label.to_owned(), description: description.to_owned() })
}


#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures::future::BoxFuture;
    use hub_core::domain::{Decision, Denied, QuestionsOutcome, ToolRequest};
    use rstest::rstest;

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
    fn questions_are_parsed() {
        assert_eq!(parse_questions(&ask_input()), Some(vec![ask_question()]));
    }

    #[test]
    fn multi_select_is_recognised() {
        let input = json!({"questions": [{"question": "q", "options": [], "multiSelect": true}]});
        let questions = parse_questions(&input).unwrap();
        assert_eq!(questions.first().map(Question::selection), Some(Selection::Multiple));
    }

    #[rstest]
    #[case(json!({}))]
    #[case(json!({"questions": []}))]
    #[case(json!({"questions": "x"}))]
    #[case(json!({"questions": [{"header": "h", "options": []}]}))]
    #[case(json!({"questions": [{"question": "q", "options": [{"description": "no label"}]}]}))]
    #[case(json!({"questions": [{"question": "q", "header": 1}]}))]
    fn malformed_questions_are_rejected(#[case] input: Value) {
        assert_eq!(parse_questions(&input), None);
    }

    struct Recording {
        delivery: FileDelivery,
        sent: Mutex<Vec<OutgoingFile>>,
    }

    impl Recording {
        fn new(delivery: FileDelivery) -> Self {
            Self { delivery, sent: Mutex::new(Vec::new()) }
        }
    }

    impl UserChannel for Recording {
        fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
            Box::pin(async { Decision::Allowed })
        }
        fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            Box::pin(async { QuestionsOutcome::Answered(Vec::new()) })
        }
        fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            self.sent.lock().unwrap().push(file);
            let delivery = self.delivery.clone();
            Box::pin(async move { delivery })
        }
    }

    #[tokio::test]
    async fn delivered_file_is_a_success() {
        let channel = Recording::new(FileDelivery::Delivered);
        let result = deliver_file(&json!({"path": "out/report.pdf"}), &channel).await;
        assert_eq!(
            result,
            ToolResult::Success("Файл out/report.pdf отправлен пользователю".to_owned())
        );
        assert_eq!(
            *channel.sent.lock().unwrap(),
            [OutgoingFile { path: "out/report.pdf".to_owned(), caption: String::new() }]
        );
    }

    #[tokio::test]
    async fn refused_file_is_a_tool_error() {
        let channel = Recording::new(FileDelivery::Denied(Denied::new("вне рабочей директории")));
        let result = deliver_file(&json!({"path": "/etc/passwd"}), &channel).await;
        assert_eq!(result, ToolResult::Error("вне рабочей директории".to_owned()));
    }

    #[rstest]
    #[case(json!({}))]
    #[case(json!({"path": "  "}))]
    #[case(json!({"path": "a", "caption": 5}))]
    #[tokio::test]
    async fn malformed_send_file_never_reaches_the_user(#[case] arguments: Value) {
        let channel = Recording::new(FileDelivery::Delivered);
        let result = deliver_file(&arguments, &channel).await;
        assert_eq!(
            result,
            ToolResult::Error("path must be a non-empty string, caption a string".to_owned())
        );
        assert!(channel.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn schemas_require_their_main_argument() {
        assert_eq!(send_file_schema().get("required"), Some(&json!(["path"])));
        assert_eq!(ask_user_schema().get("required"), Some(&json!(["questions"])));
    }
}
