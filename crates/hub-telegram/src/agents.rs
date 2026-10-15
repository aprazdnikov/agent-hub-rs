//! The agent backends behind the hub; adding one is a new `BackendKind` variant and a match arm.

use std::env;
use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use hub_agent::archive::SessionArchive;
use hub_agent::conversation::Conversation;
use hub_claude::archive::ClaudeArchive;
use hub_claude::backend::ClaudeBackend;
use hub_codex::archive::CodexArchive;
use hub_codex::backend::CodexBackend;
use hub_core::domain::{AgentEvent, BackendKind, Prompt, TopicSession};
use hub_core::settings::Settings;
use hub_hermes::archive::HermesArchive;
use hub_hermes::backend::HermesBackend;
use hub_qwen::archive::QwenArchive;
use hub_qwen::backend::QwenBackend;
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
                BackendKind::Claude => {
                    match hub_claude::version::locate(settings.claude.cli.as_deref()) {
                        Ok(cli) => {
                            ClaudeBackend::new(cli, settings.claude.clone())
                                .run(session, prompt, inbox, conversation)
                                .await
                        }
                        Err(error) => refuse(&conversation, error.to_string(), inbox).await,
                    }
                }
                BackendKind::Codex => {
                    match hub_codex::version::locate(settings.codex.cli.as_deref()) {
                        Ok(cli) => {
                            CodexBackend::new(cli, settings.codex.clone())
                                .run(session, prompt, inbox, conversation)
                                .await
                        }
                        Err(error) => refuse(&conversation, error.to_string(), inbox).await,
                    }
                }
                BackendKind::Hermes => {
                    match hub_hermes::version::locate(settings.hermes.cli.as_deref()) {
                        Ok(cli) => {
                            HermesBackend::new(cli, settings.hermes.clone())
                                .run(session, prompt, inbox, conversation)
                                .await
                        }
                        Err(error) => refuse(&conversation, error.to_string(), inbox).await,
                    }
                }
                BackendKind::Qwen => {
                    match hub_qwen::version::locate(settings.qwen.cli.as_deref()) {
                        Ok(cli) => {
                            QwenBackend::new(cli, settings.qwen.clone())
                                .run(session, prompt, inbox, conversation)
                                .await
                        }
                        Err(error) => refuse(&conversation, error.to_string(), inbox).await,
                    }
                }
            }
        })
    }

    fn archive<'a>(
        &'a self,
        backend: BackendKind,
        home: &'a Path,
    ) -> BoxFuture<'a, Result<Arc<dyn SessionArchive>, String>> {
        let settings = Arc::clone(&self.settings.borrow());
        Box::pin(async move { archive(backend, &settings, home).await })
    }
}

/// The same CLI and environment the backend runs with: the storage root follows the variables
/// the agent itself reads, and the checked version goes into the manifest.
async fn archive(
    backend: BackendKind,
    settings: &Settings,
    home: &Path,
) -> Result<Arc<dyn SessionArchive>, String> {
    let archive: Arc<dyn SessionArchive> = match backend {
        BackendKind::Claude => {
            let cli = hub_claude::version::locate(settings.claude.cli.as_deref())
                .map_err(|error| error.to_string())?;
            let version =
                hub_claude::version::check(&cli).await.map_err(|error| error.to_string())?;
            let root = hub_claude::archive::storage_root(
                env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
                home,
            );
            Arc::new(ClaudeArchive::new(root, version.to_string()))
        }
        BackendKind::Codex => {
            let cli = hub_codex::version::locate(settings.codex.cli.as_deref())
                .map_err(|error| error.to_string())?;
            let version =
                hub_codex::version::check(&cli).await.map_err(|error| error.to_string())?;
            let root = hub_codex::archive::storage_root(env::var_os("CODEX_HOME").as_deref(), home);
            Arc::new(CodexArchive::new(root, version.to_string()))
        }
        BackendKind::Qwen => {
            let cli = hub_qwen::version::locate(settings.qwen.cli.as_deref())
                .map_err(|error| error.to_string())?;
            let version =
                hub_qwen::version::check(&cli).await.map_err(|error| error.to_string())?;
            let root = hub_qwen::archive::storage_root(
                env::var_os("QWEN_RUNTIME_DIR").as_deref(),
                env::var_os("QWEN_HOME").as_deref(),
                home,
            )
            .map_err(|error| error.to_string())?;
            Arc::new(QwenArchive::new(root, version.to_string()))
        }
        BackendKind::Hermes => {
            let cli = hub_hermes::version::locate(settings.hermes.cli.as_deref())
                .map_err(|error| error.to_string())?;
            let version =
                hub_hermes::version::check(&cli).await.map_err(|error| error.to_string())?;
            // Reads the head of the launcher: short and blocking, after the async check.
            let archive =
                HermesArchive::new(&cli, settings.hermes.profile.clone(), version.to_string())
                    .map_err(|error| error.to_string())?;
            Arc::new(archive)
        }
    };
    Ok(archive)
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_agent::channel::UserChannel;
    use hub_agent::conversation::Limits;
    use hub_core::domain::{
        AbsolutePath, Decision, FileDelivery, OutgoingFile, Question, QuestionsOutcome, ToolRequest,
    };
    use hub_core::settings::{CodexDraft, Draft, QwenDraft};
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
    async fn archives_check_the_configured_cli_first() {
        let root = std::env::temp_dir();
        let missing = root.join("definitely-not-an-agent-binary");
        let mut form = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            ..Draft::default()
        };
        form.cli = missing.display().to_string();
        form.codex.cli = missing.display().to_string();
        form.qwen.cli = missing.display().to_string();
        form.hermes.cli = missing.display().to_string();
        let agents =
            HubAgents::new(watch::Sender::new(Arc::new(form.parse(&root).unwrap())).subscribe());
        for backend in BackendKind::ALL {
            let Err(reason) = agents.archive(backend, &root).await else {
                panic!("{} archive without its CLI", backend.name());
            };
            assert!(
                reason.contains("definitely-not-an-agent-binary"),
                "{}: {reason}",
                backend.name()
            );
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

    #[tokio::test]
    async fn qwen_topics_run_the_configured_qwen() {
        let missing = std::env::temp_dir().join("definitely-not-qwen-binary");
        let settings = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: std::env::temp_dir().display().to_string(),
            qwen: QwenDraft { cli: missing.display().to_string(), ..QwenDraft::default() },
            ..Draft::default()
        }
        .parse(&std::env::temp_dir())
        .unwrap();
        let agents = HubAgents::new(watch::Sender::new(Arc::new(settings)).subscribe());
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        let session = TopicSession::fresh(BackendKind::Qwen, cwd);
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

        assert_eq!(
            received.recv().await,
            Some(AgentEvent::Failed(hub_qwen::backend::QWEN_FAILED.to_owned()))
        );
    }

    #[tokio::test]
    async fn hermes_topics_run_the_configured_hermes_and_preserve_inbox() {
        let root = std::env::temp_dir();
        let mut form = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            ..Draft::default()
        };
        form.hermes.cli = root.join("definitely-not-hermes-binary").display().to_string();
        let agents =
            HubAgents::new(watch::Sender::new(Arc::new(form.parse(&root).unwrap())).subscribe());
        let session = TopicSession::fresh(BackendKind::Hermes, AbsolutePath::new(root).unwrap());
        let (events, mut received) = mpsc::channel(4);
        let conversation = Conversation {
            channel: Arc::new(Allowing),
            events,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_secs(1)),
        };
        let (queued, inbox) = mpsc::channel(1);
        queued.send(Prompt::new("later".to_owned(), Vec::new()).unwrap()).await.unwrap();
        let mut inbox = agents
            .run(&session, Prompt::new("hi".to_owned(), Vec::new()).unwrap(), inbox, conversation)
            .await;
        assert!(
            matches!(received.recv().await, Some(AgentEvent::Failed(reason)) if reason.contains("Hermes"))
        );
        assert_eq!(inbox.try_recv().unwrap().text(), "later");
    }
}
