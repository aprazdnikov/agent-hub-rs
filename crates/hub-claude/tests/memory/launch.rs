use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_agent::channel::UserChannel;
use hub_agent::conversation::{Conversation, Limits};
use hub_agent::memory::MemoryMcp;
use hub_claude::backend::ClaudeBackend;
use hub_core::domain::{
    AbsolutePath, BackendKind, Decision, FileDelivery, OutgoingFile, Prompt, Question,
    QuestionsOutcome, SessionId, ToolRequest, TopicSession,
};
use hub_core::settings::{ClaudeSettings, PermissionMode};
use rstest::rstest;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const CAPTURE_CLI: &str = r#"#!/bin/sh
printf '%s\0' "$@" > argv.bin
IFS= read -r initialize
IFS= read -r prompt
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"hub-initialize","response":{}}}'
"#;

struct Channel;

impl UserChannel for Channel {
    fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
        Box::pin(async { Decision::Allowed })
    }

    fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        Box::pin(async { QuestionsOutcome::Answered(Vec::new()) })
    }

    fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(async { FileDelivery::Delivered })
    }
}

#[rstest]
#[case::new_with_memory(None, true)]
#[case::resume_with_memory(Some("saved-session"), true)]
#[case::new_without_memory(None, false)]
#[case::resume_without_memory(Some("saved-session"), false)]
#[tokio::test]
async fn launches_with_session_scoped_memory_and_preserves_chat_mcp(
    #[case] saved: Option<&str>,
    #[case] memory_enabled: bool,
) -> Result<(), Box<dyn Error>> {
    let root = tempfile::tempdir()?;
    let project = root.path().join("project with spaces");
    std::fs::create_dir(&project)?;
    let cli = root.path().join("fake-claude");
    std::fs::write(&cli, CAPTURE_CLI)?;
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700))?;
    let cwd = AbsolutePath::new(project.clone()).ok_or("absolute project required")?;
    let program = root.path().join("agent hub $(literal)");
    let memory =
        memory_enabled.then(|| MemoryMcp::new(&program, &cwd, BackendKind::Claude)).transpose()?;
    let mut session = TopicSession::fresh(BackendKind::Claude, cwd);
    session.session = saved.and_then(SessionId::parse);
    let (events, _receiver) = mpsc::channel(8);
    let (_sender, inbox) = mpsc::channel(1);
    let conversation = Conversation {
        channel: Arc::new(Channel),
        events,
        cancel: CancellationToken::new(),
        limits: Limits::new(Duration::from_secs(1)),
    };
    let settings = ClaudeSettings {
        cli: None,
        model: None,
        permission_mode: PermissionMode::Default,
        budget: None,
    };
    let backend = ClaudeBackend::new(cli, settings).with_memory(memory);
    let prompt = Prompt::new("hello".to_owned(), Vec::new()).ok_or("prompt required")?;
    let _inbox = tokio::time::timeout(
        Duration::from_secs(10),
        backend.run(&session, prompt, inbox, conversation),
    )
    .await?;
    let argv = std::fs::read(project.join("argv.bin"))?
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8(arg.to_vec()))
        .collect::<Result<Vec<_>, _>>()?;
    let config = argv
        .windows(2)
        .find(|pair| pair.first().is_some_and(|flag| flag == "--mcp-config"))
        .and_then(|pair| pair.get(1))
        .ok_or("MCP config required")?;
    let config: Value = serde_json::from_str(config)?;
    let memory = memory_enabled.then(|| {
        json!({"type":"stdio","command":program.to_str(),
        "args":["--memory-mcp","--project-dir",project.to_str(),"--actor","claude"],"env":{}})
    });
    assert_eq!(
        json!({
            "chat":config.pointer("/mcpServers/agent-hub"),
            "memory":config.pointer("/mcpServers/agent-hub-memory"),
            "resumed":argv.iter().find(|arg| arg.starts_with("--resume=")),
            "userSettings":argv.iter().any(|arg| arg == "--setting-sources=user,project,local"),
            "strict":argv.iter().any(|arg| arg == "--strict-mcp-config"),
        }),
        json!({
            "chat":{"type":"sdk","name":"agent-hub"},"memory":memory,
            "resumed":saved.map(|session| format!("--resume={session}")),
            "userSettings":true,"strict":false,
        })
    );
    Ok(())
}
