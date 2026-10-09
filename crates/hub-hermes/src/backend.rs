//! A Hermes ACP child for one hub conversation.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use hub_agent::cli::hide_window;
use hub_agent::conversation::Conversation;
use hub_agent::mcp_http::McpHttpServer;
use hub_agent::rpc::{
    self, Connection, Envelope, Handler, Notification, RpcClient, RpcError, Wire,
};
use hub_agent::tools::TOOL_SUMMARY_LIMIT;
use hub_core::domain::{AgentEvent, Prompt, SessionId, TopicSession};
use hub_core::render::truncate;
use hub_core::settings::HermesSettings;
use serde_json::{Value, json};
use tokio::io::AsyncRead;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, LinesCodec};

use crate::protocol::{self, Capabilities, TurnResult, TurnTracker};
use crate::requests::Gate;

const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
const EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(2);
pub const HERMES_FAILED: &str = "Hermes ACP завершился, подробности в логе";

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "testing.rs"]
mod tests;

pub struct HermesBackend {
    cli: PathBuf,
    settings: HermesSettings,
}

impl HermesBackend {
    #[must_use]
    pub fn new(cli: PathBuf, settings: HermesSettings) -> Self {
        Self { cli, settings }
    }

    /// Runs one conversation, returning queued prompts not yet consumed.
    ///
    /// Mid-turn messages remain in the hub inbox; this backend never sends parallel prompts,
    /// drains Qwen's queue extension, or actively steers a running Hermes turn.
    pub async fn run(
        &self,
        session: &TopicSession,
        prompt: Prompt,
        mut inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation,
    ) -> mpsc::Receiver<Prompt> {
        if conversation.cancel.is_cancelled() {
            return inbox;
        }
        let tools = tokio::select! {
            biased;
            () = conversation.cancel.cancelled() => return inbox,
            tools = McpHttpServer::start_with_questions(Arc::clone(&conversation.channel)) => tools,
        };
        let tools = match tools {
            Ok(tools) => tools,
            Err(error) => {
                tracing::error!(%error, "Hermes hub MCP server did not start");
                emit(&conversation, AgentEvent::Failed(HERMES_FAILED.to_owned())).await;
                return inbox;
            }
        };
        let gate = Gate::new(conversation.cancel.child_token());
        let handler = gate.handler(Arc::clone(&conversation.channel));
        let process =
            match HermesProcess::spawn(&self.cli, &self.settings, session.cwd.as_path(), handler) {
                Ok(process) => process,
                Err(error) => {
                    tracing::error!(%error, "Hermes ACP did not start");
                    emit(&conversation, AgentEvent::Failed(HERMES_FAILED.to_owned())).await;
                    return inbox;
                }
            };
        let mut process = process;
        let control = SessionControl {
            client: &process.client,
            notifications: &mut process.notifications,
            tools: &tools,
            session,
            settings: &self.settings,
            gate: &gate,
        };
        if let Err(error) = serve(control, prompt, &mut inbox, &conversation).await {
            tracing::warn!(error = %failure(&error), "Hermes ACP conversation failed");
            emit(&conversation, AgentEvent::Failed(failure(&error))).await;
        }
        gate.close();
        process.stop().await;
        // The child must finish any in-flight MCP calls before the session server vanishes.
        drop(tools);
        inbox
    }
}

struct Opened {
    id: SessionId,
    capabilities: Capabilities,
}

struct SessionControl<'a> {
    client: &'a RpcClient,
    notifications: &'a mut mpsc::UnboundedReceiver<Notification>,
    tools: &'a McpHttpServer,
    session: &'a TopicSession,
    settings: &'a HermesSettings,
    gate: &'a Gate,
}

async fn open(control: SessionControl<'_>) -> Result<Opened, RpcError> {
    let SessionControl { client, notifications, tools, session, settings, gate: _ } = control;
    let initialized = client.request("initialize", protocol::initialize_params()).await?;
    let capabilities = Capabilities::parse(&initialized)?;
    let mut params = protocol::session_params(session.cwd.as_path(), tools.url(), tools.token());
    let id = match &session.session {
        Some(saved) => {
            if !capabilities.load {
                return Err(RpcError::Protocol(
                    "Hermes не поддерживает продолжение сессии; /reset — начать заново".to_owned(),
                ));
            }
            params
                .as_object_mut()
                .ok_or_else(|| RpcError::Protocol("invalid session parameters".to_owned()))?
                .insert("sessionId".to_owned(), json!(saved.as_str()));
            // session/resume silently creates a missing session in Hermes; load does not.
            let loaded = client.request("session/load", params).await?;
            require_object(&loaded, "session/load")?;
            saved.clone()
        }
        None => protocol::session_id(&client.request("session/new", params).await?)?,
    };
    if let Some(model) = &settings.model {
        let changed = client
            .request("session/set_model", json!({"sessionId": id.as_str(), "modelId": model}))
            .await?;
        require_object(&changed, "session/set_model")?;
    }
    // A loaded session may have persisted accept_edits/dont_ask. Always return to explicit approvals.
    let mode = client
        .request("session/set_mode", json!({"sessionId": id.as_str(), "modeId": "default"}))
        .await?;
    require_object(&mode, "session/set_mode")?;
    // Hermes sends load replay BEFORE the response. Nothing in it is a new hub event.
    while notifications.try_recv().is_ok() {}
    Ok(Opened { id, capabilities })
}

fn require_object(result: &Value, method: &str) -> Result<(), RpcError> {
    if result.is_object() {
        Ok(())
    } else {
        Err(RpcError::Protocol(format!("Hermes {method} returned no session result")))
    }
}

async fn serve(
    control: SessionControl<'_>,
    prompt: Prompt,
    inbox: &mut mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> Result<(), RpcError> {
    let SessionControl { client, notifications, tools, session, settings, gate } = control;
    let control = SessionControl { client, notifications, tools, session, settings, gate };
    let opened = tokio::select! {
        biased;
        () = conversation.cancel.cancelled() => return Ok(()),
        opened = tokio::time::timeout(conversation.limits.initialize, open(control)) => {
            opened.map_err(|_| RpcError::Timeout { method: "Hermes initialization".to_owned() })??
        },
    };
    let Opened { id, capabilities } = opened;
    gate.bind(id.clone());
    emit(conversation, AgentEvent::SessionStarted(id.clone())).await;
    let mut tracker = TurnTracker::new(id.clone());
    let mut next = Some(prompt);
    while let Some(prompt) = next.take() {
        if conversation.cancel.is_cancelled() {
            cancel(client, &id).await;
            return Ok(());
        }
        if !capabilities.images && !prompt.images().is_empty() {
            return Err(RpcError::Protocol("Hermes ACP не поддерживает изображения".to_owned()));
        }
        let result = turn(client, notifications, &id, &prompt, &mut tracker, conversation).await?;
        let Some(result) = result else { return Ok(()) };
        let ended = result.reason == "end_turn";
        for event in tracker.finish(&result) {
            emit(conversation, event).await;
        }
        if conversation.cancel.is_cancelled() || !ended {
            return Ok(());
        }
        // This is the ONLY receive from the inbox. A message racing this check remains in the
        // returned receiver, so the hub can start the next conversation without dropping it.
        next = inbox.try_recv().ok();
    }
    Ok(())
}

async fn turn(
    client: &RpcClient,
    notifications: &mut mpsc::UnboundedReceiver<Notification>,
    id: &SessionId,
    prompt: &Prompt,
    tracker: &mut TurnTracker,
    conversation: &Conversation,
) -> Result<Option<TurnResult>, RpcError> {
    let request = client.request_untimed("session/prompt", protocol::prompt_params(id, prompt));
    tokio::pin!(request);
    let mut updates_open = true;
    loop {
        tokio::select! {
            biased;
            () = conversation.cancel.cancelled() => {
                cancel(client, id).await;
                return Ok(None);
            },
            update = notifications.recv(), if updates_open => match update {
                Some(update) => for event in tracker.translate(&update)? { emit(conversation, event).await; },
                // The request receives Closed from the shared RPC reader on EOF.
                None => updates_open = false,
            },
            result = &mut request => return TurnResult::parse(&result?).map(Some),
        }
    }
}

async fn cancel(client: &RpcClient, id: &SessionId) {
    match tokio::time::timeout(
        CANCEL_TIMEOUT,
        client.notify_with("session/cancel", json!({"sessionId": id.as_str()})),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(error = %failure(&error), "Hermes cancel failed"),
        Err(_) => tracing::warn!("Hermes cancel input stalled"),
    }
}

async fn emit(conversation: &Conversation, event: AgentEvent) {
    tokio::select! {
        biased;
        () = conversation.cancel.cancelled() => {},
        _ = conversation.events.send(event) => {},
    }
}

fn failure(error: &RpcError) -> String {
    match error {
        RpcError::Remote { code: -32000, .. } => {
            "Hermes не авторизован: настройте выбранный профиль через `hermes model`".to_owned()
        }
        RpcError::Remote { message, .. } | RpcError::Protocol(message) => {
            format!("Hermes: {}", truncate(message, TOOL_SUMMARY_LIMIT))
        }
        RpcError::Closed | RpcError::Timeout { .. } => HERMES_FAILED.to_owned(),
    }
}

struct HermesProcess {
    client: RpcClient,
    notifications: mpsc::UnboundedReceiver<Notification>,
    reader: JoinHandle<()>,
    stderr: JoinHandle<()>,
    child: Child,
}

impl HermesProcess {
    fn spawn(
        cli: &Path,
        settings: &HermesSettings,
        cwd: &Path,
        handler: Handler,
    ) -> io::Result<Self> {
        let mut command = Command::new(cli);
        // Profile is a GLOBAL CLI option, not an argument of `acp`. No --yolo or credentials.
        command
            .args(settings.profile.iter().flat_map(|profile| ["--profile", profile.as_str()]))
            .arg("acp")
            .current_dir(cwd)
            .env("TERMINAL_CWD", cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_window(&mut command);
        let mut child = command.spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(io::Error::other("Hermes started without standard streams"));
        };
        let stderr = tokio::spawn(log_stderr(stderr));
        let wire = Wire { peer: "hermes", envelope: Envelope::JsonRpc2, timeout: REQUEST_TIMEOUT };
        let Connection { client, notifications, reader } =
            rpc::connect(stdout, stdin, handler, wire);
        Ok(Self { client, notifications, reader, stderr, child })
    }

    async fn stop(self) {
        let Self { client, notifications: _, reader, stderr, mut child } = self;
        client.close();
        match tokio::time::timeout(EXIT_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => tracing::debug!(%status, "Hermes ACP exited"),
            Ok(Err(error)) => tracing::warn!(%error, "Hermes ACP exit status unavailable"),
            Err(_) => {
                tracing::warn!("Hermes ACP did not exit on EOF; killing child");
                if let Err(error) = child.kill().await {
                    tracing::warn!(%error, "Hermes kill failed");
                }
            }
        }
        reader.abort();
        stderr.abort();
    }
}

async fn log_stderr<R: AsyncRead + Unpin>(stderr: R) {
    let mut lines = FramedRead::new(stderr, LinesCodec::new_with_max_length(64 * 1024));
    while let Some(line) = lines.next().await {
        match line {
            // Child diagnostics may contain secrets. Keep lifecycle evidence, not arbitrary text.
            Ok(line) => tracing::debug!(bytes = line.len(), "Hermes stderr line received"),
            Err(error) => {
                tracing::debug!(%error, "Hermes stderr unreadable");
                break;
            }
        }
    }
}
