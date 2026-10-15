//! Round trip through the real Codex app-server. Spends a few tokens, so it is opt-in:
//! `AGENT_HUB_LIVE_CODEX=1 cargo test -p hub-codex --test live -- --ignored`.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_agent::archive::SessionArchive;
    use hub_agent::channel::UserChannel;
    use hub_agent::conversation::{Conversation, Limits};
    use hub_codex::archive::{CodexArchive, storage_root};
    use hub_codex::backend::CodexBackend;
    use hub_codex::version::{check, locate};
    use hub_core::domain::{
        AbsolutePath, AgentEvent, BackendKind, Decision, FileDelivery, OutgoingFile, Prompt,
        Question, QuestionsOutcome, SessionId, ToolRequest, TopicSession, Usage,
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

    fn home() -> PathBuf {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap()
    }

    async fn turn(backend: &CodexBackend, session: &TopicSession, text: &str) -> Vec<AgentEvent> {
        let (events_out, mut events) = mpsc::channel(256);
        let (_inbox, inbox_in) = mpsc::channel(1);
        let conversation = Conversation {
            channel: Arc::new(Denying),
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
    #[ignore = "needs a logged-in codex and spends tokens"]
    async fn codex_snapshot_round_trip_keeps_the_history() {
        if std::env::var_os("AGENT_HUB_LIVE_CODEX").is_none() {
            return;
        }
        let cli = locate(None).unwrap();
        let version = check(&cli).await.unwrap();
        let settings = CodexSettings {
            cli: None,
            model: None,
            sandbox: Sandbox::ReadOnly,
            approval: Approval::Untrusted,
            api_key: None,
        };
        let backend = CodexBackend::new(cli, settings);
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        let fresh = TopicSession::fresh(BackendKind::Codex, cwd.clone());
        let first =
            turn(&backend, &fresh, "Запомни кодовое слово «бирюза». Ответь одним словом: ок").await;
        let id = finished_session(&first);

        let root = storage_root(std::env::var_os("CODEX_HOME").as_deref(), &home());
        let archive = CodexArchive::new(root.clone(), version.to_string());
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
