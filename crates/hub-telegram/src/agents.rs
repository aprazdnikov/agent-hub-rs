//! The agent backends behind the hub; adding one is a new `BackendKind` variant and a match arm.

use std::sync::Arc;

use futures::future::BoxFuture;
use hub_claude::backend::ClaudeBackend;
use hub_agent::conversation::Conversation;
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
                        // Nobody listening means the topic's session is already gone.
                        let _ =
                            conversation.events.send(AgentEvent::Failed(error.to_string())).await;
                        inbox
                    }
                },
            }
        })
    }
}
