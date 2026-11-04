//! The hub actor: one task owns every topic's state and handles messages one at a time
//! without waiting on the network; sessions run in their own tasks and report back.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use futures::future::BoxFuture;
use hub_agent::archive::SessionArchive;
use hub_agent::conversation::Conversation;
use hub_core::commands::{
    BackendDecision, Command, Input, NewSessionArgs, RestoreArgs, SaveArgs, decide_backend,
    join_path_args, parse_input, parse_new_args, parse_restore_args, parse_save_args,
};
use hub_core::domain::{
    AbsolutePath, BackendKind, ChatId, MessageId, Prompt, SessionId, TopicKey, TopicSession, UserId,
};
use hub_core::options::Effective;
use hub_core::progress::TaskProgress;
use hub_core::projects::RecentProjects;
use hub_core::quota::{QuotaReport, QuotaUpdate};
use hub_core::render::render_limits;
use hub_core::settings::Settings;
use hub_core::snapshot::{SnapshotName, default_name};
use hub_core::topics::{Titles, Topics};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::actions::{self, ActionRunner, Actions};
use crate::channel::{Shared, lock};
use crate::doctor::{self, DoctorRunner, Doctors};
use crate::download::download;
use crate::inbound::{Content, Inbound, Press, Turn};
use crate::limits::{self, Outcome, QuotaCache};
use crate::messenger::{Format, Messenger, Outgoing, Target};
use crate::options;
use crate::outbox::{self, Outbox};
use crate::paths::{CwdError, resolve_cwd, workspace_root};
use crate::press;
use crate::progress::Progress;
use crate::projects::{self, Projects, Stage};
use crate::results::{LastResults, ResultStore, SaveResult};
use crate::sender::Sender;
use crate::session::{First, Run};
use crate::snapshots::{SnapshotError, SnapshotStore};
use crate::texts;
use crate::transfer::{
    Done, Reservation, Restoration, RestoreJob, SaveJob, holders, local_minute, supervise, utc_now,
};

pub(crate) const INBOX: usize = 32;
const MAILBOX: usize = 256;
// Telegram delivers album parts as separate updates sharing a media group id.
const ALBUM_WAIT: Duration = Duration::from_secs(1);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

pub trait Agents: Send + Sync + 'static {
    fn run<'a>(
        &'a self,
        session: &'a TopicSession,
        prompt: Prompt,
        inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation,
        settings: Arc<Settings>,
    ) -> BoxFuture<'a, mpsc::Receiver<Prompt>>;

    /// The backend's session archive once its CLI is found and new enough; the error is the
    /// text shown to the user.
    fn archive<'a>(
        &'a self,
        backend: BackendKind,
        home: &'a Path,
    ) -> BoxFuture<'a, Result<Arc<dyn SessionArchive>, String>>;

    /// Subscription limits of every backend whose CLI is found, in `BackendKind::ALL` order;
    /// `Err` is a probe failure or timeout, which the hub may answer from its cache.
    fn quotas(&self) -> BoxFuture<'_, Vec<(BackendKind, Result<QuotaReport, String>)>>;
}

pub trait TopicStore: Send + 'static {
    fn save(&mut self, topics: &Topics) -> Result<(), String>;
    fn save_titles(&mut self, titles: &Titles) -> Result<(), String>;
    fn save_recent_projects(&mut self, recent: &RecentProjects) -> Result<(), String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicState {
    Waiting,
    Running,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicView {
    pub progress: TaskProgress,
    pub parameters: String,
    pub key: TopicKey,
    pub title: Option<String>,
    pub session: TopicSession,
    pub state: TopicState,
}

pub enum HubMessage {
    SaveResult(SaveResult),
    LastDone(TopicKey),
    ActionDone(actions::Done),
    DoctorDone(crate::doctor::Done),
    Inbound(Inbound),
    Press(Press),
    /// A prompt whose attachments finished downloading.
    Deliver {
        key: TopicKey,
        prompt: Prompt,
    },
    FlushAlbum(String),
    Bind {
        key: TopicKey,
        /// Backend events carry their generation; direct administrative bindings use None.
        generation: Option<u64>,
        session: SessionId,
    },
    /// A session task ended; `inbox` is gone only if the task crashed.
    Ended {
        key: TopicKey,
        generation: u64,
        inbox: Option<mpsc::Receiver<Prompt>>,
    },
    Stop(TopicKey),
    Reset(TopicKey),
    /// A `/save` or `/restore` reached a decision point or finished its file work.
    Snapshot(SnapshotDone),
    /// A live session reported subscription limits.
    Quota(QuotaUpdate),
    /// A `/limits` poll finished.
    Limits(LimitsDone),
    Shutdown(oneshot::Sender<()>),
}

/// A message of a snapshot job; only the hub reads it.
pub struct SnapshotDone(pub(crate) Done);

/// A finished `/limits` poll; only the hub makes and reads it.
pub struct LimitsDone(Polled);

struct Polled {
    chat: ChatId,
    /// The message to turn into the card; `None` when the placeholder was not delivered.
    message: Option<MessageId>,
    /// `None` when the poll crashed.
    outcomes: Option<Vec<Outcome>>,
}

pub struct HubSetup<A> {
    pub results: Arc<dyn ResultStore>,
    pub action_runner: Arc<dyn ActionRunner>,
    pub doctor_runner: Arc<dyn DoctorRunner>,
    pub agents: Arc<A>,
    pub messenger: Arc<dyn Messenger>,
    pub settings: watch::Receiver<Arc<Settings>>,
    pub topics: Topics,
    pub titles: Titles,
    pub recent_projects: RecentProjects,
    pub store: Box<dyn TopicStore>,
    pub home: PathBuf,
    /// The bot's username, to accept `/command@bot`.
    pub bot: String,
}

pub struct HubHandle {
    pub mailbox: mpsc::Sender<HubMessage>,
    pub views: watch::Receiver<Vec<TopicView>>,
    pub registries: Shared,
    pub task: JoinHandle<()>,
}

struct Live {
    progress: Progress,
    parameters: Effective,
    inbox: mpsc::Sender<Prompt>,
    cancel: CancellationToken,
    generation: u64,
}

struct Hub<A> {
    results: LastResults,
    outbox: Outbox,
    actions: Actions,
    doctors: Doctors,
    projects: Projects,
    options: options::Choices,
    agents: Arc<A>,
    sender: Sender,
    registries: Shared,
    settings: watch::Receiver<Arc<Settings>>,
    topics: Topics,
    store: Box<dyn TopicStore>,
    home: PathBuf,
    bot: String,
    running: HashMap<TopicKey, Live>,
    last_progress: HashMap<TopicKey, TaskProgress>,
    albums: HashMap<String, Vec<Inbound>>,
    titles: Titles,
    generation: u64,
    mailbox: mpsc::Sender<HubMessage>,
    views: watch::Sender<Vec<TopicView>>,
    shutdown: Option<(oneshot::Sender<()>, Instant)>,
    /// Topics with a `/save` or `/restore` in flight: no turn or session change starts there.
    snapshotting: HashSet<TopicKey>,
    /// Names being written now, so two topics cannot race for one name.
    naming: HashSet<SnapshotName>,
    /// Agent sessions a snapshot job works on, and the topic that asked: no second job and
    /// no turn of any topic bound to them until the hub has processed the result.
    reserved: HashMap<Reservation, TopicKey>,
    /// The last subscription limits live sessions reported, the card's fallback.
    quotas: QuotaCache,
    /// A `/limits` poll runs: each one starts up to two agent processes, so one at a time.
    polling: bool,
}

#[must_use]
pub fn spawn<A: Agents>(setup: HubSetup<A>) -> HubHandle {
    let HubSetup {
        results,
        agents,
        action_runner,
        doctor_runner,
        messenger,
        settings,
        topics,
        titles,
        recent_projects,
        store,
        home,
        bot,
    } = setup;
    let (mailbox, inbox) = mpsc::channel(MAILBOX);
    let (views, watched) = watch::channel(Vec::new());
    let registries = Shared::default();
    let hub = Hub {
        outbox: Outbox::new(results.clone(), Sender::new(messenger.clone()), settings.clone()),
        results: LastResults::new(results),
        actions: Actions::new(action_runner),
        doctors: Doctors::new(doctor_runner),
        projects: Projects::new(recent_projects),
        options: options::Choices::default(),
        agents,
        sender: Sender::new(messenger),
        registries: Arc::clone(&registries),
        settings,
        topics,
        store,
        home,
        bot,
        running: HashMap::new(),
        last_progress: HashMap::new(),
        albums: HashMap::new(),
        titles,
        generation: 0,
        mailbox: mailbox.clone(),
        views,
        shutdown: None,
        snapshotting: HashSet::new(),
        naming: HashSet::new(),
        reserved: HashMap::new(),
        quotas: QuotaCache::default(),
        polling: false,
    };
    let task = tokio::spawn(hub.run(inbox));
    HubHandle { mailbox, views: watched, registries, task }
}

impl<A: Agents> Hub<A> {
    async fn run(mut self, mut mailbox: mpsc::Receiver<HubMessage>) {
        self.publish();
        let mut refresh = tokio::time::interval(Duration::from_millis(250));
        refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let deadline = self.shutdown.as_ref().map(|(_, at)| *at);
            let message = tokio::select! {
                message = mailbox.recv() => message,
                _ = refresh.tick(), if !self.running.is_empty() => {
                    self.publish();
                    continue;
                }
                changed = self.settings.changed() => {
                    if changed.is_err() { break; }
                    self.publish();
                    continue;
                }
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => None,
            };
            let Some(message) = message else { break };
            self.handle(message);
            if self.shutdown.is_some() && self.running.is_empty() && self.actions.is_idle() {
                break;
            }
        }
        if let Some((done, _)) = self.shutdown.take() {
            // The caller may have given up waiting; there is nobody else to tell.
            let _ = done.send(());
        }
    }

    fn press(&mut self, pressed: Press) {
        if pressed.data.starts_with(options::PREFIX) {
            if self.shutdown.is_none() {
                self.option_press(pressed);
            }
        } else if pressed.data.starts_with(outbox::PREFIX) {
            if self.shutdown.is_none() {
                self.outbox.press(&pressed);
            }
        } else if pressed.data.starts_with(doctor::PREFIX) {
            if self.shutdown.is_none() {
                let settings = self.settings.borrow();
                if let Some(key) = self.doctors.press(
                    pressed,
                    &settings,
                    self.sender.clone(),
                    self.mailbox.clone(),
                ) {
                    self.actions.menu(&settings.actions, Target::Topic(key), self.sender.clone());
                }
            }
        } else if pressed.data.starts_with(projects::PREFIX) {
            if self.shutdown.is_none() {
                self.project_press(pressed);
            }
        } else if pressed.data.starts_with(actions::PREFIX) {
            if self.shutdown.is_none() {
                self.actions.press(
                    pressed,
                    &self.settings.borrow(),
                    self.sender.clone(),
                    self.mailbox.clone(),
                );
            }
        } else if pressed.data == limits::REFRESH {
            self.refresh(pressed);
        } else {
            tokio::spawn(press::handle(Arc::clone(&self.registries), self.sender.clone(), pressed));
        }
    }

    fn handle(&mut self, message: HubMessage) {
        match message {
            HubMessage::LastDone(key) => self.results.complete(key),
            HubMessage::SaveResult(saved) => self.save_result(saved),
            HubMessage::DoctorDone(done) => self.doctors.complete(
                &done,
                &self.settings.borrow(),
                &self.actions,
                self.sender.clone(),
            ),
            HubMessage::ActionDone(done) => self.actions.complete(done, self.sender.clone()),
            HubMessage::Inbound(inbound) => {
                if self.shutdown.is_none() {
                    self.inbound(inbound);
                }
            }
            HubMessage::Press(pressed) => self.press(pressed),
            HubMessage::Quota(update) => self.quotas.apply(update, Utc::now()),
            HubMessage::Limits(LimitsDone(polled)) => self.limits_done(polled),
            HubMessage::Deliver { key, prompt } => self.deliver(key, prompt),
            HubMessage::FlushAlbum(group) => {
                let parts = self.albums.remove(&group).unwrap_or_default();
                if let Some(key) = parts.first().and_then(Inbound::key) {
                    self.start_turn(key, Turn::from_parts(&parts));
                }
            }
            HubMessage::Bind { key, generation, session } => {
                if generation.is_none_or(|id| {
                    self.running.get(&key).is_some_and(|live| live.generation == id)
                }) {
                    self.bind(key, session);
                }
            }
            HubMessage::Ended { key, generation, inbox } => self.ended(key, generation, inbox),
            HubMessage::Stop(key) => {
                self.stop(key);
            }
            HubMessage::Reset(key) => {
                // Like `/reset`: not during a turn or a snapshot of this topic.
                if !self.running.contains_key(&key)
                    && !self.snapshotting.contains(&key)
                    && let Some(session) = self.topics.get(&key).cloned()
                {
                    self.put(key, session.with_session(None));
                }
            }
            HubMessage::Snapshot(SnapshotDone(done)) => self.snapshot_done(done),
            HubMessage::Shutdown(done) => {
                self.actions.shutdown();
                self.doctors.shutdown();
                self.results.shutdown();
                self.outbox.shutdown();
                self.options.shutdown();
                for live in self.running.values() {
                    live.progress.stopping();
                    live.cancel.cancel();
                }
                self.shutdown = Some((done, Instant::now() + SHUTDOWN_TIMEOUT));
            }
        }
        self.publish();
    }

    fn save_result(&self, saved: SaveResult) {
        let SaveResult { key, generation, pending, reply } = saved;
        if pending.result().key() == key
            && self
                .running
                .get(&key)
                .is_some_and(|live| live.generation == generation && !live.cancel.is_cancelled())
        {
            let saving = self.results.store.save(pending);
            let wake = self.outbox.wake();
            tokio::spawn(async move {
                let result = saving.await;
                wake.notify_one();
                let _ = reply.send(result);
            });
        } else {
            let _ = reply.send(Err("Результат прежнего запуска не сохранён".to_owned()));
        }
    }

    fn inbound(&mut self, inbound: Inbound) {
        let Some(key) = inbound.key() else {
            self.outside_topic(inbound);
            return;
        };
        if inbound.album.is_some() && matches!(inbound.content, Content::Media { .. }) {
            self.album(inbound);
            return;
        }
        match &inbound.content {
            Content::TopicCreated { name } => {
                self.rename(key, name.clone());
                match self.root() {
                    Ok(root) => {
                        let default = self.settings.borrow().default_backend;
                        let session = TopicSession::fresh(default, root);
                        self.put(key, session.clone());
                        self.say(Target::Topic(key), texts::describe(texts::NEW_SESSION, &session));
                    }
                    Err(error) => self.say(Target::Topic(key), texts::warning(&error)),
                }
            }
            Content::TopicRenamed { name } => self.rename(key, name.clone()),
            Content::Text(text) => match parse_input(text, &self.bot) {
                Input::Command { command: Command::Model, args } => {
                    self.option_command(key, inbound.user, options::Field::Model, &args);
                }
                Input::Command { command: Command::Mode, args } => {
                    self.option_command(key, inbound.user, options::Field::Mode, &args);
                }
                Input::Command { command: Command::Outbox, .. } => {
                    self.outbox.menu(key, inbound.user);
                }
                Input::Command { command: Command::Last, .. } => {
                    self.results.read(
                        key,
                        inbound.user,
                        self.settings.clone(),
                        self.sender.clone(),
                        self.mailbox.clone(),
                    );
                }
                Input::Command { command, args } => {
                    self.command(key, inbound.chat, inbound.message, command, &args);
                }
                Input::Unknown => {}
                Input::Text => self.text(key, text.clone()),
            },
            Content::Media { .. } => {
                self.start_turn(key, Turn::from_parts(std::slice::from_ref(&inbound)));
            }
            Content::Voice => self.say(Target::Topic(key), texts::VOICE_UNSUPPORTED.to_owned()),
            Content::Ignored => {}
        }
    }

    fn outside_topic(&mut self, inbound: Inbound) {
        let target = Target::Reply { chat: inbound.chat, message: inbound.message };
        match inbound.content {
            Content::Text(text) => match parse_input(&text, &self.bot) {
                Input::Command { command: Command::Help, .. } => {
                    let root = self.settings.borrow().workspace_root.clone();
                    self.say(target, texts::help(&root));
                }
                Input::Command { command: Command::Limits, .. } => {
                    self.limits(target, inbound.chat);
                }
                Input::Command { command: Command::Actions, .. } => {
                    self.actions.menu(&self.settings.borrow().actions, target, self.sender.clone());
                }
                Input::Command { .. } | Input::Text => {
                    self.say(target, texts::CREATE_TOPIC.to_owned());
                }
                Input::Unknown => {}
            },
            Content::Media { .. } | Content::Voice => {
                self.say(target, texts::CREATE_TOPIC.to_owned());
            }
            Content::TopicCreated { .. } | Content::TopicRenamed { .. } | Content::Ignored => {}
        }
    }

    fn album(&mut self, inbound: Inbound) {
        let Some(group) = inbound.album.clone() else { return };
        let parts = self.albums.entry(group.clone()).or_default();
        parts.push(inbound);
        if parts.len() == 1 {
            let mailbox = self.mailbox.clone();
            tokio::spawn(async move {
                tokio::time::sleep(ALBUM_WAIT).await;
                // The hub is gone only during shutdown, when the album no longer matters.
                let _ = mailbox.send(HubMessage::FlushAlbum(group)).await;
            });
        }
    }

    fn command(
        &mut self,
        key: TopicKey,
        chat: ChatId,
        message: MessageId,
        command: Command,
        args: &[&str],
    ) {
        match command {
            Command::Last | Command::Outbox | Command::Model | Command::Mode => {} // Handled with the live user's identity in inbound().
            Command::Projects => self.project_menu(key),
            Command::Doctor => {
                let card = self.doctors.menu(key, &self.settings.borrow());
                tokio::spawn(projects::send_card(self.sender.clone(), key, card));
            }
            Command::Actions => self.actions.menu(
                &self.settings.borrow().actions,
                Target::Topic(key),
                self.sender.clone(),
            ),
            Command::Help => {
                let root = self.settings.borrow().workspace_root.clone();
                self.say(Target::Reply { chat, message }, texts::help(&root));
            }
            Command::New => {
                if self.refuse_if_running(key) {
                    return;
                }
                let default = self.settings.borrow().default_backend;
                let NewSessionArgs { backend, cwd } = parse_new_args(args, default);
                match self.resolve(cwd.as_deref()) {
                    Ok(cwd) => {
                        let session = TopicSession::fresh(backend, cwd);
                        self.put(key, session.clone());
                        self.say(Target::Topic(key), texts::describe(texts::NEW_SESSION, &session));
                    }
                    Err(error) => self.say(Target::Topic(key), texts::warning(&error)),
                }
            }
            Command::Cwd => {
                if self.refuse_if_running(key) {
                    return;
                }
                let Some(raw) = join_path_args(args) else {
                    self.say(Target::Topic(key), texts::CWD_USAGE.to_owned());
                    return;
                };
                let resolved =
                    self.resolve(Some(&raw)).and_then(|cwd| Ok((self.session(key)?, cwd)));
                match resolved {
                    // Agent sessions are stored per project directory: a new cwd needs a new session.
                    Ok((current, cwd)) => {
                        let session = TopicSession::fresh(current.backend, cwd);
                        self.put(key, session.clone());
                        self.say(Target::Topic(key), texts::describe(texts::CWD_CHANGED, &session));
                    }
                    Err(error) => self.say(Target::Topic(key), texts::warning(&error)),
                }
            }
            Command::Reset => {
                if self.refuse_if_running(key) {
                    return;
                }
                match self.session(key) {
                    Ok(session) => {
                        let session = session.with_session(None);
                        self.put(key, session.clone());
                        self.say(
                            Target::Topic(key),
                            texts::describe(texts::CONTEXT_RESET, &session),
                        );
                    }
                    Err(error) => self.say(Target::Topic(key), texts::warning(&error)),
                }
            }
            Command::Backend => self.backend(key, args),
            Command::Stop => {
                if !self.stop(key) {
                    self.say(Target::Topic(key), texts::NOTHING_TO_STOP.to_owned());
                }
            }
            Command::Status => match self.topics.get(&key) {
                None => self.say(Target::Topic(key), texts::NO_SESSION.to_owned()),
                Some(session) => {
                    let state = if self.running.contains_key(&key) {
                        texts::RUNNING
                    } else {
                        texts::WAITING
                    };
                    self.say(
                        Target::Topic(key),
                        format!(
                            "{}\n{}\n{}",
                            texts::describe(state, session),
                            self.progress(key).describe(std::time::Instant::now()),
                            self.parameters(key, session)
                        ),
                    );
                }
            },
            Command::Save => self.save(key, args),
            Command::Saves => self.saves(key),
            Command::Restore => self.restore(key, args),
            Command::Limits => self.limits(Target::Topic(key), chat),
        }
    }

    fn backend(&mut self, key: TopicKey, args: &[&str]) {
        let current = match self.session(key) {
            Ok(current) => current,
            Err(error) => {
                self.say(Target::Topic(key), texts::warning(&error));
                return;
            }
        };
        match decide_backend(args, &current) {
            BackendDecision::Show(session) => {
                let text = texts::describe(texts::CURRENT_SESSION, &session);
                self.say(Target::Topic(key), text);
            }
            BackendDecision::Unknown(name) => {
                self.say(Target::Topic(key), texts::unknown_backend(&name));
            }
            BackendDecision::AlreadySelected(session) => {
                let text = texts::describe(texts::BACKEND_ALREADY, &session);
                self.say(Target::Topic(key), text);
            }
            BackendDecision::Switch(session) => {
                if self.refuse_if_running(key) {
                    return;
                }
                self.put(key, session.clone());
                let text = texts::describe(texts::BACKEND_SWITCHED, &session);
                self.say(Target::Topic(key), text);
            }
        }
    }

    fn text(&mut self, key: TopicKey, text: String) {
        // While the agent waits on a question, the next message in the topic is its answer.
        let pending = lock(&self.registries).questions.reply(key, &text);
        if let Some((responder, answer)) = pending
            && responder.send(answer).is_ok()
        {
            return;
        }
        self.start_turn(key, Turn::text(text));
    }

    fn start_turn(&mut self, key: TopicKey, turn: Turn) {
        self.projects.invalidate(key);
        self.options.invalidate(key);
        if !self.running.contains_key(&key) {
            self.launch(key, First::Turn(turn), None);
            return;
        }
        if !turn.has_attachments() {
            if let Some(prompt) = Prompt::new(turn.text, Vec::new()) {
                self.deliver(key, prompt);
            }
            return;
        }
        let Ok(session) = self.session(key) else { return };
        let (sender, mailbox) = (self.sender.clone(), self.mailbox.clone());
        tokio::spawn(async move {
            match download(&sender, &session.cwd, turn).await {
                Ok(prompt) => {
                    // The hub is gone only during shutdown.
                    let _ = mailbox.send(HubMessage::Deliver { key, prompt }).await;
                }
                Err(error) => sender.text(Target::Topic(key), &texts::warning(&error)).await,
            }
        });
    }

    fn deliver(&mut self, key: TopicKey, prompt: Prompt) {
        let Some(live) = self.running.get(&key) else {
            self.launch(key, First::Prompt(prompt), None);
            return;
        };
        match live.inbox.try_send(prompt) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.say(Target::Topic(key), texts::QUEUE_FULL.to_owned());
            }
            Err(mpsc::error::TrySendError::Closed(prompt)) => {
                self.running.remove(&key);
                self.launch(key, First::Prompt(prompt), None);
            }
        }
    }

    fn launch(
        &mut self,
        key: TopicKey,
        first: First,
        reuse: Option<(mpsc::Sender<Prompt>, mpsc::Receiver<Prompt>)>,
    ) {
        // Album flushes and finished downloads still arrive while the hub winds down.
        if self.shutdown.is_some() {
            tracing::info!(
                chat = key.chat.0,
                thread = key.thread.0,
                "prompt dropped: shutting down"
            );
            return;
        }
        if self.snapshotting.contains(&key) {
            self.say(Target::Topic(key), texts::SNAPSHOT_BUSY.to_owned());
            return;
        }
        if self.topics.get(&key).is_some_and(|session| self.is_reserved(session)) {
            self.say(Target::Topic(key), texts::SESSION_RESERVED.to_owned());
            return;
        }
        let session = match self.session(key) {
            Ok(session) => session,
            Err(error) => {
                self.say(Target::Topic(key), texts::warning(&error));
                return;
            }
        };
        self.options.invalidate(key);
        let mut settings = Settings::clone(&self.settings.borrow());
        let parameters = match session.preferences.resolve(session.backend, &settings) {
            Ok(parameters) => parameters,
            Err(error) => {
                self.say(Target::Topic(key), format!("⚠️ {error}"));
                return;
            }
        };
        parameters.apply(&mut settings);
        let settings = Arc::new(settings);
        let (inbox, receiver) = reuse.unwrap_or_else(|| mpsc::channel(INBOX));
        self.generation += 1;
        let cancel = CancellationToken::new();
        let progress = Progress::new();
        self.last_progress.remove(&key);
        self.running.insert(
            key,
            Live {
                progress: progress.clone(),
                parameters,
                inbox,
                cancel: cancel.clone(),
                generation: self.generation,
            },
        );
        let run = Run {
            progress,
            key,
            session,
            first,
            inbox: receiver,
            cancel,
            generation: self.generation,
            root: workspace_root(&settings.workspace_root),
            settings,
            sender: self.sender.clone(),
            registries: Arc::clone(&self.registries),
            agents: Arc::clone(&self.agents),
            home: self.home.clone(),
            mailbox: self.mailbox.clone(),
        };
        tokio::spawn(run.execute());
    }

    fn ended(&mut self, key: TopicKey, generation: u64, inbox: Option<mpsc::Receiver<Prompt>>) {
        let Some(live) = self.running.remove(&key) else { return };
        if live.generation != generation {
            self.running.insert(key, live);
            return;
        }
        live.progress.finish(live.cancel.is_cancelled());
        self.last_progress.insert(key, live.progress.snapshot());
        let Some(mut inbox) = inbox else { return };
        if self.shutdown.is_some() {
            return;
        }
        // A prompt that arrived while the session was closing starts the next one.
        if let Ok(prompt) = inbox.try_recv() {
            self.launch(key, First::Prompt(prompt), Some((live.inbox, inbox)));
        }
    }

    fn bind(&mut self, key: TopicKey, id: SessionId) {
        let Ok(session) = self.session(key) else { return };
        if session.session.as_ref() != Some(&id) {
            self.put(key, session.with_session(Some(id)));
        }
    }

    fn stop(&self, key: TopicKey) -> bool {
        if let Some(live) = self.running.get(&key) {
            live.progress.stopping();
            live.cancel.cancel();
            true
        } else {
            false
        }
    }

    fn progress(&self, key: TopicKey) -> TaskProgress {
        self.running.get(&key).map_or_else(
            || self.last_progress.get(&key).cloned().unwrap_or_default(),
            |live| live.progress.snapshot(),
        )
    }

    fn is_reserved(&self, session: &TopicSession) -> bool {
        session
            .session
            .as_ref()
            .is_some_and(|id| self.reserved.contains_key(&(session.backend, id.clone())))
    }

    /// Why a snapshot job may not take the agent session now: another job holds it, or a
    /// topic bound to it runs a turn and its history is being written.
    fn session_refusal(&self, reservation: &Reservation) -> Option<&'static str> {
        let (backend, id) = reservation;
        let running = self.topics.iter().any(|(topic, session)| {
            session.backend == *backend
                && session.session.as_ref() == Some(id)
                && self.running.contains_key(topic)
        });
        if self.reserved.contains_key(reservation) {
            Some(texts::SESSION_RESERVED)
        } else if running {
            Some(texts::SESSION_BUSY_ELSEWHERE)
        } else {
            None
        }
    }

    /// `/save` and `/restore` refuse during a turn and while another snapshot job runs here.
    fn refuse_snapshot(&self, key: TopicKey) -> bool {
        let text = if self.running.contains_key(&key) {
            texts::FINISH_TURN_FIRST
        } else if self.snapshotting.contains(&key) {
            texts::SNAPSHOT_BUSY
        } else {
            return false;
        };
        self.say(Target::Topic(key), text.to_owned());
        true
    }

    fn save(&mut self, key: TopicKey, args: &[&str]) {
        if self.refuse_snapshot(key) {
            return;
        }
        let requested = match parse_save_args(args) {
            Ok(requested) => requested,
            Err(error) => {
                self.say(Target::Topic(key), texts::warning(&error));
                return;
            }
        };
        let Some((session, id)) = self
            .topics
            .get(&key)
            .and_then(|session| Some((session.clone(), session.session.clone()?)))
        else {
            self.say(Target::Topic(key), texts::NOTHING_TO_SAVE.to_owned());
            return;
        };
        let reservation: Reservation = (session.backend, id.clone());
        if let Some(text) = self.session_refusal(&reservation) {
            self.say(Target::Topic(key), text.to_owned());
            return;
        }
        let title = self.titles.get(&key).cloned();
        let name = match requested {
            SaveArgs::Named(name) => name,
            SaveArgs::Default => {
                default_name(title.as_deref(), session.backend.name(), local_minute())
            }
        };
        if !self.naming.insert(name.clone()) {
            self.say(Target::Topic(key), texts::warning(&SnapshotError::Exists(name)));
            return;
        }
        self.snapshotting.insert(key);
        self.reserved.insert(reservation.clone(), key);
        let crashed = Done::Saved {
            key,
            name: name.clone(),
            reservation,
            outcome: Err(texts::INTERNAL_ERROR.to_owned()),
        };
        let job = SaveJob {
            key,
            name,
            title,
            session,
            id,
            created_at: utc_now(),
            store: SnapshotStore::new(&self.settings.borrow().snapshots_dir),
            home: self.home.clone(),
            agents: Arc::clone(&self.agents),
            mailbox: self.mailbox.clone(),
        };
        supervise(job.run(), self.mailbox.clone(), crashed);
    }

    fn saves(&self, key: TopicKey) {
        let store = SnapshotStore::new(&self.settings.borrow().snapshots_dir);
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let text = match tokio::task::spawn_blocking(move || store.list()).await {
                Ok(Ok(manifests)) => texts::snapshot_list(&manifests),
                Ok(Err(error)) => {
                    // The error names the snapshots directory: only its io kind is logged.
                    let kind = match &error {
                        SnapshotError::Io(io) => Some(io.kind()),
                        SnapshotError::Exists(_)
                        | SnapshotError::Missing(_)
                        | SnapshotError::Corrupt { .. }
                        | SnapshotError::Manifest(_)
                        | SnapshotError::Archive(_) => None,
                    };
                    tracing::warn!(error = ?kind, "snapshots not listed");
                    texts::warning(&error)
                }
                Err(error) => {
                    tracing::error!(panic = error.is_panic(), "snapshot list crashed");
                    texts::INTERNAL_ERROR.to_owned()
                }
            };
            sender.text(Target::Topic(key), &text).await;
        });
    }

    fn restore(&mut self, key: TopicKey, args: &[&str]) {
        if self.refuse_snapshot(key) {
            return;
        }
        let RestoreArgs { name, cwd } = match parse_restore_args(args) {
            Ok(Some(args)) => args,
            Ok(None) => {
                self.say(Target::Topic(key), texts::RESTORE_USAGE.to_owned());
                return;
            }
            Err(error) => {
                self.say(Target::Topic(key), texts::warning(&error));
                return;
            }
        };
        let resolved = self.root().and_then(|root| {
            let cwd =
                cwd.as_deref().map(|raw| resolve_cwd(&root, &self.home, Some(raw))).transpose()?;
            Ok((root, cwd))
        });
        let (root, cwd) = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                self.say(Target::Topic(key), texts::warning(&error));
                return;
            }
        };
        self.snapshotting.insert(key);
        let crashed = Done::Restored {
            key,
            reservation: None,
            outcome: Err(texts::INTERNAL_ERROR.to_owned()),
        };
        let job = RestoreJob {
            key,
            name,
            cwd,
            root,
            store: SnapshotStore::new(&self.settings.borrow().snapshots_dir),
            home: self.home.clone(),
            agents: Arc::clone(&self.agents),
            mailbox: self.mailbox.clone(),
        };
        supervise(job.run(), self.mailbox.clone(), crashed);
    }

    fn snapshot_done(&mut self, done: Done) {
        match done {
            Done::Saved { key, name, reservation, outcome } => {
                self.snapshotting.remove(&key);
                self.naming.remove(&name);
                self.reserved.remove(&reservation);
                let text = match outcome {
                    Ok(manifest) => texts::saved(&manifest),
                    Err(text) => text,
                };
                self.say(Target::Topic(key), text);
            }
            Done::Loaded { key, reservation, reply } => self.grant(key, reservation, reply),
            Done::Restored { key, reservation, outcome } => {
                self.snapshotting.remove(&key);
                match reservation {
                    Some(reservation) => {
                        self.reserved.remove(&reservation);
                    }
                    // Failed before the grant (nothing held) or crashed after it: one topic
                    // runs one job at a time, so whatever it holds belongs to this one.
                    None => self.reserved.retain(|_, holder| *holder != key),
                }
                match outcome {
                    Ok(restoration) => self.rebind(key, restoration),
                    Err(text) => self.say(Target::Topic(key), text),
                }
            }
        }
    }

    /// Phase two of `/restore` may touch the agent's files only with the session reserved and
    /// no topic bound to it running a turn.
    fn grant(&mut self, key: TopicKey, reservation: Reservation, reply: oneshot::Sender<bool>) {
        if self.shutdown.is_some() {
            // No agent files are touched while the bot stops; `/restore` is simply repeated.
            tracing::info!(
                chat = key.chat.0,
                thread = key.thread.0,
                "restore dropped: shutting down"
            );
            self.snapshotting.remove(&key);
            let _ = reply.send(false);
            return;
        }
        if let Some(text) = self.session_refusal(&reservation) {
            self.snapshotting.remove(&key);
            self.say(Target::Topic(key), text.to_owned());
            // The job only waits for this answer; if it is gone there is nothing to stop.
            let _ = reply.send(false);
            return;
        }
        // The hub handles one message at a time: nothing else sees the session in between.
        if reply.send(true).is_ok() {
            self.reserved.insert(reservation, key);
        } else {
            // The job vanished (runtime shutting down): it will not report back.
            self.snapshotting.remove(&key);
        }
    }

    /// One session, one topic: every other holder is reset and `key` bound, persisted as one
    /// save; only a successful save changes the hub's bindings and tells the topics.
    fn rebind(&mut self, key: TopicKey, restoration: Restoration) {
        let Restoration { name, session, imported } = restoration;
        let holders = holders(&self.topics, key, &session);
        let involved = || std::iter::once(&key).chain(&holders);
        let refusal = if involved().any(|topic| self.running.contains_key(topic)) {
            Some(texts::SESSION_BUSY_ELSEWHERE)
        } else if involved().any(|topic| self.snapshotting.contains(topic)) {
            Some(texts::SESSION_RESERVED)
        } else {
            None
        };
        if let Some(refusal) = refusal {
            let text = if imported {
                format!("{refusal}\n{}", texts::RESTORE_FILES_KEPT)
            } else {
                refusal.to_owned()
            };
            self.say(Target::Topic(key), text);
            return;
        }
        let next = {
            let mut next = self.topics.clone();
            for holder in &holders {
                if let Some(old) = next.get(holder).cloned() {
                    next.insert(*holder, old.with_session(None));
                }
            }
            next.insert(key, session.clone());
            next
        };
        // The store's error can name the topics file path: it is not logged.
        if self.store.save(&next).is_err() {
            tracing::error!(snapshot = name.as_str(), "restored bindings could not be saved");
            self.say(Target::Topic(key), texts::RESTORE_NOT_PERSISTED.to_owned());
            return;
        }
        self.topics = next;
        self.last_progress.remove(&key);
        self.projects.invalidate(key);
        self.options.invalidate(key);
        self.say(Target::Topic(key), texts::restored(&name, &session));
        for holder in holders {
            self.last_progress.remove(&holder);
            self.projects.invalidate(holder);
            self.options.invalidate(holder);
            self.say(Target::Topic(holder), texts::SESSION_MOVED.to_owned());
        }
    }

    /// The topic's session, binding a fresh one at the root on first use.
    fn session(&mut self, key: TopicKey) -> Result<TopicSession, CwdError> {
        if let Some(session) = self.topics.get(&key) {
            return Ok(session.clone());
        }
        let default = self.settings.borrow().default_backend;
        let session = TopicSession::fresh(default, self.root()?);
        self.put(key, session.clone());
        tracing::info!(chat = key.chat.0, thread = key.thread.0, "topic bound");
        Ok(session)
    }

    fn put(&mut self, key: TopicKey, session: TopicSession) {
        if !self.running.contains_key(&key) {
            self.last_progress.remove(&key);
        }
        self.projects.invalidate(key);
        self.options.invalidate(key);
        self.topics.insert(key, session);
        if let Err(error) = self.store.save(&self.topics) {
            tracing::error!(%error, "topics could not be saved");
        }
    }

    fn rename(&mut self, key: TopicKey, name: String) {
        self.titles.insert(key, name);
        if let Err(error) = self.store.save_titles(&self.titles) {
            tracing::error!(%error, "topic titles could not be saved");
            self.say(Target::Topic(key), "⚠️ Название темы обновлено в памяти, но не сохранено на диск. После перезапуска оно может потеряться.".to_owned());
        }
    }

    fn option_context(&self, key: TopicKey, user: UserId) -> Result<options::Context, String> {
        let settings = self.settings.borrow().clone();
        let current = self.topics.get(&key).cloned();
        let session = match &current {
            Some(current) => current.clone(),
            None => TopicSession::fresh(
                settings.default_backend,
                self.root().map_err(|error| error.to_string())?,
            ),
        };
        Ok(options::Context { key, user, current, session, settings })
    }
    fn option_refusal(&self, key: TopicKey, session: &TopicSession) -> Option<&'static str> {
        if self.running.contains_key(&key) {
            Some(texts::ALREADY_RUNNING)
        } else if self.snapshotting.contains(&key) || self.is_reserved(session) {
            Some(texts::SNAPSHOT_BUSY)
        } else {
            None
        }
    }
    fn option_command(
        &mut self,
        key: TopicKey,
        user: Option<UserId>,
        field: options::Field,
        args: &[&str],
    ) {
        let Some(user) = user else {
            return;
        };
        if !crate::telegram::allowed(&self.settings.borrow().telegram, Some(key.chat), Some(user))
            || !self.options.available(key)
        {
            return;
        }
        let message = match self.option_context(key, user) {
            Err(error) => Outgoing::plain(format!("⚠️ {error}")),
            Ok(context) => {
                if args.is_empty() {
                    let summary = self.parameters(key, &context.session);
                    self.options.menu(context, field, &summary)
                } else if let Some(refusal) = self.option_refusal(key, &context.session) {
                    Outgoing::plain(refusal.to_owned())
                } else {
                    self.options
                        .change(context, field, args)
                        .unwrap_or_else(|error| Outgoing::plain(format!("⚠️ {error}")))
                }
            }
        };
        self.options.emit(
            key,
            user,
            self.settings.clone(),
            self.sender.clone(),
            options::Reply { message: Some(message), ..options::Reply::default() },
        );
    }
    fn option_press(&mut self, press: Press) {
        let Some(key) = options::key(&press) else {
            return;
        };
        if !crate::telegram::allowed(&self.settings.borrow().telegram, press.chat, Some(press.user))
            || !self.options.available(key)
        {
            return;
        }
        let ticket = self.options.take(&press, &self.settings.borrow(), &self.topics);
        let mut reply =
            options::Reply { callback: Some(press.callback), ..options::Reply::default() };
        match ticket {
            Err(error) => reply.answer = Some(error.to_owned()),
            Ok(ticket) => {
                if let Some(refusal) = self.option_refusal(key, &ticket.context.session) {
                    reply.answer = Some(refusal.to_owned());
                } else {
                    let message = match ticket.action {
                        options::Action::Cancel => {
                            self.options.invalidate(key);
                            Outgoing::plain("Изменение отменено.".to_owned())
                        }
                        options::Action::Preview(preferences) => self
                            .options
                            .preview(ticket.context, preferences)
                            .unwrap_or_else(|error| Outgoing::plain(format!("⚠️ {error}"))),
                        options::Action::Apply(session) => {
                            let mut next = self.topics.clone();
                            next.insert(key, session.clone());
                            if self.store.save(&next).is_err() {
                                Outgoing::plain("⚠️ Настройки не сохранены на диск. Прежние параметры и контекст темы сохранены.".to_owned())
                            } else {
                                if self.topics.get(&key).and_then(|s| s.session.as_ref())
                                    != session.session.as_ref()
                                {
                                    self.last_progress.remove(&key);
                                }
                                self.topics = next;
                                self.options.invalidate(key);
                                self.projects.invalidate(key);
                                Outgoing::plain(format!(
                                    "✅ Настройки темы сохранены. {}\n{}",
                                    if session.session.is_some() {
                                        "История разговора сохранена."
                                    } else {
                                        "Следующий запуск создаст новую сессию."
                                    },
                                    self.parameters(key, &session)
                                ))
                            }
                        }
                    };
                    reply.message = Some(message);
                }
            }
        }
        self.options.emit(key, press.user, self.settings.clone(), self.sender.clone(), reply);
    }

    fn project_menu(&mut self, key: TopicKey) {
        if self.refuse_if_running(key) {
            return;
        }
        let message = self.projects.menu(key, &self.settings.borrow(), self.topics.get(&key));
        let sender = self.sender.clone();
        tokio::spawn(async move {
            projects::send_card(sender, key, message).await;
        });
    }

    fn project_press(&mut self, press: crate::inbound::Press) {
        let ticket = self.projects.take(&press, &self.settings.borrow(), &self.topics);
        let sender = self.sender.clone();
        let answer = ticket.as_ref().err().copied();
        tokio::spawn(async move {
            sender.answer(press.callback, answer).await;
        });
        let Ok(ticket) = ticket else { return };
        let key = ticket.key;
        if matches!(ticket.stage, Stage::Cancel) {
            self.projects.invalidate(key);
            self.options.invalidate(key);
            self.say(Target::Topic(key), "Выбор проекта отменён, контекст сохранён.".to_owned());
            return;
        }
        if self.refuse_if_running(key) {
            return;
        }
        let cwd = match self.resolve(Some(ticket.project.cwd())) {
            Ok(cwd) => cwd,
            Err(error) => {
                self.say(Target::Topic(key), texts::warning(&error));
                return;
            }
        };
        match &ticket.stage {
            Stage::Preview => {
                let message = self.projects.confirm(ticket, cwd, &self.settings.borrow());
                let sender = self.sender.clone();
                tokio::spawn(async move {
                    projects::send_card(sender, key, message).await;
                });
            }
            Stage::Apply(shown) => {
                if *shown != cwd {
                    self.say(Target::Topic(key), projects::STALE.to_owned());
                    return;
                }
                let mut session = TopicSession::fresh(ticket.project.backend(), cwd);
                session.preferences = ticket.project.preferences();
                let mut next = self.topics.clone();
                next.insert(key, session.clone());
                if let Err(error) = self.store.save(&next) {
                    tracing::error!(%error, "project selection could not be saved");
                    self.say(Target::Topic(key), "⚠️ Проект не выбран: не удалось сохранить привязку на диск. Контекст темы сохранён.".to_owned());
                    return;
                }
                self.topics = next;
                self.last_progress.remove(&key);
                self.projects.invalidate(key);
                self.options.invalidate(key);
                self.projects.recent.remember(ticket.project.id());
                if let Err(error) = self.store.save_recent_projects(&self.projects.recent) {
                    tracing::error!(%error, "recent projects could not be saved");
                    self.say(
                        Target::Topic(key),
                        "⚠️ Проект выбран, но список последних проектов не сохранён на диск."
                            .to_owned(),
                    );
                }
                self.say(
                    Target::Topic(key),
                    texts::describe(
                        &format!("📁 Выбран проект: {}", ticket.project.name()),
                        &session,
                    ),
                );
            }
            Stage::Cancel => {}
        }
    }

    fn root(&self) -> Result<AbsolutePath, CwdError> {
        workspace_root(&self.settings.borrow().workspace_root)
    }

    fn resolve(&self, raw: Option<&str>) -> Result<AbsolutePath, CwdError> {
        resolve_cwd(&self.root()?, &self.home, raw)
    }

    fn refuse_if_running(&self, key: TopicKey) -> bool {
        if self.snapshotting.contains(&key) {
            self.say(Target::Topic(key), texts::SNAPSHOT_BUSY.to_owned());
            return true;
        }
        let running = self.running.contains_key(&key);
        if running {
            self.say(Target::Topic(key), texts::ALREADY_RUNNING.to_owned());
        }
        running
    }

    /// `/limits`: a placeholder now, the card once every found CLI answered or timed out.
    fn limits(&mut self, target: Target, chat: ChatId) {
        if self.polling {
            self.say(target, texts::LIMITS_BUSY.to_owned());
            return;
        }
        self.polling = true;
        let sender = self.sender.clone();
        self.poll(chat, async move {
            sender.one(target, Outgoing::plain(texts::LIMITS_COLLECTING.to_owned())).await
        });
    }

    /// «Обновить» redraws its own message; a press while a poll runs, or while the hub stops,
    /// only stops the button's spinner.
    fn refresh(&mut self, pressed: Press) {
        let Press { callback, user: _, chat, message, html: _, data: _ } = pressed;
        let sender = self.sender.clone();
        tokio::spawn(async move { sender.answer(callback, None).await });
        let idle = !self.polling && self.shutdown.is_none();
        let (Some(chat), Some(message), true) = (chat, message, idle) else {
            return;
        };
        self.polling = true;
        let sender = self.sender.clone();
        self.poll(chat, async move {
            // Dropping the keyboard here also stops further presses on this card.
            let text = texts::LIMITS_COLLECTING.to_owned();
            sender.edit(chat, message, text, Format::Plain).await;
            Some(message)
        });
    }

    /// Runs the poll outside the hub; whatever happens, the hub hears back exactly once, so
    /// `polling` never stays set. That holds because only the quotas run in an inner task that
    /// may panic: the outer task awaits just the placeholder (`Sender::one` / `Sender::edit`,
    /// which are panic-free) and the mailbox send.
    fn poll(
        &self,
        chat: ChatId,
        placeholder: impl Future<Output = Option<MessageId>> + Send + 'static,
    ) {
        let agents = Arc::clone(&self.agents);
        let mailbox = self.mailbox.clone();
        tokio::spawn(async move {
            let message = placeholder.await;
            let outcomes = match message {
                None => None,
                Some(_) => match tokio::spawn(async move { agents.quotas().await }).await {
                    Ok(outcomes) => Some(outcomes),
                    Err(error) => {
                        tracing::error!(panic = error.is_panic(), "limits poll crashed");
                        None
                    }
                },
            };
            // The hub is gone only during shutdown, when the card no longer matters.
            let _ = mailbox
                .send(HubMessage::Limits(LimitsDone(Polled { chat, message, outcomes })))
                .await;
        });
    }

    fn limits_done(&mut self, polled: Polled) {
        self.polling = false;
        let Polled { chat, message, outcomes } = polled;
        let Some(message) = message else { return };
        let sender = self.sender.clone();
        let Some(outcomes) = outcomes else {
            let text = texts::INTERNAL_ERROR.to_owned();
            tokio::spawn(async move { sender.edit(chat, message, text, Format::Plain).await });
            return;
        };
        let now = Utc::now();
        let reports: Vec<QuotaReport> =
            outcomes.into_iter().map(|outcome| self.quotas.settle(outcome, now)).collect();
        tracing::info!(backends = reports.len(), "limits card");
        let default = self.settings.borrow().default_backend;
        let card = render_limits(&limits::ordered(reports, default), now, &chrono::Local);
        tokio::spawn(async move {
            sender.edit_html(chat, message, card).await;
            sender.edit_keyboard(chat, message, limits::refresh_keyboard()).await;
        });
    }

    fn say(&self, target: Target, text: String) {
        let sender = self.sender.clone();
        tokio::spawn(async move { sender.text(target, &text).await });
    }

    fn parameters(&self, key: TopicKey, session: &TopicSession) -> String {
        let next = session.preferences.resolve(session.backend, &self.settings.borrow());
        let mut description = match (self.running.get(&key), next) {
            (Some(live), Ok(next)) if live.parameters != next => format!(
                "Параметры текущего запуска:\n{}\n\nСледующий запуск:\n{}",
                live.parameters.describe(),
                next.describe()
            ),
            (Some(live), _) => {
                format!("Параметры текущего запуска:\n{}", live.parameters.describe())
            }
            (None, Ok(next)) => format!("Параметры следующего запуска:\n{}", next.describe()),
            (None, Err(error)) => format!("⚠️ {error}"),
        };
        if session.backend == BackendKind::Qwen && session.session.is_some() {
            description.push_str("\nQwen: показаны запрошенные параметры. Загруженная сессия может сохранить собственные модель и режим; /reset начнёт новую сессию.");
        }
        description
    }
    fn publish(&self) {
        let views = self
            .topics
            .iter()
            .map(|(key, session)| TopicView {
                progress: self.progress(*key),
                parameters: self.parameters(*key, session),
                key: *key,
                title: self.titles.get(key).cloned(),
                session: session.clone(),
                state: if self.running.contains_key(key) {
                    TopicState::Running
                } else {
                    TopicState::Waiting
                },
            })
            .collect();
        self.views.send_if_modified(|current| {
            if *current == views {
                false
            } else {
                *current = views;
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashSet, VecDeque};
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    use hub_agent::archive::{ArchiveError, Exported};
    use hub_core::domain::{
        AgentEvent, BackendKind, ChatId, Finished, Question, Selection, ThreadId, Usage, UserId,
    };
    use hub_core::questions::{Answer, QuestionId};
    use hub_core::quota::{Label, Percent, QuotaUpdate, UsageWindow, WindowKind};
    use hub_core::settings::Draft;
    use hub_core::snapshot::{BackendData, RelPath};

    use super::*;
    use crate::inbound::{Attachment, FileRef};
    use crate::limits::{self, Outcome};
    use crate::messenger::SendError;
    use crate::testing::{Call, FakeMessenger};

    const KEY: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };

    enum Script {
        Say(Vec<AgentEvent>),
        UntilStopped,
        Collect(usize),
        Stream(mpsc::Receiver<AgentEvent>),
        Approval,
        Crash,
    }

    #[derive(Default)]
    struct FakeAgents {
        scripts: StdMutex<VecDeque<Script>>,
        prompts: StdMutex<Vec<String>>,
        parameters: StdMutex<Vec<Arc<Settings>>>,
        archive: Arc<FakeArchive>,
        /// The next `archive` lookup panics, as a bug in a backend would.
        crash_archive: AtomicBool,
        /// Outcomes of the next polls, in order; afterwards a poll finds no CLI.
        quotas: StdMutex<VecDeque<Vec<Outcome>>>,
        /// The next poll waits until this is released.
        hold_quotas: StdMutex<Option<oneshot::Receiver<()>>>,
        /// The next poll panics, as a bug in a probe would.
        crash_quotas: AtomicBool,
        polls: AtomicUsize,
    }

    impl FakeAgents {
        fn scripted(scripts: Vec<Script>) -> Self {
            Self { scripts: StdMutex::new(scripts.into()), ..Self::default() }
        }

        fn prompts(&self) -> Vec<String> {
            self.prompts.lock().unwrap().clone()
        }
    }

    impl Agents for FakeAgents {
        fn quotas(&self) -> BoxFuture<'_, Vec<(BackendKind, Result<QuotaReport, String>)>> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            let outcomes = self.quotas.lock().unwrap().pop_front().unwrap_or_default();
            let hold = self.hold_quotas.lock().unwrap().take();
            let crash = self.crash_quotas.swap(false, Ordering::SeqCst);
            Box::pin(async move {
                if let Some(release) = hold {
                    let _ = release.await;
                }
                assert!(!crash, "опрос упал");
                outcomes
            })
        }

        fn archive<'a>(
            &'a self,
            _backend: BackendKind,
            _home: &'a Path,
        ) -> BoxFuture<'a, Result<Arc<dyn SessionArchive>, String>> {
            let archive: Arc<dyn SessionArchive> =
                Arc::clone(&self.archive) as Arc<dyn SessionArchive>;
            let crash = self.crash_archive.swap(false, Ordering::SeqCst);
            Box::pin(async move {
                assert!(!crash, "архив упал");
                Ok(archive)
            })
        }

        fn run<'a>(
            &'a self,
            _session: &'a TopicSession,
            prompt: Prompt,
            mut inbox: mpsc::Receiver<Prompt>,
            conversation: Conversation,
            settings: Arc<Settings>,
        ) -> BoxFuture<'a, mpsc::Receiver<Prompt>> {
            self.parameters.lock().unwrap().push(settings);
            self.prompts.lock().unwrap().push(prompt.text().to_owned());
            let script =
                self.scripts.lock().unwrap().pop_front().unwrap_or(Script::Say(Vec::new()));
            Box::pin(async move {
                match script {
                    Script::Crash => panic!("test session crash"),
                    Script::Approval => {
                        let requested =
                            conversation.channel.request(hub_core::domain::ToolRequest {
                                tool: "test".to_owned(),
                                summary: "test".to_owned(),
                            });
                        tokio::select! {
                            () = conversation.cancel.cancelled() => {},
                            _ = requested => { conversation.events.send(finished()).await.unwrap(); }
                        }
                    }
                    Script::Stream(mut source) => loop {
                        tokio::select! {
                            () = conversation.cancel.cancelled() => break,
                            next = source.recv() => match next {
                                Some(event) => { if conversation.events.send(event).await.is_err() { break; } }
                                None => break,
                            }
                        }
                    },
                    Script::Say(events) => {
                        for event in events {
                            conversation.events.send(event).await.unwrap();
                        }
                    }
                    Script::UntilStopped => conversation.cancel.cancelled().await,
                    Script::Collect(count) => {
                        for _ in 0..count {
                            let next = inbox.recv().await.unwrap();
                            self.prompts.lock().unwrap().push(next.text().to_owned());
                        }
                    }
                }
                inbox
            })
        }
    }

    #[derive(Default, Clone)]
    struct MemoryStore {
        saved: Arc<StdMutex<Vec<Topics>>>,
        titles: Arc<StdMutex<Titles>>,
        recent: Arc<StdMutex<RecentProjects>>,
        fail_recent: Arc<AtomicBool>,
        fail: Arc<AtomicBool>,
    }

    impl MemoryStore {
        /// What a restart would load: the last bindings that reached the "disk".
        fn last(&self) -> Option<Topics> {
            self.saved.lock().unwrap().last().cloned()
        }
    }

    impl TopicStore for MemoryStore {
        fn save_recent_projects(&mut self, recent: &RecentProjects) -> Result<(), String> {
            if self.fail.load(Ordering::SeqCst) || self.fail_recent.load(Ordering::SeqCst) {
                return Err("диск полон".to_owned());
            }
            *self.recent.lock().unwrap() = recent.clone();
            Ok(())
        }

        fn save_titles(&mut self, titles: &Titles) -> Result<(), String> {
            if self.fail.load(Ordering::SeqCst) {
                return Err("диск полон".to_owned());
            }
            *self.titles.lock().unwrap() = titles.clone();
            Ok(())
        }

        fn save(&mut self, topics: &Topics) -> Result<(), String> {
            if self.fail.load(Ordering::SeqCst) {
                return Err("диск полон".to_owned());
            }
            self.saved.lock().unwrap().push(topics.clone());
            Ok(())
        }
    }

    type Gate = StdMutex<Option<std::sync::mpsc::Receiver<()>>>;

    /// Remembers which sessions the "agent" has; `hold_*` pause the next export or import
    /// until released, `exporting`/`importing` tell the test it got there.
    #[derive(Default)]
    struct FakeArchive {
        known: StdMutex<HashSet<String>>,
        imports: StdMutex<Vec<String>>,
        fail_import: AtomicBool,
        hold_export: Gate,
        hold_import: Gate,
        exporting: AtomicBool,
        importing: AtomicBool,
    }

    impl FakeArchive {
        fn forget(&self) {
            self.known.lock().unwrap().clear();
        }

        fn imports(&self) -> Vec<String> {
            self.imports.lock().unwrap().clone()
        }
    }

    fn pause(gate: &Gate, reached: &AtomicBool) {
        reached.store(true, Ordering::SeqCst);
        let held = gate.lock().unwrap().take();
        if let Some(release) = held {
            release.recv().unwrap();
        }
    }

    impl SessionArchive for FakeArchive {
        fn export(
            &self,
            session: &SessionId,
            _cwd: &AbsolutePath,
            dest: &Path,
        ) -> Result<Exported, ArchiveError> {
            pause(&self.hold_export, &self.exporting);
            let file = RelPath::parse(&format!("fake/{}.txt", session.as_str())).unwrap();
            std::fs::create_dir_all(dest.join("fake"))?;
            std::fs::write(file.under(dest), "history")?;
            self.known.lock().unwrap().insert(session.as_str().to_owned());
            Ok(Exported {
                files: vec![file],
                data: BackendData::Claude { project: RelPath::parse("p").unwrap() },
                agent_version: "1.0.0".to_owned(),
            })
        }

        fn present(
            &self,
            session: &SessionId,
            _cwd: &AbsolutePath,
            _data: &BackendData,
        ) -> Result<bool, ArchiveError> {
            Ok(self.known.lock().unwrap().contains(session.as_str()))
        }

        fn import(
            &self,
            session: &SessionId,
            _data: &BackendData,
            _files: &[RelPath],
            _src: &Path,
            _cwd: &AbsolutePath,
        ) -> Result<(), ArchiveError> {
            pause(&self.hold_import, &self.importing);
            self.imports.lock().unwrap().push(session.as_str().to_owned());
            if self.fail_import.load(Ordering::SeqCst) {
                return Err(ArchiveError::Malformed("сбой".to_owned()));
            }
            self.known.lock().unwrap().insert(session.as_str().to_owned());
            Ok(())
        }
    }

    struct World {
        handle: HubHandle,
        messenger: Arc<FakeMessenger>,
        agents: Arc<FakeAgents>,
        store: MemoryStore,
        results: Arc<crate::testing::MemoryResults>,
        settings: watch::Sender<Arc<Settings>>,
        root: tempfile::TempDir,
    }

    impl World {
        async fn restart(&mut self) {
            let (done, stopped) = oneshot::channel();
            self.send(HubMessage::Shutdown(done)).await;
            stopped.await.unwrap();
            (&mut self.handle.task).await.unwrap();
            self.handle = spawn(HubSetup {
                results: self.results.clone(),
                action_runner: Arc::new(crate::testing::FakeActionRunner),
                doctor_runner: Arc::new(crate::testing::FakeDoctorRunner),
                agents: Arc::clone(&self.agents),
                messenger: self.messenger.clone(),
                settings: self.settings.subscribe(),
                topics: self.store.last().unwrap_or_default(),
                titles: self.store.titles.lock().unwrap().clone(),
                recent_projects: self.store.recent.lock().unwrap().clone(),
                store: Box::new(self.store.clone()),
                home: std::env::temp_dir(),
                bot: "agent_hub_bot".to_owned(),
            });
        }

        fn texts(&self) -> Vec<String> {
            self.messenger.sent_texts()
        }

        fn running(&self) -> bool {
            self.handle.views.borrow().iter().any(|view| view.state == TopicState::Running)
        }

        async fn send(&self, message: HubMessage) {
            self.handle.mailbox.send(message).await.unwrap();
        }
    }

    fn world(agents: FakeAgents) -> World {
        world_with(agents, FakeMessenger::default())
    }

    fn world_with(agents: FakeAgents, messenger: FakeMessenger) -> World {
        world_from(agents, messenger, BackendKind::Claude)
    }

    fn world_from(
        agents: FakeAgents,
        messenger: FakeMessenger,
        default_backend: BackendKind,
    ) -> World {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("project")).unwrap();
        let settings = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.path().display().to_string(),
            snapshots_dir: root.path().join("store").display().to_string(),
            approval_timeout: "5".to_owned(),
            default_backend,
            ..Draft::default()
        }
        .parse(&std::env::temp_dir())
        .unwrap();
        let (settings_tx, settings) = watch::channel(Arc::new(settings));
        let messenger = Arc::new(messenger);
        let agents = Arc::new(agents);
        let store = MemoryStore::default();
        let results = Arc::new(crate::testing::MemoryResults::default());
        let handle = spawn(HubSetup {
            results: results.clone(),
            action_runner: Arc::new(crate::testing::FakeActionRunner),
            doctor_runner: Arc::new(crate::testing::FakeDoctorRunner),
            agents: Arc::clone(&agents),
            messenger: Arc::clone(&messenger) as Arc<dyn Messenger>,
            settings,
            topics: Topics::new(),
            titles: Titles::new(),
            recent_projects: RecentProjects::default(),
            store: Box::new(store.clone()),
            home: std::env::temp_dir(),
            bot: "agent_hub_bot".to_owned(),
        });
        World { handle, messenger, agents, store, results, settings: settings_tx, root }
    }

    fn inbound(thread: Option<i32>, message: i32, content: Content) -> HubMessage {
        HubMessage::Inbound(Inbound {
            chat: ChatId(-100),
            thread: thread.map(ThreadId),
            user: Some(UserId(1)),
            message: MessageId(message),
            album: None,
            content,
        })
    }

    fn text(thread: Option<i32>, message: i32, text: &str) -> HubMessage {
        inbound(thread, message, Content::Text(text.to_owned()))
    }

    async fn eventually(what: &str, check: impl Fn() -> bool) {
        for _ in 0..400 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    fn finished() -> AgentEvent {
        AgentEvent::Finished(Finished {
            session: SessionId::parse("s-1").unwrap(),
            usage: Usage::Claude { turns: 1, cost: None },
            background: 0,
        })
    }

    #[tokio::test]
    async fn actions_work_in_general_and_topic_without_starting_an_agent() {
        let world = world(FakeAgents::default());
        let mut settings = (**world.settings.borrow()).clone();
        settings.actions = hub_core::actions::parse(
            &[hub_core::actions::ActionDraft {
                id: "vpn".to_owned(),
                name: "Рабочий VPN".to_owned(),
                command: "echo test".to_owned(),
                cwd: String::new(),
            }],
            world.root.path(),
        )
        .unwrap();
        world.settings.send_replace(Arc::new(settings));
        world.send(text(None, 1, "/actions")).await;
        eventually("general actions", || world.messenger.last_keyboard().is_some()).await;
        let data = world
            .messenger
            .last_keyboard()
            .unwrap()
            .1
            .first()
            .unwrap()
            .first()
            .unwrap()
            .data
            .clone();
        world
            .send(HubMessage::Press(Press {
                callback: "start-action".to_owned(),
                user: UserId(1),
                chat: Some(ChatId(-100)),
                message: Some(MessageId(1)),
                html: None,
                data,
            }))
            .await;
        eventually("started action", || {
            world.texts().iter().any(|text| text.contains("запуск команды"))
        })
        .await;
        world.send(text(Some(7), 2, "/actions")).await;
        eventually("topic action can stop", || {
            world.messenger.last_keyboard().is_some_and(|(_, rows)| {
                rows.iter().flatten().any(|button| button.text.starts_with('⏹'))
            })
        })
        .await;
        assert!(world.agents.prompts().is_empty());
        let (done, stopped) = oneshot::channel();
        world.send(HubMessage::Shutdown(done)).await;
        tokio::time::timeout(Duration::from_secs(2), stopped).await.unwrap().unwrap();
        world.handle.task.await.unwrap();
    }

    #[tokio::test]
    async fn new_topic_binds_a_session_at_the_root() {
        let world = world(FakeAgents::default());
        world.send(inbound(Some(7), 1, Content::TopicCreated { name: "backend".to_owned() })).await;

        eventually("greeting", || world.texts().iter().any(|t| t.starts_with("🆕 Новая сессия")))
            .await;
        assert_eq!(world.store.last().map(|topics| topics.len()), Some(1));
        eventually("titled view", || {
            world
                .handle
                .views
                .borrow()
                .first()
                .is_some_and(|v| v.title.as_deref() == Some("backend"))
        })
        .await;
    }

    fn configure_project(world: &World) {
        let mut draft = Draft::from_settings(&world.settings.borrow());
        draft.projects = vec![hub_core::projects::ProjectDraft {
            options: hub_core::options::OptionsDraft::default(),
            id: "backend".to_owned(),
            name: "Бэкенд".to_owned(),
            cwd: "project".to_owned(),
            backend: "codex".to_owned(),
            actions: Vec::new(),
            checks: Vec::new(),
        }];
        world.settings.send(Arc::new(draft.parse(&std::env::temp_dir()).unwrap())).unwrap();
    }

    async fn project_button(world: &World, label: &str) -> crate::inbound::Press {
        eventually("project keyboard", || {
            world.messenger.last_keyboard().is_some_and(|(_, rows)| {
                rows.iter().flatten().any(|button| button.text.contains(label))
            })
        })
        .await;
        let (message, rows) = world.messenger.last_keyboard().unwrap();
        let button = rows.into_iter().flatten().find(|button| button.text.contains(label)).unwrap();
        crate::inbound::Press {
            callback: uuid::Uuid::new_v4().to_string(),
            user: UserId(1),
            chat: Some(KEY.chat),
            message: Some(message),
            html: None,
            data: button.data,
        }
    }

    async fn choose_project(world: &World) -> crate::inbound::Press {
        world.send(text(Some(7), 1, "/projects")).await;
        let pick = project_button(world, "Бэкенд").await;
        world.send(HubMessage::Press(pick)).await;
        project_button(world, "Выбрать и сбросить").await
    }

    #[tokio::test]
    async fn progress_follows_background_and_next_turn_until_the_process_ends() {
        use hub_core::progress::Phase;
        let (events, received) = mpsc::channel(8);
        let world = world(FakeAgents::scripted(vec![Script::Stream(received)]));
        world.send(text(Some(7), 1, "work")).await;
        eventually("executing", || phase(&world) == Some(Phase::Running)).await;
        let mut completed = finished();
        if let AgentEvent::Finished(result) = &mut completed {
            result.background = 1;
        }
        events.send(completed).await.unwrap();
        eventually("background", || phase(&world) == Some(Phase::Background)).await;
        assert!(world.running());
        world.send(text(Some(7), 2, "/status")).await;
        eventually("status", || {
            world.texts().iter().any(|t| {
                t.contains("Фоновые задачи")
                    && t.contains("Длительность запуска")
                    && t.contains("Получен результат хода")
            })
        })
        .await;
        events.send(AgentEvent::TurnStarted).await.unwrap();
        eventually("next turn before output", || phase(&world) == Some(Phase::Running)).await;
        events.send(finished()).await.unwrap();
        eventually("finishing", || phase(&world) == Some(Phase::Finishing)).await;
        assert!(world.running());
        drop(events);
        eventually("complete", || !world.running() && phase(&world) == Some(Phase::Completed))
            .await;
    }

    fn phase(world: &World) -> Option<hub_core::progress::Phase> {
        world.handle.views.borrow().iter().find(|v| v.key == KEY).map(|v| v.progress.phase())
    }

    #[tokio::test]
    async fn progress_waiting_topic_is_busy_and_stop_clears_waiting() {
        use hub_core::progress::Phase;
        let world = world(FakeAgents::scripted(vec![Script::Approval]));
        world.send(text(Some(7), 1, "work")).await;
        eventually("approval", || phase(&world) == Some(Phase::AwaitingApproval)).await;
        assert!(world.running());
        world.send(text(Some(7), 2, "/status")).await;
        eventually("waiting status", || {
            world.texts().iter().any(|t| t.contains("Ожидает разрешения"))
        })
        .await;
        world.send(text(Some(7), 3, "/stop")).await;
        eventually("stopped", || !world.running() && phase(&world) == Some(Phase::Stopped)).await;
    }

    #[tokio::test]
    async fn progress_crash_and_preparation_failure_are_errors_and_reset_clears_them() {
        use hub_core::progress::Phase;
        let world = world(FakeAgents::scripted(vec![Script::Crash]));
        world.send(text(Some(7), 1, "crash")).await;
        eventually("crash", || !world.running() && phase(&world) == Some(Phase::Failed)).await;
        assert!(world.store.last().unwrap()[&KEY].session.is_none());
        world.send(text(Some(7), 2, "/reset")).await;
        eventually("clear without session id", || phase(&world) == Some(Phase::Unknown)).await;
        world.settings.send_modify(|settings| {
            Arc::make_mut(settings).workspace_root = world.root.path().join("missing");
        });
        world.send(text(Some(7), 3, "bad root")).await;
        eventually("preparation failed", || {
            phase(&world) == Some(Phase::Failed) && !world.running()
        })
        .await;
        assert_eq!(world.agents.prompts(), ["crash"]);
    }

    #[tokio::test]
    async fn progress_rejects_late_ended_and_binding_from_an_old_generation() {
        use hub_core::progress::Phase;
        let world =
            world(FakeAgents::scripted(vec![Script::Say(vec![finished()]), Script::UntilStopped]));
        world.send(text(Some(7), 1, "first")).await;
        eventually("first completed", || {
            phase(&world) == Some(Phase::Completed) && !world.running()
        })
        .await;
        world.send(text(Some(7), 2, "second")).await;
        eventually("second running", || phase(&world) == Some(Phase::Running)).await;
        world
            .send(HubMessage::Bind {
                key: KEY,
                generation: Some(1),
                session: SessionId::parse("stale").unwrap(),
            })
            .await;
        world.send(HubMessage::Ended { key: KEY, generation: 1, inbox: None }).await;
        world.send(text(Some(7), 3, "/status")).await;
        eventually("barrier", || world.texts().iter().any(|t| t.starts_with(texts::RUNNING))).await;
        assert_eq!(bound_to(&world, KEY), SessionId::parse("s-1"));
        assert_eq!(phase(&world), Some(Phase::Running));
        world.send(text(Some(7), 4, "/stop")).await;
        eventually("stopped", || !world.running()).await;
    }

    #[tokio::test]
    async fn progress_survives_rename_and_model_change_but_not_reset_or_restart() {
        use hub_core::progress::Phase;
        let mut world = world(FakeAgents::scripted(vec![Script::Say(vec![finished()])]));
        world.send(text(Some(7), 1, "first")).await;
        eventually("completed", || phase(&world) == Some(Phase::Completed) && !world.running())
            .await;
        world.send(inbound(Some(7), 2, Content::TopicRenamed { name: "Renamed".to_owned() })).await;
        apply_option(&world, "/model another").await;
        assert_eq!(phase(&world), Some(Phase::Completed));
        world.restart().await;
        eventually("restart clears", || phase(&world) == Some(Phase::Unknown)).await;
        assert_eq!(bound_to(&world, KEY), SessionId::parse("s-1"));
    }

    #[tokio::test]
    async fn progress_project_write_failure_preserves_the_previous_result() {
        use hub_core::progress::Phase;
        let world = world(FakeAgents::scripted(vec![Script::Say(vec![finished()])]));
        configure_project(&world);
        world.send(text(Some(7), 1, "first")).await;
        eventually("completed", || phase(&world) == Some(Phase::Completed) && !world.running())
            .await;
        let confirm = choose_project(&world).await;
        world.store.fail.store(true, Ordering::SeqCst);
        world.send(HubMessage::Press(confirm)).await;
        eventually("write failed", || said(&world, "⚠️ Проект не выбран")).await;
        assert_eq!(phase(&world), Some(Phase::Completed));
        world.store.fail.store(false, Ordering::SeqCst);
        let confirm = choose_project(&world).await;
        world.send(HubMessage::Press(confirm)).await;
        eventually("new context", || phase(&world) == Some(Phase::Unknown)).await;
    }

    #[tokio::test]
    async fn options_views_do_not_create_a_binding_or_start_a_model() {
        let world = world(FakeAgents::default());
        world.send(text(Some(7), 1, "/model")).await;
        eventually("model help", || said(&world, "⚙️ claude")).await;
        world.send(text(Some(7), 2, "/mode")).await;
        project_button(&world, "acceptEdits").await;
        assert!(world.store.last().is_none());
        assert!(world.handle.views.borrow().is_empty());
        assert!(world.agents.prompts().is_empty());
    }

    async fn option_preview(world: &World, command: &str) -> crate::inbound::Press {
        let previous = world.messenger.last_keyboard().map(|(id, _)| id);
        world.send(text(Some(7), 1, command)).await;
        eventually("new option preview", || {
            world.messenger.last_keyboard().is_some_and(|(id, rows)| {
                Some(id) != previous
                    && rows.iter().flatten().any(|b| b.text.starts_with("Применить"))
            })
        })
        .await;
        project_button(world, "Применить").await
    }

    async fn apply_option(world: &World, command: &str) {
        let before = world.texts().iter().filter(|t| t.starts_with("✅ Настройки темы")).count();
        let button = option_preview(world, command).await;
        world.send(HubMessage::Press(button)).await;
        eventually("options saved", || {
            world.texts().iter().filter(|t| t.starts_with("✅ Настройки темы")).count() > before
        })
        .await;
    }

    #[tokio::test]
    async fn options_preserve_history_across_restart_reset_and_snapshot_restore() {
        let mut world = world(FakeAgents::default());
        bound(&world, KEY, "old-session").await;
        let preview = option_preview(&world, "/model topic-model").await;
        assert_eq!(bound_to(&world, KEY), SessionId::parse("old-session"));
        assert!(world.store.last().unwrap()[&KEY].preferences.to_draft().is_none());
        world.send(HubMessage::Press(preview)).await;
        eventually("saved", || said(&world, "✅ Настройки темы")).await;
        let selected = world.store.last().unwrap()[&KEY].clone();
        assert_eq!(selected.session, SessionId::parse("old-session"));
        world.restart().await;
        eventually("restored preferences", || {
            world.handle.views.borrow().iter().any(|v| v.session == selected)
        })
        .await;
        world.send(text(Some(7), 2, "/save options")).await;
        eventually("snapshot", || said(&world, "💾 Сохранено: options")).await;
        world.send(text(Some(7), 3, "/reset")).await;
        eventually("reset", || bound_to(&world, KEY).is_none()).await;
        assert_eq!(world.store.last().unwrap()[&KEY].preferences, selected.preferences);
        world.send(text(Some(8), 4, "/restore options")).await;
        eventually("restore", || said(&world, "♻️ Восстановлено: options")).await;
        assert_eq!(world.store.last().unwrap()[&OTHER].preferences, selected.preferences);
        assert_eq!(bound_to(&world, OTHER), selected.session);
        world.send(text(Some(7), 5, "/new")).await;
        eventually("new clears preferences", || {
            world.store.last().unwrap()[&KEY].preferences.to_draft().is_none()
        })
        .await;
        assert!(world.agents.prompts().is_empty());
    }

    #[tokio::test]
    async fn options_qwen_cancel_and_disk_failure_keep_history_until_explicit_apply() {
        let world = world_from(FakeAgents::default(), FakeMessenger::default(), BackendKind::Qwen);
        bound(&world, KEY, "old-qwen").await;
        let before = world.store.last().unwrap();
        let preview = option_preview(&world, "/model qwen-model").await;
        assert!(world.texts().iter().any(|t| t.contains("контекст будет сброшен")));
        let cancel = project_button(&world, "Отмена").await;
        world.send(HubMessage::Press(cancel)).await;
        eventually("cancel", || said(&world, "Изменение отменено")).await;
        assert_eq!(world.store.last().unwrap(), before);
        world.send(HubMessage::Press(preview)).await;
        eventually("cancel invalidates apply", || {
            world
                .messenger
                .calls()
                .iter()
                .any(|c| matches!(c, Call::Answer {text: Some(t), ..} if t == options::STALE))
        })
        .await;
        let preview = option_preview(&world, "/model qwen-model").await;
        world.store.fail.store(true, Ordering::SeqCst);
        world.send(HubMessage::Press(preview)).await;
        eventually("disk error", || said(&world, "⚠️ Настройки не сохранены")).await;
        assert_eq!(world.store.last().unwrap(), before);
        assert_eq!(world.handle.views.borrow()[0].session, before[&KEY]);
        world.store.fail.store(false, Ordering::SeqCst);
        apply_option(&world, "/model qwen-model").await;
        assert!(bound_to(&world, KEY).is_none());
        assert_eq!(
            world.store.last().unwrap()[&KEY].preferences.overrides.to_draft().model,
            "qwen-model"
        );
    }

    #[tokio::test]
    async fn options_start_invalidates_preview_and_busy_topic_rejects_changes() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        bound(&world, KEY, "kept").await;
        let before = world.store.last().unwrap();
        let preview = option_preview(&world, "/model later").await;
        world.send(text(Some(7), 2, "work")).await;
        eventually("running", || world.running()).await;
        world.send(HubMessage::Press(preview)).await;
        eventually("stale", || {
            world
                .messenger
                .calls()
                .iter()
                .any(|c| matches!(c, Call::Answer {text: Some(t), ..} if t == options::STALE))
        })
        .await;
        world.send(text(Some(7), 3, "/mode plan")).await;
        eventually("busy", || world.texts().contains(&texts::ALREADY_RUNNING.to_owned())).await;
        assert_eq!(world.store.last().unwrap(), before);
        world.send(text(Some(7), 4, "/stop")).await;
        eventually("stopped", || !world.running()).await;
    }

    #[tokio::test]
    async fn options_freeze_launch_settings_and_display_the_next_launch_separately() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped, Script::Say(vec![])]));
        world.settings.send_modify(|settings| {
            Arc::make_mut(settings).claude.model = Some("first".to_owned());
        });
        world.send(text(Some(7), 1, "work")).await;
        eventually("started", || world.agents.parameters.lock().unwrap().len() == 1).await;
        world.settings.send_modify(|settings| {
            Arc::make_mut(settings).claude.model = Some("second".to_owned());
        });
        eventually("current and next parameters", || {
            world.handle.views.borrow().iter().any(|v| {
                v.parameters.contains("first")
                    && v.parameters.contains("second")
                    && v.parameters.contains("Следующий запуск")
            })
        })
        .await;
        assert_eq!(
            world.agents.parameters.lock().unwrap()[0].claude.model.as_deref(),
            Some("first")
        );
        world.send(text(Some(7), 2, "/stop")).await;
        eventually("stopped", || !world.running()).await;
        world.send(text(Some(7), 3, "next")).await;
        eventually("second launch", || world.agents.parameters.lock().unwrap().len() == 2).await;
        assert_eq!(
            world.agents.parameters.lock().unwrap()[1].claude.model.as_deref(),
            Some("second")
        );
    }

    #[tokio::test]
    async fn options_launch_resolves_each_field_from_topic_project_and_global() {
        let world = world(FakeAgents::default());
        configure_project(&world);
        let mut draft = Draft::from_settings(&world.settings.borrow());
        draft.projects[0].options.model = "project-model".to_owned();
        draft.projects[0].options.mode = "read-only/on-request".to_owned();
        world.settings.send(Arc::new(draft.parse(&std::env::temp_dir()).unwrap())).unwrap();
        let confirm = choose_project(&world).await;
        world.send(HubMessage::Press(confirm)).await;
        eventually("project", || {
            world.store.last().is_some_and(|topics| topics[&KEY].backend == BackendKind::Codex)
        })
        .await;
        apply_option(&world, "/model topic-model").await;
        world.send(text(Some(7), 2, "topic")).await;
        eventually("first launch", || {
            world.agents.parameters.lock().unwrap().len() == 1 && !world.running()
        })
        .await;
        {
            let parameters = world.agents.parameters.lock().unwrap();
            assert_eq!(parameters[0].codex.model.as_deref(), Some("topic-model"));
            assert_eq!(parameters[0].codex.sandbox, hub_core::settings::Sandbox::ReadOnly);
        }
        // Removing the saved profile does not change the copy already selected in the topic.
        let mut draft = Draft::from_settings(&world.settings.borrow());
        draft.projects.clear();
        world.settings.send(Arc::new(draft.parse(&std::env::temp_dir()).unwrap())).unwrap();
        apply_option(&world, "/model inherit").await;
        apply_option(&world, "/mode workspace-write never").await;
        world.send(text(Some(7), 3, "project")).await;
        eventually("second launch", || {
            world.agents.parameters.lock().unwrap().len() == 2 && !world.running()
        })
        .await;
        {
            let parameters = world.agents.parameters.lock().unwrap();
            assert_eq!(parameters[1].codex.model.as_deref(), Some("project-model"));
            assert_eq!(parameters[1].codex.sandbox, hub_core::settings::Sandbox::WorkspaceWrite);
            assert_eq!(parameters[1].codex.approval, hub_core::settings::Approval::Never);
        }
    }

    #[tokio::test]
    async fn outbox_command_does_not_start_a_model_or_bind_a_session() {
        let world = world(FakeAgents::default());
        world.send(text(Some(7), 1, "/outbox")).await;
        eventually("empty outbox", || {
            world.texts().iter().any(|text| text.contains("Ожидающих ответов в этой теме нет"))
        })
        .await;
        assert!(world.agents.prompts().is_empty());
        assert!(world.store.last().is_none());
    }
    #[tokio::test]
    async fn outbox_restart_delivers_remaining_parts_without_a_prompt_or_last_command() {
        use hub_core::outbox::{Action, Change, Pending};
        let mut world = world(FakeAgents::default());
        // Let the initial empty pump settle, then emulate a persisted partial delivery.
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        let record = hub_core::results::SavedResult::new(
            KEY,
            "c".repeat(32),
            chrono::Utc::now().timestamp(),
            &TopicSession::fresh(
                BackendKind::Codex,
                AbsolutePath::new(world.root.path().to_path_buf()).unwrap(),
            )
            .with_session(SessionId::parse("saved-session")),
            "answer ".repeat(700),
        )
        .unwrap();
        let pending = Pending::new(record, "finished").unwrap();
        let first = pending.chunks()[0].text.clone();
        let second = pending.chunks()[1].text.clone();
        world.results.save(pending.clone()).await.unwrap();
        let claimed =
            world.results.update(Change::new(&pending, Action::Claim)).await.unwrap().unwrap();
        world.results.update(Change::new(&claimed, Action::Ack)).await.unwrap();
        world.restart().await;
        eventually("remaining part delivered automatically", || world.texts().contains(&second))
            .await;
        assert!(!world.texts().contains(&first));
        assert!(world.agents.prompts().is_empty());
        assert!(world.store.last().is_none());
        assert!(world.results.pending(KEY.chat).await.unwrap().entries.is_empty());
    }
    #[tokio::test]
    async fn last_result_survives_failed_delivery_and_hub_restart() {
        let mut world = world_with(
            FakeAgents::scripted(vec![Script::Say(vec![
                AgentEvent::AssistantText("**saved answer**".to_owned()),
                finished(),
            ])]),
            FakeMessenger::with_sends(vec![Err(crate::messenger::SendError::Failed(
                "offline".to_owned(),
            ))]),
        );
        world.send(text(Some(7), 1, "task")).await;
        eventually("saved last answer", || {
            world.results.records.lock().unwrap().contains_key(&KEY)
        })
        .await;
        eventually("turn ended", || !world.running()).await;
        world.restart().await;
        world.send(text(Some(7), 2, "/last")).await;
        eventually("recovered answer", || {
            world.texts().iter().any(|t| t.contains("Последний сохранённый ответ"))
        })
        .await;
        eventually("recovered markdown", || {
            world.texts().iter().any(|t| t.contains("<b>saved answer</b>"))
        })
        .await;
        assert_eq!(world.agents.prompts(), ["task"]);
    }

    #[tokio::test]
    async fn last_result_is_saved_while_tool_message_delivery_is_stalled() {
        let world = world(FakeAgents::scripted(vec![Script::Say(vec![
            AgentEvent::ToolCall(hub_core::domain::ToolUse {
                tool: "test".to_owned(),
                summary: "working".to_owned(),
            }),
            AgentEvent::AssistantText("ready despite network".to_owned()),
            finished(),
        ])]));
        let release = world.messenger.delay_next_send();
        world.send(text(Some(7), 1, "task")).await;
        eventually("pending network send", || !world.messenger.send_attempts().is_empty()).await;
        eventually("saved before network release", || {
            world.results.records.lock().unwrap().contains_key(&KEY)
        })
        .await;
        assert!(!world.texts().iter().any(|text| text.contains("working")));
        release.send(()).unwrap();
        eventually("final delivered", || {
            world.texts().iter().any(|t| t.contains("ready despite network"))
        })
        .await;
    }

    #[tokio::test]
    async fn last_result_keeps_the_final_group_and_survives_errors_empty_turns_and_reset() {
        let world = world(FakeAgents::scripted(vec![Script::Say(vec![
            AgentEvent::AssistantText("progress".to_owned()),
            AgentEvent::ToolCall(hub_core::domain::ToolUse {
                tool: "test".to_owned(),
                summary: String::new(),
            }),
            AgentEvent::AssistantText("first".to_owned()),
            AgentEvent::AssistantText("second".to_owned()),
            finished(),
            AgentEvent::AssistantText("incomplete".to_owned()),
            AgentEvent::Failed("failed".to_owned()),
            finished(),
        ])]));
        world.send(text(Some(7), 1, "task")).await;
        eventually("success saved", || world.results.records.lock().unwrap().contains_key(&KEY))
            .await;
        eventually("failure shown", || world.texts().iter().any(|t| t.contains("failed"))).await;
        eventually("idle", || !world.running()).await;
        assert_eq!(
            world.results.records.lock().unwrap().get(&KEY).unwrap().text(),
            "first\n\nsecond"
        );
        world.send(text(Some(7), 2, "/reset")).await;
        world.send(text(Some(7), 3, "/last")).await;
        eventually("last after reset", || {
            world.texts().iter().any(|t| t.contains("Последний сохранённый ответ"))
        })
        .await;
        assert_eq!(world.agents.prompts(), ["task"]);
    }

    #[tokio::test]
    async fn last_result_disk_failure_is_visible_and_does_not_suppress_the_answer() {
        let world = world(FakeAgents::scripted(vec![Script::Say(vec![
            AgentEvent::AssistantText("answer".to_owned()),
            finished(),
        ])]));
        world.results.fail.store(true, Ordering::SeqCst);
        world.send(text(Some(7), 1, "task")).await;
        eventually("save failure", || {
            world.texts().iter().any(|t| t.contains("не сохранён для /last: диск полон"))
        })
        .await;
        eventually("answer delivered", || world.texts().iter().any(|t| t.contains("\nanswer\n")))
            .await;
        assert!(world.results.records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn last_result_rejects_stale_generation_and_unauthorized_reads() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.send(text(Some(7), 1, "task")).await;
        eventually("running", || world.running()).await;
        let record = hub_core::results::SavedResult::new(
            KEY,
            "b".repeat(32),
            1,
            &TopicSession::fresh(
                BackendKind::Claude,
                AbsolutePath::new(world.root.path().to_path_buf()).unwrap(),
            )
            .with_session(SessionId::parse("s")),
            "wrong".to_owned(),
        )
        .unwrap();
        let (reply, rejected) = oneshot::channel();
        world
            .send(HubMessage::SaveResult(SaveResult {
                key: KEY,
                generation: 0,
                pending: hub_core::outbox::Pending::new(record, "").unwrap(),
                reply,
            }))
            .await;
        assert!(rejected.await.unwrap().is_err());
        assert!(world.results.records.lock().unwrap().is_empty());
        let HubMessage::Inbound(mut incoming) = text(Some(7), 2, "/last") else {
            panic!("inbound")
        };
        incoming.user = Some(UserId(999));
        world.send(HubMessage::Inbound(incoming)).await;
        world.send(text(Some(7), 3, "/status")).await;
        eventually("status barrier", || !world.texts().is_empty()).await;
        assert!(!world.texts().iter().any(|t| t.contains("Сохранённого ответа")));
        world.send(HubMessage::Stop(KEY)).await;
    }

    #[tokio::test]
    async fn last_result_reports_absence_without_starting_a_session() {
        let world = world(FakeAgents::default());
        world.send(text(Some(7), 1, "/last")).await;
        eventually("empty last", || {
            world.texts().iter().any(|t| t.contains("Сохранённого ответа в этой теме нет"))
        })
        .await;
        assert!(world.agents.prompts().is_empty());
        assert!(world.store.last().is_none());
    }

    #[tokio::test]
    async fn last_result_oversize_and_stop_keep_the_previous_success() {
        let huge = "x".repeat(hub_core::results::MAX_TEXT_BYTES + 1);
        let world = world(FakeAgents::scripted(vec![
            Script::Say(vec![
                AgentEvent::AssistantText("kept".to_owned()),
                finished(),
                AgentEvent::AssistantText(huge),
                finished(),
            ]),
            Script::UntilStopped,
        ]));
        world.send(text(Some(7), 1, "first task")).await;
        eventually("oversize warning", || {
            world.texts().iter().any(|t| t.contains("превышает 64 КиБ"))
        })
        .await;
        eventually("idle", || !world.running()).await;
        world.send(text(Some(7), 2, "second task")).await;
        eventually("busy", || world.running()).await;
        world.send(HubMessage::Stop(KEY)).await;
        eventually("stopped", || world.texts().iter().any(|t| t == texts::STOPPED)).await;
        assert_eq!(world.results.records.lock().unwrap().get(&KEY).unwrap().text(), "kept");
    }

    #[tokio::test]
    async fn last_result_does_not_depend_on_delivery_queue_capacity() {
        let mut events = (0..100)
            .map(|n| {
                AgentEvent::ToolCall(hub_core::domain::ToolUse {
                    tool: "test".to_owned(),
                    summary: n.to_string(),
                })
            })
            .collect::<Vec<_>>();
        events.extend([AgentEvent::AssistantText("stored after burst".to_owned()), finished()]);
        let world = world(FakeAgents::scripted(vec![Script::Say(events)]));
        let release = world.messenger.delay_next_send();
        world.send(text(Some(7), 1, "task")).await;
        eventually("saved despite burst", || {
            world.results.records.lock().unwrap().contains_key(&KEY)
        })
        .await;
        release.send(()).unwrap();
        eventually("lost output warning", || {
            world.texts().iter().any(|t| t.contains("переполнения очереди"))
        })
        .await;
        assert_eq!(
            world.results.records.lock().unwrap().get(&KEY).unwrap().text(),
            "stored after burst"
        );
    }

    #[tokio::test]
    async fn last_result_oversize_answer_is_delivered_in_full_on_a_healthy_connection() {
        let huge = "x".repeat(200_000);
        let world = world(FakeAgents::scripted(vec![Script::Say(vec![
            AgentEvent::AssistantText(huge.clone()),
            finished(),
        ])]));
        world.send(text(Some(7), 1, "task")).await;
        eventually("started", || !world.agents.prompts().is_empty()).await;
        eventually("ended", || !world.running()).await;
        let delivered: String = world
            .texts()
            .into_iter()
            .filter(|text| text.bytes().all(|byte| byte == b'x'))
            .collect();
        assert_eq!(delivered.len(), huge.len());
        assert!(world.results.records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn last_result_large_output_keeps_markdown_formatting() {
        let huge = format!("```rust\n{}\n```", "let x = 1;\n".repeat(20_000));
        let expected = hub_core::markdown::markdown_to_html_chunks(
            &huge,
            hub_core::render::TELEGRAM_TEXT_LIMIT,
        );
        let world = world(FakeAgents::scripted(vec![Script::Say(vec![
            AgentEvent::AssistantText(huge),
            finished(),
        ])]));
        world.send(text(Some(7), 1, "task")).await;
        eventually("started", || !world.agents.prompts().is_empty()).await;
        eventually("ended", || !world.running()).await;
        let actual: Vec<_> = world
            .messenger
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Send { message, .. } if message.format == Format::Html => Some(message.text),
                _ => None,
            })
            .collect();
        assert_eq!(actual, expected);
        assert!(world.texts().iter().any(|text| text.contains("превышает 64 КиБ")));
    }

    #[tokio::test]
    async fn last_result_is_rejected_when_stop_was_already_processed() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.send(text(Some(7), 1, "task")).await;
        eventually("busy", || world.running()).await;
        let record = hub_core::results::SavedResult::new(
            KEY,
            "b".repeat(32),
            1,
            &TopicSession::fresh(
                BackendKind::Claude,
                AbsolutePath::new(world.root.path().to_path_buf()).unwrap(),
            )
            .with_session(SessionId::parse("s")),
            "cancelled".to_owned(),
        )
        .unwrap();
        let (reply, rejected) = oneshot::channel();
        world.handle.mailbox.try_send(HubMessage::Stop(KEY)).unwrap();
        world
            .handle
            .mailbox
            .try_send(HubMessage::SaveResult(SaveResult {
                key: KEY,
                generation: 1,
                pending: hub_core::outbox::Pending::new(record, "").unwrap(),
                reply,
            }))
            .unwrap();
        assert!(rejected.await.unwrap().is_err());
        assert!(world.results.records.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn doctor_preserves_context_and_only_opens_the_actions_menu() {
        let world = world(FakeAgents::default());
        configure_project(&world);
        bound(&world, KEY, "kept-session").await;
        let before = world.store.last().unwrap();
        world.send(text(Some(7), 1, "/doctor")).await;
        let pick = project_button(&world, "Бэкенд").await;
        world.send(HubMessage::Press(pick)).await;
        let repeat = project_button(&world, "Повторить").await;
        let actions = project_button(&world, "Действия").await;
        eventually("doctor report", || world.texts().iter().any(|t| t.contains("test directory")))
            .await;
        world.send(HubMessage::Press(actions)).await;
        eventually("actions menu", || {
            world.texts().iter().any(|t| t.contains("Пользовательские действия"))
        })
        .await;
        world.send(HubMessage::Press(repeat)).await;
        eventually("second doctor report", || {
            world.texts().iter().filter(|t| t.contains("Проверено:")).count() == 2
        })
        .await;
        assert_eq!(world.store.last().unwrap(), before);
        assert_eq!(bound_to(&world, KEY), SessionId::parse("kept-session"));
        assert!(world.agents.prompts().is_empty());
        assert!(!world.texts().iter().any(|t| t.contains("запуск команды")));
        let (done, stopped) = oneshot::channel();
        world.send(HubMessage::Shutdown(done)).await;
        stopped.await.unwrap();
        world.handle.task.await.unwrap();
    }

    #[tokio::test]
    async fn project_selection_requires_confirmation_and_persists_a_fresh_session() {
        let world = world(FakeAgents::default());
        configure_project(&world);
        bound(&world, KEY, "old-session").await;
        let confirm = choose_project(&world).await;
        assert_eq!(bound_to(&world, KEY), SessionId::parse("old-session"));
        world.send(HubMessage::Press(confirm.clone())).await;
        eventually("project selected", || {
            world.store.last().is_some_and(|saved| {
                saved.get(&KEY).is_some_and(|session| {
                    session.backend == BackendKind::Codex && session.session.is_none()
                })
            })
        })
        .await;
        let saved = world.store.last().unwrap();
        assert_eq!(
            saved.get(&KEY).unwrap().cwd,
            workspace_root(&world.root.path().join("project")).unwrap()
        );
        assert!(world.agents.prompts().is_empty());
        assert_eq!(world.store.recent.lock().unwrap().ids(), ["backend"]);
        bound(&world, KEY, "new-session").await;
        world.send(HubMessage::Press(confirm)).await;
        eventually("stale answer", || {
            world.messenger.calls().iter().any(|call|
            matches!(call, Call::Answer { text: Some(text), .. } if text == projects::STALE))
        })
        .await;
        assert_eq!(bound_to(&world, KEY), SessionId::parse("new-session"));
    }

    #[tokio::test]
    async fn restoring_the_same_session_invalidates_project_confirmation() {
        let world = world(FakeAgents::default());
        configure_project(&world);
        saved_snap(&world).await;
        let confirm = choose_project(&world).await;
        world.send(text(Some(7), 2, "/restore snap")).await;
        eventually("restored", || said(&world, "♻️ Восстановлено: snap")).await;
        world.send(HubMessage::Press(confirm)).await;
        eventually("stale after restore", || {
            world.messenger.calls().iter().any(|call|
            matches!(call, Call::Answer { text: Some(text), .. } if text == projects::STALE))
        })
        .await;
        assert_eq!(bound_to(&world, KEY), SessionId::parse("s-1"));
    }

    #[tokio::test]
    async fn project_cancel_and_disk_failure_keep_existing_context() {
        let world = world(FakeAgents::default());
        configure_project(&world);
        bound(&world, KEY, "old").await;
        let confirm = choose_project(&world).await;
        let cancel = project_button(&world, "Отмена").await;
        world.send(HubMessage::Press(cancel)).await;
        eventually("cancelled", || said(&world, "Выбор проекта отменён")).await;
        assert_eq!(bound_to(&world, KEY), SessionId::parse("old"));
        world.send(HubMessage::Press(confirm)).await;
        let confirm = choose_project(&world).await;
        world.store.fail.store(true, Ordering::SeqCst);
        world.send(HubMessage::Press(confirm)).await;
        eventually("write failed", || said(&world, "⚠️ Проект не выбран")).await;
        assert_eq!(
            world.handle.views.borrow().first().unwrap().session.session,
            SessionId::parse("old")
        );
        assert_eq!(bound_to(&world, KEY), SessionId::parse("old"));
    }

    #[tokio::test]
    async fn project_choice_rechecks_a_deleted_directory() {
        let world = world(FakeAgents::default());
        configure_project(&world);
        bound(&world, KEY, "old").await;
        let confirm = choose_project(&world).await;
        std::fs::remove_dir(world.root.path().join("project")).unwrap();
        world.send(HubMessage::Press(confirm)).await;
        eventually("missing directory", || {
            world.texts().iter().any(|text| text.contains("не существует"))
        })
        .await;
        assert_eq!(bound_to(&world, KEY), SessionId::parse("old"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn project_choice_rechecks_symlinks_at_confirmation() {
        let world = world(FakeAgents::default());
        configure_project(&world);
        bound(&world, KEY, "old").await;
        let confirm = choose_project(&world).await;
        let path = world.root.path().join("project");
        std::fs::remove_dir(&path).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), path).unwrap();
        world.send(HubMessage::Press(confirm)).await;
        eventually("outside root", || world.texts().iter().any(|text| text.contains("вне корня")))
            .await;
        assert_eq!(bound_to(&world, KEY), SessionId::parse("old"));
    }

    #[tokio::test]
    async fn a_new_turn_invalidates_project_buttons_and_busy_topics_refuse_the_menu() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        configure_project(&world);
        bound(&world, KEY, "old").await;
        let confirm = choose_project(&world).await;
        world.send(text(Some(7), 2, "work")).await;
        eventually("running", || world.running()).await;
        world.send(text(Some(7), 3, "/projects")).await;
        eventually("busy", || world.texts().contains(&texts::ALREADY_RUNNING.to_owned())).await;
        world.send(HubMessage::Press(confirm)).await;
        eventually("stale after turn", || {
            world.messenger.calls().iter().any(|call|
            matches!(call, Call::Answer { text: Some(text), .. } if text == projects::STALE))
        })
        .await;
        assert!(world.running());
        world.send(HubMessage::Stop(KEY)).await;
        eventually("stopped", || !world.running()).await;
    }

    #[tokio::test]
    async fn recent_project_is_shown_after_hub_restart() {
        let mut world = world(FakeAgents::default());
        configure_project(&world);
        let confirm = choose_project(&world).await;
        world.send(HubMessage::Press(confirm)).await;
        eventually("recent saved", || world.store.recent.lock().unwrap().ids() == ["backend"])
            .await;
        world.restart().await;
        world.send(text(Some(7), 2, "/projects")).await;
        let recent = project_button(&world, "🕘 Бэкенд").await;
        assert!(recent.data.starts_with(projects::PREFIX));
        assert!(world.agents.prompts().is_empty());
    }

    #[tokio::test]
    async fn failed_recent_history_write_does_not_hide_successful_selection() {
        let world = world(FakeAgents::default());
        configure_project(&world);
        world.store.fail_recent.store(true, Ordering::SeqCst);
        let confirm = choose_project(&world).await;
        world.send(HubMessage::Press(confirm)).await;
        eventually("history failure", || said(&world, "⚠️ Проект выбран, но список последних"))
            .await;
        assert_eq!(world.store.last().unwrap().get(&KEY).unwrap().backend, BackendKind::Codex);
        assert!(world.store.recent.lock().unwrap().ids().is_empty());
    }

    #[tokio::test]
    async fn known_titles_are_loaded_after_restart() {
        let mut world = world(FakeAgents::default());
        world.send(inbound(Some(7), 1, Content::TopicCreated { name: "backend".to_owned() })).await;
        eventually("persisted title", || {
            world.store.titles.lock().unwrap().get(&KEY).is_some_and(|title| title == "backend")
        })
        .await;
        world
            .send(inbound(Some(7), 2, Content::TopicRenamed { name: "backend v2".to_owned() }))
            .await;
        eventually("persisted rename", || {
            world.store.titles.lock().unwrap().get(&KEY).is_some_and(|title| title == "backend v2")
        })
        .await;
        world.restart().await;
        eventually("restored title", || {
            world
                .handle
                .views
                .borrow()
                .iter()
                .any(|view| view.key == KEY && view.title.as_deref() == Some("backend v2"))
        })
        .await;
        assert!(world.agents.prompts().is_empty());
    }

    #[tokio::test]
    async fn rename_during_a_turn_preserves_session_and_does_not_send_a_prompt() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        bound(&world, KEY, "s-1").await;
        world.send(text(Some(7), 1, "work")).await;
        eventually("running", || world.running()).await;
        let before = world.handle.views.borrow().first().unwrap().session.clone();
        world
            .send(inbound(Some(7), 2, Content::TopicRenamed { name: "Новое имя".to_owned() }))
            .await;
        eventually("renamed", || {
            world
                .handle
                .views
                .borrow()
                .first()
                .is_some_and(|view| view.title.as_deref() == Some("Новое имя"))
        })
        .await;
        let view = world.handle.views.borrow().first().unwrap().clone();
        assert_eq!(view.session, before);
        assert_eq!(view.state, TopicState::Running);
        assert_eq!(world.agents.prompts(), ["work"]);
        assert_eq!(
            world.store.titles.lock().unwrap().get(&KEY).map(String::as_str),
            Some("Новое имя")
        );
        world.send(HubMessage::Stop(KEY)).await;
        eventually("stopped", || !world.running()).await;
    }

    #[tokio::test]
    async fn rename_before_first_message_is_kept_until_binding() {
        let world = world(FakeAgents::default());
        world
            .send(inbound(Some(7), 1, Content::TopicRenamed { name: "Известная тема".to_owned() }))
            .await;
        eventually("name received", || world.store.titles.lock().unwrap().contains_key(&KEY)).await;
        assert!(world.handle.views.borrow().is_empty());
        world.send(text(Some(7), 2, "/new")).await;
        eventually("named binding", || {
            world
                .handle
                .views
                .borrow()
                .first()
                .is_some_and(|view| view.title.as_deref() == Some("Известная тема"))
        })
        .await;
        assert!(world.agents.prompts().is_empty());
    }

    #[tokio::test]
    async fn failed_title_save_is_visible_and_a_repeated_update_retries() {
        let world = world(FakeAgents::default());
        world.send(inbound(Some(7), 1, Content::TopicCreated { name: "Старое".to_owned() })).await;
        eventually("old title persisted", || world.store.titles.lock().unwrap().contains_key(&KEY))
            .await;
        world.store.fail.store(true, Ordering::SeqCst);
        world.send(inbound(Some(7), 2, Content::TopicRenamed { name: "Новое".to_owned() })).await;
        eventually("save warning", || {
            world.texts().iter().any(|text| text.contains("не сохранено на диск"))
        })
        .await;
        assert_eq!(world.handle.views.borrow().first().unwrap().title.as_deref(), Some("Новое"));
        assert_eq!(
            world.store.titles.lock().unwrap().get(&KEY).map(String::as_str),
            Some("Старое")
        );
        world.store.fail.store(false, Ordering::SeqCst);
        world.send(inbound(Some(7), 3, Content::TopicRenamed { name: "Новое".to_owned() })).await;
        eventually("new title persisted", || {
            world.store.titles.lock().unwrap().get(&KEY).is_some_and(|title| title == "Новое")
        })
        .await;
    }

    #[tokio::test]
    async fn message_runs_the_agent_and_relays_its_events() {
        let world = world(FakeAgents::scripted(vec![Script::Say(vec![
            AgentEvent::SessionStarted(SessionId::parse("s-1").unwrap()),
            AgentEvent::AssistantText("**готово**".to_owned()),
            finished(),
        ])]));
        world.send(text(Some(7), 1, "сделай")).await;

        eventually("finished", || world.texts().iter().any(|t| t.contains("✅ Готово · ходов: 1")))
            .await;
        assert!(world.texts().iter().any(|t| t.contains("<b>готово</b>")));
        assert_eq!(world.agents.prompts(), ["сделай"]);
        eventually("bound", || {
            world
                .store
                .last()
                .and_then(|topics| topics.get(&KEY).cloned())
                .is_some_and(|session| session.session == SessionId::parse("s-1"))
        })
        .await;
    }

    #[tokio::test]
    async fn general_topic_asks_for_a_topic() {
        let world = world(FakeAgents::default());
        world.send(text(None, 3, "привет")).await;
        eventually("hint", || world.texts() == [texts::CREATE_TOPIC]).await;
        assert!(world.messenger.calls().iter().any(|call| matches!(
            call,
            Call::Send { target: Target::Reply { message: MessageId(3), .. }, .. }
        )));
    }

    impl FakeAgents {
        fn polling(polls: Vec<Vec<Outcome>>) -> Self {
            Self { quotas: StdMutex::new(polls.into()), ..Self::default() }
        }
    }

    fn five_hour(percent: i64) -> Vec<UsageWindow> {
        vec![UsageWindow {
            kind: WindowKind::FiveHour,
            percent: Percent::from_int(percent),
            resets_at: None,
        }]
    }

    fn claude_max(percent: i64) -> Outcome {
        (
            BackendKind::Claude,
            Ok(QuotaReport::fresh(BackendKind::Claude, Label::new("Max"), five_hour(percent))),
        )
    }

    fn press(callback: &str, message: i32, data: &str) -> HubMessage {
        HubMessage::Press(Press {
            callback: callback.to_owned(),
            user: UserId(1),
            chat: Some(ChatId(-100)),
            message: Some(MessageId(message)),
            html: None,
            data: data.to_owned(),
        })
    }

    /// Texts the message `id` was edited to, in order.
    fn edits(world: &World, id: i32) -> Vec<(String, Format)> {
        world
            .messenger
            .calls()
            .into_iter()
            .filter_map(|call| {
                let Call::Edit { chat: _, message, text, format } = call else { return None };
                (message == MessageId(id)).then_some((text, format))
            })
            .collect()
    }

    fn has_card(world: &World, id: i32, needle: &str) -> bool {
        edits(world, id)
            .iter()
            .any(|(text, format)| *format == Format::Html && text.contains(needle))
    }

    #[tokio::test]
    async fn limits_turn_a_placeholder_into_the_card() {
        let world = world(FakeAgents::polling(vec![vec![
            claude_max(62),
            (BackendKind::Qwen, Ok(QuotaReport::not_reported(BackendKind::Qwen))),
        ]]));
        world.send(text(Some(7), 1, "/limits")).await;

        eventually("card", || has_card(&world, 1, "<b>Claude</b> (Max)")).await;
        assert!(world.messenger.calls().contains(&Call::Send {
            target: Target::Topic(KEY),
            message: Outgoing::plain(texts::LIMITS_COLLECTING.to_owned()),
            id: MessageId(1),
        }));
        assert!(has_card(&world, 1, "62%"));
        assert!(has_card(&world, 1, "Qwen — квоты подписки агент не сообщает"));
        eventually("refresh button", || {
            world.messenger.calls().contains(&Call::EditKeyboard {
                chat: ChatId(-100),
                message: MessageId(1),
                keyboard: limits::refresh_keyboard(),
            })
        })
        .await;
    }

    #[tokio::test]
    async fn a_card_telegram_cannot_parse_is_shown_as_plain_text_with_its_button() {
        let messenger = FakeMessenger::with_edits(vec![Err(SendError::Rejected(
            "can't parse entities".to_owned(),
        ))]);
        let world = world_with(FakeAgents::polling(vec![vec![claude_max(62)]]), messenger);
        world.send(text(Some(7), 1, "/limits")).await;

        eventually("refresh button", || {
            world.messenger.calls().contains(&Call::EditKeyboard {
                chat: ChatId(-100),
                message: MessageId(1),
                keyboard: limits::refresh_keyboard(),
            })
        })
        .await;
        let edits = edits(&world, 1);
        assert!(
            matches!(edits.as_slice(), [(card, Format::Plain)] if card.contains("Claude (Max)") && !card.contains('<')),
            "{edits:?}"
        );
    }

    #[tokio::test]
    async fn limits_work_in_the_general_chat() {
        let world = world(FakeAgents::polling(vec![vec![claude_max(5)]]));
        world.send(text(None, 3, "/limits")).await;
        eventually("card", || has_card(&world, 1, "5%")).await;
        assert!(world.messenger.calls().iter().any(|call| matches!(
            call,
            Call::Send { target: Target::Reply { message: MessageId(3), .. }, message, .. }
                if message.text == texts::LIMITS_COLLECTING
        )));
    }

    #[tokio::test]
    async fn the_default_backend_is_listed_first() {
        let codex = (
            BackendKind::Codex,
            Ok(QuotaReport::unavailable(
                BackendKind::Codex,
                "вход по API-ключу — лимитов подписки нет",
            )),
        );
        let agents = FakeAgents::polling(vec![vec![claude_max(1), codex]]);
        let world = world_from(agents, FakeMessenger::default(), BackendKind::Codex);
        world.send(text(Some(7), 1, "/limits")).await;
        eventually("card", || has_card(&world, 1, "<b>Claude</b>")).await;
        let (card, _) =
            edits(&world, 1).into_iter().find(|(_, format)| *format == Format::Html).unwrap();
        let codex = card.find("<b>Codex</b>").unwrap();
        let claude = card.find("<b>Claude</b>").unwrap();
        assert!(codex < claude, "{card}");
    }

    #[tokio::test]
    async fn a_failed_probe_falls_back_to_what_a_session_reported() {
        let update = QuotaUpdate::new(BackendKind::Claude, None, five_hour(40));
        let world = world(FakeAgents {
            scripts: StdMutex::new(
                vec![Script::Say(vec![AgentEvent::Quota(update), finished()])].into(),
            ),
            ..FakeAgents::polling(vec![vec![(BackendKind::Claude, Err("тайм-аут".to_owned()))]])
        });
        world.send(text(Some(7), 1, "сделай")).await;
        eventually("finished", || world.texts().iter().any(|t| t.contains("✅ Готово · ходов: 1")))
            .await;
        world.send(text(Some(7), 2, "/limits")).await;
        let id = || {
            world
                .texts()
                .iter()
                .position(|t| t == texts::LIMITS_COLLECTING)
                .map(|i| i32::try_from(i).unwrap() + 1)
        };
        eventually("cached card", || id().is_some_and(|id| has_card(&world, id, "40%"))).await;
        assert!(id().is_some_and(|id| has_card(&world, id, "(на ")));
    }

    #[tokio::test]
    async fn a_failure_without_a_cache_shows_its_reason() {
        let failed = (BackendKind::Claude, Err("Claude не ответил о лимитах за 15 с".to_owned()));
        let world = world(FakeAgents::polling(vec![vec![failed]]));
        world.send(text(Some(7), 1, "/limits")).await;
        eventually("card", || has_card(&world, 1, "⚠️ Claude не ответил о лимитах за 15 с")).await;
    }

    #[tokio::test]
    async fn refresh_redraws_the_same_message() {
        let world = world(FakeAgents::polling(vec![vec![claude_max(62)], vec![claude_max(70)]]));
        world.send(text(Some(7), 1, "/limits")).await;
        eventually("card", || has_card(&world, 1, "62%")).await;

        world.send(press("c1", 1, limits::REFRESH)).await;

        eventually("redrawn", || has_card(&world, 1, "70%")).await;
        assert!(edits(&world, 1).contains(&(texts::LIMITS_COLLECTING.to_owned(), Format::Plain)));
        eventually("answered", || {
            world
                .messenger
                .calls()
                .contains(&Call::Answer { callback: "c1".to_owned(), text: None })
        })
        .await;
        assert_eq!(world.texts(), [texts::LIMITS_COLLECTING], "no new message");
    }

    #[tokio::test]
    async fn presses_and_commands_wait_while_a_poll_runs() {
        let (release, held) = oneshot::channel();
        let agents = FakeAgents::polling(vec![vec![claude_max(10)]]);
        *agents.hold_quotas.lock().unwrap() = Some(held);
        let world = world(agents);
        world.send(text(Some(7), 1, "/limits")).await;
        eventually("placeholder", || world.texts() == [texts::LIMITS_COLLECTING]).await;

        world.send(press("c2", 1, limits::REFRESH)).await;
        world.send(text(Some(7), 2, "/limits")).await;
        eventually("busy", || world.texts().contains(&texts::LIMITS_BUSY.to_owned())).await;
        eventually("answered", || {
            world
                .messenger
                .calls()
                .contains(&Call::Answer { callback: "c2".to_owned(), text: None })
        })
        .await;
        assert!(edits(&world, 1).is_empty(), "a press during a poll changes nothing");

        release.send(()).unwrap();
        eventually("card", || has_card(&world, 1, "10%")).await;
        assert_eq!(world.agents.polls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_crashed_poll_frees_the_next_one() {
        let agents = FakeAgents::polling(vec![Vec::new(), vec![claude_max(33)]]);
        agents.crash_quotas.store(true, Ordering::SeqCst);
        let world = world(agents);
        world.send(text(Some(7), 1, "/limits")).await;
        eventually("internal error", || {
            edits(&world, 1).contains(&(texts::INTERNAL_ERROR.to_owned(), Format::Plain))
        })
        .await;
        world.send(text(Some(7), 2, "/limits")).await;
        eventually("second card", || has_card(&world, 2, "33%")).await;
    }

    #[tokio::test]
    async fn other_presses_still_reach_approvals_and_questions() {
        let world = world(FakeAgents::default());
        world.send(press("c3", 1, "ap:allow:unknown")).await;
        eventually("stale answer", || {
            world.messenger.calls().contains(&Call::Answer {
                callback: "c3".to_owned(),
                text: Some(texts::APPROVAL_STALE.to_owned()),
            })
        })
        .await;
    }

    #[tokio::test]
    async fn stop_interrupts_the_running_agent() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.send(text(Some(7), 1, "долго")).await;
        eventually("running", || world.running()).await;

        world.send(text(Some(7), 2, "/stop")).await;
        eventually("stopped", || world.texts().contains(&texts::STOPPED.to_owned())).await;
        eventually("waiting", || !world.running()).await;
    }

    #[tokio::test]
    async fn commands_are_refused_while_running() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.send(text(Some(7), 1, "долго")).await;
        eventually("running", || world.running()).await;
        world.send(text(Some(7), 2, "/new project")).await;
        eventually("refused", || world.texts().contains(&texts::ALREADY_RUNNING.to_owned())).await;
        world.send(HubMessage::Stop(KEY)).await;
    }

    #[tokio::test]
    async fn new_and_cwd_resolve_inside_the_root_only() {
        let world = world(FakeAgents::default());
        world.send(text(Some(7), 1, "/new project")).await;
        eventually("new", || {
            world.texts().iter().any(|t| t.starts_with("🆕 Новая сессия") && t.contains("project"))
        })
        .await;

        world.send(text(Some(7), 2, "/cwd ../..")).await;
        eventually("refused", || world.texts().iter().any(|t| t.starts_with("⚠️"))).await;

        world.send(text(Some(7), 3, "/cwd")).await;
        eventually("usage", || world.texts().contains(&texts::CWD_USAGE.to_owned())).await;
    }

    #[tokio::test]
    async fn status_and_reset_describe_the_session() {
        let world = world(FakeAgents::default());
        world.send(text(Some(7), 1, "/status")).await;
        eventually("no session", || world.texts().contains(&texts::NO_SESSION.to_owned())).await;
        world.send(text(Some(7), 2, "/reset")).await;
        eventually("reset", || world.texts().iter().any(|t| t.starts_with("🔄 Контекст сброшен")))
            .await;
        world.send(text(Some(7), 3, "/status")).await;
        eventually("waiting", || world.texts().iter().any(|t| t.starts_with("💤 ожидает"))).await;
    }

    #[tokio::test]
    async fn backend_is_shown_switched_and_validated() {
        let world = world(FakeAgents::default());
        world.send(text(Some(7), 1, "/backend")).await;
        eventually("shown", || {
            world
                .texts()
                .iter()
                .any(|t| t.starts_with("ℹ️ Текущая сессия") && t.contains("backend: claude"))
        })
        .await;
        world.send(text(Some(7), 2, "/backend claude")).await;
        eventually("already", || {
            world.texts().iter().any(|t| t.starts_with("ℹ️ Бэкенд уже выбран"))
        })
        .await;
        world.send(text(Some(7), 3, "/backend gpt")).await;
        eventually("unknown", || {
            world.texts().contains(
                &"⚠️ Неизвестный бэкенд gpt. Доступны: claude, codex, qwen, hermes, opencode"
                    .to_owned(),
            )
        })
        .await;
        world.send(text(Some(7), 4, "/backend codex")).await;
        eventually("switched", || {
            world
                .texts()
                .iter()
                .any(|t| t.starts_with("🔀 Бэкенд изменён") && t.contains("backend: codex"))
        })
        .await;
        world.send(text(Some(7), 5, "/backend hermes")).await;
        eventually("hermes switched", || {
            world
                .texts()
                .iter()
                .any(|t| t.starts_with("🔀 Бэкенд изменён") && t.contains("backend: hermes"))
        })
        .await;
    }

    #[tokio::test]
    async fn backend_switch_is_refused_while_running() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.send(text(Some(7), 1, "долго")).await;
        eventually("running", || world.running()).await;
        world.send(text(Some(7), 2, "/backend codex")).await;
        eventually("refused", || world.texts().contains(&texts::ALREADY_RUNNING.to_owned())).await;
        assert!(!world.texts().iter().any(|t| t.starts_with("🔀")));
        world.send(HubMessage::Stop(KEY)).await;
    }

    #[tokio::test]
    async fn new_without_a_backend_uses_the_default_from_settings() {
        let world = world_from(FakeAgents::default(), FakeMessenger::default(), BackendKind::Codex);
        world.send(text(Some(7), 1, "/new project")).await;
        eventually("new", || {
            world
                .texts()
                .iter()
                .any(|t| t.starts_with("🆕 Новая сессия") && t.contains("backend: codex"))
        })
        .await;
    }

    #[tokio::test]
    async fn messages_during_a_run_go_to_the_same_session() {
        let world = world(FakeAgents::scripted(vec![Script::Collect(1)]));
        world.send(text(Some(7), 1, "первое")).await;
        eventually("running", || world.running()).await;
        world.send(text(Some(7), 2, "второе")).await;
        eventually("collected", || world.agents.prompts() == ["первое", "второе"]).await;
    }

    #[tokio::test]
    async fn leftover_prompt_starts_a_new_session() {
        let world =
            world(FakeAgents::scripted(vec![Script::UntilStopped, Script::Say(vec![finished()])]));
        world.send(text(Some(7), 1, "первое")).await;
        eventually("running", || world.running()).await;
        world.send(HubMessage::Stop(KEY)).await;
        world.send(text(Some(7), 2, "после стопа")).await;
        eventually("relaunched", || world.agents.prompts() == ["первое", "после стопа"]).await;
    }

    #[tokio::test]
    async fn text_answers_an_open_question() {
        let world = world(FakeAgents::default());
        let (responder, answer) = oneshot::channel();
        let question =
            Question::new("Цвет?".to_owned(), String::new(), Vec::new(), Selection::Single)
                .unwrap();
        lock(&world.handle.registries).questions.open(
            QuestionId::from_random([1; 8]),
            KEY,
            question,
            responder,
        );

        world.send(text(Some(7), 1, "синий")).await;
        let answered = tokio::time::timeout(Duration::from_secs(2), answer).await.unwrap().unwrap();
        assert_eq!(answered, Answer::Text("синий".to_owned()));
        assert!(world.agents.prompts().is_empty());
    }

    #[tokio::test]
    async fn voice_gets_a_hint() {
        let world = world(FakeAgents::default());
        world.send(inbound(Some(7), 1, Content::Voice)).await;
        eventually("hint", || world.texts().contains(&texts::VOICE_UNSUPPORTED.to_owned())).await;
    }

    #[tokio::test]
    async fn album_parts_become_one_turn() {
        let mut messenger = FakeMessenger::default();
        messenger.files.insert("p1".to_owned(), vec![1]);
        messenger.files.insert("p2".to_owned(), vec![2]);
        let world = world_with(FakeAgents::default(), messenger);
        for (message, file, caption) in [(1, "p1", "смотри"), (2, "p2", "")] {
            world
                .send(HubMessage::Inbound(Inbound {
                    chat: ChatId(-100),
                    thread: Some(ThreadId(7)),
                    user: Some(UserId(1)),
                    message: MessageId(message),
                    album: Some("g".to_owned()),
                    content: Content::Media {
                        caption: caption.to_owned(),
                        attachment: Attachment::Photo(FileRef {
                            id: file.to_owned(),
                            name: None,
                            size: Some(1),
                        }),
                    },
                }))
                .await;
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        eventually("one turn", || world.agents.prompts() == ["смотри"]).await;
    }

    #[tokio::test]
    async fn shutdown_stops_running_sessions() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.send(text(Some(7), 1, "долго")).await;
        eventually("running", || world.running()).await;
        let (done, stopped) = oneshot::channel();
        world.send(HubMessage::Shutdown(done)).await;
        tokio::time::timeout(Duration::from_secs(5), stopped).await.unwrap().unwrap();
        eventually("stopped text", || world.texts().contains(&texts::STOPPED.to_owned())).await;
    }

    #[tokio::test]
    async fn nothing_starts_during_shutdown() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.send(text(Some(7), 1, "долго")).await;
        eventually("running", || world.running()).await;
        let (done, stopped) = oneshot::channel();
        world.send(HubMessage::Shutdown(done)).await;
        let late = TopicKey { chat: ChatId(-100), thread: ThreadId(8) };
        let prompt = Prompt::new("поздно".to_owned(), Vec::new()).unwrap();
        world.send(HubMessage::Deliver { key: late, prompt }).await;
        tokio::time::timeout(Duration::from_secs(5), stopped).await.unwrap().unwrap();
        assert_eq!(world.agents.prompts(), ["долго"]);
    }

    const OTHER: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(8) };
    const THIRD: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(9) };

    async fn bound(world: &World, key: TopicKey, id: &str) {
        world
            .send(HubMessage::Bind {
                key,
                generation: None,
                session: SessionId::parse(id).unwrap(),
            })
            .await;
        eventually("bound", || {
            world
                .store
                .last()
                .and_then(|topics| topics.get(&key).cloned())
                .is_some_and(|session| session.session == SessionId::parse(id))
        })
        .await;
    }

    fn said(world: &World, prefix: &str) -> bool {
        world.texts().iter().any(|text| text.starts_with(prefix))
    }

    fn count(world: &World, text: &str) -> usize {
        world.texts().iter().filter(|said| *said == text).count()
    }

    async fn saved_snap(world: &World) {
        bound(world, KEY, "s-1").await;
        world.send(text(Some(7), 1, "/save snap")).await;
        eventually("saved", || said(world, "💾 Сохранено: snap · claude · ")).await;
    }

    fn bound_to(world: &World, key: TopicKey) -> Option<SessionId> {
        world.store.last().and_then(|topics| topics.get(&key).and_then(|s| s.session.clone()))
    }

    #[tokio::test]
    async fn restore_moves_the_session_from_its_old_topic() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;

        world.send(text(Some(8), 2, "/restore snap")).await;

        eventually("restored", || said(&world, "♻️ Восстановлено: snap · claude · ")).await;
        eventually("told", || world.texts().contains(&texts::SESSION_MOVED.to_owned())).await;
        let topics = world.store.last().unwrap();
        assert_eq!(topics.get(&OTHER).and_then(|s| s.session.clone()), SessionId::parse("s-1"));
        assert_eq!(topics.get(&KEY).map(|s| s.session.is_none()), Some(true));
        assert!(world.agents.archive.imports().is_empty(), "the agent still had it");
    }

    #[tokio::test]
    async fn restore_imports_what_the_agent_lost_into_the_named_directory() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;
        world.agents.archive.forget();

        world.send(text(Some(8), 2, "/restore snap project")).await;

        eventually("restored", || {
            world
                .texts()
                .iter()
                .any(|t| t.starts_with("♻️ Восстановлено: snap") && t.ends_with("project"))
        })
        .await;
        assert_eq!(world.agents.archive.imports(), ["s-1"]);
    }

    #[tokio::test]
    async fn a_failed_import_leaves_the_topics_unchanged() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;
        world.agents.archive.forget();
        world.agents.archive.fail_import.store(true, Ordering::SeqCst);

        world.send(text(Some(8), 2, "/restore snap")).await;

        eventually("failed", || {
            world.texts().contains(&"⚠️ Данные снимка повреждены: сбой".to_owned())
        })
        .await;
        let topics = world.store.last().unwrap();
        assert_eq!(topics.get(&KEY).and_then(|s| s.session.clone()), SessionId::parse("s-1"));
        assert!(!topics.contains_key(&OTHER));
    }

    #[tokio::test]
    async fn an_unpersisted_restore_keeps_the_old_bindings() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;
        world.store.fail.store(true, Ordering::SeqCst);

        world.send(text(Some(8), 2, "/restore snap")).await;

        eventually("not persisted", || {
            world.texts().contains(&texts::RESTORE_NOT_PERSISTED.to_owned())
        })
        .await;
        assert!(!said(&world, "♻️") && !world.texts().contains(&texts::SESSION_MOVED.to_owned()));
        world.store.fail.store(false, Ordering::SeqCst);
        let persisted = world.store.last().unwrap();
        assert_eq!(persisted.get(&KEY).and_then(|s| s.session.clone()), SessionId::parse("s-1"));
        assert!(!persisted.contains_key(&OTHER));
        world.send(text(Some(8), 3, "/status")).await;
        eventually("still unbound in memory", || {
            world.texts().contains(&texts::NO_SESSION.to_owned())
        })
        .await;
    }

    #[tokio::test]
    async fn a_second_restore_of_the_same_session_waits_for_the_first() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;
        world.agents.archive.forget();
        let (release, held) = std::sync::mpsc::channel();
        *world.agents.archive.hold_import.lock().unwrap() = Some(held);

        world.send(text(Some(8), 2, "/restore snap")).await;
        eventually("first import paused", || world.agents.archive.importing.load(Ordering::SeqCst))
            .await;
        world.send(text(Some(9), 3, "/restore snap")).await;
        eventually("second refused", || count(&world, texts::SESSION_RESERVED) == 1).await;

        release.send(()).unwrap();
        eventually("first restored", || said(&world, "♻️ Восстановлено: snap")).await;
        // The second restore never reached `present`/`import`, so the first one's files were
        // never shared with it and no rollback could touch them.
        assert_eq!(world.agents.archive.imports(), ["s-1"]);
        assert_eq!(world.texts().iter().filter(|t| t.starts_with("♻️")).count(), 1);
        let topics = world.store.last().unwrap();
        assert_eq!(topics.get(&OTHER).and_then(|s| s.session.clone()), SessionId::parse("s-1"));
        assert!(!topics.contains_key(&THIRD));
    }

    #[tokio::test]
    async fn a_session_being_saved_is_neither_restored_nor_run_elsewhere() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;
        bound(&world, OTHER, "s-1").await;
        let (release, held) = std::sync::mpsc::channel();
        *world.agents.archive.hold_export.lock().unwrap() = Some(held);
        world.agents.archive.exporting.store(false, Ordering::SeqCst);

        world.send(text(Some(7), 4, "/save second")).await;
        eventually("export paused", || world.agents.archive.exporting.load(Ordering::SeqCst)).await;
        world.send(text(Some(8), 5, "/restore snap")).await;
        eventually("restore refused", || count(&world, texts::SESSION_RESERVED) == 1).await;
        world.send(text(Some(8), 6, "привет")).await;

        eventually("turn refused", || count(&world, texts::SESSION_RESERVED) == 2).await;
        assert!(world.agents.prompts().is_empty());
        release.send(()).unwrap();
        eventually("saved", || said(&world, "💾 Сохранено: second")).await;
        world.send(text(Some(8), 7, "теперь можно")).await;
        eventually("ran", || world.agents.prompts() == ["теперь можно"]).await;
    }

    #[tokio::test]
    async fn save_and_restore_wait_for_the_turn() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.send(text(Some(7), 1, "долго")).await;
        eventually("running", || world.running()).await;

        world.send(text(Some(7), 2, "/save snap")).await;
        world.send(text(Some(7), 3, "/restore snap")).await;

        eventually("refused twice", || count(&world, texts::FINISH_TURN_FIRST) == 2).await;
        world.send(HubMessage::Stop(KEY)).await;
    }

    #[tokio::test]
    async fn save_waits_for_a_turn_of_the_same_session_in_another_topic() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        bound(&world, KEY, "s-1").await;
        bound(&world, OTHER, "s-1").await;
        world.send(text(Some(8), 1, "долго")).await;
        eventually("running", || world.running()).await;

        world.send(text(Some(7), 2, "/save snap")).await;

        eventually("refused", || count(&world, texts::SESSION_BUSY_ELSEWHERE) == 1).await;
        assert!(!world.agents.archive.exporting.load(Ordering::SeqCst), "history was copied");
        world.send(HubMessage::Stop(OTHER)).await;
        eventually("waiting", || !world.running()).await;
        world.send(text(Some(7), 3, "/save snap")).await;
        eventually("saved after the turn", || said(&world, "💾 Сохранено: snap")).await;
    }

    #[tokio::test]
    async fn turns_wait_while_a_snapshot_is_being_written() {
        let world = world(FakeAgents::default());
        bound(&world, KEY, "s-1").await;
        let (release, held) = std::sync::mpsc::channel();
        *world.agents.archive.hold_export.lock().unwrap() = Some(held);

        world.send(text(Some(7), 1, "/save snap")).await;
        world.send(text(Some(7), 2, "привет")).await;
        world.send(text(Some(7), 3, "/reset")).await;

        eventually("busy twice", || count(&world, texts::SNAPSHOT_BUSY) == 2).await;
        assert!(world.agents.prompts().is_empty());
        release.send(()).unwrap();
        eventually("saved", || said(&world, "💾 Сохранено: snap")).await;
    }

    #[tokio::test]
    async fn the_gui_reset_waits_for_a_snapshot() {
        let world = world(FakeAgents::default());
        bound(&world, KEY, "s-1").await;
        let (release, held) = std::sync::mpsc::channel();
        *world.agents.archive.hold_export.lock().unwrap() = Some(held);
        world.send(text(Some(7), 1, "/save snap")).await;
        eventually("export paused", || world.agents.archive.exporting.load(Ordering::SeqCst)).await;

        world.send(HubMessage::Reset(KEY)).await;
        // The hub handles messages in order: once `/status` is answered, the reset was handled.
        world.send(text(Some(7), 2, "/status")).await;
        eventually("status", || said(&world, texts::WAITING)).await;

        assert_eq!(bound_to(&world, KEY), SessionId::parse("s-1"));
        assert!(
            world
                .texts()
                .iter()
                .any(|t| t.starts_with(texts::WAITING)
                    && t.lines().any(|line| line == "session: s-1"))
        );
        release.send(()).unwrap();
        eventually("saved", || said(&world, "💾 Сохранено: snap")).await;
        world.send(HubMessage::Reset(KEY)).await;
        eventually("reset after the save", || bound_to(&world, KEY).is_none()).await;
    }

    #[tokio::test]
    async fn a_crashed_save_releases_the_session() {
        let world = world(FakeAgents::default());
        bound(&world, KEY, "s-1").await;
        world.agents.crash_archive.store(true, Ordering::SeqCst);

        world.send(text(Some(7), 1, "/save snap")).await;

        eventually("reported", || count(&world, texts::INTERNAL_ERROR) == 1).await;
        world.send(text(Some(7), 2, "/save snap")).await;
        eventually("saved on retry", || said(&world, "💾 Сохранено: snap")).await;
        world.send(text(Some(7), 3, "дальше")).await;
        eventually("ran", || world.agents.prompts() == ["дальше"]).await;
    }

    #[tokio::test]
    async fn a_crashed_restore_releases_the_session() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;
        world.agents.crash_archive.store(true, Ordering::SeqCst);

        world.send(text(Some(8), 2, "/restore snap")).await;

        eventually("reported", || count(&world, texts::INTERNAL_ERROR) == 1).await;
        assert_eq!(bound_to(&world, KEY), SessionId::parse("s-1"));
        world.send(text(Some(7), 3, "дальше")).await;
        eventually("ran", || world.agents.prompts() == ["дальше"]).await;
        world.send(text(Some(8), 4, "/restore snap")).await;
        eventually("restored on retry", || said(&world, "♻️ Восстановлено: snap")).await;
    }

    #[tokio::test]
    async fn snapshot_commands_report_what_is_wrong() {
        let world = world(FakeAgents::default());
        world.send(text(Some(7), 1, "/save")).await;
        eventually("nothing", || world.texts().contains(&texts::NOTHING_TO_SAVE.to_owned())).await;
        world.send(text(Some(7), 2, "/save Bad")).await;
        eventually("bad name", || said(&world, "⚠️ В имени снимка допустимы")).await;
        world.send(text(Some(7), 3, "/restore")).await;
        eventually("usage", || world.texts().contains(&texts::RESTORE_USAGE.to_owned())).await;
        world.send(text(Some(7), 4, "/restore nope")).await;
        eventually("unknown", || world.texts().contains(&"⚠️ Снимок nope не найден".to_owned()))
            .await;
        world.send(text(Some(7), 5, "/restore nope ../..")).await;
        eventually("outside", || {
            world.texts().iter().any(|t| t.starts_with("⚠️") && t.contains("вне корня"))
        })
        .await;
    }

    #[tokio::test]
    async fn a_taken_name_is_refused() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;
        world.send(text(Some(7), 2, "/save snap")).await;
        eventually("taken", || world.texts().contains(&"⚠️ Снимок snap уже существует".to_owned()))
            .await;
    }

    #[tokio::test]
    async fn saves_list_snapshots_and_default_names_follow_the_title() {
        let world = world(FakeAgents::default());
        world.send(text(Some(7), 1, "/saves")).await;
        eventually("empty", || world.texts().contains(&texts::NO_SNAPSHOTS.to_owned())).await;

        world
            .send(inbound(Some(7), 2, Content::TopicCreated { name: "Старое имя".to_owned() }))
            .await;
        bound(&world, KEY, "s-1").await;
        world
            .send(inbound(Some(7), 3, Content::TopicRenamed { name: "Починить тесты".to_owned() }))
            .await;
        world.send(text(Some(7), 4, "/save")).await;
        eventually("saved", || said(&world, "💾 Сохранено: pochinit-testy-")).await;

        world.send(text(Some(7), 5, "/saves")).await;
        eventually("listed", || said(&world, "💾 Снимки, новые сверху:\npochinit-testy-")).await;
        assert!(world.texts().iter().any(|t| t.ends_with(" · Починить тесты")));
    }

    #[tokio::test]
    async fn a_snapshot_from_outside_the_root_needs_a_directory_inside_it() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;
        world.agents.archive.forget();
        let narrower = Settings {
            workspace_root: world.root.path().join("project"),
            ..Settings::clone(&world.settings.borrow())
        };
        world.settings.send(Arc::new(narrower)).unwrap();

        world.send(text(Some(8), 2, "/restore snap")).await;

        eventually("outside", || {
            world.texts().iter().any(|t| t.starts_with("⚠️") && t.contains("вне корня"))
        })
        .await;
        assert!(world.agents.archive.imports().is_empty(), "nothing imported");
        let topics = world.store.last().unwrap();
        assert_eq!(topics.get(&KEY).and_then(|s| s.session.clone()), SessionId::parse("s-1"));
        assert!(!topics.contains_key(&OTHER));
    }

    #[tokio::test]
    async fn a_restore_held_up_by_a_snapshot_elsewhere_says_a_repeat_only_rebinds() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;
        bound(&world, THIRD, "s-2").await;
        world.send(text(Some(9), 3, "/save other")).await;
        eventually("saved", || said(&world, "💾 Сохранено: other")).await;
        world.agents.archive.forget();
        let (release, held) = std::sync::mpsc::channel();
        *world.agents.archive.hold_import.lock().unwrap() = Some(held);
        world.send(text(Some(7), 4, "/restore other")).await;
        eventually("import paused", || world.agents.archive.importing.load(Ordering::SeqCst)).await;

        world.send(text(Some(8), 5, "/restore snap")).await;

        let refused = format!("{}\n{}", texts::SESSION_RESERVED, texts::RESTORE_FILES_KEPT);
        eventually("refused", || count(&world, &refused) == 1).await;
        assert_eq!(bound_to(&world, KEY), SessionId::parse("s-1"));
        release.send(()).unwrap();
        eventually("other restored", || said(&world, "♻️ Восстановлено: other")).await;
        world.send(text(Some(8), 6, "/restore snap")).await;
        eventually("rebound", || said(&world, "♻️ Восстановлено: snap")).await;
        assert_eq!(world.agents.archive.imports(), ["s-1", "s-2"], "the repeat imported nothing");
        assert_eq!(bound_to(&world, OTHER), SessionId::parse("s-1"));
    }
}
