//! Newline-delimited `Pi` RPC over a child's stdio, in both directions.
//!
//! `Pi`'s shape is not JSON-RPC: a command is a flat object with a string `id`, and the answer
//! comes back as `{"type":"response","command":…}` on the same `id`. Replies are therefore
//! correlated here, while everything the hub shows the user — prose, tool calls, the settled
//! marker, and the dialogs the bridge extension opens — leaves through the event channel or the
//! handler. A dialog is answered in its own task, so a human taking minutes does not stall the
//! stream.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::StreamExt as _;
use hub_core::domain::Prompt;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::codec::{FramedRead, LinesCodec};
use tokio_util::sync::CancellationToken;

use crate::protocol::{self, Wire};
use crate::requests::Gate;

// One event can carry a whole tool result.
const MAX_LINE: usize = 64 * 1024 * 1024;
const OUTBOX: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    #[error("{0}")]
    Refused(String),
    #[error("Pi closed the connection")]
    Closed,
    #[error("Pi did not answer {command} in time")]
    Timeout { command: String },
}

type Waiters = HashMap<String, oneshot::Sender<Result<Option<serde_json::Value>, LinkError>>>;
/// `None` once the child's output closed: nothing more can be answered.
type Pending = Arc<Mutex<Option<Waiters>>>;

/// One open `Pi` RPC conversation: send commands, read events.
pub struct Link {
    input: mpsc::Sender<String>,
    events: mpsc::UnboundedReceiver<Wire>,
    pending: Pending,
    next: Arc<AtomicU64>,
    shutdown: CancellationToken,
    writer: JoinHandle<()>,
    reader: JoinHandle<()>,
}

impl Link {
    /// Takes over the child's streams. `gate` answers the approval dialogs the bridge opens.
    pub fn spawn<R, W>(stdout: R, stdin: W, gate: &Gate) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (input, lines) = mpsc::channel(OUTBOX);
        // Unbounded: a turn waiting on a reply must not block the reader behind it.
        let (events, receiver) = mpsc::unbounded_channel();
        let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
        let shutdown = CancellationToken::new();
        let writer = tokio::spawn(write(stdin, lines, shutdown.clone()));
        let dialogs = Dialogs { input: input.clone(), gate: gate.clone() };
        let reader = tokio::spawn(read(stdout, events, Arc::clone(&pending), dialogs));
        Self {
            input,
            events: receiver,
            pending,
            next: Arc::new(AtomicU64::new(1)),
            shutdown,
            writer,
            reader,
        }
    }

    /// Sends one command and waits for its answer. The `data` of a successful reply is what the
    /// caller parses; `Pi` reports a refused command as `success: false` plus `error`.
    pub async fn call(
        &self,
        command: &str,
        body: serde_json::Value,
        timeout: Duration,
    ) -> Result<Option<serde_json::Value>, LinkError> {
        let id = protocol::request_id(self.next.fetch_add(1, Ordering::Relaxed));
        let (waiter, reply) = oneshot::channel();
        match lock(&self.pending).as_mut() {
            Some(waiters) => waiters.insert(id.clone(), waiter),
            None => return Err(LinkError::Closed),
        };
        let line = protocol::command(&id, &body);
        if self.input.send(line).await.is_err() {
            self.forget(&id);
            return Err(LinkError::Closed);
        }
        let outcome = tokio::time::timeout(timeout, reply).await;
        self.forget(&id);
        match outcome {
            Err(_) => Err(LinkError::Timeout { command: command.to_owned() }),
            Ok(Err(_)) => Err(LinkError::Closed),
            Ok(Ok(answer)) => answer,
        }
    }

    /// Reads the next thing the user should see. `None` once the child's output has closed.
    pub async fn event(&mut self) -> Option<Wire> {
        self.events.recv().await
    }

    /// A queued mid-turn message: `Pi` folds it into the running turn instead of starting a
    /// second one. Fire and forget — a steer that cannot be written means the turn is ending.
    pub async fn steer(&self, prompt: &Prompt) -> Result<(), LinkError> {
        self.write_command(protocol::steer(prompt)).await
    }

    /// Stops the running turn. `Pi` keeps the process: the conversation may be prompted again.
    pub async fn abort(&self) {
        let line = protocol::command(&self.take_id(), &protocol::abort());
        if self.input.send(line).await.is_err() {
            tracing::debug!("Pi abort input closed");
        }
    }

    async fn write_command(&self, body: serde_json::Value) -> Result<(), LinkError> {
        let line = protocol::command(&self.take_id(), &body);
        self.input.send(line).await.map_err(|_| LinkError::Closed)
    }

    /// Command ids are minted here so that a reply can always be matched to its command.
    fn take_id(&self) -> String {
        protocol::request_id(self.next.fetch_add(1, Ordering::Relaxed))
    }

    /// Closes the child's input, which is how a stdio agent is told to exit.
    pub fn close(&self) {
        self.shutdown.cancel();
    }

    /// Releases the two tasks that own the child's streams. The child itself is waited for by
    /// the process that spawned it.
    pub fn finish(&mut self) {
        let Self { input: _, events: _, pending: _, next: _, shutdown: _, writer, reader } = self;
        reader.abort();
        writer.abort();
    }

    fn forget(&self, id: &str) {
        if let Some(waiters) = lock(&self.pending).as_mut() {
            waiters.remove(id);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

async fn write<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut lines: mpsc::Receiver<String>,
    shutdown: CancellationToken,
) {
    loop {
        let line = tokio::select! {
            // What was sent before `close` still goes out, such as a `/stop`'s abort.
            biased;
            line = lines.recv() => line,
            () = shutdown.cancelled() => break,
        };
        let Some(line) = line else { break };
        if let Err(error) = write_line(&mut writer, &line).await {
            tracing::warn!(%error, "Pi input closed");
            return;
        }
    }
    if let Err(error) = writer.shutdown().await {
        tracing::debug!(%error, "Pi input already closed");
    }
}

async fn write_line<W: AsyncWrite + Unpin>(writer: &mut W, line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await
}

struct Dialogs {
    input: mpsc::Sender<String>,
    gate: Gate,
}

async fn read<R: AsyncRead + Unpin>(
    stdout: R,
    events: mpsc::UnboundedSender<Wire>,
    pending: Pending,
    dialogs: Dialogs,
) {
    let mut lines = FramedRead::new(stdout, LinesCodec::new_with_max_length(MAX_LINE));
    let mut answering = JoinSet::new();
    loop {
        tokio::select! {
            line = lines.next() => match line {
                Some(Ok(line)) => dispatch(&line, &events, &pending, &dialogs, &mut answering),
                Some(Err(error)) => {
                    tracing::warn!(%error, "Pi output unreadable");
                    break;
                }
                None => break,
            },
            Some(joined) = answering.join_next(), if !answering.is_empty() => {
                if let Err(error) = joined {
                    tracing::error!(%error, "Pi approval handler crashed");
                }
            }
        }
    }
    answering.abort_all();
    // Dropping the waiters fails every command still waiting with `Closed`.
    if let Some(waiters) = lock(&pending).take() {
        for (_, waiter) in waiters {
            let _ = waiter.send(Err(LinkError::Closed));
        }
    }
    drop(events);
}

fn dispatch(
    line: &str,
    events: &mpsc::UnboundedSender<Wire>,
    pending: &Pending,
    dialogs: &Dialogs,
    answering: &mut JoinSet<()>,
) {
    match Wire::parse(line) {
        Wire::Reply { id, data, error, .. } => {
            let answer = match error {
                Some(message) => Err(LinkError::Refused(message)),
                None => Ok(data),
            };
            let waiter = lock(pending).as_mut().and_then(|waiters| waiters.remove(&id));
            // The caller gave up (timeout); a late reply has nowhere to go.
            if let Some(waiter) = waiter {
                let _ = waiter.send(answer);
            }
        }
        // `yolo` registers no hook, so a dialog here means a hand-written extension: answering
        // it keeps the turn from hanging on nobody.
        Wire::Approval { id, summary } if !id.is_empty() => {
            let gate = dialogs.gate.clone();
            let input = dialogs.input.clone();
            answering.spawn(async move { gate.answer(&input, &id, &summary).await });
        }
        // An event `Pi` adds in a later release is not something to show the user.
        Wire::Ignored => {}
        Wire::Malformed => tracing::debug!(bytes = line.len(), "unreadable Pi line ignored"),
        event => {
            let _ = events.send(event);
        }
    }
}
