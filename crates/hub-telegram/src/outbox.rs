//! One bounded delivery pump, independent of model sessions.

use crate::{
    inbound::Press,
    messenger::{Outgoing, SendError, Target},
    results::ResultStore,
    sender::Sender,
    telegram::allowed,
};
use hub_core::{
    domain::{ThreadId, TopicKey, UserId},
    outbox::{Action, Change, Failure, Pending, Reason, State},
    questions::Button,
    settings::{Settings, TelegramSettings},
};
use std::{collections::HashMap, fmt::Write, sync::Arc, time::Duration};
use tokio::{
    sync::{Notify, watch},
    task::JoinHandle,
};

pub const PREFIX: &str = "outbox:";
const NETWORK_TIMEOUT: Duration = Duration::from_secs(20);
const PAGE: usize = 5;
#[derive(Debug)]
enum Step {
    Idle,
    Sent(Duration),
}

/// Network cancellation preserves the durable lease. After a response, checkpointing
/// must finish even across settings changes, especially an observed `RetryAfter`.
async fn step(
    store: &dyn ResultStore,
    sender: &Sender,
    settings: &mut watch::Receiver<Arc<Settings>>,
    identity: &TelegramSettings,
    now: i64,
) -> Result<Step, String> {
    if settings.borrow().telegram != *identity {
        return Ok(Step::Idle);
    }
    deliver(store, sender, settings, identity, now).await
}
async fn deliver(
    store: &dyn ResultStore,
    sender: &Sender,
    settings: &mut watch::Receiver<Arc<Settings>>,
    identity: &TelegramSettings,
    now: i64,
) -> Result<Step, String> {
    let chat = identity.chat;
    let snapshot = store.pending(chat).await?;
    if settings.borrow().telegram != *identity {
        return Ok(Step::Idle);
    }
    if snapshot.not_before > now {
        return Ok(Step::Sent(Duration::from_secs(
            u64::try_from(snapshot.not_before.saturating_sub(now)).unwrap_or(u64::MAX),
        )));
    }
    let Some(pending) = snapshot
        .entries
        .into_iter()
        .filter(|entry| entry.ready(chat, now))
        .min_by_key(|entry| (entry.due(), entry.result().completed_at()))
    else {
        return Ok(Step::Idle);
    };
    let Some(mut claimed) = store.update(Change::new(&pending, Action::Claim)).await? else {
        return Ok(Step::Idle);
    };
    if claimed.state() != State::Leased {
        return Ok(Step::Idle);
    }
    let Some(mut response) = send_current(sender, &claimed, settings, identity).await else {
        return Ok(Step::Sent(Duration::from_secs(1)));
    };
    if matches!(response, Err(SendError::Rejected(_)))
        && claimed.chunk().is_some_and(|chunk| chunk.format == hub_core::outbox::Format::Html)
    {
        let Some(plain) = store.update(Change::new(&claimed, Action::Plain)).await? else {
            return Ok(Step::Idle);
        };
        claimed = plain;
        tokio::time::sleep(Duration::from_secs(1)).await;
        let Some(result) = send_current(sender, &claimed, settings, identity).await else {
            return Ok(Step::Sent(Duration::from_secs(1)));
        };
        response = result;
    }
    let mut pause = Duration::from_secs(1);
    let action = match response {
        Ok(()) => Action::Ack,
        Err(SendError::RetryAfter(wait)) => {
            // Telegram may rate-limit the entire bot, so pause this pump as well as this entry.
            pause = pause.max(wait);
            Action::Fail(Failure::RetryAfter(
                wait.as_secs().saturating_add(u64::from(wait.subsec_nanos() > 0)),
            ))
        }
        Err(SendError::Rejected(_)) => Action::Fail(Failure::Permanent),
        Err(SendError::Failed(_)) => Action::Fail(Failure::Transient),
    };
    if let Err(error) = store.update(Change::new(&claimed, action)).await {
        // A known server wait still applies when checkpointing fails. Across a process
        // restart only a successfully persisted cooldown can be recovered.
        tracing::warn!(%error, "outbox checkpoint failed; delivery paused");
        pause = pause.max(Duration::from_secs(10));
    }
    Ok(Step::Sent(pause))
}
async fn send_current(
    sender: &Sender,
    pending: &Pending,
    settings: &mut watch::Receiver<Arc<Settings>>,
    identity: &TelegramSettings,
) -> Option<Result<(), SendError>> {
    if settings.borrow_and_update().telegram != *identity {
        return None;
    }
    tokio::select! {
        biased;
        _ = settings.changed() => None,
        response = send(sender, pending) => Some(response),
    }
}
async fn send(sender: &Sender, pending: &Pending) -> Result<(), SendError> {
    let chunk = pending
        .chunk()
        .ok_or_else(|| SendError::Rejected("Некорректная часть ответа".to_owned()))?;
    let message = match chunk.format {
        hub_core::outbox::Format::Html => Outgoing::html(chunk.text.clone()),
        hub_core::outbox::Format::Plain => Outgoing::plain(chunk.text.clone()),
    };
    tokio::time::timeout(
        NETWORK_TIMEOUT,
        sender.messenger().send(Target::Topic(pending.result().key()), message),
    )
    .await
    .map_err(|_| SendError::Failed("Тайм-аут отправки".to_owned()))?
    .map(|_| ())
}

pub struct Outbox {
    store: Arc<dyn ResultStore>,
    sender: Sender,
    settings: watch::Receiver<Arc<Settings>>,
    identity: TelegramSettings,
    pump: JoinHandle<()>,
    wake: Arc<Notify>,
    controls: HashMap<TopicKey, JoinHandle<()>>,
}
impl Outbox {
    pub fn new(
        store: Arc<dyn ResultStore>,
        sender: Sender,
        settings: watch::Receiver<Arc<Settings>>,
    ) -> Self {
        let identity = settings.borrow().telegram.clone();
        let wake = Arc::new(Notify::new());
        let pump = tokio::spawn(run(
            store.clone(),
            sender.clone(),
            settings.clone(),
            wake.clone(),
            identity.clone(),
        ));
        Self { store, sender, settings, identity, pump, wake, controls: HashMap::new() }
    }
    pub fn wake(&self) -> Arc<Notify> {
        self.wake.clone()
    }
    pub fn menu(&mut self, key: TopicKey, user: Option<UserId>) {
        self.control(Request { key, user, offset: 0, change: None, callback: None });
    }
    pub fn press(&mut self, press: &Press) {
        if let Some(request) = parse(press) {
            self.control(request);
        }
    }
    fn control(&mut self, request: Request) {
        if self.settings.borrow().telegram != self.identity
            || !allowed(&self.settings.borrow().telegram, Some(request.key.chat), request.user)
        {
            return;
        }
        self.controls.retain(|_, task| !task.is_finished());
        if self.controls.len() >= 4 || self.controls.contains_key(&request.key) {
            return;
        }
        let (store, sender, mut settings, wake) =
            (self.store.clone(), self.sender.clone(), self.settings.clone(), self.wake.clone());
        let key = request.key;
        let identity = self.identity.clone();
        settings.borrow_and_update();
        let task = tokio::spawn(async move {
            let authorized = || {
                settings.borrow().telegram == identity
                    && allowed(&settings.borrow().telegram, Some(key.chat), request.user)
            };
            let work = async {
                if !authorized() {
                    return;
                }
                let mut notice = None;
                if let Some(change) = request.change {
                    notice = match store.update(change).await {
                        Ok(_) => {
                            wake.notify_one();
                            None
                        }
                        Err(error) => Some(error),
                    };
                }
                if !authorized() {
                    return;
                }
                if let Some(callback) = request.callback {
                    sender.answer(callback, notice.as_deref()).await;
                }
                let entries = store.pending(key.chat).await;
                if !authorized() {
                    return;
                }
                let message = match entries {
                    Ok(snapshot) => card(
                        key,
                        snapshot.entries,
                        request.offset,
                        notice.as_deref(),
                        snapshot.not_before,
                    ),
                    Err(error) => Outgoing::plain(format!(
                        "⚠️ Не удалось прочитать очередь доставки: {error}"
                    )),
                };
                sender.one(Target::Topic(key), message).await;
            };
            // The watcher used by authorization is separate from the mutable cancellation watcher.
            let mut changes = settings.clone();
            tokio::select! { biased; _ = changes.changed() => {}, _ = tokio::time::timeout(Duration::from_secs(65), work) => {} }
        });
        self.controls.insert(key, task);
    }
    pub fn shutdown(&mut self) {
        self.pump.abort();
        for (_, task) in self.controls.drain() {
            task.abort();
        }
    }
}
impl Drop for Outbox {
    fn drop(&mut self) {
        self.shutdown();
    }
}

async fn run(
    store: Arc<dyn ResultStore>,
    sender: Sender,
    mut settings: watch::Receiver<Arc<Settings>>,
    wake: Arc<Notify>,
    identity: TelegramSettings,
) {
    loop {
        if settings.borrow().telegram != identity {
            return;
        }
        let result =
            step(store.as_ref(), &sender, &mut settings, &identity, chrono::Utc::now().timestamp())
                .await;
        if settings.borrow().telegram != identity {
            return;
        }
        let (delay, idle) = match result {
            Ok(Step::Idle) => (Duration::from_secs(5), true),
            Ok(Step::Sent(delay)) => (delay, false),
            Err(error) => {
                tracing::warn!(%error, "outbox storage unavailable; delivery paused");
                (Duration::from_secs(10), false)
            }
        };
        // Sleep in bounded intervals to avoid Instant overflow for an arbitrary RetryAfter.
        let mut remaining = delay;
        while !remaining.is_zero() {
            let interval = remaining.min(Duration::from_mins(1));
            let started = tokio::time::Instant::now();
            tokio::select! {
                () = tokio::time::sleep(interval) => {},
                () = wake.notified(), if idle => break,
                changed = settings.changed() => {
                    if changed.is_err() || settings.borrow().telegram != identity { return; }
                    if idle { break; }
                }
            }
            remaining = remaining.saturating_sub(started.elapsed());
        }
    }
}
struct Request {
    key: TopicKey,
    user: Option<UserId>,
    offset: usize,
    change: Option<Change>,
    callback: Option<String>,
}
fn parse(press: &Press) -> Option<Request> {
    let mut parts = press.data.strip_prefix(PREFIX)?.split(':');
    let action = parts.next()?;
    let thread = parts.next()?.parse::<i32>().ok().filter(|thread| *thread > 0)?;
    let key = TopicKey { chat: press.chat?, thread: ThreadId(thread) };
    let mut request = Request {
        key,
        user: Some(press.user),
        offset: 0,
        change: None,
        callback: Some(press.callback.clone()),
    };
    if action == "p" {
        request.offset =
            parts.next()?.parse().ok().filter(|offset| *offset <= hub_core::outbox::MAX_PENDING)?;
    } else {
        let run = parts.next()?;
        if run.len() != 32 || !run.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let revision = parts.next()?.parse().ok()?;
        let action = match action {
            "r" => Action::Retry,
            "d" => Action::Drop,
            _ => return None,
        };
        request.change = Some(Change { key, run: run.to_owned(), revision, action });
    }
    parts.next().is_none().then_some(request)
}
fn callback(change: &Change) -> String {
    let verb = match change.action {
        Action::Retry => "r",
        _ => "d",
    };
    format!("{PREFIX}{verb}:{}:{}:{}", change.key.thread.0, change.run, change.revision)
}
fn card(
    key: TopicKey,
    entries: Vec<Pending>,
    offset: usize,
    notice: Option<&str>,
    not_before: i64,
) -> Outgoing {
    let entries: Vec<_> = entries.into_iter().filter(|entry| entry.result().key() == key).collect();
    let offset = offset.min(entries.len().saturating_sub(1) / PAGE * PAGE);
    let mut text = format!("📬 Очередь доставки · {}\n", entries.len());
    if let Some(notice) = notice {
        let _ = writeln!(text, "\n⚠️ {notice}");
    }
    if entries.is_empty() {
        text.push_str("\nОжидающих ответов в этой теме нет.");
    }
    let mut keyboard = Vec::new();
    let now = chrono::Utc::now().timestamp();
    if not_before > now {
        let _ = writeln!(text, "\nTelegram просит подождать до {}.", timestamp(not_before));
    }
    for (index, entry) in entries.iter().skip(offset).take(PAGE).enumerate() {
        let time = timestamp(entry.result().completed_at());
        let state = match entry.state() {
            State::Leased if !entry.editable(now) => "отправляется".to_owned(),
            State::Waiting | State::Leased => {
                format!("ожидает, следующая попытка {}", timestamp(entry.due()))
            }
            State::Blocked(Reason::Exhausted) => "остановлена после 8 неудач".to_owned(),
            State::Blocked(Reason::Rejected) => "Telegram отклонил отправку".to_owned(),
        };
        let number = offset + index + 1;
        let _ = writeln!(
            text,
            "\n{number}. {} · {time}\nЧастей: {}/{} · {state}",
            entry.result().run(),
            entry.next(),
            entry.total()
        );
        let mut row = vec![];
        if matches!(entry.state(), State::Blocked(_)) {
            row.push(Button {
                text: format!("↻ Повторить {number}"),
                data: callback(&Change::new(entry, Action::Retry)),
            });
        }
        if entry.editable(now) {
            row.push(Button {
                text: format!("✕ Удалить {number}"),
                data: callback(&Change::new(entry, Action::Drop)),
            });
        }
        // Empty keyboard rows are not valid in Telegram.
        if !row.is_empty() {
            keyboard.push(row);
        }
    }
    let mut navigation = vec![];
    if offset > 0 {
        navigation.push(Button {
            text: "←".to_owned(),
            data: format!("{PREFIX}p:{}:{}", key.thread.0, offset.saturating_sub(PAGE)),
        });
    }
    navigation.push(Button {
        text: "Обновить".to_owned(),
        data: format!("{PREFIX}p:{}:{offset}", key.thread.0),
    });
    if offset + PAGE < entries.len() {
        navigation.push(Button {
            text: "→".to_owned(),
            data: format!("{PREFIX}p:{}:{}", key.thread.0, offset + PAGE),
        });
    }
    keyboard.push(navigation);
    text.push_str("\nХранение: 7 дней. /last — последний ответ. Повтор возможен при потере подтверждения; сверяйте ID хода и номер части.");
    Outgoing::plain(text).with_keyboard(keyboard)
}
fn timestamp(time: i64) -> String {
    chrono::DateTime::from_timestamp(time, 0)
        .map_or_else(|| "позже".to_owned(), |time| time.format("%d.%m %H:%M UTC").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeMessenger, MemoryResults};
    use hub_core::{
        domain::{AbsolutePath, BackendKind, ChatId, MessageId, SessionId, ThreadId, TopicSession},
        results::SavedResult,
        settings::Draft,
    };
    use std::sync::atomic::Ordering;

    async fn once(
        store: &dyn ResultStore,
        sender: &Sender,
        settings: &mut watch::Receiver<Arc<Settings>>,
        now: i64,
    ) -> Result<Step, String> {
        let identity = settings.borrow().telegram.clone();
        super::step(store, sender, settings, &identity, now).await
    }
    fn key() -> TopicKey {
        TopicKey { chat: ChatId(-100), thread: ThreadId(7) }
    }
    fn settings() -> watch::Sender<Arc<Settings>> {
        let root = std::env::temp_dir();
        watch::Sender::new(Arc::new(
            Draft {
                token: "1:a".to_owned(),
                chat: "-100".to_owned(),
                users: "1".to_owned(),
                workspace_root: root.display().to_string(),
                ..Draft::default()
            }
            .parse(&root)
            .unwrap(),
        ))
    }
    fn pending(text: &str) -> Pending {
        Pending::new(
            SavedResult::new(
                key(),
                "a".repeat(32),
                100,
                &TopicSession::fresh(
                    BackendKind::Codex,
                    AbsolutePath::new(std::env::temp_dir()).unwrap(),
                )
                .with_session(SessionId::parse("s")),
                text.to_owned(),
            )
            .unwrap(),
            "",
        )
        .unwrap()
    }
    async fn store(text: &str) -> Arc<MemoryResults> {
        let store = Arc::new(MemoryResults::default());
        store.time.store(100, Ordering::SeqCst);
        store.save(pending(text)).await.unwrap();
        store
    }
    #[tokio::test]
    async fn outbox_retries_offline_without_resending_acknowledged_parts() {
        let store = store(&"hello ".repeat(1500)).await;
        let messenger = Arc::new(FakeMessenger::with_sends(vec![
            Ok(()),
            Err(SendError::Failed("offline".to_owned())),
            Ok(()),
        ]));
        let sender = Sender::new(messenger.clone());
        let settings = settings();
        let mut watched = settings.subscribe();
        once(store.as_ref(), &sender, &mut watched, 100).await.unwrap();
        once(store.as_ref(), &sender, &mut watched, 100).await.unwrap();
        assert_eq!(store.pending(key().chat).await.unwrap().entries[0].next(), 1);
        assert_eq!(messenger.send_attempts().len(), 2);
        once(store.as_ref(), &sender, &mut watched, 104).await.unwrap();
        assert_eq!(messenger.send_attempts().len(), 2);
        store.time.store(105, Ordering::SeqCst);
        // Recreate state from exactly the durable journal as on process restart.
        let raw = store.journal.lock().unwrap().dump().unwrap();
        *store.journal.lock().unwrap() = hub_core::results::Results::parse(&raw, 105).unwrap();
        once(store.as_ref(), &sender, &mut settings.subscribe(), 105).await.unwrap();
        let attempts = messenger.send_attempts();
        assert_eq!(attempts[1].text, attempts[2].text);
        assert_ne!(attempts[0].text, attempts[2].text);
        assert!(attempts.iter().all(|message| message.keyboard.is_empty()));
    }
    #[tokio::test]
    async fn outbox_retry_after_and_permanent_failure_survive_manual_retry() {
        let store = store("**answer**").await;
        let messenger = Arc::new(FakeMessenger::with_sends(vec![
            Err(SendError::RetryAfter(Duration::from_hours(1))),
            Err(SendError::Rejected("bad html".to_owned())),
            Err(SendError::Rejected("forbidden".to_owned())),
            Ok(()),
        ]));
        let sender = Sender::new(messenger.clone());
        let settings = settings();
        once(store.as_ref(), &sender, &mut settings.subscribe(), 100).await.unwrap();
        once(store.as_ref(), &sender, &mut settings.subscribe(), 3699).await.unwrap();
        assert_eq!(messenger.send_attempts().len(), 1);
        store.time.store(3700, Ordering::SeqCst);
        once(store.as_ref(), &sender, &mut settings.subscribe(), 3700).await.unwrap();
        let blocked = store.pending(key().chat).await.unwrap().entries.remove(0);
        assert_eq!(blocked.state(), State::Blocked(hub_core::outbox::Reason::Rejected));
        assert_eq!(blocked.chunk().unwrap().format, hub_core::outbox::Format::Plain);
        let button = Change::new(&blocked, Action::Retry);
        store.update(button.clone()).await.unwrap();
        assert!(store.update(button).await.is_err());
        once(store.as_ref(), &sender, &mut settings.subscribe(), 3700).await.unwrap();
        assert!(store.pending(key().chat).await.unwrap().entries.is_empty());
        assert_eq!(messenger.sent_texts().len(), 1);
    }
    #[tokio::test]
    async fn outbox_never_sends_without_a_durable_claim_and_cancels_on_settings_change() {
        let store = store("private").await;
        let messenger = Arc::new(FakeMessenger::default());
        let sender = Sender::new(messenger.clone());
        let settings = settings();
        store.fail.store(true, Ordering::SeqCst);
        assert!(once(store.as_ref(), &sender, &mut settings.subscribe(), 100).await.is_err());
        assert!(messenger.send_attempts().is_empty());
        store.fail.store(false, Ordering::SeqCst);
        let release = messenger.delay_next_send();
        let mut watched = settings.subscribe();
        let sending = once(store.as_ref(), &sender, &mut watched, 100);
        tokio::pin!(sending);
        tokio::select! { result = &mut sending => panic!("unexpected {result:?}"), () = tokio::task::yield_now() => {} }
        let mut draft = Draft::from_settings(&settings.borrow());
        draft.chat = "-200".to_owned();
        settings.send(Arc::new(draft.parse(&std::env::temp_dir()).unwrap())).unwrap();
        sending.await.unwrap();
        assert!(release.send(()).is_err());
        assert!(messenger.sent_texts().is_empty());
        assert_eq!(store.pending(key().chat).await.unwrap().entries[0].next(), 0);
        assert_eq!(store.pending(key().chat).await.unwrap().entries[0].state(), State::Leased);
    }
    #[tokio::test(start_paused = true)]
    async fn outbox_settings_changes_cannot_shorten_retry_after_or_rebind_old_messenger() {
        let store = store("limited").await;
        let mut other: serde_json::Value = serde_json::to_value(pending("other")).unwrap();
        other["result"]["run"] = "b".repeat(32).into();
        store.save(serde_json::from_value(other).unwrap()).await.unwrap();
        let messenger = Arc::new(FakeMessenger::with_sends(vec![Err(SendError::RetryAfter(
            Duration::from_hours(1),
        ))]));
        let sender = Sender::new(messenger.clone());
        let settings = settings();
        let mut outbox = Outbox::new(store.clone(), sender, settings.subscribe());
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(messenger.send_attempts().len(), 1);
        // Same Telegram settings, unrelated update: the server's deadline still applies.
        let unchanged = settings.borrow().clone();
        settings.send(unchanged).unwrap();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(messenger.send_attempts().len(), 1);
        // Changing identity while sleeping must stop the previous connection's pump.
        let mut draft = Draft::from_settings(&settings.borrow());
        draft.chat = "-200".to_owned();
        settings.send(Arc::new(draft.parse(&std::env::temp_dir()).unwrap())).unwrap();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(outbox.pump.is_finished());
        outbox.shutdown();
    }
    struct DelayedCheckpoint {
        store: Arc<MemoryResults>,
        gate: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }
    impl ResultStore for DelayedCheckpoint {
        fn save(
            &self,
            pending: Pending,
        ) -> futures::future::BoxFuture<'static, Result<hub_core::outbox::SaveOutcome, String>>
        {
            self.store.save(pending)
        }
        fn load(
            &self,
            key: TopicKey,
        ) -> futures::future::BoxFuture<'static, Result<Option<SavedResult>, String>> {
            self.store.load(key)
        }
        fn pending(
            &self,
            chat: ChatId,
        ) -> futures::future::BoxFuture<'static, Result<hub_core::outbox::Snapshot, String>>
        {
            self.store.pending(chat)
        }
        fn update(
            &self,
            change: Change,
        ) -> futures::future::BoxFuture<'static, Result<Option<Pending>, String>> {
            if matches!(change.action, Action::Fail(_)) {
                let gate = self.gate.lock().unwrap().take();
                let store = self.store.clone();
                Box::pin(async move {
                    if let Some(gate) = gate {
                        gate.await.map_err(|_| "cancelled".to_owned())?;
                    }
                    store.update(change).await
                })
            } else {
                self.store.update(change)
            }
        }
    }
    #[tokio::test]
    async fn outbox_settings_change_after_429_does_not_cancel_its_checkpoint() {
        let store = store("answer").await;
        let (release, wait) = tokio::sync::oneshot::channel();
        let delayed =
            DelayedCheckpoint { store: store.clone(), gate: std::sync::Mutex::new(Some(wait)) };
        let messenger = Arc::new(FakeMessenger::with_sends(vec![Err(SendError::RetryAfter(
            Duration::from_hours(1),
        ))]));
        let sender = Sender::new(messenger);
        let settings = settings();
        let mut watched = settings.subscribe();
        let sending = once(&delayed, &sender, &mut watched, 100);
        tokio::pin!(sending);
        tokio::select! { result = &mut sending => panic!("unexpected {result:?}"), () = tokio::task::yield_now() => {} }
        assert!(delayed.gate.lock().unwrap().is_none());
        let unchanged = settings.borrow().clone();
        settings.send(unchanged).unwrap();
        tokio::select! { result = &mut sending => panic!("checkpoint was cancelled: {result:?}"), () = tokio::task::yield_now() => {} }
        release.send(()).unwrap();
        assert!(
            matches!(sending.await.unwrap(), Step::Sent(wait) if wait == Duration::from_hours(1))
        );
        assert_eq!(store.pending(key().chat).await.unwrap().not_before, 3700);
    }
    #[tokio::test]
    async fn outbox_lost_checkpoint_repeats_same_id_and_never_advances_without_ack() {
        let store = store(&"answer ".repeat(1000)).await;
        let messenger = Arc::new(FakeMessenger::default());
        let sender = Sender::new(messenger.clone());
        let settings = settings();
        let release = messenger.delay_next_send();
        let mut watched = settings.subscribe();
        let sending = once(store.as_ref(), &sender, &mut watched, 100);
        tokio::pin!(sending);
        tokio::select! { result = &mut sending => panic!("unexpected {result:?}"), () = tokio::task::yield_now() => {} }
        store.fail.store(true, Ordering::SeqCst);
        release.send(()).unwrap();
        assert!(
            matches!(sending.await.unwrap(), Step::Sent(wait) if wait >= Duration::from_secs(10))
        );
        assert_eq!(messenger.send_attempts().len(), 1);
        store.fail.store(false, Ordering::SeqCst);
        assert_eq!(store.pending(key().chat).await.unwrap().entries[0].next(), 0);
        store.time.store(160, Ordering::SeqCst);
        once(store.as_ref(), &sender, &mut settings.subscribe(), 160).await.unwrap();
        let messages = messenger.sent_texts();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0], messages[1]);
        assert_eq!(store.pending(key().chat).await.unwrap().entries[0].next(), 1);
    }
    #[tokio::test(start_paused = true)]
    async fn outbox_network_timeout_keeps_the_payload_for_retry() {
        let store = store("answer").await;
        let messenger = Arc::new(FakeMessenger::default());
        let release = messenger.delay_next_send();
        let sender = Sender::new(messenger.clone());
        let settings = settings();
        once(store.as_ref(), &sender, &mut settings.subscribe(), 100).await.unwrap();
        assert!(release.send(()).is_err());
        assert!(messenger.sent_texts().is_empty());
        let snapshot = store.pending(key().chat).await.unwrap();
        assert_eq!(snapshot.entries[0].next(), 0);
        assert_eq!(snapshot.entries[0].failures(), 1);
        assert_eq!(snapshot.entries[0].due(), 105);
    }
    #[tokio::test]
    async fn outbox_observed_retry_after_survives_a_failed_checkpoint() {
        let store = store("answer").await;
        let messenger = Arc::new(FakeMessenger::with_sends(vec![Err(SendError::RetryAfter(
            Duration::from_hours(1),
        ))]));
        let sender = Sender::new(messenger.clone());
        let release = messenger.delay_next_send();
        let settings = settings();
        let mut watched = settings.subscribe();
        let sending = once(store.as_ref(), &sender, &mut watched, 100);
        tokio::pin!(sending);
        tokio::select! { result = &mut sending => panic!("unexpected {result:?}"), () = tokio::task::yield_now() => {} }
        store.fail.store(true, Ordering::SeqCst);
        release.send(()).unwrap();
        assert!(
            matches!(sending.await.unwrap(), Step::Sent(wait) if wait == Duration::from_hours(1))
        );
    }
    #[tokio::test]
    async fn outbox_shutdown_aborts_send_and_unauthorized_controls_do_nothing() {
        let store = store("private").await;
        let messenger = Arc::new(FakeMessenger::default());
        let release = messenger.delay_next_send();
        let settings = settings();
        let mut outbox =
            Outbox::new(store.clone(), Sender::new(messenger.clone()), settings.subscribe());
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(messenger.send_attempts().len(), 1);
        let entry = store.pending(key().chat).await.unwrap().entries.remove(0);
        let data = callback(&Change::new(&entry, Action::Drop));
        outbox.menu(key(), Some(UserId(999)));
        outbox.press(&Press {
            callback: "bad".to_owned(),
            user: UserId(999),
            chat: Some(key().chat),
            message: Some(MessageId(1)),
            html: None,
            data,
        });
        assert!(outbox.controls.is_empty());
        outbox.shutdown();
        tokio::task::yield_now().await;
        assert!(release.send(()).is_err());
        assert!(messenger.sent_texts().is_empty());
        assert_eq!(store.pending(key().chat).await.unwrap().entries.len(), 1);
    }
    #[tokio::test]
    async fn outbox_menu_pages_topic_answers_and_buttons_drop_once_without_changing_last() {
        let store = Arc::new(MemoryResults::default());
        store.time.store(100, Ordering::SeqCst);
        for id in 0..8 {
            let target = if id == 7 { TopicKey { thread: ThreadId(8), ..key() } } else { key() };
            let saved = SavedResult::new(
                target,
                format!("{id:032x}"),
                100,
                &TopicSession::fresh(
                    BackendKind::Codex,
                    AbsolutePath::new(std::env::temp_dir()).unwrap(),
                )
                .with_session(SessionId::parse("s")),
                format!("answer {id}"),
            )
            .unwrap();
            let queued = Pending::new(saved, "").unwrap();
            store.save(queued.clone()).await.unwrap();
            let claimed = store.update(Change::new(&queued, Action::Claim)).await.unwrap().unwrap();
            store.update(Change::new(&claimed, Action::Fail(Failure::Permanent))).await.unwrap();
        }
        let settings = settings();
        let messenger = Arc::new(FakeMessenger::default());
        let mut outbox =
            Outbox::new(store.clone(), Sender::new(messenger.clone()), settings.subscribe());
        outbox.menu(key(), Some(UserId(1)));
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let first = messenger.sent_texts().pop().unwrap();
        assert!(first.contains("Очередь доставки · 7"));
        assert_eq!(first.matches("Частей:").count(), 5);
        let (message, keyboard) = messenger.last_keyboard().unwrap();
        let data =
            keyboard.iter().flatten().find(|button| button.text == "→").unwrap().data.clone();
        let press = |data: String| Press {
            callback: "page".to_owned(),
            user: UserId(1),
            chat: Some(key().chat),
            message: Some(message),
            html: None,
            data,
        };
        outbox.press(&press(data));
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(messenger.sent_texts().last().unwrap().matches("Частей:").count(), 2);
        let (_, keyboard) = messenger.last_keyboard().unwrap();
        let data = keyboard
            .iter()
            .flatten()
            .find(|button| button.text.contains("Удалить 6"))
            .unwrap()
            .data
            .clone();
        outbox.press(&press(data.clone()));
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(store.pending(key().chat).await.unwrap().entries.len(), 7);
        assert_eq!(store.load(key()).await.unwrap().unwrap().text(), "answer 6");
        outbox.press(&press(data));
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(store.pending(key().chat).await.unwrap().entries.len(), 7);
        assert!(messenger.sent_texts().last().unwrap().contains("уже удалена"));
        outbox.shutdown();
    }
    #[tokio::test]
    async fn outbox_controls_are_bounded_during_delivery_and_revocation_cancels_the_card() {
        let store = Arc::new(MemoryResults::default());
        let settings = settings();
        let messenger = Arc::new(FakeMessenger::default());
        let release = messenger.delay_next_send();
        let mut outbox = Outbox::new(store, Sender::new(messenger.clone()), settings.subscribe());
        outbox.menu(key(), Some(UserId(1)));
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert_eq!(messenger.send_attempts().len(), 1);
        outbox.menu(key(), Some(UserId(1)));
        for thread in 8..12 {
            outbox.menu(TopicKey { thread: ThreadId(thread), ..key() }, Some(UserId(1)));
        }
        assert_eq!(outbox.controls.len(), 4);
        let mut draft = Draft::from_settings(&settings.borrow());
        draft.users = "2".to_owned();
        settings.send(Arc::new(draft.parse(&std::env::temp_dir()).unwrap())).unwrap();
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        assert!(release.send(()).is_err());
        assert!(messenger.sent_texts().is_empty());
        outbox.shutdown();
    }
    #[test]
    fn outbox_callback_round_trips_at_telegram_limit_and_rejects_invalid_data() {
        let mut change = Change::new(&pending("text"), Action::Drop);
        change.key.thread = ThreadId(i32::MAX);
        change.revision = u32::MAX;
        let data = callback(&change);
        assert!(data.len() <= 64);
        let request = parse(&Press {
            callback: "c".to_owned(),
            user: UserId(1),
            chat: Some(key().chat),
            message: Some(MessageId(1)),
            html: None,
            data,
        })
        .unwrap();
        assert_eq!(request.key, change.key);
        assert_eq!(request.change.unwrap().revision, u32::MAX);
        assert!(
            parse(&Press {
                callback: "c".to_owned(),
                user: UserId(1),
                chat: Some(key().chat),
                message: None,
                html: None,
                data: "outbox:d:-1:bad:1".to_owned()
            })
            .is_none()
        );
    }
}
