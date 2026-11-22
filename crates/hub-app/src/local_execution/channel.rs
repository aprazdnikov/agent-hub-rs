use futures::future::BoxFuture;
use hub_agent::channel::UserChannel;
use hub_core::domain::{
    Decision, Denied, FileDelivery, OutgoingFile, Question, QuestionAnswer, QuestionsOutcome,
    ToolRequest,
};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::{oneshot, watch};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub use hub_telegram::execution::{Answer, Interaction, Kind};
struct Pending {
    interaction: Interaction,
    deadline: tokio::time::Instant,
    reply: oneshot::Sender<Answer>,
}
pub struct Channel {
    pub(super) files: OnceLock<Arc<super::files::FileTarget>>,
    pending: Mutex<Option<Pending>>,
    view: watch::Sender<Option<Interaction>>,
    cancel: CancellationToken,
    timeout: Duration,
}
impl Channel {
    pub fn new(timeout: Duration, cancel: CancellationToken) -> Self {
        Self {
            files: OnceLock::new(),
            pending: Mutex::new(None),
            view: watch::Sender::new(None),
            cancel,
            timeout,
        }
    }
    pub fn subscribe(&self) -> watch::Receiver<Option<Interaction>> {
        self.view.subscribe()
    }
    pub fn close(&self) {
        self.cancel.cancel();
        if let Ok(mut pending) = self.pending.lock() {
            pending.take();
        }
        self.view.send_replace(None);
    }
    pub fn respond(&self, id: Uuid, answer: Answer) -> Result<(), String> {
        let mut pending = self.pending.lock().map_err(|_| "Запрос недоступен")?;
        let current = pending
            .as_ref()
            .filter(|p| p.interaction.id == id)
            .ok_or("Запрос уже завершён или относится к другому запуску")?;
        if self.cancel.is_cancelled() {
            return Err("Запуск остановлен".into());
        }
        if tokio::time::Instant::now() >= current.deadline {
            pending.take();
            self.view.send_replace(None);
            return Err("Время ответа истекло. Действие не разрешено.".into());
        }
        match (&current.interaction.kind, &answer) {
            (_, Answer::Deny) | (Kind::Permission(_), Answer::Allow) => {}
            (Kind::Questions(questions), Answer::Text(texts))
                if questions.len() == texts.len()
                    && texts.iter().all(|text| !text.trim().is_empty() && text.len() <= 16384) => {}
            _ => return Err("Заполните ответ на каждый вопрос (до 16 КиБ).".into()),
        }
        let Some(current) = pending.take() else {
            return Err("Запрос завершён".into());
        };
        self.view.send_replace(None);
        current.reply.send(answer).map_err(|_| "Запрос уже завершён".into())
    }
    async fn wait(&self, kind: Kind) -> Answer {
        let id = Uuid::new_v4();
        let (reply, answer) = oneshot::channel();
        let deadline = tokio::time::Instant::now() + self.timeout;
        {
            let Ok(mut pending) = self.pending.lock() else { return Answer::Deny };
            if pending.is_some() || self.cancel.is_cancelled() {
                return Answer::Deny;
            }
            let interaction = Interaction { id, kind };
            *pending = Some(Pending { interaction: interaction.clone(), deadline, reply });
            self.view.send_replace(Some(interaction));
        }
        let result = tokio::select! {
            biased;
            () = self.cancel.cancelled() => Answer::Deny,
            () = tokio::time::sleep_until(deadline) => Answer::Deny,
            result = answer => result.unwrap_or(Answer::Deny),
        };
        if let Ok(mut pending) = self.pending.lock()
            && pending.as_ref().is_some_and(|p| p.interaction.id == id)
        {
            pending.take();
            self.view.send_replace(None);
        }
        result
    }
}
impl UserChannel for Channel {
    fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision> {
        Box::pin(async move {
            if tool.tool.len() > 1024 || tool.summary.len() > 16384 {
                return Decision::Denied(Denied::new("Запрос разрешения слишком большой"));
            }
            match self.wait(Kind::Permission(tool)).await {
                Answer::Allow => Decision::Allowed,
                Answer::Deny | Answer::Text(_) => Decision::Denied(Denied::new(
                    "Разрешение не получено: отказ, остановка или истечение времени",
                )),
            }
        })
    }
    fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        Box::pin(async move {
            if questions.is_empty()
                || questions.len() > 8
                || questions.iter().any(|q| {
                    q.text().len() > 16384
                        || q.header().len() > 1024
                        || q.options().len() > 32
                        || q.options().iter().any(|o| o.label.len() + o.description.len() > 16384)
                })
            {
                return QuestionsOutcome::Denied(Denied::new(
                    "Вопросы превышают допустимый размер",
                ));
            }
            match self.wait(Kind::Questions(questions.clone())).await {
                Answer::Text(texts) => QuestionsOutcome::Answered(
                    questions
                        .iter()
                        .zip(texts)
                        .map(|(question, answer)| QuestionAnswer {
                            question: question.text().into(),
                            answer,
                        })
                        .collect(),
                ),
                Answer::Allow | Answer::Deny => QuestionsOutcome::Denied(Denied::new(
                    "Ответ не получен: отказ, остановка или истечение времени",
                )),
            }
        })
    }
    fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(async move {
            let Some(target) = self.files.get().cloned() else {
                return FileDelivery::Denied(Denied::new("Нет активной попытки"));
            };
            let Some(dirs) = crate::dirs::AppDirs::locate() else {
                return FileDelivery::Denied(Denied::new("Каталог данных недоступен"));
            };
            let cancel = self.cancel.clone();
            match tokio::task::spawn_blocking(move || {
                target.deliver(&dirs.task_files(), &file, &cancel)
            })
            .await
            {
                Ok(Ok(())) => FileDelivery::Delivered,
                Ok(Err(error)) => FileDelivery::Denied(Denied::new(error)),
                Err(error) => FileDelivery::Denied(Denied::new(error.to_string())),
            }
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use hub_agent::channel::UserChannel;
    use std::sync::Arc;
    #[tokio::test(start_paused = true)]
    async fn approval_after_deadline_is_denied_even_before_waiter_is_polled() {
        let channel = Channel::new(Duration::from_secs(2), CancellationToken::new());
        let mut pending =
            channel.request(ToolRequest { tool: "write".into(), summary: "file".into() });
        assert!(futures::poll!(&mut pending).is_pending());
        let id = channel.subscribe().borrow().as_ref().unwrap().id;
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(channel.respond(id, Answer::Allow).is_err());
        assert!(matches!(pending.await, Decision::Denied(_)));
    }
    #[tokio::test]
    async fn permission_is_explicit_and_stale_replies_are_rejected() {
        let channel = Arc::new(Channel::new(Duration::from_secs(30), CancellationToken::new()));
        let mut view = channel.subscribe();
        let worker = channel.clone();
        let pending = tokio::spawn(async move {
            worker.request(ToolRequest { tool: "write".into(), summary: "file".into() }).await
        });
        view.changed().await.unwrap();
        let id = view.borrow().as_ref().unwrap().id;
        assert!(!pending.is_finished());
        assert!(channel.respond(uuid::Uuid::new_v4(), Answer::Allow).is_err());
        channel.respond(id, Answer::Allow).unwrap();
        assert_eq!(pending.await.unwrap(), Decision::Allowed);
        assert!(channel.respond(id, Answer::Allow).is_err());
    }
    #[tokio::test(start_paused = true)]
    async fn question_timeout_denies_and_clears_pending() {
        let channel = Channel::new(Duration::from_secs(2), CancellationToken::new());
        let question = Question::new(
            "Why?".into(),
            String::new(),
            vec![],
            hub_core::domain::Selection::Single,
        )
        .unwrap();
        assert!(matches!(channel.ask(vec![question]).await, QuestionsOutcome::Denied(_)));
        assert!(channel.subscribe().borrow().is_none());
    }
}
