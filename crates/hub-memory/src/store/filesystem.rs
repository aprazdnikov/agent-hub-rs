use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
};

use crate::Error;

use super::DATA_FILE;

#[derive(Clone, Copy)]
pub enum OpenMode {
    Read,
    Create,
    Exclusive,
}

pub struct Directory {
    path: PathBuf,
    #[cfg(unix)]
    handle: File,
}

/// Resolve pre-existing ancestors, while refusing symlinks at managed paths.
pub fn destination(path: &Path) -> Result<PathBuf, Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            Err(Error::UnsafePath(path.to_owned()))
        }
        Ok(_) => Ok(fs::canonicalize(path)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let name = path.file_name().ok_or(Error::InvalidInput("invalid memory directory"))?;
            let parent = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let parent = match fs::canonicalize(parent) {
                Ok(parent) if parent.is_dir() => parent,
                Ok(parent) => return Err(Error::UnsafePath(parent)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => destination(parent)?,
                Err(error) => return Err(error.into()),
            };
            Ok(parent.join(name))
        }
        Err(error) => Err(error.into()),
    }
}

impl Directory {
    pub fn backup(&self, original: &[u8]) -> Result<(), Error> {
        use std::io::Read;
        const BACKUP: &str = "memory.v1.backup.json";
        const STAGING: &str = ".memory.v1.backup.tmp";
        self.verify()?;
        match self.remove(STAGING) {
            Ok(()) => {}
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if self.open(BACKUP, OpenMode::Read)?.is_none() {
            let mut file = self
                .open(STAGING, OpenMode::Exclusive)?
                .ok_or(Error::Corrupt("backup staging was not created"))?;
            file.write_all(original)?;
            file.sync_all()?;
            self.publish_backup(STAGING, BACKUP)?;
        }
        let mut saved = Vec::new();
        self.open(BACKUP, OpenMode::Read)?
            .ok_or(Error::Corrupt("backup disappeared"))?
            .take(super::MAX_BYTES + 1)
            .read_to_end(&mut saved)?;
        if saved != original {
            return Err(Error::Corrupt("v1 backup differs from original"));
        }
        Ok(())
    }

    pub fn persist(&self, bytes: &[u8]) -> Result<(), Error> {
        // The writer lock makes one reusable staging file sufficient, including after crashes.
        let temporary = ".memory.tmp";
        if self.open(temporary, OpenMode::Read)?.is_some() {
            self.remove(temporary)?;
        }
        let result = (|| {
            let mut file = self
                .open(temporary, OpenMode::Exclusive)?
                .ok_or(Error::Corrupt("temporary file was not created"))?;
            file.write_all(bytes)?;
            file.sync_all()?;
            self.replace(temporary)?;
            Ok(())
        })();
        if result.is_err() {
            // Best-effort removal preserves the original failure; no data is published here.
            let _cleanup_result = self.remove(temporary);
        }
        result
    }
}

#[cfg(unix)]
impl Directory {
    /// A newly created registry must itself survive a crash before its contents are published.
    pub fn sync_parent(&self) -> Result<(), Error> {
        self.verify()?;
        let path = self.path.parent().ok_or(Error::Corrupt("registry parent missing"))?;
        let parent = Self::existing(path)?.ok_or(Error::Corrupt("registry parent disappeared"))?;
        parent.handle.sync_all()?;
        Ok(())
    }

    fn publish_backup(&self, staging: &str, target: &str) -> Result<(), Error> {
        self.verify()?;
        rustix::fs::linkat(
            &self.handle,
            staging,
            &self.handle,
            target,
            rustix::fs::AtFlags::empty(),
        )
        .map_err(std::io::Error::from)?;
        self.remove(staging)?;
        self.handle.sync_all()?;
        Ok(())
    }

    pub fn existing(path: &Path) -> Result<Option<Self>, Error> {
        use rustix::fs::{Mode, OFlags, openat};
        let mut handle = File::open("/")?;
        for component in path.components() {
            let std::path::Component::Normal(component) = component else { continue };
            handle = match openat(
                &handle,
                component,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            ) {
                Ok(fd) => File::from(fd),
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(error) => return Err(path_error(error, path)),
            };
        }
        let directory = Self { path: path.to_owned(), handle };
        directory.verify()?;
        Ok(Some(directory))
    }

    pub fn create(base: &Path, project: &str) -> Result<Self, Error> {
        use rustix::fs::{Mode, OFlags, fchmod, mkdirat, openat};
        use std::os::unix::fs::MetadataExt;
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mode = Mode::RUSR | Mode::WUSR | Mode::XUSR;
        let mut handle = File::open("/")?;
        for component in base.components() {
            let std::path::Component::Normal(component) = component else { continue };
            let next = match openat(&handle, component, flags, Mode::empty()) {
                Ok(next) => next,
                Err(rustix::io::Errno::NOENT) => {
                    match mkdirat(&handle, component, mode) {
                        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                        Err(error) => return Err(std::io::Error::from(error).into()),
                    }
                    openat(&handle, component, flags, Mode::empty())
                        .map_err(|error| path_error(error, base))?
                }
                Err(error) => return Err(path_error(error, base)),
            };
            handle = File::from(next);
        }
        match mkdirat(&handle, project, mode) {
            Ok(()) | Err(rustix::io::Errno::EXIST) => {}
            Err(error) => return Err(std::io::Error::from(error).into()),
        }
        let path = base.join(project);
        let handle = File::from(
            openat(&handle, project, flags, Mode::empty())
                .map_err(|error| path_error(error, &path))?,
        );
        if handle.metadata()?.nlink() == 0 {
            return Err(Error::UnsafePath(path));
        }
        fchmod(&handle, mode).map_err(std::io::Error::from)?;
        Ok(Self { path, handle })
    }

    pub fn open(&self, name: &str, mode: OpenMode) -> Result<Option<File>, Error> {
        use rustix::fs::{Mode, OFlags, fchmod, openat};
        use std::os::unix::fs::MetadataExt;
        self.verify()?;
        let flags = OFlags::NOFOLLOW
            | OFlags::CLOEXEC
            | OFlags::NONBLOCK
            | match mode {
                OpenMode::Read => OFlags::RDONLY,
                OpenMode::Create => OFlags::RDWR | OFlags::CREATE,
                OpenMode::Exclusive => OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
            };
        let path = self.path.join(name);
        let file = match openat(&self.handle, name, flags, Mode::RUSR | Mode::WUSR) {
            Ok(file) => File::from(file),
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => return Err(path_error(error, &path)),
        };
        let metadata = file.metadata()?;
        // An atomic replacement may unlink a reader's old snapshot after it was opened.
        if !metadata.is_file() || metadata.nlink() > 1 {
            return Err(Error::UnsafePath(path));
        }
        if !matches!(mode, OpenMode::Read) {
            fchmod(&file, Mode::RUSR | Mode::WUSR).map_err(std::io::Error::from)?;
        }
        Ok(Some(file))
    }

    fn verify(&self) -> Result<(), Error> {
        use std::os::unix::fs::MetadataExt;
        let current = fs::symlink_metadata(&self.path)?;
        let opened = self.handle.metadata()?;
        if !current.is_dir()
            || current.file_type().is_symlink()
            || current.dev() != opened.dev()
            || current.ino() != opened.ino()
        {
            return Err(Error::UnsafePath(self.path.clone()));
        }
        if let Some(parent) = self.path.parent() {
            let metadata = fs::symlink_metadata(parent)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(Error::UnsafePath(parent.to_owned()));
            }
        }
        Ok(())
    }

    fn replace(&self, temporary: &str) -> Result<(), Error> {
        self.verify()?;
        rustix::fs::renameat(&self.handle, temporary, &self.handle, DATA_FILE)
            .map_err(std::io::Error::from)?;
        self.handle.sync_all()?;
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<(), Error> {
        rustix::fs::unlinkat(&self.handle, name, rustix::fs::AtFlags::empty())
            .map_err(std::io::Error::from)?;
        Ok(())
    }
}

#[cfg(unix)]
fn path_error(error: rustix::io::Errno, path: &Path) -> Error {
    match error {
        rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => Error::UnsafePath(path.to_owned()),
        error => Error::Io(error.into()),
    }
}

#[cfg(not(unix))]
impl Directory {
    fn publish_backup(&self, staging: &str, target: &str) -> Result<(), Error> {
        self.verify()?;
        fs::hard_link(self.path.join(staging), self.path.join(target))?;
        self.remove(staging)
    }

    pub fn existing(path: &Path) -> Result<Option<Self>, Error> {
        match fs::symlink_metadata(path) {
            Ok(_) => {
                let directory = Self { path: path.to_owned() };
                directory.verify()?;
                Ok(Some(directory))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn create(base: &Path, project: &str) -> Result<Self, Error> {
        fs::create_dir_all(base)?;
        let path = base.join(project);
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        let directory = Self { path };
        directory.verify()?;
        Ok(directory)
    }

    pub fn open(&self, name: &str, mode: OpenMode) -> Result<Option<File>, Error> {
        self.verify()?;
        let path = self.path.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
                return Err(Error::UnsafePath(path));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut options = fs::OpenOptions::new();
        match mode {
            OpenMode::Read => {
                options.read(true);
            }
            OpenMode::Create => {
                options.read(true).write(true).create(true);
            }
            OpenMode::Exclusive => {
                options.write(true).create_new(true);
            }
        }
        match options.open(&path) {
            Ok(file) if file.metadata()?.is_file() => Ok(Some(file)),
            Ok(_) => Err(Error::UnsafePath(path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn verify(&self) -> Result<(), Error> {
        for path in [Some(self.path.as_path()), self.path.parent()].into_iter().flatten() {
            let metadata = fs::symlink_metadata(path)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(Error::UnsafePath(path.to_owned()));
            }
        }
        Ok(())
    }

    fn replace(&self, temporary: &str) -> Result<(), Error> {
        self.verify()?;
        fs::rename(self.path.join(temporary), self.path.join(DATA_FILE))?;
        Ok(())
    }

    fn remove(&self, name: &str) -> Result<(), Error> {
        Ok(fs::remove_file(self.path.join(name))?)
    }
}
