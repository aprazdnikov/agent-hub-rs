//! Exercise actual fsync failures in an isolated child; production has no fault-injection hook.
use hub_memory::{BindingMode, BindingPreview, EntryKind, MemoryReader, Store, WriteRequest};
use std::{path::Path, process::Output, time::Duration};
use tokio::process::Command;

async fn failing_move(root: &Path, stage: &str) -> anyhow::Result<Output> {
    let source = root.join("source");
    let target = root.join("target");
    let base = root.join("memory");
    std::fs::create_dir(&source)?;
    std::fs::create_dir(&target)?;
    Store::open(&base, &source, "test")?.write(WriteRequest {
        key: "key".into(),
        expected_revision: 0,
        kind: EntryKind::Context,
        title: "title".into(),
        body: "body".into(),
        sources: vec!["test".into()],
    })?;
    let library = root.join("fault.so");
    let compiler = Command::new("cc")
        .args(["-shared", "-fPIC", "-O2", "-Wall", "-Werror"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/memory_fsync_failure.c"))
        .arg("-ldl")
        .arg("-o")
        .arg(&library)
        .kill_on_drop(true)
        .output();
    let status = tokio::time::timeout(Duration::from_secs(30), compiler).await??;
    anyhow::ensure!(
        status.status.success(),
        "fault library: {}",
        String::from_utf8_lossy(&status.stderr)
    );
    let preview = BindingPreview::prepare(&base, &source, &target, BindingMode::Move)?;
    let output = Command::new(env!("CARGO_BIN_EXE_agent-hub"))
        .args(["--memory-project", "move", "--from"])
        .arg(&source)
        .arg("--to")
        .arg(&target)
        .arg("--store-dir")
        .arg(&base)
        .arg("--confirm")
        .arg(preview.confirmation())
        .env("LD_PRELOAD", library)
        .env("MEM04_FAULT_BASE", std::fs::canonicalize(&base)?)
        .env("MEM04_FAULT_STAGE", stage)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .kill_on_drop(true)
        .output();
    Ok(tokio::time::timeout(Duration::from_secs(15), output).await??)
}

#[tokio::test]
async fn parent_sync_publication_failure_keeps_source_binding() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let output = failing_move(root.path(), "parent").await?;
    assert!(!output.status.success(), "parent directory was not synced");
    let base = root.path().join("memory");
    assert!(!base.join(".projects/memory.json").exists());
    assert_eq!(
        MemoryReader::new(&base, &root.path().join("source"))?.get("key", None)?.revision,
        1
    );
    assert!(MemoryReader::new(&base, &root.path().join("target"))?.get("key", None).is_err());
    Ok(())
}

#[tokio::test]
async fn post_rename_publication_failure_reports_uncertainty_and_destination() -> anyhow::Result<()>
{
    let root = tempfile::tempdir()?;
    let output = failing_move(root.path(), "published").await?;
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("Сохранность привязки после сбоя диска не подтверждена"), "{error}");
    assert!(
        error.contains(&std::fs::canonicalize(root.path().join("target"))?.display().to_string())
    );
    let base = root.path().join("memory");
    assert_eq!(
        MemoryReader::new(&base, &root.path().join("target"))?.get("key", None)?.revision,
        1
    );
    assert!(MemoryReader::new(&base, &root.path().join("source")).is_err());
    Ok(())
}
