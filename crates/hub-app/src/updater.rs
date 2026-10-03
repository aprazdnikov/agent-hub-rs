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
        // A repeated click must not replace the program a second time.
        let ready = match *self.state.borrow() {
            UpdateState::Available(_) | UpdateState::Failed(_) => true,
            UpdateState::Idle | UpdateState::Installing(_) | UpdateState::Installed(_) => false,
        };
        let Some(found) = self.offer.clone().filter(|_| ready) else { return };
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

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use hub_core::settings::{Draft, UpdateCheck};

    use super::*;
    use crate::supervisor::Snapshot;

    const BINARY: &[u8] = b"new agent-hub";

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

        fn download(
            &self,
            url: String,
            _limit: u64,
        ) -> BoxFuture<'_, Result<Vec<u8>, UpdateError>> {
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
        format!(
            "{}  agent-hub-x86_64-unknown-linux-gnu
",
            crate::updates::sha256_hex(bytes)
        )
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
        tokio::time::timeout(Duration::from_mins(1), state.wait_for(|s| done(s)))
            .await
            .unwrap()
            .unwrap();
    }

    fn checks(harness: &Harness) -> usize {
        harness.source.checks.load(Ordering::SeqCst)
    }

    #[tokio::test(start_paused = true)]
    async fn newer_release_becomes_available() {
        let mut harness =
            harness(Ok(release("v0.2.0")), &sha256_line(BINARY), UpdateCheck::Enabled);
        until(&mut harness.state, |s| matches!(s, UpdateState::Available(_))).await;
        assert_eq!(
            *harness.state.borrow(),
            UpdateState::Available(Version::parse("0.2.0").unwrap())
        );
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
        let mut harness =
            harness(Ok(release("v0.2.0")), &sha256_line(BINARY), UpdateCheck::Enabled);
        until(&mut harness.state, |s| matches!(s, UpdateState::Available(_))).await;
        harness.commands.send(UpdateCommand::Install).await.unwrap();
        until(&mut harness.state, |s| matches!(s, UpdateState::Installed(_))).await;
        assert_eq!(*harness.installer.installed.lock().unwrap(), [BINARY.to_vec()]);
    }

    #[tokio::test(start_paused = true)]
    async fn corrupted_download_is_refused() {
        let mut harness =
            harness(Ok(release("v0.2.0")), &sha256_line(b"other"), UpdateCheck::Enabled);
        until(&mut harness.state, |s| matches!(s, UpdateState::Available(_))).await;
        harness.commands.send(UpdateCommand::Install).await.unwrap();
        until(&mut harness.state, |s| matches!(s, UpdateState::Failed(_))).await;
        let UpdateState::Failed(reason) = harness.state.borrow().clone() else { unreachable!() };
        assert!(reason.contains("не совпала"), "{reason}");
        assert!(harness.installer.installed.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_second_install_request_changes_nothing() {
        let mut harness =
            harness(Ok(release("v0.2.0")), &sha256_line(BINARY), UpdateCheck::Enabled);
        until(&mut harness.state, |s| matches!(s, UpdateState::Available(_))).await;
        harness.commands.send(UpdateCommand::Install).await.unwrap();
        harness.commands.send(UpdateCommand::Install).await.unwrap();
        until(&mut harness.state, |s| matches!(s, UpdateState::Installed(_))).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(harness.installer.installed.lock().unwrap().len(), 1);
        assert!(matches!(*harness.state.borrow(), UpdateState::Installed(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn nothing_is_checked_after_an_install() {
        let mut harness =
            harness(Ok(release("v0.2.0")), &sha256_line(BINARY), UpdateCheck::Enabled);
        until(&mut harness.state, |s| matches!(s, UpdateState::Available(_))).await;
        harness.commands.send(UpdateCommand::Install).await.unwrap();
        until(&mut harness.state, |s| matches!(s, UpdateState::Installed(_))).await;
        tokio::time::sleep(CHECK_EVERY * 2).await;
        assert_eq!(checks(&harness), 1);
    }
}
