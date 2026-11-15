use hub_memory::{EntryKind, MemoryReader, Store, WriteRequest};
use serde_json::json;
use std::{fs, path::PathBuf};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn request(expected_revision: u32) -> WriteRequest {
    WriteRequest {
        key: "sample".into(),
        expected_revision,
        kind: EntryKind::Progress,
        title: "title".into(),
        body: format!("revision {expected_revision}"),
        sources: vec!["test".into()],
    }
}
fn fixture() -> Result<(tempfile::TempDir, PathBuf, PathBuf, PathBuf)> {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let project = root.path().join("project");
    fs::create_dir(&project)?;
    Store::open(&base, &project, "test")?;
    let directory = fs::read_dir(&base)?.next().ok_or("missing directory")??.path();
    Ok((root, base, project, directory))
}
fn legacy(project: &std::path::Path) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&json!({"schema_version":1,"project":project,"entries":{}}))?)
}
#[test]
fn read_v1_does_not_migrate() -> Result {
    let (_root, base, project, dir) = fixture()?;
    let bytes = legacy(&project)?;
    fs::write(dir.join("memory.json"), &bytes)?;
    MemoryReader::new(&base, &project)?.inspect("", 0, 20)?;
    assert_eq!(fs::read(dir.join("memory.json"))?, bytes);
    assert!(!dir.join("memory.v1.backup.json").exists());
    Ok(())
}
#[test]
fn first_mutation_preserves_exact_v1_backup() -> Result {
    let (_root, base, project, dir) = fixture()?;
    let bytes = legacy(&project)?;
    fs::write(dir.join("memory.json"), &bytes)?;
    let store = Store::open(&base, &project, "test")?;
    store.write(request(0))?;
    assert_eq!(fs::read(dir.join("memory.v1.backup.json"))?, bytes);
    let value: serde_json::Value = serde_json::from_slice(&fs::read(dir.join("memory.json"))?)?;
    assert_eq!(value["schema_version"], 2);
    store.write(request(1))?;
    assert_eq!(store.get("sample", None)?.revision, 2);
    Ok(())
}
#[test]
fn migration_retry_reuses_matching_backup() -> Result {
    let (_root, base, project, dir) = fixture()?;
    let bytes = legacy(&project)?;
    fs::write(dir.join("memory.json"), &bytes)?;
    fs::write(dir.join("memory.v1.backup.json"), &bytes)?;
    Store::open(&base, &project, "test")?.write(request(0))?;
    assert_eq!(fs::read(dir.join("memory.v1.backup.json"))?, bytes);
    Ok(())
}
#[test]
fn different_backup_blocks_migration() -> Result {
    let (_root, base, project, dir) = fixture()?;
    let bytes = legacy(&project)?;
    fs::write(dir.join("memory.json"), &bytes)?;
    fs::write(dir.join("memory.v1.backup.json"), b"different")?;
    assert!(Store::open(&base, &project, "test")?.write(request(0)).is_err());
    assert_eq!(fs::read(dir.join("memory.json"))?, bytes);
    Ok(())
}
#[test]
fn two_open_stores_survive_initial_creation_and_migration() -> Result {
    let (_root, base, project, dir) = fixture()?;
    for old in [false, true] {
        if old {
            fs::write(dir.join("memory.json"), legacy(&project)?)?;
        }
        let a = Store::open(&base, &project, "a")?;
        let b = Store::open(&base, &project, "b")?;
        a.write(request(0))?;
        b.write(request(1))?;
    }
    Ok(())
}

#[test]
fn old_v1_writer_invalidates_admin_preview() -> Result {
    let (_root, base, project, dir) = fixture()?;
    let mut value: serde_json::Value = serde_json::from_slice(&legacy(&project)?)?;
    fs::write(dir.join("memory.json"), serde_json::to_vec(&value)?)?;
    let s = Store::open(&base, &project, "test")?;
    let preview =
        s.preview(hub_memory::AdminAction::SetPolicy(hub_memory::CleanupPolicy::Automatic))?;
    value["entries"]["external"] = serde_json::json!([{"key":"external","revision":1,"kind":"progress","title":"old writer","body":"changed","sources":["test"],"author":"old","created_at":1}]);
    let changed = serde_json::to_vec(&value)?;
    fs::write(dir.join("memory.json"), &changed)?;
    assert!(s.apply(preview).is_err());
    assert_eq!(fs::read(dir.join("memory.json"))?, changed);
    Ok(())
}

#[test]
fn incomplete_staging_backup_is_recovered_before_migration() -> Result {
    let (_root, base, project, dir) = fixture()?;
    let original = legacy(&project)?;
    fs::write(dir.join("memory.json"), &original)?;
    fs::write(dir.join(".memory.v1.backup.tmp"), b"interrupted")?;
    Store::open(&base, &project, "test")?.write(request(0))?;
    assert_eq!(fs::read(dir.join("memory.v1.backup.json"))?, original);
    assert!(!dir.join(".memory.v1.backup.tmp").exists());
    Ok(())
}
