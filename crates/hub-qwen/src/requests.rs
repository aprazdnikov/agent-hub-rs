//! Requests Qwen sends the client: approvals, its own clarifying questions, and the drain.

use hub_agent::channel::UserChannel;
use hub_agent::rpc::RequestError;
use hub_core::domain::{Decision, Question, QuestionsOutcome, ToolRequest};
use serde_json::Value;

use crate::protocol::{
    ALLOW_ONCE, DRAIN_METHOD, PERMISSION_METHOD, PermissionAsk, REJECT, answered, drain_reply,
    parse_permission, selected,
};
use crate::session::Drain;

/// Replies to one Qwen request on behalf of the human behind `channel`.
///
/// # Errors
/// [`RequestError::Malformed`] for a permission request of the wrong shape (never an approval),
/// [`RequestError::Unsupported`] for any method this client does not serve.
pub async fn answer(
    channel: &dyn UserChannel,
    drain: &Drain,
    method: &str,
    params: Value,
) -> Result<Value, RequestError> {
    match method {
        PERMISSION_METHOD => Ok(match parse_permission(&params)? {
            PermissionAsk::Tool(tool) => approve(channel, tool).await,
            PermissionAsk::Questions(questions) => ask(channel, questions).await,
        }),
        DRAIN_METHOD => Ok(drain_reply(&drain.take().await)),
        other => Err(RequestError::Unsupported(other.to_owned())),
    }
}

/// «Allow always» is never chosen: every call goes to the human again.
async fn approve(channel: &dyn UserChannel, tool: ToolRequest) -> Value {
    match channel.request(tool).await {
        Decision::Allowed => selected(ALLOW_ONCE),
        Decision::Denied(_) => selected(REJECT),
    }
}

async fn ask(channel: &dyn UserChannel, questions: Vec<Question>) -> Value {
    match channel.ask(questions.clone()).await {
        QuestionsOutcome::Answered(answers) => answered(&questions, &answers),
        // Qwen tells the model the user declined to answer.
        QuestionsOutcome::Denied(_) => selected(REJECT),
    }
}

#[cfg(test)]
mod tests {
    use hub_agent::rpc::RequestError;
    use hub_core::domain::{Decision, Denied, QuestionAnswer, QuestionsOutcome, ToolRequest};
    use rstest::rstest;
    use serde_json::{Value, json};

    use super::*;
    use crate::session::drain_channel;
    use crate::testing::Scripted;

    fn offered() -> Value {
        json!([
            {"optionId": "proceed_always_project", "name": "Always", "kind": "allow_always"},
            {"optionId": "proceed_once", "name": "Allow", "kind": "allow_once"},
            {"optionId": "cancel", "name": "Reject", "kind": "reject_once"},
        ])
    }

    fn shell() -> Value {
        json!({"sessionId": "s-1", "options": offered(), "toolCall": {
            "toolCallId": "c-1", "status": "pending", "title": "rm -rf build", "kind": "execute",
            "rawInput": {"command": "rm -rf build"}}})
    }

    fn question() -> Value {
        json!({"sessionId": "s-1", "options": offered(), "toolCall": {
            "toolCallId": "c-2", "title": "Ask user 1 question", "kind": "think",
            "_meta": {"qwenInteractionKind": "user_question", "qwenQuestions": [
                {"question": "Цвет?", "header": "Цвет", "options": [
                    {"label": "синий", "description": "спокойный"}, {"label": "красный", "description": "яркий"}]}]}}})
    }

    async fn answer_with(
        channel: &Scripted,
        method: &str,
        params: Value,
    ) -> Result<Value, RequestError> {
        let (drain, _requests) = drain_channel();
        answer(channel, &drain, method, params).await
    }

    #[rstest]
    #[case::allowed(Decision::Allowed, "proceed_once")]
    #[case::denied(Decision::Denied(Denied::new("нет")), "cancel")]
    #[tokio::test]
    async fn tool_approvals_ask_the_human(#[case] decision: Decision, #[case] option: &str) {
        let channel = Scripted { decision, ..Scripted::default() };
        let reply = answer_with(&channel, "session/request_permission", shell()).await;
        assert_eq!(reply, Ok(json!({"outcome": {"outcome": "selected", "optionId": option}})));
        assert_eq!(
            *channel.requests.lock().unwrap(),
            [ToolRequest { tool: "execute".to_owned(), summary: "rm -rf build".to_owned() }]
        );
    }

    #[tokio::test]
    async fn questions_are_answered_by_index() {
        let channel = Scripted {
            outcome: QuestionsOutcome::Answered(vec![QuestionAnswer {
                question: "Цвет?".to_owned(),
                answer: "синий".to_owned(),
            }]),
            ..Scripted::default()
        };
        let reply = answer_with(&channel, "session/request_permission", question()).await;
        assert_eq!(
            reply,
            Ok(
                json!({"outcome": {"outcome": "selected", "optionId": "proceed_once"}, "answers": {"0": "синий"}})
            )
        );
        assert_eq!(channel.questions.lock().unwrap().first().map(|q| q.options().len()), Some(2));
    }

    #[tokio::test]
    async fn declined_questions_cancel() {
        let channel = Scripted {
            outcome: QuestionsOutcome::Denied(Denied::new("нет ответа")),
            ..Scripted::default()
        };
        let reply = answer_with(&channel, "session/request_permission", question()).await;
        assert_eq!(reply, Ok(json!({"outcome": {"outcome": "selected", "optionId": "cancel"}})));
    }

    #[tokio::test]
    async fn malformed_permission_is_rejected_without_asking() {
        let channel = Scripted::default();
        let reply =
            answer_with(&channel, "session/request_permission", json!({"options": []})).await;
        assert!(matches!(reply, Err(RequestError::Malformed(_))));
        assert!(channel.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn drain_without_a_running_turn_hands_over_nothing() {
        let (drain, requests) = drain_channel();
        drop(requests);
        let reply = answer(
            &Scripted::default(),
            &drain,
            "craft/drainMidTurnQueue",
            json!({"sessionId": "s-1"}),
        )
        .await;
        assert_eq!(reply, Ok(json!({"items": [], "hasQueuedPrompt": false})));
    }

    #[tokio::test]
    async fn unknown_request_is_unsupported() {
        let reply = answer_with(&Scripted::default(), "fs/read_text_file", json!({})).await;
        assert_eq!(reply, Err(RequestError::Unsupported("fs/read_text_file".to_owned())));
    }
}
