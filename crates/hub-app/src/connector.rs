//! Starts the real bot: checks the workspace, topics and agent CLI, connects to Telegram,
//! then runs the hub and the update listener.

use std::path::PathBuf;
use std::sync::Arc;

use futures::future::BoxFuture;
use hub_claude::version::{CliError, check, locate};
use hub_core::settings::Settings;
use hub_telegram::agents::HubAgents;
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

    fn transient(error: &StartError) -> bool {
        match error {
            StartError::Telegram(telegram::StartError::Network(_)) => true,
            StartError::Telegram(telegram::StartError::InvalidToken)
            | StartError::Workspace(_)
            | StartError::Topics(_)
            | StartError::Claude(_) => false,
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
            let cli = locate(current.claude.cli.as_deref())?;
            let version = check(&cli).await?;
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
}
