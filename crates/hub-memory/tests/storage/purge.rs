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

    let report = purge(&base, &new)?;
    assert_eq!(report, PurgeReport { removed_paths: 2, had_data: true });
    assert!(!directory.exists());
    assert_eq!(registry_paths(&base)?, serde_json::json!({}));
    // The project directory itself is never part of the cascade.
    assert!(new.is_dir());
    assert_eq!(MemoryReader::new(&base, &other)?.get("key", None)?.revision, 1);

    let again = purge(&base, &new)?;
    assert_eq!(again, PurgeReport { removed_paths: 0, had_data: false });
    Ok(())
}

#[test]
fn purge_without_data_directory_cleans_only_the_registry() -> Result {
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
    fs::remove_dir_all(&directory)?;

    let report = purge(&base, &target)?;
    assert_eq!(report, PurgeReport { removed_paths: 1, had_data: false });
    assert!(!directory.exists());
    assert_eq!(registry_paths(&base)?, serde_json::json!({}));
    Ok(())
}

#[test]
fn purge_requires_the_project_directory() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    fs::create_dir(&base)?;
    assert!(matches!(purge(&base, &root.path().join("gone")), Err(Error::InvalidInput(_))));
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

    assert!(purge(&base, &project).is_err());
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
    assert!(matches!(purge(&base, &project), Err(Error::Busy)));
    drop(lock);
    assert_eq!(purge(&base, &project)?, PurgeReport { removed_paths: 0, had_data: true });
    assert!(!directory.exists());
    Ok(())
}
