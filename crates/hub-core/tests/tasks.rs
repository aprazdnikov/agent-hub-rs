use hub_core::tasks::{RunState, TaskBook};
use std::path::PathBuf;

#[test]
fn explicit_runs_are_revision_checked_and_recovered_without_replay() {
    let mut book = TaskBook::default();
    let a = book
        .create(std::env::temp_dir().join("project"), "qwen", "Task", "Inspect sources")
        .unwrap();
    let b = book
        .create(std::env::temp_dir().join("project"), "codex", "Other", "Inspect tests")
        .unwrap();
    let run = book.begin(a, 1, None).unwrap();
    assert!(book.begin(a, 1, None).is_err());
    assert!(book.begin(b, 1, None).is_err());
    let mut recovered = TaskBook::decode(&book.encode().unwrap()).unwrap();
    assert!(recovered.recover());
    let task = recovered.get(a).unwrap();
    assert_eq!(task.runs.last().unwrap().state, RunState::Interrupted);
    let revision = task.revision;
    assert!(recovered.begin(a, revision, None).is_err());
    let second =
        recovered.begin(a, revision, Some("Verify prior effects, then inspect tests")).unwrap();
    assert_ne!(run, second);
    assert!(recovered.finish(a, run, RunState::Completed).is_err());
    recovered.record(a, second, "Verified").unwrap();
    recovered.finish(a, second, RunState::Completed).unwrap();
    assert_eq!(recovered.get(a).unwrap().runs.last().unwrap().result, "Verified");
}

#[test]
fn invalid_or_future_state_is_not_treated_as_empty() {
    assert!(TaskBook::decode(b"{\"version\":999}").is_err());
    assert!(TaskBook::decode(b"garbage").is_err());
    let mut book = TaskBook::default();
    assert!(book.create(PathBuf::from("relative"), "qwen", "Title", "Do").is_err());
    assert!(book.create(std::env::temp_dir(), "unknown", "Title", "Do").is_err());
    assert!(book.create(std::env::temp_dir(), "qwen", "Title", " ").is_err());
}

#[test]
fn task_and_run_limits_refuse_without_truncating_or_losing_history() {
    let mut book = TaskBook::default();
    let id = book.create(std::env::temp_dir(), "qwen", "Task", "Inspect").unwrap();
    for attempt in 0..32 {
        let revision = book.get(id).unwrap().revision;
        let run =
            book.begin(id, revision, if attempt == 0 { None } else { Some("Next step") }).unwrap();
        let oversized = "x".repeat(64 * 1024 + 1);
        assert!(book.record(id, run, &oversized).is_err());
        book.record(id, run, "Verified").unwrap();
        book.finish(id, run, RunState::Completed).unwrap();
    }
    let prior = book.encode().unwrap();
    assert!(book.begin(id, book.get(id).unwrap().revision, Some("Another")).is_err());
    assert_eq!(book.encode().unwrap(), prior);
    for _ in 1..100 {
        book.create(std::env::temp_dir(), "qwen", "Other", "Inspect").unwrap();
    }
    assert!(book.create(std::env::temp_dir(), "qwen", "Overflow", "Inspect").is_err());
    assert_eq!(book.tasks().len(), 100);
}

#[test]
fn full_valid_legacy_book_can_publish_recovery() {
    let mut raw = serde_json::json!({"version":1,"next_id":6,"tasks":(1..=5).map(|id|serde_json::json!({
        "id":id,"revision":64,"project":std::env::temp_dir(),"backend":"qwen","title":"Task","prompt":"Inspect",
        "runs":(1..=32).map(|run|serde_json::json!({"id":run,"step":"Inspect","state":if id==5&&run==32{"running"}else{"completed"},"result":""})).collect::<Vec<_>>()
    })).collect::<Vec<_>>()});
    let mut remaining =
        hub_core::tasks::MAX_FILE_BYTES - serde_json::to_vec_pretty(&raw).unwrap().len();
    for task in raw.get_mut("tasks").unwrap().as_array_mut().unwrap() {
        for run in task.get_mut("runs").unwrap().as_array_mut().unwrap() {
            let length = remaining.min(65536);
            *run.get_mut("result").unwrap() = serde_json::Value::String("x".repeat(length));
            remaining -= length;
        }
    }
    assert_eq!(remaining, 0);
    let bytes = serde_json::to_vec_pretty(&raw).unwrap();
    assert_eq!(bytes.len(), hub_core::tasks::MAX_FILE_BYTES);
    let mut book = TaskBook::decode(&bytes).unwrap();
    assert!(book.recover());
    assert!(book.encode().is_ok(), "valid full history must remain recoverable");
}
