//! Explicit, user-local project bindings. Repository files never grant access.
use super::{DATA_FILE, LOCK_FILE, Store, filesystem, portable, unique_map};
use crate::{Error, SnapshotToken};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

const REGISTRY: &str = ".projects";
const MAX_REGISTRY: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub enum BindingMode {
    #[default]
    Move,
    Share,
}

#[derive(Clone, Debug)]
pub struct BindingPreview {
    location: Location,
    source: PathBuf,
    target: PathBuf,
    mode: BindingMode,
    confirmation: String,
}

#[derive(Clone, Debug)]
pub(super) struct Location {
    pub base: PathBuf,
    pub cwd: PathBuf,
    pub identity: PathBuf,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    schema_version: u32,
    #[serde(deserialize_with = "unique_map")]
    paths: BTreeMap<String, Option<PathBuf>>,
}
impl Registry {
    fn read(base: &Path) -> Result<Self, Error> {
        let directory = filesystem::Directory::existing(&base.join(REGISTRY))?;
        let file =
            directory.map(|d| d.open(DATA_FILE, filesystem::OpenMode::Read)).transpose()?.flatten();
        let Some(file) = file else {
            return Ok(Self { schema_version: 1, paths: BTreeMap::new() });
        };
        let mut bytes = Vec::new();
        file.take(MAX_REGISTRY + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_REGISTRY {
            return Err(Error::LimitExceeded("1 MiB project registry"));
        }
        let registry: Self = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Corrupt("invalid project registry"))?;
        registry.validate()?;
        Ok(registry)
    }
    fn validate(&self) -> Result<(), Error> {
        if self.schema_version != 1 || self.paths.len() > 4096 {
            return Err(Error::Corrupt("unsupported or oversized project registry"));
        }
        for (path, identity) in &self.paths {
            validate_path(Path::new(path))?;
            if let Some(identity) = identity {
                validate_path(identity)?;
            }
        }
        Ok(())
    }
    fn resolve(&self, path: &Path) -> Result<PathBuf, Error> {
        match self.paths.get(path_text(path)?) {
            Some(Some(identity)) => Ok(identity.clone()),
            Some(None) => Err(Error::BindingChanged),
            None => Ok(path.to_owned()),
        }
    }
    fn bytes(&self) -> Result<Vec<u8>, Error> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)
            .map_err(|_| Error::Corrupt("cannot serialize project registry"))?;
        if bytes.len() as u64 > MAX_REGISTRY {
            return Err(Error::LimitExceeded("1 MiB project registry"));
        }
        Ok(bytes)
    }
}
impl Location {
    pub fn resolve(base: &Path, cwd: &Path) -> Result<Self, Error> {
        let base = filesystem::destination(base)?;
        let identity = Registry::read(&base)?.resolve(cwd)?;
        Ok(Self { base, cwd: cwd.to_owned(), identity })
    }
    pub fn check(&self) -> Result<(), Error> {
        if Registry::read(&self.base)?.resolve(&self.cwd)? != self.identity {
            return Err(Error::BindingChanged);
        }
        Ok(())
    }
    pub fn projects(&self) -> Result<Vec<PathBuf>, Error> {
        let registry = Registry::read(&self.base)?;
        if registry.resolve(&self.cwd)? != self.identity {
            return Err(Error::BindingChanged);
        }
        let mut paths: std::collections::BTreeSet<_> = registry
            .paths
            .iter()
            .filter(|(_, identity)| identity.as_ref() == Some(&self.identity))
            .map(|(path, _)| PathBuf::from(path))
            .collect();
        if !registry.paths.contains_key(path_text(&self.identity)?) {
            paths.insert(self.identity.clone());
        }
        Ok(paths.into_iter().collect())
    }
    pub fn directory(&self) -> Result<PathBuf, Error> {
        Ok(self.base.join(portable::digest(path_text(&self.identity)?.as_bytes())))
    }
    fn store(&self) -> Result<Store, Error> {
        let directory = filesystem::Directory::existing(&self.directory()?)?
            .ok_or(Error::InvalidInput("source memory does not exist"))?;
        Ok(Store {
            directory: Arc::new(directory),
            project: self.identity.clone(),
            binding: self.clone(),
            actor: "human-admin".into(),
            incarnation: String::new(),
        })
    }
}
impl BindingPreview {
    /// Read-only preview. A missing move source must be its former canonical absolute path.
    pub fn prepare(
        base: &Path,
        source: &Path,
        target: &Path,
        mode: BindingMode,
    ) -> Result<Self, Error> {
        let source = source_path(source, mode)?;
        let target = fs::canonicalize(target).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::InvalidInput("target directory does not exist")
            } else {
                Error::Io(error)
            }
        })?;
        if !target.is_dir() {
            return Err(Error::InvalidInput("target must be a directory"));
        }
        if source == target {
            return Err(Error::InvalidInput("choose two different project directories"));
        }
        let location = Location::resolve(base, &source)?;
        let registry = Registry::read(&location.base)?;
        if registry.paths.contains_key(path_text(&target)?) {
            return Err(Error::BindingOccupied);
        }
        let destination = Location::resolve(base, &target)?;
        if let Some(directory) = filesystem::Directory::existing(&destination.directory()?)?
            && directory.open(DATA_FILE, filesystem::OpenMode::Read)?.is_some()
        {
            return Err(Error::BindingOccupied);
        }
        let store = location.store()?;
        let (snapshot, size) = store.load_with_size()?;
        if size == 0 {
            return Err(Error::InvalidInput("source memory has no saved snapshot"));
        }
        let confirmation =
            confirmation(&registry.bytes()?, &location, &target, mode, &snapshot.token())?;
        Ok(Self { location, source, target, mode, confirmation })
    }
    #[must_use]
    pub fn base(&self) -> &Path {
        &self.location.base
    }
    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }
    #[must_use]
    pub fn mode(&self) -> BindingMode {
        self.mode
    }
    #[must_use]
    pub fn confirmation(&self) -> &str {
        &self.confirmation
    }
    #[must_use]
    pub fn project_id(&self) -> String {
        portable::digest(self.location.identity.as_os_str().as_encoded_bytes())
    }
    /// Publishes only the registry; all existing data and backups stay in place.
    pub fn apply(&self) -> Result<(), Error> {
        let registry_directory = filesystem::Directory::create(&self.location.base, REGISTRY)?;
        #[cfg(unix)]
        registry_directory.sync_parent()?;
        let _registry_lock = lock(&registry_directory)?;
        let source = self.location.store()?;
        let destination = Location::resolve(&self.location.base, &self.target)?;
        let target_id = portable::digest(path_text(&destination.identity)?.as_bytes());
        let target_directory = filesystem::Directory::create(&self.location.base, &target_id)?;
        // Nonblocking locks: opposing operations fail Busy instead of deadlocking.
        let _source_lock = lock(&source.directory)?;
        let _target_lock = lock(&target_directory)?;
        let current = Self::prepare(&self.location.base, &self.source, &self.target, self.mode)?;
        if current.confirmation != self.confirmation {
            return Err(Error::StalePreview);
        }
        let mut registry = Registry::read(&self.location.base)?;
        registry
            .paths
            .insert(path_text(&self.target)?.to_owned(), Some(self.location.identity.clone()));
        if self.mode == BindingMode::Move {
            registry.paths.insert(path_text(&self.source)?.to_owned(), None);
        }
        let bytes = registry.bytes()?;
        match registry_directory.persist(&bytes) {
            Ok(()) => Ok(()),
            Err(cause) => {
                // rename can succeed before the final directory sync fails. Never tell the
                // caller it is safe to keep using the old path when publication is uncertain.
                match Registry::read(&self.location.base).and_then(|current| current.bytes()) {
                    Ok(current) if current != bytes => Err(cause),
                    Ok(_) | Err(_) => Err(Error::BindingPublicationUncertain {
                        target: self.target.clone(),
                        cause: Box::new(cause),
                    }),
                }
            }
        }
    }
}
/// Outcome of [`purge`]: registry records removed and whether the project had published
/// memory data (the data directory is removed either way when it exists).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PurgeReport {
    pub removed_paths: usize,
    pub had_data: bool,
}

/// Removes a project from hub memory: every registry record resolving to its identity
/// (and the identity record itself) plus the data directory. Nothing outside `base` is
/// deleted and the project directory itself is never touched. While another process
/// holds the data-directory lock the call reports [`Error::Busy`] and changes nothing.
pub fn purge(base: &Path, project: &Path) -> Result<PurgeReport, Error> {
    let project = fs::canonicalize(project).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::InvalidInput("project directory does not exist")
        } else {
            Error::Io(error)
        }
    })?;
    if !project.is_dir() {
        return Err(Error::InvalidInput("project must be a directory"));
    }
    let location = Location::resolve(base, &project)?;
    let identity = path_text(&location.identity)?;
    // The digest component may have been swapped for a symlink: refuse it and require
    // every removed path to stay under the canonical base.
    let directory = filesystem::destination(&location.directory()?)?;
    if !directory.starts_with(&location.base) {
        return Err(Error::UnsafePath(directory));
    }
    let existing = filesystem::Directory::existing(&directory)?;
    let lock = existing.as_ref().map(lock).transpose()?;
    let mut removed_paths = 0;
    if let Some(registry) = filesystem::Directory::existing(&location.base.join(REGISTRY))? {
        let mut current = Registry::read(&location.base)?;
        let before = current.paths.len();
        current.paths.retain(|path, bound| {
            path != identity && bound.as_deref() != Some(location.identity.as_path())
        });
        removed_paths = before - current.paths.len();
        if removed_paths > 0 {
            registry.persist(&current.bytes()?)?;
        }
    }
    // A binding preview may leave the directory seeded with only its lock file; that is
    // still removed, while `had_data` reports published memory contents.
    let had_data = existing
        .as_ref()
        .map(|directory| directory.open(DATA_FILE, filesystem::OpenMode::Read))
        .transpose()?
        .flatten()
        .is_some();
    if existing.is_some() {
        // The recursive delete must not race an open handle kept alive by the lock.
        drop(lock);
        fs::remove_dir_all(&directory)?;
    }
    Ok(PurgeReport { removed_paths, had_data })
}

fn lock(directory: &filesystem::Directory) -> Result<File, Error> {
    let file = directory
        .open(LOCK_FILE, filesystem::OpenMode::Create)?
        .ok_or(Error::Corrupt("project lock missing"))?;
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => Error::Busy,
        fs::TryLockError::Error(error) => Error::Io(error),
    })?;
    Ok(file)
}
fn source_path(path: &Path, mode: BindingMode) -> Result<PathBuf, Error> {
    validate_path(path)?;
    match fs::canonicalize(path) {
        Ok(path) if path.is_dir() => Ok(path),
        Ok(_) => Err(Error::InvalidInput("source must be a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && mode == BindingMode::Move => {
            validate_path(path)?;
            Ok(path.to_owned())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(Error::InvalidInput("source directory does not exist"))
        }
        Err(error) => Err(error.into()),
    }
}
fn validate_path(path: &Path) -> Result<(), Error> {
    if !path.is_absolute()
        || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || path_text(path)?.len() > 32768
    {
        return Err(Error::InvalidInput(
            "project binding requires a canonical absolute UTF-8 path",
        ));
    }
    Ok(())
}
fn path_text(path: &Path) -> Result<&str, Error> {
    path.to_str().ok_or(Error::InvalidInput("project path must be valid UTF-8"))
}
fn confirmation(
    registry: &[u8],
    location: &Location,
    target: &Path,
    mode: BindingMode,
    token: &SnapshotToken,
) -> Result<String, Error> {
    let bytes = serde_json::to_vec(&(
        registry,
        &location.base,
        &location.cwd,
        &location.identity,
        target,
        mode,
        token,
    ))
    .map_err(|_| Error::Corrupt("cannot serialize binding preview"))?;
    Ok(portable::digest(&bytes))
}
