//! Opt in with `AGENT_HUB_LIVE_CODEX=1 cargo test -p hub-app --lib live_codex -- --ignored`.
use super::*;
use hub_core::{
    settings::{Approval, Draft, Sandbox},
    tasks::{
        ProjectRules, TaskStatus,
        workflow::{WorkflowChoice, WorkflowPreset},
    },
};

fn git(cwd: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.test",
            "-c",
            "commit.gpgSign=false",
        ])
        .args(args)
        .current_dir(cwd)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

async fn workflow(
    cwd: &Path,
    prompt: &str,
    command: &str,
    sandbox: Sandbox,
) -> hub_core::tasks::Run {
    let mut book = TaskBook::default();
    let id = book.create(cwd.into(), "codex", "Smoke test", prompt).unwrap();
    book.configure_rules(
        cwd.into(),
        0,
        ProjectRules {
            workflow: Some(WorkflowChoice::Custom(WorkflowPreset {
                implementer: BackendKind::Codex,
                command: command.into(),
                reviewer: BackendKind::Codex,
            })),
            ..Default::default()
        },
    )
    .unwrap();
    let data = tempfile::tempdir().unwrap();
    let topics = data.path().join("topics.json");
    let mut store = crate::topics::FileTopics::new(topics.clone());
    store.save_tasks(&book).unwrap();
    let service = Arc::new(TaskService::new(Box::new(store)));
    let executor = CliExecutor { memory_program: std::env::current_exe().unwrap() };
    let mut runtime =
        Runtime::new(service, Arc::new(executor), Arc::new(crate::git::LocalGitRunner));
    let mut settings = Draft::default().execution(cwd).unwrap();
    settings.codex.sandbox = sandbox;
    settings.codex.approval = Approval::Never;
    settings.memory.enabled = false;
    runtime
        .dispatch(
            cwd,
            Request::StartChecked { id, revision: 1, next: None, rules_revision: 1 },
            Arc::new(settings),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(150), async {
        while runtime.busy() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("Codex workflow timed out");
    drop(runtime);
    let book = crate::topics::FileTopics::new(topics).load_tasks().unwrap();
    let task = book.get(id).unwrap();
    assert_eq!(task.runs.len(), 1);
    assert_eq!(task.status, TaskStatus::NeedsReview);
    let run = &task.runs[0];
    assert_eq!(run.state, RunState::Completed, "{run:?}");
    assert!(!run.workflow_report.review.is_empty());
    run.clone()
}

#[tokio::test]
#[ignore = "requires a logged-in Codex CLI and spends tokens"]
async fn live_codex_workflow() {
    if std::env::var("AGENT_HUB_LIVE_CODEX").as_deref() != Ok("1") {
        return;
    }
    let project = tempfile::tempdir().unwrap();
    git(project.path(), &["init", "-q"]);
    let run = workflow(
        project.path(),
        "Ответь одним словом HUBREADY. Задача текстовая, файлы и инструменты не нужны.",
        "echo HUBCHECK",
        Sandbox::ReadOnly,
    )
    .await;
    assert!(run.result.contains("HUBREADY"), "{}", run.result);
    let checks = &run.workflow_report.checks;
    assert_eq!(checks.len(), 1);
    assert!(checks[0].passed && checks[0].output.contains("HUBCHECK"));
}

#[tokio::test]
#[ignore = "requires a logged-in Codex CLI and spends tokens"]
async fn live_codex_repairs_file_after_failed_check() {
    if std::env::var("AGENT_HUB_LIVE_CODEX").as_deref() != Ok("1") {
        return;
    }
    let project = tempfile::tempdir().unwrap();
    let cwd = project.path();
    git(cwd, &["init", "-q"]);
    let file = cwd.join("state.txt");
    std::fs::write(&file, "INITIAL\n").unwrap();
    git(cwd, &["add", "state.txt"]);
    git(cwd, &["commit", "-qm", "fixture"]);
    let run = workflow(cwd,
        "В первой попытке запиши в state.txt одну строку DRAFT. Если хаб пришлёт ошибку проверки, замени строку на READY и передай исправленный файл пользователю инструментом send_file. Проверку запускает хаб.",
        "git grep -n . -- state.txt && git grep -F READY -- state.txt",
        Sandbox::WorkspaceWrite,
    ).await;
    let checks = &run.workflow_report.checks;
    assert_eq!(checks.len(), 2, "{checks:?}");
    assert!(!checks[0].passed && checks[0].output.contains("state.txt:1:DRAFT"));
    assert!(checks[1].passed && checks[1].output.contains("state.txt:1:READY"));
    assert_eq!(std::fs::read_to_string(file).unwrap().trim_end(), "READY");
    assert!(run.diff.contains("-INITIAL") && run.diff.contains("+READY"), "{}", run.diff);
    let attachment = run.files.iter().find(|f| f.name == "state.txt").expect("delivered file");
    let root = crate::dirs::AppDirs::locate().unwrap().task_files();
    assert_eq!(
        std::fs::read_to_string(root.join(&attachment.key).join(&attachment.name))
            .unwrap()
            .trim_end(),
        "READY"
    );
    for file in run.files {
        std::fs::remove_file(root.join(&file.key).join(&file.name)).unwrap();
        std::fs::remove_dir(root.join(file.key)).unwrap();
    }
}
