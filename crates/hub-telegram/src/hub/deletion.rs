//! Desktop-initiated deletion. Reserve the topic until Telegram answers.
use super::{Agents, Hub, HubMessage, Target, TopicKey, oneshot};

pub struct DeletionDone {
    key: TopicKey,
    result: Result<(), String>,
}

impl<A: Agents> Hub<A> {
    pub(super) fn topic_unavailable(&self, key: TopicKey) -> bool {
        self.deleting.contains_key(&key) || self.deleted.contains(&key)
    }

    pub(super) fn delete_topic(
        &mut self,
        key: TopicKey,
        reply: oneshot::Sender<Result<(), String>>,
    ) {
        if self.shutdown.is_some()
            || self.telegram_identity.chat != key.chat
            || self.settings.borrow().telegram != self.telegram_identity
            || key.thread.0 <= 1
            || !self.topics.contains_key(&key)
        {
            let _ = reply.send(Err("Удаление недоступно: выберите обычную тему настроенной группы при работающем боте.".into()));
            return;
        }
        if self.deleted.contains(&key) {
            let _ = reply.send(self.clean_deleted_topic(key));
            return;
        }
        if let Err(error) = self.check_deletion(key) {
            let _ = reply.send(Err(error));
            return;
        }
        self.deleting.insert(key, reply);
        let sender = self.sender.clone();
        let mailbox = self.mailbox.clone();
        let pending = self.results.store.pending(key.chat);
        let settings = self.settings.clone();
        let identity = self.telegram_identity.clone();
        tokio::spawn(async move {
            let result = async {
                let snapshot = pending.await.map_err(|error| format!("Не удалось проверить очередь доставки: {error}"))?;
                if snapshot.entries.iter().any(|entry| entry.result().key() == key) {
                    return Err("В теме есть недоставленные ответы. Дождитесь доставки или удалите их через /outbox.".into());
                }
                if mailbox.is_closed() || settings.borrow().telegram != identity {
                    return Err("Настройки Telegram изменились. Обновите список тем.".into());
                }
                sender.messenger().delete_topic(key).await.map_err(|error| error.to_string())
            }.await;
            let _ = mailbox.send(HubMessage::DeletionDone(DeletionDone { key, result })).await;
        });
    }

    fn check_deletion(&self, key: TopicKey) -> Result<(), String> {
        let queued = self
            .tasks
            .inspect(|book| {
                book.tasks().iter().any(|task| {
                    task.queue.as_ref().and_then(|entry| entry.origin).is_some_and(|origin| {
                        origin.chat == key.chat.0 && origin.thread == key.thread.0
                    })
                })
            })
            .map_err(|error| format!("Не удалось проверить очередь задач темы: {error}"))?;
        if queued {
            return Err("В очереди есть задачи из этой темы. Дождитесь их выполнения или снимите их с очереди.".into());
        }
        if self.deleting.contains_key(&key)
            || self.running.contains_key(&key)
            || self.task_runs.contains_key(&key)
            || self.handoff_busy(key)
            || self.snapshotting.contains(&key)
            || self.git_job.as_ref().is_some_and(|job| job.key == key)
            || self.execution.as_ref().is_some_and(|execution| {
                execution.snapshots().iter().any(|live| {
                    live.active
                        && self
                            .topics
                            .get(&key)
                            .is_some_and(|session| session.cwd.as_path() == live.project)
                })
            })
            || self.memory.busy_in(key)
            || self.voices.busy_in(key)
            || self.actions.busy_in(Target::Topic(key))
            || self.doctors.busy(key)
            || self.albums.values().flatten().any(|part| part.key() == Some(key))
            || self.topics.get(&key).is_some_and(|s| self.is_reserved(s))
        {
            return Err(
                "Дождитесь завершения активной задачи или операции и повторите удаление.".into()
            );
        }
        Ok(())
    }

    pub(super) fn finish_deletion(&mut self, done: DeletionDone) {
        let Some(reply) = self.deleting.remove(&done.key) else { return };
        let result = done.result.and_then(|()| {
            self.deleted.insert(done.key);
            self.clean_deleted_topic(done.key)
        });
        let _ = reply.send(result);
    }

    fn clean_deleted_topic(&mut self, key: TopicKey) -> Result<(), String> {
        // Keep the row available for a cleanup retry until both files are durable.
        let failed = |err: String| {
            format!(
                "Тема удалена в Telegram. Не удалось сохранить изменения на диске: {err}. Повторите удаление для завершения очистки."
            )
        };
        let mut titles = self.titles.clone();
        if titles.remove(&key).is_some() {
            self.store.save_titles(&titles).map_err(failed)?;
            self.titles = titles;
        }
        let mut topics = self.topics.clone();
        topics.remove(&key);
        self.store.save(&topics).map_err(failed)?;
        self.invalidate_handoff(key);
        self.voices.invalidate(key);
        self.projects.invalidate(key);
        self.options.invalidate(key);
        self.last_progress.remove(&key);
        self.replace_topics(topics);
        Ok(())
    }
}
