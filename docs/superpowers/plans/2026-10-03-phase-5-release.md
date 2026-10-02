# Фаза 5: автообновление, CI, релизы, README

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Приложение само находит новую версию в GitHub Releases и ставит её по кнопке с проверкой SHA-256; репозиторий получает CI на трёх ОС, `cargo deny`, релизную сборку по тегу для четырёх таргетов, иконку приложения и README под десктоп.

**Architecture:** Модуль обновлений делится на чистое ядро `updates` (версия, разбор ответа GitHub, выбор файла своей платформы, проверка контрольной суммы) и оболочку: актор `updater` с трейтами `Source` (GitHub API) и `Installer` (`self-replace`). Актор публикует `UpdateState` в `watch`, окно и трей показывают баннер и пункт меню. CI и релизы — GitHub Actions; Linux-сборка и unix-тесты дополнительно проверяются локально в Docker (`rust:1.96`).

**Tech Stack:** Rust 1.96, reqwest 0.12 (rustls, уже в дереве через teloxide), sha2 0.11, self-replace 1.5, winresource 0.1 (только Windows), GitHub Actions (`Swatinem/rust-cache`, `softprops/action-gh-release`), cargo-deny, actionlint (в Docker).

**Spec:** `docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md` (разделы «CI, релизы, автообновление», «README», «Таймауты и лимиты», «Типы ошибок»).

## Global Constraints

- Лints workspace (deny warnings, pedantic, unwrap/expect/panic/indexing) действуют на новый код; в тестах разрешены `clippy.toml`.
- Источник обновлений зашит в бинарник: `https://api.github.com/repos/aprazdnikov/agent-hub-rs/releases/latest`.
- Проверка обновлений: при старте и раз в 24 ч, только если `updates = Enabled`; таймаут запроса 15 с.
- Установка только по кнопке; файл проверяется по SHA-256 из того же релиза до замены.
- Таргеты и архивы: `x86_64-pc-windows-msvc` (`.zip`), `aarch64-apple-darwin` и `x86_64-apple-darwin` (`.tar.gz`, оба на `macos-latest`), `x86_64-unknown-linux-gnu` (`.tar.gz`, `ubuntu-22.04`). Имя `agent-hub-<target>.<ext>` + `.sha256`.
- Windows: `#![windows_subsystem = "windows"]` (уже есть), иконка через `winresource`.
- UI на русском. Коммиты без упоминания ассистента и без `Co-Authored-By`. Ничего не пушить: workflow проверяются локально (actionlint, Docker), первый реальный прогон — после пуша пользователем.

## Review Focus

1. Повреждённый или подменённый при скачивании файл → замена не происходит, баннер «контрольная сумма не совпала» → тест `corrupted_download_is_refused` (Task 2).
2. Нет сети / GitHub ответил ошибкой или не-JSON при проверке → приложение работает, баннера нет, в логе предупреждение → тест `failed_check_changes_nothing` (Task 2).
3. Пользователь выключил проверку обновлений → ни одного запроса к GitHub, в том числе через сутки → тест `disabled_checks_make_no_requests` (Task 2).
4. Релиз без файла для своей платформы или без `.sha256` → обновление не предлагается → тест `release_without_our_files_is_not_offered` (Task 1).
5. Перезапуск после установки при активных сессиях → сначала подтверждение, затем «⏹ Остановлено» в темы и новый процесс; второй экземпляр не упирается в блокировку старого → ручная проверка в Task 3 и порядок `drop(lock)` → `spawn` в `main.rs`.

## Отклонения от спеки (вносятся в спеку в Task 1)

- Для автообновления релиз публикует ещё и голый бинарник `agent-hub-<target>[.exe]` + `.sha256`: приложению не нужно распаковывать zip/tar.gz (меньше зависимостей и кода). Архивы остаются для ручной установки.
- Подтверждение при активных сессиях спрашивается на «Перезапустить», а не на «Установить»: сессии прерывает перезапуск, установка их не трогает.
- Ошибка проверки обновлений только пишется в лог; баннер показывает доступную версию, ход установки и ошибку установки.
- Иконка — один `assets/agent-hub.ico` в репозитории (сгенерирован скриптом из Task 4), окно и трей рисуют свою программно.

---

### Task 1: Ядро обновлений

**Files:**
- Create: `crates/hub-app/src/updates.rs`
- Modify: `Cargo.toml` (`sha2 = "0.11"`), `crates/hub-app/Cargo.toml` (`serde`, `serde_json`, `sha2`), `crates/hub-app/src/lib.rs` (`pub mod updates;`), `docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md`

**Interfaces:**
- Produces: `updates::{Version { major, minor, patch } (Ord, Display, parse(&str) -> Option<Version>), Target::{WindowsX64, MacArm, MacX64, LinuxX64} (current() -> Option<Target>, triple() -> &'static str, binary() -> String), Release { version, assets: Vec<Asset> }, Asset { name, url }, ReleaseError, parse_release(&str) -> Result<Release, ReleaseError>, Offer { version, binary: String, checksum: String }, offer(&Release, Version, Target) -> Option<Offer>, ChecksumError::{Malformed, Mismatch}, verify(&[u8], &str) -> Result<(), ChecksumError>}`.

- [ ] **Step 1: Зависимости и спека**

`Cargo.toml` → `[workspace.dependencies]`: `sha2 = "0.11"`. `crates/hub-app/Cargo.toml` → `[dependencies]`: `serde.workspace = true`, `serde_json.workspace = true`, `sha2.workspace = true`. В спеку, раздел «Уточнения реализации (фаза 4)» → после него добавить «### Уточнения реализации (фаза 5)» с четырьмя пунктами из «Отклонения от спеки» этого плана.

- [ ] **Step 2: Падающие тесты**

```rust
#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn version(raw: &str) -> Version {
        Version::parse(raw).unwrap()
    }

    const RELEASE: &str = r#"{
        "tag_name": "v0.3.0",
        "name": "agent-hub 0.3.0",
        "assets": [
            {"name": "agent-hub-x86_64-unknown-linux-gnu", "browser_download_url": "https://example.test/linux"},
            {"name": "agent-hub-x86_64-unknown-linux-gnu.sha256", "browser_download_url": "https://example.test/linux.sha256"},
            {"name": "agent-hub-x86_64-pc-windows-msvc.exe", "browser_download_url": "https://example.test/windows"},
            {"name": "agent-hub-x86_64-pc-windows-msvc.zip", "browser_download_url": "https://example.test/windows.zip"}
        ]
    }"#;

    #[rstest]
    #[case("0.1.0", Some((0, 1, 0)))]
    #[case("v1.20.3", Some((1, 20, 3)))]
    #[case("1.2", None)]
    #[case("1.2.3.4", None)]
    #[case("1.2.x", None)]
    #[case("1.2.3-beta", None)]
    fn versions_are_parsed(#[case] raw: &str, #[case] expected: Option<(u32, u32, u32)>) {
        let parsed = Version::parse(raw).map(|v| (v.major, v.minor, v.patch));
        assert_eq!(parsed, expected);
    }

    #[test]
    fn versions_compare_numerically() {
        assert!(version("0.10.0") > version("0.9.9"));
        assert_eq!(version("v1.2.3").to_string(), "1.2.3");
    }

    #[rstest]
    #[case(Target::WindowsX64, "agent-hub-x86_64-pc-windows-msvc.exe")]
    #[case(Target::MacArm, "agent-hub-aarch64-apple-darwin")]
    #[case(Target::MacX64, "agent-hub-x86_64-apple-darwin")]
    #[case(Target::LinuxX64, "agent-hub-x86_64-unknown-linux-gnu")]
    fn each_target_has_its_binary(#[case] target: Target, #[case] name: &str) {
        assert_eq!(target.binary(), name);
    }

    #[test]
    fn newer_release_with_our_files_is_offered() {
        let release = parse_release(RELEASE).unwrap();
        assert_eq!(
            offer(&release, version("0.2.9"), Target::LinuxX64),
            Some(Offer {
                version: version("0.3.0"),
                binary: "https://example.test/linux".to_owned(),
                checksum: "https://example.test/linux.sha256".to_owned(),
            })
        );
    }

    #[rstest]
    #[case("0.3.0")]
    #[case("0.4.0")]
    fn current_or_older_release_is_not_offered(#[case] current: &str) {
        let release = parse_release(RELEASE).unwrap();
        assert_eq!(offer(&release, version(current), Target::LinuxX64), None);
    }

    #[rstest]
    #[case(Target::WindowsX64)]
    #[case(Target::MacArm)]
    fn release_without_our_files_is_not_offered(#[case] target: Target) {
        let release = parse_release(RELEASE).unwrap();
        assert_eq!(offer(&release, version("0.1.0"), target), None);
    }

    #[rstest]
    #[case("not json")]
    #[case(r#"{"tag_name": "latest", "assets": []}"#)]
    fn malformed_release_is_an_error(#[case] raw: &str) {
        assert!(parse_release(raw).is_err());
    }

    #[test]
    fn checksum_matches_the_sha256_line() {
        // sha256("abc")
        let line = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  agent-hub";
        assert_eq!(verify(b"abc", line), Ok(()));
        assert_eq!(verify(b"abd", line), Err(ChecksumError::Mismatch));
        assert_eq!(verify(b"abc", ""), Err(ChecksumError::Malformed));
        assert_eq!(verify(b"abc", "zz  agent-hub"), Err(ChecksumError::Malformed));
    }
}
```

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p hub-app --lib updates`
Expected: FAIL — `cannot find type Version`.

- [ ] **Step 4: Реализация**

```rust
//! Update decisions without I/O: versions, the GitHub release, the file for this platform and
//! its checksum.

use std::fmt;

use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    /// `1.2.3` or `v1.2.3`; pre-releases are not offered as updates.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.strip_prefix('v').unwrap_or(raw);
        let mut parts = raw.split('.').map(str::parse::<u32>);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    WindowsX64,
    MacArm,
    MacX64,
    LinuxX64,
}

impl Target {
    /// The platform this binary was built for, if releases are published for it.
    #[must_use]
    pub fn current() -> Option<Self> {
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Some(Self::WindowsX64)
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            Some(Self::MacArm)
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            Some(Self::MacX64)
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")) {
            Some(Self::LinuxX64)
        } else {
            None
        }
    }

    #[must_use]
    pub const fn triple(self) -> &'static str {
        match self {
            Self::WindowsX64 => "x86_64-pc-windows-msvc",
            Self::MacArm => "aarch64-apple-darwin",
            Self::MacX64 => "x86_64-apple-darwin",
            Self::LinuxX64 => "x86_64-unknown-linux-gnu",
        }
    }

    /// The bare executable published next to the archives, for the updater.
    #[must_use]
    pub fn binary(self) -> String {
        match self {
            Self::WindowsX64 => format!("agent-hub-{}.exe", self.triple()),
            Self::MacArm | Self::MacX64 | Self::LinuxX64 => format!("agent-hub-{}", self.triple()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub assets: Vec<Asset>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReleaseError {
    #[error("ответ GitHub не похож на релиз: {0}")]
    Json(#[from] serde_json::Error),
    #[error("непонятная версия релиза «{0}»")]
    Version(String),
}

#[derive(Deserialize)]
struct ReleaseJson {
    tag_name: String,
    assets: Vec<AssetJson>,
}

#[derive(Deserialize)]
struct AssetJson {
    name: String,
    browser_download_url: String,
}

pub fn parse_release(raw: &str) -> Result<Release, ReleaseError> {
    let ReleaseJson { tag_name, assets } = serde_json::from_str(raw)?;
    let version = Version::parse(&tag_name).ok_or(ReleaseError::Version(tag_name))?;
    let assets = assets
        .into_iter()
        .map(|AssetJson { name, browser_download_url }| Asset { name, url: browser_download_url })
        .collect();
    Ok(Release { version, assets })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    pub version: Version,
    pub binary: String,
    pub checksum: String,
}

/// A newer release that carries both our executable and its checksum.
#[must_use]
pub fn offer(release: &Release, current: Version, target: Target) -> Option<Offer> {
    if release.version <= current {
        return None;
    }
    let name = target.binary();
    let url = |wanted: &str| {
        release.assets.iter().find(|asset| asset.name == wanted).map(|asset| asset.url.clone())
    };
    Some(Offer {
        version: release.version,
        binary: url(&name)?,
        checksum: url(&format!("{name}.sha256"))?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ChecksumError {
    #[error("файл контрольной суммы пуст или испорчен")]
    Malformed,
    #[error("контрольная сумма не совпала: файл повреждён при скачивании")]
    Mismatch,
}

/// `checksum` is a `sha256sum` line: the hex digest, then the file name.
pub fn verify(binary: &[u8], checksum: &str) -> Result<(), ChecksumError> {
    const HEX_LEN: usize = 64;
    let expected = checksum
        .split_whitespace()
        .next()
        .filter(|hex| hex.len() == HEX_LEN && hex.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or(ChecksumError::Malformed)?;
    let actual: String = Sha256::digest(binary).iter().map(|byte| format!("{byte:02x}")).collect();
    if actual.eq_ignore_ascii_case(expected) { Ok(()) } else { Err(ChecksumError::Mismatch) }
}
```

- [ ] **Step 5: Тесты проходят**

Run: `cargo test -p hub-app --lib && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS (60 + 22 = 82 теста).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/hub-app docs/superpowers/specs/2026-10-02-agent-hub-rs-design.md
git commit -m "hub-app: ядро обновлений — версии, релиз, файл платформы, SHA-256"
```

---

### Task 2: Актор обновлений, GitHub и замена бинарника

**Files:**
- Create: `crates/hub-app/src/updater.rs`, `crates/hub-app/src/github.rs`
- Modify: `Cargo.toml` (`reqwest`, `self-replace`), `crates/hub-app/Cargo.toml`, `crates/hub-app/src/lib.rs` (`pub mod github; pub mod updater;`)

**Interfaces:**
- Consumes: `updates::*` (Task 1), `supervisor::Snapshot`, `logging::Repaint`, `hub_core::settings::UpdateCheck`.
- Produces:
  - `updater::{UpdateError::{Check, Download, Checksum, Replace}, UpdateState::{Idle, Available(Version), Installing(Version), Installed(Version), Failed(String)}, UpdateCommand::Install, Source (trait: latest() -> BoxFuture<Result<String, UpdateError>>, download(String url, u64 limit) -> BoxFuture<Result<Vec<u8>, UpdateError>>), Installer (trait: install(&self, Vec<u8>) -> Result<(), UpdateError>), UpdaterSetup { source, installer, current: Version, target: Option<Target>, checks: watch::Receiver<Snapshot>, state: watch::Sender<UpdateState>, repaint: Repaint }, run(UpdaterSetup<S, I>, mpsc::Receiver<UpdateCommand>) (async)}`; константы `CHECK_EVERY` (24 ч), `CHECK_TIMEOUT` (15 с), `DOWNLOAD_TIMEOUT` (10 мин), `BINARY_LIMIT` (200 МБ), `CHECKSUM_LIMIT` (1 КБ).
  - `github::{GitHub::new() -> Result<GitHub, UpdateError>, SelfReplace}`.

- [ ] **Step 1: Зависимости**

`Cargo.toml` → `[workspace.dependencies]`:

```toml
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls"] }
self-replace = "1.5"
```

`crates/hub-app/Cargo.toml` → `[dependencies]`: `reqwest.workspace = true`, `self-replace.workspace = true`.

- [ ] **Step 2: Падающие тесты (`updater.rs`)**

```rust
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use hub_core::settings::{Draft, UpdateCheck};

    use super::*;
    use crate::supervisor::Snapshot;

    const BINARY: &[u8] = b"new agent-hub";
    // sha256("new agent-hub")
    const SUM: &str = "8cba4cb81f9e8d3d2f9b4ab3e8b55a9d9f0e5ce1ac0a2d3c58a8c3b9d1e1f2a3";

    fn release(tag: &str) -> String {
        format!(
            r#"{{"tag_name": "{tag}", "assets": [
                {{"name": "agent-hub-x86_64-unknown-linux-gnu", "browser_download_url": "bin"}},
                {{"name": "agent-hub-x86_64-unknown-linux-gnu.sha256", "browser_download_url": "sum"}}
            ]}}"#
        )
    }

    #[derive(Default)]
    struct FakeSource {
        latest: StdMutex<Option<Result<String, String>>>,
        files: HashMap<String, Vec<u8>>,
        checks: AtomicUsize,
    }

    impl Source for Arc<FakeSource> {
        fn latest(&self) -> BoxFuture<'_, Result<String, UpdateError>> {
            self.checks.fetch_add(1, Ordering::SeqCst);
            let answer = self.latest.lock().unwrap().clone().unwrap_or(Err("offline".to_owned()));
            Box::pin(async move { answer.map_err(UpdateError::Check) })
        }

        fn download(&self, url: String, _limit: u64) -> BoxFuture<'_, Result<Vec<u8>, UpdateError>> {
            let file = self.files.get(&url).cloned().ok_or(UpdateError::Download(url));
            Box::pin(async move { file })
        }
    }

    #[derive(Default)]
    struct FakeInstaller {
        installed: StdMutex<Vec<Vec<u8>>>,
    }

    impl Installer for Arc<FakeInstaller> {
        fn install(&self, binary: Vec<u8>) -> Result<(), UpdateError> {
            self.installed.lock().unwrap().push(binary);
            Ok(())
        }
    }

    struct Harness {
        commands: mpsc::Sender<UpdateCommand>,
        state: watch::Receiver<UpdateState>,
        source: Arc<FakeSource>,
        installer: Arc<FakeInstaller>,
    }

    fn sha256_line(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let hex: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
        format!("{hex}  agent-hub-x86_64-unknown-linux-gnu\n")
    }

    fn harness(latest: Result<String, String>, sum: &str, checks: UpdateCheck) -> Harness {
        let source = Arc::new(FakeSource {
            latest: StdMutex::new(Some(latest)),
            files: HashMap::from([
                ("bin".to_owned(), BINARY.to_vec()),
                ("sum".to_owned(), sum.as_bytes().to_vec()),
            ]),
            checks: AtomicUsize::new(0),
        });
        let installer = Arc::new(FakeInstaller::default());
        let snapshot = Snapshot {
            saved: Some(Draft { updates: checks, ..Draft::default() }),
            ..Snapshot::default()
        };
        let (_snapshots, watched) = watch::channel(snapshot);
        let (state, observed) = watch::channel(UpdateState::Idle);
        let (commands, inbox) = mpsc::channel(4);
        tokio::spawn(run(
            UpdaterSetup {
                source: Arc::clone(&source),
                installer: Arc::clone(&installer),
                current: Version::parse("0.1.0").unwrap(),
                target: Some(Target::LinuxX64),
                checks: watched,
                state,
                repaint: Arc::new(|| {}),
            },
            inbox,
        ));
        Harness { commands, state: observed, source, installer }
    }

    async fn until(state: &mut watch::Receiver<UpdateState>, done: impl Fn(&UpdateState) -> bool) {
        tokio::time::timeout(Duration::from_secs(60), state.wait_for(|s| done(s)))
            .await
            .unwrap()
            .unwrap();
    }

    fn checks(harness: &Harness) -> usize {
        harness.source.checks.load(Ordering::SeqCst)
    }

    #[tokio::test(start_paused = true)]
    async fn newer_release_becomes_available() {
        let mut harness = harness(Ok(release("v0.2.0")), &sha256_line(BINARY), UpdateCheck::Enabled);
        until(&mut harness.state, |s| matches!(s, UpdateState::Available(_))).await;
        assert_eq!(*harness.state.borrow(), UpdateState::Available(Version::parse("0.2.0").unwrap()));
    }

    #[tokio::test(start_paused = true)]
    async fn checks_repeat_daily() {
        let harness = harness(Ok(release("v0.1.0")), "", UpdateCheck::Enabled);
        tokio::time::sleep(CHECK_EVERY + Duration::from_secs(1)).await;
        assert_eq!(checks(&harness), 2);
        assert_eq!(*harness.state.borrow(), UpdateState::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn disabled_checks_make_no_requests() {
        let harness = harness(Ok(release("v0.2.0")), "", UpdateCheck::Disabled);
        tokio::time::sleep(CHECK_EVERY * 2).await;
        assert_eq!(checks(&harness), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_check_changes_nothing() {
        let harness = harness(Err("503".to_owned()), "", UpdateCheck::Enabled);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(checks(&harness), 1);
        assert_eq!(*harness.state.borrow(), UpdateState::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn install_verifies_and_replaces() {
        let mut harness = harness(Ok(release("v0.2.0")), &sha256_line(BINARY), UpdateCheck::Enabled);
        until(&mut harness.state, |s| matches!(s, UpdateState::Available(_))).await;
        harness.commands.send(UpdateCommand::Install).await.unwrap();
        until(&mut harness.state, |s| matches!(s, UpdateState::Installed(_))).await;
        assert_eq!(*harness.installer.installed.lock().unwrap(), [BINARY.to_vec()]);
    }

    #[tokio::test(start_paused = true)]
    async fn corrupted_download_is_refused() {
        let mut harness = harness(Ok(release("v0.2.0")), &sha256_line(b"other"), UpdateCheck::Enabled);
        until(&mut harness.state, |s| matches!(s, UpdateState::Available(_))).await;
        harness.commands.send(UpdateCommand::Install).await.unwrap();
        until(&mut harness.state, |s| matches!(s, UpdateState::Failed(_))).await;
        let UpdateState::Failed(reason) = harness.state.borrow().clone() else { unreachable!() };
        assert!(reason.contains("не совпала"), "{reason}");
        assert!(harness.installer.installed.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_checked_after_an_install() {
        let mut harness = harness(Ok(release("v0.2.0")), &sha256_line(BINARY), UpdateCheck::Enabled);
        until(&mut harness.state, |s| matches!(s, UpdateState::Available(_))).await;
        harness.commands.send(UpdateCommand::Install).await.unwrap();
        until(&mut harness.state, |s| matches!(s, UpdateState::Installed(_))).await;
        tokio::time::sleep(CHECK_EVERY * 2).await;
        assert_eq!(checks(&harness), 1);
    }
}
```

Константа `SUM` в тестах не используется — исполнитель удаляет её (контрольные суммы считаются `sha256_line`). `unreachable!()` в тесте допустим (`clippy.toml` разрешает `panic` в тестах); если clippy возражает на `unreachable`, заменить на `panic!("not failed")`.

- [ ] **Step 3: Убедиться, что тесты падают**

Run: `cargo test -p hub-app --lib updater`
Expected: FAIL — `cannot find trait Source`.

- [ ] **Step 4: Реализация `updater.rs`**

```rust
//! Finds a newer release and installs it on request, publishing what the window shows.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_core::settings::UpdateCheck;
use tokio::sync::{mpsc, watch};
use tokio::time::MissedTickBehavior;

use crate::logging::Repaint;
use crate::supervisor::Snapshot;
use crate::updates::{ChecksumError, Offer, Target, Version, offer, parse_release, verify};

pub const CHECK_EVERY: Duration = Duration::from_hours(24);
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_mins(10);
pub const BINARY_LIMIT: u64 = 200 * 1024 * 1024;
pub const CHECKSUM_LIMIT: u64 = 1024;

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("проверка обновлений не удалась: {0}")]
    Check(String),
    #[error("скачивание обновления не удалось: {0}")]
    Download(String),
    #[error(transparent)]
    Checksum(#[from] ChecksumError),
    #[error("не удалось заменить программу: {0}")]
    Replace(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateState {
    Idle,
    Available(Version),
    Installing(Version),
    /// Replaced on disk; the running process is still the old version until a restart.
    Installed(Version),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateCommand {
    Install,
}

pub trait Source: Send + Sync + 'static {
    fn latest(&self) -> BoxFuture<'_, Result<String, UpdateError>>;
    fn download(&self, url: String, limit: u64) -> BoxFuture<'_, Result<Vec<u8>, UpdateError>>;
}

pub trait Installer: Send + Sync + 'static {
    fn install(&self, binary: Vec<u8>) -> Result<(), UpdateError>;
}

pub struct UpdaterSetup<S, I> {
    pub source: S,
    pub installer: I,
    pub current: Version,
    pub target: Option<Target>,
    pub checks: watch::Receiver<Snapshot>,
    pub state: watch::Sender<UpdateState>,
    pub repaint: Repaint,
}

struct Updater<S, I> {
    source: S,
    installer: Arc<I>,
    current: Version,
    target: Target,
    checks: watch::Receiver<Snapshot>,
    state: watch::Sender<UpdateState>,
    repaint: Repaint,
    offer: Option<Offer>,
}

/// Unsaved settings default to checking, like the form does.
fn wanted(snapshot: &Snapshot) -> UpdateCheck {
    snapshot.saved.as_ref().map_or(UpdateCheck::Enabled, |draft| draft.updates)
}

pub async fn run<S: Source, I: Installer>(
    setup: UpdaterSetup<S, I>,
    mut commands: mpsc::Receiver<UpdateCommand>,
) {
    let UpdaterSetup { source, installer, current, target, checks, state, repaint } = setup;
    let Some(target) = target else {
        tracing::info!("no published builds for this platform, updates are off");
        return;
    };
    let mut updater = Updater {
        source,
        installer: Arc::new(installer),
        current,
        target,
        checks,
        state,
        repaint,
        offer: None,
    };
    let mut ticks = tokio::time::interval(CHECK_EVERY);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            command = commands.recv() => match command {
                Some(UpdateCommand::Install) => updater.install().await,
                None => return,
            },
            _ = ticks.tick() => updater.check().await,
        }
    }
}

impl<S: Source, I: Installer> Updater<S, I> {
    async fn check(&mut self) {
        let installed = matches!(*self.state.borrow(), UpdateState::Installed(_));
        let enabled = wanted(&self.checks.borrow()) == UpdateCheck::Enabled;
        if installed || !enabled {
            return;
        }
        let answer = tokio::time::timeout(CHECK_TIMEOUT, self.source.latest()).await;
        let raw = match answer {
            Ok(Ok(raw)) => raw,
            Ok(Err(error)) => {
                tracing::warn!(%error, "update check failed");
                return;
            }
            Err(_elapsed) => {
                tracing::warn!("update check timed out");
                return;
            }
        };
        let release = match parse_release(&raw) {
            Ok(release) => release,
            Err(error) => {
                tracing::warn!(%error, "update check failed");
                return;
            }
        };
        self.offer = offer(&release, self.current, self.target);
        match &self.offer {
            Some(found) => {
                tracing::info!(version = %found.version, "update available");
                self.publish(UpdateState::Available(found.version));
            }
            None => self.publish(UpdateState::Idle),
        }
    }

    async fn install(&mut self) {
        let Some(found) = self.offer.clone() else { return };
        self.publish(UpdateState::Installing(found.version));
        match self.fetch_and_replace(&found).await {
            Ok(()) => {
                tracing::info!(version = %found.version, "update installed");
                self.publish(UpdateState::Installed(found.version));
            }
            Err(error) => {
                tracing::error!(%error, "update not installed");
                self.publish(UpdateState::Failed(error.to_string()));
            }
        }
    }

    async fn fetch_and_replace(&self, found: &Offer) -> Result<(), UpdateError> {
        let late = |what: &str| UpdateError::Download(format!("{what}: время ожидания истекло"));
        let binary = tokio::time::timeout(
            DOWNLOAD_TIMEOUT,
            self.source.download(found.binary.clone(), BINARY_LIMIT),
        )
        .await
        .map_err(|_| late("файл программы"))??;
        let checksum = tokio::time::timeout(
            CHECK_TIMEOUT,
            self.source.download(found.checksum.clone(), CHECKSUM_LIMIT),
        )
        .await
        .map_err(|_| late("контрольная сумма"))??;
        verify(&binary, &String::from_utf8_lossy(&checksum))?;
        let installer = Arc::clone(&self.installer);
        tokio::task::spawn_blocking(move || installer.install(binary))
            .await
            .map_err(|error| UpdateError::Replace(error.to_string()))?
    }

    fn publish(&self, state: UpdateState) {
        self.state.send_replace(state);
        (self.repaint)();
    }
}
```

- [ ] **Step 5: Реализация `github.rs`**

```rust
//! The real update source (GitHub Releases) and installer (`self-replace`).

use std::io::Write;

use futures::future::BoxFuture;

use crate::updater::{CHECK_TIMEOUT, Installer, Source, UpdateError};

// Fixed at build time: the update source is not configurable, so it cannot be redirected.
const LATEST: &str = "https://api.github.com/repos/aprazdnikov/agent-hub-rs/releases/latest";

pub struct GitHub {
    client: reqwest::Client,
}

impl GitHub {
    pub fn new() -> Result<Self, UpdateError> {
        reqwest::Client::builder()
            .user_agent(concat!("agent-hub/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CHECK_TIMEOUT)
            .build()
            .map(|client| Self { client })
            .map_err(|error| UpdateError::Check(error.to_string()))
    }
}

impl Source for GitHub {
    fn latest(&self) -> BoxFuture<'_, Result<String, UpdateError>> {
        Box::pin(async move {
            let check = |error: reqwest::Error| UpdateError::Check(error.to_string());
            self.client
                .get(LATEST)
                .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(check)?
                .text()
                .await
                .map_err(check)
        })
    }

    fn download(&self, url: String, limit: u64) -> BoxFuture<'_, Result<Vec<u8>, UpdateError>> {
        Box::pin(async move {
            let failed = |error: reqwest::Error| UpdateError::Download(error.to_string());
            let too_large = || UpdateError::Download(format!("файл больше {limit} байт"));
            let mut response = self
                .client
                .get(url)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(failed)?;
            if response.content_length().is_some_and(|length| length > limit) {
                return Err(too_large());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(failed)? {
                bytes.extend_from_slice(&chunk);
                if u64::try_from(bytes.len()).map_or(true, |length| length > limit) {
                    return Err(too_large());
                }
            }
            Ok(bytes)
        })
    }
}

pub struct SelfReplace;

impl Installer for SelfReplace {
    fn install(&self, binary: Vec<u8>) -> Result<(), UpdateError> {
        let replace = |error: std::io::Error| UpdateError::Replace(error.to_string());
        let current = std::env::current_exe().map_err(replace)?;
        let directory = current
            .parent()
            .ok_or_else(|| UpdateError::Replace("у программы нет каталога".to_owned()))?;
        // Next to the executable, so the final rename stays on one filesystem.
        let mut file = tempfile::NamedTempFile::new_in(directory).map_err(replace)?;
        file.write_all(&binary).map_err(replace)?;
        file.as_file().sync_all().map_err(replace)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o755))
                .map_err(replace)?;
        }
        self_replace::self_replace(file.path()).map_err(replace)
    }
}
```

`map_or(true, ..)` clippy может попросить заменить на `is_none_or`; принять подсказку.

- [ ] **Step 6: Тесты проходят**

Run: `cargo test -p hub-app --lib && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS (82 + 7 = 89).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock crates/hub-app
git commit -m "hub-app: проверка и установка обновлений из GitHub Releases"
```

---

### Task 3: Обновление в окне, трее и при перезапуске

**Files:**
- Modify: `crates/hub-app/src/gui/look.rs`, `crates/hub-app/src/gui/app.rs`, `crates/hub-app/src/gui/tray.rs`, `crates/hub-app/src/gui/status.rs`, `crates/hub-app/src/main.rs`

**Interfaces:**
- Consumes: `updater::{UpdateState, UpdateCommand, run, UpdaterSetup}`, `github::{GitHub, SelfReplace}`, `updates::{Version, Target}`.
- Produces:
  - `gui::look::{Banner { text: String, action: Option<BannerAction> }, BannerAction::{Install, Restart}, banner(&UpdateState) -> Option<Banner>, update_item(&UpdateState) -> (String, bool)}`.
  - `gui::app::{Exit::{Staying, Quitting, Restarting}, ExitChoice = Arc<Mutex<Exit>>}`; `AppSetup` получает `updates: watch::Receiver<UpdateState>`, `update_commands: mpsc::Sender<UpdateCommand>`, `exit: ExitChoice`.
  - `Tray::show(&mut self, &Snapshot, &UpdateState)`.

- [ ] **Step 1: Падающие тесты (`look.rs`)**

```rust
    #[rstest]
    #[case(UpdateState::Idle, None)]
    #[case(UpdateState::Available(v("0.2.0")), Some(("Доступна версия 0.2.0", Some(BannerAction::Install))))]
    #[case(UpdateState::Installing(v("0.2.0")), Some(("Устанавливается версия 0.2.0…", None)))]
    #[case(UpdateState::Installed(v("0.2.0")), Some(("Версия 0.2.0 установлена — перезапустите приложение", Some(BannerAction::Restart))))]
    #[case(UpdateState::Failed("сеть".to_owned()), Some(("Обновление не установлено: сеть", Some(BannerAction::Install))))]
    fn update_banner(#[case] state: UpdateState, #[case] expected: Option<(&str, Option<BannerAction>)>) {
        let shown = banner(&state).map(|banner| (banner.text, banner.action));
        assert_eq!(shown, expected.map(|(text, action)| (text.to_owned(), action)));
    }

    #[rstest]
    #[case(UpdateState::Idle, ("Обновлений нет", false))]
    #[case(UpdateState::Available(v("0.2.0")), ("Доступно обновление v0.2.0", true))]
    #[case(UpdateState::Installed(v("0.2.0")), ("Перезапустите для v0.2.0", true))]
    fn update_menu_item(#[case] state: UpdateState, #[case] expected: (&str, bool)) {
        let (text, enabled) = update_item(&state);
        assert_eq!((text.as_str(), enabled), expected);
    }
```

Вспомогательная функция в тестах `look.rs`: `fn v(raw: &str) -> crate::updates::Version { crate::updates::Version::parse(raw).unwrap() }`; импорт `use crate::updater::UpdateState;`.

- [ ] **Step 2: Убедиться, что тесты падают**

Run: `cargo test -p hub-app --lib look`
Expected: FAIL — `cannot find function banner`.

- [ ] **Step 3: Реализация в `look.rs`**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerAction {
    Install,
    Restart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub text: String,
    pub action: Option<BannerAction>,
}

#[must_use]
pub fn banner(state: &UpdateState) -> Option<Banner> {
    let (text, action) = match state {
        UpdateState::Idle => return None,
        UpdateState::Available(version) => {
            (format!("Доступна версия {version}"), Some(BannerAction::Install))
        }
        UpdateState::Installing(version) => (format!("Устанавливается версия {version}…"), None),
        UpdateState::Installed(version) => (
            format!("Версия {version} установлена — перезапустите приложение"),
            Some(BannerAction::Restart),
        ),
        UpdateState::Failed(reason) => {
            (format!("Обновление не установлено: {reason}"), Some(BannerAction::Install))
        }
    };
    Some(Banner { text, action })
}

/// The tray item's text and whether it is clickable (it opens the window).
#[must_use]
pub fn update_item(state: &UpdateState) -> (String, bool) {
    match state {
        UpdateState::Idle | UpdateState::Failed(_) => ("Обновлений нет".to_owned(), false),
        UpdateState::Available(version) | UpdateState::Installing(version) => {
            (format!("Доступно обновление v{version}"), true)
        }
        UpdateState::Installed(version) => (format!("Перезапустите для v{version}"), true),
    }
}
```

`Failed` в трее → «Обновлений нет», неактивно: ошибка видна в окне, а трей не предлагает то, чего нет.

- [ ] **Step 4: Трей**

`tray.rs`: четвёртый пункт `update = MenuItem::new("Обновлений нет", false, None)`, вставить перед разделителем: `menu.append_items(&[&open, &toggle, &update, &PredefinedMenuItem::separator(), &quit])`; в `ids` добавить `(update.id().clone(), TrayAction::Open)` (тип `[(MenuId, TrayAction); 4]`); хранить `update: MenuItem` в `Tray`. `show(&mut self, snapshot: &Snapshot, updates: &UpdateState)`: в конце

```rust
        let (text, enabled) = update_item(updates);
        self.update.set_text(text);
        self.update.set_enabled(enabled);
```

- [ ] **Step 5: Окно**

`app.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    Staying,
    Quitting,
    Restarting,
}

/// How the window ended, read by `main` after the event loop returns.
pub type ExitChoice = Arc<Mutex<Exit>>;
```

- `AppSetup` и `HubApp` получают `updates: watch::Receiver<UpdateState>`, `update_commands: mpsc::Sender<UpdateCommand>`, `exit: ExitChoice` (поле `exit` в `HubApp` заменяется общим `ExitChoice`; чтение — `*self.exit.lock().unwrap_or_else(PoisonError::into_inner)`), плюс `restarting: bool`-заменитель — поле `confirm_restart: Confirm` где `enum Confirm { Hidden, Shown }`.
- `quit(ctx)` → записывает `Exit::Quitting`; новый `restart(ctx)` → `Exit::Restarting` и `ViewportCommand::Close`.
- Проверка закрытия в `logic`: прятать в трей только при `Exit::Staying`.
- `logic` передаёт `&self.updates.borrow()` в `tray.show`.
- В верхней панели под вкладками, после `notice`:

```rust
            let updates = self.updates.borrow().clone();
            if let Some(Banner { text, action }) = banner(&updates) {
                ui.horizontal(|ui| {
                    ui.label(text);
                    match action {
                        Some(BannerAction::Install) => {
                            if ui.button("Установить").clicked()
                                && self.update_commands.try_send(UpdateCommand::Install).is_err()
                            {
                                tracing::warn!("install request dropped: the updater is busy");
                            }
                        }
                        Some(BannerAction::Restart) => {
                            if ui.button("Перезапустить").clicked() {
                                if active(&snapshot) > 0 {
                                    self.confirm_restart = Confirm::Shown;
                                } else {
                                    self.restart(ui.ctx());
                                }
                            }
                        }
                        None => {
                            ui.spinner();
                        }
                    }
                });
            }
```

- Модальное окно подтверждения (как в `settings.rs`): «Перезапуск прервёт активные сессии (N). Продолжить?» → «Перезапустить» вызывает `restart`, «Отмена» скрывает.
- `status.rs`: в сетку добавить строку `ui.label("agent-hub"); ui.label(env!("CARGO_PKG_VERSION")); ui.end_row();` первой.

- [ ] **Step 6: `main.rs`**

- Каналы: `let (update_commands, update_inbox) = mpsc::channel(4); let (update_state, update_watched) = watch::channel(UpdateState::Idle); let exit: ExitChoice = Arc::new(Mutex::new(Exit::Staying));`.
- В замыкании создания окна после запуска супервизора:

```rust
            match (GitHub::new(), Version::parse(env!("CARGO_PKG_VERSION"))) {
                (Ok(source), Some(current)) => {
                    core.spawn(updater::run(
                        UpdaterSetup {
                            source,
                            installer: SelfReplace,
                            current,
                            target: Target::current(),
                            checks: checks,
                            state: update_state,
                            repaint: Arc::clone(&repaint),
                        },
                        update_inbox,
                    ));
                }
                (Err(error), _) => tracing::warn!(%error, "updates are off"),
                (Ok(_), None) => tracing::warn!("version is not x.y.z, updates are off"),
            }
```

  где `checks` — `watched.clone()`, сделанный до перемещения `watched` в `AppSetup`.
- `let _lock` → `let lock`. После остановки ядра и `runtime.shutdown_timeout(..)`:

```rust
    let restart = *exit.lock().unwrap_or_else(PoisonError::into_inner) == Exit::Restarting;
    // The new process must find the lock free, so it goes before the spawn.
    drop(lock);
    if restart {
        let program = std::env::current_exe()?;
        std::process::Command::new(program).spawn()?;
        tracing::info!("restarting into the new version");
    }
```

- [ ] **Step 7: Тесты, линтеры, ручная проверка**

Run: `cargo test -p hub-app --lib && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS (89 + 8 = 97).

Ручная проверка (Windows): запуск `target/debug/agent-hub.exe`. Репозиторий ещё без релизов → GitHub отвечает 404 → в логе `update check failed`, баннера нет, пункт трея «Обновлений нет» неактивен, вкладка «Статус» показывает версию 0.1.0. Выход через меню трея работает как раньше.

- [ ] **Step 8: Commit**

```bash
git add crates/hub-app
git commit -m "hub-app: обновление в окне и трее, перезапуск в новую версию"
```

---

### Task 4: Иконка приложения

**Files:**
- Create: `scripts/make-icon.py`, `assets/agent-hub.ico`, `crates/hub-app/build.rs`
- Modify: `crates/hub-app/Cargo.toml` (`[target.'cfg(windows)'.build-dependencies] winresource`), `Cargo.toml` (`winresource = "0.1"`), `crates/hub-app/src/main.rs` (иконка окна), `crates/hub-app/src/gui/tray.rs` (`pub fn app_icon() -> (Vec<u8>, u32)` — RGBA и сторона)

- [ ] **Step 1: Генератор `.ico` (только стандартная библиотека Python)**

`scripts/make-icon.py`:

```python
"""Draws the agent-hub icon (green disc, white ring) into assets/agent-hub.ico.

Run once after changing the design: python scripts/make-icon.py
"""
import math
import struct
import zlib
from pathlib import Path

GREEN = (46, 160, 67)
WHITE = (255, 255, 255)


def pixel(x: float, y: float, size: int) -> tuple[int, int, int, int]:
    radius = size / 2
    distance = math.hypot(x - radius, y - radius)
    if distance > radius - 0.5:
        return (0, 0, 0, 0)
    ring = abs(distance - radius * 0.55) < radius * 0.09
    r, g, b = WHITE if ring else GREEN
    return (r, g, b, 255)


def png(size: int) -> bytes:
    rows = b"".join(
        b"\x00" + b"".join(bytes(pixel(x + 0.5, y + 0.5, size)) for x in range(size))
        for y in range(size)
    )

    def chunk(kind: bytes, data: bytes) -> bytes:
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    header = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", header)
        + chunk(b"IDAT", zlib.compress(rows, 9))
        + chunk(b"IEND", b"")
    )


def ico(sizes: list[int]) -> bytes:
    images = [png(size) for size in sizes]
    offset = 6 + 16 * len(images)
    entries = b""
    for size, image in zip(sizes, images, strict=True):
        side = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", side, side, 0, 0, 1, 32, len(image), offset)
        offset += len(image)
    return struct.pack("<HHH", 0, 1, len(images)) + entries + b"".join(images)


if __name__ == "__main__":
    target = Path(__file__).resolve().parent.parent / "assets" / "agent-hub.ico"
    target.parent.mkdir(exist_ok=True)
    target.write_bytes(ico([16, 32, 48, 256]))
    print(f"wrote {target}")
```

Run: `python scripts/make-icon.py` → `assets/agent-hub.ico`.

- [ ] **Step 2: Ресурс Windows**

`Cargo.toml` → `winresource = "0.1"`. `crates/hub-app/Cargo.toml`:

```toml
[target.'cfg(windows)'.build-dependencies]
winresource.workspace = true
```

`crates/hub-app/build.rs`:

```rust
//! Embeds the application icon into the Windows executable.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/agent-hub.ico");
    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("../../assets/agent-hub.ico");
        if let Err(error) = resource.compile() {
            // A missing resource compiler must fail the build loudly, not ship without an icon.
            println!("cargo:warning=icon not embedded: {error}");
            std::process::exit(1);
        }
    }
}
```

`std::process::exit` в build-скрипте — способ упасть без `panic!` (запрещён линтом).

- [ ] **Step 3: Иконка окна**

`tray.rs`: вынести рисование круга в `pub fn app_icon() -> (Vec<u8>, u32)` — RGBA зелёного круга стороной `SIZE`, переиспользуя `circle`-логику (функция `pixels(rgb) -> Vec<u8>`; `circle` строит `Icon` из `pixels`). `main.rs`: `ViewportBuilder::default()...with_icon(Arc::new(egui::IconData { rgba, width: side, height: side }))`.

- [ ] **Step 4: Сборка и ручная проверка**

Run: `cargo build -p hub-app && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS. В Проводнике у `target/debug/agent-hub.exe` — зелёная иконка; запущенное окно показывает её в заголовке и на панели задач (скриншот).

- [ ] **Step 5: Commit**

```bash
git add scripts/make-icon.py assets/agent-hub.ico Cargo.toml Cargo.lock crates/hub-app
git commit -m "hub-app: иконка приложения для exe, окна и панели задач"
```

---

### Task 5: CI и `cargo deny`

**Files:**
- Create: `.github/workflows/ci.yml`, `deny.toml`

- [ ] **Step 1: `deny.toml`**

```toml
[graph]
all-features = false

[advisories]
version = 2
yanked = "deny"

[licenses]
version = 2
confidence-threshold = 0.9
allow = [
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unicode-3.0",
    "CDLA-Permissive-2.0",
    "MPL-2.0",
    "BSL-1.0",
    "OFL-1.1",
    "LicenseRef-UFL-1.0",
    "Ubuntu-font-1.0",
]

[bans]
multiple-versions = "allow"
wildcards = "deny"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
```

Список лицензий окончательно сверяется прогоном (Step 3): лицензия, которой нет в списке, добавляется, только если она пермиссивная; копилефт (GPL/AGPL/LGPL) — повод для разбора и записи в ledger, а не добавления. Внутренние крейты (`publish = false`) cargo-deny пропускает (`[licenses.private] ignore = true` — добавить).

- [ ] **Step 2: `.github/workflows/ci.yml`**

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

permissions:
  contents: read

env:
  CARGO_TERM_COLOR: always

jobs:
  check:
    name: ${{ matrix.os }}
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - name: Toolchain from rust-toolchain.toml
        run: rustup show
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets --locked -- -D warnings
      - run: cargo test --workspace --locked

  deny:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: EmbarkStudios/cargo-deny-action@v2
```

Системных пакетов для Ubuntu не нужно, если локальная Linux-сборка (Step 3) прошла на чистом `rust:1.96`; если ей понадобились пакеты, добавить шаг `sudo apt-get install -y …` с тем же списком и записать в ledger.

- [ ] **Step 3: Локальная проверка**

1. Linux: `docker run --rm -v <repo>:/src -v agent-hub-target:/target -v agent-hub-registry:/usr/local/cargo/registry -w /src -e CARGO_TARGET_DIR=/target rust:1.96 bash -c "cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked"` → PASS (в том числе unix-only тесты путей).
2. `cargo install cargo-deny --locked` (один раз), затем `cargo deny check` → `advisories ok, bans ok, licenses ok, sources ok`.
3. `docker run --rm -v <repo>:/repo -w /repo rhysd/actionlint:latest -color` → без замечаний.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/ci.yml deny.toml
git commit -m "ci: fmt, clippy и тесты на трёх ОС, cargo deny"
```

---

### Task 6: Релиз по тегу

**Files:**
- Create: `.github/workflows/release.yml`

- [ ] **Step 1: Workflow**

```yaml
name: Release

on:
  push:
    tags: ["v*"]

permissions:
  contents: write

env:
  CARGO_TERM_COLOR: always

jobs:
  build:
    name: ${{ matrix.target }}
    strategy:
      fail-fast: true
      matrix:
        include:
          - { target: x86_64-pc-windows-msvc, os: windows-latest, archive: zip, exe: .exe }
          - { target: aarch64-apple-darwin, os: macos-latest, archive: tar.gz, exe: "" }
          - { target: x86_64-apple-darwin, os: macos-latest, archive: tar.gz, exe: "" }
          - { target: x86_64-unknown-linux-gnu, os: ubuntu-22.04, archive: tar.gz, exe: "" }
    runs-on: ${{ matrix.os }}
    defaults:
      run:
        shell: bash
    steps:
      - uses: actions/checkout@v4
      - name: Tag matches the crate version
        run: |
          version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
          test "v${version}" = "${GITHUB_REF_NAME}" || { echo "tag ${GITHUB_REF_NAME} != v${version}"; exit 1; }
      - name: Toolchain
        run: |
          rustup show
          rustup target add ${{ matrix.target }}
      - uses: Swatinem/rust-cache@v2
        with:
          key: ${{ matrix.target }}
      - run: cargo build --release --locked --target ${{ matrix.target }} -p hub-app
      - name: Package
        run: |
          name=agent-hub-${{ matrix.target }}
          built=target/${{ matrix.target }}/release/agent-hub${{ matrix.exe }}
          mkdir -p dist stage
          cp "$built" "dist/${name}${{ matrix.exe }}"
          cp "$built" stage/
          cp README.md LICENSE stage/
          if [ "${{ matrix.archive }}" = zip ]; then
            (cd stage && 7z a "../dist/${name}.zip" .)
          else
            tar -czf "dist/${name}.tar.gz" -C stage .
          fi
          cd dist
          for file in *; do
            if command -v sha256sum >/dev/null; then sha256sum "$file"; else shasum -a 256 "$file"; fi > "${file}.sha256"
          done
          ls -l
      - uses: actions/upload-artifact@v4
        with:
          name: ${{ matrix.target }}
          path: dist/*

  publish:
    needs: build
    runs-on: ubuntu-latest
    steps:
      - uses: actions/download-artifact@v4
        with:
          path: dist
          merge-multiple: true
      - uses: softprops/action-gh-release@v2
        with:
          files: dist/*
          generate_release_notes: true
```

Цикл по `dist/*` создаёт `.sha256` для архивов и голых бинарников; уже созданные `.sha256` в цикл не попадают, потому что glob раскрывается до начала цикла.

- [ ] **Step 2: Проверка**

`docker run --rm -v <repo>:/repo -w /repo rhysd/actionlint:latest -color` → без замечаний. Локально повторить шаг Package для Windows в Git Bash на собранном `cargo build --release -p hub-app` (с `matrix.*`, подставленными вручную) → в `dist/` четыре файла: `.exe`, `.zip` и два `.sha256`; `sha256sum -c` по каждому `.sha256` → OK. Очистить `dist/` и `stage/` после проверки.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release.yml
git commit -m "ci: релиз по тегу v* — 4 таргета, архивы, бинарники и SHA-256"
```

---

### Task 7: README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Написать README (skill `readme-generator`)**

Разделы и обязательное содержание (источник текстов про бота — README Python-версии `../agent-hub/README.md`, разделы «Использование» и «Безопасность», адаптированные):

1. Заголовок и абзац: что это (Telegram-форум, тема = сессия Claude Code), десктоп с треем, Windows/macOS/Linux.
2. Требования: установленный Claude Code ≥ 2.1.280 с выполненным входом (`claude` в PATH или путь в настройках); Telegram-бот и форум-группа.
3. Установка: скачать архив своей платформы из Releases, проверить `.sha256`, распаковать; macOS — `xattr -d com.apple.quarantine agent-hub`; Windows — SmartScreen «Подробнее → Выполнить в любом случае»; Linux — значок трея требует хоста StatusNotifierItem (KDE, GNOME с расширением AppIndicator), без него окно сворачивается.
4. Первый запуск: создать бота у @BotFather (выключить Group Privacy), форум-группу, добавить бота админом; открыть «Настройки», ввести токен; узнать chat_id и свой user_id через вкладку «Лог» (написать в группу → строка `rejected update` → кнопки «Использовать chat_id» / «Добавить user_id»); корень рабочих каталогов; «Сохранить».
5. Использование: темы и сессии, команды `/new [backend] [путь]`, `/cwd`, `/reset`, `/stop`, `/status`, `/help`, подтверждения 🔐, вопросы агента, вложения (20 МБ), файлы от агента (`send_file`, 50 МБ), что приходит в тему — перенос из Python-README.
6. Окно и трей: вкладки, цвет значка, меню; закрытие окна прячет его в трей; выход — «Выход» в трее.
7. Настройки: таблица полей и где хранятся (`settings.toml`, keyring, `topics.json`, логи) с путями для трёх ОС; что применяется сразу, а что перезапускает бота.
8. Обновления: проверка при старте и раз в сутки, установка по кнопке, SHA-256, перезапуск; как выключить; остаточный риск (сумма из того же релиза).
9. Безопасность: перенос раздела Python-версии (allowlist, подтверждения, корень workspace, bypassPermissions) + токен в keyring + источник обновлений зашит.
10. Разработка: `cargo test --workspace`, `cargo clippy …`, структура крейтов (таблица hub-core/hub-claude/hub-telegram/hub-app), `python scripts/make-icon.py`, выпуск релиза (поднять `version` в `Cargo.toml`, тег `vX.Y.Z`).
11. Лицензия: MIT.

- [ ] **Step 2: Проверка**

Все команды и пути в README сверены с кодом: тексты команд с `hub-core::commands`, пути хранения с `AppDirs`, лимиты с `hub-core::attachments` и `paths`, минимальная версия с `hub_claude::version::MIN_VERSION`. Ссылки на релизы — `https://github.com/aprazdnikov/agent-hub-rs/releases`.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: README для десктопной версии"
```

---

## Самопроверка плана

- Спека «Автообновление»: проверка при старте и раз в 24 ч при `Enabled` (Task 2), баннер и пункт трея (Task 3), установка по кнопке (Task 3), скачивание файла своего таргета и SHA-256 (Task 1, 2), `self_replace` (Task 2), предложение перезапуска и подтверждение при активных сессиях (Task 3), остаточный риск — README (Task 7). «CI»: матрица трёх ОС, fmt/clippy/test, `cargo deny` отдельной задачей, кэш (Task 5). «Релиз»: 4 таргета, архивы, `.sha256`, `softprops/action-gh-release`, иконка `winresource` (Task 4, 6). «README» (Task 7). `UpdateError::{Check, Download, Checksum, Replace}` (Task 2).
- Типы: `Version`/`Target`/`Offer` (Task 1) → Task 2–3; `UpdateState`/`UpdateCommand` (Task 2) → Task 3; `Snapshot.saved.updates` — существующий `Draft::updates`.
