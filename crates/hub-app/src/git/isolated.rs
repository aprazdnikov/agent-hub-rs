//! Explicit opt-in worktrees live below the selected project and survive subsequent attempts.
use super::{required, root_head};
use hub_core::tasks::Task;
use std::{
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

pub(crate) async fn worktree(task: &Task, cancel: &CancellationToken) -> Result<PathBuf, String> {
    let project = &task.project;
    root_head(project, cancel).await?;
    let dir = task.execution_dir();
    let home = dir.parent().ok_or("Не удалось определить каталог worktree")?;
    if !required(
        project,
        &["ls-files", "--", ".agent-hub-worktrees"],
        cancel,
        "Не удалось проверить каталог worktree",
    )
    .await?
    .is_empty()
    {
        return Err(
            "Каталог .agent-hub-worktrees отслеживается Git; выберите другой проект.".into()
        );
    }
    if let Err(error) = fs::create_dir(home)
        && error.kind() != ErrorKind::AlreadyExists
    {
        return Err(error.to_string());
    }
    if fs::symlink_metadata(home).map_err(|e| e.to_string())?.file_type().is_symlink()
        || !home.is_dir()
        || fs::canonicalize(home).map_err(|e| e.to_string())?
            != fs::canonicalize(project).map_err(|e| e.to_string())?.join(".agent-hub-worktrees")
    {
        return Err("Каталог worktree недоступен или является ссылкой.".into());
    }
    let ignore = home.join(".gitignore");
    match fs::File::create_new(&ignore) {
        Ok(mut file) => file.write_all(b"*\n").map_err(|e| e.to_string())?,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(&ignore).map_err(|e| e.to_string())?;
            if !metadata.is_file()
                || metadata.len() != 2
                || fs::read(&ignore).map_err(|e| e.to_string())? != b"*\n"
            {
                return Err("Проверьте файл .agent-hub-worktrees/.gitignore".into());
            }
        }
        Err(error) => return Err(error.to_string()),
    }
    let branch = format!("hub/task-{}", task.id);
    if task.runs.is_empty() {
        if fs::symlink_metadata(&dir).is_ok() {
            return Err(format!("Каталог {} уже занят. Проверьте прежние эффекты.", dir.display()));
        }
        required(project,&["worktree","add","-b",&branch,"--",dir.to_str().ok_or("Путь worktree не представлен UTF-8")?,"HEAD"],cancel,"Не удалось создать отдельный worktree от текущего HEAD. Проверьте каталог и имя ветки.").await?;
    }
    let expected = fs::canonicalize(home).map_err(|e| e.to_string())?.join(task.id.to_string());
    if fs::canonicalize(&dir).map_err(|e| e.to_string())? != expected
        || root_head(&dir, cancel).await? != format!("refs/heads/{branch}")
    {
        return Err("Рабочий каталог или ветка задачи изменились.".into());
    }
    if common(project, cancel).await? != common(&dir, cancel).await? {
        return Err("Worktree относится к другому репозиторию".into());
    }
    Ok(dir)
}

async fn common(cwd: &Path, cancel: &CancellationToken) -> Result<PathBuf, String> {
    let common = required(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        cancel,
        "Не удалось проверить принадлежность worktree",
    )
    .await?;
    fs::canonicalize(common).map_err(|e| e.to_string())
}
