//! The core behind the window: owns the live settings and the bot, applies commands from the
//! window, and publishes what the window shows.

use hub_codex::protocol::CodexAuth;
use hub_core::domain::{BackendKind, ChatId};
use hub_qwen::backend::QwenAuth;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_core::domain::TopicKey;
use hub_core::settings::{Draft, Settings};
use hub_telegram::hub::{HubMessage, TopicView};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, sleep_until};

use crate::config::{Loaded, SettingsStore, project_errors};
use crate::error::StoreError;
use crate::logging::Repaint;

/// The hub itself gives up on sessions after 10 s; the rest covers the dispatcher.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(15);
/// First pause before reconnecting after a network failure; it doubles up to `RETRY_MAX`.
pub const RETRY_FIRST: Duration = Duration::from_secs(5);
pub const RETRY_MAX: Duration = Duration::from_mins(5);
const OUTBOX_REFRESH: Duration = Duration::from_secs(5);

pub enum Command {
    Save(Box<Draft>),
    Start,
    Stop,
    Diagnose,
    StopTopic(TopicKey),
    ResetTopic(TopicKey),
    Quit(oneshot::Sender<()>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotStatus {
    Unconfigured,
    Stopped,
    Starting,
    Retrying { reason: String, at: Instant },
    Running { username: String },
    Failed(String),
}

/// How an agent signs in to its model, where the hub knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAuth {
    Codex(CodexAuth),
    Qwen(QwenAuth),
    /// Hermes owns its provider credentials; starting ACP does not prove model sign-in.
    Hermes,
    /// `OpenCode` owns its provider credentials; starting ACP does not prove model sign-in.
    Opencode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    Ready { version: String, auth: Option<AgentAuth> },
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatus {
    pub kind: BackendKind,
    pub state: AgentState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub bot: BotStatus,
    pub chat_access: Option<hub_telegram::telegram::ChatAccess>,
    pub outbox: OutboxStatus,
    pub diagnostics: DiagnosticStatus,
    pub save_status: SaveStatus,
    pub save_revision: u64,
    pub agents: Vec<AgentStatus>,
    pub topics: Vec<TopicView>,
    /// The settings as last saved: the form's baseline.
    pub saved: Option<Draft>,
    pub notice: Option<String>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            bot: BotStatus::Unconfigured,
            chat_access: None,
            outbox: OutboxStatus::Unknown,
            diagnostics: DiagnosticStatus::Idle,
            save_status: SaveStatus::Idle,
            save_revision: 0,
            agents: Vec::new(),
            topics: Vec::new(),
            saved: None,
            notice: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveStatus {
    Idle,
    Saving,
    Delayed,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticStatus {
    Idle,
    Running,
    Complete(crate::diagnostics::Report),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxStatus {
    Unknown,
    Unavailable,
    Count(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activity {
    Running,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    Start,
    Restart,
    Live,
}

/// What saving `next` does to the bot.
#[must_use]
pub fn plan(previous: Option<&Settings>, next: &Settings, activity: Activity) -> Apply {
    match (previous, activity) {
        (None, Activity::Running | Activity::Stopped | Activity::Failed)
        | (Some(_), Activity::Failed) => Apply::Start,
        (Some(old), Activity::Running) if old.telegram != next.telegram => Apply::Restart,
        (Some(_), Activity::Running | Activity::Stopped) => Apply::Live,
    }
}

pub type Stopper = Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>;

pub struct Started {
    pub username: String,
    pub chat_access: hub_telegram::telegram::ChatAccess,
    pub agents: Vec<AgentStatus>,
    pub mailbox: mpsc::Sender<HubMessage>,
    pub views: watch::Receiver<Vec<TopicView>>,
    pub stop: Stopper,
}

pub trait Connector: Send + 'static {
    type Error: std::fmt::Display + Send + 'static;

    /// Whether the start may succeed later on its own, e.g. the network is not up yet.
    fn transient(error: &Self::Error) -> bool;

    fn diagnose(&self, settings: Arc<Settings>) -> BoxFuture<'static, crate::diagnostics::Report> {
        Box::pin(crate::diagnostics::run(settings))
    }

    fn outbox_count(&self, chat: ChatId) -> BoxFuture<'static, Result<usize, String>>;

    fn connect(
        &self,
        settings: watch::Receiver<Arc<Settings>>,
    ) -> BoxFuture<'static, Result<Started, Self::Error>>;
}

pub struct SupervisorSetup<C> {
    pub connector: C,
    pub store: Box<dyn SettingsStore>,
    pub save_guard: Option<Arc<crate::instance::InstanceLock>>,
    pub home: PathBuf,
    pub loaded: Result<Loaded, StoreError>,
    pub snapshot: watch::Sender<Snapshot>,
    pub repaint: Repaint,
}

struct Bot {
    mailbox: mpsc::Sender<HubMessage>,
    stop: Stopper,
}

pub struct Supervisor<C: Connector> {
    connector: C,
    store: Option<Box<dyn SettingsStore>>,
    save_guard: Option<Arc<crate::instance::InstanceLock>>,
    saving: Option<tokio::task::JoinHandle<SaveJob>>,
    save_slow_at: Option<Instant>,
    stop_during_save: bool,
    home: PathBuf,
    settings: Option<watch::Sender<Arc<Settings>>>,
    bot: Option<Bot>,
    connecting: Option<BoxFuture<'static, Result<Started, C::Error>>>,
    views: Option<watch::Receiver<Vec<TopicView>>>,
    retry: Option<Retry>,
    diagnostic_read: Option<BoxFuture<'static, crate::diagnostics::Report>>,
    outbox_read: Option<BoxFuture<'static, (ChatId, Result<usize, String>)>>,
    outbox_next: Instant,
    state: Snapshot,
    snapshot: watch::Sender<Snapshot>,
    repaint: Repaint,
}

struct SaveJob {
    store: Box<dyn SettingsStore>,
    draft: Draft,
    settings: Settings,
    result: Result<(), String>,
}

#[derive(Clone, Copy)]
struct Retry {
    at: Instant,
    pause: Duration,
}

enum Event<E> {
    Command(Option<Command>),
    Views { alive: bool },
    Retry,
    Started(Result<Started, E>),
    Saved(Box<Result<SaveJob, tokio::task::JoinError>>),
    SaveDelayed,
    ReadOutbox,
    DiagnosticDone(crate::diagnostics::Report),
    OutboxRead(ChatId, Result<usize, String>),
}

impl<C: Connector> Supervisor<C> {
    #[must_use]
    pub fn new(setup: SupervisorSetup<C>) -> Self {
        let SupervisorSetup { connector, store, save_guard, home, loaded, snapshot, repaint } =
            setup;
        let mut state = Snapshot::default();
        let settings = match loaded {
            Ok(Loaded::Missing) => None,
            Ok(Loaded::Ready(settings)) => {
                state.saved = Some(Draft::from_settings(&settings));
                state.bot = BotStatus::Stopped;
                Some(watch::Sender::new(Arc::new(settings)))
            }
            Ok(Loaded::Incomplete { draft, errors: _errors }) => {
                state.saved = Some(draft);
                None
            }
            Err(error) => {
                state.notice = Some(error.to_string());
                None
            }
        };
        // Published now, so the window opens on the right tab before the core starts running.
        snapshot.send_replace(state.clone());
        Self {
            connector,
            store: Some(store),
            save_guard,
            saving: None,
            save_slow_at: None,
            stop_during_save: false,
            home,
            settings,
            bot: None,
            connecting: None,
            views: None,
            retry: None,
            diagnostic_read: None,
            outbox_read: None,
            outbox_next: Instant::now(),
            state,
            snapshot,
            repaint,
        }
    }

    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        self.publish();
        if self.settings.is_some() {
            self.start();
        }
        loop {
            let mut saving = self.saving.take();
            let saved = async {
                match saving.as_mut() {
                    Some(job) => job.await,
                    None => std::future::pending().await,
                }
            };
            let save_slow_at = self.save_slow_at;
            let can_retry = self.connecting.is_none();
            let mut connecting = self.connecting.take();
            let startup = async {
                match connecting.as_mut() {
                    Some(start) => start.await,
                    None => std::future::pending().await,
                }
            };
            let mut views = self.views.take();
            let changed = async {
                match views.as_mut() {
                    Some(views) => views.changed().await.is_ok(),
                    None => std::future::pending().await,
                }
            };
            let retry = self.retry.map(|retry| retry.at);
            let mut outbox_read = self.outbox_read.take();
            let can_read = outbox_read.is_none();
            let reading = async {
                match outbox_read.as_mut() {
                    Some(read) => read.await,
                    None => std::future::pending().await,
                }
            };
            let mut diagnostic_read = self.diagnostic_read.take();
            let diagnosing = async {
                match diagnostic_read.as_mut() {
                    Some(read) => read.await,
                    None => std::future::pending().await,
                }
            };
            let event = tokio::select! {
                biased;
                command = commands.recv() => Event::Command(command),
                alive = changed => Event::Views { alive },
                () = sleep_until(retry.unwrap_or_else(Instant::now)), if retry.is_some() && can_retry => Event::Retry,
                result = startup => Event::Started(result),
                result = saved => Event::Saved(Box::new(result)),
                () = sleep_until(save_slow_at.unwrap_or_else(Instant::now)), if save_slow_at.is_some() => Event::SaveDelayed,
                (chat, result) = reading => Event::OutboxRead(chat, result),
                report = diagnosing => Event::DiagnosticDone(report),
                () = sleep_until(self.outbox_next), if self.settings.is_some() && can_read => Event::ReadOutbox,
            };
            self.saving = saving;
            self.connecting = connecting;
            self.views = views;
            self.outbox_read = outbox_read;
            self.diagnostic_read = diagnostic_read;
            match event {
                Event::Command(Some(Command::Quit(done))) => {
                    self.stop().await;
                    // The window may have stopped waiting; there is nobody else to tell.
                    let _ = done.send(());
                    return;
                }
                Event::Command(None) => {
                    self.stop().await;
                    return;
                }
                Event::Command(Some(command)) => {
                    if matches!(command, Command::Start | Command::Stop) {
                        self.retry = None;
                    }
                    self.handle(command).await;
                }
                Event::Retry => {
                    if self.bot.is_none() {
                        self.start();
                    }
                }
                Event::Saved(result) => self.finish_save(*result).await,
                Event::SaveDelayed => self.save_delayed(),
                Event::Started(result) => {
                    self.connecting = None;
                    self.finish_start(result);
                }
                Event::ReadOutbox => self.read_outbox(),
                Event::DiagnosticDone(report) => self.finish_diagnostic(report),
                Event::OutboxRead(chat, result) => self.finish_outbox_read(chat, &result),
                Event::Views { alive: true } => self.refresh_topics(),
                Event::Views { alive: false } => self.hub_stopped().await,
            }
        }
    }

    fn save_delayed(&mut self) {
        self.save_slow_at = None;
        self.state.save_status = SaveStatus::Delayed;
        self.publish();
    }

    fn finish_diagnostic(&mut self, report: crate::diagnostics::Report) {
        self.diagnostic_read = None;
        self.state.diagnostics = DiagnosticStatus::Complete(report);
        self.publish();
    }

    async fn hub_stopped(&mut self) {
        tracing::error!("hub stopped on its own");
        self.stop().await;
        self.status(BotStatus::Failed("бот остановился, подробности в логе".to_owned()));
    }

    fn finish_outbox_read(&mut self, chat: ChatId, result: &Result<usize, String>) {
        self.outbox_read = None;
        self.outbox_next = Instant::now() + OUTBOX_REFRESH;
        if self.settings.as_ref().is_some_and(|settings| settings.borrow().telegram.chat == chat) {
            self.state.outbox = match result {
                Ok(count) => OutboxStatus::Count(*count),
                Err(_) => OutboxStatus::Unavailable,
            };
            self.publish();
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::Save(draft) => self.save(*draft),
            Command::Start => {
                if self.bot.is_none() && self.connecting.is_none() && self.settings.is_some() {
                    self.stop_during_save = false;
                    self.start();
                }
            }
            Command::Stop => {
                if self.saving.is_some() {
                    self.stop_during_save = true;
                }
                self.stop().await;
                if self.settings.is_some() {
                    self.status(BotStatus::Stopped);
                }
            }
            Command::Diagnose => {
                if let Some(live) = &self.settings {
                    self.diagnostic_read =
                        Some(self.connector.diagnose(Arc::clone(&live.borrow())));
                    self.state.diagnostics = DiagnosticStatus::Running;
                    self.publish();
                }
            }
            Command::StopTopic(key) => tell(self.mailbox(), HubMessage::Stop(key)).await,
            Command::ResetTopic(key) => tell(self.mailbox(), HubMessage::Reset(key)).await,
            Command::Quit(done) => {
                // `run` handles Quit before dispatching here.
                let _ = done.send(());
            }
        }
    }

    fn save(&mut self, draft: Draft) {
        self.state.save_revision = self.state.save_revision.wrapping_add(1);
        if self.saving.is_some() || self.store.is_none() {
            self.notice(Some("Предыдущее сохранение ещё выполняется или его результат неизвестен; повтор недоступен".to_owned()));
            return;
        }
        let settings = match draft.parse(&self.home) {
            Ok(settings) => settings,
            Err(errors) => {
                let fields: Vec<_> = errors.iter().map(|error| error.message.as_str()).collect();
                self.notice(Some(format!("Настройки не сохранены: {}", fields.join("; "))));
                return;
            }
        };
        let Some(store) = self.store.take() else { return };
        let home = self.home.clone();
        let guard = self.save_guard.clone();
        self.state.notice = None;
        self.state.save_status = SaveStatus::Saving;
        self.stop_during_save = false;
        self.save_slow_at = Some(Instant::now() + Duration::from_secs(5));
        self.saving = Some(tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let result = if let Err(error) =
                hub_telegram::paths::workspace_root(&settings.workspace_root)
            {
                Err(format!("Настройки не сохранены: {error}"))
            } else {
                let errors = project_errors(&settings, &home);
                if errors.is_empty() {
                    store.save(&settings).map_err(|error| format!("{error}. Часть ключей могла измениться; проверьте настройки и повторите сохранение"))
                } else {
                    Err(format!(
                        "Настройки не сохранены: {}",
                        errors
                            .iter()
                            .map(|error| error.message.as_str())
                            .collect::<Vec<_>>()
                            .join("; ")
                    ))
                }
            };
            SaveJob { store, draft, settings, result }
        }));
        self.publish();
    }

    async fn finish_save(&mut self, outcome: Result<SaveJob, tokio::task::JoinError>) {
        self.saving = None;
        self.save_slow_at = None;
        let Ok(SaveJob { store, draft, settings, result }) = outcome else {
            self.state.save_status = SaveStatus::Uncertain;
            self.notice(Some(
                "Результат сохранения неизвестен; проверьте настройки после перезапуска".to_owned(),
            ));
            return;
        };
        self.store = Some(store);
        self.state.save_status = SaveStatus::Idle;
        if let Err(error) = result {
            self.notice(Some(error));
            return;
        }
        self.cancel_diagnostic();
        tracing::info!("settings saved");
        self.retry = None;
        self.state.saved = Some(draft);
        self.state.notice = None;
        let previous = self.settings.as_ref().map(|live| Arc::clone(&live.borrow()));
        if previous.as_ref().is_none_or(|old| old.telegram.chat != settings.telegram.chat) {
            self.outbox_read = None;
            self.outbox_next = Instant::now();
            self.state.outbox = OutboxStatus::Unknown;
        }
        let apply = if self.stop_during_save && self.bot.is_none() {
            self.connecting = None;
            self.state.bot = BotStatus::Stopped;
            Apply::Live
        } else if self.connecting.is_some() {
            self.connecting = None;
            Apply::Start
        } else {
            plan(previous.as_deref(), &settings, self.activity())
        };
        if matches!(apply, Apply::Restart | Apply::Start) {
            self.state.bot = BotStatus::Starting;
            self.state.chat_access = None;
        }
        match &self.settings {
            Some(live) => {
                live.send_replace(Arc::new(settings));
            }
            None => self.settings = Some(watch::Sender::new(Arc::new(settings))),
        }
        self.publish();
        match apply {
            Apply::Start => self.start(),
            Apply::Restart => {
                tracing::info!("telegram settings changed, restarting the bot");
                self.stop().await;
                self.start();
            }
            Apply::Live => {}
        }
    }

    fn activity(&self) -> Activity {
        match (&self.bot, &self.state.bot) {
            (Some(_), _) => Activity::Running,
            (None, BotStatus::Failed(_) | BotStatus::Retrying { .. }) => Activity::Failed,
            (
                None,
                BotStatus::Unconfigured
                | BotStatus::Stopped
                | BotStatus::Starting
                | BotStatus::Running { .. },
            ) => Activity::Stopped,
        }
    }

    fn read_outbox(&mut self) {
        if let Some(settings) = &self.settings {
            let chat = settings.borrow().telegram.chat;
            let read = self.connector.outbox_count(chat);
            self.outbox_read = Some(Box::pin(async move { (chat, read.await) }));
        }
    }

    fn start(&mut self) {
        let Some(live) = &self.settings else { return };
        let receiver = live.subscribe();
        self.connecting = Some(self.connector.connect(receiver));
        self.status(BotStatus::Starting);
    }

    fn finish_start(&mut self, result: Result<Started, C::Error>) {
        match result {
            Ok(Started { username, chat_access, agents, mailbox, views, stop }) => {
                tracing::info!(%username, "bot started");
                self.retry = None;
                self.bot = Some(Bot { mailbox, stop });
                self.views = Some(views);
                self.state.agents = agents;
                self.state.chat_access = Some(chat_access);
                self.refresh_topics();
                self.status(BotStatus::Running { username });
            }
            Err(error) if C::transient(&error) => {
                let pause =
                    self.retry.map_or(RETRY_FIRST, |retry| (retry.pause * 2).min(RETRY_MAX));
                tracing::warn!(%error, retry_in = pause.as_secs(), "bot did not start, will retry");
                let at = Instant::now() + pause;
                self.retry = Some(Retry { at, pause });
                self.status(BotStatus::Retrying { reason: error.to_string(), at });
            }
            Err(error) => {
                tracing::warn!(%error, "bot did not start");
                self.retry = None;
                self.status(BotStatus::Failed(error.to_string()));
            }
        }
    }

    fn cancel_diagnostic(&mut self) {
        self.diagnostic_read = None;
        self.state.diagnostics = DiagnosticStatus::Idle;
    }

    async fn stop(&mut self) {
        self.connecting = None;
        self.cancel_diagnostic();
        self.views = None;
        self.state.chat_access = None;
        let Some(Bot { mailbox: _mailbox, stop }) = self.bot.take() else { return };
        if tokio::time::timeout(STOP_TIMEOUT, stop()).await.is_err() {
            tracing::error!("bot did not stop in time");
        }
        self.state.topics.clear();
        self.status(BotStatus::Stopped);
    }

    /// The hub's mailbox, cloned so a send does not hold `&self` across an await.
    fn mailbox(&self) -> Option<mpsc::Sender<HubMessage>> {
        self.bot.as_ref().map(|bot| bot.mailbox.clone())
    }

    fn refresh_topics(&mut self) {
        if let Some(views) = self.views.as_mut() {
            self.state.topics.clone_from(&views.borrow_and_update());
            self.publish();
        }
    }

    fn status(&mut self, bot: BotStatus) {
        self.state.bot = bot;
        self.publish();
    }

    fn notice(&mut self, notice: Option<String>) {
        self.state.notice = notice;
        self.publish();
    }

    fn publish(&self) {
        self.snapshot.send_replace(self.state.clone());
        (self.repaint)();
    }
}

async fn tell(mailbox: Option<mpsc::Sender<HubMessage>>, message: HubMessage) {
    if let Some(mailbox) = mailbox
        && mailbox.send(message).await.is_err()
    {
        tracing::warn!("topic command dropped: the hub has stopped");
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use hub_core::domain::{ChatId, ThreadId};
    use rstest::rstest;

    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Outcome {
        Connect,
        Unseen,
        Fail,
        Offline,
        Hang,
        PendingConnect,
        GatedConnect,
    }

    #[derive(Default)]
    struct Probe {
        connects: AtomicUsize,
        stops: AtomicUsize,
        messages: StdMutex<Vec<String>>,
        settings: StdMutex<Option<watch::Receiver<Arc<Settings>>>>,
        views: StdMutex<Option<watch::Sender<Vec<TopicView>>>>,
        counts: StdMutex<VecDeque<oneshot::Receiver<Result<usize, String>>>>,
        starts: StdMutex<VecDeque<oneshot::Receiver<()>>>,
        diagnostics: StdMutex<VecDeque<oneshot::Receiver<crate::diagnostics::Report>>>,
    }

    struct FakeConnector {
        probe: Arc<Probe>,
        outcomes: StdMutex<VecDeque<Outcome>>,
    }

    impl Connector for FakeConnector {
        type Error = String;

        fn diagnose(
            &self,
            _settings: Arc<Settings>,
        ) -> BoxFuture<'static, crate::diagnostics::Report> {
            let result = self.probe.diagnostics.lock().unwrap().pop_front();
            Box::pin(async move {
                match result {
                    Some(receiver) => match receiver.await {
                        Ok(report) => report,
                        Err(_) => std::future::pending().await,
                    },
                    None => std::future::pending().await,
                }
            })
        }

        fn outbox_count(&self, _chat: ChatId) -> BoxFuture<'static, Result<usize, String>> {
            let result = self.probe.counts.lock().unwrap().pop_front();
            Box::pin(async move {
                match result {
                    Some(receiver) => receiver.await.unwrap_or_else(|_| Err("lost".to_owned())),
                    None => Ok(0),
                }
            })
        }

        fn transient(error: &String) -> bool {
            error.starts_with("Telegram недоступен")
        }

        fn connect(
            &self,
            settings: watch::Receiver<Arc<Settings>>,
        ) -> BoxFuture<'static, Result<Started, String>> {
            self.probe.connects.fetch_add(1, Ordering::SeqCst);
            *self.probe.settings.lock().unwrap() = Some(settings);
            let outcome = self.outcomes.lock().unwrap().pop_front().unwrap_or(Outcome::Connect);
            let probe = Arc::clone(&self.probe);
            let gate = if outcome == Outcome::GatedConnect {
                probe.starts.lock().unwrap().pop_front()
            } else {
                None
            };
            Box::pin(async move {
                match outcome {
                    Outcome::Fail => return Err("Telegram не принял токен бота".to_owned()),
                    Outcome::Offline => return Err("Telegram недоступен: сеть".to_owned()),
                    Outcome::PendingConnect => std::future::pending::<()>().await,
                    Outcome::GatedConnect => {
                        if let Some(gate) = gate {
                            let _ = gate.await;
                        }
                    }
                    Outcome::Connect | Outcome::Unseen | Outcome::Hang => {}
                }
                let (mailbox, mut inbox) = mpsc::channel(8);
                let recorder = Arc::clone(&probe);
                tokio::spawn(async move {
                    while let Some(message) = inbox.recv().await {
                        let line = match message {
                            HubMessage::Stop(key) => {
                                format!("stop {}/{}", key.chat.0, key.thread.0)
                            }
                            HubMessage::Reset(key) => {
                                format!("reset {}/{}", key.chat.0, key.thread.0)
                            }
                            HubMessage::HandoffDone(_)
                            | HubMessage::VoiceDone(_)
                            | HubMessage::SaveResult(_)
                            | HubMessage::LastDone(_)
                            | HubMessage::DoctorDone(_)
                            | HubMessage::ActionDone(_)
                            | HubMessage::Inbound(_)
                            | HubMessage::Press(_)
                            | HubMessage::Deliver { .. }
                            | HubMessage::FlushAlbum(_)
                            | HubMessage::Bind { .. }
                            | HubMessage::Ended { .. }
                            | HubMessage::Snapshot(_)
                            | HubMessage::Quota(_)
                            | HubMessage::Limits(_)
                            | HubMessage::Shutdown(_) => "other".to_owned(),
                        };
                        recorder.messages.lock().unwrap().push(line);
                    }
                });
                let (views, watched) = watch::channel(Vec::new());
                *probe.views.lock().unwrap() = Some(views);
                let stopper = Arc::clone(&probe);
                let stop: Stopper = Box::new(move || {
                    Box::pin(async move {
                        stopper.stops.fetch_add(1, Ordering::SeqCst);
                        if outcome == Outcome::Hang {
                            std::future::pending::<()>().await;
                        }
                    })
                });
                Ok(Started {
                    username: "hub_bot".to_owned(),
                    chat_access: if outcome == Outcome::Unseen {
                        hub_telegram::telegram::ChatAccess::Unseen
                    } else {
                        hub_telegram::telegram::ChatAccess::Visible
                    },
                    agents: vec![AgentStatus {
                        kind: BackendKind::Claude,
                        state: AgentState::Ready { version: "2.1.287".to_owned(), auth: None },
                    }],
                    mailbox,
                    views: watched,
                    stop,
                })
            })
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        saved: Arc<StdMutex<Vec<Settings>>>,
        broken: Arc<std::sync::atomic::AtomicBool>,
        gate: Arc<StdMutex<Option<std::sync::mpsc::Receiver<()>>>>,
        started: Arc<AtomicUsize>,
        panic_next: Arc<std::sync::atomic::AtomicBool>,
    }

    impl SettingsStore for MemoryStore {
        fn save(&self, settings: &Settings) -> Result<(), StoreError> {
            let gate = self.gate.lock().unwrap().take();
            self.started.fetch_add(1, Ordering::SeqCst);
            assert!(!self.panic_next.swap(false, Ordering::SeqCst), "simulated worker failure");
            if let Some(gate) = gate {
                let _ = gate.recv();
            }
            if self.broken.load(Ordering::SeqCst) {
                return Err(StoreError::Write {
                    path: "settings.toml".into(),
                    error: std::io::Error::other("диск полон"),
                });
            }
            self.saved.lock().unwrap().push(settings.clone());
            Ok(())
        }
    }

    fn draft(chat: &str, model: &str) -> Draft {
        Draft {
            token: "123:secret-token".to_owned(),
            chat: chat.to_owned(),
            users: "1".to_owned(),
            workspace_root: "~/projects".to_owned(),
            model: model.to_owned(),
            ..Draft::default()
        }
    }

    fn home() -> PathBuf {
        static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        let dir = HOME.get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            std::fs::create_dir(dir.path().join("projects")).unwrap();
            dir
        });
        dir.path().to_path_buf()
    }

    fn settings(chat: &str, model: &str) -> Settings {
        draft(chat, model).parse(&home()).unwrap()
    }

    struct Harness {
        commands: mpsc::Sender<Command>,
        snapshot: watch::Receiver<Snapshot>,
        probe: Arc<Probe>,
        saved: Arc<StdMutex<Vec<Settings>>>,
        save_gate: Arc<StdMutex<Option<std::sync::mpsc::Receiver<()>>>>,
        save_started: Arc<AtomicUsize>,
        save_panic: Arc<std::sync::atomic::AtomicBool>,
        save_broken: Arc<std::sync::atomic::AtomicBool>,
    }

    impl Harness {
        async fn send(&self, command: Command) {
            self.commands.send(command).await.unwrap();
        }

        async fn until(&mut self, what: &str, done: impl Fn(&Snapshot) -> bool) {
            let wait = self.snapshot.wait_for(|snapshot| done(snapshot));
            tokio::time::timeout(Duration::from_secs(5), wait)
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
                .unwrap();
        }

        fn connects(&self) -> usize {
            self.probe.connects.load(Ordering::SeqCst)
        }

        fn stops(&self) -> usize {
            self.probe.stops.load(Ordering::SeqCst)
        }
    }

    fn running(snapshot: &Snapshot) -> bool {
        matches!(snapshot.bot, BotStatus::Running { .. })
    }

    fn harness(loaded: Result<Loaded, StoreError>, outcomes: &[Outcome], broken: bool) -> Harness {
        harness_with_guard(loaded, outcomes, broken, None)
    }

    fn harness_with_guard(
        loaded: Result<Loaded, StoreError>,
        outcomes: &[Outcome],
        broken: bool,
        save_guard: Option<Arc<crate::instance::InstanceLock>>,
    ) -> Harness {
        let probe = Arc::new(Probe::default());
        let store = MemoryStore {
            broken: Arc::new(std::sync::atomic::AtomicBool::new(broken)),
            ..MemoryStore::default()
        };
        let saved = Arc::clone(&store.saved);
        let save_gate = Arc::clone(&store.gate);
        let save_started = Arc::clone(&store.started);
        let save_panic = Arc::clone(&store.panic_next);
        let save_broken = Arc::clone(&store.broken);
        let (snapshot, watched) = watch::channel(Snapshot::default());
        let (commands, inbox) = mpsc::channel(8);
        let supervisor = Supervisor::new(SupervisorSetup {
            connector: FakeConnector {
                probe: Arc::clone(&probe),
                outcomes: StdMutex::new(outcomes.iter().copied().collect()),
            },
            store: Box::new(store),
            save_guard,
            home: home(),
            loaded,
            snapshot,
            repaint: Arc::new(|| {}),
        });
        tokio::spawn(supervisor.run(inbox));
        Harness {
            commands,
            snapshot: watched,
            probe,
            saved,
            save_gate,
            save_started,
            save_panic,
            save_broken,
        }
    }

    #[rstest]
    #[case(None, settings("-100", ""), Activity::Stopped, Apply::Start)]
    #[case(Some(settings("-100", "")), settings("-200", ""), Activity::Running, Apply::Restart)]
    #[case(Some(settings("-100", "")), settings("-100", "opus"), Activity::Running, Apply::Live)]
    #[case(Some(settings("-100", "")), settings("-200", ""), Activity::Stopped, Apply::Live)]
    #[case(Some(settings("-100", "")), settings("-100", "opus"), Activity::Failed, Apply::Start)]
    fn changes_are_planned(
        #[case] previous: Option<Settings>,
        #[case] next: Settings,
        #[case] activity: Activity,
        #[case] expected: Apply,
    ) {
        assert_eq!(plan(previous.as_ref(), &next, activity), expected);
    }

    #[rstest]
    #[case(Ok(Loaded::Ready(settings("-100", ""))), BotStatus::Stopped)]
    #[case(Ok(Loaded::Missing), BotStatus::Unconfigured)]
    fn the_window_sees_the_loaded_state_before_the_core_runs(
        #[case] loaded: Result<Loaded, StoreError>,
        #[case] expected: BotStatus,
    ) {
        let (snapshot, watched) = watch::channel(Snapshot::default());
        let _supervisor = Supervisor::new(SupervisorSetup {
            connector: FakeConnector {
                probe: Arc::new(Probe::default()),
                outcomes: StdMutex::new(VecDeque::new()),
            },
            store: Box::new(MemoryStore::default()),
            save_guard: None,
            home: home(),
            loaded,
            snapshot,
            repaint: Arc::new(|| {}),
        });
        assert_eq!(watched.borrow().bot, expected);
    }

    #[tokio::test]
    async fn changed_telegram_settings_do_not_show_old_chat_check_while_stopping() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::Hang], false);
        harness.until("running", running).await;
        harness.send(Command::Save(Box::new(draft("-200", "")))).await;
        harness
            .until("new settings published", |s| s.saved.as_ref().is_some_and(|d| d.chat == "-200"))
            .await;
        let snapshot = harness.snapshot.borrow();
        assert_eq!((snapshot.bot.clone(), snapshot.chat_access), (BotStatus::Starting, None));
    }

    fn diagnostic_report() -> crate::diagnostics::Report {
        crate::diagnostics::Report {
            workspace: crate::diagnostics::WorkspaceCheck::Ready,
            agent: crate::diagnostics::AgentCheck::Ready("2.1.287".into()),
            agent_kind: BackendKind::Claude,
            telegram: hub_telegram::telegram::TelegramDiagnostic {
                token: hub_telegram::telegram::DiagnosticStep::Ok,
                chat: hub_telegram::telegram::DiagnosticStep::Unavailable,
                chat_kind: None,
                manage_topics: None,
            },
        }
    }

    #[tokio::test]
    async fn completed_diagnostic_is_published_and_can_be_repeated() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let (send, receive) = oneshot::channel();
        harness.probe.diagnostics.lock().unwrap().push_back(receive);
        harness.send(Command::Diagnose).await;
        harness.until("diagnostic running", |s| s.diagnostics == DiagnosticStatus::Running).await;
        send.send(diagnostic_report()).unwrap();
        harness
            .until("diagnostic finished", |s| {
                s.diagnostics == DiagnosticStatus::Complete(diagnostic_report())
            })
            .await;
        harness.send(Command::Diagnose).await;
        harness.until("diagnostic repeated", |s| s.diagnostics == DiagnosticStatus::Running).await;
    }

    #[tokio::test]
    async fn stop_cancels_an_in_progress_diagnostic() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let (send, receive) = oneshot::channel();
        harness.probe.diagnostics.lock().unwrap().push_back(receive);
        harness.send(Command::Diagnose).await;
        harness.until("diagnostic running", |s| s.diagnostics == DiagnosticStatus::Running).await;
        harness.send(Command::Stop).await;
        harness.until("stopped", |s| s.bot == BotStatus::Stopped).await;
        assert!(send.send(diagnostic_report()).is_err());
    }

    #[tokio::test]
    async fn saving_settings_cancels_an_in_progress_diagnostic() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let (send, receive) = oneshot::channel();
        harness.probe.diagnostics.lock().unwrap().push_back(receive);
        harness.send(Command::Diagnose).await;
        harness.until("diagnostic running", |s| s.diagnostics == DiagnosticStatus::Running).await;
        harness.send(Command::Save(Box::new(draft("-100", "opus")))).await;
        harness
            .until("saved diagnostic cancelled", |s| {
                s.saved.as_ref().is_some_and(|d| d.model == "opus")
                    && s.diagnostics == DiagnosticStatus::Idle
            })
            .await;
        assert!(send.send(diagnostic_report()).is_err());
    }

    #[tokio::test]
    async fn stop_interrupts_a_stalled_start() {
        let mut harness =
            harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::PendingConnect], false);
        harness.until("starting", |s| s.bot == BotStatus::Starting).await;
        harness.send(Command::Stop).await;
        harness.until("stopped", |s| s.bot == BotStatus::Stopped).await;
    }

    #[tokio::test]
    async fn save_during_start_cancels_old_attempt_and_uses_new_settings() {
        let mut harness = harness(
            Ok(Loaded::Ready(settings("-100", ""))),
            &[Outcome::PendingConnect, Outcome::Connect],
            false,
        );
        harness.until("starting", |s| s.bot == BotStatus::Starting).await;
        harness.send(Command::Save(Box::new(draft("-200", "")))).await;
        harness
            .until("new bot running", |s| {
                running(s) && s.saved.as_ref().is_some_and(|d| d.chat == "-200")
            })
            .await;
        assert_eq!(harness.connects(), 2);
        assert_eq!(
            harness.probe.settings.lock().unwrap().as_ref().unwrap().borrow().telegram.chat,
            ChatId(-200)
        );
    }

    #[tokio::test]
    async fn stop_wins_when_start_becomes_ready_at_the_same_time() {
        let mut harness =
            harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::GatedConnect], false);
        let (ready, gate) = oneshot::channel();
        harness.probe.starts.lock().unwrap().push_back(gate);
        harness.until("starting", |s| s.bot == BotStatus::Starting).await;
        tokio::task::yield_now().await;
        harness.commands.try_send(Command::Stop).unwrap();
        ready.send(()).unwrap();
        harness.until("stopped", |s| s.bot == BotStatus::Stopped).await;
        assert!(harness.probe.views.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn diagnosis_can_run_while_start_is_stalled() {
        let mut harness =
            harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::PendingConnect], false);
        harness.until("starting", |s| s.bot == BotStatus::Starting).await;
        harness.send(Command::Diagnose).await;
        harness.until("diagnostic running", |s| s.diagnostics == DiagnosticStatus::Running).await;
    }

    #[tokio::test]
    async fn quit_interrupts_a_stalled_start() {
        let mut harness =
            harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::PendingConnect], false);
        harness.until("starting", |s| s.bot == BotStatus::Starting).await;
        let (send, done) = oneshot::channel();
        harness.send(Command::Quit(send)).await;
        tokio::time::timeout(Duration::from_secs(1), done).await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_is_processed_while_a_save_worker_is_stalled() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let (release, gate) = std::sync::mpsc::channel();
        *harness.save_gate.lock().unwrap() = Some(gate);
        harness.send(Command::Save(Box::new(draft("-100", "opus")))).await;
        tokio::time::timeout(Duration::from_secs(1), async {
            while harness.save_started.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        harness.send(Command::Stop).await;
        let stopped = tokio::time::timeout(Duration::from_millis(250), async {
            harness.until("stopped", |s| s.bot == BotStatus::Stopped).await;
        })
        .await;
        release.send(()).unwrap();
        assert!(stopped.is_ok());
    }

    #[tokio::test]
    async fn saving_missing_workspace_is_rejected_by_the_worker() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let mut invalid = draft("-100", "opus");
        invalid.workspace_root = home().join("missing-workspace").display().to_string();
        harness.send(Command::Save(Box::new(invalid))).await;
        let result = tokio::time::timeout(Duration::from_millis(300), async {
            harness
                .until("workspace error", |s| {
                    s.notice.as_ref().is_some_and(|n| n.contains("корень"))
                })
                .await;
        })
        .await;
        assert!(result.is_ok());
        assert!(harness.saved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn repeated_save_is_rejected_and_the_first_result_is_applied() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let (release, gate) = std::sync::mpsc::channel();
        *harness.save_gate.lock().unwrap() = Some(gate);
        harness.send(Command::Save(Box::new(draft("-100", "first")))).await;
        harness.until("saving", |s| s.save_status == SaveStatus::Saving).await;
        harness.send(Command::Save(Box::new(draft("-100", "second")))).await;
        harness
            .until("busy", |s| s.notice.as_ref().is_some_and(|n| n.contains("повтор недоступен")))
            .await;
        release.send(()).unwrap();
        harness
            .until("first saved", |s| s.saved.as_ref().is_some_and(|d| d.model == "first"))
            .await;
        assert_eq!(harness.saved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn start_after_stop_during_save_remains_starting_after_save_finishes() {
        let mut harness = harness(
            Ok(Loaded::Ready(settings("-100", ""))),
            &[Outcome::Connect, Outcome::PendingConnect, Outcome::PendingConnect],
            false,
        );
        harness.until("running", running).await;
        let (release, gate) = std::sync::mpsc::channel();
        *harness.save_gate.lock().unwrap() = Some(gate);
        harness.send(Command::Save(Box::new(draft("-100", "later")))).await;
        harness.until("saving", |s| s.save_status == SaveStatus::Saving).await;
        harness.send(Command::Stop).await;
        harness.until("stopped", |s| s.bot == BotStatus::Stopped).await;
        harness.send(Command::Start).await;
        harness.until("starting again", |s| s.bot == BotStatus::Starting).await;
        release.send(()).unwrap();
        harness.until("saved", |s| s.saved.as_ref().is_some_and(|d| d.model == "later")).await;
        assert_eq!(harness.snapshot.borrow().bot, BotStatus::Starting);
    }

    #[tokio::test]
    async fn stop_during_first_save_does_not_start_the_bot_later() {
        let mut harness = harness(Ok(Loaded::Missing), &[], false);
        let (release, gate) = std::sync::mpsc::channel();
        *harness.save_gate.lock().unwrap() = Some(gate);
        harness.send(Command::Save(Box::new(draft("-100", "")))).await;
        harness.until("saving", |s| s.save_status == SaveStatus::Saving).await;
        harness.send(Command::Stop).await;
        tokio::task::yield_now().await;
        release.send(()).unwrap();
        harness
            .until("saved and stopped", |s| s.saved.is_some() && s.bot == BotStatus::Stopped)
            .await;
        assert_eq!(harness.connects(), 0);
    }

    #[tokio::test]
    async fn quit_during_save_keeps_the_instance_lock_until_the_write_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("instance.lock");
        let lock = Arc::new(crate::instance::InstanceLock::acquire(&path).unwrap());
        let mut harness = harness_with_guard(
            Ok(Loaded::Ready(settings("-100", ""))),
            &[],
            false,
            Some(Arc::clone(&lock)),
        );
        harness.until("running", running).await;
        let (release, gate) = std::sync::mpsc::channel();
        *harness.save_gate.lock().unwrap() = Some(gate);
        harness.send(Command::Save(Box::new(draft("-100", "late")))).await;
        harness.until("saving", |s| s.save_status == SaveStatus::Saving).await;
        let (done, wait) = oneshot::channel();
        harness.send(Command::Quit(done)).await;
        tokio::time::timeout(Duration::from_secs(1), wait).await.unwrap().unwrap();
        drop(lock);
        assert!(matches!(
            crate::instance::InstanceLock::acquire(&path),
            Err(crate::instance::InstanceError::Running)
        ));
        release.send(()).unwrap();
        let acquired = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                match crate::instance::InstanceLock::acquire(&path) {
                    Ok(lock) => break lock,
                    Err(crate::instance::InstanceError::Running) => tokio::task::yield_now().await,
                    Err(error) => panic!("unexpected lock error: {error}"),
                }
            }
        })
        .await;
        assert!(acquired.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn slow_save_remains_in_progress_and_reports_delay() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let (release, gate) = std::sync::mpsc::channel();
        *harness.save_gate.lock().unwrap() = Some(gate);
        harness.send(Command::Save(Box::new(draft("-100", "slow")))).await;
        harness.until("saving", |s| s.save_status == SaveStatus::Saving).await;
        tokio::time::advance(Duration::from_secs(6)).await;
        harness.until("delayed", |s| s.save_status == SaveStatus::Delayed).await;
        assert!(harness.saved.lock().unwrap().is_empty());
        release.send(()).unwrap();
        harness.until("finished", |s| s.saved.as_ref().is_some_and(|d| d.model == "slow")).await;
    }

    #[tokio::test]
    async fn worker_panic_leaves_an_uncertain_result_and_blocks_another_write() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        harness.save_panic.store(true, Ordering::SeqCst);
        harness.send(Command::Save(Box::new(draft("-100", "first")))).await;
        harness.until("uncertain", |s| s.save_status == SaveStatus::Uncertain).await;
        harness.send(Command::Save(Box::new(draft("-100", "second")))).await;
        harness
            .until("repeat rejected", |s| {
                s.notice.as_ref().is_some_and(|n| n.contains("повтор недоступен"))
            })
            .await;
        assert_eq!(harness.save_started.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_save_preserves_draft_and_allows_retry_after_repair() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", "old"))), &[], true);
        harness.until("running", running).await;
        harness.send(Command::Save(Box::new(draft("-100", "new")))).await;
        harness
            .until("error", |s| {
                s.notice.as_ref().is_some_and(|n| n.contains("Часть ключей могла измениться"))
            })
            .await;
        assert_eq!(harness.snapshot.borrow().saved.as_ref().unwrap().model, "old");
        harness.save_broken.store(false, Ordering::SeqCst);
        harness.send(Command::Save(Box::new(draft("-100", "new")))).await;
        harness.until("retry saved", |s| s.saved.as_ref().is_some_and(|d| d.model == "new")).await;
        assert_eq!(harness.saved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn validation_error_allows_a_later_corrected_save() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let mut invalid = draft("-100", "bad");
        invalid.workspace_root = home().join("missing").display().to_string();
        harness.send(Command::Save(Box::new(invalid))).await;
        harness.until("validation error", |s| s.notice.is_some()).await;
        harness.send(Command::Save(Box::new(draft("-100", "corrected")))).await;
        harness
            .until("corrected saved", |s| s.saved.as_ref().is_some_and(|d| d.model == "corrected"))
            .await;
        assert_eq!(harness.saved.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn missing_settings_leave_the_bot_unconfigured() {
        let mut harness = harness(Ok(Loaded::Missing), &[], false);
        harness.until("first snapshot", |s| s.bot == BotStatus::Unconfigured).await;
        tokio::task::yield_now().await;
        assert_eq!(harness.connects(), 0);
    }

    #[tokio::test]
    async fn ready_settings_start_at_launch() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let snapshot = harness.snapshot.borrow().clone();
        assert_eq!(snapshot.bot, BotStatus::Running { username: "hub_bot".to_owned() });
        assert_eq!(
            snapshot.agents,
            [AgentStatus {
                kind: BackendKind::Claude,
                state: AgentState::Ready { version: "2.1.287".to_owned(), auth: None },
            }]
        );
        assert_eq!(snapshot.saved, Some(Draft::from_settings(&settings("-100", ""))));
    }

    #[tokio::test]
    async fn unseen_chat_is_reported_without_preventing_start_and_cleared_on_stop() {
        let mut harness =
            harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::Unseen], false);
        harness.until("running", running).await;
        assert_eq!(
            harness.snapshot.borrow().chat_access,
            Some(hub_telegram::telegram::ChatAccess::Unseen)
        );
        harness.send(Command::Stop).await;
        harness.until("stopped", |s| s.bot == BotStatus::Stopped).await;
        assert_eq!(harness.snapshot.borrow().chat_access, None);
    }

    #[tokio::test]
    async fn first_save_stores_and_starts_the_bot() {
        let mut harness = harness(Ok(Loaded::Missing), &[], false);
        harness.send(Command::Save(Box::new(draft("-100", "")))).await;
        harness.until("running", running).await;
        assert_eq!(harness.saved.lock().unwrap().as_slice(), [settings("-100", "")]);
        assert_eq!(harness.connects(), 1);
    }

    #[tokio::test]
    async fn telegram_change_restarts_a_running_bot() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        harness.send(Command::Save(Box::new(draft("-200", "")))).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness.connects() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        harness.until("running again", running).await;
        assert_eq!(harness.stops(), 1);
    }

    #[tokio::test]
    async fn other_changes_apply_without_restart() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        harness.send(Command::Save(Box::new(draft("-100", "opus")))).await;
        harness.until("saved", |s| s.saved.as_ref().is_some_and(|d| d.model == "opus")).await;
        let live = harness.probe.settings.lock().unwrap().clone().unwrap();
        assert_eq!(live.borrow().claude.model.as_deref(), Some("opus"));
        assert_eq!((harness.connects(), harness.stops()), (1, 0));
    }

    #[tokio::test]
    async fn failed_start_is_reported_and_retried_on_save() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::Fail], false);
        harness.until("failed", |s| matches!(s.bot, BotStatus::Failed(_))).await;
        assert_eq!(
            harness.snapshot.borrow().bot,
            BotStatus::Failed("Telegram не принял токен бота".to_owned())
        );
        harness.send(Command::Save(Box::new(draft("-100", "opus")))).await;
        harness.until("running", running).await;
        assert_eq!(harness.connects(), 2);
    }

    async fn wait_connects(harness: &Harness, count: usize) {
        tokio::time::timeout(Duration::from_hours(1), async {
            while harness.connects() < count {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn network_failures_are_retried_with_growing_pauses() {
        let outcomes = [Outcome::Offline, Outcome::Offline, Outcome::Connect];
        let started = tokio::time::Instant::now();
        let harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &outcomes, false);
        wait_connects(&harness, 3).await;
        assert!(started.elapsed() >= RETRY_FIRST * 3);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !running(&harness.snapshot.borrow()) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn network_failure_exposes_retry_reason_and_delay() {
        let mut harness =
            harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::Offline], false);
        harness.until("retry", |s| matches!(s.bot, BotStatus::Retrying { .. })).await;
        let state = harness.snapshot.borrow().bot.clone();
        assert!(matches!(state, BotStatus::Retrying { reason, at }
            if reason == "Telegram недоступен: сеть" && at == Instant::now() + RETRY_FIRST));
    }

    #[tokio::test(start_paused = true)]
    async fn rejected_settings_are_not_retried() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::Fail], false);
        harness.until("failed", |s| matches!(s.bot, BotStatus::Failed(_))).await;
        tokio::time::sleep(Duration::from_hours(1)).await;
        assert_eq!(harness.connects(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn stop_cancels_the_retry() {
        let mut harness =
            harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::Offline], false);
        harness.until("retry", |s| matches!(s.bot, BotStatus::Retrying { .. })).await;
        harness.send(Command::Stop).await;
        harness.until("stopped", |s| s.bot == BotStatus::Stopped).await;
        tokio::time::sleep(Duration::from_hours(1)).await;
        assert_eq!(harness.connects(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn unrelated_topic_command_does_not_cancel_reconnect() {
        let mut harness =
            harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::Offline], false);
        harness.until("retry", |s| matches!(s.bot, BotStatus::Retrying { .. })).await;
        harness
            .send(Command::StopTopic(TopicKey { chat: ChatId(-100), thread: ThreadId(7) }))
            .await;
        wait_connects(&harness, 2).await;
        harness.until("running", running).await;
    }

    #[tokio::test]
    async fn stopped_bot_stays_stopped_on_save() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        harness.send(Command::Stop).await;
        harness.until("stopped", |s| s.bot == BotStatus::Stopped).await;
        harness.send(Command::Save(Box::new(draft("-200", "")))).await;
        harness.until("saved", |s| s.saved.as_ref().is_some_and(|d| d.chat == "-200")).await;
        assert_eq!(harness.snapshot.borrow().bot, BotStatus::Stopped);
        assert_eq!(harness.connects(), 1);
        harness.send(Command::Start).await;
        harness.until("running", running).await;
        assert_eq!(harness.connects(), 2);
    }

    #[tokio::test]
    async fn store_failure_is_shown_and_nothing_starts() {
        let mut harness = harness(Ok(Loaded::Missing), &[], true);
        harness.send(Command::Save(Box::new(draft("-100", "")))).await;
        harness.until("notice", |s| s.notice.is_some()).await;
        assert!(harness.snapshot.borrow().notice.as_deref().unwrap().contains("диск полон"));
        assert_eq!(harness.snapshot.borrow().bot, BotStatus::Unconfigured);
        assert_eq!(harness.connects(), 0);
    }

    #[tokio::test]
    async fn load_failure_is_shown() {
        let error = StoreError::Corrupt {
            path: "settings.toml".into(),
            reason: "плохой TOML".into(),
        };
        let mut harness = harness(Err(error), &[], false);
        harness.until("notice", |s| s.notice.is_some()).await;
        assert_eq!(
            harness.snapshot.borrow().notice.as_deref(),
            Some("settings.toml повреждён: плохой TOML")
        );
    }

    #[tokio::test]
    async fn topic_commands_reach_the_hub() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        harness.send(Command::StopTopic(key)).await;
        harness.send(Command::ResetTopic(key)).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness.probe.messages.lock().unwrap().len() < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(*harness.probe.messages.lock().unwrap(), ["stop -100/7", "reset -100/7"]);
    }

    #[tokio::test]
    async fn views_are_published() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let view = TopicView {
            progress: hub_core::progress::TaskProgress::default(),
            parameters: String::new(),
            key: TopicKey { chat: ChatId(-100), thread: ThreadId(7) },
            title: Some("backend".to_owned()),
            session: hub_core::domain::TopicSession::fresh(
                hub_core::domain::BackendKind::Claude,
                hub_core::domain::AbsolutePath::new(home()).unwrap(),
            ),
            state: hub_telegram::hub::TopicState::Running,
        };
        harness.probe.views.lock().unwrap().as_ref().unwrap().send_replace(vec![view.clone()]);
        harness.until("topics", |s| s.topics == [view.clone()]).await;
    }

    #[tokio::test(start_paused = true)]
    async fn quit_finishes_even_if_the_bot_hangs() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::Hang], false);
        harness.until("running", running).await;
        let (done, stopped) = oneshot::channel();
        harness.send(Command::Quit(done)).await;
        tokio::time::timeout(STOP_TIMEOUT + Duration::from_secs(1), stopped)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(harness.stops(), 1);
    }

    #[tokio::test]
    async fn snapshot_debug_hides_the_token() {
        let mut harness = harness(Ok(Loaded::Missing), &[], false);
        harness.send(Command::Save(Box::new(draft("-100", "")))).await;
        harness.until("running", running).await;
        assert!(!format!("{:?}", *harness.snapshot.borrow()).contains("secret-token"));
    }

    #[tokio::test]
    async fn outbox_count_survives_stop_and_rejects_stale_group_results() {
        let mut harness = harness(Ok(Loaded::Missing), &[], false);
        let (old_send, old_read) = oneshot::channel();
        let (new_send, new_read) = oneshot::channel();
        harness.probe.counts.lock().unwrap().extend([old_read, new_read]);
        harness.send(Command::Save(Box::new(draft("-100", "")))).await;
        harness.until("running", running).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness.probe.counts.lock().unwrap().len() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        harness.send(Command::Stop).await;
        harness.until("stopped", |s| s.bot == BotStatus::Stopped).await;
        harness.send(Command::Save(Box::new(draft("-200", "")))).await;
        harness.until("new group", |s| s.saved.as_ref().is_some_and(|d| d.chat == "-200")).await;
        assert_eq!(harness.snapshot.borrow().outbox, OutboxStatus::Unknown);
        let _ = old_send.send(Ok(99));
        new_send.send(Ok(2)).unwrap();
        harness.until("new count", |s| s.outbox == OutboxStatus::Count(2)).await;
        assert_eq!(harness.snapshot.borrow().bot, BotStatus::Stopped);
    }

    #[tokio::test]
    async fn outbox_storage_errors_are_unknown_instead_of_zero() {
        let mut harness = harness(Ok(Loaded::Missing), &[], false);
        let (send, read) = oneshot::channel();
        harness.probe.counts.lock().unwrap().push_back(read);
        harness.send(Command::Save(Box::new(draft("-100", "")))).await;
        send.send(Err("broken".to_owned())).unwrap();
        harness.until("storage error", |s| s.outbox == OutboxStatus::Unavailable).await;
    }
}
