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

#[test]
fn legacy_tasks_gain_review_state_without_losing_history() {
    use hub_core::tasks::TaskStatus;
    let mut book = TaskBook::default();
    let id = book.create(std::env::temp_dir(), "qwen", "Проверка", "Прочитать файл").unwrap();
    let run = book.begin(id, 1, None).unwrap();
    book.record(id, run, "Полный результат").unwrap();
    book.finish(id, run, RunState::Completed).unwrap();
    let mut raw = serde_json::to_value(&book).unwrap();
    for task in raw["tasks"].as_array_mut().unwrap() {
        task.as_object_mut().unwrap().remove("status");
        task.as_object_mut().unwrap().remove("archived");
    }
    let restored = TaskBook::decode(&serde_json::to_vec(&raw).unwrap()).unwrap();
    assert_eq!(restored.get(id).unwrap().status, TaskStatus::NeedsReview);
    assert_eq!(restored.get(id).unwrap().runs, book.get(id).unwrap().runs);
    assert_eq!(TaskBook::decode(&restored.encode().unwrap()).unwrap(), restored);
}

#[test]
fn acceptance_archive_and_reopen_are_explicit_revision_checked_actions() {
    use hub_core::tasks::TaskStatus;
    let mut book = TaskBook::default();
    let id = book.create(std::env::temp_dir(), "qwen", "Task", "Read").unwrap();
    assert_eq!(book.get(id).unwrap().status, TaskStatus::Planned);
    let run = book.begin(id, 1, None).unwrap();
    assert!(book.set_status(id, 2, TaskStatus::Accepted).is_err());
    assert!(book.set_archived(id, 2, true).is_err());
    book.finish(id, run, RunState::Completed).unwrap();
    assert_eq!(book.get(id).unwrap().status, TaskStatus::NeedsReview);
    assert!(book.set_status(id, 2, TaskStatus::Accepted).is_err());
    book.set_status(id, 3, TaskStatus::Accepted).unwrap();
    assert!(book.begin(id, 4, Some("Next")).is_err());
    book.set_status(id, 4, TaskStatus::Planned).unwrap();
    book.set_archived(id, 5, true).unwrap();
    assert!(book.begin(id, 6, Some("Next")).is_err());
    book.set_archived(id, 6, false).unwrap();
    book.begin(id, 7, Some("Next")).unwrap();
    assert!(book.recover());
    assert_eq!(book.get(id).unwrap().status, TaskStatus::NeedsReview);
}

#[test]
fn workflow_filters_use_russian_case_and_hide_archives_by_default() {
    use hub_core::tasks::{TaskFilter, TaskStatus};
    let mut book = TaskBook::default();
    let id = book.create(std::env::temp_dir(), "qwen", "Проверка", "Прочитать README").unwrap();
    let task = book.get(id).unwrap();
    assert!(TaskFilter::Active.matches(task, "ПРОВЕРКА"));
    assert!(TaskFilter::Status(TaskStatus::Planned).matches(task, "прочитать"));
    assert!(!TaskFilter::Status(TaskStatus::Accepted).matches(task, ""));
    book.set_archived(id, 1, true).unwrap();
    let task = book.get(id).unwrap();
    assert!(!TaskFilter::Active.matches(task, ""));
    assert!(TaskFilter::Archived.matches(task, "README"));
    assert!(TaskFilter::All.matches(task, ""));
}

#[test]
fn compact_full_legacy_file_with_100_tasks_can_recover() {
    let mut raw = serde_json::json!({"version":1,"next_id":101,"tasks":(1..=100).map(|id|serde_json::json!({
        "id":id,"revision":64,"project":std::env::temp_dir(),"backend":"qwen","title":"Task","prompt":"Inspect",
        "runs": if id <= 5 {(1..=32).map(|run|serde_json::json!({"id":run,"step":"Inspect","state":if id==5&&run==32{"running"}else{"completed"},"result":""})).collect::<Vec<_>>()} else {vec![]}
    })).collect::<Vec<_>>()});
    let mut remaining = hub_core::tasks::MAX_FILE_BYTES - serde_json::to_vec(&raw).unwrap().len();
    for task in raw["tasks"].as_array_mut().unwrap() {
        for run in task["runs"].as_array_mut().unwrap() {
            let length = remaining.min(65536);
            run["result"] = serde_json::Value::String("x".repeat(length));
            remaining -= length;
        }
    }
    assert_eq!(remaining, 0);
    let mut book = TaskBook::decode(&serde_json::to_vec(&raw).unwrap()).unwrap();
    assert!(book.recover());
    assert!(book.encode().is_ok());
}

#[test]
fn explicit_null_status_is_corruption_not_a_legacy_default() {
    let mut book = TaskBook::default();
    book.create(std::env::temp_dir(), "qwen", "Task", "Read").unwrap();
    let mut raw = serde_json::to_value(book).unwrap();
    raw["tasks"][0]["status"] = serde_json::Value::Null;
    assert!(TaskBook::decode(&serde_json::to_vec(&raw).unwrap()).is_err());
}

#[test]
fn transfer_preserves_legacy_run_authorship_and_never_starts_a_model() {
    let mut book = TaskBook::default();
    let id = book.create(std::env::temp_dir(), "qwen", "Task", "Read").unwrap();
    let run = book.begin(id, 1, None).unwrap();
    book.record(id, run, "Old answer").unwrap();
    book.finish(id, run, RunState::Completed).unwrap();
    let mut raw = serde_json::to_value(&book).unwrap();
    raw["tasks"][0]["runs"][0].as_object_mut().unwrap().remove("backend");
    let mut book = TaskBook::decode(&serde_json::to_vec(&raw).unwrap()).unwrap();
    book.assign(id, 3, "CoDeX").unwrap();
    assert_eq!(book.get(id).unwrap().backend, "codex");
    book.assign(id, 4, "hermes").unwrap();
    let task = book.get(id).unwrap();
    assert_eq!(task.backend, "hermes");
    assert_eq!(task.runs.len(), 1);
    assert_eq!(task.runs[0].backend.as_deref(), Some("qwen"));
    assert_eq!(task.runs[0].result, "Old answer");
    let run = book.begin(id, 5, Some("Verify prior effects")).unwrap();
    assert_eq!(book.get(id).unwrap().runs[1].backend.as_deref(), Some("hermes"));
    book.finish(id, run, RunState::Completed).unwrap();
    assert_eq!(TaskBook::decode(&book.encode().unwrap()).unwrap(), book);
}

#[test]
fn transfer_requires_editable_task_current_revision_and_known_agent() {
    use hub_core::tasks::TaskStatus;
    let mut book = TaskBook::default();
    let id = book.create(std::env::temp_dir(), "qwen", "Task", "Read").unwrap();
    let before = book.clone();
    assert!(book.assign(id, 0, "codex").is_err());
    assert!(book.assign(id, 1, "invalid").is_err());
    assert_eq!(book, before);
    book.begin(id, 1, None).unwrap();
    assert!(book.assign(id, 2, "codex").is_err());
    book.finish(id, 1, RunState::Completed).unwrap();
    book.set_archived(id, 3, true).unwrap();
    assert!(book.assign(id, 4, "codex").is_err());
    book.set_archived(id, 4, false).unwrap();
    for status in [TaskStatus::Accepted, TaskStatus::Deferred] {
        let revision = book.get(id).unwrap().revision;
        book.set_status(id, revision, status).unwrap();
        assert!(book.assign(id, revision + 1, "codex").is_err());
        book.set_status(id, revision + 1, TaskStatus::Planned).unwrap();
    }
}

#[test]
fn task_preview_is_read_only_and_preserves_multiline_step_and_previous_result() {
    let mut book = TaskBook::default();
    let id = book.create(std::env::temp_dir(), "qwen", "Task", "Read\n  code").unwrap();
    let before = book.clone();
    let preview = book.get(id).unwrap().preview(None).unwrap();
    assert!(preview.contains("Read\n  code"));
    assert_eq!(book, before);
    book.begin(id, 1, None).unwrap();
    book.record(id, 1, "Answer\n  exact").unwrap();
    book.finish(id, 1, RunState::Completed).unwrap();
    let task = book.get(id).unwrap();
    assert!(task.preview(None).is_err());
    assert!(task.preview(Some(" ")).is_err());
    let text = task.preview(Some("Check\n  next")).unwrap();
    assert!(text.contains("Check\n  next") && text.contains("Answer\n  exact"));
}

#[test]
fn continuation_includes_recent_turns_in_order_and_keeps_large_latest_reply_intact() {
    let mut book = TaskBook::default();
    let id = book.create(std::env::temp_dir(), "qwen", "Chat", "First").unwrap();
    for (next, result) in [(None, "One"), (Some("Second"), "Two"), (Some("Third"), "Three")] {
        let revision = book.get(id).unwrap().revision;
        let run = book.begin(id, revision, next).unwrap();
        book.record(id, run, result).unwrap();
        book.finish(id, run, RunState::Completed).unwrap();
    }
    let task = book.get(id).unwrap();
    let context = task.preview(Some("Next")).unwrap();
    assert!(context.find("qwen: One").unwrap() < context.find("Пользователь: Second").unwrap());
    assert!(context.find("qwen: Two").unwrap() < context.find("Пользователь: Third").unwrap());
    let revision = task.revision;
    let run = book.begin(id, revision, Some("Fourth")).unwrap();
    let large = "я".repeat(32768);
    book.record(id, run, &large).unwrap();
    book.finish(id, run, RunState::Completed).unwrap();
    assert!(book.get(id).unwrap().preview(Some("Next")).unwrap().contains(&large));
    let run = book.begin(id, book.get(id).unwrap().revision, Some("Fifth")).unwrap();
    book.record(id, run, "Five").unwrap();
    book.finish(id, run, RunState::Completed).unwrap();
    let context = book.get(id).unwrap().preview(Some("Next")).unwrap();
    assert!(context.contains("Более ранние сообщения") && context.contains("qwen: Five"));
    assert!(!context.contains("qwen: One") && !context.contains(&large));
}

#[test]
fn project_rules_are_revisioned_isolated_and_snapshotted_per_run() {
    let project = std::env::temp_dir();
    let mut book = TaskBook::default();
    let id = book.create(project.clone(), "qwen", "Test", "Inspect").unwrap();
    assert_eq!(book.rules(&project).revision, 0);
    book.set_rules(project.clone(), 0, true, "Сначала обнови Dev").unwrap();
    assert!(book.rules(&project).enabled);
    assert_eq!(book.rules(&project.join("other")).revision, 0);
    let before = book.encode().unwrap();
    assert!(book.set_rules(project.clone(), 0, false, "stale").is_err());
    assert_eq!(before, book.encode().unwrap());
    let preview = book.get(id).unwrap().preview_with_rules(None, &book.rules(&project)).unwrap();
    assert!(preview.contains("Сначала обнови Dev"));
    assert!(preview.contains("редакция 1"));
    let run = book.begin(id, 1, None).unwrap();
    book.set_rules(project.clone(), 1, false, "Новое правило").unwrap();
    assert_eq!(book.get(id).unwrap().runs[0].rules.as_ref().unwrap().text, "Сначала обнови Dev");
    book.finish(id, run, RunState::Completed).unwrap();
    let preview =
        book.get(id).unwrap().preview_with_rules(Some("Next"), &book.rules(&project)).unwrap();
    assert!(!preview.contains("Новое правило"));
    assert!(preview.contains("выключены"));
    let restored = TaskBook::decode(&book.encode().unwrap()).unwrap();
    assert_eq!(book, restored);
}

#[test]
fn invalid_project_rules_do_not_mutate_the_book() {
    let mut book = TaskBook::default();
    let before = book.encode().unwrap();
    for (path, enabled, text) in [
        (std::env::temp_dir(), true, " ".to_owned()),
        (std::env::temp_dir(), false, "x".repeat(16385)),
        (PathBuf::from("relative"), true, "Rule".to_owned()),
    ] {
        assert!(book.set_rules(path, 0, enabled, &text).is_err());
        assert_eq!(before, book.encode().unwrap());
    }
}

#[test]
fn git_preparation_requires_durable_success_and_recovers_without_replay() {
    use hub_core::tasks::git::{GitPolicy, PreparationState};
    let project = std::env::temp_dir();
    let mut book = TaskBook::default();
    let id = book.create(project.clone(), "qwen", "Task", "Do").unwrap();
    let mut rules = book.rules(&project);
    rules.git = Some(GitPolicy { remote: "origin".into(), base: "Dev".into() });
    book.configure_rules(project, 0, rules).unwrap();
    assert!(book.begin(id, 1, None).is_err());
    book.prepare_git(id, 1, "feat/test").unwrap();
    assert!(book.get(id).unwrap().active());
    let mut recovered = TaskBook::decode(&book.encode().unwrap()).unwrap();
    assert!(recovered.recover());
    assert_eq!(
        recovered.get(id).unwrap().git.as_ref().unwrap().state,
        PreparationState::Interrupted
    );
    assert!(!recovered.recover());
    assert!(recovered.begin(id, 3, None).is_err());
    book.finish_git(id, 2, Ok("a".repeat(40))).unwrap();
    assert_eq!(book.get(id).unwrap().revision, 3);
    assert!(book.finish_git(id, 2, Ok("b".repeat(40))).is_err());
    book.note_git(id, 3, "Выбрана другая ветка").unwrap();
    book.begin(id, 4, None).unwrap();
    assert!(book.get(id).unwrap().git.as_ref().unwrap().message.is_empty());
    assert!(book.prepare_git(id, 5, "feat/other").is_err());
    assert!(TaskBook::decode(&book.encode().unwrap()).is_ok());
}

#[test]
fn fifo_intents_survive_restart_but_a_claimed_preflight_never_replays() {
    let mut book = TaskBook::default();
    let project = std::env::temp_dir();
    let a = book.create(project.clone(), "qwen", "one", "Inspect").unwrap();
    let b = book.create(project, "qwen", "two", "Inspect").unwrap();
    book.enqueue(b, 1, None, true, None).unwrap();
    book.enqueue(a, 1, None, false, None).unwrap();
    assert!(book.enqueue(a, 1, None, false, None).is_err());
    let mut saved = TaskBook::decode(&book.encode().unwrap()).unwrap();
    assert_eq!(saved.next_queued(false).unwrap().0.id, b);
    saved.claim_queue(b, 2).unwrap();
    let mut restored = TaskBook::decode(&saved.encode().unwrap()).unwrap();
    assert!(!restored.recover());
    assert_eq!(restored.next_queued(false).unwrap().0.id, a);
    assert!(restored.get(b).unwrap().runs.is_empty());
    restored.unqueue(b, 2).unwrap();
    assert_eq!(restored.get(b).unwrap().revision, 3);
}

#[test]
fn stale_task_or_rule_revisions_cannot_start_waiting_work() {
    let mut book = TaskBook::default();
    let project = std::env::temp_dir();
    let id = book.create(project.clone(), "qwen", "one", "Inspect").unwrap();
    book.enqueue(id, 1, None, false, None).unwrap();
    book.set_rules(project, 0, true, "review first").unwrap();
    assert!(book.claim_queue(id, 2).is_err());
    book.assign(id, 2, "codex").unwrap();
    assert!(book.claim_queue(id, 2).is_err());
}

#[test]
fn full_book_with_two_isolated_runs_can_finish_both_in_order() {
    let limit = hub_core::tasks::MAX_FILE_BYTES - 2048;
    let mut raw = serde_json::json!({"version":1,"next_id":6,"tasks":(1..=5).map(|id|serde_json::json!({
        "id":id,"revision":64,"isolated":true,"project":std::env::temp_dir(),"backend":"qwen","title":"Task","prompt":"Inspect",
        "runs":(1..=32).map(|run|serde_json::json!({"id":run,"step":"Inspect","state":if id>=4 && run==32{"running"}else{"completed"},"result":""})).collect::<Vec<_>>()
    })).collect::<Vec<_>>()});
    let mut remaining = limit - serde_json::to_vec(&raw).unwrap().len();
    for task in raw["tasks"].as_array_mut().unwrap() {
        for run in task["runs"].as_array_mut().unwrap() {
            let n = remaining.min(65536);
            run["result"] = serde_json::Value::String("x".repeat(n));
            remaining -= n;
        }
    }
    let mut book = TaskBook::decode(&serde_json::to_vec(&raw).unwrap()).unwrap();
    assert_eq!(book.encode().unwrap().len(), limit);
    book.finish(4, 32, RunState::Completed).unwrap();
    // The other worker is still running; reopening must preserve its attempt.
    let mut book = TaskBook::decode(&book.encode().unwrap()).unwrap();
    assert!(book.recover());
    assert!(book.encode().is_ok());
}

#[test]
fn scoped_memory_survives_reload_and_respects_task_revision() {
    use hub_core::tasks::TaskBook;
    let mut book = TaskBook::default();
    let id = book.create(std::env::temp_dir(), "qwen", "Title", "Task").unwrap();
    assert_eq!(book.get(id).unwrap().memory, None);
    book.set_memory(id, 1, Some(false)).unwrap();
    assert!(book.set_memory(id, 1, Some(true)).is_err());
    let mut book = TaskBook::decode(&book.encode().unwrap()).unwrap();
    assert_eq!(book.get(id).unwrap().memory, Some(false));
    book.set_memory(id, 2, None).unwrap();
    book.enqueue(id, 3, None, false, None).unwrap();
    assert!(book.set_memory(id, 4, Some(true)).is_err());
    assert_eq!(book.get(id).unwrap().memory, None);
}

#[test]
fn editing_a_saved_prompt_checks_revision_and_keeps_the_first_run_immutable() {
    let mut book = TaskBook::default();
    let id = book.create(std::env::temp_dir(), "qwen", "Название", "Первое").unwrap();
    assert!(book.edit_prompt(id, 1, "  ").is_err());
    assert!(book.edit_prompt(id, 1, &"я".repeat(8193)).is_err());
    book.edit_prompt(id, 1, "Исправленное").unwrap();
    assert!(book.edit_prompt(id, 1, "Устарело").is_err());
    book.edit_prompt(id, 2, "Исправленное").unwrap();
    book.enqueue(id, 2, None, false, None).unwrap();
    assert!(book.edit_prompt(id, 3, "В очереди").is_err());
    book.unqueue(id, 3).unwrap();
    book.set_archived(id, 4, true).unwrap();
    assert!(book.edit_prompt(id, 5, "В архиве").is_err());
    book.set_archived(id, 5, false).unwrap();
    let mut book = TaskBook::decode(&book.encode().unwrap()).unwrap();
    let run = book.begin(id, 6, None).unwrap();
    assert_eq!(book.get(id).unwrap().runs[0].step, "Исправленное");
    book.finish(id, run, RunState::Completed).unwrap();
    assert!(book.edit_prompt(id, 8, "После запуска").is_err());
    assert_eq!(book.get(id).unwrap().prompt, "Исправленное");
}
