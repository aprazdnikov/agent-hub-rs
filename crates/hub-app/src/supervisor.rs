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
use crate::local_execution::Control;
use crate::logging::Repaint;

/// The hub itself gives up on sessions after 10 s; the rest covers the dispatcher.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(15);
/// First pause before reconnecting after a network failure; it doubles up to `RETRY_MAX`.
pub const RETRY_FIRST: Duration = Duration::from_secs(5);
pub const RETRY_MAX: Duration = Duration::from_mins(5);
const OUTBOX_REFRESH: Duration = Duration::from_secs(5);

pub enum Command {
    RefreshTaskIndex,
    LocalStop(uuid::Uuid),
    LocalAnswer {
        token: uuid::Uuid,
        id: uuid::Uuid,
        answer: crate::local_execution::Answer,
        reply: oneshot::Sender<Result<(), String>>,
    },
    LocalTasks {
        project: PathBuf,
        backend: BackendKind,
        request: hub_telegram::hub::tasks::Request,
        reply: oneshot::Sender<Result<hub_telegram::hub::tasks::Snapshot, String>>,
    },
    Tasks {
        key: TopicKey,
        project: PathBuf,
        backend: BackendKind,
        request: hub_telegram::hub::tasks::Request,
        reply: oneshot::Sender<Result<hub_telegram::hub::tasks::Snapshot, String>>,
    },
    SetMemory {
        enabled: bool,
        revision: u64,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Save(Box<Draft>),
    Start,
    Stop,
    Diagnose,
    TopicMemory {
        key: TopicKey,
        value: Option<bool>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    StopTopic(TopicKey),
    ResetTopic(TopicKey),
    DeleteTopic {
        key: TopicKey,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Removes a project with all of its hub-side data; the folder itself stays intact.
    DeleteProject {
        id: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    SyncTopics {
        reply: oneshot::Sender<Result<String, String>>,
    },
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
    /// `Pi` keeps its subscriptions in `~/.pi/agent/auth.json`, which the hub never reads.
    Pi,
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
    pub local_only: bool,
    pub task_index: Result<Vec<crate::desktop_tasks::TaskRow>, String>,
    pub bot: BotStatus,
    pub local: Option<crate::local_execution::Live>,
    pub local_jobs: Vec<crate::local_execution::Live>,
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
            local_only: false,
            task_index: Ok(Vec::new()),
            bot: BotStatus::Unconfigured,
            local: None,
            local_jobs: Vec::new(),
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
    if previous.is_some_and(|old| !old.telegram.configured()) && next.telegram.configured() {
        return Apply::Start;
    }
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

    fn task_service(&self) -> Option<Arc<hub_core::tasks::service::TaskService>> {
        None
    }

    fn attach_execution(
        &self,
        _execution: Arc<dyn hub_telegram::execution::Control>,
    ) -> Result<(), String> {
        Ok(())
    }

    fn local_executor(&self) -> Result<Arc<dyn crate::local_execution::Executor>, String> {
        let memory_program = std::env::current_exe().map_err(|error| error.to_string())?;
        Ok(Arc::new(crate::local_execution::CliExecutor { memory_program }))
    }

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
    memory_reply: Option<oneshot::Sender<Result<(), String>>>,
    /// Deferred Ok-reply of `delete_project`: the cascade finished its data steps, and the
    /// summary is only honest once the settings save that drops the profile has settled.
    project_reply: Option<(oneshot::Sender<Result<String, String>>, String)>,
    save_slow_at: Option<Instant>,
    stop_during_save: bool,
    home: PathBuf,
    settings_load_error: Option<String>,
    settings: Option<watch::Sender<Arc<Settings>>>,
    bot: Option<Bot>,
    connecting: Option<BoxFuture<'static, Result<Started, C::Error>>>,
    views: Option<watch::Receiver<Vec<TopicView>>>,
    retry: Option<Retry>,
    diagnostic_read: Option<BoxFuture<'static, crate::diagnostics::Report>>,
    outbox_read: Option<BoxFuture<'static, (ChatId, Result<usize, String>)>>,
    outbox_next: Instant,
    task_index_next: Instant,
    local: Result<Arc<crate::local_execution::SharedRuntime>, String>,
    state: Snapshot,
    snapshot: watch::Sender<Snapshot>,
    repaint: Repaint,
}

fn make_execution<C: Connector>(
    connector: &C,
    service: Arc<hub_core::tasks::service::TaskService>,
) -> Result<Arc<crate::local_execution::SharedRuntime>, String> {
    let runtime =
        Arc::new(crate::local_execution::SharedRuntime::new(crate::local_execution::Runtime::new(
            service,
            connector.local_executor()?,
            Arc::new(crate::git::LocalGitRunner),
        )));
    connector.attach_execution(runtime.clone())?;
    Ok(runtime)
}

struct SaveJob {
    store: Box<dyn SettingsStore>,
    draft: Draft,
    settings: Settings,
    result: Result<(), String>,
}

/// How the task book blocks deleting a project: running work cannot be interrupted, a
/// queued intent is about to start, and only their absence lets the cascade run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TaskBlock {
    None,
    Active,
    Queued,
}

fn task_blocks(book: &hub_core::tasks::TaskBook, shapes: &[std::path::PathBuf]) -> TaskBlock {
    let mut queued = false;
    for task in book.tasks() {
        if !shapes.contains(&task.project) {
            continue;
        }
        if task.active() {
            return TaskBlock::Active;
        }
        queued |= task.queue.as_ref().is_some_and(|entry| entry.error.is_none());
    }
    if queued { TaskBlock::Queued } else { TaskBlock::None }
}

/// The path shapes one saved project can be recorded under: the settings keep the
/// candidate form, while task creation canonicalizes through the workspace root. The
/// deletion guard, the task book cleanup and the GUI preview must agree on the same set,
/// or the modal promises rows the cascade never removes.
pub(crate) fn task_path_shapes(
    root: &std::path::Path,
    home: &std::path::Path,
    cwd: &str,
) -> Vec<std::path::PathBuf> {
    let path = hub_core::workspace::candidate(root, home, Some(cwd));
    let mut shapes = vec![path.clone()];
    for shape in [
        std::fs::canonicalize(&path).ok(),
        hub_telegram::paths::workspace_root(&path).ok().map(|root| root.as_path().to_path_buf()),
    ]
    .into_iter()
    .flatten()
    {
        if !shapes.contains(&shape) {
            shapes.push(shape);
        }
    }
    shapes
}

/// Everything the cascade needs resolved once: the settings candidate path for the
/// memory purge, the task book shapes to forget, and the other saved projects whose
/// shared memory storages must survive.
struct ProjectPaths {
    cwd: std::path::PathBuf,
    task_shapes: Vec<std::path::PathBuf>,
    living: Vec<std::path::PathBuf>,
}

impl ProjectPaths {
    fn resolve(settings: &Settings, home: &std::path::Path, id: &str, cwd_raw: &str) -> Self {
        let cwd = hub_core::workspace::candidate(&settings.workspace_root, home, Some(cwd_raw));
        let task_shapes = task_path_shapes(&settings.workspace_root, home, cwd_raw);
        let living = settings
            .projects
            .iter()
            .filter(|saved| saved.id() != id)
            .map(|saved| {
                hub_core::workspace::candidate(&settings.workspace_root, home, Some(saved.cwd()))
            })
            .collect();
        Self { cwd, task_shapes, living }
    }
}

#[derive(Clone, Copy)]
struct Retry {
    at: Instant,
    pause: Duration,
}

enum Event<E> {
    LocalTick,
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
        let settings_load_error = loaded.as_ref().err().map(ToString::to_string);
        let mut state = Snapshot::default();
        let settings = match loaded {
            Ok(Loaded::Missing) => None,
            Ok(Loaded::Ready(settings)) => {
                state.saved = Some(Draft::from_settings(&settings));
                state.local_only = !settings.telegram.configured();
                state.bot = if settings.telegram.configured() {
                    BotStatus::Stopped
                } else {
                    BotStatus::Unconfigured
                };
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
        let local = connector
            .task_service()
            .ok_or_else(|| "Локальный сервис задач недоступен".to_owned())
            .and_then(|service| make_execution(&connector, service));
        Self {
            connector,
            local,
            store: Some(store),
            save_guard,
            saving: None,
            memory_reply: None,
            project_reply: None,
            save_slow_at: None,
            stop_during_save: false,
            home,
            settings_load_error,
            settings,
            bot: None,
            connecting: None,
            views: None,
            retry: None,
            diagnostic_read: None,
            outbox_read: None,
            task_index_next: Instant::now(),
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
                () = tokio::time::sleep(Duration::from_millis(200)), if self.local.is_ok() => Event::LocalTick,
                alive = changed => Event::Views { alive },
                () = sleep_until(retry.unwrap_or_else(Instant::now)), if retry.is_some() && can_retry => Event::Retry,
                result = startup => Event::Started(result),
                result = saved => Event::Saved(Box::new(result)),
                () = sleep_until(save_slow_at.unwrap_or_else(Instant::now)), if save_slow_at.is_some() => Event::SaveDelayed,
                (chat, result) = reading => Event::OutboxRead(chat, result),
                report = diagnosing => Event::DiagnosticDone(report),
                () = sleep_until(self.outbox_next), if self.telegram_configured() && can_read => Event::ReadOutbox,
            };
            self.saving = saving;
            self.connecting = connecting;
            self.views = views;
            self.outbox_read = outbox_read;
            self.diagnostic_read = diagnostic_read;
            if !self.apply_event(event).await {
                return;
            }
        }
    }

    async fn apply_event(&mut self, event: Event<C::Error>) -> bool {
        match event {
            Event::LocalTick => {
                self.pump_queue();
                if Instant::now() >= self.task_index_next {
                    self.refresh_task_index();
                }
                let local = self.local.as_ref().ok().and_then(|local| local.snapshot());
                let jobs = self.local.as_ref().map(|r| r.snapshots()).unwrap_or_default();
                if local != self.state.local || jobs != self.state.local_jobs {
                    self.state.local_jobs = jobs;
                    self.state.local = local;
                    self.publish();
                }
            }
            Event::Command(Some(Command::Quit(done))) => {
                if let Ok(local) = &mut self.local {
                    local.shutdown().await;
                }
                self.stop().await;
                // The window may have stopped waiting; there is nobody else to tell.
                let _ = done.send(());
                return false;
            }
            Event::Command(None) => {
                if let Ok(local) = &mut self.local {
                    local.shutdown().await;
                }
                self.stop().await;
                return false;
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
        true
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

    fn refresh_task_index(&mut self) {
        self.task_index_next = Instant::now() + Duration::from_secs(3);
        if let Some(service) = self.connector.task_service() {
            let root = self.state.saved.as_ref().map(|draft| draft.workspace_root.as_str());
            let index = crate::desktop_tasks::index(&service, root, &self.home);
            if self.state.task_index != index {
                self.state.task_index = index;
                self.publish();
            }
        }
    }

    fn pump_queue(&mut self) {
        if self.mailbox().is_some()
            || self.connecting.is_some()
            || self.saving.is_some()
            || self.settings_load_error.is_some()
        {
            return;
        }
        let Some(service) = self.connector.task_service() else {
            return;
        };
        let Some(pending) = service.pending(false) else {
            return;
        };
        if !self.local.as_ref().is_ok_and(|r| r.capacity(pending.isolated)) {
            return;
        }
        let request = hub_telegram::hub::tasks::Request::RunQueued {
            id: pending.id,
            revision: pending.revision,
        };
        if let Err(error) = self.local_request(&pending.project, BackendKind::Codex, request)
            && let Err(error) = service.pause_pending(&pending, &error)
        {
            tracing::warn!(%error,"cannot pause queued task");
        }
    }
    fn local_request(
        &mut self,
        project: &std::path::Path,
        backend: BackendKind,
        request: hub_telegram::hub::tasks::Request,
    ) -> Result<hub_telegram::hub::tasks::Snapshot, String> {
        use hub_telegram::hub::tasks::Request;
        let service = self.connector.task_service().ok_or("Локальный сервис задач недоступен")?;
        let root = self.state.saved.as_ref().map(|draft| draft.workspace_root.as_str());
        let id = match &request {
            Request::RunQueued { id, .. }
            | Request::StartChecked { id, .. }
            | Request::PrepareGitChecked { id, .. } => Some(*id),
            _ => None,
        };
        if let Some(id) = id {
            if self.connecting.is_some() {
                return Err(
                    "Дождитесь подключения бота или остановите подключение перед запуском.".into(),
                );
            }
            if self.saving.is_some() {
                return Err("Дождитесь сохранения настроек".into());
            }
            let snapshot = crate::desktop_tasks::request(
                &service,
                project,
                backend,
                Request::Get(id),
                root,
                &self.home,
            )?;
            let project = snapshot.tasks.first().ok_or("Задача не найдена")?.project.clone();
            let settings = self
                .state
                .saved
                .clone()
                .unwrap_or_default()
                .execution(&self.home)
                .map_err(|errors| {
                    errors.iter().map(|error| error.message.as_str()).collect::<Vec<_>>().join("; ")
                })?;
            if self.local.is_err() {
                self.local = make_execution(&self.connector, service.clone());
            }
            let queued = matches!(request, Request::RunQueued { .. });
            self.local.as_mut().map_err(|error| error.clone())?.dispatch(
                &project,
                request,
                Arc::new(settings),
            )?;
            if !queued {
                self.retry = None;
                if matches!(self.state.bot, BotStatus::Retrying { .. }) {
                    self.state.bot = BotStatus::Stopped;
                }
            }
            return crate::desktop_tasks::request(
                &service,
                &project,
                backend,
                Request::Get(id),
                root,
                &self.home,
            );
        }
        crate::desktop_tasks::request(&service, project, backend, request, root, &self.home)
    }

    async fn handle(&mut self, command: Command) {
        match command {
            Command::RefreshTaskIndex => self.refresh_task_index(),
            Command::LocalStop(token) => {
                let result =
                    self.local.as_ref().map_err(Clone::clone).and_then(|local| local.stop(token));
                if let Err(error) = result {
                    self.notice(Some(error));
                }
            }
            Command::LocalAnswer { token, id, answer, reply } => {
                let result = self
                    .local
                    .as_ref()
                    .map_err(Clone::clone)
                    .and_then(|local| local.respond(token, id, answer));
                let _ = reply.send(result);
            }
            Command::LocalTasks { project, backend, request, reply } => {
                self.handle_local_tasks(project, backend, request, reply);
            }
            Command::Tasks { key, project, backend, request, reply } => {
                if let Some(mailbox) = self.mailbox() {
                    let message = HubMessage::Tasks { key, project, backend, request, reply };
                    if let Err(error) = mailbox.try_send(message)
                        && let HubMessage::Tasks { reply, .. } = error.into_inner()
                    {
                        let _ =
                            reply.send(Err("Бот занят или остановлен. Повторите запрос.".into()));
                    }
                } else {
                    let _ = reply.send(Err("Для управления задачами запустите бота.".into()));
                }
            }
            Command::SetMemory { enabled, revision, reply } => {
                self.set_memory_default(enabled, revision, reply);
            }
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
                if self.telegram_configured() {
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
            Command::TopicMemory { key, value, reply } => {
                if let Some(mailbox) = self.mailbox() {
                    let _ = mailbox.send(HubMessage::TopicMemory { key, value, reply }).await;
                } else {
                    let _ = reply.send(Err("Для настройки темы запустите бота.".into()));
                }
            }
            Command::StopTopic(key) => tell(self.mailbox(), HubMessage::Stop(key)).await,
            Command::DeleteTopic { key, reply } => {
                if let Some(mailbox) = self.mailbox() {
                    if let Err(error) = mailbox.try_send(HubMessage::DeleteTopic { key, reply })
                        && let HubMessage::DeleteTopic { reply, .. } = error.into_inner()
                    {
                        let _ =
                            reply.send(Err("Бот занят или остановлен. Повторите удаление.".into()));
                    }
                } else {
                    let _ = reply.send(Err("Для удаления темы запустите бота.".into()));
                }
            }
            Command::DeleteProject { id, reply } => self.delete_project(id, reply).await,
            Command::SyncTopics { reply } => {
                if let Some(mailbox) = self.mailbox() {
                    if let Err(error) = mailbox.try_send(HubMessage::SyncTopics { reply })
                        && let HubMessage::SyncTopics { reply } = error.into_inner()
                    {
                        let _ = reply
                            .send(Err("Бот занят или остановлен. Повторите синхронизацию.".into()));
                    }
                } else {
                    let _ = reply.send(Err("Для синхронизации тем запустите бота.".into()));
                }
            }
            Command::ResetTopic(key) => tell(self.mailbox(), HubMessage::Reset(key)).await,
            Command::Quit(done) => {
                // `run` handles Quit before dispatching here.
                let _ = done.send(());
            }
        }
    }

    fn set_memory_default(
        &mut self,
        enabled: bool,
        revision: u64,
        reply: oneshot::Sender<Result<(), String>>,
    ) {
        if self.saving.is_some() || self.store.is_none() || revision != self.state.save_revision {
            let _ = reply
                .send(Err("Настройки изменились или сохраняются. Обновите и повторите".into()));
        } else if let Some(mut draft) = self.state.saved.clone() {
            draft.memory.enabled = enabled;
            self.save(draft);
            if self.saving.is_some() {
                self.memory_reply = Some(reply);
            } else {
                let _ = reply.send(Err(self
                    .state
                    .notice
                    .clone()
                    .unwrap_or_else(|| "Сохранение не начато".into())));
            }
        } else {
            let _ = reply.send(Err("Сначала сохраните настройки приложения".into()));
        }
    }

    /// Idempotent cascade of a deleted project: topics and the recent list unbind via the
    /// hub actor (its only writer of `topics.json`), the task book forgets tasks and rules,
    /// memory purges only inside its own base, and the profile goes last through the usual
    /// settings `save`. Any step failure stops before the profile and says what already ran,
    /// so pressing the button again resumes the cascade. A vanished project folder stays
    /// deletable: every step tolerates data that is already gone.
    async fn delete_project(&mut self, id: String, reply: oneshot::Sender<Result<String, String>>) {
        let Some(live) = &self.settings else {
            let _ = reply
                .send(Err("Удаление недоступно: сначала сохраните настройки приложения.".into()));
            return;
        };
        let settings = Arc::clone(&live.borrow());
        let Some(project) = settings.projects.iter().find(|saved| saved.id() == id) else {
            let _ = reply.send(Ok(
                "Профиль проекта уже удалён из настроек. Данные хаба (задачи, память) могли остаться — проверьте их вручную.".into(),
            ));
            return;
        };
        let name = project.name().to_owned();
        // One resolver for guard, task book and memory: the settings candidate form, its
        // canonical folder and the workspace-root form all identify the same project.
        let paths = ProjectPaths::resolve(&settings, &self.home, &id, project.cwd());
        let Some(service) = self.connector.task_service() else {
            let _ = reply.send(Err("Сервис задач недоступен. Перезапустите приложение.".into()));
            return;
        };
        let refusal = match service.inspect(|book| task_blocks(book, &paths.task_shapes)) {
            Ok(TaskBlock::None) => None,
            Ok(TaskBlock::Active) => Some(format!(
                "Есть активные задачи проекта «{name}». Дождитесь завершения или остановите их и повторите удаление."
            )),
            // The queue analogue of the topic guard: a queued task is about to run, and
            // dropping it silently would lose declared work.
            Ok(TaskBlock::Queued) => Some(format!(
                "В очереди проекта «{name}» есть незапущенные задачи. Дождитесь их выполнения или снимите их с очереди и повторите удаление."
            )),
            Err(error) => Some(format!("Не удалось проверить задачи проекта: {error}")),
        }
        // A live git preparation or check holds the service lease; refuse before any step.
        .or_else(|| {
            service
                .reserve()
                .err()
                .map(|error| format!("Проект занят: {error}. Дождитесь завершения и повторите удаление."))
        });
        if let Some(error) = refusal {
            let _ = reply.send(Err(error));
            return;
        }
        let (topics, tasks, purged) = match self.forget_project_data(&id, paths, service).await {
            Ok(counts) => counts,
            Err((error, done)) => {
                let _ = reply.send(Err(if done.is_empty() {
                    error
                } else {
                    format!(
                        "{error} Уже выполнено: {}. Профиль проекта не удалён.",
                        done.join("; ")
                    )
                }));
                return;
            }
        };
        let mut draft = Draft::from_settings(&settings);
        draft.projects.retain(|saved| saved.id != id);
        let memory = if purged.shared {
            "память оставлена: она общая с другим проектом"
        } else if purged.had_data {
            "память удалена"
        } else {
            "памяти не было"
        };
        let summary = format!(
            "Проект «{name}» удалён: задач снято — {tasks}, тем отвязано — {topics}, записей реестра памяти снято — {}, {memory}. Профиль удаляется сохранением настроек.",
            purged.removed_paths
        );
        self.save(draft);
        // The Ok-reply rides on the settings save, the way the memory toggle's does: until
        // the save settles the profile is not yet gone, and a rejected save must not claim
        // the project was deleted.
        if self.saving.is_some() {
            self.project_reply = Some((reply, summary));
        } else {
            let _ = reply.send(Err(format!(
                "Данные проекта сняты, но сохранение настроек не началось: {}. Повторите удаление после завершения текущего сохранения.",
                self.state.notice.clone().unwrap_or_else(|| "причина неизвестна".to_owned())
            )));
        }
    }

    /// Data steps of the cascade (topics → tasks → memory), returning their counters or the
    /// failure plus what had already completed.
    async fn forget_project_data(
        &mut self,
        id: &str,
        paths: ProjectPaths,
        service: Arc<hub_core::tasks::service::TaskService>,
    ) -> Result<(usize, usize, hub_memory::PurgeReport), (String, Vec<String>)> {
        let Some(mailbox) = self.mailbox() else {
            return Err(("Для отвязки тем от проекта запустите бота.".to_owned(), Vec::new()));
        };
        let (tx, rx) = oneshot::channel();
        if mailbox.send(HubMessage::ForgetProject { id: id.to_owned(), reply: tx }).await.is_err() {
            return Err((
                "Бот остановлен. Повторите удаление после запуска.".to_owned(),
                Vec::new(),
            ));
        }
        let topics = match rx.await {
            Ok(Ok(count)) => count,
            Ok(Err(error)) => return Err((error, Vec::new())),
            Err(_) => {
                return Err((
                    "Бот не ответил на команду отвязки. Повторите удаление.".to_owned(),
                    Vec::new(),
                ));
            }
        };
        let done = vec![format!("темы отвязаны — {topics}")];
        let tasks = match service.access(hub_core::tasks::service::Access::Edit, |book, _busy| {
            Ok(paths.task_shapes.iter().map(|shape| book.forget_project(shape).0).sum())
        }) {
            Ok(removed) => removed,
            Err(error) => {
                return Err((format!("Не удалось снять задачи проекта: {error}"), done));
            }
        };
        let done = vec![format!("темы отвязаны — {topics}"), format!("задачи сняты — {tasks}")];
        let base = hub_memory::base_in(&self.home);
        let purged = match tokio::task::spawn_blocking(move || {
            hub_memory::purge(&base, &paths.cwd, &paths.living)
        })
        .await
        {
            Ok(Ok(report)) => report,
            Ok(Err(error)) => {
                return Err((
                    format!("Не удалось удалить память проекта: {}", error.user_message()),
                    done,
                ));
            }
            Err(error) => {
                return Err((format!("Память проекта не проверена: {error}"), done));
            }
        };
        Ok((topics, tasks, purged))
    }

    fn handle_local_tasks(
        &mut self,
        project: std::path::PathBuf,
        backend: hub_core::domain::BackendKind,
        request: hub_telegram::hub::tasks::Request,
        reply: tokio::sync::oneshot::Sender<Result<hub_telegram::hub::tasks::Snapshot, String>>,
    ) {
        if let Some(error) = &self.settings_load_error {
            let _ = reply.send(Err(format!("Не удалось прочитать настройки: {error}. Исправьте и сохраните настройки либо перезапустите приложение после исправления файла.")));
            return;
        }
        if matches!(
            request,
            hub_telegram::hub::tasks::Request::StartChecked { .. }
                | hub_telegram::hub::tasks::Request::PrepareGitChecked { .. }
        ) && let Some(mailbox) = self.mailbox()
        {
            let message = HubMessage::SharedTasks { project, request, reply };
            if let Err(error) = mailbox.try_send(message)
                && let HubMessage::SharedTasks { reply, .. } = error.into_inner()
            {
                let _ = reply.send(Err("Бот занят или остановлен. Повторите запрос.".into()));
            }
            return;
        }
        let result = self.local_request(&project, backend, request);
        self.state.local = self.local.as_ref().ok().and_then(|local| local.snapshot());
        self.publish();
        let _ = reply.send(result);
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
        let memory_reply = self.memory_reply.take();
        let project_reply = self.project_reply.take();
        let Ok(SaveJob { store, draft, settings, result }) = outcome else {
            if let Some(reply) = memory_reply {
                let _ = reply.send(Err("Результат сохранения неизвестен".into()));
            }
            if let Some((reply, _)) = project_reply {
                let _ = reply.send(Err(
                    "Результат сохранения неизвестен; проверьте список проектов и повторите удаление при необходимости."
                        .into(),
                ));
            }
            self.state.save_status = SaveStatus::Uncertain;
            self.notice(Some(
                "Результат сохранения неизвестен; проверьте настройки после перезапуска".to_owned(),
            ));
            return;
        };
        self.store = Some(store);
        self.state.save_status = SaveStatus::Idle;
        if let Err(error) = result {
            if let Some(reply) = memory_reply {
                let _ = reply.send(Err(error.clone()));
            }
            if let Some((reply, _)) = project_reply {
                let _ = reply.send(Err(format!(
                    "Данные проекта сняты, но профиль удалить не удалось: {error}. Повторите удаление — шаги идемпотентны."
                )));
            }
            self.notice(Some(error));
            return;
        }
        self.cancel_diagnostic();
        tracing::info!("settings saved");
        self.settings_load_error = None;
        self.retry = None;
        self.state.saved = Some(draft);
        self.state.local_only = !settings.telegram.configured();
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
        if settings.telegram.configured() && matches!(apply, Apply::Restart | Apply::Start) {
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
        if let Some(reply) = memory_reply {
            let _ = reply.send(Ok(()));
        }
        if let Some((reply, summary)) = project_reply {
            let _ = reply.send(Ok(summary));
        }
        if !self.telegram_configured() {
            self.connecting = None;
            self.stop().await;
            self.status(BotStatus::Unconfigured);
            return;
        }
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

    fn telegram_configured(&self) -> bool {
        self.settings.as_ref().is_some_and(|s| s.borrow().telegram.configured())
    }

    fn start(&mut self) {
        if !self.telegram_configured() {
            self.status(BotStatus::Unconfigured);
            return;
        }
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
        tasks: StdMutex<Option<Arc<hub_core::tasks::service::TaskService>>>,
        connects: AtomicUsize,
        stops: AtomicUsize,
        messages: StdMutex<Vec<String>>,
        settings: StdMutex<Option<watch::Receiver<Arc<Settings>>>>,
        views: StdMutex<Option<watch::Sender<Vec<TopicView>>>>,
        counts: StdMutex<VecDeque<oneshot::Receiver<Result<usize, String>>>>,
        starts: StdMutex<VecDeque<oneshot::Receiver<()>>>,
        diagnostics: StdMutex<VecDeque<oneshot::Receiver<crate::diagnostics::Report>>>,
        forget: StdMutex<VecDeque<Result<usize, String>>>,
    }

    struct BlockingLocal;
    impl crate::local_execution::Executor for BlockingLocal {
        fn run<'a>(
            &'a self,
            _: &'a hub_core::domain::TopicSession,
            _: hub_core::domain::Prompt,
            conversation: hub_agent::conversation::Conversation,
            _: Arc<hub_core::settings::ExecutionSettings>,
        ) -> BoxFuture<'a, ()> {
            Box::pin(async move {
                conversation.cancel.cancelled().await;
                tokio::time::sleep(Duration::from_millis(300)).await;
            })
        }
    }
    struct FakeConnector {
        probe: Arc<Probe>,
        outcomes: StdMutex<VecDeque<Outcome>>,
    }

    impl Connector for FakeConnector {
        type Error = String;

        fn task_service(&self) -> Option<Arc<hub_core::tasks::service::TaskService>> {
            self.probe.tasks.lock().unwrap().clone()
        }

        fn local_executor(&self) -> Result<Arc<dyn crate::local_execution::Executor>, String> {
            Ok(Arc::new(BlockingLocal))
        }

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
                            HubMessage::SyncTopics { .. } => "sync".to_owned(),
                            HubMessage::ForgetProject { id, reply } => {
                                let outcome =
                                    recorder.forget.lock().unwrap().pop_front().unwrap_or(Ok(0));
                                let _ = reply.send(outcome);
                                format!("forget_project {id}")
                            }
                            HubMessage::DeleteTopic { .. }
                            | HubMessage::DeletionDone(_)
                            | HubMessage::SyncDone(_)
                            | HubMessage::TaskResultUnavailable { .. }
                            | HubMessage::Tasks { .. }
                            | HubMessage::SharedTasks { .. }
                            | HubMessage::GitDone(_)
                            | HubMessage::TopicMemory { .. }
                            | HubMessage::MemoryDone(_)
                            | HubMessage::HandoffDone(_)
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

    fn create_local_task(
        service: &hub_core::tasks::service::TaskService,
        project: &std::path::Path,
    ) -> u64 {
        let created = crate::desktop_tasks::request(
            service,
            project,
            BackendKind::Qwen,
            hub_telegram::hub::tasks::Request::Create {
                title: "Read".into(),
                prompt: "Inspect".into(),
            },
            None,
            &home(),
        )
        .unwrap();
        created.tasks[0].id
    }

    #[tokio::test]
    async fn first_settings_save_connects_telegram_without_stopping_shared_work() {
        use hub_core::tasks::service::TaskService;
        use hub_telegram::hub::tasks::Request;
        let mut harness = harness(Ok(Loaded::Missing), &[], false);
        let root = tempfile::tempdir().unwrap();
        let project = home().join("projects");
        let service = Arc::new(TaskService::new(Box::new(crate::topics::FileTopics::new(
            root.path().join("topics.json"),
        ))));
        let id = create_local_task(&service, &project);
        *harness.probe.tasks.lock().unwrap() = Some(service);
        let (reply, result) = oneshot::channel();
        harness
            .send(Command::LocalTasks {
                project,
                backend: BackendKind::Qwen,
                request: Request::StartChecked { id, revision: 1, next: None, rules_revision: 0 },
                reply,
            })
            .await;
        result.await.unwrap().unwrap();
        harness.until("local started", |s| s.local.as_ref().is_some_and(|l| l.active)).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        harness.send(Command::Save(Box::new(draft("-100", "")))).await;
        harness.until("saved", |s| s.saved.is_some()).await;
        harness.until("bot connected", running).await;
        assert!(harness.snapshot.borrow().local.as_ref().unwrap().active);
        assert_eq!(harness.connects(), 1);
        let token = harness.snapshot.borrow().local.as_ref().unwrap().token;
        harness.send(Command::LocalStop(token)).await;
        harness.until("local ended", |s| s.local.as_ref().is_some_and(|l| !l.active)).await;
    }

    #[tokio::test]
    async fn local_execution_without_credentials_stops_on_quit_and_keeps_history() {
        use hub_core::tasks::{RunState, service::TaskService};
        use hub_telegram::hub::tasks::Request;
        let mut harness = harness(Ok(Loaded::Missing), &[], false);
        let root = tempfile::tempdir().unwrap();
        let service = Arc::new(TaskService::new(Box::new(crate::topics::FileTopics::new(
            root.path().join("topics.json"),
        ))));
        let id = create_local_task(&service, root.path());
        *harness.probe.tasks.lock().unwrap() = Some(service.clone());
        let (reply, result) = oneshot::channel();
        harness
            .send(Command::LocalTasks {
                project: root.path().to_path_buf(),
                backend: BackendKind::Qwen,
                request: Request::StartChecked { id, revision: 1, next: None, rules_revision: 0 },
                reply,
            })
            .await;
        result.await.unwrap().unwrap();
        harness.until("local running", |s| s.local.as_ref().is_some_and(|live| live.active)).await;
        assert_eq!(harness.connects(), 0);
        let (done, quit) = oneshot::channel();
        harness.send(Command::Quit(done)).await;
        tokio::time::timeout(Duration::from_secs(2), quit).await.unwrap().unwrap();
        let book = service.read().unwrap();
        // If quit beat the worker's durable begin there is no attempt; otherwise interrupted.
        assert!(book.get(id).unwrap().runs.iter().all(|run| run.state == RunState::Interrupted));
        assert_eq!(harness.connects(), 0);
    }

    #[tokio::test]
    async fn local_execution_keeps_running_when_bot_starts_and_stops() {
        use hub_core::tasks::service::TaskService;
        use hub_telegram::hub::tasks::Request;
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        harness.send(Command::Stop).await;
        harness.until("stopped", |s| s.bot == BotStatus::Stopped).await;
        let project = home().join("projects");
        let root = tempfile::tempdir().unwrap();
        let service = Arc::new(TaskService::new(Box::new(crate::topics::FileTopics::new(
            root.path().join("topics.json"),
        ))));
        let id = create_local_task(&service, &project);
        *harness.probe.tasks.lock().unwrap() = Some(service);
        let (reply, result) = oneshot::channel();
        harness
            .send(Command::LocalTasks {
                project,
                backend: BackendKind::Qwen,
                request: Request::StartChecked { id, revision: 1, next: None, rules_revision: 0 },
                reply,
            })
            .await;
        result.await.unwrap().unwrap();
        harness.until("local running", |s| s.local.as_ref().is_some_and(|l| l.active)).await;
        harness.send(Command::Start).await;
        harness.until("bot connected", running).await;
        assert!(harness.snapshot.borrow().local.as_ref().unwrap().active);
        assert_eq!(harness.connects(), 2);
        harness.send(Command::Stop).await;
        harness.until("bot stopped", |s| s.bot == BotStatus::Stopped).await;
        assert!(harness.snapshot.borrow().local.as_ref().unwrap().active);
        let token = harness.snapshot.borrow().local.as_ref().unwrap().token;
        harness.send(Command::LocalStop(token)).await;
        harness.until("local ended", |s| s.local.as_ref().is_some_and(|l| !l.active)).await;
    }

    #[tokio::test]
    async fn local_management_refuses_unknown_settings_until_explicit_save() {
        let mut harness = harness(
            Err(StoreError::Corrupt { path: "settings.toml".into(), reason: "invalid".into() }),
            &[],
            false,
        );
        let root = tempfile::tempdir().unwrap();
        let service = Arc::new(hub_core::tasks::service::TaskService::new(Box::new(
            crate::topics::FileTopics::new(root.path().join("topics.json")),
        )));
        *harness.probe.tasks.lock().unwrap() = Some(service.clone());
        let (reply, result) = oneshot::channel();
        harness
            .commands
            .send(Command::LocalTasks {
                project: root.path().to_path_buf(),
                backend: BackendKind::Qwen,
                request: hub_telegram::hub::tasks::Request::Create {
                    title: "Local".into(),
                    prompt: "Read".into(),
                },
                reply,
            })
            .await
            .unwrap();
        assert!(result.await.unwrap().is_err());
        assert!(service.read().unwrap().tasks().is_empty());
        harness.send(Command::Save(Box::new(draft("-100", "")))).await;
        harness.until("running", running).await;
        let (reply, result) = oneshot::channel();
        harness
            .commands
            .send(Command::LocalTasks {
                project: home().join("projects"),
                backend: BackendKind::Qwen,
                request: hub_telegram::hub::tasks::Request::List,
                reply,
            })
            .await
            .unwrap();
        assert!(result.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn local_management_works_without_settings_credentials_or_bot_connection() {
        let harness = harness(Ok(Loaded::Missing), &[], false);
        let root = tempfile::tempdir().unwrap();
        let service = Arc::new(hub_core::tasks::service::TaskService::new(Box::new(
            crate::topics::FileTopics::new(root.path().join("topics.json")),
        )));
        *harness.probe.tasks.lock().unwrap() = Some(service.clone());
        let (reply, result) = oneshot::channel();
        harness
            .commands
            .send(Command::LocalTasks {
                project: root.path().to_path_buf(),
                backend: BackendKind::Qwen,
                request: hub_telegram::hub::tasks::Request::Create {
                    title: "Local".into(),
                    prompt: "Read".into(),
                },
                reply,
            })
            .await
            .unwrap();
        let snapshot = result.await.unwrap().unwrap();
        assert_eq!(snapshot.tasks[0].backend, "qwen");
        assert_eq!(harness.connects(), 0);
        let (reply, result) = oneshot::channel();
        harness
            .commands
            .send(Command::LocalTasks {
                project: root.path().to_path_buf(),
                backend: BackendKind::Qwen,
                request: hub_telegram::hub::tasks::Request::Start {
                    id: snapshot.tasks[0].id,
                    revision: 1,
                    next: None,
                },
                reply,
            })
            .await
            .unwrap();
        assert!(result.await.unwrap().is_err());
        assert!(service.read().unwrap().tasks()[0].runs.is_empty());
        assert_eq!(harness.connects(), 0);
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
    async fn local_settings_can_disconnect_and_later_enable_telegram() {
        let mut harness = harness(
            Ok(Loaded::Ready(settings("-100", ""))),
            &[Outcome::Connect, Outcome::Connect],
            false,
        );
        harness.until("connected", running).await;
        let mut local = draft("-100", "local-model");
        local.token.clear();
        local.chat.clear();
        local.users.clear();
        harness.send(Command::Save(Box::new(local))).await;
        harness.until("local only", |s| s.local_only && s.bot == BotStatus::Unconfigured).await;
        assert_eq!((harness.connects(), harness.stops()), (1, 1));
        harness.send(Command::Start).await;
        harness.send(Command::Save(Box::new(draft("-100", "local-model")))).await;
        harness.until("reconnected", running).await;
        assert!(!harness.snapshot.borrow().local_only);
        assert_eq!(harness.connects(), 2);
    }

    #[tokio::test]
    async fn memory_toggle_persists_without_restarting_bot_and_rejects_stale_revision() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::Hang], false);
        harness.until("running", running).await;
        let revision = harness.snapshot.borrow().save_revision;
        let (reply, rx) = oneshot::channel();
        harness.send(Command::SetMemory { enabled: false, revision, reply }).await;
        assert!(rx.await.unwrap().is_ok());
        harness.until("disabled", |s| s.saved.as_ref().is_some_and(|d| !d.memory.enabled)).await;
        assert_eq!((harness.connects(), harness.stops()), (1, 0));
        let (reply, rx) = oneshot::channel();
        harness.send(Command::SetMemory { enabled: true, revision, reply }).await;
        assert!(rx.await.unwrap().is_err());
        assert!(!harness.snapshot.borrow().saved.as_ref().unwrap().memory.enabled);
    }

    #[tokio::test]
    async fn failed_memory_save_does_not_change_live_setting() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[Outcome::Hang], true);
        harness.until("running", running).await;
        let revision = harness.snapshot.borrow().save_revision;
        let (reply, rx) = oneshot::channel();
        harness.send(Command::SetMemory { enabled: false, revision, reply }).await;
        assert!(rx.await.unwrap().is_err());
        assert!(harness.snapshot.borrow().saved.as_ref().unwrap().memory.enabled);
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
    async fn sync_command_reaches_the_hub() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let (reply, result) = oneshot::channel();
        harness.send(Command::SyncTopics { reply }).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while harness.probe.messages.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(*harness.probe.messages.lock().unwrap(), ["sync"]);
        drop(result);
    }

    #[tokio::test]
    async fn sync_command_without_bot_reports_error() {
        let harness = harness(Ok(Loaded::Missing), &[], true);
        let (reply, result) = oneshot::channel();
        harness.send(Command::SyncTopics { reply }).await;
        assert!(result.await.expect("sync reply").unwrap_err().contains("запустите бота"));
    }

    #[tokio::test]
    async fn views_are_published() {
        let mut harness = harness(Ok(Loaded::Ready(settings("-100", ""))), &[], false);
        harness.until("running", running).await;
        let view = TopicView {
            memory_enabled: None,
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

    fn project_draft(
        id: &str,
        name: &str,
        cwd: &std::path::Path,
    ) -> hub_core::projects::ProjectDraft {
        hub_core::projects::ProjectDraft {
            options: hub_core::options::OptionsDraft::default(),
            id: id.to_owned(),
            name: name.to_owned(),
            cwd: cwd.display().to_string(),
            backend: "codex".to_owned(),
            actions: vec![],
            checks: Vec::new(),
        }
    }

    fn project_settings(id: &str, name: &str, cwd: &std::path::Path) -> Settings {
        let mut form = draft("-100", "");
        form.projects = vec![project_draft(id, name, cwd)];
        form.parse(&home()).unwrap()
    }

    fn created_project(name: &str) -> std::path::PathBuf {
        let dir = home().join("projects").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::canonicalize(dir).unwrap()
    }

    fn seeded_service(tasks_dir: &std::path::Path) -> Arc<hub_core::tasks::service::TaskService> {
        Arc::new(hub_core::tasks::service::TaskService::new(Box::new(
            crate::topics::FileTopics::new(tasks_dir.join("topics.json")),
        )))
    }

    fn forget_ok(harness: &Harness, count: usize) {
        harness.probe.forget.lock().unwrap().push_back(Ok(count));
    }

    #[tokio::test]
    async fn delete_project_cascade_reaches_every_storage() {
        let root = tempfile::tempdir().unwrap();
        let alpha = created_project("cascade-alpha");
        let beta = created_project("cascade-beta");
        let mut harness =
            harness(Ok(Loaded::Ready(project_settings("alpha", "Альфа", &alpha))), &[], false);
        harness.until("running", running).await;
        let service = seeded_service(root.path());
        service
            .access(hub_core::tasks::service::Access::Edit, |book, _| {
                book.create(alpha.clone(), "codex", "Правка", "Смотри")?;
                book.create(alpha.clone(), "codex", "Сборка", "Собирай")?;
                book.create(beta.clone(), "codex", "Чужая", "Не тронь")?;
                book.set_rules(alpha.clone(), 0, true, "правила альфы")?;
                book.set_rules(beta.clone(), 0, true, "правила беты")
            })
            .unwrap();
        *harness.probe.tasks.lock().unwrap() = Some(service.clone());
        let base = hub_memory::base_in(&home());
        let store = hub_memory::Store::open(&base, &alpha, "test").unwrap();
        store
            .write(hub_memory::WriteRequest {
                key: "k".into(),
                expected_revision: 0,
                kind: hub_memory::EntryKind::Progress,
                title: "заметка".into(),
                body: "тело".into(),
                sources: vec!["test".into()],
            })
            .unwrap();
        let memory_dir = hub_memory::MemoryReader::new(&base, &alpha)
            .unwrap()
            .inspect("", 0, 1)
            .unwrap()
            .directory;
        forget_ok(&harness, 2);

        let (reply, result) = oneshot::channel();
        harness.send(Command::DeleteProject { id: "alpha".to_owned(), reply }).await;
        let summary = result.await.unwrap().unwrap();
        assert!(summary.contains("задач снято — 2"), "got: {summary}");
        assert!(summary.contains("тем отвязано — 2"), "got: {summary}");
        assert!(summary.contains("память удалена"), "got: {summary}");
        assert_eq!(*harness.probe.messages.lock().unwrap(), ["forget_project alpha"]);

        let book = service.read().unwrap();
        assert_eq!(book.tasks().len(), 1);
        assert_eq!(book.tasks()[0].project, beta);
        assert_eq!(book.rules(&alpha), hub_core::tasks::ProjectRules::default());
        assert_eq!(book.rules(&beta).text, "правила беты");
        assert!(!memory_dir.exists());
        assert!(alpha.is_dir(), "the project folder itself is never removed");
        harness
            .until("profile gone", |s| {
                s.saved.as_ref().is_some_and(|d| d.projects.iter().all(|p| p.id != "alpha"))
            })
            .await;
        let saved = harness.saved.lock().unwrap();
        assert!(saved.last().expect("settings saved").projects.is_empty());
    }

    #[tokio::test]
    async fn delete_project_blocks_while_a_task_is_active() {
        let root = tempfile::tempdir().unwrap();
        let busy = created_project("busy-project");
        let mut harness =
            harness(Ok(Loaded::Ready(project_settings("busy", "Занятый", &busy))), &[], false);
        harness.until("running", running).await;
        let service = seeded_service(root.path());
        let id = service
            .access(hub_core::tasks::service::Access::Edit, |book, _| {
                book.create(busy.clone(), "codex", "Работа", "Пили")
            })
            .unwrap();
        service
            .access(hub_core::tasks::service::Access::Edit, |book, _| {
                book.begin(
                    id,
                    1,
                    None,
                    jiff::Timestamp::from_second(1_700_000_000).unwrap(),
                    &hub_core::settings::AgentRoles::default(),
                )
            })
            .unwrap();
        *harness.probe.tasks.lock().unwrap() = Some(service.clone());
        let base = hub_memory::base_in(&home());
        hub_memory::Store::open(&base, &busy, "test").unwrap();
        let memory_dir = hub_memory::MemoryReader::new(&base, &busy)
            .unwrap()
            .inspect("", 0, 1)
            .unwrap()
            .directory;

        let (reply, result) = oneshot::channel();
        harness.send(Command::DeleteProject { id: "busy".to_owned(), reply }).await;
        let error = result.await.unwrap().unwrap_err();
        assert!(error.contains("активные задачи"), "got: {error}");
        assert!(harness.probe.messages.lock().unwrap().is_empty());
        assert!(service.read().unwrap().tasks().iter().any(|t| t.project == busy && t.active()));
        assert!(memory_dir.exists());
        assert!(harness.saved.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_project_without_memory_or_tasks_is_not_an_error() {
        let root = tempfile::tempdir().unwrap();
        let solo = created_project("solo-project");
        let mut harness =
            harness(Ok(Loaded::Ready(project_settings("solo", "Одинокий", &solo))), &[], false);
        harness.until("running", running).await;
        *harness.probe.tasks.lock().unwrap() = Some(seeded_service(root.path()));
        forget_ok(&harness, 0);

        let (reply, result) = oneshot::channel();
        harness.send(Command::DeleteProject { id: "solo".to_owned(), reply }).await;
        let summary = result.await.unwrap().unwrap();
        assert!(summary.contains("задач снято — 0"), "got: {summary}");
        assert!(summary.contains("памяти не было"), "got: {summary}");
        harness
            .until("profile gone", |s| {
                s.saved.as_ref().is_some_and(|d| d.projects.iter().all(|p| p.id != "solo"))
            })
            .await;
    }

    #[tokio::test]
    async fn delete_project_keeps_memory_shared_with_a_living_neighbour() {
        let root = tempfile::tempdir().unwrap();
        let alpha = created_project("share-alpha");
        let beta = created_project("share-beta");
        let mut form = draft("-100", "");
        form.projects =
            vec![project_draft("alpha", "Альфа", &alpha), project_draft("beta", "Бета", &beta)];
        let mut harness = harness(Ok(Loaded::Ready(form.parse(&home()).unwrap())), &[], false);
        harness.until("running", running).await;
        *harness.probe.tasks.lock().unwrap() = Some(seeded_service(root.path()));
        let base = hub_memory::base_in(&home());
        hub_memory::Store::open(&base, &alpha, "test")
            .unwrap()
            .write(hub_memory::WriteRequest {
                key: "key".into(),
                expected_revision: 0,
                kind: hub_memory::EntryKind::Progress,
                title: "общая".into(),
                body: "тело".into(),
                sources: vec!["test".into()],
            })
            .unwrap();
        hub_memory::BindingPreview::prepare(&base, &alpha, &beta, hub_memory::BindingMode::Share)
            .unwrap()
            .apply()
            .unwrap();
        let directory = hub_memory::MemoryReader::new(&base, &beta)
            .unwrap()
            .inspect("", 0, 1)
            .unwrap()
            .directory;
        forget_ok(&harness, 0);

        let (reply, result) = oneshot::channel();
        harness.send(Command::DeleteProject { id: "alpha".to_owned(), reply }).await;
        let summary = result.await.unwrap().unwrap();
        assert!(summary.contains("память оставлена"), "got: {summary}");
        // Б1: the neighbour keeps the shared storage, its record and its data.
        assert!(directory.exists());
        assert_eq!(
            hub_memory::MemoryReader::new(&base, &beta).unwrap().get("key", None).unwrap().revision,
            1
        );
        harness
            .until("profile gone", |s| {
                s.saved.as_ref().is_some_and(|d| d.projects.iter().all(|p| p.id != "alpha"))
            })
            .await;
        assert!(
            harness
                .saved
                .lock()
                .unwrap()
                .last()
                .is_some_and(|saved| saved.projects.iter().any(|p| p.id() == "beta"))
        );
    }

    #[tokio::test]
    async fn delete_project_completes_when_the_project_folder_vanished() {
        let root = tempfile::tempdir().unwrap();
        let ghost = created_project("ghost-project");
        let mut harness =
            harness(Ok(Loaded::Ready(project_settings("ghost", "Призрак", &ghost))), &[], false);
        harness.until("running", running).await;
        *harness.probe.tasks.lock().unwrap() = Some(seeded_service(root.path()));
        let base = hub_memory::base_in(&home());
        hub_memory::Store::open(&base, &ghost, "test")
            .unwrap()
            .write(hub_memory::WriteRequest {
                key: "key".into(),
                expected_revision: 0,
                kind: hub_memory::EntryKind::Progress,
                title: "снимок".into(),
                body: "тело".into(),
                sources: vec!["test".into()],
            })
            .unwrap();
        let directory = hub_memory::MemoryReader::new(&base, &ghost)
            .unwrap()
            .inspect("", 0, 1)
            .unwrap()
            .directory;
        std::fs::remove_dir_all(&ghost).unwrap();
        forget_ok(&harness, 0);

        // Б3: a vanished folder must not make the memory step terminal.
        let (reply, result) = oneshot::channel();
        harness.send(Command::DeleteProject { id: "ghost".to_owned(), reply }).await;
        let summary = result.await.unwrap().unwrap();
        assert!(summary.contains("память удалена"), "got: {summary}");
        assert!(!directory.exists());
        harness
            .until("profile gone", |s| {
                s.saved.as_ref().is_some_and(|d| d.projects.iter().all(|p| p.id != "ghost"))
            })
            .await;
        assert!(
            harness.saved.lock().unwrap().last().is_some_and(|saved| saved.projects.is_empty())
        );
    }

    #[tokio::test]
    async fn delete_project_blocks_while_a_task_is_queued() {
        let root = tempfile::tempdir().unwrap();
        let waiting = created_project("queued-project");
        let mut harness = harness(
            Ok(Loaded::Ready(project_settings("queued", "Очередной", &waiting))),
            &[],
            false,
        );
        harness.until("running", running).await;
        let service = seeded_service(root.path());
        let id = service
            .access(hub_core::tasks::service::Access::Edit, |book, _| {
                book.create(waiting.clone(), "codex", "Очередь", "Жди")
            })
            .unwrap();
        service
            .access(hub_core::tasks::service::Access::Edit, |book, _| {
                book.enqueue(id, 1, None, false, None)
            })
            .unwrap();
        *harness.probe.tasks.lock().unwrap() = Some(service.clone());
        let base = hub_memory::base_in(&home());
        hub_memory::Store::open(&base, &waiting, "test").unwrap();

        // Н4: a queued task is about to run; the cascade must not drop it silently.
        let (reply, result) = oneshot::channel();
        harness.send(Command::DeleteProject { id: "queued".to_owned(), reply }).await;
        let error = result.await.unwrap().unwrap_err();
        assert!(error.contains("очереди"), "got: {error}");
        assert!(harness.probe.messages.lock().unwrap().is_empty());
        assert!(service.read().unwrap().tasks().iter().any(|task| task.queue.is_some()));
        assert!(harness.saved.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn delete_project_forgets_tasks_recorded_in_a_noncanonical_path_form() {
        let root = tempfile::tempdir().unwrap();
        let real = created_project("shapes-real");
        let link = home().join("projects").join("shapes-link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let mut harness =
            harness(Ok(Loaded::Ready(project_settings("shapes", "Формы", &link))), &[], false);
        harness.until("running", running).await;
        let service = seeded_service(root.path());
        service
            .access(hub_core::tasks::service::Access::Edit, |book, _| {
                book.create(link.clone(), "codex", "Связка", "Делай").map(|_| ())
            })
            .unwrap();
        *harness.probe.tasks.lock().unwrap() = Some(service.clone());
        forget_ok(&harness, 0);

        // Н3: the modal counts both spellings, so the cascade must forget both as well.
        let (reply, result) = oneshot::channel();
        harness.send(Command::DeleteProject { id: "shapes".to_owned(), reply }).await;
        let summary = result.await.unwrap().unwrap();
        assert!(summary.contains("задач снято — 1"), "got: {summary}");
        assert!(service.read().unwrap().tasks().is_empty());
    }

    #[tokio::test]
    async fn delete_project_reports_failure_when_the_settings_save_fails() {
        let root = tempfile::tempdir().unwrap();
        let fragile = created_project("fragile-project");
        let mut harness =
            harness(Ok(Loaded::Ready(project_settings("fragile", "Хрупкий", &fragile))), &[], true);
        harness.until("running", running).await;
        *harness.probe.tasks.lock().unwrap() = Some(seeded_service(root.path()));
        let base = hub_memory::base_in(&home());
        hub_memory::Store::open(&base, &fragile, "test")
            .unwrap()
            .write(hub_memory::WriteRequest {
                key: "key".into(),
                expected_revision: 0,
                kind: hub_memory::EntryKind::Progress,
                title: "снимок".into(),
                body: "тело".into(),
                sources: vec!["test".into()],
            })
            .unwrap();
        let directory = hub_memory::MemoryReader::new(&base, &fragile)
            .unwrap()
            .inspect("", 0, 1)
            .unwrap()
            .directory;
        forget_ok(&harness, 0);

        // Н5: the Ok reply waits for the settings save; a rejected save must not claim
        // the project is deleted.
        let (reply, result) = oneshot::channel();
        harness.send(Command::DeleteProject { id: "fragile".to_owned(), reply }).await;
        let error = result.await.unwrap().unwrap_err();
        assert!(error.contains("профиль удалить не удалось"), "got: {error}");
        assert!(!error.contains("удалён:"), "got: {error}");
        assert!(!directory.exists(), "the data cascade did run");
        assert!(harness.saved.lock().unwrap().is_empty());
    }
}
