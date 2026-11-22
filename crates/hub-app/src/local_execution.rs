//! One desktop operation, independent of Telegram transport. No automatic replay.
mod channel;
mod files;
mod shared;
mod workflow;
use channel::Channel;
pub use channel::{Answer, Interaction, Kind};
use futures::future::BoxFuture;
use hub_agent::conversation::{Conversation, Limits};
use hub_core::{
    domain::{AbsolutePath, AgentEvent, BackendKind, Prompt, TopicSession},
    settings::ExecutionSettings,
    tasks::{
        RunState, Task,
        service::{Access, TaskService},
    },
};
use hub_telegram::{git::GitRunner, hub::tasks::Request};
use jiff::Timestamp;
pub use shared::SharedRuntime;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub trait Executor: Send + Sync + 'static {
    fn memory_base(&self) -> Option<PathBuf> {
        None
    }
    fn run<'a>(
        &'a self,
        session: &'a TopicSession,
        prompt: Prompt,
        conversation: Conversation,
        settings: Arc<ExecutionSettings>,
    ) -> BoxFuture<'a, ()>;
}
pub struct CliExecutor {
    pub memory_program: PathBuf,
}
impl Executor for CliExecutor {
    fn memory_base(&self) -> Option<PathBuf> {
        hub_memory::default_base()
    }
    fn run<'a>(
        &'a self,
        session: &'a TopicSession,
        prompt: Prompt,
        conversation: Conversation,
        settings: Arc<ExecutionSettings>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let (_sender, inbox) = mpsc::channel(1);
            let _inbox = hub_telegram::agents::execute_local(
                session,
                prompt,
                inbox,
                conversation,
                settings,
                &self.memory_program,
            )
            .await;
        })
    }
}
pub use hub_telegram::execution::{Control, Live};
struct Job {
    token: Uuid,
    cancel: CancellationToken,
    channel: Arc<Channel>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.channel.close();
        self.cancel.cancel();
        self.task.abort();
    }
}
pub struct Runtime {
    service: Arc<TaskService>,
    executor: Arc<dyn Executor>,
    git: Arc<dyn GitRunner>,
    view: watch::Sender<Option<Live>>,
    job: Option<Job>,
    completed: Vec<Live>,
    isolated: bool,
}
impl Runtime {
    pub fn new(
        service: Arc<TaskService>,
        executor: Arc<dyn Executor>,
        git: Arc<dyn GitRunner>,
    ) -> Self {
        Self {
            service,
            executor,
            git,
            view: watch::Sender::new(None),
            job: None,
            completed: Vec::new(),
            isolated: false,
        }
    }
    #[must_use]
    pub fn busy(&self) -> bool {
        self.job.as_ref().is_some_and(|j| !j.task.is_finished())
    }
    #[must_use]
    pub fn snapshot(&self) -> Option<Live> {
        let mut live = self.view.borrow().clone()?;
        if let Some(job) = &self.job {
            live.interaction.clone_from(&job.channel.subscribe().borrow());
            if job.task.is_finished() && live.active {
                live.active = false;
                live.interaction = None;
                live.status =
                    "Исполнитель остановился неожиданно. Проверьте историю и прежние эффекты."
                        .into();
            }
        }
        Some(live)
    }
    #[must_use]
    pub fn snapshot_for(&self, project: &Path, id: u64) -> Option<Live> {
        self.snapshot().filter(|live| live.id == id && live.project == project).or_else(|| {
            self.completed
                .iter()
                .rev()
                .find(|live| live.id == id && live.project == project)
                .cloned()
        })
    }
    fn retain_completed(&mut self) {
        if let Some(previous) = self.snapshot().filter(|live| !live.active) {
            self.completed.retain(|live| live.id != previous.id);
            // TaskBook holds at most 100 tasks; retain the latest operation of each.
            if self.completed.len() == 100 {
                self.completed.remove(0);
            }
            self.completed.push(previous);
        }
    }
    pub fn stop(&self, token: Uuid) -> Result<(), String> {
        let job = self
            .job
            .as_ref()
            .filter(|j| j.token == token && !j.task.is_finished())
            .ok_or("Этот запуск уже завершён")?;
        job.channel.close();
        job.cancel.cancel();
        Ok(())
    }
    pub fn respond(&self, token: Uuid, id: Uuid, answer: Answer) -> Result<(), String> {
        self.job
            .as_ref()
            .filter(|j| j.token == token && !j.task.is_finished())
            .ok_or("Этот запуск уже завершён")?
            .channel
            .respond(id, answer)
    }
    pub async fn shutdown(&mut self) {
        if let Some(mut job) = self.job.take() {
            job.channel.close();
            job.cancel.cancel();
            if tokio::time::timeout(Duration::from_secs(12), &mut job.task).await.is_err() {
                job.task.abort();
                let _ = (&mut job.task).await;
            }
        }
    }
    fn resolve_request(&self, request: Request) -> Result<(u64, u64, u64, Action), String> {
        Ok(match request {
            Request::RunQueued { id, revision } => {
                let book = self.service.read()?;
                let q = book.get(id)?.queue.as_ref().ok_or("Задача снята с очереди")?;
                (id, revision, q.rules_revision, Action::Start(q.next.clone()))
            }
            Request::StartChecked { id, revision, next, rules_revision } => {
                (id, revision, rules_revision, Action::Start(next))
            }
            Request::PrepareGitChecked { id, revision, branch, rules_revision } => {
                (id, revision, rules_revision, Action::Prepare(branch))
            }
            _ => return Err("Для запуска обновите карточку задачи и проверьте контекст.".into()),
        })
    }
    pub fn dispatch(
        &mut self,
        project: &Path,
        request: Request,
        settings: Arc<ExecutionSettings>,
    ) -> Result<(), String> {
        if self.busy() {
            return Err(
                "Локальная операция уже выполняется. Дождитесь завершения или остановите её."
                    .into(),
            );
        }
        let queued = matches!(request, Request::RunQueued { .. });
        let (id, revision, rules_revision, action) = self.resolve_request(request)?;
        let book = self.service.read()?;
        let task = book.get(id)?;
        if task.project != project {
            return Err("Задача относится к другому проекту".into());
        }
        if task.revision != revision || book.rules(project).revision != rules_revision {
            return Err(
                "Задача или правила изменились. Обновите карточку и проверьте контекст.".into()
            );
        }
        if !queued && task.queue.is_some() {
            return Err("Сначала снимите задачу с очереди.".into());
        }
        let backend = BackendKind::parse(&task.backend).ok_or("Неизвестный агент задачи")?;
        let session = TopicSession::fresh(
            backend,
            AbsolutePath::new(project.to_path_buf()).ok_or("Нужен абсолютный каталог")?,
        );
        // Reservation covers Git check and the durable begin, not the agent's entire run.
        let lease = self.service.reserve()?;
        let task = self.service.access(Access::Internal, |book, _| {
            if book.get(id)?.revision != revision || book.rules(project).revision != rules_revision
            {
                return Err("Задача или правила изменились".into());
            }
            if queued {
                book.claim_queue(id, revision)?;
            }
            match &action {
                Action::Prepare(branch) => {
                    book.prepare_git(id, revision, branch)?;
                }
                Action::Start(next) => {
                    let mut check = book.clone();
                    check.begin(id, revision, next.as_deref(), Timestamp::now())?;
                }
            }
            Ok(book.get(id)?.clone())
        })?;
        self.isolated = task.isolated && matches!(action, Action::Start(_));
        let mut settings = settings;
        let frozen = Arc::make_mut(&mut settings);
        frozen.memory.enabled = task.memory.unwrap_or(frozen.memory.enabled);
        frozen.memory_project = Some(session.cwd.clone());
        let token = Uuid::new_v4();
        let cancel = CancellationToken::new();
        let channel = Arc::new(Channel::new(settings.timeouts.approval, cancel.clone()));
        self.retain_completed();
        self.view.send_replace(Some(Live {
            token,
            project: project.to_path_buf(),
            id,
            run: None,
            active: true,
            status: "Подготовка запуска…".into(),
            interaction: None,
        }));
        let worker = Worker {
            service: self.service.clone(),
            executor: self.executor.clone(),
            git: self.git.clone(),
            view: self.view.clone(),
            cancel: cancel.clone(),
            channel: channel.clone(),
        };
        let cleanup = matches!(action, Action::Prepare(_)).then(|| GitIntent {
            service: self.service.clone(),
            id,
            revision: task.revision,
        });
        let job = tokio::spawn(async move {
            let _cleanup = cleanup;
            let outcome =
                worker.execute(task, session, action, settings, rules_revision, lease).await;
            if queued && let Err(error) = &outcome {
                let _ = worker.service.access(Access::Internal, |book, _| {
                    if book.get(id)?.queue.is_some() {
                        book.pause_queue(id, revision, error)?;
                    }
                    Ok(())
                });
            }
            worker.channel.close();
            worker.view.send_modify(|view| {
                if let Some(view) = view {
                    view.active = false;
                    view.status = outcome.unwrap_or_else(|error| error);
                    view.interaction = None;
                }
            });
        });
        self.job = Some(Job { token, cancel, channel, task: job });
        Ok(())
    }
}
enum Action {
    Prepare(String),
    Start(Option<String>),
}
struct Worker {
    service: Arc<TaskService>,
    executor: Arc<dyn Executor>,
    git: Arc<dyn GitRunner>,
    view: watch::Sender<Option<Live>>,
    cancel: CancellationToken,
    channel: Arc<Channel>,
}
struct GitIntent {
    service: Arc<TaskService>,
    id: u64,
    revision: u64,
}
impl Drop for GitIntent {
    fn drop(&mut self) {
        if let Err(error) = self.service.access(Access::Internal, |book, _| {
            let task = book.get(self.id)?;
            if task.revision == self.revision
                && task.git.as_ref().is_some_and(|git| {
                    git.state == hub_core::tasks::git::PreparationState::Preparing
                })
            {
                book.finish_git(
                    self.id,
                    self.revision,
                    Err("Подготовка прервана. Проверьте Git перед повтором.".into()),
                )?;
            }
            Ok(())
        }) {
            tracing::error!(%error, "cannot finalize desktop Git preparation");
        }
    }
}
#[derive(Clone, Copy)]
enum Output {
    Implementation,
    Review,
}
struct Attempt {
    service: Arc<TaskService>,
    id: u64,
    run: u32,
    finished: bool,
}
impl Drop for Attempt {
    fn drop(&mut self) {
        if !self.finished
            && let Err(error) = self.service.access(Access::Internal, |book, _| {
                book.finish(self.id, self.run, RunState::Interrupted, Timestamp::now())
            })
        {
            tracing::error!(%error,"cannot finalize desktop attempt");
        }
    }
}
impl Worker {
    fn status(&self, status: &str) {
        self.view.send_modify(|view| {
            if let Some(view) = view {
                view.status = status.into();
            }
        });
    }
    async fn execute(
        &self,
        task: Task,
        session: TopicSession,
        action: Action,
        settings: Arc<ExecutionSettings>,
        rules_revision: u64,
        lease: hub_core::tasks::service::BusyLease,
    ) -> Result<String, String> {
        if let Action::Prepare(_) = action {
            let preparation = task.git.clone().ok_or("Нет параметров Git")?;
            self.status("Подготовка Git…");
            let result = self
                .git
                .run(
                    hub_telegram::git::Request {
                        project: task.project.clone(),
                        preparation,
                        verify_only: false,
                        continuation: false,
                    },
                    self.cancel.clone(),
                )
                .await;
            let message = result.as_ref().map_or_else(Clone::clone, |_| {
                "Ветка подготовлена. Запустите задачу отдельной кнопкой.".into()
            });
            self.service.access(Access::Internal, |book, _| {
                book.finish_git(task.id, task.revision, result)
            })?;
            return Ok(message);
        }
        let Action::Start(next) = action else {
            return Err("Неверная операция".into());
        };
        if self.service.read()?.rules(&task.project).git.is_some() {
            self.status("Проверка Git перед запуском…");
            let preparation = task.git.clone().ok_or("Сначала подготовьте Git")?;
            let result = self
                .git
                .run(
                    hub_telegram::git::Request {
                        project: task.project.clone(),
                        preparation,
                        verify_only: true,
                        continuation: !task.runs.is_empty(),
                    },
                    self.cancel.clone(),
                )
                .await;
            if let Err(error) = result {
                self.service.access(Access::Internal, |book, _| {
                    book.note_git(task.id, task.revision, &error)
                })?;
                return Err(error);
            }
        }
        let mut session = session;
        if task.isolated {
            if self.service.read()?.rules(&task.project).git.is_some() {
                return Err("Проект требует подготовку ветки в основном каталоге.".into());
            }
            self.status("Проверка отдельного worktree…");
            let cwd = crate::git::worktree(&task, &self.cancel).await?;
            session.cwd = AbsolutePath::new(cwd).ok_or("Нужен абсолютный путь worktree")?;
        }
        if self.cancel.is_cancelled() {
            return Err("Запуск остановлен до начала работы агента.".into());
        }
        let (run, prompt, workflow) = self.service.access(Access::Internal, |book, _| {
            if book.rules(&task.project).revision != rules_revision {
                return Err("Правила изменились. Проверьте контекст.".into());
            }
            let text = book
                .get(task.id)?
                .preview_with_rules(next.as_deref(), &book.rules(&task.project))?;
            let prompt = Prompt::new(text, vec![]).ok_or("Пустое поручение")?;
            let run = book.begin(task.id, task.revision, next.as_deref(), Timestamp::now())?;
            Ok((
                run,
                prompt,
                book.rules(&task.project).workflow.filter(|p| p.implementer == session.backend),
            ))
        })?;
        self.view.send_modify(|view| {
            if let Some(view) = view {
                view.run = Some(run);
            }
        });
        let attempt = Attempt { service: self.service.clone(), id: task.id, run, finished: false };
        self.channel
            .files
            .set(Arc::new(files::FileTarget {
                service: self.service.clone(),
                cwd: session.cwd.clone(),
                task: task.id,
                run,
            }))
            .map_err(|_| "Повторная привязка файлов попытки")?;
        drop(lease);
        if self.cancel.is_cancelled() {
            // begin() can block on persistence while a stop arrives. Dropping
            // the attempt marks it interrupted before any Git or CLI work.
            return Ok(RunState::Interrupted.label().into());
        }
        self.execute_attempt(&session, prompt, settings, attempt, workflow.as_ref()).await
    }

    async fn execute_attempt(
        &self,
        session: &TopicSession,
        prompt: Prompt,
        settings: Arc<ExecutionSettings>,
        mut attempt: Attempt,
        workflow: Option<&hub_core::tasks::workflow::WorkflowPreset>,
    ) -> Result<String, String> {
        self.status("Агент выполняет задачу");
        let cwd = session.cwd.as_path();
        let memory = if settings.memory.enabled
            && let Some(base) = self.executor.memory_base()
        {
            let project = settings.memory_project.as_ref().unwrap_or(&session.cwd).clone();
            Some(
                tokio::task::spawn_blocking(move || {
                    hub_memory::Store::open(&base, project.as_path(), "agent-hub")
                        .map_err(|e| e.to_string())
                })
                .await
                .map_err(|e| e.to_string())
                .and_then(|store| store),
            )
        } else {
            None
        };
        let baseline = crate::task_diff::baseline(cwd).await;
        let mut state = self
            .run(session, prompt.clone(), settings.clone(), &attempt, Output::Implementation)
            .await?;
        if state == RunState::Completed
            && let Some(preset) = workflow
        {
            state = self
                .verify(
                    workflow::Workflow {
                        preset,
                        original: &prompt,
                        session,
                        baseline: baseline.as_deref(),
                    },
                    settings,
                    &attempt,
                )
                .await?;
        }
        let diff = crate::task_diff::capture(cwd, baseline.as_deref()).await;
        if let Err(error) = self
            .service
            .access(Access::Internal, |book, _| book.record_diff(attempt.id, attempt.run, &diff))
        {
            tracing::warn!(%error, "Не удалось сохранить изменения попытки");
        }
        let checkpoint = self.service.access(Access::Internal, |book, _| {
            book.finish(attempt.id, attempt.run, state, Timestamp::now())?;
            let task = book.get(attempt.id)?;
            let run = task.runs.last().ok_or("Нет попытки задачи")?;
            let passed = run.workflow_report.checks.iter().filter(|check| check.passed).count();
            let checks = run.workflow_report.checks.len();
            let review = if run.workflow_report.review.is_empty() { "нет" } else { "сохранено" };
            Ok(hub_memory::WriteRequest {
                key: format!("hub-task-{}-run-{}", task.id, run.id),
                expected_revision: 0,
                kind: hub_memory::EntryKind::Progress,
                title: format!("Задача #{} · попытка {}", task.id, run.id),
                body: format!("{} · {}. Состояние: {}. Проверки пройдены: {passed}/{checks}. Ревью: {review}.", task.title, session.backend.name(), state.label()),
                sources: vec![format!("/task attempt {} {}", task.id, run.id)],
            })
        })?;
        attempt.finished = true;
        if let Some(memory) = memory {
            let saved = tokio::task::spawn_blocking(move || {
                memory?.write(checkpoint).map_err(|error| error.to_string())
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|entry| entry);
            if let Err(error) = saved {
                return Ok(format!(
                    "{} · Не удалось сохранить итог в память: {error}",
                    state.label()
                ));
            }
        }
        Ok(state.label().into())
    }
    async fn run(
        &self,
        session: &TopicSession,
        prompt: Prompt,
        settings: Arc<ExecutionSettings>,
        attempt: &Attempt,
        output: Output,
    ) -> Result<RunState, String> {
        let (events, mut received) = mpsc::channel(64);
        let conversation = Conversation {
            channel: self.channel.clone(),
            events,
            cancel: self.cancel.clone(),
            limits: Limits::new(settings.timeouts.background),
        };
        // A stop may arrive while begin() is saving. Never first-poll a cancelled launch.
        let future = async {
            if !self.cancel.is_cancelled() {
                self.executor.run(session, prompt, conversation, settings).await;
            }
        };
        tokio::pin!(future);
        let mut state = RunState::Failed;
        let mut events_open = true;
        loop {
            tokio::select! {
                biased;
                ()=self.cancel.cancelled()=>{
                    let _=tokio::time::timeout(Duration::from_secs(10),&mut future).await;
                    return Ok(RunState::Interrupted);
                },
                event=received.recv(), if events_open=>{
                    if let Some(event)=event {self.event(event,attempt,&mut state,output)?;} else {events_open=false;}
                },
                ()=&mut future=>{while let Ok(event)=received.try_recv(){self.event(event,attempt,&mut state,output)?;} break;},
            }
        }
        Ok(state)
    }
    fn event(
        &self,
        event: AgentEvent,
        attempt: &Attempt,
        state: &mut RunState,
        output: Output,
    ) -> Result<(), String> {
        match event {
            AgentEvent::AssistantText(text) if matches!(output, Output::Review) => {
                if text.trim().is_empty() || text.len() > 65536 {
                    self.set_review(attempt, "")?;
                } else {
                    self.set_review(attempt, &text)?;
                }
            }
            AgentEvent::AssistantText(text) => {
                self.service.access(Access::Internal, |book, _| {
                    if text.trim().is_empty() || text.len() > 65536 {
                        book.unavailable(
                            attempt.id,
                            attempt.run,
                            "Ответ пуст или превышает 64 КиБ; текст не обрезан.",
                        )
                    } else {
                        book.record(attempt.id, attempt.run, &text)
                    }
                })?;
            }
            AgentEvent::Finished(finished) => {
                *state = if finished.background == 0 {
                    RunState::Completed
                } else {
                    RunState::Interrupted
                };
            }
            AgentEvent::Failed(error) => {
                *state = RunState::Failed;
                self.status(&error.chars().take(512).collect::<String>());
                if matches!(output, Output::Review) {
                    return self.set_review(
                        attempt,
                        &format!("Ошибка ревью: {}", error.chars().take(512).collect::<String>()),
                    );
                }
                self.service.access(Access::Internal, |book, _| {
                    book.unavailable(
                        attempt.id,
                        attempt.run,
                        &error.chars().take(100).collect::<String>(),
                    )
                })?;
            }
            AgentEvent::BackgroundAbandoned(_) => {
                *state = RunState::Interrupted;
            }
            AgentEvent::TurnStarted => {
                *state = RunState::Failed;
            }
            AgentEvent::ToolCall(_)
            | AgentEvent::SessionStarted(_)
            | AgentEvent::BackgroundChanged(_)
            | AgentEvent::Quota(_) => {}
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests;
