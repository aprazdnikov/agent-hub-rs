//! The agent backends behind the hub; adding one is a new `BackendKind` variant and a match arm.

use std::env;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::future::{BoxFuture, join_all};
use hub_agent::archive::SessionArchive;
use hub_agent::conversation::Conversation;
use hub_claude::archive::ClaudeArchive;
use hub_claude::backend::ClaudeBackend;
use hub_codex::archive::CodexArchive;
use hub_codex::backend::CodexBackend;
use hub_core::domain::{AgentEvent, BackendKind, Prompt, TopicSession};
use hub_core::quota::QuotaReport;
use hub_core::settings::Settings;
use hub_hermes::archive::HermesArchive;
use hub_hermes::backend::HermesBackend;
use hub_opencode::backend::OpencodeBackend;
use hub_qwen::archive::QwenArchive;
use hub_qwen::backend::QwenBackend;
use tokio::sync::{mpsc, watch};

use crate::hub::Agents;
use crate::limits::Outcome;

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
                BackendKind::Opencode => {
                    match hub_opencode::version::locate(settings.opencode.cli.as_deref()) {
                        Ok(cli) => {
                            OpencodeBackend::new(cli, settings.opencode.clone())
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

    fn quotas(&self) -> BoxFuture<'_, Vec<Outcome>> {
        let settings = Arc::clone(&self.settings.borrow());
        Box::pin(async move { quotas(&settings).await })
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
            )
            .map_err(|error| error.to_string())?;
            Arc::new(ClaudeArchive::new(root, version.to_string()))
        }
        BackendKind::Codex => {
            let cli = hub_codex::version::locate(settings.codex.cli.as_deref())
                .map_err(|error| error.to_string())?;
            let version =
                hub_codex::version::check(&cli).await.map_err(|error| error.to_string())?;
            let root = hub_codex::archive::storage_root(env::var_os("CODEX_HOME").as_deref(), home)
                .map_err(|error| error.to_string())?;
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
        BackendKind::Opencode => {
            // The CLI is checked first so a missing or broken binary is reported as such.
            // OpenCode keeps sessions in its own SQLite database; snapshots are not implemented.
            let cli = hub_opencode::version::locate(settings.opencode.cli.as_deref())
                .map_err(|error| error.to_string())?;
            hub_opencode::version::check(&cli).await.map_err(|error| error.to_string())?;
            return Err(
                "Снимки и перенос сессий OpenCode не поддерживаются; история остаётся у агента"
                    .to_owned(),
            );
        }
    };
    Ok(archive)
}

/// Per backend, on top of the probes' own deadlines; dropping a probe kills its process.
const QUOTA_TIMEOUT: Duration = Duration::from_secs(20);
const QUOTA_TIMEOUT_TEXT: &str = "проба не ответила за 20 с";

/// Probes of every found CLI, side by side: one backend's failure or timeout leaves the
/// others alone. A CLI that is not found is skipped, as the card shows found agents only.
async fn quotas(settings: &Settings) -> Vec<Outcome> {
    let probes = BackendKind::ALL.into_iter().filter_map(|backend| {
        let probe: BoxFuture<'static, Result<QuotaReport, String>> = match backend {
            BackendKind::Claude => {
                let cli = hub_claude::version::locate(settings.claude.cli.as_deref()).ok()?;
                Box::pin(async move {
                    hub_claude::usage::probe(&cli).await.map_err(|error| {
                        let reason = match &error {
                            hub_claude::usage::ProbeError::Rejected(_) => {
                                "Claude отклонил запрос лимитов".to_owned()
                            }
                            hub_claude::usage::ProbeError::Shape
                            | hub_claude::usage::ProbeError::Disabled
                            | hub_claude::usage::ProbeError::Spawn(_)
                            | hub_claude::usage::ProbeError::Timeout
                            | hub_claude::usage::ProbeError::NoAnswer
                            | hub_claude::usage::ProbeError::TooLarge
                            | hub_claude::usage::ProbeError::Io(_) => error.to_string(),
                        };
                        failed(backend, error.kind(), reason)
                    })
                })
            }
            BackendKind::Codex => {
                let cli = hub_codex::version::locate(settings.codex.cli.as_deref()).ok()?;
                Box::pin(async move {
                    hub_codex::usage::probe(&cli).await.map_err(|error| {
                        let reason = match &error {
                            hub_codex::usage::ProbeError::Rpc(_) => {
                                "Codex app-server не ответил о лимитах".to_owned()
                            }
                            hub_codex::usage::ProbeError::Spawn(_)
                            | hub_codex::usage::ProbeError::Shape
                            | hub_codex::usage::ProbeError::Timeout => error.to_string(),
                        };
                        failed(backend, error.kind(), reason)
                    })
                })
            }
            BackendKind::Qwen => {
                hub_qwen::version::locate(settings.qwen.cli.as_deref()).ok()?;
                Box::pin(async { Ok(QuotaReport::not_reported(BackendKind::Qwen)) })
            }
            BackendKind::Hermes => {
                hub_hermes::version::locate(settings.hermes.cli.as_deref()).ok()?;
                Box::pin(async { Ok(QuotaReport::not_reported(BackendKind::Hermes)) })
            }
            BackendKind::Opencode => {
                hub_opencode::version::locate(settings.opencode.cli.as_deref()).ok()?;
                Box::pin(async { Ok(QuotaReport::not_reported(BackendKind::Opencode)) })
            }
        };
        Some(async move {
            let outcome = tokio::time::timeout(QUOTA_TIMEOUT, probe)
                .await
                .unwrap_or_else(|_| Err(failed(backend, "timeout", QUOTA_TIMEOUT_TEXT.to_owned())));
            (backend, outcome)
        })
    });
    join_all(probes).await
}

/// Logs the backend and the kind of failure only: the text can carry the agent's words.
fn failed(backend: BackendKind, kind: &'static str, text: String) -> String {
    tracing::warn!(backend = backend.name(), error = kind, "quota probe failed");
    text
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
        form.opencode.cli = missing.display().to_string();
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

    #[tokio::test]
    async fn opencode_topics_run_the_configured_opencode_and_preserve_inbox() {
        let root = std::env::temp_dir();
        let mut form = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            ..Draft::default()
        };
        form.opencode.cli = root.join("definitely-not-opencode-binary").display().to_string();
        let agents =
            HubAgents::new(watch::Sender::new(Arc::new(form.parse(&root).unwrap())).subscribe());
        let session = TopicSession::fresh(BackendKind::Opencode, AbsolutePath::new(root).unwrap());
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
            matches!(received.recv().await, Some(AgentEvent::Failed(reason)) if reason.contains("OpenCode"))
        );
        assert_eq!(inbox.try_recv().unwrap().text(), "later");
    }

    #[tokio::test]
    async fn quotas_probe_every_configured_cli_and_isolate_failures() {
        let root = std::env::temp_dir();
        let missing = root.join("definitely-not-an-agent-binary").display().to_string();
        let mut form = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            ..Draft::default()
        };
        form.cli.clone_from(&missing);
        form.codex.cli.clone_from(&missing);
        form.qwen.cli.clone_from(&missing);
        form.hermes.cli.clone_from(&missing);
        form.opencode.cli = missing;
        let agents =
            HubAgents::new(watch::Sender::new(Arc::new(form.parse(&root).unwrap())).subscribe());

        let outcomes = agents.quotas().await;

        let backends: Vec<_> = outcomes.iter().map(|(backend, _)| *backend).collect();
        assert_eq!(backends, BackendKind::ALL);
        let [(_, claude), (_, codex), (_, qwen), (_, hermes), (_, opencode)] = outcomes.as_slice()
        else {
            panic!("{outcomes:?}");
        };
        assert!(
            matches!(claude, Err(reason) if reason.starts_with("не удалось запустить Claude")
                || reason.starts_with("проба лимитов Claude отключена")),
            "{claude:?}"
        );
        assert!(
            matches!(codex, Err(reason) if reason.starts_with("не удалось запустить Codex app-server")),
            "{codex:?}"
        );
        assert_eq!(qwen, &Ok(QuotaReport::not_reported(BackendKind::Qwen)));
        assert_eq!(hermes, &Ok(QuotaReport::not_reported(BackendKind::Hermes)));
        assert_eq!(opencode, &Ok(QuotaReport::not_reported(BackendKind::Opencode)));
    }
}
