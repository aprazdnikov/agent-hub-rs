//! Round trip through the real CLI. Spends a few tokens, so it is opt-in:
//! `AGENT_HUB_LIVE_CLI=1 cargo test -p hub-claude --test live -- --ignored`.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_agent::archive::SessionArchive;
    use hub_agent::channel::UserChannel;
    use hub_agent::conversation::{Conversation, Limits};
    use hub_claude::archive::{ClaudeArchive, storage_root};
    use hub_claude::backend::ClaudeBackend;
    use hub_claude::version::{check, locate};
    use hub_core::domain::{
        AbsolutePath, AgentEvent, BackendKind, Decision, FileDelivery, OutgoingFile, Prompt,
        Question, QuestionsOutcome, SessionId, ToolRequest, TopicSession,
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

    fn home() -> PathBuf {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap()
    }

    async fn turn(backend: &ClaudeBackend, session: &TopicSession, text: &str) -> Vec<AgentEvent> {
        let (events_out, mut events) = mpsc::channel(256);
        let (_inbox, inbox_in) = mpsc::channel(1);
        let conversation = Conversation {
            channel: Arc::new(Allowing),
            events: events_out,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_mins(1)),
        };
        let prompt = Prompt::new(text.to_owned(), Vec::new()).unwrap();
        tokio::time::timeout(
            Duration::from_mins(5),
            backend.run(session, prompt, inbox_in, conversation),
        )
        .await
        .unwrap();
        std::iter::from_fn(|| events.try_recv().ok()).collect()
    }

    fn finished_session(seen: &[AgentEvent]) -> SessionId {
        seen.iter()
            .find_map(|event| {
                if let AgentEvent::Finished(finished) = event {
                    Some(finished.session.clone())
                } else {
                    None
                }
            })
            .unwrap()
    }

    fn said(seen: &[AgentEvent]) -> String {
        seen.iter()
            .filter_map(|event| {
                if let AgentEvent::AssistantText(text) = event { Some(text.as_str()) } else { None }
            })
            .collect()
    }

    #[tokio::test]
    #[ignore = "talks to the real agent CLI and spends tokens"]
    async fn live_snapshot_round_trip_keeps_the_history() {
        if std::env::var_os("AGENT_HUB_LIVE_CLI").is_none() {
            return;
        }
        let cli = locate(None).unwrap();
        let version = check(&cli).await.unwrap();
        let settings = ClaudeSettings {
            cli: None,
            model: Some("haiku".to_owned()),
            permission_mode: PermissionMode::Default,
            budget: None,
        };
        let backend = ClaudeBackend::new(cli, settings);
        let workdir =
            std::env::temp_dir().join(format!("agent-hub-live-snap-{}", std::process::id()));
        std::fs::create_dir_all(&workdir).unwrap();
        let cwd = AbsolutePath::new(workdir).unwrap();
        let fresh = TopicSession::fresh(BackendKind::Claude, cwd.clone());
        let first =
            turn(&backend, &fresh, "Запомни кодовое слово «бирюза». Ответь одним словом: ок").await;
        let id = finished_session(&first);

        let root = storage_root(std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(), &home()).unwrap();
        let archive = ClaudeArchive::new(root.clone(), version.to_string());
        let snapshot = tempfile::tempdir().unwrap();
        let exported = archive.export(&id, &cwd, snapshot.path()).unwrap();
        for file in &exported.files {
            std::fs::remove_file(file.under(&root)).unwrap();
        }
        assert!(!archive.present(&id, &cwd, &exported.data).unwrap());
        archive.import(&id, &exported.data, &exported.files, snapshot.path(), &cwd).unwrap();
        assert!(archive.present(&id, &cwd, &exported.data).unwrap());

        let second = turn(
            &backend,
            &fresh.with_session(Some(id)),
            "Какое кодовое слово я просил запомнить? Ответь одним словом.",
        )
        .await;
        assert!(said(&second).to_lowercase().contains("бирюз"), "{second:?}");
    }
}
