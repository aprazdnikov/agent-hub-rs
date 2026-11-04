//! Durable delivery progress; transitions are applied by the serial results store.

use crate::{
    domain::{ChatId, TopicKey},
    results::SavedResult,
};
use serde::{Deserialize, Serialize};
use std::num::NonZeroUsize;

pub const MAX_PENDING: usize = 50;
/// A bot-wide server cooldown outlives deletion of the triggering answer.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub entries: Vec<Pending>,
    pub not_before: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Cooldown {
    pub chat: i64,
    pub until: i64,
}

pub const MAX_BYTES: usize = 8 * 1024 * 1024;
pub const RETENTION_SECONDS: i64 = 7 * 24 * 60 * 60;
const MAX_FAILURES: u8 = 8;
const LEASE: i64 = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    Queued,
    SavedOnly,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    Html,
    Plain,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chunk {
    pub text: String,
    pub format: Format,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reason {
    Exhausted,
    Rejected,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Waiting,
    Leased,
    Blocked(Reason),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pending {
    result: SavedResult,
    chunks: Vec<Chunk>,
    next: usize,
    failures: u8,
    revision: u32,
    due: i64,
    state: State,
}
#[derive(Clone, Copy, Debug)]
pub enum Failure {
    Transient,
    RetryAfter(u64),
    Permanent,
}
#[derive(Clone, Copy, Debug)]
pub enum Action {
    Claim,
    Ack,
    Fail(Failure),
    Plain,
    Retry,
    Drop,
}
#[derive(Clone, Debug)]
pub struct Change {
    pub key: TopicKey,
    pub run: String,
    pub revision: u32,
    pub action: Action,
}
impl Change {
    #[must_use]
    pub fn new(pending: &Pending, action: Action) -> Self {
        Self {
            key: pending.result.key(),
            run: pending.result.run().to_owned(),
            revision: pending.revision,
            action,
        }
    }
}
impl Pending {
    pub fn new(result: SavedResult, completion: &str) -> Result<Self, String> {
        result.validate()?;
        if completion.len() > 4096 {
            return Err("Слишком длинное завершение ответа".to_owned());
        }
        let mut bodies = crate::markdown::markdown_to_html_chunks(
            result.text(),
            NonZeroUsize::MIN.saturating_add(3799),
        );
        if !completion.is_empty() {
            let footer = crate::escape::escape(completion);
            if let Some(last) = bodies
                .last_mut()
                .filter(|last| last.chars().count() + footer.chars().count() + 2 <= 3800)
            {
                last.push_str("\n\n");
                last.push_str(&footer);
            } else {
                bodies.extend(crate::markdown::markdown_to_html_chunks(
                    completion,
                    NonZeroUsize::MIN.saturating_add(3799),
                ));
            }
        }
        let total = bodies.len();
        let chunks = bodies
            .into_iter()
            .enumerate()
            .map(|(index, body)| Chunk {
                text: format!("📨 Ход {} · часть {}/{total}\n{body}", result.run(), index + 1),
                format: Format::Html,
            })
            .collect();
        let pending = Self {
            due: result.completed_at(),
            result,
            chunks,
            next: 0,
            failures: 0,
            revision: 0,
            state: State::Waiting,
        };
        pending.validate()?;
        Ok(pending)
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.result.validate()?;
        if self.chunks.is_empty()
            || self.chunks.len() > 256
            || self.next >= self.chunks.len()
            || self.failures > MAX_FAILURES
            || self.due < 0
            || self.chunks.iter().any(|chunk| {
                chunk.text.is_empty()
                    || chunk.text.len() > 64 * 1024
                    || chunk.text.chars().count() > 4096
            })
            || self.bytes() > MAX_BYTES
        {
            return Err("Некорректная запись очереди доставки".to_owned());
        }
        Ok(())
    }
    pub(crate) fn bytes(&self) -> usize {
        self.result.text().len()
            + self.result.cwd().len()
            + self.chunks.iter().map(|chunk| chunk.text.len()).sum::<usize>()
            + 8192
    }
    #[must_use]
    pub fn result(&self) -> &SavedResult {
        &self.result
    }
    #[must_use]
    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }
    #[must_use]
    pub fn chunk(&self) -> Option<&Chunk> {
        self.chunks.get(self.next)
    }
    #[must_use]
    pub fn next(&self) -> usize {
        self.next
    }
    #[must_use]
    pub fn total(&self) -> usize {
        self.chunks.len()
    }
    #[must_use]
    pub fn failures(&self) -> u8 {
        self.failures
    }
    #[must_use]
    pub fn revision(&self) -> u32 {
        self.revision
    }
    #[must_use]
    pub fn due(&self) -> i64 {
        self.due
    }
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }
    #[must_use]
    pub fn ready(&self, chat: ChatId, now: i64) -> bool {
        self.result.key().chat == chat
            && !matches!(self.state, State::Blocked(_))
            && self.due <= now
    }
    #[must_use]
    pub fn editable(&self, now: i64) -> bool {
        self.state != State::Leased || self.due <= now
    }

    /// Returns true when the entry is complete or explicitly removed.
    pub(crate) fn apply(&mut self, change: &Change, now: i64) -> Result<bool, String> {
        if self.result.key() != change.key
            || self.result.run() != change.run
            || self.revision != change.revision
        {
            return Err("Доставка уже изменилась; обновите /outbox".to_owned());
        }
        let revision = self.revision.checked_add(1).ok_or("Исчерпаны версии доставки")?;
        let mut remove = false;
        match change.action {
            Action::Claim if self.ready(change.key.chat, now) => {
                if self.failures >= MAX_FAILURES {
                    self.state = State::Blocked(Reason::Exhausted);
                } else {
                    self.failures += 1;
                    self.state = State::Leased;
                    self.due = now.saturating_add(LEASE);
                }
            }
            Action::Ack if self.state == State::Leased => {
                self.next += 1;
                self.failures = 0;
                self.due = now;
                self.state = State::Waiting;
                remove = self.next == self.chunks.len();
            }
            Action::Fail(failure) if self.state == State::Leased => {
                self.state = match failure {
                    Failure::Permanent => State::Blocked(Reason::Rejected),
                    Failure::Transient | Failure::RetryAfter(_)
                        if self.failures >= MAX_FAILURES =>
                    {
                        State::Blocked(Reason::Exhausted)
                    }
                    Failure::Transient | Failure::RetryAfter(_) => State::Waiting,
                };
                let wait = match failure {
                    Failure::RetryAfter(seconds) => i64::try_from(seconds).unwrap_or(i64::MAX),
                    Failure::Transient => match self.failures {
                        0 | 1 => 5,
                        2 => 15,
                        3 => 30,
                        4 => 60,
                        5 => 120,
                        6 => 300,
                        _ => 600,
                    },
                    Failure::Permanent => 0,
                };
                self.due = now.saturating_add(wait);
            }
            Action::Plain if self.state == State::Leased => {
                let chunk = self.chunks.get_mut(self.next).ok_or("Некорректная часть ответа")?;
                if chunk.format != Format::Html {
                    return Err("Ответ уже без форматирования".to_owned());
                }
                chunk.text = crate::escape::plain(&chunk.text);
                chunk.format = Format::Plain;
            }
            Action::Retry if matches!(self.state, State::Blocked(_)) => {
                self.state = State::Waiting;
                self.failures = 0;
                self.due = now;
            }
            Action::Drop if self.editable(now) => remove = true,
            _ => return Err("Доставка сейчас отправляется или действие уже неактуально".to_owned()),
        }
        self.revision = revision;
        Ok(remove)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{AbsolutePath, BackendKind, ChatId, SessionId, ThreadId, TopicKey, TopicSession},
        results::{Results, SavedResult},
    };

    fn record(id: u32, text: &str) -> SavedResult {
        SavedResult::new(
            TopicKey { chat: ChatId(-100), thread: ThreadId(1) },
            format!("{id:032x}"),
            100,
            &TopicSession::fresh(
                BackendKind::Codex,
                AbsolutePath::new(std::env::temp_dir()).unwrap(),
            )
            .with_session(SessionId::parse("s")),
            text.to_owned(),
        )
        .unwrap()
    }
    fn enqueue(book: &mut Results, record: SavedResult, now: i64) -> Result<SaveOutcome, String> {
        book.enqueue(Pending::new(record, "")?, now)
    }
    fn change(book: &mut Results, pending: &Pending, action: Action, now: i64) -> Option<Pending> {
        book.update(&Change::new(pending, action), now).unwrap()
    }
    #[test]
    fn outbox_resumes_only_unacknowledged_parts_and_rejects_stale_updates() {
        let mut book = Results::default();
        assert_eq!(
            enqueue(&mut book, record(1, &"hello ".repeat(1500)), 100).unwrap(),
            SaveOutcome::Queued
        );
        let first = book.pending(ChatId(-100)).remove(0);
        let claim = change(&mut book, &first, Action::Claim, 100).unwrap();
        assert_eq!(claim.state(), State::Leased);
        assert!(book.update(&Change::new(&first, Action::Claim), 100).is_err());
        assert!(book.update(&Change::new(&claim, Action::Drop), 101).is_err());
        let next = change(&mut book, &claim, Action::Ack, 101).unwrap();
        assert_eq!(next.next(), 1);
        let mut book = Results::parse(&book.dump().unwrap(), 102).unwrap();
        let restored = book.pending(ChatId(-100)).remove(0);
        assert_eq!(restored, next);
        assert!(book.pending(ChatId(-200)).is_empty());
        for _ in restored.next()..restored.total() {
            let waiting = book.pending(ChatId(-100)).remove(0);
            let claimed = change(&mut book, &waiting, Action::Claim, 102).unwrap();
            change(&mut book, &claimed, Action::Ack, 102);
        }
        assert!(book.pending(ChatId(-100)).is_empty());
        assert!(book.get(record(1, "text").key()).is_some());
    }
    #[test]
    fn outbox_crashes_are_counted_and_manual_retry_preserves_progress() {
        let mut book = Results::default();
        enqueue(&mut book, record(1, "text"), 100).unwrap();
        for attempt in 0..8 {
            let waiting = book.pending(ChatId(-100)).remove(0);
            let claimed = change(&mut book, &waiting, Action::Claim, 100 + attempt * 60).unwrap();
            assert_eq!(claimed.failures(), u8::try_from(attempt + 1).unwrap());
            book = Results::parse(&book.dump().unwrap(), 100 + attempt * 60).unwrap();
        }
        let leased = book.pending(ChatId(-100)).remove(0);
        let blocked = change(&mut book, &leased, Action::Claim, 580).unwrap();
        assert_eq!(blocked.state(), State::Blocked(Reason::Exhausted));
        let retry = change(&mut book, &blocked, Action::Retry, 580).unwrap();
        assert_eq!(retry.failures(), 0);
        assert!(book.update(&Change::new(&blocked, Action::Retry), 580).is_err());
        change(&mut book, &retry, Action::Drop, 580);
        assert!(book.pending(ChatId(-100)).is_empty());
    }
    #[test]
    fn outbox_backoff_and_plain_fallback_are_durable() {
        let mut book = Results::default();
        enqueue(&mut book, record(1, "**text**"), 100).unwrap();
        let waiting = book.pending(ChatId(-100)).remove(0);
        let claim = change(&mut book, &waiting, Action::Claim, 100).unwrap();
        let failed = change(&mut book, &claim, Action::Fail(Failure::Transient), 100).unwrap();
        assert_eq!(failed.due(), 105);
        assert!(book.update(&Change::new(&failed, Action::Claim), 104).is_err());
        let claim = change(&mut book, &failed, Action::Claim, 105).unwrap();
        let plain = change(&mut book, &claim, Action::Plain, 105).unwrap();
        assert_eq!(plain.chunk().unwrap().format, Format::Plain);
        assert!(!plain.chunk().unwrap().text.contains("<b>"));
        let failed =
            change(&mut book, &plain, Action::Fail(Failure::RetryAfter(3600)), 105).unwrap();
        assert_eq!(failed.due(), 3705);
        let claim = change(&mut book, &failed, Action::Claim, 3705).unwrap();
        let failed = change(&mut book, &claim, Action::Fail(Failure::Permanent), 3705).unwrap();
        assert_eq!(failed.state(), State::Blocked(Reason::Rejected));
    }
    #[test]
    fn outbox_full_keeps_last_and_never_evicts_undelivered_answers() {
        let mut book = Results::default();
        for id in 0..MAX_PENDING {
            assert_eq!(
                enqueue(&mut book, record(u32::try_from(id).unwrap(), "text"), 100).unwrap(),
                SaveOutcome::Queued
            );
        }
        assert_eq!(
            enqueue(&mut book, record(1000, "latest"), 100).unwrap(),
            SaveOutcome::SavedOnly
        );
        assert_eq!(book.pending(ChatId(-100)).len(), MAX_PENDING);
        assert_eq!(book.get(record(1, "text").key()).unwrap().text(), "latest");
        let expired = Results::parse(&book.dump().unwrap(), 100 + RETENTION_SECONDS).unwrap();
        assert!(expired.pending(ChatId(-100)).is_empty());
        assert!(expired.get(record(1, "text").key()).is_some());
    }
    #[test]
    fn outbox_server_cooldown_survives_retry_drop_new_answers_and_restart() {
        let mut book = Results::default();
        enqueue(&mut book, record(1, "limited"), 100).unwrap();
        for attempt in 0..8 {
            let waiting = book.pending(ChatId(-100)).remove(0);
            let claimed = change(&mut book, &waiting, Action::Claim, 100 + attempt * 1000).unwrap();
            let failure = if attempt == 7 { Failure::RetryAfter(3600) } else { Failure::Transient };
            change(&mut book, &claimed, Action::Fail(failure), 100 + attempt * 1000);
        }
        let blocked = book.pending(ChatId(-100)).remove(0);
        let retry = change(&mut book, &blocked, Action::Retry, 7100).unwrap();
        assert!(book.update(&Change::new(&retry, Action::Claim), 7100).is_err());
        change(&mut book, &retry, Action::Drop, 7100);
        enqueue(&mut book, record(2, "new"), 7100).unwrap();
        let restored = Results::parse(&book.dump().unwrap(), 7100).unwrap();
        assert_eq!(restored.snapshot(ChatId(-100)).not_before, 10700);
        assert_eq!(restored.snapshot(ChatId(-200)).not_before, 0);
    }
    #[test]
    fn outbox_byte_limit_includes_rendered_payload_and_rejects_corrupt_chunks() {
        let mut book = Results::default();
        let text = "<".repeat(crate::results::MAX_TEXT_BYTES);
        let mut full = false;
        for id in 1..=50 {
            full |= enqueue(&mut book, record(id, &text), 100).unwrap() == SaveOutcome::SavedOnly;
        }
        assert!(full);
        assert!(book.pending(ChatId(-100)).len() < MAX_PENDING);
        let saved = book.dump().unwrap();
        for chunks in [
            serde_json::json!([]),
            serde_json::json!([{ "text": "", "format": "Html" }]),
            serde_json::json!([{ "text": "too long".repeat(10000), "format": "Html" }]),
        ] {
            let mut invalid: serde_json::Value = serde_json::from_str(&saved).unwrap();
            invalid["outbox"][0]["chunks"] = chunks;
            assert!(Results::parse(&invalid.to_string(), 100).is_err());
        }
        let mut duplicate: serde_json::Value = serde_json::from_str(&saved).unwrap();
        let entry = duplicate["outbox"][0].clone();
        duplicate["outbox"][1] = entry;
        assert!(Results::parse(&duplicate.to_string(), 100).is_err());
    }
    #[test]
    fn outbox_v1_does_not_enqueue_old_answers_and_invalid_progress_is_rejected() {
        let mut book = Results::default();
        book.save(record(1, "old"), 100);
        let mut v1: serde_json::Value = serde_json::from_str(&book.dump().unwrap()).unwrap();
        v1["version"] = 1.into();
        v1.as_object_mut().unwrap().remove("outbox");
        v1.as_object_mut().unwrap().remove("cooldowns");
        assert!(Results::parse(&v1.to_string(), 100).unwrap().pending(ChatId(-100)).is_empty());
        enqueue(&mut book, record(2, "new"), 100).unwrap();
        let mut invalid: serde_json::Value = serde_json::from_str(&book.dump().unwrap()).unwrap();
        invalid["outbox"][0]["next"] = 999.into();
        assert!(Results::parse(&invalid.to_string(), 100).is_err());
    }
}
