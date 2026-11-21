use std::error::Error;
use std::io::{self, Cursor, Read};

use hub_memory::{Store, serve};
use serde_json::{Value, json};
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;

fn fixture() -> Result<TempDir, Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    std::fs::create_dir(temp.path().join("project"))?;
    Ok(temp)
}

fn store(temp: &TempDir, actor: &str) -> Result<Store, Box<dyn Error>> {
    Ok(Store::open(&temp.path().join("memory"), &temp.path().join("project"), actor)?)
}

fn initialization(version: &str) -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
        "protocolVersion":version,"capabilities":{},
        "clientInfo":{"name":"test-client","version":"1"}
    }})
}

fn initialized() -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/initialized"})
}

fn call(id: u32, name: &str, arguments: &Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name":name,"arguments":arguments
    }})
}

fn write_args() -> Value {
    json!({"key":"architecture","expected_revision":0,"kind":"decision",
        "title":"Memory architecture","body":"Use a local store.",
        "sources":["User decision in task 42"]})
}

fn exchange(store: Store, requests: &[Value]) -> Result<Vec<Value>, Box<dyn Error>> {
    let input = requests.iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n";
    let mut output = Vec::new();
    serve(store, Cursor::new(input), &mut output)?;
    String::from_utf8(output)?.lines().map(|line| Ok(serde_json::from_str(line)?)).collect()
}

#[test]
fn negotiates_known_and_unknown_protocol_versions() -> TestResult {
    let temp = fixture()?;
    let actual = ["2025-06-18", "2025-11-25", "2030-01-01"]
        .into_iter()
        .map(|version| {
            let replies = exchange(store(&temp, "reader")?, &[initialization(version)])?;
            Ok(replies.first().and_then(|reply| reply.pointer("/result/protocolVersion")).cloned())
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    assert_eq!(
        actual,
        vec![Some(json!("2025-06-18")), Some(json!("2025-11-25")), Some(json!("2025-11-25"))]
    );
    Ok(())
}

#[test]
fn requires_initialization_before_exposing_tools() -> TestResult {
    let temp = fixture()?;
    let replies = exchange(
        store(&temp, "reader")?,
        &[
            json!({"jsonrpc":"2.0","id":0,"method":"tools/list"}),
            initialization("2025-11-25"),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            initialized(),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
        ],
    )?;
    let actual = replies
        .iter()
        .map(|reply| {
            (
                reply.get("id").cloned(),
                reply.pointer("/error/code").cloned(),
                reply.pointer("/result/tools").and_then(Value::as_array).map(Vec::len),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (Some(json!(0)), Some(json!(-32002)), None),
            (Some(json!(1)), None, None),
            (Some(json!(2)), Some(json!(-32002)), None),
            (Some(json!(3)), None, Some(3))
        ]
    );
    Ok(())
}

#[test]
fn preserves_writes_across_independent_client_sessions() -> TestResult {
    let temp = fixture()?;
    exchange(
        store(&temp, "writer")?,
        &[initialization("2025-11-25"), initialized(), call(2, "memory_write", &write_args())],
    )?;
    let replies = exchange(
        store(&temp, "reader")?,
        &[
            initialization("2025-11-25"),
            initialized(),
            call(2, "memory_get", &json!({"key":"architecture"})),
            call(3, "memory_search", &json!({"query":"architecture"})),
        ],
    )?;
    let entry = replies.get(1).and_then(|reply| reply.pointer("/result/structuredContent"));
    let hits = replies
        .get(2)
        .and_then(|reply| reply.pointer("/result/structuredContent/hits"))
        .and_then(Value::as_array);
    assert_eq!(
        (
            entry.and_then(|entry| entry.get("body")).cloned(),
            entry.and_then(|entry| entry.get("author")).cloned(),
            hits.map(Vec::len)
        ),
        (Some(json!("Use a local store.")), Some(json!("writer")), Some(1))
    );
    Ok(())
}

#[test]
fn never_executes_or_answers_tools_call_notifications() -> TestResult {
    let temp = fixture()?;
    let mut notification = call(9, "memory_write", &write_args());
    notification.as_object_mut().ok_or("object required")?.remove("id");
    let replies = exchange(
        store(&temp, "writer")?,
        &[
            initialization("2025-11-25"),
            initialized(),
            notification,
            json!({"jsonrpc":"2.0","method":"unrecognized/notification"}),
            call(10, "memory_get", &json!({"key":"architecture"})),
        ],
    )?;
    assert_eq!(
        (replies.len(), replies.last().and_then(|reply| reply.pointer("/result/isError")).cloned()),
        (2, Some(json!(true)))
    );
    Ok(())
}

#[test]
fn reports_json_rpc_and_tool_errors_without_echoing_input() -> TestResult {
    let temp = fixture()?;
    let replies = exchange(
        store(&temp, "reader")?,
        &[
            initialization("2025-11-25"),
            initialized(),
            json!({"jsonrpc":"2.0","id":2,"method":"missing-secret-method"}),
            call(3, "memory_search", &json!({"limit":21})),
            call(4, "memory_get", &json!({"key":"missing"})),
            json!({"jsonrpc":"2.0","id":null,"method":"ping"}),
            json!({"jsonrpc":"2.0","id":1.5,"method":"ping"}),
        ],
    )?;
    let actual = replies
        .iter()
        .skip(1)
        .map(|reply| {
            (reply.pointer("/error/code").cloned(), reply.pointer("/result/isError").cloned())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        actual,
        vec![
            (Some(json!(-32601)), None),
            (Some(json!(-32602)), None),
            (None, Some(json!(true))),
            (Some(json!(-32600)), None),
            (Some(json!(-32600)), None)
        ]
    );
    assert!(!serde_json::to_string(&replies)?.contains("missing-secret-method"));
    Ok(())
}

#[test]
fn malformed_json_recovers_and_ping_works_before_initialization() -> TestResult {
    let temp = fixture()?;
    let mut output = Vec::new();
    serve(
        store(&temp, "reader")?,
        Cursor::new(
            b"not-json-secret\n{\"jsonrpc\":\"2.0\",\"id\":\"ping\",\"method\":\"ping\"}\n",
        ),
        &mut output,
    )?;
    let replies = String::from_utf8(output)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<Value>, _>>()?;
    assert_eq!(
        replies,
        vec![
            json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}),
            json!({"jsonrpc":"2.0","id":"ping","result":{}})
        ]
    );
    Ok(())
}

#[test]
fn oversized_unterminated_input_stops_after_bounded_read() -> TestResult {
    let temp = fixture()?;
    let mut source = Cursor::new(vec![b'x'; 2 * 1024 * 1024]);
    let mut output = Vec::new();
    let result = serve(store(&temp, "reader")?, &mut source, &mut output);
    assert_eq!(
        (result.err().map(|error| error.kind()), source.position(), output),
        (Some(io::ErrorKind::InvalidData), 1_048_577, Vec::new())
    );
    Ok(())
}

#[test]
fn accepts_a_line_at_the_size_limit() -> TestResult {
    let temp = fixture()?;
    let ping = json!({"jsonrpc":"2.0","id":1,"method":"ping"}).to_string();
    let line = format!("{ping}{}\n", " ".repeat(1_048_576 - ping.len()));
    let mut output = Vec::new();
    serve(store(&temp, "reader")?, Cursor::new(line), &mut output)?;
    assert_eq!(
        serde_json::from_slice::<Value>(&output)?,
        json!({"jsonrpc":"2.0","id":1,"result":{}})
    );
    Ok(())
}

#[test]
fn read_errors_propagate_without_json_noise() -> TestResult {
    let temp = fixture()?;
    let source = io::BufReader::new(FailedReader);
    let mut output = Vec::new();
    let result = serve(store(&temp, "reader")?, source, &mut output);
    assert_eq!(
        (result.err().map(|error| error.kind()), output),
        (Some(io::ErrorKind::BrokenPipe), Vec::new())
    );
    Ok(())
}

struct FailedReader;

impl Read for FailedReader {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "private input details"))
    }
}

#[test]
fn rejects_arguments_that_disagree_with_tool_schemas() -> TestResult {
    let temp = fixture()?;
    let replies = exchange(
        store(&temp, "reader")?,
        &[
            initialization("2025-11-25"),
            initialized(),
            call(2, "memory_get", &json!({"key":"architecture","revision":null})),
            call(3, "memory_get", &json!({"key":"architecture","revision":0})),
            call(4, "memory_search", &Value::Null),
            call(5, "memory_search", &json!({"query":"","limit":0})),
            call(6, "unknown_tool", &json!({})),
            call(
                7,
                "memory_write",
                &json!({"key":"architecture","expected_revision":0,"kind":"decision","title":"T","body":"B"}),
            ),
        ],
    )?;
    assert_eq!(
        replies
            .iter()
            .skip(1)
            .map(|reply| reply.pointer("/error/code").cloned())
            .collect::<Vec<_>>(),
        vec![Some(json!(-32602)); 6]
    );
    Ok(())
}

#[test]
fn invalid_initialized_notifications_do_not_enable_tools() -> TestResult {
    let temp = fixture()?;
    let replies = exchange(
        store(&temp, "reader")?,
        &[
            initialized(),
            initialization("2025-11-25"),
            json!({"jsonrpc":"2.0","method":"notifications/initialized","params":[]}),
            json!({"jsonrpc":"wrong","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            initialized(),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
            initialization("2025-11-25"),
        ],
    )?;
    assert_eq!(
        replies.iter().map(|reply| reply.pointer("/error/code").cloned()).collect::<Vec<_>>(),
        vec![None, Some(json!(-32002)), None, Some(json!(-32600))]
    );
    Ok(())
}

#[test]
fn write_failure_propagates_without_consuming_further_requests() -> TestResult {
    let temp = fixture()?;
    let input =
        [initialization("2025-11-25"), initialized(), call(2, "memory_write", &write_args())]
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
    let result = serve(store(&temp, "writer")?, Cursor::new(input), FailedWriter);
    assert_eq!(
        (result.err().map(|error| error.kind()), store(&temp, "reader")?.search("", 10)?.len()),
        (Some(io::ErrorKind::BrokenPipe), 0)
    );
    Ok(())
}

struct FailedWriter;

impl io::Write for FailedWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "private output details"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn status_filter_is_explicit_and_admin_tools_stay_absent() -> TestResult {
    let temp = fixture()?;
    let s = store(&temp, "test")?;
    s.write(serde_json::from_value(write_args())?)?;
    s.apply(s.preview(hub_memory::AdminAction::SetStatus {
        key: "architecture".into(),
        status: hub_memory::MemoryStatus::Archived,
    })?)?;
    let result = exchange(
        s,
        &[
            initialization("2025-11-25"),
            initialized(),
            call(2, "memory_search", &json!({})),
            call(3, "memory_search", &json!({"status":"all"})),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/list"}),
        ],
    )?;
    assert_eq!(result[1]["result"]["structuredContent"]["hits"], json!([]));
    assert_eq!(
        result[2]["result"]["structuredContent"]["hits"].as_array().ok_or("expected array")?.len(),
        1
    );
    assert_eq!(result[3]["result"]["tools"].as_array().ok_or("expected array")?.len(), 3);
    Ok(())
}

#[test]
fn direct_get_exposes_archived_status_and_available_revisions() -> TestResult {
    let temp = fixture()?;
    let opened = store(&temp, "test")?;
    opened.write(serde_json::from_value(write_args())?)?;
    opened.apply(opened.preview(hub_memory::AdminAction::SetStatus {
        key: "architecture".into(),
        status: hub_memory::MemoryStatus::Archived,
    })?)?;
    let result = exchange(
        opened,
        &[
            initialization("2025-11-25"),
            initialized(),
            call(2, "memory_get", &json!({"key":"architecture"})),
        ],
    )?;
    let payload = &result.get(1).ok_or("missing result")?["result"]["structuredContent"];
    assert_eq!(payload["status"], "archived");
    assert_eq!(payload["revisions"], json!([1]));
    assert_eq!(payload["revision"], 1);
    Ok(())
}

#[test]
fn all_backend_actors_read_bound_project_and_mcp_cannot_change_binding() -> TestResult {
    let temp = fixture()?;
    let source = temp.path().join("project");
    let target = temp.path().join("worktree");
    let base = temp.path().join("memory");
    std::fs::create_dir(&target)?;
    exchange(
        store(&temp, "codex")?,
        &[initialization("2025-11-25"), initialized(), call(2, "memory_write", &write_args())],
    )?;
    hub_memory::BindingPreview::prepare(&base, &source, &target, hub_memory::BindingMode::Share)?
        .apply()?;
    for actor in ["claude", "codex", "qwen", "hermes", "opencode", "external"] {
        let replies = exchange(
            Store::open(&base, &target, actor)?,
            &[
                initialization("2025-11-25"),
                initialized(),
                call(2, "memory_get", &json!({"key":"architecture"})),
                json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
            ],
        )?;
        let entry = replies
            .iter()
            .find(|reply| reply.get("id") == Some(&json!(2)))
            .and_then(|reply| reply.pointer("/result/structuredContent"));
        assert_eq!(entry.and_then(|e| e.get("revision")), Some(&json!(1)));
        let tools = replies
            .iter()
            .find(|reply| reply.get("id") == Some(&json!(3)))
            .and_then(|reply| reply.pointer("/result/tools"))
            .and_then(Value::as_array)
            .ok_or("missing tools")?;
        assert_eq!(tools.len(), 3);
        assert!(tools.iter().all(|tool| matches!(
            tool.get("name").and_then(Value::as_str),
            Some("memory_get" | "memory_write" | "memory_search")
        )));
    }
    Ok(())
}
