//! Persistence port for the last successful textual result of a topic.

use futures::future::BoxFuture;
use hub_core::{
    domain::{ChatId, TopicKey},
    outbox::{Change, Pending, SaveOutcome, Snapshot},
    results::SavedResult,
};

pub trait ResultStore: Send + Sync + 'static {
    /// Enqueues synchronously, preserving the hub's save order before the future is polled.
    fn save(&self, pending: Pending) -> BoxFuture<'static, Result<SaveOutcome, String>>;
    fn pending(&self, chat: ChatId) -> BoxFuture<'static, Result<Snapshot, String>>;
    /// Enqueues synchronously. Network checkpoints (Ack/Fail/Plain) finish even if the caller is cancelled.
    fn update(&self, change: Change) -> BoxFuture<'static, Result<Option<Pending>, String>>;
    fn load(&self, key: TopicKey) -> BoxFuture<'static, Result<Option<SavedResult>, String>>;
}

use crate::{
    hub::HubMessage,
    messenger::{Outgoing, Target},
    sender::Sender,
    telegram::allowed,
};
use hub_core::{
    domain::UserId, markdown::markdown_to_html_chunks, render::TELEGRAM_TEXT_LIMIT,
    settings::Settings,
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

pub struct SaveResult {
    pub(crate) key: TopicKey,
    pub(crate) generation: u64,
    pub(crate) pending: Pending,
    pub(crate) reply: oneshot::Sender<Result<SaveOutcome, String>>,
}

pub struct LastResults {
    pub store: Arc<dyn ResultStore>,
    readers: HashMap<TopicKey, JoinHandle<()>>,
}
impl LastResults {
    #[must_use]
    pub fn new(store: Arc<dyn ResultStore>) -> Self {
        Self { store, readers: HashMap::new() }
    }
    pub fn read(
        &mut self,
        key: TopicKey,
        user: Option<UserId>,
        settings: watch::Receiver<Arc<Settings>>,
        sender: Sender,
        mailbox: mpsc::Sender<HubMessage>,
    ) {
        if !allowed(&settings.borrow().telegram, Some(key.chat), user) {
            return;
        }
        if self.readers.contains_key(&key) || self.readers.len() >= 4 {
            return;
        }
        let loaded = self.store.load(key);
        let task = tokio::spawn(async move {
            let delivery = async {
                let result = loaded.await;
                if !allowed(&settings.borrow().telegram, Some(key.chat), user) {
                    return;
                }
                match result {
                    Ok(Some(record)) => {
                        let time = chrono::DateTime::from_timestamp(record.completed_at(), 0).map_or_else(|| "неизвестно".to_owned(), |time| time.format("%Y-%m-%d %H:%M:%S UTC").to_string());
                        let header = format!("📄 Последний сохранённый ответ\n{time} · {}\nКаталог: {}\nХод: {}", record.backend(), record.cwd(), record.run());
                        // The metadata may exceed Telegram's limit for a very long path.
                        for chunk in hub_core::render::split_message(&header, TELEGRAM_TEXT_LIMIT) {
                            if !allowed(&settings.borrow().telegram, Some(key.chat), user) { return; }
                            if sender.one(Target::Topic(key), Outgoing::plain(chunk)).await.is_none() { return; }
                        }
                        for chunk in markdown_to_html_chunks(record.text(), TELEGRAM_TEXT_LIMIT) {
                            if !allowed(&settings.borrow().telegram, Some(key.chat), user) { return; }
                            if sender.one(Target::Topic(key), Outgoing::html(chunk)).await.is_none() { return; }
                        }
                    }
                    Ok(None) => sender.text(Target::Topic(key), "Сохранённого ответа в этой теме нет. Хранятся текстовые ответы успешных ходов за последние 30 дней.").await,
                    Err(error) => sender.text(Target::Topic(key), &format!("⚠️ Не удалось получить последний ответ: {error}")).await,
                }
            };
            if tokio::time::timeout(Duration::from_secs(65), delivery).await.is_err() {
                tracing::warn!("last result delivery timed out");
            }
            let _ = mailbox.send(HubMessage::LastDone(key)).await;
        });
        self.readers.insert(key, task);
    }
    pub fn complete(&mut self, key: TopicKey) {
        self.readers.remove(&key);
    }
    pub fn shutdown(&mut self) {
        for (_, task) in self.readers.drain() {
            task.abort();
        }
    }
}
impl Drop for LastResults {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeMessenger;
    use hub_core::{
        domain::{AbsolutePath, BackendKind, ChatId, SessionId, ThreadId, TopicSession},
        settings::Draft,
    };
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    #[derive(Default)]
    struct Controlled {
        loads: AtomicUsize,
        releases: Mutex<Vec<oneshot::Sender<()>>>,
    }
    fn key(thread: i32) -> TopicKey {
        TopicKey { chat: ChatId(-100), thread: ThreadId(thread) }
    }
    fn settings() -> Settings {
        let root = std::env::temp_dir();
        Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            ..Draft::default()
        }
        .parse(&root)
        .unwrap()
    }
    impl ResultStore for Controlled {
        fn save(&self, _pending: Pending) -> BoxFuture<'static, Result<SaveOutcome, String>> {
            Box::pin(async { Ok(SaveOutcome::Queued) })
        }
        fn pending(&self, _chat: ChatId) -> BoxFuture<'static, Result<Snapshot, String>> {
            Box::pin(async { Ok(Snapshot::default()) })
        }
        fn update(&self, _change: Change) -> BoxFuture<'static, Result<Option<Pending>, String>> {
            Box::pin(async { Err("unused".to_owned()) })
        }
        fn load(&self, key: TopicKey) -> BoxFuture<'static, Result<Option<SavedResult>, String>> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            let (release, wait) = oneshot::channel();
            self.releases.lock().unwrap().push(release);
            Box::pin(async move {
                let _ = wait.await;
                Ok(Some(
                    SavedResult::new(
                        key,
                        "a".repeat(32),
                        1,
                        &TopicSession::fresh(
                            BackendKind::Codex,
                            AbsolutePath::new(std::env::temp_dir()).unwrap(),
                        )
                        .with_session(SessionId::parse("s")),
                        "private result".to_owned(),
                    )
                    .unwrap(),
                ))
            })
        }
    }
    #[tokio::test]
    async fn last_reader_rechecks_authorization_after_loading_and_shutdown_cancels_loads() {
        let store = Arc::new(Controlled::default());
        let mut book = LastResults::new(store.clone());
        let messenger = Arc::new(FakeMessenger::default());
        let sender = Sender::new(messenger.clone());
        let (settings, watched) = watch::channel(Arc::new(settings()));
        let (mailbox, mut inbox) = mpsc::channel(16);
        book.read(key(1), Some(UserId(1)), watched.clone(), sender.clone(), mailbox.clone());
        let mut draft = Draft::from_settings(&settings.borrow());
        draft.users = "2".to_owned();
        settings.send(Arc::new(draft.parse(&std::env::temp_dir()).unwrap())).unwrap();
        store.releases.lock().unwrap().pop().unwrap().send(()).unwrap();
        let HubMessage::LastDone(key) = inbox.recv().await.unwrap() else { panic!("last done") };
        book.complete(key);
        assert!(messenger.sent_texts().is_empty());
        book.read(key, Some(UserId(2)), watched, sender, mailbox);
        book.shutdown();
        tokio::task::yield_now().await;
        assert!(store.releases.lock().unwrap().pop().unwrap().is_closed());
    }
    #[tokio::test]
    async fn last_read_limits_include_delivery_and_prevent_duplicates() {
        let store = Arc::new(Controlled::default());
        let mut book = LastResults::new(store.clone());
        let messenger = Arc::new(FakeMessenger::default());
        let sender = Sender::new(messenger.clone());
        let release_send = messenger.delay_next_send();
        let settings = watch::Sender::new(Arc::new(settings()));
        let (mailbox, mut inbox) = mpsc::channel(16);
        book.read(key(1), Some(UserId(1)), settings.subscribe(), sender.clone(), mailbox.clone());
        store.releases.lock().unwrap().pop().unwrap().send(()).unwrap();
        tokio::task::yield_now().await;
        assert!(!messenger.send_attempts().is_empty());
        for thread in [1, 1, 2, 3, 4, 5] {
            book.read(
                key(thread),
                Some(UserId(1)),
                settings.subscribe(),
                sender.clone(),
                mailbox.clone(),
            );
        }
        assert_eq!(store.loads.load(Ordering::SeqCst), 4);
        assert_eq!(book.readers.len(), 4);
        release_send.send(()).unwrap();
        let HubMessage::LastDone(key) = inbox.recv().await.unwrap() else { panic!("last done") };
        book.complete(key);
        assert_eq!(book.readers.len(), 3);
        book.shutdown();
    }
}
