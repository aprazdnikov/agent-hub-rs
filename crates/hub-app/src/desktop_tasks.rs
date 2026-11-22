//! Local task management without credentials or a Telegram topic.
use hub_core::{
    domain::BackendKind,
    tasks::{RunState, Task, TaskStatus, git::PreparationState, service::TaskService},
};
use hub_telegram::hub::tasks::{Request, Snapshot, manage::manage};
use std::{path::Path, sync::Arc};

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

/// Cross-project navigation metadata and a short excerpt for items that need attention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub id: u64,
    pub project: std::path::PathBuf,
    pub title: String,
    /// The cross-project search reads the original prompt. GUI snapshots share its allocation.
    pub prompt: Arc<str>,
    pub backend: String,
    pub status: TaskStatus,
    pub archived: bool,
    pub reason: Option<Attention>,
    pub summary: String,
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
impl From<&Task> for TaskRow {
    fn from(task: &Task) -> Self {
        let attention = attention(task);
        Self {
            id: task.id,
            project: task.project.clone(),
            title: task.title.clone(),
            prompt: Arc::from(task.prompt.as_str()),
            backend: task.backend.clone(),
            status: task.status,
            archived: task.archived,
            reason: attention.map(|(reason, _)| reason),
            summary: attention.map_or_else(String::new, |(_, text)| excerpt(text)),
        }
    }
}

pub(crate) fn excerpt(text: &str) -> String {
    hub_core::render::truncate(text.trim(), 180).split_whitespace().collect::<Vec<_>>().join(" ")
}

fn attention(task: &Task) -> Option<(Attention, &str)> {
    if task.archived
        || task.active()
        || matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)
    {
        return None;
    }
    if let Some(queue) = &task.queue {
        return queue.error.as_deref().map(|error| (Attention::Queue, error));
    }
    if let Some(git) = &task.git
        && matches!(git.state, PreparationState::Failed | PreparationState::Interrupted)
    {
        return Some((
            Attention::Git,
            if git.message.is_empty() { git.state.label() } else { &git.message },
        ));
    }
    if task.status != TaskStatus::NeedsReview {
        return None;
    }
    let Some(run) = task.runs.last() else {
        return Some((Attention::Review, "Задача отмечена для проверки."));
    };
    if run.state == RunState::Interrupted {
        return Some((
            Attention::Interrupted,
            first_text(&[&run.diagnostic, &run.result, run.state.label()]),
        ));
    }
    if let Some(check) = run.workflow_report.checks.last().filter(|check| !check.passed) {
        return Some((
            Attention::CheckFailed,
            first_text(&[&check.output, &run.diagnostic, "Проверка завершилась с ошибкой."]),
        ));
    }
    if run.state == RunState::Failed {
        return Some((
            Attention::AgentFailed,
            first_text(&[&run.diagnostic, &run.result, run.state.label()]),
        ));
    }
    Some((
        Attention::Review,
        first_text(&[&run.result, &run.workflow_report.review, &run.diagnostic, run.state.label()]),
    ))
}

fn first_text<'a>(choices: &[&'a str]) -> &'a str {
    choices.iter().copied().find(|text| !text.trim().is_empty()).unwrap_or_default()
}

pub fn index(
    service: &TaskService,
    root: Option<&str>,
    home: &Path,
) -> Result<Vec<TaskRow>, String> {
    let root = root
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| hub_core::workspace::expand_home(raw, home))
        .map(|p| hub_telegram::paths::workspace_root(&p).map_err(|_| "Корень проектов недоступен"))
        .transpose()?;
    service.inspect(|book| {
        book.tasks()
            .iter()
            .filter(|task| {
                root.as_ref().is_none_or(|root| task.project.starts_with(root.as_path()))
            })
            .map(TaskRow::from)
            .collect()
    })
}
