//! Desktop-initiated deletion and topic synchronization. Reserve the topic until Telegram
//! answers.
use std::time::Duration;

use crate::messenger::SendError;

use super::{Agents, Hub, HubMessage, Target, TopicKey, oneshot};

pub struct DeletionDone {
    key: TopicKey,
    result: Result<(), String>,
}

/// The probe results of one synchronization walk, handed back to the actor that started it.
pub struct SyncDone {
    /// Topics the probes proved missing, in probe order.
    stale: Vec<TopicKey>,
    /// The walk stopped early because Telegram asked to wait this long.
    retry_after: Option<Duration>,
    reply: oneshot::Sender<Result<String, String>>,
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

    /// Unbinds a deleted project from its topics and the recent list. Disk first: a
    /// failed save leaves the in-memory state untouched, so the cascade can be retried.
    pub(super) fn forget_project(&mut self, id: &str) -> Result<usize, String> {
        let mut topics = self.topics.clone();
        let mut detached = 0;
        for session in topics.values_mut() {
            detached += usize::from(session.preferences.detach_project(id));
        }
        if detached > 0 {
            self.store
                .save(&topics)
                .map_err(|error| format!("Не удалось сохранить отвязку тем: {error}"))?;
            self.replace_topics(topics);
        }
        let mut recent = self.projects.recent.clone();
        recent.forget(id);
        if self.projects.recent.ids() != recent.ids() {
            self.store
                .save_recent_projects(&recent)
                .map_err(|error| format!("Не удалось обновить последние проекты: {error}"))?;
            self.projects.recent = recent;
        }
        Ok(detached)
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

impl<A: Agents> Hub<A> {
    pub(super) fn sync_topics(&mut self, reply: oneshot::Sender<Result<String, String>>) {
        if self.shutdown.is_some() || self.settings.borrow().telegram != self.telegram_identity {
            let _ = reply.send(Err(
                "Синхронизация недоступна: запустите бота с сохранёнными настройками Telegram."
                    .into(),
            ));
            return;
        }
        let candidates: Vec<TopicKey> = self
            .topics
            .keys()
            .copied()
            .filter(|key| {
                key.thread.0 > 1
                    && key.chat == self.telegram_identity.chat
                    && !self.topic_unavailable(*key)
            })
            // The same busy guards as deletion; undelivered answers are not a reason to skip,
            // because a topic missing in Telegram cannot receive them anyway.
            .filter(|key| self.check_deletion(*key).is_ok())
            .collect();
        if candidates.is_empty() {
            let _ = reply.send(Ok("Все темы актуальны".to_owned()));
            return;
        }
        let sender = self.sender.clone();
        let mailbox = self.mailbox.clone();
        let settings = self.settings.clone();
        let identity = self.telegram_identity.clone();
        tokio::spawn(async move {
            let mut stale = Vec::new();
            for key in candidates {
                if mailbox.is_closed() {
                    let _ = reply.send(Err("Бот остановлен. Синхронизация прервана.".into()));
                    return;
                }
                if settings.borrow().telegram != identity {
                    let _ = reply
                        .send(Err("Настройки Telegram изменились. Обновите список тем.".into()));
                    return;
                }
                match sender.messenger().probe_topic(key).await {
                    Ok(false) => stale.push(key),
                    // An uncertain answer must not drop local state: skip the topic and
                    // keep checking the rest.
                    Ok(true) | Err(SendError::Rejected(_) | SendError::Failed(_)) => {}
                    Err(SendError::RetryAfter(retry_after)) => {
                        // Telegram rate-limits the walk itself; stop instead of hammering.
                        let _ = mailbox
                            .send(HubMessage::SyncDone(SyncDone {
                                stale,
                                retry_after: Some(retry_after),
                                reply,
                            }))
                            .await;
                        return;
                    }
                }
            }
            let _ = mailbox
                .send(HubMessage::SyncDone(SyncDone { stale, retry_after: None, reply }))
                .await;
        });
    }

    pub(super) fn finish_sync(&mut self, done: SyncDone) {
        let mut removed = 0;
        let mut failed = 0;
        for key in done.stale {
            // A probe answer is only a decision when the topic is still unbound and free;
            // work may have arrived while the walk was running.
            if !self.topics.contains_key(&key)
                || self.topic_unavailable(key)
                || self.check_deletion(key).is_err()
            {
                continue;
            }
            // Mark deleted before cleanup, as finish_deletion does: the inbound gate drops
            // delayed TopicCreated/text for a topic the walk proved missing, and a cleanup
            // that failed on disk stays retryable through the Delete button.
            self.deleted.insert(key);
            if let Err(error) = self.clean_deleted_topic(key) {
                failed += 1;
                tracing::warn!(topic = ?key, %error, "synced topic could not be cleaned on disk");
            } else {
                removed += 1;
            }
        }
        // A stale topic left on disk is not "all topics are current"; report the failure the
        // way deletion does: what broke and how to retry.
        let disk_failure = (failed > 0).then(|| {
            format!("Не удалось очистить {failed} тем на диске — повторите синхронизацию.")
        });
        let result = match (done.retry_after, disk_failure) {
            (Some(retry_after), disk_failure) => {
                let error = SendError::RetryAfter(retry_after);
                let message = format!("{error}, повторите позже");
                Err(match disk_failure {
                    Some(disk_failure) => format!("{message}. {disk_failure}"),
                    None => message,
                })
            }
            (None, Some(disk_failure)) => Err(match removed {
                0 => disk_failure,
                removed => format!("Удалено устаревших тем: {removed}. {disk_failure}"),
            }),
            (None, None) if removed == 0 => Ok("Все темы актуальны".to_owned()),
            (None, None) => Ok(format!("Удалено устаревших тем: {removed}")),
        };
        let _ = done.reply.send(result);
    }
}
