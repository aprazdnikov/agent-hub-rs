use super::*;
use hub_core::tasks::service::TaskStorage;
use hub_core::{
    domain::{Finished, SessionId, Usage},
    tasks::TaskBook,
};

fn ts() -> jiff::Timestamp {
    jiff::Timestamp::from_second(1_700_000_000).unwrap()
}
struct Store(TaskBook);
impl TaskStorage for Store {
    fn load_tasks(&mut self) -> Result<TaskBook, String> {
        Ok(self.0.clone())
    }
    fn save_tasks(&mut self, book: &TaskBook) -> Result<(), String> {
        self.0 = book.clone();
        Ok(())
    }
}
struct Fake;
impl Executor for Fake {
    fn run<'a>(
        &'a self,
        _session: &'a TopicSession,
        _prompt: Prompt,
        conversation: Conversation,
        _settings: Arc<ExecutionSettings>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let decision = conversation
                .channel
                .request(hub_core::domain::ToolRequest {
                    tool: "inspect".into(),
                    summary: "README".into(),
                })
                .await;
            if decision == hub_core::domain::Decision::Allowed {
                conversation.events.send(AgentEvent::AssistantText("Result".into())).await.unwrap();
                conversation
                    .events
                    .send(AgentEvent::Finished(Finished {
                        session: SessionId::parse("local").unwrap(),
                        usage: Usage::Qwen { tokens: None },
                        background: 0,
                    }))
                    .await
                    .unwrap();
            }
        })
    }
}
#[tokio::test]
async fn local_run_waits_for_approval_and_persists_result() {
    let project = tempfile::tempdir().unwrap();
    let mut book = TaskBook::default();
    let id = book.create(project.path().to_path_buf(), "qwen", "Read", "Inspect").unwrap();
    let service = Arc::new(TaskService::new(Box::new(Store(book))));
    let mut runtime =
        Runtime::new(service.clone(), Arc::new(Fake), Arc::new(crate::git::LocalGitRunner));
    let settings =
        Arc::new(hub_core::settings::Draft::default().execution(project.path()).unwrap());
    runtime
        .dispatch(
            project.path(),
            Request::StartChecked { id, revision: 1, next: None, rules_revision: 0 },
            settings,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if runtime.snapshot().is_some_and(|s| s.interaction.is_some()) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(service.read().unwrap().get(id).unwrap().runs[0].state, RunState::Running);
    let live = runtime.snapshot().unwrap();
    runtime.respond(live.token, live.interaction.unwrap().id, Answer::Allow).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let book = service.read().unwrap();
    let task = book.get(id).unwrap();
    assert_eq!(task.runs[0].result, "Result");
    assert_eq!(task.runs[0].state, RunState::Completed);
    assert!(
        runtime
            .dispatch(
                project.path(),
                Request::StartChecked { id, revision: 1, next: None, rules_revision: 0 },
                Arc::new(hub_core::settings::Draft::default().execution(project.path()).unwrap())
            )
            .is_err()
    );
}

#[tokio::test]
async fn stop_denies_pending_request_and_keeps_one_interrupted_attempt() {
    let project = tempfile::tempdir().unwrap();
    let mut book = TaskBook::default();
    let id = book.create(project.path().to_path_buf(), "qwen", "Read", "Inspect").unwrap();
    let service = Arc::new(TaskService::new(Box::new(Store(book))));
    let mut runtime =
        Runtime::new(service.clone(), Arc::new(Fake), Arc::new(crate::git::LocalGitRunner));
    runtime
        .dispatch(
            project.path(),
            Request::StartChecked { id, revision: 1, next: None, rules_revision: 0 },
            Arc::new(hub_core::settings::Draft::default().execution(project.path()).unwrap()),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.snapshot().is_none_or(|s| s.interaction.is_none()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let live = runtime.snapshot().unwrap();
    runtime.stop(live.token).unwrap();
    runtime.shutdown().await;
    assert!(runtime.respond(live.token, live.interaction.unwrap().id, Answer::Allow).is_err());
    let book = service.read().unwrap();
    let task = book.get(id).unwrap();
    assert_eq!(task.runs.len(), 1);
    assert_eq!(task.runs[0].state, RunState::Interrupted);
}
struct BrokenStore(TaskBook);
impl TaskStorage for BrokenStore {
    fn load_tasks(&mut self) -> Result<TaskBook, String> {
        Ok(self.0.clone())
    }
    fn save_tasks(&mut self, _: &TaskBook) -> Result<(), String> {
        Err("disk failed".into())
    }
}
struct MustNotRun;
impl Executor for MustNotRun {
    fn run<'a>(
        &'a self,
        _: &'a TopicSession,
        _: Prompt,
        _: Conversation,
        _: Arc<ExecutionSettings>,
    ) -> BoxFuture<'a, ()> {
        panic!("must not start")
    }
}
#[tokio::test]
async fn storage_failure_never_starts_executor() {
    let project = tempfile::tempdir().unwrap();
    let mut book = TaskBook::default();
    let id = book.create(project.path().to_path_buf(), "qwen", "Read", "Inspect").unwrap();
    let service = Arc::new(TaskService::new(Box::new(BrokenStore(book))));
    let mut runtime =
        Runtime::new(service.clone(), Arc::new(MustNotRun), Arc::new(crate::git::LocalGitRunner));
    runtime
        .dispatch(
            project.path(),
            Request::StartChecked { id, revision: 1, next: None, rules_revision: 0 },
            Arc::new(hub_core::settings::Draft::default().execution(project.path()).unwrap()),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(service.read().is_err());
    assert!(runtime.snapshot().unwrap().status.contains("disk failed"));
}
struct FailedGit;
impl GitRunner for FailedGit {
    fn run(
        &self,
        _: hub_telegram::git::Request,
        _: CancellationToken,
    ) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async { Err("Git unavailable".into()) })
    }
}
#[tokio::test]
async fn git_failure_and_stale_rules_never_create_an_agent_attempt() {
    let project = tempfile::tempdir().unwrap();
    let mut book = TaskBook::default();
    let id = book.create(project.path().to_path_buf(), "qwen", "Read", "Inspect").unwrap();
    let mut rules = book.rules(project.path());
    rules.git =
        Some(hub_core::tasks::git::GitPolicy { remote: "origin".into(), base: "main".into() });
    book.configure_rules(project.path().to_path_buf(), 0, rules).unwrap();
    let service = Arc::new(TaskService::new(Box::new(Store(book))));
    let mut runtime = Runtime::new(service.clone(), Arc::new(MustNotRun), Arc::new(FailedGit));
    let settings =
        Arc::new(hub_core::settings::Draft::default().execution(project.path()).unwrap());
    assert!(
        runtime
            .dispatch(
                project.path(),
                Request::PrepareGitChecked {
                    id,
                    revision: 1,
                    branch: "task/test".into(),
                    rules_revision: 0
                },
                settings.clone()
            )
            .is_err()
    );
    runtime
        .dispatch(
            project.path(),
            Request::PrepareGitChecked {
                id,
                revision: 1,
                branch: "task/test".into(),
                rules_revision: 1,
            },
            settings,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let book = service.read().unwrap();
    let task = book.get(id).unwrap();
    assert!(task.runs.is_empty());
    assert_eq!(task.git.as_ref().unwrap().state, hub_core::tasks::git::PreparationState::Failed);
}

struct PausedStore {
    book: TaskBook,
    started: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
impl TaskStorage for PausedStore {
    fn load_tasks(&mut self) -> Result<TaskBook, String> {
        Ok(self.book.clone())
    }
    fn save_tasks(&mut self, book: &TaskBook) -> Result<(), String> {
        if book
            .tasks()
            .iter()
            .any(|task| task.runs.last().is_some_and(|run| run.state == RunState::Running))
        {
            self.started.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(3)).unwrap();
        }
        self.book = book.clone();
        Ok(())
    }
}
struct CountExecutor(Arc<std::sync::atomic::AtomicUsize>);
impl Executor for CountExecutor {
    fn run<'a>(
        &'a self,
        _: &'a TopicSession,
        _: Prompt,
        _: Conversation,
        _: Arc<ExecutionSettings>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_during_durable_begin_never_polls_executor() {
    let project = tempfile::tempdir().unwrap();
    let mut book = TaskBook::default();
    let id = book.create(project.path().to_path_buf(), "qwen", "Read", "Inspect").unwrap();
    let (started, ready) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let service =
        Arc::new(TaskService::new(Box::new(PausedStore { book, started, release: blocked })));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut runtime = Runtime::new(
        service.clone(),
        Arc::new(CountExecutor(calls.clone())),
        Arc::new(crate::git::LocalGitRunner),
    );
    runtime
        .dispatch(
            project.path(),
            Request::StartChecked { id, revision: 1, next: None, rules_revision: 0 },
            Arc::new(hub_core::settings::Draft::default().execution(project.path()).unwrap()),
        )
        .unwrap();
    tokio::task::spawn_blocking(move || ready.recv_timeout(Duration::from_secs(2)).unwrap())
        .await
        .unwrap();
    runtime.stop(runtime.snapshot().unwrap().token).unwrap();
    release.send(()).unwrap();
    runtime.shutdown().await;
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(service.read().unwrap().get(id).unwrap().runs[0].state, RunState::Interrupted);
}

#[tokio::test]
async fn two_interfaces_share_one_request_and_only_one_answer_wins() {
    use hub_telegram::execution::Control;
    let project = tempfile::tempdir().unwrap();
    let mut book = TaskBook::default();
    let id = book.create(project.path().to_path_buf(), "qwen", "Read", "Inspect").unwrap();
    let service = Arc::new(TaskService::new(Box::new(Store(book))));
    let shared = Arc::new(SharedRuntime::new(Runtime::new(
        service.clone(),
        Arc::new(Fake),
        Arc::new(crate::git::LocalGitRunner),
    )));
    let desktop = shared.clone();
    let telegram: Arc<dyn Control> = shared.clone();
    desktop
        .dispatch(
            project.path(),
            Request::StartChecked { id, revision: 1, next: None, rules_revision: 0 },
            Arc::new(hub_core::settings::Draft::default().execution(project.path()).unwrap()),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while telegram.snapshot().is_none_or(|live| live.interaction.is_none()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let live = telegram.snapshot().unwrap();
    let request = live.interaction.unwrap().id;
    telegram.respond(live.token, request, Answer::Allow).unwrap();
    assert!(desktop.respond(live.token, request, Answer::Allow).is_err());
    tokio::time::timeout(Duration::from_secs(2), async {
        while shared.busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(service.read().unwrap().get(id).unwrap().runs.len(), 1);
}

#[tokio::test]
async fn completed_task_snapshot_survives_next_task_start() {
    let project = tempfile::tempdir().unwrap();
    let mut book = TaskBook::default();
    let first = book.create(project.path().into(), "qwen", "First", "Read").unwrap();
    let second = book.create(project.path().into(), "qwen", "Second", "Read").unwrap();
    let service = Arc::new(TaskService::new(Box::new(Store(book))));
    let shared = SharedRuntime::new(Runtime::new(
        service,
        Arc::new(Fake),
        Arc::new(crate::git::LocalGitRunner),
    ));
    let settings =
        Arc::new(hub_core::settings::Draft::default().execution(project.path()).unwrap());
    shared
        .dispatch(
            project.path(),
            Request::StartChecked { id: first, revision: 1, next: None, rules_revision: 0 },
            settings.clone(),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while shared.snapshot().is_none_or(|s| s.interaction.is_none()) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let live = shared.snapshot().unwrap();
    shared.respond(live.token, live.interaction.unwrap().id, Answer::Allow).unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while shared.busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    shared
        .dispatch(
            project.path(),
            Request::StartChecked { id: second, revision: 1, next: None, rules_revision: 0 },
            settings,
        )
        .unwrap();
    let completed = shared
        .snapshot_for(project.path(), first)
        .expect("previous completion is available to delayed transport");
    assert!(!completed.active);
    assert_eq!(completed.run, Some(1));
    assert_eq!(shared.snapshot().unwrap().id, second);
    shared.shutdown().await;
}

#[derive(Default)]
struct ScriptedWorkflow {
    memory: Option<PathBuf>,
    calls: std::sync::Mutex<Vec<(BackendKind, String)>>,
}
impl Executor for ScriptedWorkflow {
    fn memory_base(&self) -> Option<PathBuf> {
        self.memory.clone()
    }
    fn run<'a>(
        &'a self,
        session: &'a TopicSession,
        prompt: Prompt,
        conversation: Conversation,
        _settings: Arc<ExecutionSettings>,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let call = {
                let mut calls = self.calls.lock().unwrap();
                calls.push((session.backend, prompt.text().into()));
                calls.len()
            };
            if call == 2 {
                std::fs::write(session.cwd.as_path().join("ready.txt"), "ok").unwrap();
            }
            let text = match session.backend {
                BackendKind::Claude => "Reviewed: ready",
                _ => "Implementation",
            };
            conversation.events.send(AgentEvent::AssistantText(text.into())).await.unwrap();
            conversation
                .events
                .send(AgentEvent::Finished(Finished {
                    session: SessionId::parse("fake").unwrap(),
                    usage: Usage::Qwen { tokens: None },
                    background: 0,
                }))
                .await
                .unwrap();
        })
    }
}

#[tokio::test]
async fn host_workflow_repairs_once_preserves_evidence_and_requires_human_acceptance() {
    use hub_core::tasks::{
        ProjectRules, TaskStatus,
        workflow::{WorkflowChoice, WorkflowPreset},
    };
    for fixable in [true, false] {
        let project = tempfile::tempdir().unwrap();
        let file = if fixable { "ready.txt" } else { "missing.txt" };
        #[cfg(unix)]
        let command = format!("cat {file}");
        #[cfg(windows)]
        let command = format!("type {file}");
        let mut book = TaskBook::default();
        let id = book.create(project.path().into(), "qwen", "Title", "Fix").unwrap();
        book.set_memory(id, 1, Some(fixable)).unwrap();
        book.configure_rules(
            project.path().into(),
            0,
            ProjectRules {
                workflow: Some(WorkflowChoice::Custom(WorkflowPreset {
                    implementer: BackendKind::Qwen,
                    command,
                    reviewer: BackendKind::Claude,
                })),
                ..Default::default()
            },
        )
        .unwrap();
        let service = Arc::new(TaskService::new(Box::new(Store(book))));
        let memory = tempfile::tempdir().unwrap();
        let executor = Arc::new(ScriptedWorkflow {
            memory: Some(memory.path().join("memory")),
            ..Default::default()
        });
        let mut runtime =
            Runtime::new(service.clone(), executor.clone(), Arc::new(crate::git::LocalGitRunner));
        runtime
            .dispatch(
                project.path(),
                Request::StartChecked { id, revision: 2, next: None, rules_revision: 1 },
                {
                    let mut settings =
                        hub_core::settings::Draft::default().execution(project.path()).unwrap();
                    settings.memory.enabled = !fixable;
                    Arc::new(settings)
                },
            )
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while runtime.busy() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let book = service.read().unwrap();
        let task = book.get(id).unwrap();
        assert_eq!(task.status, TaskStatus::NeedsReview);
        let run = task.runs.first().unwrap();
        assert_eq!(
            run.workflow_report.checks.iter().map(|c| c.passed).collect::<Vec<_>>(),
            [false, fixable]
        );
        assert_eq!(run.result, "Implementation");
        let calls = executor.calls.lock().unwrap();
        let reader =
            hub_memory::MemoryReader::new(&memory.path().join("memory"), project.path()).unwrap();
        let page = reader.inspect("", 0, 20).unwrap();
        assert_eq!(page.key_count, usize::from(fixable));
        if fixable {
            let entry = reader.get(&format!("hub-task-{id}-run-1"), None).unwrap();
            assert_eq!(entry.author, "agent-hub");
            assert_eq!(entry.sources, [format!("/task attempt {id} 1")]);
            assert!(reader.recall().unwrap().contains("1/2"));
        }
        assert_eq!(calls.len(), if fixable { 3 } else { 2 });
        assert!(calls[1].1.contains(file));
        if fixable {
            assert_eq!(calls[2].0, BackendKind::Claude);
            assert_eq!(run.workflow_report.review, "Reviewed: ready");
            assert_eq!(run.state, RunState::Completed);
        } else {
            assert!(run.workflow_report.review.is_empty());
            assert_eq!(run.state, RunState::Failed);
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_check_does_not_request_a_repair_or_review() {
    use hub_core::tasks::{
        ProjectRules,
        workflow::{WorkflowChoice, WorkflowPreset},
    };
    let project = tempfile::tempdir().unwrap();
    let mut book = TaskBook::default();
    let id = book.create(project.path().into(), "qwen", "Stop", "Check cancellation").unwrap();
    book.configure_rules(
        project.path().into(),
        0,
        ProjectRules {
            workflow: Some(WorkflowChoice::Custom(WorkflowPreset {
                implementer: BackendKind::Qwen,
                command: "touch check-started && exec sleep 60".into(),
                reviewer: BackendKind::Claude,
            })),
            ..Default::default()
        },
    )
    .unwrap();
    let service = Arc::new(TaskService::new(Box::new(Store(book))));
    let executor = Arc::new(ScriptedWorkflow::default());
    let mut runtime =
        Runtime::new(service.clone(), executor.clone(), Arc::new(crate::git::LocalGitRunner));
    runtime
        .dispatch(
            project.path(),
            Request::StartChecked { id, revision: 1, next: None, rules_revision: 1 },
            Arc::new(hub_core::settings::Draft::default().execution(project.path()).unwrap()),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !project.path().join("check-started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    runtime.stop(runtime.snapshot().unwrap().token).unwrap();
    runtime.shutdown().await;
    let book = service.read().unwrap();
    let run = &book.get(id).unwrap().runs[0];
    assert_eq!(run.state, RunState::Interrupted);
    assert_eq!(run.workflow_report.checks.len(), 1);
    assert!(!run.workflow_report.checks[0].passed);
    assert_eq!(executor.calls.lock().unwrap().len(), 1);
}

/// Runs one attempt against a scripted agent and reports the agents it actually called.
async fn inherit_roles_run(
    workflow: Option<hub_core::tasks::workflow::WorkflowChoice>,
    backend: &str,
    roles: hub_core::settings::AgentRoles,
) -> (hub_core::tasks::Run, Vec<BackendKind>) {
    use hub_core::tasks::{ProjectRules, service::TaskService};
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("ready.txt"), "ok").unwrap();
    let mut book = TaskBook::default();
    let id = book.create(project.path().into(), backend, "Roles", "Do the work").unwrap();
    book.configure_rules(
        project.path().into(),
        0,
        ProjectRules {
            workflow,
            enabled: true,
            text: "Правила проекта".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let service = Arc::new(TaskService::new(Box::new(Store(book))));
    let executor = Arc::new(ScriptedWorkflow::default());
    let mut runtime =
        Runtime::new(service.clone(), executor.clone(), Arc::new(crate::git::LocalGitRunner));
    let mut settings = hub_core::settings::Draft::default().execution(project.path()).unwrap();
    settings.roles = roles;
    runtime
        .dispatch(
            project.path(),
            Request::StartChecked { id, revision: 1, next: None, rules_revision: 1 },
            Arc::new(settings),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while runtime.busy() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    runtime.shutdown().await;
    let run = service
        .read()
        .unwrap()
        .get(id)
        .unwrap()
        .runs
        .first()
        .cloned()
        .expect("the attempt is stored");
    let called = executor.calls.lock().unwrap().iter().map(|(kind, _)| *kind).collect();
    (run, called)
}

#[tokio::test]
async fn inherit_roles_run_the_shared_reviewer_and_filter() {
    let inherited = |command: &str| {
        Some(hub_core::tasks::workflow::WorkflowChoice::Inherit { command: command.into() })
    };
    #[cfg(unix)]
    let command = "cat ready.txt";
    #[cfg(windows)]
    let command = "type ready.txt";
    // The project inherits both agents, so the review runs with the shared reviewer.
    let roles = hub_core::settings::AgentRoles {
        implementer: BackendKind::Qwen,
        reviewer: BackendKind::Hermes,
    };
    let (run, called) = inherit_roles_run(inherited(command), "qwen", roles).await;
    assert_eq!(called, [BackendKind::Qwen, BackendKind::Hermes]);
    assert_eq!(run.workflow_report.checks.len(), 1);
    assert!(run.workflow_report.checks.first().is_some_and(|check| check.passed));
    assert!(!run.workflow_report.review.is_empty());
    assert_eq!(run.state, RunState::Completed);
    // The parent filter runs on the resolved preset: another shared implementer
    // means this session is not the executor, so no check is scheduled.
    let other = hub_core::settings::AgentRoles {
        implementer: BackendKind::Opencode,
        reviewer: BackendKind::Claude,
    };
    let (run, called) = inherit_roles_run(inherited(command), "qwen", other).await;
    assert_eq!(called, [BackendKind::Qwen]);
    assert!(run.workflow_report.checks.is_empty());
    assert!(run.workflow_report.review.is_empty());
    assert_eq!(run.state, RunState::Completed);
}

#[tokio::test]
async fn inherit_roles_leave_a_project_without_workflow_off() {
    let roles = hub_core::settings::AgentRoles {
        implementer: BackendKind::Qwen,
        reviewer: BackendKind::Hermes,
    };
    let (run, called) = inherit_roles_run(None, "qwen", roles).await;
    assert_eq!(called, [BackendKind::Qwen]);
    assert!(run.workflow_report.checks.is_empty());
    assert!(run.workflow_report.review.is_empty());
}

#[tokio::test]
async fn inherit_roles_do_not_override_a_custom_preset_filter() {
    use hub_core::tasks::workflow::{WorkflowChoice, WorkflowPreset};
    #[cfg(unix)]
    let command = "cat ready.txt";
    #[cfg(windows)]
    let command = "type ready.txt";
    // A project's own preset still applies to its own implementer and reviewer.
    let roles = hub_core::settings::AgentRoles {
        implementer: BackendKind::Hermes,
        reviewer: BackendKind::Opencode,
    };
    let own = WorkflowChoice::Custom(WorkflowPreset {
        implementer: BackendKind::Codex,
        command: command.into(),
        reviewer: BackendKind::Claude,
    });
    let (run, called) = inherit_roles_run(Some(own), "codex", roles).await;
    assert_eq!(called, [BackendKind::Codex, BackendKind::Claude]);
    assert_eq!(run.workflow_report.checks.len(), 1);
    // The same preset does nothing for a session of another agent.
    let other = WorkflowChoice::Custom(WorkflowPreset {
        implementer: BackendKind::Qwen,
        command: command.into(),
        reviewer: BackendKind::Claude,
    });
    let (run, called) = inherit_roles_run(Some(other), "codex", roles).await;
    assert_eq!(called, [BackendKind::Codex]);
    assert!(run.workflow_report.checks.is_empty());
}

#[tokio::test]
async fn queue_runs_two_isolated_jobs_and_routes_approvals_to_the_correct_attempt() {
    use std::process::Command;
    let project = tempfile::tempdir().unwrap();
    let cwd = project.path();
    let git = |args: &[&str]| {
        let result = Command::new("git").args(args).current_dir(cwd).output().unwrap();
        assert!(result.status.success(), "{:?}", result.stderr);
        String::from_utf8(result.stdout).unwrap()
    };
    git(&["init", "-b", "main"]);
    git(&[
        "-c",
        "user.email=test@example.invalid",
        "-c",
        "user.name=test",
        "commit",
        "--allow-empty",
        "-m",
        "base",
    ]);
    let mut book = TaskBook::default();
    let first = book.create(cwd.into(), "qwen", "one", "Inspect").unwrap();
    let second = book.create(cwd.into(), "qwen", "two", "Inspect").unwrap();
    book.enqueue(first, 1, None, true, None).unwrap();
    book.enqueue(second, 1, None, true, None).unwrap();
    let service = Arc::new(TaskService::new(Box::new(Store(book))));
    let runtime = SharedRuntime::new(Runtime::new(
        service.clone(),
        Arc::new(Fake),
        Arc::new(crate::git::LocalGitRunner),
    ));
    let settings = Arc::new(hub_core::settings::Draft::default().execution(cwd).unwrap());
    for id in [first, second] {
        let next = service.pending(false).unwrap();
        assert_eq!(next.id, id);
        runtime
            .dispatch(cwd, Request::RunQueued { id, revision: next.revision }, settings.clone())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            while runtime.snapshot_for(cwd, id).is_none_or(|live| live.interaction.is_none()) {
                if let Some(live) = runtime.snapshot_for(cwd, id) {
                    assert!(live.active, "{}", live.status);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    assert_eq!(git(&["branch", "--show-current"]).trim(), "main");
    assert!(git(&["status", "--porcelain"]).is_empty());
    assert!(!runtime.capacity(true));
    let one = runtime.snapshot_for(cwd, first).unwrap();
    let two = runtime.snapshot_for(cwd, second).unwrap();
    assert!(
        runtime.respond(one.token, two.interaction.as_ref().unwrap().id, Answer::Allow).is_err()
    );
    runtime.respond(one.token, one.interaction.unwrap().id, Answer::Allow).unwrap();
    runtime.respond(two.token, two.interaction.unwrap().id, Answer::Deny).unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while runtime.busy() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let saved = service.read().unwrap();
    assert_eq!(saved.get(first).unwrap().runs[0].state, RunState::Completed);
    assert_eq!(saved.get(second).unwrap().runs[0].state, RunState::Failed);
    assert_ne!(
        saved.get(first).unwrap().execution_dir(),
        saved.get(second).unwrap().execution_dir()
    );
}

mod live_codex;

#[test]
fn result_file_survives_project_removal_and_reopening_task_store() {
    use hub_core::domain::OutgoingFile;
    let project = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let root = data.path().join("files");
    let topics = data.path().join("topics.json");
    let mut store = crate::topics::FileTopics::new(topics.clone());
    let mut book = TaskBook::default();
    let id = book.create(project.path().into(), "codex", "Отчёт", "Сохранить").unwrap();
    store.save_tasks(&book).unwrap();
    let service = Arc::new(TaskService::new(Box::new(store)));
    let run = service.access(Access::Internal, |book, _| book.begin(id, 1, None, ts())).unwrap();
    let target = files::FileTarget {
        service: service.clone(),
        task: id,
        run,
        cwd: AbsolutePath::new(project.path().into()).unwrap(),
    };
    let cancel = CancellationToken::new();
    std::fs::write(project.path().join("report.txt"), "report").unwrap();
    let file = OutgoingFile { path: "report.txt".into(), caption: "Итог".into() };
    target.deliver(&root, &file, &cancel).unwrap();
    cancel.cancel();
    assert!(target.deliver(&root, &file, &cancel).is_err());
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    drop(project);
    let stored = crate::topics::FileTopics::new(topics).load_tasks().unwrap();
    let files = &stored.get(id).unwrap().runs[0].files;
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].caption, "Итог");
    assert_eq!(std::fs::read(root.join(&files[0].key).join(&files[0].name)).unwrap(), b"report");
}
