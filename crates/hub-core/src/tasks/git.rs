//! Durable intent and evidence for explicit Git preparation; no process execution here.
use super::{TaskBook, TaskStatus};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitPolicy {
    pub remote: String,
    pub base: String,
}
/// Conservative portable names, additionally checked with Git before execution.
#[must_use]
pub fn valid_branch(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && !value.starts_with(['-', '/'])
        && !value.ends_with(['.', '/'])
        && !value.contains("..")
        && !value.contains("//")
        && value.split('/').all(|part| {
            !part.starts_with('.')
                && !part
                    .rsplit_once('.')
                    .is_some_and(|(_, suffix)| suffix.eq_ignore_ascii_case("lock"))
        })
        && value.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_/ .".contains(&c) && c != b' ')
        && value != "HEAD"
}
impl GitPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.remote.is_empty()
            || self.remote.len() > 80
            || self.remote.starts_with(['-', '.'])
            || !self.remote.bytes().all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            || !valid_branch(&self.base)
        {
            return Err(
                "Укажите имя remote и точное имя базовой ветки (латиница, цифры, -, _, /, .)."
                    .into(),
            );
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparationState {
    Preparing,
    Ready,
    Failed,
    Interrupted,
}
impl PreparationState {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Preparing => "Подготавливаем Git",
            Self::Ready => "Ветка подготовлена",
            Self::Failed => "Подготовка не завершена",
            Self::Interrupted => "Подготовка прервана; проверьте Git",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preparation {
    pub policy: GitPolicy,
    pub branch: String,
    pub state: PreparationState,
    pub sha: String,
    pub message: String,
}
impl Preparation {
    pub fn validate(&self) -> Result<(), String> {
        self.policy.validate()?;
        if !valid_branch(&self.branch)
            || self.branch == self.policy.base
            || self.message.len() > 512
            || (self.state == PreparationState::Ready && !valid_sha(&self.sha))
            || (self.state != PreparationState::Ready && !self.sha.is_empty())
        {
            return Err("Некорректный результат подготовки Git".into());
        }
        Ok(())
    }
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} · {}/{} → {}\nSHA базы: {}\n{}",
            self.state.label(),
            self.policy.remote,
            self.policy.base,
            self.branch,
            self.sha,
            self.message
        )
    }
}
#[must_use]
pub fn valid_sha(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|c| c.is_ascii_hexdigit())
}
impl TaskBook {
    pub fn note_git(&mut self, id: u64, revision: u64, message: &str) -> Result<(), String> {
        if message.len() > 512 {
            return Err("Диагностика Git слишком длинная".into());
        }
        let task = self.editable(id, revision)?;
        let git = task.git.as_mut().ok_or("Подготовка Git не найдена")?;
        git.message = message.into();
        task.revision += 1;
        Ok(())
    }
    pub fn prepare_git(&mut self, id: u64, revision: u64, branch: &str) -> Result<(), String> {
        if self.tasks.iter().any(super::Task::active) {
            return Err("Дождитесь завершения текущей задачи или подготовки Git.".into());
        }
        let policy = self
            .rules(&self.get(id)?.project)
            .git
            .ok_or("Сначала включите подготовку Git в правилах проекта.")?;
        let preparation = Preparation {
            policy,
            branch: branch.into(),
            state: PreparationState::Preparing,
            sha: String::new(),
            message: String::new(),
        };
        preparation.validate()?;
        let task = self.editable(id, revision)?;
        if !task.runs.is_empty()
            || task.archived
            || matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)
        {
            return Err("Подготовка новой ветки доступна только для новой задачи в работе, до первого запуска.".into());
        }
        task.revision.checked_add(2).ok_or("Лимит ревизий")?;
        task.git = Some(preparation);
        task.revision += 1;
        Ok(())
    }
    pub fn finish_git(
        &mut self,
        id: u64,
        revision: u64,
        result: Result<String, String>,
    ) -> Result<(), String> {
        let task = self.get_mut(id)?;
        if task.revision != revision
            || !task.git.as_ref().is_some_and(|git| git.state == PreparationState::Preparing)
        {
            return Err("Результат относится к прежней подготовке Git.".into());
        }
        let mut git = task.git.clone().ok_or("Подготовка не найдена")?;
        match result {
            Ok(sha) => {
                git.sha = sha;
                git.state = PreparationState::Ready;
            }
            Err(message) => {
                git.message = message;
                git.state = PreparationState::Failed;
            }
        }
        git.validate()?;
        let revision = task.revision.checked_add(1).ok_or("Лимит ревизий")?;
        task.git = Some(git);
        task.revision = revision;
        Ok(())
    }
}
