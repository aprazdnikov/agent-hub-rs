use std::process::{Output, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::process::Command;

async fn invoke(root: &std::path::Path, calls: &[Value]) -> anyhow::Result<Output> {
    std::fs::create_dir_all(root.join("project"))?;
    let mut process = Command::new(env!("CARGO_BIN_EXE_agent-hub"))
        .arg("--memory-mcp")
        .arg("--project-dir")
        .arg(root.join("project"))
        .arg("--store-dir")
        .arg(root.join("memory-data"))
        .arg("--actor")
        .arg("integration-test")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut input = process.stdin.take().ok_or_else(|| anyhow::anyhow!("missing stdin"))?;
    for call in calls {
        input.write_all(format!("{call}\n").as_bytes()).await?;
    }
    drop(input);
    Ok(tokio::time::timeout(Duration::from_secs(15), process.wait_with_output()).await??)
}

fn handshake() -> Vec<Value> {
    vec![
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":{},
            "clientInfo":{"name":"integration-test","version":"1"}
        }}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
    ]
}

fn responses(output: &Output) -> anyhow::Result<Vec<Value>> {
    assert!(
        output.status.success(),
        "MCP process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

#[tokio::test]
async fn memory_mode_serves_json_without_gui_or_settings() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut calls = handshake();
    calls.push(json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
    let output = invoke(temp.path(), &calls).await?;
    let replies = responses(&output)?;
    let tools = replies
        .iter()
        .find(|reply| reply.get("id") == Some(&json!(2)))
        .and_then(|reply| reply.pointer("/result/tools"))
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("no tools/list response"))?;
    assert!(tools.iter().any(|tool| tool.get("name") == Some(&json!("memory_write"))));
    assert!(!temp.path().join("config").exists(), "MCP must not initialize app settings");
    Ok(())
}

#[tokio::test]
async fn independent_memory_process_reads_previous_process_write() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut write = handshake();
    write.push(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
        "name":"memory_write","arguments":{
            "key":"first-decision","expected_revision":0,"kind":"decision",
            "title":"Storage","body":"Use a local shared store.","sources":["user: pilot discussion"]
        }
    }}));
    let written = responses(&invoke(temp.path(), &write).await?)?;
    let result = written
        .iter()
        .find(|reply| reply.get("id") == Some(&json!(2)))
        .and_then(|reply| reply.get("result"))
        .ok_or_else(|| anyhow::anyhow!("no write result"))?;
    assert_ne!(result.get("isError"), Some(&json!(true)), "{result}");
    let mut read = handshake();
    read.push(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
        "name":"memory_get","arguments":{"key":"first-decision"}
    }}));
    let replies = responses(&invoke(temp.path(), &read).await?)?;
    let text = replies
        .iter()
        .find(|reply| reply.get("id") == Some(&json!(3)))
        .and_then(|reply| reply.pointer("/result/content/0/text"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("no memory_get content"))?;
    let entry: Value = serde_json::from_str(text)?;
    assert_eq!(entry.get("body"), Some(&json!("Use a local shared store.")));
    Ok(())
}

#[tokio::test]
async fn competing_memory_processes_preserve_both_writes() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let save = |key: &str| {
        let mut calls = handshake();
        calls.push(json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{
            "name":"memory_write","arguments":{
                "key":key,"expected_revision":0,"kind":"progress",
                "title":key,"body":"Independent work.","sources":["integration test"]
            }
        }}));
        calls
    };
    let left = save("left");
    let right = save("right");
    let (first, second) = tokio::join!(invoke(temp.path(), &left), invoke(temp.path(), &right));
    // Busy is an explicit retryable outcome. Repeating both original requests also checks
    // that a successful writer is not duplicated when the client loses its result.
    responses(&first?)?;
    responses(&second?)?;
    for calls in [&left, &right] {
        let replies = responses(&invoke(temp.path(), calls).await?)?;
        let result = replies
            .iter()
            .find(|reply| reply.get("id") == Some(&json!(2)))
            .and_then(|reply| reply.get("result"))
            .ok_or_else(|| anyhow::anyhow!("no write result"))?;
        assert_ne!(result.get("isError"), Some(&json!(true)), "{result}");
    }
    let mut calls = handshake();
    calls.push(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{
        "name":"memory_search","arguments":{"query":"","limit":20}
    }}));
    let replies = responses(&invoke(temp.path(), &calls).await?)?;
    let text = replies
        .iter()
        .find(|reply| reply.get("id") == Some(&json!(3)))
        .and_then(|reply| reply.pointer("/result/content/0/text"))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("no search content"))?;
    let search: Value = serde_json::from_str(text)?;
    let hits = search
        .get("hits")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow::anyhow!("no search hits"))?;
    let keys: std::collections::BTreeSet<_> = hits
        .iter()
        .filter_map(|hit| Some((hit.get("key")?.as_str()?, hit.get("revision")?.as_u64()?)))
        .collect();
    assert_eq!(keys, std::collections::BTreeSet::from([("left", 1), ("right", 1)]));
    Ok(())
}
