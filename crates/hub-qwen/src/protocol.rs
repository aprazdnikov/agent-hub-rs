//! Pure mapping between Qwen Code's ACP messages and agent-hub types.

use std::collections::HashSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hub_agent::mcp_http::SERVER_NAME;
use hub_agent::rpc::{Notification, RequestError, RpcError};
use hub_agent::tools::{TOOL_SUMMARY_LIMIT, parse_questions};
use hub_core::domain::{
    AgentEvent, Finished, Prompt, Question, QuestionAnswer, SessionId, ToolRequest, ToolUse,
    TopicSession, Usage,
};
use hub_core::render::truncate;
use serde_json::{Map, Value, json};

pub const PROTOCOL_VERSION: u64 = 1;
/// `RequestError.authRequired` in the ACP SDK.
pub const AUTH_REQUIRED: i64 = -32000;
pub const ALLOW_ONCE: &str = "proceed_once";
pub const REJECT: &str = "cancel";
pub const PERMISSION_METHOD: &str = "session/request_permission";
pub const DRAIN_METHOD: &str = "craft/drainMidTurnQueue";
/// Qwen takes at most this many drained items and drops the rest.
pub const MAX_DRAIN_ITEMS: usize = 10;
const GENERIC_TOOL: &str = "tool";

/// Client-to-agent methods agent-hub uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Initialize,
    SessionNew,
    SessionLoad,
    SessionPrompt,
    SessionCancel,
}

impl Call {
    #[must_use]
    pub const fn method(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::SessionNew => "session/new",
            Self::SessionLoad => "session/load",
            Self::SessionPrompt => "session/prompt",
            Self::SessionCancel => "session/cancel",
        }
    }
}

/// Where this session's hub MCP server listens.
#[derive(Clone, Copy)]
pub struct HubTools<'a> {
    pub url: &'a str,
    pub token: &'a str,
}

// The token is a credential: it must never reach logs.
impl std::fmt::Debug for HubTools<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubTools").field("url", &self.url).field("token", &"***").finish()
    }
}

#[must_use]
pub fn initialize_params(version: &str) -> Value {
    // The hub answers no file-system or terminal requests; Qwen uses its own tools.
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
        "clientInfo": {"name": "agent-hub", "title": "agent-hub", "version": version},
    })
}

fn mcp_servers(tools: &HubTools<'_>) -> Value {
    json!([{
        "type": "http",
        "name": SERVER_NAME,
        "url": tools.url,
        "headers": [{"name": "Authorization", "value": format!("Bearer {}", tools.token)}],
    }])
}

/// `session/new` for a fresh topic session, `session/load` for a saved one.
#[must_use]
pub fn open_session(session: &TopicSession, tools: &HubTools<'_>) -> (Call, Value) {
    let cwd = session.cwd.as_path().display().to_string();
    match &session.session {
        None => (Call::SessionNew, json!({"cwd": cwd, "mcpServers": mcp_servers(tools)})),
        Some(saved) => (
            Call::SessionLoad,
            json!({"sessionId": saved.as_str(), "cwd": cwd, "mcpServers": mcp_servers(tools)}),
        ),
    }
}

pub fn parse_session_id(result: &Value) -> Result<SessionId, RpcError> {
    result
        .get("sessionId")
        .and_then(Value::as_str)
        .and_then(SessionId::parse)
        .ok_or_else(|| RpcError::Protocol(format!("session/new without a session id: {result}")))
}

#[must_use]
pub fn prompt_params(session: &SessionId, prompt: &Prompt) -> Value {
    json!({"sessionId": session.as_str(), "prompt": prompt_blocks(prompt)})
}

#[must_use]
pub fn cancel_params(session: &SessionId) -> Value {
    json!({"sessionId": session.as_str()})
}

/// Each photo as an image block, then the text if there is any.
#[must_use]
pub fn prompt_blocks(prompt: &Prompt) -> Value {
    let images = prompt.images().iter().map(|image| {
        json!({"type": "image", "mimeType": image.media.mime(), "data": STANDARD.encode(&image.data)})
    });
    let text =
        (!prompt.text().trim().is_empty()).then(|| json!({"type": "text", "text": prompt.text()}));
    Value::Array(images.chain(text).collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    Cancelled,
    Other(String),
}

pub fn parse_stop_reason(result: &Value) -> Result<StopReason, RpcError> {
    match result.get("stopReason").and_then(Value::as_str) {
        Some("end_turn") => Ok(StopReason::EndTurn),
        Some("cancelled") => Ok(StopReason::Cancelled),
        Some(other) => Ok(StopReason::Other(other.to_owned())),
        None => Err(RpcError::Protocol(format!("session/prompt without a stop reason: {result}"))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionAsk {
    Tool(ToolRequest),
    Questions(Vec<Question>),
}

/// Qwen rejects an option it did not offer, so both answers the hub gives must be on the list.
pub fn parse_permission(params: &Value) -> Result<PermissionAsk, RequestError> {
    let malformed = |what: &str| RequestError::Malformed(format!("{what}: {params}"));
    let offered: Vec<&str> = params
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| option.get("optionId").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    if !offered.contains(&ALLOW_ONCE) || !offered.contains(&REJECT) {
        return Err(malformed("permission request without proceed_once and cancel"));
    }
    let call = params
        .get("toolCall")
        .filter(|call| call.is_object())
        .ok_or_else(|| malformed("permission request without a tool call"))?;
    if call.pointer("/_meta/qwenInteractionKind").and_then(Value::as_str) == Some("user_question") {
        let raw = call.pointer("/_meta/qwenQuestions").cloned().unwrap_or(Value::Null);
        return parse_questions(&json!({"questions": raw}))
            .map(PermissionAsk::Questions)
            .ok_or_else(|| malformed("question without valid qwenQuestions"));
    }
    let ToolUse { tool, summary } = tool_use(call);
    Ok(PermissionAsk::Tool(ToolRequest { tool, summary }))
}

#[must_use]
pub fn selected(option: &str) -> Value {
    json!({"outcome": {"outcome": "selected", "optionId": option}})
}

/// Qwen reads answers keyed by the question's index, as a decimal string.
#[must_use]
pub fn answered(questions: &[Question], answers: &[QuestionAnswer]) -> Value {
    let by_index: Map<String, Value> = questions
        .iter()
        .enumerate()
        .filter_map(|(index, question)| {
            answers
                .iter()
                .find(|answer| answer.question == question.text())
                .map(|answer| (index.to_string(), Value::from(answer.answer.as_str())))
        })
        .collect();
    json!({"outcome": {"outcome": "selected", "optionId": ALLOW_ONCE}, "answers": by_index})
}

/// Prompts left in the inbox start the next turn, so Qwen never holds a queued one.
#[must_use]
pub fn drain_reply(prompts: &[Prompt]) -> Value {
    let items: Vec<Value> =
        prompts.iter().map(|prompt| json!({"content": prompt_blocks(prompt)})).collect();
    json!({"items": items, "hasQueuedPrompt": false})
}

fn tool_use(call: &Value) -> ToolUse {
    let tool = call
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or(GENERIC_TOOL);
    let summary = call
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
        .map_or_else(
            || call.get("rawInput").map_or_else(|| "{}".to_owned(), Value::to_string),
            str::to_owned,
        );
    ToolUse { tool: tool.to_owned(), summary: truncate(&summary, TOOL_SUMMARY_LIMIT) }
}

fn meta_text<'a>(update: &'a Value, key: &str) -> Option<&'a str> {
    update.get("_meta").and_then(|meta| meta.get(key)).and_then(Value::as_str)
}

/// `session/update` of one session → hub events; remembers the latest token total.
pub struct TurnTracker {
    session: SessionId,
    text: String,
    tokens: Option<u64>,
    /// Calls announced before their arguments arrived; shown once their title does.
    preparing: HashSet<String>,
}

impl TurnTracker {
    #[must_use]
    pub fn new(session: SessionId) -> Self {
        Self { session, text: String::new(), tokens: None, preparing: HashSet::new() }
    }

    pub fn translate(&mut self, notification: &Notification) -> Vec<AgentEvent> {
        let params = &notification.params;
        let ours = params.get("sessionId").and_then(Value::as_str) == Some(self.session.as_str());
        let Some(update) =
            params.get("update").filter(|_| ours && notification.method == "session/update")
        else {
            return Vec::new();
        };
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                self.chunk(update);
                Vec::new()
            }
            Some("tool_call") => self.tool_call(update),
            Some("tool_call_update") => self.tool_call_update(update),
            Some(_) | None => Vec::new(),
        }
    }

    fn chunk(&mut self, update: &Value) {
        // A subagent's text and usage describe its own context, not the answer.
        if update.pointer("/_meta/parentToolCallId").is_some() {
            return;
        }
        // Qwen writes 0 when the provider reported nothing.
        if let Some(total) = update
            .pointer("/_meta/usage/totalTokens")
            .and_then(Value::as_u64)
            .filter(|total| *total > 0)
        {
            self.tokens = Some(total);
        }
        if update.pointer("/content/type").and_then(Value::as_str) == Some("text")
            && let Some(text) = update.pointer("/content/text").and_then(Value::as_str)
        {
            self.text.push_str(text);
        }
    }

    fn tool_call(&mut self, update: &Value) -> Vec<AgentEvent> {
        let Some(id) = update.get("toolCallId").and_then(Value::as_str) else {
            return Vec::new();
        };
        if meta_text(update, "phase") == Some("preparing") {
            self.preparing.insert(id.to_owned());
            return Vec::new();
        }
        self.preparing.remove(id);
        self.announce(update)
    }

    fn tool_call_update(&mut self, update: &Value) -> Vec<AgentEvent> {
        let Some(id) = update.get("toolCallId").and_then(Value::as_str) else {
            return Vec::new();
        };
        if !self.preparing.contains(id) {
            return Vec::new();
        }
        if update.pointer("/_meta/preparationDiscarded") == Some(&Value::Bool(true)) {
            self.preparing.remove(id);
            return Vec::new();
        }
        let titled = update
            .get("title")
            .and_then(Value::as_str)
            .is_some_and(|title| !title.trim().is_empty());
        if !titled || meta_text(update, "phase") == Some("preparing") {
            return Vec::new();
        }
        self.preparing.remove(id);
        self.announce(update)
    }

    fn announce(&mut self, call: &Value) -> Vec<AgentEvent> {
        let mut events = self.flush();
        events.push(AgentEvent::ToolCall(tool_use(call)));
        events
    }

    fn flush(&mut self) -> Vec<AgentEvent> {
        let text = std::mem::take(&mut self.text);
        if text.trim().is_empty() { Vec::new() } else { vec![AgentEvent::AssistantText(text)] }
    }

    /// The turn's remaining text, then how it ended.
    pub fn finish(&mut self, stop: StopReason) -> Vec<AgentEvent> {
        self.preparing.clear();
        let mut events = self.flush();
        events.push(match stop {
            StopReason::EndTurn => AgentEvent::Finished(Finished {
                session: self.session.clone(),
                usage: Usage::Qwen { tokens: self.tokens },
                background: 0,
            }),
            StopReason::Cancelled => AgentEvent::Failed("Ход Qwen прерван".to_owned()),
            StopReason::Other(reason) => {
                AgentEvent::Failed(format!("Ход Qwen остановлен: {reason}"))
            }
        });
        events
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hub_core::domain::{
        AbsolutePath, BackendKind, Image, ImageMediaType, QuestionOption, Selection,
    };
    use rstest::rstest;

    use super::*;

    fn cwd() -> AbsolutePath {
        AbsolutePath::new(PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" })).unwrap()
    }

    fn session() -> SessionId {
        SessionId::parse("s-1").unwrap()
    }

    fn tools() -> HubTools<'static> {
        HubTools { url: "http://127.0.0.1:4321/mcp", token: "t0k3n" }
    }

    fn text(text: &str) -> Prompt {
        Prompt::new(text.to_owned(), Vec::new()).unwrap()
    }

    fn call(name: &str) -> (Call, Value) {
        let fresh = TopicSession::fresh(BackendKind::Qwen, cwd());
        match name {
            "initialize" => (Call::Initialize, initialize_params("test")),
            "session_new" => open_session(&fresh, &tools()),
            "session_load" => open_session(&fresh.with_session(Some(session())), &tools()),
            "session_prompt" => {
                let image = Image { media: ImageMediaType::Png, data: vec![1, 2, 3] };
                let prompt = Prompt::new("что на фото?".to_owned(), vec![image]).unwrap();
                (Call::SessionPrompt, prompt_params(&session(), &prompt))
            }
            "session_cancel" => (Call::SessionCancel, cancel_params(&session())),
            other => panic!("no call named {other}"),
        }
    }

    /// The working directory differs per platform; the snapshot keeps the rest exact.
    fn rendered((call, mut params): (Call, Value)) -> String {
        if let Some(cwd) = params.get_mut("cwd") {
            *cwd = json!("[cwd]");
        }
        format!("{}\n{}", call.method(), serde_json::to_string_pretty(&params).unwrap())
    }

    #[rstest]
    #[case("initialize")]
    #[case("session_new")]
    #[case("session_load")]
    #[case("session_prompt")]
    #[case("session_cancel")]
    fn calls_match_the_verified_wire_shapes(#[case] name: &str) {
        insta::assert_snapshot!(name, rendered(call(name)));
    }

    #[test]
    fn blank_text_is_not_sent() {
        let image = Image { media: ImageMediaType::Jpeg, data: vec![0] };
        let prompt = Prompt::new("  ".to_owned(), vec![image]).unwrap();
        assert_eq!(
            prompt_blocks(&prompt),
            json!([{"type": "image", "mimeType": "image/jpeg", "data": "AA=="}])
        );
    }

    #[rstest]
    #[case(json!({"sessionId": "s-1", "models": {}}), Some(session()))]
    #[case(json!({"sessionId": " "}), None)]
    #[case(json!({}), None)]
    fn session_id_is_parsed(#[case] result: Value, #[case] expected: Option<SessionId>) {
        assert_eq!(parse_session_id(&result).ok(), expected);
    }

    #[rstest]
    #[case(json!({"stopReason": "end_turn"}), Some(StopReason::EndTurn))]
    #[case(json!({"stopReason": "cancelled"}), Some(StopReason::Cancelled))]
    #[case(json!({"stopReason": "max_tokens"}), Some(StopReason::Other("max_tokens".to_owned())))]
    #[case(json!({}), None)]
    fn stop_reason_is_parsed(#[case] result: Value, #[case] expected: Option<StopReason>) {
        assert_eq!(parse_stop_reason(&result).ok(), expected);
    }

    fn update(update: Value) -> Notification {
        let mut params = json!({"sessionId": "s-1"});
        params["update"] = update;
        Notification { method: "session/update".to_owned(), params }
    }

    fn chunk(text: &str) -> Notification {
        update(
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}),
        )
    }

    fn tool(kind: &str, title: &str) -> AgentEvent {
        AgentEvent::ToolCall(ToolUse { tool: kind.to_owned(), summary: title.to_owned() })
    }

    #[rstest]
    #[case::text_waits_for_the_turn_end(vec![chunk("Прив"), chunk("ет")], vec![])]
    #[case::tool_call_flushes_text(
        vec![chunk("Смотрю"), update(json!({"sessionUpdate": "tool_call", "toolCallId": "c-1", "status": "pending", "title": "Shell: ls -la", "kind": "execute", "rawInput": {"command": "ls -la"}}))],
        vec![AgentEvent::AssistantText("Смотрю".to_owned()), tool("execute", "Shell: ls -la")]
    )]
    #[case::kind_is_optional(
        vec![update(json!({"sessionUpdate": "tool_call", "toolCallId": "c-1", "title": "x"}))],
        vec![tool("tool", "x")]
    )]
    #[case::untitled_call_shows_its_input(
        vec![update(json!({"sessionUpdate": "tool_call", "toolCallId": "c-1", "title": " ", "kind": "read", "rawInput": {"path": "a"}}))],
        vec![tool("read", r#"{"path":"a"}"#)]
    )]
    #[case::prepared_call_waits_for_its_title(
        vec![
            update(json!({"sessionUpdate": "tool_call", "toolCallId": "c-1", "title": "Shell", "kind": "execute", "rawInput": {}, "_meta": {"phase": "preparing"}})),
            update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c-1", "status": "pending", "title": "Shell: npm test", "kind": "execute"})),
            update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c-1", "status": "completed"})),
        ],
        vec![tool("execute", "Shell: npm test")]
    )]
    #[case::discarded_preparation_is_forgotten(
        vec![
            update(json!({"sessionUpdate": "tool_call", "toolCallId": "c-1", "title": "Shell", "_meta": {"phase": "preparing"}})),
            update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c-1", "status": "failed", "_meta": {"phase": "preparing", "preparationDiscarded": true}})),
        ],
        vec![]
    )]
    #[case::subagent_text_is_not_the_answer(
        vec![update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "внутреннее"}, "_meta": {"parentToolCallId": "c-9"}})), update(json!({"sessionUpdate": "tool_call", "toolCallId": "c-1", "title": "t", "kind": "read"}))],
        vec![tool("read", "t")]
    )]
    #[case::other_updates_are_ignored(
        vec![update(json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "думаю"}})), update(json!({"sessionUpdate": "plan", "entries": []})), update(json!({"sessionUpdate": "usage_update", "used": 1, "size": 2}))],
        vec![]
    )]
    #[case::other_sessions_are_ignored(
        vec![Notification { method: "session/update".to_owned(), params: json!({"sessionId": "s-2", "update": {"sessionUpdate": "tool_call", "toolCallId": "c", "title": "t"}}) }],
        vec![]
    )]
    fn updates_become_events(
        #[case] incoming: Vec<Notification>,
        #[case] expected: Vec<AgentEvent>,
    ) {
        let mut tracker = TurnTracker::new(session());
        let events: Vec<_> = incoming.iter().flat_map(|n| tracker.translate(n)).collect();
        assert_eq!(events, expected);
    }

    fn usage(total: u64, parent: Option<&str>) -> Notification {
        let mut meta =
            json!({"usage": {"inputTokens": 1, "outputTokens": 1, "totalTokens": total}});
        if let Some(parent) = parent {
            meta["parentToolCallId"] = json!(parent);
        }
        update(
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": ""}, "_meta": meta}),
        )
    }

    #[test]
    fn finished_turn_reports_text_then_the_last_main_token_total() {
        let mut tracker = TurnTracker::new(session());
        for notification in [
            chunk("Готово"),
            usage(41, None),
            usage(12_345, None),
            usage(7, Some("c-9")),
            usage(0, None),
        ] {
            tracker.translate(&notification);
        }
        assert_eq!(
            tracker.finish(StopReason::EndTurn),
            [
                AgentEvent::AssistantText("Готово".to_owned()),
                AgentEvent::Finished(Finished {
                    session: session(),
                    usage: Usage::Qwen { tokens: Some(12_345) },
                    background: 0,
                }),
            ]
        );
    }

    #[rstest]
    #[case(StopReason::Cancelled, "Ход Qwen прерван")]
    #[case(StopReason::Other("max_tokens".to_owned()), "Ход Qwen остановлен: max_tokens")]
    fn unsuccessful_turns_fail(#[case] stop: StopReason, #[case] reason: &str) {
        assert_eq!(
            TurnTracker::new(session()).finish(stop),
            [AgentEvent::Failed(reason.to_owned())]
        );
    }

    fn options() -> Value {
        json!([
            {"optionId": "proceed_always_project", "name": "Always", "kind": "allow_always"},
            {"optionId": "proceed_once", "name": "Allow", "kind": "allow_once"},
            {"optionId": "cancel", "name": "Reject", "kind": "reject_once"},
        ])
    }

    fn question(text: &str, header: &str) -> Question {
        Question::new(
            text.to_owned(),
            header.to_owned(),
            vec![
                QuestionOption {
                    label: "Postgres".to_owned(), description: "надёжно".to_owned()
                },
                QuestionOption {
                    label: "SQLite".to_owned(), description: "просто".to_owned()
                },
            ],
            Selection::Single,
        )
        .unwrap()
    }

    fn qwen_questions() -> Value {
        json!([{"question": "Какую БД?", "header": "БД", "multiSelect": false, "options": [
            {"label": "Postgres", "description": "надёжно"}, {"label": "SQLite", "description": "просто"}
        ]}])
    }

    #[rstest]
    #[case::shell(
        json!({"sessionId": "s-1", "options": options(), "toolCall": {"toolCallId": "c-1", "status": "pending", "title": "npm test", "kind": "execute", "rawInput": {"command": "npm test"}, "_meta": {"toolName": "run_shell_command"}}}),
        Ok(PermissionAsk::Tool(ToolRequest { tool: "execute".to_owned(), summary: "npm test".to_owned() }))
    )]
    #[case::hub_send_file(
        json!({"sessionId": "s-1", "options": options(), "toolCall": {"toolCallId": "c-2", "title": "", "kind": "other", "rawInput": {"path": "a.txt"}}}),
        Ok(PermissionAsk::Tool(ToolRequest { tool: "other".to_owned(), summary: r#"{"path":"a.txt"}"#.to_owned() }))
    )]
    #[case::question(
        json!({"sessionId": "s-1", "options": [{"optionId": "proceed_once", "name": "Submit", "kind": "allow_once"}, {"optionId": "cancel", "name": "Cancel", "kind": "reject_once"}], "toolCall": {"toolCallId": "c-3", "title": "Ask user 1 question", "kind": "think", "_meta": {"toolName": "ask_user_question", "qwenInteractionKind": "user_question", "qwenQuestions": qwen_questions()}}}),
        Ok(PermissionAsk::Questions(vec![question("Какую БД?", "БД")]))
    )]
    fn permission_requests_are_parsed(
        #[case] params: Value,
        #[case] expected: Result<PermissionAsk, RequestError>,
    ) {
        assert_eq!(parse_permission(&params), expected);
    }

    #[rstest]
    #[case::no_allow_once(json!({"options": [{"optionId": "cancel"}], "toolCall": {"title": "x"}}))]
    #[case::no_tool_call(json!({"options": options()}))]
    #[case::question_without_questions(json!({"options": options(), "toolCall": {"_meta": {"qwenInteractionKind": "user_question", "qwenQuestions": []}}}))]
    #[case::question_without_text(json!({"options": options(), "toolCall": {"_meta": {"qwenInteractionKind": "user_question", "qwenQuestions": [{"header": "h", "options": []}]}}}))]
    fn malformed_permission_requests_are_rejected(#[case] params: Value) {
        assert!(matches!(parse_permission(&params), Err(RequestError::Malformed(_))));
    }

    #[test]
    fn answers_are_keyed_by_question_index() {
        let asked = [question("Какую БД?", "БД"), question("Где хостить?", "Хост")];
        let answers = [
            QuestionAnswer {
                question: "Где хостить?".to_owned(), answer: "дома".to_owned()
            },
            QuestionAnswer {
                question: "Какую БД?".to_owned(),
                answer: "Postgres, SQLite".to_owned(),
            },
        ];
        assert_eq!(
            answered(&asked, &answers),
            json!({"outcome": {"outcome": "selected", "optionId": "proceed_once"},
                   "answers": {"0": "Postgres, SQLite", "1": "дома"}})
        );
    }

    #[test]
    fn drain_reply_carries_each_prompt_as_an_item() {
        assert_eq!(
            drain_reply(&[text("ещё"), text("и это")]),
            json!({"items": [
                {"content": [{"type": "text", "text": "ещё"}]},
                {"content": [{"type": "text", "text": "и это"}]},
            ], "hasQueuedPrompt": false})
        );
    }
}
