//! The in-process MCP server `agent-hub` with one tool, `send_file`, spoken as JSON-RPC inside
//! `mcp_message` control requests.

use hub_agent::tools::{
    SEND_FILE, SEND_FILE_DESCRIPTION, ToolResult, delivery_result, parse_send_file,
    send_file_schema,
};
use hub_core::domain::{FileDelivery, OutgoingFile};
use serde_json::{Value, json};

use crate::outgoing::{MCP_SERVER, object};

// Used only if the CLI does not say which protocol version it speaks.
const FALLBACK_PROTOCOL: &str = "2025-06-18";
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

#[derive(Debug, PartialEq, Eq)]
pub enum McpStep {
    Reply(Value),
    SendFile { id: Value, file: OutgoingFile },
}

#[derive(Clone, Copy)]
enum ToolOutcome {
    Success,
    Error,
}

#[must_use]
pub fn handle(server: &str, message: &Value) -> McpStep {
    let id = message.get("id").cloned();
    if server != MCP_SERVER {
        let unknown = format!("Server '{server}' not found");
        return McpStep::Reply(error(id.unwrap_or(Value::Null), METHOD_NOT_FOUND, &unknown));
    }
    let Some(id) = id else {
        // A notification gets no JSON-RPC reply, but the control request carrying it needs an ack.
        return McpStep::Reply(json!({"jsonrpc": "2.0", "result": {}}));
    };
    match message.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let version = message
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(FALLBACK_PROTOCOL);
            McpStep::Reply(result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": MCP_SERVER, "version": env!("CARGO_PKG_VERSION")},
                }),
            ))
        }
        Some("tools/list") => McpStep::Reply(result(
            id,
            json!({"tools": [{
                "name": SEND_FILE,
                "description": SEND_FILE_DESCRIPTION,
                "inputSchema": send_file_schema(),
            }]}),
        )),
        Some("tools/call")
            if message.pointer("/params/name").and_then(Value::as_str) == Some(SEND_FILE) =>
        {
            let arguments = message.pointer("/params/arguments").unwrap_or(&Value::Null);
            match parse_send_file(arguments) {
                Ok(file) => McpStep::SendFile { id, file },
                Err(reason) => McpStep::Reply(result(id, tool_text(&reason, ToolOutcome::Error))),
            }
        }
        Some("tools/call") => McpStep::Reply(error(id, INVALID_PARAMS, "Unknown tool")),
        Some(_) | None => McpStep::Reply(error(id, METHOD_NOT_FOUND, "Method not found")),
    }
}

/// A failed delivery is a tool error the agent can react to (compress, split, retry).
#[must_use]
pub fn send_file_reply(id: Value, path: &str, delivery: FileDelivery) -> Value {
    let body = match delivery_result(path, delivery) {
        ToolResult::Success(text) => tool_text(&text, ToolOutcome::Success),
        ToolResult::Error(text) => tool_text(&text, ToolOutcome::Error),
    };
    result(id, body)
}

fn tool_text(text: &str, outcome: ToolOutcome) -> Value {
    let failed = match outcome {
        ToolOutcome::Success => false,
        ToolOutcome::Error => true,
    };
    json!({"content": [{"type": "text", "text": text}], "isError": failed})
}

fn result(id: Value, value: Value) -> Value {
    object([("jsonrpc", Value::from("2.0")), ("id", id), ("result", value)])
}

fn error(id: Value, code: i64, message: &str) -> Value {
    object([
        ("jsonrpc", Value::from("2.0")),
        ("id", id),
        ("error", json!({"code": code, "message": message})),
    ])
}

#[cfg(test)]
mod tests {
    use hub_core::domain::Denied;
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    fn reply(step: McpStep) -> Value {
        match step {
            McpStep::Reply(reply) => reply,
            McpStep::SendFile { .. } => panic!("expected a reply"),
        }
    }

    #[test]
    fn initialize_echoes_the_protocol_version() {
        let message = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                             "params": {"protocolVersion": "2025-11-25", "capabilities": {}}});
        let answer = reply(handle(MCP_SERVER, &message));
        assert_eq!(answer["id"], json!(0));
        assert_eq!(answer.pointer("/result/protocolVersion"), Some(&json!("2025-11-25")));
        assert_eq!(answer.pointer("/result/serverInfo/name"), Some(&json!("agent-hub")));
        assert_eq!(answer.pointer("/result/capabilities/tools"), Some(&json!({})));
    }

    #[test]
    fn notifications_are_acknowledged() {
        let message = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert_eq!(reply(handle(MCP_SERVER, &message)), json!({"jsonrpc": "2.0", "result": {}}));
    }

    #[test]
    fn tools_list_offers_send_file() {
        let message = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        let answer = reply(handle(MCP_SERVER, &message));
        assert_eq!(answer.pointer("/result/tools/0/name"), Some(&json!("send_file")));
        assert_eq!(answer.pointer("/result/tools/0/inputSchema/required"), Some(&json!(["path"])));
    }

    #[test]
    fn send_file_call_is_delegated() {
        let message = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": "send_file", "arguments": {"path": "out/r.pdf", "caption": "Отчёт"}}});
        assert_eq!(
            handle(MCP_SERVER, &message),
            McpStep::SendFile {
                id: json!(2),
                file: OutgoingFile {
                    path: "out/r.pdf".to_owned(), caption: "Отчёт".to_owned()
                }
            }
        );
    }

    #[rstest]
    #[case(json!({}))]
    #[case(json!({"path": ""}))]
    #[case(json!({"path": 1}))]
    #[case(json!({"path": "a", "caption": 2}))]
    fn malformed_send_file_arguments_are_a_tool_error(#[case] arguments: Value) {
        let message = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                             "params": {"name": "send_file", "arguments": arguments}});
        let answer = reply(handle(MCP_SERVER, &message));
        assert_eq!(answer.pointer("/result/isError"), Some(&json!(true)));
    }

    #[rstest]
    #[case("other-server", json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list"}), -32601)]
    #[case(MCP_SERVER, json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}), -32601)]
    #[case(
        MCP_SERVER,
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "rm"}}),
        -32602
    )]
    fn unknown_targets_are_json_rpc_errors(
        #[case] server: &str,
        #[case] message: Value,
        #[case] code: i64,
    ) {
        let answer = reply(handle(server, &message));
        assert_eq!(answer["id"], json!(4));
        assert_eq!(answer.pointer("/error/code"), Some(&json!(code)));
    }

    #[test]
    fn delivery_results_are_tool_results() {
        let delivered = send_file_reply(json!(5), "out/r.pdf", FileDelivery::Delivered);
        assert_eq!(delivered.pointer("/result/isError"), Some(&json!(false)));
        assert_eq!(
            delivered.pointer("/result/content/0/text"),
            Some(&json!("Файл out/r.pdf отправлен пользователю"))
        );
        let failed =
            send_file_reply(json!(5), "big.zip", FileDelivery::Denied(Denied::new("больше 50 МБ")));
        assert_eq!(failed.pointer("/result/isError"), Some(&json!(true)));
        assert_eq!(failed.pointer("/result/content/0/text"), Some(&json!("больше 50 МБ")));
    }
}
