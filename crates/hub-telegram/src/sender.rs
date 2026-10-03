//! Delivery policy of the Python version: one retry after a flood wait, plain text when
//! Telegram rejects the markup, and best effort for chat messages so a lost message never
//! aborts the agent.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hub_core::domain::{ChatId, MessageId, TopicKey};
use hub_core::escape::plain;
use hub_core::markdown::markdown_to_html_chunks;
use hub_core::questions::Button;
use hub_core::render::{TELEGRAM_TEXT_LIMIT, split_message, truncate};

use crate::messenger::{Format, Messenger, Outgoing, SendError, Target};

// Telegram's flood waits can be long; the agent should not stall behind one for minutes.
const MAX_RETRY_WAIT: Duration = Duration::from_mins(1);
const CAPTION_LIMIT: usize = 1024;

#[derive(Clone)]
pub struct Sender {
    messenger: Arc<dyn Messenger>,
}

impl Sender {
    #[must_use]
    pub fn new(messenger: Arc<dyn Messenger>) -> Self {
        Self { messenger }
    }

    #[must_use]
    pub fn messenger(&self) -> &dyn Messenger {
        self.messenger.as_ref()
    }

    pub async fn text(&self, target: Target, text: &str) {
        for chunk in split_message(text, TELEGRAM_TEXT_LIMIT) {
            self.one(target, Outgoing::plain(chunk)).await;
        }
    }

    pub async fn markdown(&self, key: TopicKey, markdown: &str) {
        for chunk in markdown_to_html_chunks(markdown, TELEGRAM_TEXT_LIMIT) {
            self.one(Target::Topic(key), Outgoing::html(chunk)).await;
        }
    }

    /// The sent message, or `None` when it was given up (and logged).
    pub async fn one(&self, target: Target, message: Outgoing) -> Option<MessageId> {
        let mut message = message;
        let mut waited = false;
        loop {
            match self.messenger.send(target, message.clone()).await {
                Ok(id) => return Some(id),
                Err(SendError::RetryAfter(wait)) if !waited => {
                    waited = true;
                    tokio::time::sleep(wait.min(MAX_RETRY_WAIT)).await;
                }
                Err(SendError::Rejected(reason)) if message.format == Format::Html => {
                    // A formatting bug must not lose the message: resend it as plain text.
                    tracing::warn!(%reason, "telegram rejected formatting, sending plain text");
                    message =
                        Outgoing { text: plain(&message.text), format: Format::Plain, ..message };
                }
                Err(error) => {
                    tracing::warn!(%error, "telegram message given up");
                    return None;
                }
            }
        }
    }

    pub async fn edit(&self, chat: ChatId, message: MessageId, text: String, format: Format) {
        if let Err(error) = self.messenger.edit(chat, message, text, format).await {
            tracing::warn!(%error, "telegram edit failed");
        }
    }

    pub async fn edit_keyboard(
        &self,
        chat: ChatId,
        message: MessageId,
        keyboard: Vec<Vec<Button>>,
    ) {
        if let Err(error) = self.messenger.edit_keyboard(chat, message, keyboard).await {
            tracing::warn!(%error, "telegram keyboard edit failed");
        }
    }

    pub async fn answer(&self, callback: String, text: Option<&str>) {
        if let Err(error) = self.messenger.answer(callback, text.map(str::to_owned)).await {
            tracing::warn!(%error, "telegram callback answer failed");
        }
    }

    /// Unlike text, a failed file delivery is returned: the agent must learn about it.
    pub async fn document(
        &self,
        key: TopicKey,
        path: PathBuf,
        caption: &str,
    ) -> Result<(), SendError> {
        let caption = truncate(caption, CAPTION_LIMIT);
        match self.messenger.document(key, path.clone(), caption.clone()).await {
            Err(SendError::RetryAfter(wait)) => {
                tokio::time::sleep(wait.min(MAX_RETRY_WAIT)).await;
                self.messenger.document(key, path, caption).await
            }
            other => other,
        }
    }

    pub async fn typing(&self, key: TopicKey) {
        if let Err(error) = self.messenger.typing(key).await {
            tracing::warn!(%error, "telegram chat action failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use hub_core::domain::{ChatId, ThreadId};

    use super::*;
    use crate::testing::{Call, FakeMessenger};

    const KEY: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };

    fn setup(messenger: FakeMessenger) -> (Sender, Arc<FakeMessenger>) {
        let messenger = Arc::new(messenger);
        (Sender::new(Arc::clone(&messenger) as Arc<dyn Messenger>), messenger)
    }

    #[tokio::test]
    async fn long_text_is_split() {
        let (sender, messenger) = setup(FakeMessenger::default());
        sender
            .text(Target::Topic(KEY), &format!("{}\n{}", "a".repeat(4000), "b".repeat(200)))
            .await;
        assert_eq!(messenger.sent_texts().len(), 2);
    }

    #[tokio::test]
    async fn markdown_goes_out_as_html_chunks() {
        let (sender, messenger) = setup(FakeMessenger::default());
        sender.markdown(KEY, "**жирный**").await;
        assert_eq!(
            messenger.calls(),
            [Call::Send {
                target: Target::Topic(KEY),
                message: Outgoing::html("<b>жирный</b>".to_owned()),
                id: MessageId(1)
            }]
        );
    }

    #[tokio::test]
    async fn rejected_html_is_resent_as_plain_text() {
        let (sender, messenger) = setup(FakeMessenger::with_sends(vec![Err(SendError::Rejected(
            "can't parse entities".to_owned(),
        ))]));
        let sent =
            sender.one(Target::Topic(KEY), Outgoing::html("<b>a &amp; b</b>".to_owned())).await;
        assert!(sent.is_some());
        assert_eq!(messenger.sent_texts(), ["a & b"]);
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_is_honoured_once() {
        let wait = || Err(SendError::RetryAfter(Duration::from_secs(3)));
        let (sender, messenger) = setup(FakeMessenger::with_sends(vec![wait()]));
        assert!(sender.one(Target::Topic(KEY), Outgoing::plain("x".to_owned())).await.is_some());
        assert_eq!(messenger.sent_texts(), ["x"]);

        let (sender, messenger) = setup(FakeMessenger::with_sends(vec![wait(), wait()]));
        assert!(sender.one(Target::Topic(KEY), Outgoing::plain("x".to_owned())).await.is_none());
        assert!(messenger.sent_texts().is_empty());
    }

    #[tokio::test]
    async fn plain_text_failure_is_given_up() {
        let (sender, messenger) =
            setup(FakeMessenger::with_sends(vec![Err(SendError::Rejected("bad".to_owned()))]));
        assert!(sender.one(Target::Topic(KEY), Outgoing::plain("x".to_owned())).await.is_none());
        assert!(messenger.sent_texts().is_empty());
    }

    #[tokio::test]
    async fn document_caption_is_truncated() {
        let (sender, messenger) = setup(FakeMessenger::default());
        sender.document(KEY, PathBuf::from("r.pdf"), &"я".repeat(2000)).await.unwrap();
        let Some(Call::Document { caption, .. }) = messenger.calls().pop() else {
            panic!("no document")
        };
        assert_eq!(caption.chars().count(), 1024);
    }
}
