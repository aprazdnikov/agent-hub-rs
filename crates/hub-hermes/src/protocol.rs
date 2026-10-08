//! Hermes ACP wire mapping. No Qwen-specific queue or question extensions.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hub_agent::mcp_http::SERVER_NAME;
use hub_agent::rpc::{Notification, RpcError};
use hub_agent::tools::TOOL_SUMMARY_LIMIT;
use hub_core::domain::{AgentEvent, Finished, Prompt, SessionId, ToolUse, Usage};
use hub_core::render::truncate;
use serde_json::{Value, json};

pub const MCP_GUIDANCE: &str = "[agent-hub integration]\nFor clarifying questions, use the ask_user tool from the agent-hub MCP server, not the native clarify tool. To deliver local files to the user, use that server's send_file tool. Prompts received while you work are queued by the hub for a subsequent turn; active steering is not supported.\n[/agent-hub integration]";
const MAX_TEXT: usize = 8 * 1024 * 1024;

pub fn initialize_params() -> Value {
    json!({"protocolVersion": 1, "clientInfo": {"name": "agent-hub", "version": env!("CARGO_PKG_VERSION")},
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false}})
}

pub struct Capabilities {
    pub load: bool,
    pub images: bool,
}

impl Capabilities {
    pub fn parse(result: &Value) -> Result<Self, RpcError> {
        if result.get("protocolVersion").and_then(Value::as_u64) != Some(1) {
            return Err(RpcError::Protocol("Hermes ACP protocolVersion must be 1".to_owned()));
        }
        let agent =
            result.get("agentCapabilities").filter(|agent| agent.is_object()).ok_or_else(|| {
                RpcError::Protocol("Hermes initialize without agentCapabilities".to_owned())
            })?;
        Ok(Self {
            load: agent.get("loadSession").and_then(Value::as_bool) == Some(true),
            images: agent.pointer("/promptCapabilities/image").and_then(Value::as_bool)
                == Some(true),
        })
    }
}

pub fn prompt_params(session: &SessionId, prompt: &Prompt) -> Value {
    let guidance = std::iter::once(json!({"type": "text", "text": MCP_GUIDANCE}));
    let images = prompt.images().iter().map(|image| json!({"type": "image", "mimeType": image.media.mime(), "data": STANDARD.encode(&image.data)}));
    let text =
        (!prompt.text().trim().is_empty()).then(|| json!({"type": "text", "text": prompt.text()}));
    json!({"sessionId": session.as_str(), "prompt": guidance.chain(images).chain(text).collect::<Vec<_>>()})
}

pub fn session_params(cwd: &std::path::Path, url: &str, token: &str) -> Value {
    json!({"cwd": cwd.display().to_string(), "mcpServers": [{"type": "http", "name": SERVER_NAME,
        "url": url, "headers": [{"name": "Authorization", "value": format!("Bearer {token}")}]}]})
}

pub fn session_id(result: &Value) -> Result<SessionId, RpcError> {
    result
        .get("sessionId")
        .and_then(Value::as_str)
        .and_then(SessionId::parse)
        .ok_or_else(|| RpcError::Protocol("Hermes session/new without a session id".to_owned()))
}

pub struct TurnResult {
    pub reason: String,
    pub tokens: Option<u64>,
}

impl TurnResult {
    pub fn parse(result: &Value) -> Result<Self, RpcError> {
        let reason = result.get("stopReason").and_then(Value::as_str).ok_or_else(|| {
            RpcError::Protocol("Hermes session/prompt without stopReason".to_owned())
        })?;
        // usage_update is estimated context pressure, NOT token billing. Do not use it.
        let tokens = result.pointer("/usage/totalTokens").and_then(Value::as_u64);
        Ok(Self { reason: reason.to_owned(), tokens })
    }
}

pub struct TurnTracker {
    session: SessionId,
    text: String,
}

impl TurnTracker {
    pub fn new(session: SessionId) -> Self {
        Self { session, text: String::new() }
    }

    pub fn translate(&mut self, notification: &Notification) -> Result<Vec<AgentEvent>, RpcError> {
        if notification.method != "session/update"
            || notification.params.get("sessionId").and_then(Value::as_str)
                != Some(self.session.as_str())
        {
            return Ok(Vec::new());
        }
        let Some(update) = notification.params.get("update") else { return Ok(Vec::new()) };
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                if update.pointer("/content/type").and_then(Value::as_str) == Some("text")
                    && let Some(text) = update.pointer("/content/text").and_then(Value::as_str)
                {
                    if text.len() > MAX_TEXT.saturating_sub(self.text.len()) {
                        return Err(RpcError::Protocol(
                            "Hermes assistant text exceeded 8 MiB".to_owned(),
                        ));
                    }
                    self.text.push_str(text);
                }
                Ok(Vec::new())
            }
            Some("tool_call") => {
                let mut events = self.flush();
                events.push(AgentEvent::ToolCall(tool_use(update)));
                Ok(events)
            }
            // Thought chunks, tool results, plans, provenance and estimated usage are not chat prose.
            Some(_) | None => Ok(Vec::new()),
        }
    }

    fn flush(&mut self) -> Vec<AgentEvent> {
        let text = std::mem::take(&mut self.text);
        if text.trim().is_empty() { Vec::new() } else { vec![AgentEvent::AssistantText(text)] }
    }

    pub fn finish(&mut self, result: &TurnResult) -> Vec<AgentEvent> {
        let mut events = self.flush();
        match result.reason.as_str() {
            "end_turn" => events.push(AgentEvent::Finished(Finished {
                session: self.session.clone(),
                usage: Usage::Hermes { tokens: result.tokens },
                background: 0,
            })),
            "cancelled" => {}
            other => events.push(AgentEvent::Failed(format!(
                "Hermes: {}",
                truncate(other, TOOL_SUMMARY_LIMIT)
            ))),
        }
        events
    }
}

pub fn tool_use(call: &Value) -> ToolUse {
    let tool = call
        .get("kind")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("tool");
    let title = call
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("Hermes tool");
    ToolUse { tool: truncate(tool, 128), summary: truncate(title, TOOL_SUMMARY_LIMIT) }
}
