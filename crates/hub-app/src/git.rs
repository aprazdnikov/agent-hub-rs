//! Conservative Git preparation. Commands use argument arrays, never a shell.
use crate::actions::{ProcessStatus, run_program};
use futures::future::BoxFuture;
use hub_core::tasks::git::valid_sha;
use hub_telegram::git::{GitRunner, Request};
use std::{path::Path, time::Duration};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub struct LocalGitRunner;
impl GitRunner for LocalGitRunner {
    fn run(
        &self,
        request: Request,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(run(request, cancel))
    }
}
async fn command(
    cwd: &Path,
    args: &[&str],
    cancel: &CancellationToken,
) -> Result<(bool, String), String> {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "submodule.recurse=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "Never")
        .env("GIT_SSH_COMMAND", "ssh -oBatchMode=yes");
    // Ambient Git overrides must not redirect a project operation to another repository.
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(name);
    }
    let result = run_program(command, cancel.clone()).await;
    match result.status {
        ProcessStatus::Completed(code) => {
            if result.stdout.starts_with("[вывод обрезан]") {
                return Err(
                    "Вывод Git превысил допустимый размер; проверьте репозиторий вручную.".into()
                );
            }
            Ok((code == Some(0), result.stdout.trim_end_matches(['\r', '\n']).to_owned()))
        }
        ProcessStatus::Stopped => {
            Err("Подготовка остановлена. Проверьте Git: часть действий могла выполниться.".into())
        }
        ProcessStatus::Failed(_) => {
            Err("Не удалось выполнить Git. Проверьте установку Git и доступ к каталогу.".into())
        }
    }
}
async fn required(
    cwd: &Path,
    args: &[&str],
    cancel: &CancellationToken,
    message: &str,
) -> Result<String, String> {
    let (ok, text) = command(cwd, args, cancel).await?;
    if ok { Ok(text) } else { Err(message.into()) }
}
async fn clean(cwd: &Path, cancel: &CancellationToken) -> Result<(), String> {
    let status = required(
        cwd,
        &["status", "--porcelain=v1", "--untracked-files=normal", "--ignore-submodules=none"],
        cancel,
        "Не удалось проверить рабочий каталог.",
    )
    .await?;
    if !status.is_empty() {
        return Err("В каталоге есть незакоммиченные или новые файлы. Сохраните их самостоятельно; приложение не делает stash и не удаляет изменения.".into());
    }
    no_operation(cwd, cancel).await
}
async fn no_operation(cwd: &Path, cancel: &CancellationToken) -> Result<(), String> {
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "BISECT_START",
    ] {
        let path = required(
            cwd,
            &["rev-parse", "--git-path", marker],
            cancel,
            "Не удалось проверить незавершённые операции Git.",
        )
        .await?;
        if cwd.join(path).exists() {
            return Err("В репозитории не завершена операция Git (merge/rebase/cherry-pick/bisect). Завершите её самостоятельно.".into());
        }
    }
    Ok(())
}
async fn branch_available(request: &Request, cancel: &CancellationToken) -> Result<(), String> {
    let git = &request.preparation;
    let target = format!("refs/heads/{}", git.branch);
    if command(&request.project, &["show-ref", "--verify", "--quiet", &target], cancel).await?.0 {
        return Err("Рабочая ветка уже существует. Укажите новое имя; существующая ветка не перезаписывается.".into());
    }
    let trees = required(
        &request.project,
        &["worktree", "list", "--porcelain", "-z"],
        cancel,
        "Не удалось проверить worktree.",
    )
    .await?;
    let root = std::fs::canonicalize(&request.project).map_err(|_| "Каталог проекта недоступен")?;
    for record in trees.split("\0\0") {
        let Some(path) = record.split('\0').find_map(|field| field.strip_prefix("worktree "))
        else {
            continue;
        };
        let location = std::fs::canonicalize(path)
            .map_err(|_| "Один из worktree недоступен. Проверьте git worktree list.")?;
        if location == root {
            continue;
        }
        // A detached worktree may still reserve its original branch during rebase/bisect.
        no_operation(&location, cancel).await.map_err(|_| "В другом worktree не завершена операция Git или его состояние недоступно. Проверьте worktree перед подготовкой.".to_owned())?;
        if record.split('\0').any(|field| field == format!("branch refs/heads/{}", git.policy.base))
        {
            return Err(
                "Базовая ветка открыта в другом worktree. Освободите её самостоятельно.".into()
            );
        }
    }
    Ok(())
}
async fn root_head(cwd: &Path, cancel: &CancellationToken) -> Result<String, String> {
    let root = required(
        cwd,
        &["rev-parse", "--show-toplevel"],
        cancel,
        "Выбранный каталог не является рабочим репозиторием Git.",
    )
    .await?;
    if std::fs::canonicalize(root).ok() != std::fs::canonicalize(cwd).ok() {
        return Err("Выберите корень рабочего репозитория, а не вложенный каталог.".into());
    }
    let head = required(
        cwd,
        &["symbolic-ref", "--quiet", "HEAD"],
        cancel,
        "HEAD не указывает на ветку. Сначала выберите ветку вручную.",
    )
    .await?;
    Ok(head)
}
async fn verify_start(
    request: &Request,
    head: &str,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let cwd = &request.project;
    let git = &request.preparation;
    if head != format!("refs/heads/{}", git.branch) {
        return Err(format!(
            "Выбрана другая ветка. Переключитесь на {} перед запуском.",
            git.branch
        ));
    }
    if !request.continuation {
        clean(cwd, cancel).await?;
        if required(cwd, &["rev-parse", "HEAD"], cancel, "Не удалось прочитать HEAD.").await?
            != git.sha
        {
            return Err(
                "Ветка изменилась после подготовки. Проверьте Git перед первым запуском.".into()
            );
        }
    }
    required(
        cwd,
        &["merge-base", "--is-ancestor", &git.sha, "HEAD"],
        cancel,
        "История рабочей ветки больше не содержит подготовленную базу. Проверьте Git.",
    )
    .await?;
    Ok(git.sha.clone())
}
async fn validate_names(request: &Request, cancel: &CancellationToken) -> Result<(), String> {
    let cwd = &request.project;
    let git = &request.preparation;
    for branch in [&git.policy.base, &git.branch] {
        required(
            cwd,
            &["check-ref-format", "--branch", branch],
            cancel,
            "Некорректное имя ветки Git.",
        )
        .await?;
    }
    required(
        cwd,
        &["remote", "get-url", "--", &git.policy.remote],
        cancel,
        "Такой remote не найден. Укажите его имя из git remote.",
    )
    .await?;
    Ok(())
}
async fn execute(request: &Request, cancel: &CancellationToken) -> Result<String, String> {
    let cwd = &request.project;
    let git = &request.preparation;
    git.validate()?;
    let head = root_head(cwd, cancel).await?;
    if request.verify_only {
        return verify_start(request, &head, cancel).await;
    }
    clean(cwd, cancel).await?;
    validate_names(request, cancel).await?;
    branch_available(request, cancel).await?;
    let base = format!("refs/heads/{}", git.policy.base);
    let old = command(cwd, &["rev-parse", "--verify", &base], cancel).await?;
    let tracking = format!("refs/remotes/{}/{}", git.policy.remote, git.policy.base);
    let refspec = format!("+{base}:{tracking}");
    required(cwd, &["fetch", "--no-tags", "--no-prune", "--refmap=", "--no-recurse-submodules", "--", &git.policy.remote, &refspec], cancel, "Не удалось получить базовую ветку. Проверьте сеть, доступ к remote и точное имя ветки; подготовка остановлена.").await?;
    let sha = required(
        cwd,
        &["rev-parse", "--verify", &tracking],
        cancel,
        "Не удалось прочитать обновлённую базу.",
    )
    .await?;
    if !valid_sha(&sha) {
        return Err("Git вернул некорректный SHA базы.".into());
    }
    if old.0 {
        required(cwd, &["merge-base", "--is-ancestor", &old.1, &sha], cancel, "Локальная база содержит коммиты, отсутствующие на remote. Разберите расхождение вручную; reset и rebase не выполнялись.").await?;
    }
    clean(cwd, cancel).await?;
    branch_available(request, cancel).await?;
    if required(
        cwd,
        &["symbolic-ref", "--quiet", "HEAD"],
        cancel,
        "Ветка изменилась во время подготовки.",
    )
    .await?
        != head
    {
        return Err("Ветка изменилась во время подготовки; повторите после проверки Git.".into());
    }
    if head == base {
        if required(cwd, &["rev-parse", "--verify", &base], cancel, "Не удалось прочитать базу.")
            .await?
            != old.1
        {
            return Err("База изменилась во время подготовки. Проверьте Git.".into());
        }
        required(
            cwd,
            &["merge", "--ff-only", "--no-overwrite-ignore", &sha],
            cancel,
            "Не удалось обновить базовую ветку fast-forward. Проверьте Git.",
        )
        .await?;
    } else {
        let expected = if old.0 { old.1 } else { "0".repeat(sha.len()) };
        required(
            cwd,
            &["update-ref", &base, &sha, &expected],
            cancel,
            "Базовая ветка изменилась во время подготовки. Проверьте Git.",
        )
        .await?;
    }
    if required(cwd, &["rev-parse", "--verify", &base], cancel, "Не удалось проверить базу.")
        .await?
        != sha
    {
        return Err("База изменилась во время подготовки. Рабочая ветка не создавалась.".into());
    }
    clean(cwd, cancel).await?;
    required(
        cwd,
        &["switch", "--no-track", "--no-overwrite-ignore", "-c", &git.branch, &sha],
        cancel,
        "База обновлена, но создать рабочую ветку не удалось. Проверьте Git перед повтором.",
    )
    .await?;
    let actual = required(
        cwd,
        &["rev-parse", "HEAD"],
        cancel,
        "Ветка создана, но прочитать её SHA не удалось.",
    )
    .await?;
    if actual != sha {
        return Err("После создания ветки её SHA изменился. Проверьте Git.".into());
    }
    Ok(sha)
}
pub async fn run(request: Request, cancel: CancellationToken) -> Result<String, String> {
    let operation = cancel.child_token();
    if let Ok(result) =
        tokio::time::timeout(Duration::from_mins(2), execute(&request, &operation)).await
    {
        result
    } else {
        operation.cancel();
        Err("Истекло время подготовки Git. Проверьте репозиторий: часть действий могла выполниться; автоматического повтора нет.".into())
    }
}
