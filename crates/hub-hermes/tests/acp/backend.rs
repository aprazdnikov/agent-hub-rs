use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_agent::channel::UserChannel;
use hub_agent::conversation::{Conversation, Limits};
use hub_core::domain::{
    AbsolutePath, AgentEvent, BackendKind, Decision, Denied, FileDelivery, Finished, OutgoingFile,
    Prompt, Question, QuestionsOutcome, SessionId, ToolRequest, ToolUse, TopicSession, Usage,
};
use hub_core::settings::HermesSettings;
use hub_hermes::backend::HermesBackend;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::common::Fixture;
use rstest::rstest;
use serde_json::{Value, json};

struct Human;
impl UserChannel for Human {
    fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
        Box::pin(async { Decision::Denied(Denied::new("fixture denial")) })
    }
    fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        Box::pin(async { QuestionsOutcome::Denied(Denied::new("fixture denial")) })
    }
    fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(async { FileDelivery::Delivered })
    }
}

fn prompt(text: &str) -> Prompt {
    Prompt::new(text.to_owned(), Vec::new()).unwrap()
}

fn topic(fixture: &Fixture, saved: Option<&str>) -> TopicSession {
    TopicSession::fresh(
        BackendKind::Hermes,
        AbsolutePath::new(fixture.root().to_path_buf()).unwrap(),
    )
    .with_session(saved.and_then(SessionId::parse))
}

fn settings() -> HermesSettings {
    HermesSettings {
        cli: None,
        profile: Some("test-profile".to_owned()),
        model: Some("test-model".to_owned()),
    }
}

fn conversation() -> (Conversation, mpsc::Receiver<AgentEvent>) {
    let (events, received) = mpsc::channel(64);
    (
        Conversation {
            channel: Arc::new(Human),
            events,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_secs(1)),
        },
        received,
    )
}

async fn run(fixture: &Fixture, saved: Option<&str>) -> Vec<AgentEvent> {
    let (conversation, mut received) = conversation();
    let (_send, inbox) = mpsc::channel(4);
    let backend = HermesBackend::new(fixture.cli.clone(), settings());
    let _inbox = tokio::time::timeout(
        Duration::from_secs(15),
        backend.run(&topic(fixture, saved), prompt("hello"), inbox, conversation),
    )
    .await
    .unwrap();
    let mut events = Vec::new();
    while let Some(event) = received.recv().await {
        events.push(event);
    }
    events
}

fn methods(fixture: &Fixture) -> Vec<String> {
    fixture
        .records()
        .iter()
        .filter_map(|record| record.get("method").and_then(Value::as_str))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn loads_saved_session_without_replaying_old_events() {
    let fixture = Fixture::new("load");
    let events = run(&fixture, Some("s-1")).await;
    assert!(events.iter().all(|event| match event {
        AgentEvent::AssistantText(text) => !text.contains("replay"),
        AgentEvent::ToolCall(tool) => !tool.summary.contains("replay"),
        _ => true,
    }));
    assert_eq!(
        methods(&fixture),
        ["initialize", "session/load", "session/set_model", "session/set_mode", "session/prompt"]
    );
}

#[rstest]
#[case("load_error", Some("s-1"))]
#[case("load_null", Some("s-1"))]
#[case("no_load", Some("s-1"))]
#[case("bad_protocol", None)]
#[case("initialize_eof", None)]
#[case("model_null", None)]
#[tokio::test]
async fn unavailable_sessions_or_bad_handshakes_fail_without_prompting(
    #[case] case: &str,
    #[case] saved: Option<&str>,
) {
    let fixture = Fixture::new(case);
    assert!(matches!(run(&fixture, saved).await.as_slice(), [AgentEvent::Failed(_)]));
    assert!(
        !methods(&fixture)
            .iter()
            .any(|method| method == "session/prompt" || method == "session/resume")
    );
    if saved.is_some() {
        assert!(!methods(&fixture).iter().any(|method| method == "session/new"));
    }
}

#[tokio::test]
async fn eof_during_prompt_reports_failure_instead_of_a_finished_turn() {
    let fixture = Fixture::new("prompt_eof");
    assert!(matches!(
        run(&fixture, None).await.as_slice(),
        [AgentEvent::SessionStarted(_), AgentEvent::Failed(_)]
    ));
}

#[tokio::test]
async fn absent_usage_remains_unknown_despite_estimated_usage_update() {
    let fixture = Fixture::new("no_usage");
    assert!(matches!(
        run(&fixture, None).await.last(),
        Some(AgentEvent::Finished(Finished { usage: Usage::Hermes { tokens: None }, .. }))
    ));
}

#[tokio::test]
async fn launch_preserves_profile_cwd_and_approval_policy() {
    let fixture = Fixture::new("new");
    let _events = run(&fixture, None).await;
    let records = fixture.records();
    assert_eq!(
        records.first().and_then(|record| record.get("argv")),
        Some(&json!(["--profile", "test-profile", "acp"]))
    );
    let cwd = std::fs::canonicalize(fixture.root()).unwrap();
    assert_eq!(
        records
            .first()
            .and_then(|record| record.get("cwd"))
            .and_then(Value::as_str)
            .map(std::path::PathBuf::from),
        Some(cwd)
    );
    let mode =
        records.iter().find(|record| record.get("method") == Some(&json!("session/set_mode")));
    assert_eq!(mode.and_then(|mode| mode.pointer("/params/modeId")), Some(&json!("default")));
    let model =
        records.iter().find(|record| record.get("method") == Some(&json!("session/set_model")));
    assert_eq!(
        model.and_then(|model| model.pointer("/params/modelId")),
        Some(&json!("test-model"))
    );
}

#[rstest]
#[case("permission_invalid")]
#[case("permission_wrong_session")]
#[tokio::test]
async fn malformed_permissions_fail_closed(#[case] case: &str) {
    let fixture = Fixture::new(case);
    let _events = run(&fixture, None).await;
    let response =
        fixture.records().into_iter().find(|record| record.get("id") == Some(&json!("permission")));
    assert_eq!(
        response.and_then(|record| record.pointer("/error/code").cloned()),
        Some(json!(-32602))
    );
}

#[tokio::test]
async fn actual_session_mcp_supports_questions_and_file_delivery() {
    let fixture = Fixture::new("mcp");
    std::fs::write(fixture.root().join("report.txt"), "fixture report").unwrap();
    let events = run(&fixture, None).await;
    assert!(matches!(events.last(), Some(AgentEvent::Finished(_))));
    let records = fixture.records();
    let list = records.iter().find_map(|record| record.get("mcp_list")).unwrap();
    let names: Vec<_> = list
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, ["send_file", "ask_user"]);
    let question = records.iter().find_map(|record| record.get("mcp_ask")).unwrap();
    assert_eq!(question.pointer("/result/isError"), Some(&json!(true)));
    let file = records.iter().find_map(|record| record.get("mcp_file")).unwrap();
    assert_eq!(file.pointer("/result/isError"), Some(&json!(false)));
    let prompt = records
        .iter()
        .find(|record| record.get("method") == Some(&json!("session/prompt")))
        .unwrap();
    assert!(
        prompt
            .pointer("/params/prompt/0/text")
            .and_then(Value::as_str)
            .unwrap()
            .contains("ask_user")
    );
}

struct Harness {
    sender: mpsc::Sender<Prompt>,
    events: mpsc::Receiver<AgentEvent>,
    cancel: CancellationToken,
    done: tokio::task::JoinHandle<mpsc::Receiver<Prompt>>,
}

fn start(
    fixture: &Fixture,
    conversation: Conversation,
    events: mpsc::Receiver<AgentEvent>,
) -> Harness {
    let cancel = conversation.cancel.clone();
    let (sender, inbox) = mpsc::channel(8);
    let backend = HermesBackend::new(fixture.cli.clone(), settings());
    let topic = topic(fixture, None);
    let done =
        tokio::spawn(
            async move { backend.run(&topic, prompt("first"), inbox, conversation).await },
        );
    Harness { sender, events, cancel, done }
}

async fn until_tool(events: &mut mpsc::Receiver<AgentEvent>) {
    loop {
        let event =
            tokio::time::timeout(Duration::from_secs(5), events.recv()).await.unwrap().unwrap();
        if matches!(event, AgentEvent::ToolCall(_)) {
            return;
        }
        assert!(!matches!(event, AgentEvent::Failed(_)));
    }
}

#[tokio::test]
async fn midturn_inbox_is_not_drained_and_runs_sequentially() {
    let fixture = Fixture::new("queue");
    let (conversation, events) = conversation();
    let mut harness = start(&fixture, conversation, events);
    until_tool(&mut harness.events).await;
    harness.sender.send(prompt("second")).await.unwrap();
    harness.sender.send(prompt("third")).await.unwrap();
    assert_eq!(
        methods(&fixture).iter().filter(|method| method.as_str() == "session/prompt").count(),
        1
    );
    std::fs::write(fixture.root().join("release"), "").unwrap();
    let mut inbox =
        tokio::time::timeout(Duration::from_secs(5), harness.done).await.unwrap().unwrap();
    assert!(inbox.try_recv().is_err());
    let texts: Vec<_> = fixture
        .records()
        .into_iter()
        .filter(|record| record.get("method") == Some(&json!("session/prompt")))
        .filter_map(|record| {
            record
                .pointer("/params/prompt")
                .and_then(Value::as_array)
                .and_then(|blocks| blocks.last())
                .and_then(|block| block.get("text"))
                .cloned()
        })
        .collect();
    assert_eq!(texts, [json!("first"), json!("second"), json!("third")]);
}

#[tokio::test]
async fn prompt_arriving_after_end_turn_is_left_in_returned_inbox() {
    let fixture = Fixture::new("new");
    let (conversation, events) = conversation();
    let harness = start(&fixture, conversation, events);
    let mut inbox =
        tokio::time::timeout(Duration::from_secs(5), harness.done).await.unwrap().unwrap();
    harness.sender.send(prompt("late")).await.unwrap();
    assert_eq!(inbox.try_recv().unwrap().text(), "late");
}

#[tokio::test]
async fn stop_during_turn_keeps_midturn_messages_in_inbox() {
    let fixture = Fixture::new("stop_turn");
    let (conversation, events) = conversation();
    let mut harness = start(&fixture, conversation, events);
    until_tool(&mut harness.events).await;
    harness.sender.send(prompt("queued")).await.unwrap();
    harness.cancel.cancel();
    std::fs::write(fixture.root().join("release"), "").unwrap();
    let mut inbox =
        tokio::time::timeout(Duration::from_secs(5), harness.done).await.unwrap().unwrap();
    assert_eq!(inbox.try_recv().unwrap().text(), "queued");
    assert!(methods(&fixture).iter().any(|method| method == "session/cancel"));
    assert!(harness.events.try_recv().is_err());
}

#[tokio::test]
async fn cancelled_prompt_result_does_not_start_queued_turn() {
    let fixture = Fixture::new("cancelled_result");
    let (conversation, events) = conversation();
    let harness = start(&fixture, conversation, events);
    harness.sender.send(prompt("queued")).await.unwrap();
    let mut inbox =
        tokio::time::timeout(Duration::from_secs(5), harness.done).await.unwrap().unwrap();
    assert_eq!(inbox.try_recv().unwrap().text(), "queued");
}

#[tokio::test]
async fn pre_cancelled_conversation_never_spawns_child() {
    let fixture = Fixture::new("new");
    let (conversation, events) = conversation();
    conversation.cancel.cancel();
    let harness = start(&fixture, conversation, events);
    let _inbox = harness.done.await.unwrap();
    assert!(!fixture.root().join("wire.jsonl").exists());
}

#[tokio::test]
async fn stop_while_initializing_does_not_wait_for_rpc_deadline() {
    let fixture = Fixture::new("initialize_hang");
    let (conversation, events) = conversation();
    let harness = start(&fixture, conversation, events);
    // Wait for the actual child to enter initialize, not for an arbitrary sleep.
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixture.root().join("initialized").exists() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    harness.cancel.cancel();
    let _inbox = tokio::time::timeout(Duration::from_secs(4), harness.done).await.unwrap().unwrap();
    assert_eq!(methods(&fixture), ["initialize"]);
}

#[tokio::test]
async fn unsupported_images_are_rejected_before_prompt_rpc() {
    let fixture = Fixture::new("no_images");
    let (conversation, mut events) = conversation();
    let (_sender, inbox) = mpsc::channel(4);
    let image = hub_core::domain::Image {
        media: hub_core::domain::ImageMediaType::Png,
        data: vec![1, 2, 3],
    };
    let backend = HermesBackend::new(fixture.cli.clone(), settings());
    let _inbox = backend
        .run(
            &topic(&fixture, None),
            Prompt::new(String::new(), vec![image]).unwrap(),
            inbox,
            conversation,
        )
        .await;
    assert!(matches!(events.recv().await, Some(AgentEvent::SessionStarted(_))));
    assert!(matches!(events.recv().await, Some(AgentEvent::Failed(_))));
    assert!(!methods(&fixture).iter().any(|method| method == "session/prompt"));
}

#[tokio::test]
async fn permission_denial_selects_only_the_offered_reject_once_option() {
    let fixture = Fixture::new("permission");
    let _events = run(&fixture, None).await;
    let response = fixture
        .records()
        .into_iter()
        .find(|record| record.get("id") == Some(&serde_json::json!("permission")));
    assert_eq!(
        response.and_then(|record| record.get("result").cloned()),
        Some(serde_json::json!({"outcome": {"outcome": "selected", "optionId": "deny"}}))
    );
}

#[tokio::test]
async fn opens_new_session_and_reports_prompt_result_usage() {
    let fixture = Fixture::new("new");
    let id = SessionId::parse("s-1").unwrap();
    assert_eq!(
        run(&fixture, None).await,
        vec![
            AgentEvent::SessionStarted(id.clone()),
            AgentEvent::AssistantText("first ".to_owned()),
            AgentEvent::ToolCall(ToolUse {
                tool: "execute".to_owned(),
                summary: "fixture tool".to_owned()
            }),
            AgentEvent::AssistantText("answer".to_owned()),
            AgentEvent::Finished(Finished {
                session: id,
                usage: Usage::Hermes { tokens: Some(7) },
                background: 0
            }),
        ]
    );
}
