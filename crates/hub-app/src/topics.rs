//! `topics.json`, in the format shared with the Python version.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hub_core::projects::RecentProjects;
use hub_core::tasks::service::TaskStorage;
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

pub fn load_recent_projects(topics_path: &Path) -> Result<RecentProjects, StoreError> {
    let path = topics_path.with_extension("projects.json");
    match fs::read_to_string(&path) {
        Ok(raw) => {
            RecentProjects::parse(&raw).map_err(|reason| StoreError::Corrupt { path, reason })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(RecentProjects::default()),
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

impl TaskStorage for FileTopics {
    fn load_tasks(&mut self) -> Result<hub_core::tasks::TaskBook, String> {
        use std::io::Read;
        let path = self.path.with_extension("tasks.json");
        let metadata = match fs::symlink_metadata(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(hub_core::tasks::TaskBook::default());
            }
            Err(error) => return Err(format!("Не удалось прочитать задачи: {error}")),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Файл задач должен быть обычным файлом, а не ссылкой".into());
        }
        let mut bytes = Vec::new();
        fs::File::open(&path)
            .map_err(|e| e.to_string())?
            .take(hub_core::tasks::MAX_READ_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        hub_core::tasks::TaskBook::decode(&bytes)
    }
    fn save_tasks(&mut self, book: &hub_core::tasks::TaskBook) -> Result<(), String> {
        // Refuse to replace a corrupt/unknown sidecar, including external edits since startup.
        let previous = self.load_tasks()?;
        let path = self.path.with_extension("tasks.json");
        write_atomic(&path, &book.encode()?)
            .map_err(|e| format!("Не удалось сохранить задачи: {e}"))?;
        #[cfg(unix)]
        fs::File::open(path.parent().ok_or("Нет каталога задач")?)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| format!("Запись задач могла сохраниться, но её долговечность не подтверждена: {e}. Перезапустите бота перед повтором."))?;
        if previous.tasks().len() > book.tasks().len() {
            let root = self.path.with_file_name("task-files");
            purge_deleted_files(&root, &previous, book);
        }
        Ok(())
    }
}

fn purge_deleted_files(
    root: &Path,
    previous: &hub_core::tasks::TaskBook,
    current: &hub_core::tasks::TaskBook,
) {
    let retained: std::collections::HashSet<_> = current
        .tasks()
        .iter()
        .flat_map(|task| &task.runs)
        .flat_map(|run| &run.files)
        .map(|file| &file.key)
        .collect();
    for file in previous
        .tasks()
        .iter()
        .filter(|task| current.get(task.id).is_err())
        .flat_map(|task| &task.runs)
        .flat_map(|run| &run.files)
        .filter(|file| !retained.contains(&file.key))
    {
        let directory = root.join(&file.key);
        let cleanup = || -> io::Result<()> {
            for dir in [root, directory.as_path()] {
                if !fs::symlink_metadata(dir)?.is_dir() {
                    return Err(io::Error::other("Каталог вложения был заменён"));
                }
            }
            fs::remove_file(directory.join(&file.name))?;
            fs::remove_dir(&directory)
        };
        if let Err(error) = cleanup()
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(%error, path = %directory.display(), "Не удалось удалить сохранённое вложение удалённой задачи");
        }
    }
}

impl TopicStore for FileTopics {
    fn save_recent_projects(&mut self, recent: &RecentProjects) -> Result<(), String> {
        let path = self.path.with_extension("projects.json");
        let text = recent.dump().map_err(|error| error.to_string())?;
        write_atomic(&path, text.as_bytes()).map_err(|error| format!("{}: {error}", path.display()))
    }

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
    fn recent_projects_round_trip_without_changing_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        assert!(load_recent_projects(&path).unwrap().ids().is_empty());
        let mut store = FileTopics::new(path.clone());
        store.save(&Topics::new()).unwrap();
        let before = fs::read(&path).unwrap();
        let mut recent = RecentProjects::default();
        recent.remember("shop");
        store.save_recent_projects(&recent).unwrap();
        assert_eq!(load_recent_projects(&path).unwrap(), recent);
        assert_eq!(fs::read(&path).unwrap(), before);
        let sidecar = path.with_extension("projects.json");
        fs::write(&sidecar, "bad json").unwrap();
        assert!(
            load_recent_projects(&path).unwrap_err().to_string().contains("topics.projects.json")
        );
        assert_eq!(fs::read_to_string(sidecar).unwrap(), "bad json");
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
    #[test]
    fn task_sidecar_survives_reopen_and_refuses_unknown_format() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("topics.json");
        let mut store = FileTopics::new(path.clone());
        let mut tasks = store.load_tasks().unwrap();
        assert!(!path.with_extension("tasks.json").exists());
        let id = tasks.create(root.path().to_path_buf(), "qwen", "Check", "Read README").unwrap();
        tasks.begin(id, 1, None).unwrap();
        store.save_tasks(&tasks).unwrap();
        assert_eq!(FileTopics::new(path.clone()).load_tasks().unwrap(), tasks);
        assert!(!path.exists(), "task state is independent of topic bindings");
        fs::write(path.with_extension("tasks.json"), b"unknown").unwrap();
        assert!(store.load_tasks().is_err());
        assert!(store.save_tasks(&tasks).is_err());
        assert_eq!(fs::read(path.with_extension("tasks.json")).unwrap(), b"unknown");
    }
}
