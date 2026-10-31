//! Round trip through the real `OpenCode`, opt-in:
//! `AGENT_HUB_LIVE_OPENCODE=1 cargo test -p hub-opencode --test live -- --ignored`.
//! The free `opencode` provider needs no login; a configured provider spends real tokens.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hub_agent::channel::UserChannel;
use hub_agent::conversation::{Conversation, Limits};
use hub_core::domain::{
    AbsolutePath, AgentEvent, BackendKind, Decision, Denied, FileDelivery, OutgoingFile, Prompt,
    Question, QuestionsOutcome, SessionId, ToolRequest, TopicSession,
};
use hub_core::settings::OpencodeSettings;
use hub_opencode::backend::OpencodeBackend;
use hub_opencode::version::{check, locate};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Denying;

impl UserChannel for Denying {
    fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
        Box::pin(async { Decision::Denied(Denied::new("live test")) })
    }
    fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        Box::pin(async { QuestionsOutcome::Answered(Vec::new()) })
    }
    fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(async { FileDelivery::Delivered })
    }
}

use futures::future::BoxFuture;

async fn turn(backend: &OpencodeBackend, session: &TopicSession, text: &str) -> Vec<AgentEvent> {
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
#[ignore = "needs an installed opencode and spends tokens"]
async fn a_loaded_session_keeps_its_history() {
    if std::env::var_os("AGENT_HUB_LIVE_OPENCODE").is_none() {
        return;
    }
    let cli = locate(None).unwrap();
    check(&cli).await.unwrap();
    let settings = OpencodeSettings { cli: None, model: None };
    let backend = OpencodeBackend::new(cli, settings);
    let cwd: PathBuf =
        std::env::temp_dir().join(format!("agent-hub-live-opencode-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = AbsolutePath::new(cwd).unwrap();
    let first = turn(
        &backend,
        &TopicSession::fresh(BackendKind::Opencode, cwd.clone()),
        "Запомни кодовое слово «бирюза». Ответь одним словом: ок",
    )
    .await;
    let id = finished_session(&first);

    let moved = TopicSession::fresh(BackendKind::Opencode, cwd).with_session(Some(id));
    let second =
        turn(&backend, &moved, "Какое кодовое слово я просил запомнить? Ответь одним словом.")
            .await;
    assert!(said(&second).to_lowercase().contains("бирюз"), "{second:?}");
}
