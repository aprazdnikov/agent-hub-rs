//! Round trip through the real CLI. Spends a few tokens, so it is opt-in:
//! `AGENT_HUB_LIVE_CLI=1 cargo test -p hub-claude --test live -- --ignored`.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_claude::backend::ClaudeBackend;
    use hub_agent::channel::UserChannel;
    use hub_agent::conversation::{Conversation, Limits};
    use hub_claude::version::{check, locate};
    use hub_core::domain::{
        AbsolutePath, AgentEvent, BackendKind, Decision, FileDelivery, OutgoingFile, Prompt,
        Question, QuestionsOutcome, ToolRequest, TopicSession,
    };
    use hub_core::settings::{ClaudeSettings, PermissionMode};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    struct Allowing;

    impl UserChannel for Allowing {
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

    #[tokio::test]
    #[ignore = "talks to the real agent CLI and spends tokens"]
    async fn live_cli_round_trip() {
        if std::env::var_os("AGENT_HUB_LIVE_CLI").is_none() {
            return;
        }
        let cli = locate(None).unwrap();
        check(&cli).await.unwrap();
        let settings = ClaudeSettings {
            cli: None,
            model: Some("haiku".to_owned()),
            permission_mode: PermissionMode::Default,
            budget: None,
        };
        let workdir = std::env::temp_dir().join(format!("agent-hub-live-{}", std::process::id()));
        std::fs::create_dir_all(&workdir).unwrap();
        let session = TopicSession::fresh(BackendKind::Claude, AbsolutePath::new(workdir).unwrap());
        let (events_out, mut events) = mpsc::channel(64);
        let (_inbox, inbox_in) = mpsc::channel(1);
        let conversation = Conversation {
            channel: Arc::new(Allowing),
            events: events_out,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_mins(1)),
        };
        let prompt = Prompt::new("Reply with one word: pong".to_owned(), Vec::new()).unwrap();

        ClaudeBackend::new(cli, settings).run(&session, prompt, inbox_in, conversation).await;

        let mut seen = Vec::new();
        while let Some(event) = events.recv().await {
            seen.push(event);
        }
        assert!(seen.iter().any(|event| matches!(event, AgentEvent::Finished(_))), "{seen:?}");
    }
}
