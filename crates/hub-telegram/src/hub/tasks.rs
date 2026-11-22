//! Manual task operations over the existing authorized hub actor.
pub mod git;
pub mod manage;
pub(super) mod shared;
mod shared_delivery;
use super::{Agents, First, Hub, Target, TopicKey};
use hub_core::tasks::service::Access;
use hub_core::{
    domain::{BackendKind, Prompt, TopicSession},
    questions::Button,
    tasks::{ProjectRules, RunState, Task, TaskBook, TaskFilter, TaskStatus},
};

/// Inline actions for a specific revision of a completed task.
#[must_use]
pub fn result_buttons(revision: u64) -> Vec<Vec<Button>> {
    vec![
        vec![
            Button { text: "Принять".into(), data: format!("te:accept:{revision}") },
            Button { text: "Открыть diff".into(), data: format!("te:diff:{revision}") },
        ],
        vec![Button {
            text: "Вернуть с замечанием".into(),
            data: format!("te:feedback:{revision}"),
        }],
    ]
}

pub const HELP: &str = "/task — задачи проекта\n/task rules\n/task git РЕДАКЦИЯ REMOTE БАЗОВАЯ_ВЕТКА\n/task git РЕДАКЦИЯ off\n/task prepare ID РЕВИЗИЯ НОВАЯ_ВЕТКА\n/task git-stop\n/task rules-set РЕДАКЦИЯ on|off | ТЕКСТ\n/task create Название | Поручение\n/task list [all|planned|review|accepted|deferred|archived] [поиск]\n/task get ID\n/task follow ID — управление общей задачей\n/task unfollow\n/task assign ID РЕВИЗИЯ АГЕНТ\n/task preview ID [Следующий шаг]\n/task attempt ID ПОПЫТКА\n/task status ID РЕВИЗИЯ planned|review|accepted|deferred\n/task archive ID РЕВИЗИЯ\n/task restore ID РЕВИЗИЯ\n/task queue ID РЕВИЗИЯ [следующий шаг] — очередь\n/task worktree ID РЕВИЗИЯ [следующий шаг] — очередь в отдельной копии репозитория\n/task unqueue ID РЕВИЗИЯ — снять с очереди\n/task run ID РЕВИЗИЯ\n/task resume ID РЕВИЗИЯ Следующий шаг после проверки прежних эффектов\nПри создании агент и каталог берутся из текущей темы. Повторный запуск всегда требует явного следующего шага; автоматического повтора нет. Результат запуска не означает приёмку пользователем.";
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub tasks: Vec<Task>,
    pub rules: ProjectRules,
    pub git_busy: bool,
}
#[derive(Clone, Debug)]
pub enum Request {
    Queue {
        id: u64,
        revision: u64,
        isolated: bool,
        next: Option<String>,
        origin: Option<hub_core::tasks::queue::Origin>,
    },
    Unqueue {
        id: u64,
        revision: u64,
    },
    /// Internal scheduler dispatch, never parsed from a message.
    RunQueued {
        id: u64,
        revision: u64,
    },
    List,
    Rules,
    SaveGitPolicy {
        revision: u64,
        policy: Option<hub_core::tasks::git::GitPolicy>,
    },
    SaveProjectRules {
        revision: u64,
        rules: ProjectRules,
    },
    PrepareGit {
        id: u64,
        revision: u64,
        branch: String,
    },
    PrepareGitChecked {
        id: u64,
        revision: u64,
        branch: String,
        rules_revision: u64,
    },
    CancelGit,
    SaveRules {
        revision: u64,
        enabled: bool,
        text: String,
    },
    StartChecked {
        id: u64,
        revision: u64,
        next: Option<String>,
        rules_revision: u64,
    },
    Search {
        filter: TaskFilter,
        query: String,
    },
    Status {
        id: u64,
        revision: u64,
        status: TaskStatus,
    },
    Archive {
        id: u64,
        revision: u64,
        archived: bool,
    },
    Attempt {
        id: u64,
        run: u32,
    },
    Get(u64),
    Assign {
        id: u64,
        revision: u64,
        backend: BackendKind,
    },
    Preview {
        id: u64,
        next: Option<String>,
    },
    Create {
        title: String,
        prompt: String,
    },
    Start {
        id: u64,
        revision: u64,
        next: Option<String>,
    },
}
#[derive(Debug)]
pub struct ManagedRun {
    pub task: u64,
    pub run: u32,
    pub generation: u64,
}
fn word(raw: &str) -> Option<(&str, &str)> {
    let raw = raw.trim_start();
    if raw.is_empty() {
        return None;
    }
    Some(raw.split_once(char::is_whitespace).unwrap_or((raw, "")))
}
fn parse_rules(tail: &str) -> Result<Request, String> {
    let (revision, tail) = word(tail).ok_or(HELP)?;
    let (mode, text) = tail.split_once('|').ok_or(HELP)?;
    let enabled = match mode.trim() {
        "on" => true,
        "off" => false,
        _ => {
            return Err("Используйте on для включения или off для выключения правил.".into());
        }
    };
    Ok(Request::SaveRules {
        revision: revision.parse().map_err(|_| HELP)?,
        enabled,
        text: text.strip_prefix(' ').unwrap_or(text).into(),
    })
}
fn parse_git(command: &str, tail: &str) -> Result<Request, String> {
    if command == "git-stop" && tail.trim().is_empty() {
        return Ok(Request::CancelGit);
    }
    let args: Vec<_> = tail.split_whitespace().collect();
    match (command, args.as_slice()) {
        ("git", [revision, "off"]) => Ok(Request::SaveGitPolicy {
            revision: revision.parse().map_err(|_| HELP)?,
            policy: None,
        }),
        ("git", [revision, remote, base]) => Ok(Request::SaveGitPolicy {
            revision: revision.parse().map_err(|_| HELP)?,
            policy: Some(hub_core::tasks::git::GitPolicy {
                remote: (*remote).into(),
                base: (*base).into(),
            }),
        }),
        ("prepare", [id, revision, branch]) => Ok(Request::PrepareGit {
            id: id.parse().map_err(|_| HELP)?,
            revision: revision.parse().map_err(|_| HELP)?,
            branch: (*branch).into(),
        }),
        _ => Err(HELP.into()),
    }
}
fn parse_queue(command: &str, tail: &str) -> Result<Request, String> {
    let (id, rest) = word(tail).ok_or(HELP)?;
    let (revision, next) = word(rest).ok_or(HELP)?;
    let id = id.parse().map_err(|_| HELP)?;
    let revision = revision.parse().map_err(|_| HELP)?;
    if command == "unqueue" {
        if !next.trim().is_empty() {
            return Err(HELP.into());
        }
        Ok(Request::Unqueue { id, revision })
    } else {
        Ok(Request::Queue {
            id,
            revision,
            isolated: command == "worktree",
            next: (!next.trim().is_empty()).then(|| next.into()),
            origin: None,
        })
    }
}
pub fn parse(raw: &str) -> Result<Request, String> {
    let Some((command, tail)) = word(raw) else { return Ok(Request::List) };
    match command {
        "git" | "prepare" | "git-stop" => parse_git(command, tail),
        "rules" if tail.trim().is_empty() => Ok(Request::Rules),
        "rules-set" => parse_rules(tail),
        "list" if tail.trim().is_empty() => Ok(Request::List),
        "list" => {
            let (filter, query) = word(tail).ok_or(HELP)?;
            let (filter, query) = match filter {
                "all" => (TaskFilter::All, query),
                "archived" => (TaskFilter::Archived, query),
                "planned" | "review" | "accepted" | "deferred" => {
                    (TaskFilter::Status(TaskStatus::parse(filter)?), query)
                }
                _ => (TaskFilter::Active, tail.trim_start()),
            };
            Ok(Request::Search { filter, query: query.into() })
        }
        "status" | "archive" | "restore" | "attempt" => {
            let (id, tail) = word(tail).ok_or(HELP)?;
            let (number, rest) = word(tail).ok_or(HELP)?;
            let id = id.parse().map_err(|_| HELP)?;
            if command == "status" {
                return Ok(Request::Status {
                    id,
                    revision: number.parse().map_err(|_| HELP)?,
                    status: TaskStatus::parse(rest.trim())?,
                });
            }
            if !rest.trim().is_empty() {
                return Err(HELP.into());
            }
            if command == "attempt" {
                Ok(Request::Attempt { id, run: number.parse().map_err(|_| HELP)? })
            } else {
                Ok(Request::Archive {
                    id,
                    revision: number.parse().map_err(|_| HELP)?,
                    archived: command == "archive",
                })
            }
        }
        "assign" => {
            let (id, rest) = word(tail).ok_or(HELP)?;
            let (revision, backend) = word(rest).ok_or(HELP)?;
            Ok(Request::Assign {
                id: id.parse().map_err(|_| HELP)?,
                revision: revision.parse().map_err(|_| HELP)?,
                backend: BackendKind::parse(backend.trim()).ok_or(
                    "Неизвестный агент: используйте codex, claude, qwen, hermes или opencode",
                )?,
            })
        }
        "preview" => {
            let (id, step) = word(tail).ok_or(HELP)?;
            Ok(Request::Preview {
                id: id.parse().map_err(|_| HELP)?,
                next: (!step.trim().is_empty()).then(|| step.into()),
            })
        }
        "create" => {
            let (title, prompt) = tail.split_once('|').ok_or(HELP)?;
            Ok(Request::Create {
                title: title.trim().into(),
                prompt: prompt.strip_prefix(' ').unwrap_or(prompt).into(),
            })
        }
        "get" => {
            let (id, rest) = word(tail).ok_or(HELP)?;
            if !rest.trim().is_empty() {
                return Err(HELP.into());
            }
            Ok(Request::Get(id.parse().map_err(|_| HELP)?))
        }
        "queue" | "worktree" | "unqueue" => parse_queue(command, tail),
        "run" | "resume" => {
            let (id, tail) = word(tail).ok_or(HELP)?;
            let (revision, step) = word(tail).ok_or(HELP)?;
            let next = match command {
                "run" if step.trim().is_empty() => None,
                "resume" if !step.trim().is_empty() => Some(step.into()),
                _ => return Err(HELP.into()),
            };
            Ok(Request::Start {
                id: id.parse().map_err(|_| HELP)?,
                revision: revision.parse().map_err(|_| HELP)?,
                next,
            })
        }
        _ => Err(HELP.into()),
    }
}

#[must_use]
pub fn describe(task: &Task) -> String {
    let status = task.status.label();
    let queue = task.queue.as_ref().map_or(String::new(), |q| {
        q.error.clone().unwrap_or_else(|| {
            format!(
                "В очереди · позиция {}{}",
                q.sequence,
                if q.isolated { " · worktree" } else { "" }
            )
        })
    });
    let archive = if task.archived { " · В архиве" } else { "" };
    let history = task
        .runs
        .iter()
        .map(|run| {
            format!(
                "Попытка {}: {} · {} · /task attempt {} {}",
                run.id,
                run.state.label(),
                run.backend.as_deref().unwrap_or(&task.backend),
                task.id,
                run.id
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let result = task.runs.last().map_or("Запусков ещё нет", |run| {
        if run.result.is_empty() {
            "Сохранённого текста результата нет"
        } else {
            &run.result
        }
    });
    let report = task.runs.last().map(|r| r.workflow_report.describe()).unwrap_or_default();
    let diagnostic = task.runs.last().map_or("", |run| run.diagnostic.as_str());
    let git = task.git.as_ref().map_or(String::new(), hub_core::tasks::git::Preparation::describe);
    format!(
        "Задача #{} · r{} · {} · {status}{archive}\n{queue}\n{git}\n{}\nПроект: {}\nПоручение: {}\n{}\n\nПоследний результат:\n{}\n{diagnostic}\n{report}\n\nДля первого запуска: /task run {} {}\nДля продолжения: /task resume {} {} СЛЕДУЮЩИЙ ШАГ\nЗавершение запуска не означает приёмку задачи.",
        task.id,
        task.revision,
        task.backend,
        task.title,
        task.project.display(),
        task.prompt,
        history,
        result,
        task.id,
        task.revision,
        task.id,
        task.revision
    )
}
pub fn describe_attempt(task: &Task, run: u32) -> Result<String, String> {
    let attempt = task.runs.iter().find(|attempt| attempt.id == run).ok_or("Попытка не найдена")?;
    let result = if attempt.result.is_empty() {
        "Сохранённого текста результата нет"
    } else {
        &attempt.result
    };
    let rules = attempt.rules.as_ref().map_or(
        "Редакция правил не сохранена (старый запуск или правила ещё не задавались)".into(),
        ProjectRules::describe,
    );
    let report = attempt.workflow_report.describe();
    let changes = if attempt.diff.is_empty() {
        String::new()
    } else {
        format!("\nИзменения:\n{}", attempt.diff)
    };
    Ok(format!(
        "Задача #{} · попытка {} · {} · {}\nШаг:\n{}\nРезультат:\n{}\n{}\n{rules}\n{report}{changes}",
        task.id,
        run,
        attempt.state.label(),
        attempt.backend.as_deref().unwrap_or(&task.backend),
        attempt.step,
        result,
        attempt.diagnostic,
    ))
}
impl<A: Agents> Hub<A> {
    pub(super) fn task_inbound(
        &mut self,
        inbound: &crate::inbound::Inbound,
        key: TopicKey,
    ) -> bool {
        if self.task_runs.contains_key(&key)
            && matches!(
                inbound.content,
                crate::inbound::Content::Media { .. }
                    | crate::inbound::Content::Voice { .. }
                    | crate::inbound::Content::VideoNote
            )
        {
            self.say(Target::Topic(key),"В теме выполняется сохранённая задача. Вложения и голосовые не ставятся в очередь; дождитесь завершения либо используйте /stop.".into());
            return true;
        }
        let crate::inbound::Content::Text(text) = &inbound.content else { return false };
        let hub_core::commands::Input::Command {
            command: hub_core::commands::Command::Task, ..
        } = hub_core::commands::parse_input(text, &self.bot)
        else {
            return false;
        };
        if crate::telegram::allowed(&self.settings.borrow().telegram, Some(key.chat), inbound.user)
        {
            self.task_command(key, inbound.user, word(text).map_or("", |(_, args)| args));
        }
        true
    }

    fn task_command(&mut self, key: TopicKey, user: Option<hub_core::domain::UserId>, raw: &str) {
        if !self.task_deliveries.available(key) {
            self.say(
                Target::Topic(key),
                "Предыдущая карточка ещё отправляется. Повторите команду после её получения."
                    .into(),
            );
            return;
        }
        if self.shared_command(key, user, raw) {
            return;
        }
        let request = parse(raw).and_then(|mut request| {
            if let Request::Queue { origin, .. } = &mut request {
                let user = user.ok_or("Неизвестный пользователь")?;
                *origin = Some(hub_core::tasks::queue::Origin {
                    chat: key.chat.0,
                    thread: key.thread.0,
                    user: Some(user.0),
                });
            }
            Ok(request)
        });
        let preview = matches!(&request, Ok(Request::Preview { .. }));
        let result = request.clone().and_then(|request| self.task_snapshot(key, request));
        if self.execution.is_some()
            && result.is_ok()
            && let Ok(
                Request::Start { id, .. }
                | Request::StartChecked { id, .. }
                | Request::PrepareGit { id, .. }
                | Request::PrepareGitChecked { id, .. },
            ) = &request
        {
            self.shared_command(key, user, &format!("follow {id}"));
        }
        let text = render_reply(request, result);
        self.task_deliveries.send(
            key,
            user,
            text,
            preview,
            self.sender.clone(),
            self.settings.clone(),
        );
    }
    pub(super) fn task_snapshot(
        &mut self,
        key: TopicKey,
        mut request: Request,
    ) -> Result<Snapshot, String> {
        let session = self.task_context(key, &request)?;
        if let Request::Queue { origin, id, .. } = &mut request {
            if self.execution.is_none() {
                return Err("Очередь требует общего исполнителя приложения.".into());
            }
            let book = self.tasks.read()?;
            let task = book.get(*id)?;
            if task.project != session.cwd.as_path() || task.backend != session.backend.name() {
                return Err("Выберите тему проекта с назначенным агентом.".into());
            }
            // A desktop command sent through a topic also keeps that topic's execution settings.
            origin.get_or_insert(hub_core::tasks::queue::Origin {
                chat: key.chat.0,
                thread: key.thread.0,
                user: None,
            });
        }
        if self.execution.is_some()
            && matches!(request, Request::CancelGit)
            && self.tasks.access(Access::Read, |_, busy| Ok(busy))?
        {
            if !self.stop_preflight(key) {
                return Err("В этой теме нет общей подготовки Git".into());
            }
            return manage::manage(
                &self.tasks,
                session.cwd.as_path(),
                session.backend,
                Request::List,
            );
        }
        if self.execution.is_some()
            && matches!(
                request,
                Request::Start { .. }
                    | Request::StartChecked { .. }
                    | Request::PrepareGit { .. }
                    | Request::PrepareGitChecked { .. }
            )
        {
            return self.shared_task(session.cwd.as_path(), request, Some(&session));
        }
        if !matches!(
            request,
            Request::Start { .. }
                | Request::StartChecked { .. }
                | Request::PrepareGit { .. }
                | Request::PrepareGitChecked { .. }
                | Request::CancelGit
        ) {
            return manage::manage(&self.tasks, session.cwd.as_path(), session.backend, request);
        }
        let tasks = self.task_request(key, request)?;
        let session = self.topics.get(&key).ok_or("Выберите тему")?;
        let rules = self.tasks.read()?.rules(session.cwd.as_path());
        Ok(Snapshot { tasks, rules, git_busy: self.git_busy() })
    }
    fn task_context(&self, key: TopicKey, request: &Request) -> Result<TopicSession, String> {
        if key.chat != self.settings.borrow().telegram.chat {
            return Err("Тема не относится к настроенной Telegram-группе".into());
        }
        if self.shutdown.is_some() {
            return Err("Приложение завершает работу".into());
        }
        let session = self.topics.get(&key).ok_or("Выберите тему с проектом и агентом")?.clone();
        if self.git_job.is_some()
            && !matches!(
                &request,
                Request::List
                    | Request::Search { .. }
                    | Request::Get(_)
                    | Request::Attempt { .. }
                    | Request::Rules
                    | Request::Preview { .. }
                    | Request::CancelGit
            )
        {
            return Err("Дождитесь завершения подготовки или проверки Git.".into());
        }
        Ok(session)
    }
    pub(super) fn task_request(
        &mut self,
        key: TopicKey,
        request: Request,
    ) -> Result<Vec<Task>, String> {
        let session = self.task_context(key, &request)?;
        match request {
            Request::CancelGit => {
                if !self.cancel_git(key) {
                    return Err(
                        "В этой теме нет подготовки Git. Выберите тему, где она запущена.".into()
                    );
                }
                Ok(Vec::new())
            }
            Request::PrepareGit { id, revision, branch } => {
                self.prepare_git(key, &session, id, revision, &branch, &self.tasks.read()?)
            }
            Request::PrepareGitChecked { id, revision, branch, rules_revision } => {
                let book = self.tasks.read()?;
                if book.rules(session.cwd.as_path()).revision != rules_revision {
                    return Err(
                        "Правила изменились. Проверьте настройки Git перед подготовкой.".into()
                    );
                }
                self.prepare_git(key, &session, id, revision, &branch, &book)
            }
            Request::StartChecked { id, revision, next, rules_revision } => {
                let book = self.tasks.read()?;
                if book.rules(session.cwd.as_path()).revision != rules_revision {
                    return Err(
                        "Правила проекта изменились. Обновите и проверьте контекст перед запуском."
                            .into(),
                    );
                }
                self.start_task(key, &session, id, revision, next.as_deref(), book)
            }
            Request::Start { id, revision, next } => {
                self.start_task(key, &session, id, revision, next.as_deref(), self.tasks.read()?)
            }
            request => manage::manage(&self.tasks, session.cwd.as_path(), session.backend, request)
                .map(|snapshot| snapshot.tasks),
        }
    }
    fn start_task(
        &mut self,
        key: TopicKey,
        session: &TopicSession,
        id: u64,
        revision: u64,
        next: Option<&str>,
        book: TaskBook,
    ) -> Result<Vec<Task>, String> {
        if book.get(id)?.isolated || book.get(id)?.queue.is_some() {
            return Err("Для выполнения этой задачи нужен общий исполнитель приложения.".into());
        }
        if book.rules(session.cwd.as_path()).git.is_some() {
            self.checked_git_start(key, session, id, revision, next, book)
        } else {
            self.launch_task(key, session, id, revision, next, &book)
        }
    }
    fn launch_task(
        &mut self,
        key: TopicKey,
        session: &TopicSession,
        id: u64,
        revision: u64,
        next: Option<&str>,
        book: &TaskBook,
    ) -> Result<Vec<Task>, String> {
        let project = session.cwd.as_path();
        let task = book.get(id)?;
        if task.project != project || task.backend != session.backend.name() {
            return Err("Выберите тему с тем же каталогом и агентом, что указаны в задаче".into());
        }
        if self.running.contains_key(&key)
            || self.handoff_busy(key)
            || self.snapshotting.contains(&key)
            || self.is_reserved(session)
        {
            return Err("Тема или сессия занята. Дождитесь завершения либо остановите её.".into());
        }
        let expected_rules = book.rules(project).revision;
        let (task, run, prompt) = self.tasks.access(Access::Internal, |book, _| {
            if book.rules(project).revision != expected_rules {
                return Err("Правила изменились. Проверьте контекст перед запуском.".into());
            }
            let prompt =
                Prompt::new(book.get(id)?.preview_with_rules(next, &book.rules(project))?, vec![])
                    .ok_or("Пустое поручение")?;
            let run = book.begin(id, revision, next)?;
            Ok((book.get(id)?.clone(), run, prompt))
        })?; // Durable attempt BEFORE launching the model.
        self.launch(key, First::Prompt(prompt), None);
        if let Some(live) = self.running.get(&key) {
            self.task_runs.insert(key, ManagedRun { task: id, run, generation: live.generation });
            Ok(vec![task])
        } else {
            self.tasks
                .access(Access::Internal, |book, _| book.finish(id, run, RunState::Interrupted))?;
            Err("Запуск не начат. Попытка отмечена прерванной; проверьте сообщение темы.".into())
        }
    }
    pub(super) fn record_task(&mut self, key: TopicKey, generation: u64, result: &str) {
        let Some(run) = self.task_runs.get(&key).filter(|run| run.generation == generation) else {
            return;
        };
        if let Err(error) =
            self.tasks.access(Access::Internal, |book, _| book.record(run.task, run.run, result))
        {
            if let Some(live) = self.running.get(&key) {
                live.cancel.cancel();
            }
            self.say(Target::Topic(key), format!("Не удалось сохранить результат задачи: {error}"));
        }
    }
    pub(super) fn unavailable_task(&mut self, key: TopicKey, generation: u64, reason: &str) {
        let Some(run) = self.task_runs.get(&key).filter(|run| run.generation == generation) else {
            return;
        };
        if let Err(error) = self
            .tasks
            .access(Access::Internal, |book, _| book.unavailable(run.task, run.run, reason))
        {
            self.say(
                Target::Topic(key),
                format!("Не удалось сохранить диагностику задачи: {error}"),
            );
        }
    }
    pub(super) fn finish_task(&mut self, key: TopicKey, generation: u64, state: RunState) {
        if self.task_runs.get(&key).is_none_or(|run| run.generation != generation) {
            return;
        }
        let Some(run) = self.task_runs.remove(&key) else { return };
        if let Err(error) =
            self.tasks.access(Access::Internal, |book, _| book.finish(run.task, run.run, state))
        {
            self.say(Target::Topic(key), format!("Не удалось сохранить состояние задачи: {error}"));
        }
    }
}

fn preview_parts(text: &str) -> Vec<String> {
    let chunks = crate::memory::context::parts(text);
    let total = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| format!("Предпросмотр · часть {}/{total}\n\n{chunk}", index + 1))
        .collect()
}

#[derive(Default)]
pub(super) struct Deliveries {
    readers: std::collections::HashMap<TopicKey, tokio::task::JoinHandle<()>>,
}
impl Deliveries {
    fn available(&mut self, key: TopicKey) -> bool {
        self.readers.retain(|_, task| !task.is_finished());
        !self.readers.contains_key(&key) && self.readers.len() < 4
    }

    fn send(
        &mut self,
        key: TopicKey,
        user: Option<hub_core::domain::UserId>,
        text: String,
        preview: bool,
        sender: crate::sender::Sender,
        settings: tokio::sync::watch::Receiver<std::sync::Arc<hub_core::settings::Settings>>,
    ) {
        self.readers.retain(|_, task| !task.is_finished());
        if self.readers.contains_key(&key) || self.readers.len() >= 4 {
            return;
        }
        let task = tokio::spawn(async move {
            let delivery = async {
                let chunks = if preview {
                    preview_parts(&text)
                } else {
                    crate::memory::context::parts(&text).into_iter().map(str::to_owned).collect()
                };
                for chunk in chunks {
                    if !crate::telegram::allowed(&settings.borrow().telegram, Some(key.chat), user)
                    {
                        return;
                    }
                    if sender
                        .one(Target::Topic(key), crate::messenger::Outgoing::plain(chunk))
                        .await
                        .is_none()
                    {
                        return;
                    }
                }
            };
            if tokio::time::timeout(std::time::Duration::from_secs(65), delivery).await.is_err() {
                tracing::warn!("task result delivery timed out");
            }
        });
        self.readers.insert(key, task);
    }
    pub(super) fn shutdown(&mut self) {
        for (_, task) in self.readers.drain() {
            task.abort();
        }
    }
}
impl Drop for Deliveries {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn render_reply(request: Result<Request, String>, result: Result<Snapshot, String>) -> String {
    match result {
        Ok(_) if matches!(request, Ok(Request::CancelGit)) => {
            "Остановка Git запрошена. Дождитесь результата и проверьте репозиторий.".into()
        }
        Ok(Snapshot { rules, .. })
            if matches!(
                request,
                Ok(Request::Rules | Request::SaveRules { .. } | Request::SaveGitPolicy { .. })
            ) =>
        {
            format!(
                "{}\n\nСохранённый текст:\n{}\n\nТекст правил передаётся агенту как инструкции; подготовка Git включается отдельно.",
                rules.describe(),
                rules.text
            )
        }
        Ok(Snapshot { tasks, .. }) if tasks.is_empty() => {
            format!("Задачи по выбранным условиям не найдены.\n{HELP}")
        }
        Ok(Snapshot { tasks, .. })
            if matches!(request, Ok(Request::List | Request::Search { .. })) =>
        {
            let rows = tasks
                .iter()
                .map(|task| {
                    format!(
                        "#{} · r{} · {} · {} · {} · {}{}",
                        task.id,
                        task.revision,
                        task.backend,
                        task.title,
                        task.status.label(),
                        task.runs
                            .last()
                            .map_or("Не запускалась", |run| run.state.label()),
                        if task.archived { " · В архиве" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("{rows}\n\n{HELP}")
        }
        Ok(Snapshot { tasks, .. }) if matches!(request, Ok(Request::Attempt { .. })) => {
            let run = match request {
                Ok(Request::Attempt { run, .. }) => run,
                _ => 0,
            };
            tasks
                .first()
                .ok_or_else(|| "Задача не найдена".to_owned())
                .and_then(|task| describe_attempt(task, run))
                .unwrap_or_else(|error| format!("Задачи: {error}"))
        }
        Ok(Snapshot { tasks, rules, .. }) if matches!(request, Ok(Request::Preview { .. })) => {
            let next = if let Ok(Request::Preview { next, .. }) = request { next } else { None };
            tasks.first().ok_or_else(|| "Задача не найдена".to_owned())
                    .and_then(|task| task.preview_with_rules(next.as_deref(), &rules).map(|text| format!("Предпросмотр поручения хаба · r{}\n{text}\n\nCLI может добавить свои инструкции и историю выбранной сессии.", task.revision)))
                    .unwrap_or_else(|error| format!("Задачи: {error}"))
        }
        Ok(Snapshot { tasks, .. }) => tasks.iter().map(describe).collect::<Vec<_>>().join("\n\n"),
        Err(error) => format!("Задачи: {error}"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn reply_uses_one_captured_snapshot_even_after_rules_change() {
        use super::*;
        use hub_core::tasks::service::{Access, TaskService, TaskStorage};
        struct Store;
        impl TaskStorage for Store {
            fn load_tasks(&mut self) -> Result<TaskBook, String> {
                Ok(TaskBook::default())
            }
            fn save_tasks(&mut self, _: &TaskBook) -> Result<(), String> {
                Ok(())
            }
        }
        let service = TaskService::new(Box::new(Store));
        let project = std::env::temp_dir();
        let snapshot = manage::manage(
            &service,
            &project,
            BackendKind::Qwen,
            Request::SaveRules { revision: 0, enabled: true, text: "Captured rules".into() },
        )
        .unwrap();
        service
            .access(Access::Edit, |book, _| book.set_rules(project.clone(), 1, true, "New rules"))
            .unwrap();
        let text = render_reply(Ok(Request::Rules), Ok(snapshot));
        assert!(text.contains("Captured rules"));
        assert!(!text.contains("New rules"));
        let error = render_reply(Ok(Request::Rules), Err("storage unavailable".into()));
        assert!(error.contains("storage unavailable"));
        assert!(!error.contains("Сохранённый текст"));
    }
    #[test]
    fn preview_parts_are_numbered_without_losing_prompt_text() {
        let text = "Полный контекст 😀\n".repeat(700);
        let parts = super::preview_parts(&text);
        assert!(parts.len() > 1);
        let mut joined = String::new();
        for (index, part) in parts.iter().enumerate() {
            let prefix = format!("Предпросмотр · часть {}/{}\n\n", index + 1, parts.len());
            joined.push_str(part.strip_prefix(&prefix).expect("numbered part"));
            assert!(part.encode_utf16().count() <= 4096);
        }
        assert_eq!(joined, text);
    }
}
