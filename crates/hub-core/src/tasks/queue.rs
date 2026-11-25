//! Persistent FIFO intents. A claimed intent is never replayed after a restart.
use super::{MAX_PROMPT, Task, TaskBook, TaskStatus, text};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub chat: i64,
    pub thread: i32,
    pub user: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub sequence: u64,
    pub revision: u64,
    pub rules_revision: u64,
    pub next: Option<String>,
    pub isolated: bool,
    pub origin: Option<Origin>,
    /// Filled before side effects. An interrupted preflight requires an explicit requeue.
    pub error: Option<String>,
}
impl Entry {
    pub(super) fn valid(&self) -> bool {
        self.sequence != 0
            && self.revision != 0
            && self.next.as_ref().is_none_or(|step| text(step, MAX_PROMPT))
            && self.error.as_ref().is_none_or(|error| text(error, 2048))
    }
}
impl Task {
    #[must_use]
    pub fn execution_dir(&self) -> PathBuf {
        if self.isolated {
            self.project.join(".agent-hub-worktrees").join(self.id.to_string())
        } else {
            self.project.clone()
        }
    }
}
impl TaskBook {
    pub fn enqueue(
        &mut self,
        id: u64,
        revision: u64,
        next: Option<String>,
        isolated: bool,
        origin: Option<Origin>,
    ) -> Result<(), String> {
        let task = self.get(id)?;
        if task.queue.is_some()
            || task.active()
            || task.revision != revision
            || task.archived
            || matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)
        {
            return Err("Задача изменилась, занята или уже в очереди.".into());
        }
        if !task.runs.is_empty() && isolated != task.isolated {
            return Err("После первого запуска рабочий каталог задачи менять нельзя.".into());
        }
        let rules = self.rules(&task.project);
        if isolated && rules.git.is_some() {
            return Err("Для обязательной подготовки ветки используйте обычную очередь.".into());
        }
        task.preview_with_rules(next.as_deref(), &rules)?;
        let sequence = self
            .tasks()
            .iter()
            .filter_map(|t| t.queue.as_ref().map(|q| q.sequence))
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("Лимит очереди")?;
        let revision = revision.checked_add(1).ok_or("Лимит ревизий")?;
        let task = self.get_mut(id)?;
        task.revision = revision;
        task.queue = Some(Entry {
            sequence,
            revision,
            rules_revision: rules.revision,
            next,
            isolated,
            origin,
            error: None,
        });
        Ok(())
    }
    pub fn unqueue(&mut self, id: u64, revision: u64) -> Result<(), String> {
        let task = self.get_mut(id)?;
        if task.revision != revision || task.active() || task.queue.is_none() {
            return Err("Задача уже запущена, изменена или отсутствует в очереди.".into());
        }
        task.revision = revision.checked_add(1).ok_or("Лимит ревизий")?;
        task.queue = None;
        Ok(())
    }
    pub fn pause_queue(&mut self, id: u64, revision: u64, error: &str) -> Result<(), String> {
        let task = self.get_mut(id)?;
        let q =
            task.queue.as_mut().filter(|q| q.revision == revision).ok_or("Очередь изменилась")?;
        q.error = Some(error.chars().take(480).collect());
        Ok(())
    }
    pub fn claim_queue(&mut self, id: u64, revision: u64) -> Result<(), String> {
        let rules_revision = self.rules(&self.get(id)?.project).revision;
        if self.get(id)?.queue.as_ref().is_none_or(|q| q.rules_revision != rules_revision) {
            return Err(
                "Правила проекта изменились. Снимите задачу с очереди и проверьте новый контекст."
                    .into(),
            );
        }
        let task = self.get_mut(id)?;
        let q = task
            .queue
            .as_mut()
            .filter(|q| q.revision == revision && q.error.is_none() && task.revision == revision)
            .ok_or("Очередь изменилась")?;
        q.error = Some("Запуск принят. Если приложение остановилось, проверьте рабочий каталог и повторно поставьте задачу в очередь.".into());
        task.isolated = q.isolated;
        Ok(())
    }
    #[must_use]
    pub fn next_queued(&self, connected: bool) -> Option<(&Task, &Entry)> {
        self.tasks()
            .iter()
            .filter_map(|t| {
                t.queue
                    .as_ref()
                    .filter(|q| q.error.is_none() && (connected || q.origin.is_none()))
                    .map(|q| (t, q))
            })
            .min_by_key(|(_, q)| q.sequence)
    }
    /// Clears the queue origin of every task bound to a Telegram topic, leaving the queued work
    /// itself intact so it can run detached. Returns the number of entries unbound.
    pub fn detach_topic_queue(&mut self, origin: &Origin) -> usize {
        let mut detached = 0;
        for task in &mut self.tasks {
            // Only the origin is touched; status, revision and sequence are preserved.
            if let Some(entry) = &mut task.queue
                && entry
                    .origin
                    .is_some_and(|bound| bound.chat == origin.chat && bound.thread == origin.thread)
            {
                entry.origin = None;
                detached += 1;
            }
        }
        detached
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bound(chat: i64, thread: i32) -> Origin {
        Origin { chat, thread, user: Some(1) }
    }

    #[test]
    fn detach_topic_queue_clears_only_the_matching_topic_and_keeps_the_rest() {
        let mut book = TaskBook::default();
        let gone = book.create(std::env::temp_dir(), "qwen", "gone", "Inspect").unwrap();
        let other = book.create(std::env::temp_dir(), "qwen", "other", "Inspect").unwrap();
        let unbound = book.create(std::env::temp_dir(), "qwen", "plain", "Inspect").unwrap();
        book.enqueue(gone, 1, None, false, Some(bound(-100, 7))).unwrap();
        book.enqueue(other, book.get(other).unwrap().revision, None, false, Some(bound(-100, 8)))
            .unwrap();
        book.enqueue(unbound, book.get(unbound).unwrap().revision, None, false, None).unwrap();

        assert_eq!(book.detach_topic_queue(&bound(-100, 7)), 1);

        assert_eq!(book.get(gone).unwrap().queue.as_ref().unwrap().origin, None);
        assert_eq!(book.get(other).unwrap().queue.as_ref().unwrap().origin, Some(bound(-100, 8)));
        assert_eq!(book.get(unbound).unwrap().queue.as_ref().unwrap().origin, None);
        // The detached task stays queued and unchanged apart from its origin.
        assert_eq!(book.get(gone).unwrap().queue.as_ref().unwrap().error, None);
        assert!(book.get(other).unwrap().queue.as_ref().is_some());
        // Idempotent: nothing is bound to the topic any more.
        assert_eq!(book.detach_topic_queue(&bound(-100, 7)), 0);
    }
}
