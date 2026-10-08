//! Requests the Codex app-server sends the client: approvals, hub tools and questions.

use hub_agent::channel::UserChannel;
use hub_agent::tools::{
    ASK_USER, ASK_USER_SHAPE, SEND_FILE, TOOL_SUMMARY_LIMIT, ToolResult, deliver_file,
    parse_option, parse_questions,
};
use hub_core::domain::{Decision, Question, QuestionsOutcome, Selection, ToolRequest};
use hub_core::render::truncate;
use serde_json::{Map, Value, json};

use crate::protocol::{PATCH, SHELL, UNNAMED_FILE_CHANGE};
use hub_agent::rpc::RequestError;

/// Replies to one app-server request on behalf of the human behind `channel`.
pub async fn answer(
    channel: &dyn UserChannel,
    method: &str,
    params: Value,
) -> Result<Value, RequestError> {
    match method {
        "item/commandExecution/requestApproval" => {
            Ok(approve(channel, request(SHELL, &command_summary(&params))).await)
        }
        "item/fileChange/requestApproval" => {
            Ok(approve(channel, request(PATCH, &file_change_summary(&params))).await)
        }
        "item/tool/call" => Ok(tool_response(call_tool(channel, &params).await?)),
        "item/tool/requestUserInput" => user_input(channel, &params).await,
        other => Err(RequestError::Unsupported(other.to_owned())),
    }
}

fn request(tool: &str, summary: &str) -> ToolRequest {
    ToolRequest { tool: tool.to_owned(), summary: truncate(summary, TOOL_SUMMARY_LIMIT) }
}

fn nonblank<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str).filter(|text| !text.trim().is_empty())
}

fn command_summary(params: &Value) -> String {
    nonblank(params, "command")
        .or_else(|| nonblank(params, "reason"))
        .map_or_else(|| params.to_string(), str::to_owned)
}

/// The changed paths were already shown as the `patch` line of the item.
fn file_change_summary(params: &Value) -> String {
    match (nonblank(params, "reason"), params.get("grantRoot").and_then(Value::as_str)) {
        (Some(reason), _) => reason.to_owned(),
        (None, Some(root)) => format!("запись в {root}"),
        (None, None) => UNNAMED_FILE_CHANGE.to_owned(),
    }
}

async fn approve(channel: &dyn UserChannel, tool: ToolRequest) -> Value {
    match channel.request(tool).await {
        Decision::Allowed => json!({"decision": "accept"}),
        Decision::Denied(_) => json!({"decision": "decline"}),
    }
}

async fn call_tool(channel: &dyn UserChannel, params: &Value) -> Result<ToolResult, RequestError> {
    let tool = params.get("tool").and_then(Value::as_str).ok_or_else(|| {
        RequestError::Malformed(format!("tool call without a tool name: {params}"))
    })?;
    let arguments = params.get("arguments").filter(|arguments| arguments.is_object());
    Ok(match (tool, arguments) {
        (SEND_FILE, Some(arguments)) => deliver_file(arguments, channel).await,
        (ASK_USER, Some(arguments)) => ask(channel, arguments).await,
        (tool, Some(_) | None) => {
            ToolResult::Error(format!("unknown tool {tool} or its arguments are not an object"))
        }
    })
}

async fn ask(channel: &dyn UserChannel, arguments: &Value) -> ToolResult {
    let Some(questions) = parse_questions(arguments) else {
        return ToolResult::Error(ASK_USER_SHAPE.to_owned());
    };
    match channel.ask(questions).await {
        QuestionsOutcome::Answered(answers) => ToolResult::Success(
            answers
                .iter()
                .map(|answer| format!("{}: {}", answer.question, answer.answer))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        QuestionsOutcome::Denied(denied) => ToolResult::Error(denied.reason),
    }
}

fn tool_response(result: ToolResult) -> Value {
    let (success, text) = match result {
        ToolResult::Success(text) => (true, text),
        ToolResult::Error(text) => (false, text),
    };
    json!({"success": success, "contentItems": [{"type": "inputText", "text": text}]})
}

async fn user_input(channel: &dyn UserChannel, params: &Value) -> Result<Value, RequestError> {
    let asked = user_questions(params)?;
    let outcome = channel.ask(asked.iter().map(|(_, question)| question.clone()).collect()).await;
    Ok(match outcome {
        QuestionsOutcome::Answered(answers) => {
            let replies: Map<String, Value> = asked
                .iter()
                .filter_map(|(id, question)| {
                    answers
                        .iter()
                        .find(|answer| answer.question == question.text())
                        .map(|answer| (id.clone(), json!({"answers": [answer.answer]})))
                })
                .collect();
            json!({"answers": replies})
        }
        // Codex goes on with its own judgment when a question stays unanswered.
        QuestionsOutcome::Denied(_) => json!({"answers": {}}),
    })
}

fn user_questions(params: &Value) -> Result<Vec<(String, Question)>, RequestError> {
    params
        .get("questions")
        .and_then(Value::as_array)
        .filter(|questions| !questions.is_empty())
        .ok_or_else(|| {
            RequestError::Malformed(format!("requestUserInput without questions: {params}"))
        })?
        .iter()
        .map(user_question)
        .collect()
}

fn user_question(raw: &Value) -> Result<(String, Question), RequestError> {
    let malformed = || RequestError::Malformed(format!("malformed question: {raw}"));
    let field = |key: &str| raw.get(key).and_then(Value::as_str).ok_or_else(malformed);
    let (id, header, text) = (field("id")?, field("header")?, field("question")?);
    let options = match raw.get("options") {
        None | Some(Value::Null) => Vec::new(),
        Some(options) => options
            .as_array()
            .ok_or_else(malformed)?
            .iter()
            .map(parse_option)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(malformed)?,
    };
    let question = Question::new(text.to_owned(), header.to_owned(), options, Selection::Single)
        .ok_or_else(malformed)?;
    Ok((id.to_owned(), question))
}

#[cfg(test)]
mod tests {
    use hub_core::domain::{Decision, Denied, QuestionAnswer, QuestionsOutcome, ToolRequest};
    use rstest::rstest;

    use super::*;
    use crate::testing::Scripted;

    const COMMAND: &str = "item/commandExecution/requestApproval";
    const FILE_CHANGE: &str = "item/fileChange/requestApproval";
    const TOOL_CALL: &str = "item/tool/call";
    const USER_INPUT: &str = "item/tool/requestUserInput";

    fn answering(question: &str, answer: &str) -> Scripted {
        Scripted {
            outcome: QuestionsOutcome::Answered(vec![QuestionAnswer {
                question: question.to_owned(),
                answer: answer.to_owned(),
            }]),
            ..Scripted::default()
        }
    }

    fn tool_text(success: bool, text: &str) -> Value {
        json!({"success": success, "contentItems": [{"type": "inputText", "text": text}]})
    }

    #[rstest]
    #[case(COMMAND, json!({"command": "npm test", "reason": "tests"}), "shell", "npm test")]
    #[case(COMMAND, json!({"command": " ", "reason": "нужна сеть"}), "shell", "нужна сеть")]
    #[case(COMMAND, json!({"cwd": "/w"}), "shell", r#"{"cwd":"/w"}"#)]
    #[case(FILE_CHANGE, json!({"reason": "правка конфига"}), "patch", "правка конфига")]
    #[case(FILE_CHANGE, json!({"grantRoot": "/w"}), "patch", "запись в /w")]
    #[case(FILE_CHANGE, json!({}), "patch", "изменение файлов")]
    #[tokio::test]
    async fn approvals_ask_the_human(
        #[case] method: &str,
        #[case] params: Value,
        #[case] tool: &str,
        #[case] summary: &str,
    ) {
        let channel = Scripted::default();
        assert_eq!(answer(&channel, method, params).await, Ok(json!({"decision": "accept"})));
        assert_eq!(
            *channel.requests.lock().unwrap(),
            [ToolRequest { tool: tool.to_owned(), summary: summary.to_owned() }]
        );
    }

    #[tokio::test]
    async fn denied_approval_declines() {
        let channel =
            Scripted { decision: Decision::Denied(Denied::new("нет")), ..Scripted::default() };
        let reply = answer(&channel, COMMAND, json!({"command": "rm -rf /"})).await;
        assert_eq!(reply, Ok(json!({"decision": "decline"})));
    }

    #[tokio::test]
    async fn send_file_delivers_through_the_channel() {
        let params = json!({"tool": "send_file", "arguments": {"path": "a.txt"}});
        assert_eq!(
            answer(&Scripted::default(), TOOL_CALL, params).await,
            Ok(tool_text(true, "Файл a.txt отправлен пользователю"))
        );
    }

    #[tokio::test]
    async fn ask_user_returns_answers_as_lines() {
        let channel = answering("Цвет?", "синий");
        let params =
            json!({"tool": "ask_user", "arguments": {"questions": [{"question": "Цвет?"}]}});
        assert_eq!(answer(&channel, TOOL_CALL, params).await, Ok(tool_text(true, "Цвет?: синий")));
    }

    #[rstest]
    #[case(json!({"tool": "ask_user", "arguments": {"questions": []}}), ASK_USER_SHAPE)]
    #[case(json!({"tool": "deploy", "arguments": {}}), "unknown tool deploy or its arguments are not an object")]
    #[case(json!({"tool": "send_file", "arguments": "a.txt"}), "unknown tool send_file or its arguments are not an object")]
    #[tokio::test]
    async fn bad_tool_calls_are_tool_errors(#[case] params: Value, #[case] text: &str) {
        assert_eq!(
            answer(&Scripted::default(), TOOL_CALL, params).await,
            Ok(tool_text(false, text))
        );
    }

    #[tokio::test]
    async fn tool_call_without_a_name_is_malformed() {
        let reply = answer(&Scripted::default(), TOOL_CALL, json!({"arguments": {}})).await;
        assert!(matches!(reply, Err(RequestError::Malformed(_))));
    }

    #[tokio::test]
    async fn user_input_answers_by_question_id() {
        let channel = answering("Какую БД?", "Postgres");
        let params = json!({"questions": [{
            "id": "db",
            "header": "БД",
            "question": "Какую БД?",
            "options": [{"label": "Postgres", "description": ""}, {"label": "SQLite"}],
        }]});
        assert_eq!(
            answer(&channel, USER_INPUT, params).await,
            Ok(json!({"answers": {"db": {"answers": ["Postgres"]}}}))
        );
        assert_eq!(channel.questions.lock().unwrap().first().map(|q| q.options().len()), Some(2));
    }

    #[tokio::test]
    async fn declined_user_input_leaves_codex_to_decide() {
        let channel = Scripted {
            outcome: QuestionsOutcome::Denied(Denied::new("нет ответа")),
            ..Scripted::default()
        };
        let params =
            json!({"questions": [{"id": "q", "header": "", "question": "Да?", "options": null}]});
        assert_eq!(answer(&channel, USER_INPUT, params).await, Ok(json!({"answers": {}})));
    }

    #[rstest]
    #[case(json!({}))]
    #[case(json!({"questions": []}))]
    #[case(json!({"questions": [{"id": "q", "question": "Да?"}]}))]
    #[case(json!({"questions": [{"id": "q", "header": "", "question": "Да?", "options": [{"label": ""}]}]}))]
    #[tokio::test]
    async fn malformed_user_input_is_rejected(#[case] params: Value) {
        assert!(matches!(
            answer(&Scripted::default(), USER_INPUT, params).await,
            Err(RequestError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn unknown_request_is_unsupported() {
        assert_eq!(
            answer(&Scripted::default(), "item/permissions/requestApproval", json!({})).await,
            Err(RequestError::Unsupported("item/permissions/requestApproval".to_owned()))
        );
    }
}
