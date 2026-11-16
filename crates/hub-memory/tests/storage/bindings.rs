use hub_memory::{BindingMode, BindingPreview, EntryKind, MemoryReader, Store, WriteRequest};
use std::{fs, path::Path};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn request(revision: u32) -> WriteRequest {
    WriteRequest {
        key: "key".into(),
        expected_revision: revision,
        kind: EntryKind::Progress,
        title: "shared".into(),
        body: format!("revision {revision}"),
        sources: vec!["test".into()],
    }
}
fn write(base: &Path, project: &Path) -> Result<Store> {
    let store = Store::open(base, project, "test")?;
    store.write(request(0))?;
    Ok(store)
}
#[test]
fn move_after_rename_keeps_history_and_rejects_old_handles() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let old = root.path().join("old");
    let new = root.path().join("new");
    fs::create_dir(&old)?;
    let old = fs::canonicalize(old)?;
    let store = write(&base, &old)?;
    let reader = MemoryReader::new(&base, &old)?;
    let original = reader.inspect("", 0, 20)?.directory;
    let bytes = fs::read(original.join("memory.json"))?;
    fs::rename(&old, &new)?;
    let preview = BindingPreview::prepare(&base, &old, &new, BindingMode::Move)?;
    preview.apply()?;
    let moved = Store::open(&base, &new, "qwen")?;
    assert_eq!(moved.get("key", None)?.revision, 1);
    assert_eq!(MemoryReader::new(&base, &new)?.inspect("", 0, 20)?.directory, original);
    assert_eq!(fs::read(original.join("memory.json"))?, bytes);
    assert!(store.write(request(1)).is_err());
    assert!(reader.get("key", None).is_err());
    fs::create_dir(&old)?;
    assert!(Store::open(&base, &old, "old").is_err());
    moved.write(request(1))?;
    assert_eq!(moved.get("key", None)?.revision, 2);
    Ok(())
}
#[test]
fn shared_worktree_uses_same_revisions_but_unrelated_directory_is_isolated() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let source = root.path().join("source");
    let target = root.path().join("target");
    let other = root.path().join("other");
    for path in [&source, &target, &other] {
        fs::create_dir(path)?;
    }
    let source_store = write(&base, &source)?;
    let stale = Store::open(&base, &target, "old")?;
    let stale_reader = MemoryReader::new(&base, &target)?;
    let preview = BindingPreview::prepare(&base, &source, &target, BindingMode::Share)?;
    preview.clone().apply()?;
    assert!(preview.apply().is_err());
    assert!(stale.write(request(0)).is_err());
    assert!(stale_reader.inspect("", 0, 20).is_err());
    Store::open(&base, &target, "new")?.write(request(1))?;
    assert_eq!(source_store.get("key", None)?.revision, 2);
    let page = MemoryReader::new(&base, &source)?.inspect("", 0, 20)?;
    assert_eq!(page.bound_projects.len(), 2);
    assert!(page.bound_projects.contains(&fs::canonicalize(&target)?));
    assert!(Store::open(&base, &other, "other")?.search("", 20)?.is_empty());
    Ok(())
}
#[test]
fn destination_data_and_source_changes_invalidate_binding() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir(&source)?;
    fs::create_dir(&target)?;
    let store = write(&base, &source)?;
    let preview = BindingPreview::prepare(&base, &source, &target, BindingMode::Share)?;
    store.write(request(1))?;
    assert!(preview.apply().is_err());
    let preview = BindingPreview::prepare(&base, &source, &target, BindingMode::Share)?;
    write(&base, &target)?;
    assert!(preview.apply().is_err());
    assert!(BindingPreview::prepare(&base, &source, &target, BindingMode::Share).is_err());
    assert_eq!(store.get("key", None)?.revision, 2);
    assert_eq!(Store::open(&base, &target, "test")?.get("key", None)?.revision, 1);
    Ok(())
}
#[test]
fn corrupt_registry_fails_closed_and_staging_does_not_publish() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir(&source)?;
    fs::create_dir(&target)?;
    write(&base, &source)?;
    fs::create_dir(base.join(".projects"))?;
    fs::write(base.join(".projects/.memory.tmp"), b"partial")?;
    assert!(MemoryReader::new(&base, &target)?.inspect("", 0, 20)?.entries.is_empty());
    BindingPreview::prepare(&base, &source, &target, BindingMode::Share)?.apply()?;
    assert_eq!(Store::open(&base, &target, "test")?.get("key", None)?.revision, 1);
    fs::write(base.join(".projects/memory.json"), b"broken")?;
    assert!(Store::open(&base, &source, "test").is_err());
    assert!(MemoryReader::new(&base, &target).is_err());
    Ok(())
}
#[cfg(unix)]
#[test]
fn symlink_registry_is_rejected() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let source = root.path().join("source");
    fs::create_dir(&base)?;
    fs::create_dir(&source)?;
    std::os::unix::fs::symlink(&source, base.join(".projects"))?;
    assert!(Store::open(&base, &source, "test").is_err());
    assert!(MemoryReader::new(&base, &source).is_err());
    Ok(())
}

#[test]
fn legacy_v1_move_preserves_exact_snapshot_then_backs_up_on_write() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let old = root.path().join("old");
    let new = root.path().join("new");
    fs::create_dir(&old)?;
    let old = fs::canonicalize(old)?;
    let store = write(&base, &old)?;
    let directory = MemoryReader::new(&base, &old)?.inspect("", 0, 20)?.directory;
    let entry = store.get("key", None)?;
    let bytes = serde_json::to_vec(
        &serde_json::json!({"schema_version":1,"project":old,"entries":{"key":[entry]}}),
    )?;
    fs::write(directory.join("memory.json"), &bytes)?;
    fs::rename(&old, &new)?;
    BindingPreview::prepare(&base, &old, &new, BindingMode::Move)?.apply()?;
    assert_eq!(fs::read(directory.join("memory.json"))?, bytes);
    Store::open(&base, &new, "qwen")?.write(request(1))?;
    assert_eq!(fs::read(directory.join("memory.v1.backup.json"))?, bytes);
    assert_eq!(MemoryReader::new(&base, &new)?.get("key", Some(1))?, entry);
    Ok(())
}

#[test]
fn binding_refuses_busy_source_and_snapshot_with_no_live_entries() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let source = root.path().join("source");
    let target = root.path().join("target");
    fs::create_dir(&source)?;
    fs::create_dir(&target)?;
    write(&base, &source)?;
    let preview = BindingPreview::prepare(&base, &source, &target, BindingMode::Share)?;
    let dir = MemoryReader::new(&base, &source)?.inspect("", 0, 20)?.directory;
    let lock = fs::File::open(dir.join(".lock"))?;
    lock.lock()?;
    assert!(matches!(preview.apply(), Err(hub_memory::Error::Busy)));
    drop(lock);
    let target_store = Store::open(&base, &target, "test")?;
    target_store.apply(
        target_store
            .preview(hub_memory::AdminAction::SetPolicy(hub_memory::CleanupPolicy::Automatic))?,
    )?;
    assert!(BindingPreview::prepare(&base, &source, &target, BindingMode::Share).is_err());
    Ok(())
}

#[test]
fn import_preview_after_move_names_the_current_destination() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let old = root.path().join("old");
    let new = root.path().join("new");
    fs::create_dir(&old)?;
    fs::create_dir(&new)?;
    write(&base, &old)?;
    let export = MemoryReader::new(&base, &old)?.export()?;
    BindingPreview::prepare(&base, &old, &new, BindingMode::Move)?.apply()?;
    let preview = Store::open(&base, &new, "human")?
        .preview_import(export, std::collections::BTreeMap::new())?;
    assert_eq!(preview.target, fs::canonicalize(&new)?);
    Ok(())
}
