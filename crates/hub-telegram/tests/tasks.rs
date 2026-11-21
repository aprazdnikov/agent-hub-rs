use hub_telegram::hub::tasks::{Request, parse};
#[test]
fn task_commands_require_exact_run_revision_and_explicit_resume_step() {
    assert!(
        matches!(parse("create Проверка | Прочитать README"),Ok(Request::Create{title,prompt}) if title=="Проверка"&&prompt=="Прочитать README")
    );
    assert!(matches!(parse("run 1 2"), Ok(Request::Start { id: 1, revision: 2, next: None })));
    assert!(
        matches!(parse("resume 1 3 Проверить эффекты"),Ok(Request::Start{next:Some(step),..}) if step=="Проверить эффекты")
    );
    for invalid in [
        "run 1",
        "run 1 2 extra",
        "run x 2",
        "resume 1 2",
        "get 1 extra",
        "list extra",
        "create no separator",
    ] {
        assert!(parse(invalid).is_err(), "{invalid}");
    }
}
