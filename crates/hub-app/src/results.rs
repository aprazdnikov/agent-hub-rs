//! A bounded, serial disk worker kept across bot restarts.

use futures::future::BoxFuture;
use hub_core::{
    domain::{ChatId, TopicKey},
    outbox::{Action, Change, Failure, Pending, SaveOutcome, Snapshot},
    results::{MAX_FILE_BYTES, Results, SavedResult},
};
use hub_telegram::results::ResultStore;
use std::{
    fs,
    io::{self, Read},
    path::Path,
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::oneshot;

const WAIT: Duration = Duration::from_secs(5);
const QUEUE: usize = 16;
enum Operation {
    Save(Box<Pending>),
    Load(TopicKey),
    Pending(ChatId),
    Count(ChatId),
    Update(Change),
}
enum Response {
    Saved(SaveOutcome),
    Last(Option<SavedResult>),
    Pending(Snapshot),
    Count(usize),
    Updated(Option<Pending>),
}
struct Request {
    operation: Operation,
    reply: oneshot::Sender<Result<Response, String>>,
}

pub struct FileResults {
    queue: Result<mpsc::SyncSender<Request>, String>,
}
impl FileResults {
    #[must_use]
    pub fn new(topics: &Path) -> Self {
        let path = topics.with_extension("results.json");
        Self::start(move |operation| operate(&path, operation))
    }
    /// Read only the number of queued results for one group through the same serial worker.
    #[must_use]
    pub fn count(&self, chat: ChatId) -> BoxFuture<'static, Result<usize, String>> {
        let request = self.request(Operation::Count(chat));
        Box::pin(async move {
            match request.await? {
                Response::Count(count) => Ok(count),
                _ => Err("Некорректный ответ хранилища".to_owned()),
            }
        })
    }
    fn start(
        mut handle: impl FnMut(Operation) -> Result<Response, String> + Send + 'static,
    ) -> Self {
        let (queue, receiver) = mpsc::sync_channel::<Request>(QUEUE);
        let worker = std::thread::Builder::new().name("last-results".to_owned()).spawn(move || {
            for request in receiver {
                // A known network outcome must survive shutdown/settings cancellation.
                // Cancelled reads, claims and user edits can still be skipped safely.
                let checkpoint = matches!(
                    &request.operation,
                    Operation::Update(Change {
                        action: Action::Ack | Action::Fail(_) | Action::Plain,
                        ..
                    })
                );
                if request.reply.is_closed() && !checkpoint {
                    continue;
                }
                let result = handle(request.operation);
                let _ = request.reply.send(result);
            }
        });
        Self {
            queue: worker
                .map(|_| queue)
                .map_err(|error| format!("Не удалось запустить хранилище результатов: {error}")),
        }
    }
    fn request(&self, operation: Operation) -> BoxFuture<'static, Result<Response, String>> {
        let (reply, receiver) = oneshot::channel();
        let accepted = self.queue.as_ref().map_err(Clone::clone).and_then(|queue| {
            queue
                .try_send(Request { operation, reply })
                .map_err(|_| "Хранилище результатов занято или недоступно".to_owned())
        });
        Box::pin(async move {
            accepted?;
            tokio::time::timeout(WAIT, receiver).await
                .map_err(|_| "Хранилище результатов не ответило за 5 с; начатая запись может завершиться позже".to_owned())?
                .map_err(|_| "Хранилище результатов остановилось".to_owned())?
        })
    }
}
impl ResultStore for FileResults {
    fn save(&self, pending: Pending) -> BoxFuture<'static, Result<SaveOutcome, String>> {
        let request = self.request(Operation::Save(Box::new(pending)));
        Box::pin(async move {
            match request.await? {
                Response::Saved(outcome) => Ok(outcome),
                _ => Err("Некорректный ответ хранилища".to_owned()),
            }
        })
    }
    fn load(&self, key: TopicKey) -> BoxFuture<'static, Result<Option<SavedResult>, String>> {
        let request = self.request(Operation::Load(key));
        Box::pin(async move {
            match request.await? {
                Response::Last(result) => Ok(result),
                _ => Err("Некорректный ответ хранилища".to_owned()),
            }
        })
    }
    fn pending(&self, chat: ChatId) -> BoxFuture<'static, Result<Snapshot, String>> {
        let request = self.request(Operation::Pending(chat));
        Box::pin(async move {
            match request.await? {
                Response::Pending(entries) => Ok(entries),
                _ => Err("Некорректный ответ хранилища".to_owned()),
            }
        })
    }
    fn update(&self, change: Change) -> BoxFuture<'static, Result<Option<Pending>, String>> {
        let request = self.request(Operation::Update(change));
        Box::pin(async move {
            match request.await? {
                Response::Updated(entry) => Ok(entry),
                _ => Err("Некорректный ответ хранилища".to_owned()),
            }
        })
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|time| i64::try_from(time.as_secs()).ok())
        .unwrap_or(0)
}
fn read(path: &Path, now: i64) -> Result<Results, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err("Файл результатов не является обычным файлом".to_owned());
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Results::default()),
        Err(error) => return Err(format!("Не удалось прочитать результаты: {error}")),
    }
    let mut raw = String::new();
    fs::File::open(path)
        .and_then(|file| file.take((MAX_FILE_BYTES + 1) as u64).read_to_string(&mut raw))
        .map_err(|error| format!("Не удалось прочитать результаты: {error}"))?;
    Results::parse(&raw, now)
}
fn operate(path: &Path, operation: Operation) -> Result<Response, String> {
    let time = now();
    let mut book = read(path, time)?;
    let response = match operation {
        Operation::Load(key) => return Ok(Response::Last(book.get(key).cloned())),
        Operation::Pending(chat) => return Ok(Response::Pending(book.snapshot(chat))),
        Operation::Count(chat) => return Ok(Response::Count(book.snapshot(chat).entries.len())),
        Operation::Save(pending) => Response::Saved(book.enqueue(*pending, time)?),
        Operation::Update(change) => {
            // Wall-clock seconds are truncated. Round the response time up so a persisted
            // RetryAfter cannot expire a fractional second before Telegram's full wait.
            let time = if matches!(change.action, Action::Fail(Failure::RetryAfter(_))) {
                time.saturating_add(1)
            } else {
                time
            };
            Response::Updated(book.update(&change, time)?)
        }
    };
    let raw = book.dump()?;
    crate::atomic::write_atomic(path, raw.as_bytes())
        .map_err(|error| format!("Не удалось сохранить результат: {error}"))?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::domain::{AbsolutePath, BackendKind, ChatId, SessionId, ThreadId, TopicSession};
    fn key() -> TopicKey {
        TopicKey { chat: ChatId(-100), thread: ThreadId(7) }
    }
    fn result(text: &str) -> Pending {
        Pending::new(
            SavedResult::new(
                key(),
                uuid::Uuid::new_v4().simple().to_string(),
                now(),
                &TopicSession::fresh(
                    BackendKind::Codex,
                    AbsolutePath::new(std::env::temp_dir()).unwrap(),
                )
                .with_session(SessionId::parse("session")),
                text.to_owned(),
            )
            .unwrap(),
            "",
        )
        .unwrap()
    }
    #[tokio::test]
    async fn count_reports_only_the_requested_group_and_storage_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        let store = FileResults::new(&path);
        assert_eq!(store.count(key().chat).await.unwrap(), 0);
        store.save(result("private answer")).await.unwrap();
        assert_eq!(store.count(key().chat).await.unwrap(), 1);
        assert_eq!(store.count(ChatId(-200)).await.unwrap(), 0);
        fs::write(path.with_extension("results.json"), "broken").unwrap();
        assert!(store.count(key().chat).await.is_err());
    }
    #[tokio::test]
    async fn last_result_survives_store_restart_and_preserves_markdown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        let store = FileResults::new(&path);
        assert!(store.load(key()).await.unwrap().is_none());
        store.save(result("**answer**")).await.unwrap();
        let restored = FileResults::new(&path).load(key()).await.unwrap().unwrap();
        assert_eq!(restored.text(), "**answer**");
        assert!(
            FileResults::new(&path)
                .load(TopicKey { chat: ChatId(-200), ..key() })
                .await
                .unwrap()
                .is_none()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(path.with_extension("results.json")).unwrap().permissions().mode()
                    & 0o777,
                0o600
            );
        }
    }
    #[tokio::test]
    async fn corrupt_last_results_are_reported_and_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        let file = path.with_extension("results.json");
        fs::write(&file, "broken").unwrap();
        let store = FileResults::new(&path);
        assert!(store.load(key()).await.is_err());
        assert!(store.save(result("new")).await.is_err());
        assert_eq!(fs::read_to_string(file).unwrap(), "broken");
    }
    #[tokio::test]
    async fn last_result_write_failure_is_visible() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file"), "x").unwrap();
        let store = FileResults::new(&dir.path().join("file/topics.json"));
        assert!(store.save(result("answer")).await.is_err());
    }
    #[tokio::test]
    async fn last_result_queue_preserves_call_order_before_futures_are_polled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("results.json");
        let (entered, started) = oneshot::channel();
        let mut entered = Some(entered);
        let (release, wait) = mpsc::channel();
        let store = FileResults::start(move |operation| {
            if let Some(entered) = entered.take() {
                entered.send(()).unwrap();
                wait.recv().unwrap();
            }
            operate(&path, operation)
        });
        let first = store.save(result("first"));
        started.await.unwrap(); // save enqueues before its future is first polled.
        let cancelled = store.save(result("cancelled"));
        drop(cancelled);
        let second = store.save(result("second"));
        release.send(()).unwrap();
        second.await.unwrap();
        first.await.unwrap();
        assert_eq!(store.load(key()).await.unwrap().unwrap().text(), "second");
    }

    #[tokio::test(start_paused = true)]
    async fn last_result_worker_is_bounded_after_timeout_and_skips_cancelled_queue_entries() {
        let (entered, started) = oneshot::channel();
        let mut entered = Some(entered);
        let (release, wait) = mpsc::channel();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let store = FileResults::start(move |_| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(entered) = entered.take() {
                entered.send(()).unwrap();
                wait.recv().unwrap();
            }
            Ok(Response::Last(None))
        });
        let first = store.load(key());
        started.await.unwrap();
        let mut pending = Vec::new();
        for _ in 0..QUEUE {
            pending.push(store.load(key()));
        }
        assert!(store.load(key()).await.unwrap_err().contains("занято"));
        assert!(first.await.unwrap_err().contains("5 с"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        drop(pending);
        release.send(()).unwrap();
    }

    #[tokio::test]
    async fn outbox_migration_and_checkpoints_survive_restarting_the_disk_worker() {
        use hub_core::outbox::{Action, Failure};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        let file = path.with_extension("results.json");
        let old = result("old");
        let v1 = serde_json::json!({"version": 1, "results": [old.result()]});
        fs::write(&file, v1.to_string()).unwrap();
        let store = FileResults::new(&path);
        assert!(store.pending(key().chat).await.unwrap().entries.is_empty());
        assert_eq!(store.load(key()).await.unwrap().unwrap().text(), "old");
        store.save(result(&"answer ".repeat(1500))).await.unwrap();
        let queued = store.pending(key().chat).await.unwrap().entries.remove(0);
        let claimed = store.update(Change::new(&queued, Action::Claim)).await.unwrap().unwrap();
        let ack = store.update(Change::new(&claimed, Action::Ack)).await.unwrap().unwrap();
        let stale = Change::new(&claimed, Action::Ack);
        let restarted = FileResults::new(&path);
        assert!(restarted.update(stale).await.is_err());
        let loaded = restarted.pending(key().chat).await.unwrap().entries.remove(0);
        assert_eq!(loaded, ack);
        assert_eq!(loaded.next(), 1);
        let claimed = restarted.update(Change::new(&loaded, Action::Claim)).await.unwrap().unwrap();
        let received_at = now();
        let blocked = restarted
            .update(Change::new(&claimed, Action::Fail(Failure::RetryAfter(3600))))
            .await
            .unwrap()
            .unwrap();
        restarted.update(Change::new(&blocked, Action::Drop)).await.unwrap();
        let restarted = FileResults::new(&path);
        let snapshot = restarted.pending(key().chat).await.unwrap();
        assert!(snapshot.entries.is_empty());
        assert!(snapshot.not_before >= received_at + 3601);
        assert!(restarted.load(key()).await.unwrap().unwrap().text().starts_with("answer"));
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(file).unwrap()).unwrap();
        assert_eq!(raw["version"], 2);
    }
    #[tokio::test]
    async fn outbox_accepted_checkpoint_finishes_when_its_caller_is_cancelled() {
        use hub_core::outbox::Action;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        let initial = FileResults::new(&path);
        let pending = result(&"answer ".repeat(1000));
        initial.save(pending.clone()).await.unwrap();
        let claimed = initial.update(Change::new(&pending, Action::Claim)).await.unwrap().unwrap();
        let file = path.with_extension("results.json");
        let (entered, started) = oneshot::channel();
        let mut entered = Some(entered);
        let (release, wait) = mpsc::channel();
        let store = FileResults::start(move |operation| {
            if let Some(entered) = entered.take() {
                entered.send(()).unwrap();
                wait.recv().unwrap();
            }
            operate(&file, operation)
        });
        let blocked = store.load(key());
        started.await.unwrap();
        let checkpoint = store.update(Change::new(&claimed, Action::Ack));
        drop(checkpoint); // Hub shutdown after Telegram accepted the part.
        release.send(()).unwrap();
        blocked.await.unwrap();
        assert_eq!(store.pending(key().chat).await.unwrap().entries[0].next(), 1);
    }
    #[tokio::test(start_paused = true)]
    async fn outbox_save_timeout_can_complete_late_with_the_identical_payload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("results.json");
        let (entered, started) = oneshot::channel();
        let mut entered = Some(entered);
        let (release, wait) = mpsc::channel();
        let store = FileResults::start(move |operation| {
            if let Some(entered) = entered.take() {
                entered.send(()).unwrap();
                wait.recv().unwrap();
            }
            operate(&path, operation)
        });
        let pending = result("**finished while disk stalled**");
        let saved = store.save(pending.clone());
        started.await.unwrap();
        assert!(saved.await.unwrap_err().contains("завершиться позже"));
        // Tokio's virtual time must not auto-advance past a real disk-thread response.
        tokio::time::resume();
        release.send(()).unwrap();
        assert_eq!(store.pending(key().chat).await.unwrap().entries, [pending]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn last_result_symlink_is_neither_read_nor_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        let secret = dir.path().join("outside");
        fs::write(&secret, "private").unwrap();
        std::os::unix::fs::symlink(&secret, path.with_extension("results.json")).unwrap();
        let store = FileResults::new(&path);
        assert!(store.load(key()).await.is_err());
        assert!(store.save(result("answer")).await.is_err());
        assert_eq!(fs::read_to_string(secret).unwrap(), "private");
    }
}
