//! A bounded, serial disk worker kept across bot restarts.

use futures::future::BoxFuture;
use hub_core::{
    domain::TopicKey,
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
    Save(SavedResult),
    Load(TopicKey),
}
struct Request {
    operation: Operation,
    reply: oneshot::Sender<Result<Option<SavedResult>, String>>,
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
    fn start(
        mut handle: impl FnMut(Operation) -> Result<Option<SavedResult>, String> + Send + 'static,
    ) -> Self {
        let (queue, receiver) = mpsc::sync_channel::<Request>(QUEUE);
        let worker = std::thread::Builder::new().name("last-results".to_owned()).spawn(move || {
            for request in receiver {
                if request.reply.is_closed() {
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
    fn request(
        &self,
        operation: Operation,
    ) -> BoxFuture<'static, Result<Option<SavedResult>, String>> {
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
    fn save(&self, result: SavedResult) -> BoxFuture<'static, Result<(), String>> {
        let request = self.request(Operation::Save(result));
        Box::pin(async move { request.await.map(|_| ()) })
    }
    fn load(&self, key: TopicKey) -> BoxFuture<'static, Result<Option<SavedResult>, String>> {
        self.request(Operation::Load(key))
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
fn operate(path: &Path, operation: Operation) -> Result<Option<SavedResult>, String> {
    let time = now();
    let mut book = read(path, time)?;
    match operation {
        Operation::Load(key) => Ok(book.get(key).cloned()),
        Operation::Save(record) => {
            book.save(record, time);
            let raw = book.dump()?;
            crate::atomic::write_atomic(path, raw.as_bytes())
                .map_err(|error| format!("Не удалось сохранить результат: {error}"))?;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::domain::{AbsolutePath, BackendKind, ChatId, SessionId, ThreadId, TopicSession};
    fn key() -> TopicKey {
        TopicKey { chat: ChatId(-100), thread: ThreadId(7) }
    }
    fn result(text: &str) -> SavedResult {
        SavedResult::new(
            key(),
            "a".repeat(32),
            now(),
            &TopicSession::fresh(
                BackendKind::Codex,
                AbsolutePath::new(std::env::temp_dir()).unwrap(),
            )
            .with_session(SessionId::parse("session")),
            text.to_owned(),
        )
        .unwrap()
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
            Ok(None)
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
