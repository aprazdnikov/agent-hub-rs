//! The agent side of a duplex pipe, for tests.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};

use crate::rpc::{Connection, Envelope, Handler, RequestError, Wire, connect};

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

pub fn pair(handler: Handler, wire: Wire) -> (Connection, Peer) {
    let (ours, theirs) = tokio::io::duplex(1 << 20);
    let (our_read, our_write) = tokio::io::split(ours);
    let (their_read, their_write) = tokio::io::split(theirs);
    let connection = connect(our_read, our_write, handler, wire);
    (connection, Peer { lines: BufReader::new(their_read).lines(), writer: their_write })
}

pub fn bare(timeout: Duration) -> Wire {
    Wire { peer: "test agent", envelope: Envelope::Bare, timeout }
}

pub fn refusing() -> Handler {
    Arc::new(|method, _params| Box::pin(async move { Err(RequestError::Unsupported(method)) }))
}
