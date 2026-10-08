//! The agent backends behind the hub; adding one is a new `BackendKind` variant and a match arm.

use std::sync::Arc;

use futures::future::BoxFuture;
use hub_agent::conversation::Conversation;
use hub_claude::backend::ClaudeBackend;
use hub_codex::backend::CodexBackend;
use hub_core::domain::{AgentEvent, BackendKind, Prompt, TopicSession};
use hub_core::settings::Settings;
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
                BackendKind::Qwen => {
                    // Replaced by the Qwen backend once `hub-qwen` exists.
                    refuse(&conversation, "Qwen пока не подключён".to_owned(), inbox).await
                }
            }
        })
    }
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
    use hub_core::settings::{CodexDraft, Draft};
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
}
