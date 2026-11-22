//! Local task management without credentials or a Telegram topic.
use hub_core::{domain::BackendKind, tasks::service::TaskService};
use hub_telegram::hub::tasks::{Request, Snapshot, manage::manage};
use std::path::Path;

pub fn request(
    service: &TaskService,
    project: &Path,
    backend: BackendKind,
    request: Request,
    root: Option<&str>,
    home: &Path,
) -> Result<Snapshot, String> {
    if !project.is_absolute() {
        return Err("Выберите абсолютный каталог проекта.".into());
    }
    let project =
        hub_telegram::paths::workspace_root(project).map_err(|error| error.to_string())?;
    if let Some(raw) = root.filter(|root| !root.trim().is_empty()) {
        let root = hub_core::workspace::expand_home(raw, home);
        let root = hub_telegram::paths::workspace_root(&root).map_err(|error| error.to_string())?;
        if !hub_telegram::paths::inside(&root, &project) {
            return Err("Выбранный каталог находится вне настроенного корня проектов.".into());
        }
    }
    manage(service, project.as_path(), backend, request)
}

/// Compact metadata for navigation across projects, without chat transcripts or patch output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub id: u64,
    pub project: std::path::PathBuf,
    pub title: String,
    pub reason: Option<Attention>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
    Queue,
    Git,
    Interrupted,
    CheckFailed,
    AgentFailed,
    Review,
}
impl Attention {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Queue => "Очередь остановлена",
            Self::Git => "Проверить Git",
            Self::Interrupted => "Запуск прерван",
            Self::CheckFailed => "Проверка не прошла",
            Self::AgentFailed => "Ошибка запуска",
            Self::Review => "Проверить результат",
        }
    }
}
pub fn index(
    service: &TaskService,
    root: Option<&str>,
    home: &Path,
) -> Result<Vec<TaskRow>, String> {
    use hub_core::tasks::{RunState, TaskStatus, git::PreparationState};
    let root = root
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| hub_core::workspace::expand_home(raw, home))
        .map(|p| hub_telegram::paths::workspace_root(&p).map_err(|_| "Корень проектов недоступен"))
        .transpose()?;
    service.inspect(|book| {
        book.tasks()
            .iter()
            .filter(|task| {
                !task.archived
                    && root.as_ref().is_none_or(|root| task.project.starts_with(root.as_path()))
            })
            .map(|task| {
                let last = task.runs.last();
                let reason = if task.active()
                    || matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)
                {
                    None
                } else if task.queue.as_ref().is_some_and(|q| q.error.is_some()) {
                    Some(Attention::Queue)
                } else if task.queue.is_some() {
                    None
                } else if task.git.as_ref().is_some_and(|g| {
                    matches!(g.state, PreparationState::Failed | PreparationState::Interrupted)
                }) {
                    Some(Attention::Git)
                } else if task.status != TaskStatus::NeedsReview {
                    None
                } else if last.is_some_and(|r| r.state == RunState::Interrupted) {
                    Some(Attention::Interrupted)
                } else if last
                    .is_some_and(|r| r.workflow_report.checks.last().is_some_and(|c| !c.passed))
                {
                    Some(Attention::CheckFailed)
                } else if last.is_some_and(|r| r.state == RunState::Failed) {
                    Some(Attention::AgentFailed)
                } else {
                    Some(Attention::Review)
                };
                TaskRow {
                    id: task.id,
                    project: task.project.clone(),
                    title: task.title.clone(),
                    reason,
                }
            })
            .collect()
    })
}
