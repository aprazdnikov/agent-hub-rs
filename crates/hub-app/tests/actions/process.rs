use std::{path::Path, time::Duration};

use hub_app::actions::{ProcessResult, ProcessStatus, run};
use tokio_util::sync::CancellationToken;

async fn execute(command: &str, cwd: &Path) -> anyhow::Result<ProcessResult> {
    Ok(tokio::time::timeout(Duration::from_secs(4), run(command, cwd, CancellationToken::new()))
        .await?)
}

#[cfg(unix)]
#[tokio::test]
async fn captures_separate_streams_and_nonzero_exit_without_retry() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let result =
        execute("printf x >> attempts; printf out; printf err >&2; exit 7", cwd.path()).await?;
    assert_eq!(
        (result.status, result.stdout, result.stderr, std::fs::read(cwd.path().join("attempts"))?),
        (ProcessStatus::Completed(Some(7)), "out".into(), "err".into(), b"x".to_vec())
    );
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn captures_separate_streams_and_nonzero_exit_without_retry() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let result =
        execute("echo x>>attempts & echo out & echo err 1>&2 & exit /B 7", cwd.path()).await?;
    assert_eq!(
        (
            result.status,
            result.stdout.trim(),
            result.stderr.trim(),
            std::fs::read_to_string(cwd.path().join("attempts"))?.trim()
        ),
        (ProcessStatus::Completed(Some(7)), "out", "err", "x")
    );
    Ok(())
}

#[tokio::test]
async fn invalid_working_directory_fails_without_exposing_command() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let result = execute("secret-command-marker", &cwd.path().join("missing")).await?;
    let ProcessStatus::Failed(error) = result.status else {
        anyhow::bail!("Invalid directory should fail to spawn");
    };
    assert!(!error.contains("secret-command-marker"));
    Ok(())
}

#[tokio::test]
async fn cancellation_before_start_does_not_run_command() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let result = run("echo unexpected > started", cwd.path(), cancel).await;
    assert_eq!(
        (result.status, cwd.path().join("started").exists()),
        (ProcessStatus::Stopped, false)
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn drains_large_output_and_retains_bounded_tail() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let result = execute(
        "i=0; while [ $i -lt 3000 ]; do printf 0123456789; printf abcdefghij >&2; i=$((i + 1)); done; printf stdout-end; printf stderr-end >&2",
        cwd.path(),
    ).await?;
    assert!(matches!(result.status, ProcessStatus::Completed(Some(0))));
    assert!(result.stdout.len() <= 4096 && result.stderr.len() <= 4096);
    assert!(result.stdout.contains("обрезан") && result.stderr.contains("обрезан"));
    assert!(result.stdout.ends_with("stdout-end") && result.stderr.ends_with("stderr-end"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn stdin_is_closed_without_a_terminal() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let result = execute("if read value; then exit 9; else printf closed; fi", cwd.path()).await?;
    assert_eq!(
        (result.status, result.stdout),
        (ProcessStatus::Completed(Some(0)), "closed".into())
    );
    Ok(())
}

#[cfg(unix)]
const TREE_COMMAND: &str = "sh -c 'trap \"\" TERM; echo ready > ready; sleep 3; echo survived > survived; sleep 30' & wait";

#[cfg(unix)]
#[tokio::test]
async fn cancellation_allows_term_cleanup_before_force_stopping() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let path = cwd.path().to_owned();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        // wait is interrupted by TERM; a synchronous sleep can defer the trap
        // past the grace period if it is forked while the group is signalled.
        run(r#"trap 'echo cleaned > cleaned; exit 0' TERM; sleep 30 & sleeper=$!; echo ready > ready; wait "$sleeper""#, &path, task_cancel).await
    });
    wait_for_file(&cwd.path().join("ready")).await?;
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), task).await??;
    assert_eq!(
        (result.status, cwd.path().join("cleaned").exists()),
        (ProcessStatus::Stopped, true)
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn signalled_shell_cleans_up_term_resistant_descendants() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let result = execute(
        "sh -c 'trap \"\" TERM; echo ready > ready; sleep 1; echo survived > survived' & while [ ! -e ready ]; do sleep 0.01; done; kill -TERM $$",
        cwd.path(),
    ).await?;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        (result.status, cwd.path().join("survived").exists()),
        (ProcessStatus::Completed(None), false)
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn cancellation_allows_descendant_cleanup_after_the_shell_exits() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let path = cwd.path().to_owned();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        // Start the child before publishing readiness, then use an interruptible
        // wait so the descendant can run its delayed trap after the shell exits.
        run(r#"sh -c 'trap "sleep 0.1; echo cleaned > cleaned; exit 0" TERM; sleep 30 & sleeper=$!; echo ready > ready; wait "$sleeper"' & wait"#, &path, task_cancel).await
    });
    wait_for_file(&cwd.path().join("ready")).await?;
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), task).await??;
    assert_eq!(
        (result.status, cwd.path().join("cleaned").exists()),
        (ProcessStatus::Stopped, true)
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn invalid_utf8_cannot_expand_output_past_the_limit() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let result = execute(
        "i=0; while [ $i -lt 6000 ]; do printf '\\377'; i=$((i + 1)); done; printf end",
        cwd.path(),
    )
    .await?;
    assert!(matches!(result.status, ProcessStatus::Completed(Some(0))));
    assert!(result.stdout.len() <= 4096);
    assert!(result.stdout.contains("обрезан") && result.stdout.ends_with("end"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn command_has_no_runtime_deadline() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let path = cwd.path().to_owned();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task =
        tokio::spawn(async move { run("echo ready > ready; sleep 30", &path, task_cancel).await });
    wait_for_file(&cwd.path().join("ready")).await?;
    tokio::time::pause();
    tokio::time::advance(Duration::from_hours(720)).await;
    tokio::task::yield_now().await;
    let still_running = !task.is_finished();
    tokio::time::resume();
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(3), task).await??;
    assert_eq!((still_running, result.status), (true, ProcessStatus::Stopped));
    Ok(())
}

#[cfg(unix)]
async fn wait_for_file(path: &Path) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn long_running_command_stays_active_until_cancelled_and_stops_descendants()
-> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let path = cwd.path().to_owned();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let mut task = tokio::spawn(async move { run(TREE_COMMAND, &path, task_cancel).await });
    wait_for_file(&cwd.path().join("ready")).await?;
    assert!(tokio::time::timeout(Duration::from_millis(150), &mut task).await.is_err());
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(3), task).await??;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        (result.status, cwd.path().join("survived").exists()),
        (ProcessStatus::Stopped, false)
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_run_future_stops_descendants() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let path = cwd.path().to_owned();
    let task =
        tokio::spawn(async move { run(TREE_COMMAND, &path, CancellationToken::new()).await });
    wait_for_file(&cwd.path().join("ready")).await?;
    task.abort();
    assert!(task.await.is_err_and(|error| error.is_cancelled()));
    tokio::time::sleep(Duration::from_millis(3100)).await;
    assert!(!cwd.path().join("survived").exists());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn completed_shell_cleans_up_remaining_background_descendants() -> anyhow::Result<()> {
    let cwd = tempfile::tempdir()?;
    let result = execute("sh -c 'sleep 1; echo survived > survived' & exit 0", cwd.path()).await?;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(
        (result.status, cwd.path().join("survived").exists()),
        (ProcessStatus::Completed(Some(0)), false)
    );
    Ok(())
}
