//! A recording `Messenger` for tests.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use futures::future::BoxFuture;
use hub_core::domain::{ChatId, MessageId, ThreadId, TopicKey};
use hub_core::questions::Button;
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use crate::messenger::{Format, Messenger, Outgoing, SendError, Target};

#[derive(Default)]
pub(crate) struct FakeActionRunner;

impl crate::actions::ActionRunner for FakeActionRunner {
    fn run(
        &self,
        _action: hub_core::actions::Action,
        cancel: tokio_util::sync::CancellationToken,
    ) -> BoxFuture<'static, String> {
        Box::pin(async move {
            cancel.cancelled().await;
            "⏹ Остановлено".to_owned()
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Call {
    CreateTopic { chat: ChatId, name: String, thread: ThreadId },
    Send { target: Target, message: Outgoing, id: MessageId },
    Edit { chat: ChatId, message: MessageId, text: String, format: Format },
    EditCard { chat: ChatId, message: MessageId, card: Outgoing },
    EditKeyboard { chat: ChatId, message: MessageId, keyboard: Vec<Vec<Button>> },
    Answer { callback: String, text: Option<String> },
    Document { key: TopicKey, path: PathBuf, caption: String },
    Typing(TopicKey),
}

#[derive(Default)]
pub(crate) struct FakeMessenger {
    calls: Mutex<Vec<Call>>,
    send_attempts: Mutex<Vec<Outgoing>>,
    card_attempts: Mutex<Vec<Outgoing>>,
    topic_attempts: Mutex<Vec<(ChatId, String)>>,
    topics: Mutex<VecDeque<Result<ThreadId, SendError>>>,
    sends: Mutex<VecDeque<Result<(), SendError>>>,
    edits: Mutex<VecDeque<Result<(), SendError>>>,
    edit_delay: Mutex<Option<oneshot::Receiver<()>>>,
    send_delay: Mutex<Option<oneshot::Receiver<()>>>,
    topic_delay: Mutex<Option<oneshot::Receiver<()>>>,
    pub(crate) files: HashMap<String, Vec<u8>>,
    next: AtomicI32,
    next_topic: AtomicI32,
}

impl FakeMessenger {
    /// Queue a result for the next creation; unscripted topics get unique IDs from 10000.
    pub fn topic_result(&self, result: Result<ThreadId, SendError>) {
        self.topics.lock().unwrap().push_back(result);
    }

    /// Hold the next creation, while still recording its attempt immediately.
    pub fn hold_topic(&self, release: oneshot::Receiver<()>) {
        *self.topic_delay.lock().unwrap() = Some(release);
    }

    pub fn topic_attempts(&self) -> Vec<(ChatId, String)> {
        self.topic_attempts.lock().unwrap().clone()
    }

    /// Outcomes of the next sends, in order; afterwards every send succeeds.
    pub(crate) fn with_sends(sends: Vec<Result<(), SendError>>) -> Self {
        Self { sends: Mutex::new(sends.into()), ..Self::default() }
    }

    /// Outcomes of the next edits, in order; afterwards every edit succeeds. A failed edit
    /// is not recorded.
    pub(crate) fn with_edits(edits: Vec<Result<(), SendError>>) -> Self {
        Self { edits: Mutex::new(edits.into()), ..Self::default() }
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub(crate) fn send_attempts(&self) -> Vec<Outgoing> {
        self.send_attempts.lock().unwrap().clone()
    }

    pub(crate) fn card_attempts(&self) -> Vec<Outgoing> {
        self.card_attempts.lock().unwrap().clone()
    }

    /// Hold the next edit until the test explicitly releases it.
    pub(crate) fn delay_next_edit(&self) -> oneshot::Sender<()> {
        let (release, wait) = oneshot::channel();
        *self.edit_delay.lock().unwrap() = Some(wait);
        release
    }

    pub fn delay_next_send(&self) -> oneshot::Sender<()> {
        let (release, wait) = oneshot::channel();
        *self.send_delay.lock().unwrap() = Some(wait);
        release
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
    fn create_topic(
        &self,
        chat: ChatId,
        name: String,
    ) -> BoxFuture<'_, Result<ThreadId, SendError>> {
        self.topic_attempts.lock().unwrap().push((chat, name.clone()));
        let scripted = self.topics.lock().unwrap().pop_front();
        let delay = self.topic_delay.lock().unwrap().take();
        Box::pin(async move {
            if let Some(delay) = delay {
                let _ = delay.await;
            }
            let thread = scripted.unwrap_or_else(|| {
                Ok(ThreadId(self.next_topic.fetch_add(1, Ordering::SeqCst) + 10_000))
            })?;
            self.record(Call::CreateTopic { chat, name, thread });
            Ok(thread)
        })
    }

    fn edit_card(
        &self,
        chat: ChatId,
        message: MessageId,
        card: Outgoing,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        self.card_attempts.lock().unwrap().push(card.clone());
        let scripted = self.edits.lock().unwrap().pop_front().unwrap_or(Ok(()));
        let delay = self.edit_delay.lock().unwrap().take();
        Box::pin(async move {
            if let Some(delay) = delay {
                let _ = delay.await;
            }
            scripted.map(|()| self.record(Call::EditCard { chat, message, card }))
        })
    }
    fn send(
        &self,
        target: Target,
        message: Outgoing,
    ) -> BoxFuture<'_, Result<MessageId, SendError>> {
        self.send_attempts.lock().unwrap().push(message.clone());
        let scripted = self.sends.lock().unwrap().pop_front().unwrap_or(Ok(()));
        let delay = self.send_delay.lock().unwrap().take();
        Box::pin(async move {
            if let Some(delay) = delay {
                let _ = delay.await;
            }
            scripted.map(|()| {
                let id = MessageId(self.next.fetch_add(1, Ordering::SeqCst) + 1);
                self.record(Call::Send { target, message, id });
                id
            })
        })
    }

    fn edit(
        &self,
        chat: ChatId,
        message: MessageId,
        text: String,
        format: Format,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        let scripted = self.edits.lock().unwrap().pop_front().unwrap_or(Ok(()));
        let delay = self.edit_delay.lock().unwrap().take();
        Box::pin(async move {
            if let Some(delay) = delay {
                let _ = delay.await;
            }
            scripted.map(|()| self.record(Call::Edit { chat, message, text, format }))
        })
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

    fn fetch_limited(
        &self,
        file: String,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<u8>, SendError>> {
        let found = self
            .files
            .get(&file)
            .ok_or_else(|| SendError::Failed("Не удалось получить файл".to_owned()))
            .and_then(|bytes| {
                if bytes.len() > limit {
                    Err(SendError::Rejected("Файл превышает допустимый размер".to_owned()))
                } else {
                    Ok(bytes.clone())
                }
            });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::exact(3, Ok(vec![1, 2, 3]))]
    #[case::overflow(2, Err(SendError::Rejected("Файл превышает допустимый размер".to_owned())))]
    #[case::zero(0, Err(SendError::Rejected("Файл превышает допустимый размер".to_owned())))]
    #[tokio::test]
    async fn limited_fetch_checks_before_cloning_and_has_no_message_effects(
        #[case] limit: usize,
        #[case] expected: Result<Vec<u8>, SendError>,
    ) {
        let mut messenger = FakeMessenger::default();
        messenger.files.insert("voice".to_owned(), vec![1, 2, 3]);
        assert_eq!(messenger.fetch_limited("voice".to_owned(), limit).await, expected);
        assert!(messenger.calls().is_empty());
    }

    #[tokio::test]
    async fn limited_fetch_missing_file_is_safe_and_has_no_message_effects() {
        let messenger = FakeMessenger::default();
        assert_eq!(
            messenger.fetch_limited("https://invalid/secret".to_owned(), 3).await,
            Err(SendError::Failed("Не удалось получить файл".to_owned()))
        );
        assert!(messenger.calls().is_empty());
    }
}

pub struct FakeDoctorRunner;
impl crate::doctor::DoctorRunner for FakeDoctorRunner {
    fn run(
        &self,
        _project: hub_core::projects::Project,
        _settings: std::sync::Arc<hub_core::settings::Settings>,
    ) -> futures::future::BoxFuture<'static, Result<hub_core::doctor::Report, String>> {
        Box::pin(async {
            Ok(hub_core::doctor::Report {
                directory: Ok("test directory".to_owned()),
                cli: Ok("test cli".to_owned()),
                resources: vec![],
            })
        })
    }
}

#[derive(Default)]
pub struct MemoryResults {
    pub records: std::sync::Mutex<
        std::collections::HashMap<hub_core::domain::TopicKey, hub_core::results::SavedResult>,
    >,
    pub fail: std::sync::atomic::AtomicBool,
    pub journal: Mutex<hub_core::results::Results>,
    pub time: std::sync::atomic::AtomicI64,
}
impl MemoryResults {
    pub fn now(&self) -> i64 {
        let time = self.time.load(std::sync::atomic::Ordering::SeqCst);
        if time == 0 { chrono::Utc::now().timestamp() } else { time }
    }
}
impl crate::results::ResultStore for MemoryResults {
    fn save(
        &self,
        pending: hub_core::outbox::Pending,
    ) -> BoxFuture<'static, Result<hub_core::outbox::SaveOutcome, String>> {
        let result = if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            Err("диск полон".to_owned())
        } else {
            self.records.lock().unwrap().insert(pending.result().key(), pending.result().clone());
            self.journal.lock().unwrap().enqueue(pending, self.now())
        };
        Box::pin(std::future::ready(result))
    }
    fn load(
        &self,
        key: hub_core::domain::TopicKey,
    ) -> BoxFuture<'static, Result<Option<hub_core::results::SavedResult>, String>> {
        let record = self.records.lock().unwrap().get(&key).cloned();
        Box::pin(std::future::ready(Ok(record)))
    }
    fn pending(
        &self,
        chat: hub_core::domain::ChatId,
    ) -> BoxFuture<'static, Result<hub_core::outbox::Snapshot, String>> {
        let result = if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            Err("диск полон".to_owned())
        } else {
            Ok(self.journal.lock().unwrap().snapshot(chat))
        };
        Box::pin(std::future::ready(result))
    }
    fn update(
        &self,
        change: hub_core::outbox::Change,
    ) -> BoxFuture<'static, Result<Option<hub_core::outbox::Pending>, String>> {
        let result = if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
            Err("диск полон".to_owned())
        } else {
            self.journal.lock().unwrap().update(&change, self.now())
        };
        Box::pin(std::future::ready(result))
    }
}
