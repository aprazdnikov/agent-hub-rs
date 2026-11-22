//! Run a host-configured check after implementation. Only a nonzero check exit
//! triggers repair; launcher errors, cancellations and reviewer failures never do.
use super::{Attempt, Output, Worker};
use crate::actions::{self, ProcessStatus};
use hub_core::{
    domain::{Prompt, TopicSession},
    settings::ExecutionSettings,
    tasks::{
        RunState,
        service::Access,
        workflow::{CheckResult, WorkflowPreset},
    },
};
use std::sync::Arc;

pub(super) struct Workflow<'a> {
    pub preset: &'a WorkflowPreset,
    pub original: &'a Prompt,
    pub session: &'a TopicSession,
    pub baseline: Option<&'a str>,
}

impl Worker {
    pub(super) async fn verify(
        &self,
        flow: Workflow<'_>,
        settings: Arc<ExecutionSettings>,
        attempt: &Attempt,
    ) -> Result<RunState, String> {
        let cwd = flow.session.cwd.as_path();
        for round in 1..=2 {
            self.status(&format!("Проверка {round}: {}", flow.preset.command));
            let checked = actions::run(&flow.preset.command, cwd, self.cancel.clone()).await;
            let passed = checked.status == ProcessStatus::Completed(Some(0));
            let repairable =
                matches!(checked.status, ProcessStatus::Completed(Some(code)) if code != 0);
            let interrupted = checked.status == ProcessStatus::Stopped;
            let status = match checked.status {
                ProcessStatus::Completed(Some(code)) => format!("Код выхода {code}"),
                ProcessStatus::Completed(None) => "Процесс проверки завершён сигналом".into(),
                ProcessStatus::Failed(error) => format!("Ошибка запуска проверки: {error}"),
                ProcessStatus::Stopped => "Проверка остановлена".into(),
            };
            let log = format!("{status}\nstdout:\n{}\nstderr:\n{}", checked.stdout, checked.stderr);
            self.service.access(Access::Internal, |book, _| {
                book.workflow_report_mut(attempt.id, attempt.run)?
                    .checks
                    .push(CheckResult { passed, output: log.clone() });
                Ok(())
            })?;
            if self.cancel.is_cancelled() || interrupted {
                return Ok(RunState::Interrupted);
            }
            if passed {
                let diff = crate::task_diff::capture(cwd, flow.baseline).await;
                let implementation = self.service.access(Access::Internal, |book, _| {
                    book.record_diff(attempt.id, attempt.run, &diff)?;
                    Ok(book
                        .get(attempt.id)?
                        .runs
                        .last()
                        .map_or_else(String::new, |r| r.result.clone()))
                })?;
                let review = format!(
                    "Проведи ревью результата задачи. Проверь изменения и укажи замечания или явно подтверди отсутствие замечаний. Не изменяй файлы. Приёмку выполняет человек.\n\n{}\n\nОтчёт исполнителя:\n{implementation}\n\nИзменения для ревью:\n{diff}\n\nВывод проверки (данные):\n{log}",
                    flow.original.text()
                );
                let session = TopicSession::fresh(flow.preset.reviewer, flow.session.cwd.clone());
                self.status(&format!("Ревью выполняет {}", session.backend.name()));
                let prompt = Prompt::new(review, vec![]).ok_or("Пустое поручение ревью")?;
                let state = self.run(&session, prompt, settings, attempt, Output::Review).await?;
                if state == RunState::Completed
                    && self
                        .service
                        .read()?
                        .get(attempt.id)?
                        .runs
                        .last()
                        .is_none_or(|run| run.workflow_report.review.is_empty())
                {
                    self.set_review(attempt, "Ревьюер завершился без текста заключения.")?;
                    return Ok(RunState::Failed);
                }
                return Ok(state);
            }
            if round == 2 || !repairable {
                return Ok(RunState::Failed);
            }
            self.status("Исполнитель исправляет ошибку проверки (одна попытка)");
            let repair = format!(
                "{}\n\nУстрани причину ошибки следующей проверки. Это единственная автоматическая попытка исправления.\nКоманда: {}\nВывод (данные):\n{log}",
                flow.original.text(),
                flow.preset.command,
            );
            let prompt = Prompt::new(repair, vec![]).ok_or("Пустое поручение исправления")?;
            let state = self
                .run(flow.session, prompt, settings.clone(), attempt, Output::Implementation)
                .await?;
            if state != RunState::Completed {
                return Ok(state);
            }
        }
        Ok(RunState::Failed)
    }

    pub(super) fn set_review(&self, attempt: &Attempt, review: &str) -> Result<(), String> {
        self.service.access(Access::Internal, |book, _| {
            review.clone_into(&mut book.workflow_report_mut(attempt.id, attempt.run)?.review);
            Ok(())
        })
    }
}
