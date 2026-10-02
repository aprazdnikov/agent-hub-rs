//! One topic session as a task: downloads the first turn, runs the agent, relays its events
//! to the topic, and reports back to the hub.

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use hub_claude::session::{Conversation, Limits};
use hub_core::domain::{AbsolutePath, AgentEvent, Prompt, SessionId, TopicKey, TopicSession};
use hub_core::render::{format_abandoned, format_finished};
use hub_core::settings::Settings;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::channel::{Shared, TelegramChannel};
use crate::download::download;
use crate::hub::{Agents, HubMessage};
use crate::inbound::Turn;
use crate::messenger::{Outgoing, Target};
use crate::paths::{CwdError, inside};
use crate::sender::Sender;
use crate::texts;

const EVENTS: usize = 256;

pub(crate) enum First {
    Turn(Turn),
    Prompt(Prompt),
}

pub(crate) struct Run<A> {
    pub(crate) key: TopicKey,
    pub(crate) session: TopicSession,
    pub(crate) first: First,
    pub(crate) inbox: mpsc::Receiver<Prompt>,
    pub(crate) cancel: CancellationToken,
    pub(crate) generation: u64,
    pub(crate) root: Result<AbsolutePath, CwdError>,
    pub(crate) settings: Arc<Settings>,
    pub(crate) sender: Sender,
    pub(crate) registries: Shared,
    pub(crate) agents: Arc<A>,
    pub(crate) home: PathBuf,
    pub(crate) mailbox: mpsc::Sender<HubMessage>,
}

impl<A: Agents> Run<A> {
    pub(crate) async fn execute(self) {
        let (key, generation) = (self.key, self.generation);
        let (sender, mailbox) = (self.sender.clone(), self.mailbox.clone());
        let inbox = if let Ok(inbox) = AssertUnwindSafe(self.converse()).catch_unwind().await {
            Some(inbox)
        } else {
            tracing::error!(chat = key.chat.0, thread = key.thread.0, "session crashed");
            sender.text(Target::Topic(key), texts::INTERNAL_ERROR).await;
            None
        };
        // The hub is gone only during shutdown, when leftover prompts no longer matter.
        let _ = mailbox.send(HubMessage::Ended { key, generation, inbox }).await;
    }

    async fn converse(self) -> mpsc::Receiver<Prompt> {
        let Self {
            key,
            session,
            first,
            inbox,
            cancel,
            generation: _generation,
            root,
            settings,
            sender,
            registries,
            agents,
            home,
            mailbox,
        } = self;
        sender.typing(key).await;
        let root = match root {
            Ok(root) => root,
            Err(error) => {
                sender.text(Target::Topic(key), &texts::warning(&error)).await;
                return inbox;
            }
        };
        if !inside(&root, &session.cwd) {
            sender.text(Target::Topic(key), &texts::outside_root(&session.cwd, &root)).await;
            return inbox;
        }
        let prompt = match first {
            First::Prompt(prompt) => prompt,
            First::Turn(turn) => match download(&sender, &session.cwd, turn).await {
                Ok(prompt) => prompt,
                Err(error) => {
                    sender.text(Target::Topic(key), &texts::warning(&error)).await;
                    return inbox;
                }
            },
        };
        let channel = Arc::new(TelegramChannel::new(
            sender.clone(),
            key,
            session.cwd.clone(),
            home,
            registries,
            settings.timeouts.approval,
        ));
        let (events, received) = mpsc::channel(EVENTS);
        let conversation = Conversation {
            channel,
            events,
            cancel: cancel.clone(),
            limits: Limits::new(settings.timeouts.background),
        };
        let background = settings.timeouts.background;
        let (inbox, ()) = tokio::join!(
            agents.run(&session, prompt, inbox, conversation),
            relay(received, &sender, key, &mailbox, background),
        );
        if cancel.is_cancelled() {
            sender.text(Target::Topic(key), texts::STOPPED).await;
        }
        inbox
    }
}

async fn relay(
    mut events: mpsc::Receiver<AgentEvent>,
    sender: &Sender,
    key: TopicKey,
    mailbox: &mpsc::Sender<HubMessage>,
    background: Duration,
) {
    while let Some(event) = events.recv().await {
        match event {
            AgentEvent::SessionStarted(session) => bind(mailbox, key, session).await,
            AgentEvent::AssistantText(text) => sender.markdown(key, &text).await,
            AgentEvent::ToolCall(call) => {
                sender.one(Target::Topic(key), Outgoing::html(texts::tool_call_html(&call))).await;
            }
            AgentEvent::Finished(finished) => {
                tracing::info!(
                    turns = finished.turns,
                    background = finished.background,
                    "turn finished"
                );
                bind(mailbox, key, finished.session.clone()).await;
                let line = format_finished(finished.turns, finished.cost, finished.background);
                sender.text(Target::Topic(key), &line).await;
            }
            AgentEvent::BackgroundAbandoned(tasks) => {
                tracing::warn!(tasks = tasks.len(), "background tasks abandoned");
                sender.text(Target::Topic(key), &format_abandoned(&tasks, background)).await;
            }
            AgentEvent::Failed(reason) => {
                tracing::warn!(%reason, "turn failed");
                sender.text(Target::Topic(key), &texts::failure(&reason)).await;
            }
        }
    }
}

/// Persisted at once, so a crash mid-turn can still resume the session.
async fn bind(mailbox: &mpsc::Sender<HubMessage>, key: TopicKey, session: SessionId) {
    // The hub is gone only during shutdown.
    let _ = mailbox.send(HubMessage::Bind { key, session }).await;
}
