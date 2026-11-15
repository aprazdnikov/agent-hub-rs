use hub_memory::{
    AdminAction, CleanupPolicy, EntryKind, ImportChoice, MemoryExport, MemoryReader, Store,
    WriteRequest,
};
use std::{collections::BTreeMap, fs};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn req(body: &str, n: u32) -> WriteRequest {
    WriteRequest {
        key: "key".into(),
        expected_revision: n,
        kind: EntryKind::Progress,
        title: "title".into(),
        body: body.into(),
        sources: vec!["source".into()],
    }
}
fn setup(root: &std::path::Path, name: &str) -> Result<(Store, MemoryReader)> {
    let p = root.join(name);
    fs::create_dir(&p)?;
    let b = root.join("memory");
    Ok((Store::open(&b, &p, "test")?, MemoryReader::new(&b, &p)?))
}
#[test]
fn roundtrip_and_repeated_import_preserve_history_and_policy() -> Result {
    let root = tempfile::tempdir()?;
    let (a, ar) = setup(root.path(), "a")?;
    let (b, br) = setup(root.path(), "b")?;
    a.write(req("one", 0))?;
    a.write(req("two", 1))?;
    a.apply(a.preview(AdminAction::SetPolicy(CleanupPolicy::Automatic))?)?;
    for _ in 0..2 {
        let preview = b.preview_import(ar.export()?, BTreeMap::new())?;
        b.apply_import(preview)?;
    }
    assert_eq!(br.details("key", None)?.revisions, vec![1, 2]);
    assert_eq!(b.get("key", None)?, a.get("key", None)?);
    assert_eq!(
        br.overview("", 0, 20, hub_memory::StatusFilter::All)?.policy,
        CleanupPolicy::Manual
    );
    Ok(())
}
#[test]
fn renamed_conflict_is_idempotent_after_local_write() -> Result {
    let root = tempfile::tempdir()?;
    let (a, ar) = setup(root.path(), "a")?;
    let (b, br) = setup(root.path(), "b")?;
    a.write(req("incoming", 0))?;
    b.write(req("local", 0))?;
    let choices = BTreeMap::from([("key".into(), ImportChoice::CopyAs("copy".into()))]);
    b.apply_import(b.preview_import(ar.export()?, choices.clone())?)?;
    let mut update = req("evolved", 1);
    update.key = "copy".into();
    b.write(update)?;
    b.apply_import(b.preview_import(ar.export()?, choices)?)?;
    assert_eq!(br.details("copy", None)?.revisions, vec![1, 2]);
    assert_eq!(b.get("key", None)?.body, "local");
    Ok(())
}
#[test]
fn reset_requires_current_verified_export_and_fences_old_store() -> Result {
    let root = tempfile::tempdir()?;
    let (s, r) = setup(root.path(), "a")?;
    s.write(req("one", 0))?;
    let export = r.export()?;
    let verified = export.write_new(&root.path().join("export.json"))?;
    let token = r.overview("", 0, 20, hub_memory::StatusFilter::All)?.token;
    s.reset(token, vec!["key".into()], verified)?;
    assert!(s.write(req("two", 1)).is_err());
    assert_eq!(r.get("key", None)?.revision, 1);
    assert!(export.write_new(&root.path().join("export.json")).is_err());
    Ok(())
}
#[test]
fn oversized_input_is_bounded() {
    let input = std::io::repeat(b' ').take(16 * 1024 * 1024 + 1);
    assert!(MemoryExport::decode(input).is_err());
}
use std::io::Read;

#[test]
fn disjoint_history_and_divergent_metadata_conflict() -> Result {
    let root = tempfile::tempdir()?;
    let (source, reader) = setup(root.path(), "source")?;
    let (target, _) = setup(root.path(), "target")?;
    source.write(req("first", 0))?;
    target.apply_import(target.preview_import(reader.export()?, BTreeMap::new())?)?;
    source.write(req("second", 1))?;
    source
        .apply(source.preview(AdminAction::Prune {
            revisions: BTreeMap::from([("key".into(), vec![1])]),
        })?)?;
    let preview = target.preview_import(reader.export()?, BTreeMap::new())?;
    assert_eq!(preview.items.first().ok_or("missing item")?.kind, hub_memory::ImportKind::Conflict);
    assert_eq!(target.get("key", None)?.revision, 1);
    Ok(())
}
#[test]
fn copy_target_cannot_shadow_another_incoming_key() -> Result {
    let root = tempfile::tempdir()?;
    let (source, reader) = setup(root.path(), "source")?;
    let (target, _) = setup(root.path(), "target")?;
    let mut first = req("first", 0);
    first.key = "a".into();
    source.write(first)?;
    let mut second = req("second", 0);
    second.key = "b".into();
    source.write(second)?;
    let choices = BTreeMap::from([("a".into(), ImportChoice::CopyAs("b".into()))]);
    assert!(target.preview_import(reader.export()?, choices).is_err());
    assert!(target.search("", 20)?.is_empty());
    Ok(())
}
#[test]
fn changed_export_and_stale_import_do_not_modify_store() -> Result {
    let root = tempfile::tempdir()?;
    let (source, reader) = setup(root.path(), "source")?;
    source.write(req("one", 0))?;
    let receipt = reader.export()?.write_new(&root.path().join("copy.json"))?;
    let token = reader.overview("", 0, 20, hub_memory::StatusFilter::All)?.token;
    fs::write(root.path().join("copy.json"), b"changed")?;
    assert!(source.reset(token, vec![], receipt).is_err());
    let preview = source.preview_import(reader.export()?, BTreeMap::new())?;
    source.write(req("two", 1))?;
    assert!(source.apply_import(preview).is_err());
    assert_eq!(source.get("key", None)?.revision, 2);
    Ok(())
}
#[test]
fn incoming_tombstone_does_not_delete_live_key() -> Result {
    let root = tempfile::tempdir()?;
    let (source, reader) = setup(root.path(), "source")?;
    let (target, _) = setup(root.path(), "target")?;
    source.write(req("same", 0))?;
    target.apply_import(target.preview_import(reader.export()?, BTreeMap::new())?)?;
    source.apply(source.preview(AdminAction::Delete { keys: vec!["key".into()] })?)?;
    let preview = target.preview_import(reader.export()?, BTreeMap::new())?;
    assert_eq!(preview.items.first().ok_or("missing item")?.kind, hub_memory::ImportKind::Conflict);
    target.apply_import(preview)?;
    assert_eq!(target.get("key", None)?.body, "same");
    Ok(())
}

#[test]
fn reset_rejects_export_with_matching_token_but_different_contents() -> Result {
    let root = tempfile::tempdir()?;
    let (store, reader) = setup(root.path(), "source")?;
    store.write(req("original", 0))?;
    let receipt = reader.export()?.write_temporary()?;
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(receipt.path())?)?;
    *value.pointer_mut("/snapshot/entries/key/0/body").ok_or("missing body")? =
        serde_json::json!("different");
    let forged = MemoryExport::decode(serde_json::to_vec(&value)?.as_slice())?.write_temporary()?;
    let token = reader.overview("", 0, 20, hub_memory::StatusFilter::All)?.token;
    assert!(store.reset(token, vec![], forged).is_err());
    assert_eq!(store.get("key", None)?.body, "original");
    Ok(())
}

#[test]
fn two_copy_choices_cannot_target_same_key() -> Result {
    let root = tempfile::tempdir()?;
    let (source, reader) = setup(root.path(), "source")?;
    let (target, target_reader) = setup(root.path(), "target")?;
    source.write(req("incoming", 0))?;
    let mut other = req("other", 0);
    other.key = "other".into();
    source.write(other)?;
    target.write(req("local", 0))?;
    let path = target_reader.inspect("", 0, 20)?.directory.join("memory.json");
    let before = fs::read(&path)?;
    let choices = BTreeMap::from([
        ("key".into(), ImportChoice::CopyAs("same".into())),
        ("other".into(), ImportChoice::CopyAs("same".into())),
    ]);
    assert!(target.preview_import(reader.export()?, choices).is_err());
    assert_eq!(fs::read(path)?, before);
    Ok(())
}
#[test]
fn conflict_preview_contains_both_histories_sources_and_deleted_state() -> Result {
    let root = tempfile::tempdir()?;
    let (source, reader) = setup(root.path(), "source")?;
    let (target, _) = setup(root.path(), "target")?;
    source.write(req("incoming", 0))?;
    source.write(req("incoming new", 1))?;
    target.write(req("local", 0))?;
    let preview = target.preview_import(reader.export()?, BTreeMap::new())?;
    let conflict = preview.conflict("key")?;
    assert_eq!(conflict.local.history.first().ok_or("local history")?.body, "local");
    assert_eq!(conflict.incoming.history.len(), 2);
    assert_eq!(conflict.incoming.history.last().ok_or("incoming history")?.sources, vec!["source"]);
    target.apply(target.preview(AdminAction::Delete { keys: vec!["key".into()] })?)?;
    let preview = target.preview_import(reader.export()?, BTreeMap::new())?;
    assert_eq!(preview.conflict("key")?.local.deleted_revision, Some(1));
    Ok(())
}
