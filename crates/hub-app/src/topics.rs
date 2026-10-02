//! `topics.json`, in the format shared with the Python version.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hub_core::topics::{Topics, dump_state, parse_state};
use hub_telegram::hub::TopicStore;

use crate::atomic::write_atomic;
use crate::error::StoreError;

pub fn load_topics(path: &Path) -> Result<Topics, StoreError> {
    match fs::read_to_string(path) {
        Ok(raw) => parse_state(&raw).map_err(|error| StoreError::Corrupt {
            path: path.to_path_buf(),
            reason: error.to_string(),
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Topics::new()),
        Err(error) => Err(StoreError::Read { path: path.to_path_buf(), error }),
    }
}

pub struct FileTopics {
    path: PathBuf,
}

impl FileTopics {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl TopicStore for FileTopics {
    fn save(&mut self, topics: &Topics) -> Result<(), String> {
        let text = dump_state(topics).map_err(|error| error.to_string())?;
        write_atomic(&self.path, text.as_bytes())
            .map_err(|error| format!("{}: {error}", self.path.display()))
    }
}

#[cfg(test)]
mod tests {
    use hub_core::domain::{
        AbsolutePath, BackendKind, ChatId, SessionId, ThreadId, TopicKey, TopicSession,
    };

    use super::*;

    #[test]
    fn missing_file_means_no_topics() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_topics(&dir.path().join("topics.json")).unwrap().is_empty());
    }

    #[test]
    fn saved_topics_load_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        let cwd = AbsolutePath::new(dir.path().to_path_buf()).unwrap();
        let topics = Topics::from([(
            TopicKey { chat: ChatId(-100), thread: ThreadId(7) },
            TopicSession::fresh(BackendKind::Claude, cwd).with_session(SessionId::parse("s-1")),
        )]);
        FileTopics::new(path.clone()).save(&topics).unwrap();
        assert_eq!(load_topics(&path).unwrap(), topics);
    }

    #[test]
    fn corrupt_topics_are_reported_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        std::fs::write(&path, "{\"version\": 9, \"topics\": []}").unwrap();
        let error = load_topics(&path).unwrap_err();
        assert!(error.to_string().contains("topics.json"));
        assert!(error.to_string().contains("версия"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"version\": 9, \"topics\": []}");
    }
}
