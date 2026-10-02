//! A recording `Messenger` for tests.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use futures::future::BoxFuture;
use hub_core::domain::{ChatId, MessageId, TopicKey};
use hub_core::questions::Button;
use tokio::io::AsyncWriteExt;

use crate::messenger::{Format, Messenger, Outgoing, SendError, Target};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    Send { target: Target, message: Outgoing, id: MessageId },
    Edit { chat: ChatId, message: MessageId, text: String, format: Format },
    EditKeyboard { chat: ChatId, message: MessageId, keyboard: Vec<Vec<Button>> },
    Answer { callback: String, text: Option<String> },
    Document { key: TopicKey, path: PathBuf, caption: String },
    Typing(TopicKey),
}

#[derive(Default)]
pub(crate) struct FakeMessenger {
    calls: Mutex<Vec<Call>>,
    sends: Mutex<VecDeque<Result<(), SendError>>>,
    pub(crate) files: HashMap<String, Vec<u8>>,
    next: AtomicI32,
}

impl FakeMessenger {
    /// Outcomes of the next sends, in order; afterwards every send succeeds.
    pub(crate) fn with_sends(sends: Vec<Result<(), SendError>>) -> Self {
        Self { sends: Mutex::new(sends.into()), ..Self::default() }
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    /// Texts of delivered messages, in order.
    pub(crate) fn sent_texts(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Send { message, .. } => Some(message.text),
                _ => None,
            })
            .collect()
    }

    /// The last delivered message that carried a keyboard.
    pub(crate) fn last_keyboard(&self) -> Option<(MessageId, Vec<Vec<Button>>)> {
        self.calls().into_iter().rev().find_map(|call| match call {
            Call::Send { message, id, .. } if !message.keyboard.is_empty() => {
                Some((id, message.keyboard))
            }
            _ => None,
        })
    }

    fn record(&self, call: Call) {
        self.calls.lock().unwrap().push(call);
    }
}

impl Messenger for FakeMessenger {
    fn send(
        &self,
        target: Target,
        message: Outgoing,
    ) -> BoxFuture<'_, Result<MessageId, SendError>> {
        let scripted = self.sends.lock().unwrap().pop_front().unwrap_or(Ok(()));
        let result = scripted.map(|()| {
            let id = MessageId(self.next.fetch_add(1, Ordering::SeqCst) + 1);
            self.record(Call::Send { target, message, id });
            id
        });
        Box::pin(async move { result })
    }

    fn edit(
        &self,
        chat: ChatId,
        message: MessageId,
        text: String,
        format: Format,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        self.record(Call::Edit { chat, message, text, format });
        Box::pin(async { Ok(()) })
    }

    fn edit_keyboard(
        &self,
        chat: ChatId,
        message: MessageId,
        keyboard: Vec<Vec<Button>>,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        self.record(Call::EditKeyboard { chat, message, keyboard });
        Box::pin(async { Ok(()) })
    }

    fn answer(
        &self,
        callback: String,
        text: Option<String>,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        self.record(Call::Answer { callback, text });
        Box::pin(async { Ok(()) })
    }

    fn document(
        &self,
        key: TopicKey,
        path: PathBuf,
        caption: String,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        self.record(Call::Document { key, path, caption });
        Box::pin(async { Ok(()) })
    }

    fn typing(&self, key: TopicKey) -> BoxFuture<'_, Result<(), SendError>> {
        self.record(Call::Typing(key));
        Box::pin(async { Ok(()) })
    }

    fn fetch(&self, file: String) -> BoxFuture<'_, Result<Vec<u8>, SendError>> {
        let found =
            self.files.get(&file).cloned().ok_or(SendError::Failed(format!("no file {file}")));
        Box::pin(async move { found })
    }

    fn save(
        &self,
        file: String,
        mut target: tokio::fs::File,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        let found = self.files.get(&file).cloned();
        Box::pin(async move {
            let bytes = found.ok_or(SendError::Failed(format!("no file {file}")))?;
            target.write_all(&bytes).await.map_err(|error| SendError::Failed(error.to_string()))?;
            target.flush().await.map_err(|error| SendError::Failed(error.to_string()))
        })
    }
}
