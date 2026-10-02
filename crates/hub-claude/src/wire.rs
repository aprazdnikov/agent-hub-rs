//! Messages the agent CLI prints on stdout (stream-json), parsed into closed types.

use serde::Deserialize;
use serde_json::{Number, Value};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Incoming {
    System(System),
    Assistant(Assistant),
    User(User),
    Result(Outcome),
    ControlRequest {
        request_id: String,
        request: ControlRequest,
    },
    ControlResponse {
        response: ControlReply,
    },
    ControlCancelRequest {
        request_id: String,
    },
    /// Kinds this hub does not act on: rate limits, command lifecycle, stream events, …
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum System {
    Init {
        session_id: String,
    },
    TaskStarted {
        task_id: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        is_backgrounded: Option<bool>,
    },
    TaskUpdated {
        #[serde(default)]
        task_id: String,
        #[serde(default)]
        patch: TaskPatch,
    },
    TaskNotification {
        task_id: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct TaskPatch {
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Assistant {
    pub message: AssistantBody,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantBody {
    #[serde(default)]
    pub content: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    ToolUse {
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct User {
    #[serde(default)]
    pub uuid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Outcome {
    pub subtype: String,
    pub is_error: bool,
    #[serde(default)]
    pub num_turns: u32,
    pub session_id: String,
    #[serde(default)]
    pub total_cost_usd: Option<Number>,
    #[serde(default)]
    pub result: Option<String>,
    // Kept loose: a malformed origin must not make the whole result unreadable.
    #[serde(default)]
    pub origin: Option<Value>,
}

impl Outcome {
    /// A turn the CLI started on its own, e.g. to report a finished background task.
    #[must_use]
    pub fn injected(&self) -> bool {
        self.origin
            .as_ref()
            .and_then(|origin| origin.get("kind"))
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != "human")
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlRequest {
    CanUseTool {
        tool_name: String,
        #[serde(default)]
        input: Value,
    },
    McpMessage {
        server_name: String,
        message: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlReply {
    Success {
        request_id: String,
    },
    Error {
        request_id: String,
        #[serde(default)]
        error: String,
    },
}

pub fn parse_line(line: &str) -> Result<Incoming, serde_json::Error> {
    serde_json::from_str(line)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    const FIXTURES: [(&str, &str); 4] = [
        ("approval", include_str!("../tests/fixtures/approval.jsonl")),
        ("question", include_str!("../tests/fixtures/question.jsonl")),
        ("sendfile", include_str!("../tests/fixtures/sendfile.jsonl")),
        ("background", include_str!("../tests/fixtures/background.jsonl")),
    ];

    fn parse(value: &serde_json::Value) -> Incoming {
        parse_line(&value.to_string()).unwrap()
    }

    #[test]
    fn init_carries_session() {
        assert_eq!(
            parse(&json!({"type": "system", "subtype": "init", "session_id": "s", "tools": []})),
            Incoming::System(System::Init { session_id: "s".to_owned() })
        );
    }

    #[test]
    fn task_messages_are_parsed() {
        assert_eq!(
            parse(&json!({"type": "system", "subtype": "task_started", "task_id": "t",
                          "description": "sleep", "is_backgrounded": true})),
            Incoming::System(System::TaskStarted {
                task_id: "t".to_owned(),
                description: "sleep".to_owned(),
                is_backgrounded: Some(true),
            })
        );
        assert_eq!(
            parse(&json!({"type": "system", "subtype": "task_updated", "task_id": "t",
                          "patch": {"status": "completed", "end_time": 1}})),
            Incoming::System(System::TaskUpdated {
                task_id: "t".to_owned(),
                patch: TaskPatch { status: Some("completed".to_owned()) },
            })
        );
    }

    #[test]
    fn assistant_blocks_are_parsed() {
        let message = parse(&json!({"type": "assistant", "session_id": "s", "message": {
        "content": [
            {"type": "thinking", "thinking": "", "signature": ""},
            {"type": "text", "text": "hi"},
            {"type": "tool_use", "id": "t", "name": "Bash", "input": {"command": "ls"}}
        ]}}));
        assert_eq!(
            message,
            Incoming::Assistant(Assistant {
                message: AssistantBody {
                    content: vec![
                        Block::Other,
                        Block::Text { text: "hi".to_owned() },
                        Block::ToolUse { name: "Bash".to_owned(), input: json!({"command": "ls"}) },
                    ]
                },
                session_id: Some("s".to_owned()),
            })
        );
    }

    #[rstest]
    #[case(json!({"kind": "task-notification"}), true)]
    #[case(json!({"kind": "human"}), false)]
    #[case(json!("garbage"), false)]
    fn injected_results_are_recognised(#[case] origin: serde_json::Value, #[case] injected: bool) {
        let Incoming::Result(outcome) = parse(&json!({"type": "result", "subtype": "success",
            "is_error": false, "num_turns": 1, "session_id": "s", "origin": origin}))
        else {
            panic!("not a result");
        };
        assert_eq!(outcome.injected(), injected);
    }

    #[test]
    fn control_messages_are_parsed() {
        assert_eq!(
            parse(&json!({"type": "control_request", "request_id": "r",
                          "request": {"subtype": "can_use_tool", "tool_name": "Write",
                                      "input": {"file_path": "a"}, "tool_use_id": "t"}})),
            Incoming::ControlRequest {
                request_id: "r".to_owned(),
                request: ControlRequest::CanUseTool {
                    tool_name: "Write".to_owned(),
                    input: json!({"file_path": "a"}),
                },
            }
        );
        assert_eq!(
            parse(&json!({"type": "control_response",
                          "response": {"subtype": "error", "request_id": "r", "error": "no"}})),
            Incoming::ControlResponse {
                response: ControlReply::Error {
                    request_id: "r".to_owned(),
                    error: "no".to_owned()
                },
            }
        );
        assert_eq!(
            parse(&json!({"type": "control_cancel_request", "request_id": "r"})),
            Incoming::ControlCancelRequest { request_id: "r".to_owned() }
        );
    }

    #[rstest]
    #[case(json!({"type": "rate_limit_event", "rate_limit_info": {}}), Incoming::Other)]
    #[case(json!({"type": "system", "subtype": "thinking_tokens"}), Incoming::System(System::Other))]
    #[case(
        json!({"type": "control_request", "request_id": "r", "request": {"subtype": "hook_callback"}}),
        Incoming::ControlRequest { request_id: "r".to_owned(), request: ControlRequest::Other }
    )]
    fn unknown_kinds_are_tolerated(#[case] value: serde_json::Value, #[case] expected: Incoming) {
        assert_eq!(parse(&value), expected);
    }

    #[test]
    fn every_recorded_line_parses_and_known_kinds_are_recognised() {
        for (name, text) in FIXTURES {
            for line in text.lines() {
                let message =
                    parse_line(line).unwrap_or_else(|error| panic!("{name}: {error}: {line}"));
                let kind: serde_json::Value = serde_json::from_str(line).unwrap();
                let known = matches!(
                    kind["type"].as_str(),
                    Some("assistant" | "user" | "result" | "control_request" | "control_response")
                );
                assert!(!known || message != Incoming::Other, "{name}: {line}");
            }
        }
    }
}
