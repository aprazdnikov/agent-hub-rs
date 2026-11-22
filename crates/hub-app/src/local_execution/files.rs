//! Keep an immutable copy independently of the project and its worktrees.
use hub_core::{
    attachments::MAX_SEND_BYTES,
    domain::{AbsolutePath, OutgoingFile},
    tasks::{
        ResultFile,
        service::{Access, TaskService},
    },
};
use std::{
    fs,
    io::{self, Read},
    path::Path,
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

pub(super) struct FileTarget {
    pub service: Arc<TaskService>,
    pub cwd: AbsolutePath,
    pub task: u64,
    pub run: u32,
}

impl FileTarget {
    pub fn deliver(
        &self,
        root: &Path,
        outgoing: &OutgoingFile,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        if cancel.is_cancelled() {
            return Err("Запуск остановлен".into());
        }
        if outgoing.caption.len() > 4096 {
            return Err("Подпись файла превышает 4 КиБ".into());
        }
        let source = hub_telegram::paths::outgoing_path(
            &self.cwd,
            &crate::gui::home_or_root(),
            &outgoing.path,
            MAX_SEND_BYTES,
        )
        .map_err(|e| e.to_string())?;
        let name = source.file_name().and_then(|s| s.to_str()).ok_or("Некорректное имя файла")?;
        fs::create_dir_all(root).map_err(|e| e.to_string())?;
        let key = uuid::Uuid::new_v4().simple().to_string();
        let dir = root.join(&key);
        fs::create_dir(&dir).map_err(|e| e.to_string())?;
        let result = (|| {
            let path = dir.join(name);
            let mut copy = tempfile::NamedTempFile::new_in(&dir).map_err(|e| e.to_string())?;
            let source = fs::File::open(&source).map_err(|e| e.to_string())?;
            let bytes = io::copy(&mut source.take(MAX_SEND_BYTES + 1), &mut copy)
                .map_err(|e| e.to_string())?;
            if bytes > MAX_SEND_BYTES {
                return Err("Файл превышает допустимый размер".into());
            }
            copy.as_file().sync_all().map_err(|e| e.to_string())?;
            copy.persist_noclobber(path).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            for folder in [dir.as_path(), root] {
                fs::File::open(folder).and_then(|f| f.sync_all()).map_err(|e| e.to_string())?;
            }
            self.service.access(Access::Internal, |book, _| {
                if cancel.is_cancelled() {
                    return Err("Запуск остановлен".into());
                }
                book.record_file(
                    self.task,
                    self.run,
                    ResultFile { key, name: name.into(), caption: outgoing.caption.clone(), bytes },
                )
            })
        })();
        if result.is_err() && self.service.read().is_ok() {
            let _ = fs::remove_dir_all(dir);
        }
        result
    }
}
