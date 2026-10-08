//! The `qwen --acp` child for one conversation: the hub's MCP server, the process, the session.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use hub_agent::channel::UserChannel;
use hub_agent::cli::hide_window;
use hub_agent::conversation::Conversation;
use hub_agent::mcp_http::McpHttpServer;
use hub_agent::rpc::{
    self, Connection, Envelope, Handler, Notification, RpcClient, RpcError, Wire,
};
use hub_agent::tools::TOOL_SUMMARY_LIMIT;
use hub_core::domain::{AgentEvent, Prompt, SessionId, TopicSession};
use hub_core::render::truncate;
use hub_core::settings::QwenSettings;
use tokio::io::AsyncRead;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, LinesCodec};

use crate::protocol::{
    AUTH_REQUIRED, Call, HubTools, TurnTracker, initialize_params, open_session, parse_session_id,
};
use crate::requests;
use crate::session::{Drain, DrainRequests, RpcAgent, Turns, converse, drain_channel, emit, fail};

// Handshake and session control only; a turn runs until it ends, /stop, or the process exits.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
// qwen exits on stdin EOF after closing its sessions; the grace lets it do that before a kill.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_STDERR_LINE: usize = 64 * 1024;
// Without it qwen restarts itself as a child Node process that outlives the parent for up to 120 s.
const NO_RELAUNCH: &str = "QWEN_CODE_NO_RELAUNCH";
pub const NOT_AUTHENTICATED: &str =
    "Qwen не авторизован: настройте qwen (/auth) или задайте API-ключ в настройках";
pub const QWEN_FAILED: &str = "Qwen Code завершился, подробности в логе";

/// How Qwen gets its model credentials; shown on the status tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QwenAuth {
    HubKey,
    OwnSetup,
}

impl QwenAuth {
    #[must_use]
    pub fn of(settings: &QwenSettings) -> Self {
        match settings.endpoint {
            Some(_) => Self::HubKey,
            None => Self::OwnSetup,
        }
    }
}

pub struct QwenBackend {
    cli: PathBuf,
    settings: QwenSettings,
}

impl QwenBackend {
    #[must_use]
    pub fn new(cli: PathBuf, settings: QwenSettings) -> Self {
        Self { cli, settings }
    }

    /// Runs one conversation and returns the inbox with the prompts it did not take.
    ///
    /// `serve` runs exactly once per process, and `stop` takes the process by value afterwards,
    /// so no turn can start on a connection whose conversation has ended.
    pub async fn run(
        &self,
        session: &TopicSession,
        prompt: Prompt,
        inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation,
    ) -> mpsc::Receiver<Prompt> {
        let tools = match McpHttpServer::start(Arc::clone(&conversation.channel)).await {
            Ok(tools) => tools,
            Err(error) => {
                tracing::error!(%error, "hub MCP server for qwen did not start");
                fail(&conversation, QWEN_FAILED.to_owned()).await;
                return inbox;
            }
        };
        let (drain, mut drains) = drain_channel();
        let handler = handler(Arc::clone(&conversation.channel), drain);
        let mut agent = match QwenProcess::spawn(&self.cli, &self.settings, handler) {
            Ok(agent) => agent,
            Err(error) => {
                tracing::error!(%error, cli = %self.cli.display(), "qwen did not start");
                fail(&conversation, QWEN_FAILED.to_owned()).await;
                return inbox;
            }
        };
        let link = Link {
            client: &agent.client,
            notifications: &mut agent.notifications,
            drains: &mut drains,
        };
        let hub = HubTools { url: tools.url(), token: tools.token() };
        let inbox = serve(link, &hub, session, prompt, inbox, &conversation).await;
        agent.stop().await;
        // Only now: qwen's MCP client must not see the server vanish in the middle of a call.
        drop(tools);
        inbox
    }
}

fn handler(channel: Arc<dyn UserChannel>, drain: Drain) -> Handler {
    Arc::new(move |method, params| {
        let channel = Arc::clone(&channel);
        let drain = drain.clone();
        Box::pin(async move { requests::answer(channel.as_ref(), &drain, &method, params).await })
    })
}

pub struct Link<'a> {
    pub client: &'a RpcClient,
    pub notifications: &'a mut mpsc::UnboundedReceiver<Notification>,
    pub drains: &'a mut DrainRequests,
}

/// One conversation over an established connection: handshake, session, turns.
pub async fn serve(
    link: Link<'_>,
    tools: &HubTools<'_>,
    session: &TopicSession,
    prompt: Prompt,
    inbox: mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> mpsc::Receiver<Prompt> {
    let Link { client, notifications, drains } = link;
    let opened = tokio::select! {
        biased;
        () = conversation.cancel.cancelled() => return inbox,
        opened = open(client, notifications, tools, session) => opened,
    };
    let id = match opened {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(error = %bounded(&error), "qwen session did not open");
            fail(conversation, error.to_string()).await;
            return inbox;
        }
    };
    emit(conversation, AgentEvent::SessionStarted(id.clone())).await;
    let agent = RpcAgent::new(client.clone(), id.clone());
    let turns = Turns { agent: &agent, notifications, drains };
    let (inbox, outcome) = converse(turns, TurnTracker::new(id), prompt, inbox, conversation).await;
    if let Err(error) = outcome {
        tracing::warn!(error = %bounded(&error), "qwen session broke");
        fail(conversation, session_failure(&error)).await;
    }
    inbox
}

/// Qwen's error text can quote whole commands or replies; logs and the chat get a bounded part.
fn bounded(error: &dyn fmt::Debug) -> String {
    truncate(&format!("{error:?}"), TOOL_SUMMARY_LIMIT)
}

#[derive(Debug)]
enum OpenError {
    NotAuthenticated,
    NotLoaded { session: SessionId, message: String },
    Rpc(RpcError),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAuthenticated => f.write_str(NOT_AUTHENTICATED),
            Self::NotLoaded { session, message } => write!(
                f,
                "Qwen не смог продолжить сессию {}: {}. /reset — начать заново",
                session.as_str(),
                truncate(message, TOOL_SUMMARY_LIMIT)
            ),
            Self::Rpc(error) => f.write_str(&session_failure(error)),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<RpcError> for OpenError {
    fn from(error: RpcError) -> Self {
        Self::Rpc(error)
    }
}

fn session_failure(error: &RpcError) -> String {
    match error {
        RpcError::Remote { message, .. } => {
            format!("Qwen: {}", truncate(message, TOOL_SUMMARY_LIMIT))
        }
        RpcError::Closed | RpcError::Protocol(_) | RpcError::Timeout { .. } => {
            QWEN_FAILED.to_owned()
        }
    }
}

async fn open(
    client: &RpcClient,
    notifications: &mut mpsc::UnboundedReceiver<Notification>,
    tools: &HubTools<'_>,
    session: &TopicSession,
) -> Result<SessionId, OpenError> {
    client.request(Call::Initialize.method(), initialize_params(env!("CARGO_PKG_VERSION"))).await?;
    let (call, params) = open_session(session, tools);
    let opened = client.request(call.method(), params).await;
    // A loaded session replays its history as updates before the reply; none of it is news.
    while notifications.try_recv().is_ok() {}
    match (opened, &session.session) {
        (Ok(result), None) => Ok(parse_session_id(&result)?),
        // `session/load` does not repeat the id it was given.
        (Ok(_), Some(saved)) => Ok(saved.clone()),
        (Err(RpcError::Remote { code: AUTH_REQUIRED, .. }), _) => Err(OpenError::NotAuthenticated),
        (Err(RpcError::Remote { message, .. }), Some(saved)) => {
            Err(OpenError::NotLoaded { session: saved.clone(), message })
        }
        (
            Err(
                error @ (RpcError::Remote { .. }
                | RpcError::Closed
                | RpcError::Protocol(_)
                | RpcError::Timeout { .. }),
            ),
            None,
        )
        | (
            Err(error @ (RpcError::Closed | RpcError::Protocol(_) | RpcError::Timeout { .. })),
            Some(_),
        ) => Err(OpenError::Rpc(error)),
    }
}

fn launch_args(settings: &QwenSettings) -> Vec<&str> {
    ["--acp", "--approval-mode", settings.approval.wire()]
        .into_iter()
        .chain(settings.model.iter().flat_map(|model| ["-m", model.as_str()]))
        .chain(settings.endpoint.iter().flat_map(|_| ["--auth-type", "openai"]))
        .collect()
}

/// The key goes only to this child; without an endpoint qwen keeps its own setup.
fn child_env(settings: &QwenSettings) -> Vec<(&'static str, &str)> {
    settings
        .endpoint
        .iter()
        .flat_map(|endpoint| {
            [("OPENAI_API_KEY", endpoint.key().expose()), ("OPENAI_BASE_URL", endpoint.base_url())]
                .into_iter()
                .chain(settings.model.iter().map(|model| ("OPENAI_MODEL", model.as_str())))
        })
        .collect()
}

struct QwenProcess {
    client: RpcClient,
    notifications: mpsc::UnboundedReceiver<Notification>,
    reader: JoinHandle<()>,
    child: Child,
}

impl QwenProcess {
    fn spawn(cli: &Path, settings: &QwenSettings, handler: Handler) -> io::Result<Self> {
        let mut command = Command::new(cli);
        command
            .args(launch_args(settings))
            .env(NO_RELAUNCH, "true")
            .envs(child_env(settings))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_window(&mut command);
        let mut child = command.spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(io::Error::other("qwen started without standard streams"));
        };
        tokio::spawn(log_stderr(stderr));
        let wire = Wire { peer: "qwen", envelope: Envelope::JsonRpc2, timeout: REQUEST_TIMEOUT };
        let Connection { client, notifications, reader } =
            rpc::connect(stdout, stdin, handler, wire);
        Ok(Self { client, notifications, reader, child })
    }

    /// Closes qwen's stdin, gives it `EXIT_TIMEOUT` to exit, then kills it.
    async fn stop(self) {
        let Self { client, notifications: _, reader, mut child } = self;
        client.close();
        match tokio::time::timeout(EXIT_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => tracing::debug!(%status, "qwen exited"),
            // `child` is dropped below, and `kill_on_drop` still ends the process.
            Ok(Err(error)) => tracing::warn!(%error, "qwen exit status unavailable"),
            Err(_) => {
                tracing::warn!("qwen did not exit on stdin close, killing it");
                if let Err(error) = child.kill().await {
                    tracing::warn!(%error, "qwen could not be killed");
                }
            }
        }
        // Answers still in flight have nobody left to reach.
        reader.abort();
    }
}

async fn log_stderr<R: AsyncRead + Unpin>(stderr: R) {
    let mut lines = FramedRead::new(stderr, LinesCodec::new_with_max_length(MAX_STDERR_LINE));
    while let Some(line) = lines.next().await {
        match line {
            Ok(line) => tracing::info!(%line, "qwen stderr"),
            Err(error) => {
                tracing::debug!(%error, "qwen stderr unreadable");
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use hub_agent::conversation::Limits;
    use hub_agent::tools::TOOL_SUMMARY_LIMIT;
    use hub_core::domain::{AbsolutePath, BackendKind, Finished, Usage};
    use hub_core::settings::{Draft, QwenApproval, QwenDraft};
    use rstest::rstest;
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::testing::{Peer, Scripted, pair};

    enum Reply {
        Result(Value),
        Error(i64, String),
    }

    struct Step {
        method: &'static str,
        before: Vec<Value>,
        reply: Reply,
    }

    fn ok(method: &'static str, result: Value) -> Step {
        Step { method, before: Vec::new(), reply: Reply::Result(result) }
    }

    fn err(method: &'static str, code: i64, message: &str) -> Step {
        Step { method, before: Vec::new(), reply: Reply::Error(code, message.to_owned()) }
    }

    impl Step {
        /// Updates Qwen sends before it replies, as it does for a turn and for a load replay.
        fn preceded_by(self, before: Vec<Value>) -> Self {
            Self { before, ..self }
        }
    }

    async fn next_request(peer: &mut Peer, seen: &mut Vec<Value>) -> Value {
        loop {
            let message = peer.read().await.unwrap();
            seen.push(message.clone());
            if message.get("id").is_some() && message.get("method").is_some() {
                return message;
            }
        }
    }

    /// Answers the client's requests in order and records every message it got.
    async fn script(mut peer: Peer, steps: Vec<Step>) -> (Peer, Vec<Value>) {
        let mut seen = Vec::new();
        for step in steps {
            let request = next_request(&mut peer, &mut seen).await;
            assert_eq!(request.get("method").and_then(Value::as_str), Some(step.method));
            for update in step.before {
                peer.write(update).await;
            }
            let id = request.get("id").cloned().unwrap();
            peer.write(match step.reply {
                Reply::Result(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Reply::Error(code, message) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
                }
            })
            .await;
        }
        (peer, seen)
    }

    /// `script`, then records everything else the client sends until it closes Qwen's input.
    async fn fake_qwen(peer: Peer, steps: Vec<Step>) -> Vec<Value> {
        let (mut peer, mut seen) = script(peer, steps).await;
        while let Some(message) = peer.read().await {
            seen.push(message);
        }
        seen
    }

    fn update(update: &Value) -> Value {
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s-1", "update": update}})
    }

    fn chunk(text: &str) -> Value {
        update(
            &json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}),
        )
    }

    fn usage(total: u64) -> Value {
        update(
            &json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": ""},
                      "_meta": {"usage": {"inputTokens": 1, "outputTokens": 1, "totalTokens": total}}}),
        )
    }

    fn tools() -> HubTools<'static> {
        HubTools { url: "http://127.0.0.1:4321/mcp", token: "t0k3n" }
    }

    fn topic(saved: Option<&str>) -> TopicSession {
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        TopicSession::fresh(BackendKind::Qwen, cwd).with_session(saved.and_then(SessionId::parse))
    }

    fn session_id() -> SessionId {
        SessionId::parse("s-1").unwrap()
    }

    fn prompt(text: &str) -> Prompt {
        Prompt::new(text.to_owned(), Vec::new()).unwrap()
    }

    fn conversation() -> (Conversation, mpsc::Receiver<AgentEvent>) {
        let (events, received) = mpsc::channel(64);
        let conversation = Conversation {
            channel: Arc::new(Scripted::default()),
            events,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_secs(1)),
        };
        (conversation, received)
    }

    async fn collect(mut received: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Some(event) = received.recv().await {
            events.push(event);
        }
        events
    }

    fn methods(seen: &[Value]) -> Vec<String> {
        seen.iter()
            .filter_map(|m| m.get("method").and_then(Value::as_str))
            .map(str::to_owned)
            .collect()
    }

    fn sent<'a>(seen: &'a [Value], method: &str) -> Option<&'a Value> {
        seen.iter().find(|m| m.get("method") == Some(&json!(method))).and_then(|m| m.get("params"))
    }

    /// Runs `serve` against a fake Qwen; returns the events and everything Qwen saw.
    async fn run(topic: TopicSession, steps: Vec<Step>) -> (Vec<AgentEvent>, Vec<Value>) {
        let (drain, mut drains) = drain_channel();
        let (mut connection, peer) =
            pair(handler(Arc::new(Scripted::default()), drain), Duration::from_secs(5));
        let server = tokio::spawn(fake_qwen(peer, steps));
        let (conversation, received) = conversation();
        let (_inbox_in, inbox) = mpsc::channel(4);
        let link = Link {
            client: &connection.client,
            notifications: &mut connection.notifications,
            drains: &mut drains,
        };
        let _inbox = serve(link, &tools(), &topic, prompt("hi"), inbox, &conversation).await;
        drop(conversation);
        // Qwen's input closes, so the fake has seen every message the client would ever send.
        connection.client.close();
        let seen = server.await.unwrap();
        (collect(received).await, seen)
    }

    fn initialized() -> Step {
        ok(
            "initialize",
            json!({"protocolVersion": 1, "agentInfo": {"name": "qwen-code", "version": "0.25.0"}}),
        )
    }

    fn end_turn() -> Value {
        json!({"stopReason": "end_turn"})
    }

    #[tokio::test]
    async fn new_session_brings_the_hub_tools_and_runs_a_turn() {
        let (events, seen) = run(
            topic(None),
            vec![
                initialized(),
                ok("session/new", json!({"sessionId": "s-1", "models": {}, "modes": {}})),
                ok("session/prompt", end_turn()).preceded_by(vec![chunk("Привет"), usage(42)]),
            ],
        )
        .await;
        assert_eq!(
            events,
            [
                AgentEvent::SessionStarted(session_id()),
                AgentEvent::AssistantText("Привет".to_owned()),
                AgentEvent::Finished(Finished {
                    session: session_id(),
                    usage: Usage::Qwen { tokens: Some(42) },
                    background: 0,
                }),
            ]
        );
        assert_eq!(methods(&seen), ["initialize", "session/new", "session/prompt"]);
        assert_eq!(
            sent(&seen, "session/new").and_then(|p| p.pointer("/mcpServers/0/headers/0/value")),
            Some(&json!("Bearer t0k3n"))
        );
        assert!(seen.iter().all(|message| message.get("jsonrpc") == Some(&json!("2.0"))));
    }

    #[tokio::test]
    async fn saved_session_is_loaded_and_its_replay_is_dropped() {
        let replay = vec![
            chunk("старый ответ"),
            update(
                &json!({"sessionUpdate": "tool_call", "toolCallId": "old", "title": "Shell: ls", "kind": "execute"}),
            ),
        ];
        let (events, seen) = run(
            topic(Some("s-1")),
            vec![
                initialized(),
                ok("session/load", json!({"models": {}})).preceded_by(replay),
                ok("session/prompt", end_turn()),
            ],
        )
        .await;
        assert_eq!(
            events,
            [
                AgentEvent::SessionStarted(session_id()),
                AgentEvent::Finished(Finished {
                    session: session_id(),
                    usage: Usage::Qwen { tokens: None },
                    background: 0,
                }),
            ]
        );
        assert_eq!(
            sent(&seen, "session/load").and_then(|p| p.get("sessionId")),
            Some(&json!("s-1"))
        );
    }

    #[rstest]
    #[case::new(None, "session/new")]
    #[case::load(Some("s-1"), "session/load")]
    #[tokio::test]
    async fn missing_auth_fails_with_a_hint(
        #[case] saved: Option<&str>,
        #[case] open: &'static str,
    ) {
        let refused =
            err(open, -32000, "Authentication required: Use Qwen Code CLI to authenticate first.");
        let (events, seen) = run(topic(saved), vec![initialized(), refused]).await;
        assert_eq!(events, [AgentEvent::Failed(NOT_AUTHENTICATED.to_owned())]);
        assert_eq!(methods(&seen), ["initialize", open]);
    }

    #[rstest]
    #[case::not_started(
        None,
        err("session/new", -32603, "Internal error: bad model"),
        "Qwen: Internal error: bad model"
    )]
    #[case::not_loaded(
        Some("s-1"),
        err("session/load", -32002, "Resource not found: session:s-1"),
        "Qwen не смог продолжить сессию s-1: Resource not found: session:s-1. /reset — начать заново"
    )]
    #[tokio::test]
    async fn a_session_that_does_not_open_fails_with_its_reason(
        #[case] saved: Option<&str>,
        #[case] open: Step,
        #[case] reason: &str,
    ) {
        let (events, _seen) = run(topic(saved), vec![initialized(), open]).await;
        assert_eq!(events, [AgentEvent::Failed(reason.to_owned())]);
    }

    #[tokio::test]
    async fn an_error_mid_turn_fails_with_its_message() {
        let (events, _seen) = run(
            topic(None),
            vec![
                initialized(),
                ok("session/new", json!({"sessionId": "s-1"})),
                err("session/prompt", -32603, "Internal error: quota exceeded"),
            ],
        )
        .await;
        assert_eq!(
            events,
            [
                AgentEvent::SessionStarted(session_id()),
                AgentEvent::Failed("Qwen: Internal error: quota exceeded".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn a_long_error_reaches_the_user_cut() {
        let long = format!("Internal error: {}", "x".repeat(TOOL_SUMMARY_LIMIT * 2));
        let (events, _seen) =
            run(topic(None), vec![initialized(), err("session/new", -32603, &long)]).await;
        let cut: String = long.chars().take(TOOL_SUMMARY_LIMIT - 1).collect();
        assert_eq!(events, [AgentEvent::Failed(format!("Qwen: {cut}…"))]);
    }

    #[tokio::test]
    async fn qwen_exiting_mid_turn_fails_the_turn() {
        let (drain, mut drains) = drain_channel();
        let (mut connection, peer) =
            pair(handler(Arc::new(Scripted::default()), drain), Duration::from_secs(5));
        let server = tokio::spawn(async move {
            let steps = vec![initialized(), ok("session/new", json!({"sessionId": "s-1"}))];
            let (mut peer, mut seen) = script(peer, steps).await;
            next_request(&mut peer, &mut seen).await;
            drop(peer);
        });
        let (conversation, received) = conversation();
        let (_inbox_in, inbox) = mpsc::channel(4);
        let link = Link {
            client: &connection.client,
            notifications: &mut connection.notifications,
            drains: &mut drains,
        };
        let _inbox = serve(link, &tools(), &topic(None), prompt("hi"), inbox, &conversation).await;
        drop(conversation);
        server.await.unwrap();
        assert_eq!(
            collect(received).await,
            [AgentEvent::SessionStarted(session_id()), AgentEvent::Failed(QWEN_FAILED.to_owned())]
        );
    }

    #[tokio::test]
    async fn a_drain_mid_turn_hands_over_the_waiting_prompt() {
        let (drain, mut drains) = drain_channel();
        let (mut connection, peer) =
            pair(handler(Arc::new(Scripted::default()), drain), Duration::from_secs(5));
        let server = tokio::spawn(async move {
            let steps = vec![initialized(), ok("session/new", json!({"sessionId": "s-1"}))];
            let (mut peer, mut seen) = script(peer, steps).await;
            let turn = next_request(&mut peer, &mut seen).await;
            peer.write(json!({"jsonrpc": "2.0", "id": 0, "method": "craft/drainMidTurnQueue",
                              "params": {"sessionId": "s-1", "promptId": "p-1"}}))
                .await;
            let drained = peer.read().await.unwrap();
            let id = turn.get("id").cloned().unwrap();
            peer.write(json!({"jsonrpc": "2.0", "id": id, "result": {"stopReason": "end_turn"}}))
                .await;
            drained
        });
        let (conversation, received) = conversation();
        let (inbox_in, inbox) = mpsc::channel(4);
        inbox_in.send(prompt("ещё")).await.unwrap();
        let link = Link {
            client: &connection.client,
            notifications: &mut connection.notifications,
            drains: &mut drains,
        };
        let _inbox = serve(link, &tools(), &topic(None), prompt("hi"), inbox, &conversation).await;
        drop(conversation);
        assert_eq!(
            server.await.unwrap(),
            json!({"jsonrpc": "2.0", "id": 0, "result": {
                "items": [{"content": [{"type": "text", "text": "ещё"}]}],
                "hasQueuedPrompt": false
            }})
        );
        assert_eq!(collect(received).await.len(), 2);
    }

    #[tokio::test]
    async fn stop_while_the_session_opens_ends_without_a_turn() {
        let (drain, mut drains) = drain_channel();
        let (mut connection, peer) =
            pair(handler(Arc::new(Scripted::default()), drain), Duration::from_secs(5));
        let server = tokio::spawn(fake_qwen(peer, vec![initialized()]));
        let (conversation, received) = conversation();
        let (inbox_in, inbox) = mpsc::channel(4);
        inbox_in.send(prompt("потом")).await.unwrap();
        let cancel = conversation.cancel.clone();
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });
        let link = Link {
            client: &connection.client,
            notifications: &mut connection.notifications,
            drains: &mut drains,
        };
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            serve(link, &tools(), &topic(None), prompt("hi"), inbox, &conversation),
        )
        .await;
        stopper.await.unwrap();
        let mut leftover = outcome.expect("serve must return on /stop while session/new waits");
        drop(conversation);
        connection.client.close();
        let seen = server.await.unwrap();
        assert_eq!(methods(&seen), ["initialize", "session/new"]);
        assert!(collect(received).await.is_empty());
        assert_eq!(leftover.recv().await.map(|p| p.text().to_owned()), Some("потом".to_owned()));
    }

    fn settings(qwen: QwenDraft) -> QwenSettings {
        let root = std::env::temp_dir();
        Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            qwen,
            ..Draft::default()
        }
        .parse(&root)
        .unwrap()
        .qwen
    }

    #[rstest]
    #[case::own_setup(QwenDraft::default(), &["--acp", "--approval-mode", "default"], &[])]
    #[case::model_and_mode(
        QwenDraft { model: "qwen3-coder-plus".to_owned(), approval: QwenApproval::Yolo, ..QwenDraft::default() },
        &["--acp", "--approval-mode", "yolo", "-m", "qwen3-coder-plus"],
        &[]
    )]
    #[case::hub_key(
        QwenDraft { model: "m".to_owned(), base_url: "https://x/v1".to_owned(), api_key: "sk-q".to_owned(), ..QwenDraft::default() },
        &["--acp", "--approval-mode", "default", "-m", "m", "--auth-type", "openai"],
        &[("OPENAI_API_KEY", "sk-q"), ("OPENAI_BASE_URL", "https://x/v1"), ("OPENAI_MODEL", "m")]
    )]
    fn process_gets_its_mode_model_and_key(
        #[case] qwen: QwenDraft,
        #[case] args: &[&str],
        #[case] env: &[(&str, &str)],
    ) {
        let settings = settings(qwen);
        assert_eq!((launch_args(&settings), child_env(&settings)), (args.to_vec(), env.to_vec()));
    }

    #[test]
    fn auth_follows_the_endpoint() {
        let own = settings(QwenDraft::default());
        let hub = settings(QwenDraft {
            base_url: "https://x".to_owned(),
            api_key: "k".to_owned(),
            ..QwenDraft::default()
        });
        assert_eq!(
            (QwenAuth::of(&own), QwenAuth::of(&hub)),
            (QwenAuth::OwnSetup, QwenAuth::HubKey)
        );
    }

    #[tokio::test]
    async fn missing_binary_fails_the_turn_and_keeps_the_inbox() {
        let backend = QwenBackend::new(
            PathBuf::from("definitely-not-qwen-binary"),
            settings(QwenDraft::default()),
        );
        let (conversation, mut received) = conversation();
        let (inbox_in, inbox) = mpsc::channel(4);
        inbox_in.send(prompt("потом")).await.unwrap();
        let mut leftover = backend.run(&topic(None), prompt("hi"), inbox, conversation).await;
        assert_eq!(received.recv().await, Some(AgentEvent::Failed(QWEN_FAILED.to_owned())));
        assert!(leftover.try_recv().is_ok());
    }
}
