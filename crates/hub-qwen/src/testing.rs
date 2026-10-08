//! The Qwen side of a duplex pipe, and a scripted human, for tests.

use std::sync::Mutex;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_agent::channel::UserChannel;
use hub_agent::rpc::{Connection, Envelope, Handler, Wire, connect};
use hub_core::domain::{
    Decision, FileDelivery, OutgoingFile, Question, QuestionsOutcome, ToolRequest,
};
use serde_json::Value;
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};

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

/// A client connected to a fake `qwen --acp` over an in-memory pipe.
pub fn pair(handler: Handler, timeout: Duration) -> (Connection, Peer) {
    let (ours, theirs) = tokio::io::duplex(1 << 20);
    let (our_read, our_write) = tokio::io::split(ours);
    let (their_read, their_write) = tokio::io::split(theirs);
    let wire = Wire { peer: "qwen", envelope: Envelope::JsonRpc2, timeout };
    let connection = connect(our_read, our_write, handler, wire);
    (connection, Peer { lines: BufReader::new(their_read).lines(), writer: their_write })
}

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
