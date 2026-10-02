# Фаза 2: hub-claude — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Крейт `hub-claude` — разговор с `claude` CLI по протоколу stream-json: разбор сообщений, ответы на control-запросы (подтверждения, вопросы, in-process MCP `send_file`), отслеживание фоновых задач, запуск процесса и проверка версии CLI.

**Architecture:** Чистые модули (`wire`, `outgoing`, `permissions`, `mcp`, `activity`, `tracker`, разбор версии) тестируются без процессов. Оболочка `session::converse` работает поверх любых `AsyncRead`/`AsyncWrite`, поэтому интеграционные тесты гоняют её через `tokio::io::duplex` с поддельным CLI внутри теста. `backend::ClaudeBackend` только запускает процесс и подключает его потоки к `converse`. Эталон поведения — `agent-hub/src/agent_hub/backends/claude.py` и записанные транскрипты живого CLI 2.1.287 в `crates/hub-claude/tests/fixtures/`.

**Tech Stack:** Rust 1.96, tokio 1.53 (process, io-util, time, sync, macros), tokio-util 0.7 (codec, CancellationToken), futures 0.3, serde_json 1, base64 0.23, uuid 1.26, which 8, tracing 0.1, insta 1.48.

**Spec:** `docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md`

## Global Constraints

- Ограничения фазы 1 (`docs/superpowers/plans/2026-10-02-phase-1-hub-core.md`, раздел Global Constraints) действуют без изменений.
- Минимальная версия CLI — `2.1.280`; таймауты: `initialize` 60 с, `claude --version` 10 с, завершение процесса после закрытия stdin 5 с, окно settling 30 с.
- Предел строки stdout CLI — 16 МБ.
- Аргументы CLI: `--output-format stream-json --verbose --input-format stream-json --permission-prompt-tool stdio --permission-mode <mode> [--model M] [--max-budget-usd B] [--resume=<id>] --setting-sources=user,project,local --replay-user-messages --mcp-config {"mcpServers":{"agent-hub":{"type":"sdk","name":"agent-hub"}}}`; окружение `CLAUDE_CODE_ENTRYPOINT=sdk-rs`; на Windows `CREATE_NO_WINDOW`.
- Тексты для пользователя — как в Python-версии: «Claude завершился без результата», `"{subtype}: {result or 'ошибка выполнения'}"`, «Файл {path} отправлен пользователю».
- Неизвестные типы сообщений CLI (`command_lifecycle`, `rate_limit_event`, `system/thinking_tokens`, …) не ломают разговор.

## Review Focus

1. Невалидная строка stdout (не JSON, не UTF-8, обрезанная) — разговор не падает: не-JSON пропускается с предупреждением, ошибка кодека завершает разговор событием `Failed`. Тесты: `unparsable_line_is_skipped`, `invalid_utf8_fails_the_conversation`.
2. CLI умирает посреди ожидания подтверждения — висящий обработчик снимается, паники нет, разговор завершается. Тест: `cli_exit_without_result_fails`, `control_cancel_request_abandons_pending_approval`.
3. `/stop` во время ожидания человека — CLI получает `interrupt`, stdin закрывается, ответ на висящий запрос не пишется. Тест: `cancel_sends_interrupt_and_ends`.
4. Сообщение пользователя приходит ровно когда разговор закрывается — оно не теряется: `converse` возвращает inbox вызывающему. Тест: `leftover_prompt_is_returned`.
5. CLI отклоняет `initialize` или молчит — `Failed` с понятным текстом, без зависания. Тест: `initialize_error_fails`.

## Отклонения от спеки (вносятся в спеку в Task 1)

- Вместо трейта `AgentBackend` — `ClaudeBackend::run(...) -> mpsc::Receiver<Prompt>`; выбор бэкенда enum-диспетчеризацией делает `hub-telegram`. Возврат inbox нужен, чтобы актор `Hub` не терял сообщения, пришедшие при закрытии разговора.
- Вместо отдельного бинарника «фейкового CLI» — поддельный CLI внутри интеграционных тестов поверх `tokio::io::duplex`; живой CLI проверяется тестом `#[ignore]` `live_cli_round_trip` (запуск: `AGENT_HUB_LIVE_CLI=1 cargo test -p hub-claude -- --ignored`).
- `summarize_tool_input` для неизвестных инструментов выводит компактный JSON (`{"q":"привет"}`), а не `json.dumps` с пробелами.

---

### Task 1: Крейт, разбор сообщений CLI

**Files:**
- Modify: `Cargo.toml` (members, workspace.dependencies)
- Create: `crates/hub-claude/Cargo.toml`, `crates/hub-claude/src/lib.rs`, `crates/hub-claude/src/wire.rs`
- Add (already captured): `crates/hub-claude/tests/fixtures/{approval,question,sendfile,background}.jsonl`
- Modify: `docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md` (раздел «hub-claude: протокол CLI → Контракт» и «Тестирование»)

**Interfaces:**
- Produces: `wire::Incoming::{System(System), Assistant(Assistant), User(User), Result(Outcome), ControlRequest { request_id, request }, ControlResponse { response }, ControlCancelRequest { request_id }, Other}`, `wire::System::{Init { session_id }, TaskStarted { task_id, description, is_backgrounded }, TaskUpdated { task_id, patch }, TaskNotification { task_id }, Other}`, `wire::TaskPatch { status }`, `wire::Assistant { message: AssistantBody { content: Vec<Block> }, session_id }`, `wire::Block::{Text { text }, ToolUse { name, input }, Other}`, `wire::User { uuid }`, `wire::Outcome { subtype, is_error, num_turns, session_id, total_cost_usd: Option<Number>, result, origin: Option<Value> }` + `Outcome::injected()`, `wire::ControlRequest::{CanUseTool { tool_name, input }, McpMessage { server_name, message }, Other}`, `wire::ControlReply::{Success { request_id }, Error { request_id, error }}`, `wire::parse_line(&str) -> Result<Incoming, serde_json::Error>`.

- [ ] **Step 1: Workspace и манифест**

`Cargo.toml` — `members = ["crates/hub-core", "crates/hub-claude"]`, в `[workspace.dependencies]` добавить:

```toml
hub-core = { path = "crates/hub-core" }
base64 = "0.23"
futures = { version = "0.3", default-features = false, features = ["std"] }
insta = "1.48"
tokio = "1.53"
tokio-util = "0.7"
tracing = "0.1"
uuid = "1.26"
which = "8.0"
```

`crates/hub-claude/Cargo.toml`:

```toml
[package]
name = "hub-claude"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
base64.workspace = true
futures.workspace = true
hub-core.workspace = true
rust_decimal.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["io-util", "macros", "process", "rt", "sync", "time"] }
tokio-util = { workspace = true, features = ["codec"] }
tracing.workspace = true
uuid = { workspace = true, features = ["v4"] }
which.workspace = true

[dev-dependencies]
insta.workspace = true
rstest.workspace = true
tokio = { workspace = true, features = ["io-util", "macros", "process", "rt", "rt-multi-thread", "sync", "time"] }

[lints]
workspace = true
```

`crates/hub-claude/src/lib.rs`:

```rust
pub mod wire;
```

- [ ] **Step 2: Падающие тесты разбора**

`crates/hub-claude/src/wire.rs`:

```rust
#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    const FIXTURES: [(&str, &str); 4] = [
        ("approval", include_str!("../tests/fixtures/approval.jsonl")),
        ("question", include_str!("../tests/fixtures/question.jsonl")),
        ("sendfile", include_str!("../tests/fixtures/sendfile.jsonl")),
        ("background", include_str!("../tests/fixtures/background.jsonl")),
    ];

    fn parse(value: &serde_json::Value) -> Incoming {
        parse_line(&value.to_string()).unwrap()
    }

    #[test]
    fn init_carries_session() {
        assert_eq!(
            parse(&json!({"type": "system", "subtype": "init", "session_id": "s", "tools": []})),
            Incoming::System(System::Init { session_id: "s".to_owned() })
        );
    }

    #[test]
    fn task_messages_are_parsed() {
        assert_eq!(
            parse(&json!({"type": "system", "subtype": "task_started", "task_id": "t",
                          "description": "sleep", "is_backgrounded": true})),
            Incoming::System(System::TaskStarted {
                task_id: "t".to_owned(),
                description: "sleep".to_owned(),
                is_backgrounded: Some(true),
            })
        );
        assert_eq!(
            parse(&json!({"type": "system", "subtype": "task_updated", "task_id": "t",
                          "patch": {"status": "completed", "end_time": 1}})),
            Incoming::System(System::TaskUpdated {
                task_id: "t".to_owned(),
                patch: TaskPatch { status: Some("completed".to_owned()) },
            })
        );
    }

    #[test]
    fn assistant_blocks_are_parsed() {
        let message = parse(&json!({"type": "assistant", "session_id": "s", "message": {
            "content": [
                {"type": "thinking", "thinking": "", "signature": ""},
                {"type": "text", "text": "hi"},
                {"type": "tool_use", "id": "t", "name": "Bash", "input": {"command": "ls"}}
            ]}}));
        assert_eq!(
            message,
            Incoming::Assistant(Assistant {
                message: AssistantBody {
                    content: vec![
                        Block::Other,
                        Block::Text { text: "hi".to_owned() },
                        Block::ToolUse { name: "Bash".to_owned(), input: json!({"command": "ls"}) },
                    ]
                },
                session_id: Some("s".to_owned()),
            })
        );
    }

    #[rstest]
    #[case(json!({"kind": "task-notification"}), true)]
    #[case(json!({"kind": "human"}), false)]
    #[case(json!("garbage"), false)]
    fn injected_results_are_recognised(#[case] origin: serde_json::Value, #[case] injected: bool) {
        let Incoming::Result(outcome) = parse(&json!({"type": "result", "subtype": "success",
            "is_error": false, "num_turns": 1, "session_id": "s", "origin": origin}))
        else {
            panic!("not a result");
        };
        assert_eq!(outcome.injected(), injected);
    }

    #[test]
    fn control_messages_are_parsed() {
        assert_eq!(
            parse(&json!({"type": "control_request", "request_id": "r",
                          "request": {"subtype": "can_use_tool", "tool_name": "Write",
                                      "input": {"file_path": "a"}, "tool_use_id": "t"}})),
            Incoming::ControlRequest {
                request_id: "r".to_owned(),
                request: ControlRequest::CanUseTool {
                    tool_name: "Write".to_owned(),
                    input: json!({"file_path": "a"}),
                },
            }
        );
        assert_eq!(
            parse(&json!({"type": "control_response",
                          "response": {"subtype": "error", "request_id": "r", "error": "no"}})),
            Incoming::ControlResponse {
                response: ControlReply::Error { request_id: "r".to_owned(), error: "no".to_owned() },
            }
        );
        assert_eq!(
            parse(&json!({"type": "control_cancel_request", "request_id": "r"})),
            Incoming::ControlCancelRequest { request_id: "r".to_owned() }
        );
    }

    #[rstest]
    #[case(json!({"type": "rate_limit_event", "rate_limit_info": {}}), Incoming::Other)]
    #[case(json!({"type": "system", "subtype": "thinking_tokens"}), Incoming::System(System::Other))]
    #[case(json!({"type": "control_request", "request_id": "r", "request": {"subtype": "hook_callback"}}),
           Incoming::ControlRequest { request_id: "r".to_owned(), request: ControlRequest::Other })]
    fn unknown_kinds_are_tolerated(#[case] value: serde_json::Value, #[case] expected: Incoming) {
        assert_eq!(parse(&value), expected);
    }

    #[test]
    fn every_recorded_line_parses_and_known_kinds_are_recognised() {
        for (name, text) in FIXTURES {
            for line in text.lines() {
                let message = parse_line(line).unwrap_or_else(|error| panic!("{name}: {error}: {line}"));
                let kind: serde_json::Value = serde_json::from_str(line).unwrap();
                let known = matches!(
                    kind["type"].as_str(),
                    Some("assistant" | "user" | "result" | "control_request" | "control_response")
                );
                assert!(!known || message != Incoming::Other, "{name}: {line}");
            }
        }
    }
}
```

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p hub-claude --lib`
Expected: FAIL — `cannot find type Incoming`.

- [ ] **Step 4: Реализовать `wire.rs`**

Над тестовым модулем:

```rust
//! Messages the agent CLI prints on stdout (stream-json), parsed into closed types.

use serde::Deserialize;
use serde_json::{Number, Value};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Incoming {
    System(System),
    Assistant(Assistant),
    User(User),
    Result(Outcome),
    ControlRequest { request_id: String, request: ControlRequest },
    ControlResponse { response: ControlReply },
    ControlCancelRequest { request_id: String },
    /// Kinds this hub does not act on: rate limits, command lifecycle, stream events, …
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum System {
    Init {
        session_id: String,
    },
    TaskStarted {
        task_id: String,
        #[serde(default)]
        description: String,
        #[serde(default)]
        is_backgrounded: Option<bool>,
    },
    TaskUpdated {
        #[serde(default)]
        task_id: String,
        #[serde(default)]
        patch: TaskPatch,
    },
    TaskNotification {
        task_id: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct TaskPatch {
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Assistant {
    pub message: AssistantBody,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AssistantBody {
    #[serde(default)]
    pub content: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    ToolUse {
        name: String,
        #[serde(default)]
        input: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct User {
    #[serde(default)]
    pub uuid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Outcome {
    pub subtype: String,
    pub is_error: bool,
    #[serde(default)]
    pub num_turns: u32,
    pub session_id: String,
    #[serde(default)]
    pub total_cost_usd: Option<Number>,
    #[serde(default)]
    pub result: Option<String>,
    // Kept loose: a malformed origin must not make the whole result unreadable.
    #[serde(default)]
    pub origin: Option<Value>,
}

impl Outcome {
    /// A turn the CLI started on its own, e.g. to report a finished background task.
    #[must_use]
    pub fn injected(&self) -> bool {
        self.origin
            .as_ref()
            .and_then(|origin| origin.get("kind"))
            .and_then(Value::as_str)
            .is_some_and(|kind| kind != "human")
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlRequest {
    CanUseTool {
        tool_name: String,
        #[serde(default)]
        input: Value,
    },
    McpMessage {
        server_name: String,
        message: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlReply {
    Success {
        request_id: String,
    },
    Error {
        request_id: String,
        #[serde(default)]
        error: String,
    },
}

pub fn parse_line(line: &str) -> Result<Incoming, serde_json::Error> {
    serde_json::from_str(line)
}
```

- [ ] **Step 5: Прогнать тесты и линтеры**

Run: `cargo test -p hub-claude --lib && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 6: Уточнить спеку**

В разделе «hub-claude: протокол CLI → Контракт» заменить блок с трейтом `AgentBackend` на:

```markdown
```rust
impl ClaudeBackend {
    pub async fn run(
        &self,
        session: &TopicSession,
        prompt: Prompt,
        inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation, // channel, events, cancel, limits
    ) -> mpsc::Receiver<Prompt>;
}
```

- Inbox возвращается вызывающему: сообщения, пришедшие, пока разговор закрывался, не теряются —
  актор `Hub` проверяет его и при необходимости запускает разговор снова.
- Выбор бэкенда — enum-диспетчеризация в `hub-telegram`: `enum Backend { Claude(ClaudeBackend) }`.
```

и в «Тестирование» заменить фразу про тестовый бинарник фейкового CLI на: «интеграционные тесты `session` с поддельным CLI внутри теста поверх `tokio::io::duplex`; живой CLI — тест `#[ignore]` `live_cli_round_trip`».

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/hub-claude docs
git commit -m "hub-claude: разбор сообщений CLI и записанные транскрипты"
```

---

### Task 2: Исходящие сообщения и решения по разрешениям

**Files:**
- Create: `crates/hub-claude/src/outgoing.rs`, `crates/hub-claude/src/permissions.rs`
- Modify: `crates/hub-claude/src/lib.rs`

**Interfaces:**
- Consumes: `hub_core::domain::{Prompt, SessionId, Decision, Question, QuestionOption, QuestionAnswer, QuestionsOutcome, Selection, ToolRequest}`, `hub_core::settings::ClaudeSettings`, `hub_core::render::truncate`.
- Produces: `outgoing::{MCP_SERVER, ENTRYPOINT, user_message(&Prompt, &str) -> Value, control_request(&str, Value) -> Value, initialize() -> Value, interrupt() -> Value, control_success(&str, Value) -> Value, control_error(&str, &str) -> Value, cli_args(&ClaudeSettings, Option<&SessionId>) -> Vec<String>}`; `permissions::{ASK_USER_QUESTION, SEND_FILE, SEND_FILE_TOOL, TOOL_SUMMARY_LIMIT, Route::{Allow, Ask(Vec<Question>), Request(ToolRequest)}, route(&str, &Value) -> Route, parse_questions(&Value) -> Option<Vec<Question>>, summarize_tool_input(&str, &Value) -> String, allow(Value) -> Value, deny(&str) -> Value, decided(Value, Decision) -> Value, answered(Value, QuestionsOutcome) -> Value}`.

- [ ] **Step 1: Падающие тесты**

`crates/hub-claude/src/outgoing.rs`:

```rust
#[cfg(test)]
mod tests {
    use hub_core::domain::{Image, ImageMediaType};
    use hub_core::settings::{Draft, PermissionMode};
    use serde_json::json;

    use super::*;

    fn settings(draft: Draft) -> ClaudeSettings {
        let home = if cfg!(windows) { r"C:\Users\me" } else { "/home/me" };
        Draft {
            token: "1:a".to_owned(),
            chat: "-1".to_owned(),
            users: "1".to_owned(),
            workspace_root: "~/p".to_owned(),
            ..draft
        }
        .parse(std::path::Path::new(home))
        .unwrap()
        .claude
    }

    #[test]
    fn user_message_puts_images_before_text() {
        let image = Image { media: ImageMediaType::Jpeg, data: vec![0xff, 0xd8] };
        let prompt = Prompt::new("что на фото?".to_owned(), vec![image]).unwrap();
        assert_eq!(
            user_message(&prompt, "p1"),
            json!({
                "type": "user",
                "message": {"role": "user", "content": [
                    {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": "/9g="}},
                    {"type": "text", "text": "что на фото?"}
                ]},
                "parent_tool_use_id": null,
                "uuid": "p1"
            })
        );
    }

    #[test]
    fn user_message_without_text_has_only_images() {
        let image = Image { media: ImageMediaType::Png, data: vec![1] };
        let prompt = Prompt::new(String::new(), vec![image]).unwrap();
        let content = user_message(&prompt, "p").pointer("/message/content").cloned().unwrap();
        assert_eq!(content.as_array().map(Vec::len), Some(1));
        assert_eq!(content.pointer("/0/type"), Some(&json!("image")));
    }

    #[test]
    fn control_lines_have_the_cli_shape() {
        assert_eq!(
            control_request("r", initialize()),
            json!({"type": "control_request", "request_id": "r", "request": {"subtype": "initialize", "hooks": null}})
        );
        assert_eq!(
            control_success("r", json!({"x": 1})),
            json!({"type": "control_response", "response": {"subtype": "success", "request_id": "r", "response": {"x": 1}}})
        );
        assert_eq!(
            control_error("r", "no"),
            json!({"type": "control_response", "response": {"subtype": "error", "request_id": "r", "error": "no"}})
        );
    }

    #[test]
    fn minimal_cli_args() {
        assert_eq!(
            cli_args(&settings(Draft::default()), None),
            [
                "--output-format", "stream-json", "--verbose", "--input-format", "stream-json",
                "--permission-prompt-tool", "stdio", "--permission-mode", "default",
                "--setting-sources=user,project,local", "--replay-user-messages", "--mcp-config",
                r#"{"mcpServers":{"agent-hub":{"type":"sdk","name":"agent-hub"}}}"#,
            ]
        );
    }

    #[test]
    fn optional_cli_args() {
        let draft = Draft {
            model: "claude-opus-5-5".to_owned(),
            budget: "2.50".to_owned(),
            permission_mode: PermissionMode::AcceptEdits,
            ..Draft::default()
        };
        let args = cli_args(&settings(draft), SessionId::parse("abc").as_ref());
        let joined = args.join(" ");
        assert!(joined.contains("--permission-mode acceptEdits"));
        assert!(joined.contains("--model claude-opus-5-5"));
        assert!(joined.contains("--max-budget-usd 2.50"));
        assert!(args.contains(&"--resume=abc".to_owned()));
    }
}
```

`crates/hub-claude/src/permissions.rs`:

```rust
#[cfg(test)]
mod tests {
    use hub_core::domain::Denied;
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    fn ask_input() -> Value {
        json!({"questions": [{
            "question": "Какой формат?",
            "header": "Формат",
            "options": [
                {"label": "Кратко", "description": "Только суть"},
                {"label": "Подробно", "description": "С примерами"}
            ],
            "multiSelect": false
        }]})
    }

    fn ask_question() -> Question {
        Question::new(
            "Какой формат?".to_owned(),
            "Формат".to_owned(),
            vec![
                QuestionOption { label: "Кратко".to_owned(), description: "Только суть".to_owned() },
                QuestionOption { label: "Подробно".to_owned(), description: "С примерами".to_owned() },
            ],
            Selection::Single,
        )
        .unwrap()
    }

    #[test]
    fn questions_are_parsed() {
        assert_eq!(parse_questions(&ask_input()), Some(vec![ask_question()]));
    }

    #[test]
    fn multi_select_is_recognised() {
        let input = json!({"questions": [{"question": "q", "options": [], "multiSelect": true}]});
        let questions = parse_questions(&input).unwrap();
        assert_eq!(questions.first().map(Question::selection), Some(Selection::Multiple));
    }

    #[rstest]
    #[case(json!({}))]
    #[case(json!({"questions": []}))]
    #[case(json!({"questions": "x"}))]
    #[case(json!({"questions": [{"header": "h", "options": []}]}))]
    #[case(json!({"questions": [{"question": "q", "options": [{"description": "no label"}]}]}))]
    #[case(json!({"questions": [{"question": "q", "header": 1}]}))]
    fn malformed_questions_are_rejected(#[case] input: Value) {
        assert_eq!(parse_questions(&input), None);
    }

    #[test]
    fn questions_route_to_ask() {
        assert_eq!(route(ASK_USER_QUESTION, &ask_input()), Route::Ask(vec![ask_question()]));
    }

    #[test]
    fn malformed_question_falls_back_to_approval() {
        assert_eq!(
            route(ASK_USER_QUESTION, &json!({"questions": []})),
            Route::Request(ToolRequest {
                tool: ASK_USER_QUESTION.to_owned(),
                summary: r#"{"questions":[]}"#.to_owned()
            })
        );
    }

    #[test]
    fn send_file_is_allowed_without_asking() {
        assert_eq!(route(SEND_FILE_TOOL, &json!({"path": "a.pdf"})), Route::Allow);
    }

    #[test]
    fn other_tools_go_through_approval() {
        assert_eq!(
            route("Bash", &json!({"command": "ls"})),
            Route::Request(ToolRequest { tool: "Bash".to_owned(), summary: "ls".to_owned() })
        );
    }

    #[rstest]
    #[case("Read", json!({"file_path": "/a/b.py", "limit": 10}), "/a/b.py")]
    #[case("NotebookEdit", json!({"notebook_path": "n.ipynb"}), "n.ipynb")]
    #[case("WebSearch", json!({"query": "rust"}), "rust")]
    #[case("mcp__x", json!({"q": "привет"}), r#"{"q":"привет"}"#)]
    #[case("Bash", json!({"command": 1}), r#"{"command":1}"#)]
    fn tool_input_summaries(#[case] tool: &str, #[case] input: Value, #[case] expected: &str) {
        assert_eq!(summarize_tool_input(tool, &input), expected);
    }

    #[test]
    fn summary_is_truncated() {
        let input = json!({"command": "x".repeat(TOOL_SUMMARY_LIMIT * 2)});
        assert_eq!(summarize_tool_input("Bash", &input).chars().count(), TOOL_SUMMARY_LIMIT);
    }

    #[test]
    fn answers_are_returned_as_tool_input() {
        let outcome = QuestionsOutcome::Answered(vec![QuestionAnswer {
            question: "Какой формат?".to_owned(),
            answer: "Кратко".to_owned(),
        }]);
        let mut expected = ask_input();
        expected["answers"] = json!({"Какой формат?": "Кратко"});
        assert_eq!(
            answered(ask_input(), outcome),
            json!({"behavior": "allow", "updatedInput": expected})
        );
    }

    #[test]
    fn declined_questions_deny_the_tool() {
        assert_eq!(
            answered(ask_input(), QuestionsOutcome::Denied(Denied::new("нет"))),
            json!({"behavior": "deny", "message": "нет"})
        );
    }

    #[test]
    fn decisions_map_to_behaviors() {
        let input = json!({"command": "ls"});
        assert_eq!(
            decided(input.clone(), Decision::Allowed),
            json!({"behavior": "allow", "updatedInput": {"command": "ls"}})
        );
        assert_eq!(
            decided(input, Decision::Denied(Denied::new("нет"))),
            json!({"behavior": "deny", "message": "нет"})
        );
    }
}
```

`crates/hub-claude/src/lib.rs`:

```rust
pub mod outgoing;
pub mod permissions;
pub mod wire;
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-claude --lib`
Expected: FAIL — функции не определены.

- [ ] **Step 3: Реализовать `outgoing.rs`**

```rust
//! Lines this hub writes to the agent CLI's stdin, and the CLI's command line.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hub_core::domain::{Prompt, SessionId};
use hub_core::settings::ClaudeSettings;
use serde_json::{Value, json};

pub const MCP_SERVER: &str = "agent-hub";
const MCP_CONFIG: &str = r#"{"mcpServers":{"agent-hub":{"type":"sdk","name":"agent-hub"}}}"#;
// Lets the CLI tell SDK hosts apart.
pub const ENTRYPOINT: (&str, &str) = ("CLAUDE_CODE_ENTRYPOINT", "sdk-rs");

/// Stream-json user message with images first, as the Messages API recommends.
#[must_use]
pub fn user_message(prompt: &Prompt, uuid: &str) -> Value {
    let images = prompt.images().iter().map(|image| {
        json!({"type": "image", "source": {
            "type": "base64",
            "media_type": image.media.mime(),
            "data": STANDARD.encode(&image.data),
        }})
    });
    let text = (!prompt.text().trim().is_empty())
        .then(|| json!({"type": "text", "text": prompt.text()}));
    let content: Vec<Value> = images.chain(text).collect();
    json!({
        "type": "user",
        "message": {"role": "user", "content": content},
        "parent_tool_use_id": null,
        "uuid": uuid,
    })
}

#[must_use]
pub fn control_request(request_id: &str, request: Value) -> Value {
    json!({"type": "control_request", "request_id": request_id, "request": request})
}

#[must_use]
pub fn initialize() -> Value {
    json!({"subtype": "initialize", "hooks": null})
}

#[must_use]
pub fn interrupt() -> Value {
    json!({"subtype": "interrupt"})
}

#[must_use]
pub fn control_success(request_id: &str, response: Value) -> Value {
    json!({"type": "control_response", "response": {
        "subtype": "success", "request_id": request_id, "response": response,
    }})
}

#[must_use]
pub fn control_error(request_id: &str, error: &str) -> Value {
    json!({"type": "control_response", "response": {
        "subtype": "error", "request_id": request_id, "error": error,
    }})
}

/// The operator's own setup (CLAUDE.md, permission allowlists, skills, MCP servers) is loaded,
/// so a topic behaves like the CLI started in the same directory.
#[must_use]
pub fn cli_args(settings: &ClaudeSettings, session: Option<&SessionId>) -> Vec<String> {
    let mut args: Vec<String> = [
        "--output-format",
        "stream-json",
        "--verbose",
        "--input-format",
        "stream-json",
        "--permission-prompt-tool",
        "stdio",
        "--permission-mode",
        settings.permission_mode.wire(),
    ]
    .map(str::to_owned)
    .into();
    if let Some(model) = &settings.model {
        args.extend(["--model".to_owned(), model.clone()]);
    }
    if let Some(budget) = settings.budget {
        args.extend(["--max-budget-usd".to_owned(), budget.amount().to_string()]);
    }
    if let Some(session) = session {
        args.push(format!("--resume={}", session.as_str()));
    }
    // Replay echoes each prompt when a turn takes it in: prompts sent mid-turn are merged into
    // that turn, so counting results cannot tell when all prompts are answered.
    args.extend(
        ["--setting-sources=user,project,local", "--replay-user-messages", "--mcp-config", MCP_CONFIG]
            .map(str::to_owned),
    );
    args
}
```

- [ ] **Step 4: Реализовать `permissions.rs`**

```rust
//! `can_use_tool` decisions: clarifying questions go to the human as questions, the rest as
//! approvals; a question input that cannot be parsed still reaches the human as an approval.

use hub_core::domain::{
    Decision, Question, QuestionAnswer, QuestionOption, QuestionsOutcome, Selection, ToolRequest,
};
use hub_core::render::truncate;
use serde_json::{Map, Value, json};

pub const ASK_USER_QUESTION: &str = "AskUserQuestion";
pub const SEND_FILE: &str = "send_file";
pub const SEND_FILE_TOOL: &str = "mcp__agent-hub__send_file";
pub const TOOL_SUMMARY_LIMIT: usize = 600;

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Allow,
    Ask(Vec<Question>),
    Request(ToolRequest),
}

#[must_use]
pub fn route(tool: &str, input: &Value) -> Route {
    if tool == SEND_FILE_TOOL {
        // Only reaches the user's own chat, which already sees everything the agent prints.
        return Route::Allow;
    }
    if tool == ASK_USER_QUESTION
        && let Some(questions) = parse_questions(input)
    {
        return Route::Ask(questions);
    }
    Route::Request(ToolRequest { tool: tool.to_owned(), summary: summarize_tool_input(tool, input) })
}

#[must_use]
pub fn parse_questions(input: &Value) -> Option<Vec<Question>> {
    let raw = input.get("questions")?.as_array()?;
    if raw.is_empty() {
        return None;
    }
    raw.iter().map(parse_question).collect()
}

fn parse_question(raw: &Value) -> Option<Question> {
    let object = raw.as_object()?;
    let text = object.get("question")?.as_str()?;
    let header = match object.get("header") {
        None => "",
        Some(header) => header.as_str()?,
    };
    let options = match object.get("options") {
        None => Vec::new(),
        Some(options) => options.as_array()?.iter().map(parse_option).collect::<Option<_>>()?,
    };
    let selection = match object.get("multiSelect") {
        Some(Value::Bool(true)) => Selection::Multiple,
        Some(_) | None => Selection::Single,
    };
    Question::new(text.to_owned(), header.to_owned(), options, selection)
}

fn parse_option(raw: &Value) -> Option<QuestionOption> {
    let object = raw.as_object()?;
    let label = object.get("label")?.as_str().filter(|label| !label.trim().is_empty())?;
    let description = match object.get("description") {
        None => "",
        Some(description) => description.as_str()?,
    };
    Some(QuestionOption { label: label.to_owned(), description: description.to_owned() })
}

/// One human-readable line for the most common tools.
#[must_use]
pub fn summarize_tool_input(tool: &str, input: &Value) -> String {
    let key = match tool {
        "Bash" => Some("command"),
        "Read" | "Write" | "Edit" | "MultiEdit" => Some("file_path"),
        "NotebookEdit" => Some("notebook_path"),
        "Glob" | "Grep" => Some("pattern"),
        "WebFetch" => Some("url"),
        "WebSearch" => Some("query"),
        _ => None,
    };
    let text = key
        .and_then(|key| input.get(key))
        .and_then(Value::as_str)
        .map_or_else(|| input.to_string(), str::to_owned);
    truncate(&text, TOOL_SUMMARY_LIMIT)
}

#[must_use]
pub fn allow(input: Value) -> Value {
    json!({"behavior": "allow", "updatedInput": input})
}

#[must_use]
pub fn deny(reason: &str) -> Value {
    json!({"behavior": "deny", "message": reason})
}

#[must_use]
pub fn decided(input: Value, decision: Decision) -> Value {
    match decision {
        Decision::Allowed => allow(input),
        Decision::Denied(denied) => deny(&denied.reason),
    }
}

/// The CLI reads the answers from the tool input and hands them to the model.
#[must_use]
pub fn answered(input: Value, outcome: QuestionsOutcome) -> Value {
    match outcome {
        QuestionsOutcome::Answered(answers) => allow(with_answers(input, &answers)),
        QuestionsOutcome::Denied(denied) => deny(&denied.reason),
    }
}

fn with_answers(input: Value, answers: &[QuestionAnswer]) -> Value {
    let Value::Object(mut object) = input else {
        return input;
    };
    let answers: Map<String, Value> = answers
        .iter()
        .map(|answer| (answer.question.clone(), Value::String(answer.answer.clone())))
        .collect();
    object.insert("answers".to_owned(), Value::Object(answers));
    Value::Object(object)
}
```

- [ ] **Step 5: Прогнать тесты и линтеры**

Run: `cargo test -p hub-claude --lib && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-claude
git commit -m "hub-claude: исходящие сообщения и решения по разрешениям"
```

---

### Task 3: In-process MCP-сервер `agent-hub`

**Files:**
- Create: `crates/hub-claude/src/mcp.rs`
- Modify: `crates/hub-claude/src/lib.rs` (`pub mod mcp;`)

**Interfaces:**
- Consumes: `outgoing::MCP_SERVER`, `permissions::SEND_FILE`, `hub_core::domain::{FileDelivery, OutgoingFile}`.
- Produces: `mcp::McpStep::{Reply(Value), SendFile { id: Value, file: OutgoingFile }}`, `mcp::handle(&str, &Value) -> McpStep`, `mcp::send_file_reply(Value, &str, FileDelivery) -> Value`.

- [ ] **Step 1: Падающие тесты**

`crates/hub-claude/src/mcp.rs`:

```rust
#[cfg(test)]
mod tests {
    use hub_core::domain::Denied;
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    fn reply(step: McpStep) -> Value {
        match step {
            McpStep::Reply(reply) => reply,
            McpStep::SendFile { .. } => panic!("expected a reply"),
        }
    }

    #[test]
    fn initialize_echoes_the_protocol_version() {
        let message = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                             "params": {"protocolVersion": "2025-11-25", "capabilities": {}}});
        let answer = reply(handle(MCP_SERVER, &message));
        assert_eq!(answer["id"], json!(0));
        assert_eq!(answer.pointer("/result/protocolVersion"), Some(&json!("2025-11-25")));
        assert_eq!(answer.pointer("/result/serverInfo/name"), Some(&json!("agent-hub")));
        assert_eq!(answer.pointer("/result/capabilities/tools"), Some(&json!({})));
    }

    #[test]
    fn notifications_are_acknowledged() {
        let message = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert_eq!(reply(handle(MCP_SERVER, &message)), json!({"jsonrpc": "2.0", "result": {}}));
    }

    #[test]
    fn tools_list_offers_send_file() {
        let answer = reply(handle(MCP_SERVER, &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})));
        assert_eq!(answer.pointer("/result/tools/0/name"), Some(&json!("send_file")));
        assert_eq!(answer.pointer("/result/tools/0/inputSchema/required"), Some(&json!(["path"])));
    }

    #[test]
    fn send_file_call_is_delegated() {
        let message = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                             "params": {"name": "send_file", "arguments": {"path": "out/r.pdf", "caption": "Отчёт"}}});
        assert_eq!(
            handle(MCP_SERVER, &message),
            McpStep::SendFile {
                id: json!(2),
                file: OutgoingFile { path: "out/r.pdf".to_owned(), caption: "Отчёт".to_owned() }
            }
        );
    }

    #[rstest]
    #[case(json!({}))]
    #[case(json!({"path": ""}))]
    #[case(json!({"path": 1}))]
    #[case(json!({"path": "a", "caption": 2}))]
    fn malformed_send_file_arguments_are_a_tool_error(#[case] arguments: Value) {
        let message = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                             "params": {"name": "send_file", "arguments": arguments}});
        let answer = reply(handle(MCP_SERVER, &message));
        assert_eq!(answer.pointer("/result/isError"), Some(&json!(true)));
    }

    #[rstest]
    #[case("other-server", json!({"jsonrpc": "2.0", "id": 4, "method": "tools/list"}), -32601)]
    #[case(MCP_SERVER, json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}), -32601)]
    #[case(MCP_SERVER, json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "rm"}}), -32602)]
    fn unknown_targets_are_json_rpc_errors(#[case] server: &str, #[case] message: Value, #[case] code: i64) {
        let answer = reply(handle(server, &message));
        assert_eq!(answer["id"], json!(4));
        assert_eq!(answer.pointer("/error/code"), Some(&json!(code)));
    }

    #[test]
    fn delivery_results_are_tool_results() {
        let delivered = send_file_reply(json!(5), "out/r.pdf", FileDelivery::Delivered);
        assert_eq!(delivered.pointer("/result/isError"), Some(&json!(false)));
        assert_eq!(
            delivered.pointer("/result/content/0/text"),
            Some(&json!("Файл out/r.pdf отправлен пользователю"))
        );
        let failed = send_file_reply(json!(5), "big.zip", FileDelivery::Denied(Denied::new("больше 50 МБ")));
        assert_eq!(failed.pointer("/result/isError"), Some(&json!(true)));
        assert_eq!(failed.pointer("/result/content/0/text"), Some(&json!("больше 50 МБ")));
    }
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-claude --lib mcp`
Expected: FAIL — `cannot find function handle`.

- [ ] **Step 3: Реализовать `mcp.rs`**

```rust
//! The in-process MCP server `agent-hub` with one tool, `send_file`, spoken as JSON-RPC inside
//! `mcp_message` control requests.

use hub_core::domain::{FileDelivery, OutgoingFile};
use serde_json::{Value, json};

use crate::outgoing::MCP_SERVER;
use crate::permissions::SEND_FILE;

const DESCRIPTION: &str = "Send a file to the user in their Telegram chat. The user only sees \
    your text replies, so use this whenever they ask for a file or a file is the natural result \
    (a PDF report, an archive, an image, a CSV export). `path` is absolute or relative to the \
    working directory and must stay inside it; `caption` is optional text shown under the file.";
// Used only if the CLI does not say which protocol version it speaks.
const FALLBACK_PROTOCOL: &str = "2025-06-18";
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

#[derive(Debug, PartialEq, Eq)]
pub enum McpStep {
    Reply(Value),
    SendFile { id: Value, file: OutgoingFile },
}

enum ToolOutcome {
    Success,
    Error,
}

#[must_use]
pub fn handle(server: &str, message: &Value) -> McpStep {
    let id = message.get("id").cloned();
    if server != MCP_SERVER {
        let error = error(id.unwrap_or(Value::Null), METHOD_NOT_FOUND, &format!("Server '{server}' not found"));
        return McpStep::Reply(error);
    }
    let Some(id) = id else {
        // A notification gets no JSON-RPC reply, but the control request carrying it needs an ack.
        return McpStep::Reply(json!({"jsonrpc": "2.0", "result": {}}));
    };
    match message.get("method").and_then(Value::as_str) {
        Some("initialize") => {
            let version = message
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(FALLBACK_PROTOCOL);
            McpStep::Reply(result(id, json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": MCP_SERVER, "version": env!("CARGO_PKG_VERSION")},
            })))
        }
        Some("tools/list") => McpStep::Reply(result(id, json!({"tools": [{
            "name": SEND_FILE,
            "description": DESCRIPTION,
            "inputSchema": {
                "type": "object",
                "properties": {"path": {"type": "string"}, "caption": {"type": "string"}},
                "required": ["path"],
            },
        }]}))),
        Some("tools/call")
            if message.pointer("/params/name").and_then(Value::as_str) == Some(SEND_FILE) =>
        {
            let arguments = message.pointer("/params/arguments").unwrap_or(&Value::Null);
            match send_file_args(arguments) {
                Ok(file) => McpStep::SendFile { id, file },
                Err(reason) => McpStep::Reply(result(id, tool_text(&reason, ToolOutcome::Error))),
            }
        }
        Some("tools/call") => McpStep::Reply(error(id, INVALID_PARAMS, "Unknown tool")),
        Some(_) | None => McpStep::Reply(error(id, METHOD_NOT_FOUND, "Method not found")),
    }
}

/// A failed delivery is a tool error the agent can react to (compress, split, retry).
#[must_use]
pub fn send_file_reply(id: Value, path: &str, delivery: FileDelivery) -> Value {
    let body = match delivery {
        FileDelivery::Delivered => {
            tool_text(&format!("Файл {path} отправлен пользователю"), ToolOutcome::Success)
        }
        FileDelivery::Denied(denied) => tool_text(&denied.reason, ToolOutcome::Error),
    };
    result(id, body)
}

fn send_file_args(arguments: &Value) -> Result<OutgoingFile, String> {
    let path = arguments.get("path").and_then(Value::as_str).filter(|path| !path.trim().is_empty());
    let caption = match arguments.get("caption") {
        None => Some(""),
        Some(caption) => caption.as_str(),
    };
    match (path, caption) {
        (Some(path), Some(caption)) => {
            Ok(OutgoingFile { path: path.to_owned(), caption: caption.to_owned() })
        }
        (Some(_) | None, Some(_) | None) => {
            Err("path must be a non-empty string, caption a string".to_owned())
        }
    }
}

fn tool_text(text: &str, outcome: ToolOutcome) -> Value {
    let failed = match outcome {
        ToolOutcome::Success => false,
        ToolOutcome::Error => true,
    };
    json!({"content": [{"type": "text", "text": text}], "isError": failed})
}

fn result(id: Value, value: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": value})
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}
```

- [ ] **Step 4: Прогнать тесты и линтеры**

Run: `cargo test -p hub-claude --lib && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hub-claude
git commit -m "hub-claude: in-process MCP-сервер с send_file"
```

---

### Task 4: Активность сессии и перевод в события

**Files:**
- Create: `crates/hub-claude/src/activity.rs`, `crates/hub-claude/src/tracker.rs`, `crates/hub-claude/tests/replay.rs`
- Modify: `crates/hub-claude/src/lib.rs` (`pub mod activity; pub mod tracker;`)

**Interfaces:**
- Consumes: `wire::*`, `permissions::{ASK_USER_QUESTION, SEND_FILE_TOOL, summarize_tool_input}`, `hub_core::domain::{AgentEvent, Finished, SessionId, ToolUse}`.
- Produces: `activity::Phase::{Busy, Background, Settling, Idle}`, `activity::SessionActivity` (`Default`, `sent(String)`, `awaiting_result() -> bool`, `background() -> Vec<String>`, `phase() -> Phase`, `observe(&Incoming)`), `tracker::SessionTracker` (`Default`, `translate(&Incoming, usize) -> Vec<AgentEvent>`).

- [ ] **Step 1: Падающие тесты активности (перенос `test_activity.py`)**

`crates/hub-claude/src/activity.rs`:

```rust
#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::wire::parse_line;

    fn message(value: &Value) -> Incoming {
        parse_line(&value.to_string()).unwrap()
    }

    fn started(id: &str, description: &str, background: bool) -> Incoming {
        message(&json!({"type": "system", "subtype": "task_started", "task_id": id,
                        "description": description, "is_backgrounded": background}))
    }

    fn notified(id: &str) -> Incoming {
        message(&json!({"type": "system", "subtype": "task_notification", "task_id": id}))
    }

    fn completed(id: &str) -> Incoming {
        message(&json!({"type": "system", "subtype": "task_updated", "task_id": id,
                        "patch": {"status": "completed"}}))
    }

    fn result(injected: bool) -> Incoming {
        let origin = if injected { json!({"kind": "task-notification"}) } else { Value::Null };
        message(&json!({"type": "result", "subtype": "success", "is_error": false,
                        "num_turns": 1, "session_id": "s", "origin": origin}))
    }

    fn text() -> Incoming {
        message(&json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "BG-DONE"}]}}))
    }

    fn init() -> Incoming {
        message(&json!({"type": "system", "subtype": "init", "session_id": "s"}))
    }

    fn accepted(id: &str) -> Incoming {
        message(&json!({"type": "user", "uuid": id, "message": {"role": "user", "content": "…"}}))
    }

    fn activity(messages: &[Incoming]) -> SessionActivity {
        let mut activity = SessionActivity::default();
        activity.sent("p1".to_owned());
        activity.observe(&accepted("p1"));
        for message in messages {
            activity.observe(message);
        }
        activity
    }

    #[test]
    fn plain_turn_is_busy_until_its_result() {
        let mut activity = activity(&[]);
        assert_eq!(activity.phase(), Phase::Busy);
        activity.observe(&result(false));
        assert_eq!(activity.phase(), Phase::Idle);
    }

    #[test]
    fn foreground_subagent_does_not_keep_session_open() {
        let activity = activity(&[
            started("a", "sub", false), completed("a"), notified("a"), text(), result(false),
        ]);
        assert_eq!(activity.phase(), Phase::Idle);
    }

    #[test]
    fn background_task_keeps_session_open_until_injected_turn_ends() {
        let mut activity = activity(&[started("b", "sleep 8", true), result(false)]);
        assert_eq!(activity.phase(), Phase::Background);
        assert_eq!(activity.background(), ["sleep 8"]);

        activity.observe(&notified("b"));
        assert_eq!(activity.phase(), Phase::Settling);

        activity.observe(&init());
        assert_eq!(activity.phase(), Phase::Busy);

        activity.observe(&text());
        activity.observe(&result(true));
        assert_eq!(activity.phase(), Phase::Idle);
    }

    #[test]
    fn task_update_before_notification_waits_for_the_injected_turn() {
        let mut activity = activity(&[started("b", "sleep", true), result(false), completed("b")]);
        assert_eq!(activity.phase(), Phase::Settling);
        activity.observe(&notified("b"));
        activity.observe(&text());
        activity.observe(&result(true));
        assert_eq!(activity.phase(), Phase::Idle);
    }

    #[test]
    fn background_task_finishing_during_a_turn_still_expects_its_report() {
        let activity = activity(&[started("b", "sleep", true), completed("b"), result(false)]);
        assert_eq!(activity.phase(), Phase::Settling);
    }

    #[test]
    fn task_ending_during_an_injected_turn_expects_another_one() {
        let mut activity = activity(&[
            started("b1", "one", true), started("b2", "two", true), result(false), notified("b1"), text(),
        ]);
        activity.observe(&notified("b2"));
        activity.observe(&result(true));
        assert_eq!(activity.phase(), Phase::Settling);
    }

    #[test]
    fn one_injected_turn_may_report_several_tasks() {
        let mut activity =
            activity(&[started("b1", "one", true), started("b2", "two", true), result(false), notified("b1")]);
        assert_eq!(activity.phase(), Phase::Background);
        activity.observe(&notified("b2"));
        activity.observe(&text());
        activity.observe(&result(true));
        assert_eq!(activity.phase(), Phase::Idle);
    }

    #[test]
    fn message_sent_while_background_runs_is_busy_until_its_result() {
        let mut activity = activity(&[started("b", "sleep", true), result(false)]);
        activity.sent("p2".to_owned());
        assert_eq!(activity.phase(), Phase::Busy);
        activity.observe(&accepted("p2"));
        activity.observe(&result(false));
        assert_eq!(activity.phase(), Phase::Background);
    }

    #[test]
    fn message_merged_into_the_running_turn_ends_with_it() {
        let mut activity = activity(&[]);
        activity.sent("p2".to_owned());
        activity.observe(&accepted("p2"));
        activity.observe(&result(false));
        assert_eq!(activity.phase(), Phase::Idle);
    }

    #[test]
    fn message_not_yet_accepted_keeps_session_busy_after_a_result() {
        let mut activity = activity(&[]);
        activity.sent("p2".to_owned());
        activity.observe(&result(false));
        assert_eq!(activity.phase(), Phase::Busy);
    }

    #[test]
    fn injected_result_does_not_answer_a_human_prompt() {
        let mut activity = activity(&[started("b", "sleep", true), result(false), notified("b")]);
        activity.sent("p2".to_owned());
        activity.observe(&result(true));
        assert_eq!(activity.phase(), Phase::Busy);
    }

    #[test]
    fn running_background_tasks_are_listed() {
        assert_eq!(activity(&[started("b", "sleep", true)]).background(), ["sleep"]);
    }

    #[test]
    fn awaiting_result_until_the_prompt_is_answered() {
        let mut activity = activity(&[]);
        assert!(activity.awaiting_result());
        activity.observe(&result(false));
        assert!(!activity.awaiting_result());
    }
}
```

- [ ] **Step 2: Падающие тесты трекера (перенос `test_claude_backend.py`)**

`crates/hub-claude/src/tracker.rs`:

```rust
#[cfg(test)]
mod tests {
    use hub_core::domain::{AgentEvent, Finished, SessionId, ToolUse};
    use rust_decimal::Decimal;
    use serde_json::{Value, json};

    use super::*;
    use crate::permissions::TOOL_SUMMARY_LIMIT;
    use crate::wire::parse_line;

    fn message(value: &Value) -> Incoming {
        parse_line(&value.to_string()).unwrap()
    }

    fn result(is_error: bool, cost: Value) -> Incoming {
        message(&json!({"type": "result", "subtype": if is_error { "error_during_execution" } else { "success" },
                        "is_error": is_error, "num_turns": 3, "session_id": "s-1",
                        "total_cost_usd": cost, "result": if is_error { "boom" } else { "ok" }}))
    }

    fn session() -> SessionId {
        SessionId::parse("s-1").unwrap()
    }

    #[test]
    fn session_start_is_reported_once() {
        let mut tracker = SessionTracker::default();
        let init = message(&json!({"type": "system", "subtype": "init", "session_id": "s-1"}));
        assert_eq!(tracker.translate(&init, 0), [AgentEvent::SessionStarted(session())]);
        assert_eq!(tracker.translate(&init, 0), []);
    }

    #[test]
    fn assistant_blocks_become_text_and_tool_calls() {
        let assistant = message(&json!({"type": "assistant", "message": {"content": [
            {"type": "text", "text": "Смотрю тесты"},
            {"type": "text", "text": "   "},
            {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "uv run pytest"}},
            {"type": "tool_use", "id": "t2", "name": "AskUserQuestion", "input": {}},
            {"type": "tool_use", "id": "t3", "name": "mcp__agent-hub__send_file", "input": {"path": "a"}}
        ]}}));
        assert_eq!(
            SessionTracker::default().translate(&assistant, 0),
            [
                AgentEvent::AssistantText("Смотрю тесты".to_owned()),
                AgentEvent::ToolCall(ToolUse { tool: "Bash".to_owned(), summary: "uv run pytest".to_owned() }),
            ]
        );
    }

    #[test]
    fn success_result_finishes_turn() {
        assert_eq!(
            SessionTracker::default().translate(&result(false, json!(0.1234)), 2),
            [
                AgentEvent::SessionStarted(session()),
                AgentEvent::Finished(Finished {
                    session: session(),
                    turns: 3,
                    cost: Some(Decimal::new(1234, 4)),
                    background: 2,
                }),
            ]
        );
    }

    #[test]
    fn missing_cost_is_none() {
        let events = SessionTracker::default().translate(&result(false, Value::Null), 0);
        assert_eq!(
            events.last(),
            Some(&AgentEvent::Finished(Finished { session: session(), turns: 3, cost: None, background: 0 }))
        );
    }

    #[test]
    fn tiny_cost_in_scientific_notation_is_kept() {
        let events = SessionTracker::default().translate(&result(false, json!(1e-7)), 0);
        let Some(AgentEvent::Finished(finished)) = events.last() else { panic!("not finished") };
        assert!(finished.cost.is_some_and(|cost| cost > Decimal::ZERO));
    }

    #[test]
    fn error_result_fails_turn() {
        let events = SessionTracker::default().translate(&result(true, Value::Null), 0);
        assert_eq!(events.last(), Some(&AgentEvent::Failed("error_during_execution: boom".to_owned())));
    }

    #[test]
    fn error_without_text_has_a_default_reason() {
        let outcome = message(&json!({"type": "result", "subtype": "error_max_turns", "is_error": true,
                                      "num_turns": 1, "session_id": "s-1", "result": ""}));
        let events = SessionTracker::default().translate(&outcome, 0);
        assert_eq!(events.last(), Some(&AgentEvent::Failed("error_max_turns: ошибка выполнения".to_owned())));
    }

    #[test]
    fn long_tool_summary_is_truncated() {
        let assistant = message(&json!({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "name": "Bash", "input": {"command": "x".repeat(TOOL_SUMMARY_LIMIT * 2)}}
        ]}}));
        let events = SessionTracker::default().translate(&assistant, 0);
        let Some(AgentEvent::ToolCall(call)) = events.first() else { panic!("no tool call") };
        assert_eq!(call.summary.chars().count(), TOOL_SUMMARY_LIMIT);
    }
}
```

`crates/hub-claude/src/lib.rs`:

```rust
pub mod activity;
pub mod mcp;
pub mod outgoing;
pub mod permissions;
pub mod tracker;
pub mod wire;
```

- [ ] **Step 3: Падающий тест на записанных транскриптах**

`crates/hub-claude/tests/replay.rs`:

```rust
//! Real CLI transcripts replayed through activity tracking and event translation.

use hub_claude::activity::{Phase, SessionActivity};
use hub_claude::tracker::SessionTracker;
use hub_claude::wire::{Incoming, User, parse_line};
use hub_core::domain::AgentEvent;
use rstest::rstest;

fn replay(name: &str) -> (Vec<AgentEvent>, Phase) {
    let path = format!("{}/tests/fixtures/{name}.jsonl", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(path).unwrap();
    let messages: Vec<Incoming> = text.lines().map(|line| parse_line(line).unwrap()).collect();
    let prompt = messages
        .iter()
        .find_map(|message| match message {
            Incoming::User(User { uuid: Some(uuid) }) => Some(uuid.clone()),
            _ => None,
        })
        .unwrap();
    let mut activity = SessionActivity::default();
    let mut tracker = SessionTracker::default();
    activity.sent(prompt);
    let events = messages
        .iter()
        .flat_map(|message| {
            activity.observe(message);
            tracker.translate(message, activity.background().len())
        })
        .collect();
    (events, activity.phase())
}

#[rstest]
#[case("approval")]
#[case("question")]
#[case("sendfile")]
#[case("background")]
fn transcript_replays_to_idle(#[case] name: &str) {
    let (events, phase) = replay(name);
    assert_eq!(phase, Phase::Idle);
    insta::assert_debug_snapshot!(format!("replay_{name}"), events);
}
```

- [ ] **Step 4: Убедиться, что тесты падают**

Run: `cargo test -p hub-claude`
Expected: FAIL — `cannot find type SessionActivity`.

- [ ] **Step 5: Реализовать `activity.rs`**

Над тестовым модулем:

```rust
//! What the CLI session is still doing, to know when closing it loses nothing.
//!
//! Only backgrounded tasks matter: foreground subagents finish inside their turn, while a
//! background task reports in a turn the CLI starts on its own after the task ends.

use std::collections::HashSet;

use crate::wire::{Incoming, System, TaskPatch, User};

// Statuses after which a task no longer runs.
const TERMINAL_STATUSES: [&str; 4] = ["completed", "failed", "stopped", "killed"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The agent is working on a turn.
    Busy,
    /// Idle, but background tasks are still running.
    Background,
    /// Tasks reported; the CLI is about to start a turn with their results.
    Settling,
    /// Nothing left; the session can be closed.
    Idle,
}

#[derive(Debug, Default)]
pub struct SessionActivity {
    // Sent prompts the CLI has not taken in yet.
    waiting: HashSet<String>,
    // A turn with our prompts is in progress.
    answering: bool,
    // Running background tasks: id and description, in start order.
    tasks: Vec<(String, String)>,
    // A background task ended; its report turn has not begun.
    report_due: bool,
    // A report turn started by the CLI is in progress.
    reporting: bool,
}

impl SessionActivity {
    pub fn sent(&mut self, prompt: String) {
        self.waiting.insert(prompt);
    }

    #[must_use]
    pub fn awaiting_result(&self) -> bool {
        !self.waiting.is_empty() || self.answering
    }

    #[must_use]
    pub fn background(&self) -> Vec<String> {
        self.tasks.iter().map(|(_, description)| description.clone()).collect()
    }

    #[must_use]
    pub fn phase(&self) -> Phase {
        if self.awaiting_result() || self.reporting {
            Phase::Busy
        } else if !self.tasks.is_empty() {
            Phase::Background
        } else if self.report_due {
            // Also covers a task killed without a report: the settling window closes it.
            Phase::Settling
        } else {
            Phase::Idle
        }
    }

    pub fn observe(&mut self, message: &Incoming) {
        match message {
            Incoming::User(User { uuid: Some(uuid) }) if self.waiting.contains(uuid) => {
                self.waiting.remove(uuid);
                self.answering = true;
            }
            Incoming::System(System::TaskStarted { task_id, description, is_backgrounded })
                if *is_backgrounded != Some(false) =>
            {
                self.tasks.push((task_id.clone(), description.clone()));
            }
            Incoming::System(System::TaskNotification { task_id }) => self.task_ended(task_id),
            // Arrives before the notification, so it alone must not close the session.
            Incoming::System(System::TaskUpdated { task_id, patch }) if is_terminal(patch) => {
                self.task_ended(task_id);
            }
            Incoming::Result(outcome) if outcome.injected() => self.reporting = false,
            Incoming::Result(_) => self.answering = false,
            Incoming::Assistant(_) | Incoming::System(System::Init { .. })
                if self.report_due && !self.answering =>
            {
                self.report_due = false;
                self.reporting = true;
            }
            Incoming::User(_)
            | Incoming::Assistant(_)
            | Incoming::System(_)
            | Incoming::ControlRequest { .. }
            | Incoming::ControlResponse { .. }
            | Incoming::ControlCancelRequest { .. }
            | Incoming::Other => {}
        }
    }

    fn task_ended(&mut self, task_id: &str) {
        if let Some(position) = self.tasks.iter().position(|(id, _)| id == task_id) {
            self.tasks.remove(position);
            self.report_due = true;
        }
    }
}

fn is_terminal(patch: &TaskPatch) -> bool {
    patch.status.as_deref().is_some_and(|status| TERMINAL_STATUSES.contains(&status))
}
```

- [ ] **Step 6: Реализовать `tracker.rs`**

Над тестовым модулем:

```rust
//! CLI messages → domain events, remembering the session id.

use std::str::FromStr;

use hub_core::domain::{AgentEvent, Finished, SessionId, ToolUse};
use rust_decimal::Decimal;
use serde_json::Number;

use crate::permissions::{ASK_USER_QUESTION, SEND_FILE_TOOL, summarize_tool_input};
use crate::wire::{Assistant, Block, Incoming, Outcome, System};

// Tools the user already sees as their own message, not as a tool line.
const SILENT_TOOLS: [&str; 2] = [ASK_USER_QUESTION, SEND_FILE_TOOL];

#[derive(Debug, Default)]
pub struct SessionTracker {
    session: Option<SessionId>,
}

impl SessionTracker {
    /// `background` is how many background tasks still run, reported with a finished turn.
    #[must_use]
    pub fn translate(&mut self, message: &Incoming, background: usize) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        if let Some(found) = session_of(message)
            && self.session.as_ref() != Some(&found)
        {
            self.session = Some(found.clone());
            events.push(AgentEvent::SessionStarted(found));
        }
        match message {
            Incoming::Assistant(Assistant { message, .. }) => {
                events.extend(message.content.iter().filter_map(block_event));
            }
            Incoming::Result(outcome) => events.push(outcome_event(outcome, background)),
            Incoming::System(_)
            | Incoming::User(_)
            | Incoming::ControlRequest { .. }
            | Incoming::ControlResponse { .. }
            | Incoming::ControlCancelRequest { .. }
            | Incoming::Other => {}
        }
        events
    }
}

fn session_of(message: &Incoming) -> Option<SessionId> {
    match message {
        Incoming::System(System::Init { session_id })
        | Incoming::Result(Outcome { session_id, .. }) => SessionId::parse(session_id),
        Incoming::Assistant(Assistant { session_id, .. }) => {
            session_id.as_deref().and_then(SessionId::parse)
        }
        Incoming::System(_)
        | Incoming::User(_)
        | Incoming::ControlRequest { .. }
        | Incoming::ControlResponse { .. }
        | Incoming::ControlCancelRequest { .. }
        | Incoming::Other => None,
    }
}

fn block_event(block: &Block) -> Option<AgentEvent> {
    match block {
        Block::Text { text } if !text.trim().is_empty() => Some(AgentEvent::AssistantText(text.clone())),
        Block::ToolUse { name, input } if !SILENT_TOOLS.contains(&name.as_str()) => {
            Some(AgentEvent::ToolCall(ToolUse {
                tool: name.clone(),
                summary: summarize_tool_input(name, input),
            }))
        }
        Block::Text { .. } | Block::ToolUse { .. } | Block::Other => None,
    }
}

fn outcome_event(outcome: &Outcome, background: usize) -> AgentEvent {
    if outcome.is_error {
        let detail = outcome
            .result
            .as_deref()
            .filter(|result| !result.is_empty())
            .unwrap_or("ошибка выполнения");
        return AgentEvent::Failed(format!("{}: {detail}", outcome.subtype));
    }
    match SessionId::parse(&outcome.session_id) {
        Some(session) => AgentEvent::Finished(Finished {
            session,
            turns: outcome.num_turns,
            cost: outcome.total_cost_usd.as_ref().and_then(cost),
            background,
        }),
        None => AgentEvent::Failed(format!("{}: нет session_id", outcome.subtype)),
    }
}

/// Exact decimal of the number as printed, like Python's `Decimal(str(float))`.
fn cost(number: &Number) -> Option<Decimal> {
    let text = number.to_string();
    Decimal::from_str(&text).or_else(|_| Decimal::from_scientific(&text)).ok()
}
```

- [ ] **Step 6a: Принять snapshot'ы**

Run: `INSTA_UPDATE=always cargo test -p hub-claude --test replay`, затем просмотреть `crates/hub-claude/tests/snapshots/*.snap`. Ожидаемое содержимое:
- `approval`: `SessionStarted`, `ToolCall(Bash "echo hello")`, `AssistantText("Done.")`, `Finished { turns: 2, background: 0, .. }`;
- `question`: `SessionStarted`, `AssistantText("Blue")`, `Finished`; вызов `AskUserQuestion` не выводится;
- `sendfile`: `ToolCall(Write …note.txt)`, `ToolCall(ToolSearch …)` или подобный, без строки `send_file`, `Finished { turns: 4 }`;
- `background`: первый `Finished { background: 1 }`, затем `AssistantText` с `bg-done`, второй `Finished { background: 0 }`.

Если snapshot расходится с этим списком — разбирать причину, а не принимать.

- [ ] **Step 7: Прогнать тесты и линтеры**

Run: `cargo test -p hub-claude && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/hub-claude
git commit -m "hub-claude: фазы сессии, фоновые задачи и события агента"
```

---

### Task 5: Поиск и проверка версии CLI

**Files:**
- Create: `crates/hub-claude/src/version.rs`, `crates/hub-claude/src/process.rs`
- Modify: `crates/hub-claude/src/lib.rs` (`pub mod version; mod process;`)

**Interfaces:**
- Produces: `version::Version { major, minor, patch }` (`Ord`, `Display`), `version::MIN_VERSION`, `version::parse_version(&str) -> Option<Version>`, `version::CliError::{Missing, Spawn { path, source }, Timeout { path }, Unrecognized(String), TooOld { found }}`, `version::locate(Option<&Path>) -> Result<PathBuf, CliError>`, `version::check(&Path) -> Result<Version, CliError>` (async), `process::hide_window(&mut tokio::process::Command)` (crate-private).

- [ ] **Step 1: Падающие тесты**

`crates/hub-claude/src/version.rs`:

```rust
#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("2.1.287 (Claude Code)\n", Some(Version { major: 2, minor: 1, patch: 287 }))]
    #[case("2.1.280", Some(Version { major: 2, minor: 1, patch: 280 }))]
    #[case("", None)]
    #[case("Claude Code 2.1.287", None)]
    #[case("2.1", None)]
    #[case("2.1.x", None)]
    #[case("2.1.287.4", None)]
    fn versions_are_parsed(#[case] output: &str, #[case] expected: Option<Version>) {
        assert_eq!(parse_version(output), expected);
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(Version { major: 2, minor: 1, patch: 1000 } > MIN_VERSION);
        assert!(Version { major: 2, minor: 1, patch: 279 } < MIN_VERSION);
        assert_eq!(MIN_VERSION.to_string(), "2.1.280");
    }

    #[test]
    fn configured_path_is_used_as_is() {
        let path = std::env::temp_dir().join("claude-custom");
        assert_eq!(locate(Some(&path)).unwrap(), path);
    }

    #[tokio::test]
    async fn missing_binary_is_a_spawn_error() {
        let path = std::env::temp_dir().join("definitely-not-claude-binary");
        assert!(matches!(check(&path).await, Err(CliError::Spawn { .. })));
    }
}
```

`crates/hub-claude/src/lib.rs` — добавить `mod process;` и `pub mod version;`.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-claude --lib version`
Expected: FAIL — `cannot find type Version`.

- [ ] **Step 3: Реализовать `process.rs`**

```rust
use tokio::process::Command;

/// Without it every CLI process flashes a console window under the desktop app.
#[cfg(windows)]
pub(crate) fn hide_window(command: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn hide_window(_command: &mut Command) {}
```

- [ ] **Step 4: Реализовать `version.rs`**

Над тестовым модулем:

```rust
//! Which agent CLI to run and whether it speaks the protocol this hub expects.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::process::hide_window;

/// The protocol features this hub relies on (task messages, result origin, replayed prompts)
/// were verified against this version.
pub const MIN_VERSION: Version = Version { major: 2, minor: 1, patch: 280 };
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
const CLI_NAME: &str = "claude";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("Claude Code не найден: укажите путь в настройках или установите `claude` в PATH")]
    Missing,
    #[error("не удалось запустить {}", path.display())]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("`{} --version` не ответил за {} с", path.display(), VERSION_TIMEOUT.as_secs())]
    Timeout { path: PathBuf },
    #[error("непонятный ответ `claude --version`: {0}")]
    Unrecognized(String),
    #[error("Claude Code {found} слишком старый, нужен {} или новее", MIN_VERSION)]
    TooOld { found: Version },
}

/// `2.1.287 (Claude Code)` → 2.1.287.
#[must_use]
pub fn parse_version(output: &str) -> Option<Version> {
    let token = output.split_whitespace().next()?;
    let mut parts = token.split('.').map(str::parse::<u32>);
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) => {
            Some(Version { major, minor, patch })
        }
        _ => None,
    }
}

pub fn locate(configured: Option<&Path>) -> Result<PathBuf, CliError> {
    match configured {
        Some(path) => Ok(path.to_path_buf()),
        None => which::which(CLI_NAME).map_err(|_| CliError::Missing),
    }
}

pub async fn check(cli: &Path) -> Result<Version, CliError> {
    let mut command = Command::new(cli);
    command.arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    hide_window(&mut command);
    let output = tokio::time::timeout(VERSION_TIMEOUT, command.output())
        .await
        .map_err(|_| CliError::Timeout { path: cli.to_path_buf() })?
        .map_err(|source| CliError::Spawn { path: cli.to_path_buf(), source })?;
    let text = String::from_utf8_lossy(&output.stdout);
    let found = parse_version(&text).ok_or_else(|| CliError::Unrecognized(text.trim().to_owned()))?;
    if found < MIN_VERSION { Err(CliError::TooOld { found }) } else { Ok(found) }
}
```

- [ ] **Step 5: Прогнать тесты и линтеры**

Run: `cargo test -p hub-claude && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-claude
git commit -m "hub-claude: поиск CLI и проверка его версии"
```

---

### Task 6: Разговор с CLI поверх потоков

**Files:**
- Create: `crates/hub-claude/src/channel.rs`, `crates/hub-claude/src/session.rs`, `crates/hub-claude/tests/session.rs`
- Modify: `crates/hub-claude/src/lib.rs` (`pub mod channel; pub mod session;`)

**Interfaces:**
- Consumes: `activity`, `tracker`, `wire`, `outgoing`, `permissions`, `mcp`; `hub_core::domain::{AgentEvent, Decision, FileDelivery, OutgoingFile, Prompt, Question, QuestionsOutcome, ToolRequest}`.
- Produces:
  - `channel::UserChannel` — `request(&self, ToolRequest) -> BoxFuture<'_, Decision>`, `ask(&self, Vec<Question>) -> BoxFuture<'_, QuestionsOutcome>`, `send_file(&self, OutgoingFile) -> BoxFuture<'_, FileDelivery>`; `Send + Sync`.
  - `session::Limits { background, settle, initialize }` + `Limits::new(background: Duration)` (settle 30 с, initialize 60 с).
  - `session::Conversation { channel: Arc<dyn UserChannel>, events: mpsc::Sender<AgentEvent>, cancel: CancellationToken, limits: Limits }`.
  - `session::converse<R, W>(R, W, Prompt, mpsc::Receiver<Prompt>, Conversation) -> mpsc::Receiver<Prompt>` (async; `R: AsyncRead + Unpin + Send + 'static`, `W: AsyncWrite + Unpin + Send + 'static`).

- [ ] **Step 1: Трейт канала**

`crates/hub-claude/src/channel.rs`:

```rust
//! The human on the other side: approves tools, answers questions, receives files.

use futures::future::BoxFuture;
use hub_core::domain::{Decision, FileDelivery, OutgoingFile, Question, QuestionsOutcome, ToolRequest};

/// Boxed futures keep the trait object-safe, so the backend does not depend on the messenger.
pub trait UserChannel: Send + Sync {
    fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision>;
    fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome>;
    fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery>;
}
```

- [ ] **Step 2: Падающие интеграционные тесты**

`crates/hub-claude/tests/session.rs`:

```rust
//! The conversation loop against a scripted CLI on an in-memory pipe.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use hub_claude::channel::UserChannel;
use hub_claude::session::{Conversation, Limits, converse};
use hub_core::domain::{
    AgentEvent, Decision, Denied, FileDelivery, Finished, OutgoingFile, Prompt, Question,
    QuestionAnswer, QuestionsOutcome, SessionId, ToolRequest,
};
use rust_decimal::Decimal;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const WAIT: Duration = Duration::from_secs(5);

enum Approval {
    Answer(Decision),
    Never,
}

#[derive(Default)]
struct Calls {
    requests: Vec<ToolRequest>,
    asked: Vec<Vec<Question>>,
    sent: Vec<OutgoingFile>,
}

struct FakeChannel {
    approval: Approval,
    outcome: QuestionsOutcome,
    delivery: FileDelivery,
    calls: Mutex<Calls>,
}

impl FakeChannel {
    fn allowing() -> Self {
        Self {
            approval: Approval::Answer(Decision::Allowed),
            outcome: QuestionsOutcome::Answered(Vec::new()),
            delivery: FileDelivery::Delivered,
            calls: Mutex::default(),
        }
    }
}

impl UserChannel for FakeChannel {
    fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision> {
        self.calls.lock().unwrap().requests.push(tool);
        match &self.approval {
            Approval::Answer(decision) => {
                let decision = decision.clone();
                Box::pin(async move { decision })
            }
            Approval::Never => Box::pin(std::future::pending()),
        }
    }

    fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        self.calls.lock().unwrap().asked.push(questions);
        let outcome = self.outcome.clone();
        Box::pin(async move { outcome })
    }

    fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        self.calls.lock().unwrap().sent.push(file);
        let delivery = self.delivery.clone();
        Box::pin(async move { delivery })
    }
}

struct FakeCli {
    input: Lines<BufReader<ReadHalf<DuplexStream>>>,
    output: WriteHalf<DuplexStream>,
}

impl FakeCli {
    async fn recv(&mut self) -> Option<Value> {
        let line = tokio::time::timeout(WAIT, self.input.next_line()).await.unwrap().unwrap()?;
        Some(serde_json::from_str(&line).unwrap())
    }

    async fn expect(&mut self) -> Value {
        self.recv().await.expect("the hub closed stdin")
    }

    async fn send(&mut self, message: &Value) {
        self.send_raw(format!("{message}\n").as_bytes()).await;
    }

    async fn send_raw(&mut self, bytes: &[u8]) {
        self.output.write_all(bytes).await.unwrap();
    }

    async fn close(&mut self) {
        self.output.shutdown().await.unwrap();
    }

    /// Answers `initialize` and takes the first prompt in, as the real CLI does.
    async fn handshake(&mut self) {
        let initialize = self.expect().await;
        assert_eq!(initialize["request"]["subtype"], "initialize");
        self.send(&json!({"type": "control_response", "response": {
            "subtype": "success", "request_id": initialize["request_id"], "response": {}}}))
            .await;
        let prompt = self.expect().await;
        assert_eq!(prompt["type"], "user");
        self.accept(&prompt).await;
    }

    async fn accept(&mut self, prompt: &Value) {
        self.send(&json!({"type": "user", "uuid": prompt["uuid"], "isReplay": true,
                          "message": {"role": "user", "content": "…"}}))
            .await;
    }
}

struct Harness {
    cli: FakeCli,
    events: mpsc::Receiver<AgentEvent>,
    inbox: mpsc::Sender<Prompt>,
    cancel: CancellationToken,
    run: JoinHandle<mpsc::Receiver<Prompt>>,
    channel: Arc<FakeChannel>,
}

impl Harness {
    fn start(channel: FakeChannel, limits: Limits) -> Self {
        let (hub, cli) = tokio::io::duplex(1 << 20);
        let (hub_read, hub_write) = tokio::io::split(hub);
        let (cli_read, cli_write) = tokio::io::split(cli);
        let (events_out, events) = mpsc::channel(64);
        let (inbox, inbox_in) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let channel = Arc::new(channel);
        let conversation = Conversation {
            channel: Arc::clone(&channel) as Arc<dyn UserChannel>,
            events: events_out,
            cancel: cancel.clone(),
            limits,
        };
        let run = tokio::spawn(converse(hub_read, hub_write, prompt("first"), inbox_in, conversation));
        Self {
            cli: FakeCli { input: BufReader::new(cli_read).lines(), output: cli_write },
            events,
            inbox,
            cancel,
            run,
            channel,
        }
    }

    async fn finish(mut self) -> (Vec<AgentEvent>, mpsc::Receiver<Prompt>, FakeCli) {
        let leftover = tokio::time::timeout(WAIT, self.run).await.unwrap().unwrap();
        let mut events = Vec::new();
        while let Some(event) = self.events.recv().await {
            events.push(event);
        }
        (events, leftover, self.cli)
    }
}

fn limits() -> Limits {
    Limits::new(Duration::from_secs(60))
}

fn prompt(text: &str) -> Prompt {
    Prompt::new(text.to_owned(), Vec::new()).unwrap()
}

fn session() -> SessionId {
    SessionId::parse("s").unwrap()
}

fn result(origin: Option<&str>) -> Value {
    json!({"type": "result", "subtype": "success", "is_error": false, "num_turns": 1,
           "session_id": "s", "total_cost_usd": 0.5,
           "origin": origin.map(|kind| json!({"kind": kind}))})
}

fn text(text: &str) -> Value {
    json!({"type": "assistant", "session_id": "s", "message": {"content": [{"type": "text", "text": text}]}})
}

fn started(id: &str, description: &str) -> Value {
    json!({"type": "system", "subtype": "task_started", "task_id": id, "description": description,
           "is_backgrounded": true})
}

fn notified(id: &str) -> Value {
    json!({"type": "system", "subtype": "task_notification", "task_id": id})
}

fn can_use_tool(id: &str, tool: &str, input: &Value) -> Value {
    json!({"type": "control_request", "request_id": id,
           "request": {"subtype": "can_use_tool", "tool_name": tool, "input": input}})
}

fn finished(background: usize) -> AgentEvent {
    AgentEvent::Finished(Finished { session: session(), turns: 1, cost: Some(Decimal::new(5, 1)), background })
}

#[tokio::test]
async fn plain_turn_relays_text_and_finishes() {
    let mut harness = Harness::start(FakeChannel::allowing(), limits());
    harness.cli.handshake().await;
    harness.cli.send(&text("Привет")).await;
    harness.cli.send(&result(None)).await;

    let (events, mut leftover, _cli) = harness.finish().await;
    assert_eq!(
        events,
        [AgentEvent::SessionStarted(session()), AgentEvent::AssistantText("Привет".to_owned()), finished(0)]
    );
    assert!(leftover.try_recv().is_err());
}

#[tokio::test]
async fn approval_is_asked_and_answered() {
    let mut harness = Harness::start(FakeChannel::allowing(), limits());
    harness.cli.handshake().await;
    harness.cli.send(&can_use_tool("r1", "Bash", &json!({"command": "ls"}))).await;

    let reply = harness.cli.expect().await;
    assert_eq!(reply["response"]["request_id"], "r1");
    assert_eq!(reply["response"]["response"], json!({"behavior": "allow", "updatedInput": {"command": "ls"}}));
    assert_eq!(
        harness.channel.calls.lock().unwrap().requests,
        [ToolRequest { tool: "Bash".to_owned(), summary: "ls".to_owned() }]
    );
    harness.cli.send(&result(None)).await;
    harness.finish().await;
}

#[tokio::test]
async fn denied_approval_returns_its_reason() {
    let channel = FakeChannel { approval: Approval::Answer(Decision::Denied(Denied::new("нет"))), ..FakeChannel::allowing() };
    let mut harness = Harness::start(channel, limits());
    harness.cli.handshake().await;
    harness.cli.send(&can_use_tool("r1", "Bash", &json!({"command": "rm -rf /"}))).await;

    let reply = harness.cli.expect().await;
    assert_eq!(reply["response"]["response"], json!({"behavior": "deny", "message": "нет"}));
    harness.cli.send(&result(None)).await;
    harness.finish().await;
}

#[tokio::test]
async fn question_answers_go_into_the_tool_input() {
    let answers = vec![QuestionAnswer { question: "Цвет?".to_owned(), answer: "Синий".to_owned() }];
    let channel = FakeChannel { outcome: QuestionsOutcome::Answered(answers), ..FakeChannel::allowing() };
    let mut harness = Harness::start(channel, limits());
    harness.cli.handshake().await;
    let input = json!({"questions": [{"question": "Цвет?", "header": "", "options": [{"label": "Синий"}]}]});
    harness.cli.send(&can_use_tool("r1", "AskUserQuestion", &input)).await;

    let reply = harness.cli.expect().await;
    assert_eq!(reply["response"]["response"]["updatedInput"]["answers"], json!({"Цвет?": "Синий"}));
    assert_eq!(harness.channel.calls.lock().unwrap().asked.len(), 1);
    harness.cli.send(&result(None)).await;
    harness.finish().await;
}

#[tokio::test]
async fn send_file_goes_through_mcp() {
    let mut harness = Harness::start(FakeChannel::allowing(), limits());
    harness.cli.handshake().await;
    harness
        .cli
        .send(&json!({"type": "control_request", "request_id": "m1", "request": {
            "subtype": "mcp_message", "server_name": "agent-hub", "message": {
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "send_file", "arguments": {"path": "out/r.pdf"}}}}}))
        .await;

    let reply = harness.cli.expect().await;
    assert_eq!(reply["response"]["request_id"], "m1");
    assert_eq!(
        reply["response"]["response"]["mcp_response"]["result"]["content"][0]["text"],
        "Файл out/r.pdf отправлен пользователю"
    );
    assert_eq!(
        harness.channel.calls.lock().unwrap().sent,
        [OutgoingFile { path: "out/r.pdf".to_owned(), caption: String::new() }]
    );
    harness.cli.send(&result(None)).await;
    harness.finish().await;
}

#[tokio::test]
async fn background_task_keeps_the_conversation_open_for_its_report() {
    let mut harness = Harness::start(FakeChannel::allowing(), limits());
    harness.cli.handshake().await;
    for message in [started("b", "sleep"), result(None), notified("b"), text("BG-DONE"), result(Some("task-notification"))] {
        harness.cli.send(&message).await;
    }

    let (events, _, _) = harness.finish().await;
    let backgrounds: Vec<usize> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Finished(finished) => Some(finished.background),
            _ => None,
        })
        .collect();
    assert_eq!(backgrounds, [1, 0]);
    assert!(events.contains(&AgentEvent::AssistantText("BG-DONE".to_owned())));
}

#[tokio::test]
async fn prompt_during_background_goes_to_the_same_conversation() {
    let mut harness = Harness::start(FakeChannel::allowing(), limits());
    harness.cli.handshake().await;
    harness.cli.send(&started("b", "sleep")).await;
    harness.cli.send(&result(None)).await;

    harness.inbox.send(prompt("ещё вопрос")).await.unwrap();
    let next = harness.cli.expect().await;
    assert_eq!(next["message"]["content"][0]["text"], "ещё вопрос");
    harness.cli.accept(&next).await;
    for message in [result(None), notified("b"), text("BG-DONE"), result(Some("task-notification"))] {
        harness.cli.send(&message).await;
    }
    harness.finish().await;
}

#[tokio::test]
async fn background_outliving_the_budget_is_abandoned() {
    let mut harness = Harness::start(FakeChannel::allowing(), Limits::new(Duration::from_millis(20)));
    harness.cli.handshake().await;
    harness.cli.send(&started("b", "sleep 600")).await;
    harness.cli.send(&result(None)).await;

    let (events, _, _) = harness.finish().await;
    assert_eq!(events.last(), Some(&AgentEvent::BackgroundAbandoned(vec!["sleep 600".to_owned()])));
}

#[tokio::test]
async fn cli_exit_without_result_fails() {
    let mut harness = Harness::start(FakeChannel::allowing(), limits());
    harness.cli.handshake().await;
    harness.cli.close().await;

    let (events, _, _) = harness.finish().await;
    assert_eq!(events.last(), Some(&AgentEvent::Failed("Claude завершился без результата".to_owned())));
}

#[tokio::test]
async fn cli_exit_after_result_is_not_a_failure() {
    let mut harness = Harness::start(FakeChannel::allowing(), limits());
    harness.cli.handshake().await;
    harness.cli.send(&started("b", "sleep")).await;
    harness.cli.send(&result(None)).await;
    harness.cli.close().await;

    let (events, _, _) = harness.finish().await;
    assert_eq!(events.last(), Some(&finished(1)));
}

#[tokio::test]
async fn cancel_sends_interrupt_and_ends() {
    let channel = FakeChannel { approval: Approval::Never, ..FakeChannel::allowing() };
    let mut harness = Harness::start(channel, limits());
    harness.cli.handshake().await;
    harness.cli.send(&can_use_tool("r1", "Bash", &json!({"command": "ls"}))).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    harness.cancel.cancel();

    let interrupt = harness.cli.expect().await;
    assert_eq!(interrupt["request"]["subtype"], "interrupt");
    let (_, _, mut cli) = harness.finish().await;
    assert_eq!(cli.recv().await, None, "no reply to the abandoned approval");
}

#[tokio::test]
async fn control_cancel_request_abandons_pending_approval() {
    let channel = FakeChannel { approval: Approval::Never, ..FakeChannel::allowing() };
    let mut harness = Harness::start(channel, limits());
    harness.cli.handshake().await;
    harness.cli.send(&can_use_tool("r1", "Bash", &json!({"command": "ls"}))).await;
    harness.cli.send(&json!({"type": "control_cancel_request", "request_id": "r1"})).await;
    harness.cli.send(&result(None)).await;

    let (events, _, mut cli) = harness.finish().await;
    assert_eq!(events.last(), Some(&finished(0)));
    assert_eq!(cli.recv().await, None);
}

#[tokio::test]
async fn initialize_error_fails() {
    let mut harness = Harness::start(FakeChannel::allowing(), limits());
    let initialize = harness.cli.expect().await;
    harness
        .cli
        .send(&json!({"type": "control_response", "response": {
            "subtype": "error", "request_id": initialize["request_id"], "error": "bad flags"}}))
        .await;

    let (events, _, _) = harness.finish().await;
    assert_eq!(events.last(), Some(&AgentEvent::Failed("Claude отклонил инициализацию: bad flags".to_owned())));
}

#[tokio::test]
async fn unparsable_line_is_skipped() {
    let mut harness = Harness::start(FakeChannel::allowing(), limits());
    harness.cli.handshake().await;
    harness.cli.send_raw(b"not json\n").await;
    harness.cli.send(&result(None)).await;

    let (events, _, _) = harness.finish().await;
    assert_eq!(events.last(), Some(&finished(0)));
}

#[tokio::test]
async fn invalid_utf8_fails_the_conversation() {
    let mut harness = Harness::start(FakeChannel::allowing(), limits());
    harness.cli.handshake().await;
    harness.cli.send_raw(&[0xff, 0xfe, b'\n']).await;

    let (events, _, _) = harness.finish().await;
    assert!(matches!(events.last(), Some(AgentEvent::Failed(reason)) if reason.starts_with("Не удалось прочитать вывод Claude")));
}

#[tokio::test]
async fn leftover_prompt_is_returned() {
    let channel = FakeChannel { approval: Approval::Never, ..FakeChannel::allowing() };
    let mut harness = Harness::start(channel, limits());
    harness.cli.handshake().await;
    harness.cancel.cancel();
    harness.inbox.send(prompt("после остановки")).await.unwrap();

    let (_, mut leftover, _) = harness.finish().await;
    let pending = leftover.try_recv();
    assert!(pending.is_ok_and(|prompt| prompt.text() == "после остановки"));
}
```

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p hub-claude --test session`
Expected: FAIL — `cannot find function converse`.

- [ ] **Step 4: Реализовать `session.rs`**

```rust
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
        Self { background, settle: Duration::from_secs(30), initialize: Duration::from_secs(60) }
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
            let step = tokio::select! {
                () = self.conversation.cancel.cancelled() => Step::Cancelled,
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
        inbox
    }

    async fn step(&mut self, step: Step, inbox_open: &mut bool) -> Flow {
        match step {
            Step::Cancelled => {
                self.write(&outgoing::control_request(INTERRUPT, outgoing::interrupt())).await;
                Flow::Stop
            }
            Step::Line(None) => {
                if self.activity.awaiting_result() {
                    self.emit(AgentEvent::Failed("Claude завершился без результата".to_owned())).await;
                }
                Flow::Stop
            }
            Step::Line(Some(Err(error))) => {
                self.emit(AgentEvent::Failed(format!("Не удалось прочитать вывод Claude: {error}"))).await;
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
                        self.emit(AgentEvent::BackgroundAbandoned(self.activity.background())).await;
                    }
                    Phase::Settling | Phase::Busy | Phase::Idle => {}
                }
                Flow::Stop
            }
            Step::InitializeTimeout => {
                let seconds = self.conversation.limits.initialize.as_secs();
                self.emit(AgentEvent::Failed(format!("Claude не ответил на инициализацию за {seconds} с"))).await;
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
                self.emit(AgentEvent::Failed(format!("Claude отклонил инициализацию: {error}"))).await;
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

    async fn write(&self, message: &Value) {
        if self.outbox.send(message.to_string()).await.is_err() {
            tracing::warn!("agent cli stdin is closed");
        }
    }

    /// False when nobody listens to the events any more.
    async fn emit(&self, event: AgentEvent) -> bool {
        self.conversation.events.send(event).await.is_ok()
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
```

- [ ] **Step 5: Прогнать тесты и линтеры**

Run: `cargo test -p hub-claude && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS. Если `select!` не компилируется из-за одновременного заимствования `self.conversation` и `self.handlers` — вынести `let cancel = self.conversation.cancel.clone();` перед `select!` и ждать `cancel.cancelled()`.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-claude
git commit -m "hub-claude: разговор с CLI — подтверждения, вопросы, send_file, фоновые задачи"
```

---

### Task 7: Процесс CLI

**Files:**
- Create: `crates/hub-claude/src/backend.rs`, `crates/hub-claude/tests/live.rs`
- Modify: `crates/hub-claude/src/lib.rs` (`pub mod backend;`)

**Interfaces:**
- Consumes: `outgoing::{cli_args, ENTRYPOINT}`, `process::hide_window`, `session::{converse, Conversation, Limits}`, `hub_core::settings::ClaudeSettings`, `hub_core::domain::{AgentEvent, Prompt, TopicSession}`.
- Produces: `backend::ClaudeBackend::new(PathBuf, ClaudeSettings) -> ClaudeBackend`, `ClaudeBackend::run(&self, &TopicSession, Prompt, mpsc::Receiver<Prompt>, Conversation) -> mpsc::Receiver<Prompt>` (async).

- [ ] **Step 1: Падающие тесты**

`crates/hub-claude/src/backend.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_core::domain::{
        AbsolutePath, BackendKind, Decision, FileDelivery, OutgoingFile, Question, QuestionsOutcome,
        ToolRequest,
    };
    use hub_core::settings::PermissionMode;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::channel::UserChannel;
    use crate::session::Limits;

    struct Refusing;

    impl UserChannel for Refusing {
        fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
            Box::pin(async { Decision::Allowed })
        }
        fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            Box::pin(async { QuestionsOutcome::Answered(Vec::new()) })
        }
        fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            Box::pin(async { FileDelivery::Delivered })
        }
    }

    fn settings() -> ClaudeSettings {
        ClaudeSettings { cli: None, model: None, permission_mode: PermissionMode::Default, budget: None }
    }

    #[tokio::test]
    async fn missing_cli_fails_the_conversation_and_keeps_the_inbox() {
        let backend = ClaudeBackend::new(std::env::temp_dir().join("definitely-not-claude"), settings());
        let session = TopicSession::fresh(BackendKind::Claude, AbsolutePath::new(std::env::temp_dir()).unwrap());
        let (events_out, mut events) = mpsc::channel(4);
        let (inbox, inbox_in) = mpsc::channel(1);
        inbox.send(Prompt::new("later".to_owned(), Vec::new()).unwrap()).await.unwrap();
        let conversation = Conversation {
            channel: Arc::new(Refusing),
            events: events_out,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_secs(1)),
        };

        let mut leftover = backend
            .run(&session, Prompt::new("hi".to_owned(), Vec::new()).unwrap(), inbox_in, conversation)
            .await;

        assert!(matches!(events.recv().await, Some(AgentEvent::Failed(reason)) if reason.starts_with("Не удалось запустить Claude")));
        assert!(leftover.try_recv().is_ok());
    }
}
```

`crates/hub-claude/tests/live.rs`:

```rust
//! Round trip through the real CLI. Costs a little: run with
//! `AGENT_HUB_LIVE_CLI=1 cargo test -p hub-claude --test live -- --ignored`.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_claude::backend::ClaudeBackend;
use hub_claude::channel::UserChannel;
use hub_claude::session::{Conversation, Limits};
use hub_claude::version::{check, locate};
use hub_core::domain::{
    AbsolutePath, AgentEvent, BackendKind, Decision, FileDelivery, OutgoingFile, Prompt, Question,
    QuestionsOutcome, ToolRequest, TopicSession,
};
use hub_core::settings::{ClaudeSettings, PermissionMode};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Allowing;

impl UserChannel for Allowing {
    fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
        Box::pin(async { Decision::Allowed })
    }
    fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        Box::pin(async { QuestionsOutcome::Answered(Vec::new()) })
    }
    fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(async { FileDelivery::Delivered })
    }
}

#[tokio::test]
#[ignore = "talks to the real agent CLI and spends tokens"]
async fn live_cli_round_trip() {
    if std::env::var_os("AGENT_HUB_LIVE_CLI").is_none() {
        return;
    }
    let cli = locate(None).unwrap();
    check(&cli).await.unwrap();
    let settings = ClaudeSettings {
        cli: None,
        model: Some("haiku".to_owned()),
        permission_mode: PermissionMode::Default,
        budget: None,
    };
    let workdir = std::env::temp_dir().join(format!("agent-hub-live-{}", std::process::id()));
    std::fs::create_dir_all(&workdir).unwrap();
    let session = TopicSession::fresh(BackendKind::Claude, AbsolutePath::new(workdir).unwrap());
    let (events_out, mut events) = mpsc::channel(64);
    let (_inbox, inbox_in) = mpsc::channel(1);
    let conversation = Conversation {
        channel: Arc::new(Allowing),
        events: events_out,
        cancel: CancellationToken::new(),
        limits: Limits::new(Duration::from_secs(60)),
    };

    ClaudeBackend::new(cli, settings)
        .run(&session, Prompt::new("Reply with one word: pong".to_owned(), Vec::new()).unwrap(), inbox_in, conversation)
        .await;

    let mut seen = Vec::new();
    while let Some(event) = events.recv().await {
        seen.push(event);
    }
    assert!(seen.iter().any(|event| matches!(event, AgentEvent::Finished(_))), "{seen:?}");
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-claude`
Expected: FAIL — `cannot find type ClaudeBackend`.

- [ ] **Step 3: Реализовать `backend.rs`**

Над тестовым модулем:

```rust
//! The agent CLI as a child process for one conversation.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use futures::StreamExt;
use hub_core::domain::{AgentEvent, Prompt, TopicSession};
use hub_core::settings::ClaudeSettings;
use tokio::io::AsyncRead;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LinesCodec};

use crate::outgoing::{ENTRYPOINT, cli_args};
use crate::process::hide_window;
use crate::session::{Conversation, converse};

// After stdin closes the CLI finishes on its own; this bounds the wait before killing it.
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STDERR_LINE: usize = 64 * 1024;

pub struct ClaudeBackend {
    cli: PathBuf,
    settings: ClaudeSettings,
}

impl ClaudeBackend {
    #[must_use]
    pub fn new(cli: PathBuf, settings: ClaudeSettings) -> Self {
        Self { cli, settings }
    }

    /// Runs one conversation and returns the inbox with prompts it did not take.
    pub async fn run(
        &self,
        session: &TopicSession,
        prompt: Prompt,
        inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation,
    ) -> mpsc::Receiver<Prompt> {
        let mut command = Command::new(&self.cli);
        command
            .args(cli_args(&self.settings, session.session.as_ref()))
            .current_dir(session.cwd.as_path())
            .env(ENTRYPOINT.0, ENTRYPOINT.1)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_window(&mut command);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                fail(&conversation, format!("Не удалось запустить Claude: {error}")).await;
                return inbox;
            }
        };
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            fail(&conversation, "Claude запущен без стандартных потоков".to_owned()).await;
            return inbox;
        };
        tokio::spawn(log_stderr(stderr));
        let inbox = converse(stdout, stdin, prompt, inbox, conversation).await;
        if tokio::time::timeout(EXIT_TIMEOUT, child.wait()).await.is_err()
            && let Err(error) = child.kill().await
        {
            tracing::warn!(%error, "agent cli could not be killed");
        }
        inbox
    }
}

async fn fail(conversation: &Conversation, reason: String) {
    // Nobody listening means the topic is gone; there is no one left to tell.
    let _ = conversation.events.send(AgentEvent::Failed(reason)).await;
}

async fn log_stderr<R: AsyncRead + Unpin>(stderr: R) {
    let mut lines = FramedRead::new(stderr, LinesCodec::new_with_max_length(MAX_STDERR_LINE));
    while let Some(line) = lines.next().await {
        match line {
            Ok(line) => tracing::warn!(%line, "agent cli stderr"),
            Err(error) => {
                tracing::debug!(%error, "agent cli stderr unreadable");
                return;
            }
        }
    }
}
```

- [ ] **Step 4: Прогнать тесты и линтеры**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS (`live_cli_round_trip` пропущен как `ignored`).

- [ ] **Step 5: Живой прогон**

Run: `AGENT_HUB_LIVE_CLI=1 cargo test -p hub-claude --test live -- --ignored`
Expected: PASS. Это единственный шаг, который тратит токены; если CLI не авторизован — отметить в ledger и не считать провалом задачи.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-claude
git commit -m "hub-claude: процесс CLI и живой тест"
```

---

## Самопроверка плана

- Покрытие спеки (раздел «hub-claude»): запуск и аргументы (Task 2, 7), `wire` (Task 1), `transport` и `session` (Task 6), `control` — `can_use_tool`, `mcp_message`, отмена (Task 2, 3, 6), `SessionActivity`/`SessionTracker` (Task 4), совместимость CLI (Task 5), snapshot-тесты на транскриптах (Task 4), фейковый CLI (Task 6, отклонение записано), отмена через interrupt → закрытие stdin → kill через 5 с (Task 6, 7).
- Типы между задачами: `Incoming`/`System`/`Outcome` из Task 1 используются в Task 4 и 6 с теми же именами; `permissions::{route, allow, decided, answered}` из Task 2 — в Task 6; `mcp::{handle, send_file_reply, McpStep}` из Task 3 — в Task 6; `Limits::new`, `Conversation` из Task 6 — в Task 7.
