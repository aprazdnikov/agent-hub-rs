//! Desktop-initiated deletion and topic synchronization. Reserve the topic until Telegram
//! answers.
use std::time::Duration;

use hub_core::tasks::{queue::Origin, service::Access};

use crate::messenger::SendError;

use super::{Agents, Hub, HubMessage, Target, TopicKey, oneshot};

pub struct DeletionDone {
    key: TopicKey,
    result: Result<(), String>,
}

/// The categorized results of one synchronization walk, handed back to the actor that started it.
pub struct SyncDone {
    /// Topics the probes proved missing, in probe order.
    stale: Vec<TopicKey>,
    /// Topics the probes found still present.
    alive: usize,
    /// Topics whose probe answer could not be trusted (rejected/failed, not rate-limited).
    uncertain: usize,
    /// Topics the actor skipped before the walk because they were busy.
    busy: usize,
    /// The walk stopped early because Telegram asked to wait this long.
    retry_after: Option<Duration>,
    reply: oneshot::Sender<Result<String, String>>,
}

/// Renders the synchronization report from the category counts.
///
/// Two single-purpose messages stay terse; anything mixed lists only the non-empty categories.
/// The rate-limit and disk-failure tails are added by the caller, not here.
fn sync_summary(
    removed: usize,
    alive: usize,
    busy: usize,
    uncertain: usize,
    detached: usize,
) -> String {
    // Detached-task reporting is a suffix that only ever accompanies removals.
    let detach_note = (detached > 0).then(|| format!("Задач откреплено: {detached}"));
    if busy == 0 && uncertain == 0 && (removed == 0 || alive == 0) {
        let base = match removed {
            0 => "Все темы актуальны.".to_owned(),
            removed => format!("Удалено устаревших тем: {removed}."),
        };
        return match detach_note {
            Some(note) => format!("{base} {note}"),
            None => base,
        };
    }
    let mut parts = Vec::new();
    if removed > 0 {
        parts.push(format!("Удалено: {removed}"));
    }
    if alive > 0 {
        parts.push(format!("Живых: {alive}"));
    }
    if busy > 0 {
        parts.push(format!("Пропущено занятых: {busy}"));
    }
    if uncertain > 0 {
        parts.push(format!("Не удалось проверить: {uncertain} — повторите синхронизацию"));
    }
    if let Some(note) = detach_note {
        parts.push(note);
    }
    format!("{}.", parts.join(". "))
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

    /// An in-flight launch, session reservation or destructive operation on the topic. These must
    /// finish before the topic is touched at all, so they block even the forced cleanup of a topic
    /// Telegram already proved missing.
    fn active_operation(&self, key: TopicKey) -> bool {
        self.deleting.contains_key(&key)
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
            || self.topics.get(&key).is_some_and(|s| self.is_reserved(s))
    }

    /// Auxiliary per-topic work (memory, voice, actions, doctor, album parts). It blocks a manual
    /// deletion, but not the cleanup of a topic proved missing — such a topic cannot take part in
    /// these operations meaningfully once it is gone remotely.
    fn auxiliary_busy(&self, key: TopicKey) -> bool {
        self.memory.busy_in(key)
            || self.voices.busy_in(key)
            || self.actions.busy_in(Target::Topic(key))
            || self.doctors.busy(key)
            || self.albums.values().flatten().any(|part| part.key() == Some(key))
    }

    /// Blocks cleanup only on genuinely active states. A proven-missing topic whose task queue
    /// still holds its intents is force-cleaned instead: the queue is detached rather than being a
    /// permanent block.
    fn check_force_cleanup(&self, key: TopicKey) -> Result<(), ()> {
        if self.active_operation(key) { Err(()) } else { Ok(()) }
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
        if self.active_operation(key) || self.auxiliary_busy(key) {
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

impl<A: Agents> Hub<A> {
    pub(super) fn sync_topics(&mut self, reply: oneshot::Sender<Result<String, String>>) {
        if self.shutdown.is_some() || self.settings.borrow().telegram != self.telegram_identity {
            let _ = reply.send(Err(
                "Синхронизация недоступна: запустите бота с сохранёнными настройками Telegram."
                    .into(),
            ));
            return;
        }
        // Eligible topics: a forum thread in the configured chat that is not already gone.
        let eligible: Vec<TopicKey> = self
            .topics
            .keys()
            .copied()
            .filter(|key| {
                key.thread.0 > 1
                    && key.chat == self.telegram_identity.chat
                    && !self.topic_unavailable(*key)
            })
            .collect();
        // A proven-missing topic must be reachable even when its task queue still holds intents,
        // so only genuinely active work (check_force_cleanup) skips a candidate; a queued-only topic
        // is probed and, if gone, cleaned with its queue detached. Anything skipped is reported as
        // busy rather than silently dropped from the count.
        let mut candidates = Vec::new();
        let mut busy = 0;
        for key in eligible {
            if self.check_force_cleanup(key).is_ok() {
                candidates.push(key);
            } else {
                busy += 1;
                tracing::debug!(topic = ?key, category = "busy", "sync skipped a busy topic");
            }
        }
        if candidates.is_empty() {
            let _ = reply.send(Ok(sync_summary(0, 0, busy, 0, 0)));
            return;
        }
        let sender = self.sender.clone();
        let mailbox = self.mailbox.clone();
        let settings = self.settings.clone();
        let identity = self.telegram_identity.clone();
        tokio::spawn(async move {
            let mut stale = Vec::new();
            let mut alive = 0;
            let mut uncertain = 0;
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
                    Ok(true) => alive += 1,
                    // An uncertain answer must not drop local state: skip the topic and
                    // keep checking the rest, but report it so the user knows it was not verified.
                    Err(SendError::Rejected(_) | SendError::Failed(_)) => {
                        uncertain += 1;
                        tracing::debug!(
                            topic = ?key,
                            category = "uncertain",
                            "sync could not verify a topic"
                        );
                    }
                    Err(SendError::RetryAfter(retry_after)) => {
                        // Telegram rate-limits the walk itself; stop instead of hammering.
                        let _ = mailbox
                            .send(HubMessage::SyncDone(SyncDone {
                                stale,
                                alive,
                                uncertain,
                                busy,
                                retry_after: Some(retry_after),
                                reply,
                            }))
                            .await;
                        return;
                    }
                }
            }
            let _ = mailbox
                .send(HubMessage::SyncDone(SyncDone {
                    stale,
                    alive,
                    uncertain,
                    busy,
                    retry_after: None,
                    reply,
                }))
                .await;
        });
    }

    pub(super) fn finish_sync(&mut self, done: SyncDone) {
        let mut removed = 0;
        let mut failed = 0;
        let mut detached = 0;
        let mut skipped = 0;
        for key in done.stale {
            // A probe answer is only a decision when the topic is still unbound and free; an active
            // operation may have started while the walk was running. A queued-only topic stays
            // eligible here: its queue is detached below instead of blocking the proven removal.
            if !self.topics.contains_key(&key) {
                // Already cleaned by another path; local state matches Telegram, nothing to report.
                continue;
            }
            if self.topic_unavailable(key) || self.check_force_cleanup(key).is_err() {
                // The row still sits on disk for a topic Telegram proved gone. It must reach the
                // busy count, or the report would claim "Все темы актуальны" over a real deferral.
                skipped += 1;
                tracing::debug!(
                    topic = ?key,
                    category = "skipped",
                    "sync deferred a proven-missing topic"
                );
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
                // Unbind the topic's queued tasks so they survive as detached work. Only the queue
                // origin changes; task status is preserved.
                let origin = Origin { chat: key.chat.0, thread: key.thread.0, user: None };
                match self
                    .tasks
                    .access(Access::Edit, |book, _| Ok(book.detach_topic_queue(&origin)))
                {
                    Ok(count) => detached += count,
                    Err(error) => tracing::warn!(
                        topic = ?key,
                        %error,
                        "synced topic queue could not be detached"
                    ),
                }
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
            // Topics that turned busy or unavailable after their probe join the pre-walk
            // busy count: both are deferrals the user has to see.
            (None, None) => {
                Ok(sync_summary(removed, done.alive, done.busy + skipped, done.uncertain, detached))
            }
        };
        let _ = done.reply.send(result);
    }
}
