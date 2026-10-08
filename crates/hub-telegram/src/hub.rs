//! The hub actor: one task owns every topic's state and handles messages one at a time
//! without waiting on the network; sessions run in their own tasks and report back.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_agent::conversation::Conversation;
use hub_core::commands::{
    BackendDecision, Command, Input, NewSessionArgs, decide_backend, join_path_args, parse_input,
    parse_new_args,
};
use hub_core::domain::{
    AbsolutePath, ChatId, MessageId, Prompt, SessionId, TopicKey, TopicSession,
};
use hub_core::settings::Settings;
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
use crate::texts;

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
    Shutdown(oneshot::Sender<()>),
}

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
                if !self.running.contains_key(&key)
                    && let Some(session) = self.topics.get(&key).cloned()
                {
                    self.put(key, session.with_session(None));
                }
            }
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
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use hub_core::domain::{
        AgentEvent, BackendKind, ChatId, Finished, Question, Selection, ThreadId, Usage, UserId,
    };
    use hub_core::questions::{Answer, QuestionId};
    use hub_core::settings::Draft;

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
    struct MemoryStore(Arc<StdMutex<Vec<Topics>>>);

    impl MemoryStore {
        fn last(&self) -> Option<Topics> {
            self.0.lock().unwrap().last().cloned()
        }
    }

    impl TopicStore for MemoryStore {
        fn save(&mut self, topics: &Topics) -> Result<(), String> {
            self.0.lock().unwrap().push(topics.clone());
            Ok(())
        }
    }

    struct World {
        handle: HubHandle,
        messenger: Arc<FakeMessenger>,
        agents: Arc<FakeAgents>,
        store: MemoryStore,
        _settings: watch::Sender<Arc<Settings>>,
        _root: tempfile::TempDir,
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
        World { handle, messenger, agents, store, _settings: settings_tx, _root: root }
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
            world
                .texts()
                .contains(&"⚠️ Неизвестный бэкенд gpt. Доступны: claude, codex, qwen".to_owned())
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
}
