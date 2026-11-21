use std::ffi::OsString;

use hub_memory::run_cli;

#[test]
fn cli_requires_an_explicit_project() {
    assert_eq!(run_cli(Vec::new()), Err("missing --project-dir".into()));
}

#[test]
fn cli_rejects_invalid_arguments_without_creating_store() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let store = temp.path().join("must-not-exist");
    let scenarios = [
        vec![OsString::from("--project-dir"), OsString::from("relative")],
        vec![OsString::from("--project-dir"), OsString::from("/nonexistent-project-memory-test")],
        vec![
            OsString::from("--project-dir"),
            temp.path().as_os_str().to_owned(),
            OsString::from("--actor"),
        ],
        vec![
            OsString::from("--project-dir"),
            temp.path().as_os_str().to_owned(),
            OsString::from("--actor"),
            OsString::from("actor"),
            OsString::from("--actor"),
            OsString::from("again"),
        ],
        vec![
            OsString::from("--project-dir"),
            temp.path().as_os_str().to_owned(),
            OsString::from("--unknown-secret"),
        ],
    ];
    for mut args in scenarios {
        args.extend([OsString::from("--store-dir"), store.as_os_str().to_owned()]);
        let error = run_cli(args).err().ok_or("arguments must fail")?;
        assert!(!error.contains("unknown-secret"));
        assert!(!store.exists());
    }
    Ok(())
}

#[test]
fn project_cli_requires_preview_confirmation_and_preserves_isolation()
-> Result<(), Box<dyn std::error::Error>> {
    use hub_memory::{
        BindingMode, BindingPreview, EntryKind, Store, WriteRequest, run_project_cli,
    };
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    let target = temp.path().join("target");
    let base = temp.path().join("memory");
    std::fs::create_dir(&source)?;
    std::fs::create_dir(&target)?;
    let store = Store::open(&base, &source, "test")?;
    store.write(WriteRequest {
        key: "key".into(),
        expected_revision: 0,
        kind: EntryKind::Context,
        title: "title".into(),
        body: "body".into(),
        sources: vec!["test".into()],
    })?;
    let args = vec![
        OsString::from("share"),
        "--from".into(),
        source.into_os_string(),
        "--to".into(),
        target.clone().into_os_string(),
        "--store-dir".into(),
        base.clone().into_os_string(),
    ];
    let output = run_project_cli(args.clone())?;
    assert!(output.contains("--confirm"));
    assert!(!base.join(".projects").exists());
    let mut wrong = args.clone();
    wrong.extend(["--confirm".into(), "wrong".into()]);
    assert!(run_project_cli(wrong).is_err());
    assert!(!base.join(".projects").exists());
    let preview =
        BindingPreview::prepare(&base, &temp.path().join("source"), &target, BindingMode::Share)?;
    let mut confirmed = args;
    confirmed.extend(["--confirm".into(), preview.confirmation().into()]);
    assert!(run_project_cli(confirmed)?.contains("сохранена"));
    assert_eq!(Store::open(&base, &target, "qwen")?.get("key", None)?.revision, 1);
    Ok(())
}
