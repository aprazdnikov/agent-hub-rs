//! The hub's own MCP server for one agent session, over HTTP on the loopback interface.
//!
//! It offers `send_file`, with opt-in `ask_user`. Every request carries a bearer token and the
//! server's own `Host`, and none may carry an `Origin`, so other local processes and web pages
//! (DNS rebinding included) cannot reach the user's chat; dropping the server stops it.

use std::convert::Infallible;
use std::fmt;
use std::io;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hub_core::domain::QuestionsOutcome;
use hyper::body::{Bytes, Incoming};
use hyper::header::{
    ALLOW, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HOST, HeaderValue, ORIGIN,
};
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
use crate::tools::{
    ASK_USER, ASK_USER_DESCRIPTION, ASK_USER_SHAPE, SEND_FILE, SEND_FILE_DESCRIPTION, ToolResult,
    ask_user_schema, deliver_file, parse_questions, send_file_schema,
};

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
    /// Adds the hub's question tool for agents without native user-question transport.
    pub async fn start_with_questions(channel: Arc<dyn UserChannel>) -> io::Result<Self> {
        Self::start_tools(channel, Tools::FilesAndQuestions).await
    }

    pub async fn start(channel: Arc<dyn UserChannel>) -> io::Result<Self> {
        Self::start_tools(channel, Tools::Files).await
    }

    async fn start_tools(channel: Arc<dyn UserChannel>, tools: Tools) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let token = new_token();
        let stop = CancellationToken::new();
        let state = Arc::new(State {
            token: token.clone(),
            host: format!("127.0.0.1:{port}"),
            channel,
            tools,
        });
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

#[derive(Clone, Copy)]
enum Tools {
    Files,
    FilesAndQuestions,
}

struct State {
    token: String,
    /// The only `Host` the server answers to: its own loopback address.
    host: String,
    channel: Arc<dyn UserChannel>,
    tools: Tools,
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
    Forbidden,
    NotFound,
    MethodNotAllowed,
}

struct Headers {
    authorization: Option<HeaderValue>,
    host: Option<HeaderValue>,
    origin: Option<HeaderValue>,
}

impl Headers {
    fn of(request: &Request<Incoming>) -> Self {
        let headers = request.headers();
        Self {
            authorization: headers.get(AUTHORIZATION).cloned(),
            host: headers.get(HOST).cloned(),
            origin: headers.get(ORIGIN).cloned(),
        }
    }
}

struct Expected<'a> {
    token: &'a str,
    host: &'a str,
}

/// The token is checked first, so an unauthenticated caller learns nothing, not even the rules
/// below. Then the DNS-rebinding guard the MCP transport requires: a browser always sends
/// `Origin` (qwen's Node client sends none), and a rebound page carries a foreign `Host`; a
/// missing `Host` is refused because HTTP/1.1 requires one.
fn admit(method: &Method, path: &str, headers: &Headers, expected: &Expected<'_>) -> Admission {
    let wanted = format!("Bearer {}", expected.token);
    let presented = headers.authorization.as_ref().map_or(b"".as_slice(), HeaderValue::as_bytes);
    if !bool::from(presented.ct_eq(wanted.as_bytes())) {
        return Admission::Unauthorized;
    }
    let host_matches = headers.host.as_ref().is_some_and(|host| host == expected.host);
    if headers.origin.is_some() || !host_matches {
        return Admission::Forbidden;
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
    AskUser { id: Value, arguments: Value },
}

fn dispatch(message: &Value, tools: Tools) -> Step {
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
        "tools/list" => Step::Reply(rpc_result(&id, &json!({"tools": tool_schemas(tools)}))),
        "tools/call" => match message.pointer("/params/name").and_then(Value::as_str) {
            Some(SEND_FILE) => Step::SendFile {
                id,
                arguments: message.pointer("/params/arguments").cloned().unwrap_or(Value::Null),
            },
            Some(ASK_USER) if matches!(tools, Tools::FilesAndQuestions) => Step::AskUser {
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
        &Headers::of(&request),
        &Expected { token: &state.token, host: &state.host },
    );
    match admission {
        Admission::Unauthorized => return empty(StatusCode::UNAUTHORIZED),
        Admission::Forbidden => return empty(StatusCode::FORBIDDEN),
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
    match dispatch(&message, state.tools) {
        Step::Reply(reply) => json_response(StatusCode::OK, &reply),
        Step::Accepted => empty(StatusCode::ACCEPTED),
        Step::Invalid(reply) => json_response(StatusCode::BAD_REQUEST, &reply),
        Step::SendFile { id, arguments } => {
            let result = deliver_file(&arguments, state.channel.as_ref()).await;
            json_response(StatusCode::OK, &rpc_result(&id, &tool_result(result)))
        }
        Step::AskUser { id, arguments } => {
            let result = ask_user(&arguments, state.channel.as_ref()).await;
            json_response(StatusCode::OK, &rpc_result(&id, &tool_result(result)))
        }
    }
}

fn tool_schemas(tools: Tools) -> Vec<Value> {
    let file = json!({"name": SEND_FILE, "description": SEND_FILE_DESCRIPTION, "inputSchema": send_file_schema()});
    match tools {
        Tools::Files => vec![file],
        Tools::FilesAndQuestions => vec![
            file,
            json!({
                "name": ASK_USER, "description": ASK_USER_DESCRIPTION, "inputSchema": ask_user_schema(),
            }),
        ],
    }
}

async fn ask_user(arguments: &Value, channel: &dyn UserChannel) -> ToolResult {
    let Some(questions) = parse_questions(arguments) else {
        return ToolResult::Error(ASK_USER_SHAPE.to_owned());
    };
    if questions.len() > 5 || questions.iter().any(|question| question.options().len() > 4) {
        return ToolResult::Error("At most five questions with four options each".to_owned());
    }
    match channel.ask(questions).await {
        QuestionsOutcome::Answered(answers) => ToolResult::Success(
            json!({"answers": answers.into_iter().map(|answer| {
            json!({"question": answer.question, "answer": answer.answer})
        }).collect::<Vec<_>>()})
            .to_string(),
        ),
        QuestionsOutcome::Denied(denied) => ToolResult::Error(denied.reason),
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

    const HOST: &str = "127.0.0.1:4321";

    #[rstest]
    #[case::no_token(Method::POST, "/mcp", None, Some(HOST), None, Admission::Unauthorized)]
    #[case::wrong_token(
        Method::POST,
        "/mcp",
        Some("Bearer nope"),
        Some(HOST),
        None,
        Admission::Unauthorized
    )]
    #[case::token_without_scheme(
        Method::POST,
        "/mcp",
        Some("t0k3n"),
        Some(HOST),
        None,
        Admission::Unauthorized
    )]
    #[case::unknown_path(
        Method::POST,
        "/other",
        Some("Bearer t0k3n"),
        Some(HOST),
        None,
        Admission::NotFound
    )]
    #[case::get(
        Method::GET,
        "/mcp",
        Some("Bearer t0k3n"),
        Some(HOST),
        None,
        Admission::MethodNotAllowed
    )]
    #[case::post(Method::POST, "/mcp", Some("Bearer t0k3n"), Some(HOST), None, Admission::Mcp)]
    #[case::origin_present(
        Method::POST,
        "/mcp",
        Some("Bearer t0k3n"),
        Some(HOST),
        Some("http://evil.example"),
        Admission::Forbidden
    )]
    #[case::origin_null(
        Method::POST,
        "/mcp",
        Some("Bearer t0k3n"),
        Some(HOST),
        Some("null"),
        Admission::Forbidden
    )]
    #[case::foreign_host(
        Method::POST,
        "/mcp",
        Some("Bearer t0k3n"),
        Some("evil.example"),
        None,
        Admission::Forbidden
    )]
    #[case::wrong_port(
        Method::POST,
        "/mcp",
        Some("Bearer t0k3n"),
        Some("127.0.0.1:9999"),
        None,
        Admission::Forbidden
    )]
    #[case::host_without_port(
        Method::POST,
        "/mcp",
        Some("Bearer t0k3n"),
        Some("127.0.0.1"),
        None,
        Admission::Forbidden
    )]
    #[case::localhost_name(
        Method::POST,
        "/mcp",
        Some("Bearer t0k3n"),
        Some("localhost:4321"),
        None,
        Admission::Forbidden
    )]
    #[case::missing_host(
        Method::POST,
        "/mcp",
        Some("Bearer t0k3n"),
        None,
        None,
        Admission::Forbidden
    )]
    #[case::unauthenticated_origin_learns_nothing(
        Method::POST,
        "/mcp",
        None,
        Some("evil.example"),
        Some("http://evil.example"),
        Admission::Unauthorized
    )]
    fn requests_are_admitted(
        #[case] method: Method,
        #[case] path: &str,
        #[case] authorization: Option<&'static str>,
        #[case] host: Option<&'static str>,
        #[case] origin: Option<&'static str>,
        #[case] expected: Admission,
    ) {
        let headers = Headers {
            authorization: authorization.map(HeaderValue::from_static),
            host: host.map(HeaderValue::from_static),
            origin: origin.map(HeaderValue::from_static),
        };
        assert_eq!(
            admit(&method, path, &headers, &Expected { token: TOKEN, host: HOST }),
            expected
        );
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
        assert_eq!(dispatch(&message, Tools::Files), expected);
    }

    #[test]
    fn initialize_echoes_the_protocol_and_names_the_server() {
        let message = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                             "params": {"protocolVersion": "2025-03-26", "capabilities": {}}});
        let Step::Reply(reply) = dispatch(&message, Tools::Files) else {
            panic!("expected a reply")
        };
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
            dispatch(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}), Tools::Files)
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
