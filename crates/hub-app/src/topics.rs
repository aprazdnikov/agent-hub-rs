//! `topics.json`, in the format shared with the Python version.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hub_core::topics::{Titles, Topics, dump_state, dump_titles, parse_state, parse_titles};
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

/// The sidecar is optional, so existing installations need no migration.
pub fn load_titles(topics_path: &Path) -> Result<Titles, StoreError> {
    let path = topics_path.with_extension("titles.json");
    match fs::read_to_string(&path) {
        Ok(raw) => parse_titles(&raw)
            .map_err(|error| StoreError::Corrupt { path, reason: error.to_string() }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Titles::new()),
        Err(error) => Err(StoreError::Read { path, error }),
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
    fn save_titles(&mut self, titles: &Titles) -> Result<(), String> {
        let path = self.path.with_extension("titles.json");
        let text = dump_titles(titles).map_err(|error| error.to_string())?;
        write_atomic(&path, text.as_bytes()).map_err(|error| format!("{}: {error}", path.display()))
    }

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
        assert!(load_titles(&dir.path().join("topics.json")).unwrap().is_empty());
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
        assert!(load_titles(&path).unwrap().is_empty(), "old installations have no title sidecar");
        let bindings = fs::read(&path).unwrap();
        let mut store = FileTopics::new(path.clone());
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        for name in ["Старое имя", "Новое имя 🦀"] {
            let titles = Titles::from([(key, name.to_owned())]);
            store.save_titles(&titles).unwrap();
            assert_eq!(load_titles(&path).unwrap(), titles);
            assert_eq!(fs::read(&path).unwrap(), bindings, "rename must not rewrite session state");
        }
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            2,
            "atomic writes leave no temporary files"
        );
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

    #[test]
    fn corrupt_title_sidecar_is_reported_without_overwriting_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        let sidecar = path.with_extension("titles.json");
        for raw in ["bad json", r#"{"version": 99, "titles": []}"#] {
            fs::write(&sidecar, raw).unwrap();
            let error = load_titles(&path).unwrap_err();
            assert!(error.to_string().contains("topics.titles.json"));
            assert_eq!(fs::read_to_string(&sidecar).unwrap(), raw);
        }
    }

    #[test]
    fn title_write_error_does_not_damage_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        let mut store = FileTopics::new(path.clone());
        store.save(&Topics::new()).unwrap();
        let before = fs::read(&path).unwrap();
        fs::create_dir(path.with_extension("titles.json")).unwrap();
        assert!(store.save_titles(&Titles::new()).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}
