//! Durable task state. Starting a model is deliberately outside this module.
use crate::domain::BackendKind;
use crate::settings::AgentRoles;
pub mod git;
pub mod queue;
pub mod workflow;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Extra bounded space lets a legacy full file record terminal recovery metadata.
pub const MAX_READ_BYTES: usize = MAX_FILE_BYTES + 1024;
const MAX_PROMPT: usize = 16 * 1024;
const MAX_RESULT: usize = 64 * 1024;

/// Explicit, project-scoped user instructions. Revision zero means never configured.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRules {
    pub revision: u64,
    pub enabled: bool,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<git::GitPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<workflow::WorkflowChoice>,
}
impl ProjectRules {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(workflow) = &self.workflow {
            workflow.validate()?;
        }
        if let Some(git) = &self.git {
            git.validate()?;
        }
        if self.text.len() > MAX_PROMPT || (self.enabled && self.text.trim().is_empty()) {
            return Err("Введите правила до 16 КиБ; для включения нужен непустой текст.".into());
        }
        Ok(())
    }
    /// The rules header every description shares, plus the workflow line when there is one.
    fn described(&self, workflow: Option<&str>) -> String {
        let text = if self.enabled {
            format!("Правила проекта · редакция {} (включены):\n{}", self.revision, self.text)
        } else {
            format!("Правила проекта · редакция {}: выключены", self.revision)
        };
        let git = self
            .git
            .as_ref()
            .map_or("Подготовка Git выключена".to_owned(), |policy| {
                format!("Подготовка Git обязательна: {}/{}", policy.remote, policy.base)
            });
        workflow.map_or_else(|| format!("{text}\n{git}"), |plan| format!("{text}\n{git}\n{plan}"))
    }
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.workflow {
            Some(workflow::WorkflowChoice::Custom(plan)) => self.described(Some(&plan.describe())),
            Some(workflow::WorkflowChoice::Inherit { command }) => {
                self.described(Some(&format!("Исправить и проверить по общим ролям: {command}.")))
            }
            None => self.described(None),
        }
    }
    /// The same description for a reader: the inherited mode already resolved to its agents.
    #[must_use]
    pub fn describe_with_roles(&self, roles: &AgentRoles) -> String {
        match self.effective_workflow(roles) {
            Some(plan) => self.described(Some(&plan.describe())),
            None => self.described(None),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRuleEntry {
    project: PathBuf,
    rules: ProjectRules,
}

/// Human workflow status, independent of a model attempt's outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Planned,
    NeedsReview,
    Accepted,
    Deferred,
}
impl TaskStatus {
    pub const ALL: [Self; 4] = [Self::Planned, Self::NeedsReview, Self::Accepted, Self::Deferred];
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Planned => "Запланирована",
            Self::NeedsReview => "Нужна проверка",
            Self::Accepted => "Принята",
            Self::Deferred => "Отложена",
        }
    }
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "planned" => Ok(Self::Planned),
            "review" => Ok(Self::NeedsReview),
            "accepted" => Ok(Self::Accepted),
            "deferred" => Ok(Self::Deferred),
            _ => {
                Err("Неизвестный статус. Используйте planned, review, accepted или deferred."
                    .into())
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Completed,
    Failed,
    Interrupted,
}
impl RunState {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Running => "Выполняется",
            Self::Completed => "Запуск завершён",
            Self::Failed => "Ошибка",
            Self::Interrupted => "Прерван; проверьте результат",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultFile {
    pub key: String,
    pub name: String,
    pub caption: String,
    pub bytes: u64,
}

impl ResultFile {
    fn valid(&self) -> bool {
        self.key.len() == 32
            && self.key.bytes().all(|c| c.is_ascii_hexdigit())
            && !self.name.is_empty()
            && self.name.len() <= 255
            && self.name != "."
            && self.name != ".."
            && !self.name.contains(['/', '\\', ':'])
            && !self.name.chars().any(char::is_control)
            && self.caption.len() <= 4096
            && self.bytes <= crate::attachments::MAX_SEND_BYTES
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub id: u32,
    pub step: String,
    pub state: RunState,
    pub result: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub diff: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ResultFile>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub diagnostic: String,
    #[serde(
        default,
        deserialize_with = "explicit_backend",
        skip_serializing_if = "Option::is_none"
    )]
    pub backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules: Option<ProjectRules>,
    /// The workflow resolved from the shared roles when the attempt started. History renders
    /// the agents that actually ran; editing the global roles afterwards must not rewrite it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<workflow::WorkflowPreset>,
    #[serde(default, skip_serializing_if = "workflow::WorkflowReport::is_empty")]
    pub workflow_report: workflow::WorkflowReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<Timestamp>,
}
impl Run {
    /// The frozen rules block of the attempt: the workflow line names the agents recorded at
    /// its start. GUI history and the Telegram attempt report render through this one method.
    #[must_use]
    pub fn described_rules(&self) -> Option<String> {
        let rules = self.rules.as_ref()?;
        Some(match &self.workflow {
            Some(preset) => rules.described(Some(&preset.describe())),
            // Attempts saved before roles were frozen keep their previous wording.
            None => rules.describe(),
        })
    }
    /// The trusted check command this attempt was started with.
    #[must_use]
    pub fn check_command(&self) -> Option<&str> {
        match (&self.workflow, &self.rules) {
            (Some(preset), _) => Some(&preset.command),
            (None, Some(rules)) => rules.workflow.as_ref().map(workflow::WorkflowChoice::command),
            (None, None) => None,
        }
    }
}
fn explicit_backend<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    String::deserialize(deserializer).map(|backend| Some(backend.to_ascii_lowercase()))
}
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(from = "StoredTask")]
pub struct Task {
    pub id: u64,
    pub revision: u64,
    pub project: PathBuf,
    pub backend: String,
    pub title: String,
    pub prompt: String,
    pub runs: Vec<Run>,
    pub status: TaskStatus,
    pub archived: bool,
    pub git: Option<git::Preparation>,
    pub queue: Option<queue::Entry>,
    pub isolated: bool,
    pub memory: Option<bool>,
}
impl Serialize for Task {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        // Omit derivable legacy defaults: even a full old file remains recoverable.
        let default_status =
            if self.runs.is_empty() { TaskStatus::Planned } else { TaskStatus::NeedsReview };
        let explicit_status = self.status != default_status;
        let mut fields = serializer.serialize_struct(
            "Task",
            7 + usize::from(explicit_status)
                + usize::from(self.archived)
                + usize::from(self.git.is_some())
                + usize::from(self.queue.is_some())
                + usize::from(self.isolated)
                + usize::from(self.memory.is_some()),
        )?;
        fields.serialize_field("id", &self.id)?;
        fields.serialize_field("revision", &self.revision)?;
        fields.serialize_field("project", &self.project)?;
        fields.serialize_field("backend", &self.backend)?;
        fields.serialize_field("title", &self.title)?;
        fields.serialize_field("prompt", &self.prompt)?;
        fields.serialize_field("runs", &self.runs)?;
        if explicit_status {
            fields.serialize_field("status", &self.status)?;
        }
        if self.archived {
            fields.serialize_field("archived", &self.archived)?;
        }
        if let Some(git) = &self.git {
            fields.serialize_field("git", git)?;
        }
        if let Some(queue) = &self.queue {
            fields.serialize_field("queue", queue)?;
        }
        if let Some(memory) = self.memory {
            fields.serialize_field("memory", &memory)?;
        }
        if self.isolated {
            fields.serialize_field("isolated", &true)?;
        }
        fields.end()
    }
}
// Missing workflow fields are supported only for the previous file format.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredTask {
    id: u64,
    revision: u64,
    project: PathBuf,
    backend: String,
    title: String,
    prompt: String,
    runs: Vec<Run>,
    #[serde(default, deserialize_with = "explicit_status")]
    status: Option<TaskStatus>,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    git: Option<git::Preparation>,
    #[serde(default)]
    queue: Option<queue::Entry>,
    #[serde(default)]
    isolated: bool,
    #[serde(default)]
    memory: Option<bool>,
}
fn explicit_status<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<TaskStatus>, D::Error> {
    TaskStatus::deserialize(deserializer).map(Some)
}
impl From<StoredTask> for Task {
    fn from(raw: StoredTask) -> Self {
        let status = raw.status.unwrap_or(if raw.runs.is_empty() {
            TaskStatus::Planned
        } else {
            TaskStatus::NeedsReview
        });
        Self {
            id: raw.id,
            revision: raw.revision,
            project: raw.project,
            backend: raw.backend.to_ascii_lowercase(),
            title: raw.title,
            prompt: raw.prompt,
            runs: raw.runs,
            status,
            archived: raw.archived,
            git: raw.git,
            queue: raw.queue,
            isolated: raw.isolated,
            memory: raw.memory,
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskFilter {
    #[default]
    Active,
    All,
    Archived,
    Status(TaskStatus),
}
impl TaskFilter {
    /// Status and archive selection before applying a text query.
    #[must_use]
    pub fn includes(self, status: TaskStatus, archived: bool) -> bool {
        match self {
            Self::Active => !archived,
            Self::All => true,
            Self::Archived => archived,
            Self::Status(required) => !archived && status == required,
        }
    }
    #[must_use]
    pub fn matches(self, task: &Task, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        self.includes(task.status, task.archived)
            && (query.is_empty()
                || task.title.to_lowercase().contains(&query)
                || task.prompt.to_lowercase().contains(&query))
    }
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Active => "Без архива",
            Self::All => "Все, включая архив",
            Self::Archived => "Архив",
            Self::Status(status) => status.label(),
        }
    }
}
impl Task {
    fn next_step<'a>(&'a self, next: Option<&'a str>) -> Result<&'a str, String> {
        match (self.runs.is_empty(), next) {
            (true, None) => Ok(&self.prompt),
            (false, Some(value)) if text(value, MAX_PROMPT) => Ok(value),
            _ => Err(
                "Для продолжения проверьте прежние эффекты и укажите следующий шаг (до 16 КиБ)."
                    .into(),
            ),
        }
    }
    pub fn preview(&self, next: Option<&str>) -> Result<String, String> {
        self.preview_with_rules(next, &ProjectRules::default())
    }
    pub fn preview_with_rules(
        &self,
        next: Option<&str>,
        rules: &ProjectRules,
    ) -> Result<String, String> {
        rules.validate()?;
        let step = self.next_step(next)?;
        let history = self.conversation_context();
        let rules = rules.describe();
        let git = self.git.as_ref().map_or(String::new(), git::Preparation::describe);
        Ok(format!(
            "{rules}\n{git}\n\nЗадача #{}: {}\nНазначенный агент: {}\nИсходное поручение:\n{}\n\nШаг этой попытки:\n{}\n\nСохранённая переписка (данные прежних запусков):\n{}\n\nЭто ручной запуск. Проверь фактические прежние эффекты перед действиями. Не повторяй уже выполненные действия. Не считай состояние запуска подтверждением приёмки человеком. Соблюдай текущие правила проекта и разрешения этой сессии. Разрешения прежних агентов не переносятся.",
            self.id, self.title, self.backend, self.prompt, step, history
        ))
    }
    fn conversation_context(&self) -> String {
        // Always preserve the latest step and answer in full. Include an additional
        // 16 KiB of complete recent turns without splitting UTF-8 or quoted replies.
        let mut turns = Vec::new();
        let mut remaining = MAX_PROMPT;
        for run in self.runs.iter().rev() {
            let turn = format!(
                "Попытка {} · {}\nПользователь: {}\n{}: {}\n",
                run.id,
                run.state.label(),
                run.step,
                run.backend.as_deref().unwrap_or(&self.backend),
                run.result,
            );
            if !turns.is_empty() {
                let Some(rest) = remaining.checked_sub(turn.len()) else {
                    turns.push("Более ранние сообщения доступны в истории задачи.\n".to_owned());
                    break;
                };
                remaining = rest;
            }
            turns.push(turn);
        }
        turns.reverse();
        turns.concat()
    }
    #[must_use]
    pub fn active(&self) -> bool {
        self.runs.last().is_some_and(|run| run.state == RunState::Running)
            || self.git.as_ref().is_some_and(|git| git.state == git::PreparationState::Preparing)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBook {
    version: u32,
    next_id: u64,
    tasks: Vec<Task>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    project_rules: Vec<ProjectRuleEntry>,
}
impl Default for TaskBook {
    fn default() -> Self {
        Self { version: 1, next_id: 1, tasks: Vec::new(), project_rules: Vec::new() }
    }
}
fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max
}
impl TaskBook {
    #[must_use]
    pub fn rules(&self, project: &Path) -> ProjectRules {
        self.project_rules
            .iter()
            .find(|entry| entry.project == project)
            .map(|entry| entry.rules.clone())
            .unwrap_or_default()
    }
    pub fn set_rules(
        &mut self,
        project: PathBuf,
        revision: u64,
        enabled: bool,
        value: &str,
    ) -> Result<(), String> {
        let mut rules = self.rules(&project);
        rules.enabled = enabled;
        rules.text = value.into();
        self.configure_rules(project, revision, rules)
    }
    pub fn configure_rules(
        &mut self,
        project: PathBuf,
        revision: u64,
        mut rules: ProjectRules,
    ) -> Result<(), String> {
        if !project.is_absolute() || project.as_os_str().len() > 4096 {
            return Err("Укажите абсолютный каталог проекта до 4096 байт.".into());
        }
        let current = self.rules(&project);
        if current.revision != revision {
            return Err(
                "Правила проекта изменились. Загрузите сохранённую редакцию и повторите правку."
                    .into(),
            );
        }
        rules.revision = revision.checked_add(1).ok_or("Лимит редакций правил")?;
        rules.validate()?;
        if current.enabled == rules.enabled
            && current.text == rules.text
            && current.git == rules.git
            && current.workflow == rules.workflow
        {
            return Ok(());
        }
        if let Some(entry) = self.project_rules.iter_mut().find(|entry| entry.project == project) {
            entry.rules = rules;
        } else {
            if self.project_rules.len() >= 100 {
                return Err("Достигнут лимит правил для 100 проектов.".into());
            }
            self.project_rules.push(ProjectRuleEntry { project, rules });
        }
        Ok(())
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_READ_BYTES {
            return Err("Файл задач превышает 8 МиБ".into());
        }
        let book: Self = serde_json::from_slice(bytes)
            .map_err(|_| "Файл задач повреждён или имеет неизвестный формат")?;
        book.validate()?;
        if book.tasks.iter().any(Task::active)
            && serde_json::to_vec(&book).map_err(|e| e.to_string())?.len() > MAX_FILE_BYTES
        {
            return Err("Активная задача превышает допустимый объём".into());
        }
        Ok(book)
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        // Each live attempt needs its own space to publish a terminal state while its
        // neighbour is still running. Waiting intents may need a diagnostic before launch.
        let active = self.tasks.iter().filter(|task| task.active()).count();
        let waiting = self
            .tasks
            .iter()
            .filter(|task| task.queue.as_ref().is_some_and(|entry| entry.error.is_none()))
            .count();
        let limit = if active == 0 { MAX_READ_BYTES } else { MAX_FILE_BYTES - active * 1024 };
        let limit = limit - waiting * 3072;
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        if bytes.len() <= limit {
            return Ok(bytes);
        }
        let compact = serde_json::to_vec(self).map_err(|e| e.to_string())?;
        if compact.len() > limit {
            return Err("Файл задач заполнен. История сохранена; для завершения текущего запуска зарезервировано служебное место.".into());
        }
        Ok(compact)
    }

    fn validate(&self) -> Result<(), String> {
        let mut ids = std::collections::BTreeSet::new();
        let mut running = 0;
        let mut exclusive = false;
        let mut positions = std::collections::BTreeSet::new();
        if self.version != 1 || self.next_id == 0 || self.tasks.len() > 100 {
            return Err("Неизвестная версия или неверные лимиты файла задач".into());
        }
        let mut projects = std::collections::BTreeSet::new();
        if self.project_rules.len() > 100 {
            return Err("Достигнут лимит правил для 100 проектов.".into());
        }
        for entry in &self.project_rules {
            if !entry.project.is_absolute()
                || entry.project.as_os_str().len() > 4096
                || !projects.insert(&entry.project)
                || entry.rules.revision == 0
            {
                return Err("Некорректная запись правил проекта".into());
            }
            entry.rules.validate()?;
        }
        for task in &self.tasks {
            if let Some(q) = &task.queue
                && (!q.valid() || !positions.insert(q.sequence))
            {
                return Err("Повреждена очередь задач".into());
            }
            if task.active() && !task.isolated {
                exclusive = true;
            }
            if let Some(git) = &task.git {
                git.validate()?;
                running += usize::from(git.state == git::PreparationState::Preparing);
            }
            if task.id == 0
                || task.id >= self.next_id
                || !ids.insert(task.id)
                || task.revision == 0
                || !task.project.is_absolute()
                || task.project.as_os_str().len() > 4096
                || BackendKind::parse(&task.backend).is_none()
                || !text(&task.title, 256)
                || !text(&task.prompt, MAX_PROMPT)
                || task.runs.len() > 32
                || (task.active()
                    && (task.archived
                        || matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)))
            {
                return Err("Некорректная запись задачи".into());
            }
            for (index, run) in task.runs.iter().enumerate() {
                run.workflow_report.validate()?;
                if let Some(preset) = &run.workflow {
                    preset.validate()?;
                }
                if let Some(rules) = &run.rules {
                    rules.validate()?;
                    if rules.revision == 0 {
                        return Err("Некорректная редакция правил в истории".into());
                    }
                }
                if usize::try_from(run.id).ok() != Some(index + 1)
                    || !text(&run.step, MAX_PROMPT)
                    || run.result.len() > MAX_RESULT
                    || run.files.len() > 32
                    || run.files.iter().any(|file| !file.valid())
                    || run.diff.len() > MAX_RESULT
                    || run.diagnostic.len() > 512
                    || run.backend.as_deref().is_some_and(|backend| {
                        BackendKind::parse(backend).is_none()
                            || (run.state == RunState::Running && backend != task.backend)
                    })
                    || (run.state == RunState::Running && index + 1 != task.runs.len())
                {
                    return Err("Некорректная история запусков".into());
                }
                running += usize::from(run.state == RunState::Running);
            }
        }
        if running > 2 || (running > 1 && exclusive) {
            return Err("В файле задач более одного активного запуска".into());
        }
        Ok(())
    }
    #[must_use]
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }
    pub fn get(&self, id: u64) -> Result<&Task, String> {
        self.tasks.iter().find(|t| t.id == id).ok_or_else(|| "Задача не найдена".into())
    }
    fn get_mut(&mut self, id: u64) -> Result<&mut Task, String> {
        self.tasks.iter_mut().find(|t| t.id == id).ok_or_else(|| "Задача не найдена".into())
    }
    pub fn create(
        &mut self,
        project: PathBuf,
        backend: &str,
        title: &str,
        prompt: &str,
    ) -> Result<u64, String> {
        if self.tasks.len() >= 100 || self.next_id == u64::MAX {
            return Err("Достигнут лимит 100 задач".into());
        }
        if !project.is_absolute()
            || project.as_os_str().len() > 4096
            || BackendKind::parse(backend).is_none()
            || !text(title, 256)
            || !text(prompt, MAX_PROMPT)
        {
            return Err(
                "Укажите абсолютный каталог, агента, название до 256 байт и поручение до 16 КиБ"
                    .into(),
            );
        }
        let id = self.next_id;
        self.tasks.push(Task {
            id,
            revision: 1,
            project,
            backend: backend.to_ascii_lowercase(),
            title: title.into(),
            prompt: prompt.into(),
            runs: Vec::new(),
            status: TaskStatus::Planned,
            archived: false,
            git: None,
            queue: None,
            isolated: false,
            memory: None,
        });
        self.next_id += 1;
        Ok(id)
    }
    fn editable(&mut self, id: u64, revision: u64) -> Result<&mut Task, String> {
        let task = self.get_mut(id)?;
        if task.revision != revision {
            return Err("Задача изменилась. Обновите карточку перед изменением.".into());
        }
        if task.active() {
            return Err("Дождитесь завершения задачи или остановите её.".into());
        }
        task.revision.checked_add(1).ok_or("Лимит ревизий")?;
        Ok(task)
    }
    pub fn edit_prompt(&mut self, id: u64, revision: u64, prompt: &str) -> Result<(), String> {
        let task = self.editable(id, revision)?;
        if !task.runs.is_empty()
            || task.archived
            || task.queue.is_some()
            || matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)
        {
            return Err("Редактировать можно поручение открытой задачи до первого запуска и до постановки в очередь.".into());
        }
        if !text(prompt, MAX_PROMPT) {
            return Err("Введите поручение объёмом до 16 КиБ.".into());
        }
        if task.prompt != prompt {
            prompt.clone_into(&mut task.prompt);
            task.revision += 1;
        }
        Ok(())
    }
    pub fn set_memory(
        &mut self,
        id: u64,
        revision: u64,
        memory: Option<bool>,
    ) -> Result<(), String> {
        let task = self.editable(id, revision)?;
        if task.queue.is_some() {
            return Err("Сначала снимите задачу с очереди.".into());
        }
        if task.memory != memory {
            task.memory = memory;
            task.revision += 1;
        }
        Ok(())
    }
    pub fn set_status(&mut self, id: u64, revision: u64, status: TaskStatus) -> Result<(), String> {
        let task = self.editable(id, revision)?;
        if task.archived {
            return Err("Сначала восстановите задачу из архива.".into());
        }
        if matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)
            && status != task.status
            && status != TaskStatus::Planned
        {
            return Err("Сначала верните задачу в статус «Запланирована».".into());
        }
        if task.status != status {
            task.status = status;
            task.revision += 1;
        }
        Ok(())
    }
    pub fn delete(&mut self, id: u64, revision: u64) -> Result<(), String> {
        if self.editable(id, revision)?.queue.is_some() {
            return Err("Сначала снимите задачу с очереди.".into());
        }
        self.tasks.retain(|task| task.id != id);
        Ok(())
    }
    pub fn set_archived(&mut self, id: u64, revision: u64, archived: bool) -> Result<(), String> {
        let task = self.editable(id, revision)?;
        if task.archived != archived {
            task.archived = archived;
            task.revision += 1;
        }
        Ok(())
    }
    pub fn assign(&mut self, id: u64, revision: u64, backend: &str) -> Result<(), String> {
        let backend = BackendKind::parse(backend).ok_or("Неизвестный агент")?.name();
        let task = self.editable(id, revision)?;
        if task.archived || matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred) {
            return Err(
                "Перед передачей восстановите задачу из архива и верните её в работу.".into()
            );
        }
        if task.backend != backend {
            // Legacy attempts predate reassignment: bind them to their original owner
            // before changing the task, never to the next assignee.
            for run in &mut task.runs {
                if run.backend.is_none() {
                    run.backend = Some(task.backend.clone());
                }
            }
            task.backend = backend.into();
            task.revision += 1;
        }
        Ok(())
    }
    /// Starts an attempt and freezes the workflow resolution with it: `workflow` records the
    /// agents the shared roles produced at this instant, so later settings edits cannot rename
    /// the participants of a finished report.
    pub fn begin(
        &mut self,
        id: u64,
        revision: u64,
        next: Option<&str>,
        now: Timestamp,
        roles: &AgentRoles,
    ) -> Result<u32, String> {
        let project_rules = self.rules(&self.get(id)?.project);
        if let Some(policy) = &project_rules.git {
            let prepared = self.get(id)?.git.as_ref().is_some_and(|git| {
                git.state == git::PreparationState::Ready && git.policy == *policy
            });
            if !prepared {
                return Err("Сначала подготовьте Git по текущим правилам проекта.".into());
            }
        }
        let isolated = self.get(id)?.isolated;
        if self.tasks.iter().filter(|t| t.active()).count() >= 2
            || self.tasks.iter().any(|t| t.active() && (t.id == id || !isolated || !t.isolated))
        {
            return Err(
                "Другая задача уже выполняется. Дождитесь завершения или остановите её.".into()
            );
        }
        let task = self.get_mut(id)?;
        if task.revision != revision {
            return Err("Задача изменилась. Обновите карточку перед запуском.".into());
        }
        if task.archived || matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred) {
            return Err(
                "Сначала восстановите задачу из архива и верните её в статус «Запланирована»."
                    .into(),
            );
        }
        if task.runs.len() >= 32 {
            return Err("Достигнут лимит 32 запусков; история сохранена.".into());
        }
        if task.queue.as_ref().is_some_and(|q| q.error.is_none()) {
            return Err("Сначала снимите задачу с очереди.".into());
        }
        let step = task.next_step(next)?.to_owned();
        let id = u32::try_from(task.runs.len() + 1).map_err(|e| e.to_string())?;
        task.revision.checked_add(2).ok_or("Лимит ревизий: нет места для завершения попытки")?;
        task.revision += 1;
        if let Some(git) = &mut task.git
            && git.state == git::PreparationState::Ready
        {
            git.message.clear();
        }
        task.queue = None;
        let workflow = project_rules.effective_workflow(roles).map(std::borrow::Cow::into_owned);
        task.runs.push(Run {
            id,
            step,
            state: RunState::Running,
            result: String::new(),
            diff: String::new(),
            files: Vec::new(),
            diagnostic: String::new(),
            backend: Some(task.backend.clone()),
            rules: (project_rules.revision != 0).then_some(project_rules),
            workflow,
            workflow_report: workflow::WorkflowReport::default(),
            started_at: Some(now),
            finished_at: None,
        });
        Ok(id)
    }
    fn active(&mut self, id: u64, run: u32) -> Result<&mut Task, String> {
        let task = self.get_mut(id)?;
        if !task.runs.last().is_some_and(|r| r.id == run && r.state == RunState::Running) {
            return Err("Событие относится к завершённому или другому запуску".into());
        }
        Ok(task)
    }
    pub fn record_file(&mut self, id: u64, run: u32, file: ResultFile) -> Result<(), String> {
        let attempt = self.active(id, run)?.runs.last_mut().ok_or("Попытка не найдена")?;
        if !file.valid() || attempt.files.len() >= 32 {
            return Err("Некорректное вложение или превышен лимит в 32 файла на попытку".into());
        }
        attempt.files.push(file);
        Ok(())
    }
    pub fn record_diff(&mut self, id: u64, run: u32, diff: &str) -> Result<(), String> {
        if diff.len() > MAX_RESULT {
            return Err("Снимок изменений превышает 64 КиБ.".into());
        }
        if let Some(last) = self.active(id, run)?.runs.last_mut() {
            diff.clone_into(&mut last.diff);
        }
        Ok(())
    }
    pub fn workflow_report_mut(
        &mut self,
        id: u64,
        run: u32,
    ) -> Result<&mut workflow::WorkflowReport, String> {
        self.active(id, run)?
            .runs
            .last_mut()
            .map(|r| &mut r.workflow_report)
            .ok_or_else(|| "Попытка не найдена".into())
    }
    pub fn record(&mut self, id: u64, run: u32, result: &str) -> Result<(), String> {
        if !text(result, MAX_RESULT) {
            return Err("Результат пуст или превышает 64 КиБ; текст не обрезан".into());
        }
        let task = self.active(id, run)?;
        if let Some(last) = task.runs.last_mut() {
            last.result = result.into();
            last.diagnostic.clear();
        }
        Ok(())
    }
    pub fn unavailable(&mut self, id: u64, run: u32, reason: &str) -> Result<(), String> {
        if !text(reason, 512) {
            return Err("Некорректная диагностика результата".into());
        }
        let task = self.active(id, run)?;
        if let Some(last) = task.runs.last_mut() {
            last.result.clear();
            last.diagnostic = reason.into();
        }
        Ok(())
    }
    pub fn finish(
        &mut self,
        id: u64,
        run: u32,
        state: RunState,
        now: Timestamp,
    ) -> Result<(), String> {
        if state == RunState::Running {
            return Err("Нужно конечное состояние запуска".into());
        }
        let task = self.active(id, run)?;
        task.revision = task.revision.checked_add(1).ok_or("Лимит ревизий")?;
        task.status = TaskStatus::NeedsReview;
        if let Some(last) = task.runs.last_mut() {
            last.state = state;
            last.finished_at = Some(now);
            if last.result.is_empty() && last.diagnostic.is_empty() {
                last.diagnostic = "Запуск завершён без сохранённого текста ответа.".into();
            }
        }
        Ok(())
    }
    /// Recovery never creates a new attempt. Unknown side effects require human inspection.
    pub fn recover(&mut self) -> bool {
        let mut changed = false;
        for task in &mut self.tasks {
            if let Some(git) = &mut task.git
                && git.state == git::PreparationState::Preparing
            {
                git.state = git::PreparationState::Interrupted;
                git.message = "Приложение остановилось во время подготовки. Проверьте Git; автоматического повтора нет.".into();
                task.revision = task.revision.saturating_add(1);
                changed = true;
            }
            if let Some(run) = task.runs.last_mut()
                && run.state == RunState::Running
            {
                run.state = RunState::Interrupted;
                if run.diagnostic.is_empty() {
                    run.diagnostic="Приложение остановилось до сохранения конечного состояния. Проверьте прежние эффекты перед продолжением.".into();
                }
                task.revision = task.revision.saturating_add(1);
                task.status = TaskStatus::NeedsReview;
                changed = true;
            }
        }
        changed
    }
}

pub mod service;
