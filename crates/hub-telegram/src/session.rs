//! One topic session as a task: downloads the first turn, runs the agent, relays its events
//! to the topic, and reports back to the hub.

use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt;
use hub_agent::conversation::{Conversation, Limits};
use hub_core::domain::{AbsolutePath, AgentEvent, Prompt, SessionId, TopicKey, TopicSession};
use hub_core::render::{format_abandoned, format_finished};
use hub_core::settings::Settings;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::channel::{Shared, TelegramChannel};
use crate::delivery::{self, Delivery, Style};
use crate::download::download;
use crate::hub::{Agents, HubMessage};
use crate::inbound::Turn;
use crate::messenger::Target;
use crate::paths::{CwdError, inside};
use crate::results::SaveResult;
use crate::sender::Sender;
use crate::texts;
use hub_core::results::{MAX_TEXT_BYTES, SavedResult};

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
            generation,
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
            relay(
                received,
                &sender,
                key,
                &mailbox,
                background,
                ResultContext { generation, session: &session, cancel: &cancel }
            ),
        );
        if cancel.is_cancelled() {
            sender.text(Target::Topic(key), texts::STOPPED).await;
        }
        inbox
    }
}

struct ResultContext<'a> {
    generation: u64,
    session: &'a TopicSession,
    cancel: &'a CancellationToken,
}

#[derive(Default)]
struct Pending {
    text: String,
    overflow: bool,
}
impl Pending {
    async fn push(&mut self, text: String, output: &Delivery) {
        if text.trim().is_empty() {
            return;
        }
        if self.overflow
            || self.text.len().saturating_add(text.len()).saturating_add(if self.text.is_empty() {
                0
            } else {
                2
            }) > MAX_TEXT_BYTES
        {
            if !self.text.is_empty() {
                output.send_all(Style::Markdown, std::mem::take(&mut self.text)).await;
            }
            self.overflow = true;
            output.send_all(Style::Markdown, text).await;
        } else {
            if !self.text.is_empty() {
                self.text.push_str("\n\n");
            }
            self.text.push_str(&text);
        }
    }
    fn flush(&mut self, output: &Delivery) {
        if !self.text.is_empty() {
            output.send(Style::Markdown, std::mem::take(&mut self.text));
        }
    }
    fn boundary(&mut self, output: &Delivery) {
        self.flush(output);
        self.overflow = false;
    }
}

async fn relay(
    events: mpsc::Receiver<AgentEvent>,
    sender: &Sender,
    key: TopicKey,
    mailbox: &mpsc::Sender<HubMessage>,
    background: Duration,
    context: ResultContext<'_>,
) {
    let (output, delivery) = delivery::channel();
    tokio::join!(
        collect(events, output, key, mailbox, background, &context),
        delivery.run(sender, key, context.cancel)
    );
}

async fn collect(
    mut events: mpsc::Receiver<AgentEvent>,
    output: Delivery,
    key: TopicKey,
    mailbox: &mpsc::Sender<HubMessage>,
    background: Duration,
    context: &ResultContext<'_>,
) {
    let mut pending = Pending::default();
    while let Some(event) = events.recv().await {
        match event {
            AgentEvent::SessionStarted(session) => bind(mailbox, key, session).await,
            AgentEvent::AssistantText(text) => pending.push(text, &output).await,
            AgentEvent::ToolCall(call) => {
                pending.boundary(&output);
                output.send(Style::Html, texts::tool_call_html(&call));
            }
            AgentEvent::Finished(finished) => {
                tracing::info!(usage = ?finished.usage, background = finished.background, "turn finished");
                if !context.cancel.is_cancelled() {
                    if pending.overflow {
                        output
                            .send_all(
                                Style::Plain,
                                "⚠️ Ответ превышает 64 КиБ и не сохранён для /last.".to_owned(),
                            )
                            .await;
                    } else if !pending.text.is_empty() {
                        let session =
                            context.session.clone().with_session(Some(finished.session.clone()));
                        let record = SavedResult::new(
                            key,
                            uuid::Uuid::new_v4().simple().to_string(),
                            chrono::Utc::now().timestamp(),
                            &session,
                            pending.text.clone(),
                        );
                        let saved = match record {
                            Ok(record) => {
                                save_result(mailbox, key, context.generation, record).await
                            }
                            Err(error) => Err(error),
                        };
                        if let Err(error) = saved {
                            tracing::warn!(%error, "last result not saved");
                            output.final_message(
                                Style::Plain,
                                format!("⚠️ Ответ не сохранён для /last: {error}"),
                            );
                        }
                    }
                }
                if !pending.text.is_empty() {
                    output.final_message(Style::Markdown, std::mem::take(&mut pending.text));
                }
                pending.overflow = false;
                bind(mailbox, key, finished.session.clone()).await;
                output.final_message(Style::Plain, format_finished(&finished));
            }
            AgentEvent::BackgroundAbandoned(tasks) => {
                pending.boundary(&output);
                tracing::warn!(tasks = tasks.len(), "background tasks abandoned");
                output.send(Style::Plain, format_abandoned(&tasks, background));
            }
            AgentEvent::Failed(reason) => {
                pending.boundary(&output);
                tracing::warn!(%reason, "turn failed");
                output.send(Style::Plain, texts::failure(&reason));
            }
            AgentEvent::Quota(update) => {
                let _ = mailbox.send(HubMessage::Quota(update)).await;
            }
        }
    }
    pending.flush(&output);
}

async fn save_result(
    mailbox: &mpsc::Sender<HubMessage>,
    key: TopicKey,
    generation: u64,
    record: SavedResult,
) -> Result<(), String> {
    let (reply, saved) = oneshot::channel();
    mailbox
        .send(HubMessage::SaveResult(SaveResult { key, generation, record, reply }))
        .await
        .map_err(|_| "Бот уже остановлен".to_owned())?;
    tokio::time::timeout(Duration::from_secs(6), saved)
        .await
        .map_err(|_| "Сохранение не подтвердилось за 6 с".to_owned())?
        .map_err(|_| "Хранилище не подтвердило запись".to_owned())?
}

/// Persisted at once, so a crash mid-turn can still resume the session.
async fn bind(mailbox: &mpsc::Sender<HubMessage>, key: TopicKey, session: SessionId) {
    // The hub is gone only during shutdown.
    let _ = mailbox.send(HubMessage::Bind { key, session }).await;
}
