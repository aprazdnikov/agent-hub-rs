//! Регрессии связи и цикла хода на реальном wire-контракте `Pi`.
//!
//! Подставной агент говорит по JSON-lines через duplex-канал: отвечает на команды по их `id`
//! и присылает события ровно в той форме, что снята с установленной сборки 1.6.0. Каждый тест
//! разбирает подставной процесс на части, чтобы незавершённая команда не блокировала того,
//! кто на неё отвечает.

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
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, DuplexStream, duplex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{open, settle, turn};
use crate::link::{Link, LinkError};
use crate::protocol::{self, Wire};
use crate::requests::Gate;

const PIPE: usize = 64 * 1024;
const FIVE: Duration = Duration::from_secs(5);
const SESSION: &str = "01a21635-d3b7-7148-8b45-a492b1a4ab56";

/// Трубка с двух сторон: `link` — это хаб, `heard` — команды хаба, `answers` — реплики агента.
struct Child {
    link: Link,
    heard: BufReader<DuplexStream>,
    answers: DuplexStream,
}

fn child() -> Child {
    let (to_hub, child_reads) = duplex(PIPE);
    let (child_writes, to_link) = duplex(PIPE);
    let gate = Gate::new(Arc::new(Refusing), CancellationToken::new());
    Child {
        link: Link::spawn(to_link, to_hub, &gate),
        heard: BufReader::new(child_reads),
        answers: child_writes,
    }
}

/// Канал, который всегда отказывает: диалоги согласования проверяются отдельно.
struct Refusing;

impl UserChannel for Refusing {
    fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
        Box::pin(async { Decision::Denied(Denied { reason: "нет".to_owned() }) })
    }

    fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        Box::pin(async { QuestionsOutcome::Denied(Denied { reason: "нет".to_owned() }) })
    }

    fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(async { FileDelivery::Delivered })
    }
}

/// Одна строка от агента хабу.
async fn say(answers: &mut DuplexStream, message: Value) {
    let line = format!("{message}\n");
    answers.write_all(line.as_bytes()).await.unwrap();
    answers.flush().await.unwrap();
}

/// Одна команда хаба, прочитанная и распакованная. EOF — это провал теста, а не пустой ответ.
async fn asked(reader: &mut BufReader<DuplexStream>) -> Value {
    let mut line = String::new();
    let written = reader.read_line(&mut line).await.unwrap();
    assert!(written > 0, "хаб закрыл ввод агента, не отправив команды");
    serde_json::from_str(&line).unwrap()
}

/// Ждёт команды, отвечает на неё тем же `id` и возвращает команду для проверок.
async fn answer(
    reader: &mut BufReader<DuplexStream>,
    writer: &mut DuplexStream,
    command: &str,
    data: Value,
) -> Value {
    let sent = asked(reader).await;
    let id = sent["id"].clone();
    say(
        writer,
        json!({"id": id, "type": "response", "command": command, "success": true, "data": data}),
    )
    .await;
    sent
}

fn conversation() -> (Conversation, mpsc::Receiver<AgentEvent>) {
    let (events, seen) = mpsc::channel(16);
    (
        Conversation {
            channel: Arc::new(Refusing),
            events,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_secs(1)),
        },
        seen,
    )
}

fn topic() -> TopicSession {
    TopicSession::fresh(BackendKind::Pi, AbsolutePath::new(std::env::temp_dir()).unwrap())
        .with_session(Some(SessionId::parse(SESSION).unwrap()))
}

fn thinking(level: PiThinking) -> PiSettings {
    PiSettings { cli: None, model: None, approval: PiApproval::Default, thinking: level }
}

async fn drain(mut events: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
    let mut seen = Vec::new();
    while let Some(event) = events.recv().await {
        seen.push(event);
    }
    seen
}

#[tokio::test]
async fn a_reply_is_matched_to_the_command_that_asked() {
    let Child { link, mut heard, mut answers } = child();
    let command = async { link.call("get_state", protocol::get_state(), FIVE).await };
    let scripted = async {
        let sent = asked(&mut heard).await;
        assert_eq!(sent["type"], "get_state");
        // Ответ на чужой id не должен закрыть эту команду.
        say(&mut answers, json!({"id": "cmd-9", "type": "response", "command": "get_state", "success": true, "data": {"sessionId": "wrong"}})).await;
        say(&mut answers, json!({"id": sent["id"], "type": "response", "command": "get_state", "success": true, "data": {"sessionId": SESSION, "thinkingLevel": "medium"}})).await;
    };
    let (outcome, ()) = tokio::join!(command, scripted);
    assert_eq!(outcome.unwrap().unwrap()["sessionId"], SESSION);
}

#[tokio::test]
async fn a_refused_command_carries_the_agent_message() {
    let Child { link, mut heard, mut answers } = child();
    let command =
        async { link.call("set_model", protocol::set_model("openai-codex", "nope"), FIVE).await };
    let scripted = async {
        let sent = asked(&mut heard).await;
        assert_eq!(sent["type"], "set_model");
        say(&mut answers, json!({"id": sent["id"], "type": "response", "command": "set_model", "success": false, "error": "unknown model"})).await;
    };
    let (outcome, ()) = tokio::join!(command, scripted);
    assert_eq!(outcome, Err(LinkError::Refused("unknown model".to_owned())));
}

#[tokio::test]
async fn thinking_is_hidden_and_tool_calls_are_shown() {
    let Child { mut link, heard: _, mut answers } = child();
    let expect = protocol::tool_use(&json!({"toolName": "read", "args": {"path": "Cargo.toml"}}));
    say(&mut answers, json!({"type": "message_update", "assistantMessageEvent": {"type": "text_delta", "delta": "прив"}})).await;
    say(&mut answers, json!({"type": "tool_execution_start", "toolCallId": "c1", "toolName": "read", "args": {"path": "Cargo.toml"}})).await;
    say(&mut answers, json!({"type": "message_update", "assistantMessageEvent": {"type": "thinking_delta", "delta": "скрыто"}})).await;
    say(&mut answers, json!({"type": "agent_end", "messages": [], "willRetry": true})).await;
    say(&mut answers, json!({"type": "agent_settled", "aborted": false})).await;
    let mut seen = Vec::new();
    for _ in 0..4 {
        seen.push(link.event().await.unwrap());
    }
    assert_eq!(
        seen,
        vec![
            Wire::Text("прив".to_owned()),
            Wire::Tool(expect),
            Wire::Retrying,
            Wire::Settled { aborted: false },
        ]
    );
}

#[tokio::test]
async fn a_closed_child_fails_the_command_and_the_events() {
    let Child { mut link, heard: _, answers } = child();
    let command = async { link.call("get_state", protocol::get_state(), FIVE).await };
    drop(answers);
    assert_eq!(command.await, Err(LinkError::Closed));
    // Поток событий тоже заканчивается: цикл хода не крутится вхолостую.
    assert!(link.event().await.is_none());
}

#[tokio::test]
async fn an_open_dialog_is_answered_once() {
    let Child { link: _, mut heard, mut answers } = child();
    let dialog = json!({"type": "extension_ui_request", "id": "u1", "method": "confirm", "title": "agent-hub", "message": "Разрешить bash: ls"});
    say(&mut answers, dialog.clone()).await;
    say(&mut answers, dialog).await;
    let first = asked(&mut heard).await;
    assert_eq!(first, json!({"type": "extension_ui_response", "id": "u1", "confirmed": false}));
}

#[tokio::test]
async fn a_mode_the_model_lacks_is_refused() {
    let Child { mut link, mut heard, mut answers } = child();
    let settings = thinking(PiThinking::Max);
    let session = topic();
    let opening = open(&mut link, &settings, &session);
    let scripted = async {
        let state = answer(
            &mut heard,
            &mut answers,
            "get_state",
            json!({"sessionId": SESSION, "thinkingLevel": "medium"}),
        )
        .await;
        assert_eq!(state["type"], "get_state");
        let levels = answer(
            &mut heard,
            &mut answers,
            "get_available_thinking_levels",
            json!({"levels": ["off", "low", "medium"]}),
        )
        .await;
        assert_eq!(levels["type"], "get_available_thinking_levels");
    };
    let (outcome, ()) = tokio::join!(opening, scripted);
    let error = outcome.unwrap_err();
    assert!(error.to_string().contains("не поддерживает"), "{error}");
}

#[tokio::test]
async fn the_chosen_mode_is_applied_after_startup() {
    let Child { mut link, mut heard, mut answers } = child();
    let settings = thinking(PiThinking::High);
    let session = topic();
    let opening = open(&mut link, &settings, &session);
    let scripted = async {
        answer(
            &mut heard,
            &mut answers,
            "get_state",
            json!({"sessionId": SESSION, "thinkingLevel": "medium"}),
        )
        .await;
        answer(
            &mut heard,
            &mut answers,
            "get_available_thinking_levels",
            json!({"levels": ["off", "medium", "high"]}),
        )
        .await;
        answer(&mut heard, &mut answers, "set_thinking_level", Value::Null).await
    };
    let (outcome, written) = tokio::join!(opening, scripted);
    assert_eq!(outcome.unwrap(), SessionId::parse(SESSION).unwrap());
    assert_eq!(written["type"], "set_thinking_level");
    assert_eq!(written["level"], "high");
}

#[tokio::test]
async fn a_model_switch_reaches_the_running_agent() {
    let Child { mut link, mut heard, mut answers } = child();
    let settings = PiSettings {
        cli: None,
        model: Some("qwen-token-plan/qwen4-max".to_owned()),
        approval: PiApproval::Default,
        thinking: PiThinking::High,
    };
    let session = topic();
    let opening = open(&mut link, &settings, &session);
    let scripted = async {
        answer(
            &mut heard,
            &mut answers,
            "get_state",
            json!({
                "sessionId": SESSION,
                "thinkingLevel": "medium",
                "model": {"id": "gpt-5.6-luna", "provider": "openai-codex"}
            }),
        )
        .await;
        let switch = answer(&mut heard, &mut answers, "set_model", Value::Null).await;
        assert_eq!(switch["type"], "set_model");
        assert_eq!(switch["provider"], "qwen-token-plan");
        assert_eq!(switch["modelId"], "qwen4-max");
        answer(
            &mut heard,
            &mut answers,
            "get_state",
            json!({
                "sessionId": SESSION,
                "thinkingLevel": "medium",
                "model": {"id": "qwen4-max", "provider": "qwen-token-plan"}
            }),
        )
        .await;
        answer(
            &mut heard,
            &mut answers,
            "get_available_thinking_levels",
            json!({"levels": ["low", "high"]}),
        )
        .await;
        answer(&mut heard, &mut answers, "set_thinking_level", Value::Null).await
    };
    let (outcome, written) = tokio::join!(opening, scripted);
    assert_eq!(outcome.unwrap(), SessionId::parse(SESSION).unwrap());
    // Уровень после смены модели читается заново: у новой модели он свой.
    assert_eq!(written["type"], "set_thinking_level");
    assert_eq!(written["level"], "high");
}

#[tokio::test]
async fn a_finished_turn_reports_text_and_usage() {
    let Child { mut link, mut heard, mut answers } = child();
    let (conversation, events) = conversation();
    let id = SessionId::parse(SESSION).unwrap();
    let (_held, mut inbox) = mpsc::channel(1);
    let prompt = Prompt::new("сделай".to_owned(), Vec::new()).unwrap();
    let running = turn(&mut link, &id, &prompt, &mut inbox, &conversation);
    let scripted = async {
        let sent =
            answer(&mut heard, &mut answers, "prompt", json!({"disposition": "started"})).await;
        assert_eq!(sent["type"], "prompt");
        say(&mut answers, json!({"type": "message_update", "assistantMessageEvent": {"type": "text_delta", "delta": "готово"}})).await;
        say(&mut answers, json!({"type": "agent_settled", "aborted": false})).await;
        answer(&mut heard, &mut answers, "get_session_stats", json!({"tokens": {"total": 18894}}))
            .await;
    };
    let (outcome, ()) = tokio::join!(running, scripted);
    outcome.unwrap();
    drop(conversation);
    assert_eq!(
        drain(events).await,
        vec![
            AgentEvent::TurnStarted,
            AgentEvent::AssistantText("готово".to_owned()),
            AgentEvent::Finished(protocol::finished(
                id,
                Some(&json!({"tokens": {"total": 18894}}))
            )),
        ]
    );
}

/// Регрессия живого прогона: модель не найдена, Pi пишет ошибку в `message_end`, и до
/// исправления ход выглядел как «Готово · 0 токенов» без единого слова об отказе.
#[tokio::test]
async fn a_failed_turn_reaches_the_user_instead_of_silence() {
    let Child { mut link, mut heard, mut answers } = child();
    let (conversation, events) = conversation();
    let id = SessionId::parse(SESSION).unwrap();
    let (_held, mut inbox) = mpsc::channel(1);
    let prompt = Prompt::new("сделай".to_owned(), Vec::new()).unwrap();
    let running = turn(&mut link, &id, &prompt, &mut inbox, &conversation);
    let scripted = async {
        answer(&mut heard, &mut answers, "prompt", json!({"disposition": "started"})).await;
        say(&mut answers, json!({"type": "message_end", "message": {"role": "assistant", "stopReason": "error", "errorMessage": "404: Model not exist."}})).await;
    };
    let (outcome, ()) = tokio::join!(running, scripted);
    assert_eq!(outcome.unwrap_err().to_string(), "ход не выполнен: 404: Model not exist.");
    drop(conversation);
    // Ни текста, ни итога: только начатый ход — остальное берёт на себя отчёт об ошибке.
    assert_eq!(drain(events).await, vec![AgentEvent::TurnStarted]);
}

#[tokio::test]
async fn a_mid_turn_message_becomes_a_steer() {
    let Child { link, mut heard, answers: _ } = child();
    let queued = Prompt::new("и ещё это".to_owned(), Vec::new()).unwrap();
    let steered = link.steer(&queued);
    let line = asked(&mut heard);
    let (outcome, line) = tokio::join!(steered, line);
    outcome.unwrap();
    assert_eq!(line["type"], "steer");
    assert_eq!(line["message"], "и ещё это");
}

#[tokio::test]
async fn stopping_aborts_the_running_turn() {
    let Child { link, mut heard, answers: _ } = child();
    let aborting = link.abort();
    let line = asked(&mut heard);
    let ((), line) = tokio::join!(aborting, line);
    assert_eq!(line["type"], "abort");
}

#[tokio::test]
async fn the_turn_outcome_does_not_depend_on_usage() {
    let Child { mut link, mut heard, mut answers } = child();
    let (conversation, events) = conversation();
    let id = SessionId::parse(SESSION).unwrap();
    let settling = settle(&mut link, &id, &conversation);
    let scripted = async {
        let sent = asked(&mut heard).await;
        assert_eq!(sent["type"], "get_session_stats");
        say(&mut answers, json!({"id": sent["id"], "type": "response", "command": "get_session_stats", "success": false, "error": "нет сессии"})).await;
    };
    let (outcome, ()) = tokio::join!(settling, scripted);
    outcome.unwrap();
    drop(conversation);
    assert_eq!(drain(events).await, vec![AgentEvent::Finished(protocol::finished(id, None))]);
}
