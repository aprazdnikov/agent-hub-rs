use hub_memory::{
    AdminAction, ContextBrief, ContextRequest, ContextSelection, EntryKind, MemoryReader,
    MemoryStatus, Store, WriteRequest,
};
use std::{collections::BTreeMap, fs};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn brief() -> ContextBrief {
    ContextBrief {
        goal: "Продолжить MEM-01".into(),
        constraints: "Без публикации".into(),
        state: "Памятка создана".into(),
        next_step: "Проверить источники".into(),
        sources: vec!["user: текущее поручение".into()],
    }
}
fn request() -> ContextRequest {
    ContextRequest {
        brief: brief(),
        selections: vec![ContextSelection { key: "pilot".into(), revision: 1 }],
        max_bytes: 16384,
    }
}
fn entry(expected_revision: u32) -> WriteRequest {
    WriteRequest {
        key: "pilot".into(),
        expected_revision,
        kind: EntryKind::Progress,
        title: "MEM-01".into(),
        body: "Ограничение: не менять язык памятки.\nДалее: проверить источники.".into(),
        sources: vec!["docs/missing.md".into()],
    }
}
#[test]
fn complete_context_has_exact_budget_boundary_and_does_not_read_sources() -> Result {
    let root = tempfile::tempdir()?;
    let project = root.path().join("repo");
    fs::create_dir(&project)?;
    let base = root.path().join("memory");
    Store::open(&base, &project, "test")?.write(entry(0))?;
    let reader = MemoryReader::new(&base, &project)?;
    let mut input = request();
    let packet = reader.context(&input)?;
    assert!(packet.text().contains("Без публикации"));
    assert!(packet.text().contains("не менять язык памятки"));
    assert!(packet.text().contains("docs/missing.md"));
    assert!(packet.text().contains("не проверены"));
    assert!(!project.join("docs").exists());
    assert!(packet.text().len() > packet.text().chars().count());
    input.max_bytes = packet.text().len();
    assert_eq!(reader.context(&input)?.text(), packet.text());
    input.max_bytes -= 1;
    let error = reader.context(&input).err().ok_or("must exceed budget")?;
    assert!(matches!(error, hub_memory::Error::ContextBudget { .. }));
    Ok(())
}
#[test]
fn stale_and_historical_records_are_marked_and_missing_revisions_fail_whole_package() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let store = Store::open(&base, root.path(), "test")?;
    store.write(entry(0))?;
    store.write(entry(1))?;
    store.apply(
        store
            .preview(AdminAction::SetStatus { key: "pilot".into(), status: MemoryStatus::Stale })?,
    )?;
    let reader = MemoryReader::new(&base, root.path())?;
    let packet = reader.context(&request())?;
    assert!(packet.warnings().iter().any(|text| text.contains("устаревш")));
    assert!(packet.warnings().iter().any(|text| text.contains("r1") && text.contains("r2")));
    store.apply(store.preview(AdminAction::Prune {
        revisions: BTreeMap::from([("pilot".into(), vec![1])]),
    })?)?;
    assert!(reader.context(&request()).is_err());
    Ok(())
}
#[test]
fn issuing_context_rechecks_project_snapshot_and_binding() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let a = root.path().join("a");
    let b = root.path().join("b");
    fs::create_dir(&a)?;
    fs::create_dir(&b)?;
    let store = Store::open(&base, &a, "test")?;
    store.write(entry(0))?;
    let reader = MemoryReader::new(&base, &a)?;
    let packet = reader.context(&request())?;
    reader.validate_context(&packet)?;
    assert!(MemoryReader::new(&base, &b)?.validate_context(&packet).is_err());
    store.write(entry(1))?;
    assert!(reader.validate_context(&packet).is_err());
    let packet = reader.context(&request())?;
    hub_memory::BindingPreview::prepare(&base, &a, &b, hub_memory::BindingMode::Move)?.apply()?;
    assert!(reader.validate_context(&packet).is_err());
    assert!(MemoryReader::new(&base, &b)?.validate_context(&packet).is_err());
    Ok(())
}
#[test]
fn invalid_or_duplicate_selection_does_not_create_memory() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let reader = MemoryReader::new(&base, root.path())?;
    let mut input = request();
    input.selections.push(input.selections.first().ok_or("selection")?.clone());
    assert!(reader.context(&input).is_err());
    input = request();
    input.brief.constraints.clear();
    assert!(reader.context(&input).is_err());
    input = request();
    input.max_bytes = 65537;
    assert!(reader.context(&input).is_err());
    assert!(!base.exists());
    Ok(())
}
