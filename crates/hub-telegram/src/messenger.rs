//! The part of the Bot API the hub uses, so everything above it is testable without Telegram.

use std::path::PathBuf;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_core::domain::{ChatId, MessageId, ThreadId, TopicKey};
use hub_core::questions::Button;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Plain,
    Html,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub text: String,
    pub format: Format,
    /// Inline keyboard rows; empty means none.
    pub keyboard: Vec<Vec<Button>>,
    /// Ask for a reply to this message; incompatible with an inline keyboard.
    pub force_reply: bool,
    /// Reply within a topic; `Target::Reply` already supplies its own message id.
    pub reply_to: Option<MessageId>,
}

impl Outgoing {
    #[must_use]
    pub fn plain(text: String) -> Self {
        Self {
            text,
            format: Format::Plain,
            keyboard: Vec::new(),
            force_reply: false,
            reply_to: None,
        }
    }

    #[must_use]
    pub fn html(text: String) -> Self {
        Self {
            text,
            format: Format::Html,
            keyboard: Vec::new(),
            force_reply: false,
            reply_to: None,
        }
    }

    #[must_use]
    pub fn with_keyboard(self, keyboard: Vec<Vec<Button>>) -> Self {
        Self { keyboard, ..self }
    }

    #[must_use]
    pub fn with_force_reply(self) -> Self {
        Self { force_reply: true, ..self }
    }

    #[must_use]
    pub fn replying_to(self, message: MessageId) -> Self {
        Self { reply_to: Some(message), ..self }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Topic(TopicKey),
    /// A reply to a message outside any topic, e.g. in the General topic.
    Reply {
        chat: ChatId,
        message: MessageId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    #[error("Telegram просит подождать {} с", .0.as_secs())]
    RetryAfter(Duration),
    #[error("Telegram отклонил запрос: {0}")]
    Rejected(String),
    #[error("Telegram недоступен: {0}")]
    Failed(String),
}

pub trait Messenger: Send + Sync {
    /// Creates one forum topic without automatic retries. An uncertain result may leave a topic.
    fn create_topic(
        &self,
        chat: ChatId,
        name: String,
    ) -> BoxFuture<'_, Result<ThreadId, SendError>>;
    fn send(
        &self,
        target: Target,
        message: Outgoing,
    ) -> BoxFuture<'_, Result<MessageId, SendError>>;
    /// Replaces the text and drops the inline keyboard.
    fn edit(
        &self,
        chat: ChatId,
        message: MessageId,
        text: String,
        format: Format,
    ) -> BoxFuture<'_, Result<(), SendError>>;
    fn edit_keyboard(
        &self,
        chat: ChatId,
        message: MessageId,
        keyboard: Vec<Vec<Button>>,
    ) -> BoxFuture<'_, Result<(), SendError>>;
    /// Replaces text and keyboard in one request (including an empty keyboard).
    fn edit_card(
        &self,
        chat: ChatId,
        message: MessageId,
        card: Outgoing,
    ) -> BoxFuture<'_, Result<(), SendError>>;
    fn answer(
        &self,
        callback: String,
        text: Option<String>,
    ) -> BoxFuture<'_, Result<(), SendError>>;
    fn document(
        &self,
        key: TopicKey,
        path: PathBuf,
        caption: String,
    ) -> BoxFuture<'_, Result<(), SendError>>;
    fn typing(&self, key: TopicKey) -> BoxFuture<'_, Result<(), SendError>>;
    fn fetch(&self, file: String) -> BoxFuture<'_, Result<Vec<u8>, SendError>>;
    /// Reject oversized metadata and stop downloading before buffering more than `limit` bytes.
    fn fetch_limited(
        &self,
        file: String,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<u8>, SendError>>;
    fn save(&self, file: String, target: tokio::fs::File) -> BoxFuture<'_, Result<(), SendError>>;
}
