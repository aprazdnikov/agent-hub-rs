//! The core behind the window: owns the live settings and the bot, applies commands from the
//! window, and publishes what the window shows.

use hub_codex::protocol::CodexAuth;
use hub_core::domain::BackendKind;
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

use crate::config::{Loaded, SettingsStore};
use crate::error::StoreError;
use crate::logging::Repaint;

/// The hub itself gives up on sessions after 10 s; the rest covers the dispatcher.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(15);
/// First pause before reconnecting after a network failure; it doubles up to `RETRY_MAX`.
pub const RETRY_FIRST: Duration = Duration::from_secs(5);
pub const RETRY_MAX: Duration = Duration::from_mins(5);

pub enum Command {
    Save(Box<Draft>),
    Start,
    Stop,
    StopTopic(TopicKey),
    ResetTopic(TopicKey),
    Quit(oneshot::Sender<()>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotStatus {
    Unconfigured,
    Stopped,
    Starting,
    Running { username: String },
    Failed(String),
}

/// How an agent signs in to its model, where the hub knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAuth {
    Codex(CodexAuth),
    Qwen(QwenAuth),
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
            agents: Vec::new(),
            topics: Vec::new(),
            saved: None,
            notice: None,
        }
    }
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
    pub agents: Vec<AgentStatus>,
    pub mailbox: mpsc::Sender<HubMessage>,
    pub views: watch::Receiver<Vec<TopicView>>,
    pub stop: Stopper,
}

pub trait Connector: Send + 'static {
    type Error: std::fmt::Display + Send;

    /// Whether the start may succeed later on its own, e.g. the network is not up yet.
    fn transient(error: &Self::Error) -> bool;

    fn connect(
        &self,
        settings: watch::Receiver<Arc<Settings>>,
    ) -> BoxFuture<'_, Result<Started, Self::Error>>;
}

pub struct SupervisorSetup<C> {
    pub connector: C,
    pub store: Box<dyn SettingsStore>,
    pub home: PathBuf,
    pub loaded: Result<Loaded, StoreError>,
    pub snapshot: watch::Sender<Snapshot>,
    pub repaint: Repaint,
}

struct Bot {
    mailbox: mpsc::Sender<HubMessage>,
    stop: Stopper,
}

pub struct Supervisor<C> {
    connector: C,
    store: Box<dyn SettingsStore>,
    home: PathBuf,
    settings: Option<watch::Sender<Arc<Settings>>>,
    bot: Option<Bot>,
    views: Option<watch::Receiver<Vec<TopicView>>>,
    retry: Option<Retry>,
    state: Snapshot,
    snapshot: watch::Sender<Snapshot>,
    repaint: Repaint,
}

#[derive(Clone, Copy)]
struct Retry {
    at: Instant,
    pause: Duration,
}

enum Event {
    Command(Option<Command>),
    Views { alive: bool },
    Retry,
}

impl<C: Connector> Supervisor<C> {
    #[must_use]
    pub fn new(setup: SupervisorSetup<C>) -> Self {
        let SupervisorSetup { connector, store, home, loaded, snapshot, repaint } = setup;
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
            store,
            home,
            settings,
            bot: None,
            views: None,
            retry: None,
            state,
            snapshot,
            repaint,
        }
    }

    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>) {
        self.publish();
        if self.settings.is_some() {
            self.start().await;
        }
        loop {
            let mut views = self.views.take();
            let changed = async {
                match views.as_mut() {
                    Some(views) => views.changed().await.is_ok(),
                    None => std::future::pending().await,
                }
            };
            let retry = self.retry.map(|retry| retry.at);
            let event = tokio::select! {
                biased;
                command = commands.recv() => Event::Command(command),
                alive = changed => Event::Views { alive },
                () = sleep_until(retry.unwrap_or_else(Instant::now)), if retry.is_some() => Event::Retry,
            };
            self.views = views;
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
                    // Whatever the user does next supersedes a pending automatic retry.
                    self.retry = None;
                    self.handle(command).await;
                }
                Event::Retry => {
                    if self.bot.is_none() {
                        self.start().await;
                    }
                }
                Event::Views { alive: true } => self.refresh_topics(),
                Event::Views { alive: false } => {
                    tracing::error!("hub stopped on its own");
                    self.stop().await;
                    self.status(BotStatus::Failed(
                        "бот остановился, подробности в логе".to_owned(),
                    ));
                }
            }
        }
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::Save(draft) => self.save(*draft).await,
            Command::Start => {
                if self.bot.is_none() && self.settings.is_some() {
                    self.start().await;
                }
            }
            Command::Stop => {
                self.stop().await;
                if self.settings.is_some() {
                    self.status(BotStatus::Stopped);
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

    async fn save(&mut self, draft: Draft) {
        let settings = match draft.parse(&self.home) {
            Ok(settings) => settings,
            Err(errors) => {
                let fields: Vec<_> = errors.iter().map(|error| error.message.as_str()).collect();
                self.notice(Some(format!("Настройки не сохранены: {}", fields.join("; "))));
                return;
            }
        };
        if let Err(error) = self.store.save(&settings) {
            tracing::error!(%error, "settings could not be saved");
            self.notice(Some(error.to_string()));
            return;
        }
        tracing::info!("settings saved");
        self.state.saved = Some(draft);
        self.state.notice = None;
        let previous = self.settings.as_ref().map(|live| Arc::clone(&live.borrow()));
        let apply = plan(previous.as_deref(), &settings, self.activity());
        match &self.settings {
            Some(live) => {
                live.send_replace(Arc::new(settings));
            }
            None => self.settings = Some(watch::Sender::new(Arc::new(settings))),
        }
        self.publish();
        match apply {
            Apply::Start => self.start().await,
            Apply::Restart => {
                tracing::info!("telegram settings changed, restarting the bot");
                self.stop().await;
                self.start().await;
            }
            Apply::Live => {}
        }
    }

    fn activity(&self) -> Activity {
        match (&self.bot, &self.state.bot) {
            (Some(_), _) => Activity::Running,
            (None, BotStatus::Failed(_)) => Activity::Failed,
            (
                None,
                BotStatus::Unconfigured
                | BotStatus::Stopped
                | BotStatus::Starting
                | BotStatus::Running { .. },
            ) => Activity::Stopped,
        }
    }

    async fn start(&mut self) {
        let Some(live) = &self.settings else { return };
        let receiver = live.subscribe();
        self.status(BotStatus::Starting);
        match self.connector.connect(receiver).await {
            Ok(Started { username, agents, mailbox, views, stop }) => {
                tracing::info!(%username, "bot started");
                self.retry = None;
                self.bot = Some(Bot { mailbox, stop });
                self.views = Some(views);
                self.state.agents = agents;
                self.refresh_topics();
                self.status(BotStatus::Running { username });
            }
            Err(error) if C::transient(&error) => {
                let pause =
                    self.retry.map_or(RETRY_FIRST, |retry| (retry.pause * 2).min(RETRY_MAX));
                tracing::warn!(%error, retry_in = pause.as_secs(), "bot did not start, will retry");
                self.retry = Some(Retry { at: Instant::now() + pause, pause });
                self.status(BotStatus::Failed(format!(
                    "{error} — повтор через {} с",
                    pause.as_secs()
                )));
            }
            Err(error) => {
                tracing::warn!(%error, "bot did not start");
                self.retry = None;
                self.status(BotStatus::Failed(error.to_string()));
            }
        }
    }

    async fn stop(&mut self) {
        self.views = None;
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
        Fail,
        Offline,
        Hang,
    }

    #[derive(Default)]
    struct Probe {
        connects: AtomicUsize,
        stops: AtomicUsize,
        messages: StdMutex<Vec<String>>,
        settings: StdMutex<Option<watch::Receiver<Arc<Settings>>>>,
        views: StdMutex<Option<watch::Sender<Vec<TopicView>>>>,
    }

    struct FakeConnector {
        probe: Arc<Probe>,
        outcomes: StdMutex<VecDeque<Outcome>>,
    }

    impl Connector for FakeConnector {
        type Error = String;

        fn transient(error: &String) -> bool {
            error.starts_with("Telegram недоступен")
        }

        fn connect(
            &self,
            settings: watch::Receiver<Arc<Settings>>,
        ) -> BoxFuture<'_, Result<Started, String>> {
            self.probe.connects.fetch_add(1, Ordering::SeqCst);
            *self.probe.settings.lock().unwrap() = Some(settings);
            let outcome = self.outcomes.lock().unwrap().pop_front().unwrap_or(Outcome::Connect);
            let probe = Arc::clone(&self.probe);
            Box::pin(async move {
                match outcome {
                    Outcome::Fail => return Err("Telegram не принял токен бота".to_owned()),
                    Outcome::Offline => return Err("Telegram недоступен: сеть".to_owned()),
                    Outcome::Connect | Outcome::Hang => {}
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
                            HubMessage::Inbound(_)
                            | HubMessage::Press(_)
                            | HubMessage::Deliver { .. }
                            | HubMessage::FlushAlbum(_)
                            | HubMessage::Bind { .. }
                            | HubMessage::Ended { .. }
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
        broken: bool,
    }

    impl SettingsStore for MemoryStore {
        fn save(&self, settings: &Settings) -> Result<(), StoreError> {
            if self.broken {
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
        PathBuf::from(if cfg!(windows) { r"C:\Users\me" } else { "/home/me" })
    }

    fn settings(chat: &str, model: &str) -> Settings {
        draft(chat, model).parse(&home()).unwrap()
    }

    struct Harness {
        commands: mpsc::Sender<Command>,
        snapshot: watch::Receiver<Snapshot>,
        probe: Arc<Probe>,
        saved: Arc<StdMutex<Vec<Settings>>>,
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
        let probe = Arc::new(Probe::default());
        let store = MemoryStore { broken, ..MemoryStore::default() };
        let saved = Arc::clone(&store.saved);
        let (snapshot, watched) = watch::channel(Snapshot::default());
        let (commands, inbox) = mpsc::channel(8);
        let supervisor = Supervisor::new(SupervisorSetup {
            connector: FakeConnector {
                probe: Arc::clone(&probe),
                outcomes: StdMutex::new(outcomes.iter().copied().collect()),
            },
            store: Box::new(store),
            home: home(),
            loaded,
            snapshot,
            repaint: Arc::new(|| {}),
        });
        tokio::spawn(supervisor.run(inbox));
        Harness { commands, snapshot: watched, probe, saved }
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
            home: home(),
            loaded,
            snapshot,
            repaint: Arc::new(|| {}),
        });
        assert_eq!(watched.borrow().bot, expected);
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
        harness.until("failed", |s| matches!(s.bot, BotStatus::Failed(_))).await;
        harness.send(Command::Stop).await;
        harness.until("stopped", |s| s.bot == BotStatus::Stopped).await;
        tokio::time::sleep(Duration::from_hours(1)).await;
        assert_eq!(harness.connects(), 1);
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
}
