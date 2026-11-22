//! Read-only, bounded review snapshots. No patch is applied or replayed.
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

const LIMIT: usize = 16 * 1024;
async fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let read = async {
        let mut cmd = Command::new("git");
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        ] {
            cmd.env_remove(name);
        }
        let mut child = cmd
            .args(["--no-pager", "-c", "core.fsmonitor=false", "-c", "color.ui=false"])
            .args(args)
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .ok()?;
        let mut output = Vec::new();
        child.stdout.take()?.take(LIMIT as u64 + 1).read_to_end(&mut output).await.ok()?;
        if output.len() > LIMIT {
            let _ = child.kill().await;
            output.truncate(LIMIT);
            // A cut-off line is unsuitable as a reference for an annotation.
            output.truncate(output.iter().rposition(|&b| b == b'\n').unwrap_or(0));
            output.extend_from_slice("\n… вывод сокращён …".as_bytes());
        } else if !child.wait().await.ok()?.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output).into_owned())
    };
    tokio::time::timeout(Duration::from_secs(4), read).await.ok().flatten()
}

pub(super) async fn baseline(cwd: &Path) -> Option<String> {
    let sha = git(cwd, &["rev-parse", "--verify", "HEAD"]).await?;
    let sha = sha.trim();
    hub_core::tasks::git::valid_sha(sha).then(|| sha.to_owned())
}

pub(super) async fn capture(cwd: &Path, base: Option<&str>) -> String {
    let Some(sha) = base.filter(|sha| hub_core::tasks::git::valid_sha(sha)) else {
        return "Снимок Git недоступен: у проекта не было исходного коммита.".into();
    };
    let args = [
        "diff",
        "--relative",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        sha,
        "--",
        ".",
    ];
    let (patch, new_files) = tokio::join!(
        git(cwd, &args),
        git(cwd, &["ls-files", "--others", "--exclude-standard", "--", "."])
    );
    let Some(patch) = patch else {
        return "Не удалось прочитать изменения Git.".into();
    };
    let mut text = format!(
        "Рабочий каталог относительно {} при старте попытки (включая ранее незакоммиченные правки).\n",
        &sha[..12]
    );
    if patch.is_empty() {
        text.push_str("Отслеживаемые файлы не изменились.\n");
    } else {
        text.push_str(&patch);
    }
    match new_files {
        Some(files) if !files.trim().is_empty() => {
            text.push_str("\nНовые файлы (содержимое не включено):\n");
            text.push_str(&files);
        }
        None => text.push_str("\nНе удалось получить список новых файлов."),
        _ => {}
    }
    if text.len() > 60 * 1024 {
        text.truncate(text.floor_char_boundary(60 * 1024));
        text.push_str("\n… вывод сокращён …");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn includes_committed_staged_and_untracked_changes_since_start() {
        async fn run(cwd: &Path, args: &[&str]) {
            assert!(
                Command::new("git")
                    .current_dir(cwd)
                    .args([
                        "-c",
                        "user.name=Test",
                        "-c",
                        "user.email=test@example.test",
                        "-c",
                        "commit.gpgSign=false"
                    ])
                    .args(args)
                    .output()
                    .await
                    .unwrap()
                    .status
                    .success()
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        run(cwd, &["init", "-q"]).await;
        std::fs::write(cwd.join("tracked"), "original\n").unwrap();
        run(cwd, &["add", "tracked"]).await;
        run(cwd, &["commit", "-qm", "base"]).await;
        let base = baseline(cwd).await.unwrap();
        std::fs::write(cwd.join("tracked"), "edited\n").unwrap();
        run(cwd, &["commit", "-qam", "change"]).await;
        std::fs::write(cwd.join("staged"), "staged content\n").unwrap();
        run(cwd, &["add", "staged"]).await;
        std::fs::write(cwd.join("untracked"), "untracked content").unwrap();
        let diff = capture(cwd, Some(&base)).await;
        assert!(
            diff.contains("+edited")
                && diff.contains("+staged content")
                && diff.contains("untracked")
        );
        assert!(!diff.contains("untracked content"));
    }
}
