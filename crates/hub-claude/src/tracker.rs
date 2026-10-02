//! CLI messages → domain events, remembering the session id.

use std::str::FromStr;

use hub_core::domain::{AgentEvent, Finished, SessionId, ToolUse};
use rust_decimal::Decimal;
use serde_json::Number;

use crate::permissions::{ASK_USER_QUESTION, SEND_FILE_TOOL, summarize_tool_input};
use crate::wire::{Assistant, Block, Incoming, Outcome, System};

// Tools the user already sees as their own message, not as a tool line.
const SILENT_TOOLS: [&str; 2] = [ASK_USER_QUESTION, SEND_FILE_TOOL];

#[derive(Debug, Default)]
pub struct SessionTracker {
    session: Option<SessionId>,
}

impl SessionTracker {
    /// `background` is how many background tasks still run, reported with a finished turn.
    #[must_use]
    pub fn translate(&mut self, message: &Incoming, background: usize) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        if let Some(found) = session_of(message)
            && self.session.as_ref() != Some(&found)
        {
            self.session = Some(found.clone());
            events.push(AgentEvent::SessionStarted(found));
        }
        match message {
            Incoming::Assistant(Assistant { message, .. }) => {
                events.extend(message.content.iter().filter_map(block_event));
            }
            Incoming::Result(outcome) => events.push(outcome_event(outcome, background)),
            Incoming::System(_)
            | Incoming::User(_)
            | Incoming::ControlRequest { .. }
            | Incoming::ControlResponse { .. }
            | Incoming::ControlCancelRequest { .. }
            | Incoming::Other => {}
        }
        events
    }
}

fn session_of(message: &Incoming) -> Option<SessionId> {
    match message {
        Incoming::System(System::Init { session_id })
        | Incoming::Result(Outcome { session_id, .. }) => SessionId::parse(session_id),
        Incoming::Assistant(Assistant { session_id, .. }) => {
            session_id.as_deref().and_then(SessionId::parse)
        }
        Incoming::System(_)
        | Incoming::User(_)
        | Incoming::ControlRequest { .. }
        | Incoming::ControlResponse { .. }
        | Incoming::ControlCancelRequest { .. }
        | Incoming::Other => None,
    }
}

fn block_event(block: &Block) -> Option<AgentEvent> {
    match block {
        Block::Text { text } if !text.trim().is_empty() => {
            Some(AgentEvent::AssistantText(text.clone()))
        }
        Block::ToolUse { name, input } if !SILENT_TOOLS.contains(&name.as_str()) => {
            Some(AgentEvent::ToolCall(ToolUse {
                tool: name.clone(),
                summary: summarize_tool_input(name, input),
            }))
        }
        Block::Text { .. } | Block::ToolUse { .. } | Block::Other => None,
    }
}

fn outcome_event(outcome: &Outcome, background: usize) -> AgentEvent {
    if outcome.is_error {
        let detail = outcome
            .result
            .as_deref()
            .filter(|result| !result.is_empty())
            .unwrap_or("ошибка выполнения");
        return AgentEvent::Failed(format!("{}: {detail}", outcome.subtype));
    }
    match SessionId::parse(&outcome.session_id) {
        Some(session) => AgentEvent::Finished(Finished {
            session,
            turns: outcome.num_turns,
            cost: outcome.total_cost_usd.as_ref().and_then(cost),
            background,
        }),
        None => AgentEvent::Failed(format!("{}: нет session_id", outcome.subtype)),
    }
}

/// Exact decimal of the number as printed, like Python's `Decimal(str(float))`.
fn cost(number: &Number) -> Option<Decimal> {
    let text = number.to_string();
    Decimal::from_str(&text).or_else(|_| Decimal::from_scientific(&text)).ok()
}

#[cfg(test)]
mod tests {
    use hub_core::domain::{AgentEvent, Finished, SessionId, ToolUse};
    use rust_decimal::Decimal;
    use serde_json::{Value, json};

    use super::*;
    use crate::permissions::TOOL_SUMMARY_LIMIT;
    use crate::wire::parse_line;

    fn message(value: &Value) -> Incoming {
        parse_line(&value.to_string()).unwrap()
    }

    fn result(is_error: bool, cost: &Value) -> Incoming {
        message(&json!({"type": "result",
                        "subtype": if is_error { "error_during_execution" } else { "success" },
                        "is_error": is_error, "num_turns": 3, "session_id": "s-1",
                        "total_cost_usd": cost, "result": if is_error { "boom" } else { "ok" }}))
    }

    fn session() -> SessionId {
        SessionId::parse("s-1").unwrap()
    }

    #[test]
    fn session_start_is_reported_once() {
        let mut tracker = SessionTracker::default();
        let init = message(&json!({"type": "system", "subtype": "init", "session_id": "s-1"}));
        assert_eq!(tracker.translate(&init, 0), [AgentEvent::SessionStarted(session())]);
        assert_eq!(tracker.translate(&init, 0), []);
    }

    #[test]
    fn assistant_blocks_become_text_and_tool_calls() {
        let assistant = message(&json!({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "Смотрю тесты"},
            {"type": "text", "text": "   "},
            {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "uv run pytest"}},
            {"type": "tool_use", "id": "t2", "name": "AskUserQuestion", "input": {}},
            {"type": "tool_use", "id": "t3", "name": "mcp__agent-hub__send_file", "input": {"path": "a"}}
        ]}}));
        assert_eq!(
            SessionTracker::default().translate(&assistant, 0),
            [
                AgentEvent::AssistantText("Смотрю тесты".to_owned()),
                AgentEvent::ToolCall(ToolUse {
                    tool: "Bash".to_owned(),
                    summary: "uv run pytest".to_owned()
                }),
            ]
        );
    }

    #[test]
    fn success_result_finishes_turn() {
        assert_eq!(
            SessionTracker::default().translate(&result(false, &json!(0.1234)), 2),
            [
                AgentEvent::SessionStarted(session()),
                AgentEvent::Finished(Finished {
                    session: session(),
                    turns: 3,
                    cost: Some(Decimal::new(1234, 4)),
                    background: 2,
                }),
            ]
        );
    }

    #[test]
    fn missing_cost_is_none() {
        let events = SessionTracker::default().translate(&result(false, &Value::Null), 0);
        assert_eq!(
            events.last(),
            Some(&AgentEvent::Finished(Finished {
                session: session(),
                turns: 3,
                cost: None,
                background: 0
            }))
        );
    }

    #[test]
    fn tiny_cost_in_scientific_notation_is_kept() {
        let events = SessionTracker::default().translate(&result(false, &json!(1e-7)), 0);
        let Some(AgentEvent::Finished(finished)) = events.last() else { panic!("not finished") };
        assert!(finished.cost.is_some_and(|cost| cost > Decimal::ZERO));
    }

    #[test]
    fn error_result_fails_turn() {
        let events = SessionTracker::default().translate(&result(true, &Value::Null), 0);
        assert_eq!(
            events.last(),
            Some(&AgentEvent::Failed("error_during_execution: boom".to_owned()))
        );
    }

    #[test]
    fn error_without_text_has_a_default_reason() {
        let outcome = message(&json!({"type": "result", "subtype": "error_max_turns",
                                      "is_error": true, "num_turns": 1, "session_id": "s-1",
                                      "result": ""}));
        let events = SessionTracker::default().translate(&outcome, 0);
        assert_eq!(
            events.last(),
            Some(&AgentEvent::Failed("error_max_turns: ошибка выполнения".to_owned()))
        );
    }

    #[test]
    fn long_tool_summary_is_truncated() {
        let assistant = message(&json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "name": "Bash",
             "input": {"command": "x".repeat(TOOL_SUMMARY_LIMIT * 2)}}
        ]}}));
        let events = SessionTracker::default().translate(&assistant, 0);
        let Some(AgentEvent::ToolCall(call)) = events.first() else { panic!("no tool call") };
        assert_eq!(call.summary.chars().count(), TOOL_SUMMARY_LIMIT);
    }
}
