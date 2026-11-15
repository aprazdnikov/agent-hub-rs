use std::{
    collections::BTreeMap,
    fmt::Write as FmtWrite,
    fs::{self, File, TryLockError},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Entry, Error, SearchHit, WriteRequest};

const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_KEYS: usize = 500;
const MAX_REVISIONS: usize = 32;
const SCHEMA_VERSION: u32 = 1;
const DATA_FILE: &str = "memory.json";
const LOCK_FILE: &str = ".lock";

/// A project-scoped store. Clones and independent processes read the same durable state.
#[derive(Clone)]
pub struct Store {
    directory: Arc<filesystem::Directory>,
    project: PathBuf,
    actor: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    project: PathBuf,
    #[serde(deserialize_with = "unique_histories")]
    entries: BTreeMap<String, Vec<Entry>>,
}

#[derive(Deserialize)]
struct Header {
    schema_version: u32,
}

impl Store {
    /// Opens local memory at the caller's chosen location; no network access is performed.
    pub fn open(base: &Path, project: &Path, actor: &str) -> Result<Self, Error> {
        validate_actor(actor)?;
        let project = fs::canonicalize(project)?;
        if !project.is_dir() {
            return Err(Error::InvalidInput("project must be an existing directory"));
        }
        let identity =
            project.to_str().ok_or(Error::InvalidInput("project path must be valid UTF-8"))?;
        let mut project_id = String::with_capacity(64);
        for byte in Sha256::digest(identity.as_bytes()) {
            write!(project_id, "{byte:02x}")
                .map_err(|_| Error::Corrupt("could not format project identity"))?;
        }
        let base = filesystem::destination(base)?;
        let store = Self {
            directory: Arc::new(filesystem::Directory::create(&base, &project_id)?),
            project,
            actor: actor.to_owned(),
        };
        store.directory.open(LOCK_FILE, filesystem::OpenMode::Create)?;
        store.load()?;
        Ok(store)
    }

    /// Atomically appends a revision, or returns the original result of an identical retry.
    pub fn write(&self, request: WriteRequest) -> Result<Entry, Error> {
        validate_fields(&request.key, &request.title, &request.body, &request.sources)?;
        let _lock = self.acquire_lock()?;
        let mut snapshot = self.load()?;
        if let Some(existing) = snapshot.entries.get(&request.key) {
            if let Some(retry) = usize::try_from(request.expected_revision)
                .ok()
                .and_then(|revision| existing.get(revision))
                .filter(|entry| same_payload(entry, &request))
            {
                return Ok(retry.clone());
            }
            let actual = existing.last().map_or(0, |entry| entry.revision);
            ensure_revision(&request, actual)?;
            if existing.len() >= MAX_REVISIONS {
                return Err(Error::LimitExceeded("32 revisions per key"));
            }
        } else {
            ensure_revision(&request, 0)?;
            if snapshot.entries.len() >= MAX_KEYS {
                return Err(Error::LimitExceeded("500 keys per project"));
            }
        }
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Corrupt("system clock predates the Unix epoch"))?
            .as_secs()
            .try_into()
            .map_err(|_| Error::Corrupt("system clock exceeds timestamp range"))?;
        let WriteRequest { key, expected_revision, kind, title, body, sources } = request;
        let entry = Entry {
            key,
            revision: expected_revision
                .checked_add(1)
                .ok_or(Error::LimitExceeded("revision range"))?,
            kind,
            title,
            body,
            sources,
            author: self.actor.clone(),
            created_at,
        };
        snapshot.entries.entry(entry.key.clone()).or_default().push(entry.clone());
        let bytes = serde_json::to_vec(&snapshot)
            .map_err(|_| Error::Corrupt("could not serialize validated memory"))?;
        if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_BYTES) {
            return Err(Error::LimitExceeded("8 MiB per project"));
        }
        self.directory.persist(&bytes)?;
        Ok(entry)
    }

    /// Reads the latest revision by default, or an explicitly requested historical revision.
    pub fn get(&self, key: &str, revision: Option<u32>) -> Result<Entry, Error> {
        validate_key(key)?;
        if revision == Some(0) {
            return Err(Error::InvalidInput("stored revisions start at 1"));
        }
        let snapshot = self.load()?;
        snapshot
            .entries
            .get(key)
            .and_then(|history| match revision {
                Some(revision) => history.iter().find(|entry| entry.revision == revision),
                None => history.last(),
            })
            .cloned()
            .ok_or_else(|| Error::NotFound { key: key.to_owned(), revision })
    }

    /// Searches latest revisions with case-insensitive terms; an empty query lists recent entries.
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>, Error> {
        if query.len() > 512 {
            return Err(Error::InvalidInput("query exceeds 512 UTF-8 bytes"));
        }
        if !(1..=20).contains(&limit) {
            return Err(Error::InvalidInput("search limit must be between 1 and 20"));
        }
        let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
        let snapshot = self.load()?;
        let entries = {
            let mut entries: Vec<_> = snapshot
                .entries
                .values()
                .filter_map(|history| history.last())
                .filter(|entry| {
                    let text =
                        format!("{}\n{}\n{}", entry.title, entry.body, entry.sources.join("\n"))
                            .to_lowercase();
                    terms.iter().all(|term| text.contains(term))
                })
                .collect();
            entries.sort_by(|left, right| {
                right.created_at.cmp(&left.created_at).then_with(|| left.key.cmp(&right.key))
            });
            entries
        };
        Ok(entries
            .into_iter()
            .take(limit)
            .map(|entry| SearchHit {
                key: entry.key.clone(),
                revision: entry.revision,
                kind: entry.kind,
                title: entry.title.clone(),
                excerpt: entry.body.chars().take(400).collect(),
                created_at: entry.created_at,
            })
            .collect())
    }

    fn acquire_lock(&self) -> Result<File, Error> {
        let file = self
            .directory
            .open(LOCK_FILE, filesystem::OpenMode::Create)?
            .ok_or(Error::Corrupt("lock file was not created"))?;
        file.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => Error::Busy,
            TryLockError::Error(error) => Error::Io(error),
        })?;
        Ok(file)
    }

    fn load(&self) -> Result<Snapshot, Error> {
        let Some(file) = self.directory.open(DATA_FILE, filesystem::OpenMode::Read)? else {
            return Ok(Snapshot {
                schema_version: SCHEMA_VERSION,
                project: self.project.clone(),
                entries: BTreeMap::new(),
            });
        };
        if file.metadata()?.len() > MAX_BYTES {
            return Err(Error::LimitExceeded("8 MiB per project"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_BYTES) {
            return Err(Error::LimitExceeded("8 MiB per project"));
        }
        let header: Header = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Corrupt("invalid JSON or schema header"))?;
        if header.schema_version != SCHEMA_VERSION {
            return Err(Error::UnsupportedVersion(header.schema_version));
        }
        let snapshot: Snapshot = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Corrupt("invalid JSON or schema fields"))?;
        snapshot.validate(&self.project)?;
        Ok(snapshot)
    }
}

impl Snapshot {
    fn validate(&self, project: &Path) -> Result<(), Error> {
        if self.project != project {
            return Err(Error::ProjectMismatch);
        }
        if self.entries.len() > MAX_KEYS {
            return Err(Error::Corrupt("key count exceeds the schema limit"));
        }
        for (key, history) in &self.entries {
            if history.is_empty() || history.len() > MAX_REVISIONS {
                return Err(Error::Corrupt("invalid revision count"));
            }
            for (index, entry) in history.iter().enumerate() {
                if key != &entry.key || usize::try_from(entry.revision).ok() != index.checked_add(1)
                {
                    return Err(Error::Corrupt("entry key or revision sequence is invalid"));
                }
                validate_fields(&entry.key, &entry.title, &entry.body, &entry.sources)
                    .and_then(|()| validate_actor(&entry.author))
                    .map_err(|_| Error::Corrupt("entry fields violate schema limits"))?;
                if entry.created_at < 0 {
                    return Err(Error::Corrupt("negative entry timestamp"));
                }
            }
        }
        Ok(())
    }
}

fn validate_key(key: &str) -> Result<(), Error> {
    if key.is_empty()
        || key.len() > 64
        || !key.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(Error::InvalidInput(
            "key must contain 1..64 ASCII letters, digits, '_' or '-'",
        ));
    }
    Ok(())
}

fn validate_actor(actor: &str) -> Result<(), Error> {
    if actor.trim().is_empty() || actor.len() > 128 {
        return Err(Error::InvalidInput("actor must contain 1..128 UTF-8 bytes"));
    }
    Ok(())
}

fn validate_fields(key: &str, title: &str, body: &str, sources: &[String]) -> Result<(), Error> {
    validate_key(key)?;
    if title.len() > 256 || body.len() > 16_384 {
        return Err(Error::InvalidInput("title exceeds 256 or body exceeds 16384 UTF-8 bytes"));
    }
    if !(1..=8).contains(&sources.len())
        || sources.iter().any(|source| source.trim().is_empty() || source.len() > 2048)
    {
        return Err(Error::InvalidInput(
            "provide 1..8 nonempty sources of at most 2048 UTF-8 bytes each",
        ));
    }
    Ok(())
}

fn ensure_revision(request: &WriteRequest, actual: u32) -> Result<(), Error> {
    if request.expected_revision != actual {
        return Err(Error::Conflict {
            key: request.key.clone(),
            expected: request.expected_revision,
            actual,
        });
    }
    Ok(())
}

fn same_payload(entry: &Entry, request: &WriteRequest) -> bool {
    let WriteRequest { key, expected_revision: _, kind, title, body, sources } = request;
    entry.key == *key
        && entry.kind == *kind
        && entry.title == *title
        && entry.body == *body
        && entry.sources == *sources
}

fn unique_histories<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, Vec<Entry>>, D::Error> {
    struct UniqueHistories;
    impl<'de> serde::de::Visitor<'de> for UniqueHistories {
        type Value = BTreeMap<String, Vec<Entry>>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("unique entry keys")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut entries = BTreeMap::new();
            while let Some((key, value)) = map.next_entry()? {
                if entries.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate entry key"));
                }
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_map(UniqueHistories)
}

mod filesystem {
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
                let name =
                    path.file_name().ok_or(Error::InvalidInput("invalid memory directory"))?;
                let parent = path
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."));
                let parent = match fs::canonicalize(parent) {
                    Ok(parent) if parent.is_dir() => parent,
                    Ok(parent) => return Err(Error::UnsafePath(parent)),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        destination(parent)?
                    }
                    Err(error) => return Err(error.into()),
                };
                Ok(parent.join(name))
            }
            Err(error) => Err(error.into()),
        }
    }

    impl Directory {
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
            fchmod(&file, Mode::RUSR | Mode::WUSR).map_err(std::io::Error::from)?;
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
            rustix::io::Errno::LOOP | rustix::io::Errno::NOTDIR => {
                Error::UnsafePath(path.to_owned())
            }
            error => Error::Io(error.into()),
        }
    }

    #[cfg(not(unix))]
    impl Directory {
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
}
