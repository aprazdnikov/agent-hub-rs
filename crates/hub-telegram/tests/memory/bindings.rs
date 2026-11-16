use hub_memory::{BindingMode, EntryKind, MemoryReader, Store, WriteRequest};
use hub_telegram::memory::lifecycle::{Action, Panel, run};
use std::fs;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
#[test]
fn binding_confirmation_cannot_be_applied_in_another_project() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let source = root.path().join("source");
    let target = root.path().join("target");
    let other = root.path().join("other");
    for path in [&source, &target, &other] {
        fs::create_dir(path)?;
    }
    Store::open(&base, &source, "test")?.write(WriteRequest {
        key: "key".into(),
        expected_revision: 0,
        kind: EntryKind::Context,
        title: "title".into(),
        body: "body".into(),
        sources: vec!["test".into()],
    })?;
    let Panel::Binding(preview) =
        run(&base, &target, &Action::Bind { source: source.clone(), mode: BindingMode::Share })?
    else {
        return Err("expected preview".into());
    };
    assert!(!base.join(".projects").exists());
    assert!(run(&base, &other, &Action::ApplyBinding(preview.clone())).is_err());
    assert!(run(&base, &target, &Action::ApplyBinding(preview)).is_ok());
    assert_eq!(MemoryReader::new(&base, &target)?.get("key", None)?.revision, 1);
    assert!(MemoryReader::new(&base, &other)?.inspect("", 0, 20)?.entries.is_empty());
    Ok(())
}

#[test]
fn binding_input_errors_explain_how_to_correct_selection() -> Result {
    use hub_telegram::memory::{Request, read};
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let source = root.path().join("source");
    let target = root.path().join("target");
    let file = root.path().join("file");
    fs::create_dir(&source)?;
    fs::create_dir(&target)?;
    fs::write(&file, "not a directory")?;
    let missing = root.path().join("missing");
    for (from, to, expected) in [
        (&source, &source, "Источник и целевой каталог совпадают"),
        (&missing, &target, "Исходный каталог не найден"),
        (&source, &missing, "Целевой каталог не найден"),
        (&source, &target, "В исходном каталоге пока нет сохранённой памяти"),
        (&file, &target, "В качестве источника выбран файл"),
        (&source, &file, "В качестве целевого каталога выбран файл"),
    ] {
        let request = Request::Admin(Box::new(Action::Bind {
            source: from.clone(),
            mode: BindingMode::Share,
        }));
        let error = read(&base, to, &request).err().ok_or("expected input error")?;
        assert!(error.starts_with(expected), "{error}");
        let args = vec![
            "share".into(),
            "--from".into(),
            from.as_os_str().to_owned(),
            "--to".into(),
            to.as_os_str().to_owned(),
            "--store-dir".into(),
            base.as_os_str().to_owned(),
        ];
        assert_eq!(hub_memory::run_project_cli(args).err(), Some(error));
        assert!(!base.exists(), "invalid input must not create memory");
    }
    Ok(())
}
