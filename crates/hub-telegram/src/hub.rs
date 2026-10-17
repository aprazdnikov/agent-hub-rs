//! The hub actor: one task owns every topic's state and handles messages one at a time
//! without waiting on the network; sessions run in their own tasks and report back.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_agent::archive::SessionArchive;
use hub_agent::conversation::Conversation;
use hub_core::commands::{
    BackendDecision, Command, Input, NewSessionArgs, RestoreArgs, SaveArgs, decide_backend,
    join_path_args, parse_input, parse_new_args, parse_restore_args, parse_save_args,
};
use hub_core::domain::{
    AbsolutePath, BackendKind, ChatId, MessageId, Prompt, SessionId, TopicKey, TopicSession,
};
use hub_core::quota::QuotaReport;
use hub_core::settings::Settings;
use hub_core::snapshot::{SnapshotName, default_name};
use hub_core::topics::Topics;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;

use crate::channel::{Shared, lock};
use crate::download::download;
use crate::inbound::{Content, Inbound, Press, Turn};
use crate::messenger::{Messenger, Target};
use crate::paths::{CwdError, resolve_cwd, workspace_root};
use crate::press;
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopicState {
    Waiting,
    Running,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicView {
    pub key: TopicKey,
    pub title: Option<String>,
    pub session: TopicSession,
    pub state: TopicState,
}

pub enum HubMessage {
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
    Shutdown(oneshot::Sender<()>),
}

/// A message of a snapshot job; only the hub reads it.
pub struct SnapshotDone(pub(crate) Done);

pub struct HubSetup<A> {
    pub agents: Arc<A>,
    pub messenger: Arc<dyn Messenger>,
    pub settings: watch::Receiver<Arc<Settings>>,
    pub topics: Topics,
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
    inbox: mpsc::Sender<Prompt>,
    cancel: CancellationToken,
    generation: u64,
}

struct Hub<A> {
    agents: Arc<A>,
    sender: Sender,
    registries: Shared,
    settings: watch::Receiver<Arc<Settings>>,
    topics: Topics,
    store: Box<dyn TopicStore>,
    home: PathBuf,
    bot: String,
    running: HashMap<TopicKey, Live>,
    albums: HashMap<String, Vec<Inbound>>,
    titles: HashMap<TopicKey, String>,
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
}

#[must_use]
pub fn spawn<A: Agents>(setup: HubSetup<A>) -> HubHandle {
    let HubSetup { agents, messenger, settings, topics, store, home, bot } = setup;
    let (mailbox, inbox) = mpsc::channel(MAILBOX);
    let (views, watched) = watch::channel(Vec::new());
    let registries = Shared::default();
    let hub = Hub {
        agents,
        sender: Sender::new(messenger),
        registries: Arc::clone(&registries),
        settings,
        topics,
        store,
        home,
        bot,
        running: HashMap::new(),
        albums: HashMap::new(),
        titles: HashMap::new(),
        generation: 0,
        mailbox: mailbox.clone(),
        views,
        shutdown: None,
        snapshotting: HashSet::new(),
        naming: HashSet::new(),
        reserved: HashMap::new(),
    };
    let task = tokio::spawn(hub.run(inbox));
    HubHandle { mailbox, views: watched, registries, task }
}

impl<A: Agents> Hub<A> {
    async fn run(mut self, mut mailbox: mpsc::Receiver<HubMessage>) {
        self.publish();
        loop {
            let deadline = self.shutdown.as_ref().map(|(_, at)| *at);
            let message = tokio::select! {
                message = mailbox.recv() => message,
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => None,
            };
            let Some(message) = message else { break };
            self.handle(message);
            if self.shutdown.is_some() && self.running.is_empty() {
                break;
            }
        }
        if let Some((done, _)) = self.shutdown.take() {
            // The caller may have given up waiting; there is nobody else to tell.
            let _ = done.send(());
        }
    }

    fn handle(&mut self, message: HubMessage) {
        match message {
            HubMessage::Inbound(inbound) => {
                if self.shutdown.is_none() {
                    self.inbound(inbound);
                }
            }
            HubMessage::Press(pressed) => {
                tokio::spawn(press::handle(
                    Arc::clone(&self.registries),
                    self.sender.clone(),
                    pressed,
                ));
            }
            HubMessage::Deliver { key, prompt } => self.deliver(key, prompt),
            HubMessage::FlushAlbum(group) => {
                let parts = self.albums.remove(&group).unwrap_or_default();
                if let Some(key) = parts.first().and_then(Inbound::key) {
                    self.start_turn(key, Turn::from_parts(&parts));
                }
            }
            HubMessage::Bind { key, session } => self.bind(key, session),
            HubMessage::Ended { key, generation, inbox } => self.ended(key, generation, inbox),
            HubMessage::Stop(key) => {
                if let Some(live) = self.running.get(&key) {
                    live.cancel.cancel();
                }
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
                for live in self.running.values() {
                    live.cancel.cancel();
                }
                self.shutdown = Some((done, Instant::now() + SHUTDOWN_TIMEOUT));
            }
        }
        self.publish();
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
                self.titles.insert(key, name.clone());
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
            Content::Text(text) => match parse_input(text, &self.bot) {
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

    fn outside_topic(&self, inbound: Inbound) {
        let target = Target::Reply { chat: inbound.chat, message: inbound.message };
        match inbound.content {
            Content::Text(text) => match parse_input(&text, &self.bot) {
                Input::Command { command: Command::Help, .. } => {
                    let root = self.settings.borrow().workspace_root.clone();
                    self.say(target, texts::help(&root));
                }
                Input::Command { .. } | Input::Text => {
                    self.say(target, texts::CREATE_TOPIC.to_owned());
                }
                Input::Unknown => {}
            },
            Content::Media { .. } | Content::Voice => {
                self.say(target, texts::CREATE_TOPIC.to_owned());
            }
            Content::TopicCreated { .. } | Content::Ignored => {}
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
            Command::Stop => match self.running.get(&key) {
                Some(live) => live.cancel.cancel(),
                None => self.say(Target::Topic(key), texts::NOTHING_TO_STOP.to_owned()),
            },
            Command::Status => match self.topics.get(&key) {
                None => self.say(Target::Topic(key), texts::NO_SESSION.to_owned()),
                Some(session) => {
                    let state = if self.running.contains_key(&key) {
                        texts::RUNNING
                    } else {
                        texts::WAITING
                    };
                    self.say(Target::Topic(key), texts::describe(state, session));
                }
            },
            Command::Save => self.save(key, args),
            Command::Saves => self.saves(key),
            Command::Restore => self.restore(key, args),
            Command::Limits => self.say(Target::Topic(key), texts::LIMITS_PENDING.to_owned()),
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
        let settings = Arc::clone(&self.settings.borrow());
        let (inbox, receiver) = reuse.unwrap_or_else(|| mpsc::channel(INBOX));
        self.generation += 1;
        let cancel = CancellationToken::new();
        self.running
            .insert(key, Live { inbox, cancel: cancel.clone(), generation: self.generation });
        let run = Run {
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
        self.say(Target::Topic(key), texts::restored(&name, &session));
        for holder in holders {
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
        self.topics.insert(key, session);
        if let Err(error) = self.store.save(&self.topics) {
            tracing::error!(%error, "topics could not be saved");
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

    fn say(&self, target: Target, text: String) {
        let sender = self.sender.clone();
        tokio::spawn(async move { sender.text(target, &text).await });
    }

    fn publish(&self) {
        let views = self
            .topics
            .iter()
            .map(|(key, session)| TopicView {
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
        self.views.send_replace(views);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashSet, VecDeque};
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use hub_agent::archive::{ArchiveError, Exported};
    use hub_core::domain::{
        AgentEvent, BackendKind, ChatId, Finished, Question, Selection, ThreadId, Usage, UserId,
    };
    use hub_core::questions::{Answer, QuestionId};
    use hub_core::settings::Draft;
    use hub_core::snapshot::{BackendData, RelPath};

    use super::*;
    use crate::inbound::{Attachment, FileRef};
    use crate::testing::{Call, FakeMessenger};

    const KEY: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };

    enum Script {
        Say(Vec<AgentEvent>),
        UntilStopped,
        Collect(usize),
    }

    #[derive(Default)]
    struct FakeAgents {
        scripts: StdMutex<VecDeque<Script>>,
        prompts: StdMutex<Vec<String>>,
        archive: Arc<FakeArchive>,
        /// The next `archive` lookup panics, as a bug in a backend would.
        crash_archive: AtomicBool,
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
            Box::pin(async { Vec::new() })
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
        ) -> BoxFuture<'a, mpsc::Receiver<Prompt>> {
            self.prompts.lock().unwrap().push(prompt.text().to_owned());
            let script =
                self.scripts.lock().unwrap().pop_front().unwrap_or(Script::Say(Vec::new()));
            Box::pin(async move {
                match script {
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
        fail: Arc<AtomicBool>,
    }

    impl MemoryStore {
        /// What a restart would load: the last bindings that reached the "disk".
        fn last(&self) -> Option<Topics> {
            self.saved.lock().unwrap().last().cloned()
        }
    }

    impl TopicStore for MemoryStore {
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
        settings: watch::Sender<Arc<Settings>>,
        root: tempfile::TempDir,
    }

    impl World {
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
        let handle = spawn(HubSetup {
            agents: Arc::clone(&agents),
            messenger: Arc::clone(&messenger) as Arc<dyn Messenger>,
            settings,
            topics: Topics::new(),
            store: Box::new(store.clone()),
            home: std::env::temp_dir(),
            bot: "agent_hub_bot".to_owned(),
        });
        World { handle, messenger, agents, store, settings: settings_tx, root }
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

    #[tokio::test]
    async fn message_runs_the_agent_and_relays_its_events() {
        let world = world(FakeAgents::scripted(vec![Script::Say(vec![
            AgentEvent::SessionStarted(SessionId::parse("s-1").unwrap()),
            AgentEvent::AssistantText("**готово**".to_owned()),
            finished(),
        ])]));
        world.send(text(Some(7), 1, "сделай")).await;

        eventually("finished", || world.texts().iter().any(|t| t == "✅ Готово · ходов: 1")).await;
        assert!(world.texts().contains(&"<b>готово</b>".to_owned()));
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
                &"⚠️ Неизвестный бэкенд gpt. Доступны: claude, codex, qwen, hermes".to_owned(),
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
        world.send(HubMessage::Bind { key, session: SessionId::parse(id).unwrap() }).await;
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
        assert!(world.texts().iter().any(|t| t.starts_with(texts::WAITING) && t.ends_with("s-1")));
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
            .send(inbound(Some(7), 2, Content::TopicCreated { name: "Починить тесты".to_owned() }))
            .await;
        bound(&world, KEY, "s-1").await;
        world.send(text(Some(7), 3, "/save")).await;
        eventually("saved", || said(&world, "💾 Сохранено: pochinit-testy-")).await;

        world.send(text(Some(7), 4, "/saves")).await;
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
