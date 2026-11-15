use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use hub_agent::rpc::{self, Envelope, Handler, RequestError, Wire};
use hub_codex::protocol::{initialize_params, open_thread};
use hub_core::domain::{AbsolutePath, BackendKind, SessionId, TopicSession};
use hub_core::settings::{Approval, CodexSettings, Sandbox};
use rstest::rstest;
use serde_json::{Value, json};
use tokio::process::Command;

#[rstest]
#[case::new_thread(None)]
#[case::resumed_thread(Some("startup-history"))]
#[tokio::test]
#[ignore = "requires installed codex; startup only, no model calls or login"]
async fn disabled_memory_is_valid_and_preserves_unrelated_servers(#[case] saved: Option<&str>) {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        r#"
[mcp_servers.unrelated]
command = "__unrelated_disabled__"
enabled = false
[mcp_servers.agent-hub-memory]
command = "__previous_memory_command__"
enabled = true
"#,
    )
    .unwrap();
    let mut process = Command::new(hub_codex::version::locate(None).unwrap())
        .arg("app-server")
        .env("CODEX_HOME", home.path())
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let handler: Handler =
        Arc::new(|method, _| Box::pin(async move { Err(RequestError::Unsupported(method)) }));
    let connection = rpc::connect(
        process.stdout.take().unwrap(),
        process.stdin.take().unwrap(),
        handler,
        Wire {
            peer: "codex startup test",
            envelope: Envelope::Bare,
            timeout: Duration::from_secs(20),
        },
    );
    let client = &connection.client;
    client.request("initialize", initialize_params()).await.unwrap();
    client.notify("initialized").await.unwrap();
    let topic = TopicSession::fresh(
        BackendKind::Codex,
        AbsolutePath::new(home.path().to_path_buf()).unwrap(),
    )
    .with_session(saved.and_then(SessionId::parse));
    let settings = CodexSettings {
        cli: None,
        model: None,
        sandbox: Sandbox::ReadOnly,
        approval: Approval::Untrusted,
        api_key: None,
    };
    let (method, mut params) = open_thread(&topic, &settings);
    if saved.is_some() {
        // A new thread has no on-disk rollout until a turn occurs. Supplying in-memory
        // history exercises the real resume loader without any model call or user history.
        params.as_object_mut().unwrap().insert("history".to_owned(), json!([
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Startup-only fixture"}]}
        ]));
    }
    let opened = client.request(method.method(), params).await.unwrap();
    let thread = opened.pointer("/thread/id").unwrap();
    let status = client.request("mcpServerStatus/list", json!({"threadId": thread})).await.unwrap();
    let rows: Vec<_> = status
        .get("data")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row.get("name").and_then(Value::as_str),
                row.get("runtimeStatus").and_then(Value::as_str),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [(Some("agent-hub-memory"), Some("disabled")), (Some("unrelated"), Some("disabled"))]
    );
    client.close();
    tokio::time::timeout(Duration::from_secs(5), process.wait()).await.unwrap().unwrap();
    connection.reader.abort();
}
