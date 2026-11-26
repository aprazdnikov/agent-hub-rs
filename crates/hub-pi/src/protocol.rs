//! `Pi` RPC wire mapping, checked against the installed build 1.6.0.
//!
//! Pi speaks newline-delimited JSON in both directions: flat command objects on stdin, and
//! either a `{type:"response"}` reply or a stream event on stdout. There is no JSON-RPC 2.0
//! member and no `method`/`params`/`result`, so `hub_agent::rpc` does not fit and this module
//! owns the shapes.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hub_agent::tools::TOOL_SUMMARY_LIMIT;
use hub_core::domain::{Finished, Prompt, SessionId, ToolUse, Usage};
use hub_core::render::truncate;
use hub_core::settings::PiThinking;
use serde_json::{Value, json};

/// Tool calls the bridge intercepts when the hub runs `Pi` in the guarded approval mode.
pub const GATE_TOOLS: &str = "bash,edit,write";

/// Provider errors reach the chat as one line; the payload behind them is for the log.
pub(crate) const ERROR_LIMIT: usize = 300;

/// Environment the bridge extension reads. Values reach only the child, never argv or the file.
pub const ENV_GATE: &str = "AGENT_HUB_PI_GATE";
pub const ENV_MCP_URL: &str = "AGENT_HUB_PI_MCP_URL";
pub const ENV_MCP_TOKEN: &str = "AGENT_HUB_PI_MCP_TOKEN";
pub const ENV_MEMORY: &str = "AGENT_HUB_PI_MEMORY";
pub const BRIDGE_FILE: &str = "agent-hub-pi.mjs";

/// Said to the agent with every prompt: unlike ACP agents, `Pi` renames MCP tools by server,
/// so the guidance must use the names the model actually has.
pub const MCP_GUIDANCE: &str = "[agent-hub integration]\nFor clarifying questions use the mcp__agent_hub__ask_user tool, not a plain question in your reply: the user reads Telegram, not this terminal, and only that tool reaches them. To deliver a local file, use mcp__agent_hub__send_file. A message that arrives while you work steers this turn.\n[/agent-hub integration]";

/// The id this hub puts on a command. `Pi` repeats it on the answer, so the two must agree.
#[must_use]
pub fn request_id(next: u64) -> String {
    format!("cmd-{next}")
}

/// One command line for the agent's stdin.
#[must_use]
pub fn command(id: &str, body: &Value) -> String {
    let mut fields = body.as_object().cloned().unwrap_or_default();
    fields.insert("id".to_owned(), json!(id));
    Value::Object(fields).to_string()
}

#[must_use]
pub fn get_state() -> Value {
    json!({"type": "get_state"})
}

#[must_use]
pub fn get_available_thinking_levels() -> Value {
    json!({"type": "get_available_thinking_levels"})
}

#[must_use]
pub fn set_thinking_level(thinking: PiThinking) -> Value {
    json!({"type": "set_thinking_level", "level": thinking.wire()})
}

#[must_use]
pub fn set_model(provider: &str, model: &str) -> Value {
    json!({"type": "set_model", "provider": provider, "modelId": model})
}

#[must_use]
pub fn prompt(prompt: &Prompt) -> Value {
    let body = prompt.text();
    // Images alone still need the guidance: the model must know what the hub tools are for.
    let text = if body.trim().is_empty() {
        MCP_GUIDANCE.to_owned()
    } else {
        format!("{body}\n\n{MCP_GUIDANCE}")
    };
    with_images(json!({"type": "prompt", "message": text}), prompt)
}

#[must_use]
pub fn steer(prompt: &Prompt) -> Value {
    with_images(json!({"type": "steer", "message": prompt.text()}), prompt)
}

/// `Pi` takes inline image content next to the text, in the shape `pi-ai` names `ImageContent`.
fn with_images(mut command: Value, prompt: &Prompt) -> Value {
    let images = prompt
        .images()
        .iter()
        .map(|image| {
            json!({
                "type": "image",
                "mimeType": image.media.mime(),
                "data": STANDARD.encode(&image.data),
            })
        })
        .collect::<Vec<_>>();
    if !images.is_empty()
        && let Some(fields) = command.as_object_mut()
    {
        fields.insert("images".to_owned(), Value::Array(images));
    }
    command
}

#[must_use]
pub fn abort() -> Value {
    json!({"type": "abort"})
}

/// Flags that decide which `Pi` session the child opens. Nothing here is user text except a
/// session id the hub received from `Pi` itself and validated as `SessionId`.
///
/// The thinking level is deliberately absent: levels differ per model, and a level `Pi` rejects
/// at startup would kill the child before the hub could tell the user why. `open` sets it over
/// RPC once the model is known and the level is advertised.
#[must_use]
pub fn session_args(resume: Option<&str>, model: Option<&str>) -> Vec<String> {
    let mut args = vec!["--mode".to_owned(), "rpc".to_owned()];
    if let Some(id) = resume {
        args.extend(["--session".to_owned(), id.to_owned()]);
    }
    if let Some(model) = model {
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    args
}

/// The model the agent reports, in the same `provider/id` form the hub keeps in settings.
#[must_use]
pub fn current_model(state: &Value) -> Option<String> {
    let model = state.get("model")?;
    let provider = model.get("provider").and_then(Value::as_str)?;
    let id = model.get("id").and_then(Value::as_str)?;
    Some(format!("{provider}/{id}"))
}

#[must_use]
pub fn get_session_stats() -> Value {
    json!({"type": "get_session_stats"})
}

/// What one stdout line means to the hub. Unknown lines are `Ignored`, never an error: `Pi`
/// adds events between releases, and a new one must not end a working conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wire {
    /// A reply to a command the hub sent, correlated by `id`.
    Reply { id: String, command: String, data: Option<Value>, error: Option<String> },
    /// Assistant prose, ready for the chat.
    Text(String),
    /// A tool call the agent made.
    Tool(ToolUse),
    /// An extension asking the human something; the hub must answer or `Pi` waits for its own
    /// dialog timeout.
    Approval { id: String, summary: String },
    /// The turn is over and `Pi` is idle: the hub may report the result and take the next
    /// message. `aborted` says the user stopped it rather than the model finishing.
    Settled { aborted: bool },
    /// The agent said it will retry the turn; the answer is not final.
    Retrying,
    /// A turn the model could not complete: `Pi` keeps the process alive and reports the
    /// provider's answer inside the assistant message. Without this the user sees silence.
    Error { reason: String },
    /// Any other event or notification.
    Ignored,
    /// Output that is not a JSON object at all.
    Malformed,
}

impl Wire {
    #[must_use]
    pub fn parse(line: &str) -> Self {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return Self::Malformed;
        };
        match field(&value, "type").as_str() {
            "response" => Self::Reply {
                id: field(&value, "id"),
                command: field(&value, "command"),
                data: value.get("data").filter(|data| !data.is_null()).cloned(),
                error: (value.get("success").and_then(Value::as_bool) == Some(false))
                    .then(|| field(&value, "error")),
            },
            "extension_ui_request" if field(&value, "method") == "confirm" => {
                Self::Approval { id: field(&value, "id"), summary: field(&value, "message") }
            }
            "message_update" => message_text(&value).map_or(Self::Ignored, Self::Text),
            "message_end" => match assistant_error(&value) {
                Some(reason) => Self::Error { reason: truncate(&reason, ERROR_LIMIT) },
                None => Self::Ignored,
            },
            "tool_execution_start" => Self::Tool(tool_use(&value)),
            "agent_settled" => Self::Settled {
                aborted: value.get("aborted").and_then(Value::as_bool).unwrap_or(false),
            },
            "agent_end" if value.get("willRetry").and_then(Value::as_bool) == Some(true) => {
                Self::Retrying
            }
            _ => Self::Ignored,
        }
    }

    /// The answer line for a dialog. `Pi`'s `confirm` resolves on `confirmed`, so denying is
    /// explicit: an unanswered dialog would hang the turn until the agent's own timeout.
    #[must_use]
    pub fn approval_answer(id: &str, allowed: bool) -> String {
        json!({"type": "extension_ui_response", "id": id, "confirmed": allowed}).to_string()
    }
}

fn field(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

/// Pi marks a failed turn on the assistant message: `stopReason` says `error` and
/// `errorMessage` carries the provider's own words. It is not prose, so it never arrives as a
/// text delta — the hub would otherwise end the turn showing nothing at all.
fn assistant_error(value: &Value) -> Option<String> {
    let message = value.get("message")?;
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    let failed = message.get("stopReason").and_then(Value::as_str) == Some("error");
    let reason = message
        .get("errorMessage")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty());
    if !failed && reason.is_none() {
        return None;
    }
    Some(reason.unwrap_or("агент не выполнил ход").to_owned())
}

/// Assistant prose arrives as deltas inside `message_update`; thinking and tool-call deltas are
/// not prose and never reach the chat.
fn message_text(value: &Value) -> Option<String> {
    let event = value.get("assistantMessageEvent")?;
    (event.get("type").and_then(Value::as_str) == Some("text_delta"))
        .then(|| event.get("delta").and_then(Value::as_str).filter(|text| !text.is_empty()))
        .flatten()
        .map(str::to_owned)
}

/// The line the hub shows for a tool call: the tool and its most identifying argument.
#[must_use]
pub fn tool_use(call: &Value) -> ToolUse {
    let tool = field(call, "toolName");
    let args = call.get("args");
    let summary = ["command", "path", "pattern", "query"]
        .into_iter()
        .find_map(|key| args.and_then(|args| args.get(key)).and_then(Value::as_str))
        .map_or_else(|| args.map_or_else(|| tool.clone(), ToString::to_string), ToOwned::to_owned);
    ToolUse { tool: truncate(&tool, 128), summary: truncate(&summary, TOOL_SUMMARY_LIMIT) }
}

/// Session identity, from `get_state`. `Pi` mints the id itself, so the hub learns it here
/// rather than guessing a format.
///
/// # Errors
///
/// Returns a refusal when the reply carries no usable id.
pub fn session_id(data: &Value) -> Result<SessionId, String> {
    data.get("sessionId")
        .and_then(Value::as_str)
        .and_then(SessionId::parse)
        .ok_or_else(|| "Pi вернул состояние без идентификатора сессии".to_owned())
}

#[must_use]
pub fn advertised_levels(data: &Value) -> Vec<String> {
    data.get("levels")
        .and_then(Value::as_array)
        .map(|levels| {
            levels.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Why the requested mode cannot be applied, or `None` when the model advertises it. An empty
/// advertised list means `Pi` declared nothing, and the agent's own default rules.
#[must_use]
pub fn unsupported_mode(advertised: &[String], thinking: PiThinking) -> Option<String> {
    let level = thinking.wire();
    if advertised.is_empty() || advertised.iter().any(|available| available == level) {
        return None;
    }
    Some(format!(
        "Pi не поддерживает режим «{}»; доступны: {}",
        thinking.label(),
        advertised.join(", ")
    ))
}

/// The usage line of a finished turn. `Pi` reports tokens for the whole session, not per turn,
/// so the hub takes the running total.
#[must_use]
pub fn finished(session: SessionId, stats: Option<&Value>) -> Finished {
    Finished {
        session,
        usage: Usage::Pi {
            tokens: stats.and_then(|stats| stats.pointer("/tokens/total")).and_then(Value::as_u64),
        },
        background: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn commands_carry_their_correlation_id() {
        // Compared as JSON: `serde_json` orders object keys, and the order is not the contract.
        let state = serde_json::from_str::<Value>(&command(&request_id(7), &get_state())).unwrap();
        assert_eq!(state, json!({"type": "get_state", "id": "cmd-7"}));
        let model = serde_json::from_str::<Value>(&command(
            &request_id(1),
            &set_model("openai-codex", "gpt-5.4"),
        ))
        .unwrap();
        assert_eq!(
            model,
            json!({"type": "set_model", "provider": "openai-codex", "modelId": "gpt-5.4", "id": "cmd-1"})
        );
    }

    #[rstest]
    #[case(
        r#"{"id":"cmd-1","type":"response","command":"get_state","success":true,"data":{"sessionId":"01a2"}}"#,
        Wire::Reply { id: "cmd-1".to_owned(), command: "get_state".to_owned(), data: Some(json!({"sessionId":"01a2"})), error: None }
    )]
    #[case(
        r#"{"id":"cmd-2","type":"response","command":"set_model","success":false,"error":"unknown model"}"#,
        Wire::Reply { id: "cmd-2".to_owned(), command: "set_model".to_owned(), data: None, error: Some("unknown model".to_owned()) }
    )]
    #[case(
        r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":"hi"}}"#,
        Wire::Text("hi".to_owned())
    )]
    #[case(
        r#"{"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"hmm"}}"#,
        Wire::Ignored
    )]
    #[case(
        r#"{"type":"message_update","assistantMessageEvent":{"type":"toolcall_end","toolCall":{}}}"#,
        Wire::Ignored
    )]
    #[case(
        r#"{"type":"tool_execution_start","toolCallId":"c1","toolName":"bash","args":{"command":"ls -a"}}"#,
        Wire::Tool(ToolUse{tool:"bash".to_owned(),summary:"ls -a".to_owned()})
    )]
    #[case(
        r#"{"type":"extension_ui_request","id":"u1","method":"confirm","title":"agent-hub","message":"Allow bash: ls"}"#,
        Wire::Approval { id: "u1".to_owned(), summary: "Allow bash: ls".to_owned() }
    )]
    #[case(
        r#"{"type":"extension_ui_request","id":"u2","method":"notify","message":"hi"}"#,
        Wire::Ignored
    )]
    #[case(r#"{"type":"turn_end","message":{}}"#, Wire::Ignored)]
    #[case(r#"{"type":"agent_settled","aborted":false}"#, Wire::Settled { aborted: false })]
    #[case(r#"{"type":"agent_settled","aborted":true}"#, Wire::Settled { aborted: true })]
    #[case(r#"{"type":"agent_end","messages":[],"willRetry":true}"#, Wire::Retrying)]
    #[case(r#"{"type":"agent_end","messages":[]}"#, Wire::Ignored)]
    #[case(r#"{"type":"message_start","message":{"role":"system"}}"#, Wire::Ignored)]
    // Живая запись 1.6.0: ошибка провайдера приходит в `message_end`, а не текстовым дельтом.
    #[case(
        r#"{"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"404: {\"message\":\"Model not exist.\"}"}}"#,
        Wire::Error { reason: r#"404: {"message":"Model not exist."}"#.to_owned() }
    )]
    #[case(
        r#"{"type":"message_end","message":{"role":"assistant","stopReason":"error"}}"#,
        Wire::Error { reason: "агент не выполнил ход".to_owned() }
    )]
    #[case(
        r#"{"type":"message_end","message":{"role":"assistant","stopReason":"toolUse","content":[]}}"#,
        Wire::Ignored
    )]
    #[case(
        r#"{"type":"message_end","message":{"role":"user","content":[],"errorMessage":"эхо не ошибка"}}"#,
        Wire::Ignored
    )]
    #[case(r"not json", Wire::Malformed)]
    #[case(r#"{"surprise":1}"#, Wire::Ignored)]
    fn lines_are_parsed(#[case] line: &str, #[case] expected: Wire) {
        assert_eq!(Wire::parse(line), expected);
    }

    /// Ошибки провайдера приходят одной строкой: записанный ответ модели может быть на
    /// десятки килобайт, и весь он не должен лезть в чат.
    #[test]
    fn a_long_provider_error_is_cut_to_one_line() {
        let reason = "x".repeat(4 * ERROR_LIMIT);
        let line = format!(
            r#"{{"type":"message_end","message":{{"role":"assistant","stopReason":"error","errorMessage":"{reason}"}}}}"#
        );
        let Wire::Error { reason } = Wire::parse(&line) else {
            panic!("ошибка провайдера не распознана: {line}");
        };
        assert!(reason.chars().count() <= ERROR_LIMIT + 4, "длина {}", reason.chars().count());
    }

    #[test]
    fn an_empty_text_delta_is_not_prose() {
        let line =
            r#"{"type":"message_update","assistantMessageEvent":{"type":"text_delta","delta":""}}"#;
        assert_eq!(Wire::parse(line), Wire::Ignored);
    }

    #[test]
    fn approval_answer_names_the_dialog_it_closes() {
        let answer = Wire::approval_answer("u1", false);
        assert!(answer.contains(r#""id":"u1""#), "{answer}");
        assert!(answer.contains(r#""confirmed":false"#), "{answer}");
        assert!(answer.contains("extension_ui_response"), "{answer}");
    }

    #[test]
    fn tool_summary_prefers_the_identifying_argument() {
        let call = json!({"toolName": "edit", "args": {"path": "src/main.rs"}});
        assert_eq!(
            tool_use(&call),
            ToolUse { tool: "edit".to_owned(), summary: "src/main.rs".to_owned() }
        );
        let unknown = json!({"toolName": "draw", "args": {"colour": "red"}});
        assert_eq!(tool_use(&unknown).summary, r#"{"colour":"red"}"#);
        let none = json!({"toolName": "think"});
        assert_eq!(tool_use(&none).summary, "think");
    }

    #[rstest]
    #[case(json!({"sessionId": "01a21635-d3b7-7148-8b45-a492b1a4ab56"}), true)]
    #[case(json!({"sessionId": "   "}), false)]
    #[case(json!({}), false)]
    fn session_id_is_taken_only_when_usable(#[case] data: Value, #[case] valid: bool) {
        assert_eq!(session_id(&data).is_ok(), valid);
    }

    #[test]
    fn advertised_levels_are_collected() {
        assert_eq!(
            advertised_levels(&json!({"levels": ["off", "low", "medium"]})),
            ["off", "low", "medium"]
        );
        assert_eq!(advertised_levels(&json!({})), Vec::<String>::new());
    }

    #[test]
    fn an_advertised_mode_is_never_refused() {
        let levels = ["off".to_owned(), "high".to_owned()];
        assert_eq!(unsupported_mode(&levels, PiThinking::Off), None);
        assert_eq!(unsupported_mode(&levels, PiThinking::High), None);
        assert_eq!(unsupported_mode(&[], PiThinking::High), None);
    }

    #[test]
    fn an_unadvertised_mode_names_the_available_ones() {
        let reason =
            unsupported_mode(&["off".to_owned(), "high".to_owned()], PiThinking::Xhigh).unwrap();
        assert!(reason.contains("Pi не поддерживает режим"), "{reason}");
        assert!(reason.contains("off, high"), "{reason}");
    }

    #[test]
    fn finished_reads_the_session_token_total() {
        let stats = json!({"tokens": {"total": 18894}, "cost": 0});
        let session = SessionId::parse("01a21635-d3b7-7148-8b45-a492b1a4ab56").unwrap();
        assert_eq!(
            finished(session.clone(), Some(&stats)),
            Finished { session, usage: Usage::Pi { tokens: Some(18894) }, background: 0 }
        );
    }

    #[test]
    fn finished_without_stats_reports_nothing() {
        let session = SessionId::parse("s").unwrap();
        assert_eq!(finished(session.clone(), None).usage, Usage::Pi { tokens: None });
    }

    #[test]
    fn a_fresh_session_names_the_model_but_not_the_level() {
        assert_eq!(
            session_args(None, Some("openai-codex/gpt-5.6-luna")),
            ["--mode", "rpc", "--model", "openai-codex/gpt-5.6-luna"]
        );
    }

    #[test]
    fn a_saved_session_is_the_one_resumed() {
        assert_eq!(session_args(Some("01a2"), None), ["--mode", "rpc", "--session", "01a2"]);
    }

    #[test]
    fn the_reported_model_reads_back_as_a_settings_value() {
        // The shape `get_state` returned live on 1.6.0.
        let state = json!({"model": {"id": "gpt-5.6-luna", "provider": "openai-codex"}});
        assert_eq!(current_model(&state).as_deref(), Some("openai-codex/gpt-5.6-luna"));
        assert_eq!(current_model(&json!({})), None);
        assert_eq!(current_model(&json!({"model": {"id": "x"}})), None);
    }

    #[test]
    fn guidance_names_the_tools_as_pi_calls_them() {
        assert!(MCP_GUIDANCE.contains("mcp__agent_hub__ask_user"), "{MCP_GUIDANCE}");
        assert!(MCP_GUIDANCE.contains("mcp__agent_hub__send_file"), "{MCP_GUIDANCE}");
    }
}
