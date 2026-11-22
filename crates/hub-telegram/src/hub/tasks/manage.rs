//! Shared synchronous management operations; no Telegram or process side effects.
use super::{Request, Snapshot};
use hub_core::{
    domain::BackendKind,
    tasks::{
        Task, TaskBook,
        service::{Access, TaskService},
    },
};
use std::path::Path;

pub fn manage(
    service: &TaskService,
    project: &Path,
    backend: BackendKind,
    request: Request,
) -> Result<Snapshot, String> {
    let access = if matches!(
        request,
        Request::List
            | Request::Search { .. }
            | Request::Rules
            | Request::Get(_)
            | Request::Attempt { .. }
            | Request::Preview { .. }
    ) {
        Access::Read
    } else {
        Access::Edit
    };
    service.access(access, |book, busy| {
        let tasks = apply(book, project, backend, request)?;
        Ok(Snapshot { tasks, rules: book.rules(project), git_busy: busy })
    })
}
fn task<'a>(book: &'a TaskBook, project: &Path, id: u64) -> Result<&'a Task, String> {
    let task = book.get(id)?;
    if task.project != project {
        return Err("Задача принадлежит другому проекту".into());
    }
    Ok(task)
}
fn apply(
    book: &mut TaskBook,
    project: &Path,
    backend: BackendKind,
    request: Request,
) -> Result<Vec<Task>, String> {
    match request {
        Request::List => Ok(book
            .tasks()
            .iter()
            .filter(|task| task.project == project && !task.archived)
            .cloned()
            .collect()),
        Request::Search { filter, query } => Ok(book
            .tasks()
            .iter()
            .filter(|task| task.project == project && filter.matches(task, &query))
            .cloned()
            .collect()),
        Request::Get(id) => Ok(vec![task(book, project, id)?.clone()]),
        Request::Attempt { id, run } => {
            let task = task(book, project, id)?;
            if !task.runs.iter().any(|attempt| attempt.id == run) {
                return Err("Попытка не найдена".into());
            }
            Ok(vec![task.clone()])
        }
        Request::Preview { id, next } => {
            let task = task(book, project, id)?;
            task.preview_with_rules(next.as_deref(), &book.rules(project))?;
            Ok(vec![task.clone()])
        }
        Request::Create { title, prompt } => {
            let id = book.create(project.to_path_buf(), backend.name(), &title, &prompt)?;
            Ok(vec![book.get(id)?.clone()])
        }
        Request::Status { id, revision, status } => {
            task(book, project, id)?;
            book.set_status(id, revision, status)?;
            Ok(vec![book.get(id)?.clone()])
        }
        Request::Archive { id, revision, archived } => {
            task(book, project, id)?;
            book.set_archived(id, revision, archived)?;
            Ok(vec![book.get(id)?.clone()])
        }
        Request::Assign { id, revision, backend } => {
            task(book, project, id)?;
            book.assign(id, revision, backend.name())?;
            Ok(vec![book.get(id)?.clone()])
        }
        Request::Rules => Ok(Vec::new()),
        Request::SaveRules { revision, enabled, text } => {
            book.set_rules(project.to_path_buf(), revision, enabled, &text)?;
            Ok(Vec::new())
        }
        Request::SaveGitPolicy { revision, policy } => {
            let mut rules = book.rules(project);
            rules.git = policy;
            book.configure_rules(project.to_path_buf(), revision, rules)?;
            Ok(Vec::new())
        }
        Request::SaveProjectRules { revision, rules } => {
            book.configure_rules(project.to_path_buf(), revision, rules)?;
            Ok(Vec::new())
        }
        Request::Start { .. }
        | Request::StartChecked { .. }
        | Request::PrepareGit { .. }
        | Request::PrepareGitChecked { .. }
        | Request::CancelGit => {
            Err("Запуск агента и подготовка Git пока доступны в режиме «В теме Telegram».".into())
        }
    }
}
