use hub_memory::{
    BindingMode, BindingPreview, EntryKind, Error, MemoryReader, PurgeReport, Store, WriteRequest,
    purge,
};
use std::{fs, path::Path};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn write(base: &Path, project: &Path) -> Result {
    let store = Store::open(base, project, "test")?;
    store.write(WriteRequest {
        key: "key".into(),
        expected_revision: 0,
        kind: EntryKind::Progress,
        title: "kept".into(),
        body: "body".into(),
        sources: vec!["test".into()],
    })?;
    Ok(())
}

fn registry_paths(base: &Path) -> Result<serde_json::Value> {
    let raw = fs::read_to_string(base.join(".projects").join("memory.json"))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw)?;
    parsed.get("paths").cloned().ok_or_else(|| "registry has no paths map".into())
}

#[test]
fn purge_removes_both_bound_paths_and_the_data_directory() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let old = root.path().join("old");
    let new = root.path().join("new");
    let other = root.path().join("other");
    for path in [&old, &new, &other] {
        fs::create_dir(path)?;
    }
    let old = fs::canonicalize(old)?;
    let other = fs::canonicalize(other)?;
    write(&base, &old)?;
    let directory = MemoryReader::new(&base, &old)?.inspect("", 0, 20)?.directory;
    fs::rename(&old, &new)?;
    let new = fs::canonicalize(new)?;
    BindingPreview::prepare(&base, &old, &new, BindingMode::Move)?.apply()?;
    write(&base, &other)?;

    // Only the moved project itself remains: nothing else resolves to the old identity,
    // so the whole lineage (binding, tombstone, data directory) goes.
    let report = purge(&base, &new, std::slice::from_ref(&other))?;
    assert_eq!(report, PurgeReport { removed_paths: 2, had_data: true, shared: false });
    assert!(!directory.exists());
    assert_eq!(registry_paths(&base)?, serde_json::json!({}));
    // The project directory itself is never part of the cascade.
    assert!(new.is_dir());
    assert_eq!(MemoryReader::new(&base, &other)?.get("key", None)?.revision, 1);

    let again = purge(&base, &new, &[other])?;
    assert_eq!(again, PurgeReport { removed_paths: 0, had_data: false, shared: false });
    Ok(())
}

#[test]
fn purge_of_a_shared_source_keeps_the_neighbour_storage_and_records() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let source = root.path().join("source");
    let target = root.path().join("target");
    for path in [&source, &target] {
        fs::create_dir(path)?;
    }
    let source = fs::canonicalize(source)?;
    let target = fs::canonicalize(target)?;
    write(&base, &source)?;
    let directory = MemoryReader::new(&base, &source)?.inspect("", 0, 20)?.directory;
    BindingPreview::prepare(&base, &source, &target, BindingMode::Share)?.apply()?;

    // Б1 regression: the neighbour lives in Share, so its storage and registry record
    // must survive the source's purge.
    let report = purge(&base, &source, std::slice::from_ref(&target))?;
    assert_eq!(report, PurgeReport { removed_paths: 0, had_data: false, shared: true });
    assert!(directory.exists());
    assert_eq!(MemoryReader::new(&base, &target)?.get("key", None)?.revision, 1);
    assert_eq!(
        registry_paths(&base)?,
        serde_json::json!({ target.to_str().unwrap(): source.to_str().unwrap() })
    );

    // The last living holder releasing the storage deletes it for good.
    let report = purge(&base, &target, &[])?;
    assert_eq!(report, PurgeReport { removed_paths: 1, had_data: true, shared: false });
    assert!(!directory.exists());
    assert_eq!(registry_paths(&base)?, serde_json::json!({}));
    Ok(())
}

#[test]
fn purge_of_a_shared_target_only_removes_its_own_binding() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let source = root.path().join("source");
    let target = root.path().join("target");
    for path in [&source, &target] {
        fs::create_dir(path)?;
    }
    let source = fs::canonicalize(source)?;
    let target = fs::canonicalize(target)?;
    write(&base, &source)?;
    let directory = MemoryReader::new(&base, &source)?.inspect("", 0, 20)?.directory;
    BindingPreview::prepare(&base, &source, &target, BindingMode::Share)?.apply()?;

    // The identity belongs to the still-living source: its own record and directory stay.
    let report = purge(&base, &target, std::slice::from_ref(&source))?;
    assert_eq!(report, PurgeReport { removed_paths: 1, had_data: false, shared: true });
    assert!(directory.exists());
    assert_eq!(MemoryReader::new(&base, &source)?.get("key", None)?.revision, 1);
    assert_eq!(registry_paths(&base)?, serde_json::json!({}));

    // With the source profile gone too, purging it releases the storage.
    let report = purge(&base, &source, &[])?;
    assert_eq!(report, PurgeReport { removed_paths: 0, had_data: true, shared: false });
    assert!(!directory.exists());
    Ok(())
}

#[test]
fn purge_serializes_with_binding_apply_behind_the_registry_lock() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let source = root.path().join("source");
    let target = root.path().join("target");
    let alone = root.path().join("alone");
    for path in [&source, &target, &alone] {
        fs::create_dir(path)?;
    }
    let source = fs::canonicalize(source)?;
    let target = fs::canonicalize(target)?;
    let alone = fs::canonicalize(alone)?;
    write(&base, &source)?;
    let directory = MemoryReader::new(&base, &source)?.inspect("", 0, 20)?.directory;
    BindingPreview::prepare(&base, &source, &target, BindingMode::Share)?.apply()?;
    write(&base, &alone)?;
    let alone_directory = MemoryReader::new(&base, &alone)?.inspect("", 0, 20)?.directory;

    // Б2 regression: `BindingPreview::apply` publishes `.projects` under its lock; purge
    // must respect the same lock instead of racing a read-modify-write on it.
    let registry_lock = fs::File::open(base.join(".projects").join(".lock"))?;
    registry_lock.lock()?;
    let error = purge(&base, &source, std::slice::from_ref(&target)).unwrap_err();
    assert!(matches!(error, Error::Busy), "got: {error}");
    assert_eq!(
        registry_paths(&base)?,
        serde_json::json!({ target.to_str().unwrap(): source.to_str().unwrap() }),
        "a locked registry must not change"
    );
    assert!(directory.exists());
    drop(registry_lock);
    assert!(purge(&base, &source, &[target])?.shared);
    // The untouched second project still reads its own storage.
    assert_eq!(MemoryReader::new(&base, &alone)?.get("key", None)?.revision, 1);
    assert!(alone_directory.exists());
    Ok(())
}

#[test]
fn purge_of_a_vanished_project_directory_still_cleans_the_registry() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let source = root.path().join("source");
    let target = root.path().join("target");
    for path in [&source, &target] {
        fs::create_dir(path)?;
    }
    let source = fs::canonicalize(source)?;
    write(&base, &source)?;
    let directory = MemoryReader::new(&base, &source)?.inspect("", 0, 20)?.directory;
    // The project folder disappears the way users delete it: without a move binding.
    fs::remove_dir_all(&source)?;

    // Б3 regression: a vanished directory must not turn the memory step into a terminal
    // failure — the retry of the cascade has to be able to clean the registry and data.
    let report = purge(&base, &source, &[])?;
    assert_eq!(report, PurgeReport { removed_paths: 0, had_data: true, shared: false });
    assert!(!directory.exists());
    // Repeating the purge of an absent path stays a successful no-op.
    let again = purge(&base, &source, &[])?;
    assert_eq!(again, PurgeReport { removed_paths: 0, had_data: false, shared: false });
    Ok(())
}

#[test]
fn purge_of_a_moved_away_source_cleans_the_lineage_in_dependency_order() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    fs::create_dir(root.path().join("old"))?;
    let old = fs::canonicalize(root.path().join("old"))?;
    write(&base, &old)?;
    let directory = MemoryReader::new(&base, &old)?.inspect("", 0, 20)?.directory;
    fs::rename(&old, root.path().join("new"))?;
    let new = fs::canonicalize(root.path().join("new"))?;
    BindingPreview::prepare(&base, &old, &new, BindingMode::Move)?.apply()?;
    // Re-opening the former path hits the tombstone: this used to abort the purge.
    fs::create_dir(&old)?;
    assert!(matches!(MemoryReader::new(&base, &old), Err(Error::BindingChanged)));

    // The stale profile of the former path goes first while the moved project lives:
    // the tombstone and the shared directory must stay for it.
    let report = purge(&base, &old, std::slice::from_ref(&new))?;
    assert_eq!(report, PurgeReport { removed_paths: 0, had_data: false, shared: true });
    assert!(directory.exists());
    assert_eq!(MemoryReader::new(&base, &new)?.get("key", None)?.revision, 1);

    // With only the moved profile left, purging it removes the whole lineage.
    let report = purge(&base, &new, &[])?;
    assert_eq!(report, PurgeReport { removed_paths: 2, had_data: true, shared: false });
    assert!(!directory.exists());
    assert_eq!(registry_paths(&base)?, serde_json::json!({}));
    Ok(())
}

#[test]
fn purge_rejects_relative_or_file_paths_but_accepts_absent_directories() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    fs::create_dir(&base)?;
    assert!(matches!(purge(&base, Path::new("relative"), &[]), Err(Error::InvalidInput(_))));
    let file = root.path().join("file");
    fs::write(&file, b"x")?;
    assert!(matches!(purge(&base, &file, &[]), Err(Error::InvalidInput(_))));
    // The absent canonical directory is the retryable case from Б3, not an error.
    assert_eq!(purge(&base, &root.path().join("gone"), &[])?, PurgeReport::default());
    Ok(())
}

#[cfg(unix)]
#[test]
fn purge_refuses_a_symlinked_data_directory_and_keeps_the_target() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let project = root.path().join("project");
    let outside = root.path().join("outside");
    for path in [&base, &project, &outside] {
        fs::create_dir(path)?;
    }
    fs::write(outside.join("keep.txt"), b"keep")?;
    let project = fs::canonicalize(project)?;
    write(&base, &project)?;
    let directory = MemoryReader::new(&base, &project)?.inspect("", 0, 20)?.directory;
    fs::remove_dir_all(&directory)?;
    std::os::unix::fs::symlink(fs::canonicalize(&outside)?, &directory)?;

    assert!(purge(&base, &project, &[]).is_err());
    assert!(outside.join("keep.txt").exists());
    Ok(())
}

#[test]
fn purge_reports_busy_while_the_store_lock_is_held() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let project = root.path().join("project");
    fs::create_dir(&project)?;
    let project = fs::canonicalize(project)?;
    write(&base, &project)?;
    let directory = MemoryReader::new(&base, &project)?.inspect("", 0, 20)?.directory;
    let lock = fs::File::open(directory.join(".lock"))?;
    lock.lock()?;
    assert!(matches!(purge(&base, &project, &[]), Err(Error::Busy)));
    drop(lock);
    assert_eq!(
        purge(&base, &project, &[])?,
        PurgeReport { removed_paths: 0, had_data: true, shared: false }
    );
    assert!(!directory.exists());
    Ok(())
}
