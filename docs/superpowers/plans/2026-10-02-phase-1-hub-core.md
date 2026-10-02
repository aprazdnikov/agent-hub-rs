# Фаза 1: hub-core — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cargo workspace и крейт `hub-core` — функциональное ядро agent-hub-rs без I/O: доменные типы, форматирование, Markdown → Telegram HTML, кнопки подтверждений и вопросов, вложения, команды, настройки, формат `topics.json`.

**Architecture:** Чистые функции и типы, перенесённые из Python-версии `agent-hub` (эталон поведения — её модули и тесты). Крейт не зависит от tokio и не обращается к файловой системе; реестры ожидающих ответов обобщены по типу «ответчика», чтобы оболочка подставила `oneshot::Sender`, а тесты — `()`.

**Tech Stack:** Rust 1.96 (edition 2024), pulldown-cmark 0.13, rust_decimal 1.43, serde 1, serde_json 1, thiserror 2, rstest 0.27.

**Spec:** `docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md`

## Дорожная карта

| Фаза | План | Результат |
|---|---|---|
| 1 | этот файл | `hub-core` + workspace, все тесты зелёные |
| 2 | `2026-10-02-phase-2-hub-claude.md` (пишется после фазы 1) | Протокол CLI, фейковый CLI, сессия с фоновыми задачами |
| 3 | `…-phase-3-hub-telegram.md` | Актор `Hub`, teloxide, `UserChannel`, файловые проверки путей |
| 4 | `…-phase-4-hub-app.md` | Супервизор, хранилища, keyring, GUI, трей, single-instance |
| 5 | `…-phase-5-release.md` | CI, релизы, автообновление, README |

## Global Constraints

- Тулчейн `1.96`, edition `2024`, resolver `3`.
- `[workspace.lints]`: `rust.warnings = "deny"`, `unsafe_code = "forbid"`, clippy `pedantic` deny, `unwrap_used`, `expect_used`, `panic`, `indexing_slicing` = deny; `missing_errors_doc` = allow.
- В тестах `unwrap`/`expect`/`panic`/индексация разрешены через `clippy.toml`.
- Длины текста считаются в символах (`chars().count()`), как `len()` в Python.
- Тексты для пользователя — на русском, байт-в-байт как в Python-версии.
- `hub-core` не зависит от tokio и не делает I/O.
- Комментарии — только «почему».
- Коммиты без упоминаний Claude/ассистента и без трейлеров `Co-Authored-By`.

## Review Focus

1. Markdown с конструкциями, которые pulldown-cmark разбирает иначе, чем markdown-it (опасные ссылки `javascript:`, сырой HTML, tight/loose списки) — результат обязан содержать только теги из белого списка Telegram и экранированный текст. Тест: `only_telegram_tags_are_emitted`, `dangerous_link_is_plain_text`, `raw_html_is_escaped`.
2. Очень длинные «слова» и сущности (`&` × 300, строка без переводов) — разбиение обязано завершаться и укладываться в лимит. Тест: `oversized_paragraph_falls_back_to_escaped_text`, `single_char_over_limit_terminates`.
3. Не-ASCII в путях, именах файлов и callback-данных (кириллица, `²`) — без паник на границах UTF-8. Тесты: `safe_filename` с кириллицей, `malformed_callback_data_is_rejected` с `²`, `split_message_respects_char_boundaries`.
4. `topics.json`, записанный Python-версией, читается без потерь. Тест: `python_written_state_is_read`.
5. Токен бота не попадает в `Debug`-вывод настроек и черновика формы. Тест: `debug_output_hides_token`.

---

### Task 1: Workspace и доменные типы

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `clippy.toml`, `rustfmt.toml`
- Create: `crates/hub-core/Cargo.toml`, `crates/hub-core/src/lib.rs`, `crates/hub-core/src/domain.rs`, `crates/hub-core/src/ids.rs`
- Modify: `.gitignore` (убрать JetBrains-комментарии не нужно; добавить `/target` уже есть — без изменений)
- Modify: `docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md` (уточнения раздела крейтов)

**Interfaces:**
- Produces: `ChatId(i64)`, `ThreadId(i32)`, `UserId(u64)`, `MessageId(i32)`, `TopicKey { chat, thread }`, `SessionId::parse(&str) -> Option<SessionId>`, `SessionId::as_str`, `BackendKind::{Claude, ALL, name(), parse()}`, `AbsolutePath::new(PathBuf) -> Option<_>`, `AbsolutePath::as_path`, `TopicSession { backend, cwd, session }`, `TopicSession::fresh`, `TopicSession::with_session`, `ToolRequest { tool, summary }`, `ToolUse { tool, summary }`, `Denied { reason }`, `Denied::new`, `Decision::{Allowed, Denied}`, `Selection::{Single, Multiple}`, `QuestionOption { label, description }`, `Question::new(text, header, options, selection) -> Option<Question>` + геттеры `text() header() options() selection()`, `QuestionAnswer { question, answer }`, `QuestionsOutcome::{Answered(Vec<QuestionAnswer>), Denied(Denied)}`, `OutgoingFile { path, caption }`, `FileDelivery::{Delivered, Denied}`, `ImageMediaType::{Jpeg, Png, Gif, Webp}` + `mime()`, `Image { media, data }`, `Prompt::new(String, Vec<Image>) -> Option<Prompt>` + `text() images()`, `Finished { session, turns, cost, background }`, `AgentEvent::{SessionStarted, AssistantText, ToolCall(ToolUse), Finished, Failed, BackgroundAbandoned}`, `ids::hex(&[u8]) -> String` (crate-private).

- [ ] **Step 1: Создать конфигурацию workspace**

`Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crates/hub-core"]

[workspace.package]
version = "0.1.0"
edition = "2024"
rust-version = "1.96"
license = "MIT"
repository = "https://github.com/aprazdnikov/agent-hub-rs"
publish = false

[workspace.dependencies]
pulldown-cmark = { version = "0.13", default-features = false }
rust_decimal = "1.43"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
thiserror = "2.0"
rstest = "0.27"

[workspace.lints.rust]
warnings = "deny"
unsafe_code = "forbid"

[workspace.lints.clippy]
pedantic = { level = "deny", priority = -1 }
missing_errors_doc = "allow"
unwrap_used = "deny"
expect_used = "deny"
panic = "deny"
indexing_slicing = "deny"
```

`rust-toolchain.toml`:

```toml
[toolchain]
channel = "1.96"
components = ["rustfmt", "clippy"]
```

`clippy.toml`:

```toml
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-panic-in-tests = true
allow-indexing-slicing-in-tests = true
```

`rustfmt.toml`:

```toml
edition = "2024"
max_width = 100
```

`crates/hub-core/Cargo.toml`:

```toml
[package]
name = "hub-core"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
pulldown-cmark.workspace = true
rust_decimal.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true

[dev-dependencies]
rstest.workspace = true

[lints]
workspace = true
```

`crates/hub-core/src/lib.rs`:

```rust
pub mod domain;
mod ids;
```

- [ ] **Step 2: Написать падающие тесты доменных типов**

`crates/hub-core/src/domain.rs` (только тестовый модуль, типов ещё нет):

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rstest::rstest;

    use super::*;

    fn absolute() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" })
    }

    #[rstest]
    #[case("", None)]
    #[case("   ", None)]
    #[case(" abc ", Some("abc"))]
    fn session_id_is_non_empty(#[case] raw: &str, #[case] expected: Option<&str>) {
        assert_eq!(SessionId::parse(raw).as_ref().map(SessionId::as_str), expected);
    }

    #[rstest]
    #[case("claude", Some(BackendKind::Claude))]
    #[case("Claude", Some(BackendKind::Claude))]
    #[case("gpt", None)]
    fn backend_kind_parses_case_insensitively(
        #[case] raw: &str,
        #[case] expected: Option<BackendKind>,
    ) {
        assert_eq!(BackendKind::parse(raw), expected);
    }

    #[test]
    fn relative_cwd_is_rejected() {
        assert!(AbsolutePath::new(PathBuf::from("rel")).is_none());
        assert_eq!(AbsolutePath::new(absolute()).map(|p| p.as_path().to_path_buf()), Some(absolute()));
    }

    #[test]
    fn prompt_needs_text_or_images() {
        assert!(Prompt::new("  ".to_owned(), Vec::new()).is_none());
        let image = Image { media: ImageMediaType::Png, data: vec![1] };
        let prompt = Prompt::new(String::new(), vec![image]).unwrap();
        assert_eq!(prompt.images().len(), 1);
        assert_eq!(Prompt::new("hi".to_owned(), Vec::new()).unwrap().text(), "hi");
    }

    #[test]
    fn question_needs_text() {
        assert!(Question::new(" ".to_owned(), String::new(), Vec::new(), Selection::Single).is_none());
    }

    #[test]
    fn with_session_replaces_only_session() {
        let cwd = AbsolutePath::new(absolute()).unwrap();
        let session = TopicSession::fresh(BackendKind::Claude, cwd.clone());
        let resumed = session.with_session(SessionId::parse("s"));
        assert_eq!(resumed.cwd, cwd);
        assert_eq!(resumed.session, SessionId::parse("s"));
    }

    #[test]
    fn image_media_types_are_api_mime_types() {
        let mimes: Vec<_> = [ImageMediaType::Jpeg, ImageMediaType::Png, ImageMediaType::Gif, ImageMediaType::Webp]
            .into_iter()
            .map(ImageMediaType::mime)
            .collect();
        assert_eq!(mimes, ["image/jpeg", "image/png", "image/gif", "image/webp"]);
    }
}
```

- [ ] **Step 3: Убедиться, что тесты не компилируются**

Run: `cargo test -p hub-core`
Expected: FAIL — `cannot find type SessionId` и т. п.

- [ ] **Step 4: Реализовать доменные типы**

Вставить над тестовым модулем `crates/hub-core/src/domain.rs`:

```rust
use std::path::{Path, PathBuf};

use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChatId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThreadId(pub i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageId(pub i32);

/// One forum topic of one Telegram chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TopicKey {
    pub chat: ChatId,
    pub thread: ThreadId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    Claude,
}

impl BackendKind {
    pub const ALL: [Self; 1] = [Self::Claude];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name().eq_ignore_ascii_case(raw))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AbsolutePath(PathBuf);

impl AbsolutePath {
    #[must_use]
    pub fn new(path: PathBuf) -> Option<Self> {
        path.is_absolute().then_some(Self(path))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicSession {
    pub backend: BackendKind,
    pub cwd: AbsolutePath,
    pub session: Option<SessionId>,
}

impl TopicSession {
    #[must_use]
    pub fn fresh(backend: BackendKind, cwd: AbsolutePath) -> Self {
        Self { backend, cwd, session: None }
    }

    #[must_use]
    pub fn with_session(self, session: Option<SessionId>) -> Self {
        Self { session, ..self }
    }
}

/// A tool call the agent wants to make, awaiting a human decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRequest {
    pub tool: String,
    pub summary: String,
}

/// A tool call the agent made, shown to the user as one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUse {
    pub tool: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied {
    pub reason: String,
}

impl Denied {
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self { reason: reason.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    Denied(Denied),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Single,
    Multiple,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// A clarifying question, answered by an option label or free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    text: String,
    header: String,
    options: Vec<QuestionOption>,
    selection: Selection,
}

impl Question {
    #[must_use]
    pub fn new(
        text: String,
        header: String,
        options: Vec<QuestionOption>,
        selection: Selection,
    ) -> Option<Self> {
        (!text.trim().is_empty()).then_some(Self { text, header, options, selection })
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn header(&self) -> &str {
        &self.header
    }

    #[must_use]
    pub fn options(&self) -> &[QuestionOption] {
        &self.options
    }

    #[must_use]
    pub fn selection(&self) -> Selection {
        self.selection
    }
}

/// Answer keyed by question text; multi-select labels are joined with ", ".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionAnswer {
    pub question: String,
    pub answer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionsOutcome {
    Answered(Vec<QuestionAnswer>),
    Denied(Denied),
}

/// A file the agent asks to deliver; `path` is as the agent wrote it, not yet checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingFile {
    pub path: String,
    pub caption: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDelivery {
    Delivered,
    Denied(Denied),
}

/// Image formats the model accepts inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMediaType {
    Jpeg,
    Png,
    Gif,
    Webp,
}

impl ImageMediaType {
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub media: ImageMediaType,
    pub data: Vec<u8>,
}

/// One user turn: text plus images sent inline to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    text: String,
    images: Vec<Image>,
}

impl Prompt {
    #[must_use]
    pub fn new(text: String, images: Vec<Image>) -> Option<Self> {
        (!text.trim().is_empty() || !images.is_empty()).then_some(Self { text, images })
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn images(&self) -> &[Image] {
        &self.images
    }
}

/// One agent turn ended; `background` tasks keep running and report in later turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub session: SessionId,
    pub turns: u32,
    pub cost: Option<Decimal>,
    pub background: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    SessionStarted(SessionId),
    AssistantText(String),
    ToolCall(ToolUse),
    Finished(Finished),
    Failed(String),
    BackgroundAbandoned(Vec<String>),
}
```

`crates/hub-core/src/ids.rs`:

```rust
use std::fmt::Write;

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
        // Writing into a String cannot fail.
        let _ = write!(out, "{byte:02x}");
        out
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_is_lowercase_and_padded() {
        assert_eq!(super::hex(&[0x0a, 0xff]), "0aff");
    }
}
```

- [ ] **Step 5: Прогнать тесты и линтеры**

Run: `cargo test -p hub-core && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS, без предупреждений. Если `warnings = "deny"` в `[workspace.lints.rust]` отвергается cargo — заменить на `.cargo/config.toml` с `[build] rustflags = ["-D", "warnings"]` и отметить это в спеке.

- [ ] **Step 6: Уточнить спеку**

В `docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md`, раздел «Cargo workspace», заменить строки таблицы `hub-core` и `hub-claude`:

```markdown
| `hub-core` | Функциональное ядро без I/O: доменные типы, разбор настроек и формы, раскрытие путей workspace (без обращения к ФС), имена и пути вложений, разбор команд, Markdown → Telegram HTML, разбиение сообщений, callback_data, реестры подтверждений и вопросов (обобщённые по ответчику), формат `topics.json` | serde, serde_json, pulldown-cmark, rust_decimal, thiserror |
| `hub-claude` | Протокол `claude` CLI: чистые модули `wire`, `activity` (`SessionActivity`), `tracker` (`SessionTracker`), `permissions` (решения `can_use_tool`, разбор вопросов, сводки инструментов) + оболочка: процесс, транспорт, control-запросы, MCP `send_file`, трейты `AgentBackend` и `UserChannel` | tokio, serde_json, tokio-util |
```

и добавить после таблицы абзац:

```markdown
Проверки путей, требующие файловой системы (канонизация, симлинки, существование каталога, размер файла), живут в оболочках: `hub-telegram::paths` (`/new`, `/cwd`, вложения, `send_file`) и `hub-app` (корень workspace из настроек). Канонизация — `dunce::canonicalize`, чтобы на Windows не появлялся префикс `\\?\`.
```

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock rust-toolchain.toml clippy.toml rustfmt.toml crates docs
git commit -m "Workspace и доменные типы hub-core"
```

---

### Task 2: Экранирование и форматирование сообщений

**Files:**
- Create: `crates/hub-core/src/escape.rs`, `crates/hub-core/src/render.rs`
- Modify: `crates/hub-core/src/lib.rs`

**Interfaces:**
- Consumes: —
- Produces: `escape::escape(&str) -> String`, `escape::plain(&str) -> String`, `render::TELEGRAM_TEXT_LIMIT: NonZeroUsize`, `render::char_len(&str) -> usize`, `render::split_message(&str, NonZeroUsize) -> Vec<String>`, `render::truncate(&str, usize) -> String`, `render::format_finished(u32, Option<Decimal>, usize) -> String`, `render::format_abandoned(&[String], Duration) -> String`.

- [ ] **Step 1: Написать падающие тесты**

`crates/hub-core/src/escape.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup_characters() {
        assert_eq!(escape(r#"a < b & "c" > 'd'"#), "a &lt; b &amp; &quot;c&quot; &gt; &#x27;d&#x27;");
    }

    #[test]
    fn plain_drops_tags_and_decodes_entities() {
        assert_eq!(plain("<b>a &amp; b</b> <code>&lt;x&gt;</code>"), "a & b <x>");
    }

    #[test]
    fn plain_keeps_lone_brackets_and_unknown_entities() {
        assert_eq!(plain("a <> b &nbsp c &#65; &#x42;"), "a <> b &nbsp c A B");
    }

    #[test]
    fn plain_round_trips_escape() {
        let text = r#"if a < b && c > "d" { 'e' }"#;
        assert_eq!(plain(&escape(text)), text);
    }
}
```

`crates/hub-core/src/render.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use rstest::rstest;
    use rust_decimal::Decimal;

    use super::*;

    fn limit(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    #[test]
    fn short_text_is_one_chunk() {
        assert_eq!(split_message("  hello  ", TELEGRAM_TEXT_LIMIT), ["hello"]);
    }

    #[test]
    fn empty_text_has_no_chunks() {
        assert!(split_message("   ", TELEGRAM_TEXT_LIMIT).is_empty());
    }

    #[test]
    fn long_text_splits_on_line_boundary() {
        assert_eq!(split_message("aaaa\nbbbb\ncccc", limit(9)), ["aaaa\nbbbb", "cccc"]);
    }

    #[test]
    fn line_longer_than_limit_is_hard_split() {
        assert_eq!(split_message("abcdefgh", limit(3)), ["abc", "def", "gh"]);
    }

    #[test]
    fn every_chunk_respects_limit() {
        let text = (1..60).map(|n| "x".repeat(n)).collect::<Vec<_>>().join("\n");
        let chunks = split_message(&text, limit(50));
        assert!(chunks.iter().all(|chunk| char_len(chunk) <= 50));
        assert_eq!(chunks.concat().replace('\n', ""), text.replace('\n', ""));
    }

    #[test]
    fn split_message_respects_char_boundaries() {
        assert_eq!(split_message("абвгдеё", limit(3)), ["абв", "где", "ё"]);
    }

    #[test]
    fn truncate_marks_cut() {
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(truncate("abc", 4), "abc");
        assert_eq!(truncate("абвгд", 3), "аб…");
    }

    #[rstest]
    #[case(None, "✅ Готово · ходов: 3")]
    #[case(Some("0.1234"), "✅ Готово · ходов: 3 · $0.12")]
    #[case(Some("2"), "✅ Готово · ходов: 3 · $2.00")]
    #[case(Some("0.125"), "✅ Готово · ходов: 3 · $0.12")]
    fn finished_line(#[case] cost: Option<&str>, #[case] expected: &str) {
        let cost = cost.map(|raw| raw.parse::<Decimal>().unwrap());
        assert_eq!(format_finished(3, cost, 0), expected);
    }

    #[test]
    fn finished_mentions_running_background_tasks() {
        assert_eq!(
            format_finished(3, None, 2),
            "✅ Готово · ходов: 3 · ⏳ в фоне задач: 2, пришлю результат"
        );
    }

    #[test]
    fn abandoned_lists_tasks() {
        let tasks = ["sleep 600".to_owned(), "npm test".to_owned()];
        assert_eq!(
            format_abandoned(&tasks, Duration::from_secs(1800)),
            "⌛ Фоновые задачи не завершились за 30 мин, сессия закрыта:\n• sleep 600\n• npm test"
        );
    }
}
```

`crates/hub-core/src/lib.rs`:

```rust
pub mod domain;
pub mod escape;
mod ids;
pub mod render;
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-core`
Expected: FAIL — `cannot find function escape` и т. п.

- [ ] **Step 3: Реализовать `escape.rs`**

Над тестовым модулем:

```rust
/// Longest entity we decode, e.g. `&#x10FFFF;`; bounds the scan for `;`.
const MAX_ENTITY_CHARS: usize = 12;

#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Fallback when Telegram rejects the markup: drop tags, decode entities.
#[must_use]
pub fn plain(html: &str) -> String {
    unescape(&strip_tags(html))
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        let (before, tail) = rest.split_at(start);
        out.push_str(before);
        let after = tail.get(1..).unwrap_or_default();
        match after.find('>') {
            Some(end) if end > 0 => rest = after.get(end + 1..).unwrap_or_default(),
            Some(_) | None => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        let (before, tail) = rest.split_at(start);
        out.push_str(before);
        match decode_entity(tail) {
            Some((decoded, used)) => {
                out.push(decoded);
                rest = tail.get(used..).unwrap_or_default();
            }
            None => {
                out.push('&');
                rest = tail.get(1..).unwrap_or_default();
            }
        }
    }
    out.push_str(rest);
    out
}

/// `tail` starts with `&`; returns the character and the byte length of the entity.
fn decode_entity(tail: &str) -> Option<(char, usize)> {
    let (end, _) = tail.char_indices().take(MAX_ENTITY_CHARS).find(|&(_, c)| c == ';')?;
    let name = tail.get(1..end)?;
    let decoded = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((decoded, end + 1))
}
```

- [ ] **Step 4: Реализовать `render.rs`**

Над тестовым модулем:

```rust
use std::num::NonZeroUsize;
use std::time::Duration;

use rust_decimal::Decimal;

// `NonZeroUsize::new(..).unwrap()` is not allowed by the lints; 1 + 4095 is const and total.
pub const TELEGRAM_TEXT_LIMIT: NonZeroUsize = NonZeroUsize::MIN.saturating_add(4095);

#[must_use]
pub fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// Byte offset of the `n`-th character, or the end of `text`.
fn char_offset(text: &str, n: usize) -> usize {
    text.char_indices().nth(n).map_or(text.len(), |(offset, _)| offset)
}

/// Split `text` into chunks of at most `limit` chars, preferring line boundaries.
#[must_use]
pub fn split_message(text: &str, limit: NonZeroUsize) -> Vec<String> {
    let limit = limit.get();
    let mut chunks = Vec::new();
    let mut rest = text.trim();
    while char_len(rest) > limit {
        let (window, _) = rest.split_at(char_offset(rest, limit + 1));
        let cut = match window.rfind('\n') {
            Some(newline) if newline > 0 => newline,
            Some(_) | None => char_offset(rest, limit),
        };
        let (head, tail) = rest.split_at(cut);
        chunks.push(head.trim_end().to_owned());
        rest = tail.trim_start_matches('\n');
    }
    if !rest.is_empty() {
        chunks.push(rest.to_owned());
    }
    chunks
}

#[must_use]
pub fn truncate(text: &str, limit: usize) -> String {
    if char_len(text) <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[must_use]
pub fn format_finished(turns: u32, cost: Option<Decimal>, background: usize) -> String {
    let cost = cost.map_or_else(String::new, |cost| {
        // Banker's rounding, as Python's Decimal.quantize, then always two decimals.
        let mut cents = cost.round_dp(2);
        cents.rescale(2);
        format!(" · ${cents}")
    });
    let pending = if background == 0 {
        String::new()
    } else {
        format!(" · ⏳ в фоне задач: {background}, пришлю результат")
    };
    format!("✅ Готово · ходов: {turns}{cost}{pending}")
}

#[must_use]
pub fn format_abandoned(tasks: &[String], timeout: Duration) -> String {
    let listing = tasks.iter().map(|task| format!("• {task}")).collect::<Vec<_>>().join("\n");
    format!(
        "⌛ Фоновые задачи не завершились за {} мин, сессия закрыта:\n{listing}",
        timeout.as_secs() / 60
    )
}
```

- [ ] **Step 5: Прогнать тесты и линтеры**

Run: `cargo test -p hub-core && cargo clippy --all-targets -- -D warnings`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-core
git commit -m "Экранирование HTML и форматирование сообщений"
```

---

### Task 3: Markdown → Telegram HTML

**Files:**
- Create: `crates/hub-core/src/markdown.rs`
- Modify: `crates/hub-core/src/lib.rs` (добавить `pub mod markdown;`)

**Interfaces:**
- Consumes: `escape::{escape, plain}`, `render::{char_len, split_message}`.
- Produces: `markdown::markdown_to_html_chunks(&str, NonZeroUsize) -> Vec<String>`.

Отличие от Python-версии: pulldown-cmark, в отличие от markdown-it, разбирает `[x](javascript:…)` как ссылку. Небезопасная схема всё равно не становится `<a>` — ссылка выводится текстом `x (javascript:alert(1))`.

- [ ] **Step 1: Написать падающие тесты**

`crates/hub-core/src/markdown.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use rstest::rstest;

    use super::*;
    use crate::escape::plain;
    use crate::render::{TELEGRAM_TEXT_LIMIT, char_len};

    const ALLOWED_TAGS: [&str; 7] = ["b", "i", "s", "code", "pre", "blockquote", "a"];

    fn limit(n: usize) -> NonZeroUsize {
        NonZeroUsize::new(n).unwrap()
    }

    fn render(markdown: &str) -> String {
        let chunks = markdown_to_html_chunks(markdown, TELEGRAM_TEXT_LIMIT);
        assert_eq!(chunks.len(), 1, "{chunks:?}");
        chunks.into_iter().next().unwrap()
    }

    fn tags(html: &str) -> Vec<String> {
        html.split('<')
            .skip(1)
            .filter_map(|part| part.split_once('>').map(|(tag, _)| tag.to_owned()))
            .collect()
    }

    fn tag_name(tag: &str) -> &str {
        tag.trim_start_matches('/').split_whitespace().next().unwrap_or_default()
    }

    #[rstest]
    #[case("**bold** and *italic*", "<b>bold</b> and <i>italic</i>")]
    #[case("~~gone~~", "<s>gone</s>")]
    #[case("run `uv sync`", "run <code>uv sync</code>")]
    #[case("# Title", "<b>Title</b>")]
    #[case("a < b & c > d", "a &lt; b &amp; c &gt; d")]
    #[case("---", "──────────")]
    fn inline_and_simple_blocks(#[case] markdown: &str, #[case] expected: &str) {
        assert_eq!(render(markdown), expected);
    }

    #[test]
    fn raw_html_is_escaped() {
        assert_eq!(render("<script>x</script>"), "&lt;script&gt;x&lt;/script&gt;");
        assert_eq!(render("a <b>x</b>"), "a &lt;b&gt;x&lt;/b&gt;");
    }

    #[test]
    fn fenced_code_keeps_language_and_escapes() {
        assert_eq!(
            render("```python\nif a < b:\n    pass\n```"),
            "<pre><code class=\"language-python\">if a &lt; b:\n    pass</code></pre>"
        );
    }

    #[test]
    fn fence_without_language_is_plain_pre() {
        assert_eq!(render("```\nls\n```"), "<pre>ls</pre>");
    }

    #[test]
    fn lists_get_markers_and_nesting() {
        assert_eq!(
            render("- one\n- two\n  - nested\n\n3. three\n4. four"),
            "• one\n• two\n\u{a0}\u{a0}• nested\n\n3. three\n4. four"
        );
    }

    #[test]
    fn loose_list_renders_like_tight() {
        assert_eq!(render("- one\n\n- two"), "• one\n• two");
    }

    #[test]
    fn links_keep_safe_schemes_only() {
        assert_eq!(
            render("[docs](https://example.com/?a=1&b=2)"),
            "<a href=\"https://example.com/?a=1&amp;b=2\">docs</a>"
        );
        assert_eq!(render("[file](src/app.py)"), "file (src/app.py)");
    }

    #[test]
    fn dangerous_link_is_plain_text() {
        assert_eq!(render("[x](javascript:alert(1))"), "x (javascript:alert(1))");
    }

    #[test]
    fn two_column_table_becomes_key_value_lines() {
        assert_eq!(
            render("| Ключ | Значение |\n|---|---|\n| timeout | **30** с |\n| retries | 3 |"),
            "<b>timeout</b>: <b>30</b> с\n<b>retries</b>: 3"
        );
    }

    #[test]
    fn wide_table_becomes_one_card_per_row() {
        let markdown = "| Репозиторий | Язык | Тесты |\n|---|---|---|\n\
                        | agent-hub | `Python` | 42 |\n| skill-issue | Markdown |  |";
        assert_eq!(
            render(markdown),
            "<b>agent-hub</b>\n\u{a0}\u{a0}Язык: <code>Python</code>\n\u{a0}\u{a0}Тесты: 42\
             \n\n<b>skill-issue</b>\n\u{a0}\u{a0}Язык: Markdown"
        );
    }

    #[test]
    fn single_column_table_becomes_bullets() {
        assert_eq!(render("| Файл |\n|---|\n| a.py |\n| b.py |"), "• a.py\n• b.py");
    }

    #[test]
    fn table_cell_escapes_markup() {
        assert_eq!(render("| k | v |\n|---|---|\n| a<b | x & y |"), "<b>a&lt;b</b>: x &amp; y");
    }

    #[test]
    fn long_table_splits_between_cards() {
        let rows = (0..40)
            .map(|i| format!("| row{i} | {} | {} |", "x".repeat(40), "y".repeat(40)))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = markdown_to_html_chunks(&format!("| n | a | b |\n|---|---|---|\n{rows}"), limit(500));

        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(char_len(chunk) <= 500);
            assert!(chunk.starts_with("<b>row"));
            assert_eq!(chunk.matches("<b>").count(), chunk.matches("</b>").count());
        }
    }

    #[test]
    fn blockquote() {
        assert_eq!(render("> quoted **text**"), "<blockquote>quoted <b>text</b></blockquote>");
    }

    #[test]
    fn blocks_are_separated_by_blank_line() {
        assert_eq!(render("para one\n\npara two"), "para one\n\npara two");
    }

    #[test]
    fn long_text_is_split_into_valid_chunks() {
        let markdown = (0..60)
            .map(|i| format!("**{i}** {}", "word & ".repeat(40)))
            .collect::<Vec<_>>()
            .join("\n\n");
        let chunks = markdown_to_html_chunks(&markdown, limit(500));

        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(char_len(chunk) <= 500);
            assert_eq!(chunk.matches("<b>").count(), chunk.matches("</b>").count());
        }
    }

    #[test]
    fn huge_code_block_is_split_into_several_pre() {
        let code = (0..200).map(|i| format!("line {i} <tag>")).collect::<Vec<_>>().join("\n");
        let chunks = markdown_to_html_chunks(&format!("```\n{code}\n```"), limit(400));

        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(char_len(chunk) <= 400);
            assert!(chunk.starts_with("<pre>"));
            assert!(chunk.ends_with("</pre>"));
        }
        assert_eq!(plain(&chunks.join("\n")), code);
    }

    #[test]
    fn oversized_paragraph_falls_back_to_escaped_text() {
        let chunks = markdown_to_html_chunks(&"&".repeat(300), limit(100));

        assert!(chunks.iter().all(|chunk| char_len(chunk) <= 100));
        assert_eq!(plain(&chunks.concat()).replace('\n', ""), "&".repeat(300));
    }

    #[test]
    fn single_char_over_limit_terminates() {
        let chunks = markdown_to_html_chunks("&&&", limit(2));
        assert_eq!(plain(&chunks.concat()).replace('\n', ""), "&&&");
    }

    #[test]
    fn only_telegram_tags_are_emitted() {
        let markdown = "# H\n\n**b** *i* ~~s~~ `c` [l](https://x.y)\n\n> q\n\n- a\n\n\
                        ```js\nx\n```\n\n|a|b|\n|-|-|\n|1|2|\n\n<div>raw</div>\n\n[j](javascript:x)";
        let html = render(markdown);
        let found = tags(&html);
        assert!(!found.is_empty());
        assert!(found.iter().all(|tag| ALLOWED_TAGS.contains(&tag_name(tag))), "{found:?}");
    }

    #[test]
    fn empty_markdown_has_no_chunks() {
        assert!(markdown_to_html_chunks("   ", TELEGRAM_TEXT_LIMIT).is_empty());
    }
}
```

`crates/hub-core/src/lib.rs` — добавить `pub mod markdown;`.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-core markdown`
Expected: FAIL — `cannot find function markdown_to_html_chunks`.

- [ ] **Step 3: Реализовать разбор в дерево и рендеринг**

Над тестовым модулем `markdown.rs`:

```rust
//! Agent Markdown → the HTML subset Telegram accepts (parse_mode=HTML).
//!
//! Telegram supports only inline tags plus <pre>/<blockquote>, so block structure is expressed
//! with text: headings become bold, list items get bullet markers, tables become key-value
//! lines or per-row cards.

use std::collections::VecDeque;
use std::iter;
use std::num::NonZeroUsize;

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag};

use crate::escape::{escape, plain};
use crate::render::{char_len, split_message};

// Telegram trims plain leading spaces.
const INDENT: &str = "\u{a0}\u{a0}";
const BLOCK_SEPARATOR: &str = "\n\n";
const RULE: &str = "──────────";
// Relative or exotic links make Telegram reject the whole message.
const LINK_SCHEMES: [&str; 4] = ["http://", "https://", "mailto:", "tg://"];

enum Inline {
    Text(String),
    Code(String),
    Break,
    Strong(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strike(Vec<Inline>),
    Link { href: String, children: Vec<Inline> },
    Image { src: String, children: Vec<Inline> },
    Span(Vec<Inline>),
}

enum Block {
    Paragraph(Vec<Inline>),
    Heading(Vec<Inline>),
    Code { info: String, text: String },
    Quote(Vec<Block>),
    List { start: Option<u64>, items: Vec<Vec<Block>> },
    Table { head: Vec<Vec<Inline>>, rows: Vec<Vec<Vec<Inline>>> },
    Rule,
}

struct Table {
    pieces: Vec<String>,
    separator: &'static str,
}

impl Table {
    fn joined(&self) -> String {
        self.pieces.join(self.separator)
    }
}

/// Render `markdown` as Telegram HTML messages, each at most `limit` characters.
#[must_use]
pub fn markdown_to_html_chunks(markdown: &str, limit: NonZeroUsize) -> Vec<String> {
    let mut events = Parser::new_ext(markdown, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH);
    let pieces = blocks(&mut events).iter().flat_map(|block| top_block(block, limit)).collect::<Vec<_>>();
    pack(pieces, limit.get(), BLOCK_SEPARATOR)
}

// --- events → tree --------------------------------------------------------------------------

/// Reads blocks until the enclosing `End` (consumed) or the end of input.
fn blocks(events: &mut Parser<'_>) -> Vec<Block> {
    let mut out = Vec::new();
    // Tight list items carry inline content without a paragraph around it.
    let mut loose = Vec::new();
    while let Some(event) = events.next() {
        let block = match event {
            Event::End(_) => break,
            Event::Start(Tag::Paragraph) => Block::Paragraph(inlines(events)),
            Event::Start(Tag::Heading { .. }) => Block::Heading(inlines(events)),
            Event::Start(Tag::BlockQuote(_)) => Block::Quote(blocks(events)),
            Event::Start(Tag::CodeBlock(kind)) => {
                Block::Code { info: code_info(kind), text: text(events) }
            }
            // Raw HTML is shown as text: Telegram would reject or misread it.
            Event::Start(Tag::HtmlBlock) => {
                Block::Paragraph(vec![Inline::Text(text(events).trim_end_matches('\n').to_owned())])
            }
            Event::Start(Tag::List(start)) => Block::List { start, items: items(events) },
            Event::Start(Tag::Table(_)) => table(events),
            Event::Rule => Block::Rule,
            other => {
                loose.extend(inline(other, events));
                continue;
            }
        };
        flush(&mut loose, &mut out);
        out.push(block);
    }
    flush(&mut loose, &mut out);
    out
}

fn flush(loose: &mut Vec<Inline>, out: &mut Vec<Block>) {
    if !loose.is_empty() {
        out.push(Block::Paragraph(std::mem::take(loose)));
    }
}

fn inlines(events: &mut Parser<'_>) -> Vec<Inline> {
    let mut out = Vec::new();
    while let Some(event) = events.next() {
        if matches!(event, Event::End(_)) {
            break;
        }
        out.extend(inline(event, events));
    }
    out
}

fn inline(event: Event<'_>, events: &mut Parser<'_>) -> Option<Inline> {
    match event {
        Event::Start(tag) => Some(inline_tag(tag, events)),
        Event::Text(text)
        | Event::Html(text)
        | Event::InlineHtml(text)
        | Event::InlineMath(text)
        | Event::DisplayMath(text) => Some(Inline::Text(text.into_string())),
        Event::Code(code) => Some(Inline::Code(code.into_string())),
        Event::SoftBreak | Event::HardBreak => Some(Inline::Break),
        Event::FootnoteReference(name) => Some(Inline::Text(format!("[^{name}]"))),
        Event::TaskListMarker(done) => Some(Inline::Text(if done { "☑ " } else { "☐ " }.to_owned())),
        Event::Rule | Event::End(_) => None,
    }
}

fn inline_tag(tag: Tag<'_>, events: &mut Parser<'_>) -> Inline {
    match tag {
        Tag::Strong => Inline::Strong(inlines(events)),
        Tag::Emphasis => Inline::Emphasis(inlines(events)),
        Tag::Strikethrough => Inline::Strike(inlines(events)),
        Tag::Link { dest_url, .. } => Inline::Link { href: dest_url.into_string(), children: inlines(events) },
        Tag::Image { dest_url, .. } => Inline::Image { src: dest_url.into_string(), children: inlines(events) },
        // Extensions that are not enabled; keep their text.
        _ => Inline::Span(inlines(events)),
    }
}

fn text(events: &mut Parser<'_>) -> String {
    let mut out = String::new();
    for event in events.by_ref() {
        match event {
            Event::End(_) => break,
            Event::Text(text) | Event::Html(text) => out.push_str(&text),
            _ => {}
        }
    }
    out
}

fn code_info(kind: CodeBlockKind<'_>) -> String {
    match kind {
        CodeBlockKind::Fenced(info) => info.into_string(),
        CodeBlockKind::Indented => String::new(),
    }
}

fn items(events: &mut Parser<'_>) -> Vec<Vec<Block>> {
    let mut out = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::Item) => out.push(blocks(events)),
            Event::End(_) => break,
            _ => {}
        }
    }
    out
}

fn table(events: &mut Parser<'_>) -> Block {
    let mut head = Vec::new();
    let mut rows = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::TableHead) => head = cells(events),
            Event::Start(Tag::TableRow) => rows.push(cells(events)),
            Event::End(_) => break,
            _ => {}
        }
    }
    Block::Table { head, rows }
}

fn cells(events: &mut Parser<'_>) -> Vec<Vec<Inline>> {
    let mut out = Vec::new();
    while let Some(event) = events.next() {
        match event {
            Event::Start(Tag::TableCell) => out.push(inlines(events)),
            // Whether the head wraps its cells in a row differs between versions.
            Event::Start(Tag::TableRow) => out.extend(cells(events)),
            Event::End(_) => break,
            _ => {}
        }
    }
    out
}

// --- tree → HTML ----------------------------------------------------------------------------

fn top_block(block: &Block, limit: NonZeroUsize) -> Vec<String> {
    let max = limit.get();
    if let Block::Table { head, rows } = block {
        let table = render_table(head, rows);
        return pack(table.pieces, max, table.separator)
            .into_iter()
            .flat_map(|piece| {
                if char_len(&piece) <= max { vec![piece] } else { split_escaped(&plain(&piece), limit) }
            })
            .collect();
    }
    let rendered = render_block(block, 0);
    if char_len(&rendered) <= max {
        return if rendered.is_empty() { Vec::new() } else { vec![rendered] };
    }
    if let Block::Code { info, text } = block {
        return split_code(text, info, limit);
    }
    split_escaped(&plain_block(block), limit)
}

fn pack(blocks: Vec<String>, limit: usize, separator: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for block in blocks {
        if current.is_empty() {
            current = block;
        } else if char_len(&current) + char_len(separator) + char_len(&block) <= limit {
            current.push_str(separator);
            current.push_str(&block);
        } else {
            chunks.push(std::mem::replace(&mut current, block));
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn join(parts: impl Iterator<Item = String>, separator: &str) -> String {
    parts.collect::<Vec<_>>().join(separator)
}

fn render_block(block: &Block, depth: usize) -> String {
    match block {
        Block::Paragraph(content) => render_inlines(content),
        Block::Heading(content) => format!("<b>{}</b>", render_inlines(content)),
        Block::Code { info, text } => pre(text, info),
        Block::Quote(children) => {
            format!("<blockquote>{}</blockquote>", render_blocks(children, depth, "\n"))
        }
        Block::List { start, items } => render_list(*start, items, depth),
        Block::Table { head, rows } => render_table(head, rows).joined(),
        Block::Rule => RULE.to_owned(),
    }
}

fn render_blocks(blocks: &[Block], depth: usize, separator: &str) -> String {
    join(
        blocks.iter().map(|block| render_block(block, depth)).filter(|rendered| !rendered.is_empty()),
        separator,
    )
}

fn render_list(start: Option<u64>, items: &[Vec<Block>], depth: usize) -> String {
    let indent = INDENT.repeat(depth);
    join(
        items.iter().zip(0_u64..).map(|(item, offset)| {
            let marker = start.map_or_else(|| "•".to_owned(), |first| format!("{}.", first.saturating_add(offset)));
            // Nested lists render their own deeper indentation.
            format!("{indent}{marker} {}", render_blocks(item, depth + 1, "\n"))
        }),
        "\n",
    )
}

fn render_table(head: &[Vec<Inline>], rows: &[Vec<Vec<Inline>>]) -> Table {
    let header: Vec<String> = head.iter().map(|cell| render_inlines(cell)).collect();
    let rows = rows.iter().map(|row| {
        let mut cells: Vec<String> = row.iter().map(|cell| render_inlines(cell).trim().to_owned()).collect();
        cells.resize(header.len(), String::new());
        cells
    });
    match header.as_slice() {
        [] => Table { pieces: Vec::new(), separator: "\n" },
        [_] => Table { pieces: rows.map(|row| format!("• {}", cell(&row, 0))).collect(), separator: "\n" },
        [_, _] => Table {
            pieces: rows.map(|row| format!("<b>{}</b>: {}", cell(&row, 0), cell(&row, 1))).collect(),
            separator: "\n",
        },
        [_, labels @ ..] => Table { pieces: rows.map(|row| card(labels, &row)).collect(), separator: BLOCK_SEPARATOR },
    }
}

fn cell(row: &[String], index: usize) -> &str {
    row.get(index).map_or("", String::as_str)
}

/// Wide rows read as cards on a phone: first cell as title, the rest as labelled fields.
fn card(labels: &[String], row: &[String]) -> String {
    let empty: &[String] = &[];
    let (title, values) = row.split_first().map_or(("", empty), |(title, values)| (title.as_str(), values));
    let title = if title.is_empty() { "—" } else { title };
    let fields = labels.iter().zip(values).filter(|(_, value)| !value.is_empty()).map(|(label, value)| {
        if label.is_empty() { format!("{INDENT}{value}") } else { format!("{INDENT}{label}: {value}") }
    });
    join(iter::once(format!("<b>{title}</b>")).chain(fields), "\n")
}

fn render_inlines(content: &[Inline]) -> String {
    content.iter().map(render_inline).collect()
}

fn render_inline(inline: &Inline) -> String {
    match inline {
        Inline::Text(text) => escape(text),
        Inline::Code(code) => format!("<code>{}</code>", escape(code)),
        Inline::Break => "\n".to_owned(),
        Inline::Strong(children) => format!("<b>{}</b>", render_inlines(children)),
        Inline::Emphasis(children) => format!("<i>{}</i>", render_inlines(children)),
        Inline::Strike(children) => format!("<s>{}</s>", render_inlines(children)),
        Inline::Link { href, children } => link(href, &render_inlines(children)),
        Inline::Image { src, children } => escape(&format!("{} ({src})", plain_inlines(children))),
        Inline::Span(children) => render_inlines(children),
    }
}

fn link(href: &str, label: &str) -> String {
    if LINK_SCHEMES.iter().any(|scheme| href.starts_with(scheme)) {
        format!("<a href=\"{}\">{label}</a>", escape(href))
    } else if href.is_empty() {
        label.to_owned()
    } else {
        format!("{label} ({})", escape(href))
    }
}

fn pre(code: &str, info: &str) -> String {
    let body = escape(code.trim_end_matches('\n'));
    let language = info.split_whitespace().next().unwrap_or_default();
    if is_language(language) {
        format!("<pre><code class=\"language-{language}\">{body}</code></pre>")
    } else {
        format!("<pre>{body}</pre>")
    }
}

fn is_language(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || "_#+.-".contains(c))
}

fn split_code(code: &str, info: &str, limit: NonZeroUsize) -> Vec<String> {
    let budget = limit.get().saturating_sub(char_len(&pre("", info)));
    let mut pieces = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut size = 0;
    for line in code.trim_end_matches('\n').split('\n') {
        let cost = char_len(&escape(line)) + 1;
        if !current.is_empty() && size + cost > budget {
            pieces.push(current.join("\n"));
            current.clear();
            size = 0;
        }
        current.push(line);
        size += cost;
    }
    if !current.is_empty() {
        pieces.push(current.join("\n"));
    }
    pieces
        .into_iter()
        .flat_map(|piece| {
            let rendered = pre(&piece, info);
            if char_len(&rendered) <= limit.get() { vec![rendered] } else { split_escaped(&piece, limit) }
        })
        .collect()
}

/// Split plain text so that each escaped chunk fits; never cuts an entity in half.
fn split_escaped(text: &str, limit: NonZeroUsize) -> Vec<String> {
    let mut result = Vec::new();
    let mut pending: VecDeque<String> = split_message(text, limit).into();
    while let Some(chunk) = pending.pop_front() {
        let escaped = escape(&chunk);
        // A single character cannot be split further, even if its entity exceeds the limit.
        if char_len(&escaped) <= limit.get() || char_len(&chunk) <= 1 {
            result.push(escaped);
            continue;
        }
        let half = NonZeroUsize::new(char_len(&chunk) / 2).unwrap_or(NonZeroUsize::MIN);
        for piece in split_message(&chunk, half).into_iter().rev() {
            pending.push_front(piece);
        }
    }
    result
}

fn plain_block(block: &Block) -> String {
    match block {
        Block::Paragraph(content) | Block::Heading(content) => plain_inlines(content),
        Block::Code { text, .. } => text.clone(),
        Block::Quote(children) => children.iter().map(plain_block).collect(),
        Block::List { items, .. } => items.iter().flatten().map(plain_block).collect(),
        Block::Table { head, rows } => {
            head.iter().chain(rows.iter().flatten()).map(|cell| plain_inlines(cell)).collect()
        }
        Block::Rule => String::new(),
    }
}

fn plain_inlines(content: &[Inline]) -> String {
    content.iter().map(plain_inline).collect()
}

fn plain_inline(inline: &Inline) -> String {
    match inline {
        Inline::Text(text) | Inline::Code(text) => text.clone(),
        Inline::Break => " ".to_owned(),
        Inline::Strong(children)
        | Inline::Emphasis(children)
        | Inline::Strike(children)
        | Inline::Span(children)
        | Inline::Link { children, .. }
        | Inline::Image { children, .. } => plain_inlines(children),
    }
}
```

Если компилятор сообщит, что `pulldown_cmark::Event` помечен `#[non_exhaustive]`, — добавить в `inline` ветку `_ => None` с комментарием `// Foreign enum; new events are ignored.`

- [ ] **Step 4: Прогнать тесты**

Run: `cargo test -p hub-core markdown`
Expected: PASS. При расхождении в `lists_get_markers_and_nesting` или таблицах — вывести события `Parser::new_ext(...)` в тесте через `dbg!` и поправить разбор, а не ожидание.

- [ ] **Step 5: Линтеры**

Run: `cargo clippy --all-targets -- -D warnings && cargo fmt`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-core
git commit -m "Markdown агента в разметку Telegram"
```

---

### Task 4: Подтверждения и вопросы

**Files:**
- Create: `crates/hub-core/src/approvals.rs`, `crates/hub-core/src/questions.rs`
- Modify: `crates/hub-core/src/lib.rs` (`pub mod approvals; pub mod questions;`)

**Interfaces:**
- Consumes: `domain::{Decision, Denied, Question, QuestionOption, Selection, TopicKey}`, `escape::escape`, `render::truncate`, `ids::hex`.
- Produces:
  - `approvals::ApprovalId::from_random([u8; 8])`, `approvals::Verdict::{Allow, Deny}` + `decision()`, `approvals::ApprovalAnswer { id, verdict }`, `approvals::callback_data(&ApprovalId, Verdict) -> String`, `approvals::parse_callback_data(&str) -> Option<ApprovalAnswer>`, `approvals::ApprovalRegistry<R>` с `open(ApprovalId, R)`, `close(&ApprovalId)`, `resolve(&ApprovalAnswer) -> Option<(R, Decision)>`, `approvals::DENIED_BY_USER`.
  - `questions::QuestionId::from_random([u8; 8])`, `questions::QuestionAction::{Pick(usize), Submit, Decline}`, `questions::QuestionPress { id, action }`, `questions::Answer::{Text(String), Declined(Denied)}`, `questions::Button { text, data }`, `questions::Press<R>::{Accepted { responder, answer }, SelectionChanged { question, selected }, NothingSelected, Stale}`, `questions::QuestionRegistry<R>` с `open(QuestionId, TopicKey, Question, R)`, `close(&QuestionId)`, `press(&QuestionPress) -> Press<R>`, `reply(TopicKey, &str) -> Option<(R, Answer)>`, функции `callback_data`, `parse_callback_data`, `keyboard(&QuestionId, &Question, &BTreeSet<usize>) -> Vec<Vec<Button>>`, `question_html(&Question) -> String`, `answer_line(&Answer) -> String`.

- [ ] **Step 1: Написать падающие тесты подтверждений**

`crates/hub-core/src/approvals.rs`:

```rust
#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn id() -> ApprovalId {
        ApprovalId::from_random([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef])
    }

    #[rstest]
    fn callback_data_round_trips(#[values(Verdict::Allow, Verdict::Deny)] verdict: Verdict) {
        let data = callback_data(&id(), verdict);
        assert!(data.len() <= 64, "Telegram callback_data limit");
        assert_eq!(parse_callback_data(&data), Some(ApprovalAnswer { id: id(), verdict }));
    }

    #[rstest]
    #[case("")]
    #[case("ap")]
    #[case("ap:allow:")]
    #[case("ap:maybe:x")]
    #[case("xx:allow:x")]
    #[case("ap:allow:x:y")]
    fn malformed_callback_data_is_rejected(#[case] data: &str) {
        assert_eq!(parse_callback_data(data), None);
    }

    #[test]
    fn resolve_delivers_decision_once() {
        let mut registry = ApprovalRegistry::default();
        registry.open(id(), "responder");

        let answer = ApprovalAnswer { id: id(), verdict: Verdict::Allow };
        assert_eq!(registry.resolve(&answer), Some(("responder", Decision::Allowed)));
        assert_eq!(registry.resolve(&answer), None);
    }

    #[test]
    fn closed_request_cannot_be_resolved() {
        let mut registry = ApprovalRegistry::default();
        registry.open(id(), ());
        registry.close(&id());

        assert_eq!(registry.resolve(&ApprovalAnswer { id: id(), verdict: Verdict::Allow }), None);
    }

    #[test]
    fn deny_carries_reason() {
        assert_eq!(Verdict::Deny.decision(), Decision::Denied(Denied::new(DENIED_BY_USER)));
    }
}
```

- [ ] **Step 2: Написать падающие тесты вопросов**

`crates/hub-core/src/questions.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use rstest::rstest;

    use super::*;
    use crate::domain::{ChatId, QuestionOption, Selection, ThreadId};

    const KEY: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
    const OTHER_KEY: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(8) };

    fn options() -> Vec<QuestionOption> {
        vec![
            QuestionOption { label: "Кратко".to_owned(), description: "Только суть".to_owned() },
            QuestionOption { label: "Подробно".to_owned(), description: String::new() },
        ]
    }

    fn single() -> Question {
        Question::new("Какой формат?".to_owned(), "Формат".to_owned(), options(), Selection::Single).unwrap()
    }

    fn multi() -> Question {
        Question::new("Какие разделы?".to_owned(), String::new(), options(), Selection::Multiple).unwrap()
    }

    fn id() -> QuestionId {
        QuestionId::from_random([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef])
    }

    fn press(action: QuestionAction) -> QuestionPress {
        QuestionPress { id: id(), action }
    }

    fn selected(indices: &[usize]) -> BTreeSet<usize> {
        indices.iter().copied().collect()
    }

    #[rstest]
    #[case(QuestionAction::Pick(0))]
    #[case(QuestionAction::Pick(11))]
    #[case(QuestionAction::Submit)]
    #[case(QuestionAction::Decline)]
    fn callback_data_round_trips(#[case] action: QuestionAction) {
        let data = callback_data(&id(), action);
        assert!(data.len() <= 64, "Telegram callback_data limit");
        assert_eq!(parse_callback_data(&data), Some(press(action)));
    }

    #[rstest]
    #[case("")]
    #[case("qa")]
    #[case("qa:x:")]
    #[case("qa:x:maybe")]
    #[case("qa:x:-1")]
    #[case("ap:x:0")]
    #[case("qa:x:0:1")]
    #[case("qa::0")]
    #[case("qa:x:²")]
    fn malformed_callback_data_is_rejected(#[case] data: &str) {
        assert_eq!(parse_callback_data(data), None);
    }

    #[test]
    fn single_choice_pick_answers_with_label() {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, single(), "r");

        assert_eq!(
            registry.press(&press(QuestionAction::Pick(1))),
            Press::Accepted { responder: "r", answer: Answer::Text("Подробно".to_owned()) }
        );
        assert_eq!(registry.press(&press(QuestionAction::Pick(0))), Press::Stale);
    }

    #[test]
    fn multi_choice_toggles_then_submits_in_option_order() {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, multi(), "r");

        assert_eq!(
            registry.press(&press(QuestionAction::Pick(1))),
            Press::SelectionChanged { question: multi(), selected: selected(&[1]) }
        );
        assert_eq!(
            registry.press(&press(QuestionAction::Pick(0))),
            Press::SelectionChanged { question: multi(), selected: selected(&[0, 1]) }
        );
        assert_eq!(
            registry.press(&press(QuestionAction::Pick(1))),
            Press::SelectionChanged { question: multi(), selected: selected(&[0]) }
        );
        registry.press(&press(QuestionAction::Pick(1)));

        assert_eq!(
            registry.press(&press(QuestionAction::Submit)),
            Press::Accepted { responder: "r", answer: Answer::Text("Кратко, Подробно".to_owned()) }
        );
    }

    #[test]
    fn multi_choice_submit_requires_selection() {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, multi(), ());
        assert_eq!(registry.press(&press(QuestionAction::Submit)), Press::NothingSelected);
    }

    #[rstest]
    #[case(QuestionAction::Pick(2))]
    #[case(QuestionAction::Submit)]
    fn invalid_press_for_single_choice_is_stale(#[case] action: QuestionAction) {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, single(), ());
        assert_eq!(registry.press(&press(action)), Press::Stale);
        assert!(registry.reply(KEY, "still open").is_some());
    }

    #[test]
    fn decline_denies() {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, single(), ());
        assert_eq!(
            registry.press(&press(QuestionAction::Decline)),
            Press::Accepted { responder: (), answer: Answer::Declined(Denied::new(DECLINED_BY_USER)) }
        );
    }

    #[test]
    fn text_reply_answers_pending_question_of_its_topic_only() {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, single(), "r");

        assert_eq!(registry.reply(OTHER_KEY, "чужой"), None);
        assert_eq!(registry.reply(KEY, "свой вариант"), Some(("r", Answer::Text("свой вариант".to_owned()))));
        assert_eq!(registry.reply(KEY, "ещё"), None);
    }

    #[test]
    fn reply_answers_the_oldest_question_first() {
        let mut registry = QuestionRegistry::default();
        registry.open(QuestionId::from_random([1; 8]), KEY, single(), "first");
        registry.open(QuestionId::from_random([2; 8]), KEY, single(), "second");

        assert_eq!(registry.reply(KEY, "a").map(|(responder, _)| responder), Some("first"));
    }

    #[test]
    fn closed_question_is_stale() {
        let mut registry = QuestionRegistry::default();
        registry.open(id(), KEY, single(), ());
        registry.close(&id());

        assert_eq!(registry.press(&press(QuestionAction::Pick(0))), Press::Stale);
        assert_eq!(registry.reply(KEY, "x"), None);
    }

    fn button(text: &str, action: QuestionAction) -> Button {
        Button { text: text.to_owned(), data: callback_data(&id(), action) }
    }

    #[test]
    fn single_choice_keyboard_has_options_and_decline() {
        assert_eq!(
            keyboard(&id(), &single(), &BTreeSet::new()),
            vec![
                vec![button("Кратко", QuestionAction::Pick(0))],
                vec![button("Подробно", QuestionAction::Pick(1))],
                vec![button("❌ Не отвечать", QuestionAction::Decline)],
            ]
        );
    }

    #[test]
    fn multi_choice_keyboard_marks_selection_and_submits() {
        assert_eq!(
            keyboard(&id(), &multi(), &selected(&[1])),
            vec![
                vec![button("☐ Кратко", QuestionAction::Pick(0))],
                vec![button("☑ Подробно", QuestionAction::Pick(1))],
                vec![
                    button("✅ Готово", QuestionAction::Submit),
                    button("❌ Не отвечать", QuestionAction::Decline),
                ],
            ]
        );
    }

    #[test]
    fn question_html_escapes_and_lists_descriptions() {
        let question = Question::new("a < b?".to_owned(), "H&M".to_owned(), options(), Selection::Single).unwrap();
        assert_eq!(
            question_html(&question),
            "❓ <b>H&amp;M</b>\na &lt; b?\n\n• <b>Кратко</b> — Только суть\n• <b>Подробно</b>\n\n\
             <i>Выберите вариант или напишите свой ответ сообщением.</i>"
        );
    }

    #[test]
    fn question_html_without_header() {
        assert!(question_html(&multi()).starts_with("❓ Какие разделы?\n\n"));
        assert!(question_html(&multi()).ends_with("<i>Отметьте варианты и нажмите «Готово» или напишите свой ответ сообщением.</i>"));
    }

    #[test]
    fn answer_line_escapes_and_truncates() {
        assert_eq!(answer_line(&Answer::Text("a<b".to_owned())), "💬 a&lt;b");
        assert_eq!(answer_line(&Answer::Declined(Denied::new("x"))), "❌ Без ответа");
        assert_eq!(answer_line(&Answer::Text("я".repeat(600))).chars().count(), "💬 ".chars().count() + 500);
    }
}
```

`crates/hub-core/src/lib.rs`:

```rust
pub mod approvals;
pub mod domain;
pub mod escape;
mod ids;
pub mod markdown;
pub mod questions;
pub mod render;
```

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p hub-core approvals questions`
Expected: FAIL — типы не определены.

- [ ] **Step 4: Реализовать `approvals.rs`**

Над тестовым модулем:

```rust
//! Pending human decisions on tool calls, keyed by an opaque id carried in button data.

use std::collections::HashMap;

use crate::domain::{Decision, Denied};
use crate::ids::hex;

pub const CALLBACK_PREFIX: &str = "ap";
pub const DENIED_BY_USER: &str = "Пользователь запретил этот вызов";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApprovalId(String);

impl ApprovalId {
    #[must_use]
    pub fn from_random(bytes: [u8; 8]) -> Self {
        Self(hex(&bytes))
    }

    fn parse(raw: &str) -> Option<Self> {
        (!raw.is_empty()).then(|| Self(raw.to_owned()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
}

impl Verdict {
    const fn code(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    fn parse(code: &str) -> Option<Self> {
        [Self::Allow, Self::Deny].into_iter().find(|verdict| verdict.code() == code)
    }

    #[must_use]
    pub fn decision(self) -> Decision {
        match self {
            Self::Allow => Decision::Allowed,
            Self::Deny => Decision::Denied(Denied::new(DENIED_BY_USER)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalAnswer {
    pub id: ApprovalId,
    pub verdict: Verdict,
}

#[must_use]
pub fn callback_data(id: &ApprovalId, verdict: Verdict) -> String {
    format!("{CALLBACK_PREFIX}:{}:{}", verdict.code(), id.0)
}

#[must_use]
pub fn parse_callback_data(data: &str) -> Option<ApprovalAnswer> {
    match data.split(':').collect::<Vec<_>>().as_slice() {
        [prefix, verdict, id] if *prefix == CALLBACK_PREFIX => {
            Some(ApprovalAnswer { verdict: Verdict::parse(verdict)?, id: ApprovalId::parse(id)? })
        }
        _ => None,
    }
}

/// Open approval requests; `R` is whatever delivers the decision back to the asker.
#[derive(Debug)]
pub struct ApprovalRegistry<R> {
    pending: HashMap<ApprovalId, R>,
}

impl<R> Default for ApprovalRegistry<R> {
    fn default() -> Self {
        Self { pending: HashMap::new() }
    }
}

impl<R> ApprovalRegistry<R> {
    pub fn open(&mut self, id: ApprovalId, responder: R) {
        self.pending.insert(id, responder);
    }

    pub fn close(&mut self, id: &ApprovalId) {
        self.pending.remove(id);
    }

    /// The responder and decision; `None` when the request is gone (answered, timed out, stopped).
    pub fn resolve(&mut self, answer: &ApprovalAnswer) -> Option<(R, Decision)> {
        self.pending.remove(&answer.id).map(|responder| (responder, answer.verdict.decision()))
    }
}
```

- [ ] **Step 5: Реализовать `questions.rs`**

Над тестовым модулем:

```rust
//! Pending agent questions answered with buttons or a free-text message in the same topic.

use std::collections::BTreeSet;

use crate::domain::{Denied, Question, Selection, TopicKey};
use crate::escape::escape;
use crate::ids::hex;
use crate::render::truncate;

pub const CALLBACK_PREFIX: &str = "qa";
const SUBMIT: &str = "done";
const DECLINE: &str = "no";
pub const DECLINED_BY_USER: &str = "Пользователь отказался отвечать на вопрос";
pub const BUTTON_TEXT_LIMIT: usize = 60;
// Keeps the whole question inside one Telegram message.
pub const QUESTION_TEXT_LIMIT: usize = 2000;
pub const DESCRIPTION_TEXT_LIMIT: usize = 300;
pub const ANSWER_TEXT_LIMIT: usize = 500;
// Multi-select answers use the separator the agent CLI expects.
pub const ANSWER_SEPARATOR: &str = ", ";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QuestionId(String);

impl QuestionId {
    #[must_use]
    pub fn from_random(bytes: [u8; 8]) -> Self {
        Self(hex(&bytes))
    }

    fn parse(raw: &str) -> Option<Self> {
        (!raw.is_empty()).then(|| Self(raw.to_owned()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionAction {
    Pick(usize),
    Submit,
    Decline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionPress {
    pub id: QuestionId,
    pub action: QuestionAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Text(String),
    Declined(Denied),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub text: String,
    pub data: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Press<R> {
    Accepted { responder: R, answer: Answer },
    SelectionChanged { question: Question, selected: BTreeSet<usize> },
    NothingSelected,
    /// The question is gone (answered, timed out, stopped) or the button does not fit it.
    Stale,
}

struct Pending<R> {
    id: QuestionId,
    key: TopicKey,
    question: Question,
    selected: BTreeSet<usize>,
    responder: R,
}

enum Step {
    Resolve(Answer),
    Changed,
    NothingSelected,
    Stale,
}

/// Open questions in the order they were asked; `R` delivers the answer back to the asker.
pub struct QuestionRegistry<R> {
    pending: Vec<Pending<R>>,
}

impl<R> Default for QuestionRegistry<R> {
    fn default() -> Self {
        Self { pending: Vec::new() }
    }
}

impl<R> QuestionRegistry<R> {
    pub fn open(&mut self, id: QuestionId, key: TopicKey, question: Question, responder: R) {
        self.pending.push(Pending { id, key, question, selected: BTreeSet::new(), responder });
    }

    pub fn close(&mut self, id: &QuestionId) {
        self.pending.retain(|pending| pending.id != *id);
    }

    pub fn press(&mut self, press: &QuestionPress) -> Press<R> {
        let Some(position) = self.pending.iter().position(|pending| pending.id == press.id) else {
            return Press::Stale;
        };
        let Some(pending) = self.pending.get_mut(position) else {
            return Press::Stale;
        };
        match step(pending, press.action) {
            Step::Resolve(answer) => {
                let pending = self.pending.remove(position);
                Press::Accepted { responder: pending.responder, answer }
            }
            Step::Changed => Press::SelectionChanged {
                question: pending.question.clone(),
                selected: pending.selected.clone(),
            },
            Step::NothingSelected => Press::NothingSelected,
            Step::Stale => Press::Stale,
        }
    }

    /// Answer the topic's oldest open question with free text.
    pub fn reply(&mut self, key: TopicKey, text: &str) -> Option<(R, Answer)> {
        let position = self.pending.iter().position(|pending| pending.key == key)?;
        let pending = self.pending.remove(position);
        Some((pending.responder, Answer::Text(text.to_owned())))
    }
}

fn step<R>(pending: &mut Pending<R>, action: QuestionAction) -> Step {
    let options = pending.question.options();
    match (action, pending.question.selection()) {
        (QuestionAction::Pick(index), _) if index >= options.len() => Step::Stale,
        (QuestionAction::Pick(index), Selection::Multiple) => {
            if !pending.selected.remove(&index) {
                pending.selected.insert(index);
            }
            Step::Changed
        }
        (QuestionAction::Pick(index), Selection::Single) => options
            .get(index)
            .map_or(Step::Stale, |option| Step::Resolve(Answer::Text(option.label.clone()))),
        (QuestionAction::Submit, Selection::Single) => Step::Stale,
        (QuestionAction::Submit, Selection::Multiple) if pending.selected.is_empty() => Step::NothingSelected,
        (QuestionAction::Submit, Selection::Multiple) => {
            let labels = pending
                .selected
                .iter()
                .filter_map(|index| options.get(*index))
                .map(|option| option.label.as_str())
                .collect::<Vec<_>>();
            Step::Resolve(Answer::Text(labels.join(ANSWER_SEPARATOR)))
        }
        (QuestionAction::Decline, _) => Step::Resolve(Answer::Declined(Denied::new(DECLINED_BY_USER))),
    }
}

#[must_use]
pub fn callback_data(id: &QuestionId, action: QuestionAction) -> String {
    let code = match action {
        QuestionAction::Pick(index) => index.to_string(),
        QuestionAction::Submit => SUBMIT.to_owned(),
        QuestionAction::Decline => DECLINE.to_owned(),
    };
    format!("{CALLBACK_PREFIX}:{}:{code}", id.0)
}

#[must_use]
pub fn parse_callback_data(data: &str) -> Option<QuestionPress> {
    match data.split(':').collect::<Vec<_>>().as_slice() {
        [prefix, id, code] if *prefix == CALLBACK_PREFIX => {
            Some(QuestionPress { id: QuestionId::parse(id)?, action: parse_action(code)? })
        }
        _ => None,
    }
}

fn parse_action(code: &str) -> Option<QuestionAction> {
    match code {
        SUBMIT => Some(QuestionAction::Submit),
        DECLINE => Some(QuestionAction::Decline),
        _ if !code.is_empty() && code.bytes().all(|byte| byte.is_ascii_digit()) => {
            code.parse().ok().map(QuestionAction::Pick)
        }
        _ => None,
    }
}

#[must_use]
pub fn keyboard(id: &QuestionId, question: &Question, selected: &BTreeSet<usize>) -> Vec<Vec<Button>> {
    let button = |text: String, action| Button { text, data: callback_data(id, action) };
    let mut rows: Vec<Vec<Button>> = question
        .options()
        .iter()
        .enumerate()
        .map(|(index, option)| {
            let label = truncate(&option.label, BUTTON_TEXT_LIMIT);
            let text = match question.selection() {
                Selection::Single => label,
                Selection::Multiple => {
                    format!("{} {label}", if selected.contains(&index) { "☑" } else { "☐" })
                }
            };
            vec![button(text, QuestionAction::Pick(index))]
        })
        .collect();
    let decline = button("❌ Не отвечать".to_owned(), QuestionAction::Decline);
    rows.push(match question.selection() {
        Selection::Single => vec![decline],
        Selection::Multiple => vec![button("✅ Готово".to_owned(), QuestionAction::Submit), decline],
    });
    rows
}

#[must_use]
pub fn question_html(question: &Question) -> String {
    let title = if question.header().trim().is_empty() {
        "❓ ".to_owned()
    } else {
        format!("❓ <b>{}</b>\n", escape(question.header()))
    };
    let options = question
        .options()
        .iter()
        .map(|option| {
            let description = if option.description.trim().is_empty() {
                String::new()
            } else {
                format!(" — {}", escape(&truncate(&option.description, DESCRIPTION_TEXT_LIMIT)))
            };
            format!("• <b>{}</b>{description}", escape(&option.label))
        })
        .collect::<Vec<_>>()
        .join("\n");
    let hint = match question.selection() {
        Selection::Single => "Выберите вариант или напишите свой ответ сообщением.",
        Selection::Multiple => "Отметьте варианты и нажмите «Готово» или напишите свой ответ сообщением.",
    };
    let text = escape(&truncate(question.text(), QUESTION_TEXT_LIMIT));
    [format!("{title}{text}"), options, format!("<i>{hint}</i>")]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[must_use]
pub fn answer_line(answer: &Answer) -> String {
    match answer {
        Answer::Text(text) => format!("💬 {}", escape(&truncate(text, ANSWER_TEXT_LIMIT))),
        Answer::Declined(_) => "❌ Без ответа".to_owned(),
    }
}
```

- [ ] **Step 6: Прогнать тесты и линтеры**

Run: `cargo test -p hub-core && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/hub-core
git commit -m "Кнопки подтверждений и вопросов агента"
```

---

### Task 5: Вложения, команды, пути workspace

**Files:**
- Create: `crates/hub-core/src/attachments.rs`, `crates/hub-core/src/commands.rs`, `crates/hub-core/src/workspace.rs`
- Modify: `crates/hub-core/src/lib.rs` (`pub mod attachments; pub mod commands; pub mod workspace;`)

**Interfaces:**
- Consumes: `domain::{BackendKind, MessageId}`.
- Produces:
  - `attachments::{UPLOADS_DIR: [&str; 2], GITIGNORE: &str, MAX_DOWNLOAD_BYTES: u64, MAX_SEND_BYTES: u64, MAX_FILENAME_LENGTH: usize}`, `attachments::uploads_dir(&Path) -> PathBuf`, `attachments::safe_filename(Option<&str>, &str) -> String`, `attachments::upload_path(&Path, MessageId, Option<&str>) -> PathBuf`, `attachments::prompt_text(&str, &[PathBuf]) -> String`.
  - `commands::Command::{Help, New, Cwd, Reset, Stop, Status}`, `commands::Input<'a>::{Command { command, args }, Unknown, Text}`, `commands::parse_input(&str, &str) -> Input`, `commands::NewSessionArgs { backend, cwd }`, `commands::parse_new_args(&[&str]) -> NewSessionArgs`, `commands::join_path_args(&[&str]) -> Option<String>`.
  - `workspace::expand_home(&str, &Path) -> PathBuf`, `workspace::candidate(&Path, &Path, Option<&str>) -> PathBuf`.

- [ ] **Step 1: Написать падающие тесты**

`crates/hub-core/src/attachments.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(Some("report.pdf"), "report.pdf")]
    #[case(Some("Отчёт за май.xlsx"), "Отчёт_за_май.xlsx")]
    #[case(Some("../../etc/passwd"), "passwd")]
    #[case(Some("..\\..\\win.ini"), "win.ini")]
    #[case(Some(".env"), "env")]
    #[case(Some("..."), "file")]
    #[case(Some(""), "file")]
    #[case(None, "file")]
    #[case(Some("a:b*c?.txt"), "a_b_c_.txt")]
    fn safe_filenames(#[case] raw: Option<&str>, #[case] expected: &str) {
        assert_eq!(safe_filename(raw, "file"), expected);
    }

    #[test]
    fn safe_filename_keeps_extension_when_truncating() {
        let name = safe_filename(Some(&format!("{}.tar.gz", "x".repeat(300))), "file");
        assert!(name.chars().count() <= MAX_FILENAME_LENGTH);
        assert!(name.ends_with(".gz"));
    }

    #[test]
    fn safe_filename_truncates_long_suffix() {
        let name = safe_filename(Some(&format!("a.{}", "я".repeat(300))), "file");
        assert_eq!(name.chars().count(), MAX_FILENAME_LENGTH);
    }

    #[test]
    fn upload_path_stays_in_uploads_dir() {
        let cwd = PathBuf::from("project");
        assert_eq!(
            upload_path(&cwd, MessageId(42), Some("../../../x.txt")),
            cwd.join(".agent-hub").join("uploads").join("42-x.txt")
        );
    }

    #[test]
    fn prompt_text_lists_files_after_text() {
        let files = [PathBuf::from("u/1-a.pdf"), PathBuf::from("u/2-b.csv")];
        assert_eq!(
            prompt_text("сравни", &files),
            format!(
                "сравни\n\nПриложенные файлы:\n- {}\n- {}",
                files[0].display(),
                files[1].display()
            )
        );
    }

    #[test]
    fn prompt_text_without_files_is_unchanged() {
        assert_eq!(prompt_text("привет", &[]), "привет");
    }

    #[test]
    fn prompt_text_with_only_files() {
        let file = PathBuf::from("a");
        assert_eq!(prompt_text("  ", &[file]), "Приложенные файлы:\n- a");
    }
}
```

`crates/hub-core/src/commands.rs`:

```rust
#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case(&[], None)]
    #[case(&["claude"], None)]
    #[case(&["Claude", "shop/backend"], Some("shop/backend"))]
    #[case(&["shop/backend"], Some("shop/backend"))]
    #[case(&["my", "dir"], Some("my dir"))]
    fn new_args(#[case] args: &[&str], #[case] cwd: Option<&str>) {
        assert_eq!(
            parse_new_args(args),
            NewSessionArgs { backend: BackendKind::Claude, cwd: cwd.map(str::to_owned) }
        );
    }

    #[test]
    fn path_args_are_joined() {
        assert_eq!(join_path_args(&[]), None);
        assert_eq!(join_path_args(&["a", "b"]), Some("a b".to_owned()));
    }

    #[rstest]
    #[case("/new shop backend", Input::Command { command: Command::New, args: vec!["shop", "backend"] })]
    #[case("/start", Input::Command { command: Command::Help, args: vec![] })]
    #[case("/STOP", Input::Command { command: Command::Stop, args: vec![] })]
    #[case("/cwd@agent_hub_bot x", Input::Command { command: Command::Cwd, args: vec!["x"] })]
    #[case("/cwd@Agent_Hub_Bot x", Input::Command { command: Command::Cwd, args: vec!["x"] })]
    #[case("/cwd@other_bot x", Input::Unknown)]
    #[case("/unknown", Input::Unknown)]
    #[case("hello /new", Input::Text)]
    #[case("/", Input::Text)]
    fn input_is_classified(#[case] text: &str, #[case] expected: Input<'_>) {
        assert_eq!(parse_input(text, "agent_hub_bot"), expected);
    }
}
```

`crates/hub-core/src/workspace.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" })
    }

    fn home() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\Users\me" } else { "/home/me" })
    }

    #[test]
    fn missing_path_means_root() {
        assert_eq!(candidate(&root(), &home(), None), root());
        assert_eq!(candidate(&root(), &home(), Some("  ")), root());
    }

    #[test]
    fn relative_path_is_under_root() {
        assert_eq!(candidate(&root(), &home(), Some("project/sub")), root().join("project/sub"));
    }

    #[test]
    fn absolute_path_is_kept() {
        let inside = root().join("project");
        assert_eq!(candidate(&root(), &home(), inside.to_str()), inside);
    }

    #[test]
    fn tilde_expands_to_home() {
        assert_eq!(expand_home("~", &home()), home());
        assert_eq!(expand_home("~/p", &home()), home().join("p"));
        assert_eq!(expand_home("~other/p", &home()), PathBuf::from("~other/p"));
    }
}
```

`crates/hub-core/src/lib.rs`:

```rust
pub mod approvals;
pub mod attachments;
pub mod commands;
pub mod domain;
pub mod escape;
mod ids;
pub mod markdown;
pub mod questions;
pub mod render;
pub mod workspace;
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-core attachments commands workspace`
Expected: FAIL — функции не определены.

- [ ] **Step 3: Реализовать `attachments.rs`**

Над тестовым модулем:

```rust
//! Where user uploads land in the session directory and how the agent is told about them.

use std::path::{Path, PathBuf};

use crate::domain::MessageId;

// Relative to the session cwd, so the agent reads uploads without leaving its project.
pub const UPLOADS_DIR: [&str; 2] = [".agent-hub", "uploads"];
// Keeps uploads out of the project's git status without touching its own .gitignore.
pub const GITIGNORE: &str = "*\n";
// Bot API getFile refuses larger files.
pub const MAX_DOWNLOAD_BYTES: u64 = 20 * 1024 * 1024;
// Bot API sendDocument refuses larger files.
pub const MAX_SEND_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_FILENAME_LENGTH: usize = 100;

#[must_use]
pub fn uploads_dir(cwd: &Path) -> PathBuf {
    UPLOADS_DIR.iter().fold(cwd.to_path_buf(), |path, part| path.join(part))
}

/// A single path component that cannot traverse, hide, or overflow.
#[must_use]
pub fn safe_filename(raw: Option<&str>, fallback: &str) -> String {
    let base = raw.unwrap_or_default().rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '_' })
        .collect();
    let name = cleaned.trim_start_matches('.');
    if name.trim_matches(['.', '_']).is_empty() {
        return fallback.to_owned();
    }
    if name.chars().count() <= MAX_FILENAME_LENGTH {
        return name.to_owned();
    }
    match name.rsplit_once('.') {
        Some((stem, suffix)) if suffix.chars().count() < MAX_FILENAME_LENGTH / 2 => {
            let keep = MAX_FILENAME_LENGTH - suffix.chars().count() - 1;
            format!("{}.{suffix}", stem.chars().take(keep).collect::<String>())
        }
        Some(_) | None => name.chars().take(MAX_FILENAME_LENGTH).collect(),
    }
}

/// Message ids are unique per chat, so uploads from different messages never collide.
#[must_use]
pub fn upload_path(cwd: &Path, message: MessageId, filename: Option<&str>) -> PathBuf {
    uploads_dir(cwd).join(format!("{}-{}", message.0, safe_filename(filename, "file")))
}

#[must_use]
pub fn prompt_text(text: &str, files: &[PathBuf]) -> String {
    if files.is_empty() {
        return text.to_owned();
    }
    let listing = std::iter::once("Приложенные файлы:".to_owned())
        .chain(files.iter().map(|path| format!("- {}", path.display())))
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() { listing } else { format!("{}\n\n{listing}", text.trim()) }
}
```

- [ ] **Step 4: Реализовать `commands.rs`**

Над тестовым модулем:

```rust
//! Pure parsing of bot commands and their arguments.

use crate::domain::BackendKind;

pub const DEFAULT_BACKEND: BackendKind = BackendKind::Claude;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Help,
    New,
    Cwd,
    Reset,
    Stop,
    Status,
}

impl Command {
    fn parse(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "start" | "help" => Some(Self::Help),
            "new" => Some(Self::New),
            "cwd" => Some(Self::Cwd),
            "reset" => Some(Self::Reset),
            "stop" => Some(Self::Stop),
            "status" => Some(Self::Status),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input<'a> {
    Command { command: Command, args: Vec<&'a str> },
    /// A command this bot does not know or one addressed to another bot; ignored.
    Unknown,
    Text,
}

#[must_use]
pub fn parse_input<'a>(text: &'a str, bot: &str) -> Input<'a> {
    let Some(body) = text.strip_prefix('/') else {
        return Input::Text;
    };
    let mut words = body.split_whitespace();
    let Some(head) = words.next() else {
        return Input::Text;
    };
    let (name, mention) = head.split_once('@').map_or((head, None), |(name, bot)| (name, Some(bot)));
    if mention.is_some_and(|mention| !mention.eq_ignore_ascii_case(bot)) {
        return Input::Unknown;
    }
    Command::parse(name).map_or(Input::Unknown, |command| Input::Command { command, args: words.collect() })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSessionArgs {
    pub backend: BackendKind,
    pub cwd: Option<String>,
}

/// `/new [backend] [path]`; a first word that is not a backend name starts the path.
#[must_use]
pub fn parse_new_args(args: &[&str]) -> NewSessionArgs {
    match args.split_first() {
        None => NewSessionArgs { backend: DEFAULT_BACKEND, cwd: None },
        Some((first, rest)) => match BackendKind::parse(first) {
            Some(backend) => NewSessionArgs { backend, cwd: join_path_args(rest) },
            None => NewSessionArgs { backend: DEFAULT_BACKEND, cwd: join_path_args(args) },
        },
    }
}

#[must_use]
pub fn join_path_args(args: &[&str]) -> Option<String> {
    let joined = args.join(" ");
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}
```

- [ ] **Step 5: Реализовать `workspace.rs`**

Над тестовым модулем:

```rust
//! Working-directory candidates; containment and existence are checked by the shell.

use std::path::{Path, PathBuf};

#[must_use]
pub fn expand_home(raw: &str, home: &Path) -> PathBuf {
    match raw.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with(['/', '\\']) => home.join(rest.trim_start_matches(['/', '\\'])),
        Some(_) | None => PathBuf::from(raw),
    }
}

/// `raw` absolute, `~`-prefixed or relative to `root`; blank means `root`.
#[must_use]
pub fn candidate(root: &Path, home: &Path, raw: Option<&str>) -> PathBuf {
    match raw.map(str::trim).filter(|raw| !raw.is_empty()) {
        None => root.to_path_buf(),
        Some(raw) => {
            let path = expand_home(raw, home);
            if path.is_absolute() { path } else { root.join(path) }
        }
    }
}
```

- [ ] **Step 6: Прогнать тесты и линтеры**

Run: `cargo test -p hub-core && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/hub-core
git commit -m "Вложения, команды бота и пути workspace"
```

---

### Task 6: Настройки и черновик формы

**Files:**
- Create: `crates/hub-core/src/settings.rs`
- Modify: `crates/hub-core/src/lib.rs` (`pub mod settings;`)

**Interfaces:**
- Consumes: `domain::{ChatId, UserId}`, `workspace::expand_home`.
- Produces: `settings::Token` (`expose()`, `Debug` скрывает значение), `settings::Users` (`contains(UserId)`, `iter()`), `settings::PermissionMode::{Default, AcceptEdits, Plan, BypassPermissions}` + `ALL`, `wire()`, `parse()`, `settings::Budget` (`amount()`), `settings::UpdateCheck::{Enabled, Disabled}`, `settings::{TelegramSettings { token, chat, users }, ClaudeSettings { cli, model, permission_mode, budget }, Timeouts { approval, background }, Settings { telegram, workspace_root, claude, timeouts, updates }}`, `settings::Field` (10 вариантов), `settings::FieldError { field, message }`, `settings::Draft` (+ `Draft::parse(&self, &Path) -> Result<Settings, Vec<FieldError>>`, `Draft::from_settings(&Settings)`, `Default`), `settings::SettingsFile` (serde) + `SettingsFile::from_settings(&Settings)`, `SettingsFile::to_draft(&self, String) -> Result<Draft, FieldError>`, `settings::DEFAULT_APPROVAL_TIMEOUT`, `settings::DEFAULT_BACKGROUND_TIMEOUT`.

- [ ] **Step 1: Написать падающие тесты**

`crates/hub-core/src/settings.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rstest::rstest;

    use super::*;

    fn home() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\Users\me" } else { "/home/me" })
    }

    fn draft() -> Draft {
        Draft {
            token: "123:abc".to_owned(),
            chat: "-1001234567890".to_owned(),
            users: "111, 222".to_owned(),
            workspace_root: "~/Projects".to_owned(),
            ..Draft::default()
        }
    }

    #[test]
    fn minimal_draft_uses_defaults() {
        let settings = draft().parse(&home()).unwrap();

        assert_eq!(settings.telegram.token.expose(), "123:abc");
        assert_eq!(settings.telegram.chat, ChatId(-1_001_234_567_890));
        assert_eq!(settings.telegram.users.iter().collect::<Vec<_>>(), [UserId(111), UserId(222)]);
        assert_eq!(settings.workspace_root, home().join("Projects"));
        assert_eq!(settings.timeouts.approval, DEFAULT_APPROVAL_TIMEOUT);
        assert_eq!(settings.timeouts.background, DEFAULT_BACKGROUND_TIMEOUT);
        assert_eq!(settings.claude.permission_mode, PermissionMode::Default);
        assert_eq!(settings.claude.model, None);
        assert_eq!(settings.claude.budget, None);
        assert_eq!(settings.claude.cli, None);
        assert_eq!(settings.updates, UpdateCheck::Enabled);
    }

    #[test]
    fn optional_values_are_parsed() {
        let settings = Draft {
            permission_mode: PermissionMode::AcceptEdits,
            model: " claude-opus-5-5 ".to_owned(),
            budget: "2.50".to_owned(),
            approval_timeout: "30".to_owned(),
            background_timeout: "7200".to_owned(),
            cli: "~/bin/claude".to_owned(),
            updates: UpdateCheck::Disabled,
            ..draft()
        }
        .parse(&home())
        .unwrap();

        assert_eq!(settings.claude.permission_mode, PermissionMode::AcceptEdits);
        assert_eq!(settings.claude.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(settings.claude.budget.map(Budget::amount), Some("2.50".parse().unwrap()));
        assert_eq!(settings.timeouts.approval, Duration::from_secs(30));
        assert_eq!(settings.timeouts.background, Duration::from_secs(7200));
        assert_eq!(settings.claude.cli, Some(home().join("bin/claude")));
        assert_eq!(settings.updates, UpdateCheck::Disabled);
    }

    #[rstest]
    #[case(Draft { token: String::new(), ..draft() }, Field::Token)]
    #[case(Draft { token: "12 3".to_owned(), ..draft() }, Field::Token)]
    #[case(Draft { chat: "abc".to_owned(), ..draft() }, Field::Chat)]
    #[case(Draft { users: " , ".to_owned(), ..draft() }, Field::Users)]
    #[case(Draft { users: "1, x".to_owned(), ..draft() }, Field::Users)]
    #[case(Draft { workspace_root: "relative".to_owned(), ..draft() }, Field::WorkspaceRoot)]
    #[case(Draft { cli: "claude".to_owned(), ..draft() }, Field::Cli)]
    #[case(Draft { budget: "-1".to_owned(), ..draft() }, Field::Budget)]
    #[case(Draft { budget: "NaN".to_owned(), ..draft() }, Field::Budget)]
    #[case(Draft { approval_timeout: "0".to_owned(), ..draft() }, Field::ApprovalTimeout)]
    #[case(Draft { background_timeout: "-5".to_owned(), ..draft() }, Field::BackgroundTimeout)]
    fn invalid_values_are_reported_per_field(#[case] draft: Draft, #[case] field: Field) {
        let errors = draft.parse(&home()).unwrap_err();
        assert_eq!(errors.iter().map(|error| error.field).collect::<Vec<_>>(), [field]);
        assert!(errors.iter().all(|error| !error.message.is_empty()));
    }

    #[test]
    fn all_errors_are_reported_at_once() {
        let errors = Draft::default().parse(&home()).unwrap_err();
        let fields: Vec<_> = errors.iter().map(|error| error.field).collect();
        assert_eq!(fields, [Field::Token, Field::Chat, Field::Users, Field::WorkspaceRoot]);
    }

    #[test]
    fn draft_round_trips_through_settings() {
        let settings = Draft { budget: "5".to_owned(), model: "m".to_owned(), ..draft() }
            .parse(&home())
            .unwrap();
        assert_eq!(Draft::from_settings(&settings).parse(&home()).unwrap(), settings);
    }

    #[test]
    fn file_round_trips_through_draft() {
        let settings = draft().parse(&home()).unwrap();
        let file = SettingsFile::from_settings(&settings);
        let restored = file.to_draft(settings.telegram.token.expose().to_owned()).unwrap();
        assert_eq!(restored.parse(&home()).unwrap(), settings);
    }

    #[test]
    fn file_with_unknown_permission_mode_is_rejected() {
        let mut file = SettingsFile::from_settings(&draft().parse(&home()).unwrap());
        file.claude.permission_mode = Some("yolo".to_owned());
        assert_eq!(file.to_draft(String::new()).unwrap_err().field, Field::PermissionMode);
    }

    #[test]
    fn debug_output_hides_token() {
        let settings = draft().parse(&home()).unwrap();
        assert!(!format!("{settings:?}").contains("123:abc"));
        assert!(!format!("{:?}", draft()).contains("123:abc"));
    }

    #[test]
    fn permission_modes_have_cli_names() {
        let names: Vec<_> = PermissionMode::ALL.into_iter().map(PermissionMode::wire).collect();
        assert_eq!(names, ["default", "acceptEdits", "plan", "bypassPermissions"]);
        assert_eq!(PermissionMode::parse("plan"), Some(PermissionMode::Plan));
        assert_eq!(PermissionMode::parse("yolo"), None);
    }
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-core settings`
Expected: FAIL — типы не определены.

- [ ] **Step 3: Реализовать `settings.rs`**

Над тестовым модулем:

```rust
//! Settings: the GUI form draft, the TOML file shape, and one parser for both.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::domain::{ChatId, UserId};
use crate::workspace::expand_home;

pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(600);
pub const DEFAULT_BACKGROUND_TIMEOUT: Duration = Duration::from_secs(1800);

#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(***)")
    }
}

/// Non-empty allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Users(BTreeSet<UserId>);

impl Users {
    #[must_use]
    pub fn contains(&self, user: UserId) -> bool {
        self.0.contains(&user)
    }

    pub fn iter(&self) -> impl Iterator<Item = UserId> + '_ {
        self.0.iter().copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    BypassPermissions,
}

impl PermissionMode {
    pub const ALL: [Self; 4] = [Self::Default, Self::AcceptEdits, Self::Plan, Self::BypassPermissions];

    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::BypassPermissions => "bypassPermissions",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.wire() == raw)
    }
}

/// Positive spending cap per task, in USD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget(Decimal);

impl Budget {
    #[must_use]
    pub fn amount(self) -> Decimal {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateCheck {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramSettings {
    pub token: Token,
    pub chat: ChatId,
    pub users: Users,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSettings {
    pub cli: Option<PathBuf>,
    pub model: Option<String>,
    pub permission_mode: PermissionMode,
    pub budget: Option<Budget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub approval: Duration,
    pub background: Duration,
}

/// Parsed settings. `workspace_root` is absolute; whether it exists is checked by the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub telegram: TelegramSettings,
    pub workspace_root: PathBuf,
    pub claude: ClaudeSettings,
    pub timeouts: Timeouts,
    pub updates: UpdateCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Token,
    Chat,
    Users,
    WorkspaceRoot,
    Cli,
    Model,
    PermissionMode,
    Budget,
    ApprovalTimeout,
    BackgroundTimeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: Field,
    pub message: String,
}

/// The settings form as typed by the user.
#[derive(Clone, PartialEq, Eq)]
pub struct Draft {
    pub token: String,
    pub chat: String,
    pub users: String,
    pub workspace_root: String,
    pub cli: String,
    pub model: String,
    pub permission_mode: PermissionMode,
    pub budget: String,
    pub approval_timeout: String,
    pub background_timeout: String,
    pub updates: UpdateCheck,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            token: String::new(),
            chat: String::new(),
            users: String::new(),
            workspace_root: String::new(),
            cli: String::new(),
            model: String::new(),
            permission_mode: PermissionMode::Default,
            budget: String::new(),
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT.as_secs().to_string(),
            background_timeout: DEFAULT_BACKGROUND_TIMEOUT.as_secs().to_string(),
            updates: UpdateCheck::Enabled,
        }
    }
}

impl fmt::Debug for Draft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Draft")
            .field("token", &"***")
            .field("chat", &self.chat)
            .field("users", &self.users)
            .field("workspace_root", &self.workspace_root)
            .field("cli", &self.cli)
            .field("model", &self.model)
            .field("permission_mode", &self.permission_mode)
            .field("budget", &self.budget)
            .field("approval_timeout", &self.approval_timeout)
            .field("background_timeout", &self.background_timeout)
            .field("updates", &self.updates)
            .finish()
    }
}

impl Draft {
    /// Parses every field and reports all errors at once, in form order.
    pub fn parse(&self, home: &Path) -> Result<Settings, Vec<FieldError>> {
        let mut errors = Vec::new();
        let token = check(&mut errors, Field::Token, parse_token(&self.token));
        let chat = check(&mut errors, Field::Chat, parse_chat(&self.chat));
        let users = check(&mut errors, Field::Users, parse_users(&self.users));
        let root = check(&mut errors, Field::WorkspaceRoot, parse_root(&self.workspace_root, home));
        let cli = check(&mut errors, Field::Cli, parse_cli(&self.cli, home));
        let budget = check(&mut errors, Field::Budget, parse_budget(&self.budget));
        let approval = check(&mut errors, Field::ApprovalTimeout, parse_seconds(&self.approval_timeout));
        let background = check(&mut errors, Field::BackgroundTimeout, parse_seconds(&self.background_timeout));
        let (Some(token), Some(chat), Some(users), Some(workspace_root), Some(cli), Some(budget), Some(approval), Some(background)) =
            (token, chat, users, root, cli, budget, approval, background)
        else {
            return Err(errors);
        };
        Ok(Settings {
            telegram: TelegramSettings { token, chat, users },
            workspace_root,
            claude: ClaudeSettings {
                cli,
                model: non_blank(&self.model),
                permission_mode: self.permission_mode,
                budget,
            },
            timeouts: Timeouts { approval, background },
            updates: self.updates,
        })
    }

    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            token: settings.telegram.token.expose().to_owned(),
            chat: settings.telegram.chat.0.to_string(),
            users: settings.telegram.users.iter().map(|user| user.0.to_string()).collect::<Vec<_>>().join(", "),
            workspace_root: settings.workspace_root.display().to_string(),
            cli: settings.claude.cli.as_ref().map(|cli| cli.display().to_string()).unwrap_or_default(),
            model: settings.claude.model.clone().unwrap_or_default(),
            permission_mode: settings.claude.permission_mode,
            budget: settings.claude.budget.map(|budget| budget.amount().to_string()).unwrap_or_default(),
            approval_timeout: settings.timeouts.approval.as_secs().to_string(),
            background_timeout: settings.timeouts.background.as_secs().to_string(),
            updates: settings.updates,
        }
    }
}

fn check<T>(errors: &mut Vec<FieldError>, field: Field, result: Result<T, String>) -> Option<T> {
    result.map_err(|message| errors.push(FieldError { field, message })).ok()
}

fn non_blank(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn parse_token(raw: &str) -> Result<Token, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Укажите токен бота от @BotFather".to_owned());
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err("Токен не должен содержать пробелов".to_owned());
    }
    Ok(Token(trimmed.to_owned()))
}

fn parse_chat(raw: &str) -> Result<ChatId, String> {
    raw.trim().parse().map(ChatId).map_err(|_| "Ожидается целое число, например -1001234567890".to_owned())
}

fn parse_users(raw: &str) -> Result<Users, String> {
    let users = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().map(UserId).map_err(|_| format!("«{part}» — не user id")))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if users.is_empty() {
        return Err("Нужен хотя бы один user id".to_owned());
    }
    Ok(Users(users))
}

fn parse_root(raw: &str, home: &Path) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Укажите корень рабочих директорий".to_owned());
    }
    absolute(trimmed, home)
}

fn parse_cli(raw: &str, home: &Path) -> Result<Option<PathBuf>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() { Ok(None) } else { absolute(trimmed, home).map(Some) }
}

fn absolute(raw: &str, home: &Path) -> Result<PathBuf, String> {
    let path = expand_home(raw, home);
    if path.is_absolute() { Ok(path) } else { Err("Нужен абсолютный путь или путь от ~".to_owned()) }
}

fn parse_budget(raw: &str) -> Result<Option<Budget>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let amount = Decimal::from_str(trimmed).map_err(|_| "Ожидается число, например 5 или 2.50".to_owned())?;
    if amount <= Decimal::ZERO {
        return Err("Лимит должен быть больше нуля".to_owned());
    }
    Ok(Some(Budget(amount)))
}

fn parse_seconds(raw: &str) -> Result<Duration, String> {
    match raw.trim().parse::<u64>() {
        Ok(seconds) if seconds > 0 => Ok(Duration::from_secs(seconds)),
        Ok(_) | Err(_) => Err("Ожидается целое число секунд больше нуля".to_owned()),
    }
}

/// `settings.toml`; the token is kept in the OS keyring, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsFile {
    pub telegram: TelegramFile,
    pub workspace: WorkspaceFile,
    #[serde(default)]
    pub claude: ClaudeFile,
    #[serde(default)]
    pub timeouts: TimeoutsFile,
    #[serde(default)]
    pub updates: UpdatesFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramFile {
    pub chat: i64,
    pub users: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceFile {
    pub root: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeFile {
    pub cli: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub budget: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeoutsFile {
    pub approval_seconds: Option<u64>,
    pub background_seconds: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatesFile {
    pub check: Option<bool>,
}

impl SettingsFile {
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            telegram: TelegramFile {
                chat: settings.telegram.chat.0,
                users: settings.telegram.users.iter().map(|user| user.0).collect(),
            },
            workspace: WorkspaceFile { root: settings.workspace_root.display().to_string() },
            claude: ClaudeFile {
                cli: settings.claude.cli.as_ref().map(|cli| cli.display().to_string()),
                model: settings.claude.model.clone(),
                permission_mode: Some(settings.claude.permission_mode.wire().to_owned()),
                budget: settings.claude.budget.map(|budget| budget.amount().to_string()),
            },
            timeouts: TimeoutsFile {
                approval_seconds: Some(settings.timeouts.approval.as_secs()),
                background_seconds: Some(settings.timeouts.background.as_secs()),
            },
            updates: UpdatesFile {
                check: Some(match settings.updates {
                    UpdateCheck::Enabled => true,
                    UpdateCheck::Disabled => false,
                }),
            },
        }
    }

    /// The file as a form draft, so file and form share one parser.
    pub fn to_draft(&self, token: String) -> Result<Draft, FieldError> {
        let permission_mode = match self.claude.permission_mode.as_deref() {
            None => PermissionMode::Default,
            Some(raw) => PermissionMode::parse(raw).ok_or_else(|| FieldError {
                field: Field::PermissionMode,
                message: format!("Неизвестный режим «{raw}»"),
            })?,
        };
        let defaults = Draft::default();
        Ok(Draft {
            token,
            chat: self.telegram.chat.to_string(),
            users: self.telegram.users.iter().map(u64::to_string).collect::<Vec<_>>().join(", "),
            workspace_root: self.workspace.root.clone(),
            cli: self.claude.cli.clone().unwrap_or_default(),
            model: self.claude.model.clone().unwrap_or_default(),
            permission_mode,
            budget: self.claude.budget.clone().unwrap_or_default(),
            approval_timeout: self.timeouts.approval_seconds.map_or(defaults.approval_timeout, |s| s.to_string()),
            background_timeout: self.timeouts.background_seconds.map_or(defaults.background_timeout, |s| s.to_string()),
            updates: match self.updates.check {
                Some(false) => UpdateCheck::Disabled,
                Some(true) | None => UpdateCheck::Enabled,
            },
        })
    }
}
```

`Field::Model` в разборе не выдаётся (пустая модель = из настроек Claude Code), но нужен GUI для привязки подсказок к полю — оставить вариант.

- [ ] **Step 4: Прогнать тесты и линтеры**

Run: `cargo test -p hub-core && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS. Если clippy `dead_code` ругается на `Field::Model` — использовать его в `parse`: модель с пробелами внутри → ошибка `Field::Model` «Имя модели не должно содержать пробелов» и тест-кейс `#[case(Draft { model: "a b".to_owned(), ..draft() }, Field::Model)]`.

- [ ] **Step 5: Commit**

```bash
git add crates/hub-core
git commit -m "Настройки: черновик формы, файл и единый разбор"
```

---

### Task 7: Формат topics.json

**Files:**
- Create: `crates/hub-core/src/topics.rs`
- Modify: `crates/hub-core/src/lib.rs` (`pub mod topics;`)

**Interfaces:**
- Consumes: `domain::{AbsolutePath, BackendKind, ChatId, SessionId, ThreadId, TopicKey, TopicSession}`.
- Produces: `topics::Topics = BTreeMap<TopicKey, TopicSession>`, `topics::parse_state(&str) -> Result<Topics, CorruptState>`, `topics::dump_state(&Topics) -> Result<String, DumpError>`, `topics::CorruptState::{Json, Version, Entry}`, `topics::DumpError::{Json, NonUtf8Path}`.

- [ ] **Step 1: Написать падающие тесты**

`crates/hub-core/src/topics.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rstest::rstest;

    use super::*;

    fn absolute(name: &str) -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" }).join(name)
    }

    fn session(name: &str, id: Option<&str>) -> TopicSession {
        TopicSession {
            backend: BackendKind::Claude,
            cwd: AbsolutePath::new(absolute(name)).unwrap(),
            session: id.and_then(SessionId::parse),
        }
    }

    fn json_path(name: &str) -> String {
        serde_json::to_string(&absolute(name).display().to_string()).unwrap()
    }

    #[test]
    fn empty_state_round_trips() {
        assert_eq!(parse_state(&dump_state(&Topics::new()).unwrap()).unwrap(), Topics::new());
    }

    #[test]
    fn sessions_round_trip() {
        let topics = Topics::from([
            (TopicKey { chat: ChatId(-100_123), thread: ThreadId(42) }, session("a", Some("abc"))),
            (TopicKey { chat: ChatId(1), thread: ThreadId(2) }, session("b", None)),
        ]);
        assert_eq!(parse_state(&dump_state(&topics).unwrap()).unwrap(), topics);
    }

    #[test]
    fn python_written_state_is_read() {
        let raw = format!(
            r#"{{
  "version": 1,
  "topics": [
    {{
      "chat_id": -100123,
      "thread_id": 42,
      "backend": "claude",
      "cwd": {},
      "session_id": "abc"
    }}
  ]
}}"#,
            json_path("проект")
        );
        let topics = parse_state(&raw).unwrap();
        assert_eq!(
            topics.get(&TopicKey { chat: ChatId(-100_123), thread: ThreadId(42) }),
            Some(&session("проект", Some("abc")))
        );
    }

    #[test]
    fn dump_keeps_non_ascii_readable() {
        let topics = Topics::from([(TopicKey { chat: ChatId(1), thread: ThreadId(2) }, session("проект", None))]);
        assert!(dump_state(&topics).unwrap().contains("проект"));
    }

    #[rstest]
    #[case("not json".to_owned())]
    #[case(r#"{"version": 999, "topics": []}"#.to_owned())]
    #[case(r#"{"version": 1, "topics": {}}"#.to_owned())]
    #[case(r#"{"version": 1, "topics": [{"chat_id": "1"}]}"#.to_owned())]
    #[case(format!(r#"{{"version": 1, "topics": [{{"chat_id": 1, "thread_id": 2, "backend": "gpt", "cwd": {}, "session_id": null}}]}}"#, json_path("a")))]
    #[case(r#"{"version": 1, "topics": [{"chat_id": 1, "thread_id": 2, "backend": "claude", "cwd": "rel", "session_id": null}]}"#.to_owned())]
    #[case(format!(r#"{{"version": 1, "topics": [{{"chat_id": 1, "thread_id": 2, "backend": "claude", "cwd": {}, "session_id": " "}}]}}"#, json_path("a")))]
    fn corrupt_state_is_rejected(#[case] raw: String) {
        assert!(parse_state(&raw).is_err());
    }
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-core topics`
Expected: FAIL — функции не определены.

- [ ] **Step 3: Реализовать `topics.rs`**

Над тестовым модулем:

```rust
//! `topics.json`: topic → session bindings, format version 1 shared with the Python version.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::{AbsolutePath, BackendKind, ChatId, SessionId, ThreadId, TopicKey, TopicSession};

const FORMAT_VERSION: u32 = 1;

pub type Topics = BTreeMap<TopicKey, TopicSession>;

#[derive(Debug, thiserror::Error)]
pub enum CorruptState {
    #[error("файл не является JSON нужной формы")]
    Json(#[from] serde_json::Error),
    #[error("ожидалась версия формата {FORMAT_VERSION}, найдена {0}")]
    Version(u32),
    #[error("тема №{index}: {reason}")]
    Entry { index: usize, reason: &'static str },
}

#[derive(Debug, thiserror::Error)]
pub enum DumpError {
    #[error("не удалось сериализовать привязки тем")]
    Json(#[from] serde_json::Error),
    #[error("путь {0} не в UTF-8")]
    NonUtf8Path(PathBuf),
}

#[derive(Serialize, Deserialize)]
struct StateFile {
    version: u32,
    topics: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    #[serde(rename = "chat_id")]
    chat: i64,
    #[serde(rename = "thread_id")]
    thread: i32,
    backend: String,
    cwd: String,
    #[serde(rename = "session_id")]
    session: Option<String>,
}

pub fn parse_state(raw: &str) -> Result<Topics, CorruptState> {
    let file: StateFile = serde_json::from_str(raw)?;
    if file.version != FORMAT_VERSION {
        return Err(CorruptState::Version(file.version));
    }
    file.topics
        .into_iter()
        .enumerate()
        .map(|(index, entry)| parse_entry(entry).map_err(|reason| CorruptState::Entry { index, reason }))
        .collect()
}

fn parse_entry(entry: Entry) -> Result<(TopicKey, TopicSession), &'static str> {
    let backend = BackendKind::parse(&entry.backend).ok_or("неизвестный бэкенд")?;
    let cwd = AbsolutePath::new(PathBuf::from(entry.cwd)).ok_or("cwd должен быть абсолютным путём")?;
    let session = match entry.session {
        None => None,
        Some(raw) => Some(SessionId::parse(&raw).ok_or("пустой session_id")?),
    };
    Ok((
        TopicKey { chat: ChatId(entry.chat), thread: ThreadId(entry.thread) },
        TopicSession { backend, cwd, session },
    ))
}

pub fn dump_state(topics: &Topics) -> Result<String, DumpError> {
    let entries = topics
        .iter()
        .map(|(key, session)| {
            let cwd = session.cwd.as_path();
            Ok(Entry {
                chat: key.chat.0,
                thread: key.thread.0,
                backend: session.backend.name().to_owned(),
                cwd: cwd.to_str().ok_or_else(|| DumpError::NonUtf8Path(cwd.to_path_buf()))?.to_owned(),
                session: session.session.as_ref().map(|id| id.as_str().to_owned()),
            })
        })
        .collect::<Result<Vec<_>, DumpError>>()?;
    Ok(serde_json::to_string_pretty(&StateFile { version: FORMAT_VERSION, topics: entries })?)
}
```

- [ ] **Step 4: Прогнать весь крейт**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hub-core
git commit -m "Формат topics.json, совместимый с Python-версией"
```

---

## Самопроверка плана

- Покрытие спеки для фазы 1: доменные типы (Task 1), разбиение и форматы (Task 2), Markdown (Task 3), подтверждения и вопросы (Task 4), вложения/команды/пути (Task 5), настройки и форма (Task 6), `topics.json` (Task 7). Файловые проверки, протокол CLI, Telegram, GUI — фазы 2–5 по дорожной карте.
- Типы между задачами: `TopicKey { chat, thread }`, `Denied::new`, `Question::new(...) -> Option`, `truncate(&str, usize) -> String`, `char_len` — используются одинаково во всех задачах.
