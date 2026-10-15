//! Round trip through the real Hermes, opt-in:
//! `AGENT_HUB_LIVE_HERMES=1 [AGENT_HUB_LIVE_HERMES_PROFILE=<p>] cargo test -p hub-hermes --test live -- --ignored`.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_agent::archive::SessionArchive;
use hub_agent::channel::UserChannel;
use hub_agent::conversation::{Conversation, Limits};
use hub_core::domain::{
    AbsolutePath, AgentEvent, BackendKind, Decision, Denied, FileDelivery, OutgoingFile, Prompt,
    Question, QuestionsOutcome, SessionId, ToolRequest, TopicSession,
};
use hub_core::settings::HermesSettings;
use hub_hermes::archive::HermesArchive;
use hub_hermes::backend::HermesBackend;
use hub_hermes::version::{check, locate};
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

async fn turn(backend: &HermesBackend, session: &TopicSession, text: &str) -> Vec<AgentEvent> {
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
#[ignore = "needs a configured hermes and spends tokens"]
async fn hermes_snapshot_moves_to_another_directory_with_its_history() {
    if std::env::var_os("AGENT_HUB_LIVE_HERMES").is_none() {
        return;
    }
    let cli = locate(None).unwrap();
    let version = check(&cli).await.unwrap();
    let profile = std::env::var("AGENT_HUB_LIVE_HERMES_PROFILE").ok();
    let settings = HermesSettings { cli: None, profile: profile.clone(), model: None };
    let backend = HermesBackend::new(cli.clone(), settings);
    let base: PathBuf =
        std::env::temp_dir().join(format!("agent-hub-live-hermes-{}", std::process::id()));
    let (old, new) = (base.join("old"), base.join("new"));
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&new).unwrap();
    let (old, new) = (AbsolutePath::new(old).unwrap(), AbsolutePath::new(new).unwrap());
    let first = turn(
        &backend,
        &TopicSession::fresh(BackendKind::Hermes, old.clone()),
        "Запомни кодовое слово «бирюза». Ответь одним словом: ок",
    )
    .await;
    let id = finished_session(&first);

    let archive = HermesArchive::new(&cli, profile.clone(), version.to_string()).unwrap();
    let snapshot = tempfile::tempdir().unwrap();
    let exported = archive.export(&id, &old, snapshot.path()).unwrap();
    let deleted = std::process::Command::new(&cli)
        .args(profile.iter().flat_map(|profile| ["--profile", profile.as_str()]))
        .args(["sessions", "delete", id.as_str(), "--yes"])
        .status()
        .unwrap();
    assert!(deleted.success());
    assert!(!archive.present(&id, &new, &exported.data).unwrap());
    archive.import(&id, &exported.data, &exported.files, snapshot.path(), &new).unwrap();
    assert!(archive.present(&id, &new, &exported.data).unwrap());

    let moved = TopicSession::fresh(BackendKind::Hermes, new).with_session(Some(id));
    let second =
        turn(&backend, &moved, "Какое кодовое слово я просил запомнить? Ответь одним словом.")
            .await;
    assert!(said(&second).to_lowercase().contains("бирюз"), "{second:?}");
}
