//! The part of the Bot API the hub uses, so everything above it is testable without Telegram.

use std::path::PathBuf;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_core::domain::{ChatId, MessageId, TopicKey};
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
}

impl Outgoing {
    #[must_use]
    pub fn plain(text: String) -> Self {
        Self { text, format: Format::Plain, keyboard: Vec::new() }
    }

    #[must_use]
    pub fn html(text: String) -> Self {
        Self { text, format: Format::Html, keyboard: Vec::new() }
    }

    #[must_use]
    pub fn with_keyboard(self, keyboard: Vec<Vec<Button>>) -> Self {
        Self { keyboard, ..self }
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
    fn save(&self, file: String, target: tokio::fs::File) -> BoxFuture<'_, Result<(), SendError>>;
}
