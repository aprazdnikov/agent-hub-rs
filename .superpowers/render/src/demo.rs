//! Fictional demo data for the two screenshots. No real user data is read.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::anyhow;
use hub_app::desktop_tasks::{Attention, TaskRow};
use hub_app::local_execution::{Interaction, Kind, Live};
use hub_app::supervisor::{BotStatus, Snapshot};
use hub_core::domain::{BackendKind, Question, QuestionOption, Selection};
use hub_core::options::OptionsDraft;
use hub_core::projects::ProjectDraft;
use hub_core::settings::Draft;
use hub_core::tasks::workflow::{CheckResult, WorkflowReport};
use hub_core::tasks::{ProjectRules, ResultFile, Run, RunState, Task, TaskStatus};
use hub_telegram::hub::tasks::Snapshot as TaskSnapshot;
use jiff::Timestamp;
use uuid::Uuid;

pub struct Demo {
    pub snapshot: Snapshot,
    tasks: BTreeMap<PathBuf, Vec<Task>>,
}

impl Demo {
    /// Answer the GUI's task reads the way the real core would.
    pub fn task_snapshot(&self, project: &Path) -> TaskSnapshot {
        TaskSnapshot {
            tasks: self.tasks.get(project).cloned().unwrap_or_default(),
            rules: ProjectRules::default(),
            git_busy: false,
        }
    }
}

fn ts(raw: &str) -> anyhow::Result<Timestamp> {
    Ok(raw.parse::<Timestamp>()?)
}

const REVIEW_SUMMARY: &str = "Проверил на ширине 375 и 1280 пикселей. В обоих случаях можно сразу печатать следующий запрос. Обе проверки прошли.";

fn index_rows(atlas: &Path, vitrina: &Path) -> Vec<TaskRow> {
    vec![
        TaskRow {
            id: 1,
            project: atlas.to_path_buf(),
            title: "Отчёт по очереди".to_owned(),
            prompt: Arc::from("Отчёт по очереди"),
            backend: "claude".to_owned(),
            status: TaskStatus::Planned,
            archived: false,
            reason: None,
            summary: String::new(),
            files: 0,
            checks: None,
        },
        TaskRow {
            id: 2,
            project: atlas.to_path_buf(),
            title: "Инструкция для команды".to_owned(),
            prompt: Arc::from("Инструкция для команды"),
            backend: "claude".to_owned(),
            status: TaskStatus::Accepted,
            archived: false,
            reason: None,
            summary: String::new(),
            files: 0,
            checks: None,
        },
        TaskRow {
            id: 3,
            project: vitrina.to_path_buf(),
            title: "Поиск по каталогу".to_owned(),
            prompt: Arc::from("Поиск по каталогу"),
            backend: "codex".to_owned(),
            status: TaskStatus::NeedsReview,
            archived: false,
            reason: Some(Attention::Review),
            summary: REVIEW_SUMMARY.to_owned(),
            files: 2,
            checks: Some((2, 2)),
        },
        TaskRow {
            id: 4,
            project: vitrina.to_path_buf(),
            title: "Перевод формы заказа".to_owned(),
            prompt: Arc::from("Перевод формы заказа"),
            backend: "codex".to_owned(),
            status: TaskStatus::Planned,
            archived: false,
            reason: None,
            summary: String::new(),
            files: 0,
            checks: None,
        },
    ]
}

/// The live Atlas run paused on a clarifying question (inbox question card).
fn atlas_question(atlas: &Path) -> anyhow::Result<Live> {
    let question = Question::new(
        "Какие задачи включить в отчёт?".to_owned(),
        "Отчёт".to_owned(),
        vec![
            QuestionOption {
                label: "Все".to_owned(),
                description: "Все задачи проекта за неделю".to_owned(),
            },
            QuestionOption {
                label: "В очереди".to_owned(),
                description: "Только задачи, стоящие в очереди".to_owned(),
            },
        ],
        Selection::Single,
    )
    .ok_or_else(|| anyhow!("demo question text must not be empty"))?;
    Ok(Live {
        token: Uuid::from_u128(0x1001),
        project: atlas.to_path_buf(),
        id: 1,
        run: Some(1),
        active: true,
        status: "Агент выполняет задачу".to_owned(),
        interaction: Some(Interaction {
            id: Uuid::from_u128(0x2001),
            kind: Kind::Questions(vec![question]),
        }),
    })
}

fn run(
    id: u32,
    step: &str,
    result: &str,
    start: &str,
    finish: &str,
    files: Vec<ResultFile>,
    checks: Vec<CheckResult>,
) -> anyhow::Result<Run> {
    Ok(Run {
        id,
        step: step.to_owned(),
        state: RunState::Completed,
        result: result.to_owned(),
        diff: String::new(),
        files,
        diagnostic: String::new(),
        backend: Some("codex".to_owned()),
        rules: None,
        workflow_report: WorkflowReport { checks, review: String::new() },
        started_at: Some(ts(start)?),
        finished_at: Some(ts(finish)?),
    })
}

fn search_runs() -> anyhow::Result<Vec<Run>> {
    Ok(vec![
        run(
            1,
            "Верни фокус в поле поиска после сброса фильтра. Проверь сценарий с клавиатуры.",
            "После сброса фильтра курсор возвращается в поле поиска. Добавил тест, который вводит новый запрос с клавиатуры.",
            "2026-11-23T10:02:00Z",
            "2026-11-23T10:04:00Z",
            Vec::new(),
            Vec::new(),
        )?,
        run(
            2,
            "Проверь тот же сценарий с клавиатуры в узком и широком окне: на ширине 375 и 1280 пикселей.",
            REVIEW_SUMMARY,
            "2026-11-23T10:12:00Z",
            "2026-11-23T10:15:00Z",
            vec![
                ResultFile {
                    key: "a1b2c3d4e5f60718293a4b5c6d7e8f90".to_owned(),
                    name: "search-focus-375.png".to_owned(),
                    caption: String::new(),
                    bytes: 48_213,
                },
                ResultFile {
                    key: "b2c3d4e5f60718293a4b5c6d7e8f90a1".to_owned(),
                    name: "search-focus-1280.png".to_owned(),
                    caption: String::new(),
                    bytes: 51_406,
                },
            ],
            vec![
                CheckResult { passed: true, output: "375 px: фокус в поле поиска".to_owned() },
                CheckResult { passed: true, output: "1280 px: фокус в поле поиска".to_owned() },
            ],
        )?,
    ])
}

fn task(
    id: u64,
    project: &Path,
    title: &str,
    prompt: &str,
    backend: &str,
    status: TaskStatus,
    runs: Vec<Run>,
) -> Task {
    Task {
        id,
        revision: 1,
        project: project.to_path_buf(),
        backend: backend.to_owned(),
        title: title.to_owned(),
        prompt: prompt.to_owned(),
        runs,
        status,
        archived: false,
        git: None,
        queue: None,
        isolated: false,
        memory: None,
    }
}

fn task_books(atlas: &Path, vitrina: &Path) -> anyhow::Result<BTreeMap<PathBuf, Vec<Task>>> {
    let mut books: BTreeMap<PathBuf, Vec<Task>> = BTreeMap::new();
    books.insert(
        atlas.to_path_buf(),
        vec![
            task(1, atlas, "Отчёт по очереди", "Собери отчёт по задачам очереди за неделю.", "claude", TaskStatus::Planned, Vec::new()),
            task(2, atlas, "Инструкция для команды", "Опиши порядок работы с очередью задач.", "claude", TaskStatus::Accepted, Vec::new()),
        ],
    );
    books.insert(
        vitrina.to_path_buf(),
        vec![
            task(3, vitrina, "Поиск по каталогу", "Верни фокус в поле поиска после сброса фильтра.", "codex", TaskStatus::NeedsReview, search_runs()?),
            task(4, vitrina, "Перевод формы заказа", "Переведи форму заказа на английский.", "codex", TaskStatus::Planned, Vec::new()),
        ],
    );
    Ok(books)
}

fn project(id: &str, name: &str, cwd: &str, backend: &str) -> ProjectDraft {
    ProjectDraft {
        options: OptionsDraft::default(),
        id: id.to_owned(),
        name: name.to_owned(),
        cwd: cwd.to_owned(),
        backend: backend.to_owned(),
        actions: Vec::new(),
        checks: Vec::new(),
    }
}

fn settings_draft(root: &Path) -> Draft {
    let Draft {
        memory,
        speech,
        progress_mode,
        actions,
        projects: _projects,
        token,
        chat,
        users,
        workspace_root: _workspace_root,
        snapshots_dir,
        default_backend: _default_backend,
        cli,
        model,
        permission_mode,
        budget,
        codex,
        qwen,
        hermes,
        opencode,
        approval_timeout,
        background_timeout,
        updates,
    } = Draft::default();
    Draft {
        memory,
        speech,
        progress_mode,
        actions,
        token,
        chat,
        users,
        snapshots_dir,
        cli,
        model,
        permission_mode,
        budget,
        codex,
        qwen,
        hermes,
        opencode,
        approval_timeout,
        background_timeout,
        updates,
        workspace_root: root.display().to_string(),
        projects: vec![
            project("atlas", "Atlas", "Atlas", "claude"),
            project("vitrina", "Витрина", "Vitrina", "codex"),
        ],
        default_backend: BackendKind::Codex,
    }
}

/// Create the demo project directories and build the snapshot plus task books.
pub fn build(root: &Path) -> anyhow::Result<Demo> {
    fs::create_dir_all(root.join("Atlas"))?;
    fs::create_dir_all(root.join("Vitrina"))?;
    let atlas = fs::canonicalize(root.join("Atlas"))?;
    let vitrina = fs::canonicalize(root.join("Vitrina"))?;

    let Snapshot {
        local_only: _local_only,
        task_index: _task_index,
        bot: _bot,
        local: _local,
        local_jobs: _local_jobs,
        chat_access,
        outbox,
        diagnostics,
        save_status,
        save_revision,
        agents,
        topics,
        saved: _saved,
        notice,
    } = Snapshot::default();
    let snapshot = Snapshot {
        local_only: true,
        task_index: Ok(index_rows(&atlas, &vitrina)),
        bot: BotStatus::Stopped,
        local: None,
        local_jobs: vec![atlas_question(&atlas)?],
        chat_access,
        outbox,
        diagnostics,
        save_status,
        save_revision,
        agents,
        topics,
        saved: Some(settings_draft(root)),
        notice,
    };

    Ok(Demo { snapshot, tasks: task_books(&atlas, &vitrina)? })
}
