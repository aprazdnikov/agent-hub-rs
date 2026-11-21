use hub_memory::{
    AdminAction, CleanupPolicy, EntryKind, MemoryReader, MemoryStatus, StatusFilter, Store,
    WriteRequest,
};
use std::{collections::BTreeMap, fs};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn request(n: u32) -> WriteRequest {
    WriteRequest {
        key: "key".into(),
        expected_revision: n,
        kind: EntryKind::Progress,
        title: "title".into(),
        body: format!("body {n}"),
        sources: vec!["source".into()],
    }
}
fn setup() -> Result<(tempfile::TempDir, Store, MemoryReader)> {
    let root = tempfile::tempdir()?;
    let project = root.path().join("repo");
    fs::create_dir(&project)?;
    let base = root.path().join("mem");
    let store = Store::open(&base, &project, "test")?;
    let reader = MemoryReader::new(&base, &project)?;
    Ok((root, store, reader))
}
fn apply(store: &Store, action: AdminAction) -> Result {
    store.apply(store.preview(action)?)?;
    Ok(())
}
#[test]
fn automatic_33rd_keeps_2_through_33_and_retry_uses_numbers() -> Result {
    let (_d, s, r) = setup()?;
    apply(&s, AdminAction::SetPolicy(CleanupPolicy::Automatic))?;
    for n in 0..33 {
        s.write(request(n))?;
    }
    assert_eq!(r.details("key", None)?.revisions, (2..=33).collect::<Vec<_>>());
    assert_eq!(s.write(request(32))?.revision, 33);
    assert!(s.write(request(0)).is_err());
    assert_eq!(s.get("key", None)?.revision, 33);
    Ok(())
}
#[test]
fn archive_and_stale_are_hidden_and_archive_blocks_write() -> Result {
    let (_d, s, r) = setup()?;
    s.write(request(0))?;
    for status in [MemoryStatus::Stale, MemoryStatus::Archived] {
        apply(&s, AdminAction::SetStatus { key: "key".into(), status })?;
        assert!(s.search("", 20)?.is_empty());
        assert_eq!(s.search_filtered("", 20, StatusFilter::All)?.len(), 1);
        assert_eq!(r.details("key", None)?.entry.revision, 1);
    }
    assert!(s.write(request(1)).is_err());
    apply(&s, AdminAction::SetStatus { key: "key".into(), status: MemoryStatus::Active })?;
    s.write(request(1))?;
    Ok(())
}
#[test]
fn stale_preview_and_deleted_key_cannot_change_data() -> Result {
    let (_d, s, _r) = setup()?;
    s.write(request(0))?;
    let preview = s.preview(AdminAction::Delete { keys: vec!["key".into()] })?;
    s.write(request(1))?;
    assert!(s.apply(preview).is_err());
    assert_eq!(s.get("key", None)?.revision, 2);
    apply(&s, AdminAction::Delete { keys: vec!["key".into()] })?;
    assert!(s.write(request(0)).is_err());
    assert!(s.write(request(2)).is_err());
    assert!(s.get("key", None).is_err());
    Ok(())
}
#[test]
fn pruning_preserves_latest_and_enabling_auto_does_not_prune() -> Result {
    let (_d, s, r) = setup()?;
    for n in 0..4 {
        s.write(request(n))?;
    }
    let map = |n| BTreeMap::from([("key".into(), vec![n])]);
    assert!(s.preview(AdminAction::Prune { revisions: map(4) }).is_err());
    apply(&s, AdminAction::Prune { revisions: map(2) })?;
    assert_eq!(r.details("key", None)?.revisions, vec![1, 3, 4]);
    apply(&s, AdminAction::SetPolicy(CleanupPolicy::Automatic))?;
    assert_eq!(r.details("key", None)?.revisions, vec![1, 3, 4]);
    s.write(request(4))?;
    Ok(())
}

#[test]
fn pruned_history_error_lists_available_versions() -> Result {
    let (_root, store, reader) = setup()?;
    for revision in 0..3 {
        store.write(request(revision))?;
    }
    apply(&store, AdminAction::Prune { revisions: BTreeMap::from([("key".into(), vec![2])]) })?;
    let error =
        reader.details("key", Some(2)).err().ok_or("expected missing revision")?.to_string();
    assert!(error.contains("[1, 3]"));
    assert!(error.contains("pruned"));
    Ok(())
}
