use hub_memory::{EntryKind, Store, WriteRequest};
use hub_telegram::memory::{Request, View, read};

#[test]
fn missing_revision_explains_available_history_without_losing_latest() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    let base = root.path().join("memory");
    std::fs::create_dir(&project).unwrap();
    let store = Store::open(&base, &project, "test").unwrap();
    for expected_revision in 0..4 {
        store
            .write(WriteRequest {
                key: "pilot".into(),
                expected_revision,
                kind: EntryKind::Progress,
                title: "Pilot".into(),
                body: "body".into(),
                sources: vec!["test".into()],
            })
            .unwrap();
    }
    let error = read(&base, &project, &Request::Get { key: "pilot".into(), revision: Some(5) })
        .unwrap_err();
    assert_eq!(
        error,
        "У записи «pilot» нет ревизии 5. Доступны ревизии 1, 2, 3, 4. Чтобы открыть последнюю, укажите 0 в GUI или не указывайте ревизию в /memory get."
    );
    assert!(
        matches!(read(&base, &project, &Request::Get { key: "pilot".into(), revision: None }), Ok(View::Entry(entry)) if entry.revision == 4)
    );
    let error = read(&base, &project, &Request::Get { key: "absent".into(), revision: Some(1) })
        .unwrap_err();
    assert_eq!(
        error,
        "Запись «absent» не найдена в памяти выбранного проекта. Проверьте ключ и каталог проекта."
    );
}

#[test]
fn lifecycle_auto_command_only_previews() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let request = hub_telegram::memory::parse("policy auto")?;
    let result = read(&base, root.path(), &request)?;
    assert!(matches!(result, View::Admin(_)));
    assert_eq!(
        hub_memory::MemoryReader::new(&base, root.path())?
            .overview("", 0, 20, hub_memory::StatusFilter::All)?
            .policy,
        hub_memory::CleanupPolicy::Manual
    );
    Ok(())
}
