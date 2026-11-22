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
