use super::*;
use hub_core::tasks::service::TaskStorage;
use hub_core::{
    domain::{Finished, SessionId, Usage},
    tasks::TaskBook,
};
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
