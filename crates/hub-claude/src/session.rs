//! One CLI conversation: relays messages and further prompts until nothing is left to do.
//!
//! Closing the CLI kills its background tasks, so the conversation stays open while they run;
//! their results arrive as turns the CLI starts on its own.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures::{Stream, StreamExt};
use hub_core::domain::{AgentEvent, Prompt};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinError, JoinSet};
use tokio::time::{Instant, sleep_until, timeout};
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::activity::{Phase, SessionActivity};
use crate::channel::UserChannel;
use crate::mcp::{self, McpStep};
use crate::outgoing;
use crate::permissions::{self, Route};
use crate::tracker::SessionTracker;
use crate::wire::{self, ControlReply, ControlRequest, Incoming};

// One stream-json message can carry a large tool result.
const MAX_LINE: usize = 16 * 1024 * 1024;
const INITIALIZE: &str = "hub-initialize";
const INTERRUPT: &str = "hub-interrupt";
const OUTBOX: usize = 64;
const FLUSH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// How long background tasks may run after the agent's answer.
    pub background: Duration,
    /// After a task reports, the CLI starts a turn of its own within this window.
    pub settle: Duration,
    pub initialize: Duration,
}

impl Limits {
    #[must_use]
    pub const fn new(background: Duration) -> Self {
        Self { background, settle: Duration::from_secs(30), initialize: Duration::from_mins(1) }
    }
}

pub struct Conversation {
    pub channel: Arc<dyn UserChannel>,
    pub events: mpsc::Sender<AgentEvent>,
    pub cancel: CancellationToken,
    pub limits: Limits,
}

enum Flow {
    Continue,
    Stop,
}

enum Step {
    Cancelled,
    Line(Option<Result<String, LinesCodecError>>),
    Prompt(Option<Prompt>),
    Deadline(Phase),
    InitializeTimeout,
    Handled(Result<String, JoinError>),
}

/// Returns the inbox, so prompts that arrive while the conversation closes are not lost.
pub async fn converse<R, W>(
    reader: R,
    writer: W,
    prompt: Prompt,
    inbox: mpsc::Receiver<Prompt>,
    conversation: Conversation,
) -> mpsc::Receiver<Prompt>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (outbox, lines) = mpsc::channel(OUTBOX);
    let writer = tokio::spawn(write_lines(writer, lines));
    let mut state = State {
        outbox,
        conversation,
        activity: SessionActivity::default(),
        tracker: SessionTracker::default(),
        handlers: JoinSet::new(),
        aborts: HashMap::new(),
        initialize: None,
    };
    let inbox = state
        .run(FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_LINE)), prompt, inbox)
        .await;
    // Dropping the state aborts pending handlers and closes the outbox, which closes stdin.
    drop(state);
    if timeout(FLUSH_TIMEOUT, writer).await.is_err() {
        tracing::warn!("agent cli stdin did not drain");
    }
    inbox
}

struct State {
    outbox: mpsc::Sender<String>,
    conversation: Conversation,
    activity: SessionActivity,
    tracker: SessionTracker,
    handlers: JoinSet<String>,
    aborts: HashMap<String, AbortHandle>,
    initialize: Option<Instant>,
}

impl State {
    async fn run<S>(
        &mut self,
        mut lines: S,
        prompt: Prompt,
        mut inbox: mpsc::Receiver<Prompt>,
    ) -> mpsc::Receiver<Prompt>
    where
        S: Stream<Item = Result<String, LinesCodecError>> + Unpin,
    {
        self.initialize = Some(Instant::now() + self.conversation.limits.initialize);
        self.write(&outgoing::control_request(INITIALIZE, outgoing::initialize())).await;
        self.send_prompt(&prompt).await;
        let mut background: Option<Instant> = None;
        let mut inbox_open = true;
        loop {
            let phase = self.activity.phase();
            if phase == Phase::Idle {
                // A prompt that arrived meanwhile must be sent even though the agent is idle.
                match inbox.try_recv() {
                    Ok(next) => {
                        self.send_prompt(&next).await;
                        continue;
                    }
                    Err(_) => break,
                }
            }
            let limits = self.conversation.limits;
            let deadline = match phase {
                Phase::Busy | Phase::Idle => {
                    background = None;
                    None
                }
                Phase::Background => {
                    Some(*background.get_or_insert_with(|| Instant::now() + limits.background))
                }
                Phase::Settling => {
                    background = None;
                    Some(Instant::now() + limits.settle)
                }
            };
            let initialize = self.initialize;
            let cancel = self.conversation.cancel.clone();
            let step = tokio::select! {
                // A stop must win over a prompt that arrived at the same moment, which then
                // stays in the inbox for the caller instead of reaching an interrupted CLI.
                biased;
                () = cancel.cancelled() => Step::Cancelled,
                line = lines.next() => Step::Line(line),
                prompt = inbox.recv(), if inbox_open => Step::Prompt(prompt),
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    Step::Deadline(phase)
                }
                () = sleep_until(initialize.unwrap_or_else(Instant::now)), if initialize.is_some() => {
                    Step::InitializeTimeout
                }
                Some(handled) = self.handlers.join_next(), if !self.handlers.is_empty() => {
                    Step::Handled(handled)
                }
            };
            match self.step(step, &mut inbox_open).await {
                Flow::Continue => {}
                Flow::Stop => break,
            }
        }
        if self.conversation.cancel.is_cancelled() {
            self.interrupt();
        }
        inbox
    }

    async fn step(&mut self, step: Step, inbox_open: &mut bool) -> Flow {
        match step {
            Step::Cancelled => Flow::Stop,
            Step::Line(None) => {
                if self.activity.awaiting_result() {
                    self.emit(AgentEvent::Failed("Claude завершился без результата".to_owned()))
                        .await;
                }
                Flow::Stop
            }
            Step::Line(Some(Err(error))) => {
                self.emit(AgentEvent::Failed(format!(
                    "Не удалось прочитать вывод Claude: {error}"
                )))
                .await;
                Flow::Stop
            }
            Step::Line(Some(Ok(line))) => match wire::parse_line(&line) {
                Ok(message) => self.receive(message).await,
                Err(error) => {
                    tracing::warn!(%error, "unreadable agent cli message skipped");
                    Flow::Continue
                }
            },
            Step::Prompt(Some(prompt)) => {
                self.send_prompt(&prompt).await;
                Flow::Continue
            }
            Step::Prompt(None) => {
                *inbox_open = false;
                Flow::Continue
            }
            Step::Deadline(phase) => {
                match phase {
                    Phase::Background => {
                        self.emit(AgentEvent::BackgroundAbandoned(self.activity.background()))
                            .await;
                    }
                    Phase::Settling | Phase::Busy | Phase::Idle => {}
                }
                Flow::Stop
            }
            Step::InitializeTimeout => {
                let seconds = self.conversation.limits.initialize.as_secs();
                self.emit(AgentEvent::Failed(format!(
                    "Claude не ответил на инициализацию за {seconds} с"
                )))
                .await;
                Flow::Stop
            }
            Step::Handled(Ok(request_id)) => {
                self.aborts.remove(&request_id);
                Flow::Continue
            }
            Step::Handled(Err(error)) => {
                if error.is_panic() {
                    tracing::error!(%error, "control request handler panicked");
                }
                Flow::Continue
            }
        }
    }

    async fn receive(&mut self, message: Incoming) -> Flow {
        match message {
            Incoming::ControlRequest { request_id, request } => {
                self.spawn_handler(request_id, request);
                Flow::Continue
            }
            Incoming::ControlCancelRequest { request_id } => {
                if let Some(handler) = self.aborts.remove(&request_id) {
                    handler.abort();
                }
                Flow::Continue
            }
            Incoming::ControlResponse { response: ControlReply::Success { request_id } } => {
                if request_id == INITIALIZE {
                    self.initialize = None;
                }
                Flow::Continue
            }
            Incoming::ControlResponse { response: ControlReply::Error { request_id, error } } => {
                if request_id != INITIALIZE {
                    tracing::warn!(%request_id, %error, "agent cli rejected a control request");
                    return Flow::Continue;
                }
                self.emit(AgentEvent::Failed(format!("Claude отклонил инициализацию: {error}")))
                    .await;
                Flow::Stop
            }
            Incoming::System(_)
            | Incoming::Assistant(_)
            | Incoming::User(_)
            | Incoming::Result(_)
            | Incoming::Other => {
                self.activity.observe(&message);
                let background = self.activity.background().len();
                for event in self.tracker.translate(&message, background) {
                    if !self.emit(event).await {
                        return Flow::Stop;
                    }
                }
                Flow::Continue
            }
        }
    }

    fn spawn_handler(&mut self, request_id: String, request: ControlRequest) {
        let channel = Arc::clone(&self.conversation.channel);
        let outbox = self.outbox.clone();
        let id = request_id.clone();
        let handler = self.handlers.spawn(async move {
            let reply = match answer(request, channel.as_ref()).await {
                Ok(response) => outgoing::control_success(&id, response),
                Err(error) => outgoing::control_error(&id, &error),
            };
            // The CLI may have exited meanwhile; its closed stdout ends the conversation.
            let _ = outbox.send(reply.to_string()).await;
            id
        });
        self.aborts.insert(request_id, handler);
    }

    async fn send_prompt(&mut self, prompt: &Prompt) {
        let uuid = Uuid::new_v4().to_string();
        self.write(&outgoing::user_message(prompt, &uuid)).await;
        self.activity.sent(uuid);
    }

    /// Gives up when the conversation is stopped, so a CLI that does not read stdin cannot
    /// hold `/stop` back.
    async fn write(&self, message: &Value) {
        tokio::select! {
            biased;
            () = self.conversation.cancel.cancelled() => {}
            sent = self.outbox.send(message.to_string()) => {
                if sent.is_err() {
                    tracing::warn!("agent cli stdin is closed");
                }
            }
        }
    }

    fn interrupt(&self) {
        let line = outgoing::control_request(INTERRUPT, outgoing::interrupt()).to_string();
        // A full outbox means the CLI stopped reading stdin; it is killed after the exit timeout.
        if let Err(error) = self.outbox.try_send(line) {
            tracing::warn!(%error, "interrupt not delivered to agent cli");
        }
    }

    /// False when nobody listens to the events any more or the conversation is stopped, so a
    /// slow reader cannot hold `/stop` back.
    async fn emit(&self, event: AgentEvent) -> bool {
        tokio::select! {
            biased;
            () = self.conversation.cancel.cancelled() => false,
            sent = self.conversation.events.send(event) => sent.is_ok(),
        }
    }
}

async fn answer(request: ControlRequest, channel: &dyn UserChannel) -> Result<Value, String> {
    match request {
        ControlRequest::CanUseTool { tool_name, input } => {
            Ok(match permissions::route(&tool_name, &input) {
                Route::Allow => permissions::allow(input),
                Route::Ask(questions) => permissions::answered(input, channel.ask(questions).await),
                Route::Request(tool) => permissions::decided(input, channel.request(tool).await),
            })
        }
        ControlRequest::McpMessage { server_name, message } => {
            let reply = match mcp::handle(&server_name, &message) {
                McpStep::Reply(reply) => reply,
                McpStep::SendFile { id, file } => {
                    let path = file.path.clone();
                    mcp::send_file_reply(id, &path, channel.send_file(file).await)
                }
            };
            Ok(json!({"mcp_response": reply}))
        }
        ControlRequest::Other => Err("unsupported control request".to_owned()),
    }
}

async fn write_lines<W: AsyncWrite + Unpin>(mut writer: W, mut lines: mpsc::Receiver<String>) {
    while let Some(line) = lines.recv().await {
        let written = async {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await
        };
        if let Err(error) = written.await {
            tracing::warn!(%error, "writing to agent cli failed");
            return;
        }
    }
    // Closing stdin tells the CLI that no more input will come.
    if let Err(error) = writer.shutdown().await {
        tracing::debug!(%error, "closing agent cli stdin failed");
    }
}
