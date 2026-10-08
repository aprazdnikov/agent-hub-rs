//! Starts the real bot: checks the workspace, topics and agent CLI, connects to Telegram,
//! then runs the hub and the update listener.

use futures::future::join_all;
use hub_codex::backend::{ProbeError, probe_auth};
use hub_codex::protocol::CodexAuth;
use hub_core::domain::BackendKind;
use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use hub_core::settings::Settings;
use hub_qwen::backend::QwenAuth;
use hub_telegram::agents::HubAgents;
use hub_telegram::hub::{self, HubHandle, HubMessage, HubSetup};
use hub_telegram::paths::{CwdError, workspace_root};
use hub_telegram::telegram::{self, Connection, Listener};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::error::StoreError;
use crate::supervisor::{AgentAuth, AgentState, AgentStatus, Connector, Started};
use crate::topics::{FileTopics, load_topics};

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Claude(#[from] hub_claude::version::CliError),
    #[error(transparent)]
    Codex(#[from] hub_codex::version::CliError),
    #[error(transparent)]
    Login(#[from] ProbeError),
    #[error(transparent)]
    Qwen(#[from] hub_qwen::version::CliError),
    #[error(transparent)]
    Hermes(#[from] hub_hermes::version::CliError),
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error(transparent)]
    Workspace(#[from] CwdError),
    #[error(transparent)]
    Topics(#[from] StoreError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    Telegram(#[from] telegram::StartError),
}

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
                tracing::warn!(
                    "codex is not logged in: run `codex login` or set an OpenAI API key"
                );
            }
            Ok(AgentState::Ready {
                version: version.to_string(),
                auth: Some(AgentAuth::Codex(auth)),
            })
        }
        BackendKind::Hermes => {
            let cli = hub_hermes::version::locate(settings.hermes.cli.as_deref())?;
            let version = hub_hermes::version::check(&cli).await?;
            // No trial session: Hermes owns its profiles, credentials and session database.
            Ok(AgentState::Ready { version: version.to_string(), auth: Some(AgentAuth::Hermes) })
        }
        BackendKind::Qwen => {
            let cli = hub_qwen::version::locate(settings.qwen.cli.as_deref())?;
            let version = hub_qwen::version::check(&cli).await?;
            // Sign-in is not probed: qwen reports it only on session/new, and a trial session
            // would leave a file in ~/.qwen.
            let auth = AgentAuth::Qwen(QwenAuth::of(&settings.qwen));
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
            Err(error) => {
                Ok(AgentStatus { kind, state: AgentState::Unavailable(error.to_string()) })
            }
        })
        .collect()
}

pub struct TelegramConnector {
    pub home: PathBuf,
    pub topics: PathBuf,
}

impl Connector for TelegramConnector {
    type Error = StartError;

    fn transient(error: &StartError) -> bool {
        match error {
            StartError::Telegram(telegram::StartError::Network(_)) => true,
            StartError::Telegram(telegram::StartError::InvalidToken)
            | StartError::Workspace(_)
            | StartError::Topics(_)
            | StartError::Agent(_) => false,
        }
    }

    fn connect(
        &self,
        settings: watch::Receiver<Arc<Settings>>,
    ) -> BoxFuture<'_, Result<Started, StartError>> {
        Box::pin(async move {
            let current = Arc::clone(&settings.borrow());
            workspace_root(&current.workspace_root)?;
            let topics = load_topics(&self.topics)?;
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
            let Connection { bot, messenger, username } =
                telegram::connect(&current.telegram.token, current.telegram.chat).await?;
            let HubHandle { mailbox, views, registries: _registries, task } =
                hub::spawn(HubSetup {
                    agents: Arc::new(HubAgents::new(settings.clone())),
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
                agents,
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

#[cfg(test)]
mod tests {
    use hub_core::domain::BackendKind;
    use hub_core::settings::{Draft, QwenDraft};

    use crate::supervisor::{AgentState, AgentStatus};

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

    #[test]
    fn only_an_unreachable_telegram_is_worth_retrying() {
        let offline = StartError::Telegram(telegram::StartError::Network("timeout".to_owned()));
        let rejected = StartError::Telegram(telegram::StartError::InvalidToken);
        let missing = StartError::Workspace(CwdError::Missing("/нет".into()));
        assert!(TelegramConnector::transient(&offline));
        assert!(!TelegramConnector::transient(&rejected));
        assert!(!TelegramConnector::transient(&missing));
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
        let connector =
            TelegramConnector { home: dir.path().to_path_buf(), topics: topics.clone() };
        let error = connector.connect(settings(dir.path())).await.err().unwrap();
        assert!(matches!(error, StartError::Topics(StoreError::Corrupt { .. })));
        assert!(error.to_string().contains("topics.json"));
        assert_eq!(std::fs::read_to_string(&topics).unwrap(), "не json");
    }

    fn ready(version: &str) -> AgentState {
        AgentState::Ready { version: version.to_owned(), auth: None }
    }

    #[tokio::test]
    async fn configured_missing_hermes_is_reported_without_telegram() {
        let dir = tempfile::tempdir().unwrap();
        let mut form = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: dir.path().display().to_string(),
            ..Draft::default()
        };
        form.hermes.cli = dir.path().join("missing-hermes").display().to_string();
        let settings = form.parse(dir.path()).unwrap();
        let error = check_agent(BackendKind::Hermes, &settings).await.unwrap_err();
        assert!(matches!(error, AgentError::Hermes(_)));
        assert!(matches!(
            statuses(BackendKind::Hermes, vec![(BackendKind::Hermes, Err(error))]),
            Err(AgentError::Hermes(_))
        ));
    }

    #[test]
    fn optional_missing_hermes_does_not_block_the_default() {
        let missing = AgentError::Hermes(hub_hermes::version::CliError::Missing);
        let statuses = statuses(
            BackendKind::Claude,
            vec![(BackendKind::Claude, Ok(ready("2.1.287"))), (BackendKind::Hermes, Err(missing))],
        )
        .unwrap();
        assert!(statuses.iter().any(|status| status.kind == BackendKind::Hermes
            && matches!(status.state, AgentState::Unavailable(_))));
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
                        "Codex не найден: укажите путь в настройках или установите `codex` в PATH"
                            .to_owned()
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

    #[test]
    fn qwen_that_is_not_the_default_may_be_missing() {
        let checked = vec![
            (BackendKind::Claude, Ok(ready("2.1.287"))),
            (BackendKind::Qwen, Err(AgentError::Qwen(hub_qwen::version::CliError::Missing))),
        ];
        assert_eq!(
            statuses(BackendKind::Claude, checked).unwrap(),
            [
                AgentStatus { kind: BackendKind::Claude, state: ready("2.1.287") },
                AgentStatus {
                    kind: BackendKind::Qwen,
                    state: AgentState::Unavailable(
                        "Qwen Code не найден: укажите путь в настройках или установите `qwen` в PATH"
                            .to_owned()
                    ),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_missing_default_qwen_stops_the_start() {
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
            default_backend: BackendKind::Qwen,
            qwen: QwenDraft {
                cli: dir.path().join("нет-qwen").display().to_string(),
                ..QwenDraft::default()
            },
            ..Draft::default()
        }
        .parse(dir.path())
        .unwrap();
        let receiver = watch::Sender::new(Arc::new(settings)).subscribe();
        let error = connector.connect(receiver).await.err().unwrap();
        assert!(matches!(error, StartError::Agent(AgentError::Qwen(_))));
    }
}
