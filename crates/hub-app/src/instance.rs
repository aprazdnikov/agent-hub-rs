//! One running copy per user: a second one would poll the same bot and fight over topics.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum InstanceError {
    #[error("agent-hub уже запущен")]
    Running,
    #[error("не удалось открыть {}: {error}", path.display())]
    Io { path: PathBuf, error: io::Error },
}

pub struct InstanceLock {
    file: File,
}

impl InstanceLock {
    pub fn acquire(path: &Path) -> Result<Self, InstanceError> {
        let failed = |error| InstanceError::Io { path: path.to_path_buf(), error };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(failed)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(failed)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file }),
            Err(TryLockError::WouldBlock) => Err(InstanceError::Running),
            Err(TryLockError::Error(error)) => Err(failed(error)),
        }
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        // The OS releases the lock with the handle anyway; unlocking first only makes it prompt.
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lock_is_refused_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data").join("agent-hub.lock");
        let first = InstanceLock::acquire(&path).unwrap();
        assert!(matches!(InstanceLock::acquire(&path), Err(InstanceError::Running)));
        drop(first);
        assert!(InstanceLock::acquire(&path).is_ok());
    }
}
