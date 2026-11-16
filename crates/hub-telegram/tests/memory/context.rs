use hub_memory::{EntryKind, Store, WriteRequest};
use hub_telegram::memory::{
    Request, View,
    lifecycle::{Action, Panel},
    parse, read,
};
use std::fs;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const FORM: &str = "context\nЗаписи: pilot@1\nЦель: Продолжить проверку\nОграничения: Без публикации\nСостояние: Памятка готова\nСледующий шаг: Проверить ссылки\nИсточник: user: поручение\nБюджет: 16384";
#[test]
fn context_form_and_issue_are_bound_to_project_and_current_snapshot() -> Result {
    let root = tempfile::tempdir()?;
    let base = root.path().join("memory");
    let a = root.path().join("a");
    let b = root.path().join("b");
    fs::create_dir(&a)?;
    fs::create_dir(&b)?;
    let store = Store::open(&base, &a, "test")?;
    let entry = |expected_revision| WriteRequest {
        key: "pilot".into(),
        expected_revision,
        kind: EntryKind::Progress,
        title: "Pilot".into(),
        body: "Далее — проверка".into(),
        sources: vec!["test".into()],
    };
    store.write(entry(0))?;
    let request = parse(FORM)?;
    let View::Admin(panel) = read(&base, &a, &request)? else {
        return Err("expected admin".into());
    };
    let Panel::Context(packet) = *panel else {
        return Err("expected context preview".into());
    };
    let issue = Request::Admin(Box::new(Action::ValidateContext(packet.clone())));
    assert!(
        matches!(read(&base,&a,&issue)?,View::Admin(panel) if matches!(*panel,Panel::ContextReady(_)))
    );
    assert!(read(&base, &b, &issue).is_err());
    store.write(entry(1))?;
    assert!(read(&base, &a, &issue).is_err());
    assert!(packet.text().contains("Без публикации"));
    Ok(())
}
#[test]
fn malformed_context_form_is_rejected_instead_of_dropping_fields() {
    for form in [
        FORM.replace("Цель:", "Неизвестно:"),
        format!("{FORM}\nЦель: другая"),
        FORM.replace("pilot@1", "pilot"),
        FORM.replace("16384", "много"),
    ] {
        assert!(parse(&form).is_err());
    }
}
