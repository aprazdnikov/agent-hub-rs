//! Bounded last successful text results, independent of session bindings.

use crate::domain::{BackendKind, ChatId, ThreadId, TopicKey, TopicSession};
use serde::{Deserialize, Serialize};

pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_RESULTS: usize = 100;
pub const RETENTION_SECONDS: i64 = 30 * 24 * 60 * 60;
pub const MAX_FILE_BYTES: usize = MAX_RESULTS * (MAX_TEXT_BYTES + 8192 + 128) * 6 + 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedResult {
    chat: i64,
    thread: i32,
    run: String,
    completed_at: i64,
    backend: String,
    cwd: String,
    session: String,
    text: String,
}
impl SavedResult {
    pub fn new(
        key: TopicKey,
        run: String,
        completed_at: i64,
        session: &TopicSession,
        text: String,
    ) -> Result<Self, String> {
        let record = Self {
            chat: key.chat.0,
            thread: key.thread.0,
            run,
            completed_at,
            backend: session.backend.name().to_owned(),
            cwd: session.cwd.as_path().display().to_string(),
            session: session.session.as_ref().map(|id| id.as_str().to_owned()).unwrap_or_default(),
            text,
        };
        record.validate()?;
        Ok(record)
    }
    fn validate(&self) -> Result<(), String> {
        if self.chat == 0
            || self.thread <= 0
            || self.run.len() != 32
            || !self.run.bytes().all(|b| b.is_ascii_hexdigit())
            || self.completed_at < 0
            || chrono::DateTime::from_timestamp(self.completed_at, 0).is_none()
            || BackendKind::parse(&self.backend).is_none()
            || self.cwd.is_empty()
            || self.cwd.len() > 4096
            || self.session.is_empty()
            || self.session.len() > 4096
            || self.text.trim().is_empty()
            || self.text.len() > MAX_TEXT_BYTES
        {
            return Err("Некорректная запись результата".to_owned());
        }
        Ok(())
    }
    #[must_use]
    pub fn key(&self) -> TopicKey {
        TopicKey { chat: ChatId(self.chat), thread: ThreadId(self.thread) }
    }
    #[must_use]
    pub fn run(&self) -> &str {
        &self.run
    }
    #[must_use]
    pub fn completed_at(&self) -> i64 {
        self.completed_at
    }
    #[must_use]
    pub fn backend(&self) -> &str {
        &self.backend
    }
    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Default)]
pub struct Results {
    records: Vec<SavedResult>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    results: Vec<SavedResult>,
}
impl Results {
    pub fn parse(raw: &str, now: i64) -> Result<Self, String> {
        if raw.len() > MAX_FILE_BYTES {
            return Err("Файл результатов превышает допустимый размер".to_owned());
        }
        let file: File =
            serde_json::from_str(raw).map_err(|_| "Повреждённый файл результатов".to_owned())?;
        if file.version != 1 || file.results.len() > MAX_RESULTS {
            return Err("Некорректная версия или число результатов".to_owned());
        }
        let mut keys = std::collections::HashSet::new();
        for record in &file.results {
            record.validate()?;
            if !keys.insert(record.key()) {
                return Err("Повтор темы в файле результатов".to_owned());
            }
        }
        let mut book = Self { records: file.results };
        book.prune(now);
        Ok(book)
    }
    fn prune(&mut self, now: i64) {
        self.records.retain(|record| now.saturating_sub(record.completed_at) < RETENTION_SECONDS);
    }
    pub fn dump(&self) -> Result<String, String> {
        serde_json::to_string(&File { version: 1, results: self.records.clone() })
            .map_err(|_| "Не удалось сериализовать результаты".to_owned())
    }
    pub fn save(&mut self, record: SavedResult, now: i64) {
        self.prune(now);
        self.records.retain(|saved| saved.key() != record.key());
        self.records.insert(0, record);
        self.records.sort_by_key(|record| std::cmp::Reverse(record.completed_at));
        self.records.truncate(MAX_RESULTS);
    }
    #[must_use]
    pub fn get(&self, key: TopicKey) -> Option<&SavedResult> {
        self.records.iter().find(|record| record.key() == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AbsolutePath, SessionId};
    fn key(thread: i32) -> TopicKey {
        TopicKey { chat: ChatId(-100), thread: ThreadId(thread) }
    }
    fn result(thread: i32, time: i64, text: &str) -> SavedResult {
        let session = TopicSession::fresh(
            BackendKind::Codex,
            AbsolutePath::new(std::env::temp_dir()).unwrap(),
        )
        .with_session(SessionId::parse("session"));
        SavedResult::new(key(thread), "a".repeat(32), time, &session, text.to_owned()).unwrap()
    }
    #[test]
    fn last_results_round_trip_and_replace_only_the_same_topic() {
        let mut book = Results::default();
        book.save(result(1, 10, "old"), 10);
        book.save(result(2, 20, "other topic"), 20);
        book.save(result(1, 30, "**new**"), 30);
        let restored = Results::parse(&book.dump().unwrap(), 30).unwrap();
        assert_eq!(restored.get(key(1)).unwrap().text(), "**new**");
        assert_eq!(restored.get(key(2)).unwrap().text(), "other topic");
        assert!(restored.get(TopicKey { chat: ChatId(-200), ..key(1) }).is_none());
    }
    #[test]
    fn last_results_have_age_and_count_limits() {
        let mut book = Results::default();
        for thread in 1..=101 {
            book.save(result(thread, i64::from(thread), "text"), 101);
        }
        assert!(book.get(key(1)).is_none());
        assert!(book.get(key(101)).is_some());
        let expired = Results::parse(&book.dump().unwrap(), RETENTION_SECONDS + 101).unwrap();
        assert!(expired.get(key(101)).is_none());
    }
    #[test]
    fn last_results_reject_corruption_duplicates_and_oversized_text() {
        assert!(Results::parse("broken", 0).is_err());
        assert!(Results::parse(r#"{"version":2,"results":[]}"#, 0).is_err());
        let record = result(1, 0, "text");
        let duplicate = serde_json::to_string(&File {
            version: 1,
            results: vec![record.clone(), record.clone()],
        })
        .unwrap();
        assert!(Results::parse(&duplicate, 0).is_err());
        let mut invalid = record;
        invalid.text = "я".repeat(MAX_TEXT_BYTES);
        let raw = serde_json::to_string(&File { version: 1, results: vec![invalid] }).unwrap();
        assert!(Results::parse(&raw, 0).is_err());
    }
}
