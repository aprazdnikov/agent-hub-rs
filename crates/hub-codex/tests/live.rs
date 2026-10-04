//! Round trip through the real Codex app-server. Spends a few tokens, so it is opt-in:
//! `AGENT_HUB_LIVE_CODEX=1 cargo test -p hub-codex --test live -- --ignored`.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_agent::channel::UserChannel;
    use hub_agent::conversation::{Conversation, Limits};
    use hub_codex::backend::CodexBackend;
    use hub_codex::version::{check, locate};
    use hub_core::domain::{
        AbsolutePath, AgentEvent, BackendKind, Decision, FileDelivery, OutgoingFile, Prompt,
        Question, QuestionsOutcome, ToolRequest, TopicSession, Usage,
    };
    use hub_core::settings::{Approval, CodexSettings, Sandbox};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    struct Denying;

    impl UserChannel for Denying {
        fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
            Box::pin(async { Decision::Denied(hub_core::domain::Denied::new("live test")) })
        }
        fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            Box::pin(async { QuestionsOutcome::Answered(Vec::new()) })
        }
        fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            Box::pin(async { FileDelivery::Delivered })
        }
    }

    #[tokio::test]
    #[ignore = "needs a logged-in codex and spends tokens"]
    async fn codex_answers_a_prompt() {
        if std::env::var_os("AGENT_HUB_LIVE_CODEX").is_none() {
            return;
        }
        let cli = locate(None).unwrap();
        check(&cli).await.unwrap();
        let settings = CodexSettings {
            cli: None,
            model: None,
            sandbox: Sandbox::ReadOnly,
            approval: Approval::Untrusted,
            api_key: None,
        };
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        let session = TopicSession::fresh(BackendKind::Codex, cwd);
        let (events, mut received) = mpsc::channel(256);
        let conversation = Conversation {
            channel: Arc::new(Denying),
            events,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_mins(1)),
        };
        let (_inbox_in, inbox) = mpsc::channel(1);
        let prompt = Prompt::new("Ответь одним словом: да".to_owned(), Vec::new()).unwrap();

        let backend = CodexBackend::new(cli, settings);
        let run = backend.run(&session, prompt, inbox, conversation);
        let _inbox = tokio::time::timeout(Duration::from_mins(3), run).await.unwrap();

        let mut seen = Vec::new();
        while let Ok(event) = received.try_recv() {
            seen.push(event);
        }
        assert!(matches!(seen.first(), Some(AgentEvent::SessionStarted(_))), "{seen:?}");
        assert!(
            matches!(
                seen.last(),
                Some(AgentEvent::Finished(finished)) if matches!(finished.usage, Usage::Codex { tokens: Some(_) })
            ),
            "{seen:?}"
        );
    }
}
