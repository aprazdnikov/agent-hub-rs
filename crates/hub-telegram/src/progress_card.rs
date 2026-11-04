//! Lossy progress snapshots, isolated from the reliable result delivery path.

use crate::{
    inbound::Press,
    messenger::{Outgoing, SendError, Target},
    progress::Progress,
    sender::Sender,
};
use hub_core::{
    domain::{ThreadId, TopicKey},
    questions::Button,
    settings::{ProgressMode, Settings, TelegramSettings},
};
use std::fmt::Write;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::{sync::watch, task::JoinHandle, time::Instant};

pub const PREFIX: &str = "task:";
const LIMIT: usize = 64;
const UPDATE: Duration = Duration::from_secs(5);
const REQUEST: Duration = Duration::from_secs(10);
const FINALIZE: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Tools {
    count: u64,
    last: String,
}

#[derive(Clone)]
pub struct Card {
    callback: String,
    active: Arc<AtomicBool>,
    tools: Arc<Mutex<Tools>>,
}
impl Card {
    pub fn compact(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }
    pub fn matches(&self, data: &str) -> bool {
        self.callback == data
    }
    pub fn tool(&self, name: &str) {
        let mut tools = self.tools.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        tools.count = tools.count.saturating_add(1);
        tools.last = name.chars().filter(|c| !c.is_control()).take(80).collect();
    }
    fn render(&self, progress: &Progress) -> Outgoing {
        let snapshot = progress.snapshot();
        let tools = self.tools.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut text =
            format!("📍 Прогресс задачи\n{}", snapshot.describe(std::time::Instant::now()));
        if tools.count > 0 {
            let _ = write!(text, "\nИнструменты: {} · {}", tools.count, tools.last);
        }
        let keyboard = if snapshot.is_finished() {
            Vec::new()
        } else {
            vec![vec![Button {
                text: "⏹ Остановить".into(), data: self.callback.clone()
            }]]
        };
        Outgoing::plain(text).with_keyboard(keyboard)
    }
}

struct Fallback(Card);
impl Drop for Fallback {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::Relaxed);
    }
}

/// Only card requests share this budget. Interactive prompts/results keep their own delivery.
#[derive(Default)]
struct Rate(tokio::sync::Mutex<Option<Instant>>);
impl Rate {
    async fn acquire(&self) {
        loop {
            let mut next = self.0.lock().await;
            let now = Instant::now();
            if let Some(at) = *next
                && at > now
            {
                drop(next);
                tokio::time::sleep_until(at).await;
            } else {
                *next = Some(now + Duration::from_secs(1));
                return;
            }
        }
    }
    async fn defer(&self, delay: Duration) -> bool {
        let Some(at) = Instant::now().checked_add(delay) else {
            return false;
        };
        let mut next = self.0.lock().await;
        *next = Some(next.map_or(at, |previous| previous.max(at)));
        true
    }
}

#[derive(Default)]
pub struct Cards {
    tasks: Vec<JoinHandle<()>>,
    answers: Vec<JoinHandle<()>>,
    rate: Arc<Rate>,
}
impl Cards {
    pub fn start(
        &mut self,
        key: TopicKey,
        progress: Progress,
        sender: Sender,
        settings: watch::Receiver<Arc<Settings>>,
    ) -> Option<Card> {
        self.tasks.retain(|task| !task.is_finished());
        if settings.borrow().progress_mode != ProgressMode::Compact || self.tasks.len() >= LIMIT {
            return None;
        }
        let card = Card {
            callback: format!("{PREFIX}{}:{}", key.thread.0, uuid::Uuid::new_v4().simple()),
            active: Arc::new(AtomicBool::new(true)),
            tools: Arc::new(Mutex::new(Tools::default())),
        };
        let fallback = Fallback(card.clone());
        let rate = self.rate.clone();
        let identity = settings.borrow().telegram.clone();
        self.tasks.push(tokio::spawn(async move {
            // The guard also disables compact output if this task is aborted before its first poll.
            let guard = fallback;
            let work = update(key, &guard.0, &progress, &sender, &rate);
            tokio::pin!(work);
            tokio::select! {
                biased;
                () = identity_changed(settings, identity) => {},
                () = async {
                    tokio::select! {
                        result = &mut work => { report(result); return; },
                        () = progress.finished() => {},
                    }
                    if let Ok(result) = tokio::time::timeout(FINALIZE, &mut work).await {
                        report(result);
                    } else {
                        tracing::warn!("progress card finalization timed out");
                    }
                } => {},
            }
        }));
        Some(card)
    }
    pub fn answer(
        &mut self,
        sender: Sender,
        callback: String,
        stopped: bool,
        settings: watch::Receiver<Arc<Settings>>,
    ) {
        self.answers.retain(|task| !task.is_finished());
        if self.answers.len() >= 4 {
            return;
        }
        let identity = settings.borrow().telegram.clone();
        self.answers.push(tokio::spawn(async move {
            let text = if stopped { "Останавливаю…" } else { "Этот запуск уже завершён или кнопка недоступна." };
            tokio::select! {
                biased;
                () = identity_changed(settings, identity) => {},
                _ = tokio::time::timeout(Duration::from_secs(5), sender.answer(callback, Some(text))) => {},
            }
        }));
    }
    pub fn shutdown(&mut self) {
        for task in self.tasks.drain(..).chain(self.answers.drain(..)) {
            task.abort();
        }
    }
}
impl Drop for Cards {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub fn target(press: &Press, identity: &TelegramSettings) -> Option<TopicKey> {
    if !crate::telegram::allowed(identity, press.chat, Some(press.user)) || press.message.is_none()
    {
        return None;
    }
    let (thread, _) = press.data.strip_prefix(PREFIX)?.split_once(':')?;
    let thread: i32 = thread.parse().ok()?;
    (thread > 0).then_some(TopicKey { chat: identity.chat, thread: ThreadId(thread) })
}

async fn identity_changed(
    mut settings: watch::Receiver<Arc<Settings>>,
    identity: TelegramSettings,
) {
    loop {
        if settings.borrow_and_update().telegram != identity {
            return;
        }
        if settings.changed().await.is_err() {
            return;
        }
    }
}

fn report(result: Result<(), SendError>) {
    if let Err(error) = result {
        tracing::warn!(%error, "progress card disabled");
    }
}

async fn update(
    key: TopicKey,
    card: &Card,
    progress: &Progress,
    sender: &Sender,
    rate: &Rate,
) -> Result<(), SendError> {
    let mut message = None;
    let mut previous = None;
    loop {
        rate.acquire().await;
        let next = card.render(progress);
        let finished = next.keyboard.is_empty();
        if previous.as_ref() != Some(&next) {
            let request = async {
                if let Some(id) = message {
                    sender.messenger().edit_card(key.chat, id, next.clone()).await
                } else {
                    message =
                        Some(sender.messenger().send(Target::Topic(key), next.clone()).await?);
                    Ok(())
                }
            };
            match tokio::time::timeout(REQUEST, request).await {
                Ok(Ok(())) => previous = Some(next),
                Ok(Err(SendError::RetryAfter(delay))) if rate.defer(delay).await => {
                    tokio::time::sleep(UPDATE).await;
                    continue;
                }
                Ok(Err(error)) => return Err(error),
                Err(_) => return Err(SendError::Failed("Тайм-аут карточки".into())),
            }
        }
        if finished {
            return Ok(());
        }
        tokio::time::sleep(UPDATE).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Call, FakeMessenger};
    use hub_core::domain::AgentEvent;
    use hub_core::domain::{ChatId, Finished, SessionId, ThreadId, Usage};
    use hub_core::settings::Draft;

    const KEY: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };

    fn settings() -> watch::Sender<Arc<Settings>> {
        let settings = Draft {
            token: "1:a".into(),
            chat: "-100".into(),
            users: "1".into(),
            workspace_root: std::env::temp_dir().display().to_string(),
            ..Draft::default()
        }
        .parse(&std::env::temp_dir())
        .unwrap();
        watch::Sender::new(Arc::new(settings))
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    fn finish(progress: &Progress) {
        progress.observe(&AgentEvent::Finished(Finished {
            session: SessionId::parse("session").unwrap(),
            usage: Usage::Claude { turns: 1, cost: None },
            background: 0,
        }));
        progress.finish(false);
    }

    #[tokio::test(start_paused = true)]
    async fn bursts_coalesce_and_finished_card_removes_stop() {
        let messenger = Arc::new(FakeMessenger::default());
        let settings = settings();
        let mut cards = Cards::default();
        let progress = Progress::new();
        let card = cards
            .start(KEY, progress.clone(), Sender::new(messenger.clone()), settings.subscribe())
            .unwrap();
        settle().await;
        assert_eq!(messenger.sent_texts().len(), 1);
        for _ in 0..1000 {
            card.tool("read\nfile");
        }
        tokio::time::advance(Duration::from_secs(4)).await;
        settle().await;
        assert_eq!(messenger.calls().len(), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        let calls = messenger.calls();
        let Call::EditCard { card: edited, .. } = calls.last().unwrap() else { panic!("no edit") };
        assert!(edited.text.contains("1000"));
        assert!(edited.text.contains("readfile"));
        assert!(!edited.keyboard.is_empty());
        finish(&progress);
        tokio::time::advance(Duration::from_secs(5)).await;
        settle().await;
        let calls = messenger.calls();
        let Call::EditCard { card: edited, .. } = calls.last().unwrap() else {
            panic!("no final edit")
        };
        assert!(edited.keyboard.is_empty());
        assert!(edited.text.contains("Завершено"));
        assert!(cards.tasks.iter().all(tokio::task::JoinHandle::is_finished));
    }

    #[tokio::test(start_paused = true)]
    async fn failed_creation_falls_back_without_duplicate_send() {
        let messenger =
            Arc::new(FakeMessenger::with_sends(vec![Err(SendError::Failed("offline".into()))]));
        let settings = settings();
        let mut cards = Cards::default();
        let card = cards
            .start(KEY, Progress::new(), Sender::new(messenger.clone()), settings.subscribe())
            .unwrap();
        settle().await;
        assert!(!card.compact());
        tokio::time::advance(Duration::from_mins(1)).await;
        settle().await;
        assert_eq!(messenger.send_attempts().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limit_waits_full_delay_and_finalization_is_bounded() {
        let messenger = Arc::new(FakeMessenger::with_edits(vec![Err(SendError::RetryAfter(
            Duration::from_mins(1),
        ))]));
        let settings = settings();
        let mut cards = Cards::default();
        let progress = Progress::new();
        let card = cards
            .start(KEY, progress.clone(), Sender::new(messenger.clone()), settings.subscribe())
            .unwrap();
        settle().await;
        card.tool("read");
        tokio::time::advance(Duration::from_secs(5)).await;
        settle().await;
        assert_eq!(messenger.card_attempts().len(), 1);
        tokio::time::advance(Duration::from_secs(59)).await;
        settle().await;
        assert_eq!(messenger.card_attempts().len(), 1);
        finish(&progress);
        settle().await;
        tokio::time::advance(Duration::from_secs(1)).await;
        settle().await;
        assert_eq!(messenger.card_attempts().len(), 2);
        assert!(cards.tasks.iter().all(tokio::task::JoinHandle::is_finished));
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_pauses_other_cards_and_finalization_releases_slots() {
        let messenger = Arc::new(FakeMessenger::with_edits(vec![Err(SendError::RetryAfter(
            Duration::from_mins(1),
        ))]));
        let settings = settings();
        let mut cards = Cards::default();
        let progress = Progress::new();
        let first = cards
            .start(KEY, progress.clone(), Sender::new(messenger.clone()), settings.subscribe())
            .unwrap();
        settle().await;
        first.tool("read");
        tokio::time::advance(UPDATE).await;
        settle().await;
        let second_progress = Progress::new();
        let second = cards
            .start(
                KEY,
                second_progress.clone(),
                Sender::new(messenger.clone()),
                settings.subscribe(),
            )
            .unwrap();
        finish(&progress);
        finish(&second_progress);
        settle().await;
        tokio::time::advance(FINALIZE).await;
        settle().await;
        assert!(!first.compact() && !second.compact());
        assert_eq!(messenger.send_attempts().len(), 1);
        assert_eq!(messenger.card_attempts().len(), 1);
        assert!(cards.tasks.iter().all(tokio::task::JoinHandle::is_finished));
        // The old visible keyboard is harmless: Hub rejects callbacks once this run has ended.
        assert!(!messenger.last_keyboard().unwrap().1.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn slow_requests_time_out_and_deleted_cards_do_not_retry() {
        let messenger = Arc::new(FakeMessenger::default());
        let settings = settings();
        let mut cards = Cards::default();
        let release = messenger.delay_next_send();
        let card = cards
            .start(KEY, Progress::new(), Sender::new(messenger.clone()), settings.subscribe())
            .unwrap();
        settle().await;
        tokio::time::advance(REQUEST).await;
        settle().await;
        assert!(!card.compact());
        assert!(release.send(()).is_err());
        assert!(messenger.calls().is_empty());

        let messenger = Arc::new(FakeMessenger::with_edits(vec![Err(SendError::Rejected(
            "message to edit not found".into(),
        ))]));
        let card = cards
            .start(KEY, Progress::new(), Sender::new(messenger.clone()), settings.subscribe())
            .unwrap();
        settle().await;
        card.tool("write");
        tokio::time::advance(UPDATE).await;
        settle().await;
        assert!(!card.compact());
        tokio::time::advance(Duration::from_mins(1)).await;
        settle().await;
        assert_eq!(messenger.card_attempts().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn mode_is_frozen_but_identity_change_cancels_inflight_edit() {
        let messenger = Arc::new(FakeMessenger::default());
        let settings = settings();
        let mut cards = Cards::default();
        let card = cards
            .start(KEY, Progress::new(), Sender::new(messenger.clone()), settings.subscribe())
            .unwrap();
        settle().await;
        settings.send_modify(|s| Arc::make_mut(s).progress_mode = ProgressMode::Detailed);
        settle().await;
        assert!(card.compact());
        assert!(
            cards
                .start(KEY, Progress::new(), Sender::new(messenger.clone()), settings.subscribe())
                .is_none()
        );
        let release = messenger.delay_next_edit();
        card.tool("read");
        tokio::time::advance(UPDATE).await;
        settle().await;
        assert_eq!(messenger.card_attempts().len(), 1);
        settings.send_modify(|s| Arc::make_mut(s).telegram.chat = ChatId(-200));
        settle().await;
        assert!(!card.compact());
        assert!(release.send(()).is_err());
        assert_eq!(messenger.calls().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn capacity_shared_rate_and_shutdown_are_bounded() {
        let messenger = Arc::new(FakeMessenger::default());
        let settings = settings();
        let mut cards = Cards::default();
        let handles: Vec<_> = (0..LIMIT)
            .map(|_| {
                cards
                    .start(
                        KEY,
                        Progress::new(),
                        Sender::new(messenger.clone()),
                        settings.subscribe(),
                    )
                    .unwrap()
            })
            .collect();
        assert!(
            cards
                .start(KEY, Progress::new(), Sender::new(messenger.clone()), settings.subscribe())
                .is_none()
        );
        assert!(handles.iter().all(|c| c.callback.len() <= 64));
        assert_ne!(handles[0].callback, handles[1].callback);
        settle().await;
        assert_eq!(messenger.send_attempts().len(), 1);
        tokio::time::advance(Duration::from_millis(999)).await;
        settle().await;
        assert_eq!(messenger.send_attempts().len(), 1);
        tokio::time::advance(Duration::from_millis(1)).await;
        settle().await;
        assert_eq!(messenger.send_attempts().len(), 2);
        for _ in 0..20 {
            cards.answer(Sender::new(messenger.clone()), "ack".into(), true, settings.subscribe());
        }
        assert_eq!(cards.answers.len(), 4);
        cards.shutdown();
        settle().await;
        assert!(handles.iter().all(|c| !c.compact()));
        tokio::time::advance(Duration::from_secs(100)).await;
        settle().await;
        assert_eq!(messenger.send_attempts().len(), 2);
    }
}
