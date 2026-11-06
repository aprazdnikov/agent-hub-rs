//! Independent read-only checks for the last saved configuration.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::sync::Semaphore;

const CHECK_TIMEOUT: Duration = Duration::from_secs(10);
static WORKSPACE_WORKER: Semaphore = Semaphore::const_new(1);
static CLI_LOOKUP_WORKER: Semaphore = Semaphore::const_new(1);

async fn blocking_check<T: Send + 'static>(
    slot: &'static Semaphore,
    check: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let permit = slot.acquire().await.ok()?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        check()
    })
    .await
    .ok()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceCheck {
    Ready,
    Unavailable,
    TimedOut,
}

pub async fn check_workspace(path: &Path) -> WorkspaceCheck {
    let path = path.to_path_buf();
    match tokio::time::timeout(
        CHECK_TIMEOUT,
        blocking_check(&WORKSPACE_WORKER, move || {
            if hub_telegram::paths::workspace_root(&path).is_ok() {
                WorkspaceCheck::Ready
            } else {
                WorkspaceCheck::Unavailable
            }
        }),
    )
    .await
    {
        Ok(Some(result)) => result,
        Ok(None) => WorkspaceCheck::Unavailable,
        Err(_) => WorkspaceCheck::TimedOut,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentCheck {
    Ready(String),
    Unavailable,
    TimedOut,
}

pub async fn check_cli(settings: &hub_core::settings::Settings) -> AgentCheck {
    use hub_core::domain::BackendKind;

    let kind = settings.default_backend;
    let configured = match kind {
        BackendKind::Claude => settings.claude.cli.clone(),
        BackendKind::Codex => settings.codex.cli.clone(),
        BackendKind::Qwen => settings.qwen.cli.clone(),
        BackendKind::Hermes => settings.hermes.cli.clone(),
        BackendKind::Opencode => settings.opencode.cli.clone(),
    };
    let checked = async move {
        let path =
            blocking_check(&CLI_LOOKUP_WORKER, move || locate_cli(kind, configured.as_deref()))
                .await??;
        match kind {
            BackendKind::Claude => {
                hub_claude::version::check(&path).await.ok().map(|version| version.to_string())
            }
            BackendKind::Codex => {
                hub_codex::version::check(&path).await.ok().map(|version| version.to_string())
            }
            BackendKind::Qwen => {
                hub_qwen::version::check(&path).await.ok().map(|version| version.to_string())
            }
            BackendKind::Hermes => {
                hub_hermes::version::check(&path).await.ok().map(|version| version.to_string())
            }
            BackendKind::Opencode => {
                hub_opencode::version::check(&path).await.ok().map(|version| version.to_string())
            }
        }
    };
    match tokio::time::timeout(CHECK_TIMEOUT, checked).await {
        Ok(Some(version)) => AgentCheck::Ready(version),
        Ok(None) => AgentCheck::Unavailable,
        Err(_) => AgentCheck::TimedOut,
    }
}

fn locate_cli(kind: hub_core::domain::BackendKind, path: Option<&Path>) -> Option<PathBuf> {
    use hub_core::domain::BackendKind;
    match kind {
        BackendKind::Claude => hub_claude::version::locate(path).ok(),
        BackendKind::Codex => hub_codex::version::locate(path).ok(),
        BackendKind::Qwen => hub_qwen::version::locate(path).ok(),
        BackendKind::Hermes => hub_hermes::version::locate(path).ok(),
        BackendKind::Opencode => hub_opencode::version::locate(path).ok(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub workspace: WorkspaceCheck,
    pub agent: AgentCheck,
    pub agent_kind: hub_core::domain::BackendKind,
    pub telegram: hub_telegram::telegram::TelegramDiagnostic,
}

/// Checks a snapshot of the saved settings, without starting a task or a bot.
pub async fn run(settings: std::sync::Arc<hub_core::settings::Settings>) -> Report {
    let (workspace, agent, telegram) = tokio::join!(
        check_workspace(&settings.workspace_root),
        check_cli(&settings),
        hub_telegram::telegram::diagnose(&settings.telegram.token, settings.telegram.chat),
    );
    Report { workspace, agent, agent_kind: settings.default_backend, telegram }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_blocking_lookup_keeps_its_slot_until_worker_exits() {
        static SLOT: Semaphore = Semaphore::const_new(1);
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let task = tokio::spawn(blocking_check(&SLOT, move || {
            let _ = started.send(());
            let _ = wait.recv();
        }));
        ready.await.unwrap();
        task.abort();
        assert!(SLOT.try_acquire().is_err());
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while SLOT.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn missing_selected_cli_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let settings = hub_core::settings::Draft {
            token: "1:a".into(),
            chat: "-100".into(),
            users: "1".into(),
            workspace_root: dir.path().display().to_string(),
            cli: dir.path().join("missing-cli").display().to_string(),
            ..Default::default()
        }
        .parse(dir.path())
        .unwrap();
        assert_eq!(check_cli(&settings).await, AgentCheck::Unavailable);
    }

    #[tokio::test]
    async fn missing_workspace_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let result = check_workspace(&dir.path().join("missing")).await;
        assert_eq!(result, WorkspaceCheck::Unavailable);
    }
}
