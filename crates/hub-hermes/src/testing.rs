//! In-memory ACP transcript regressions using the real shared RPC reader/writer.

use super::*;
use futures::future::BoxFuture;
use hub_agent::channel::UserChannel;
use hub_agent::rpc::RequestError;
use hub_core::domain::{
    Decision, Denied, FileDelivery, Finished, OutgoingFile, Question, QuestionsOutcome,
    ToolRequest, ToolUse, Usage,
};
use std::fmt::Write as _;
use tokio::io::AsyncWriteExt as _;
use tokio_util::sync::CancellationToken;

struct Human;
impl UserChannel for Human {
    fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
        Box::pin(async { Decision::Denied(Denied::new("fixture")) })
    }
    fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        Box::pin(async { QuestionsOutcome::Denied(Denied::new("fixture")) })
    }
    fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(async { FileDelivery::Delivered })
    }
}

fn connection() -> (Connection, tokio::io::DuplexStream) {
    let (host, peer) = tokio::io::duplex(8192);
    let (read, write) = tokio::io::split(host);
    let handler: Handler =
        Arc::new(|method, _| Box::pin(async move { Err(RequestError::Unsupported(method)) }));
    (
        rpc::connect(
            read,
            write,
            handler,
            Wire {
                peer: "hermes-fixture",
                envelope: Envelope::JsonRpc2,
                timeout: Duration::from_secs(1),
            },
        ),
        peer,
    )
}

fn conversation() -> (Conversation, mpsc::Receiver<AgentEvent>) {
    let (events, received) = mpsc::channel(16);
    (
        Conversation {
            channel: Arc::new(Human),
            events,
            cancel: CancellationToken::new(),
            limits: hub_agent::conversation::Limits::new(Duration::from_secs(1)),
        },
        received,
    )
}

#[tokio::test]
async fn coalesced_updates_before_result_are_not_lost_or_mixed_with_other_sessions() {
    let (Connection { client, mut notifications, reader }, peer) = connection();
    let server = tokio::spawn(async move {
        let (read, mut write) = tokio::io::split(peer);
        let mut lines = FramedRead::new(read, LinesCodec::new());
        let request: Value = serde_json::from_str(&lines.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(request["method"], "session/prompt");
        let update = |session: &str, update: Value| json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session, "update": update}});
        let transcript = [
            update("other", json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "foreign"}})),
            update("s-1", json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "before"}})),
            update("s-1", json!({"sessionUpdate": "tool_call", "toolCallId": "t", "kind": "execute", "title": "command"})),
            update("s-1", json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "after"}})),
            json!({"jsonrpc": "2.0", "id": request["id"], "result": {"stopReason": "end_turn", "usage": {"totalTokens": 0}}}),
        ].iter().fold(String::new(), |mut text, message| {
            writeln!(&mut text, "{message}").unwrap();
            text
        });
        write.write_all(transcript.as_bytes()).await.unwrap();
    });
    let id = SessionId::parse("s-1").unwrap();
    let prompt = Prompt::new("test".to_owned(), Vec::new()).unwrap();
    let (conversation, mut events) = conversation();
    let mut tracker = TurnTracker::new(id.clone());
    let result = turn(&client, &mut notifications, &id, &prompt, &mut tracker, &conversation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(events.try_recv().unwrap(), AgentEvent::AssistantText("before".to_owned()));
    assert_eq!(
        events.try_recv().unwrap(),
        AgentEvent::ToolCall(ToolUse { tool: "execute".to_owned(), summary: "command".to_owned() })
    );
    assert!(events.try_recv().is_err());
    assert_eq!(
        tracker.finish(&result),
        vec![
            AgentEvent::AssistantText("after".to_owned()),
            AgentEvent::Finished(Finished {
                session: id,
                usage: Usage::Hermes { tokens: Some(0) },
                background: 0
            }),
        ]
    );
    server.await.unwrap();
    reader.await.unwrap();
}

#[tokio::test]
async fn peer_eof_fails_an_inflight_prompt_without_waiting_for_control_timeout() {
    let (Connection { client, mut notifications, reader }, peer) = connection();
    let server = tokio::spawn(async move {
        let mut lines = FramedRead::new(peer, LinesCodec::new());
        assert!(lines.next().await.is_some());
        // Dropping the peer closes both halves while the prompt request is still pending.
    });
    let id = SessionId::parse("s-1").unwrap();
    let prompt = Prompt::new("test".to_owned(), Vec::new()).unwrap();
    let (conversation, mut events) = conversation();
    let mut tracker = TurnTracker::new(id.clone());
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        turn(&client, &mut notifications, &id, &prompt, &mut tracker, &conversation),
    )
    .await
    .unwrap();
    assert!(matches!(result, Err(RpcError::Closed)));
    assert!(events.try_recv().is_err());
    server.await.unwrap();
    reader.await.unwrap();
}
