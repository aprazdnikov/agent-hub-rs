//! The `codex app-server` child for one conversation: login, thread, turns.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use hub_agent::cli::hide_window;
use hub_agent::conversation::Conversation;
use hub_core::domain::{AgentEvent, Prompt, SessionId, TopicSession};
use hub_core::settings::CodexSettings;
use serde_json::json;
use tokio::io::AsyncRead;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, LinesCodec};

use crate::protocol::{
    Call, CodexAuth, TurnTracker, auth_state, initialize_params, login_params, open_thread,
    parse_thread_id,
};
use crate::requests;
use crate::rpc::{self, Connection, Handler, Notification, RequestError, RpcClient, RpcError};
use crate::session::{RpcThread, converse};

// Handshake and thread/turn control only; a turn itself runs until it ends or /stop.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
// app-server exits on stdin EOF; the grace lets it shut down cleanly before a kill.
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STDERR_LINE: usize = 64 * 1024;
// Commands Codex runs inherit its environment; app-server takes the key via login, not env.
const HIDDEN_FROM_CHILD: &str = "OPENAI_API_KEY";
pub const NOT_LOGGED_IN: &str =
    "Codex не авторизован: выполните `codex login` или задайте API-ключ OpenAI в настройках";
pub const APP_SERVER_FAILED: &str = "Codex app-server завершился, подробности в логе";

pub struct CodexBackend {
    cli: PathBuf,
    settings: CodexSettings,
}

impl CodexBackend {
    #[must_use]
    pub fn new(cli: PathBuf, settings: CodexSettings) -> Self {
        Self { cli, settings }
    }

    /// Runs one conversation and returns the inbox with the prompts it did not take.
    pub async fn run(
        &self,
        session: &TopicSession,
        prompt: Prompt,
        inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation,
    ) -> mpsc::Receiver<Prompt> {
        let channel = Arc::clone(&conversation.channel);
        let handler: Handler = Arc::new(move |method, params| {
            let channel = Arc::clone(&channel);
            Box::pin(async move { requests::answer(channel.as_ref(), &method, params).await })
        });
        let mut server = match AppServer::spawn(&self.cli, handler) {
            Ok(server) => server,
            Err(error) => {
                fail(&conversation, format!("Не удалось запустить Codex: {error}")).await;
                return inbox;
            }
        };
        let link = Link { client: &server.client, notifications: &mut server.notifications };
        let inbox = serve(link, &self.settings, session, prompt, inbox, &conversation).await;
        server.stop().await;
        inbox
    }
}

pub struct Link<'a> {
    pub client: &'a RpcClient,
    pub notifications: &'a mut mpsc::UnboundedReceiver<Notification>,
}

/// One conversation over an established connection: login, thread, turns.
pub async fn serve(
    link: Link<'_>,
    settings: &CodexSettings,
    session: &TopicSession,
    prompt: Prompt,
    inbox: mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> mpsc::Receiver<Prompt> {
    let Link { client, notifications } = link;
    let opened = tokio::select! {
        biased;
        () = conversation.cancel.cancelled() => return inbox,
        opened = open(client, settings, session) => opened,
    };
    let thread = match opened {
        Ok(thread) => thread,
        Err(error) => {
            tracing::warn!(%error, "codex session did not open");
            fail(conversation, error.to_string()).await;
            return inbox;
        }
    };
    emit(conversation, AgentEvent::SessionStarted(thread.clone())).await;
    let tracker = TurnTracker::new(thread.clone());
    let rpc_thread = RpcThread::new(client.clone(), thread);
    let (inbox, outcome) =
        converse(&rpc_thread, notifications, tracker, prompt, inbox, conversation).await;
    if let Err(error) = outcome {
        tracing::warn!(%error, "codex session broke");
        fail(conversation, session_failure(&error)).await;
    }
    inbox
}

#[derive(Debug)]
enum OpenError {
    NotLoggedIn,
    NotStarted(String),
    NotResumed { session: SessionId, message: String },
    Rpc(RpcError),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLoggedIn => f.write_str(NOT_LOGGED_IN),
            Self::NotStarted(message) => write!(f, "Codex не начал сессию: {message}"),
            Self::NotResumed { session, message } => write!(
                f,
                "Codex не смог продолжить сессию {}: {message}. /reset — начать заново",
                session.as_str()
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
        RpcError::Remote { message, .. } => format!("Codex: {message}"),
        RpcError::Closed | RpcError::Protocol(_) | RpcError::Timeout { .. } => {
            APP_SERVER_FAILED.to_owned()
        }
    }
}

async fn open(
    client: &RpcClient,
    settings: &CodexSettings,
    session: &TopicSession,
) -> Result<SessionId, OpenError> {
    let auth = probe(client, settings).await?;
    if auth == CodexAuth::Missing {
        return Err(OpenError::NotLoggedIn);
    }
    let (call, params) = open_thread(session, settings);
    match client.request(call.method(), params).await {
        Ok(result) => Ok(parse_thread_id(&result)?),
        Err(RpcError::Remote { message, .. }) => Err(match &session.session {
            None => OpenError::NotStarted(message),
            Some(saved) => OpenError::NotResumed { session: saved.clone(), message },
        }),
        Err(other @ (RpcError::Closed | RpcError::Protocol(_) | RpcError::Timeout { .. })) => {
            Err(other.into())
        }
    }
}

/// The handshake, then how Codex is logged in.
async fn handshake(client: &RpcClient) -> Result<CodexAuth, RpcError> {
    client.request(Call::Initialize.method(), initialize_params()).await?;
    client.notify(Call::Initialized.method()).await?;
    Ok(auth_state(&client.request(Call::AccountRead.method(), json!({})).await?))
}

/// The key replaces only a missing or API-key login, so a changed key applies from the next
/// session: logging in rewrites `auth.json`, and a `ChatGPT` login must survive a key left in
/// the settings.
async fn login(
    client: &RpcClient,
    settings: &CodexSettings,
    auth: CodexAuth,
) -> Result<CodexAuth, RpcError> {
    match (&settings.api_key, auth) {
        (Some(key), CodexAuth::Missing | CodexAuth::ApiKey) => {
            client.request(Call::Login.method(), login_params(key)).await?;
            Ok(auth_state(&client.request(Call::AccountRead.method(), json!({})).await?))
        }
        (
            None,
            CodexAuth::Missing
            | CodexAuth::ApiKey
            | CodexAuth::ChatGpt
            | CodexAuth::Other
            | CodexAuth::NotRequired,
        )
        | (Some(_), CodexAuth::ChatGpt | CodexAuth::Other | CodexAuth::NotRequired) => Ok(auth),
    }
}

/// How Codex is logged in, logging in with the configured key if it may.
pub async fn probe(client: &RpcClient, settings: &CodexSettings) -> Result<CodexAuth, RpcError> {
    let auth = handshake(client).await?;
    login(client, settings, auth).await
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("не удалось запустить Codex app-server: {0}")]
    Spawn(io::Error),
    #[error("Codex app-server: {0}")]
    Rpc(#[from] RpcError),
}

pub async fn probe_auth(cli: &Path, settings: &CodexSettings) -> Result<CodexAuth, ProbeError> {
    let refuse: Handler =
        Arc::new(|method, _params| Box::pin(async move { Err(RequestError::Unsupported(method)) }));
    let server = AppServer::spawn(cli, refuse).map_err(ProbeError::Spawn)?;
    let auth = probe(&server.client, settings).await;
    server.stop().await;
    Ok(auth?)
}

struct AppServer {
    client: RpcClient,
    notifications: mpsc::UnboundedReceiver<Notification>,
    reader: JoinHandle<()>,
    child: Child,
}

impl AppServer {
    fn spawn(cli: &Path, handler: Handler) -> io::Result<Self> {
        let mut command = Command::new(cli);
        command
            .arg("app-server")
            .env_remove(HIDDEN_FROM_CHILD)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_window(&mut command);
        let mut child = command.spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(io::Error::other("app-server started without standard streams"));
        };
        tokio::spawn(log_stderr(stderr));
        let Connection { client, notifications, reader } =
            rpc::connect(stdout, stdin, handler, REQUEST_TIMEOUT);
        Ok(Self { client, notifications, reader, child })
    }

    async fn stop(mut self) {
        self.client.close();
        if tokio::time::timeout(EXIT_TIMEOUT, self.child.wait()).await.is_err() {
            tracing::warn!("codex app-server did not exit on stdin close, killing it");
            if let Err(error) = self.child.kill().await {
                tracing::warn!(%error, "codex app-server could not be killed");
            }
        }
        // Answers still in flight have nobody left to reach.
        self.reader.abort();
    }
}

async fn log_stderr<R: AsyncRead + Unpin>(stderr: R) {
    let mut lines = FramedRead::new(stderr, LinesCodec::new_with_max_length(MAX_STDERR_LINE));
    while let Some(line) = lines.next().await {
        match line {
            Ok(line) => tracing::info!(%line, "codex stderr"),
            Err(error) => {
                tracing::debug!(%error, "codex stderr unreadable");
                return;
            }
        }
    }
}

async fn emit(conversation: &Conversation, event: AgentEvent) {
    // Nobody listening means the topic is gone; there is no one left to tell.
    let _ = conversation.events.send(event).await;
}

async fn fail(conversation: &Conversation, reason: String) {
    emit(conversation, AgentEvent::Failed(reason)).await;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use hub_agent::conversation::Limits;
    use hub_core::domain::{AbsolutePath, BackendKind, Finished, Usage};
    use hub_core::settings::{ApiKey, Approval, Sandbox};
    use rstest::rstest;
    use tokio_util::sync::CancellationToken;

    use serde_json::Value;

    use super::*;
    use crate::testing::{Peer, Scripted, pair, refusing};

    enum Reply {
        Result(Value),
        Error(&'static str),
    }

    struct Step {
        method: &'static str,
        reply: Reply,
        then: Vec<Value>,
    }

    fn ok(method: &'static str, result: Value) -> Step {
        Step { method, reply: Reply::Result(result), then: Vec::new() }
    }

    fn err(method: &'static str, message: &'static str) -> Step {
        Step { method, reply: Reply::Error(message), then: Vec::new() }
    }

    impl Step {
        fn then(self, notifications: Vec<Value>) -> Self {
            Self { then: notifications, ..self }
        }
    }

    /// Answers the client's requests in order and records every message it got.
    async fn fake_server(mut peer: Peer, steps: Vec<Step>) -> (Peer, Vec<Value>) {
        let mut seen = Vec::new();
        for step in steps {
            let request = loop {
                let message = peer.read().await.unwrap();
                seen.push(message.clone());
                if message.get("id").is_some() {
                    break message;
                }
            };
            assert_eq!(request.get("method").and_then(Value::as_str), Some(step.method));
            let id = request.get("id").cloned().unwrap();
            let reply = match step.reply {
                Reply::Result(result) => json!({"id": id, "result": result}),
                Reply::Error(message) => {
                    json!({"id": id, "error": {"code": -32000, "message": message}})
                }
            };
            peer.write(reply).await;
            for notification in step.then {
                peer.write(notification).await;
            }
        }
        (peer, seen)
    }

    fn settings(api_key: Option<&str>) -> CodexSettings {
        CodexSettings {
            cli: None,
            model: None,
            sandbox: Sandbox::WorkspaceWrite,
            approval: Approval::OnRequest,
            api_key: api_key.and_then(ApiKey::parse),
        }
    }

    fn session(saved: Option<&str>) -> TopicSession {
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        TopicSession::fresh(BackendKind::Codex, cwd).with_session(saved.and_then(SessionId::parse))
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

    fn chatgpt() -> Value {
        json!({"account": {"type": "chatgpt"}, "requiresOpenaiAuth": true})
    }

    fn logged_out() -> Value {
        json!({"account": null, "requiresOpenaiAuth": true})
    }

    fn api_key_login() -> Value {
        json!({"account": {"type": "apiKey"}})
    }

    fn turn_done(turn: &str) -> Value {
        json!({"method": "turn/completed", "params": {"turn": {"id": turn, "status": "completed"}}})
    }

    fn thread() -> SessionId {
        SessionId::parse("t-1").unwrap()
    }

    fn methods(seen: &[Value]) -> Vec<String> {
        seen.iter()
            .filter_map(|message| message.get("method").and_then(Value::as_str))
            .map(str::to_owned)
            .collect()
    }

    async fn collect(mut received: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Some(event) = received.recv().await {
            events.push(event);
        }
        events
    }

    /// Runs `serve` against a fake app-server; returns the events and what the server saw.
    async fn run(
        settings: CodexSettings,
        topic: TopicSession,
        steps: Vec<Step>,
    ) -> (Vec<AgentEvent>, Vec<Value>) {
        let (mut connection, peer) = pair(refusing(), Duration::from_secs(5));
        let server = tokio::spawn(fake_server(peer, steps));
        let (conversation, received) = conversation();
        let (_inbox_in, inbox) = mpsc::channel(4);
        let link =
            Link { client: &connection.client, notifications: &mut connection.notifications };
        let _inbox = serve(link, &settings, &topic, prompt("hi"), inbox, &conversation).await;
        drop(conversation);
        let (_peer, seen) = server.await.unwrap();
        (collect(received).await, seen)
    }

    fn opened(open: Step) -> Vec<Step> {
        vec![
            ok("initialize", json!({})),
            ok("account/read", chatgpt()),
            open,
            ok("turn/start", json!({"turn": {"id": "u-1"}})).then(vec![turn_done("u-1")]),
        ]
    }

    #[tokio::test]
    async fn new_session_starts_a_thread_and_runs_a_turn() {
        let (events, seen) = run(
            settings(None),
            session(None),
            vec![
                ok("initialize", json!({})),
                ok("account/read", chatgpt()),
                ok("thread/start", json!({"thread": {"id": "t-1"}})),
                ok("turn/start", json!({"turn": {"id": "u-1"}})).then(vec![
                    json!({"method": "item/completed", "params": {"item": {"type": "agentMessage", "text": "Привет"}}}),
                    json!({"method": "thread/tokenUsage/updated", "params": {"tokenUsage": {"total": {"totalTokens": 42}}}}),
                    turn_done("u-1"),
                ]),
            ],
        )
        .await;
        assert_eq!(
            events,
            [
                AgentEvent::SessionStarted(thread()),
                AgentEvent::AssistantText("Привет".to_owned()),
                AgentEvent::Finished(Finished {
                    session: thread(),
                    usage: Usage::Codex { tokens: Some(42) },
                    background: 0,
                }),
            ]
        );
        assert_eq!(
            methods(&seen),
            ["initialize", "initialized", "account/read", "thread/start", "turn/start"]
        );
    }

    #[tokio::test]
    async fn saved_session_resumes_its_thread() {
        let resume = ok("thread/resume", json!({"thread": {"id": "t-1"}}));
        let (events, seen) = run(settings(None), session(Some("t-1")), opened(resume)).await;
        assert_eq!(events.first(), Some(&AgentEvent::SessionStarted(thread())));
        assert!(methods(&seen).contains(&"thread/resume".to_owned()));
    }

    #[tokio::test]
    async fn missing_login_without_a_key_fails_before_opening_a_thread() {
        let (events, seen) = run(
            settings(None),
            session(None),
            vec![ok("initialize", json!({})), ok("account/read", logged_out())],
        )
        .await;
        assert_eq!(events, [AgentEvent::Failed(NOT_LOGGED_IN.to_owned())]);
        assert!(!methods(&seen).contains(&"thread/start".to_owned()));
    }

    #[tokio::test]
    async fn missing_login_with_a_key_logs_in_and_starts() {
        let (events, seen) = run(
            settings(Some("sk-test")),
            session(None),
            vec![
                ok("initialize", json!({})),
                ok("account/read", logged_out()),
                ok("account/login/start", json!({})),
                ok("account/read", api_key_login()),
                ok("thread/start", json!({"thread": {"id": "t-1"}})),
                ok("turn/start", json!({"turn": {"id": "u-1"}})).then(vec![turn_done("u-1")]),
            ],
        )
        .await;
        assert_eq!(events.first(), Some(&AgentEvent::SessionStarted(thread())));
        let login = seen
            .iter()
            .find(|message| message.get("method") == Some(&json!("account/login/start")))
            .and_then(|message| message.get("params"));
        assert_eq!(login, Some(&json!({"type": "apiKey", "apiKey": "sk-test"})));
    }

    #[tokio::test]
    async fn a_changed_key_replaces_a_key_login_in_a_session() {
        let (events, seen) = run(
            settings(Some("sk-new")),
            session(None),
            vec![
                ok("initialize", json!({})),
                ok("account/read", api_key_login()),
                ok("account/login/start", json!({})),
                ok("account/read", api_key_login()),
                ok("thread/start", json!({"thread": {"id": "t-1"}})),
                ok("turn/start", json!({"turn": {"id": "u-1"}})).then(vec![turn_done("u-1")]),
            ],
        )
        .await;
        assert_eq!(events.first(), Some(&AgentEvent::SessionStarted(thread())));
        let login = seen
            .iter()
            .find(|message| message.get("method") == Some(&json!("account/login/start")))
            .and_then(|message| message.get("params"));
        assert_eq!(login, Some(&json!({"type": "apiKey", "apiKey": "sk-new"})));
    }

    #[tokio::test]
    async fn stop_while_the_thread_opens_ends_without_a_turn() {
        let (mut connection, peer) = pair(refusing(), Duration::from_secs(5));
        let server = tokio::spawn(fake_server(
            peer,
            vec![ok("initialize", json!({})), ok("account/read", chatgpt())],
        ));
        let (conversation, received) = conversation();
        let (inbox_in, inbox) = mpsc::channel(4);
        inbox_in.send(prompt("потом")).await.unwrap();
        let cancel = conversation.cancel.clone();
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });
        let link =
            Link { client: &connection.client, notifications: &mut connection.notifications };
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            serve(link, &settings(None), &session(None), prompt("hi"), inbox, &conversation),
        )
        .await;
        stopper.await.unwrap();
        let mut leftover = outcome.expect("serve must return on /stop while thread/start waits");
        drop(conversation);
        let (_peer, seen) = server.await.unwrap();
        assert!(!methods(&seen).contains(&"turn/start".to_owned()));
        assert!(collect(received).await.is_empty());
        assert_eq!(leftover.recv().await.map(|p| p.text().to_owned()), Some("потом".to_owned()));
    }

    #[rstest]
    #[case(
        Some("t-1"),
        err("thread/resume", "no rollout found"),
        "Codex не смог продолжить сессию t-1: no rollout found. /reset — начать заново"
    )]
    #[case(None, err("thread/start", "bad model"), "Codex не начал сессию: bad model")]
    #[tokio::test]
    async fn a_thread_that_does_not_open_fails_with_a_hint(
        #[case] saved: Option<&str>,
        #[case] open: Step,
        #[case] reason: &str,
    ) {
        let steps = vec![ok("initialize", json!({})), ok("account/read", chatgpt()), open];
        let (events, _seen) = run(settings(None), session(saved), steps).await;
        assert_eq!(events, [AgentEvent::Failed(reason.to_owned())]);
    }

    #[tokio::test]
    async fn rpc_error_mid_session_fails_with_its_message() {
        let (events, _seen) = run(
            settings(None),
            session(None),
            vec![
                ok("initialize", json!({})),
                ok("account/read", chatgpt()),
                ok("thread/start", json!({"thread": {"id": "t-1"}})),
                err("turn/start", "quota exceeded"),
            ],
        )
        .await;
        assert_eq!(
            events,
            [
                AgentEvent::SessionStarted(thread()),
                AgentEvent::Failed("Codex: quota exceeded".to_owned())
            ]
        );
    }

    #[tokio::test]
    async fn app_server_exiting_mid_turn_fails_the_turn() {
        let (mut connection, peer) = pair(refusing(), Duration::from_secs(5));
        let server = tokio::spawn(async move {
            let steps = vec![
                ok("initialize", json!({})),
                ok("account/read", chatgpt()),
                ok("thread/start", json!({"thread": {"id": "t-1"}})),
                ok("turn/start", json!({"turn": {"id": "u-1"}})),
            ];
            let (peer, _seen) = fake_server(peer, steps).await;
            drop(peer);
        });
        let (conversation, received) = conversation();
        let (_inbox_in, inbox) = mpsc::channel(4);
        let link =
            Link { client: &connection.client, notifications: &mut connection.notifications };
        let _inbox =
            serve(link, &settings(None), &session(None), prompt("hi"), inbox, &conversation).await;
        drop(conversation);
        server.await.unwrap();
        assert_eq!(
            collect(received).await,
            [
                AgentEvent::SessionStarted(thread()),
                AgentEvent::Failed(APP_SERVER_FAILED.to_owned())
            ]
        );
    }

    #[rstest]
    #[case(logged_out(), true, CodexAuth::ApiKey)]
    #[case(api_key_login(), true, CodexAuth::ApiKey)]
    #[case(chatgpt(), false, CodexAuth::ChatGpt)]
    #[tokio::test]
    async fn probe_logs_in_with_the_key_unless_chatgpt(
        #[case] account: Value,
        #[case] logs_in: bool,
        #[case] expected: CodexAuth,
    ) {
        let (connection, peer) = pair(refusing(), Duration::from_secs(5));
        let login = [ok("account/login/start", json!({})), ok("account/read", api_key_login())];
        let steps = [ok("initialize", json!({})), ok("account/read", account)]
            .into_iter()
            .chain(login.into_iter().filter(|_| logs_in))
            .collect();
        let server = tokio::spawn(fake_server(peer, steps));
        assert_eq!(probe(&connection.client, &settings(Some("sk-new"))).await, Ok(expected));
        let (_peer, seen) = server.await.unwrap();
        assert_eq!(methods(&seen).contains(&"account/login/start".to_owned()), logs_in);
    }

    #[tokio::test]
    async fn missing_binary_fails_the_turn_and_keeps_the_inbox() {
        let backend =
            CodexBackend::new(PathBuf::from("definitely-not-codex-binary"), settings(None));
        let (conversation, mut received) = conversation();
        let (inbox_in, inbox) = mpsc::channel(4);
        inbox_in.send(prompt("потом")).await.unwrap();
        let mut leftover = backend.run(&session(None), prompt("hi"), inbox, conversation).await;
        assert!(matches!(
            received.recv().await,
            Some(AgentEvent::Failed(reason)) if reason.starts_with("Не удалось запустить Codex")
        ));
        assert!(leftover.try_recv().is_ok());
    }

    #[tokio::test]
    async fn probe_of_a_missing_binary_is_a_spawn_error() {
        let outcome = probe_auth(Path::new("definitely-not-codex-binary"), &settings(None)).await;
        assert!(matches!(outcome, Err(ProbeError::Spawn(_))));
    }
}
