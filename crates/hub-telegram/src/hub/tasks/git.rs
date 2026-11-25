//! The actor owns durable preparation; workers only execute Git and report evidence.
use super::{Agents, Hub, Task, TaskBook, TopicKey, TopicSession};
use crate::{git::Request, hub::HubMessage, messenger::Target};
use futures::FutureExt;
use hub_core::tasks::service::{Access, BusyLease};
use jiff::Timestamp;
use tokio_util::sync::CancellationToken;

enum Intent {
    Prepare,
    Start { next: Option<String> },
}

pub(in crate::hub) struct Job {
    ticket: uuid::Uuid,
    pub(in crate::hub) key: TopicKey,
    session: TopicSession,
    settings: std::sync::Arc<hub_core::settings::Settings>,
    id: u64,
    revision: u64,
    rules_revision: u64,
    intent: Intent,
    pub cancel: CancellationToken,
    worker: tokio::task::JoinHandle<()>,
    _lease: BusyLease,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.worker.abort();
    }
}
pub struct Done {
    ticket: uuid::Uuid,
    result: Result<String, String>,
}
impl<A: Agents> Hub<A> {
    pub(in crate::hub) fn cancel_git_if_settings_changed(&self) {
        if let Some(job) = &self.git_job
            && job.settings != *self.settings.borrow()
        {
            job.cancel.cancel();
        }
    }
    pub(in crate::hub) fn git_busy(&self) -> bool {
        self.git_job.is_some() || self.execution.as_ref().is_some_and(|execution| execution.busy())
    }
    pub(in crate::hub) fn cancel_git(&self, key: TopicKey) -> bool {
        if let Some(job) = &self.git_job
            && job.key == key
        {
            job.cancel.cancel();
            return true;
        }
        false
    }
    fn git_idle(&self, session: &TopicSession) -> Result<(), String> {
        if !self.root().is_ok_and(|root| crate::paths::inside(&root, &session.cwd)) {
            return Err("Каталог задачи недоступен или находится вне текущего корня проектов. Проверьте настройки и тему.".into());
        }
        if self.git_busy()
            || !self.running.is_empty()
            || !self.actions.is_idle()
            || !self.handoffs.is_idle()
            || !self.snapshotting.is_empty()
        {
            return Err(
                "Дождитесь завершения работы хаба перед подготовкой или проверкой Git.".into()
            );
        }
        Ok(())
    }
    pub(in crate::hub) fn prepare_git(
        &mut self,
        key: TopicKey,
        session: &TopicSession,
        id: u64,
        revision: u64,
        branch: &str,
        book: &TaskBook,
    ) -> Result<Vec<Task>, String> {
        self.git_idle(session)?;
        if book.get(id)?.project != session.cwd.as_path() {
            return Err("Задача принадлежит другому проекту".into());
        }
        let lease = self.tasks.reserve()?;
        let expected_rules = book.rules(session.cwd.as_path()).revision;
        let task = self.tasks.access(Access::Internal, |book, _| {
            if book.rules(session.cwd.as_path()).revision != expected_rules {
                return Err("Правила изменились. Проверьте настройки Git.".into());
            }
            book.prepare_git(id, revision, branch)?;
            Ok(book.get(id)?.clone())
        })?;
        self.spawn_git(key, session, &task, Intent::Prepare, lease, expected_rules)?;
        Ok(vec![task])
    }
    pub(in crate::hub) fn checked_git_start(
        &mut self,
        key: TopicKey,
        session: &TopicSession,
        id: u64,
        revision: u64,
        next: Option<&str>,
        mut book: TaskBook,
    ) -> Result<Vec<Task>, String> {
        self.git_idle(session)?;
        if book.get(id)?.project != session.cwd.as_path()
            || book.get(id)?.backend != session.backend.name()
        {
            return Err("Выберите тему с каталогом и агентом задачи.".into());
        }
        let lease = self.tasks.reserve()?;
        let rules_revision = book.rules(session.cwd.as_path()).revision;
        // Validate all pure preconditions without publishing an attempt before verification.
        let task = book.get(id)?.clone();
        book.begin(id, revision, next, Timestamp::now(), &self.settings.borrow().roles)?;
        self.spawn_git(
            key,
            session,
            &task,
            Intent::Start { next: next.map(str::to_owned) },
            lease,
            rules_revision,
        )?;
        Ok(vec![task])
    }
    fn spawn_git(
        &mut self,
        key: TopicKey,
        session: &TopicSession,
        task: &Task,
        intent: Intent,
        lease: BusyLease,
        rules_revision: u64,
    ) -> Result<(), String> {
        let preparation = task.git.clone().ok_or("Сначала подготовьте ветку Git.")?;
        let request = Request {
            project: task.project.clone(),
            preparation,
            verify_only: matches!(intent, Intent::Start { .. }),
            continuation: !task.runs.is_empty(),
        };
        let cancel = CancellationToken::new();
        let worker_cancel = cancel.clone();
        let runner = self.git_runner.clone();
        let mailbox = self.mailbox.clone();
        let ticket = uuid::Uuid::new_v4();
        let worker = tokio::spawn(async move {
            let result = std::panic::AssertUnwindSafe(runner.run(request, worker_cancel))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    Err("Подготовка Git неожиданно завершилась. Проверьте репозиторий.".into())
                });
            let _ = mailbox.send(HubMessage::GitDone(Done { ticket, result })).await;
        });
        self.git_job = Some(Job {
            ticket,
            key,
            session: session.clone(),
            settings: self.settings.borrow().clone(),
            id: task.id,
            revision: task.revision,
            rules_revision,
            intent,
            cancel,
            worker,
            _lease: lease,
        });
        Ok(())
    }
    pub(in crate::hub) fn git_done(&mut self, done: Done) {
        if self.git_job.as_ref().is_none_or(|job| job.ticket != done.ticket) {
            return;
        }
        let Some(job) = self.git_job.take() else { return };
        let result = self.finish_git_job(&job, done.result);
        if self.shutdown.is_none() && self.settings.borrow().telegram.chat == job.key.chat {
            let message = result.unwrap_or_else(|error| format!("Git: {error}"));
            self.say(Target::Topic(job.key), message);
        }
    }
    fn finish_git_job(
        &mut self,
        job: &Job,
        result: Result<String, String>,
    ) -> Result<String, String> {
        let book = self.tasks.read()?;
        if matches!(job.intent, Intent::Prepare) {
            let result = if job.cancel.is_cancelled() {
                Err("Подготовка остановлена. Проверьте Git перед повтором.".into())
            } else {
                result
            };
            let message = self.tasks.access(Access::Internal, |book, _| {
                book.finish_git(job.id, job.revision, result)?;
                Ok(book
                    .get(job.id)?
                    .git
                    .as_ref()
                    .map_or(String::new(), hub_core::tasks::git::Preparation::describe))
            })?;
            return Ok(message);
        }
        let error = if job.cancel.is_cancelled() || self.shutdown.is_some() {
            Some("Проверка перед запуском остановлена; агент не запускался.".to_owned())
        } else if self.topics.get(&job.key) != Some(&job.session)
            || *self.settings.borrow() != job.settings
            || self.settings.borrow().telegram.chat != job.key.chat
            || book.rules(job.session.cwd.as_path()).revision != job.rules_revision
        {
            Some("Контекст темы или правила изменились. Проверьте их перед запуском.".into())
        } else {
            result.err()
        };
        if let Some(message) = error {
            self.tasks.access(Access::Internal, |book, _| {
                book.note_git(job.id, job.revision, &message)
            })?;
            return Err(message);
        }
        let next = match &job.intent {
            Intent::Start { next } => next.as_deref(),
            Intent::Prepare => None,
        };
        self.launch_task(job.key, &job.session, job.id, job.revision, next, &book)?;
        Ok("Git проверен; задача запущена.".into())
    }
}
