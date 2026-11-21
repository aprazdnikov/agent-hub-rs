//! Durable task state. Starting a model is deliberately outside this module.
use crate::domain::BackendKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Extra bounded space lets a legacy full file record terminal recovery metadata.
pub const MAX_READ_BYTES: usize = MAX_FILE_BYTES + 1024;
const MAX_PROMPT: usize = 16 * 1024;
const MAX_RESULT: usize = 64 * 1024;

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
pub struct Run {
    pub id: u32,
    pub step: String,
    pub state: RunState,
    pub result: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub diagnostic: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub id: u64,
    pub revision: u64,
    pub project: PathBuf,
    pub backend: String,
    pub title: String,
    pub prompt: String,
    pub runs: Vec<Run>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskBook {
    version: u32,
    next_id: u64,
    tasks: Vec<Task>,
}
impl Default for TaskBook {
    fn default() -> Self {
        Self { version: 1, next_id: 1, tasks: Vec::new() }
    }
}
fn text(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max
}
impl TaskBook {
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_READ_BYTES {
            return Err("Файл задач превышает 8 МиБ".into());
        }
        let book: Self = serde_json::from_slice(bytes)
            .map_err(|_| "Файл задач повреждён или имеет неизвестный формат")?;
        book.validate()?;
        if book
            .tasks
            .iter()
            .any(|task| task.runs.last().is_some_and(|run| run.state == RunState::Running))
            && serde_json::to_vec(&book).map_err(|e| e.to_string())?.len() > MAX_FILE_BYTES
        {
            return Err("Активная задача превышает допустимый объём".into());
        }
        Ok(book)
    }
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let active = self
            .tasks
            .iter()
            .any(|task| task.runs.last().is_some_and(|run| run.state == RunState::Running));
        let limit = if active { MAX_FILE_BYTES - 1024 } else { MAX_READ_BYTES };
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
        if self.version != 1 || self.next_id == 0 || self.tasks.len() > 100 {
            return Err("Неизвестная версия или неверные лимиты файла задач".into());
        }
        for task in &self.tasks {
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
            {
                return Err("Некорректная запись задачи".into());
            }
            for (index, run) in task.runs.iter().enumerate() {
                if usize::try_from(run.id).ok() != Some(index + 1)
                    || !text(&run.step, MAX_PROMPT)
                    || run.result.len() > MAX_RESULT
                    || run.diagnostic.len() > 512
                    || (run.state == RunState::Running && index + 1 != task.runs.len())
                {
                    return Err("Некорректная история запусков".into());
                }
                running += usize::from(run.state == RunState::Running);
            }
        }
        if running > 1 {
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
            backend: backend.into(),
            title: title.into(),
            prompt: prompt.into(),
            runs: Vec::new(),
        });
        self.next_id += 1;
        Ok(id)
    }
    pub fn begin(&mut self, id: u64, revision: u64, next: Option<&str>) -> Result<u32, String> {
        if self.tasks.iter().any(|t| t.runs.last().is_some_and(|r| r.state == RunState::Running)) {
            return Err(
                "Другая задача уже выполняется. Дождитесь завершения или остановите её.".into()
            );
        }
        let task = self.get_mut(id)?;
        if task.revision != revision {
            return Err("Задача изменилась. Обновите карточку перед запуском.".into());
        }
        if task.runs.len() >= 32 {
            return Err("Достигнут лимит 32 запусков; история сохранена.".into());
        }
        let step = match (task.runs.is_empty(), next) {
            (true, None) => task.prompt.clone(),
            (false, Some(value)) if text(value, MAX_PROMPT) => value.into(),
            _ => return Err(
                "Для продолжения проверьте прежние эффекты и укажите следующий шаг (до 16 КиБ)."
                    .into(),
            ),
        };
        let id = u32::try_from(task.runs.len() + 1).map_err(|e| e.to_string())?;
        task.revision.checked_add(2).ok_or("Лимит ревизий: нет места для завершения попытки")?;
        task.revision += 1;
        task.runs.push(Run {
            id,
            step,
            state: RunState::Running,
            result: String::new(),
            diagnostic: String::new(),
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
    pub fn finish(&mut self, id: u64, run: u32, state: RunState) -> Result<(), String> {
        if state == RunState::Running {
            return Err("Нужно конечное состояние запуска".into());
        }
        let task = self.active(id, run)?;
        task.revision = task.revision.checked_add(1).ok_or("Лимит ревизий")?;
        if let Some(last) = task.runs.last_mut() {
            last.state = state;
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
            if let Some(run) = task.runs.last_mut()
                && run.state == RunState::Running
            {
                run.state = RunState::Interrupted;
                if run.diagnostic.is_empty() {
                    run.diagnostic="Приложение остановилось до сохранения конечного состояния. Проверьте прежние эффекты перед продолжением.".into();
                }
                task.revision = task.revision.saturating_add(1);
                changed = true;
            }
        }
        changed
    }
}
