//! The hub's own MCP server for one agent session, over HTTP on the loopback interface.
//!
//! It offers only `send_file`. Every request must carry the session's bearer token, so other
//! local processes and web pages cannot reach the user's chat; dropping the server stops it.

use std::convert::Infallible;
use std::fmt;
use std::io;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{ALLOW, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_util::sync::{CancellationToken, DropGuard};
use uuid::Uuid;

use crate::channel::UserChannel;
use crate::rpc::{INVALID_PARAMS, METHOD_NOT_FOUND};
use crate::tools::{SEND_FILE, SEND_FILE_DESCRIPTION, ToolResult, deliver_file, send_file_schema};

pub const SERVER_NAME: &str = "agent-hub";
pub const MCP_PATH: &str = "/mcp";
const MAX_BODY: usize = 1024 * 1024;
const MAX_CONNECTIONS: usize = 16;
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
// Used only if the client does not say which protocol version it speaks.
const FALLBACK_PROTOCOL: &str = "2025-06-18";
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;

pub struct McpHttpServer {
    url: String,
    token: String,
    _stop: DropGuard,
}

impl fmt::Debug for McpHttpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpHttpServer")
            .field("url", &self.url)
            .field("token", &"***")
            .finish_non_exhaustive()
    }
}

impl McpHttpServer {
    pub async fn start(channel: Arc<dyn UserChannel>) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let token = new_token();
        let stop = CancellationToken::new();
        let state = Arc::new(State { token: token.clone(), channel });
        tokio::spawn(accept(listener, state, stop.clone()));
        Ok(Self {
            url: format!("http://127.0.0.1:{port}{MCP_PATH}"),
            token,
            _stop: stop.drop_guard(),
        })
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }
}

/// Two v4 UUIDs from the OS generator: 244 random bits, above the 128 the design asks for.
fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

struct State {
    token: String,
    channel: Arc<dyn UserChannel>,
}

async fn accept(listener: TcpListener, state: Arc<State>, stop: CancellationToken) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            accepted = listener.accept(), if connections.len() < MAX_CONNECTIONS => match accepted {
                Ok((stream, _peer)) => {
                    connections.spawn(serve_connection(stream, Arc::clone(&state)));
                }
                Err(error) => {
                    tracing::warn!(%error, "hub MCP server could not accept a connection");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
            Some(done) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = done {
                    tracing::error!(%error, "hub MCP connection crashed");
                }
            }
        }
    }
    // Dropping the set aborts the connections still open, a delivery in flight included.
}

async fn serve_connection(stream: TcpStream, state: Arc<State>) {
    let service = service_fn(move |request| {
        let state = Arc::clone(&state);
        async move { Ok::<_, Infallible>(respond(request, &state).await) }
    });
    let mut builder = http1::Builder::new();
    builder.timer(TokioTimer::new()).header_read_timeout(READ_TIMEOUT);
    if let Err(error) = builder.serve_connection(TokioIo::new(stream), service).await {
        tracing::debug!(%error, "hub MCP connection ended with an error");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    Mcp,
    Unauthorized,
    NotFound,
    MethodNotAllowed,
}

/// The token is checked before anything else, so an unauthenticated caller learns nothing.
fn admit(
    method: &Method,
    path: &str,
    authorization: Option<&HeaderValue>,
    token: &str,
) -> Admission {
    let expected = format!("Bearer {token}");
    let presented = authorization.map_or(b"".as_slice(), HeaderValue::as_bytes);
    if !bool::from(presented.ct_eq(expected.as_bytes())) {
        return Admission::Unauthorized;
    }
    if path != MCP_PATH {
        return Admission::NotFound;
    }
    if *method != Method::POST {
        return Admission::MethodNotAllowed;
    }
    Admission::Mcp
}

#[derive(Debug, PartialEq)]
enum Step {
    Reply(Value),
    Accepted,
    Invalid(Value),
    SendFile { id: Value, arguments: Value },
}

fn dispatch(message: &Value) -> Step {
    let Some(object) = message.as_object() else {
        return Step::Invalid(rpc_error(&Value::Null, INVALID_REQUEST, "Invalid request"));
    };
    // Notifications and the client's replies get no JSON-RPC answer.
    let (Some(id), Some(method)) =
        (object.get("id").cloned(), object.get("method").and_then(Value::as_str))
    else {
        return Step::Accepted;
    };
    match method {
        "initialize" => {
            let version = message
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(FALLBACK_PROTOCOL);
            Step::Reply(rpc_result(
                &id,
                &json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
                }),
            ))
        }
        "ping" => Step::Reply(rpc_result(&id, &json!({}))),
        "tools/list" => Step::Reply(rpc_result(
            &id,
            &json!({"tools": [{
                "name": SEND_FILE,
                "description": SEND_FILE_DESCRIPTION,
                "inputSchema": send_file_schema(),
            }]}),
        )),
        "tools/call" => match message.pointer("/params/name").and_then(Value::as_str) {
            Some(SEND_FILE) => Step::SendFile {
                id,
                arguments: message.pointer("/params/arguments").cloned().unwrap_or(Value::Null),
            },
            Some(other) => {
                Step::Reply(rpc_error(&id, INVALID_PARAMS, &format!("Unknown tool: {other}")))
            }
            None => Step::Reply(rpc_error(&id, INVALID_PARAMS, "Unknown tool: ")),
        },
        other => {
            Step::Reply(rpc_error(&id, METHOD_NOT_FOUND, &format!("Method not found: {other}")))
        }
    }
}

async fn respond(request: Request<Incoming>, state: &State) -> Response<Full<Bytes>> {
    let admission = admit(
        request.method(),
        request.uri().path(),
        request.headers().get(AUTHORIZATION),
        &state.token,
    );
    match admission {
        Admission::Unauthorized => return empty(StatusCode::UNAUTHORIZED),
        Admission::NotFound => return empty(StatusCode::NOT_FOUND),
        Admission::MethodNotAllowed => {
            let mut response = empty(StatusCode::METHOD_NOT_ALLOWED);
            response.headers_mut().insert(ALLOW, HeaderValue::from_static("POST"));
            return response;
        }
        Admission::Mcp => {}
    }
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|length| length.to_str().ok())
        .and_then(|length| length.parse::<usize>().ok());
    if declared.is_some_and(|length| length > MAX_BODY) {
        return empty(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let body = Limited::new(request.into_body(), MAX_BODY).collect();
    let body = match tokio::time::timeout(READ_TIMEOUT, body).await {
        Err(_) => return empty(StatusCode::REQUEST_TIMEOUT),
        Ok(Err(error)) if error.downcast_ref::<LengthLimitError>().is_some() => {
            return empty(StatusCode::PAYLOAD_TOO_LARGE);
        }
        Ok(Err(error)) => {
            tracing::debug!(%error, "hub MCP request body unreadable");
            return empty(StatusCode::BAD_REQUEST);
        }
        Ok(Ok(collected)) => collected.to_bytes(),
    };
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            &rpc_error(&Value::Null, PARSE_ERROR, "Parse error"),
        );
    };
    match dispatch(&message) {
        Step::Reply(reply) => json_response(StatusCode::OK, &reply),
        Step::Accepted => empty(StatusCode::ACCEPTED),
        Step::Invalid(reply) => json_response(StatusCode::BAD_REQUEST, &reply),
        Step::SendFile { id, arguments } => {
            let result = deliver_file(&arguments, state.channel.as_ref()).await;
            json_response(StatusCode::OK, &rpc_result(&id, &tool_result(result)))
        }
    }
}

fn tool_result(result: ToolResult) -> Value {
    let (text, failed) = match result {
        ToolResult::Success(text) => (text, false),
        ToolResult::Error(text) => (text, true),
    };
    json!({"content": [{"type": "text", "text": text}], "isError": failed})
}

fn rpc_result(id: &Value, result: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn empty(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
}

fn json_response(status: StatusCode, body: &Value) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body.to_string())));
    *response.status_mut() = status;
    response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    const TOKEN: &str = "t0k3n";

    #[rstest]
    #[case::no_token(Method::POST, "/mcp", None, Admission::Unauthorized)]
    #[case::wrong_token(Method::POST, "/mcp", Some("Bearer nope"), Admission::Unauthorized)]
    #[case::token_without_scheme(Method::POST, "/mcp", Some("t0k3n"), Admission::Unauthorized)]
    #[case::unknown_path(Method::POST, "/other", Some("Bearer t0k3n"), Admission::NotFound)]
    #[case::get(Method::GET, "/mcp", Some("Bearer t0k3n"), Admission::MethodNotAllowed)]
    #[case::post(Method::POST, "/mcp", Some("Bearer t0k3n"), Admission::Mcp)]
    fn requests_are_admitted(
        #[case] method: Method,
        #[case] path: &str,
        #[case] authorization: Option<&'static str>,
        #[case] expected: Admission,
    ) {
        let header = authorization.map(HeaderValue::from_static);
        assert_eq!(admit(&method, path, header.as_ref(), TOKEN), expected);
    }

    #[rstest]
    #[case::notification(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), Step::Accepted)]
    #[case::client_reply(json!({"jsonrpc": "2.0", "id": 3, "result": {}}), Step::Accepted)]
    #[case::ping(json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}), Step::Reply(json!({"jsonrpc": "2.0", "id": 1, "result": {}})))]
    #[case::unknown_method(json!({"jsonrpc": "2.0", "id": 2, "method": "resources/list"}), Step::Reply(json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -32601, "message": "Method not found: resources/list"}})))]
    #[case::unknown_tool(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "rm"}}), Step::Reply(json!({"jsonrpc": "2.0", "id": 4, "error": {"code": -32602, "message": "Unknown tool: rm"}})))]
    #[case::send_file(json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "send_file", "arguments": {"path": "a.txt"}}}), Step::SendFile { id: json!(5), arguments: json!({"path": "a.txt"}) })]
    #[case::not_an_object(json!([1]), Step::Invalid(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "Invalid request"}})))]
    fn messages_are_dispatched(#[case] message: Value, #[case] expected: Step) {
        assert_eq!(dispatch(&message), expected);
    }

    #[test]
    fn initialize_echoes_the_protocol_and_names_the_server() {
        let message = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                             "params": {"protocolVersion": "2025-03-26", "capabilities": {}}});
        let Step::Reply(reply) = dispatch(&message) else { panic!("expected a reply") };
        assert_eq!(
            (
                reply.pointer("/result/protocolVersion"),
                reply.pointer("/result/serverInfo/name"),
                reply.pointer("/result/capabilities")
            ),
            (Some(&json!("2025-03-26")), Some(&json!("agent-hub")), Some(&json!({"tools": {}})))
        );
    }

    #[test]
    fn tools_list_offers_only_send_file() {
        let Step::Reply(reply) =
            dispatch(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
        else {
            panic!("expected a reply")
        };
        assert_eq!(reply.pointer("/result/tools/0/name"), Some(&json!("send_file")));
        assert_eq!(reply.pointer("/result/tools/1"), None);
    }

    #[test]
    fn tokens_are_long_and_fresh() {
        let (first, second) = (new_token(), new_token());
        assert!(first.len() >= 64 && first != second);
    }
}
