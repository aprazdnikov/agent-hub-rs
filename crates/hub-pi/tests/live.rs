//! Круг по настоящему `Pi`, по желанию оператора:
//! `AGENT_HUB_LIVE_PI=1 cargo test -p hub-pi --test live -- --ignored`.
//! Нужен установленный `pi` со входом в подписку; модель тратит реальные токены.
//! Переменная `AGENT_HUB_LIVE_PI_MODEL` выбирает подписку (по умолчанию — бесплатная Qwen).
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_agent::channel::UserChannel;
use hub_agent::conversation::{Conversation, Limits};
use hub_core::domain::{
    AbsolutePath, AgentEvent, BackendKind, Decision, Denied, FileDelivery, OutgoingFile, Prompt,
    Question, QuestionsOutcome, SessionId, ToolRequest, TopicSession,
};
use hub_core::settings::{PiApproval, PiSettings, PiThinking};
use hub_pi::backend::PiBackend;
use hub_pi::version::{check, locate};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Оператор живого прогона не отвечает: согласований тест не требует.
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

fn model() -> String {
    std::env::var("AGENT_HUB_LIVE_PI_MODEL")
        .unwrap_or_else(|_| "qwen-token-plan/qwen3.8-max".to_owned())
}

fn backend(thinking: PiThinking) -> PiBackend {
    let cli = locate(None).unwrap();
    let settings =
        PiSettings { cli: None, model: Some(model()), approval: PiApproval::Default, thinking };
    PiBackend::new(cli, settings)
}

fn cwd() -> AbsolutePath {
    let dir = std::env::temp_dir().join(format!("agent-hub-live-pi-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    AbsolutePath::new(dir).unwrap()
}

async fn turn(backend: &PiBackend, session: &TopicSession, text: &str) -> Vec<AgentEvent> {
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
        .find_map(|event| {
            if let AgentEvent::AssistantText(text) = event { Some(text.clone()) } else { None }
        })
        .unwrap_or_default()
}

/// `Pi` сам выдаёт идентификатор сессии: хаб учится его у `get_state` и обязан
/// вернуть тот же самый, иначе продолжение разговора невозможно.
#[tokio::test]
#[ignore = "нужен установленный pi и реальная подписка"]
async fn a_loaded_session_keeps_its_history() {
    if std::env::var_os("AGENT_HUB_LIVE_PI").is_none() {
        return;
    }
    check(&locate(None).unwrap()).await.unwrap();
    let backend = backend(PiThinking::Low);
    let root = cwd();
    let first = turn(
        &backend,
        &TopicSession::fresh(BackendKind::Pi, root.clone()),
        "Запомни кодовое слово «бирюза». Ответь одним словом: ок",
    )
    .await;
    let id = finished_session(&first);

    let moved = TopicSession::fresh(BackendKind::Pi, root).with_session(Some(id.clone()));
    let second =
        turn(&backend, &moved, "Какое кодовое слово я просил запомнить? Ответь одним словом.")
            .await;
    assert_eq!(finished_session(&second), id, "Pi opened another session");
    assert!(said(&second).to_lowercase().contains("бирюз"), "{second:?}");
}

/// Уровень размышлений применяется к сессии: неверный уровень убил бы процесс,
/// поэтому ход обязан закончиться ответом при каждом advertised значении.
#[tokio::test]
#[ignore = "нужен установленный pi и реальная подписка"]
async fn thinking_level_is_applied_to_the_turn() {
    if std::env::var_os("AGENT_HUB_LIVE_PI").is_none() {
        return;
    }
    let backend = backend(PiThinking::High);
    let events = turn(
        &backend,
        &TopicSession::fresh(BackendKind::Pi, cwd()),
        "Сколько будет 7*6? Ответь одним числом.",
    )
    .await;
    assert!(matches!(events.last(), Some(AgentEvent::Finished(_))), "{events:?}");
    assert!(said(&events).contains('4'), "{events:?}");
}
