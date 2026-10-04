//! The app-server side of a duplex pipe, and a scripted human, for tests.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};

use crate::rpc::{Connection, Handler, RequestError, connect};

pub struct Peer {
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    writer: WriteHalf<DuplexStream>,
}

impl Peer {
    /// The next message from the client; `None` once the client closed its output.
    pub async fn read(&mut self) -> Option<Value> {
        let line = self.lines.next_line().await.unwrap()?;
        Some(serde_json::from_str(&line).unwrap())
    }

    pub async fn write(&mut self, message: Value) {
        self.writer.write_all(format!("{message}\n").as_bytes()).await.unwrap();
    }
}

pub fn pair(handler: Handler, timeout: Duration) -> (Connection, Peer) {
    let (ours, theirs) = tokio::io::duplex(1 << 20);
    let (our_read, our_write) = tokio::io::split(ours);
    let (their_read, their_write) = tokio::io::split(theirs);
    let connection = connect(our_read, our_write, handler, timeout);
    (connection, Peer { lines: BufReader::new(their_read).lines(), writer: their_write })
}

pub fn refusing() -> Handler {
    Arc::new(|method, _params| Box::pin(async move { Err(RequestError::Unsupported(method)) }))
}

use std::sync::Mutex;

use futures::future::BoxFuture;
use hub_agent::channel::UserChannel;
use hub_core::domain::{
    Decision, FileDelivery, OutgoingFile, Question, QuestionsOutcome, ToolRequest,
};

/// A human who answers every request the same scripted way and remembers what was asked.
pub struct Scripted {
    pub decision: Decision,
    pub outcome: QuestionsOutcome,
    pub delivery: FileDelivery,
    pub requests: Mutex<Vec<ToolRequest>>,
    pub questions: Mutex<Vec<Question>>,
}

impl Default for Scripted {
    fn default() -> Self {
        Self {
            decision: Decision::Allowed,
            outcome: QuestionsOutcome::Answered(Vec::new()),
            delivery: FileDelivery::Delivered,
            requests: Mutex::new(Vec::new()),
            questions: Mutex::new(Vec::new()),
        }
    }
}

impl UserChannel for Scripted {
    fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision> {
        self.requests.lock().unwrap().push(tool);
        Box::pin(async { self.decision.clone() })
    }

    fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        self.questions.lock().unwrap().extend(questions);
        Box::pin(async { self.outcome.clone() })
    }

    fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(async { self.delivery.clone() })
    }
}
