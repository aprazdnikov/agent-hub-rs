use hub_memory::{AdminAction, CleanupPolicy, EntryKind, MemoryReader, Store, WriteRequest};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn req() -> WriteRequest {
    WriteRequest {
        key: "new".into(),
        expected_revision: 0,
        kind: EntryKind::Progress,
        title: "title".into(),
        body: "b".repeat(16384),
        sources: vec!["s".repeat(2048); 8],
    }
}
fn fixture() -> Result<(tempfile::TempDir, Store, MemoryReader, PathBuf)> {
    let root = tempfile::tempdir()?;
    let project = root.path().join("repo");
    fs::create_dir(&project)?;
    let base = root.path().join("memory");
    let store = Store::open(&base, &project, "test")?;
    store.write(req())?;
    store.apply(store.preview(AdminAction::SetPolicy(CleanupPolicy::Automatic))?)?;
    let reader = MemoryReader::new(&base, &project)?;
    let path = reader.inspect("", 0, 20)?.directory.join("memory.json");
    Ok((root, store, reader, path))
}
fn saturated(path: &std::path::Path, history: bool) -> Result {
    let mut value: Value = serde_json::from_slice(&fs::read(path)?)?;
    let entry = value.pointer("/entries/new/0").ok_or("missing entry")?.clone();
    *value.get_mut("entries").ok_or("entries")? = json!({});
    if history {
        let mut first = entry.clone();
        *first.get_mut("key").ok_or("key")? = json!("history");
        *first.get_mut("created_at").ok_or("timestamp")? = json!(0);
        let mut last = first.clone();
        *last.get_mut("revision").ok_or("revision")? = json!(2);
        value
            .get_mut("entries")
            .and_then(Value::as_object_mut)
            .ok_or("entries")?
            .insert("history".into(), json!([first, last]));
    }
    for index in 0..500 {
        let key = format!("key-{index:03}");
        let mut added = entry.clone();
        *added.get_mut("key").ok_or("key")? = json!(key);
        value
            .get_mut("entries")
            .and_then(Value::as_object_mut)
            .ok_or("entries")?
            .insert(key.clone(), json!([added]));
        if serde_json::to_vec(&value)?.len() > 8 * 1024 * 1024 {
            value.get_mut("entries").and_then(Value::as_object_mut).ok_or("entries")?.remove(&key);
            break;
        }
    }
    fs::write(path, serde_json::to_vec(&value)?)?;
    Ok(())
}
#[test]
fn automatic_byte_pruning_keeps_all_latest_and_sources() -> Result {
    let (_root, store, reader, path) = fixture()?;
    saturated(&path, true)?;
    store.write(req())?;
    let details = reader.details("history", None)?;
    assert_eq!(details.revisions, vec![2]);
    assert_eq!(details.entry.sources, vec!["s".repeat(2048); 8]);
    assert!(fs::metadata(path)?.len() <= 8 * 1024 * 1024);
    Ok(())
}
#[test]
fn impossible_byte_fit_preserves_entire_snapshot() -> Result {
    let (_root, store, _reader, path) = fixture()?;
    saturated(&path, false)?;
    let before = fs::read(&path)?;
    assert!(store.write(req()).is_err());
    assert_eq!(fs::read(path)?, before);
    Ok(())
}
#[test]
fn revision_overflow_and_tombstone_limit_preserve_snapshot() -> Result {
    let (_root, store, _reader, path) = fixture()?;
    let mut value: Value = serde_json::from_slice(&fs::read(&path)?)?;
    value["entries"]["new"][0]["revision"] = json!(u32::MAX);
    fs::write(&path, serde_json::to_vec(&value)?)?;
    let before = fs::read(&path)?;
    let mut update = req();
    update.expected_revision = u32::MAX;
    assert!(store.write(update).is_err());
    assert_eq!(fs::read(&path)?, before);
    value["tombstones"] = (0..4096)
        .map(|i| (format!("deleted-{i}"), json!({"revision":1,"provenance":null})))
        .collect::<serde_json::Map<_, _>>()
        .into();
    fs::write(&path, serde_json::to_vec(&value)?)?;
    let before = fs::read(&path)?;
    assert!(store.preview(AdminAction::Delete { keys: vec!["new".into()] }).is_err());
    assert_eq!(fs::read(path)?, before);
    Ok(())
}
#[test]
fn failed_snapshot_persist_leaves_verified_backup_and_legacy_data() -> Result {
    let root = tempfile::tempdir()?;
    let project = root.path().join("repo");
    fs::create_dir(&project)?;
    let base = root.path().join("memory");
    let store = Store::open(&base, &project, "test")?;
    let dir = MemoryReader::new(&base, &project)?.inspect("", 0, 20)?.directory;
    let original = serde_json::to_vec(
        &json!({"schema_version":1,"project":fs::canonicalize(&project)?,"entries":{}}),
    )?;
    fs::write(dir.join("memory.json"), &original)?;
    fs::create_dir(dir.join(".memory.tmp"))?;
    assert!(store.write(req()).is_err());
    assert_eq!(fs::read(dir.join("memory.json"))?, original);
    assert_eq!(fs::read(dir.join("memory.v1.backup.json"))?, original);
    fs::remove_dir(dir.join(".memory.tmp"))?;
    store.write(req())?;
    Ok(())
}
