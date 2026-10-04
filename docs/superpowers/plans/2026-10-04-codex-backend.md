# Бэкенд Codex — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** OpenAI Codex как второй агент рядом с Claude, выбираемый на уровне темы, с поведением Python-версии agent-hub.

**Architecture:** Общее для бэкендов уходит из `hub-claude` в новый крейт `hub-agent`. Новый крейт `hub-codex` говорит с `codex app-server` по построчному JSON-RPC: `rpc` (транспорт), `protocol` (чистый перевод сообщений), `requests` (ответы на запросы сервера), `session` (цикл ходов с `turn/steer`), `backend` (процесс, вход, тред). `hub-core` получает `BackendKind::Codex`, `Usage`, `/backend` и настройки Codex; `hub-telegram` и `hub-app` подключают их.

**Tech Stack:** Rust 2024 (1.96), tokio, tokio-util (`LinesCodec`, `CancellationToken`), serde_json, thiserror, base64, which, keyring, eframe; тесты — rstest, insta, `tokio::io::duplex`.

**Spec:** `docs/superpowers/specs/2026-10-04-codex-backend-design.md`

## Global Constraints

- Линты рабочего пространства: `warnings = "deny"`, `clippy::pedantic = deny`, `unwrap_used`/`expect_used`/`panic`/`indexing_slicing = deny` (в `#[cfg(test)]` — как в существующих тестах, без новых `#[allow]` в продакшен-коде).
- Проверка после каждой задачи: `cargo test --workspace` и `cargo clippy --workspace --all-targets` без предупреждений.
- Протокол проверен на `codex-cli 0.160.0`; `MIN_VERSION = 0.160.0`.
- `approvalsReviewer: "user"` всегда.
- Песочница по умолчанию `workspace-write`, одобрения по умолчанию `on-request`.
- Тайм-аут управляющих вызовов — 60 с; длительность хода не ограничена; остановка app-server: закрыть stdin, ждать 5 с, затем `kill`.
- Лимит строки stdout app-server — 64 МиБ.
- `OPENAI_API_KEY` убирается из окружения `codex app-server`.
- API-ключ OpenAI хранится в системном хранилище ключей (аккаунт `openai-api-key`), в `settings.toml` не пишется, в `Debug` не выводится.
- Тексты для пользователя — по-русски, дословно как в спецификации и в Python-версии.
- Комментарии только «почему»; никаких `_ =>` на собственных enum.
- Код в плане написан против текущих API, но не компилировался: если `clippy::pedantic` просит `#[must_use]`, ссылку вместо значения (`needless_pass_by_value`) или иной мелкий рефакторинг — исправить код по линту, не подавляя его `#[allow]`, сохраняя имена и сигнатуры из блоков **Interfaces**, где это возможно.
- Коммит после каждой задачи, в конце сообщения — `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **`codex` из npm на Windows — это `codex.cmd`.** `which::which("codex")` находит его по `PATHEXT`, и `Command::new` должен его запускать так же, как `claude.cmd` сейчас. Тест в задаче 6 проверяет, что `locate` отдаёт найденный путь как есть; ручная проверка — задача 16.
2. **Сообщение пользователя приходит, когда ход уже закончился, а `turn/completed` ещё не прочитан.** Steer получит ошибку; сообщение должно дождаться `turn/completed` и уйти новым ходом, а не потеряться. Тесты в задаче 10.
3. **app-server падает посреди хода** (EOF на stdout). Все ожидающие вызовы получают `Closed`, тема — «Codex app-server завершился…», процесс не висит. Тесты в задачах 7 и 11.
4. **`/stop` во время ожидания кнопки одобрения.** Сессия должна выйти сразу, app-server — остановиться, а задача ответа на запрос — прерваться. Тесты в задачах 10 и 7 (`abort` задач ответа при закрытии).
5. **Старый `settings.toml` без `[agents]` и `[codex]`.** Должен читаться без ошибок с бэкендом Claude по умолчанию. Тест в задаче 4.

---

### Task 1: Крейт `hub-agent`: канал, разговор, поиск CLI

Перенос без изменения поведения: `UserChannel`, `Conversation`, `Limits`, `hide_window` и общая часть проверки версии переезжают из `hub-claude` в новый крейт.

**Files:**
- Create: `crates/hub-agent/Cargo.toml`
- Create: `crates/hub-agent/src/lib.rs`
- Create: `crates/hub-agent/src/channel.rs` (содержимое `crates/hub-claude/src/channel.rs`)
- Create: `crates/hub-agent/src/conversation.rs`
- Create: `crates/hub-agent/src/cli.rs`
- Delete: `crates/hub-claude/src/channel.rs`, `crates/hub-claude/src/process.rs`
- Modify: `Cargo.toml` (workspace members и dependencies)
- Modify: `crates/hub-claude/Cargo.toml`, `crates/hub-claude/src/lib.rs`, `crates/hub-claude/src/session.rs`, `crates/hub-claude/src/backend.rs`, `crates/hub-claude/src/version.rs`, `crates/hub-claude/tests/session.rs`, `crates/hub-claude/tests/live.rs`
- Modify: `crates/hub-telegram/Cargo.toml`, `crates/hub-telegram/src/agents.rs`, `crates/hub-telegram/src/channel.rs`, `crates/hub-telegram/src/hub.rs`, `crates/hub-telegram/src/session.rs`

**Interfaces:**
- Produces:
  - `hub_agent::channel::UserChannel` (трейт без изменений: `request`, `ask`, `send_file`).
  - `hub_agent::conversation::{Conversation, Limits}` (поля и `Limits::new(background)` без изменений).
  - `hub_agent::cli::{Version, VersionError, locate, version_output, hide_window}`:
    - `Version { major: u32, minor: u32, patch: u32 }`, `Version::parse(token: &str) -> Option<Version>`, `Display` как `1.2.3`, `Ord`.
    - `locate(name: &str, configured: Option<&Path>) -> Option<PathBuf>`.
    - `async fn version_output(cli: &Path) -> Result<String, VersionError>`.
    - `VersionError { Spawn { path, source }, Timeout { path } }`.
    - `hide_window(command: &mut tokio::process::Command)`.
  - `hub_claude::version::CliError { Missing, Version(VersionError), Unrecognized(String), TooOld { found } }`; `hub_claude::version::Version` остаётся доступным через `pub use`.

- [ ] **Step 1: Создать крейт**

`crates/hub-agent/Cargo.toml`:

```toml
[package]
name = "hub-agent"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
futures.workspace = true
hub-core.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["process", "sync", "time"] }
tokio-util.workspace = true
which.workspace = true

[dev-dependencies]
rstest.workspace = true
tokio = { workspace = true, features = ["macros", "process", "rt", "sync", "time"] }

[lints]
workspace = true
```

В корневом `Cargo.toml` (`hub-codex` добавляется в задаче 6):

```toml
members = ["crates/hub-core", "crates/hub-agent", "crates/hub-claude", "crates/hub-telegram", "crates/hub-app"]
```

и в `[workspace.dependencies]` после `hub-core`:

```toml
hub-agent = { path = "crates/hub-agent" }
```

`crates/hub-agent/src/lib.rs`:

```rust
pub mod channel;
pub mod cli;
pub mod conversation;
```

- [ ] **Step 2: Перенести канал и разговор**

`crates/hub-agent/src/channel.rs` — файл `crates/hub-claude/src/channel.rs` целиком, без изменений.

`crates/hub-agent/src/conversation.rs`:

```rust
//! What a backend needs to run one conversation: the human, the event sink, cancellation, limits.

use std::sync::Arc;
use std::time::Duration;

use hub_core::domain::AgentEvent;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::channel::UserChannel;
```

Ниже — `pub struct Limits`, `impl Limits` и `pub struct Conversation`, вырезанные из `crates/hub-claude/src/session.rs` без изменений (строки с `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub struct Limits` по конец `pub struct Conversation`).

В `crates/hub-claude/src/session.rs` удалить эти определения и импортировать:

```rust
use hub_agent::channel::UserChannel;
use hub_agent::conversation::Conversation;
```

(убрать `use crate::channel::UserChannel;`; неиспользуемые после переноса импорты `Arc`, `CancellationToken` удалить, если компилятор о них сообщит).

- [ ] **Step 3: Написать тесты `cli`**

`crates/hub-agent/src/cli.rs` (тесты внизу файла; реализация — в шаге 5):

```rust
#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("2.1.287", Some(Version { major: 2, minor: 1, patch: 287 }))]
    #[case("0.160.0", Some(Version { major: 0, minor: 160, patch: 0 }))]
    #[case("", None)]
    #[case("2.1", None)]
    #[case("2.1.x", None)]
    #[case("2.1.287.4", None)]
    fn version_tokens_are_parsed(#[case] token: &str, #[case] expected: Option<Version>) {
        assert_eq!(Version::parse(token), expected);
    }

    #[test]
    fn versions_compare_numerically_and_print_dotted() {
        let old = Version { major: 0, minor: 159, patch: 9 };
        let new = Version { major: 0, minor: 160, patch: 0 };
        assert!(old < new);
        assert_eq!(new.to_string(), "0.160.0");
    }

    #[test]
    fn configured_path_is_used_as_is() {
        let path = std::env::temp_dir().join("agent-custom");
        assert_eq!(locate("agent", Some(&path)), Some(path));
    }

    #[test]
    fn unknown_name_is_not_found() {
        assert_eq!(locate("definitely-not-an-agent-cli", None), None);
    }

    #[tokio::test]
    async fn missing_binary_is_a_spawn_error() {
        let path = std::env::temp_dir().join("definitely-not-an-agent-binary");
        assert!(matches!(version_output(&path).await, Err(VersionError::Spawn { .. })));
    }
}
```

- [ ] **Step 4: Убедиться, что тесты не компилируются**

Run: `cargo test -p hub-agent`
Expected: FAIL — `cannot find type Version`.

- [ ] **Step 5: Реализовать `cli`**

Над тестами в `crates/hub-agent/src/cli.rs`:

```rust
//! Locating an agent CLI and asking it for its version.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// `2.1.287` → 2.1.287; a fourth component means it is not a version.
    #[must_use]
    pub fn parse(token: &str) -> Option<Self> {
        let mut parts = token.split('.').map(str::parse::<u32>);
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) => {
                Some(Self { major, minor, patch })
            }
            _ => None,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    #[error("не удалось запустить {}", path.display())]
    Spawn {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("`{} --version` не ответил за {} с", path.display(), VERSION_TIMEOUT.as_secs())]
    Timeout { path: PathBuf },
}

#[must_use]
pub fn locate(name: &str, configured: Option<&Path>) -> Option<PathBuf> {
    match configured {
        Some(path) => Some(path.to_path_buf()),
        None => which::which(name).ok(),
    }
}

pub async fn version_output(cli: &Path) -> Result<String, VersionError> {
    let mut command = Command::new(cli);
    command.arg("--version").stdin(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    hide_window(&mut command);
    let output = tokio::time::timeout(VERSION_TIMEOUT, command.output())
        .await
        .map_err(|_| VersionError::Timeout { path: cli.to_path_buf() })?
        .map_err(|source| VersionError::Spawn { path: cli.to_path_buf(), source })?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Without it every CLI process flashes a console window under the desktop app.
#[cfg(windows)]
pub fn hide_window(command: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_window(_command: &mut Command) {}
```

- [ ] **Step 6: Перевести `hub-claude` на `hub-agent`**

`crates/hub-claude/Cargo.toml`, в `[dependencies]`: `hub-agent.workspace = true`.

`crates/hub-claude/src/lib.rs`: убрать `pub mod channel;` и `mod process;`.

`crates/hub-claude/src/version.rs` — заменить всё до `#[cfg(test)]` на:

```rust
//! Which Claude CLI to run and whether it speaks the protocol this hub expects.

use std::path::{Path, PathBuf};

use hub_agent::cli::{self, VersionError};
pub use hub_agent::cli::Version;

/// The protocol features this hub relies on (task messages, result origin, replayed prompts)
/// were verified against this version.
pub const MIN_VERSION: Version = Version { major: 2, minor: 1, patch: 280 };
const CLI_NAME: &str = "claude";

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("Claude Code не найден: укажите путь в настройках или установите `claude` в PATH")]
    Missing,
    #[error(transparent)]
    Version(#[from] VersionError),
    #[error("непонятный ответ `claude --version`: {0}")]
    Unrecognized(String),
    #[error("Claude Code {found} слишком старый, нужен {} или новее", MIN_VERSION)]
    TooOld { found: Version },
}

/// `2.1.287 (Claude Code)` → 2.1.287.
#[must_use]
pub fn parse_version(output: &str) -> Option<Version> {
    output.split_whitespace().next().and_then(Version::parse)
}

pub fn locate(configured: Option<&Path>) -> Result<PathBuf, CliError> {
    cli::locate(CLI_NAME, configured).ok_or(CliError::Missing)
}

pub async fn check(path: &Path) -> Result<Version, CliError> {
    let text = cli::version_output(path).await?;
    let found =
        parse_version(&text).ok_or_else(|| CliError::Unrecognized(text.trim().to_owned()))?;
    if found < MIN_VERSION { Err(CliError::TooOld { found }) } else { Ok(found) }
}
```

В тестах `version.rs` заменить последний тест на:

```rust
    #[tokio::test]
    async fn missing_binary_is_a_spawn_error() {
        let path = std::env::temp_dir().join("definitely-not-claude-binary");
        assert!(matches!(
            check(&path).await,
            Err(CliError::Version(VersionError::Spawn { .. }))
        ));
    }
```

`crates/hub-claude/src/backend.rs`: `use crate::process::hide_window;` → `use hub_agent::cli::hide_window;`; `use crate::session::{Conversation, converse};` → `use hub_agent::conversation::Conversation;` и `use crate::session::converse;`; в тестах `use crate::channel::UserChannel;` → `use hub_agent::channel::UserChannel;`, `use crate::session::Limits;` → `use hub_agent::conversation::Limits;`.

`crates/hub-claude/tests/session.rs` и `crates/hub-claude/tests/live.rs`: `hub_claude::channel::UserChannel` → `hub_agent::channel::UserChannel`, `hub_claude::session::{Conversation, Limits}` → `hub_agent::conversation::{Conversation, Limits}` (функция `converse` остаётся в `hub_claude::session`). Добавить `hub-agent.workspace = true` в `[dev-dependencies]` `hub-claude`, если тестам он не виден через `[dependencies]` (он виден — интеграционные тесты видят зависимости крейта).

- [ ] **Step 7: Перевести `hub-telegram`**

`crates/hub-telegram/Cargo.toml`, `[dependencies]`: `hub-agent.workspace = true`.

Заменить импорты:
- `crates/hub-telegram/src/channel.rs`: `use hub_claude::channel::UserChannel;` → `use hub_agent::channel::UserChannel;`
- `crates/hub-telegram/src/hub.rs`: `use hub_claude::session::Conversation;` → `use hub_agent::conversation::Conversation;`
- `crates/hub-telegram/src/session.rs`: `use hub_claude::session::{Conversation, Limits};` → `use hub_agent::conversation::{Conversation, Limits};`
- `crates/hub-telegram/src/agents.rs`: `use hub_claude::session::Conversation;` → `use hub_agent::conversation::Conversation;`

Проверить, что больше нет ссылок: `rg "hub_claude::(channel|session::(Conversation|Limits))|crate::process" crates` — пусто.

- [ ] **Step 8: Прогнать всё**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS, без предупреждений; тесты `hub-claude` и `hub-telegram` проходят без изменения ожиданий.

- [ ] **Step 9: Commit**

```bash
git add -A crates Cargo.toml Cargo.lock
git commit -m "Крейт hub-agent: общий канал, разговор и поиск CLI" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Инструменты хаба в `hub-agent`

`send_file` и разбор вопросов переезжают из `hub-claude` в `hub_agent::tools`; добавляются описание и схема `ask_user` и общий `deliver_file`.

**Files:**
- Create: `crates/hub-agent/src/tools.rs`
- Modify: `crates/hub-agent/src/lib.rs`
- Modify: `crates/hub-claude/src/mcp.rs`, `crates/hub-claude/src/permissions.rs` (и места, где они использовались: `rg "permissions::(parse_questions|SEND_FILE\b|TOOL_SUMMARY_LIMIT)|mcp::DESCRIPTION" crates`)

**Interfaces:**
- Consumes: `hub_agent::channel::UserChannel` (задача 1).
- Produces (`hub_agent::tools`):
  - `pub const SEND_FILE: &str = "send_file"`, `pub const ASK_USER: &str = "ask_user"`, `pub const TOOL_SUMMARY_LIMIT: usize = 600`.
  - `pub const SEND_FILE_DESCRIPTION: &str`, `pub const ASK_USER_DESCRIPTION: &str`, `pub const ASK_USER_SHAPE: &str`.
  - `pub fn send_file_schema() -> Value`, `pub fn ask_user_schema() -> Value`.
  - `pub enum ToolResult { Success(String), Error(String) }`.
  - `pub fn parse_send_file(arguments: &Value) -> Result<OutgoingFile, String>`.
  - `pub fn delivery_result(path: &str, delivery: FileDelivery) -> ToolResult`.
  - `pub async fn deliver_file(arguments: &Value, channel: &dyn UserChannel) -> ToolResult`.
  - `pub fn parse_questions(input: &Value) -> Option<Vec<Question>>`, `pub fn parse_option(raw: &Value) -> Option<QuestionOption>`.

- [ ] **Step 1: Написать тесты**

`crates/hub-agent/src/tools.rs`, модуль тестов. Перенести сюда из `crates/hub-claude/src/permissions.rs` тесты `questions_are_parsed`, `multi_select_is_recognised`, `malformed_questions_are_rejected` и их помощники `ask_input`/`ask_question` без изменений, затем добавить:

```rust
    use std::sync::Mutex;

    use futures::future::BoxFuture;
    use hub_core::domain::{Decision, Denied, QuestionsOutcome, ToolRequest};

    struct Recording {
        delivery: FileDelivery,
        sent: Mutex<Vec<OutgoingFile>>,
    }

    impl Recording {
        fn new(delivery: FileDelivery) -> Self {
            Self { delivery, sent: Mutex::new(Vec::new()) }
        }
    }

    impl UserChannel for Recording {
        fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
            Box::pin(async { Decision::Allowed })
        }
        fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            Box::pin(async { QuestionsOutcome::Answered(Vec::new()) })
        }
        fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            self.sent.lock().unwrap().push(file);
            let delivery = self.delivery.clone();
            Box::pin(async move { delivery })
        }
    }

    #[tokio::test]
    async fn delivered_file_is_a_success() {
        let channel = Recording::new(FileDelivery::Delivered);
        let result = deliver_file(&json!({"path": "out/report.pdf"}), &channel).await;
        assert_eq!(result, ToolResult::Success("Файл out/report.pdf отправлен пользователю".to_owned()));
        assert_eq!(
            *channel.sent.lock().unwrap(),
            [OutgoingFile { path: "out/report.pdf".to_owned(), caption: String::new() }]
        );
    }

    #[tokio::test]
    async fn refused_file_is_a_tool_error() {
        let channel = Recording::new(FileDelivery::Denied(Denied::new("вне рабочей директории")));
        let result = deliver_file(&json!({"path": "/etc/passwd"}), &channel).await;
        assert_eq!(result, ToolResult::Error("вне рабочей директории".to_owned()));
    }

    #[rstest]
    #[case(json!({}))]
    #[case(json!({"path": "  "}))]
    #[case(json!({"path": "a", "caption": 5}))]
    #[tokio::test]
    async fn malformed_send_file_never_reaches_the_user(#[case] arguments: Value) {
        let channel = Recording::new(FileDelivery::Delivered);
        let result = deliver_file(&arguments, &channel).await;
        assert_eq!(
            result,
            ToolResult::Error("path must be a non-empty string, caption a string".to_owned())
        );
        assert!(channel.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn schemas_require_their_main_argument() {
        assert_eq!(send_file_schema()["required"], json!(["path"]));
        assert_eq!(ask_user_schema()["required"], json!(["questions"]));
    }
```

(Для `#[cfg(test)]`-модуля разрешены `unwrap` и индексирование `Value[...]`, как в существующих тестах; если линт `indexing_slicing` срабатывает на `Value`-индекс, использовать `.get("required")`: `assert_eq!(send_file_schema().get("required"), Some(&json!(["path"])));`.)

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-agent tools`
Expected: FAIL — `cannot find function deliver_file`.

- [ ] **Step 3: Реализовать `tools`**

`crates/hub-agent/src/lib.rs`: добавить `pub mod tools;`. В `crates/hub-agent/Cargo.toml` `[dev-dependencies]` ничего не меняется.

`crates/hub-agent/src/tools.rs`, над тестами:

```rust
//! Tools agent-hub itself gives an agent, and parsing of their inputs.

use hub_core::domain::{FileDelivery, OutgoingFile, Question, QuestionOption, Selection};
use serde_json::{Value, json};

use crate::channel::UserChannel;

pub const SEND_FILE: &str = "send_file";
pub const ASK_USER: &str = "ask_user";
pub const TOOL_SUMMARY_LIMIT: usize = 600;

pub const SEND_FILE_DESCRIPTION: &str = "Send a file to the user in their Telegram chat. The user \
    only sees your text replies, so use this whenever they ask for a file or a file is the \
    natural result (a PDF report, an archive, an image, a CSV export). `path` is absolute or \
    relative to the working directory and must stay inside it; `caption` is optional text shown \
    under the file.";
pub const ASK_USER_DESCRIPTION: &str = "Ask the user clarifying questions in their Telegram chat \
    and wait for the answers. Use it only when an answer would materially change the work. Each \
    question has a short header and up to four options the user picks with a button; the user \
    may also reply with free text. Set multiSelect to let the user pick several options.";
pub const ASK_USER_SHAPE: &str =
    "questions must be a non-empty list of {question, header, options: [{label, description}], multiSelect}";
const SEND_FILE_SHAPE: &str = "path must be a non-empty string, caption a string";

#[must_use]
pub fn send_file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"path": {"type": "string"}, "caption": {"type": "string"}},
        "required": ["path"],
    })
}

#[must_use]
pub fn ask_user_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "questions": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        "question": {"type": "string"},
                        "header": {"type": "string"},
                        "options": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "label": {"type": "string"},
                                    "description": {"type": "string"},
                                },
                                "required": ["label"],
                            },
                        },
                        "multiSelect": {"type": "boolean"},
                    },
                    "required": ["question"],
                },
            }
        },
        "required": ["questions"],
    })
}

/// What a hub tool returns to the agent; an error is something the agent can react to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolResult {
    Success(String),
    Error(String),
}

pub fn parse_send_file(arguments: &Value) -> Result<OutgoingFile, String> {
    let path = arguments.get("path").and_then(Value::as_str).filter(|path| !path.trim().is_empty());
    let caption = match arguments.get("caption") {
        None => Some(""),
        Some(caption) => caption.as_str(),
    };
    match (path, caption) {
        (Some(path), Some(caption)) => {
            Ok(OutgoingFile { path: path.to_owned(), caption: caption.to_owned() })
        }
        (Some(_) | None, Some(_) | None) => Err(SEND_FILE_SHAPE.to_owned()),
    }
}

#[must_use]
pub fn delivery_result(path: &str, delivery: FileDelivery) -> ToolResult {
    match delivery {
        FileDelivery::Delivered => ToolResult::Success(format!("Файл {path} отправлен пользователю")),
        FileDelivery::Denied(denied) => ToolResult::Error(denied.reason),
    }
}

pub async fn deliver_file(arguments: &Value, channel: &dyn UserChannel) -> ToolResult {
    match parse_send_file(arguments) {
        Err(reason) => ToolResult::Error(reason),
        Ok(file) => {
            let path = file.path.clone();
            delivery_result(&path, channel.send_file(file).await)
        }
    }
}
```

Ниже — `parse_questions`, `parse_question` (приватная) и `parse_option` (теперь `pub`), вырезанные из `crates/hub-claude/src/permissions.rs` без изменений тела.

- [ ] **Step 4: Перевести `hub-claude` на `tools`**

`crates/hub-claude/src/permissions.rs`:
- удалить `pub const SEND_FILE`, `pub const TOOL_SUMMARY_LIMIT`, `parse_questions`, `parse_question`, `parse_option` и перенесённые тесты;
- добавить `use hub_agent::tools::{SEND_FILE, TOOL_SUMMARY_LIMIT, parse_questions};` (оставить `SEND_FILE_TOOL` как есть);
- убрать из `use hub_core::domain::{...}` импорты, ставшие ненужными (`QuestionOption`, `Selection`).

`crates/hub-claude/src/mcp.rs`:
- удалить `const DESCRIPTION` и `fn send_file_args`; импортировать `use hub_agent::tools::{SEND_FILE, SEND_FILE_DESCRIPTION, ToolResult, delivery_result, parse_send_file, send_file_schema};` и убрать `use crate::permissions::SEND_FILE;`;
- в `tools/list` заменить `"description": DESCRIPTION` и литерал схемы на `"description": SEND_FILE_DESCRIPTION, "inputSchema": send_file_schema()`;
- `send_file_args(arguments)` → `parse_send_file(arguments)`;
- `send_file_reply` переписать через общий результат:

```rust
/// A failed delivery is a tool error the agent can react to (compress, split, retry).
#[must_use]
pub fn send_file_reply(id: Value, path: &str, delivery: FileDelivery) -> Value {
    let body = match delivery_result(path, delivery) {
        ToolResult::Success(text) => tool_text(&text, ToolOutcome::Success),
        ToolResult::Error(text) => tool_text(&text, ToolOutcome::Error),
    };
    result(id, body)
}
```

Исправить прочие ссылки, найденные `rg "permissions::(parse_questions|SEND_FILE\b|TOOL_SUMMARY_LIMIT)" crates`, на `hub_agent::tools::...`.

- [ ] **Step 5: Прогнать всё**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS; снимки и тесты `hub-claude` (MCP `tools/list`, ответы `send_file`) не меняются.

- [ ] **Step 6: Commit**

```bash
git add -A crates
git commit -m "hub-agent: инструменты хаба send_file и ask_user" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: `Usage` в итоге хода

**Files:**
- Modify: `crates/hub-core/src/domain.rs:257-264` (`Finished`)
- Modify: `crates/hub-core/src/render.rs` (`format_finished` и тесты)
- Modify: `crates/hub-claude/src/tracker.rs:88-93` и его тесты, `crates/hub-claude/tests/session.rs:221-226`
- Modify: `crates/hub-telegram/src/session.rs:143-151`, `crates/hub-telegram/src/hub.rs:723-730`

**Interfaces:**
- Produces:
  - `hub_core::domain::Usage { Claude { turns: u32, cost: Option<Decimal> }, Codex { tokens: Option<u64> } }`.
  - `hub_core::domain::Finished { session: SessionId, usage: Usage, background: usize }`.
  - `hub_core::render::format_finished(finished: &Finished) -> String`.

- [ ] **Step 1: Написать тесты**

В `crates/hub-core/src/render.rs` заменить тесты `finished_line` и `finished_mentions_running_background_tasks` на:

```rust
    fn claude(turns: u32, cost: Option<&str>, background: usize) -> Finished {
        Finished {
            session: SessionId::parse("s").unwrap(),
            usage: Usage::Claude { turns, cost: cost.map(|raw| raw.parse::<Decimal>().unwrap()) },
            background,
        }
    }

    fn codex(tokens: Option<u64>) -> Finished {
        Finished {
            session: SessionId::parse("t").unwrap(),
            usage: Usage::Codex { tokens },
            background: 0,
        }
    }

    #[rstest]
    #[case(None, "✅ Готово · ходов: 3")]
    #[case(Some("0.1234"), "✅ Готово · ходов: 3 · $0.12")]
    #[case(Some("2"), "✅ Готово · ходов: 3 · $2.00")]
    #[case(Some("0.125"), "✅ Готово · ходов: 3 · $0.12")]
    fn claude_finished_line(#[case] cost: Option<&str>, #[case] expected: &str) {
        assert_eq!(format_finished(&claude(3, cost, 0)), expected);
    }

    #[test]
    fn finished_mentions_running_background_tasks() {
        assert_eq!(
            format_finished(&claude(3, None, 2)),
            "✅ Готово · ходов: 3 · ⏳ в фоне задач: 2, пришлю результат"
        );
    }

    #[rstest]
    #[case(None, "✅ Готово")]
    #[case(Some(0), "✅ Готово · токенов в сессии: 0")]
    #[case(Some(999), "✅ Готово · токенов в сессии: 999")]
    #[case(Some(12_345), "✅ Готово · токенов в сессии: 12 345")]
    #[case(Some(1_000_000), "✅ Готово · токенов в сессии: 1 000 000")]
    fn codex_finished_line(#[case] tokens: Option<u64>, #[case] expected: &str) {
        assert_eq!(format_finished(&codex(tokens)), expected);
    }
```

и в `use` тестового модуля добавить `use crate::domain::{Finished, SessionId, Usage};`.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-core render`
Expected: FAIL — `cannot find type Usage`.

- [ ] **Step 3: Реализовать**

`crates/hub-core/src/domain.rs` — заменить `Finished`:

```rust
/// What the agent reports about a finished turn: Claude counts turns and cost, Codex tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Usage {
    Claude { turns: u32, cost: Option<Decimal> },
    Codex { tokens: Option<u64> },
}

/// One agent turn ended; `background` tasks keep running and report in later turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub session: SessionId,
    pub usage: Usage,
    pub background: usize,
}
```

`crates/hub-core/src/render.rs` — заменить `format_finished`:

```rust
#[must_use]
pub fn format_finished(finished: &Finished) -> String {
    let usage = match &finished.usage {
        Usage::Claude { turns, cost } => format!(" · ходов: {turns}{}", cost_text(*cost)),
        Usage::Codex { tokens } => tokens.map_or_else(String::new, |tokens| {
            format!(" · токенов в сессии: {}", group_thousands(tokens))
        }),
    };
    let pending = if finished.background == 0 {
        String::new()
    } else {
        format!(" · ⏳ в фоне задач: {}, пришлю результат", finished.background)
    };
    format!("✅ Готово{usage}{pending}")
}

fn cost_text(cost: Option<Decimal>) -> String {
    cost.map_or_else(String::new, |cost| {
        // Banker's rounding, as Python's Decimal.quantize, then always two decimals.
        let mut cents = cost.round_dp(2);
        cents.rescale(2);
        format!(" · ${cents}")
    })
}

/// `12345` → `12 345`, as Python's `f"{n:_}".replace("_", " ")`.
fn group_thousands(number: u64) -> String {
    let digits = number.to_string();
    let len = digits.len();
    digits.chars().enumerate().fold(String::with_capacity(len + len / 3), |mut out, (index, digit)| {
        if index > 0 && (len - index) % 3 == 0 {
            out.push(' ');
        }
        out.push(digit);
        out
    })
}
```

и импорт `use crate::domain::{Finished, Usage};`.

- [ ] **Step 4: Обновить потребителей**

`crates/hub-claude/src/tracker.rs` — конструктор:

```rust
        Some(session) => AgentEvent::Finished(Finished {
            session,
            usage: Usage::Claude {
                turns: outcome.num_turns,
                cost: outcome.total_cost_usd.as_ref().and_then(cost),
            },
            background,
        }),
```

(импорт `Usage` из `hub_core::domain`), и в тестах того же файла и `crates/hub-claude/tests/session.rs` каждое `Finished { session, turns: N, cost: X, background: B }` → `Finished { session, usage: Usage::Claude { turns: N, cost: X }, background: B }`.

`crates/hub-telegram/src/session.rs` — ветка `Finished`:

```rust
            AgentEvent::Finished(finished) => {
                tracing::info!(usage = ?finished.usage, background = finished.background, "turn finished");
                bind(mailbox, key, finished.session.clone()).await;
                sender.text(Target::Topic(key), &format_finished(&finished)).await;
            }
```

`crates/hub-telegram/src/hub.rs` — тестовый `finished()`:

```rust
    fn finished() -> AgentEvent {
        AgentEvent::Finished(Finished {
            session: SessionId::parse("s-1").unwrap(),
            usage: Usage::Claude { turns: 1, cost: None },
            background: 0,
        })
    }
```

(импорт `Usage` в тестовом модуле).

- [ ] **Step 5: Прогнать всё**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A crates
git commit -m "Итог хода: сводка Usage для каждого бэкенда" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: `BackendKind::Codex` и настройки Codex

**Files:**
- Modify: `crates/hub-core/src/domain.rs:40-60` (`BackendKind`) и тест `backend_kind_parses_case_insensitively`
- Modify: `crates/hub-core/src/settings.rs`
- Modify: `crates/hub-core/Cargo.toml` (`[dev-dependencies] toml.workspace = true`)
- Modify: `crates/hub-telegram/src/agents.rs` (временная ветка Codex)
- Modify: `crates/hub-app/src/config.rs:49` (`to_draft`)
- Modify: все литералы `Settings { .. }` вне `settings.rs`, если есть (`rg "Settings \{" crates --glob '!**/settings.rs'`)

**Interfaces:**
- Produces (`hub_core`):
  - `domain::BackendKind::Codex`, `BackendKind::ALL: [BackendKind; 2] = [Claude, Codex]`, `name()` → `"codex"`.
  - `settings::Sandbox { ReadOnly, WorkspaceWrite, DangerFullAccess }` с `ALL`, `wire()` (`"read-only"`, `"workspace-write"`, `"danger-full-access"`), `parse(&str) -> Option<Self>`.
  - `settings::Approval { Untrusted, OnRequest, Never }` с `ALL`, `wire()` (`"untrusted"`, `"on-request"`, `"never"`), `parse`.
  - `settings::ApiKey` (newtype, `ApiKey::parse(&str) -> Option<ApiKey>` — одно непустое слово, `expose() -> &str`, `Debug` = `ApiKey(***)`).
  - `settings::CodexSettings { cli: Option<PathBuf>, model: Option<String>, sandbox: Sandbox, approval: Approval, api_key: Option<ApiKey> }`.
  - `Settings { telegram, workspace_root, default_backend: BackendKind, claude, codex: CodexSettings, timeouts, updates }`.
  - `settings::Field::{DefaultBackend, Codex(CodexField)}`, `settings::CodexField { Cli, Model, Sandbox, Approval, ApiKey }`.
  - `settings::CodexDraft { cli: String, model: String, sandbox: Sandbox, approval: Approval, api_key: String }`; `Draft { .., default_backend: BackendKind, codex: CodexDraft }`.
  - `settings::Keys { token: String, api_key: String }`; `SettingsFile::to_draft(&self, keys: Keys) -> Result<Draft, FieldError>`.
  - `settings::{AgentsFile { default: Option<String> }, CodexFile { cli, model, sandbox, approval: Option<String> }}`; `SettingsFile { telegram, workspace, agents, claude, codex, timeouts, updates }`.

- [ ] **Step 1: Написать тесты**

`crates/hub-core/src/domain.rs`, в `backend_kind_parses_case_insensitively` добавить случаи:

```rust
    #[case("codex", Some(BackendKind::Codex))]
    #[case("CODEX", Some(BackendKind::Codex))]
```

`crates/hub-core/src/settings.rs`, тесты. В `minimal_draft_uses_defaults` добавить:

```rust
        assert_eq!(settings.default_backend, BackendKind::Claude);
        assert_eq!(settings.codex.cli, None);
        assert_eq!(settings.codex.model, None);
        assert_eq!(settings.codex.sandbox, Sandbox::WorkspaceWrite);
        assert_eq!(settings.codex.approval, Approval::OnRequest);
        assert_eq!(settings.codex.api_key, None);
```

Новые тесты:

```rust
    fn codex_draft() -> Draft {
        Draft {
            default_backend: BackendKind::Codex,
            codex: CodexDraft {
                cli: "~/bin/codex".to_owned(),
                model: " gpt-5.5-codex ".to_owned(),
                sandbox: Sandbox::ReadOnly,
                approval: Approval::Untrusted,
                api_key: " sk-test ".to_owned(),
            },
            ..draft()
        }
    }

    #[test]
    fn codex_values_are_parsed() {
        let settings = codex_draft().parse(&home()).unwrap();

        assert_eq!(settings.default_backend, BackendKind::Codex);
        assert_eq!(settings.codex.cli, Some(home().join("bin/codex")));
        assert_eq!(settings.codex.model.as_deref(), Some("gpt-5.5-codex"));
        assert_eq!(settings.codex.sandbox, Sandbox::ReadOnly);
        assert_eq!(settings.codex.approval, Approval::Untrusted);
        assert_eq!(settings.codex.api_key.as_ref().map(ApiKey::expose), Some("sk-test"));
    }

    #[rstest]
    #[case(CodexDraft { cli: "codex".to_owned(), ..CodexDraft::default() }, CodexField::Cli)]
    #[case(CodexDraft { model: "a b".to_owned(), ..CodexDraft::default() }, CodexField::Model)]
    #[case(CodexDraft { api_key: "sk a".to_owned(), ..CodexDraft::default() }, CodexField::ApiKey)]
    fn invalid_codex_values_are_reported(#[case] codex: CodexDraft, #[case] field: CodexField) {
        let errors = Draft { codex, ..draft() }.parse(&home()).unwrap_err();
        assert_eq!(errors.iter().map(|error| error.field).collect::<Vec<_>>(), [Field::Codex(field)]);
    }

    #[test]
    fn codex_file_round_trips_through_draft_without_the_key() {
        let settings = codex_draft().parse(&home()).unwrap();
        let file = SettingsFile::from_settings(&settings);
        let keys = Keys {
            token: settings.telegram.token.expose().to_owned(),
            api_key: "sk-test".to_owned(),
        };
        assert_eq!(file.to_draft(keys).unwrap().parse(&home()).unwrap(), settings);
        assert!(!toml::to_string(&file).unwrap().contains("sk-test"));
    }

    #[test]
    fn old_file_without_agents_and_codex_reads_with_defaults() {
        let file: SettingsFile = toml::from_str(
            "[telegram]\nchat = -100\nusers = [1]\n\n[workspace]\nroot = \"~/w\"\n",
        )
        .unwrap();
        let draft = file.to_draft(Keys { token: "1:a".to_owned(), api_key: String::new() }).unwrap();
        assert_eq!(draft.default_backend, BackendKind::Claude);
        assert_eq!(draft.codex, CodexDraft::default());
    }

    #[rstest]
    #[case("[agents]\ndefault = \"gpt\"\n", Field::DefaultBackend)]
    #[case("[codex]\nsandbox = \"yolo\"\n", Field::Codex(CodexField::Sandbox))]
    #[case("[codex]\napproval = \"always\"\n", Field::Codex(CodexField::Approval))]
    fn file_with_unknown_choice_is_rejected(#[case] extra: &str, #[case] field: Field) {
        let text = format!("[telegram]\nchat = -100\nusers = [1]\n\n[workspace]\nroot = \"~/w\"\n\n{extra}");
        let file: SettingsFile = toml::from_str(&text).unwrap();
        let keys = Keys { token: String::new(), api_key: String::new() };
        assert_eq!(file.to_draft(keys).unwrap_err().field, field);
    }

    #[test]
    fn debug_output_hides_api_key() {
        let settings = codex_draft().parse(&home()).unwrap();
        assert!(!format!("{settings:?}").contains("sk-test"));
        assert!(!format!("{:?}", codex_draft()).contains("sk-test"));
    }

    #[test]
    fn codex_choices_have_wire_names() {
        let sandboxes: Vec<_> = Sandbox::ALL.into_iter().map(Sandbox::wire).collect();
        assert_eq!(sandboxes, ["read-only", "workspace-write", "danger-full-access"]);
        let approvals: Vec<_> = Approval::ALL.into_iter().map(Approval::wire).collect();
        assert_eq!(approvals, ["untrusted", "on-request", "never"]);
        assert_eq!(Sandbox::parse("read-only"), Some(Sandbox::ReadOnly));
        assert_eq!(Approval::parse("yolo"), None);
    }
```

В существующих тестах `file_round_trips_through_draft` и `file_with_unknown_permission_mode_is_rejected` заменить `to_draft(token)` на `to_draft(Keys { token, api_key: String::new() })` (во втором — `Keys { token: String::new(), api_key: String::new() }`). В `use` тестового модуля добавить `use crate::domain::BackendKind;`.

`crates/hub-core/Cargo.toml`, `[dev-dependencies]`: `toml.workspace = true`.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-core`
Expected: FAIL — `no variant named Codex`, `cannot find type CodexDraft`.

- [ ] **Step 3: Реализовать `BackendKind::Codex`**

`crates/hub-core/src/domain.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    Claude,
    Codex,
}

impl BackendKind {
    pub const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
```

(`parse` без изменений).

- [ ] **Step 4: Реализовать настройки**

`crates/hub-core/src/settings.rs`. Импорт: `use crate::domain::{BackendKind, ChatId, UserId};`.

После `PermissionMode`:

```rust
/// What the OS lets commands started by Codex touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sandbox {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl Sandbox {
    pub const ALL: [Self; 3] = [Self::ReadOnly, Self::WorkspaceWrite, Self::DangerFullAccess];

    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|sandbox| sandbox.wire() == raw)
    }
}

/// When Codex asks the human before acting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    Untrusted,
    OnRequest,
    Never,
}

impl Approval {
    pub const ALL: [Self; 3] = [Self::Untrusted, Self::OnRequest, Self::Never];

    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|approval| approval.wire() == raw)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// A key is one non-empty word; surrounding whitespace is dropped.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        (!trimmed.is_empty() && !trimmed.chars().any(char::is_whitespace))
            .then(|| Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(***)")
    }
}
```

После `ClaudeSettings`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSettings {
    pub cli: Option<PathBuf>,
    pub model: Option<String>,
    pub sandbox: Sandbox,
    pub approval: Approval,
    /// Used only when Codex has no login yet or is logged in with a key.
    pub api_key: Option<ApiKey>,
}
```

`Settings`:

```rust
pub struct Settings {
    pub telegram: TelegramSettings,
    pub workspace_root: PathBuf,
    pub default_backend: BackendKind,
    pub claude: ClaudeSettings,
    pub codex: CodexSettings,
    pub timeouts: Timeouts,
    pub updates: UpdateCheck,
}
```

`Field` — добавить варианты в конец:

```rust
    DefaultBackend,
    Codex(CodexField),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexField {
    Cli,
    Model,
    Sandbox,
    Approval,
    ApiKey,
}
```

Черновик Codex — перед `Draft`:

```rust
#[derive(Clone, PartialEq, Eq)]
pub struct CodexDraft {
    pub cli: String,
    pub model: String,
    pub sandbox: Sandbox,
    pub approval: Approval,
    pub api_key: String,
}

impl Default for CodexDraft {
    fn default() -> Self {
        Self {
            cli: String::new(),
            model: String::new(),
            sandbox: Sandbox::WorkspaceWrite,
            approval: Approval::OnRequest,
            api_key: String::new(),
        }
    }
}

impl fmt::Debug for CodexDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { cli, model, sandbox, approval, api_key: _api_key } = self;
        f.debug_struct("CodexDraft")
            .field("cli", cli)
            .field("model", model)
            .field("sandbox", sandbox)
            .field("approval", approval)
            .field("api_key", &"***")
            .finish()
    }
}
```

`Draft`: добавить поля `pub default_backend: BackendKind` (после `workspace_root`) и `pub codex: CodexDraft` (после `budget`); в `Default` — `default_backend: BackendKind::Claude`, `codex: CodexDraft::default()`; в `Debug` — деструктурировать оба поля и вывести `.field("default_backend", default_backend)` и `.field("codex", codex)`.

`Draft::parse` — после `budget`:

```rust
        let codex_cli =
            check(&mut errors, Field::Codex(CodexField::Cli), parse_cli(&self.codex.cli, home));
        let codex_model =
            check(&mut errors, Field::Codex(CodexField::Model), parse_model(&self.codex.model));
        let api_key =
            check(&mut errors, Field::Codex(CodexField::ApiKey), parse_api_key(&self.codex.api_key));
```

добавить `Some(codex_cli), Some(codex_model), Some(api_key)` в кортеж `let (...) = (...) else` (после `Some(budget)` и `budget` соответственно) и в результат:

```rust
        Ok(Settings {
            telegram: TelegramSettings { token, chat, users },
            workspace_root,
            default_backend: self.default_backend,
            claude: ClaudeSettings { cli, model, permission_mode: self.permission_mode, budget },
            codex: CodexSettings {
                cli: codex_cli,
                model: codex_model,
                sandbox: self.codex.sandbox,
                approval: self.codex.approval,
                api_key,
            },
            timeouts: Timeouts { approval, background },
            updates: self.updates,
        })
```

`Draft::from_settings` — добавить:

```rust
            default_backend: settings.default_backend,
            codex: CodexDraft {
                cli: settings
                    .codex
                    .cli
                    .as_ref()
                    .map(|cli| cli.display().to_string())
                    .unwrap_or_default(),
                model: settings.codex.model.clone().unwrap_or_default(),
                sandbox: settings.codex.sandbox,
                approval: settings.codex.approval,
                api_key: settings
                    .codex
                    .api_key
                    .as_ref()
                    .map(|key| key.expose().to_owned())
                    .unwrap_or_default(),
            },
```

Парсер ключа рядом с `parse_model`:

```rust
/// Empty means no key: Codex uses its own login.
fn parse_api_key(raw: &str) -> Result<Option<ApiKey>, String> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    ApiKey::parse(raw).map(Some).ok_or_else(|| "Ключ не должен содержать пробелов".to_owned())
}
```

Файл. `SettingsFile`:

```rust
pub struct SettingsFile {
    pub telegram: TelegramFile,
    pub workspace: WorkspaceFile,
    #[serde(default)]
    pub agents: AgentsFile,
    #[serde(default)]
    pub claude: ClaudeFile,
    #[serde(default)]
    pub codex: CodexFile,
    #[serde(default)]
    pub timeouts: TimeoutsFile,
    #[serde(default)]
    pub updates: UpdatesFile,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentsFile {
    pub default: Option<String>,
}

/// The API key is kept in the OS keyring, not here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexFile {
    pub cli: Option<String>,
    pub model: Option<String>,
    pub sandbox: Option<String>,
    pub approval: Option<String>,
}

/// Secrets the file does not hold, read from the keyring by the caller.
pub struct Keys {
    pub token: String,
    pub api_key: String,
}
```

`SettingsFile::from_settings` — добавить:

```rust
            agents: AgentsFile { default: Some(settings.default_backend.name().to_owned()) },
            codex: CodexFile {
                cli: settings.codex.cli.as_ref().map(|cli| cli.display().to_string()),
                model: settings.codex.model.clone(),
                sandbox: Some(settings.codex.sandbox.wire().to_owned()),
                approval: Some(settings.codex.approval.wire().to_owned()),
            },
```

`SettingsFile::to_draft` — новая сигнатура и выбор значений из списков:

```rust
    /// The file as a form draft, so file and form share one parser.
    pub fn to_draft(&self, keys: Keys) -> Result<Draft, FieldError> {
        let Keys { token, api_key } = keys;
        let permission_mode = choice(
            self.claude.permission_mode.as_deref(),
            PermissionMode::Default,
            PermissionMode::parse,
            Field::PermissionMode,
            "Неизвестный режим",
        )?;
        let default_backend = choice(
            self.agents.default.as_deref(),
            BackendKind::Claude,
            BackendKind::parse,
            Field::DefaultBackend,
            "Неизвестный бэкенд",
        )?;
        let sandbox = choice(
            self.codex.sandbox.as_deref(),
            Sandbox::WorkspaceWrite,
            Sandbox::parse,
            Field::Codex(CodexField::Sandbox),
            "Неизвестная песочница",
        )?;
        let approval = choice(
            self.codex.approval.as_deref(),
            Approval::OnRequest,
            Approval::parse,
            Field::Codex(CodexField::Approval),
            "Неизвестный режим одобрений",
        )?;
        let defaults = Draft::default();
        Ok(Draft {
            token,
            chat: self.telegram.chat.to_string(),
            users: self.telegram.users.iter().map(u64::to_string).collect::<Vec<_>>().join(", "),
            workspace_root: self.workspace.root.clone(),
            default_backend,
            cli: self.claude.cli.clone().unwrap_or_default(),
            model: self.claude.model.clone().unwrap_or_default(),
            permission_mode,
            budget: self.claude.budget.clone().unwrap_or_default(),
            codex: CodexDraft {
                cli: self.codex.cli.clone().unwrap_or_default(),
                model: self.codex.model.clone().unwrap_or_default(),
                sandbox,
                approval,
                api_key,
            },
            approval_timeout: self
                .timeouts
                .approval_seconds
                .map_or(defaults.approval_timeout, |seconds| seconds.to_string()),
            background_timeout: self
                .timeouts
                .background_seconds
                .map_or(defaults.background_timeout, |seconds| seconds.to_string()),
            updates: match self.updates.check {
                Some(false) => UpdateCheck::Disabled,
                Some(true) | None => UpdateCheck::Enabled,
            },
        })
    }
}

fn choice<T>(
    raw: Option<&str>,
    default: T,
    parse: fn(&str) -> Option<T>,
    field: Field,
    unknown: &str,
) -> Result<T, FieldError> {
    match raw {
        None => Ok(default),
        Some(raw) => parse(raw).ok_or_else(|| FieldError { field, message: format!("{unknown} «{raw}»") }),
    }
}
```

- [ ] **Step 5: Потребители**

`crates/hub-app/src/config.rs:49`:

```rust
        let keys = Keys { token, api_key: String::new() };
        let draft = file.to_draft(keys).map_err(|error| corrupt(error.message))?;
```

(импорт `Keys` из `hub_core::settings`; ключ из хранилища подключается в задаче 13).

`crates/hub-telegram/src/agents.rs` — ветка `match session.backend`:

```rust
                BackendKind::Codex => {
                    // Replaced by the Codex backend once `hub-codex` exists.
                    let failed = AgentEvent::Failed("Codex пока не подключён".to_owned());
                    // Nobody listening means the topic's session is already gone.
                    let _ = conversation.events.send(failed).await;
                    inbox
                }
```

`rg "Settings \{" crates --glob '!**/settings.rs'` — если найдутся литералы `Settings`, добавить `default_backend: BackendKind::Claude` и `codex` по умолчанию (через `Draft::default().codex`, разобранный тем же `parse`, или явный `CodexSettings { cli: None, model: None, sandbox: Sandbox::WorkspaceWrite, approval: Approval::OnRequest, api_key: None }`).

- [ ] **Step 6: Прогнать всё**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A crates
git commit -m "Настройки Codex и бэкенд по умолчанию" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: `/backend` и бэкенд по умолчанию в командах

**Files:**
- Modify: `crates/hub-core/src/commands.rs`
- Modify: `crates/hub-telegram/src/hub.rs` (`command`, `session`, тесты)
- Modify: `crates/hub-telegram/src/texts.rs` (справка, новые тексты, тесты)

**Interfaces:**
- Consumes: `Settings.default_backend` (задача 4).
- Produces:
  - `hub_core::commands::Command::Backend`.
  - `hub_core::commands::parse_new_args(args: &[&str], default: BackendKind) -> NewSessionArgs`.
  - `hub_core::commands::BackendDecision { Show(TopicSession), Unknown(String), AlreadySelected(TopicSession), Switch(TopicSession) }`.
  - `hub_core::commands::decide_backend(args: &[&str], current: &TopicSession) -> BackendDecision`.
  - `hub_telegram::texts::{CURRENT_SESSION, BACKEND_ALREADY, BACKEND_SWITCHED, unknown_backend(name: &str) -> String}`.
  - Константа `DEFAULT_BACKEND` удаляется.

- [ ] **Step 1: Написать тесты команд**

`crates/hub-core/src/commands.rs`, тесты. Заменить `new_args`:

```rust
    #[rstest]
    #[case(&[], BackendKind::Codex, None)]
    #[case(&["claude"], BackendKind::Claude, None)]
    #[case(&["Claude", "shop/backend"], BackendKind::Claude, Some("shop/backend"))]
    #[case(&["codex", "shop"], BackendKind::Codex, Some("shop"))]
    #[case(&["shop/backend"], BackendKind::Codex, Some("shop/backend"))]
    #[case(&["my", "dir"], BackendKind::Codex, Some("my dir"))]
    fn new_args_fall_back_to_the_default_backend(
        #[case] args: &[&str],
        #[case] backend: BackendKind,
        #[case] cwd: Option<&str>,
    ) {
        assert_eq!(
            parse_new_args(args, BackendKind::Codex),
            NewSessionArgs { backend, cwd: cwd.map(str::to_owned) }
        );
    }

    fn current() -> TopicSession {
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        TopicSession::fresh(BackendKind::Claude, cwd).with_session(SessionId::parse("s-1"))
    }

    #[test]
    fn backend_without_a_name_shows_the_session() {
        assert_eq!(decide_backend(&[], &current()), BackendDecision::Show(current()));
    }

    #[test]
    fn same_backend_is_already_selected() {
        assert_eq!(decide_backend(&["CLAUDE"], &current()), BackendDecision::AlreadySelected(current()));
    }

    #[test]
    fn other_backend_keeps_the_directory_and_drops_the_session() {
        let switched = TopicSession::fresh(BackendKind::Codex, current().cwd);
        assert_eq!(decide_backend(&["codex"], &current()), BackendDecision::Switch(switched));
    }

    #[rstest]
    #[case(&["gpt"], "gpt")]
    #[case(&["codex", "now"], "codex now")]
    fn unknown_backend_names_what_was_typed(#[case] args: &[&str], #[case] name: &str) {
        assert_eq!(decide_backend(args, &current()), BackendDecision::Unknown(name.to_owned()));
    }
```

В `input_is_classified` добавить случай:

```rust
    #[case("/backend codex", Input::Command { command: Command::Backend, args: vec!["codex"] })]
```

Импорт в тестах: `use crate::domain::{AbsolutePath, SessionId, TopicSession};`.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-core commands`
Expected: FAIL — `no variant named Backend`, `this function takes 1 argument`.

- [ ] **Step 3: Реализовать команды**

`crates/hub-core/src/commands.rs`:
- импорт `use crate::domain::{BackendKind, TopicSession};`, удалить `pub const DEFAULT_BACKEND`;
- `Command::Backend` и в `Command::parse` строка `"backend" => Some(Self::Backend),`;
- `parse_new_args`:

```rust
/// `/new [backend] [path]`; a first word that is not a backend name starts the path.
#[must_use]
pub fn parse_new_args(args: &[&str], default: BackendKind) -> NewSessionArgs {
    match args.split_first() {
        None => NewSessionArgs { backend: default, cwd: None },
        Some((first, rest)) => match BackendKind::parse(first) {
            Some(backend) => NewSessionArgs { backend, cwd: join_path_args(rest) },
            None => NewSessionArgs { backend: default, cwd: join_path_args(args) },
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendDecision {
    Show(TopicSession),
    Unknown(String),
    AlreadySelected(TopicSession),
    Switch(TopicSession),
}

/// `/backend [name]`: no name shows the current session.
#[must_use]
pub fn decide_backend(args: &[&str], current: &TopicSession) -> BackendDecision {
    match args {
        [] => BackendDecision::Show(current.clone()),
        [name] => match BackendKind::parse(name) {
            None => BackendDecision::Unknown((*name).to_owned()),
            Some(kind) if kind == current.backend => BackendDecision::AlreadySelected(current.clone()),
            // One agent cannot continue another's session.
            Some(kind) => BackendDecision::Switch(TopicSession::fresh(kind, current.cwd.clone())),
        },
        [_, _, ..] => BackendDecision::Unknown(args.join(" ")),
    }
}
```

- [ ] **Step 4: Тесты хаба и текстов**

`crates/hub-telegram/src/texts.rs`, тест `help_names_root_uploads_and_backends`: `assert!(text.ends_with("Бэкенды: claude, codex"));` и добавить `assert!(text.contains("/backend [claude|codex] — сменить агента в этой теме (сброс контекста)"));`. Новый тест:

```rust
    #[test]
    fn unknown_backend_lists_the_known_ones() {
        assert_eq!(unknown_backend("gpt"), "⚠️ Неизвестный бэкенд gpt. Доступны: claude, codex");
    }
```

`crates/hub-telegram/src/hub.rs`, тесты. Разделить `world_with` так, чтобы можно было задать бэкенд по умолчанию:

```rust
    fn world_with(agents: FakeAgents, messenger: FakeMessenger) -> World {
        world_from(agents, messenger, BackendKind::Claude)
    }

    fn world_from(agents: FakeAgents, messenger: FakeMessenger, default_backend: BackendKind) -> World {
        // тело прежнего world_with; в Draft добавить `default_backend,`
    }
```

Новые тесты:

```rust
    #[tokio::test]
    async fn backend_is_shown_switched_and_validated() {
        let world = world(FakeAgents::default());
        world.send(text(Some(7), 1, "/backend")).await;
        eventually("shown", || {
            world.texts().iter().any(|t| t.starts_with("ℹ️ Текущая сессия") && t.contains("backend: claude"))
        })
        .await;
        world.send(text(Some(7), 2, "/backend claude")).await;
        eventually("already", || world.texts().iter().any(|t| t.starts_with("ℹ️ Бэкенд уже выбран")))
            .await;
        world.send(text(Some(7), 3, "/backend gpt")).await;
        eventually("unknown", || {
            world.texts().contains(&"⚠️ Неизвестный бэкенд gpt. Доступны: claude, codex".to_owned())
        })
        .await;
        world.send(text(Some(7), 4, "/backend codex")).await;
        eventually("switched", || {
            world.texts().iter().any(|t| t.starts_with("🔀 Бэкенд изменён") && t.contains("backend: codex"))
        })
        .await;
    }

    #[tokio::test]
    async fn backend_switch_is_refused_while_running() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.send(text(Some(7), 1, "долго")).await;
        eventually("running", || world.running()).await;
        world.send(text(Some(7), 2, "/backend codex")).await;
        eventually("refused", || world.texts().contains(&texts::ALREADY_RUNNING.to_owned())).await;
        assert!(!world.texts().iter().any(|t| t.starts_with("🔀")));
        world.send(HubMessage::Stop(KEY)).await;
    }

    #[tokio::test]
    async fn new_without_a_backend_uses_the_default_from_settings() {
        let world = world_from(FakeAgents::default(), FakeMessenger::default(), BackendKind::Codex);
        world.send(text(Some(7), 1, "/new project")).await;
        eventually("new", || {
            world.texts().iter().any(|t| t.starts_with("🆕 Новая сессия") && t.contains("backend: codex"))
        })
        .await;
    }
```

(импорт `BackendKind` в тестовом модуле).

- [ ] **Step 5: Убедиться, что тесты падают**

Run: `cargo test -p hub-telegram`
Expected: FAIL — `non-exhaustive patterns: Command::Backend`, `cannot find value unknown_backend`.

- [ ] **Step 6: Реализовать в хабе**

`crates/hub-telegram/src/texts.rs`:

```rust
pub const CURRENT_SESSION: &str = "ℹ️ Текущая сессия";
pub const BACKEND_ALREADY: &str = "ℹ️ Бэкенд уже выбран";
pub const BACKEND_SWITCHED: &str = "🔀 Бэкенд изменён";

#[must_use]
pub fn unknown_backend(name: &str) -> String {
    format!("⚠️ Неизвестный бэкенд {name}. Доступны: {}", backend_names())
}

fn backend_names() -> String {
    BackendKind::ALL.iter().map(|kind| kind.name()).collect::<Vec<_>>().join(", ")
}
```

В `help` использовать `backend_names()` вместо локального `backends` и после строки `/new` добавить строку:

```
         /backend [claude|codex] — сменить агента в этой теме (сброс контекста)\n\
```

`crates/hub-telegram/src/hub.rs`:
- импорт: `use hub_core::commands::{BackendDecision, Command, NewSessionArgs, decide_backend, join_path_args, parse_new_args, ...}` (без `DEFAULT_BACKEND`);
- `Command::New`: `let default = self.settings.borrow().default_backend; let NewSessionArgs { backend, cwd } = parse_new_args(args, default);`;
- `fn session`: `let default = self.settings.borrow().default_backend; let session = TopicSession::fresh(default, self.root()?);`;
- новая ветка после `Command::Reset`:

```rust
            Command::Backend => {
                let current = match self.session(key) {
                    Ok(current) => current,
                    Err(error) => {
                        self.say(Target::Topic(key), texts::warning(&error));
                        return;
                    }
                };
                match decide_backend(args, &current) {
                    BackendDecision::Show(session) => {
                        self.say(Target::Topic(key), texts::describe(texts::CURRENT_SESSION, &session));
                    }
                    BackendDecision::Unknown(name) => {
                        self.say(Target::Topic(key), texts::unknown_backend(&name));
                    }
                    BackendDecision::AlreadySelected(session) => {
                        self.say(Target::Topic(key), texts::describe(texts::BACKEND_ALREADY, &session));
                    }
                    BackendDecision::Switch(session) => {
                        if self.refuse_if_running(key) {
                            return;
                        }
                        self.put(key, session.clone());
                        self.say(Target::Topic(key), texts::describe(texts::BACKEND_SWITCHED, &session));
                    }
                }
            }
```

- [ ] **Step 7: Прогнать всё**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add -A crates
git commit -m "Команда /backend и бэкенд по умолчанию из настроек" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 6: Крейт `hub-codex` и поиск `codex`

**Files:**
- Create: `crates/hub-codex/Cargo.toml`, `crates/hub-codex/src/lib.rs`, `crates/hub-codex/src/version.rs`
- Modify: `Cargo.toml` (members, `hub-codex` в `[workspace.dependencies]`)

**Interfaces:**
- Consumes: `hub_agent::cli::{Version, VersionError, locate, version_output}` (задача 1).
- Produces (`hub_codex::version`): `MIN_VERSION = 0.160.0`, `CliError { Missing, Version(VersionError), Unrecognized(String), TooOld { found } }`, `parse_version(output: &str) -> Option<Version>`, `locate(configured: Option<&Path>) -> Result<PathBuf, CliError>`, `async fn check(path: &Path) -> Result<Version, CliError>`, `pub use hub_agent::cli::Version`.

- [ ] **Step 1: Создать крейт**

Корневой `Cargo.toml`: в `members` добавить `"crates/hub-codex"` после `hub-claude`, в `[workspace.dependencies]` после `hub-claude`:

```toml
hub-codex = { path = "crates/hub-codex" }
```

`crates/hub-codex/Cargo.toml`:

```toml
[package]
name = "hub-codex"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
base64.workspace = true
futures.workspace = true
hub-agent.workspace = true
hub-core.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["io-util", "macros", "process", "rt", "sync", "time"] }
tokio-util = { workspace = true, features = ["codec"] }
tracing.workspace = true

[dev-dependencies]
rstest.workspace = true
tokio = { workspace = true, features = ["io-util", "macros", "process", "rt", "rt-multi-thread", "sync", "time"] }

[lints]
workspace = true
```

`crates/hub-codex/src/lib.rs`:

```rust
pub mod version;
```

- [ ] **Step 2: Написать тесты**

`crates/hub-codex/src/version.rs`, внизу:

```rust
#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("codex-cli 0.160.0\n", Some(Version { major: 0, minor: 160, patch: 0 }))]
    #[case("codex-cli 1.2.3", Some(Version { major: 1, minor: 2, patch: 3 }))]
    #[case("", None)]
    #[case("codex-cli", None)]
    #[case("codex-cli 0.160", None)]
    fn versions_are_parsed(#[case] output: &str, #[case] expected: Option<Version>) {
        assert_eq!(parse_version(output), expected);
    }

    #[test]
    fn minimum_is_the_verified_protocol() {
        assert_eq!(MIN_VERSION.to_string(), "0.160.0");
        assert!(Version { major: 0, minor: 159, patch: 99 } < MIN_VERSION);
    }

    #[test]
    fn configured_path_is_used_as_is() {
        // A `codex.cmd` from npm on Windows is found by `which` and must be run as found.
        let path = std::env::temp_dir().join("codex.cmd");
        assert_eq!(locate(Some(&path)).unwrap(), path);
    }

    #[tokio::test]
    async fn missing_binary_is_a_spawn_error() {
        let path = std::env::temp_dir().join("definitely-not-codex-binary");
        assert!(matches!(check(&path).await, Err(CliError::Version(VersionError::Spawn { .. }))));
    }
}
```

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p hub-codex`
Expected: FAIL — `cannot find function parse_version`.

- [ ] **Step 4: Реализовать**

Над тестами:

```rust
//! Which Codex CLI to run and whether it speaks the app-server protocol this hub expects.

use std::path::{Path, PathBuf};

use hub_agent::cli::{self, VersionError};
pub use hub_agent::cli::Version;

/// The app-server protocol this hub speaks was verified against this version.
pub const MIN_VERSION: Version = Version { major: 0, minor: 160, patch: 0 };
const CLI_NAME: &str = "codex";

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("Codex не найден: укажите путь в настройках или установите `codex` в PATH")]
    Missing,
    #[error(transparent)]
    Version(#[from] VersionError),
    #[error("непонятный ответ `codex --version`: {0}")]
    Unrecognized(String),
    #[error("Codex {found} слишком старый, нужен {} или новее", MIN_VERSION)]
    TooOld { found: Version },
}

/// `codex-cli 0.160.0` → 0.160.0.
#[must_use]
pub fn parse_version(output: &str) -> Option<Version> {
    output.split_whitespace().find_map(Version::parse)
}

pub fn locate(configured: Option<&Path>) -> Result<PathBuf, CliError> {
    cli::locate(CLI_NAME, configured).ok_or(CliError::Missing)
}

pub async fn check(path: &Path) -> Result<Version, CliError> {
    let text = cli::version_output(path).await?;
    let found =
        parse_version(&text).ok_or_else(|| CliError::Unrecognized(text.trim().to_owned()))?;
    if found < MIN_VERSION { Err(CliError::TooOld { found }) } else { Ok(found) }
}
```

- [ ] **Step 5: Прогнать**

Run: `cargo test -p hub-codex && cargo clippy --workspace --all-targets`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A crates Cargo.toml Cargo.lock
git commit -m "Крейт hub-codex: поиск и проверка версии codex" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: JSON-RPC по stdio (`rpc`)

**Files:**
- Create: `crates/hub-codex/src/rpc.rs`, `crates/hub-codex/src/testing.rs`
- Modify: `crates/hub-codex/src/lib.rs`

**Interfaces:**
- Produces (`hub_codex::rpc`):
  - `RpcError { Remote { code: i64, message: String }, Closed, Protocol(String), Timeout { method: String } }` (`thiserror`, `Clone`, `PartialEq`, `Eq`).
  - `RequestError { Unsupported(String), Malformed(String) }`.
  - `Notification { method: String, params: Value }`.
  - `type Handler = Arc<dyn Fn(String, Value) -> BoxFuture<'static, Result<Value, RequestError>> + Send + Sync>`.
  - `RpcClient` (`Clone`): `async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError>`, `async fn notify(&self, method: &str) -> Result<(), RpcError>`, `fn close(&self)`.
  - `Connection { client: RpcClient, notifications: mpsc::UnboundedReceiver<Notification>, reader: JoinHandle<()> }`.
  - `fn connect<R, W>(reader: R, writer: W, handler: Handler, timeout: Duration) -> Connection` (`R: AsyncRead + Unpin + Send + 'static`, `W: AsyncWrite + Unpin + Send + 'static`).
  - `METHOD_NOT_FOUND = -32601`, `INVALID_PARAMS = -32602`.
- Produces (`#[cfg(test)] crate::testing`): `Peer { async fn read(&mut self) -> Option<Value>, async fn write(&mut self, Value) }`, `pair(handler, timeout) -> (Connection, Peer)`, `refusing() -> Handler`.

- [ ] **Step 1: Тестовый собеседник**

`crates/hub-codex/src/lib.rs`:

```rust
pub mod rpc;
#[cfg(test)]
mod testing;
pub mod version;
```

`crates/hub-codex/src/testing.rs`:

```rust
//! The app-server side of a duplex pipe, and a scripted human, for tests.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf};

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
```

- [ ] **Step 2: Написать тесты `rpc`**

`crates/hub-codex/src/rpc.rs`, внизу:

```rust
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
        peer.write(json!({"id": id(&sent), "error": {"code": -32000, "message": "no active turn"}}))
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
        peer.write(json!({"id": "s-1", "method": "item/commandExecution/requestApproval", "params": {}}))
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
            Some(json!({"id": 7, "error": {"code": METHOD_NOT_FOUND, "message": "unsupported: mystery"}}))
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
        assert_eq!(request.await.unwrap(), Err(RpcError::Timeout { method: "initialize".to_owned() }));
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
```

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p hub-codex rpc`
Expected: FAIL — `cannot find function connect`.

- [ ] **Step 4: Реализовать `rpc`**

Над тестами:

```rust
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
    let link = Link { outbox: outbox.clone(), pending: Arc::clone(&pending), notify, handler };
    let reader = tokio::spawn(read(reader, link));
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

async fn read<R: AsyncRead + Unpin>(reader: R, link: Link) {
    let mut lines = FramedRead::new(reader, LinesCodec::new_with_max_length(MAX_LINE));
    let mut answering = JoinSet::new();
    loop {
        tokio::select! {
            line = lines.next() => match line {
                Some(Ok(line)) => link.dispatch(&line, &mut answering),
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
    lock(&link.pending).take();
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
```

- [ ] **Step 5: Прогнать**

Run: `cargo test -p hub-codex && cargo clippy --workspace --all-targets`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A crates
git commit -m "hub-codex: JSON-RPC клиент к app-server" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: Протокол Codex (`protocol`)

Чистые функции: параметры вызовов, разбор ответов, перевод уведомлений в события хаба.

**Files:**
- Create: `crates/hub-codex/src/protocol.rs`
- Modify: `crates/hub-codex/src/lib.rs` (`pub mod protocol;`)

**Interfaces:**
- Consumes: `hub_agent::tools::{SEND_FILE, ASK_USER, SEND_FILE_DESCRIPTION, ASK_USER_DESCRIPTION, TOOL_SUMMARY_LIMIT, send_file_schema, ask_user_schema}`; `hub_core::settings::{ApiKey, CodexSettings}`; `crate::rpc::{Notification, RpcError}`.
- Produces (`hub_codex::protocol`):
  - `TurnId` (newtype, `TurnId::new(String)`, `as_str()`).
  - `Call { Initialize, Initialized, AccountRead, Login, ThreadStart, ThreadResume, TurnStart, TurnSteer }`, `Call::method(self) -> &'static str`.
  - `CodexAuth { ApiKey, ChatGpt, Other, NotRequired, Missing }`.
  - `SHELL = "shell"`, `PATCH = "patch"`, `WEB_SEARCH = "web_search"`, `UNNAMED_FILE_CHANGE = "изменение файлов"`.
  - `initialize_params() -> Value`, `login_params(&ApiKey) -> Value`, `auth_state(&Value) -> CodexAuth`, `hub_tools() -> Value`, `open_thread(&TopicSession, &CodexSettings) -> (Call, Value)`, `parse_thread_id(&Value) -> Result<SessionId, RpcError>`, `parse_turn_id(&Value) -> Result<TurnId, RpcError>`, `turn_params(&SessionId, &Prompt) -> Value`, `steer_params(&SessionId, &TurnId, &Prompt) -> Value`, `user_input(&Prompt) -> Value`, `tool_call(&Value) -> Option<ToolUse>`, `agent_text(&Value) -> Option<String>`.
  - `Translation { events: Vec<AgentEvent>, completed: Option<TurnId> }`; `TurnTracker::new(SessionId)`, `TurnTracker::translate(&mut self, &Notification) -> Result<Translation, RpcError>`.

- [ ] **Step 1: Написать тесты**

`crates/hub-codex/src/protocol.rs`, внизу:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hub_core::domain::{AbsolutePath, BackendKind, ImageMediaType};
    use hub_core::settings::{Approval, Sandbox};
    use rstest::rstest;

    use super::*;

    fn cwd() -> AbsolutePath {
        AbsolutePath::new(PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" })).unwrap()
    }

    fn settings(model: Option<&str>) -> CodexSettings {
        CodexSettings {
            cli: None,
            model: model.map(str::to_owned),
            sandbox: Sandbox::WorkspaceWrite,
            approval: Approval::OnRequest,
            api_key: None,
        }
    }

    fn notification(method: &str, params: Value) -> Notification {
        Notification { method: method.to_owned(), params }
    }

    fn thread() -> SessionId {
        SessionId::parse("t-1").unwrap()
    }

    fn turn(id: &str) -> TurnId {
        TurnId::new(id.to_owned())
    }

    #[test]
    fn new_session_starts_a_thread_with_hub_tools() {
        let session = TopicSession::fresh(BackendKind::Codex, cwd());
        assert_eq!(
            open_thread(&session, &settings(Some("gpt-5.5-codex"))),
            (
                Call::ThreadStart,
                json!({
                    "cwd": cwd().as_path().display().to_string(),
                    "sandbox": "workspace-write",
                    "approvalPolicy": "on-request",
                    "approvalsReviewer": "user",
                    "model": "gpt-5.5-codex",
                    "dynamicTools": hub_tools(),
                })
            )
        );
    }

    #[test]
    fn saved_session_resumes_its_thread_without_tools() {
        let session = TopicSession::fresh(BackendKind::Codex, cwd()).with_session(Some(thread()));
        assert_eq!(
            open_thread(&session, &settings(None)),
            (
                Call::ThreadResume,
                json!({
                    "cwd": cwd().as_path().display().to_string(),
                    "sandbox": "workspace-write",
                    "approvalPolicy": "on-request",
                    "approvalsReviewer": "user",
                    "threadId": "t-1",
                })
            )
        );
    }

    #[test]
    fn hub_tools_are_send_file_and_ask_user() {
        let names: Vec<_> = hub_tools()
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        assert_eq!(names, ["send_file", "ask_user"]);
    }

    #[test]
    fn images_go_first_as_data_urls() {
        let image = Image { media: ImageMediaType::Png, data: vec![1, 2, 3] };
        let prompt = Prompt::new("что на фото?".to_owned(), vec![image]).unwrap();
        assert_eq!(
            user_input(&prompt),
            json!([
                {"type": "image", "url": "data:image/png;base64,AQID"},
                {"type": "text", "text": "что на фото?"},
            ])
        );
    }

    #[test]
    fn blank_text_is_not_sent() {
        let image = Image { media: ImageMediaType::Jpeg, data: vec![0] };
        let prompt = Prompt::new("  ".to_owned(), vec![image]).unwrap();
        assert_eq!(user_input(&prompt), json!([{"type": "image", "url": "data:image/jpeg;base64,AA=="}]));
    }

    #[test]
    fn steer_names_the_expected_turn() {
        let prompt = Prompt::new("ещё".to_owned(), Vec::new()).unwrap();
        assert_eq!(
            steer_params(&thread(), &turn("u-1"), &prompt),
            json!({"threadId": "t-1", "expectedTurnId": "u-1", "input": [{"type": "text", "text": "ещё"}]})
        );
    }

    #[rstest]
    #[case(json!({"account": {"type": "apiKey"}}), CodexAuth::ApiKey)]
    #[case(json!({"account": {"type": "chatgpt"}}), CodexAuth::ChatGpt)]
    #[case(json!({"account": {"type": "enterprise"}}), CodexAuth::Other)]
    #[case(json!({"account": null, "requiresOpenaiAuth": false}), CodexAuth::NotRequired)]
    #[case(json!({"account": null, "requiresOpenaiAuth": true}), CodexAuth::Missing)]
    #[case(json!({}), CodexAuth::Missing)]
    fn auth_state_is_read(#[case] result: Value, #[case] expected: CodexAuth) {
        assert_eq!(auth_state(&result), expected);
    }

    #[rstest]
    #[case(json!({"thread": {"id": "t-1"}}), Some(thread()))]
    #[case(json!({"thread": {}}), None)]
    fn thread_id_is_parsed(#[case] result: Value, #[case] expected: Option<SessionId>) {
        assert_eq!(parse_thread_id(&result).ok(), expected);
    }

    #[rstest]
    #[case(json!({"turn": {"id": "u-1"}}), Some(turn("u-1")))]
    #[case(json!({"turn": null}), None)]
    fn turn_id_is_parsed(#[case] result: Value, #[case] expected: Option<TurnId>) {
        assert_eq!(parse_turn_id(&result).ok(), expected);
    }

    #[rstest]
    #[case(json!({"type": "commandExecution", "command": "npm test"}), Some(("shell", "npm test")))]
    #[case(json!({"type": "fileChange", "changes": [{"path": "a.rs"}, {"path": "b.rs"}]}), Some(("patch", "a.rs, b.rs")))]
    #[case(json!({"type": "fileChange", "changes": []}), Some(("patch", "изменение файлов")))]
    #[case(json!({"type": "mcpToolCall", "server": "gh", "tool": "pr", "arguments": {"n": 1}}), Some(("gh/pr", r#"{"n":1}"#)))]
    #[case(json!({"type": "mcpToolCall", "server": "gh", "tool": "pr"}), Some(("gh/pr", "{}")))]
    #[case(json!({"type": "webSearch", "query": "rust"}), Some(("web_search", "rust")))]
    #[case(json!({"type": "reasoning"}), None)]
    #[case(json!({"type": "dynamicToolCall", "tool": "send_file"}), None)]
    fn started_items_become_tool_lines(#[case] item: Value, #[case] expected: Option<(&str, &str)>) {
        let expected = expected
            .map(|(tool, summary)| ToolUse { tool: tool.to_owned(), summary: summary.to_owned() });
        assert_eq!(tool_call(&item), expected);
    }

    #[test]
    fn long_commands_are_truncated() {
        let command = "x".repeat(1000);
        let call = tool_call(&json!({"type": "commandExecution", "command": command})).unwrap();
        assert_eq!(call.summary.chars().count(), 600);
    }

    #[test]
    fn a_completed_turn_reports_the_latest_token_total() {
        let mut tracker = TurnTracker::new(thread());
        for total in [41, 12345] {
            let usage = json!({"tokenUsage": {"total": {"totalTokens": total}}});
            tracker.translate(&notification("thread/tokenUsage/updated", usage)).unwrap();
        }
        let completed = json!({"turn": {"id": "u-1", "status": "completed"}});
        assert_eq!(
            tracker.translate(&notification("turn/completed", completed)).unwrap(),
            Translation {
                events: vec![AgentEvent::Finished(Finished {
                    session: thread(),
                    usage: Usage::Codex { tokens: Some(12345) },
                    background: 0,
                })],
                completed: Some(turn("u-1")),
            }
        );
    }

    #[rstest]
    #[case(json!({"id": "u-1", "status": "interrupted"}), "Ход Codex прерван")]
    #[case(json!({"id": "u-1", "status": "failed", "error": {"message": "quota exceeded"}}), "quota exceeded")]
    #[case(json!({"id": "u-1", "status": "failed"}), "Ход Codex завершился ошибкой")]
    fn an_unsuccessful_turn_fails(#[case] ended: Value, #[case] reason: &str) {
        let translation = TurnTracker::new(thread())
            .translate(&notification("turn/completed", json!({"turn": ended})))
            .unwrap();
        assert_eq!(
            translation,
            Translation { events: vec![AgentEvent::Failed(reason.to_owned())], completed: Some(turn("u-1")) }
        );
    }

    #[test]
    fn turn_completed_without_an_id_is_a_protocol_error() {
        let outcome = TurnTracker::new(thread())
            .translate(&notification("turn/completed", json!({"turn": {}})));
        assert!(matches!(outcome, Err(RpcError::Protocol(_))));
    }

    #[rstest]
    #[case(notification("item/completed", json!({"item": {"type": "agentMessage", "text": "Готово"}})), vec![AgentEvent::AssistantText("Готово".to_owned())])]
    #[case(notification("item/completed", json!({"item": {"type": "agentMessage", "text": " "}})), vec![])]
    #[case(notification("item/started", json!({"item": {"type": "webSearch", "query": "q"}})), vec![AgentEvent::ToolCall(ToolUse { tool: "web_search".to_owned(), summary: "q".to_owned() })])]
    #[case(notification("item/agentMessage/delta", json!({"delta": "Го"})), vec![])]
    fn notifications_become_events(#[case] incoming: Notification, #[case] events: Vec<AgentEvent>) {
        let translation = TurnTracker::new(thread()).translate(&incoming).unwrap();
        assert_eq!(translation, Translation { events, completed: None });
    }
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-codex protocol`
Expected: FAIL — модуль `protocol` не найден.

- [ ] **Step 3: Реализовать**

`crates/hub-codex/src/lib.rs`: добавить `pub mod protocol;`.

`crates/hub-codex/src/protocol.rs`, над тестами:

```rust
//! Pure mapping between Codex app-server messages and agent-hub types.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hub_agent::tools::{
    ASK_USER, ASK_USER_DESCRIPTION, SEND_FILE, SEND_FILE_DESCRIPTION, TOOL_SUMMARY_LIMIT,
    ask_user_schema, send_file_schema,
};
use hub_core::domain::{AgentEvent, Finished, Image, Prompt, SessionId, ToolUse, TopicSession, Usage};
use hub_core::render::truncate;
use hub_core::settings::{ApiKey, CodexSettings};
use serde_json::{Map, Value, json};

use crate::rpc::{Notification, RpcError};

// Approvals go to the human in Telegram, never to Codex's own reviewer agent.
const APPROVALS_REVIEWER: &str = "user";
pub const SHELL: &str = "shell";
pub const PATCH: &str = "patch";
pub const WEB_SEARCH: &str = "web_search";
pub const UNNAMED_FILE_CHANGE: &str = "изменение файлов";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnId(String);

impl TurnId {
    #[must_use]
    pub fn new(id: String) -> Self {
        Self(id)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Client-to-server methods agent-hub uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Initialize,
    Initialized,
    AccountRead,
    Login,
    ThreadStart,
    ThreadResume,
    TurnStart,
    TurnSteer,
}

impl Call {
    #[must_use]
    pub const fn method(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::Initialized => "initialized",
            Self::AccountRead => "account/read",
            Self::Login => "account/login/start",
            Self::ThreadStart => "thread/start",
            Self::ThreadResume => "thread/resume",
            Self::TurnStart => "turn/start",
            Self::TurnSteer => "turn/steer",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAuth {
    ApiKey,
    ChatGpt,
    Other,
    NotRequired,
    Missing,
}

#[derive(Debug, Default, PartialEq)]
pub struct Translation {
    pub events: Vec<AgentEvent>,
    /// The turn this notification completed, if any.
    pub completed: Option<TurnId>,
}

impl Translation {
    fn of(event: Option<AgentEvent>) -> Self {
        Self { events: event.into_iter().collect(), completed: None }
    }
}

#[must_use]
pub fn initialize_params() -> Value {
    // Dynamic tools are part of the experimental app-server API.
    json!({
        "clientInfo": {"name": "agent-hub", "title": "agent-hub", "version": env!("CARGO_PKG_VERSION")},
        "capabilities": {"experimentalApi": true},
    })
}

#[must_use]
pub fn login_params(key: &ApiKey) -> Value {
    json!({"type": "apiKey", "apiKey": key.expose()})
}

#[must_use]
pub fn auth_state(result: &Value) -> CodexAuth {
    match result.pointer("/account/type").and_then(Value::as_str) {
        Some("apiKey") => CodexAuth::ApiKey,
        Some("chatgpt") => CodexAuth::ChatGpt,
        Some(_) => CodexAuth::Other,
        None if result.get("requiresOpenaiAuth") == Some(&Value::Bool(false)) => {
            CodexAuth::NotRequired
        }
        None => CodexAuth::Missing,
    }
}

#[must_use]
pub fn hub_tools() -> Value {
    json!([
        {"type": "function", "name": SEND_FILE, "description": SEND_FILE_DESCRIPTION, "inputSchema": send_file_schema()},
        {"type": "function", "name": ASK_USER, "description": ASK_USER_DESCRIPTION, "inputSchema": ask_user_schema()},
    ])
}

/// `thread/start` for a new topic session, `thread/resume` for a saved one.
#[must_use]
pub fn open_thread(session: &TopicSession, settings: &CodexSettings) -> (Call, Value) {
    let mut params = Map::new();
    params.insert("cwd".to_owned(), Value::from(session.cwd.as_path().display().to_string()));
    params.insert("sandbox".to_owned(), Value::from(settings.sandbox.wire()));
    params.insert("approvalPolicy".to_owned(), Value::from(settings.approval.wire()));
    params.insert("approvalsReviewer".to_owned(), Value::from(APPROVALS_REVIEWER));
    if let Some(model) = &settings.model {
        params.insert("model".to_owned(), Value::from(model.as_str()));
    }
    match &session.session {
        None => {
            params.insert("dynamicTools".to_owned(), hub_tools());
            (Call::ThreadStart, Value::Object(params))
        }
        Some(thread) => {
            // A resumed thread keeps the dynamic tools it was started with.
            params.insert("threadId".to_owned(), Value::from(thread.as_str()));
            (Call::ThreadResume, Value::Object(params))
        }
    }
}

pub fn parse_thread_id(result: &Value) -> Result<SessionId, RpcError> {
    result
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .and_then(SessionId::parse)
        .ok_or_else(|| RpcError::Protocol(format!("thread response without an id: {result}")))
}

pub fn parse_turn_id(result: &Value) -> Result<TurnId, RpcError> {
    result
        .pointer("/turn/id")
        .and_then(Value::as_str)
        .map(|id| TurnId(id.to_owned()))
        .ok_or_else(|| RpcError::Protocol(format!("turn response without an id: {result}")))
}

#[must_use]
pub fn turn_params(thread: &SessionId, prompt: &Prompt) -> Value {
    json!({"threadId": thread.as_str(), "input": user_input(prompt)})
}

#[must_use]
pub fn steer_params(thread: &SessionId, turn: &TurnId, prompt: &Prompt) -> Value {
    json!({"threadId": thread.as_str(), "expectedTurnId": turn.as_str(), "input": user_input(prompt)})
}

#[must_use]
pub fn user_input(prompt: &Prompt) -> Value {
    let images = prompt
        .images()
        .iter()
        .map(|image| json!({"type": "image", "url": data_url(image)}));
    let text = (!prompt.text().trim().is_empty())
        .then(|| json!({"type": "text", "text": prompt.text()}));
    Value::Array(images.chain(text).collect())
}

fn data_url(image: &Image) -> String {
    format!("data:{};base64,{}", image.media.mime(), STANDARD.encode(&image.data))
}

#[must_use]
pub fn tool_call(item: &Value) -> Option<ToolUse> {
    let text = |key: &str| item.get(key).and_then(Value::as_str);
    let line = |tool: &str, summary: &str| ToolUse {
        tool: tool.to_owned(),
        summary: truncate(summary, TOOL_SUMMARY_LIMIT),
    };
    match text("type")? {
        "commandExecution" => text("command").map(|command| line(SHELL, command)),
        "fileChange" => {
            let paths: Vec<&str> = item
                .get("changes")?
                .as_array()?
                .iter()
                .filter_map(|change| change.get("path").and_then(Value::as_str))
                .collect();
            let summary =
                if paths.is_empty() { UNNAMED_FILE_CHANGE.to_owned() } else { paths.join(", ") };
            Some(line(PATCH, &summary))
        }
        "mcpToolCall" => {
            let name = format!("{}/{}", text("server")?, text("tool")?);
            let arguments = item.get("arguments").map_or_else(|| "{}".to_owned(), Value::to_string);
            Some(line(&name, &arguments))
        }
        "webSearch" => text("query").map(|query| line(WEB_SEARCH, query)),
        _ => None,
    }
}

#[must_use]
pub fn agent_text(item: &Value) -> Option<String> {
    match item.get("type").and_then(Value::as_str) {
        Some("agentMessage") => item
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned),
        Some(_) | None => None,
    }
}

/// Notifications of one thread → hub events; remembers the thread's token use.
pub struct TurnTracker {
    thread: SessionId,
    tokens: Option<u64>,
}

impl TurnTracker {
    #[must_use]
    pub fn new(thread: SessionId) -> Self {
        Self { thread, tokens: None }
    }

    pub fn translate(&mut self, notification: &Notification) -> Result<Translation, RpcError> {
        let params = &notification.params;
        match notification.method.as_str() {
            "item/started" => Ok(Translation::of(
                params.get("item").and_then(tool_call).map(AgentEvent::ToolCall),
            )),
            "item/completed" => Ok(Translation::of(
                params.get("item").and_then(agent_text).map(AgentEvent::AssistantText),
            )),
            "thread/tokenUsage/updated" => {
                if let Some(total) =
                    params.pointer("/tokenUsage/total/totalTokens").and_then(Value::as_u64)
                {
                    self.tokens = Some(total);
                }
                Ok(Translation::default())
            }
            "turn/completed" => self.completed(params),
            _ => Ok(Translation::default()),
        }
    }

    fn completed(&self, params: &Value) -> Result<Translation, RpcError> {
        let turn = params.get("turn").unwrap_or(&Value::Null);
        let id = turn.get("id").and_then(Value::as_str).ok_or_else(|| {
            RpcError::Protocol(format!("turn/completed without a turn id: {params}"))
        })?;
        let status = turn.get("status").and_then(Value::as_str);
        let error = turn.pointer("/error/message").and_then(Value::as_str);
        let event = match (status, error) {
            (Some("completed"), _) => AgentEvent::Finished(Finished {
                session: self.thread.clone(),
                usage: Usage::Codex { tokens: self.tokens },
                background: 0,
            }),
            (Some("interrupted"), _) => AgentEvent::Failed("Ход Codex прерван".to_owned()),
            (_, Some(message)) => AgentEvent::Failed(message.to_owned()),
            (_, None) => AgentEvent::Failed("Ход Codex завершился ошибкой".to_owned()),
        };
        Ok(Translation { events: vec![event], completed: Some(TurnId(id.to_owned())) })
    }
}
```

(`base64` уже используется в `crates/hub-claude/src/outgoing.rs` тем же способом: `use base64::Engine; use base64::engine::general_purpose::STANDARD;` — свериться, если сигнатура `encode` отличается.)

- [ ] **Step 4: Прогнать**

Run: `cargo test -p hub-codex && cargo clippy --workspace --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates
git commit -m "hub-codex: перевод сообщений app-server" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: Запросы сервера (`requests`)

**Files:**
- Create: `crates/hub-codex/src/requests.rs`
- Modify: `crates/hub-codex/src/lib.rs` (`pub mod requests;`), `crates/hub-codex/src/testing.rs` (поддельный человек `Scripted`)

**Interfaces:**
- Consumes: `hub_agent::channel::UserChannel`, `hub_agent::tools::{ASK_USER, ASK_USER_SHAPE, SEND_FILE, TOOL_SUMMARY_LIMIT, ToolResult, deliver_file, parse_option, parse_questions}`, `crate::protocol::{PATCH, SHELL, UNNAMED_FILE_CHANGE}`, `crate::rpc::RequestError`.
- Produces: `async fn hub_codex::requests::answer(channel: &dyn UserChannel, method: &str, params: Value) -> Result<Value, RequestError>`; `#[cfg(test)] crate::testing::Scripted`.

- [ ] **Step 1: Поддельный человек**

Дописать в `crates/hub-codex/src/testing.rs`:

```rust
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
```

- [ ] **Step 2: Написать тесты**

`crates/hub-codex/src/requests.rs`, внизу:

```rust
#[cfg(test)]
mod tests {
    use hub_core::domain::{Decision, Denied, QuestionAnswer, QuestionsOutcome, ToolRequest};
    use rstest::rstest;

    use super::*;
    use crate::testing::Scripted;

    const COMMAND: &str = "item/commandExecution/requestApproval";
    const FILE_CHANGE: &str = "item/fileChange/requestApproval";
    const TOOL_CALL: &str = "item/tool/call";
    const USER_INPUT: &str = "item/tool/requestUserInput";

    fn answering(question: &str, answer: &str) -> Scripted {
        Scripted {
            outcome: QuestionsOutcome::Answered(vec![QuestionAnswer {
                question: question.to_owned(),
                answer: answer.to_owned(),
            }]),
            ..Scripted::default()
        }
    }

    fn tool_text(success: bool, text: &str) -> Value {
        json!({"success": success, "contentItems": [{"type": "inputText", "text": text}]})
    }

    #[rstest]
    #[case(COMMAND, json!({"command": "npm test", "reason": "tests"}), "shell", "npm test")]
    #[case(COMMAND, json!({"command": " ", "reason": "нужна сеть"}), "shell", "нужна сеть")]
    #[case(COMMAND, json!({"cwd": "/w"}), "shell", r#"{"cwd":"/w"}"#)]
    #[case(FILE_CHANGE, json!({"reason": "правка конфига"}), "patch", "правка конфига")]
    #[case(FILE_CHANGE, json!({"grantRoot": "/w"}), "patch", "запись в /w")]
    #[case(FILE_CHANGE, json!({}), "patch", "изменение файлов")]
    #[tokio::test]
    async fn approvals_ask_the_human(
        #[case] method: &str,
        #[case] params: Value,
        #[case] tool: &str,
        #[case] summary: &str,
    ) {
        let channel = Scripted::default();
        assert_eq!(answer(&channel, method, params).await, Ok(json!({"decision": "accept"})));
        assert_eq!(
            *channel.requests.lock().unwrap(),
            [ToolRequest { tool: tool.to_owned(), summary: summary.to_owned() }]
        );
    }

    #[tokio::test]
    async fn denied_approval_declines() {
        let channel =
            Scripted { decision: Decision::Denied(Denied::new("нет")), ..Scripted::default() };
        let reply = answer(&channel, COMMAND, json!({"command": "rm -rf /"})).await;
        assert_eq!(reply, Ok(json!({"decision": "decline"})));
    }

    #[tokio::test]
    async fn send_file_delivers_through_the_channel() {
        let params = json!({"tool": "send_file", "arguments": {"path": "a.txt"}});
        assert_eq!(
            answer(&Scripted::default(), TOOL_CALL, params).await,
            Ok(tool_text(true, "Файл a.txt отправлен пользователю"))
        );
    }

    #[tokio::test]
    async fn ask_user_returns_answers_as_lines() {
        let channel = answering("Цвет?", "синий");
        let params = json!({"tool": "ask_user", "arguments": {"questions": [{"question": "Цвет?"}]}});
        assert_eq!(answer(&channel, TOOL_CALL, params).await, Ok(tool_text(true, "Цвет?: синий")));
    }

    #[rstest]
    #[case(json!({"tool": "ask_user", "arguments": {"questions": []}}), ASK_USER_SHAPE)]
    #[case(json!({"tool": "deploy", "arguments": {}}), "unknown tool deploy or its arguments are not an object")]
    #[case(json!({"tool": "send_file", "arguments": "a.txt"}), "unknown tool send_file or its arguments are not an object")]
    #[tokio::test]
    async fn bad_tool_calls_are_tool_errors(#[case] params: Value, #[case] text: &str) {
        assert_eq!(answer(&Scripted::default(), TOOL_CALL, params).await, Ok(tool_text(false, text)));
    }

    #[tokio::test]
    async fn tool_call_without_a_name_is_malformed() {
        let reply = answer(&Scripted::default(), TOOL_CALL, json!({"arguments": {}})).await;
        assert!(matches!(reply, Err(RequestError::Malformed(_))));
    }

    #[tokio::test]
    async fn user_input_answers_by_question_id() {
        let channel = answering("Какую БД?", "Postgres");
        let params = json!({"questions": [{
            "id": "db",
            "header": "БД",
            "question": "Какую БД?",
            "options": [{"label": "Postgres", "description": ""}, {"label": "SQLite"}],
        }]});
        assert_eq!(
            answer(&channel, USER_INPUT, params).await,
            Ok(json!({"answers": {"db": {"answers": ["Postgres"]}}}))
        );
        assert_eq!(channel.questions.lock().unwrap().first().map(|q| q.options().len()), Some(2));
    }

    #[tokio::test]
    async fn declined_user_input_leaves_codex_to_decide() {
        let channel = Scripted {
            outcome: QuestionsOutcome::Denied(Denied::new("нет ответа")),
            ..Scripted::default()
        };
        let params =
            json!({"questions": [{"id": "q", "header": "", "question": "Да?", "options": null}]});
        assert_eq!(answer(&channel, USER_INPUT, params).await, Ok(json!({"answers": {}})));
    }

    #[rstest]
    #[case(json!({}))]
    #[case(json!({"questions": []}))]
    #[case(json!({"questions": [{"id": "q", "question": "Да?"}]}))]
    #[case(json!({"questions": [{"id": "q", "header": "", "question": "Да?", "options": [{"label": ""}]}]}))]
    #[tokio::test]
    async fn malformed_user_input_is_rejected(#[case] params: Value) {
        assert!(matches!(
            answer(&Scripted::default(), USER_INPUT, params).await,
            Err(RequestError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn unknown_request_is_unsupported() {
        assert_eq!(
            answer(&Scripted::default(), "item/permissions/requestApproval", json!({})).await,
            Err(RequestError::Unsupported("item/permissions/requestApproval".to_owned()))
        );
    }
}
```

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p hub-codex requests`
Expected: FAIL — модуль `requests` не найден.

- [ ] **Step 4: Реализовать**

`crates/hub-codex/src/lib.rs`: `pub mod requests;`.

`crates/hub-codex/src/requests.rs`, над тестами:

```rust
//! Requests the Codex app-server sends the client: approvals, hub tools and questions.

use hub_agent::channel::UserChannel;
use hub_agent::tools::{
    ASK_USER, ASK_USER_SHAPE, SEND_FILE, TOOL_SUMMARY_LIMIT, ToolResult, deliver_file,
    parse_option, parse_questions,
};
use hub_core::domain::{Decision, Question, QuestionsOutcome, Selection, ToolRequest};
use hub_core::render::truncate;
use serde_json::{Map, Value, json};

use crate::protocol::{PATCH, SHELL, UNNAMED_FILE_CHANGE};
use crate::rpc::RequestError;

/// Replies to one app-server request on behalf of the human behind `channel`.
pub async fn answer(
    channel: &dyn UserChannel,
    method: &str,
    params: Value,
) -> Result<Value, RequestError> {
    match method {
        "item/commandExecution/requestApproval" => {
            Ok(approve(channel, request(SHELL, &command_summary(&params))).await)
        }
        "item/fileChange/requestApproval" => {
            Ok(approve(channel, request(PATCH, &file_change_summary(&params))).await)
        }
        "item/tool/call" => Ok(tool_response(call_tool(channel, &params).await?)),
        "item/tool/requestUserInput" => user_input(channel, &params).await,
        other => Err(RequestError::Unsupported(other.to_owned())),
    }
}

fn request(tool: &str, summary: &str) -> ToolRequest {
    ToolRequest { tool: tool.to_owned(), summary: truncate(summary, TOOL_SUMMARY_LIMIT) }
}

fn nonblank<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str).filter(|text| !text.trim().is_empty())
}

fn command_summary(params: &Value) -> String {
    nonblank(params, "command")
        .or_else(|| nonblank(params, "reason"))
        .map_or_else(|| params.to_string(), str::to_owned)
}

/// The changed paths were already shown as the `patch` line of the item.
fn file_change_summary(params: &Value) -> String {
    match (nonblank(params, "reason"), params.get("grantRoot").and_then(Value::as_str)) {
        (Some(reason), _) => reason.to_owned(),
        (None, Some(root)) => format!("запись в {root}"),
        (None, None) => UNNAMED_FILE_CHANGE.to_owned(),
    }
}

async fn approve(channel: &dyn UserChannel, tool: ToolRequest) -> Value {
    match channel.request(tool).await {
        Decision::Allowed => json!({"decision": "accept"}),
        Decision::Denied(_) => json!({"decision": "decline"}),
    }
}

async fn call_tool(channel: &dyn UserChannel, params: &Value) -> Result<ToolResult, RequestError> {
    let tool = params.get("tool").and_then(Value::as_str).ok_or_else(|| {
        RequestError::Malformed(format!("tool call without a tool name: {params}"))
    })?;
    let arguments = params.get("arguments").filter(|arguments| arguments.is_object());
    Ok(match (tool, arguments) {
        (SEND_FILE, Some(arguments)) => deliver_file(arguments, channel).await,
        (ASK_USER, Some(arguments)) => ask(channel, arguments).await,
        (tool, Some(_) | None) => {
            ToolResult::Error(format!("unknown tool {tool} or its arguments are not an object"))
        }
    })
}

async fn ask(channel: &dyn UserChannel, arguments: &Value) -> ToolResult {
    let Some(questions) = parse_questions(arguments) else {
        return ToolResult::Error(ASK_USER_SHAPE.to_owned());
    };
    match channel.ask(questions).await {
        QuestionsOutcome::Answered(answers) => ToolResult::Success(
            answers
                .iter()
                .map(|answer| format!("{}: {}", answer.question, answer.answer))
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        QuestionsOutcome::Denied(denied) => ToolResult::Error(denied.reason),
    }
}

fn tool_response(result: ToolResult) -> Value {
    let (success, text) = match result {
        ToolResult::Success(text) => (true, text),
        ToolResult::Error(text) => (false, text),
    };
    json!({"success": success, "contentItems": [{"type": "inputText", "text": text}]})
}

async fn user_input(channel: &dyn UserChannel, params: &Value) -> Result<Value, RequestError> {
    let asked = user_questions(params)?;
    let outcome = channel.ask(asked.iter().map(|(_, question)| question.clone()).collect()).await;
    Ok(match outcome {
        QuestionsOutcome::Answered(answers) => {
            let replies: Map<String, Value> = asked
                .iter()
                .filter_map(|(id, question)| {
                    answers
                        .iter()
                        .find(|answer| answer.question == question.text())
                        .map(|answer| (id.clone(), json!({"answers": [answer.answer]})))
                })
                .collect();
            json!({"answers": replies})
        }
        // Codex goes on with its own judgment when a question stays unanswered.
        QuestionsOutcome::Denied(_) => json!({"answers": {}}),
    })
}

fn user_questions(params: &Value) -> Result<Vec<(String, Question)>, RequestError> {
    params
        .get("questions")
        .and_then(Value::as_array)
        .filter(|questions| !questions.is_empty())
        .ok_or_else(|| {
            RequestError::Malformed(format!("requestUserInput without questions: {params}"))
        })?
        .iter()
        .map(user_question)
        .collect()
}

fn user_question(raw: &Value) -> Result<(String, Question), RequestError> {
    let malformed = || RequestError::Malformed(format!("malformed question: {raw}"));
    let field = |key: &str| raw.get(key).and_then(Value::as_str).ok_or_else(malformed);
    let (id, header, text) = (field("id")?, field("header")?, field("question")?);
    let options = match raw.get("options") {
        None | Some(Value::Null) => Vec::new(),
        Some(options) => options
            .as_array()
            .ok_or_else(malformed)?
            .iter()
            .map(parse_option)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(malformed)?,
    };
    let question = Question::new(text.to_owned(), header.to_owned(), options, Selection::Single)
        .ok_or_else(malformed)?;
    Ok((id.to_owned(), question))
}
```

- [ ] **Step 5: Прогнать**

Run: `cargo test -p hub-codex && cargo clippy --workspace --all-targets`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A crates
git commit -m "hub-codex: одобрения, инструменты хаба и вопросы app-server" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 10: Цикл ходов (`session`)

**Files:**
- Create: `crates/hub-codex/src/session.rs`
- Modify: `crates/hub-codex/src/lib.rs` (`pub mod session;`)

**Interfaces:**
- Consumes: `hub_agent::conversation::Conversation`; `crate::protocol::{Call, Translation, TurnId, TurnTracker, parse_turn_id, steer_params, turn_params}`; `crate::rpc::{Notification, RpcClient, RpcError}`.
- Produces (`hub_codex::session`):
  - `trait Thread: Send + Sync { fn start_turn<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<TurnId, RpcError>>; fn steer<'a>(&'a self, turn: &'a TurnId, prompt: &'a Prompt) -> BoxFuture<'a, Result<(), RpcError>>; }`.
  - `RpcThread::new(client: RpcClient, thread: SessionId)`, `impl Thread for RpcThread`.
  - `async fn converse<T: Thread>(thread: &T, notifications: &mut mpsc::UnboundedReceiver<Notification>, tracker: TurnTracker, prompt: Prompt, inbox: mpsc::Receiver<Prompt>, conversation: &Conversation) -> (mpsc::Receiver<Prompt>, Result<(), RpcError>)`.

- [ ] **Step 1: Написать тесты**

`crates/hub-codex/src/session.rs`, внизу:

```rust
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use hub_agent::conversation::Limits;
    use hub_core::domain::{Finished, Usage};
    use serde_json::json;
    use tokio::task::JoinHandle;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::testing::Scripted;

    #[derive(Default)]
    struct FakeThread {
        calls: Mutex<Vec<String>>,
        steers: Mutex<VecDeque<Result<(), RpcError>>>,
        turns: Mutex<u32>,
    }

    impl FakeThread {
        fn rejecting(times: usize) -> Self {
            let rejected = RpcError::Remote { code: -32000, message: "no active turn".to_owned() };
            Self { steers: Mutex::new(vec![Err(rejected); times].into()), ..Self::default() }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Thread for FakeThread {
        fn start_turn<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<TurnId, RpcError>> {
            let mut turns = self.turns.lock().unwrap();
            *turns += 1;
            self.calls.lock().unwrap().push(format!("start:{}", prompt.text()));
            let id = TurnId::new(format!("u-{turns}"));
            Box::pin(async move { Ok(id) })
        }

        fn steer<'a>(&'a self, turn: &'a TurnId, prompt: &'a Prompt) -> BoxFuture<'a, Result<(), RpcError>> {
            self.calls.lock().unwrap().push(format!("steer:{}:{}", turn.as_str(), prompt.text()));
            let outcome = self.steers.lock().unwrap().pop_front().unwrap_or(Ok(()));
            Box::pin(async move { outcome })
        }
    }

    struct Harness {
        thread: Arc<FakeThread>,
        notify: mpsc::UnboundedSender<Notification>,
        inbox: mpsc::Sender<Prompt>,
        cancel: CancellationToken,
        events: mpsc::Receiver<AgentEvent>,
        done: JoinHandle<(mpsc::Receiver<Prompt>, Result<(), RpcError>)>,
    }

    fn prompt(text: &str) -> Prompt {
        Prompt::new(text.to_owned(), Vec::new()).unwrap()
    }

    fn thread_id() -> SessionId {
        SessionId::parse("t-1").unwrap()
    }

    fn start(thread: FakeThread) -> Harness {
        let thread = Arc::new(thread);
        let (notify, mut notifications) = mpsc::unbounded_channel();
        let (inbox, inbox_out) = mpsc::channel(8);
        let (events_in, events) = mpsc::channel(64);
        let cancel = CancellationToken::new();
        let conversation = Conversation {
            channel: Arc::new(Scripted::default()),
            events: events_in,
            cancel: cancel.clone(),
            limits: Limits::new(Duration::from_secs(1)),
        };
        let done = tokio::spawn({
            let thread = Arc::clone(&thread);
            async move {
                let tracker = TurnTracker::new(thread_id());
                converse(thread.as_ref(), &mut notifications, tracker, prompt("hi"), inbox_out, &conversation)
                    .await
            }
        });
        Harness { thread, notify, inbox, cancel, events, done }
    }

    fn completed(turn: &str) -> Notification {
        Notification {
            method: "turn/completed".to_owned(),
            params: json!({"turn": {"id": turn, "status": "completed"}}),
        }
    }

    fn said(text: &str) -> Notification {
        Notification {
            method: "item/completed".to_owned(),
            params: json!({"item": {"type": "agentMessage", "text": text}}),
        }
    }

    async fn eventually(what: &str, check: impl Fn() -> bool) {
        for _ in 0..400 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    fn drain(events: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        std::iter::from_fn(|| events.try_recv().ok()).collect()
    }

    #[tokio::test]
    async fn a_turn_relays_text_and_finishes() {
        let mut harness = start(FakeThread::default());
        harness.notify.send(said("Привет")).unwrap();
        harness.notify.send(completed("u-1")).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(harness.thread.calls(), ["start:hi"]);
        let finished = AgentEvent::Finished(Finished {
            session: thread_id(),
            usage: Usage::Codex { tokens: None },
            background: 0,
        });
        assert_eq!(drain(&mut harness.events), [AgentEvent::AssistantText("Привет".to_owned()), finished]);
    }

    #[tokio::test]
    async fn a_prompt_during_a_turn_steers_it() {
        let harness = start(FakeThread::default());
        harness.inbox.send(prompt("ещё")).await.unwrap();
        eventually("steered", || harness.thread.calls().len() == 2).await;
        harness.notify.send(completed("u-1")).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(harness.thread.calls(), ["start:hi", "steer:u-1:ещё"]);
    }

    #[tokio::test]
    async fn a_rejected_steer_waits_for_the_turn_and_starts_the_next() {
        let harness = start(FakeThread::rejecting(1));
        harness.inbox.send(prompt("поздно")).await.unwrap();
        eventually("steer tried", || harness.thread.calls().len() == 2).await;
        harness.notify.send(completed("u-1")).unwrap();
        eventually("next turn", || harness.thread.calls().len() == 3).await;
        harness.notify.send(completed("u-2")).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(harness.thread.calls(), ["start:hi", "steer:u-1:поздно", "start:поздно"]);
    }

    #[tokio::test]
    async fn rejected_prompts_keep_their_order() {
        let harness = start(FakeThread::rejecting(2));
        harness.inbox.send(prompt("a")).await.unwrap();
        eventually("a tried", || harness.thread.calls().len() == 2).await;
        harness.inbox.send(prompt("b")).await.unwrap();
        eventually("a tried again", || harness.thread.calls().len() == 3).await;
        harness.notify.send(completed("u-1")).unwrap();
        eventually("a started, b steered", || harness.thread.calls().len() == 5).await;
        harness.notify.send(completed("u-2")).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(
            harness.thread.calls(),
            ["start:hi", "steer:u-1:a", "steer:u-1:a", "start:a", "steer:u-2:b"]
        );
    }

    #[tokio::test]
    async fn completion_of_another_turn_is_not_the_end() {
        let mut harness = start(FakeThread::default());
        harness.notify.send(completed("u-9")).unwrap();
        harness.notify.send(said("всё ещё работаю")).unwrap();
        harness.notify.send(completed("u-1")).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert!(drain(&mut harness.events).contains(&AgentEvent::AssistantText("всё ещё работаю".to_owned())));
    }

    #[tokio::test]
    async fn closed_notifications_are_a_closed_transport() {
        let harness = start(FakeThread::default());
        drop(harness.notify);
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Err(RpcError::Closed));
    }

    #[tokio::test]
    async fn stop_ends_the_conversation_at_once_and_keeps_the_inbox() {
        let harness = start(FakeThread::default());
        eventually("started", || harness.thread.calls().len() == 1).await;
        harness.cancel.cancel();
        let (mut inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        harness.inbox.send(prompt("после стопа")).await.unwrap();
        assert_eq!(inbox.recv().await.map(|p| p.text().to_owned()), Some("после стопа".to_owned()));
    }

    #[tokio::test]
    async fn a_failed_turn_ends_the_conversation_with_its_reason() {
        let mut harness = start(FakeThread::default());
        let failed = Notification {
            method: "turn/completed".to_owned(),
            params: json!({"turn": {"id": "u-1", "status": "failed", "error": {"message": "quota"}}}),
        };
        harness.notify.send(failed).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(drain(&mut harness.events), [AgentEvent::Failed("quota".to_owned())]);
    }
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-codex session`
Expected: FAIL — модуль `session` не найден.

- [ ] **Step 3: Реализовать**

`crates/hub-codex/src/lib.rs`: `pub mod session;`.

`crates/hub-codex/src/session.rs`, над тестами:

```rust
//! One Codex conversation: turns, prompts sent mid-turn, and the end of the conversation.
//!
//! Prompts sent meanwhile join the running turn. One the turn rejects waits, in order with the
//! others, for that turn to complete and then starts the next turn; a turn is never started
//! while another runs.

use std::collections::VecDeque;

use futures::future::BoxFuture;
use hub_agent::conversation::Conversation;
use hub_core::domain::{AgentEvent, Prompt, SessionId};
use tokio::sync::mpsc;

use crate::protocol::{
    Call, Translation, TurnId, TurnTracker, parse_turn_id, steer_params, turn_params,
};
use crate::rpc::{Notification, RpcClient, RpcError};

/// The part of an app-server thread a conversation needs.
pub trait Thread: Send + Sync {
    fn start_turn<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<TurnId, RpcError>>;
    fn steer<'a>(&'a self, turn: &'a TurnId, prompt: &'a Prompt) -> BoxFuture<'a, Result<(), RpcError>>;
}

pub struct RpcThread {
    client: RpcClient,
    thread: SessionId,
}

impl RpcThread {
    #[must_use]
    pub fn new(client: RpcClient, thread: SessionId) -> Self {
        Self { client, thread }
    }
}

impl Thread for RpcThread {
    fn start_turn<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<TurnId, RpcError>> {
        Box::pin(async move {
            let params = turn_params(&self.thread, prompt);
            parse_turn_id(&self.client.request(Call::TurnStart.method(), params).await?)
        })
    }

    fn steer<'a>(&'a self, turn: &'a TurnId, prompt: &'a Prompt) -> BoxFuture<'a, Result<(), RpcError>> {
        Box::pin(async move {
            let params = steer_params(&self.thread, turn, prompt);
            self.client.request(Call::TurnSteer.method(), params).await.map(drop)
        })
    }
}

/// Relays a thread until no turn runs and nothing is queued, or until /stop.
///
/// Returns the inbox, so prompts that arrive while the conversation closes are not lost.
pub async fn converse<T: Thread>(
    thread: &T,
    notifications: &mut mpsc::UnboundedReceiver<Notification>,
    mut tracker: TurnTracker,
    prompt: Prompt,
    mut inbox: mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> (mpsc::Receiver<Prompt>, Result<(), RpcError>) {
    let outcome =
        relay(thread, notifications, &mut tracker, prompt, &mut inbox, conversation).await;
    (inbox, outcome)
}

async fn relay<T: Thread>(
    thread: &T,
    notifications: &mut mpsc::UnboundedReceiver<Notification>,
    tracker: &mut TurnTracker,
    prompt: Prompt,
    inbox: &mut mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> Result<(), RpcError> {
    let mut queued = VecDeque::new();
    let mut active = Some(thread.start_turn(&prompt).await?);
    let mut inbox_open = true;
    while active.is_some() || !queued.is_empty() {
        tokio::select! {
            () = conversation.cancel.cancelled() => return Ok(()),
            next = inbox.recv(), if inbox_open => match next {
                Some(prompt) => {
                    queued.push_back(prompt);
                    active = deliver(thread, active, &mut queued).await?;
                }
                None => inbox_open = false,
            },
            notification = notifications.recv() => {
                let notification = notification.ok_or(RpcError::Closed)?;
                let Translation { events, completed } = tracker.translate(&notification)?;
                for event in events {
                    emit(conversation, event).await;
                }
                if completed.is_some() && completed == active {
                    active = deliver(thread, None, &mut queued).await?;
                }
            }
        }
    }
    Ok(())
}

/// Hands `queued` to the running turn, or to a new one if none runs; returns that turn.
///
/// A rejected steer leaves the prompt and everything after it queued: the turn may still be
/// running, so only its `turn/completed` makes starting another one safe.
async fn deliver<T: Thread>(
    thread: &T,
    active: Option<TurnId>,
    queued: &mut VecDeque<Prompt>,
) -> Result<Option<TurnId>, RpcError> {
    let active = match active {
        Some(turn) => turn,
        None => match queued.pop_front() {
            None => return Ok(None),
            Some(prompt) => thread.start_turn(&prompt).await?,
        },
    };
    while let Some(next) = queued.front() {
        match thread.steer(&active, next).await {
            Ok(()) => {
                queued.pop_front();
            }
            Err(RpcError::Remote { .. }) => break,
            Err(other @ (RpcError::Closed | RpcError::Protocol(_) | RpcError::Timeout { .. })) => {
                return Err(other);
            }
        }
    }
    Ok(Some(active))
}

async fn emit(conversation: &Conversation, event: AgentEvent) {
    // Nobody listening means the topic is gone; there is no one left to tell.
    let _ = conversation.events.send(event).await;
}
```

- [ ] **Step 4: Прогнать**

Run: `cargo test -p hub-codex && cargo clippy --workspace --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates
git commit -m "hub-codex: цикл ходов с turn/steer" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 11: Бэкенд: процесс, вход, тред (`backend`)

**Files:**
- Create: `crates/hub-codex/src/backend.rs`
- Modify: `crates/hub-codex/src/lib.rs` (`pub mod backend;`)

**Interfaces:**
- Consumes: задачи 7–10; `hub_agent::cli::hide_window`; `hub_core::settings::{ApiKey, CodexSettings}`.
- Produces (`hub_codex::backend`):
  - `CodexBackend::new(cli: PathBuf, settings: CodexSettings)`; `async fn run(&self, session: &TopicSession, prompt: Prompt, inbox: mpsc::Receiver<Prompt>, conversation: Conversation) -> mpsc::Receiver<Prompt>`.
  - `Link<'a> { client: &'a RpcClient, notifications: &'a mut mpsc::UnboundedReceiver<Notification> }`; `async fn serve(link: Link<'_>, settings: &CodexSettings, session: &TopicSession, prompt: Prompt, inbox: mpsc::Receiver<Prompt>, conversation: &Conversation) -> mpsc::Receiver<Prompt>`.
  - `async fn probe(client: &RpcClient, settings: &CodexSettings) -> Result<CodexAuth, RpcError>`; `async fn probe_auth(cli: &Path, settings: &CodexSettings) -> Result<CodexAuth, ProbeError>`.
  - `ProbeError { Spawn(io::Error), Rpc(RpcError) }`.
  - `NOT_LOGGED_IN`, `APP_SERVER_FAILED`.

- [ ] **Step 1: Написать тесты**

`crates/hub-codex/src/backend.rs`, внизу:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use hub_agent::conversation::Limits;
    use hub_core::domain::{AbsolutePath, BackendKind, Finished, Usage};
    use hub_core::settings::{ApiKey, Approval, Sandbox};
    use rstest::rstest;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::testing::{Peer, Scripted, pair, refusing};

    enum Reply {
        Result(Value),
        Error(&'static str),
    }

    struct Step {
        method: &'static str,
        reply: Reply,
        then: Vec<Value>,
    }

    fn ok(method: &'static str, result: Value) -> Step {
        Step { method, reply: Reply::Result(result), then: Vec::new() }
    }

    fn err(method: &'static str, message: &'static str) -> Step {
        Step { method, reply: Reply::Error(message), then: Vec::new() }
    }

    impl Step {
        fn then(self, notifications: Vec<Value>) -> Self {
            Self { then: notifications, ..self }
        }
    }

    /// Answers the client's requests in order and records every message it got.
    async fn fake_server(mut peer: Peer, steps: Vec<Step>) -> (Peer, Vec<Value>) {
        let mut seen = Vec::new();
        for step in steps {
            let request = loop {
                let message = peer.read().await.unwrap();
                seen.push(message.clone());
                if message.get("id").is_some() {
                    break message;
                }
            };
            assert_eq!(request.get("method").and_then(Value::as_str), Some(step.method));
            let id = request.get("id").cloned().unwrap();
            let reply = match step.reply {
                Reply::Result(result) => json!({"id": id, "result": result}),
                Reply::Error(message) => {
                    json!({"id": id, "error": {"code": -32000, "message": message}})
                }
            };
            peer.write(reply).await;
            for notification in step.then {
                peer.write(notification).await;
            }
        }
        (peer, seen)
    }

    fn settings(api_key: Option<&str>) -> CodexSettings {
        CodexSettings {
            cli: None,
            model: None,
            sandbox: Sandbox::WorkspaceWrite,
            approval: Approval::OnRequest,
            api_key: api_key.and_then(ApiKey::parse),
        }
    }

    fn session(saved: Option<&str>) -> TopicSession {
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        TopicSession::fresh(BackendKind::Codex, cwd).with_session(saved.and_then(SessionId::parse))
    }

    fn prompt(text: &str) -> Prompt {
        Prompt::new(text.to_owned(), Vec::new()).unwrap()
    }

    fn conversation() -> (Conversation, mpsc::Receiver<AgentEvent>) {
        let (events, received) = mpsc::channel(64);
        let conversation = Conversation {
            channel: Arc::new(Scripted::default()),
            events,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_secs(1)),
        };
        (conversation, received)
    }

    fn chatgpt() -> Value {
        json!({"account": {"type": "chatgpt"}, "requiresOpenaiAuth": true})
    }

    fn logged_out() -> Value {
        json!({"account": null, "requiresOpenaiAuth": true})
    }

    fn api_key_login() -> Value {
        json!({"account": {"type": "apiKey"}})
    }

    fn turn_done(turn: &str) -> Value {
        json!({"method": "turn/completed", "params": {"turn": {"id": turn, "status": "completed"}}})
    }

    fn thread() -> SessionId {
        SessionId::parse("t-1").unwrap()
    }

    fn methods(seen: &[Value]) -> Vec<String> {
        seen.iter()
            .filter_map(|message| message.get("method").and_then(Value::as_str))
            .map(str::to_owned)
            .collect()
    }

    async fn collect(mut received: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Some(event) = received.recv().await {
            events.push(event);
        }
        events
    }

    /// Runs `serve` against a fake app-server; returns the events and what the server saw.
    async fn run(
        settings: CodexSettings,
        topic: TopicSession,
        steps: Vec<Step>,
    ) -> (Vec<AgentEvent>, Vec<Value>) {
        let (mut connection, peer) = pair(refusing(), Duration::from_secs(5));
        let server = tokio::spawn(fake_server(peer, steps));
        let (conversation, received) = conversation();
        let (_inbox_in, inbox) = mpsc::channel(4);
        let link = Link { client: &connection.client, notifications: &mut connection.notifications };
        let _inbox = serve(link, &settings, &topic, prompt("hi"), inbox, &conversation).await;
        drop(conversation);
        let (_peer, seen) = server.await.unwrap();
        (collect(received).await, seen)
    }

    fn opened(open: Step) -> Vec<Step> {
        vec![
            ok("initialize", json!({})),
            ok("account/read", chatgpt()),
            open,
            ok("turn/start", json!({"turn": {"id": "u-1"}})).then(vec![turn_done("u-1")]),
        ]
    }

    #[tokio::test]
    async fn new_session_starts_a_thread_and_runs_a_turn() {
        let (events, seen) = run(
            settings(None),
            session(None),
            vec![
                ok("initialize", json!({})),
                ok("account/read", chatgpt()),
                ok("thread/start", json!({"thread": {"id": "t-1"}})),
                ok("turn/start", json!({"turn": {"id": "u-1"}})).then(vec![
                    json!({"method": "item/completed", "params": {"item": {"type": "agentMessage", "text": "Привет"}}}),
                    json!({"method": "thread/tokenUsage/updated", "params": {"tokenUsage": {"total": {"totalTokens": 42}}}}),
                    turn_done("u-1"),
                ]),
            ],
        )
        .await;
        assert_eq!(
            events,
            [
                AgentEvent::SessionStarted(thread()),
                AgentEvent::AssistantText("Привет".to_owned()),
                AgentEvent::Finished(Finished {
                    session: thread(),
                    usage: Usage::Codex { tokens: Some(42) },
                    background: 0,
                }),
            ]
        );
        assert_eq!(
            methods(&seen),
            ["initialize", "initialized", "account/read", "thread/start", "turn/start"]
        );
    }

    #[tokio::test]
    async fn saved_session_resumes_its_thread() {
        let resume = ok("thread/resume", json!({"thread": {"id": "t-1"}}));
        let (events, seen) = run(settings(None), session(Some("t-1")), opened(resume)).await;
        assert_eq!(events.first(), Some(&AgentEvent::SessionStarted(thread())));
        assert!(methods(&seen).contains(&"thread/resume".to_owned()));
    }

    #[tokio::test]
    async fn missing_login_without_a_key_fails_before_opening_a_thread() {
        let (events, seen) = run(
            settings(None),
            session(None),
            vec![ok("initialize", json!({})), ok("account/read", logged_out())],
        )
        .await;
        assert_eq!(events, [AgentEvent::Failed(NOT_LOGGED_IN.to_owned())]);
        assert!(!methods(&seen).contains(&"thread/start".to_owned()));
    }

    #[tokio::test]
    async fn missing_login_with_a_key_logs_in_and_starts() {
        let (events, seen) = run(
            settings(Some("sk-test")),
            session(None),
            vec![
                ok("initialize", json!({})),
                ok("account/read", logged_out()),
                ok("account/login/start", json!({})),
                ok("account/read", api_key_login()),
                ok("thread/start", json!({"thread": {"id": "t-1"}})),
                ok("turn/start", json!({"turn": {"id": "u-1"}})).then(vec![turn_done("u-1")]),
            ],
        )
        .await;
        assert_eq!(events.first(), Some(&AgentEvent::SessionStarted(thread())));
        let login = seen
            .iter()
            .find(|message| message.get("method") == Some(&json!("account/login/start")))
            .and_then(|message| message.get("params"));
        assert_eq!(login, Some(&json!({"type": "apiKey", "apiKey": "sk-test"})));
    }

    #[tokio::test]
    async fn a_key_login_is_not_repeated_in_a_session() {
        let (_events, seen) = run(
            settings(Some("sk-test")),
            session(None),
            vec![
                ok("initialize", json!({})),
                ok("account/read", api_key_login()),
                ok("thread/start", json!({"thread": {"id": "t-1"}})),
                ok("turn/start", json!({"turn": {"id": "u-1"}})).then(vec![turn_done("u-1")]),
            ],
        )
        .await;
        assert!(!methods(&seen).contains(&"account/login/start".to_owned()));
    }

    #[rstest]
    #[case(Some("t-1"), err("thread/resume", "no rollout found"), "Codex не смог продолжить сессию t-1: no rollout found. /reset — начать заново")]
    #[case(None, err("thread/start", "bad model"), "Codex не начал сессию: bad model")]
    #[tokio::test]
    async fn a_thread_that_does_not_open_fails_with_a_hint(
        #[case] saved: Option<&str>,
        #[case] open: Step,
        #[case] reason: &str,
    ) {
        let steps = vec![ok("initialize", json!({})), ok("account/read", chatgpt()), open];
        let (events, _seen) = run(settings(None), session(saved), steps).await;
        assert_eq!(events, [AgentEvent::Failed(reason.to_owned())]);
    }

    #[tokio::test]
    async fn rpc_error_mid_session_fails_with_its_message() {
        let (events, _seen) = run(
            settings(None),
            session(None),
            vec![
                ok("initialize", json!({})),
                ok("account/read", chatgpt()),
                ok("thread/start", json!({"thread": {"id": "t-1"}})),
                err("turn/start", "quota exceeded"),
            ],
        )
        .await;
        assert_eq!(
            events,
            [AgentEvent::SessionStarted(thread()), AgentEvent::Failed("Codex: quota exceeded".to_owned())]
        );
    }

    #[tokio::test]
    async fn app_server_exiting_mid_turn_fails_the_turn() {
        let (mut connection, peer) = pair(refusing(), Duration::from_secs(5));
        let server = tokio::spawn(async move {
            let steps = vec![
                ok("initialize", json!({})),
                ok("account/read", chatgpt()),
                ok("thread/start", json!({"thread": {"id": "t-1"}})),
                ok("turn/start", json!({"turn": {"id": "u-1"}})),
            ];
            let (peer, _seen) = fake_server(peer, steps).await;
            drop(peer);
        });
        let (conversation, received) = conversation();
        let (_inbox_in, inbox) = mpsc::channel(4);
        let link = Link { client: &connection.client, notifications: &mut connection.notifications };
        let _inbox =
            serve(link, &settings(None), &session(None), prompt("hi"), inbox, &conversation).await;
        drop(conversation);
        server.await.unwrap();
        assert_eq!(
            collect(received).await,
            [AgentEvent::SessionStarted(thread()), AgentEvent::Failed(APP_SERVER_FAILED.to_owned())]
        );
    }

    #[rstest]
    #[case(logged_out(), true, CodexAuth::ApiKey)]
    #[case(api_key_login(), true, CodexAuth::ApiKey)]
    #[case(chatgpt(), false, CodexAuth::ChatGpt)]
    #[tokio::test]
    async fn probe_logs_in_with_the_key_unless_chatgpt(
        #[case] account: Value,
        #[case] logs_in: bool,
        #[case] expected: CodexAuth,
    ) {
        let (connection, peer) = pair(refusing(), Duration::from_secs(5));
        let login = [ok("account/login/start", json!({})), ok("account/read", api_key_login())];
        let steps = [ok("initialize", json!({})), ok("account/read", account)]
            .into_iter()
            .chain(login.into_iter().filter(|_| logs_in))
            .collect();
        let server = tokio::spawn(fake_server(peer, steps));
        assert_eq!(probe(&connection.client, &settings(Some("sk-new"))).await, Ok(expected));
        let (_peer, seen) = server.await.unwrap();
        assert_eq!(methods(&seen).contains(&"account/login/start".to_owned()), logs_in);
    }

    #[tokio::test]
    async fn missing_binary_fails_the_turn_and_keeps_the_inbox() {
        let backend = CodexBackend::new(PathBuf::from("definitely-not-codex-binary"), settings(None));
        let (conversation, mut received) = conversation();
        let (inbox_in, inbox) = mpsc::channel(4);
        inbox_in.send(prompt("потом")).await.unwrap();
        let mut leftover = backend.run(&session(None), prompt("hi"), inbox, conversation).await;
        assert!(matches!(
            received.recv().await,
            Some(AgentEvent::Failed(reason)) if reason.starts_with("Не удалось запустить Codex")
        ));
        assert!(leftover.try_recv().is_ok());
    }

    #[tokio::test]
    async fn probe_of_a_missing_binary_is_a_spawn_error() {
        let outcome = probe_auth(Path::new("definitely-not-codex-binary"), &settings(None)).await;
        assert!(matches!(outcome, Err(ProbeError::Spawn(_))));
    }
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-codex backend`
Expected: FAIL — модуль `backend` не найден.

- [ ] **Step 3: Реализовать**

`crates/hub-codex/src/lib.rs`: `pub mod backend;`.

`crates/hub-codex/src/backend.rs`, над тестами:

```rust
//! The `codex app-server` child for one conversation: login, thread, turns.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use hub_agent::cli::hide_window;
use hub_agent::conversation::Conversation;
use hub_core::domain::{AgentEvent, Prompt, SessionId, TopicSession};
use hub_core::settings::CodexSettings;
use serde_json::{Value, json};
use tokio::io::AsyncRead;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, LinesCodec};

use crate::protocol::{
    Call, CodexAuth, TurnTracker, auth_state, initialize_params, login_params, open_thread,
    parse_thread_id,
};
use crate::requests;
use crate::rpc::{self, Connection, Handler, Notification, RequestError, RpcClient, RpcError};
use crate::session::{RpcThread, converse};

// Handshake and thread/turn control only; a turn itself runs until it ends or /stop.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
// app-server exits on stdin EOF; the grace lets it shut down cleanly before a kill.
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_STDERR_LINE: usize = 64 * 1024;
// Commands Codex runs inherit its environment; app-server takes the key via login, not env.
const HIDDEN_FROM_CHILD: &str = "OPENAI_API_KEY";
pub const NOT_LOGGED_IN: &str =
    "Codex не авторизован: выполните `codex login` или задайте API-ключ OpenAI в настройках";
pub const APP_SERVER_FAILED: &str = "Codex app-server завершился, подробности в логе";

pub struct CodexBackend {
    cli: PathBuf,
    settings: CodexSettings,
}

impl CodexBackend {
    #[must_use]
    pub fn new(cli: PathBuf, settings: CodexSettings) -> Self {
        Self { cli, settings }
    }

    /// Runs one conversation and returns the inbox with the prompts it did not take.
    pub async fn run(
        &self,
        session: &TopicSession,
        prompt: Prompt,
        inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation,
    ) -> mpsc::Receiver<Prompt> {
        let channel = Arc::clone(&conversation.channel);
        let handler: Handler = Arc::new(move |method, params| {
            let channel = Arc::clone(&channel);
            Box::pin(async move { requests::answer(channel.as_ref(), &method, params).await })
        });
        let mut server = match AppServer::spawn(&self.cli, handler) {
            Ok(server) => server,
            Err(error) => {
                fail(&conversation, format!("Не удалось запустить Codex: {error}")).await;
                return inbox;
            }
        };
        let link = Link { client: &server.client, notifications: &mut server.notifications };
        let inbox = serve(link, &self.settings, session, prompt, inbox, &conversation).await;
        server.stop().await;
        inbox
    }
}

pub struct Link<'a> {
    pub client: &'a RpcClient,
    pub notifications: &'a mut mpsc::UnboundedReceiver<Notification>,
}

/// One conversation over an established connection: login, thread, turns.
pub async fn serve(
    link: Link<'_>,
    settings: &CodexSettings,
    session: &TopicSession,
    prompt: Prompt,
    inbox: mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> mpsc::Receiver<Prompt> {
    let Link { client, notifications } = link;
    let thread = match open(client, settings, session).await {
        Ok(thread) => thread,
        Err(error) => {
            tracing::warn!(%error, "codex session did not open");
            fail(conversation, error.to_string()).await;
            return inbox;
        }
    };
    emit(conversation, AgentEvent::SessionStarted(thread.clone())).await;
    let tracker = TurnTracker::new(thread.clone());
    let rpc_thread = RpcThread::new(client.clone(), thread);
    let (inbox, outcome) =
        converse(&rpc_thread, notifications, tracker, prompt, inbox, conversation).await;
    if let Err(error) = outcome {
        tracing::warn!(%error, "codex session broke");
        fail(conversation, session_failure(&error)).await;
    }
    inbox
}

#[derive(Debug)]
enum OpenError {
    NotLoggedIn,
    NotStarted(String),
    NotResumed { session: SessionId, message: String },
    Rpc(RpcError),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotLoggedIn => f.write_str(NOT_LOGGED_IN),
            Self::NotStarted(message) => write!(f, "Codex не начал сессию: {message}"),
            Self::NotResumed { session, message } => write!(
                f,
                "Codex не смог продолжить сессию {}: {message}. /reset — начать заново",
                session.as_str()
            ),
            Self::Rpc(error) => f.write_str(&session_failure(error)),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<RpcError> for OpenError {
    fn from(error: RpcError) -> Self {
        Self::Rpc(error)
    }
}

fn session_failure(error: &RpcError) -> String {
    match error {
        RpcError::Remote { message, .. } => format!("Codex: {message}"),
        RpcError::Closed | RpcError::Protocol(_) | RpcError::Timeout { .. } => {
            APP_SERVER_FAILED.to_owned()
        }
    }
}

async fn open(
    client: &RpcClient,
    settings: &CodexSettings,
    session: &TopicSession,
) -> Result<SessionId, OpenError> {
    let auth = match handshake(client).await? {
        CodexAuth::Missing => login(client, settings, CodexAuth::Missing).await?,
        auth @ (CodexAuth::ApiKey | CodexAuth::ChatGpt | CodexAuth::Other | CodexAuth::NotRequired) => {
            auth
        }
    };
    if auth == CodexAuth::Missing {
        return Err(OpenError::NotLoggedIn);
    }
    let (call, params) = open_thread(session, settings);
    match client.request(call.method(), params).await {
        Ok(result) => Ok(parse_thread_id(&result)?),
        Err(RpcError::Remote { message, .. }) => Err(match &session.session {
            None => OpenError::NotStarted(message),
            Some(saved) => OpenError::NotResumed { session: saved.clone(), message },
        }),
        Err(other @ (RpcError::Closed | RpcError::Protocol(_) | RpcError::Timeout { .. })) => {
            Err(other.into())
        }
    }
}

/// The handshake, then how Codex is logged in.
async fn handshake(client: &RpcClient) -> Result<CodexAuth, RpcError> {
    client.request(Call::Initialize.method(), initialize_params()).await?;
    client.notify(Call::Initialized.method()).await?;
    Ok(auth_state(&client.request(Call::AccountRead.method(), json!({})).await?))
}

/// The key replaces only a missing or API-key login: logging in rewrites `auth.json`, and a
/// ChatGPT login must survive a key left in the settings.
async fn login(
    client: &RpcClient,
    settings: &CodexSettings,
    auth: CodexAuth,
) -> Result<CodexAuth, RpcError> {
    match (&settings.api_key, auth) {
        (Some(key), CodexAuth::Missing | CodexAuth::ApiKey) => {
            client.request(Call::Login.method(), login_params(key)).await?;
            Ok(auth_state(&client.request(Call::AccountRead.method(), json!({})).await?))
        }
        (
            None,
            CodexAuth::Missing
            | CodexAuth::ApiKey
            | CodexAuth::ChatGpt
            | CodexAuth::Other
            | CodexAuth::NotRequired,
        )
        | (Some(_), CodexAuth::ChatGpt | CodexAuth::Other | CodexAuth::NotRequired) => Ok(auth),
    }
}

/// How Codex is logged in, logging in with the configured key if it may. Unlike a session,
/// the probe logs in again over an API-key login, so a rotated key is picked up.
pub async fn probe(client: &RpcClient, settings: &CodexSettings) -> Result<CodexAuth, RpcError> {
    let auth = handshake(client).await?;
    login(client, settings, auth).await
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("не удалось запустить Codex app-server: {0}")]
    Spawn(io::Error),
    #[error("Codex app-server: {0}")]
    Rpc(#[from] RpcError),
}

pub async fn probe_auth(cli: &Path, settings: &CodexSettings) -> Result<CodexAuth, ProbeError> {
    let refuse: Handler = Arc::new(|method, _params| {
        Box::pin(async move { Err(RequestError::Unsupported(method)) })
    });
    let server = AppServer::spawn(cli, refuse).map_err(ProbeError::Spawn)?;
    let auth = probe(&server.client, settings).await;
    server.stop().await;
    Ok(auth?)
}

struct AppServer {
    client: RpcClient,
    notifications: mpsc::UnboundedReceiver<Notification>,
    reader: JoinHandle<()>,
    child: Child,
}

impl AppServer {
    fn spawn(cli: &Path, handler: Handler) -> io::Result<Self> {
        let mut command = Command::new(cli);
        command
            .arg("app-server")
            .env_remove(HIDDEN_FROM_CHILD)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_window(&mut command);
        let mut child = command.spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(io::Error::other("app-server started without standard streams"));
        };
        tokio::spawn(log_stderr(stderr));
        let Connection { client, notifications, reader } =
            rpc::connect(stdout, stdin, handler, REQUEST_TIMEOUT);
        Ok(Self { client, notifications, reader, child })
    }

    async fn stop(mut self) {
        self.client.close();
        if tokio::time::timeout(EXIT_TIMEOUT, self.child.wait()).await.is_err() {
            tracing::warn!("codex app-server did not exit on stdin close, killing it");
            if let Err(error) = self.child.kill().await {
                tracing::warn!(%error, "codex app-server could not be killed");
            }
        }
        // Answers still in flight have nobody left to reach.
        self.reader.abort();
    }
}

async fn log_stderr<R: AsyncRead + Unpin>(stderr: R) {
    let mut lines = FramedRead::new(stderr, LinesCodec::new_with_max_length(MAX_STDERR_LINE));
    while let Some(line) = lines.next().await {
        match line {
            Ok(line) => tracing::info!(%line, "codex stderr"),
            Err(error) => {
                tracing::debug!(%error, "codex stderr unreadable");
                return;
            }
        }
    }
}

async fn emit(conversation: &Conversation, event: AgentEvent) {
    // Nobody listening means the topic is gone; there is no one left to tell.
    let _ = conversation.events.send(event).await;
}

async fn fail(conversation: &Conversation, reason: String) {
    emit(conversation, AgentEvent::Failed(reason)).await;
}
```

- [ ] **Step 4: Прогнать**

Run: `cargo test -p hub-codex && cargo clippy --workspace --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates
git commit -m "hub-codex: бэкенд — процесс app-server, вход и тред" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---
### Task 12: Хаб запускает Codex (`HubAgents`)

**Files:**
- Modify: `crates/hub-telegram/Cargo.toml` (`hub-codex.workspace = true`)
- Modify: `crates/hub-telegram/src/agents.rs`
- Modify: `crates/hub-app/src/connector.rs` (`ClaudeAgents` → `HubAgents`)

**Interfaces:**
- Consumes: `hub_codex::backend::CodexBackend`, `hub_codex::version::locate` (задачи 6, 11).
- Produces: `hub_telegram::agents::HubAgents::new(settings: watch::Receiver<Arc<Settings>>)` (замена `ClaudeAgents`).

- [ ] **Step 1: Написать тест**

`crates/hub-telegram/src/agents.rs`, внизу:

```rust
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_agent::channel::UserChannel;
    use hub_agent::conversation::Limits;
    use hub_core::domain::{
        AbsolutePath, Decision, FileDelivery, OutgoingFile, Question, QuestionsOutcome, ToolRequest,
    };
    use hub_core::settings::{CodexDraft, Draft};
    use tokio_util::sync::CancellationToken;

    use super::*;

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
    async fn codex_topics_run_the_configured_codex() {
        let missing = std::env::temp_dir().join("definitely-not-codex-binary");
        let settings = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: std::env::temp_dir().display().to_string(),
            codex: CodexDraft { cli: missing.display().to_string(), ..CodexDraft::default() },
            ..Draft::default()
        }
        .parse(&std::env::temp_dir())
        .unwrap();
        let agents = HubAgents::new(watch::Sender::new(Arc::new(settings)).subscribe());
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        let session = TopicSession::fresh(BackendKind::Codex, cwd);
        let (events, mut received) = mpsc::channel(4);
        let conversation = Conversation {
            channel: Arc::new(Allowing),
            events,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_secs(1)),
        };
        let (_inbox_in, inbox) = mpsc::channel(1);
        let prompt = Prompt::new("hi".to_owned(), Vec::new()).unwrap();

        let _inbox = agents.run(&session, prompt, inbox, conversation).await;

        assert!(matches!(
            received.recv().await,
            Some(AgentEvent::Failed(reason)) if reason.starts_with("Не удалось запустить Codex")
        ));
    }
}
```

Добавить в `[dev-dependencies]` `hub-telegram` то, чего там нет из использованного (`tokio-util` уже в зависимостях).

- [ ] **Step 2: Убедиться, что тест падает**

Run: `cargo test -p hub-telegram agents`
Expected: FAIL — `cannot find type HubAgents`.

- [ ] **Step 3: Реализовать**

`crates/hub-telegram/Cargo.toml`, `[dependencies]`: `hub-codex.workspace = true`.

`crates/hub-telegram/src/agents.rs` — заменить всё до тестов:

```rust
//! The agent backends behind the hub; adding one is a new `BackendKind` variant and a match arm.

use std::sync::Arc;

use futures::future::BoxFuture;
use hub_agent::conversation::Conversation;
use hub_claude::backend::ClaudeBackend;
use hub_codex::backend::CodexBackend;
use hub_core::domain::{AgentEvent, BackendKind, Prompt, TopicSession};
use hub_core::settings::Settings;
use tokio::sync::{mpsc, watch};

use crate::hub::Agents;

pub struct HubAgents {
    settings: watch::Receiver<Arc<Settings>>,
}

impl HubAgents {
    #[must_use]
    pub fn new(settings: watch::Receiver<Arc<Settings>>) -> Self {
        Self { settings }
    }
}

impl Agents for HubAgents {
    fn run<'a>(
        &'a self,
        session: &'a TopicSession,
        prompt: Prompt,
        inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation,
    ) -> BoxFuture<'a, mpsc::Receiver<Prompt>> {
        // Settings are read per session, so changes apply from the next one.
        let settings = Arc::clone(&self.settings.borrow());
        Box::pin(async move {
            match session.backend {
                BackendKind::Claude => match hub_claude::version::locate(settings.claude.cli.as_deref()) {
                    Ok(cli) => {
                        ClaudeBackend::new(cli, settings.claude.clone())
                            .run(session, prompt, inbox, conversation)
                            .await
                    }
                    Err(error) => refuse(&conversation, error.to_string(), inbox).await,
                },
                BackendKind::Codex => match hub_codex::version::locate(settings.codex.cli.as_deref()) {
                    Ok(cli) => {
                        CodexBackend::new(cli, settings.codex.clone())
                            .run(session, prompt, inbox, conversation)
                            .await
                    }
                    Err(error) => refuse(&conversation, error.to_string(), inbox).await,
                },
            }
        })
    }
}

async fn refuse(
    conversation: &Conversation,
    reason: String,
    inbox: mpsc::Receiver<Prompt>,
) -> mpsc::Receiver<Prompt> {
    // Nobody listening means the topic's session is already gone.
    let _ = conversation.events.send(AgentEvent::Failed(reason)).await;
    inbox
}
```

`crates/hub-app/src/connector.rs`: `use hub_telegram::agents::ClaudeAgents;` → `use hub_telegram::agents::HubAgents;`, `ClaudeAgents::new(` → `HubAgents::new(`.

- [ ] **Step 4: Прогнать**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates
git commit -m "Хаб запускает Codex в темах с бэкендом codex" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 13: Ключ OpenAI в хранилище ключей

**Files:**
- Modify: `crates/hub-app/src/secrets.rs`
- Modify: `crates/hub-app/src/config.rs`
- Modify: прочие вызовы `Secrets::read`/`write` (`rg "secrets\.(read|write)|impl Secrets for" crates/hub-app`)

**Interfaces:**
- Consumes: `hub_core::settings::{ApiKey, Keys}` (задача 4).
- Produces: `hub_app::secrets::Secret { TelegramToken, OpenAiKey }`; `trait Secrets { fn read(&self, secret: Secret) -> Result<Option<String>, SecretError>; fn write(&self, secret: Secret, value: Option<&str>) -> Result<(), SecretError>; }` (`None` удаляет запись).

- [ ] **Step 1: Написать тесты**

`crates/hub-app/src/config.rs`, тесты:

```rust
    #[test]
    fn api_key_goes_to_the_keyring_not_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut form = draft(dir.path());
        form.codex.api_key = "sk-secret".to_owned();
        let settings = form.parse(dir.path()).unwrap();
        let store = store(dir.path());
        store.save(&settings).unwrap();
        let text = std::fs::read_to_string(dir.path().join("settings.toml")).unwrap();
        assert!(!text.contains("sk-secret"));
        match store.load(dir.path()).unwrap() {
            Loaded::Ready(loaded) => {
                assert_eq!(loaded.codex.api_key.as_ref().map(ApiKey::expose), Some("sk-secret"));
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn cleared_api_key_is_removed_from_the_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let mut form = draft(dir.path());
        form.codex.api_key = "sk-secret".to_owned();
        store.save(&form.parse(dir.path()).unwrap()).unwrap();
        form.codex.api_key = String::new();
        store.save(&form.parse(dir.path()).unwrap()).unwrap();
        match store.load(dir.path()).unwrap() {
            Loaded::Ready(loaded) => assert_eq!(loaded.codex.api_key, None),
            other => panic!("expected Ready, got {other:?}"),
        }
    }
```

(импорт `use hub_core::settings::ApiKey;` в тестах).

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-app config`
Expected: FAIL — первый тест: ключ после загрузки `None`.

- [ ] **Step 3: Реализовать**

`crates/hub-app/src/secrets.rs` — заменить целиком:

```rust
//! Secrets live in the OS credential store, never in a file.

const SERVICE: &str = "agent-hub";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Secret {
    TelegramToken,
    OpenAiKey,
}

impl Secret {
    const fn account(self) -> &'static str {
        match self {
            Self::TelegramToken => "telegram-token",
            Self::OpenAiKey => "openai-api-key",
        }
    }
}

/// The store's own message; it never carries the secret itself.
#[derive(Debug, thiserror::Error)]
#[error("системное хранилище ключей недоступно: {0}")]
pub struct SecretError(String);

pub trait Secrets: Send + Sync {
    fn read(&self, secret: Secret) -> Result<Option<String>, SecretError>;
    /// `None` removes the secret.
    fn write(&self, secret: Secret, value: Option<&str>) -> Result<(), SecretError>;
}

pub struct Keyring;

fn entry(secret: Secret) -> Result<keyring::Entry, SecretError> {
    keyring::Entry::new(SERVICE, secret.account()).map_err(|error| SecretError(error.to_string()))
}

impl Secrets for Keyring {
    fn read(&self, secret: Secret) -> Result<Option<String>, SecretError> {
        match entry(secret)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecretError(error.to_string())),
        }
    }

    fn write(&self, secret: Secret, value: Option<&str>) -> Result<(), SecretError> {
        let entry = entry(secret)?;
        let written = match value {
            Some(value) => entry.set_password(value),
            None => match entry.delete_credential() {
                Err(keyring::Error::NoEntry) => Ok(()),
                other => other,
            },
        };
        written.map_err(|error| SecretError(error.to_string()))
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemorySecrets(std::sync::Mutex<std::collections::HashMap<Secret, String>>);

#[cfg(test)]
impl Secrets for MemorySecrets {
    fn read(&self, secret: Secret) -> Result<Option<String>, SecretError> {
        Ok(self.0.lock().unwrap().get(&secret).cloned())
    }

    fn write(&self, secret: Secret, value: Option<&str>) -> Result<(), SecretError> {
        let mut stored = self.0.lock().unwrap();
        match value {
            Some(value) => stored.insert(secret, value.to_owned()),
            None => stored.remove(&secret),
        };
        Ok(())
    }
}
```

`crates/hub-app/src/config.rs`:
- импорт: `use hub_core::settings::{ApiKey, Draft, FieldError, Keys, Settings, SettingsFile};` и `use crate::secrets::{Secret, Secrets};`;
- в `load`:

```rust
        let keys = Keys {
            token: self.secrets.read(Secret::TelegramToken)?.unwrap_or_default(),
            api_key: self.secrets.read(Secret::OpenAiKey)?.unwrap_or_default(),
        };
        let draft = file.to_draft(keys).map_err(|error| corrupt(error.message))?;
```

- в `save`:

```rust
        // The secrets go first: a file pointing at a token that was never stored is worse.
        self.secrets.write(Secret::TelegramToken, Some(settings.telegram.token.expose()))?;
        self.secrets.write(Secret::OpenAiKey, settings.codex.api_key.as_ref().map(ApiKey::expose))?;
        write_atomic(&self.path, text.as_bytes()).map_err(write)
```

Остальные вызовы из `rg` перевести на `Secret::TelegramToken`.

- [ ] **Step 4: Прогнать**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates
git commit -m "Ключ OpenAI хранится в системном хранилище ключей" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 14: Проверка агентов при запуске бота

**Files:**
- Modify: `crates/hub-app/Cargo.toml` (`hub-codex.workspace = true`)
- Modify: `crates/hub-app/src/connector.rs`
- Modify: `crates/hub-app/src/supervisor.rs` (`Snapshot`, `Started`, `start`, тесты)
- Modify: `crates/hub-app/src/gui/status.rs` (временно — строка агентов; окончательный вид в задаче 15)

**Interfaces:**
- Consumes: `hub_claude::version::{check, locate, CliError}`, `hub_codex::version::{check, locate, CliError}`, `hub_codex::backend::{probe_auth, ProbeError}`, `hub_codex::protocol::CodexAuth`.
- Produces:
  - `hub_app::supervisor::AgentState { Ready { version: String, auth: Option<CodexAuth> }, Unavailable(String) }`, `hub_app::supervisor::AgentStatus { kind: BackendKind, state: AgentState }`.
  - `Snapshot.agents: Vec<AgentStatus>` вместо `claude: Option<String>`; `Started.agents: Vec<AgentStatus>` вместо `claude: String`.
  - `hub_app::connector::AgentError { Claude(hub_claude::version::CliError), Codex(hub_codex::version::CliError), Login(ProbeError) }`; `StartError::Agent(AgentError)` вместо `StartError::Claude`.
  - `hub_app::connector::statuses(default: BackendKind, checked: Vec<(BackendKind, Result<AgentState, AgentError>)>) -> Result<Vec<AgentStatus>, AgentError>`.

- [ ] **Step 1: Написать тесты**

`crates/hub-app/src/connector.rs`, тесты:

```rust
    fn ready(version: &str) -> AgentState {
        AgentState::Ready { version: version.to_owned(), auth: None }
    }

    #[test]
    fn the_default_agent_must_be_available() {
        let checked = vec![
            (BackendKind::Claude, Err(AgentError::Claude(hub_claude::version::CliError::Missing))),
            (BackendKind::Codex, Ok(ready("0.160.0"))),
        ];
        assert!(matches!(statuses(BackendKind::Claude, checked), Err(AgentError::Claude(_))));
    }

    #[test]
    fn another_agent_may_be_unavailable() {
        let checked = vec![
            (BackendKind::Claude, Ok(ready("2.1.287"))),
            (BackendKind::Codex, Err(AgentError::Codex(hub_codex::version::CliError::Missing))),
        ];
        assert_eq!(
            statuses(BackendKind::Claude, checked).unwrap(),
            [
                AgentStatus { kind: BackendKind::Claude, state: ready("2.1.287") },
                AgentStatus {
                    kind: BackendKind::Codex,
                    state: AgentState::Unavailable(
                        "Codex не найден: укажите путь в настройках или установите `codex` в PATH".to_owned()
                    ),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_missing_default_agent_stops_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let connector = TelegramConnector {
            home: dir.path().to_path_buf(),
            topics: dir.path().join("topics.json"),
        };
        let settings = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: dir.path().display().to_string(),
            cli: dir.path().join("нет-claude").display().to_string(),
            ..Draft::default()
        }
        .parse(dir.path())
        .unwrap();
        let receiver = watch::Sender::new(Arc::new(settings)).subscribe();
        let error = connector.connect(receiver).await.err().unwrap();
        assert!(matches!(error, StartError::Agent(AgentError::Claude(_))));
        assert!(!TelegramConnector::transient(&error));
    }
```

(импорт `use hub_core::domain::BackendKind;` и `use crate::supervisor::{AgentState, AgentStatus};` в тестах).

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-app connector`
Expected: FAIL — `cannot find type AgentState`.

- [ ] **Step 3: Реализовать состояние в супервизоре**

`crates/hub-app/Cargo.toml`, `[dependencies]`: `hub-codex.workspace = true`, `hub-core` уже есть.

`crates/hub-app/src/supervisor.rs`:

```rust
use hub_codex::protocol::CodexAuth;
use hub_core::domain::BackendKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    Ready { version: String, auth: Option<CodexAuth> },
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatus {
    pub kind: BackendKind,
    pub state: AgentState,
}
```

- `Snapshot`: поле `claude: Option<String>` → `agents: Vec<AgentStatus>`; в `Default` — `agents: Vec::new()`.
- `Started`: `claude: String` → `agents: Vec<AgentStatus>`.
- `start`:

```rust
            Ok(Started { username, agents, mailbox, views, stop }) => {
                tracing::info!(%username, "bot started");
                self.retry = None;
                self.bot = Some(Bot { mailbox, stop });
                self.views = Some(views);
                self.state.agents = agents;
                self.refresh_topics();
                self.status(BotStatus::Running { username });
            }
```

- тестовый коннектор (`claude: "2.1.287".to_owned()`, около строки 483) → `agents: vec![AgentStatus { kind: BackendKind::Claude, state: AgentState::Ready { version: "2.1.287".to_owned(), auth: None } }]`; прочие ссылки на `snapshot.claude`/`state.claude` в тестах (`rg "\.claude\b" crates/hub-app/src`) перевести на `agents`.

`crates/hub-app/src/gui/status.rs` — временно, чтобы собиралось, строку «Claude Code» заменить на:

```rust
        for agent in &snapshot.agents {
            ui.label(agent.kind.name());
            ui.label(format!("{:?}", agent.state));
            ui.end_row();
        }
```

(окончательный вид — задача 15).

- [ ] **Step 4: Реализовать проверку агентов**

`crates/hub-app/src/connector.rs`:

```rust
use futures::future::join_all;
use hub_codex::backend::{ProbeError, probe_auth};
use hub_codex::protocol::CodexAuth;
use hub_core::domain::BackendKind;

use crate::supervisor::{AgentState, AgentStatus, Connector, Started};

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Claude(#[from] hub_claude::version::CliError),
    #[error(transparent)]
    Codex(#[from] hub_codex::version::CliError),
    #[error(transparent)]
    Login(#[from] ProbeError),
}
```

В `StartError` вариант `Claude(#[from] CliError)` заменить на `Agent(#[from] AgentError)`; в `transient` — `StartError::Agent(_)` вместо `StartError::Claude(_)`; удалить `use hub_claude::version::{CliError, check, locate};`.

```rust
async fn check_agent(kind: BackendKind, settings: &Settings) -> Result<AgentState, AgentError> {
    match kind {
        BackendKind::Claude => {
            let cli = hub_claude::version::locate(settings.claude.cli.as_deref())?;
            let version = hub_claude::version::check(&cli).await?;
            Ok(AgentState::Ready { version: version.to_string(), auth: None })
        }
        BackendKind::Codex => {
            let cli = hub_codex::version::locate(settings.codex.cli.as_deref())?;
            let version = hub_codex::version::check(&cli).await?;
            let auth = probe_auth(&cli, &settings.codex).await?;
            if auth == CodexAuth::Missing {
                tracing::warn!("codex is not logged in: run `codex login` or set an OpenAI API key");
            }
            Ok(AgentState::Ready { version: version.to_string(), auth: Some(auth) })
        }
    }
}

/// The default agent must work; another one that does not is reported, not fatal.
pub fn statuses(
    default: BackendKind,
    checked: Vec<(BackendKind, Result<AgentState, AgentError>)>,
) -> Result<Vec<AgentStatus>, AgentError> {
    checked
        .into_iter()
        .map(|(kind, outcome)| match outcome {
            Ok(state) => Ok(AgentStatus { kind, state }),
            Err(error) if kind == default => Err(error),
            Err(error) => Ok(AgentStatus { kind, state: AgentState::Unavailable(error.to_string()) }),
        })
        .collect()
}
```

В `connect` вместо `let cli = locate(...)?; let version = check(&cli).await?;`:

```rust
            let checked = join_all(BackendKind::ALL.iter().map(|&kind| {
                let current = &current;
                async move { (kind, check_agent(kind, current).await) }
            }))
            .await;
            let agents = statuses(current.default_backend, checked)?;
            for agent in &agents {
                if let AgentState::Unavailable(reason) = &agent.state {
                    tracing::warn!(backend = agent.kind.name(), %reason, "agent unavailable");
                }
            }
```

и в `Started { .. }` — `agents,` вместо `claude: version.to_string(),`.

- [ ] **Step 5: Прогнать**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A crates
git commit -m "Запуск бота проверяет оба агента и вход в Codex" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 15: GUI: настройки Codex и статус агентов

**Files:**
- Modify: `crates/hub-app/src/gui/look.rs`
- Modify: `crates/hub-app/src/gui/status.rs`
- Modify: `crates/hub-app/src/gui/settings.rs`

**Interfaces:**
- Consumes: `AgentState`, `AgentStatus` (задача 14); `CodexDraft`, `CodexField`, `Sandbox`, `Approval`, `Field::DefaultBackend` (задача 4).
- Produces: `hub_app::gui::look::{agent_title(kind: BackendKind) -> &'static str, agent_line(state: Option<&AgentState>) -> String}`.

- [ ] **Step 1: Написать тесты**

`crates/hub-app/src/gui/look.rs`, тесты:

```rust
    #[rstest::rstest]
    #[case(None, "—")]
    #[case(Some(AgentState::Ready { version: "2.1.287".to_owned(), auth: None }), "2.1.287")]
    #[case(Some(AgentState::Ready { version: "0.160.0".to_owned(), auth: Some(CodexAuth::ChatGpt) }), "0.160.0 · вход: ChatGPT")]
    #[case(Some(AgentState::Ready { version: "0.160.0".to_owned(), auth: Some(CodexAuth::ApiKey) }), "0.160.0 · вход: API-ключ")]
    #[case(Some(AgentState::Ready { version: "0.160.0".to_owned(), auth: Some(CodexAuth::Missing) }), "0.160.0 · вход: не выполнен — `codex login` или API-ключ")]
    #[case(Some(AgentState::Unavailable("Codex не найден".to_owned())), "недоступен: Codex не найден")]
    fn agent_lines(#[case] state: Option<AgentState>, #[case] expected: &str) {
        assert_eq!(agent_line(state.as_ref()), expected);
    }

    #[test]
    fn agents_have_titles() {
        assert_eq!(agent_title(BackendKind::Claude), "Claude Code");
        assert_eq!(agent_title(BackendKind::Codex), "Codex");
    }
```

(импорты в тестах: `use hub_codex::protocol::CodexAuth;`, `use hub_core::domain::BackendKind;`, `use crate::supervisor::AgentState;`; `rstest` уже в `[dev-dependencies]` `hub-app`).

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-app look`
Expected: FAIL — `cannot find function agent_line`.

- [ ] **Step 3: Реализовать `look`**

`crates/hub-app/src/gui/look.rs`:

```rust
use hub_codex::protocol::CodexAuth;
use hub_core::domain::BackendKind;

use crate::supervisor::AgentState;

#[must_use]
pub const fn agent_title(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Claude => "Claude Code",
        BackendKind::Codex => "Codex",
    }
}

#[must_use]
pub fn agent_line(state: Option<&AgentState>) -> String {
    match state {
        None => "—".to_owned(),
        Some(AgentState::Ready { version, auth: None }) => version.clone(),
        Some(AgentState::Ready { version, auth: Some(auth) }) => {
            format!("{version} · вход: {}", auth_text(*auth))
        }
        Some(AgentState::Unavailable(reason)) => format!("недоступен: {reason}"),
    }
}

const fn auth_text(auth: CodexAuth) -> &'static str {
    match auth {
        CodexAuth::ApiKey => "API-ключ",
        CodexAuth::ChatGpt => "ChatGPT",
        CodexAuth::Other => "другой способ",
        CodexAuth::NotRequired => "не требуется",
        CodexAuth::Missing => "не выполнен — `codex login` или API-ключ",
    }
}
```

`crates/hub-app/src/gui/status.rs` — временный цикл из задачи 14 заменить на:

```rust
        for kind in BackendKind::ALL {
            let state =
                snapshot.agents.iter().find(|agent| agent.kind == kind).map(|agent| &agent.state);
            ui.label(agent_title(kind));
            ui.label(agent_line(state));
            ui.end_row();
        }
```

(импорты: `use hub_core::domain::BackendKind;`, `use crate::gui::look::{active, agent_line, agent_title, status_text};`).

- [ ] **Step 4: Форма настроек**

`crates/hub-app/src/gui/settings.rs`:
- импорт: `use hub_core::domain::BackendKind;` и `use hub_core::settings::{Approval, CodexField, Draft, Field, FieldError, PermissionMode, Sandbox, UpdateCheck};`;
- в `SettingsForm` поле `reveal_key: bool`;
- в `show` порядок секций:

```rust
            self.agent(ui, &errors);
            ui.separator();
            self.codex(ui, &errors);
            ui.separator();
            self.timeouts(ui, &errors);
```

- в начало `fn agent` (перед `ui.heading("Claude Code")`):

```rust
        ui.heading("Агенты");
        ui.horizontal(|ui| {
            ui.label("Агент по умолчанию");
            egui::ComboBox::from_id_salt("default_backend")
                .selected_text(self.draft.default_backend.name())
                .show_ui(ui, |ui| {
                    for kind in BackendKind::ALL {
                        ui.selectable_value(&mut self.draft.default_backend, kind, kind.name());
                    }
                });
        });
        ui.label("Используется в /new без имени агента; /backend меняет агента в теме.");
        ui.separator();
```

- новый метод:

```rust
    fn codex(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Codex");
        ui.horizontal(|ui| {
            ui.label("Путь к codex");
            ui.add(egui::TextEdit::singleline(&mut self.draft.codex.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.codex.cli)
            {
                self.draft.codex.cli = path;
            }
        });
        messages(ui, errors, Field::Codex(CodexField::Cli));
        ui.horizontal(|ui| {
            ui.label("Модель");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.codex.model)
                    .hint_text("из настроек Codex"),
            );
        });
        messages(ui, errors, Field::Codex(CodexField::Model));
        ui.horizontal(|ui| {
            ui.label("Песочница");
            egui::ComboBox::from_id_salt("codex_sandbox")
                .selected_text(self.draft.codex.sandbox.wire())
                .show_ui(ui, |ui| {
                    for sandbox in Sandbox::ALL {
                        ui.selectable_value(&mut self.draft.codex.sandbox, sandbox, sandbox.wire());
                    }
                });
        });
        if self.draft.codex.sandbox == Sandbox::DangerFullAccess {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "danger-full-access: команды Codex выполняются без песочницы ОС.",
            );
        }
        ui.horizontal(|ui| {
            ui.label("Одобрения");
            egui::ComboBox::from_id_salt("codex_approval")
                .selected_text(self.draft.codex.approval.wire())
                .show_ui(ui, |ui| {
                    for approval in Approval::ALL {
                        ui.selectable_value(&mut self.draft.codex.approval, approval, approval.wire());
                    }
                });
        });
        if self.draft.codex.approval == Approval::Never {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "never: Codex ничего не спрашивает перед действиями.",
            );
        }
        ui.horizontal(|ui| {
            ui.label("API-ключ OpenAI");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.codex.api_key)
                    .password(!self.reveal_key)
                    .hint_text("вход через codex login"),
            );
            ui.checkbox(&mut self.reveal_key, "показать");
        });
        messages(ui, errors, Field::Codex(CodexField::ApiKey));
        ui.label(
            "Без ключа используется вход `codex login`. Ключ хранится в системном хранилище \
             ключей и не заменяет вход по подписке ChatGPT.",
        );
    }
```

- [ ] **Step 5: Прогнать и посмотреть**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets`
Expected: PASS.

Run: `cargo run -p hub-app` — на вкладке «Настройки» видны «Агенты» и «Codex», на «Статус» — строки «Claude Code» и «Codex» (до запуска бота — «—»). Закрыть через меню трея «Выход» или Диспетчер задач.

- [ ] **Step 6: Commit**

```bash
git add -A crates
git commit -m "GUI: настройки Codex и статус агентов" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 16: Живой тест, README и ручная проверка

**Files:**
- Create: `crates/hub-codex/tests/live.rs`
- Modify: `README.md`

**Interfaces:**
- Consumes: `hub_codex::backend::CodexBackend`, `hub_codex::version::{check, locate}`.

- [ ] **Step 1: Живой тест**

`crates/hub-codex/tests/live.rs`:

```rust
//! Round trip through the real Codex app-server. Spends a few tokens, so it is opt-in:
//! `AGENT_HUB_LIVE_CODEX=1 cargo test -p hub-codex --test live -- --ignored`.

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_agent::channel::UserChannel;
    use hub_agent::conversation::{Conversation, Limits};
    use hub_codex::backend::CodexBackend;
    use hub_codex::version::{check, locate};
    use hub_core::domain::{
        AbsolutePath, AgentEvent, BackendKind, Decision, FileDelivery, OutgoingFile, Prompt,
        Question, QuestionsOutcome, ToolRequest, TopicSession, Usage,
    };
    use hub_core::settings::{Approval, CodexSettings, Sandbox};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    struct Denying;

    impl UserChannel for Denying {
        fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
            Box::pin(async { Decision::Denied(hub_core::domain::Denied::new("live test")) })
        }
        fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            Box::pin(async { QuestionsOutcome::Answered(Vec::new()) })
        }
        fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            Box::pin(async { FileDelivery::Delivered })
        }
    }

    #[tokio::test]
    #[ignore = "needs a logged-in codex and spends tokens"]
    async fn codex_answers_a_prompt() {
        if std::env::var_os("AGENT_HUB_LIVE_CODEX").is_none() {
            return;
        }
        let cli = locate(None).unwrap();
        check(&cli).await.unwrap();
        let settings = CodexSettings {
            cli: None,
            model: None,
            sandbox: Sandbox::ReadOnly,
            approval: Approval::Untrusted,
            api_key: None,
        };
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        let session = TopicSession::fresh(BackendKind::Codex, cwd);
        let (events, mut received) = mpsc::channel(256);
        let conversation = Conversation {
            channel: Arc::new(Denying),
            events,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_secs(60)),
        };
        let (_inbox_in, inbox) = mpsc::channel(1);
        let prompt = Prompt::new("Ответь одним словом: да".to_owned(), Vec::new()).unwrap();

        let run = CodexBackend::new(cli, settings).run(&session, prompt, inbox, conversation);
        let _inbox = tokio::time::timeout(Duration::from_mins(3), run).await.unwrap();

        let mut seen = Vec::new();
        while let Ok(event) = received.try_recv() {
            seen.push(event);
        }
        assert!(matches!(seen.first(), Some(AgentEvent::SessionStarted(_))), "{seen:?}");
        assert!(
            matches!(
                seen.last(),
                Some(AgentEvent::Finished(finished)) if matches!(finished.usage, Usage::Codex { tokens: Some(_) })
            ),
            "{seen:?}"
        );
    }
}
```

`crates/hub-codex/Cargo.toml`, `[dev-dependencies]`: добавить `tokio-util.workspace = true`, если интеграционный тест его не видит (он видит обычные зависимости — добавлять не нужно).

Run: `cargo test -p hub-codex --test live`
Expected: PASS (тест пропущен как `ignored`).

- [ ] **Step 2: README**

`README.md`:
- «Требования»: строка «Codex CLI 0.160 или новее (`npm i -g @openai/codex`) — если нужен агент Codex; вход — `codex login` или API-ключ OpenAI в настройках».
- «Команды»: после `/new` — «`/backend [claude|codex]` — сменить агента в этой теме (сброс контекста)»; в описании `/new` — «без имени агента используется агент по умолчанию из настроек».
- Новый подраздел «Codex» в «Использование»:

```markdown
### Codex

Тема работает с Claude Code или с Codex: `/new codex <путь>` создаёт сессию Codex, `/backend codex` переключает существующую тему (контекст сбрасывается). Агент по умолчанию задаётся в настройках.

Вход в Codex — один из двух способов:
- подписка ChatGPT: выполните `codex login` в терминале один раз;
- API-ключ OpenAI: укажите его в «Настройки → Codex». Ключ используется, только если входа нет или это уже вход по ключу; вход по подписке ChatGPT не заменяется (для перехода на ключ — `codex logout`).

Права Codex задаются в «Настройки → Codex»: песочница (`read-only`, `workspace-write` — по умолчанию, `danger-full-access`) и одобрения (`untrusted`, `on-request` — по умолчанию, `never`). Одобрения команд и правок приходят в тему кнопками, как у Claude. В итоге хода Codex показывает число токенов в сессии; фоновых задач у Codex нет.
```

- «Настройки и файлы»: таблицы `[agents]` (`default`) и `[codex]` (`cli`, `model`, `sandbox`, `approval`); «API-ключ OpenAI хранится в системном хранилище ключей (`agent-hub` / `openai-api-key`), не в файле».
- «Окно и трей» / «Статус»: на вкладке «Статус» — версия и вход каждого агента.

- [ ] **Step 3: Полная проверка**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets && cargo fmt --all -- --check`
Expected: PASS, без предупреждений.

- [ ] **Step 4: Ручная проверка на Windows**

1. Установить Codex: `npm i -g @openai/codex`, `codex --version` ≥ 0.160.0, `codex login`.
2. `cargo run -p hub-app --release`; вкладка «Статус»: «Codex 0.160.x · вход: ChatGPT».
3. В теме Telegram: `/backend codex` → «🔀 Бэкенд изменён … backend: codex».
4. Написать задачу, требующую команды (например, «выведи список файлов»): приходят строка `🔧 shell …`, кнопки одобрения; после «Разрешить» — ответ и «✅ Готово · токенов в сессии: N».
5. Во время хода отправить второе сообщение — оно учитывается в том же ходе (или следующим ходом).
6. `/stop` во время ожидания кнопки → «⏹ Остановлено», процесс `codex` исчезает из Диспетчера задач в течение 5 с.
7. Приложить фото с вопросом — Codex его описывает.
8. Попросить «пришли файл README.md» — файл приходит в тему.
9. `/backend claude` и новая задача — работает Claude, как раньше.
10. Убрать `codex` из PATH и задать бэкенд по умолчанию Claude → бот стартует, «Статус»: «Codex · недоступен: Codex не найден…».

- [ ] **Step 5: Commit**

```bash
git add -A crates README.md
git commit -m "Codex: живой тест и документация" -m "Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```
