//! Pure mapping between Codex app-server messages and agent-hub types.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hub_agent::tools::{
    ASK_USER, ASK_USER_DESCRIPTION, SEND_FILE, SEND_FILE_DESCRIPTION, TOOL_SUMMARY_LIMIT,
    ask_user_schema, send_file_schema,
};
use hub_core::domain::{
    AgentEvent, Finished, Image, Prompt, SessionId, ToolUse, TopicSession, Usage,
};
use hub_core::render::truncate;
use hub_core::settings::{ApiKey, CodexSettings};
use serde_json::{Map, Value, json};

use hub_agent::rpc::{Notification, RpcError};

// Approvals go to the human in Telegram, never to Codex's own reviewer agent.
const APPROVALS_REVIEWER: &str = "user";
pub const SHELL: &str = "shell";
pub const PATCH: &str = "patch";
pub const WEB_SEARCH: &str = "web_search";
pub const UNNAMED_FILE_CHANGE: &str = "изменение файлов";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnId(String);

impl TurnId {
    #[must_use]
    pub fn new(id: String) -> Self {
        Self(id)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Client-to-server methods agent-hub uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Initialize,
    Initialized,
    AccountRead,
    Login,
    ThreadStart,
    ThreadResume,
    TurnStart,
    TurnSteer,
}

impl Call {
    #[must_use]
    pub const fn method(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::Initialized => "initialized",
            Self::AccountRead => "account/read",
            Self::Login => "account/login/start",
            Self::ThreadStart => "thread/start",
            Self::ThreadResume => "thread/resume",
            Self::TurnStart => "turn/start",
            Self::TurnSteer => "turn/steer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAuth {
    ApiKey,
    ChatGpt,
    Other,
    NotRequired,
    Missing,
}

#[derive(Debug, Default, PartialEq)]
pub struct Translation {
    pub events: Vec<AgentEvent>,
    /// The turn this notification completed, if any.
    pub completed: Option<TurnId>,
}

impl Translation {
    fn of(event: Option<AgentEvent>) -> Self {
        Self { events: event.into_iter().collect(), completed: None }
    }
}

#[must_use]
pub fn initialize_params() -> Value {
    // Dynamic tools are part of the experimental app-server API.
    json!({
        "clientInfo": {"name": "agent-hub", "title": "agent-hub", "version": env!("CARGO_PKG_VERSION")},
        "capabilities": {"experimentalApi": true},
    })
}

#[must_use]
pub fn login_params(key: &ApiKey) -> Value {
    json!({"type": "apiKey", "apiKey": key.expose()})
}

#[must_use]
pub fn auth_state(result: &Value) -> CodexAuth {
    match result.pointer("/account/type").and_then(Value::as_str) {
        Some("apiKey") => CodexAuth::ApiKey,
        Some("chatgpt") => CodexAuth::ChatGpt,
        Some(_) => CodexAuth::Other,
        None if result.get("requiresOpenaiAuth") == Some(&Value::Bool(false)) => {
            CodexAuth::NotRequired
        }
        None => CodexAuth::Missing,
    }
}

#[must_use]
pub fn hub_tools() -> Value {
    json!([
        {"type": "function", "name": SEND_FILE, "description": SEND_FILE_DESCRIPTION, "inputSchema": send_file_schema()},
        {"type": "function", "name": ASK_USER, "description": ASK_USER_DESCRIPTION, "inputSchema": ask_user_schema()},
    ])
}

/// `thread/start` for a new topic session, `thread/resume` for a saved one.
#[must_use]
pub fn open_thread(session: &TopicSession, settings: &CodexSettings) -> (Call, Value) {
    let mut params = Map::new();
    params.insert("cwd".to_owned(), Value::from(session.cwd.as_path().display().to_string()));
    params.insert("sandbox".to_owned(), Value::from(settings.sandbox.wire()));
    params.insert("approvalPolicy".to_owned(), Value::from(settings.approval.wire()));
    params.insert("approvalsReviewer".to_owned(), Value::from(APPROVALS_REVIEWER));
    if let Some(model) = &settings.model {
        params.insert("model".to_owned(), Value::from(model.as_str()));
    }
    match &session.session {
        None => {
            params.insert("dynamicTools".to_owned(), hub_tools());
            (Call::ThreadStart, Value::Object(params))
        }
        Some(thread) => {
            // A resumed thread keeps the dynamic tools it was started with.
            params.insert("threadId".to_owned(), Value::from(thread.as_str()));
            (Call::ThreadResume, Value::Object(params))
        }
    }
}

pub fn parse_thread_id(result: &Value) -> Result<SessionId, RpcError> {
    result
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .and_then(SessionId::parse)
        .ok_or_else(|| RpcError::Protocol(format!("thread response without an id: {result}")))
}

pub fn parse_turn_id(result: &Value) -> Result<TurnId, RpcError> {
    result
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .map(|id| TurnId(id.to_owned()))
        .ok_or_else(|| RpcError::Protocol(format!("turn response without an id: {result}")))
}

#[must_use]
pub fn turn_params(thread: &SessionId, prompt: &Prompt) -> Value {
    json!({"threadId": thread.as_str(), "input": user_input(prompt)})
}

#[must_use]
pub fn steer_params(thread: &SessionId, turn: &TurnId, prompt: &Prompt) -> Value {
    json!({"threadId": thread.as_str(), "expectedTurnId": turn.as_str(), "input": user_input(prompt)})
}

#[must_use]
pub fn user_input(prompt: &Prompt) -> Value {
    let images =
        prompt.images().iter().map(|image| json!({"type": "image", "url": data_url(image)}));
    let text =
        (!prompt.text().trim().is_empty()).then(|| json!({"type": "text", "text": prompt.text()}));
    Value::Array(images.chain(text).collect())
}

fn data_url(image: &Image) -> String {
    format!("data:{};base64,{}", image.media.mime(), STANDARD.encode(&image.data))
}

#[must_use]
pub fn tool_call(item: &Value) -> Option<ToolUse> {
    let text = |key: &str| item.get(key).and_then(Value::as_str);
    let line = |tool: &str, summary: &str| ToolUse {
        tool: tool.to_owned(),
        summary: truncate(summary, TOOL_SUMMARY_LIMIT),
    };
    match text("type")? {
        "commandExecution" => text("command").map(|command| line(SHELL, command)),
        "fileChange" => {
            let paths: Vec<&str> = item
                .get("changes")?
                .as_array()?
                .iter()
                .filter_map(|change| change.get("path").and_then(Value::as_str))
                .collect();
            let summary =
                if paths.is_empty() { UNNAMED_FILE_CHANGE.to_owned() } else { paths.join(", ") };
            Some(line(PATCH, &summary))
        }
        "mcpToolCall" => {
            let name = format!("{}/{}", text("server")?, text("tool")?);
            let arguments = item.get("arguments").map_or_else(|| "{}".to_owned(), Value::to_string);
            Some(line(&name, &arguments))
        }
        "webSearch" => text("query").map(|query| line(WEB_SEARCH, query)),
        _ => None,
    }
}

#[must_use]
pub fn agent_text(item: &Value) -> Option<String> {
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => item
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned),
        Some(_) | None => None,
    }
}

/// Notifications of one thread → hub events; remembers the thread's token use.
pub struct TurnTracker {
    thread: SessionId,
    tokens: Option<u64>,
}

impl TurnTracker {
    #[must_use]
    pub fn new(thread: SessionId) -> Self {
        Self { thread, tokens: None }
    }

    pub fn translate(&mut self, notification: &Notification) -> Result<Translation, RpcError> {
        let params = &notification.params;
        match notification.method.as_str() {
            "item/started" => Ok(Translation::of(
                params.get("item").and_then(tool_call).map(AgentEvent::ToolCall),
            )),
            "item/completed" => Ok(Translation::of(
                params.get("item").and_then(agent_text).map(AgentEvent::AssistantText),
            )),
            "thread/tokenUsage/updated" => {
                if let Some(total) =
                    params.pointer("/tokenUsage/total/totalTokens").and_then(Value::as_u64)
                {
                    self.tokens = Some(total);
                }
                Ok(Translation::default())
            }
            "turn/completed" => self.completed(params),
            _ => Ok(Translation::default()),
        }
    }

    fn completed(&self, params: &Value) -> Result<Translation, RpcError> {
        let turn = params.get("turn").unwrap_or(&Value::Null);
        let id = turn.get("id").and_then(Value::as_str).ok_or_else(|| {
            RpcError::Protocol(format!("turn/completed without a turn id: {params}"))
        })?;
        let status = turn.get("status").and_then(Value::as_str);
        let error = turn.pointer("/error/message").and_then(Value::as_str);
        let event = match (status, error) {
            (Some("completed"), _) => AgentEvent::Finished(Finished {
                session: self.thread.clone(),
                usage: Usage::Codex { tokens: self.tokens },
                background: 0,
            }),
            (Some("interrupted"), _) => AgentEvent::Failed("Ход Codex прерван".to_owned()),
            (_, Some(message)) => AgentEvent::Failed(message.to_owned()),
            (_, None) => AgentEvent::Failed("Ход Codex завершился ошибкой".to_owned()),
        };
        Ok(Translation { events: vec![event], completed: Some(TurnId(id.to_owned())) })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hub_core::domain::{AbsolutePath, BackendKind, ImageMediaType};
    use hub_core::settings::{Approval, Sandbox};
    use rstest::rstest;

    use super::*;

    fn cwd() -> AbsolutePath {
        AbsolutePath::new(PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" })).unwrap()
    }

    fn settings(model: Option<&str>) -> CodexSettings {
        CodexSettings {
            cli: None,
            model: model.map(str::to_owned),
            sandbox: Sandbox::WorkspaceWrite,
            approval: Approval::OnRequest,
            api_key: None,
        }
    }

    fn notification(method: &str, params: Value) -> Notification {
        Notification { method: method.to_owned(), params }
    }

    fn thread() -> SessionId {
        SessionId::parse("t-1").unwrap()
    }

    fn turn(id: &str) -> TurnId {
        TurnId::new(id.to_owned())
    }

    #[test]
    fn new_session_starts_a_thread_with_hub_tools() {
        let session = TopicSession::fresh(BackendKind::Codex, cwd());
        assert_eq!(
            open_thread(&session, &settings(Some("gpt-5.5-codex"))),
            (
                Call::ThreadStart,
                json!({
                    "cwd": cwd().as_path().display().to_string(),
                    "sandbox": "workspace-write",
                    "approvalPolicy": "on-request",
                    "approvalsReviewer": "user",
                    "model": "gpt-5.5-codex",
                    "dynamicTools": hub_tools(),
                })
            )
        );
    }

    #[test]
    fn saved_session_resumes_its_thread_without_tools() {
        let session = TopicSession::fresh(BackendKind::Codex, cwd()).with_session(Some(thread()));
        assert_eq!(
            open_thread(&session, &settings(None)),
            (
                Call::ThreadResume,
                json!({
                    "cwd": cwd().as_path().display().to_string(),
                    "sandbox": "workspace-write",
                    "approvalPolicy": "on-request",
                    "approvalsReviewer": "user",
                    "threadId": "t-1",
                })
            )
        );
    }

    #[test]
    fn hub_tools_are_send_file_and_ask_user() {
        let names: Vec<_> = hub_tools()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        assert_eq!(names, ["send_file", "ask_user"]);
    }

    #[test]
    fn images_go_first_as_data_urls() {
        let image = Image { media: ImageMediaType::Png, data: vec![1, 2, 3] };
        let prompt = Prompt::new("что на фото?".to_owned(), vec![image]).unwrap();
        assert_eq!(
            user_input(&prompt),
            json!([
                {"type": "image", "url": "data:image/png;base64,AQID"},
                {"type": "text", "text": "что на фото?"},
            ])
        );
    }

    #[test]
    fn blank_text_is_not_sent() {
        let image = Image { media: ImageMediaType::Jpeg, data: vec![0] };
        let prompt = Prompt::new("  ".to_owned(), vec![image]).unwrap();
        assert_eq!(
            user_input(&prompt),
            json!([{"type": "image", "url": "data:image/jpeg;base64,AA=="}])
        );
    }

    #[test]
    fn steer_names_the_expected_turn() {
        let prompt = Prompt::new("ещё".to_owned(), Vec::new()).unwrap();
        assert_eq!(
            steer_params(&thread(), &turn("u-1"), &prompt),
            json!({"threadId": "t-1", "expectedTurnId": "u-1", "input": [{"type": "text", "text": "ещё"}]})
        );
    }

    #[rstest]
    #[case(json!({"account": {"type": "apiKey"}}), CodexAuth::ApiKey)]
    #[case(json!({"account": {"type": "chatgpt"}}), CodexAuth::ChatGpt)]
    #[case(json!({"account": {"type": "enterprise"}}), CodexAuth::Other)]
    #[case(json!({"account": null, "requiresOpenaiAuth": false}), CodexAuth::NotRequired)]
    #[case(json!({"account": null, "requiresOpenaiAuth": true}), CodexAuth::Missing)]
    #[case(json!({}), CodexAuth::Missing)]
    fn auth_state_is_read(#[case] result: Value, #[case] expected: CodexAuth) {
        assert_eq!(auth_state(&result), expected);
    }

    #[rstest]
    #[case(json!({"thread": {"id": "t-1"}}), Some(thread()))]
    #[case(json!({"thread": {}}), None)]
    fn thread_id_is_parsed(#[case] result: Value, #[case] expected: Option<SessionId>) {
        assert_eq!(parse_thread_id(&result).ok(), expected);
    }

    #[rstest]
    #[case(json!({"turn": {"id": "u-1"}}), Some(turn("u-1")))]
    #[case(json!({"turn": null}), None)]
    fn turn_id_is_parsed(#[case] result: Value, #[case] expected: Option<TurnId>) {
        assert_eq!(parse_turn_id(&result).ok(), expected);
    }

    #[rstest]
    #[case(json!({"type": "commandExecution", "command": "npm test"}), Some(("shell", "npm test")))]
    #[case(json!({"type": "fileChange", "changes": [{"path": "a.rs"}, {"path": "b.rs"}]}), Some(("patch", "a.rs, b.rs")))]
    #[case(json!({"type": "fileChange", "changes": []}), Some(("patch", "изменение файлов")))]
    #[case(json!({"type": "mcpToolCall", "server": "gh", "tool": "pr", "arguments": {"n": 1}}), Some(("gh/pr", r#"{"n":1}"#)))]
    #[case(json!({"type": "mcpToolCall", "server": "gh", "tool": "pr"}), Some(("gh/pr", "{}")))]
    #[case(json!({"type": "webSearch", "query": "rust"}), Some(("web_search", "rust")))]
    #[case(json!({"type": "reasoning"}), None)]
    #[case(json!({"type": "dynamicToolCall", "tool": "send_file"}), None)]
    fn started_items_become_tool_lines(
        #[case] item: Value,
        #[case] expected: Option<(&str, &str)>,
    ) {
        let expected = expected
            .map(|(tool, summary)| ToolUse { tool: tool.to_owned(), summary: summary.to_owned() });
        assert_eq!(tool_call(&item), expected);
    }

    #[test]
    fn long_commands_are_truncated() {
        let command = "x".repeat(1000);
        let call = tool_call(&json!({"type": "commandExecution", "command": command})).unwrap();
        assert_eq!(call.summary.chars().count(), 600);
    }

    #[test]
    fn a_completed_turn_reports_the_latest_token_total() {
        let mut tracker = TurnTracker::new(thread());
        for total in [41, 12345] {
            let usage = json!({"tokenUsage": {"total": {"totalTokens": total}}});
            tracker.translate(&notification("thread/tokenUsage/updated", usage)).unwrap();
        }
        let completed = json!({"turn": {"id": "u-1", "status": "completed"}});
        assert_eq!(
            tracker.translate(&notification("turn/completed", completed)).unwrap(),
            Translation {
                events: vec![AgentEvent::Finished(Finished {
                    session: thread(),
                    usage: Usage::Codex { tokens: Some(12345) },
                    background: 0,
                })],
                completed: Some(turn("u-1")),
            }
        );
    }

    #[rstest]
    #[case(json!({"id": "u-1", "status": "interrupted"}), "Ход Codex прерван")]
    #[case(json!({"id": "u-1", "status": "failed", "error": {"message": "quota exceeded"}}), "quota exceeded")]
    #[case(json!({"id": "u-1", "status": "failed"}), "Ход Codex завершился ошибкой")]
    fn an_unsuccessful_turn_fails(#[case] ended: Value, #[case] reason: &str) {
        let translation = TurnTracker::new(thread())
            .translate(&notification("turn/completed", json!({"turn": ended})))
            .unwrap();
        assert_eq!(
            translation,
            Translation {
                events: vec![AgentEvent::Failed(reason.to_owned())],
                completed: Some(turn("u-1"))
            }
        );
    }

    #[test]
    fn turn_completed_without_an_id_is_a_protocol_error() {
        let outcome = TurnTracker::new(thread())
            .translate(&notification("turn/completed", json!({"turn": {}})));
        assert!(matches!(outcome, Err(RpcError::Protocol(_))));
    }

    #[rstest]
    #[case(notification("item/completed", json!({"item": {"type": "agentMessage", "text": "Готово"}})), vec![AgentEvent::AssistantText("Готово".to_owned())])]
    #[case(notification("item/completed", json!({"item": {"type": "agentMessage", "text": " "}})), vec![])]
    #[case(notification("item/started", json!({"item": {"type": "webSearch", "query": "q"}})), vec![AgentEvent::ToolCall(ToolUse { tool: "web_search".to_owned(), summary: "q".to_owned() })])]
    #[case(notification("item/agentMessage/delta", json!({"delta": "Го"})), vec![])]
    fn notifications_become_events(
        #[case] incoming: Notification,
        #[case] events: Vec<AgentEvent>,
    ) {
        let translation = TurnTracker::new(thread()).translate(&incoming).unwrap();
        assert_eq!(translation, Translation { events, completed: None });
    }
}
