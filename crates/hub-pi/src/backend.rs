//! An `Pi` RPC child for one hub conversation.
//!
//! One process per conversation: the hub starts `pi --mode rpc`, opens or resumes the topic's
//! session, then feeds prompts to it until the user stops it or the child dies. A message that
//! arrives while the agent works is steered into the running turn, because `Pi` accepts that
//! natively and the user expects a mid-turn message to change what is happening now.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use hub_agent::cli::hide_window;
use hub_agent::conversation::Conversation;
use hub_agent::mcp_http::McpHttpServer;
use hub_agent::memory::MemoryMcp;
use hub_core::domain::{AgentEvent, Prompt, SessionId, TopicSession};
use hub_core::render::truncate;
use hub_core::settings::PiSettings;
use serde_json::Value;
use tokio::io::AsyncRead;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, LinesCodec};

use crate::link::{Link, LinkError};
use crate::protocol::{self, Wire};
use crate::requests::{self, BRIDGE, Gate};

pub const PI_FAILED: &str = "Pi RPC завершился, подробности в логе";
/// A control command is answered from memory; only a wedged child takes longer than this.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const STDERR_LINE: usize = 64 * 1024;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "testing.rs"]
mod tests;

pub struct PiBackend {
    cli: PathBuf,
    settings: PiSettings,
    memory: Option<MemoryMcp>,
}

impl PiBackend {
    #[must_use]
    pub fn new(cli: PathBuf, settings: PiSettings) -> Self {
        Self { cli, settings, memory: None }
    }

    #[must_use]
    pub fn with_memory(mut self, memory: Option<MemoryMcp>) -> Self {
        self.memory = memory;
        self
    }

    /// Runs one conversation, returning queued prompts not yet consumed.
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
                tracing::error!(%error, "Pi hub MCP server did not start");
                emit(&conversation, AgentEvent::Failed(PI_FAILED.to_owned())).await;
                return inbox;
            }
        };
        let bridge = match Bridge::new(tools.url(), tools.token(), self.memory.as_ref()) {
            Ok(bridge) => bridge,
            Err(error) => {
                tracing::error!(%error, "Pi bridge could not be prepared");
                emit(&conversation, AgentEvent::Failed(failure(&error))).await;
                return inbox;
            }
        };
        let gate = Gate::new(Arc::clone(&conversation.channel), conversation.cancel.child_token());
        let mut process = match PiProcess::spawn(&self.cli, bridge, &self.settings, session, &gate)
        {
            Ok(process) => process,
            Err(error) => {
                tracing::error!(%error, "Pi RPC did not start");
                emit(&conversation, AgentEvent::Failed(failure(&error))).await;
                return inbox;
            }
        };
        let served = tokio::select! {
            biased;
            () = conversation.cancel.cancelled() => Ok(()),
            served = serve(&mut process, &self.settings, session, prompt, &mut inbox, &conversation) => served,
        };
        if let Err(error) = served {
            tracing::warn!(error = %failure(&error), "Pi RPC conversation failed");
            emit(&conversation, AgentEvent::Failed(failure(&error))).await;
        }
        process.stop().await;
        // The child must finish any in-flight MCP calls before the session server vanishes.
        drop(tools);
        inbox
    }
}

/// The child, its RPC link, and the private directory holding the bridge extension.
struct PiProcess {
    link: Link,
    child: Child,
    stderr: JoinHandle<()>,
    bridge: Bridge,
}

impl PiProcess {
    fn spawn(
        cli: &Path,
        bridge: Bridge,
        settings: &PiSettings,
        session: &TopicSession,
        gate: &Gate,
    ) -> Result<Self, LinkError> {
        let resume = session.session.as_ref().map(SessionId::as_str);
        let mut command = Command::new(cli);
        command
            .args(protocol::session_args(resume, settings.model.as_deref()))
            .args(["--extension", bridge.path.as_str()])
            .current_dir(session.cwd.as_path())
            .env(protocol::ENV_GATE, requests::gate_tools(settings.approval))
            .env(protocol::ENV_MCP_URL, bridge.url.as_str())
            .env(protocol::ENV_MCP_TOKEN, bridge.token.as_str())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(memory) = &bridge.memory {
            command.env(protocol::ENV_MEMORY, memory);
        }
        hide_window(&mut command);
        let mut child = command.spawn().map_err(|source| {
            LinkError::Refused(format!("не удалось запустить {}: {source}", cli.display()))
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(LinkError::Refused("Pi стартовал без стандартных потоков".to_owned()));
        };
        let stderr = tokio::spawn(log_stderr(stderr));
        Ok(Self { link: Link::spawn(stdout, stdin, gate), child, stderr, bridge })
    }

    async fn stop(&mut self) {
        let Self { link, child, stderr, bridge } = self;
        link.close();
        match tokio::time::timeout(EXIT_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => tracing::debug!(%status, "Pi RPC exited"),
            Ok(Err(error)) => tracing::warn!(%error, "Pi RPC exit status unavailable"),
            Err(_) => {
                tracing::warn!("Pi RPC did not exit on EOF; killing child");
                if let Err(error) = child.kill().await {
                    tracing::warn!(%error, "Pi kill failed");
                }
            }
        }
        stderr.abort();
        link.finish();
        bridge.release();
    }
}

/// A bridge that cannot be built stops the start, with the reason as text.
fn refused(what: &str, error: impl std::fmt::Display) -> LinkError {
    LinkError::Refused(format!("{what}: {error}"))
}

/// What the extension needs, kept out of argv: the URL, the token and the memory server.
struct Bridge {
    path: String,
    url: String,
    token: String,
    memory: Option<String>,
    work: Option<tempfile::TempDir>,
}

impl Bridge {
    fn new(url: &str, token: &str, memory: Option<&MemoryMcp>) -> Result<Self, LinkError> {
        let work = tempfile::TempDir::new()
            .map_err(|error| refused("нет каталога для моста Pi", error))?;
        let path = work.path().join(protocol::BRIDGE_FILE);
        std::fs::write(&path, BRIDGE).map_err(|error| refused("мост Pi не записан", error))?;
        Ok(Self {
            path: path.display().to_string(),
            url: url.to_owned(),
            token: token.to_owned(),
            memory: memory.map(|memory| memory.pi_server().to_string()),
            work: Some(work),
        })
    }

    /// The directory holds a bearer token, so it goes as soon as the child is done with it.
    /// Dropping a `TempDir` reports a failed removal by panicking, which a hub must not do.
    fn release(&mut self) {
        let Some(work) = self.work.take() else { return };
        if let Err(error) = work.close() {
            tracing::warn!(%error, "Pi bridge directory was left behind");
        }
    }
}

/// Opens the session, then runs prompts until the child or the user ends the conversation.
async fn serve(
    process: &mut PiProcess,
    settings: &PiSettings,
    session: &TopicSession,
    mut prompt: Prompt,
    inbox: &mut mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> Result<(), LinkError> {
    let id = open(&mut process.link, settings, session).await?;
    emit(conversation, AgentEvent::SessionStarted(id.clone())).await;
    loop {
        if conversation.cancel.is_cancelled() {
            return Ok(());
        }
        turn(&mut process.link, &id, &prompt, inbox, conversation).await?;
        // This is the ONLY receive between turns. A message that raced it stays in the inbox, so
        // the hub can start the next conversation without dropping it.
        let Some(next) = inbox.try_recv().ok() else { return Ok(()) };
        prompt = next;
    }
}

/// Learns the session id and puts the child in the mode the topic chose.
///
/// # Errors
///
/// Opens the session the hub keeps, then puts the child in the model and mode the settings ask
/// for. A mode this model does not advertise is refused, never guessed at.
async fn open(
    link: &mut Link,
    settings: &PiSettings,
    session: &TopicSession,
) -> Result<SessionId, LinkError> {
    let mut state = state_of(link).await?;
    let id = protocol::session_id(&state).map_err(LinkError::Refused)?;
    if session.session.as_ref().is_some_and(|saved| *saved != id) {
        // `Pi` opens a fresh session rather than failing on an id it cannot find, which would
        // silently lose the topic's history. The hub reports the id it actually holds.
        tracing::warn!(opened = id.as_str(), "Pi did not reopen the stored session");
    }
    // A model switch resets the thinking level to that model's own default.
    if switched_model(link, settings, &state).await? {
        state = state_of(link).await?;
    }
    apply_mode(link, settings, &state).await?;
    Ok(id)
}

/// What the child reports about the session it has open.
async fn state_of(link: &mut Link) -> Result<Value, LinkError> {
    let state = link.call("get_state", protocol::get_state(), CONTROL_TIMEOUT).await?;
    state.ok_or_else(|| LinkError::Refused("Pi не вернул состояние сессии".to_owned()))
}

/// Whether the running child had to change the model under the conversation. A model the agent
/// refuses is an error the user must see: settings can name a model a subscription no longer
/// serves, and the process of the next turn is not the only way to reach one.
async fn switched_model(
    link: &mut Link,
    settings: &PiSettings,
    state: &Value,
) -> Result<bool, LinkError> {
    let Some(spec) = settings.model.as_deref() else { return Ok(false) };
    // A name without a provider belongs to `--model` at spawn; `set_model` cannot address it.
    let Some((provider, model)) = spec.split_once('/') else { return Ok(false) };
    if protocol::current_model(state).as_deref() == Some(spec) {
        return Ok(false);
    }
    let command = protocol::set_model(provider, model);
    link.call("set_model", command, CONTROL_TIMEOUT).await?;
    Ok(true)
}

/// Sets the thinking level the conversation is configured for.
async fn apply_mode(
    link: &mut Link,
    settings: &PiSettings,
    state: &Value,
) -> Result<(), LinkError> {
    let wanted = settings.thinking;
    // A build that advertises nothing is not refused: `unsupported_mode` allows it, and the hub
    // still asks for the level it was configured with.
    let levels = link
        .call(
            "get_available_thinking_levels",
            protocol::get_available_thinking_levels(),
            CONTROL_TIMEOUT,
        )
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    if let Some(reason) = protocol::unsupported_mode(&protocol::advertised_levels(&levels), wanted)
    {
        return Err(LinkError::Refused(reason));
    }
    if state.get("thinkingLevel").and_then(Value::as_str) == Some(wanted.wire()) {
        return Ok(());
    }
    link.call("set_thinking_level", protocol::set_thinking_level(wanted), CONTROL_TIMEOUT).await?;
    Ok(())
}

/// One prompt: sent, streamed, and closed by the agent settling.
async fn turn(
    link: &mut Link,
    id: &SessionId,
    prompt: &Prompt,
    inbox: &mut mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> Result<(), LinkError> {
    emit(conversation, AgentEvent::TurnStarted).await;
    if link.call("prompt", protocol::prompt(prompt), CONTROL_TIMEOUT).await?.is_none() {
        return Err(LinkError::Refused("Pi не принял сообщение".to_owned()));
    }
    loop {
        tokio::select! {
            biased;
            () = conversation.cancel.cancelled() => {
                link.abort().await;
                return Ok(());
            }
            queued = inbox.recv() => {
                let Some(queued) = queued else { return Ok(()); };
                link.steer(&queued).await?;
            }
            event = link.event() => match event {
                None => return Err(LinkError::Closed),
                Some(Wire::Settled { aborted }) => {
                    return if aborted { Ok(()) } else { settle(link, id, conversation).await };
                }
                Some(Wire::Text(text)) => emit(conversation, AgentEvent::AssistantText(text)).await,
                Some(Wire::Tool(tool)) => emit(conversation, AgentEvent::ToolCall(tool)).await,
                // A turn the model could not finish: the process stays alive, so the only way the
                // user learns of it is the hub saying so.
                Some(Wire::Error { reason }) => {
                    // Текст принадлежит провайдеру: префикс бэкенда добавит `failure`.
                    return Err(LinkError::Refused(format!("ход не выполнен: {reason}")));
                }
                // Retries, answers and dialogs are all handled inside the link: the turn only
                // ends on a settled event.
                Some(
                    Wire::Retrying | Wire::Reply { .. } | Wire::Approval { .. }
                    | Wire::Ignored | Wire::Malformed,
                ) => {}
            },
        }
    }
}

/// The usage line of a finished turn. `Pi` reports tokens for the whole session, so a missing or
/// late answer leaves the count unknown rather than ending the conversation.
async fn settle(
    link: &mut Link,
    id: &SessionId,
    conversation: &Conversation,
) -> Result<(), LinkError> {
    let stats =
        link.call("get_session_stats", protocol::get_session_stats(), CONTROL_TIMEOUT).await;
    let stats = match stats {
        Ok(stats) => stats,
        Err(error) => {
            tracing::debug!(error = %failure(&error), "Pi session stats unavailable");
            None
        }
    };
    emit(conversation, AgentEvent::Finished(protocol::finished(id.clone(), stats.as_ref()))).await;
    Ok(())
}

async fn emit(conversation: &Conversation, event: AgentEvent) {
    tokio::select! {
        biased;
        () = conversation.cancel.cancelled() => {},
        _ = conversation.events.send(event) => {},
    }
}

fn failure(error: &LinkError) -> String {
    match error {
        // Отказ несёт причину от провайдера: в строку чата она помещается целиком.
        LinkError::Refused(message) => {
            format!("Pi: {}", truncate(message, protocol::ERROR_LIMIT))
        }
        LinkError::Closed | LinkError::Timeout { .. } => PI_FAILED.to_owned(),
    }
}

async fn log_stderr<R: AsyncRead + Unpin>(stderr: R) {
    let mut lines = FramedRead::new(stderr, LinesCodec::new_with_max_length(STDERR_LINE));
    while let Some(line) = lines.next().await {
        match line {
            // Child diagnostics may contain secrets. Keep lifecycle evidence, not arbitrary text.
            Ok(line) => tracing::debug!(bytes = line.len(), "Pi stderr line received"),
            Err(error) => {
                tracing::debug!(%error, "Pi stderr unreadable");
                break;
            }
        }
    }
}
