# Фаза 3: hub-telegram — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Крейт `hub-telegram` — Telegram-оболочка agent-hub: актор `Hub` (темы, сессии, команды, альбомы), `TelegramChannel` (подтверждения, вопросы, `send_file`), загрузка вложений, проверки путей на файловой системе и обвязка teloxide 0.17.

**Architecture:** Всё, что зависит от Bot API, спрятано за трейтом `Messenger` (отправка, правка, ответы на кнопки, файлы). Политика доставки Python-версии (повтор после `RetryAfter`, откат с HTML на обычный текст, разбиение) живёт в `Sender` поверх `Messenger`. Актор `Hub` владеет состоянием тем и обрабатывает сообщения по одному, не ожидая сети: любая отправка — отдельная задача. Сессия темы — отдельная задача, которая сообщает о себе актору (`Bind`, `Ended`). Реестры подтверждений и вопросов разделяются между актором, каналом и обработчиком кнопок через `Arc<Mutex<_>>` с короткими критическими секциями. Агент подключается через трейт `Agents`; рабочая реализация `ClaudeAgents` выбирает бэкенд `match`-ем по `BackendKind`. Эталон — `agent-hub/src/agent_hub/bot.py`.

**Tech Stack:** Rust 1.96, teloxide 0.17 (`rustls`), tokio 1.53, tokio-util 0.7, futures 0.3, dunce 1.0, uuid 1.26, tracing 0.1; тесты — rstest, tempfile 3.27, serde_json.

**Spec:** `docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md`

## Global Constraints

- Ограничения фаз 1–2 действуют без изменений (линты, тексты по-русски байт-в-байт как в Python, без упоминаний Claude в коммитах).
- Таймауты: HTTP-клиент Telegram по умолчанию teloxide (17 с, соединение 5 с); отправка и скачивание файлов — отдельный клиент, 120 с; ожидание после `RetryAfter` — не больше 60 с; ожидание остановки сессий при выключении — 10 с; склейка альбома — 1 с.
- Лимиты: скачивание вложения 20 МБ, отправка файла 50 МБ, подпись 1024 символа, inbox сессии — 32 сообщения, очередь событий сессии — 256.
- Пути: канонизация только через `dunce::canonicalize`; проверки «внутри корня» и «внутри cwd» делаются после разрешения симлинков.
- Актор не ждёт сети: обработка `HubMessage` синхронна, все отправки — `tokio::spawn`.
- Интеграционные тесты в `tests/` оборачиваются в `#[cfg(test)] mod tests` (правило из фазы 2).

## Review Focus

1. Агент или пользователь указывает путь через симлинк или `..` за пределы корня/cwd (`/cwd`, `/new`, `send_file`, вложение с именем `../../x`, заранее подложенный симлинк в `.agent-hub/uploads`) — отказ без записи и чтения снаружи. Тесты: `escape_from_root_is_rejected`, `symlink_escape_is_rejected` (unix), `outgoing_path_rejects_unsendable`, `prepare_upload_rejects_symlink_escape` (unix), `planted_upload_target_is_refused` (unix).
2. Нажатие кнопки после таймаута, после `/stop` или дважды — ответ «уже неактуален», без паники и без двойного решения. Тесты: `stale_approval_press_is_answered_as_stale`, `dropped_request_closes_its_approval`.
3. Сообщение приходит в тему, пока её сессия завершается, — оно не теряется и запускает новую сессию. Тест: `leftover_prompt_starts_a_new_session`.
4. Пользователь вне allowlist или чужой чат — апдейт отбрасывается до актора и логируется с chat_id/user_id. Тест: `only_allowed_users_in_the_chat_pass`.
5. Telegram отвергает HTML-разметку или просит подождать — сообщение всё равно доставляется (обычным текстом) или честно теряется с записью в лог, агент продолжает. Тесты: `rejected_html_is_resent_as_plain_text`, `retry_after_is_honoured_once`.

## Отклонения от спеки (вносятся в спеку в Task 1)

- Реестры подтверждений и вопросов — `Arc<Mutex<Registries>>`, а не состояние актора: канал ждёт ответа в задаче сессии, а нажатие кнопки обрабатывается отдельной задачей; блокировка держится только на время вставки/удаления.
- Сообщения, пришедшие после `/stop` и не прочитанные сессией, запускают новую сессию (Python их терял вместе с задачей).
- Ответ на `/help` и подсказка «создайте тему» в общей теме отправляются ответом на сообщение (`Target::Reply`), как `reply_text` в Python.

---

### Task 1: Крейт и проверки путей

**Files:**
- Modify: `Cargo.toml` (members, workspace.dependencies), `docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md`
- Create: `crates/hub-telegram/Cargo.toml`, `crates/hub-telegram/src/lib.rs`, `crates/hub-telegram/src/paths.rs`

**Interfaces:**
- Produces: `paths::CwdError::{Root, Missing, Outside, NotDirectory}`, `paths::workspace_root(&Path) -> Result<AbsolutePath, CwdError>`, `paths::resolve_cwd(&AbsolutePath, &Path, Option<&str>) -> Result<AbsolutePath, CwdError>`, `paths::inside(&AbsolutePath, &AbsolutePath) -> bool`, `paths::FileError::{Outside, Missing, TooLarge}`, `paths::outgoing_path(&AbsolutePath, &Path, &str, u64) -> Result<PathBuf, FileError>`, `paths::UploadError::{Outside, Exists, Io}`, `paths::prepare_upload(&AbsolutePath, &Path) -> Result<(), UploadError>`.

- [ ] **Step 1: Workspace и манифест**

`Cargo.toml`: `members = ["crates/hub-core", "crates/hub-claude", "crates/hub-telegram"]`; в `[workspace.dependencies]` добавить:

```toml
hub-claude = { path = "crates/hub-claude" }
dunce = "1.0"
tempfile = "3.27"
teloxide = { version = "0.17", default-features = false, features = ["rustls"] }
```

`crates/hub-telegram/Cargo.toml`:

```toml
[package]
name = "hub-telegram"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
dunce.workspace = true
futures.workspace = true
hub-claude.workspace = true
hub-core.workspace = true
teloxide.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["fs", "macros", "rt", "sync", "time"] }
tokio-util.workspace = true
tracing.workspace = true
uuid = { workspace = true, features = ["v4"] }

[dev-dependencies]
rstest.workspace = true
serde_json.workspace = true
tempfile.workspace = true
tokio = { workspace = true, features = ["fs", "macros", "rt", "rt-multi-thread", "sync", "test-util", "time"] }

[lints]
workspace = true
```

`crates/hub-telegram/src/lib.rs`:

```rust
pub mod paths;
```

- [ ] **Step 2: Падающие тесты (перенос `test_workspace.py`, `test_attachments.py`)**

`crates/hub-telegram/src/paths.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::fs;

    use hub_core::attachments::upload_path;
    use hub_core::domain::MessageId;
    use rstest::rstest;
    use tempfile::TempDir;

    use super::*;

    fn home() -> PathBuf {
        std::env::temp_dir()
    }

    /// A root with `project/sub` and a regular file `file.txt`.
    fn workspace() -> (TempDir, AbsolutePath) {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("project").join("sub")).unwrap();
        fs::write(dir.path().join("file.txt"), "x").unwrap();
        let root = workspace_root(dir.path()).unwrap();
        (dir, root)
    }

    #[test]
    fn missing_path_means_root() {
        let (_dir, root) = workspace();
        assert_eq!(resolve_cwd(&root, &home(), None).unwrap(), root);
        assert_eq!(resolve_cwd(&root, &home(), Some("  ")).unwrap(), root);
    }

    #[test]
    fn relative_path_is_under_root() {
        let (_dir, root) = workspace();
        let expected = root.as_path().join("project").join("sub");
        assert_eq!(resolve_cwd(&root, &home(), Some("project/sub")).unwrap().as_path(), expected);
    }

    #[test]
    fn absolute_path_inside_root_is_accepted() {
        let (_dir, root) = workspace();
        let inside = root.as_path().join("project");
        let resolved = resolve_cwd(&root, &home(), inside.to_str()).unwrap();
        assert_eq!(resolved.as_path(), inside);
    }

    #[rstest]
    #[case("..")]
    #[case("../..")]
    #[case("/")]
    #[case("project/../../")]
    fn escape_from_root_is_rejected(#[case] raw: &str) {
        let (_dir, root) = workspace();
        assert!(matches!(resolve_cwd(&root, &home(), Some(raw)), Err(CwdError::Outside { .. })));
    }

    #[rstest]
    #[case("missing")]
    #[case("file.txt")]
    fn non_directory_is_rejected(#[case] raw: &str) {
        let (_dir, root) = workspace();
        assert!(resolve_cwd(&root, &home(), Some(raw)).is_err());
    }

    #[test]
    fn missing_root_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(workspace_root(&dir.path().join("gone")), Err(CwdError::Root { .. })));
    }

    #[test]
    fn stored_cwd_outside_a_new_root_is_detected() {
        let (_dir, root) = workspace();
        let other = tempfile::tempdir().unwrap();
        let elsewhere = workspace_root(other.path()).unwrap();
        let project = resolve_cwd(&root, &home(), Some("project")).unwrap();
        assert!(inside(&root, &project));
        assert!(!inside(&elsewhere, &project));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        let (_dir, root) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.as_path().join("link")).unwrap();
        assert!(matches!(resolve_cwd(&root, &home(), Some("link")), Err(CwdError::Outside { .. })));
    }

    #[test]
    fn outgoing_path_resolves_relative_to_cwd() {
        let (_dir, root) = workspace();
        fs::create_dir(root.as_path().join("out")).unwrap();
        fs::write(root.as_path().join("out").join("r.pdf"), "%PDF").unwrap();
        let expected = root.as_path().join("out").join("r.pdf");
        assert_eq!(outgoing_path(&root, &home(), "out/r.pdf", 10).unwrap(), expected);
        let absolute = expected.to_str().unwrap().to_owned();
        assert_eq!(outgoing_path(&root, &home(), &absolute, 10).unwrap(), expected);
    }

    #[rstest]
    #[case("missing.pdf")]
    #[case("project")]
    #[case("../secret")]
    fn outgoing_path_rejects_unsendable(#[case] raw: &str) {
        let parent = tempfile::tempdir().unwrap();
        fs::create_dir_all(parent.path().join("cwd").join("project")).unwrap();
        fs::write(parent.path().join("secret"), "x").unwrap();
        let cwd = workspace_root(&parent.path().join("cwd")).unwrap();
        assert!(outgoing_path(&cwd, &home(), raw, 10).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn outgoing_symlink_out_of_cwd_is_rejected() {
        let parent = tempfile::tempdir().unwrap();
        fs::create_dir(parent.path().join("cwd")).unwrap();
        fs::write(parent.path().join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(parent.path().join("secret"), parent.path().join("cwd").join("link"))
            .unwrap();
        let cwd = workspace_root(&parent.path().join("cwd")).unwrap();
        assert!(matches!(outgoing_path(&cwd, &home(), "link", 10), Err(FileError::Outside { .. })));
    }

    #[test]
    fn outgoing_path_rejects_oversized() {
        let (_dir, root) = workspace();
        fs::write(root.as_path().join("big.bin"), vec![0_u8; 11]).unwrap();
        assert!(matches!(
            outgoing_path(&root, &home(), "big.bin", 10),
            Err(FileError::TooLarge { size: 11, limit: 10, .. })
        ));
    }

    #[test]
    fn prepare_upload_creates_ignored_directory() {
        let (_dir, root) = workspace();
        prepare_upload(&root, &upload_path(root.as_path(), MessageId(1), Some("a"))).unwrap();
        assert!(root.as_path().join(".agent-hub").join("uploads").is_dir());
        assert_eq!(
            fs::read_to_string(root.as_path().join(".agent-hub").join(".gitignore")).unwrap(),
            "*\n"
        );
    }

    #[test]
    fn prepare_upload_refuses_existing_target() {
        let (_dir, root) = workspace();
        let target = upload_path(root.as_path(), MessageId(5), Some("a.txt"));
        prepare_upload(&root, &target).unwrap();
        fs::write(&target, "planted").unwrap();
        assert!(matches!(prepare_upload(&root, &target), Err(UploadError::Exists(_))));
    }

    #[cfg(unix)]
    #[test]
    fn prepare_upload_rejects_symlink_escape() {
        let (_dir, root) = workspace();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.as_path().join(".agent-hub")).unwrap();
        let target = upload_path(root.as_path(), MessageId(1), Some("a"));
        assert!(matches!(prepare_upload(&root, &target), Err(UploadError::Outside { .. })));
    }

    #[cfg(unix)]
    #[test]
    fn planted_upload_target_is_refused() {
        let (_dir, root) = workspace();
        let target = upload_path(root.as_path(), MessageId(5), Some("a.txt"));
        prepare_upload(&root, &target).unwrap();
        fs::write(root.as_path().join("secret"), "x").unwrap();
        std::os::unix::fs::symlink(root.as_path().join("secret"), &target).unwrap();
        assert!(matches!(prepare_upload(&root, &target), Err(UploadError::Exists(_))));
    }
}
```

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p hub-telegram --lib`
Expected: FAIL — `cannot find function workspace_root`.

- [ ] **Step 4: Реализовать `paths.rs`**

```rust
//! Filesystem checks for working directories and for files crossing the chat boundary.
//! Symlinks are resolved before every containment check, so a link cannot lead outside.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hub_core::attachments::{GITIGNORE, UPLOADS_DIR};
use hub_core::domain::AbsolutePath;
use hub_core::workspace::{candidate, expand_home};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum CwdError {
    #[error("корень рабочих директорий {} недоступен", path.display())]
    Root {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} не существует", .0.display())]
    Missing(PathBuf),
    #[error("{} вне корня рабочих директорий {}", path.display(), root.display())]
    Outside { path: PathBuf, root: PathBuf },
    #[error("{} — не директория", .0.display())]
    NotDirectory(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum FileError {
    #[error("{raw}: файл вне рабочей директории {}", cwd.display())]
    Outside { raw: String, cwd: PathBuf },
    #[error("{0}: файл не найден")]
    Missing(String),
    #[error("{raw}: {} МБ, Telegram принимает от бота не больше {} МБ", size / MIB, limit / MIB)]
    TooLarge { raw: String, size: u64, limit: u64 },
}

#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    #[error("{} ведёт за пределы {}", directory.display(), cwd.display())]
    Outside { directory: PathBuf, cwd: PathBuf },
    #[error("{} уже существует", .0.display())]
    Exists(PathBuf),
    #[error("Не удалось сохранить вложение: {0}")]
    Io(#[from] io::Error),
}

/// The configured root in canonical form, so containment checks compare like with like.
pub fn workspace_root(path: &Path) -> Result<AbsolutePath, CwdError> {
    let canonical = dunce::canonicalize(path)
        .map_err(|source| CwdError::Root { path: path.to_path_buf(), source })?;
    directory(canonical)
}

/// `raw` — absolute, `~`-prefixed or relative to `root`; blank means `root`.
pub fn resolve_cwd(
    root: &AbsolutePath,
    home: &Path,
    raw: Option<&str>,
) -> Result<AbsolutePath, CwdError> {
    let path = candidate(root.as_path(), home, raw);
    let resolved = dunce::canonicalize(&path).map_err(|_| CwdError::Missing(path))?;
    if !resolved.starts_with(root.as_path()) {
        return Err(CwdError::Outside { path: resolved, root: root.as_path().to_path_buf() });
    }
    directory(resolved)
}

/// Whether a stored working directory is still inside the (possibly changed) root.
#[must_use]
pub fn inside(root: &AbsolutePath, cwd: &AbsolutePath) -> bool {
    dunce::canonicalize(cwd.as_path()).is_ok_and(|real| real.starts_with(root.as_path()))
}

fn directory(path: PathBuf) -> Result<AbsolutePath, CwdError> {
    if !path.is_dir() {
        return Err(CwdError::NotDirectory(path));
    }
    let shown = path.clone();
    AbsolutePath::new(path).ok_or(CwdError::NotDirectory(shown))
}

/// A file the agent wants to send: only a regular file inside `cwd` qualifies.
pub fn outgoing_path(
    cwd: &AbsolutePath,
    home: &Path,
    raw: &str,
    limit: u64,
) -> Result<PathBuf, FileError> {
    let missing = || FileError::Missing(raw.to_owned());
    let root = dunce::canonicalize(cwd.as_path()).map_err(|_| missing())?;
    let path = dunce::canonicalize(root.join(expand_home(raw, home))).map_err(|_| missing())?;
    if !path.starts_with(&root) {
        return Err(FileError::Outside { raw: raw.to_owned(), cwd: root });
    }
    let metadata = fs::metadata(&path).map_err(|_| missing())?;
    if !metadata.is_file() {
        return Err(missing());
    }
    if metadata.len() > limit {
        return Err(FileError::TooLarge { raw: raw.to_owned(), size: metadata.len(), limit });
    }
    Ok(path)
}

/// Makes `target` writable without letting a symlink redirect the write out of `cwd`.
pub fn prepare_upload(cwd: &AbsolutePath, target: &Path) -> Result<(), UploadError> {
    let root = dunce::canonicalize(cwd.as_path())?;
    let directory = target.parent().unwrap_or(cwd.as_path());
    fs::create_dir_all(directory)?;
    let real = dunce::canonicalize(directory)?;
    if !real.starts_with(&root) {
        return Err(UploadError::Outside { directory: directory.to_path_buf(), cwd: root });
    }
    // Names are unique per message, so an existing entry was planted, not uploaded.
    if fs::symlink_metadata(target).is_ok() {
        return Err(UploadError::Exists(target.to_path_buf()));
    }
    let [hub, _uploads] = UPLOADS_DIR;
    let ignore = cwd.as_path().join(hub).join(".gitignore");
    if fs::symlink_metadata(&ignore).is_err() {
        fs::write(&ignore, GITIGNORE)?;
    }
    Ok(())
}
```

- [ ] **Step 5: Прогнать тесты и линтеры**

Run: `cargo test -p hub-telegram --lib && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS. Если `teloxide` с `default-features = false, features = ["rustls"]` не собирается — посмотреть ошибку и добавить недостающую фичу (`ctrlc_handler` не нужна), записав решение в ledger.

- [ ] **Step 6: Отклонения в спеке**

В раздел «hub-telegram» спеки добавить три пункта из раздела «Отклонения от спеки» этого плана (реестры под `Arc<Mutex<_>>`, сообщения после `/stop` запускают новую сессию, ответы в общей теме — `reply`).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/hub-telegram docs
git commit -m "hub-telegram: проверки путей workspace, вложений и send_file"
```

---

### Task 2: Тексты и входящие апдейты

**Files:**
- Create: `crates/hub-telegram/src/texts.rs`, `crates/hub-telegram/src/inbound.rs`
- Modify: `crates/hub-telegram/src/lib.rs`

**Interfaces:**
- Produces:
  - `texts::{CREATE_TOPIC, ALREADY_RUNNING, NOTHING_TO_STOP, NO_SESSION, RUNNING, WAITING, VOICE_UNSUPPORTED, CWD_USAGE, STOPPED, INTERNAL_ERROR, NEW_SESSION, CWD_CHANGED, CONTEXT_RESET, APPROVAL_STALE, QUESTION_STALE, NOTHING_SELECTED, QUEUE_FULL, APPROVAL_UNSENT, QUESTION_UNSENT}`, `texts::help(&Path) -> String`, `texts::describe(&str, &TopicSession) -> String`, `texts::approval_html(&ToolRequest) -> String`, `texts::tool_call_html(&ToolUse) -> String`, `texts::failure(&str) -> String`, `texts::warning(&dyn Display) -> String`, `texts::verdict(&Decision) -> &'static str`, `texts::no_answer(Duration) -> String`, `texts::outside_root(&AbsolutePath, &AbsolutePath) -> String`.
  - `inbound::{FileRef { id, name, size }, Attachment::{Photo(FileRef), File(FileRef)}, Content::{Text(String), Media { caption, attachment }, Voice, TopicCreated { name }, Ignored}, Inbound { chat, thread, user, message, album, content }, Press { callback, user, chat, message, html, data }, Upload { message, file }, Turn { text, photos, files }}`, `Inbound::key() -> Option<TopicKey>`, `Turn::text(String) -> Turn`, `Turn::from_parts(&[Inbound]) -> Turn`, `Turn::has_attachments() -> bool`.

- [ ] **Step 1: Падающие тесты**

`crates/hub-telegram/src/texts.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use hub_core::domain::{BackendKind, Denied, SessionId};

    use super::*;

    fn absolute(name: &str) -> AbsolutePath {
        let base = PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" });
        AbsolutePath::new(base.join(name)).unwrap()
    }

    #[test]
    fn help_names_root_uploads_and_backends() {
        let text = help(absolute("").as_path());
        assert!(text.starts_with("Каждая тема этой группы — отдельная сессия агента."));
        assert!(text.contains(".agent-hub/uploads"));
        assert!(text.contains(&absolute("").as_path().display().to_string()));
        assert!(text.ends_with("Бэкенды: claude"));
    }

    #[test]
    fn describe_lists_backend_cwd_and_session() {
        let session = TopicSession::fresh(BackendKind::Claude, absolute("p"));
        assert_eq!(
            describe(NEW_SESSION, &session),
            format!("🆕 Новая сессия\nbackend: claude\ncwd: {}\nsession: —", absolute("p").as_path().display())
        );
        let resumed = session.with_session(SessionId::parse("abc"));
        assert!(describe(WAITING, &resumed).ends_with("session: abc"));
    }

    #[test]
    fn approval_escapes_and_truncates() {
        let tool = ToolRequest { tool: "Bash".to_owned(), summary: format!("a<b {}", "x".repeat(4000)) };
        let html = approval_html(&tool);
        assert!(html.starts_with("🔐 <b>Bash</b>\n<pre>a&lt;b "));
        assert!(html.ends_with("…</pre>"));
    }

    #[test]
    fn tool_call_is_one_line() {
        let call = ToolUse { tool: "Read".to_owned(), summary: "src/<main>.rs".to_owned() };
        assert_eq!(tool_call_html(&call), "🔧 <b>Read</b> <code>src/&lt;main&gt;.rs</code>");
    }

    #[test]
    fn failure_is_marked_and_truncated() {
        assert_eq!(failure("boom"), "❌ boom");
        assert_eq!(failure(&"x".repeat(5000)).chars().count(), 3500);
    }

    #[test]
    fn verdicts_and_timeouts() {
        assert_eq!(verdict(&Decision::Allowed), "✅ Разрешено");
        assert_eq!(verdict(&Decision::Denied(Denied::new("нет"))), "❌ Запрещено");
        assert_eq!(no_answer(Duration::from_secs(600)), "Нет ответа пользователя за 600 с");
        assert_eq!(warning(&"плохо"), "⚠️ плохо");
    }
}
```

`crates/hub-telegram/src/inbound.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn part(message: i32, caption: &str, attachment: Attachment) -> Inbound {
        Inbound {
            chat: ChatId(-100),
            thread: Some(ThreadId(7)),
            user: Some(UserId(1)),
            message: MessageId(message),
            album: Some("g".to_owned()),
            content: Content::Media { caption: caption.to_owned(), attachment },
        }
    }

    fn file(id: &str, name: Option<&str>) -> FileRef {
        FileRef { id: id.to_owned(), name: name.map(str::to_owned), size: Some(10) }
    }

    #[test]
    fn key_needs_a_topic() {
        let mut inbound = part(1, "", Attachment::Photo(file("p", None)));
        assert_eq!(inbound.key(), Some(TopicKey { chat: ChatId(-100), thread: ThreadId(7) }));
        inbound.thread = None;
        assert_eq!(inbound.key(), None);
    }

    #[test]
    fn album_joins_captions_and_keeps_attachment_order() {
        let parts = [
            part(1, "первая", Attachment::Photo(file("p1", None))),
            part(2, "", Attachment::File(file("d1", Some("a.pdf")))),
            part(3, "третья", Attachment::Photo(file("p2", None))),
        ];
        let turn = Turn::from_parts(&parts);
        assert_eq!(turn.text, "первая\nтретья");
        assert_eq!(turn.photos.iter().map(|u| (u.message, u.file.id.as_str())).collect::<Vec<_>>(),
                   [(MessageId(1), "p1"), (MessageId(3), "p2")]);
        assert_eq!(turn.files.iter().map(|u| u.file.name.as_deref()).collect::<Vec<_>>(), [Some("a.pdf")]);
        assert!(turn.has_attachments());
        assert!(!Turn::text("hi".to_owned()).has_attachments());
    }
}
```

`crates/hub-telegram/src/lib.rs`:

```rust
pub mod inbound;
pub mod paths;
pub mod texts;
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-telegram --lib`
Expected: FAIL — `cannot find function help`.

- [ ] **Step 3: Реализовать `texts.rs`**

```rust
//! What the bot says, word for word as in the Python version.

use std::fmt::Display;
use std::path::Path;
use std::time::Duration;

use hub_core::attachments::UPLOADS_DIR;
use hub_core::domain::{AbsolutePath, BackendKind, Decision, ToolRequest, ToolUse, TopicSession};
use hub_core::escape::escape;
use hub_core::render::truncate;

pub const CREATE_TOPIC: &str = "Создайте тему в группе — каждая тема это отдельная сессия.";
pub const ALREADY_RUNNING: &str = "⏳ В этой теме уже выполняется задача. /stop — прервать.";
pub const NOTHING_TO_STOP: &str = "Нечего останавливать";
pub const NO_SESSION: &str = "Сессии нет — напишите задачу или /new";
pub const RUNNING: &str = "⏳ выполняется";
pub const WAITING: &str = "💤 ожидает";
pub const VOICE_UNSUPPORTED: &str =
    "🎤 Голосовые сообщения пока не поддерживаются — напишите текстом.";
pub const CWD_USAGE: &str = "Укажите путь: /cwd <путь>";
pub const STOPPED: &str = "⏹ Остановлено";
pub const INTERNAL_ERROR: &str = "💥 Внутренняя ошибка agent-hub, подробности в логе приложения";
pub const NEW_SESSION: &str = "🆕 Новая сессия";
pub const CWD_CHANGED: &str = "📁 Директория изменена";
pub const CONTEXT_RESET: &str = "🔄 Контекст сброшен";
pub const APPROVAL_STALE: &str = "Запрос уже неактуален";
pub const QUESTION_STALE: &str = "Вопрос уже неактуален";
pub const NOTHING_SELECTED: &str = "Отметьте хотя бы один вариант";
pub const QUEUE_FULL: &str = "⚠️ Слишком много сообщений в очереди — дождитесь ответа агента.";
pub const APPROVAL_UNSENT: &str = "Не удалось отправить запрос подтверждения в Telegram";
pub const QUESTION_UNSENT: &str = "Не удалось отправить вопрос в Telegram";
const APPROVAL_TEXT_LIMIT: usize = 3500;
const TOOL_CALL_TEXT_LIMIT: usize = 900;
const FAILURE_TEXT_LIMIT: usize = 3500;

#[must_use]
pub fn help(root: &Path) -> String {
    let uploads = UPLOADS_DIR.join("/");
    let backends =
        BackendKind::ALL.iter().map(|kind| kind.name()).collect::<Vec<_>>().join(", ");
    format!(
        "Каждая тема этой группы — отдельная сессия агента.\n\n\
         Просто пишите задачу в теме. Команды:\n\
         /new [backend] [путь] — новая сессия в этой теме (сброс контекста)\n\
         /cwd <путь> — сменить рабочую директорию (сброс контекста)\n\
         /reset — начать разговор заново в той же директории\n\
         /stop — прервать текущую задачу\n\
         /status — состояние сессии\n\
         /help — эта справка\n\n\
         Можно прикладывать фото (агент их видит) и файлы (сохраняются в {uploads} в\n\
         рабочей директории). Голосовые сообщения не поддерживаются.\n\n\
         Пути абсолютные или относительно корня: {}\n\
         Бэкенды: {backends}",
        root.display()
    )
}

#[must_use]
pub fn describe(title: &str, session: &TopicSession) -> String {
    format!(
        "{title}\nbackend: {}\ncwd: {}\nsession: {}",
        session.backend.name(),
        session.cwd.as_path().display(),
        session.session.as_ref().map_or("—", |id| id.as_str())
    )
}

#[must_use]
pub fn approval_html(tool: &ToolRequest) -> String {
    let summary = escape(&truncate(&tool.summary, APPROVAL_TEXT_LIMIT));
    format!("🔐 <b>{}</b>\n<pre>{summary}</pre>", escape(&tool.tool))
}

#[must_use]
pub fn tool_call_html(call: &ToolUse) -> String {
    let line = escape(&truncate(&call.summary, TOOL_CALL_TEXT_LIMIT));
    format!("🔧 <b>{}</b> <code>{line}</code>", escape(&call.tool))
}

#[must_use]
pub fn failure(reason: &str) -> String {
    truncate(&format!("❌ {reason}"), FAILURE_TEXT_LIMIT)
}

#[must_use]
pub fn warning(error: &dyn Display) -> String {
    format!("⚠️ {error}")
}

#[must_use]
pub fn verdict(decision: &Decision) -> &'static str {
    match decision {
        Decision::Allowed => "✅ Разрешено",
        Decision::Denied(_) => "❌ Запрещено",
    }
}

#[must_use]
pub fn no_answer(timeout: Duration) -> String {
    format!("Нет ответа пользователя за {} с", timeout.as_secs())
}

#[must_use]
pub fn outside_root(cwd: &AbsolutePath, root: &AbsolutePath) -> String {
    format!(
        "⚠️ Рабочая директория {} вне корня {}. Смените её: /cwd <путь>",
        cwd.as_path().display(),
        root.as_path().display()
    )
}
```

- [ ] **Step 4: Реализовать `inbound.rs`**

```rust
//! Telegram updates reduced to what the hub acts on, independent of the Bot API library.

use hub_core::domain::{ChatId, MessageId, ThreadId, TopicKey, UserId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    pub id: String,
    pub name: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    /// Telegram re-encodes stored photos as JPEG; the model sees them inline.
    Photo(FileRef),
    /// Documents, audio and video land in the session's uploads directory.
    File(FileRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Media { caption: String, attachment: Attachment },
    Voice,
    TopicCreated { name: String },
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub chat: ChatId,
    /// Set only for messages inside a forum topic.
    pub thread: Option<ThreadId>,
    pub user: Option<UserId>,
    pub message: MessageId,
    pub album: Option<String>,
    pub content: Content,
}

impl Inbound {
    #[must_use]
    pub fn key(&self) -> Option<TopicKey> {
        self.thread.map(|thread| TopicKey { chat: self.chat, thread })
    }
}

/// A button press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Press {
    pub callback: String,
    pub user: UserId,
    pub chat: Option<ChatId>,
    pub message: Option<MessageId>,
    /// The pressed message as Telegram HTML, to append a verdict to.
    pub html: Option<String>,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub message: MessageId,
    pub file: FileRef,
}

/// One user turn before its attachments are downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub text: String,
    pub photos: Vec<Upload>,
    pub files: Vec<Upload>,
}

impl Turn {
    #[must_use]
    pub fn text(text: String) -> Self {
        Self { text, photos: Vec::new(), files: Vec::new() }
    }

    /// One turn from a message or an album: captions joined, attachments in message order.
    #[must_use]
    pub fn from_parts(parts: &[Inbound]) -> Self {
        let mut turn = Self::text(String::new());
        let mut texts = Vec::new();
        for part in parts {
            match &part.content {
                Content::Text(text) => texts.push(text.as_str()),
                Content::Media { caption, attachment } => {
                    texts.push(caption.as_str());
                    let upload = |file: &FileRef| Upload { message: part.message, file: file.clone() };
                    match attachment {
                        Attachment::Photo(file) => turn.photos.push(upload(file)),
                        Attachment::File(file) => turn.files.push(upload(file)),
                    }
                }
                Content::Voice | Content::TopicCreated { .. } | Content::Ignored => {}
            }
        }
        turn.text = texts.into_iter().filter(|text| !text.is_empty()).collect::<Vec<_>>().join("\n");
        turn
    }

    #[must_use]
    pub fn has_attachments(&self) -> bool {
        !self.photos.is_empty() || !self.files.is_empty()
    }
}
```

- [ ] **Step 5: Прогнать тесты и линтеры**

Run: `cargo test -p hub-telegram --lib && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-telegram
git commit -m "hub-telegram: тексты бота и входящие апдейты"
```

---

### Task 3: Messenger и политика доставки

**Files:**
- Create: `crates/hub-telegram/src/messenger.rs`, `crates/hub-telegram/src/sender.rs`, `crates/hub-telegram/src/testing.rs`
- Modify: `crates/hub-telegram/src/lib.rs`

**Interfaces:**
- Consumes: `hub_core::questions::Button`, `hub_core::render::{split_message, TELEGRAM_TEXT_LIMIT, truncate}`, `hub_core::markdown::markdown_to_html_chunks`, `hub_core::escape::plain`.
- Produces:
  - `messenger::Format::{Plain, Html}`, `messenger::Outgoing { text, format, keyboard }` + `Outgoing::plain(String)`, `Outgoing::html(String)`, `Outgoing::with_keyboard(Vec<Vec<Button>>)`, `messenger::Target::{Topic(TopicKey), Reply { chat, message }}`, `messenger::SendError::{RetryAfter(Duration), Rejected(String), Failed(String)}`, `messenger::Messenger` (methods `send`, `edit`, `edit_keyboard`, `answer`, `document`, `typing`, `fetch`, `save`, all returning `BoxFuture<'_, Result<_, SendError>>`).
  - `sender::Sender` (`Clone`): `new(Arc<dyn Messenger>)`, `messenger() -> &dyn Messenger`, `text(Target, &str)`, `markdown(TopicKey, &str)`, `one(Target, Outgoing) -> Option<MessageId>`, `edit(ChatId, MessageId, String, Format)`, `edit_keyboard(ChatId, MessageId, Vec<Vec<Button>>)`, `answer(String, Option<&str>)`, `document(TopicKey, PathBuf, &str) -> Result<(), SendError>`, `typing(TopicKey)`.
  - `testing::{FakeMessenger, Call}` (`#[cfg(test)]`, crate-private): records every call; `FakeMessenger::with_sends(Vec<Result<(), SendError>>)` scripts send outcomes; `files` map for `fetch`/`save`; `sent_texts()`, `calls()`.

- [ ] **Step 1: Трейт Messenger**

`crates/hub-telegram/src/messenger.rs`:

```rust
//! The part of the Bot API the hub uses, so everything above it is testable without Telegram.

use std::path::PathBuf;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_core::domain::{ChatId, MessageId, TopicKey};
use hub_core::questions::Button;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Plain,
    Html,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub text: String,
    pub format: Format,
    /// Inline keyboard rows; empty means none.
    pub keyboard: Vec<Vec<Button>>,
}

impl Outgoing {
    #[must_use]
    pub fn plain(text: String) -> Self {
        Self { text, format: Format::Plain, keyboard: Vec::new() }
    }

    #[must_use]
    pub fn html(text: String) -> Self {
        Self { text, format: Format::Html, keyboard: Vec::new() }
    }

    #[must_use]
    pub fn with_keyboard(self, keyboard: Vec<Vec<Button>>) -> Self {
        Self { keyboard, ..self }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Topic(TopicKey),
    /// A reply to a message outside any topic, e.g. in the General topic.
    Reply { chat: ChatId, message: MessageId },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    #[error("Telegram просит подождать {} с", .0.as_secs())]
    RetryAfter(Duration),
    #[error("Telegram отклонил запрос: {0}")]
    Rejected(String),
    #[error("Telegram недоступен: {0}")]
    Failed(String),
}

pub trait Messenger: Send + Sync {
    fn send(&self, target: Target, message: Outgoing) -> BoxFuture<'_, Result<MessageId, SendError>>;
    /// Replaces the text and drops the inline keyboard.
    fn edit(
        &self,
        chat: ChatId,
        message: MessageId,
        text: String,
        format: Format,
    ) -> BoxFuture<'_, Result<(), SendError>>;
    fn edit_keyboard(
        &self,
        chat: ChatId,
        message: MessageId,
        keyboard: Vec<Vec<Button>>,
    ) -> BoxFuture<'_, Result<(), SendError>>;
    fn answer(&self, callback: String, text: Option<String>) -> BoxFuture<'_, Result<(), SendError>>;
    fn document(
        &self,
        key: TopicKey,
        path: PathBuf,
        caption: String,
    ) -> BoxFuture<'_, Result<(), SendError>>;
    fn typing(&self, key: TopicKey) -> BoxFuture<'_, Result<(), SendError>>;
    fn fetch(&self, file: String) -> BoxFuture<'_, Result<Vec<u8>, SendError>>;
    fn save(&self, file: String, target: tokio::fs::File) -> BoxFuture<'_, Result<(), SendError>>;
}
```

- [ ] **Step 2: Поддельный Messenger для тестов**

`crates/hub-telegram/src/testing.rs`:

```rust
//! A recording `Messenger` for tests.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use futures::future::BoxFuture;
use hub_core::domain::{ChatId, MessageId, TopicKey};
use hub_core::questions::Button;
use tokio::io::AsyncWriteExt;

use crate::messenger::{Format, Messenger, Outgoing, SendError, Target};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    Send { target: Target, message: Outgoing, id: MessageId },
    Edit { chat: ChatId, message: MessageId, text: String, format: Format },
    EditKeyboard { chat: ChatId, message: MessageId, keyboard: Vec<Vec<Button>> },
    Answer { callback: String, text: Option<String> },
    Document { key: TopicKey, path: PathBuf, caption: String },
    Typing(TopicKey),
}

#[derive(Default)]
pub(crate) struct FakeMessenger {
    calls: Mutex<Vec<Call>>,
    sends: Mutex<VecDeque<Result<(), SendError>>>,
    pub(crate) files: HashMap<String, Vec<u8>>,
    next: AtomicI32,
}

impl FakeMessenger {
    /// Outcomes of the next sends, in order; afterwards every send succeeds.
    pub(crate) fn with_sends(sends: Vec<Result<(), SendError>>) -> Self {
        Self { sends: Mutex::new(sends.into()), ..Self::default() }
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// Texts of delivered messages, in order.
    pub(crate) fn sent_texts(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Send { message, .. } => Some(message.text),
                _ => None,
            })
            .collect()
    }

    /// The last delivered message that carried a keyboard.
    pub(crate) fn last_keyboard(&self) -> Option<(MessageId, Vec<Vec<Button>>)> {
        self.calls().into_iter().rev().find_map(|call| match call {
            Call::Send { message, id, .. } if !message.keyboard.is_empty() => Some((id, message.keyboard)),
            _ => None,
        })
    }

    fn record(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Messenger for FakeMessenger {
    fn send(&self, target: Target, message: Outgoing) -> BoxFuture<'_, Result<MessageId, SendError>> {
        let scripted = self.sends.lock().unwrap().pop_front().unwrap_or(Ok(()));
        let result = scripted.map(|()| {
            let id = MessageId(self.next.fetch_add(1, Ordering::SeqCst) + 1);
            self.record(Call::Send { target, message, id });
            id
        });
        Box::pin(async move { result })
    }

    fn edit(&self, chat: ChatId, message: MessageId, text: String, format: Format) -> BoxFuture<'_, Result<(), SendError>> {
        self.record(Call::Edit { chat, message, text, format });
        Box::pin(async { Ok(()) })
    }

    fn edit_keyboard(&self, chat: ChatId, message: MessageId, keyboard: Vec<Vec<Button>>) -> BoxFuture<'_, Result<(), SendError>> {
        self.record(Call::EditKeyboard { chat, message, keyboard });
        Box::pin(async { Ok(()) })
    }

    fn answer(&self, callback: String, text: Option<String>) -> BoxFuture<'_, Result<(), SendError>> {
        self.record(Call::Answer { callback, text });
        Box::pin(async { Ok(()) })
    }

    fn document(&self, key: TopicKey, path: PathBuf, caption: String) -> BoxFuture<'_, Result<(), SendError>> {
        self.record(Call::Document { key, path, caption });
        Box::pin(async { Ok(()) })
    }

    fn typing(&self, key: TopicKey) -> BoxFuture<'_, Result<(), SendError>> {
        self.record(Call::Typing(key));
        Box::pin(async { Ok(()) })
    }

    fn fetch(&self, file: String) -> BoxFuture<'_, Result<Vec<u8>, SendError>> {
        let found = self.files.get(&file).cloned().ok_or(SendError::Failed(format!("no file {file}")));
        Box::pin(async move { found })
    }

    fn save(&self, file: String, mut target: tokio::fs::File) -> BoxFuture<'_, Result<(), SendError>> {
        let found = self.files.get(&file).cloned();
        Box::pin(async move {
            let bytes = found.ok_or(SendError::Failed(format!("no file {file}")))?;
            target.write_all(&bytes).await.map_err(|error| SendError::Failed(error.to_string()))?;
            target.flush().await.map_err(|error| SendError::Failed(error.to_string()))
        })
    }
}
```

- [ ] **Step 3: Падающие тесты Sender**

`crates/hub-telegram/src/sender.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use hub_core::domain::{ChatId, ThreadId};

    use super::*;
    use crate::testing::{Call, FakeMessenger};

    const KEY: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };

    fn sender(messenger: FakeMessenger) -> (Sender, Arc<FakeMessenger>) {
        let messenger = Arc::new(messenger);
        (Sender::new(Arc::clone(&messenger) as Arc<dyn Messenger>), messenger)
    }

    #[tokio::test]
    async fn long_text_is_split() {
        let (sender, messenger) = sender(FakeMessenger::default());
        sender.text(Target::Topic(KEY), &format!("{}\n{}", "a".repeat(4000), "b".repeat(200))).await;
        assert_eq!(messenger.sent_texts().len(), 2);
    }

    #[tokio::test]
    async fn markdown_goes_out_as_html_chunks() {
        let (sender, messenger) = sender(FakeMessenger::default());
        sender.markdown(KEY, "**жирный**").await;
        assert_eq!(
            messenger.calls(),
            [Call::Send {
                target: Target::Topic(KEY),
                message: Outgoing::html("<b>жирный</b>".to_owned()),
                id: MessageId(1)
            }]
        );
    }

    #[tokio::test]
    async fn rejected_html_is_resent_as_plain_text() {
        let (sender, messenger) =
            sender(FakeMessenger::with_sends(vec![Err(SendError::Rejected("can't parse entities".to_owned()))]));
        let sent = sender.one(Target::Topic(KEY), Outgoing::html("<b>a &amp; b</b>".to_owned())).await;
        assert!(sent.is_some());
        assert_eq!(messenger.sent_texts(), ["a & b"]);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_is_honoured_once() {
        let (sender, messenger) = sender(FakeMessenger::with_sends(vec![
            Err(SendError::RetryAfter(Duration::from_secs(3))),
        ]));
        assert!(sender.one(Target::Topic(KEY), Outgoing::plain("x".to_owned())).await.is_some());
        assert_eq!(messenger.sent_texts(), ["x"]);

        let (sender, messenger) = sender(FakeMessenger::with_sends(vec![
            Err(SendError::RetryAfter(Duration::from_secs(3))),
            Err(SendError::RetryAfter(Duration::from_secs(3))),
        ]));
        assert!(sender.one(Target::Topic(KEY), Outgoing::plain("x".to_owned())).await.is_none());
        assert!(messenger.sent_texts().is_empty());
    }

    #[tokio::test]
    async fn plain_text_failure_is_given_up() {
        let (sender, messenger) =
            sender(FakeMessenger::with_sends(vec![Err(SendError::Rejected("bad".to_owned()))]));
        assert!(sender.one(Target::Topic(KEY), Outgoing::plain("x".to_owned())).await.is_none());
        assert!(messenger.sent_texts().is_empty());
    }

    #[tokio::test]
    async fn document_caption_is_truncated() {
        let (sender, messenger) = sender(FakeMessenger::default());
        sender.document(KEY, PathBuf::from("r.pdf"), &"я".repeat(2000)).await.unwrap();
        let Some(Call::Document { caption, .. }) = messenger.calls().pop() else { panic!("no document") };
        assert_eq!(caption.chars().count(), 1024);
    }
}
```

`crates/hub-telegram/src/lib.rs`:

```rust
pub mod inbound;
pub mod messenger;
pub mod paths;
pub mod sender;
#[cfg(test)]
mod testing;
pub mod texts;
```

- [ ] **Step 4: Убедиться, что тесты падают**

Run: `cargo test -p hub-telegram --lib sender`
Expected: FAIL — `cannot find type Sender`.

- [ ] **Step 5: Реализовать `sender.rs`**

```rust
//! Delivery policy of the Python version: one retry after a flood wait, plain text when
//! Telegram rejects the markup, and best effort for chat messages so a lost message never
//! aborts the agent.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hub_core::domain::{ChatId, MessageId, TopicKey};
use hub_core::escape::plain;
use hub_core::markdown::markdown_to_html_chunks;
use hub_core::questions::Button;
use hub_core::render::{TELEGRAM_TEXT_LIMIT, split_message, truncate};

use crate::messenger::{Format, Messenger, Outgoing, SendError, Target};

// Telegram's flood waits can be long; the agent should not stall behind one for minutes.
const MAX_RETRY_WAIT: Duration = Duration::from_mins(1);
const CAPTION_LIMIT: usize = 1024;

#[derive(Clone)]
pub struct Sender {
    messenger: Arc<dyn Messenger>,
}

impl Sender {
    #[must_use]
    pub fn new(messenger: Arc<dyn Messenger>) -> Self {
        Self { messenger }
    }

    #[must_use]
    pub fn messenger(&self) -> &dyn Messenger {
        self.messenger.as_ref()
    }

    pub async fn text(&self, target: Target, text: &str) {
        for chunk in split_message(text, TELEGRAM_TEXT_LIMIT) {
            self.one(target, Outgoing::plain(chunk)).await;
        }
    }

    pub async fn markdown(&self, key: TopicKey, markdown: &str) {
        for chunk in markdown_to_html_chunks(markdown, TELEGRAM_TEXT_LIMIT) {
            self.one(Target::Topic(key), Outgoing::html(chunk)).await;
        }
    }

    /// The sent message, or `None` when it was given up (and logged).
    pub async fn one(&self, target: Target, message: Outgoing) -> Option<MessageId> {
        let mut message = message;
        let mut waited = false;
        loop {
            match self.messenger.send(target, message.clone()).await {
                Ok(id) => return Some(id),
                Err(SendError::RetryAfter(wait)) if !waited => {
                    waited = true;
                    tokio::time::sleep(wait.min(MAX_RETRY_WAIT)).await;
                }
                Err(SendError::Rejected(reason)) if message.format == Format::Html => {
                    // A formatting bug must not lose the message: resend it as plain text.
                    tracing::warn!(%reason, "telegram rejected formatting, sending plain text");
                    message = Outgoing { text: plain(&message.text), format: Format::Plain, ..message };
                }
                Err(error) => {
                    tracing::warn!(%error, "telegram message given up");
                    return None;
                }
            }
        }
    }

    pub async fn edit(&self, chat: ChatId, message: MessageId, text: String, format: Format) {
        if let Err(error) = self.messenger.edit(chat, message, text, format).await {
            tracing::warn!(%error, "telegram edit failed");
        }
    }

    pub async fn edit_keyboard(&self, chat: ChatId, message: MessageId, keyboard: Vec<Vec<Button>>) {
        if let Err(error) = self.messenger.edit_keyboard(chat, message, keyboard).await {
            tracing::warn!(%error, "telegram keyboard edit failed");
        }
    }

    pub async fn answer(&self, callback: String, text: Option<&str>) {
        if let Err(error) = self.messenger.answer(callback, text.map(str::to_owned)).await {
            tracing::warn!(%error, "telegram callback answer failed");
        }
    }

    /// Unlike text, a failed file delivery is returned: the agent must learn about it.
    pub async fn document(&self, key: TopicKey, path: PathBuf, caption: &str) -> Result<(), SendError> {
        let caption = truncate(caption, CAPTION_LIMIT);
        match self.messenger.document(key, path.clone(), caption.clone()).await {
            Err(SendError::RetryAfter(wait)) => {
                tokio::time::sleep(wait.min(MAX_RETRY_WAIT)).await;
                self.messenger.document(key, path, caption).await
            }
            other => other,
        }
    }

    pub async fn typing(&self, key: TopicKey) {
        if let Err(error) = self.messenger.typing(key).await {
            tracing::warn!(%error, "telegram chat action failed");
        }
    }
}
```

- [ ] **Step 6: Прогнать тесты и линтеры**

Run: `cargo test -p hub-telegram --lib && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/hub-telegram
git commit -m "hub-telegram: Messenger и политика доставки сообщений"
```

---

### Task 4: Канал пользователя и кнопки

**Files:**
- Create: `crates/hub-telegram/src/channel.rs`, `crates/hub-telegram/src/press.rs`
- Modify: `crates/hub-telegram/src/lib.rs`

**Interfaces:**
- Consumes: `hub_claude::channel::UserChannel`, `hub_core::approvals::*`, `hub_core::questions::*`, `paths::outgoing_path`, `sender::Sender`, `texts::*`.
- Produces:
  - `channel::Registries { approvals: ApprovalRegistry<oneshot::Sender<Decision>>, questions: QuestionRegistry<oneshot::Sender<Answer>> }` (`Default`), `channel::Shared = Arc<Mutex<Registries>>`, `channel::lock(&Shared) -> MutexGuard<'_, Registries>`, `channel::TelegramChannel::new(Sender, TopicKey, AbsolutePath, PathBuf, Shared, Duration)` implementing `UserChannel`.
  - `press::handle(Shared, Sender, Press)` (async): resolves approvals and questions from button data.

- [ ] **Step 1: Падающие тесты**

`crates/hub-telegram/src/channel.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use hub_core::domain::{ChatId, Denied, QuestionOption, Selection, ThreadId, UserId};

    use super::*;
    use crate::inbound::Press;
    use crate::messenger::Messenger;
    use crate::paths::workspace_root;
    use crate::press;
    use crate::testing::{Call, FakeMessenger};

    const KEY: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };

    struct Setup {
        channel: Arc<TelegramChannel>,
        messenger: Arc<FakeMessenger>,
        sender: Sender,
        registries: Shared,
        _dir: tempfile::TempDir,
    }

    fn setup(timeout: Duration) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let cwd = workspace_root(dir.path()).unwrap();
        let messenger = Arc::new(FakeMessenger::default());
        let sender = Sender::new(Arc::clone(&messenger) as Arc<dyn Messenger>);
        let registries = Shared::default();
        let channel = Arc::new(TelegramChannel::new(
            sender.clone(),
            KEY,
            cwd,
            std::env::temp_dir(),
            Arc::clone(&registries),
            timeout,
        ));
        Setup { channel, messenger, sender, registries, _dir: dir }
    }

    async fn keyboard(messenger: &FakeMessenger) -> (MessageId, Vec<Vec<Button>>) {
        for _ in 0..200 {
            if let Some(found) = messenger.last_keyboard() {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("no keyboard was sent");
    }

    fn press(data: &str, message: MessageId) -> Press {
        Press {
            callback: "cb".to_owned(),
            user: UserId(1),
            chat: Some(KEY.chat),
            message: Some(message),
            html: Some("🔐 <b>Bash</b>".to_owned()),
            data: data.to_owned(),
        }
    }

    fn button(rows: &[Vec<Button>], row: usize, column: usize) -> String {
        rows[row][column].data.clone()
    }

    fn question(selection: Selection) -> Question {
        Question::new(
            "Цвет?".to_owned(),
            "Цвет".to_owned(),
            vec![
                QuestionOption { label: "Красный".to_owned(), description: String::new() },
                QuestionOption { label: "Синий".to_owned(), description: String::new() },
            ],
            selection,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn approval_is_asked_with_buttons_and_resolved_by_a_press() {
        let setup = setup(Duration::from_secs(5));
        let channel = Arc::clone(&setup.channel);
        let request = tokio::spawn(async move {
            channel.request(ToolRequest { tool: "Bash".to_owned(), summary: "ls".to_owned() }).await
        });

        let (message, rows) = keyboard(&setup.messenger).await;
        assert_eq!(rows[0].iter().map(|b| b.text.as_str()).collect::<Vec<_>>(), ["✅ Разрешить", "❌ Запретить"]);
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), press(&button(&rows, 0, 0), message)).await;

        assert_eq!(request.await.unwrap(), Decision::Allowed);
        assert!(setup.messenger.calls().contains(&Call::Edit {
            chat: KEY.chat,
            message,
            text: "🔐 <b>Bash</b>\n\n✅ Разрешено".to_owned(),
            format: Format::Html,
        }));
    }

    #[tokio::test]
    async fn unanswered_approval_is_denied_after_the_timeout() {
        let setup = setup(Duration::from_millis(50));
        let decision = setup.channel.request(ToolRequest { tool: "Bash".to_owned(), summary: "ls".to_owned() }).await;
        assert_eq!(decision, Decision::Denied(Denied::new("Нет ответа пользователя за 0 с")));
        assert!(setup.messenger.calls().iter().any(|call| matches!(call,
            Call::Edit { text, .. } if text.ends_with("⌛ Нет ответа — запрещено"))));
    }

    #[tokio::test]
    async fn stale_approval_press_is_answered_as_stale() {
        let setup = setup(Duration::from_millis(50));
        let _ = setup.channel.request(ToolRequest { tool: "Bash".to_owned(), summary: "ls".to_owned() }).await;
        let (message, rows) = keyboard(&setup.messenger).await;
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), press(&button(&rows, 0, 0), message)).await;
        assert!(setup.messenger.calls().contains(&Call::Answer {
            callback: "cb".to_owned(),
            text: Some("Запрос уже неактуален".to_owned())
        }));
    }

    #[tokio::test]
    async fn dropped_request_closes_its_approval() {
        let setup = setup(Duration::from_secs(5));
        let channel = Arc::clone(&setup.channel);
        let request = tokio::spawn(async move {
            channel.request(ToolRequest { tool: "Bash".to_owned(), summary: "ls".to_owned() }).await
        });
        let (message, rows) = keyboard(&setup.messenger).await;
        request.abort();
        let _ = request.await;
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), press(&button(&rows, 0, 0), message)).await;
        assert!(setup.messenger.calls().contains(&Call::Answer {
            callback: "cb".to_owned(),
            text: Some("Запрос уже неактуален".to_owned())
        }));
    }

    #[tokio::test]
    async fn single_choice_question_is_answered_by_a_press() {
        let setup = setup(Duration::from_secs(5));
        let channel = Arc::clone(&setup.channel);
        let asked = tokio::spawn(async move { channel.ask(vec![question(Selection::Single)]).await });

        let (message, rows) = keyboard(&setup.messenger).await;
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), press(&button(&rows, 1, 0), message)).await;

        assert_eq!(
            asked.await.unwrap(),
            QuestionsOutcome::Answered(vec![QuestionAnswer { question: "Цвет?".to_owned(), answer: "Синий".to_owned() }])
        );
        assert!(setup.messenger.calls().iter().any(|call| matches!(call,
            Call::Edit { text, .. } if text.ends_with("💬 Синий"))));
    }

    #[tokio::test]
    async fn multi_choice_toggles_redraw_the_keyboard() {
        let setup = setup(Duration::from_secs(5));
        let channel = Arc::clone(&setup.channel);
        let asked = tokio::spawn(async move { channel.ask(vec![question(Selection::Multiple)]).await });

        let (message, rows) = keyboard(&setup.messenger).await;
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), press(&button(&rows, 0, 0), message)).await;
        assert!(setup.messenger.calls().iter().any(|call| matches!(call,
            Call::EditKeyboard { keyboard, .. } if keyboard[0][0].text == "☑ Красный")));
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), press(&button(&rows, 2, 0), message)).await;

        assert_eq!(
            asked.await.unwrap(),
            QuestionsOutcome::Answered(vec![QuestionAnswer { question: "Цвет?".to_owned(), answer: "Красный".to_owned() }])
        );
    }

    #[tokio::test]
    async fn declined_question_denies() {
        let setup = setup(Duration::from_secs(5));
        let channel = Arc::clone(&setup.channel);
        let asked = tokio::spawn(async move { channel.ask(vec![question(Selection::Single)]).await });
        let (message, rows) = keyboard(&setup.messenger).await;
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), press(&button(&rows, 2, 0), message)).await;
        assert!(matches!(asked.await.unwrap(), QuestionsOutcome::Denied(_)));
    }

    #[tokio::test]
    async fn file_inside_cwd_is_sent_and_outside_is_refused() {
        let setup = setup(Duration::from_secs(5));
        let cwd = setup._dir.path();
        fs::write(cwd.join("r.pdf"), "%PDF").unwrap();

        let sent = setup.channel.send_file(OutgoingFile { path: "r.pdf".to_owned(), caption: "Отчёт".to_owned() }).await;
        assert_eq!(sent, FileDelivery::Delivered);
        assert!(setup.messenger.calls().iter().any(|call| matches!(call,
            Call::Document { caption, .. } if caption == "Отчёт")));

        let refused = setup.channel.send_file(OutgoingFile { path: "../x".to_owned(), caption: String::new() }).await;
        assert!(matches!(refused, FileDelivery::Denied(_)));
    }

    #[tokio::test]
    async fn unknown_button_is_answered_as_stale() {
        let setup = setup(Duration::from_secs(5));
        press::handle(Arc::clone(&setup.registries), setup.sender.clone(), press("zz:1", MessageId(9))).await;
        assert!(setup.messenger.calls().contains(&Call::Answer {
            callback: "cb".to_owned(),
            text: Some("Вопрос уже неактуален".to_owned())
        }));
    }
}
```

`crates/hub-telegram/src/lib.rs` — добавить `pub mod channel;` и `pub mod press;`; `crates/hub-telegram/src/press.rs` пока пустой файл с комментарием модуля:

```rust
//! Button presses: approvals and question answers coming back from the chat.
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-telegram --lib channel`
Expected: FAIL — `cannot find type TelegramChannel`.

- [ ] **Step 3: Реализовать `channel.rs`**

```rust
//! The human behind a topic: approvals and questions as messages with buttons, files as
//! documents. Waiting happens in the session task; button presses resolve it through the
//! shared registries.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures::future::BoxFuture;
use hub_claude::channel::UserChannel;
use hub_core::approvals::{ApprovalId, ApprovalRegistry, Verdict, callback_data};
use hub_core::attachments::MAX_SEND_BYTES;
use hub_core::domain::{
    AbsolutePath, Decision, Denied, FileDelivery, MessageId, OutgoingFile, Question,
    QuestionAnswer, QuestionsOutcome, ToolRequest, TopicKey,
};
use hub_core::questions::{self, Answer, Button, QuestionId, QuestionRegistry};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::messenger::{Format, Outgoing, Target};
use crate::paths::outgoing_path;
use crate::sender::Sender;
use crate::texts;

#[derive(Default)]
pub struct Registries {
    pub approvals: ApprovalRegistry<oneshot::Sender<Decision>>,
    pub questions: QuestionRegistry<oneshot::Sender<Answer>>,
}

pub type Shared = Arc<Mutex<Registries>>;

/// Registries stay usable after a panic elsewhere: their state is a plain map, never half-updated.
pub fn lock(registries: &Shared) -> MutexGuard<'_, Registries> {
    registries.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn random_id() -> [u8; 8] {
    Uuid::new_v4().as_u64_pair().0.to_le_bytes()
}

pub struct TelegramChannel {
    sender: Sender,
    key: TopicKey,
    cwd: AbsolutePath,
    home: PathBuf,
    registries: Shared,
    timeout: Duration,
}

impl TelegramChannel {
    #[must_use]
    pub fn new(
        sender: Sender,
        key: TopicKey,
        cwd: AbsolutePath,
        home: PathBuf,
        registries: Shared,
        timeout: Duration,
    ) -> Self {
        Self { sender, key, cwd, home, registries, timeout }
    }

    async fn approval(&self, tool: ToolRequest) -> Decision {
        let id = ApprovalId::from_random(random_id());
        let (responder, decision) = oneshot::channel();
        lock(&self.registries).approvals.open(id.clone(), responder);
        let _open = OpenApproval { registries: &self.registries, id: id.clone() };
        let text = texts::approval_html(&tool);
        let keyboard = vec![vec![
            Button { text: "✅ Разрешить".to_owned(), data: callback_data(&id, Verdict::Allow) },
            Button { text: "❌ Запретить".to_owned(), data: callback_data(&id, Verdict::Deny) },
        ]];
        let outgoing = Outgoing::html(text.clone()).with_keyboard(keyboard);
        let Some(message) = self.sender.one(Target::Topic(self.key), outgoing).await else {
            return Decision::Denied(Denied::new(texts::APPROVAL_UNSENT));
        };
        match tokio::time::timeout(self.timeout, decision).await {
            Ok(Ok(decision)) => decision,
            Ok(Err(_)) => Decision::Denied(Denied::new(texts::no_answer(self.timeout))),
            Err(_) => {
                let expired = format!("{text}\n\n⌛ Нет ответа — запрещено");
                self.sender.edit(self.key.chat, message, expired, Format::Html).await;
                Decision::Denied(Denied::new(texts::no_answer(self.timeout)))
            }
        }
    }

    async fn questions(&self, questions: Vec<Question>) -> QuestionsOutcome {
        let mut answers = Vec::new();
        for question in questions {
            match self.question(&question).await {
                Answer::Text(answer) => {
                    answers.push(QuestionAnswer { question: question.text().to_owned(), answer });
                }
                Answer::Declined(denied) => return QuestionsOutcome::Denied(denied),
            }
        }
        QuestionsOutcome::Answered(answers)
    }

    async fn question(&self, question: &Question) -> Answer {
        let id = QuestionId::from_random(random_id());
        let (responder, answer) = oneshot::channel();
        lock(&self.registries).questions.open(id.clone(), self.key, question.clone(), responder);
        let _open = OpenQuestion { registries: &self.registries, id: id.clone() };
        let text = questions::question_html(question);
        let keyboard = questions::keyboard(&id, question, &BTreeSet::new());
        let outgoing = Outgoing::html(text.clone()).with_keyboard(keyboard);
        let Some(message) = self.sender.one(Target::Topic(self.key), outgoing).await else {
            return Answer::Declined(Denied::new(texts::QUESTION_UNSENT));
        };
        match tokio::time::timeout(self.timeout, answer).await {
            Ok(Ok(answer)) => {
                let shown = format!("{text}\n\n{}", questions::answer_line(&answer));
                self.sender.edit(self.key.chat, message, shown, Format::Html).await;
                answer
            }
            Ok(Err(_)) => Answer::Declined(Denied::new(texts::no_answer(self.timeout))),
            Err(_) => {
                self.edit_expired(message, &text).await;
                Answer::Declined(Denied::new(texts::no_answer(self.timeout)))
            }
        }
    }

    async fn edit_expired(&self, message: MessageId, text: &str) {
        let expired = format!("{text}\n\n⌛ Нет ответа");
        self.sender.edit(self.key.chat, message, expired, Format::Html).await;
    }

    async fn file(&self, file: OutgoingFile) -> FileDelivery {
        let cwd = self.cwd.clone();
        let home = self.home.clone();
        let raw = file.path.clone();
        let checked = tokio::task::spawn_blocking(move || outgoing_path(&cwd, &home, &raw, MAX_SEND_BYTES)).await;
        let path = match checked {
            Ok(Ok(path)) => path,
            Ok(Err(error)) => return FileDelivery::Denied(Denied::new(error.to_string())),
            Err(error) => return FileDelivery::Denied(Denied::new(format!("Проверка файла прервана: {error}"))),
        };
        match self.sender.document(self.key, path.clone(), &file.caption).await {
            Ok(()) => {
                tracing::info!(file = %path.display(), "file sent");
                FileDelivery::Delivered
            }
            Err(error) => {
                tracing::warn!(%error, "file delivery failed");
                FileDelivery::Denied(Denied::new(format!("Telegram не принял файл: {error}")))
            }
        }
    }
}

impl UserChannel for TelegramChannel {
    fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision> {
        Box::pin(self.approval(tool))
    }

    fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        Box::pin(self.questions(questions))
    }

    fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(self.file(file))
    }
}

/// Closes the approval however the wait ends, including the session being stopped.
struct OpenApproval<'a> {
    registries: &'a Shared,
    id: ApprovalId,
}

impl Drop for OpenApproval<'_> {
    fn drop(&mut self) {
        lock(self.registries).approvals.close(&self.id);
    }
}

struct OpenQuestion<'a> {
    registries: &'a Shared,
    id: QuestionId,
}

impl Drop for OpenQuestion<'_> {
    fn drop(&mut self) {
        lock(self.registries).questions.close(&self.id);
    }
}
```

Ожидание в `approval` упростить через `edit_expired` нельзя (другой текст) — оставить как есть.

- [ ] **Step 4: Реализовать `press.rs`**

```rust
//! Button presses: approvals and question answers coming back from the chat.

use hub_core::approvals;
use hub_core::questions::{self, Press as Pressed};

use crate::channel::{Shared, lock};
use crate::inbound::Press;
use crate::messenger::Format;
use crate::sender::Sender;
use crate::texts;

pub async fn handle(registries: Shared, sender: Sender, press: Press) {
    if let Some(answer) = approvals::parse_callback_data(&press.data) {
        let resolved = lock(&registries).approvals.resolve(&answer);
        let delivered = resolved.and_then(|(responder, decision)| {
            responder.send(decision.clone()).is_ok().then_some(decision)
        });
        match delivered {
            Some(decision) => {
                sender.answer(press.callback, None).await;
                if let (Some(chat), Some(message), Some(html)) = (press.chat, press.message, press.html) {
                    let text = format!("{html}\n\n{}", texts::verdict(&decision));
                    sender.edit(chat, message, text, Format::Html).await;
                }
            }
            None => sender.answer(press.callback, Some(texts::APPROVAL_STALE)).await,
        }
        return;
    }
    let Some(pressed) = questions::parse_callback_data(&press.data) else {
        sender.answer(press.callback, Some(texts::QUESTION_STALE)).await;
        return;
    };
    let result = lock(&registries).questions.press(&pressed);
    match result {
        Pressed::Accepted { responder, answer } => {
            // The asking channel edits the message once it has the answer.
            let stale = responder.send(answer).is_err().then_some(texts::QUESTION_STALE);
            sender.answer(press.callback, stale).await;
        }
        Pressed::SelectionChanged { question, selected } => {
            sender.answer(press.callback, None).await;
            if let (Some(chat), Some(message)) = (press.chat, press.message) {
                let keyboard = questions::keyboard(&pressed.id, &question, &selected);
                sender.edit_keyboard(chat, message, keyboard).await;
            }
        }
        Pressed::NothingSelected => sender.answer(press.callback, Some(texts::NOTHING_SELECTED)).await,
        Pressed::Stale => sender.answer(press.callback, Some(texts::QUESTION_STALE)).await,
    }
}
```

- [ ] **Step 5: Прогнать тесты и линтеры**

Run: `cargo test -p hub-telegram --lib && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS. Тест `unanswered_approval_is_denied_after_the_timeout` ожидает «за 0 с» — `no_answer` печатает целые секунды таймаута (50 мс → 0); это осознанно, в бою таймаут ≥ 1 с.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-telegram
git commit -m "hub-telegram: подтверждения, вопросы и send_file через Telegram"
```

---

### Task 5: Скачивание вложений

**Files:**
- Create: `crates/hub-telegram/src/download.rs`
- Modify: `crates/hub-telegram/src/lib.rs` (`pub mod download;`)

**Interfaces:**
- Consumes: `inbound::Turn`, `sender::Sender`, `paths::{prepare_upload, UploadError}`, `hub_core::attachments::{upload_path, prompt_text, MAX_DOWNLOAD_BYTES}`.
- Produces: `download::DownloadError::{TooLarge { name }, Fetch(SendError), Store(UploadError), Interrupted, Empty}`, `download::download(&Sender, &AbsolutePath, Turn) -> Result<Prompt, DownloadError>` (async).

- [ ] **Step 1: Падающие тесты**

`crates/hub-telegram/src/download.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use hub_core::domain::{ImageMediaType, MessageId};

    use super::*;
    use crate::inbound::{FileRef, Upload};
    use crate::messenger::Messenger;
    use crate::paths::workspace_root;
    use crate::testing::FakeMessenger;

    fn upload(message: i32, id: &str, name: Option<&str>, size: u64) -> Upload {
        Upload { message: MessageId(message), file: FileRef { id: id.to_owned(), name: name.map(str::to_owned), size: Some(size) } }
    }

    fn sender_with(files: &[(&str, &[u8])]) -> Sender {
        let mut messenger = FakeMessenger::default();
        for (id, bytes) in files {
            messenger.files.insert((*id).to_owned(), bytes.to_vec());
        }
        Sender::new(Arc::new(messenger) as Arc<dyn Messenger>)
    }

    #[tokio::test]
    async fn photos_become_images_and_files_land_in_uploads() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = workspace_root(dir.path()).unwrap();
        let sender = sender_with(&[("p", b"\xff\xd8"), ("d", b"%PDF")]);
        let turn = Turn {
            text: "сравни".to_owned(),
            photos: vec![upload(1, "p", None, 2)],
            files: vec![upload(2, "d", Some("отчёт.pdf"), 4)],
        };

        let prompt = download(&sender, &cwd, turn).await.unwrap();

        assert_eq!(prompt.images().iter().map(|i| (i.media, i.data.clone())).collect::<Vec<_>>(),
                   [(ImageMediaType::Jpeg, vec![0xff, 0xd8])]);
        let saved = cwd.as_path().join(".agent-hub").join("uploads").join("2-отчёт.pdf");
        assert_eq!(std::fs::read(&saved).unwrap(), b"%PDF");
        assert!(prompt.text().starts_with("сравни\n\nПриложенные файлы:\n- "));
        assert!(prompt.text().ends_with("2-отчёт.pdf"));
    }

    #[tokio::test]
    async fn oversized_attachment_is_refused_before_download() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = workspace_root(dir.path()).unwrap();
        let turn = Turn { text: String::new(), photos: Vec::new(), files: vec![upload(1, "d", Some("big.zip"), 21 * 1024 * 1024)] };
        let error = download(&sender_with(&[]), &cwd, turn).await.unwrap_err();
        assert_eq!(error.to_string(), "big.zip: больше 20 МБ, Telegram не отдаёт боту такие файлы");
    }

    #[tokio::test]
    async fn failed_fetch_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = workspace_root(dir.path()).unwrap();
        let turn = Turn { text: String::new(), photos: vec![upload(1, "gone", None, 2)], files: Vec::new() };
        let error = download(&sender_with(&[]), &cwd, turn).await.unwrap_err();
        assert!(error.to_string().starts_with("Не удалось скачать вложение"));
    }
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-telegram --lib download`
Expected: FAIL — `cannot find function download`.

- [ ] **Step 3: Реализовать `download.rs`**

```rust
//! Attachments of a turn: photos inline for the model, other files into the session's
//! uploads directory.

use hub_core::attachments::{MAX_DOWNLOAD_BYTES, prompt_text, upload_path};
use hub_core::domain::{AbsolutePath, Image, ImageMediaType, Prompt};

use crate::inbound::Turn;
use crate::messenger::SendError;
use crate::paths::{UploadError, prepare_upload};
use crate::sender::Sender;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("{name}: больше 20 МБ, Telegram не отдаёт боту такие файлы")]
    TooLarge { name: String },
    #[error("Не удалось скачать вложение: {0}")]
    Fetch(SendError),
    #[error(transparent)]
    Store(#[from] UploadError),
    #[error("Сохранение вложения прервано")]
    Interrupted,
    #[error("В сообщении нет ни текста, ни вложений")]
    Empty,
}

pub async fn download(sender: &Sender, cwd: &AbsolutePath, turn: Turn) -> Result<Prompt, DownloadError> {
    let Turn { text, photos, files } = turn;
    if let Some(oversized) = photos
        .iter()
        .chain(&files)
        .find(|upload| upload.file.size.is_some_and(|size| size > MAX_DOWNLOAD_BYTES))
    {
        let name = oversized.file.name.clone().unwrap_or_else(|| "фото".to_owned());
        return Err(DownloadError::TooLarge { name });
    }
    let mut images = Vec::with_capacity(photos.len());
    for photo in photos {
        let data = sender.messenger().fetch(photo.file.id).await.map_err(DownloadError::Fetch)?;
        // Telegram re-encodes every photo it stores as JPEG.
        images.push(Image { media: ImageMediaType::Jpeg, data });
    }
    let mut paths = Vec::with_capacity(files.len());
    for upload in files {
        let target = upload_path(cwd.as_path(), upload.message, upload.file.name.as_deref());
        let (root, checked) = (cwd.clone(), target.clone());
        tokio::task::spawn_blocking(move || prepare_upload(&root, &checked))
            .await
            .map_err(|_| DownloadError::Interrupted)??;
        // `create_new` refuses an entry planted between the check and the write.
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .await
            .map_err(|error| DownloadError::Store(UploadError::Io(error)))?;
        sender.messenger().save(upload.file.id, file).await.map_err(DownloadError::Fetch)?;
        paths.push(target);
    }
    Prompt::new(prompt_text(&text, &paths), images).ok_or(DownloadError::Empty)
}
```

- [ ] **Step 4: Прогнать тесты и линтеры**

Run: `cargo test -p hub-telegram --lib && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hub-telegram
git commit -m "hub-telegram: скачивание вложений в рабочую директорию"
```

---

### Task 6: Актор Hub

**Files:**
- Create: `crates/hub-telegram/src/hub.rs`, `crates/hub-telegram/src/session.rs`
- Modify: `crates/hub-telegram/src/lib.rs` (`pub mod hub; mod session;`)

**Interfaces:**
- Consumes: всё из Task 1–5; `hub_claude::session::{Conversation, Limits}`; `hub_core::commands::*`, `hub_core::topics::Topics`, `hub_core::settings::Settings`, `hub_core::render::{format_finished, format_abandoned}`.
- Produces:
  - `hub::Agents` — `fn run<'a>(&'a self, session: &'a TopicSession, prompt: Prompt, inbox: mpsc::Receiver<Prompt>, conversation: Conversation) -> BoxFuture<'a, mpsc::Receiver<Prompt>>`; `Send + Sync + 'static`.
  - `hub::TopicStore` — `fn save(&mut self, topics: &Topics) -> Result<(), String>`; `Send + 'static`.
  - `hub::TopicState::{Waiting, Running}`, `hub::TopicView { key, title, session, state }`.
  - `hub::HubMessage::{Inbound(Inbound), Press(Press), Deliver { key, prompt }, FlushAlbum(String), Bind { key, session }, Ended { key, generation, inbox }, Stop(TopicKey), Reset(TopicKey), Shutdown(oneshot::Sender<()>)}`.
  - `hub::HubSetup<A> { agents, messenger, settings, topics, store, home, bot }`, `hub::HubHandle { mailbox, views, task }`, `hub::spawn<A: Agents>(HubSetup<A>) -> HubHandle`.

- [ ] **Step 1: Падающие тесты**

`crates/hub-telegram/src/hub.rs` (тестовый модуль; реализация — Step 3):

```rust
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use hub_core::domain::{ChatId, Finished, ThreadId, UserId};
    use hub_core::settings::Draft;

    use super::*;
    use crate::inbound::{Attachment, Content, FileRef};
    use crate::testing::FakeMessenger;

    const KEY: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };

    enum Script {
        Say(Vec<AgentEvent>),
        UntilStopped,
        Collect(usize),
    }

    #[derive(Default)]
    struct FakeAgents {
        scripts: StdMutex<VecDeque<Script>>,
        prompts: StdMutex<Vec<String>>,
        sessions: StdMutex<Vec<TopicSession>>,
    }

    impl FakeAgents {
        fn scripted(scripts: Vec<Script>) -> Self {
            Self { scripts: StdMutex::new(scripts.into()), ..Self::default() }
        }
    }

    impl Agents for FakeAgents {
        fn run<'a>(
            &'a self,
            session: &'a TopicSession,
            prompt: Prompt,
            mut inbox: mpsc::Receiver<Prompt>,
            conversation: Conversation,
        ) -> BoxFuture<'a, mpsc::Receiver<Prompt>> {
            self.prompts.lock().unwrap().push(prompt.text().to_owned());
            self.sessions.lock().unwrap().push(session.clone());
            let script = self.scripts.lock().unwrap().pop_front().unwrap_or(Script::Say(Vec::new()));
            Box::pin(async move {
                match script {
                    Script::Say(events) => {
                        for event in events {
                            conversation.events.send(event).await.unwrap();
                        }
                    }
                    Script::UntilStopped => conversation.cancel.cancelled().await,
                    Script::Collect(count) => {
                        for _ in 0..count {
                            let next = inbox.recv().await.unwrap();
                            self.prompts.lock().unwrap().push(next.text().to_owned());
                        }
                    }
                }
                inbox
            })
        }
    }

    #[derive(Default, Clone)]
    struct MemoryStore(Arc<StdMutex<Vec<Topics>>>);

    impl TopicStore for MemoryStore {
        fn save(&mut self, topics: &Topics) -> Result<(), String> {
            self.0.lock().unwrap().push(topics.clone());
            Ok(())
        }
    }

    struct World {
        handle: HubHandle,
        messenger: Arc<FakeMessenger>,
        agents: Arc<FakeAgents>,
        store: MemoryStore,
        root: tempfile::TempDir,
    }

    fn world(agents: FakeAgents) -> World {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("project")).unwrap();
        let settings = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.path().display().to_string(),
            approval_timeout: "5".to_owned(),
            ..Draft::default()
        }
        .parse(&std::env::temp_dir())
        .unwrap();
        let (_settings_tx, settings) = watch::channel(Arc::new(settings));
        std::mem::forget(_settings_tx);
        let messenger = Arc::new(FakeMessenger::default());
        let agents = Arc::new(agents);
        let store = MemoryStore::default();
        let handle = spawn(HubSetup {
            agents: Arc::clone(&agents),
            messenger: Arc::clone(&messenger) as Arc<dyn Messenger>,
            settings,
            topics: Topics::new(),
            store: Box::new(store.clone()),
            home: std::env::temp_dir(),
            bot: "agent_hub_bot".to_owned(),
        });
        World { handle, messenger, agents, store, root }
    }

    fn text(thread: Option<i32>, message: i32, text: &str) -> HubMessage {
        HubMessage::Inbound(Inbound {
            chat: ChatId(-100),
            thread: thread.map(ThreadId),
            user: Some(UserId(1)),
            message: MessageId(message),
            album: None,
            content: Content::Text(text.to_owned()),
        })
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

    fn finished() -> AgentEvent {
        AgentEvent::Finished(Finished {
            session: SessionId::parse("s-1").unwrap(),
            turns: 1,
            cost: None,
            background: 0,
        })
    }

    fn texts_of(world: &World) -> Vec<String> {
        world.messenger.sent_texts()
    }

    #[tokio::test]
    async fn new_topic_binds_a_session_at_the_root() {
        let world = world(FakeAgents::default());
        world.handle.mailbox.send(HubMessage::Inbound(Inbound {
            chat: ChatId(-100), thread: Some(ThreadId(7)), user: Some(UserId(1)), message: MessageId(1),
            album: None, content: Content::TopicCreated { name: "backend".to_owned() },
        })).await.unwrap();

        eventually("greeting", || texts_of(&world).iter().any(|t| t.starts_with("🆕 Новая сессия"))).await;
        assert_eq!(world.store.0.lock().unwrap().last().map(Topics::len), Some(1));
        eventually("view", || world.handle.views.borrow().first().is_some_and(|v| v.title.as_deref() == Some("backend"))).await;
    }

    #[tokio::test]
    async fn message_runs_the_agent_and_relays_its_events() {
        let world = world(FakeAgents::scripted(vec![Script::Say(vec![
            AgentEvent::SessionStarted(SessionId::parse("s-1").unwrap()),
            AgentEvent::AssistantText("**готово**".to_owned()),
            finished(),
        ])]));
        world.handle.mailbox.send(text(Some(7), 1, "сделай")).await.unwrap();

        eventually("finished", || texts_of(&world).iter().any(|t| t == "✅ Готово · ходов: 1")).await;
        assert!(texts_of(&world).contains(&"<b>готово</b>".to_owned()));
        assert_eq!(*world.agents.prompts.lock().unwrap(), ["сделай"]);
        eventually("bound", || world.store.0.lock().unwrap().last()
            .and_then(|topics| topics.get(&KEY).cloned())
            .is_some_and(|session| session.session == SessionId::parse("s-1"))).await;
    }

    #[tokio::test]
    async fn general_topic_asks_for_a_topic() {
        let world = world(FakeAgents::default());
        world.handle.mailbox.send(text(None, 3, "привет")).await.unwrap();
        eventually("hint", || texts_of(&world) == [texts::CREATE_TOPIC]).await;
        assert!(world.messenger.calls().iter().any(|call| matches!(call,
            Call::Send { target: Target::Reply { message: MessageId(3), .. }, .. })));
    }

    #[tokio::test]
    async fn stop_interrupts_the_running_agent() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.handle.mailbox.send(text(Some(7), 1, "долго")).await.unwrap();
        eventually("running", || world.handle.views.borrow().iter().any(|v| v.state == TopicState::Running)).await;

        world.handle.mailbox.send(text(Some(7), 2, "/stop")).await.unwrap();
        eventually("stopped", || texts_of(&world).contains(&texts::STOPPED.to_owned())).await;
        eventually("waiting", || world.handle.views.borrow().iter().all(|v| v.state == TopicState::Waiting)).await;
    }

    #[tokio::test]
    async fn commands_are_refused_while_running() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.handle.mailbox.send(text(Some(7), 1, "долго")).await.unwrap();
        eventually("running", || world.handle.views.borrow().iter().any(|v| v.state == TopicState::Running)).await;
        world.handle.mailbox.send(text(Some(7), 2, "/new project")).await.unwrap();
        eventually("refused", || texts_of(&world).contains(&texts::ALREADY_RUNNING.to_owned())).await;
        world.handle.mailbox.send(HubMessage::Stop(KEY)).await.unwrap();
    }

    #[tokio::test]
    async fn new_and_cwd_resolve_inside_the_root_only() {
        let world = world(FakeAgents::default());
        world.handle.mailbox.send(text(Some(7), 1, "/new project")).await.unwrap();
        eventually("new", || texts_of(&world).iter().any(|t| t.starts_with("🆕 Новая сессия") && t.contains("project"))).await;

        world.handle.mailbox.send(text(Some(7), 2, "/cwd ../..")).await.unwrap();
        eventually("refused", || texts_of(&world).iter().any(|t| t.starts_with("⚠️"))).await;

        world.handle.mailbox.send(text(Some(7), 3, "/cwd")).await.unwrap();
        eventually("usage", || texts_of(&world).contains(&texts::CWD_USAGE.to_owned())).await;
    }

    #[tokio::test]
    async fn status_and_reset_describe_the_session() {
        let world = world(FakeAgents::default());
        world.handle.mailbox.send(text(Some(7), 1, "/status")).await.unwrap();
        eventually("no session", || texts_of(&world).contains(&texts::NO_SESSION.to_owned())).await;
        world.handle.mailbox.send(text(Some(7), 2, "/reset")).await.unwrap();
        eventually("reset", || texts_of(&world).iter().any(|t| t.starts_with("🔄 Контекст сброшен"))).await;
        world.handle.mailbox.send(text(Some(7), 3, "/status")).await.unwrap();
        eventually("waiting", || texts_of(&world).iter().any(|t| t.starts_with("💤 ожидает"))).await;
    }

    #[tokio::test]
    async fn messages_during_a_run_go_to_the_same_session() {
        let world = world(FakeAgents::scripted(vec![Script::Collect(1)]));
        world.handle.mailbox.send(text(Some(7), 1, "первое")).await.unwrap();
        eventually("running", || world.handle.views.borrow().iter().any(|v| v.state == TopicState::Running)).await;
        world.handle.mailbox.send(text(Some(7), 2, "второе")).await.unwrap();
        eventually("collected", || *world.agents.prompts.lock().unwrap() == ["первое", "второе"]).await;
    }

    #[tokio::test]
    async fn leftover_prompt_starts_a_new_session() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped, Script::Say(vec![finished()])]));
        world.handle.mailbox.send(text(Some(7), 1, "первое")).await.unwrap();
        eventually("running", || world.handle.views.borrow().iter().any(|v| v.state == TopicState::Running)).await;
        world.handle.mailbox.send(HubMessage::Stop(KEY)).await.unwrap();
        world.handle.mailbox.send(text(Some(7), 2, "после стопа")).await.unwrap();
        eventually("relaunched", || *world.agents.prompts.lock().unwrap() == ["первое", "после стопа"]).await;
    }

    #[tokio::test]
    async fn text_answers_an_open_question() {
        let world = world(FakeAgents::default());
        let (responder, answer) = oneshot::channel();
        let question = hub_core::domain::Question::new("Цвет?".to_owned(), String::new(), Vec::new(),
            hub_core::domain::Selection::Single).unwrap();
        lock(&world.handle_registries()).questions.open(QuestionId::from_random([1; 8]), KEY, question, responder);

        world.handle.mailbox.send(text(Some(7), 1, "синий")).await.unwrap();
        assert_eq!(tokio::time::timeout(Duration::from_secs(2), answer).await.unwrap().unwrap(),
                   Answer::Text("синий".to_owned()));
        assert!(world.agents.prompts.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn voice_gets_a_hint() {
        let world = world(FakeAgents::default());
        world.handle.mailbox.send(HubMessage::Inbound(Inbound {
            chat: ChatId(-100), thread: Some(ThreadId(7)), user: Some(UserId(1)), message: MessageId(1),
            album: None, content: Content::Voice,
        })).await.unwrap();
        eventually("hint", || texts_of(&world).contains(&texts::VOICE_UNSUPPORTED.to_owned())).await;
    }

    #[tokio::test(start_paused = true)]
    async fn album_parts_become_one_turn() {
        let mut messenger = FakeMessenger::default();
        messenger.files.insert("p1".to_owned(), vec![1]);
        messenger.files.insert("p2".to_owned(), vec![2]);
        let world = world_with(FakeAgents::default(), messenger);
        for (message, file, caption) in [(1, "p1", "смотри"), (2, "p2", "")] {
            world.handle.mailbox.send(HubMessage::Inbound(Inbound {
                chat: ChatId(-100), thread: Some(ThreadId(7)), user: Some(UserId(1)), message: MessageId(message),
                album: Some("g".to_owned()),
                content: Content::Media { caption: caption.to_owned(),
                    attachment: Attachment::Photo(FileRef { id: file.to_owned(), name: None, size: Some(1) }) },
            })).await.unwrap();
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        eventually("one turn", || *world.agents.prompts.lock().unwrap() == ["смотри"]).await;
    }

    #[tokio::test]
    async fn shutdown_stops_running_sessions() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.handle.mailbox.send(text(Some(7), 1, "долго")).await.unwrap();
        eventually("running", || world.handle.views.borrow().iter().any(|v| v.state == TopicState::Running)).await;
        let (done, stopped) = oneshot::channel();
        world.handle.mailbox.send(HubMessage::Shutdown(done)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), stopped).await.unwrap().unwrap();
        assert!(texts_of(&world).contains(&texts::STOPPED.to_owned()));
    }
}
```

Тестам нужны две вспомогательные вещи, их добавить в тестовый модуль рядом с `world`: `world_with(agents, messenger)` (как `world`, но с готовым `FakeMessenger`; `world` вызывает `world_with(agents, FakeMessenger::default())`) и метод `World::handle_registries()`, возвращающий `Shared` из `HubHandle` — для этого `HubHandle` получает публичное поле `registries: Shared` (тот же объект, что у актора). Импорты тестового модуля дополнить: `use crate::channel::lock; use crate::messenger::Target; use crate::testing::Call; use hub_core::questions::{Answer, QuestionId};`.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-telegram --lib hub`
Expected: FAIL — `cannot find trait Agents`.

- [ ] **Step 3: Реализовать `hub.rs`**

```rust
//! The hub actor: one task owns every topic's state and handles messages one at a time
//! without waiting on the network; sessions run in their own tasks and report back.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_claude::session::Conversation;
use hub_core::commands::{Command, Input, NewSessionArgs, join_path_args, parse_input, parse_new_args};
use hub_core::domain::{AbsolutePath, MessageId, Prompt, SessionId, TopicKey, TopicSession, ChatId};
use hub_core::settings::Settings;
use hub_core::topics::Topics;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::channel::{Shared, lock};
use crate::inbound::{Content, Inbound, Press, Turn};
use crate::messenger::{Messenger, Target};
use crate::paths::{CwdError, resolve_cwd, workspace_root};
use crate::press;
use crate::sender::Sender;
use crate::session::{First, Run};
use crate::texts;

pub(crate) const INBOX: usize = 32;
const MAILBOX: usize = 256;
// Telegram delivers album parts as separate updates sharing a media group id.
const ALBUM_WAIT: Duration = Duration::from_secs(1);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

pub trait Agents: Send + Sync + 'static {
    fn run<'a>(
        &'a self,
        session: &'a TopicSession,
        prompt: Prompt,
        inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation,
    ) -> BoxFuture<'a, mpsc::Receiver<Prompt>>;
}

pub trait TopicStore: Send + 'static {
    fn save(&mut self, topics: &Topics) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicState {
    Waiting,
    Running,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicView {
    pub key: TopicKey,
    pub title: Option<String>,
    pub session: TopicSession,
    pub state: TopicState,
}

pub enum HubMessage {
    Inbound(Inbound),
    Press(Press),
    /// A prompt whose attachments finished downloading.
    Deliver { key: TopicKey, prompt: Prompt },
    FlushAlbum(String),
    Bind { key: TopicKey, session: SessionId },
    /// A session task ended; `inbox` is gone only if the task crashed.
    Ended { key: TopicKey, generation: u64, inbox: Option<mpsc::Receiver<Prompt>> },
    Stop(TopicKey),
    Reset(TopicKey),
    Shutdown(oneshot::Sender<()>),
}

pub struct HubSetup<A> {
    pub agents: Arc<A>,
    pub messenger: Arc<dyn Messenger>,
    pub settings: watch::Receiver<Arc<Settings>>,
    pub topics: Topics,
    pub store: Box<dyn TopicStore>,
    pub home: PathBuf,
    /// The bot's username, to accept `/command@bot`.
    pub bot: String,
}

pub struct HubHandle {
    pub mailbox: mpsc::Sender<HubMessage>,
    pub views: watch::Receiver<Vec<TopicView>>,
    pub registries: Shared,
    pub task: JoinHandle<()>,
}

struct Live {
    inbox: mpsc::Sender<Prompt>,
    cancel: CancellationToken,
    generation: u64,
}

struct Hub<A> {
    agents: Arc<A>,
    sender: Sender,
    registries: Shared,
    settings: watch::Receiver<Arc<Settings>>,
    topics: Topics,
    store: Box<dyn TopicStore>,
    home: PathBuf,
    bot: String,
    running: HashMap<TopicKey, Live>,
    albums: HashMap<String, Vec<Inbound>>,
    titles: HashMap<TopicKey, String>,
    generation: u64,
    mailbox: mpsc::Sender<HubMessage>,
    views: watch::Sender<Vec<TopicView>>,
    shutdown: Option<(oneshot::Sender<()>, Instant)>,
}

pub fn spawn<A: Agents>(setup: HubSetup<A>) -> HubHandle {
    let HubSetup { agents, messenger, settings, topics, store, home, bot } = setup;
    let (mailbox, inbox) = mpsc::channel(MAILBOX);
    let (views, watched) = watch::channel(Vec::new());
    let registries = Shared::default();
    let hub = Hub {
        agents,
        sender: Sender::new(messenger),
        registries: Arc::clone(&registries),
        settings,
        topics,
        store,
        home,
        bot,
        running: HashMap::new(),
        albums: HashMap::new(),
        titles: HashMap::new(),
        generation: 0,
        mailbox: mailbox.clone(),
        views,
        shutdown: None,
    };
    let task = tokio::spawn(hub.run(inbox));
    HubHandle { mailbox, views: watched, registries, task }
}

impl<A: Agents> Hub<A> {
    async fn run(mut self, mut mailbox: mpsc::Receiver<HubMessage>) {
        self.publish();
        loop {
            let deadline = self.shutdown.as_ref().map(|(_, at)| *at);
            let message = tokio::select! {
                message = mailbox.recv() => message,
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => None,
            };
            let Some(message) = message else { break };
            self.handle(message);
            if self.shutdown.is_some() && self.running.is_empty() {
                break;
            }
        }
        if let Some((done, _)) = self.shutdown.take() {
            // The caller may have given up waiting; nothing else to tell then.
            let _ = done.send(());
        }
    }

    fn handle(&mut self, message: HubMessage) {
        match message {
            HubMessage::Inbound(inbound) if self.shutdown.is_none() => self.inbound(inbound),
            HubMessage::Press(pressed) => {
                tokio::spawn(press::handle(Arc::clone(&self.registries), self.sender.clone(), pressed));
            }
            HubMessage::Deliver { key, prompt } => self.deliver(key, prompt),
            HubMessage::FlushAlbum(group) => {
                let parts = self.albums.remove(&group).unwrap_or_default();
                if let Some(key) = parts.first().and_then(Inbound::key) {
                    self.start_turn(key, Turn::from_parts(&parts));
                }
            }
            HubMessage::Bind { key, session } => self.bind(key, session),
            HubMessage::Ended { key, generation, inbox } => self.ended(key, generation, inbox),
            HubMessage::Stop(key) => {
                if let Some(live) = self.running.get(&key) {
                    live.cancel.cancel();
                }
            }
            HubMessage::Reset(key) => {
                if !self.running.contains_key(&key)
                    && let Some(session) = self.topics.get(&key).cloned()
                {
                    self.put(key, session.with_session(None));
                }
            }
            HubMessage::Shutdown(done) => {
                for live in self.running.values() {
                    live.cancel.cancel();
                }
                self.shutdown = Some((done, Instant::now() + SHUTDOWN_TIMEOUT));
            }
            HubMessage::Inbound(_) => {}
        }
        self.publish();
    }

    fn inbound(&mut self, inbound: Inbound) {
        let Some(key) = inbound.key() else {
            self.outside_topic(inbound);
            return;
        };
        if inbound.album.is_some() && matches!(inbound.content, Content::Media { .. }) {
            self.album(inbound);
            return;
        }
        let Inbound { chat, message, content, .. } = inbound;
        match content {
            Content::TopicCreated { name } => {
                self.titles.insert(key, name);
                match self.root() {
                    Ok(root) => {
                        let session = TopicSession::fresh(hub_core::commands::DEFAULT_BACKEND, root);
                        self.put(key, session.clone());
                        self.say(Target::Topic(key), texts::describe(texts::NEW_SESSION, &session));
                    }
                    Err(error) => self.say(Target::Topic(key), texts::warning(&error)),
                }
            }
            Content::Text(text) => match parse_input(&text, &self.bot) {
                Input::Command { command, args } => self.command(key, chat, message, command, &args),
                Input::Unknown => {}
                Input::Text => self.text(key, text.clone()),
            },
            Content::Media { .. } => {
                let parts = [Inbound { chat, thread: Some(key.thread), user: None, message, album: None, content }];
                self.start_turn(key, Turn::from_parts(&parts));
            }
            Content::Voice => self.say(Target::Topic(key), texts::VOICE_UNSUPPORTED.to_owned()),
            Content::Ignored => {}
        }
    }

    fn outside_topic(&self, inbound: Inbound) {
        let target = Target::Reply { chat: inbound.chat, message: inbound.message };
        match inbound.content {
            Content::Text(text) => match parse_input(&text, &self.bot) {
                Input::Command { command: Command::Help, .. } => {
                    let root = self.settings.borrow().workspace_root.clone();
                    self.say(target, texts::help(&root));
                }
                Input::Command { .. } | Input::Text => self.say(target, texts::CREATE_TOPIC.to_owned()),
                Input::Unknown => {}
            },
            Content::Media { .. } | Content::Voice => self.say(target, texts::CREATE_TOPIC.to_owned()),
            Content::TopicCreated { .. } | Content::Ignored => {}
        }
    }

    fn album(&mut self, inbound: Inbound) {
        let Some(group) = inbound.album.clone() else { return };
        let parts = self.albums.entry(group.clone()).or_default();
        parts.push(inbound);
        if parts.len() == 1 {
            let mailbox = self.mailbox.clone();
            tokio::spawn(async move {
                tokio::time::sleep(ALBUM_WAIT).await;
                // The hub is gone only during shutdown, when the album no longer matters.
                let _ = mailbox.send(HubMessage::FlushAlbum(group)).await;
            });
        }
    }

    fn command(&mut self, key: TopicKey, chat: ChatId, message: MessageId, command: Command, args: &[&str]) {
        match command {
            Command::Help => {
                let root = self.settings.borrow().workspace_root.clone();
                self.say(Target::Reply { chat, message }, texts::help(&root));
            }
            Command::New => {
                if self.refuse_if_running(key) {
                    return;
                }
                let NewSessionArgs { backend, cwd } = parse_new_args(args);
                match self.resolve(cwd.as_deref()) {
                    Ok(cwd) => {
                        let session = TopicSession::fresh(backend, cwd);
                        self.put(key, session.clone());
                        self.say(Target::Topic(key), texts::describe(texts::NEW_SESSION, &session));
                    }
                    Err(error) => self.say(Target::Topic(key), texts::warning(&error)),
                }
            }
            Command::Cwd => {
                if self.refuse_if_running(key) {
                    return;
                }
                let Some(raw) = join_path_args(args) else {
                    self.say(Target::Topic(key), texts::CWD_USAGE.to_owned());
                    return;
                };
                let resolved = self.resolve(Some(&raw)).and_then(|cwd| Ok((self.session(key)?, cwd)));
                match resolved {
                    // Agent sessions are stored per project directory, so a new cwd needs a new session.
                    Ok((current, cwd)) => {
                        let session = TopicSession::fresh(current.backend, cwd);
                        self.put(key, session.clone());
                        self.say(Target::Topic(key), texts::describe(texts::CWD_CHANGED, &session));
                    }
                    Err(error) => self.say(Target::Topic(key), texts::warning(&error)),
                }
            }
            Command::Reset => {
                if self.refuse_if_running(key) {
                    return;
                }
                match self.session(key) {
                    Ok(session) => {
                        let session = session.with_session(None);
                        self.put(key, session.clone());
                        self.say(Target::Topic(key), texts::describe(texts::CONTEXT_RESET, &session));
                    }
                    Err(error) => self.say(Target::Topic(key), texts::warning(&error)),
                }
            }
            Command::Stop => match self.running.get(&key) {
                Some(live) => live.cancel.cancel(),
                None => self.say(Target::Topic(key), texts::NOTHING_TO_STOP.to_owned()),
            },
            Command::Status => match self.topics.get(&key) {
                None => self.say(Target::Topic(key), texts::NO_SESSION.to_owned()),
                Some(session) => {
                    let state = if self.running.contains_key(&key) { texts::RUNNING } else { texts::WAITING };
                    self.say(Target::Topic(key), texts::describe(state, session));
                }
            },
        }
    }

    fn text(&mut self, key: TopicKey, text: String) {
        // While the agent waits on a question, the next message in the topic is its answer.
        let pending = lock(&self.registries).questions.reply(key, &text);
        if let Some((responder, answer)) = pending
            && responder.send(answer).is_ok()
        {
            return;
        }
        self.start_turn(key, Turn::text(text));
    }

    fn start_turn(&mut self, key: TopicKey, turn: Turn) {
        if !self.running.contains_key(&key) {
            self.launch(key, First::Turn(turn), None);
            return;
        }
        if !turn.has_attachments() {
            if let Some(prompt) = Prompt::new(turn.text, Vec::new()) {
                self.deliver(key, prompt);
            }
            return;
        }
        let Ok(session) = self.session(key) else { return };
        let (sender, mailbox) = (self.sender.clone(), self.mailbox.clone());
        tokio::spawn(async move {
            match crate::download::download(&sender, &session.cwd, turn).await {
                Ok(prompt) => {
                    // The hub is gone only during shutdown.
                    let _ = mailbox.send(HubMessage::Deliver { key, prompt }).await;
                }
                Err(error) => sender.text(Target::Topic(key), &texts::warning(&error)).await,
            }
        });
    }

    fn deliver(&mut self, key: TopicKey, prompt: Prompt) {
        let Some(live) = self.running.get(&key) else {
            self.launch(key, First::Prompt(prompt), None);
            return;
        };
        match live.inbox.try_send(prompt) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => self.say(Target::Topic(key), texts::QUEUE_FULL.to_owned()),
            Err(mpsc::error::TrySendError::Closed(prompt)) => {
                self.running.remove(&key);
                self.launch(key, First::Prompt(prompt), None);
            }
        }
    }

    fn launch(
        &mut self,
        key: TopicKey,
        first: First,
        reuse: Option<(mpsc::Sender<Prompt>, mpsc::Receiver<Prompt>)>,
    ) {
        let session = match self.session(key) {
            Ok(session) => session,
            Err(error) => {
                self.say(Target::Topic(key), texts::warning(&error));
                return;
            }
        };
        let settings = Arc::clone(&self.settings.borrow());
        let (inbox, receiver) = reuse.unwrap_or_else(|| mpsc::channel(INBOX));
        self.generation += 1;
        let cancel = CancellationToken::new();
        self.running.insert(key, Live { inbox, cancel: cancel.clone(), generation: self.generation });
        let run = Run {
            key,
            session,
            first,
            inbox: receiver,
            cancel,
            generation: self.generation,
            root: workspace_root(&settings.workspace_root),
            settings,
            sender: self.sender.clone(),
            registries: Arc::clone(&self.registries),
            agents: Arc::clone(&self.agents),
            home: self.home.clone(),
            mailbox: self.mailbox.clone(),
        };
        tokio::spawn(run.execute());
    }

    fn ended(&mut self, key: TopicKey, generation: u64, inbox: Option<mpsc::Receiver<Prompt>>) {
        let Some(live) = self.running.remove(&key) else { return };
        if live.generation != generation {
            self.running.insert(key, live);
            return;
        }
        let Some(mut inbox) = inbox else { return };
        if self.shutdown.is_some() {
            return;
        }
        // A prompt that arrived while the session was closing starts the next one.
        if let Ok(prompt) = inbox.try_recv() {
            self.launch(key, First::Prompt(prompt), Some((live.inbox, inbox)));
        }
    }

    fn bind(&mut self, key: TopicKey, id: SessionId) {
        let Ok(session) = self.session(key) else { return };
        if session.session.as_ref() != Some(&id) {
            self.put(key, session.with_session(Some(id)));
        }
    }

    /// The topic's session, binding a fresh one at the root on first use.
    fn session(&mut self, key: TopicKey) -> Result<TopicSession, CwdError> {
        if let Some(session) = self.topics.get(&key) {
            return Ok(session.clone());
        }
        let session = TopicSession::fresh(hub_core::commands::DEFAULT_BACKEND, self.root()?);
        self.put(key, session.clone());
        tracing::info!(chat = key.chat.0, thread = key.thread.0, "topic bound");
        Ok(session)
    }

    fn put(&mut self, key: TopicKey, session: TopicSession) {
        self.topics.insert(key, session);
        if let Err(error) = self.store.save(&self.topics) {
            tracing::error!(%error, "topics could not be saved");
        }
    }

    fn root(&self) -> Result<AbsolutePath, CwdError> {
        workspace_root(&self.settings.borrow().workspace_root)
    }

    fn resolve(&self, raw: Option<&str>) -> Result<AbsolutePath, CwdError> {
        resolve_cwd(&self.root()?, &self.home, raw)
    }

    fn refuse_if_running(&self, key: TopicKey) -> bool {
        let running = self.running.contains_key(&key);
        if running {
            self.say(Target::Topic(key), texts::ALREADY_RUNNING.to_owned());
        }
        running
    }

    fn say(&self, target: Target, text: String) {
        let sender = self.sender.clone();
        tokio::spawn(async move { sender.text(target, &text).await });
    }

    fn publish(&self) {
        let views = self
            .topics
            .iter()
            .map(|(key, session)| TopicView {
                key: *key,
                title: self.titles.get(key).cloned(),
                session: session.clone(),
                state: if self.running.contains_key(key) { TopicState::Running } else { TopicState::Waiting },
            })
            .collect();
        self.views.send_replace(views);
    }
}
```

- [ ] **Step 4: Реализовать `session.rs` (задача сессии темы)**

```rust
//! One topic session as a task: downloads the first turn, runs the agent, relays its events
//! to the topic, and reports back to the hub.

use std::path::PathBuf;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use hub_claude::session::{Conversation, Limits};
use hub_core::domain::{AbsolutePath, AgentEvent, Prompt, TopicKey, TopicSession};
use hub_core::render::{format_abandoned, format_finished};
use hub_core::settings::Settings;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::channel::{Shared, TelegramChannel};
use crate::download::download;
use crate::hub::{Agents, HubMessage};
use crate::inbound::Turn;
use crate::messenger::{Outgoing, Target};
use crate::paths::{CwdError, inside};
use crate::sender::Sender;
use crate::texts;

const EVENTS: usize = 256;

pub(crate) enum First {
    Turn(Turn),
    Prompt(Prompt),
}

pub(crate) struct Run<A> {
    pub(crate) key: TopicKey,
    pub(crate) session: TopicSession,
    pub(crate) first: First,
    pub(crate) inbox: mpsc::Receiver<Prompt>,
    pub(crate) cancel: CancellationToken,
    pub(crate) generation: u64,
    pub(crate) root: Result<AbsolutePath, CwdError>,
    pub(crate) settings: Arc<Settings>,
    pub(crate) sender: Sender,
    pub(crate) registries: Shared,
    pub(crate) agents: Arc<A>,
    pub(crate) home: PathBuf,
    pub(crate) mailbox: mpsc::Sender<HubMessage>,
}

impl<A: Agents> Run<A> {
    pub(crate) async fn execute(self) {
        let (key, generation) = (self.key, self.generation);
        let (sender, mailbox) = (self.sender.clone(), self.mailbox.clone());
        let inbox = match AssertUnwindSafe(self.converse()).catch_unwind().await {
            Ok(inbox) => Some(inbox),
            Err(_) => {
                tracing::error!(chat = key.chat.0, thread = key.thread.0, "session crashed");
                sender.text(Target::Topic(key), texts::INTERNAL_ERROR).await;
                None
            }
        };
        // The hub is gone only during shutdown, when leftover prompts no longer matter.
        let _ = mailbox.send(HubMessage::Ended { key, generation, inbox }).await;
    }

    async fn converse(self) -> mpsc::Receiver<Prompt> {
        let Self { key, session, first, inbox, cancel, root, settings, sender, registries, agents, home, mailbox, .. } = self;
        sender.typing(key).await;
        let root = match root {
            Ok(root) => root,
            Err(error) => {
                sender.text(Target::Topic(key), &texts::warning(&error)).await;
                return inbox;
            }
        };
        if !inside(&root, &session.cwd) {
            sender.text(Target::Topic(key), &texts::outside_root(&session.cwd, &root)).await;
            return inbox;
        }
        let prompt = match first {
            First::Prompt(prompt) => prompt,
            First::Turn(turn) => match download(&sender, &session.cwd, turn).await {
                Ok(prompt) => prompt,
                Err(error) => {
                    sender.text(Target::Topic(key), &texts::warning(&error)).await;
                    return inbox;
                }
            },
        };
        let channel = Arc::new(TelegramChannel::new(
            sender.clone(),
            key,
            session.cwd.clone(),
            home,
            registries,
            settings.timeouts.approval,
        ));
        let (events, received) = mpsc::channel(EVENTS);
        let conversation = Conversation {
            channel,
            events,
            cancel: cancel.clone(),
            limits: Limits::new(settings.timeouts.background),
        };
        let background = settings.timeouts.background;
        let (inbox, ()) = tokio::join!(
            agents.run(&session, prompt, inbox, conversation),
            relay(received, &sender, key, &mailbox, background),
        );
        if cancel.is_cancelled() {
            sender.text(Target::Topic(key), texts::STOPPED).await;
        }
        inbox
    }
}

async fn relay(
    mut events: mpsc::Receiver<AgentEvent>,
    sender: &Sender,
    key: TopicKey,
    mailbox: &mpsc::Sender<HubMessage>,
    background: Duration,
) {
    while let Some(event) = events.recv().await {
        match event {
            AgentEvent::SessionStarted(session) => bind(mailbox, key, session).await,
            AgentEvent::AssistantText(text) => sender.markdown(key, &text).await,
            AgentEvent::ToolCall(call) => {
                sender.one(Target::Topic(key), Outgoing::html(texts::tool_call_html(&call))).await;
            }
            AgentEvent::Finished(finished) => {
                tracing::info!(turns = finished.turns, background = finished.background, "turn finished");
                bind(mailbox, key, finished.session.clone()).await;
                let line = format_finished(finished.turns, finished.cost, finished.background);
                sender.text(Target::Topic(key), &line).await;
            }
            AgentEvent::BackgroundAbandoned(tasks) => {
                tracing::warn!(tasks = tasks.len(), "background tasks abandoned");
                sender.text(Target::Topic(key), &format_abandoned(&tasks, background)).await;
            }
            AgentEvent::Failed(reason) => {
                tracing::warn!(%reason, "turn failed");
                sender.text(Target::Topic(key), &texts::failure(&reason)).await;
            }
        }
    }
}

/// Persisted at once, so a crash mid-turn can still resume the session.
async fn bind(mailbox: &mpsc::Sender<HubMessage>, key: TopicKey, session: hub_core::domain::SessionId) {
    // The hub is gone only during shutdown.
    let _ = mailbox.send(HubMessage::Bind { key, session }).await;
}
```

- [ ] **Step 5: Прогнать тесты и линтеры**

Run: `cargo test -p hub-telegram --lib && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS. Если тест на альбом зависает из-за `start_paused` и реального ввода-вывода `tempfile` — заменить его на тест с реальным временем (ожидание 1,5 с), записав решение в ledger.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-telegram
git commit -m "hub-telegram: актор Hub — темы, сессии, команды, альбомы"
```

---

### Task 7: Обвязка teloxide и агенты

**Files:**
- Create: `crates/hub-telegram/src/telegram.rs`, `crates/hub-telegram/src/agents.rs`
- Modify: `crates/hub-telegram/src/lib.rs` (`pub mod agents; pub mod telegram;`)

**Interfaces:**
- Consumes: `hub::{Agents, HubMessage}`, `inbound::*`, `messenger::*`, `hub_claude::{backend::ClaudeBackend, version::locate}`, `hub_core::settings::{Settings, TelegramSettings, Token}`.
- Produces:
  - `telegram::convert(&Message) -> Inbound`, `telegram::pressed(&CallbackQuery) -> Press`, `telegram::allowed(&TelegramSettings, Option<ChatId>, Option<UserId>) -> bool`.
  - `telegram::TelegramMessenger` implementing `Messenger`; `telegram::StartError::{InvalidToken, ChatNotFound, Network(String)}`; `telegram::connect(&Token, ChatId) -> Result<Connection, StartError>` (async), `telegram::Connection { bot: Bot, messenger: Arc<TelegramMessenger>, username: String }`; `telegram::listen(Bot, TelegramSettings, mpsc::Sender<HubMessage>) -> Listener`, `telegram::Listener { stop: ShutdownToken, task: JoinHandle<()> }` + `Listener::stop(self)` (async).
  - `agents::ClaudeAgents::new(watch::Receiver<Arc<Settings>>)` implementing `Agents`.

- [ ] **Step 1: Падающие тесты преобразования апдейтов и allowlist**

`crates/hub-telegram/src/telegram.rs`:

```rust
#[cfg(test)]
mod tests {
    use hub_core::settings::Draft;
    use serde_json::json;

    use super::*;

    fn message(extra: serde_json::Value) -> Message {
        let mut base = json!({
            "message_id": 5,
            "date": 1_700_000_000,
            "chat": {"id": -100, "type": "supergroup", "title": "g", "is_forum": true},
            "from": {"id": 1, "is_bot": false, "first_name": "u"},
        });
        base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    #[test]
    fn topic_text_message_is_converted() {
        let inbound = convert(&message(json!({"message_thread_id": 7, "is_topic_message": true, "text": "/new x"})));
        assert_eq!(inbound.key(), Some(TopicKey { chat: hub_core::domain::ChatId(-100), thread: hub_core::domain::ThreadId(7) }));
        assert_eq!(inbound.user, Some(hub_core::domain::UserId(1)));
        assert_eq!(inbound.content, Content::Text("/new x".to_owned()));
    }

    #[test]
    fn reply_thread_outside_a_topic_is_not_a_topic() {
        let inbound = convert(&message(json!({"message_thread_id": 9, "text": "hi"})));
        assert_eq!(inbound.key(), None);
    }

    #[test]
    fn largest_photo_and_documents_become_attachments() {
        let photo = convert(&message(json!({"message_thread_id": 7, "is_topic_message": true, "caption": "что это?",
            "media_group_id": "g1",
            "photo": [
                {"file_id": "small", "file_unique_id": "s", "width": 90, "height": 90, "file_size": 10},
                {"file_id": "large", "file_unique_id": "l", "width": 900, "height": 900, "file_size": 100}
            ]})));
        assert_eq!(photo.album.as_deref(), Some("g1"));
        assert_eq!(photo.content, Content::Media {
            caption: "что это?".to_owned(),
            attachment: Attachment::Photo(FileRef { id: "large".to_owned(), name: None, size: Some(100) }),
        });
        let document = convert(&message(json!({"message_thread_id": 7, "is_topic_message": true,
            "document": {"file_id": "d", "file_unique_id": "du", "file_name": "a.pdf", "file_size": 5}})));
        assert_eq!(document.content, Content::Media {
            caption: String::new(),
            attachment: Attachment::File(FileRef { id: "d".to_owned(), name: Some("a.pdf".to_owned()), size: Some(5) }),
        });
    }

    #[test]
    fn voice_and_topic_creation_are_recognised() {
        let voice = convert(&message(json!({"message_thread_id": 7, "is_topic_message": true,
            "voice": {"file_id": "v", "file_unique_id": "vu", "duration": 1}})));
        assert_eq!(voice.content, Content::Voice);
        let created = convert(&message(json!({"message_thread_id": 7, "is_topic_message": true,
            "forum_topic_created": {"name": "backend", "icon_color": 0}})));
        assert_eq!(created.content, Content::TopicCreated { name: "backend".to_owned() });
    }

    #[test]
    fn only_allowed_users_in_the_chat_pass() {
        let telegram = Draft {
            token: "1:a".to_owned(), chat: "-100".to_owned(), users: "1, 2".to_owned(),
            workspace_root: "~/p".to_owned(), ..Draft::default()
        }
        .parse(&std::env::temp_dir())
        .unwrap()
        .telegram;
        let chat = Some(hub_core::domain::ChatId(-100));
        assert!(allowed(&telegram, chat, Some(hub_core::domain::UserId(2))));
        assert!(!allowed(&telegram, chat, Some(hub_core::domain::UserId(3))));
        assert!(!allowed(&telegram, Some(hub_core::domain::ChatId(-1)), Some(hub_core::domain::UserId(1))));
        assert!(!allowed(&telegram, chat, None));
    }
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-telegram --lib telegram`
Expected: FAIL — `cannot find function convert`.

- [ ] **Step 3: Реализовать `telegram.rs`**

```rust
//! The Bot API through teloxide: update conversion, the allowlist guard, the `Messenger`
//! implementation and the long-polling listener.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_core::domain::{ChatId, MessageId, ThreadId, TopicKey, UserId};
use hub_core::questions::Button;
use hub_core::settings::{TelegramSettings, Token};
use teloxide::dispatching::{Dispatcher, ShutdownToken, UpdateFilterExt};
use teloxide::net::Download;
use teloxide::prelude::{Bot, CallbackQuery, Message, Request, Requester, Update};
use teloxide::types::{
    self as tg, ChatAction, FileId, InlineKeyboardButton, InlineKeyboardMarkup, InputFile,
    ParseMode, ReplyParameters,
};
use teloxide::utils::render::RenderMessageTextHelper;
use teloxide::{ApiError, RequestError};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::hub::HubMessage;
use crate::inbound::{Attachment, Content, FileRef, Inbound, Press};
use crate::messenger::{Format, Messenger, Outgoing, SendError, Target};

// Uploads and downloads of large files take far longer than ordinary requests.
const FILE_TIMEOUT: Duration = Duration::from_secs(120);

#[must_use]
pub fn convert(message: &Message) -> Inbound {
    let thread = if message.is_topic_message { message.thread_id.map(|thread| ThreadId(thread.0.0)) } else { None };
    Inbound {
        chat: ChatId(message.chat.id.0),
        thread,
        user: message.from.as_ref().map(|user| UserId(user.id.0)),
        message: MessageId(message.id.0),
        album: message.media_group_id().map(|group| group.0.clone()),
        content: content(message),
    }
}

fn content(message: &Message) -> Content {
    let caption = message.caption().unwrap_or_default().to_owned();
    let media = |attachment| Content::Media { caption: caption.clone(), attachment };
    if let Some(created) = message.forum_topic_created() {
        return Content::TopicCreated { name: created.name.clone() };
    }
    if message.voice().is_some() || message.video_note().is_some() {
        return Content::Voice;
    }
    if let Some(photo) = message.photo().and_then(|sizes| sizes.last()) {
        return media(Attachment::Photo(FileRef { id: photo.file.id.0.clone(), name: None, size: Some(u64::from(photo.file.size)) }));
    }
    let file = message
        .document()
        .map(|document| (&document.file, document.file_name.clone()))
        .or_else(|| message.audio().map(|audio| (&audio.file, audio.file_name.clone())))
        .or_else(|| message.video().map(|video| (&video.file, video.file_name.clone())));
    if let Some((file, name)) = file {
        return media(Attachment::File(FileRef { id: file.id.0.clone(), name, size: Some(u64::from(file.size)) }));
    }
    match message.text() {
        Some(text) => Content::Text(text.to_owned()),
        None => Content::Ignored,
    }
}

#[must_use]
pub fn pressed(query: &CallbackQuery) -> Press {
    let message = query.regular_message();
    Press {
        callback: query.id.0.clone(),
        user: UserId(query.from.id.0),
        chat: message.map(|message| ChatId(message.chat.id.0)),
        message: message.map(|message| MessageId(message.id.0)),
        html: message.and_then(RenderMessageTextHelper::html_text),
        data: query.data.clone().unwrap_or_default(),
    }
}

#[must_use]
pub fn allowed(telegram: &TelegramSettings, chat: Option<ChatId>, user: Option<UserId>) -> bool {
    chat == Some(telegram.chat) && user.is_some_and(|user| telegram.users.contains(user))
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("Telegram не принял токен бота")]
    InvalidToken,
    #[error("бот не видит группу: добавьте его в группу и проверьте chat_id")]
    ChatNotFound,
    #[error("Telegram недоступен: {0}")]
    Network(String),
}

pub struct Connection {
    pub bot: Bot,
    pub messenger: Arc<TelegramMessenger>,
    pub username: String,
}

/// Checks the token and the chat before the hub starts.
pub async fn connect(token: &Token, chat: ChatId) -> Result<Connection, StartError> {
    let bot = Bot::new(token.expose());
    let client = teloxide::net::default_reqwest_settings()
        .timeout(FILE_TIMEOUT)
        .build()
        .map_err(|error| StartError::Network(error.to_string()))?;
    let files = Bot::with_client(token.expose(), client);
    let me = bot.get_me().await.map_err(|error| match error {
        RequestError::Api(ApiError::InvalidToken | ApiError::NotFound) => StartError::InvalidToken,
        other => StartError::Network(other.to_string()),
    })?;
    bot.get_chat(tg::ChatId(chat.0)).await.map_err(|error| match error {
        RequestError::Api(ApiError::ChatNotFound) => StartError::ChatNotFound,
        other => StartError::Network(other.to_string()),
    })?;
    let username = me.user.username.clone().unwrap_or_default();
    let messenger = Arc::new(TelegramMessenger { bot: bot.clone(), files });
    Ok(Connection { bot, messenger, username })
}

pub struct TelegramMessenger {
    bot: Bot,
    files: Bot,
}

fn sent(error: RequestError) -> SendError {
    match error {
        RequestError::RetryAfter(seconds) => SendError::RetryAfter(seconds.duration()),
        RequestError::Api(api) => SendError::Rejected(api.to_string()),
        other => SendError::Failed(other.to_string()),
    }
}

fn markup(keyboard: Vec<Vec<Button>>) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(keyboard.into_iter().map(|row| {
        row.into_iter().map(|button| InlineKeyboardButton::callback(button.text, button.data)).collect::<Vec<_>>()
    }))
}

fn thread(key: TopicKey) -> tg::ThreadId {
    tg::ThreadId(tg::MessageId(key.thread.0))
}

impl Messenger for TelegramMessenger {
    fn send(&self, target: Target, message: Outgoing) -> BoxFuture<'_, Result<MessageId, SendError>> {
        Box::pin(async move {
            let Outgoing { text, format, keyboard } = message;
            let chat = match target {
                Target::Topic(key) => key.chat,
                Target::Reply { chat, .. } => chat,
            };
            let mut request = self.bot.send_message(tg::ChatId(chat.0), text);
            match target {
                Target::Topic(key) => request = request.message_thread_id(thread(key)),
                Target::Reply { message, .. } => {
                    request = request.reply_parameters(ReplyParameters::new(tg::MessageId(message.0)));
                }
            }
            if format == Format::Html {
                request = request.parse_mode(ParseMode::Html);
            }
            if !keyboard.is_empty() {
                request = request.reply_markup(markup(keyboard));
            }
            request.await.map(|sent| MessageId(sent.id.0)).map_err(sent)
        })
    }

    fn edit(&self, chat: ChatId, message: MessageId, text: String, format: Format) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            let mut request = self.bot.edit_message_text(tg::ChatId(chat.0), tg::MessageId(message.0), text);
            if format == Format::Html {
                request = request.parse_mode(ParseMode::Html);
            }
            request.await.map(|_| ()).map_err(sent)
        })
    }

    fn edit_keyboard(&self, chat: ChatId, message: MessageId, keyboard: Vec<Vec<Button>>) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            self.bot
                .edit_message_reply_markup(tg::ChatId(chat.0), tg::MessageId(message.0))
                .reply_markup(markup(keyboard))
                .await
                .map(|_| ())
                .map_err(sent)
        })
    }

    fn answer(&self, callback: String, text: Option<String>) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            let mut request = self.bot.answer_callback_query(tg::CallbackQueryId(callback));
            if let Some(text) = text {
                request = request.text(text);
            }
            request.await.map(|_| ()).map_err(sent)
        })
    }

    fn document(&self, key: TopicKey, path: PathBuf, caption: String) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            let mut request = self.files.send_document(tg::ChatId(key.chat.0), InputFile::file(path)).message_thread_id(thread(key));
            if !caption.is_empty() {
                request = request.caption(caption);
            }
            request.await.map(|_| ()).map_err(sent)
        })
    }

    fn typing(&self, key: TopicKey) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            self.bot
                .send_chat_action(tg::ChatId(key.chat.0), ChatAction::Typing)
                .message_thread_id(thread(key))
                .await
                .map(|_| ())
                .map_err(sent)
        })
    }

    fn fetch(&self, file: String) -> BoxFuture<'_, Result<Vec<u8>, SendError>> {
        Box::pin(async move {
            let file = self.files.get_file(FileId(file)).await.map_err(sent)?;
            let mut bytes = Vec::new();
            self.files.download_file(&file.path, &mut bytes).await.map_err(|error| SendError::Failed(error.to_string()))?;
            Ok(bytes)
        })
    }

    fn save(&self, file: String, mut target: tokio::fs::File) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            let file = self.files.get_file(FileId(file)).await.map_err(sent)?;
            self.files.download_file(&file.path, &mut target).await.map_err(|error| SendError::Failed(error.to_string()))?;
            target.flush().await.map_err(|error| SendError::Failed(error.to_string()))
        })
    }
}

pub struct Listener {
    stop: ShutdownToken,
    task: JoinHandle<()>,
}

impl Listener {
    /// Stops long polling and waits for in-flight handlers.
    pub async fn stop(self) {
        if let Ok(done) = self.stop.shutdown() {
            done.await;
        }
        if let Err(error) = self.task.await {
            tracing::error!(%error, "telegram listener crashed");
        }
    }
}

struct Guard {
    telegram: TelegramSettings,
    hub: mpsc::Sender<HubMessage>,
}

#[must_use]
pub fn listen(bot: Bot, telegram: TelegramSettings, hub: mpsc::Sender<HubMessage>) -> Listener {
    let guard = Arc::new(Guard { telegram, hub });
    let handler = teloxide::dptree::entry()
        .branch(Update::filter_message().endpoint(on_message))
        .branch(Update::filter_callback_query().endpoint(on_press));
    let mut dispatcher = Dispatcher::builder(bot, handler)
        .dependencies(teloxide::dptree::deps![guard])
        .default_handler(|_update| async {})
        .build();
    let stop = dispatcher.shutdown_token();
    let task = tokio::spawn(async move { dispatcher.dispatch().await });
    Listener { stop, task }
}

async fn on_message(message: Message, guard: Arc<Guard>) -> Result<(), RequestError> {
    let inbound = convert(&message);
    if allowed(&guard.telegram, Some(inbound.chat), inbound.user) {
        forward(&guard, HubMessage::Inbound(inbound)).await;
    } else {
        tracing::warn!(chat_id = inbound.chat.0, user_id = ?inbound.user.map(|user| user.0), "rejected update");
    }
    Ok(())
}

async fn on_press(query: CallbackQuery, guard: Arc<Guard>) -> Result<(), RequestError> {
    let press = pressed(&query);
    if allowed(&guard.telegram, press.chat, Some(press.user)) {
        forward(&guard, HubMessage::Press(press)).await;
    } else {
        tracing::warn!(chat_id = ?press.chat.map(|chat| chat.0), user_id = press.user.0, "rejected update");
    }
    Ok(())
}

async fn forward(guard: &Guard, message: HubMessage) {
    if guard.hub.send(message).await.is_err() {
        tracing::warn!("update dropped: the hub has stopped");
    }
}
```

Если какие-то имена teloxide 0.17 отличаются (например, `ApiError::NotFound`, `ReplyParameters::new`, поле `file.size` типа `u32`), — свериться с исходником в `~/.cargo/registry/src/*/teloxide-core-0.13.0` и поправить, записав решение в ledger.

- [ ] **Step 4: Реализовать `agents.rs`**

```rust
//! The agent backends behind the hub; adding one is a new `BackendKind` variant and a match arm.

use std::sync::Arc;

use futures::future::BoxFuture;
use hub_claude::backend::ClaudeBackend;
use hub_claude::session::Conversation;
use hub_claude::version::locate;
use hub_core::domain::{AgentEvent, BackendKind, Prompt, TopicSession};
use hub_core::settings::Settings;
use tokio::sync::{mpsc, watch};

use crate::hub::Agents;

pub struct ClaudeAgents {
    settings: watch::Receiver<Arc<Settings>>,
}

impl ClaudeAgents {
    #[must_use]
    pub fn new(settings: watch::Receiver<Arc<Settings>>) -> Self {
        Self { settings }
    }
}

impl Agents for ClaudeAgents {
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
                BackendKind::Claude => match locate(settings.claude.cli.as_deref()) {
                    Ok(cli) => {
                        ClaudeBackend::new(cli, settings.claude.clone())
                            .run(session, prompt, inbox, conversation)
                            .await
                    }
                    Err(error) => {
                        // Nobody listening means the topic is gone.
                        let _ = conversation.events.send(AgentEvent::Failed(error.to_string())).await;
                        inbox
                    }
                },
            }
        })
    }
}
```

- [ ] **Step 5: Прогнать весь workspace**

Run: `cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-telegram
git commit -m "hub-telegram: обвязка teloxide и выбор бэкенда"
```

---

## Самопроверка плана

- Покрытие спеки (раздел «hub-telegram»): актор `Hub` (Task 6), входящие апдейты и guard (Task 2, 7), сообщение в теме / inbox / новая сессия (Task 6), альбомы (Task 6), `UserChannel` (Task 4), `TelegramSender` с повтором и откатом на plain (Task 3), команды (Task 6), snapshot для GUI — `views` (Task 6), остановка при выключении (Task 6), проверки путей (Task 1), вложения 20/50 МБ (Task 1, 5). Супервизор, хранилища, keyring, GUI — фаза 4.
- Типы между задачами: `Sender` (Task 3) используется в Task 4–6 с одной сигнатурой; `Shared`/`lock` (Task 4) — в Task 6; `Turn`/`Inbound`/`Press` (Task 2) — в Task 5–7; `HubMessage` (Task 6) — в Task 7; `First`/`Run` (Task 6) — только внутри крейта.
