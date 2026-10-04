//! Newline-delimited JSON-RPC 2.0 over a child's stdio, in both directions.
//!
//! Each server request is answered in its own task, so one waiting on a human does not stall
//! the stream. Once the peer closes its output every call fails with `RpcError::Closed`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::codec::{FramedRead, LinesCodec};
use tokio_util::sync::CancellationToken;

// A whole turn item (a diff, a command's output) arrives as one line.
const MAX_LINE: usize = 64 * 1024 * 1024;
const OUTBOX: usize = 64;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    #[error("{message}")]
    Remote { code: i64, message: String },
    #[error("app-server closed the connection")]
    Closed,
    #[error("unexpected app-server message: {0}")]
    Protocol(String),
    #[error("app-server did not answer {method} in time")]
    Timeout { method: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Malformed(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

pub type Handler =
    Arc<dyn Fn(String, Value) -> BoxFuture<'static, Result<Value, RequestError>> + Send + Sync>;

type Waiters = HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>;
/// `None` once the peer's output closed: nothing more can be answered.
type Pending = Arc<Mutex<Option<Waiters>>>;

#[derive(Clone)]
pub struct RpcClient {
    outbox: mpsc::Sender<String>,
    pending: Pending,
    next: Arc<AtomicU64>,
    timeout: Duration,
    shutdown: CancellationToken,
}

pub struct Connection {
    pub client: RpcClient,
    pub notifications: mpsc::UnboundedReceiver<Notification>,
    pub reader: JoinHandle<()>,
}

#[must_use]
pub fn connect<R, W>(reader: R, writer: W, handler: Handler, timeout: Duration) -> Connection
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (outbox, lines) = mpsc::channel(OUTBOX);
    // Unbounded: a session waiting on a steer reply must not block the reader behind it.
    let (notify, notifications) = mpsc::unbounded_channel();
    let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
    let shutdown = CancellationToken::new();
    tokio::spawn(write(writer, lines, shutdown.clone()));
    let router = Link { outbox: outbox.clone(), pending: Arc::clone(&pending), notify, handler };
    let reader = tokio::spawn(read(reader, router));
    let client =
        RpcClient { outbox, pending, next: Arc::new(AtomicU64::new(1)), timeout, shutdown };
    Connection { client, notifications, reader }
}

impl RpcClient {
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (waiter, reply) = oneshot::channel();
        match lock(&self.pending).as_mut() {
            Some(waiters) => waiters.insert(id, waiter),
            None => return Err(RpcError::Closed),
        };
        let line = json!({"id": id, "method": method, "params": params}).to_string();
        if self.outbox.send(line).await.is_err() {
            self.forget(id);
            return Err(RpcError::Closed);
        }
        let outcome = tokio::time::timeout(self.timeout, reply).await;
        self.forget(id);
        match outcome {
            Err(_) => Err(RpcError::Timeout { method: method.to_owned() }),
            Ok(Err(_)) => Err(RpcError::Closed),
            Ok(Ok(reply)) => reply,
        }
    }

    pub async fn notify(&self, method: &str) -> Result<(), RpcError> {
        self.outbox.send(json!({"method": method}).to_string()).await.map_err(|_| RpcError::Closed)
    }

    /// Closes the peer's input, which is how app-server is told to exit.
    pub fn close(&self) {
        self.shutdown.cancel();
    }

    fn forget(&self, id: u64) {
        if let Some(waiters) = lock(&self.pending).as_mut() {
            waiters.remove(&id);
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
            () = shutdown.cancelled() => break,
            line = lines.recv() => line,
        };
        let Some(line) = line else { break };
        let written = async {
            writer.write_all(line.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await
        }
        .await;
        if let Err(error) = written {
            tracing::warn!(%error, "app-server input closed");
            return;
        }
    }
    if let Err(error) = writer.shutdown().await {
        tracing::debug!(%error, "app-server input already closed");
    }
}

struct Link {
    outbox: mpsc::Sender<String>,
    pending: Pending,
    notify: mpsc::UnboundedSender<Notification>,
    handler: Handler,
}

async fn read<R: AsyncRead + Unpin>(reader: R, router: Link) {
    let mut lines = FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_LINE));
    let mut answering = JoinSet::new();
    loop {
        tokio::select! {
            line = lines.next() => match line {
                Some(Ok(line)) => router.dispatch(&line, &mut answering),
                Some(Err(error)) => {
                    tracing::warn!(%error, "app-server output unreadable");
                    break;
                }
                None => break,
            },
            Some(joined) = answering.join_next(), if !answering.is_empty() => {
                if let Err(error) = joined {
                    tracing::error!(%error, "server request handler crashed");
                }
            }
        }
    }
    answering.abort_all();
    // Dropping the waiters fails every request still waiting with `Closed`.
    lock(&router.pending).take();
}

impl Link {
    fn dispatch(&self, line: &str, answering: &mut JoinSet<()>) {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%error, "unparsable app-server line");
                return;
            }
        };
        let id = message.get("id").filter(|id| id.is_u64() || id.is_string()).cloned();
        let params = message.get("params").filter(|params| params.is_object()).cloned();
        let params = params.unwrap_or_else(|| json!({}));
        match (id, message.get("method").and_then(Value::as_str)) {
            (Some(id), Some(method)) => {
                let reply = (self.handler)(method.to_owned(), params);
                answering.spawn(answer(id, method.to_owned(), reply, self.outbox.clone()));
            }
            (None, Some(method)) => {
                // The session is gone; nobody is left to read it.
                let _ = self.notify.send(Notification { method: method.to_owned(), params });
            }
            (Some(id), None) => self.resolve(&id, &message),
            (None, None) => tracing::warn!(%message, "unexpected app-server message"),
        }
    }

    fn resolve(&self, id: &Value, message: &Value) {
        let Some(id) = id.as_u64() else {
            tracing::warn!(%id, "reply to a request this client never sent");
            return;
        };
        let reply = match (message.get("error"), message.get("result")) {
            (Some(error), _) => match (
                error.get("code").and_then(Value::as_i64),
                error.get("message").and_then(Value::as_str),
            ) {
                (Some(code), Some(text)) => {
                    Err(RpcError::Remote { code, message: text.to_owned() })
                }
                (Some(_) | None, Some(_) | None) => {
                    Err(RpcError::Protocol(format!("malformed error: {error}")))
                }
            },
            (None, Some(result)) => Ok(result.clone()),
            (None, None) => Err(RpcError::Protocol(format!("malformed response: {message}"))),
        };
        let waiter = lock(&self.pending).as_mut().and_then(|waiters| waiters.remove(&id));
        if let Some(waiter) = waiter {
            // The caller gave up (timeout); the late reply has nowhere to go.
            let _ = waiter.send(reply);
        }
    }
}

async fn answer(
    id: Value,
    method: String,
    reply: BoxFuture<'static, Result<Value, RequestError>>,
    outbox: mpsc::Sender<String>,
) {
    let message = match reply.await {
        Ok(result) => json!({"id": id, "result": result}),
        Err(error @ RequestError::Unsupported(_)) => {
            tracing::debug!(%method, "unsupported server request");
            json!({"id": id, "error": {"code": METHOD_NOT_FOUND, "message": error.to_string()}})
        }
        Err(error @ RequestError::Malformed(_)) => {
            tracing::warn!(%method, %error, "malformed server request");
            json!({"id": id, "error": {"code": INVALID_PARAMS, "message": error.to_string()}})
        }
    };
    // The peer is gone; there is nobody left to answer.
    let _ = outbox.send(message.to_string()).await;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use tokio::sync::oneshot;

    use super::*;
    use crate::testing::{pair, refusing};

    const LONG: Duration = Duration::from_secs(5);

    fn id(message: &Value) -> Value {
        message.get("id").cloned().unwrap()
    }

    #[tokio::test]
    async fn responses_are_matched_by_id() {
        let (connection, mut peer) = pair(refusing(), LONG);
        let first = tokio::spawn({
            let client = connection.client.clone();
            async move { client.request("a", json!({})).await }
        });
        let a = peer.read().await.unwrap();
        let second = tokio::spawn({
            let client = connection.client.clone();
            async move { client.request("b", json!({"x": 1})).await }
        });
        let b = peer.read().await.unwrap();
        assert_eq!(
            (a.get("method").cloned(), b.get("params").cloned()),
            (Some(json!("a")), Some(json!({"x": 1})))
        );
        peer.write(json!({"id": id(&b), "result": {"name": "b"}})).await;
        peer.write(json!({"id": id(&a), "result": {"name": "a"}})).await;
        assert_eq!(
            (first.await.unwrap(), second.await.unwrap()),
            (Ok(json!({"name": "a"})), Ok(json!({"name": "b"})))
        );
    }

    #[tokio::test]
    async fn error_reply_is_a_remote_error() {
        let (connection, mut peer) = pair(refusing(), LONG);
        let request =
            tokio::spawn(async move { connection.client.request("turn/steer", json!({})).await });
        let sent = peer.read().await.unwrap();
        peer.write(
            json!({"id": id(&sent), "error": {"code": -32000, "message": "no active turn"}}),
        )
        .await;
        assert_eq!(
            request.await.unwrap(),
            Err(RpcError::Remote { code: -32000, message: "no active turn".to_owned() })
        );
    }

    #[tokio::test]
    async fn notifications_arrive_in_order_with_object_params() {
        let (mut connection, mut peer) = pair(refusing(), LONG);
        peer.write(json!({"method": "item/started", "params": {"n": 1}})).await;
        peer.write(json!({"method": "turn/completed"})).await;
        assert_eq!(
            (connection.notifications.recv().await, connection.notifications.recv().await),
            (
                Some(Notification { method: "item/started".to_owned(), params: json!({"n": 1}) }),
                Some(Notification { method: "turn/completed".to_owned(), params: json!({}) }),
            )
        );
    }

    #[tokio::test]
    async fn a_request_waiting_on_a_human_does_not_stall_the_stream() {
        let (release, released) = oneshot::channel::<()>();
        let released = Arc::new(tokio::sync::Mutex::new(Some(released)));
        let handler: Handler = Arc::new(move |_method, _params| {
            let released = Arc::clone(&released);
            Box::pin(async move {
                if let Some(wait) = released.lock().await.take() {
                    let _ = wait.await;
                }
                Ok(json!({"decision": "accept"}))
            })
        });
        let (connection, mut peer) = pair(handler, LONG);
        peer.write(
            json!({"id": "s-1", "method": "item/commandExecution/requestApproval", "params": {}}),
        )
        .await;
        let request = tokio::spawn({
            let client = connection.client.clone();
            async move { client.request("turn/start", json!({})).await }
        });
        let sent = peer.read().await.unwrap();
        peer.write(json!({"id": id(&sent), "result": {"turn": {"id": "u-1"}}})).await;
        assert_eq!(request.await.unwrap(), Ok(json!({"turn": {"id": "u-1"}})));
        release.send(()).unwrap();
        assert_eq!(peer.read().await, Some(json!({"id": "s-1", "result": {"decision": "accept"}})));
    }

    #[tokio::test]
    async fn unsupported_and_malformed_requests_get_error_codes() {
        let handler: Handler = Arc::new(|method, _params| {
            Box::pin(async move {
                match method.as_str() {
                    "bad" => Err(RequestError::Malformed("no questions".to_owned())),
                    _ => Err(RequestError::Unsupported(method)),
                }
            })
        });
        let (_connection, mut peer) = pair(handler, LONG);
        peer.write(json!({"id": 7, "method": "mystery", "params": {}})).await;
        assert_eq!(
            peer.read().await,
            Some(
                json!({"id": 7, "error": {"code": METHOD_NOT_FOUND, "message": "unsupported: mystery"}})
            )
        );
        peer.write(json!({"id": 8, "method": "bad", "params": {}})).await;
        assert_eq!(
            peer.read().await,
            Some(json!({"id": 8, "error": {"code": INVALID_PARAMS, "message": "no questions"}}))
        );
    }

    #[tokio::test]
    async fn closed_output_fails_waiting_and_later_requests() {
        let (mut connection, mut peer) = pair(refusing(), LONG);
        let request = tokio::spawn({
            let client = connection.client.clone();
            async move { client.request("turn/start", json!({})).await }
        });
        peer.read().await.unwrap();
        drop(peer);
        assert_eq!(request.await.unwrap(), Err(RpcError::Closed));
        assert_eq!(connection.client.request("turn/start", json!({})).await, Err(RpcError::Closed));
        assert_eq!(connection.notifications.recv().await, None);
    }

    #[tokio::test]
    async fn answers_in_flight_are_dropped_when_the_output_closes() {
        let (held, dropped) = oneshot::channel::<()>();
        let held = Arc::new(std::sync::Mutex::new(Some(held)));
        let handler: Handler = Arc::new(move |_method, _params| {
            let guard = held.lock().unwrap().take();
            Box::pin(async move {
                let _guard = guard;
                std::future::pending::<()>().await;
                Ok(json!({}))
            })
        });
        let (connection, mut peer) = pair(handler, LONG);
        peer.write(json!({"id": 1, "method": "item/tool/requestUserInput", "params": {}})).await;
        tokio::task::yield_now().await;
        drop(peer);
        connection.reader.await.unwrap();
        assert!(dropped.await.is_err());
    }

    #[tokio::test]
    async fn silent_peer_times_out() {
        let (connection, mut peer) = pair(refusing(), Duration::from_millis(50));
        let request =
            tokio::spawn(async move { connection.client.request("initialize", json!({})).await });
        peer.read().await.unwrap();
        assert_eq!(
            request.await.unwrap(),
            Err(RpcError::Timeout { method: "initialize".to_owned() })
        );
    }

    #[tokio::test]
    async fn close_ends_the_peer_input() {
        let (connection, mut peer) = pair(refusing(), LONG);
        connection.client.notify("initialized").await.unwrap();
        assert_eq!(peer.read().await, Some(json!({"method": "initialized"})));
        connection.client.close();
        assert_eq!(peer.read().await, None);
    }
}
