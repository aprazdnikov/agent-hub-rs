# Фаза 4: hub-app — приложение с окном и треем

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Бинарник `agent-hub`: супервизор бота поверх `hub-telegram`, хранилища настроек (`settings.toml` + keyring) и тем (`topics.json`), лог в файл и в окно, окно eframe с вкладками Статус/Темы/Лог/Настройки, иконка в трее, запрет второго экземпляра.

**Architecture:** Главный поток — eframe и трей. Фоновый tokio-runtime — `Supervisor`: принимает `Command` из GUI (`mpsc`), публикует `Snapshot` (`watch`) и будит окно (`Repaint`). Супервизор не знает о Telegram напрямую: бот запускает `Connector` (боевой — `TelegramConnector`, в тестах — фейк), поэтому решения о старте и перезапуске проверяются без сети. Чистые решения — `supervisor::plan` и `gui::look` — покрыты тестами; окно тестов не имеет.

**Tech Stack:** Rust 1.96, eframe/egui 0.36, tray-icon 0.26 (на Linux бэкенд `ksni`), keyring 4.2 (API `v1`), directories 6, rfd 0.17, toml 1.1, tracing-subscriber 0.3, tracing-appender 0.2, tempfile 3, anyhow 1, tokio.

**Spec:** `docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md` (разделы «Архитектура», «Хранение», «Настройки», «Snapshot и лог», «GUI и трей», «Ошибки и надёжность»).

## Global Constraints

- `[workspace.lints]`: `rust.warnings = "deny"`, `unsafe_code = "forbid"`, clippy `pedantic` + `unwrap_used`, `expect_used`, `panic`, `indexing_slicing` = deny; в тестах unwrap/expect/panic/indexing разрешены `clippy.toml`.
- Комментарии — только «почему». Совпадения `_ =>` на своих enum запрещены.
- UI и сообщения — на русском.
- Токен бота хранится только в keyring (сервис `agent-hub`, учётная запись `telegram-token`) и никогда не попадает в лог, `Debug`, `settings.toml` или текст ошибки.
- Каталоги — `directories::ProjectDirs::from("", "", "agent-hub")`: `config_dir/settings.toml`, `data_dir/topics.json`, `data_dir/logs/`, `data_dir/agent-hub.lock`.
- Запись `settings.toml` и `topics.json` атомарна: временный файл в том же каталоге → `sync_all` → rename.
- Повреждённый `topics.json` не перезаписывается: бот не стартует, в статусе путь и причина.
- Остановка приложения: «⏹ Остановлено» в работающие темы, общий таймаут ожидания 15 с (внутри — 10 с актора `Hub`).
- Коммиты без упоминания ассистента и без `Co-Authored-By`.
- Каждая задача заканчивается `cargo test -p hub-app`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`.

## Review Focus

1. Токен утекает в лог, `Debug` снимка или ошибку хранилища → тест `snapshot_debug_hides_the_token` (Task 4) и `token_is_not_written_to_the_file` (Task 2).
2. Второй экземпляр запускается, пока первый работает, → второй получает `InstanceError::Running`, первый не затронут; после выхода первого запуск снова возможен → тест `second_lock_is_refused_until_the_first_is_dropped` (Task 1).
3. Повреждённые `settings.toml` или `topics.json` → файл не перезаписан, статус называет путь и причину → тесты `corrupt_settings_are_reported_and_kept` (Task 2), `corrupt_topics_are_reported_and_kept` (Task 2), `corrupt_topics_stop_the_start` (Task 5).
4. Смена Telegram-настроек при работающем боте → бот останавливается и стартует заново; прочие изменения не перезапускают бота, а новые значения доходят до следующей сессии; бот, остановленный пользователем, не стартует от сохранения → тесты `telegram_change_restarts_a_running_bot`, `other_changes_apply_without_restart`, `stopped_bot_stays_stopped_on_save` (Task 4).
5. Выход, пока бот не может остановиться (сеть висит), → выход всё равно завершается за таймаут → тест `quit_finishes_even_if_the_bot_hangs` (Task 4).

## Отклонения от спеки (вносятся в спеку в Task 1)

- Single-instance — `std::fs::File::try_lock` (стабилен с Rust 1.89) вместо `fs4`.
- Трей на Linux — бэкенд `ksni` (StatusNotifierItem по D-Bus, свой поток), поэтому GTK в процессе не нужен; если трей не создался, закрытие окна = выход (на любой ОС).
- События трея обрабатываются в `App::logic`: eframe 0.36 вызывает его и при скрытом окне после `request_repaint`.
- Статус темы во вкладке «Темы» — «свободна» / «работает»: актор `Hub` не отслеживает фон и ожидание ответа.
- Время в строках вкладки «Лог» — UTC (форматтер `tracing-subscriber` без зависимости от часового пояса).
- Баннер и пункт меню обновления, команда `InstallUpdate` — фаза 5.

---

### Task 1: Крейт hub-app, каталоги, атомарная запись, single-instance

**Files:**
- Modify: `Cargo.toml` (members, workspace.dependencies), `docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md`
- Create: `crates/hub-app/Cargo.toml`, `crates/hub-app/src/lib.rs`, `crates/hub-app/src/main.rs`, `crates/hub-app/src/dirs.rs`, `crates/hub-app/src/atomic.rs`, `crates/hub-app/src/instance.rs`

**Interfaces:**
- Produces: `dirs::AppDirs::{locate() -> Option<AppDirs>, settings(), topics(), logs(), lock()}` (все `-> PathBuf`); `atomic::write_atomic(&Path, &[u8]) -> io::Result<()>`; `instance::InstanceLock::acquire(&Path) -> Result<InstanceLock, InstanceError>`, `InstanceError::{Running, Io { path, error }}`.

- [ ] **Step 1: Workspace и манифест**

`Cargo.toml`: `members = ["crates/hub-core", "crates/hub-claude", "crates/hub-telegram", "crates/hub-app"]`; в `[workspace.dependencies]` добавить:

```toml
hub-telegram = { path = "crates/hub-telegram" }
anyhow = "1.0"
directories = "6.0"
eframe = { version = "0.36", default-features = false, features = ["default_fonts", "glow", "wayland", "x11"] }
keyring = "4.2"
rfd = "0.17"
toml = "1.1"
tracing-appender = "0.2"
tracing-subscriber = { version = "0.3", default-features = false, features = ["fmt", "registry", "std"] }
tray-icon = { version = "0.26", default-features = false }
```

`crates/hub-app/Cargo.toml`:

```toml
[package]
name = "hub-app"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish.workspace = true

[[bin]]
name = "agent-hub"
path = "src/main.rs"

[dependencies]
anyhow.workspace = true
directories.workspace = true
eframe.workspace = true
futures.workspace = true
hub-claude.workspace = true
hub-core.workspace = true
hub-telegram.workspace = true
keyring.workspace = true
rfd.workspace = true
tempfile.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "sync", "time"] }
toml.workspace = true
tracing.workspace = true
tracing-appender.workspace = true
tracing-subscriber.workspace = true

[target.'cfg(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"))'.dependencies]
tray-icon = { workspace = true, features = ["ksni"] }

[target.'cfg(not(any(target_os = "linux", target_os = "freebsd", target_os = "openbsd")))'.dependencies]
tray-icon.workspace = true

[dev-dependencies]
rstest.workspace = true
tokio = { workspace = true, features = ["macros", "rt-multi-thread", "sync", "test-util", "time"] }

[lints]
workspace = true
```

`src/lib.rs`:

```rust
pub mod atomic;
pub mod dirs;
pub mod instance;
```

`src/main.rs` (временный, заменяется в Task 8):

```rust
fn main() {}
```

Спека: в раздел «Уточнения реализации (фаза 3)» не трогать; добавить перед «## Ошибки и надёжность» раздел «### Уточнения реализации (фаза 4)» с шестью пунктами из «Отклонения от спеки» этого плана.

- [ ] **Step 2: Падающие тесты**

`src/atomic.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_missing_directories_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b.toml");
        write_atomic(&path, b"x = 1").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"x = 1");
    }

    #[test]
    fn replaces_and_leaves_no_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        write_atomic(&path, b"old").unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
```

`src/instance.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_is_refused_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join("agent-hub.lock");
        let first = InstanceLock::acquire(&path).unwrap();
        assert!(matches!(InstanceLock::acquire(&path), Err(InstanceError::Running)));
        drop(first);
        assert!(InstanceLock::acquire(&path).is_ok());
    }
}
```

`src/dirs.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_live_in_config_and_data_directories() {
        let dirs = AppDirs { config: PathBuf::from("c"), data: PathBuf::from("d") };
        assert_eq!(dirs.settings(), PathBuf::from("c").join("settings.toml"));
        assert_eq!(dirs.topics(), PathBuf::from("d").join("topics.json"));
        assert_eq!(dirs.logs(), PathBuf::from("d").join("logs"));
        assert_eq!(dirs.lock(), PathBuf::from("d").join("agent-hub.lock"));
    }
}
```

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p hub-app`
Expected: FAIL — `cannot find function write_atomic`, `cannot find type InstanceLock`, `cannot find struct AppDirs`.

- [ ] **Step 4: Реализация**

`src/dirs.rs` (перед тестами):

```rust
//! Where the application keeps its files.

use std::path::PathBuf;

use directories::ProjectDirs;

pub struct AppDirs {
    config: PathBuf,
    data: PathBuf,
}

impl AppDirs {
    #[must_use]
    pub fn locate() -> Option<Self> {
        ProjectDirs::from("", "", "agent-hub").map(|dirs| Self {
            config: dirs.config_dir().to_path_buf(),
            data: dirs.data_dir().to_path_buf(),
        })
    }

    #[must_use]
    pub fn settings(&self) -> PathBuf {
        self.config.join("settings.toml")
    }

    #[must_use]
    pub fn topics(&self) -> PathBuf {
        self.data.join("topics.json")
    }

    #[must_use]
    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }

    #[must_use]
    pub fn lock(&self) -> PathBuf {
        self.data.join("agent-hub.lock")
    }
}
```

`src/atomic.rs`:

```rust
//! Whole-file replacement that never leaves a half-written file behind.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "путь без каталога"))?;
    fs::create_dir_all(directory)?;
    // The temporary file must share the target's filesystem for the rename to be atomic.
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}
```

`src/instance.rs`:

```rust
//! One running copy per user: a second one would poll the same bot and fight over topics.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum InstanceError {
    #[error("agent-hub уже запущен")]
    Running,
    #[error("не удалось открыть {}: {error}", path.display())]
    Io { path: PathBuf, error: io::Error },
}

pub struct InstanceLock {
    file: File,
}

impl InstanceLock {
    pub fn acquire(path: &Path) -> Result<Self, InstanceError> {
        let failed = |error| InstanceError::Io { path: path.to_path_buf(), error };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(failed)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(failed)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file }),
            Err(TryLockError::WouldBlock) => Err(InstanceError::Running),
            Err(TryLockError::Error(error)) => Err(failed(error)),
        }
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // The OS releases the lock with the handle anyway; unlocking first only makes it prompt.
        let _ = self.file.unlock();
    }
}
```

- [ ] **Step 5: Тесты проходят**

Run: `cargo test -p hub-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS (4 теста).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/hub-app docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md
git commit -m "hub-app: каталоги, атомарная запись и единственный экземпляр"
```

---

### Task 2: Хранилища настроек, токена и тем

**Files:**
- Create: `crates/hub-app/src/error.rs`, `crates/hub-app/src/secrets.rs`, `crates/hub-app/src/config.rs`, `crates/hub-app/src/topics.rs`
- Modify: `crates/hub-app/src/lib.rs` (`pub mod config; pub mod error; pub mod secrets; pub mod topics;`)

**Interfaces:**
- Consumes: `atomic::write_atomic`; `hub_core::settings::{Draft, FieldError, Settings, SettingsFile}`; `hub_core::topics::{Topics, parse_state, dump_state}`; `hub_telegram::hub::TopicStore`.
- Produces:
  - `error::StoreError::{Read { path, error: io::Error }, Corrupt { path, reason: String }, Write { path, error: io::Error }, Secret(SecretError)}`.
  - `secrets::{Secrets (trait: read() -> Result<Option<String>, SecretError>, write(&str) -> Result<(), SecretError>), SecretError, Keyring}`; `#[cfg(test)] secrets::MemorySecrets::default()`.
  - `config::{Loaded::{Missing, Ready(Settings), Incomplete { draft: Draft, errors: Vec<FieldError> }}, SettingsStore (trait: save(&Settings) -> Result<(), StoreError>), FileSettings::new(PathBuf, Box<dyn Secrets>), FileSettings::load(&Path home) -> Result<Loaded, StoreError>}`.
  - `topics::{load_topics(&Path) -> Result<Topics, StoreError>, FileTopics::new(PathBuf)}` (`FileTopics: TopicStore`).

- [ ] **Step 1: Падающие тесты**

`src/config.rs`:

```rust
#[cfg(test)]
mod tests {
    use hub_core::settings::Field;

    use super::*;
    use crate::secrets::MemorySecrets;

    fn draft(root: &Path) -> Draft {
        Draft {
            token: "123:secret-token".to_owned(),
            chat: "-100".to_owned(),
            users: "1, 2".to_owned(),
            workspace_root: root.display().to_string(),
            model: "opus".to_owned(),
            ..Draft::default()
        }
    }

    fn store(dir: &Path) -> FileSettings {
        FileSettings::new(dir.join("settings.toml"), Box::new(MemorySecrets::default()))
    }

    #[test]
    fn missing_file_means_no_settings() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(store(dir.path()).load(dir.path()), Ok(Loaded::Missing)));
    }

    #[test]
    fn saved_settings_load_back() {
        let dir = tempfile::tempdir().unwrap();
        let settings = draft(dir.path()).parse(dir.path()).unwrap();
        let store = store(dir.path());
        store.save(&settings).unwrap();
        match store.load(dir.path()).unwrap() {
            Loaded::Ready(loaded) => assert_eq!(loaded, settings),
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn token_is_not_written_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let settings = draft(dir.path()).parse(dir.path()).unwrap();
        store(dir.path()).save(&settings).unwrap();
        let text = std::fs::read_to_string(dir.path().join("settings.toml")).unwrap();
        assert!(!text.contains("secret-token"));
        assert!(text.contains("-100"));
    }

    #[test]
    fn missing_token_leaves_the_form_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let settings = draft(dir.path()).parse(dir.path()).unwrap();
        store(dir.path()).save(&settings).unwrap();
        let fresh = store(dir.path());
        match fresh.load(dir.path()).unwrap() {
            Loaded::Incomplete { draft, errors } => {
                assert_eq!(draft.chat, "-100");
                assert_eq!(errors.iter().map(|e| e.field).collect::<Vec<_>>(), [Field::Token]);
            }
            other => panic!("expected Incomplete, got {other:?}"),
        }
    }

    #[test]
    fn corrupt_settings_are_reported_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "telegram = 5").unwrap();
        let error = store(dir.path()).load(dir.path()).unwrap_err();
        assert!(matches!(error, StoreError::Corrupt { .. }));
        assert!(error.to_string().contains("settings.toml"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "telegram = 5");
    }
}
```

`missing_token_leaves_the_form_incomplete` использует свежий `MemorySecrets` (новый `store(..)`), поэтому токена в нём нет.

`src/topics.rs`:

```rust
#[cfg(test)]
mod tests {
    use hub_core::domain::{
        AbsolutePath, BackendKind, ChatId, SessionId, ThreadId, TopicKey, TopicSession,
    };

    use super::*;

    #[test]
    fn missing_file_means_no_topics() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_topics(&dir.path().join("topics.json")).unwrap().is_empty());
    }

    #[test]
    fn saved_topics_load_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        let cwd = AbsolutePath::new(dir.path().to_path_buf()).unwrap();
        let topics = Topics::from([(
            TopicKey { chat: ChatId(-100), thread: ThreadId(7) },
            TopicSession::fresh(BackendKind::Claude, cwd)
                .with_session(SessionId::parse("s-1")),
        )]);
        FileTopics::new(path.clone()).save(&topics).unwrap();
        assert_eq!(load_topics(&path).unwrap(), topics);
    }

    #[test]
    fn corrupt_topics_are_reported_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        std::fs::write(&path, "{\"version\": 9, \"topics\": []}").unwrap();
        let error = load_topics(&path).unwrap_err();
        assert!(error.to_string().contains("topics.json"));
        assert!(error.to_string().contains("версия"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"version\": 9, \"topics\": []}");
    }
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-app`
Expected: FAIL — `cannot find type FileSettings`, `cannot find function load_topics`.

- [ ] **Step 3: Реализация**

`src/error.rs`:

```rust
//! Failures of the files the application owns.

use std::io;
use std::path::PathBuf;

use crate::secrets::SecretError;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("не удалось прочитать {}: {error}", path.display())]
    Read { path: PathBuf, error: io::Error },
    #[error("{} повреждён: {reason}", path.display())]
    Corrupt { path: PathBuf, reason: String },
    #[error("не удалось записать {}: {error}", path.display())]
    Write { path: PathBuf, error: io::Error },
    #[error(transparent)]
    Secret(#[from] SecretError),
}
```

`src/secrets.rs`:

```rust
//! The bot token lives in the OS credential store, never in a file.

const SERVICE: &str = "agent-hub";
const ACCOUNT: &str = "telegram-token";

/// The store's own message; it never carries the secret itself.
#[derive(Debug, thiserror::Error)]
#[error("системное хранилище ключей недоступно: {0}")]
pub struct SecretError(String);

pub trait Secrets: Send + Sync {
    fn read(&self) -> Result<Option<String>, SecretError>;
    fn write(&self, token: &str) -> Result<(), SecretError>;
}

pub struct Keyring;

fn entry() -> Result<keyring::Entry, SecretError> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(|error| SecretError(error.to_string()))
}

impl Secrets for Keyring {
    fn read(&self) -> Result<Option<String>, SecretError> {
        match entry()?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecretError(error.to_string())),
        }
    }

    fn write(&self, token: &str) -> Result<(), SecretError> {
        entry()?.set_password(token).map_err(|error| SecretError(error.to_string()))
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemorySecrets(std::sync::Mutex<Option<String>>);

#[cfg(test)]
impl Secrets for MemorySecrets {
    fn read(&self) -> Result<Option<String>, SecretError> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn write(&self, token: &str) -> Result<(), SecretError> {
        *self.0.lock().unwrap() = Some(token.to_owned());
        Ok(())
    }
}
```

`src/config.rs`:

```rust
//! `settings.toml` plus the token from the keyring, parsed by the same code as the form.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hub_core::settings::{Draft, FieldError, Settings, SettingsFile};

use crate::atomic::write_atomic;
use crate::error::StoreError;
use crate::secrets::Secrets;

#[derive(Debug)]
pub enum Loaded {
    Missing,
    Ready(Settings),
    /// The file is readable but the form still has errors, e.g. no token in the keyring yet.
    Incomplete { draft: Draft, errors: Vec<FieldError> },
}

pub trait SettingsStore: Send + 'static {
    fn save(&self, settings: &Settings) -> Result<(), StoreError>;
}

pub struct FileSettings {
    path: PathBuf,
    secrets: Box<dyn Secrets>,
}

impl FileSettings {
    #[must_use]
    pub fn new(path: PathBuf, secrets: Box<dyn Secrets>) -> Self {
        Self { path, secrets }
    }

    pub fn load(&self, home: &Path) -> Result<Loaded, StoreError> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Loaded::Missing),
            Err(error) => return Err(StoreError::Read { path: self.path.clone(), error }),
        };
        let corrupt = |reason: String| StoreError::Corrupt { path: self.path.clone(), reason };
        let file: SettingsFile =
            toml::from_str(&raw).map_err(|error| corrupt(error.message().to_owned()))?;
        let token = self.secrets.read()?.unwrap_or_default();
        let draft = file.to_draft(token).map_err(|error| corrupt(error.message))?;
        Ok(match draft.parse(home) {
            Ok(settings) => Loaded::Ready(settings),
            Err(errors) => Loaded::Incomplete { draft, errors },
        })
    }
}

impl SettingsStore for FileSettings {
    fn save(&self, settings: &Settings) -> Result<(), StoreError> {
        let write = |error| StoreError::Write { path: self.path.clone(), error };
        let text = toml::to_string_pretty(&SettingsFile::from_settings(settings))
            .map_err(|error| write(io::Error::other(error)))?;
        // The token goes first: a file pointing at a token that was never stored is worse.
        self.secrets.write(settings.telegram.token.expose())?;
        write_atomic(&self.path, text.as_bytes()).map_err(write)
    }
}
```

`src/topics.rs`:

```rust
//! `topics.json`, in the format shared with the Python version.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hub_core::topics::{Topics, dump_state, parse_state};
use hub_telegram::hub::TopicStore;

use crate::atomic::write_atomic;
use crate::error::StoreError;

pub fn load_topics(path: &Path) -> Result<Topics, StoreError> {
    match fs::read_to_string(path) {
        Ok(raw) => parse_state(&raw).map_err(|error| StoreError::Corrupt {
            path: path.to_path_buf(),
            reason: error.to_string(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Topics::new()),
        Err(error) => Err(StoreError::Read { path: path.to_path_buf(), error }),
    }
}

pub struct FileTopics {
    path: PathBuf,
}

impl FileTopics {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl TopicStore for FileTopics {
    fn save(&mut self, topics: &Topics) -> Result<(), String> {
        let text = dump_state(topics).map_err(|error| error.to_string())?;
        write_atomic(&self.path, text.as_bytes())
            .map_err(|error| format!("{}: {error}", self.path.display()))
    }
}
```

- [ ] **Step 4: Тесты проходят**

Run: `cargo test -p hub-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS (12 тестов).

- [ ] **Step 5: Commit**

```bash
git add crates/hub-app
git commit -m "hub-app: хранилища настроек, токена и тем"
```

---

### Task 3: Лог в файл и в окно

**Files:**
- Create: `crates/hub-app/src/logging.rs`
- Modify: `crates/hub-app/src/lib.rs` (`pub mod logging;`)

**Interfaces:**
- Produces:
  - `logging::Repaint = Arc<dyn Fn() + Send + Sync>`.
  - `logging::LogLine { time: String, level: tracing::Level, text: String, rejected: Option<Rejected> }`, `logging::Rejected { chat: Option<i64>, user: Option<u64> }`.
  - `logging::LogBuffer` (`Clone + Default`, `tracing_subscriber::Layer`): `lines() -> Vec<LogLine>`, `on_change(Repaint)`; ёмкость `CAPACITY = 500`.
  - `logging::init(&Path logs, LogBuffer) -> Result<WorkerGuard, LogError>`.

- [ ] **Step 1: Падающие тесты**

```rust
#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    fn capture(emit: impl FnOnce()) -> Vec<LogLine> {
        let buffer = LogBuffer::default();
        let subscriber = tracing_subscriber::registry().with(buffer.clone());
        tracing::subscriber::with_default(subscriber, emit);
        buffer.lines()
    }

    #[test]
    fn message_and_fields_form_the_text() {
        let lines = capture(|| tracing::info!(turns = 3, "turn finished"));
        let [line] = lines.as_slice() else { panic!("one line expected: {lines:?}") };
        assert_eq!(line.level, Level::INFO);
        assert_eq!(line.text, "turn finished turns=3");
        assert_eq!(line.rejected, None);
    }

    #[test]
    fn rejected_update_carries_its_ids() {
        let lines = capture(|| {
            tracing::warn!(chat_id = -100_i64, user_id = Some(5_u64), "rejected update");
            tracing::warn!(chat_id = Some(-7_i64), user_id = 9_u64, "rejected update");
        });
        let rejected: Vec<_> = lines.iter().map(|line| line.rejected).collect();
        assert_eq!(
            rejected,
            [
                Some(Rejected { chat: Some(-100), user: Some(5) }),
                Some(Rejected { chat: Some(-7), user: Some(9) }),
            ]
        );
    }

    #[test]
    fn only_the_last_lines_are_kept() {
        let lines = capture(|| {
            for index in 0..CAPACITY + 3 {
                tracing::info!(index, "tick");
            }
        });
        assert_eq!(lines.len(), CAPACITY);
        assert_eq!(lines.first().map(|line| line.text.as_str()), Some("tick index=3"));
    }

    #[test]
    fn change_wakes_the_window() {
        let buffer = LogBuffer::default();
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&woken);
        buffer.on_change(Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let subscriber = tracing_subscriber::registry().with(buffer.clone());
        tracing::subscriber::with_default(subscriber, || tracing::info!("one"));
        assert_eq!(woken.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
```

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-app logging`
Expected: FAIL — `cannot find type LogBuffer`.

- [ ] **Step 3: Реализация**

```rust
//! Logs go to a daily file and to a ring buffer the window shows.

use std::collections::VecDeque;
use std::fmt::{self, Write as _};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{self, Rotation};
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, fmt as layer_fmt};

pub const CAPACITY: usize = 500;
const KEPT_FILES: usize = 14;
const REJECTED: &str = "rejected update";

pub type Repaint = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejected {
    pub chat: Option<i64>,
    pub user: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    pub time: String,
    pub level: Level,
    pub text: String,
    pub rejected: Option<Rejected>,
}

#[derive(Clone, Default)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<LogLine>>>,
    repaint: Arc<OnceLock<Repaint>>,
}

impl LogBuffer {
    #[must_use]
    pub fn lines(&self) -> Vec<LogLine> {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner).iter().cloned().collect()
    }

    /// The window exists only after logging starts, so it subscribes later.
    pub fn on_change(&self, repaint: Repaint) {
        // A second subscriber is never registered; ignoring it keeps the first.
        let _ = self.repaint.set(repaint);
    }

    fn push(&self, line: LogLine) {
        {
            let mut lines = self.lines.lock().unwrap_or_else(PoisonError::into_inner);
            if lines.len() == CAPACITY {
                lines.pop_front();
            }
            lines.push_back(line);
        }
        if let Some(repaint) = self.repaint.get() {
            repaint();
        }
    }
}

#[derive(Default)]
struct Fields {
    message: String,
    rest: String,
    chat: Option<i64>,
    user: Option<u64>,
}

impl Fields {
    fn extra(&mut self, field: &Field, value: &dyn fmt::Display) {
        // Writing into a String cannot fail.
        let _ = write!(self.rest, " {}={value}", field.name());
    }
}

impl Visit for Fields {
    fn record_i64(&mut self, field: &Field, value: i64) {
        if field.name() == "chat_id" {
            self.chat = Some(value);
        }
        self.extra(field, &value);
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "user_id" => self.user = Some(value),
            "chat_id" => self.chat = i64::try_from(value).ok(),
            _other => {}
        }
        self.extra(field, &value);
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            value.clone_into(&mut self.message);
        } else {
            self.extra(field, &value);
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.extra(field, &format_args!("{value:?}"));
        }
    }
}

impl<S: Subscriber> Layer<S> for LogBuffer {
    fn on_event(&self, event: &Event<'_>, _context: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let mut time = String::new();
        // Formatting the clock into a String cannot fail.
        let _ = SystemTime.format_time(&mut Writer::new(&mut time));
        let rejected = (fields.message == REJECTED)
            .then_some(Rejected { chat: fields.chat, user: fields.user });
        self.push(LogLine {
            time,
            level: *event.metadata().level(),
            text: format!("{}{}", fields.message, fields.rest),
            rejected,
        });
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("не удалось открыть каталог логов: {0}")]
    Appender(#[from] rolling::InitError),
    #[error("логирование уже настроено: {0}")]
    Subscriber(#[from] tracing_subscriber::util::TryInitError),
}

fn filter() -> Targets {
    // HTTP internals log request URLs, and Telegram's carry the bot token.
    Targets::new()
        .with_default(Level::INFO)
        .with_target("reqwest", Level::WARN)
        .with_target("hyper", Level::WARN)
        .with_target("hyper_util", Level::WARN)
}

pub fn init(logs: &Path, buffer: LogBuffer) -> Result<WorkerGuard, LogError> {
    let appender = rolling::Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("agent-hub")
        .filename_suffix("log")
        .max_log_files(KEPT_FILES)
        .build(logs)?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::registry()
        .with(layer_fmt::layer().with_writer(writer).with_ansi(false).with_filter(filter()))
        .with(buffer.with_filter(filter()))
        .try_init()?;
    Ok(guard)
}
```

- [ ] **Step 4: Тесты проходят**

Run: `cargo test -p hub-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS (16 тестов).

- [ ] **Step 5: Commit**

```bash
git add crates/hub-app
git commit -m "hub-app: лог в файл и кольцевой буфер для окна"
```

---

### Task 4: Супервизор

**Files:**
- Create: `crates/hub-app/src/supervisor.rs`
- Modify: `crates/hub-app/src/lib.rs` (`pub mod supervisor;`)

**Interfaces:**
- Consumes: `config::{Loaded, SettingsStore}`, `error::StoreError`, `logging::Repaint`, `hub_telegram::hub::{HubMessage, TopicView}`, `hub_core::settings::{Draft, Settings}`.
- Produces:
  - `supervisor::Command::{Save(Box<Draft>), Start, Stop, StopTopic(TopicKey), ResetTopic(TopicKey), Quit(oneshot::Sender<()>)}`.
  - `supervisor::BotStatus::{Unconfigured, Stopped, Starting, Running { username: String }, Failed(String)}`.
  - `supervisor::Snapshot { bot: BotStatus, claude: Option<String>, topics: Vec<TopicView>, saved: Option<Draft>, notice: Option<String> }` (`Default`: `Unconfigured`, пусто).
  - `supervisor::{Activity::{Running, Stopped, Failed}, Apply::{Start, Restart, Live}, plan(Option<&Settings>, &Settings, Activity) -> Apply}`.
  - `supervisor::Started { username: String, claude: String, mailbox: mpsc::Sender<HubMessage>, views: watch::Receiver<Vec<TopicView>>, stop: Stopper }`, `Stopper = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>`.
  - `supervisor::Connector` (trait): `type Error: std::fmt::Display + Send;` и `connect(&self, watch::Receiver<Arc<Settings>>) -> BoxFuture<'_, Result<Started, Self::Error>>`. Ошибка — ассоциированный тип: супервизор её только показывает, конкретный тип (`StartError`) задаёт коннектор из Task 5.
  - `supervisor::{SupervisorSetup { connector, store: Box<dyn SettingsStore>, home: PathBuf, loaded: Result<Loaded, StoreError>, snapshot: watch::Sender<Snapshot>, repaint: Repaint }, Supervisor::new(SupervisorSetup<C>), Supervisor::run(self, mpsc::Receiver<Command>)}`.
  - `supervisor::STOP_TIMEOUT: Duration = 15 s`.

- [ ] **Step 1: Падающие тесты**

```rust
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use hub_core::domain::{ChatId, ThreadId};
    use rstest::rstest;

    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Outcome {
        Connect,
        Fail,
        Hang,
    }

    #[derive(Default)]
    struct Probe {
        connects: AtomicUsize,
        stops: AtomicUsize,
        messages: StdMutex<Vec<String>>,
        settings: StdMutex<Option<watch::Receiver<Arc<Settings>>>>,
        views: StdMutex<Option<watch::Sender<Vec<TopicView>>>>,
    }

    struct FakeConnector {
        probe: Arc<Probe>,
        outcomes: StdMutex<VecDeque<Outcome>>,
    }

    impl Connector for FakeConnector {
        type Error = String;

        fn connect(
            &self,
            settings: watch::Receiver<Arc<Settings>>,
        ) -> BoxFuture<'_, Result<Started, String>> {
            self.probe.connects.fetch_add(1, Ordering::SeqCst);
            *self.probe.settings.lock().unwrap() = Some(settings);
            let outcome = self.outcomes.lock().unwrap().pop_front().unwrap_or(Outcome::Connect);
            let probe = Arc::clone(&self.probe);
            Box::pin(async move {
                if outcome == Outcome::Fail {
                    return Err("Telegram не принял токен бота".to_owned());
                }
                let (mailbox, mut inbox) = mpsc::channel(8);
                let recorder = Arc::clone(&probe);
                tokio::spawn(async move {
                    while let Some(message) = inbox.recv().await {
                        let line = match message {
                            HubMessage::Stop(key) => format!("stop {}/{}", key.chat.0, key.thread.0),
                            HubMessage::Reset(key) => format!("reset {}/{}", key.chat.0, key.thread.0),
                            HubMessage::Inbound(_)
                            | HubMessage::Press(_)
                            | HubMessage::Deliver { .. }
                            | HubMessage::FlushAlbum(_)
                            | HubMessage::Bind { .. }
                            | HubMessage::Ended { .. }
                            | HubMessage::Shutdown(_) => "other".to_owned(),
                        };
                        recorder.messages.lock().unwrap().push(line);
                    }
                });
                let (views, watched) = watch::channel(Vec::new());
                *probe.views.lock().unwrap() = Some(views);
                let stopper = Arc::clone(&probe);
                let stop: Stopper = Box::new(move || {
                    Box::pin(async move {
                        stopper.stops.fetch_add(1, Ordering::SeqCst);
                        if outcome == Outcome::Hang {
                            std::future::pending::<()>().await;
                        }
                    })
                });
                Ok(Started {
                    username: "hub_bot".to_owned(),
                    claude: "2.1.287".to_owned(),
                    mailbox,
                    views: watched,
                    stop,
                })
            })
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        saved: Arc<StdMutex<Vec<Settings>>>,
        broken: bool,
    }

    impl SettingsStore for MemoryStore {
        fn save(&self, settings: &Settings) -> Result<(), StoreError> {
            if self.broken {
                return Err(StoreError::Write {
                    path: "settings.toml".into(),
                    error: std::io::Error::other("диск полон"),
                });
            }
            self.saved.lock().unwrap().push(settings.clone());
            Ok(())
        }
    }

    fn draft(chat: &str, model: &str) -> Draft {
        Draft {
            token: "123:secret-token".to_owned(),
            chat: chat.to_owned(),
            users: "1".to_owned(),
            workspace_root: "~/projects".to_owned(),
            model: model.to_owned(),
            ..Draft::default()
        }
    }

    fn home() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\Users\me" } else { "/home/me" })
    }

    fn settings(chat: &str, model: &str) -> Settings {
        draft(chat, model).parse(&home()).unwrap()
    }

    struct Harness {
        commands: mpsc::Sender<Command>,
        snapshot: watch::Receiver<Snapshot>,
        probe: Arc<Probe>,
        saved: Arc<StdMutex<Vec<Settings>>>,
    }

    impl Harness {
        async fn send(&self, command: Command) {
            self.commands.send(command).await.unwrap();
        }

        async fn until(&mut self, what: &str, done: impl Fn(&Snapshot) -> bool) {
            let wait = self.snapshot.wait_for(|snapshot| done(snapshot));
            tokio::time::timeout(Duration::from_secs(5), wait)
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .unwrap();
        }

        fn connects(&self) -> usize {
            self.probe.connects.load(Ordering::SeqCst)
        }

        fn stops(&self) -> usize {
            self.probe.stops.load(Ordering::SeqCst)
        }
    }

    fn running(snapshot: &Snapshot) -> bool {
        matches!(snapshot.bot, BotStatus::Running { .. })
    }

    fn harness(loaded: Result<Loaded, StoreError>, outcomes: &[Outcome], broken: bool) -> Harness {
        let probe = Arc::new(Probe::default());
        let store = MemoryStore { broken, ..MemoryStore::default() };
        let saved = Arc::clone(&store.saved);
        let (snapshot, watched) = watch::channel(Snapshot::default());
        let (commands, inbox) = mpsc::channel(8);
        let supervisor = Supervisor::new(SupervisorSetup {
            connector: FakeConnector {
                probe: Arc::clone(&probe),
                outcomes: StdMutex::new(outcomes.iter().copied().collect()),
            },
            store: Box::new(store),
            home: home(),
            loaded,
            snapshot,
            repaint: Arc::new(|| {}),
        });
        tokio::spawn(supervisor.run(inbox));
        Harness { commands, snapshot: watched, probe, saved }
    }

    #[rstest]
    #[case(None, settings("-100", ""), Activity::Stopped, Apply::Start)]
    #[case(Some(settings("-100", "")), settings("-200", ""), Activity::Running, Apply::Restart)]
    #[case(Some(settings("-100", "")), settings("-100", "opus"), Activity::Running, Apply::Live)]
    #[case(Some(settings("-100", "")), settings("-200", ""), Activity::Stopped, Apply::Live)]
    #[case(Some(settings("-100", "")), settings("-100", "opus"), Activity::Failed, Apply::Start)]
    fn changes_are_planned(
        #[case] previous: Option<Settings>,
        #[case] next: Settings,
        #[case] activity: Activity,
        #[case] expected: Apply,
    ) {
        assert_eq!(plan(previous.as_ref(), &next, activity), expected);
    }

    #[tokio::test]
    async fn missing_settings_leave_the_bot_unconfigured() {
        let mut harness = harness(Ok(Loaded::Missing), &[], false);
        harness.until("first snapshot", |s| s.bot == BotStatus::Unconfigured).await;
        tokio::task::yield_now().await;
        assert_eq!(harness.connects(), 0);
    }

    #[tokio::test]
    async fn ready_settings_start_at_launch() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let snapshot = harness.snapshot.borrow().clone();
        assert_eq!(snapshot.bot, BotStatus::Running { username: "hub_bot".to_owned() });
        assert_eq!(snapshot.claude.as_deref(), Some("2.1.287"));
        assert_eq!(snapshot.saved, Some(Draft::from_settings(&settings("-100", ""))));
    }

    #[tokio::test]
    async fn first_save_stores_and_starts_the_bot() {
        let mut harness = harness(Ok(Loaded::Missing), &[], false);
        harness.send(Command::Save(Box::new(draft("-100", "")))).await;
        harness.until("running", running).await;
        assert_eq!(harness.saved.lock().unwrap().as_slice(), [settings("-100", "")]);
        assert_eq!(harness.connects(), 1);
    }

    #[tokio::test]
    async fn telegram_change_restarts_a_running_bot() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        harness.send(Command::Save(Box::new(draft("-200", "")))).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness.connects() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        harness.until("running again", running).await;
        assert_eq!(harness.stops(), 1);
    }

    #[tokio::test]
    async fn other_changes_apply_without_restart() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        harness.send(Command::Save(Box::new(draft("-100", "opus")))).await;
        harness.until("saved", |s| s.saved.as_ref().is_some_and(|d| d.model == "opus")).await;
        let live = harness.probe.settings.lock().unwrap().clone().unwrap();
        assert_eq!(live.borrow().claude.model.as_deref(), Some("opus"));
        assert_eq!((harness.connects(), harness.stops()), (1, 0));
    }

    #[tokio::test]
    async fn failed_start_is_reported_and_retried_on_save() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::Fail], false);
        harness.until("failed", |s| matches!(s.bot, BotStatus::Failed(_))).await;
        assert_eq!(
            harness.snapshot.borrow().bot,
            BotStatus::Failed("Telegram не принял токен бота".to_owned())
        );
        harness.send(Command::Save(Box::new(draft("-100", "opus")))).await;
        harness.until("running", running).await;
        assert_eq!(harness.connects(), 2);
    }

    #[tokio::test]
    async fn stopped_bot_stays_stopped_on_save() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        harness.send(Command::Stop).await;
        harness.until("stopped", |s| s.bot == BotStatus::Stopped).await;
        harness.send(Command::Save(Box::new(draft("-200", "")))).await;
        harness.until("saved", |s| s.saved.as_ref().is_some_and(|d| d.chat == "-200")).await;
        assert_eq!(harness.snapshot.borrow().bot, BotStatus::Stopped);
        assert_eq!(harness.connects(), 1);
        harness.send(Command::Start).await;
        harness.until("running", running).await;
        assert_eq!(harness.connects(), 2);
    }

    #[tokio::test]
    async fn store_failure_is_shown_and_nothing_starts() {
        let mut harness = harness(Ok(Loaded::Missing), &[], true);
        harness.send(Command::Save(Box::new(draft("-100", "")))).await;
        harness.until("notice", |s| s.notice.is_some()).await;
        assert!(harness.snapshot.borrow().notice.as_deref().unwrap().contains("диск полон"));
        assert_eq!(harness.snapshot.borrow().bot, BotStatus::Unconfigured);
        assert_eq!(harness.connects(), 0);
    }

    #[tokio::test]
    async fn load_failure_is_shown() {
        let error = StoreError::Corrupt { path: "settings.toml".into(), reason: "плохой TOML".into() };
        let mut harness = harness(Err(error), &[], false);
        harness.until("notice", |s| s.notice.is_some()).await;
        assert_eq!(
            harness.snapshot.borrow().notice.as_deref(),
            Some("settings.toml повреждён: плохой TOML")
        );
    }

    #[tokio::test]
    async fn topic_commands_reach_the_hub() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        harness.send(Command::StopTopic(key)).await;
        harness.send(Command::ResetTopic(key)).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness.probe.messages.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(*harness.probe.messages.lock().unwrap(), ["stop -100/7", "reset -100/7"]);
    }

    #[tokio::test]
    async fn views_are_published() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let view = TopicView {
            key: TopicKey { chat: ChatId(-100), thread: ThreadId(7) },
            title: Some("backend".to_owned()),
            session: hub_core::domain::TopicSession::fresh(
                hub_core::domain::BackendKind::Claude,
                hub_core::domain::AbsolutePath::new(home()).unwrap(),
            ),
            state: hub_telegram::hub::TopicState::Running,
        };
        harness.probe.views.lock().unwrap().as_ref().unwrap().send_replace(vec![view.clone()]);
        harness.until("topics", |s| s.topics == [view.clone()]).await;
    }

    #[tokio::test(start_paused = true)]
    async fn quit_finishes_even_if_the_bot_hangs() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::Hang], false);
        harness.until("running", running).await;
        let (done, stopped) = oneshot::channel();
        harness.send(Command::Quit(done)).await;
        tokio::time::timeout(STOP_TIMEOUT + Duration::from_secs(1), stopped).await.unwrap().unwrap();
        assert_eq!(harness.stops(), 1);
    }

    #[tokio::test]
    async fn snapshot_debug_hides_the_token() {
        let mut harness = harness(Ok(Loaded::Missing), &[], false);
        harness.send(Command::Save(Box::new(draft("-100", "")))).await;
        harness.until("running", running).await;
        assert!(!format!("{:?}", *harness.snapshot.borrow()).contains("secret-token"));
    }
}
```

Перезапуск в `telegram_change_restarts_a_running_bot` проверяется счётчиками `connects` и `stops`: статус проходит Running → Stopped → Starting → Running слишком быстро, чтобы ловить промежуточные снимки.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-app supervisor`
Expected: FAIL — `cannot find type Supervisor`.

- [ ] **Step 3: Реализация**

```rust
//! The core behind the window: owns the live settings and the bot, applies commands from the
//! window, and publishes what the window shows.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_core::domain::TopicKey;
use hub_core::settings::{Draft, Settings};
use hub_telegram::hub::{HubMessage, TopicView};
use tokio::sync::{mpsc, oneshot, watch};

use crate::config::{Loaded, SettingsStore};
use crate::error::StoreError;
use crate::logging::Repaint;

/// The hub itself gives up on sessions after 10 s; the rest covers the dispatcher.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(15);

pub enum Command {
    Save(Box<Draft>),
    Start,
    Stop,
    StopTopic(TopicKey),
    ResetTopic(TopicKey),
    Quit(oneshot::Sender<()>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotStatus {
    Unconfigured,
    Stopped,
    Starting,
    Running { username: String },
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub bot: BotStatus,
    pub claude: Option<String>,
    pub topics: Vec<TopicView>,
    /// The settings as last saved: the form's baseline.
    pub saved: Option<Draft>,
    pub notice: Option<String>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self { bot: BotStatus::Unconfigured, claude: None, topics: Vec::new(), saved: None, notice: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Running,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    Start,
    Restart,
    Live,
}

/// What saving `next` does to the bot.
#[must_use]
pub fn plan(previous: Option<&Settings>, next: &Settings, activity: Activity) -> Apply {
    match (previous, activity) {
        (None, Activity::Running | Activity::Stopped | Activity::Failed)
        | (Some(_), Activity::Failed) => Apply::Start,
        (Some(old), Activity::Running) if old.telegram != next.telegram => Apply::Restart,
        (Some(_), Activity::Running | Activity::Stopped) => Apply::Live,
    }
}

pub type Stopper = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

pub struct Started {
    pub username: String,
    pub claude: String,
    pub mailbox: mpsc::Sender<HubMessage>,
    pub views: watch::Receiver<Vec<TopicView>>,
    pub stop: Stopper,
}

pub trait Connector: Send + 'static {
    type Error: std::fmt::Display + Send;

    fn connect(
        &self,
        settings: watch::Receiver<Arc<Settings>>,
    ) -> BoxFuture<'_, Result<Started, Self::Error>>;
}

pub struct SupervisorSetup<C> {
    pub connector: C,
    pub store: Box<dyn SettingsStore>,
    pub home: PathBuf,
    pub loaded: Result<Loaded, StoreError>,
    pub snapshot: watch::Sender<Snapshot>,
    pub repaint: Repaint,
}

struct Bot {
    mailbox: mpsc::Sender<HubMessage>,
    stop: Stopper,
}

pub struct Supervisor<C> {
    connector: C,
    store: Box<dyn SettingsStore>,
    home: PathBuf,
    settings: Option<watch::Sender<Arc<Settings>>>,
    bot: Option<Bot>,
    views: Option<watch::Receiver<Vec<TopicView>>>,
    state: Snapshot,
    snapshot: watch::Sender<Snapshot>,
    repaint: Repaint,
}

enum Event {
    Command(Option<Command>),
    Views { alive: bool },
}

impl<C: Connector> Supervisor<C> {
    #[must_use]
    pub fn new(setup: SupervisorSetup<C>) -> Self {
        let SupervisorSetup { connector, store, home, loaded, snapshot, repaint } = setup;
        let mut state = Snapshot::default();
        let settings = match loaded {
            Ok(Loaded::Missing) => None,
            Ok(Loaded::Ready(settings)) => {
                state.saved = Some(Draft::from_settings(&settings));
                Some(watch::Sender::new(Arc::new(settings)))
            }
            Ok(Loaded::Incomplete { draft, errors: _errors }) => {
                state.saved = Some(draft);
                None
            }
            Err(error) => {
                state.notice = Some(error.to_string());
                None
            }
        };
        Self { connector, store, home, settings, bot: None, views: None, state, snapshot, repaint }
    }

    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        self.publish();
        if self.settings.is_some() {
            self.start().await;
        }
        loop {
            let mut views = self.views.take();
            let changed = async {
                match views.as_mut() {
                    Some(views) => views.changed().await.is_ok(),
                    None => std::future::pending().await,
                }
            };
            let event = tokio::select! {
                biased;
                command = commands.recv() => Event::Command(command),
                alive = changed => Event::Views { alive },
            };
            self.views = views;
            match event {
                Event::Command(Some(Command::Quit(done))) => {
                    self.stop().await;
                    // The window may have stopped waiting; there is nobody else to tell.
                    let _ = done.send(());
                    return;
                }
                Event::Command(None) => {
                    self.stop().await;
                    return;
                }
                Event::Command(Some(command)) => self.handle(command).await,
                Event::Views { alive: true } => self.refresh_topics(),
                Event::Views { alive: false } => {
                    tracing::error!("hub stopped on its own");
                    self.stop().await;
                    self.status(BotStatus::Failed("бот остановился, подробности в логе".to_owned()));
                }
            }
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::Save(draft) => self.save(*draft).await,
            Command::Start => {
                if self.bot.is_none() && self.settings.is_some() {
                    self.start().await;
                }
            }
            Command::Stop => {
                self.stop().await;
                if self.settings.is_some() {
                    self.status(BotStatus::Stopped);
                }
            }
            Command::StopTopic(key) => self.tell(HubMessage::Stop(key)).await,
            Command::ResetTopic(key) => self.tell(HubMessage::Reset(key)).await,
            Command::Quit(done) => {
                // `run` handles Quit before dispatching here.
                let _ = done.send(());
            }
        }
    }

    async fn save(&mut self, draft: Draft) {
        let settings = match draft.parse(&self.home) {
            Ok(settings) => settings,
            Err(errors) => {
                let fields: Vec<_> = errors.iter().map(|error| error.message.as_str()).collect();
                self.notice(Some(format!("Настройки не сохранены: {}", fields.join("; "))));
                return;
            }
        };
        if let Err(error) = self.store.save(&settings) {
            tracing::error!(%error, "settings could not be saved");
            self.notice(Some(error.to_string()));
            return;
        }
        tracing::info!("settings saved");
        self.state.saved = Some(draft);
        self.state.notice = None;
        let previous = self.settings.as_ref().map(|live| Arc::clone(&live.borrow()));
        let apply = plan(previous.as_deref(), &settings, self.activity());
        match &self.settings {
            Some(live) => {
                live.send_replace(Arc::new(settings));
            }
            None => self.settings = Some(watch::Sender::new(Arc::new(settings))),
        }
        self.publish();
        match apply {
            Apply::Start => self.start().await,
            Apply::Restart => {
                tracing::info!("telegram settings changed, restarting the bot");
                self.stop().await;
                self.start().await;
            }
            Apply::Live => {}
        }
    }

    fn activity(&self) -> Activity {
        match (&self.bot, &self.state.bot) {
            (Some(_), _) => Activity::Running,
            (None, BotStatus::Failed(_)) => Activity::Failed,
            (
                None,
                BotStatus::Unconfigured
                | BotStatus::Stopped
                | BotStatus::Starting
                | BotStatus::Running { .. },
            ) => Activity::Stopped,
        }
    }

    async fn start(&mut self) {
        let Some(live) = &self.settings else { return };
        let receiver = live.subscribe();
        self.status(BotStatus::Starting);
        match self.connector.connect(receiver).await {
            Ok(Started { username, claude, mailbox, views, stop }) => {
                tracing::info!(%username, %claude, "bot started");
                self.bot = Some(Bot { mailbox, stop });
                self.views = Some(views);
                self.state.claude = Some(claude);
                self.refresh_topics();
                self.status(BotStatus::Running { username });
            }
            Err(error) => {
                tracing::warn!(%error, "bot did not start");
                self.status(BotStatus::Failed(error.to_string()));
            }
        }
    }

    async fn stop(&mut self) {
        self.views = None;
        let Some(Bot { mailbox: _mailbox, stop }) = self.bot.take() else { return };
        if tokio::time::timeout(STOP_TIMEOUT, stop()).await.is_err() {
            tracing::error!("bot did not stop in time");
        }
        self.state.topics.clear();
        self.status(BotStatus::Stopped);
    }

    async fn tell(&self, message: HubMessage) {
        if let Some(bot) = &self.bot
            && bot.mailbox.send(message).await.is_err()
        {
            tracing::warn!("topic command dropped: the hub has stopped");
        }
    }

    fn refresh_topics(&mut self) {
        if let Some(views) = self.views.as_mut() {
            self.state.topics = views.borrow_and_update().clone();
            self.publish();
        }
    }

    fn status(&mut self, bot: BotStatus) {
        self.state.bot = bot;
        self.publish();
    }

    fn notice(&mut self, notice: Option<String>) {
        self.state.notice = notice;
        self.publish();
    }

    fn publish(&self) {
        self.snapshot.send_replace(self.state.clone());
        (self.repaint)();
    }
}
```

`Draft` и `TopicView` должны реализовывать `Eq`, чтобы `Snapshot: Eq`; если clippy или компилятор возразят (например, `TopicView` без `Eq`), снять `Eq` со `Snapshot` и записать решение в ledger.

- [ ] **Step 4: Тесты проходят**

Run: `cargo test -p hub-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS (33 теста; 5 из них — случаи `changes_are_planned`).

- [ ] **Step 5: Commit**

```bash
git add crates/hub-app
git commit -m "hub-app: супервизор бота — сохранение, запуск, перезапуск, остановка"
```

---

### Task 5: Боевой коннектор

**Files:**
- Create: `crates/hub-app/src/connector.rs`
- Modify: `crates/hub-app/src/lib.rs` (`pub mod connector;`)

**Interfaces:**
- Consumes: `supervisor::{Connector, Started, Stopper}`, `topics::{load_topics, FileTopics}`, `error::StoreError`; `hub_telegram::{paths::{workspace_root, CwdError}, telegram::{self, Connection, Listener}, hub::{self, HubHandle, HubMessage, HubSetup}, agents::ClaudeAgents}`; `hub_claude::version::{locate, check, CliError}`.
- Produces: `connector::StartError::{Workspace(CwdError), Topics(StoreError), Claude(CliError), Telegram(telegram::StartError)}`; `connector::TelegramConnector { home: PathBuf, topics: PathBuf }` (`Connector<Error = StartError>`).

Порядок проверок дешёвые → дорогие, и сеть последней: корень workspace → `topics.json` → `claude` → Telegram. Благодаря этому первые две ветки проверяются тестами без сети и CLI.

- [ ] **Step 1: Падающие тесты**

```rust
#[cfg(test)]
mod tests {
    use hub_core::settings::Draft;

    use super::*;

    fn settings(root: &std::path::Path) -> watch::Receiver<Arc<Settings>> {
        let settings = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            ..Draft::default()
        }
        .parse(root)
        .unwrap();
        watch::Sender::new(Arc::new(settings)).subscribe()
    }

    #[tokio::test]
    async fn missing_workspace_root_stops_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let connector = TelegramConnector {
            home: dir.path().to_path_buf(),
            topics: dir.path().join("topics.json"),
        };
        let error = connector.connect(settings(&dir.path().join("нет"))).await.err().unwrap();
        assert!(matches!(error, StartError::Workspace(_)));
    }

    #[tokio::test]
    async fn corrupt_topics_stop_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let topics = dir.path().join("topics.json");
        std::fs::write(&topics, "не json").unwrap();
        let connector = TelegramConnector { home: dir.path().to_path_buf(), topics: topics.clone() };
        let error = connector.connect(settings(dir.path())).await.err().unwrap();
        assert!(matches!(error, StartError::Topics(StoreError::Corrupt { .. })));
        assert!(error.to_string().contains("topics.json"));
        assert_eq!(std::fs::read_to_string(&topics).unwrap(), "не json");
    }
}
```

`Started` не реализует `Debug`, поэтому тесты используют `.err().unwrap()`, а не `unwrap_err()`.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-app connector`
Expected: FAIL — `cannot find type TelegramConnector`.

- [ ] **Step 3: Реализация**

```rust
//! Starts the real bot: checks the workspace, topics and agent CLI, connects to Telegram,
//! then runs the hub and the update listener.

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use hub_claude::version::{CliError, check, locate};
use hub_core::settings::Settings;
use hub_telegram::agents::ClaudeAgents;
use hub_telegram::hub::{self, HubHandle, HubMessage, HubSetup};
use hub_telegram::paths::{CwdError, workspace_root};
use hub_telegram::telegram::{self, Connection, Listener};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::error::StoreError;
use crate::supervisor::{Connector, Started};
use crate::topics::{FileTopics, load_topics};

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Workspace(#[from] CwdError),
    #[error(transparent)]
    Topics(#[from] StoreError),
    #[error(transparent)]
    Claude(#[from] CliError),
    #[error(transparent)]
    Telegram(#[from] telegram::StartError),
}

pub struct TelegramConnector {
    pub home: PathBuf,
    pub topics: PathBuf,
}

impl Connector for TelegramConnector {
    type Error = StartError;

    fn connect(
        &self,
        settings: watch::Receiver<Arc<Settings>>,
    ) -> BoxFuture<'_, Result<Started, StartError>> {
        Box::pin(async move {
            let current = Arc::clone(&settings.borrow());
            workspace_root(&current.workspace_root)?;
            let topics = load_topics(&self.topics)?;
            let cli = locate(current.claude.cli.as_deref())?;
            let version = check(&cli).await?;
            let Connection { bot, messenger, username } =
                telegram::connect(&current.telegram.token, current.telegram.chat).await?;
            let HubHandle { mailbox, views, registries: _registries, task } =
                hub::spawn(HubSetup {
                    agents: Arc::new(ClaudeAgents::new(settings.clone())),
                    messenger,
                    settings,
                    topics,
                    store: Box::new(FileTopics::new(self.topics.clone())),
                    home: self.home.clone(),
                    bot: username.clone(),
                });
            let listener = telegram::listen(bot, current.telegram.clone(), mailbox.clone());
            let hub = mailbox.clone();
            Ok(Started {
                username,
                claude: version.to_string(),
                mailbox,
                views,
                stop: Box::new(move || Box::pin(shutdown(hub, listener, task))),
            })
        })
    }
}

/// Sessions first, so they can still post «⏹ Остановлено»; then the update listener.
async fn shutdown(hub: mpsc::Sender<HubMessage>, listener: Listener, task: JoinHandle<()>) {
    let (done, stopped) = oneshot::channel();
    if hub.send(HubMessage::Shutdown(done)).await.is_ok() {
        // An error means the hub already exited, which is what we wait for anyway.
        let _ = stopped.await;
    }
    listener.stop().await;
    if let Err(error) = task.await {
        tracing::error!(%error, "hub crashed");
    }
}
```

`messenger` из `Connection` — `Arc<TelegramMessenger>`; в поле `HubSetup::messenger: Arc<dyn Messenger>` он приводится неявно. Если компилятор не выведет приведение, написать `messenger: messenger as Arc<dyn Messenger>` и импортировать `hub_telegram::messenger::Messenger`.

- [ ] **Step 4: Тесты проходят**

Run: `cargo test -p hub-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS (35 тестов).

- [ ] **Step 5: Commit**

```bash
git add crates/hub-app
git commit -m "hub-app: запуск бота — проверки, Telegram, актор и приём апдейтов"
```

---

### Task 6: Внешний вид статуса и трей

**Files:**
- Create: `crates/hub-app/src/gui/mod.rs`, `crates/hub-app/src/gui/look.rs`, `crates/hub-app/src/gui/tray.rs`
- Modify: `crates/hub-app/src/lib.rs` (`pub mod gui;`)

**Interfaces:**
- Consumes: `supervisor::{BotStatus, Snapshot}`, `logging::Repaint`, `hub_telegram::hub::TopicState`, `hub_core::settings::Draft`.
- Produces:
  - `gui::look::{Health::{Connected, Stopped, Failed}, health(&BotStatus) -> Health, active(&Snapshot) -> usize, tooltip(&Snapshot) -> String, status_text(&BotStatus) -> String, short_session(&str) -> String, use_chat(&mut Draft, i64), add_user(&mut Draft, u64), telegram_changed(Option<&Draft>, &Draft) -> bool}`.
  - `gui::tray::{Tray::create(Repaint) -> Result<(Tray, std::sync::mpsc::Receiver<TrayAction>), TrayError>, Tray::show(&mut self, &Snapshot), TrayAction::{Open, Toggle, Quit}}`.

- [ ] **Step 1: Падающие тесты (`look.rs`)**

```rust
#[cfg(test)]
mod tests {
    use hub_core::domain::{AbsolutePath, BackendKind, ChatId, ThreadId, TopicKey, TopicSession};
    use hub_telegram::hub::TopicView;
    use rstest::rstest;

    use super::*;

    fn view(thread: i32, state: TopicState) -> TopicView {
        let root = if cfg!(windows) { r"C:\p" } else { "/p" };
        TopicView {
            key: TopicKey { chat: ChatId(-100), thread: ThreadId(thread) },
            title: None,
            session: TopicSession::fresh(
                BackendKind::Claude,
                AbsolutePath::new(root.into()).unwrap(),
            ),
            state,
        }
    }

    #[rstest]
    #[case(BotStatus::Running { username: "b".to_owned() }, Health::Connected)]
    #[case(BotStatus::Starting, Health::Stopped)]
    #[case(BotStatus::Stopped, Health::Stopped)]
    #[case(BotStatus::Unconfigured, Health::Stopped)]
    #[case(BotStatus::Failed("x".to_owned()), Health::Failed)]
    fn status_maps_to_health(#[case] status: BotStatus, #[case] expected: Health) {
        assert_eq!(health(&status), expected);
    }

    #[test]
    fn tooltip_counts_running_sessions() {
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "b".to_owned() },
            topics: vec![view(1, TopicState::Running), view(2, TopicState::Waiting)],
            ..Snapshot::default()
        };
        assert_eq!(tooltip(&snapshot), "agent-hub — подключён · активных сессий: 1");
        assert_eq!(tooltip(&Snapshot::default()), "agent-hub — не настроен");
    }

    #[rstest]
    #[case("abc", "abc")]
    #[case("0123456789abcdef", "01234567…")]
    fn session_ids_are_shortened(#[case] id: &str, #[case] expected: &str) {
        assert_eq!(short_session(id), expected);
    }

    #[rstest]
    #[case("", 5, "5")]
    #[case("1, 2", 5, "1, 2, 5")]
    #[case("1, 5", 5, "1, 5")]
    #[case("15", 5, "15, 5")]
    fn rejected_user_is_added_once(#[case] users: &str, #[case] user: u64, #[case] expected: &str) {
        let mut draft = Draft { users: users.to_owned(), ..Draft::default() };
        add_user(&mut draft, user);
        assert_eq!(draft.users, expected);
    }

    #[test]
    fn rejected_chat_replaces_the_chat() {
        let mut draft = Draft { chat: "-1".to_owned(), ..Draft::default() };
        use_chat(&mut draft, -100);
        assert_eq!(draft.chat, "-100");
    }

    #[rstest]
    #[case(Draft { token: "2:b".to_owned(), ..Draft::default() }, true)]
    #[case(Draft { chat: "-5".to_owned(), ..Draft::default() }, true)]
    #[case(Draft { users: "9".to_owned(), ..Draft::default() }, true)]
    #[case(Draft { model: "opus".to_owned(), ..Draft::default() }, false)]
    fn telegram_fields_are_detected(#[case] draft: Draft, #[case] expected: bool) {
        assert_eq!(telegram_changed(Some(&Draft::default()), &draft), expected);
        assert!(!telegram_changed(None, &draft));
    }
}
```

`telegram_changed(None, _)` — `false`: при первой настройке прерывать нечего.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-app look`
Expected: FAIL — `cannot find function health`.

- [ ] **Step 3: Реализация `look.rs`**

```rust
//! Pure presentation decisions shared by the window and the tray.

use hub_core::settings::Draft;
use hub_telegram::hub::TopicState;

use crate::supervisor::{BotStatus, Snapshot};

const SHORT_SESSION: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Connected,
    Stopped,
    Failed,
}

#[must_use]
pub fn health(status: &BotStatus) -> Health {
    match status {
        BotStatus::Running { .. } => Health::Connected,
        BotStatus::Unconfigured | BotStatus::Stopped | BotStatus::Starting => Health::Stopped,
        BotStatus::Failed(_) => Health::Failed,
    }
}

#[must_use]
pub fn active(snapshot: &Snapshot) -> usize {
    snapshot.topics.iter().filter(|topic| topic.state == TopicState::Running).count()
}

#[must_use]
pub fn tooltip(snapshot: &Snapshot) -> String {
    match &snapshot.bot {
        BotStatus::Running { .. } => {
            format!("agent-hub — подключён · активных сессий: {}", active(snapshot))
        }
        BotStatus::Starting => "agent-hub — подключение…".to_owned(),
        BotStatus::Stopped => "agent-hub — остановлен".to_owned(),
        BotStatus::Unconfigured => "agent-hub — не настроен".to_owned(),
        BotStatus::Failed(_) => "agent-hub — ошибка".to_owned(),
    }
}

#[must_use]
pub fn status_text(status: &BotStatus) -> String {
    match status {
        BotStatus::Unconfigured => "Не настроен: заполните вкладку «Настройки»".to_owned(),
        BotStatus::Stopped => "Остановлен".to_owned(),
        BotStatus::Starting => "Подключение…".to_owned(),
        BotStatus::Running { username } => format!("Подключён как @{username}"),
        BotStatus::Failed(reason) => format!("Ошибка: {reason}"),
    }
}

#[must_use]
pub fn short_session(id: &str) -> String {
    match id.char_indices().nth(SHORT_SESSION) {
        Some((end, _)) => format!("{}…", id.get(..end).unwrap_or(id)),
        None => id.to_owned(),
    }
}

pub fn use_chat(draft: &mut Draft, chat: i64) {
    draft.chat = chat.to_string();
}

pub fn add_user(draft: &mut Draft, user: u64) {
    let user = user.to_string();
    let present = draft
        .users
        .split(|c: char| c == ',' || c.is_whitespace())
        .any(|part| part == user);
    if present {
        return;
    }
    draft.users = if draft.users.trim().is_empty() {
        user
    } else {
        format!("{}, {user}", draft.users.trim_end())
    };
}

/// Saving these restarts the bot and interrupts running sessions.
#[must_use]
pub fn telegram_changed(saved: Option<&Draft>, draft: &Draft) -> bool {
    saved.is_some_and(|saved| {
        saved.token != draft.token || saved.chat != draft.chat || saved.users != draft.users
    })
}
```

- [ ] **Step 4: Реализация `tray.rs` и `gui/mod.rs`**

`gui/mod.rs`:

```rust
pub mod look;
pub mod tray;
```

`gui/tray.rs`:

```rust
//! The tray icon: health colour, tooltip and the menu.

use std::sync::mpsc;

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{BadIcon, Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::gui::look::{Health, health, tooltip};
use crate::logging::Repaint;
use crate::supervisor::{BotStatus, Snapshot};

const SIZE: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    Toggle,
    Quit,
}

#[derive(Debug, thiserror::Error)]
pub enum TrayError {
    #[error("иконка трея: {0}")]
    Icon(#[from] BadIcon),
    #[error("меню трея: {0}")]
    Menu(#[from] tray_icon::menu::Error),
    #[error("трей недоступен: {0}")]
    Tray(#[from] tray_icon::Error),
}

pub struct Tray {
    icon: TrayIcon,
    toggle: MenuItem,
    health: Health,
}

fn circle(rgb: [u8; 3]) -> Result<Icon, BadIcon> {
    let radius = f64::from(SIZE) / 2.0;
    let pixels = (0..SIZE * SIZE)
        .flat_map(|index| {
            let (x, y) = (f64::from(index % SIZE) + 0.5, f64::from(index / SIZE) + 0.5);
            let inside = (x - radius).hypot(y - radius) <= radius - 1.0;
            let [r, g, b] = rgb;
            [r, g, b, if inside { u8::MAX } else { 0 }]
        })
        .collect();
    Icon::from_rgba(pixels, SIZE, SIZE)
}

fn icon(health: Health) -> Result<Icon, BadIcon> {
    match health {
        Health::Connected => circle([46, 160, 67]),
        Health::Stopped => circle([140, 140, 140]),
        Health::Failed => circle([210, 50, 45]),
    }
}

impl Tray {
    pub fn create(repaint: Repaint) -> Result<(Self, mpsc::Receiver<TrayAction>), TrayError> {
        let open = MenuItem::new("Открыть", true, None);
        let toggle = MenuItem::new("Запустить бота", false, None);
        let quit = MenuItem::new("Выход", true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &toggle, &PredefinedMenuItem::separator(), &quit])?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("agent-hub")
            .with_icon(icon(Health::Stopped)?)
            .build()?;
        let (sender, actions) = mpsc::channel();
        let ids: [(MenuId, TrayAction); 3] = [
            (open.id().clone(), TrayAction::Open),
            (toggle.id().clone(), TrayAction::Toggle),
            (quit.id().clone(), TrayAction::Quit),
        ];
        let menu_sender = sender.clone();
        let menu_repaint = repaint.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, action)) = ids.iter().find(|(id, _)| *id == event.id) {
                // The window is gone only while the process exits.
                let _ = menu_sender.send(*action);
                menu_repaint();
            }
        }));
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let open = match event {
                TrayIconEvent::DoubleClick { .. } => true,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => cfg!(not(target_os = "macos")),
                TrayIconEvent::Click { .. }
                | TrayIconEvent::Enter { .. }
                | TrayIconEvent::Move { .. }
                | TrayIconEvent::Leave { .. } => false,
                _ => false,
            };
            if open {
                // The window is gone only while the process exits.
                let _ = sender.send(TrayAction::Open);
                repaint();
            }
        }));
        Ok((Self { icon, toggle, health: Health::Stopped }, actions))
    }

    pub fn show(&mut self, snapshot: &Snapshot) {
        let health = health(&snapshot.bot);
        if health != self.health {
            match icon(health) {
                Ok(icon) => {
                    if let Err(error) = self.icon.set_icon(Some(icon)) {
                        tracing::warn!(%error, "tray icon not updated");
                    }
                }
                Err(error) => tracing::warn!(%error, "tray icon not drawn"),
            }
            self.health = health;
        }
        if let Err(error) = self.icon.set_tooltip(Some(tooltip(snapshot))) {
            tracing::warn!(%error, "tray tooltip not updated");
        }
        let (text, enabled) = match &snapshot.bot {
            BotStatus::Running { .. } | BotStatus::Starting => ("Остановить бота", true),
            BotStatus::Stopped | BotStatus::Failed(_) => ("Запустить бота", true),
            BotStatus::Unconfigured => ("Запустить бота", false),
        };
        self.toggle.set_text(text);
        self.toggle.set_enabled(enabled);
    }
}
```

`TrayIconEvent` — `#[non_exhaustive]` внешнего крейта, поэтому последняя ветка `_ => false` допустима (правило про `_` касается только своих enum). Если варианты `Enter`/`Move`/`Leave` в 0.26 называются иначе, оставить их перечисление по фактическому enum и записать в ledger. Если `menu.append_items` или `tray_icon::menu::Error` отличаются, свериться с `~/.cargo/registry/src/*/muda-0.21.0/src/lib.rs`.

- [ ] **Step 5: Тесты проходят**

Run: `cargo test -p hub-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS (52 теста).

- [ ] **Step 6: Commit**

```bash
git add crates/hub-app
git commit -m "hub-app: состояние для трея и окна, иконка в трее"
```

---

### Task 7: Окно — вкладки Статус, Темы, Лог, Настройки

**Files:**
- Create: `crates/hub-app/src/gui/app.rs`, `crates/hub-app/src/gui/status.rs`, `crates/hub-app/src/gui/topics.rs`, `crates/hub-app/src/gui/log.rs`, `crates/hub-app/src/gui/settings.rs`, `crates/hub-app/src/gui/folder.rs`
- Modify: `crates/hub-app/src/gui/mod.rs`

**Interfaces:**
- Consumes: `supervisor::{Command, Snapshot, BotStatus}`, `logging::{LogBuffer, LogLine}`, `gui::look::*`, `gui::tray::{Tray, TrayAction}`, `hub_core::settings::{Draft, Field, FieldError, PermissionMode, UpdateCheck}`.
- Produces: `gui::app::{HubApp, AppSetup { commands: tokio::sync::mpsc::Sender<Command>, snapshot: watch::Receiver<Snapshot>, logs: LogBuffer, tray: Option<(Tray, std::sync::mpsc::Receiver<TrayAction>)>, home: PathBuf, logs_dir: PathBuf }, HubApp::new(AppSetup) -> HubApp}` (`eframe::App`).

Окно без автотестов (спека: «GUI — без автотестов; логика формы в `hub-core`»). Решения, которые можно проверить, уже вынесены в `look.rs` (Task 6). Проверка — ручная в Task 8.

- [ ] **Step 1: `gui/folder.rs` — открыть каталог в файловом менеджере**

```rust
//! Opens a directory in the system file manager.

use std::io;
use std::path::Path;
use std::process::Command;

pub fn open(path: &Path) -> io::Result<()> {
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    // The file manager outlives us; we do not wait for it.
    Command::new(program).arg(path).spawn().map(drop)
}
```

- [ ] **Step 2: `gui/status.rs`**

```rust
//! The «Статус» tab.

use eframe::egui;

use crate::gui::look::{active, status_text};
use crate::supervisor::{BotStatus, Command, Snapshot};

pub fn show(ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
    ui.heading("Бот");
    ui.label(status_text(&snapshot.bot));
    ui.horizontal(|ui| {
        let (start, stop) = match &snapshot.bot {
            BotStatus::Unconfigured => (false, false),
            BotStatus::Stopped | BotStatus::Failed(_) => (true, false),
            BotStatus::Starting | BotStatus::Running { .. } => (false, true),
        };
        if ui.add_enabled(start, egui::Button::new("▶ Запустить")).clicked() {
            send(Command::Start);
        }
        if ui.add_enabled(stop, egui::Button::new("⏹ Остановить")).clicked() {
            send(Command::Stop);
        }
    });
    ui.separator();
    egui::Grid::new("status").num_columns(2).show(ui, |ui| {
        ui.label("Claude Code");
        ui.label(snapshot.claude.as_deref().unwrap_or("—"));
        ui.end_row();
        ui.label("Активных сессий");
        ui.label(active(snapshot).to_string());
        ui.end_row();
        ui.label("Тем");
        ui.label(snapshot.topics.len().to_string());
        ui.end_row();
    });
    if let Some(notice) = &snapshot.notice {
        ui.separator();
        ui.colored_label(ui.visuals().error_fg_color, notice);
    }
}
```

- [ ] **Step 3: `gui/topics.rs`**

```rust
//! The «Темы» tab.

use eframe::egui;
use hub_core::domain::TopicKey;
use hub_telegram::hub::TopicState;

use crate::gui::look::short_session;
use crate::supervisor::{Command, Snapshot};

#[derive(Default)]
pub struct TopicsView {
    resetting: Option<TopicKey>,
}

impl TopicsView {
    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        if snapshot.topics.is_empty() {
            ui.label("Тем пока нет: они появляются, когда бот запущен и в группе есть сообщения.");
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("topics").num_columns(6).striped(true).show(ui, |ui| {
                for header in ["Тема", "Бэкенд", "Каталог", "Сессия", "Статус", ""] {
                    ui.strong(header);
                }
                ui.end_row();
                for topic in &snapshot.topics {
                    let name = topic.title.clone().unwrap_or_else(|| format!("#{}", topic.key.thread.0));
                    ui.label(name);
                    ui.label(topic.session.backend.name());
                    ui.label(topic.session.cwd.as_path().display().to_string());
                    match &topic.session.session {
                        Some(id) => {
                            let response = ui
                                .add(egui::Label::new(short_session(id.as_str())).sense(egui::Sense::click()))
                                .on_hover_text("Скопировать полный session_id");
                            if response.clicked() {
                                ui.ctx().copy_text(id.as_str().to_owned());
                            }
                        }
                        None => {
                            ui.label("—");
                        }
                    }
                    let running = topic.state == TopicState::Running;
                    ui.label(if running { "работает" } else { "свободна" });
                    ui.horizontal(|ui| {
                        if ui.add_enabled(running, egui::Button::new("Stop")).clicked() {
                            send(Command::StopTopic(topic.key));
                        }
                        if ui.add_enabled(!running, egui::Button::new("Reset")).clicked() {
                            self.resetting = Some(topic.key);
                        }
                    });
                    ui.end_row();
                }
            });
        });
        self.confirm(ui.ctx(), send);
    }

    fn confirm(&mut self, ctx: &egui::Context, send: &mut impl FnMut(Command)) {
        let Some(key) = self.resetting else { return };
        let modal = egui::Modal::new(egui::Id::new("reset")).show(ctx, |ui| {
            ui.label("Сбросить контекст темы? Агент начнёт следующую задачу с чистого листа.");
            ui.horizontal(|ui| {
                if ui.button("Сбросить").clicked() {
                    send(Command::ResetTopic(key));
                    self.resetting = None;
                }
                if ui.button("Отмена").clicked() {
                    self.resetting = None;
                }
            });
        });
        if modal.should_close() {
            self.resetting = None;
        }
    }
}
```

- [ ] **Step 4: `gui/log.rs`**

```rust
//! The «Лог» tab.

use std::path::Path;

use eframe::egui;
use tracing::Level;

use crate::gui::folder;
use crate::logging::LogLine;

const LEVELS: [Level; 4] = [Level::ERROR, Level::WARN, Level::INFO, Level::DEBUG];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogAction {
    UseChat(i64),
    AddUser(u64),
}

pub struct LogView {
    level: Level,
    follow: bool,
}

impl Default for LogView {
    fn default() -> Self {
        Self { level: Level::INFO, follow: true }
    }
}

impl LogView {
    pub fn show(&mut self, ui: &mut egui::Ui, lines: &[LogLine], folder_path: &Path) -> Option<LogAction> {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("level")
                .selected_text(self.level.as_str())
                .show_ui(ui, |ui| {
                    for level in LEVELS {
                        ui.selectable_value(&mut self.level, level, level.as_str());
                    }
                });
            ui.checkbox(&mut self.follow, "Автопрокрутка");
            if ui.button("Открыть папку логов").clicked()
                && let Err(error) = folder::open(folder_path)
            {
                tracing::warn!(%error, "log folder not opened");
            }
        });
        ui.separator();
        let mut action = None;
        egui::ScrollArea::vertical().auto_shrink(false).stick_to_bottom(self.follow).show(ui, |ui| {
            for line in lines.iter().filter(|line| line.level <= self.level) {
                ui.horizontal_wrapped(|ui| {
                    let text = format!("{} {:5} {}", line.time, line.level.as_str(), line.text);
                    match line.rejected {
                        Some(rejected) => {
                            ui.colored_label(ui.visuals().warn_fg_color, text);
                            if let Some(chat) = rejected.chat
                                && ui.small_button("Использовать chat_id").clicked()
                            {
                                action = Some(LogAction::UseChat(chat));
                            }
                            if let Some(user) = rejected.user
                                && ui.small_button("Добавить user_id").clicked()
                            {
                                action = Some(LogAction::AddUser(user));
                            }
                        }
                        None => {
                            ui.monospace(text);
                        }
                    }
                });
            }
        });
        action
    }
}
```

`tracing::Level` упорядочен «ERROR < WARN < INFO < DEBUG < TRACE» по подробности, поэтому `line.level <= self.level` оставляет строки не подробнее выбранного уровня; проверить на запуске (ошибки видны при фильтре INFO).

- [ ] **Step 5: `gui/settings.rs`**

```rust
//! The «Настройки» tab: a draft of every setting, saved only when it parses.

use std::path::{Path, PathBuf};

use eframe::egui;
use hub_core::settings::{Draft, Field, FieldError, PermissionMode, UpdateCheck};

use crate::gui::look::{add_user, telegram_changed, use_chat};
use crate::gui::log::LogAction;
use crate::supervisor::Command;

#[derive(Default)]
pub struct SettingsForm {
    draft: Draft,
    synced: Option<Draft>,
    reveal: bool,
    confirming: bool,
}

fn errors_for<'a>(errors: &'a [FieldError], field: Field) -> impl Iterator<Item = &'a str> {
    errors.iter().filter(move |error| error.field == field).map(|error| error.message.as_str())
}

fn messages(ui: &mut egui::Ui, errors: &[FieldError], field: Field) {
    for message in errors_for(errors, field) {
        ui.colored_label(ui.visuals().error_fg_color, message);
    }
}

fn pick_folder(start: &str) -> Option<String> {
    let dialog = rfd::FileDialog::new();
    let dialog = if start.is_empty() { dialog } else { dialog.set_directory(start) };
    dialog.pick_folder().map(|path| path.display().to_string())
}

fn pick_file(start: &str) -> Option<String> {
    let dialog = rfd::FileDialog::new();
    let start = Path::new(start).parent().map(Path::to_path_buf).unwrap_or_default();
    let dialog = if start.as_os_str().is_empty() { dialog } else { dialog.set_directory(start) };
    dialog.pick_file().map(|path| path.display().to_string())
}

impl SettingsForm {
    /// Adopts newly saved settings unless the user is in the middle of editing.
    pub fn sync(&mut self, saved: Option<&Draft>) {
        if self.synced.as_ref() == saved {
            return;
        }
        let pristine = self.synced.as_ref().map_or(self.draft == Draft::default(), |synced| *synced == self.draft);
        if pristine {
            self.draft = saved.cloned().unwrap_or_default();
        }
        self.synced = saved.cloned();
    }

    pub fn apply(&mut self, action: LogAction) {
        match action {
            LogAction::UseChat(chat) => use_chat(&mut self.draft, chat),
            LogAction::AddUser(user) => add_user(&mut self.draft, user),
        }
    }

    fn errors(&self, home: &Path) -> Vec<FieldError> {
        match self.draft.parse(home) {
            Ok(settings) if settings.workspace_root.is_dir() => Vec::new(),
            Ok(_) => vec![FieldError {
                field: Field::WorkspaceRoot,
                message: "Такого каталога нет".to_owned(),
            }],
            Err(errors) => errors,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, home: &Path, sessions: usize, send: &mut impl FnMut(Command)) {
        let errors = self.errors(home);
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.telegram(ui, &errors);
            ui.separator();
            self.workspace(ui, &errors);
            ui.separator();
            self.agent(ui, &errors);
            ui.separator();
            self.timeouts(ui, &errors);
        });
        ui.separator();
        let changed = self.synced.as_ref().map_or(true, |synced| *synced != self.draft);
        ui.horizontal(|ui| {
            let save = ui.add_enabled(errors.is_empty() && changed, egui::Button::new("Сохранить"));
            if save.clicked() {
                if telegram_changed(self.synced.as_ref(), &self.draft) && sessions > 0 {
                    self.confirming = true;
                } else {
                    send(Command::Save(Box::new(self.draft.clone())));
                }
            }
            if ui.add_enabled(changed, egui::Button::new("Отменить изменения")).clicked() {
                self.draft = self.synced.clone().unwrap_or_default();
            }
        });
        self.confirm(ui.ctx(), sessions, send);
    }

    fn telegram(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Telegram");
        ui.label("Изменение этих полей перезапускает бота и прерывает работающие сессии.");
        ui.horizontal(|ui| {
            ui.label("Токен бота");
            ui.add(egui::TextEdit::singleline(&mut self.draft.token).password(!self.reveal));
            ui.checkbox(&mut self.reveal, "показать");
        });
        messages(ui, errors, Field::Token);
        ui.horizontal(|ui| {
            ui.label("chat_id группы");
            ui.text_edit_singleline(&mut self.draft.chat);
        });
        messages(ui, errors, Field::Chat);
        ui.horizontal(|ui| {
            ui.label("Разрешённые user_id");
            ui.add(egui::TextEdit::singleline(&mut self.draft.users).hint_text("111, 222"));
        });
        messages(ui, errors, Field::Users);
    }

    fn workspace(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Рабочие каталоги");
        ui.horizontal(|ui| {
            ui.label("Корень");
            ui.text_edit_singleline(&mut self.draft.workspace_root);
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_folder(&self.draft.workspace_root)
            {
                self.draft.workspace_root = path;
            }
        });
        messages(ui, errors, Field::WorkspaceRoot);
    }

    fn agent(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Claude Code");
        ui.horizontal(|ui| {
            ui.label("Путь к claude");
            ui.add(egui::TextEdit::singleline(&mut self.draft.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.cli)
            {
                self.draft.cli = path;
            }
        });
        messages(ui, errors, Field::Cli);
        ui.horizontal(|ui| {
            ui.label("Модель");
            ui.add(egui::TextEdit::singleline(&mut self.draft.model).hint_text("из настроек Claude Code"));
        });
        messages(ui, errors, Field::Model);
        ui.horizontal(|ui| {
            ui.label("Режим разрешений");
            egui::ComboBox::from_id_salt("permission_mode")
                .selected_text(self.draft.permission_mode.wire())
                .show_ui(ui, |ui| {
                    for mode in PermissionMode::ALL {
                        ui.selectable_value(&mut self.draft.permission_mode, mode, mode.wire());
                    }
                });
        });
        if self.draft.permission_mode == PermissionMode::BypassPermissions {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "bypassPermissions: агент выполняет любые инструменты без подтверждения.",
            );
        }
        ui.horizontal(|ui| {
            ui.label("Лимит на задачу, $");
            ui.add(egui::TextEdit::singleline(&mut self.draft.budget).hint_text("без лимита"));
        });
        messages(ui, errors, Field::Budget);
    }

    fn timeouts(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Таймауты и обновления");
        ui.horizontal(|ui| {
            ui.label("Ожидание подтверждения, с");
            ui.text_edit_singleline(&mut self.draft.approval_timeout);
        });
        messages(ui, errors, Field::ApprovalTimeout);
        ui.horizontal(|ui| {
            ui.label("Фоновые задачи, с");
            ui.text_edit_singleline(&mut self.draft.background_timeout);
        });
        messages(ui, errors, Field::BackgroundTimeout);
        let mut check = self.draft.updates == UpdateCheck::Enabled;
        if ui.checkbox(&mut check, "Проверять обновления").changed() {
            self.draft.updates = if check { UpdateCheck::Enabled } else { UpdateCheck::Disabled };
        }
    }

    fn confirm(&mut self, ctx: &egui::Context, sessions: usize, send: &mut impl FnMut(Command)) {
        if !self.confirming {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("restart")).show(ctx, |ui| {
            ui.label(format!(
                "Сохранение перезапустит бота и прервёт активные сессии ({sessions}). Продолжить?"
            ));
            ui.horizontal(|ui| {
                if ui.button("Сохранить и перезапустить").clicked() {
                    send(Command::Save(Box::new(self.draft.clone())));
                    self.confirming = false;
                }
                if ui.button("Отмена").clicked() {
                    self.confirming = false;
                }
            });
        });
        if modal.should_close() {
            self.confirming = false;
        }
    }
}

#[must_use]
pub fn home_or_root() -> PathBuf {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()).unwrap_or_default()
}
```

`home_or_root` используется в `main.rs` (Task 8); если clippy пометит его как лишний здесь, перенести в `main.rs` и записать в ledger.

- [ ] **Step 6: `gui/app.rs` и `gui/mod.rs`**

`gui/mod.rs`:

```rust
pub mod app;
mod folder;
mod log;
pub mod look;
mod settings;
mod status;
mod topics;
pub mod tray;

pub use settings::home_or_root;
```

`gui/app.rs`:

```rust
//! The window: four tabs over the latest snapshot; closing it hides it into the tray.

use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;

use eframe::egui;
use tokio::sync::{mpsc, watch};

use crate::gui::log::LogView;
use crate::gui::look::active;
use crate::gui::settings::SettingsForm;
use crate::gui::status;
use crate::gui::topics::TopicsView;
use crate::gui::tray::{Tray, TrayAction};
use crate::logging::LogBuffer;
use crate::supervisor::{BotStatus, Command, Snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Status,
    Topics,
    Log,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    Staying,
    Quitting,
}

pub struct AppSetup {
    pub commands: mpsc::Sender<Command>,
    pub snapshot: watch::Receiver<Snapshot>,
    pub logs: LogBuffer,
    pub tray: Option<(Tray, std_mpsc::Receiver<TrayAction>)>,
    pub home: PathBuf,
    pub logs_dir: PathBuf,
}

pub struct HubApp {
    commands: mpsc::Sender<Command>,
    snapshot: watch::Receiver<Snapshot>,
    logs: LogBuffer,
    tray: Option<(Tray, std_mpsc::Receiver<TrayAction>)>,
    home: PathBuf,
    logs_dir: PathBuf,
    tab: Tab,
    exit: Exit,
    topics: TopicsView,
    log: LogView,
    form: SettingsForm,
}

impl HubApp {
    #[must_use]
    pub fn new(setup: AppSetup) -> Self {
        let AppSetup { commands, snapshot, logs, tray, home, logs_dir } = setup;
        let tab = match snapshot.borrow().bot {
            BotStatus::Unconfigured => Tab::Settings,
            BotStatus::Stopped
            | BotStatus::Starting
            | BotStatus::Running { .. }
            | BotStatus::Failed(_) => Tab::Status,
        };
        Self {
            commands,
            snapshot,
            logs,
            tray,
            home,
            logs_dir,
            tab,
            exit: Exit::Staying,
            topics: TopicsView::default(),
            log: LogView::default(),
            form: SettingsForm::default(),
        }
    }

    fn send(commands: &mpsc::Sender<Command>, command: Command) {
        if commands.try_send(command).is_err() {
            tracing::warn!("command dropped: the core is busy or stopped");
        }
    }

    fn quit(&mut self, ctx: &egui::Context) {
        self.exit = Exit::Quitting;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    fn tray_actions(&mut self, ctx: &egui::Context, snapshot: &Snapshot) {
        let actions: Vec<TrayAction> = match &self.tray {
            Some((_, actions)) => actions.try_iter().collect(),
            None => Vec::new(),
        };
        for action in actions {
            match action {
                TrayAction::Open => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                TrayAction::Toggle => {
                    let command = match snapshot.bot {
                        BotStatus::Running { .. } | BotStatus::Starting => Command::Stop,
                        BotStatus::Stopped | BotStatus::Failed(_) | BotStatus::Unconfigured => {
                            Command::Start
                        }
                    };
                    Self::send(&self.commands, command);
                }
                TrayAction::Quit => self.quit(ctx),
            }
        }
    }
}

impl eframe::App for HubApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let snapshot = self.snapshot.borrow().clone();
        self.tray_actions(ctx, &snapshot);
        if let Some((tray, _)) = &mut self.tray {
            tray.show(&snapshot);
        }
        self.form.sync(snapshot.saved.as_ref());
        let closing = ctx.input(|input| input.viewport().close_requested());
        if closing && self.exit == Exit::Staying && self.tray.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let snapshot = self.snapshot.borrow().clone();
        let commands = self.commands.clone();
        let mut send = |command| Self::send(&commands, command);
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Status, "Статус");
                ui.selectable_value(&mut self.tab, Tab::Topics, "Темы");
                ui.selectable_value(&mut self.tab, Tab::Log, "Лог");
                ui.selectable_value(&mut self.tab, Tab::Settings, "Настройки");
            });
        });
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Status => status::show(ui, &snapshot, &mut send),
            Tab::Topics => self.topics.show(ui, &snapshot, &mut send),
            Tab::Log => {
                if let Some(action) = self.log.show(ui, &self.logs.lines(), &self.logs_dir) {
                    self.form.apply(action);
                    self.tab = Tab::Settings;
                }
            }
            Tab::Settings => self.form.show(ui, &self.home, active(&snapshot), &mut send),
        });
    }
}
```

Если в egui 0.36 нет `egui::Panel::top` с `show(ui, ..)` (проверено в исходнике: `Panel::top(id)` и `Panel::show(self, ui, ..)` есть), использовать `egui::TopBottomPanel`. Если `ui.add(egui::Label::new(..).sense(..))` или `Modal::should_close` отличаются, свериться с `~/.cargo/registry/src/*/egui-0.36.2/src` и записать отличие в ledger.

- [ ] **Step 7: Сборка и линтеры**

Run: `cargo build -p hub-app && cargo test -p hub-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS (52 теста, окно собирается).

- [ ] **Step 8: Commit**

```bash
git add crates/hub-app
git commit -m "hub-app: окно — статус, темы, лог и настройки"
```

---

### Task 8: Точка входа и ручная проверка

**Files:**
- Modify: `crates/hub-app/src/main.rs`

**Interfaces:**
- Consumes: всё из Task 1–7.

- [ ] **Step 1: `main.rs`**

```rust
#![windows_subsystem = "windows"]

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use eframe::egui;
use hub_app::config::FileSettings;
use hub_app::connector::TelegramConnector;
use hub_app::dirs::AppDirs;
use hub_app::gui::app::{AppSetup, HubApp};
use hub_app::gui::home_or_root;
use hub_app::gui::tray::Tray;
use hub_app::instance::{InstanceError, InstanceLock};
use hub_app::logging::{self, LogBuffer, Repaint};
use hub_app::secrets::Keyring;
use hub_app::supervisor::{Command, STOP_TIMEOUT, Snapshot, Supervisor, SupervisorSetup};
use tokio::sync::{mpsc, oneshot, watch};

const COMMANDS: usize = 32;
const RUNTIME_GRACE: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = format!("{error:#}"), "agent-hub failed");
            alert(&format!("{error:#}"));
            ExitCode::FAILURE
        }
    }
}

fn alert(text: &str) {
    rfd::MessageDialog::new()
        .set_title("agent-hub")
        .set_description(text)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

fn run() -> anyhow::Result<()> {
    let dirs = AppDirs::locate().context("не найден домашний каталог пользователя")?;
    let _lock = match InstanceLock::acquire(&dirs.lock()) {
        Ok(lock) => lock,
        Err(InstanceError::Running) => {
            alert("agent-hub уже запущен — его значок в трее.");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let logs = LogBuffer::default();
    let _guard = logging::init(&dirs.logs(), logs.clone())?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "agent-hub started");
    let home = home_or_root();
    let store = FileSettings::new(dirs.settings(), Box::new(Keyring));
    let loaded = store.load(&home);
    if let Err(error) = &loaded {
        tracing::error!(%error, "settings not loaded");
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("agent-hub-core")
        .build()?;
    let (commands, inbox) = mpsc::channel(COMMANDS);
    let (snapshot, watched) = watch::channel(Snapshot::default());
    let core = runtime.handle().clone();
    let window = commands.clone();
    let connector = TelegramConnector { home: home.clone(), topics: dirs.topics() };
    let logs_dir = dirs.logs();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("agent-hub")
            .with_inner_size([960.0, 640.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "agent-hub",
        options,
        Box::new(move |creation| {
            let context = creation.egui_ctx.clone();
            let repaint: Repaint = Arc::new(move || context.request_repaint());
            logs.on_change(Arc::clone(&repaint));
            let supervisor = Supervisor::new(SupervisorSetup {
                connector,
                store: Box::new(store),
                home: home.clone(),
                loaded,
                snapshot,
                repaint: Arc::clone(&repaint),
            });
            core.spawn(supervisor.run(inbox));
            let tray = match Tray::create(repaint) {
                Ok(tray) => Some(tray),
                Err(error) => {
                    tracing::warn!(%error, "no tray: closing the window quits");
                    None
                }
            };
            Ok(Box::new(HubApp::new(AppSetup {
                commands: window,
                snapshot: watched,
                logs,
                tray,
                home,
                logs_dir,
            })))
        }),
    )
    .map_err(|error| anyhow::anyhow!("окно не открылось: {error}"))?;
    let (done, stopped) = oneshot::channel();
    if commands.blocking_send(Command::Quit(done)).is_ok() {
        runtime.block_on(async {
            // Past the deadline we exit anyway; the log says what did not stop.
            let _ = tokio::time::timeout(STOP_TIMEOUT + Duration::from_secs(1), stopped).await;
        });
    }
    runtime.shutdown_timeout(RUNTIME_GRACE);
    tracing::info!("agent-hub stopped");
    Ok(())
}
```

Если `eframe::NativeOptions` не реализует `Default` с полем `viewport` — свериться с `eframe-0.36.2/src/epi.rs`.

- [ ] **Step 2: Сборка и линтеры**

Run: `cargo build -p hub-app && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 3: Ручная проверка (Windows, в этой сессии)**

1. `cargo run -p hub-app --bin agent-hub` при отсутствии `settings.toml` → окно на вкладке «Настройки», иконка в трее серая, подсказка «agent-hub — не настроен».
2. Второй `cargo run -p hub-app --bin agent-hub` → диалог «agent-hub уже запущен…», второй процесс завершается, первый работает.
3. Закрыть окно крестиком → окно скрыто, процесс жив; клик по иконке трея → окно снова видно.
4. Вкладка «Лог» показывает строку `agent-hub started`.
5. Трей → «Выход» → процесс завершается, `data_dir/logs/agent-hub.*.log` содержит `agent-hub stopped`.
6. Проверка с настоящим ботом (токен тестового бота, тестовая группа) — по желанию пользователя; без неё бот с неверным токеном даёт статус «Ошибка: Telegram не принял токен бота» и красную иконку.

Если шаг не проходит — systematic-debugging, исправление с тестом там, где решение чистое; записать в ledger.

- [ ] **Step 4: Commit**

```bash
git add crates/hub-app
git commit -m "hub-app: точка входа — блокировка, лог, супервизор, окно и трей"
```

---

## Самопроверка плана

- Покрытие спеки: каталоги и lock (Task 1), атомарная запись (Task 1), `settings.toml` + keyring + одна функция разбора для файла и формы (Task 2), `topics.json` формат v1 и отказ при повреждении (Task 2, 5), лог-файл с ротацией + буфер 500 строк + `rejected update` с кнопками (Task 3, 7), `Command`/`Snapshot`/`request_repaint` (Task 4, 8), применение настроек: Telegram → перезапуск, остальное — со следующей сессии (Task 4), `BotStartError` (Task 5 `StartError`; `ClaudeMissing`/`ClaudeTooOld` — варианты `CliError`), трей: цвет, подсказка, меню (Task 6), Linux без трея → закрытие = выход (Task 7 `logic` + Task 8), вкладки (Task 7), первый запуск без настроек и второй экземпляр (Task 8), остановка за 15 с (Task 4, 8). Обновления — фаза 5.
- Типы между задачами: `Repaint` объявлен в `logging` (Task 3) и используется в Task 4, 6, 8; `Snapshot`/`BotStatus`/`Command` (Task 4) — в Task 6–8; `StartError` (Task 5) — ассоциированный `Connector::Error`; `Loaded` (Task 2) — в Task 4 и 8; `TopicView`/`TopicState` — из `hub-telegram`.
