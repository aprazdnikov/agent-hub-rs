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
