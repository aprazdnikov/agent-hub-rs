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
const REGISTRY_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

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
        // Same brief wait as `purge` uses: the two writers of `.projects` serialize on it
        // and both publications are short.
        let _registry_lock = registry_lock(&registry_directory)?;
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
/// Outcome of [`purge`]: registry records removed, whether the data directory was deleted
/// with published memory inside, and whether the storage was kept because other living
/// projects still resolve to the same identity.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PurgeReport {
    pub removed_paths: usize,
    pub had_data: bool,
    pub shared: bool,
}

/// Removes a project from hub memory: its own registry records and — unless [`purge`]
/// receives another living project that resolves to the same identity — the alias records
/// and the data directory. `living` lists the projects that must keep working afterwards
/// (the saved settings minus the deleted one): a storage shared with any of them stays
/// intact together with their records, so deleting one profile never destroys another's
/// memory. Nothing outside `base` is deleted and the project directory itself is never
/// touched. A project directory that has already vanished stays purgeable, so the cascade
/// is retryable to the end. While another process holds the registry or the data lock (a
/// live MCP store or a concurrent binding apply) the call reports [`Error::Busy`] and
/// changes nothing.
pub fn purge(base: &Path, project: &Path, living: &[PathBuf]) -> Result<PurgeReport, Error> {
    let base = filesystem::destination(base)?;
    let project = former_path(project)?;
    let living = living_paths(living);
    // The registry is shared with `BindingPreview::apply`, which publishes `.projects`
    // under this lock and through the same staging name: the whole read-modify-write of
    // both writers must serialize on it.
    let registry_directory = filesystem::Directory::create(&base, REGISTRY)?;
    #[cfg(unix)]
    registry_directory.sync_parent()?;
    let _registry_lock = registry_lock(&registry_directory)?;
    let mut registry = Registry::read(&base)?;
    let project_text = path_text(&project)?;
    // A moved-away source is tombstoned `Some(None)`: it has no reachable identity, but
    // its records still key the former path.
    let is_tombstone = registry.paths.get(project_text).is_some_and(Option::is_none);
    let identity =
        registry.paths.get(project_text).cloned().flatten().unwrap_or_else(|| project.clone());
    // The digest component may have been swapped for a symlink: refuse it and require
    // every removed path to stay under the canonical base.
    let directory =
        filesystem::destination(&base.join(portable::digest(path_text(&identity)?.as_bytes())))?;
    if !directory.starts_with(&base) {
        return Err(Error::UnsafePath(directory));
    }
    let existing = filesystem::Directory::existing(&directory)?;
    // The identity path itself still owns the storage unless its own record is a
    // tombstone: a moved-away source cannot silently re-adopt the lineage by existing.
    let identity_is_held = identity != project
        && registry.paths.get(path_text(&identity)?).is_none_or(Option::is_some)
        && is_living(&living, &identity);
    let shared = identity_is_held
        || registry.paths.iter().any(|(path, bound)| {
            bound.as_deref() == Some(identity.as_path())
                && Path::new(path.as_str()) != project
                && is_living(&living, Path::new(path.as_str()))
        });
    let mut removed_paths = 0;
    // The purged project's own binding goes in both outcomes; its tombstone only when the
    // lineage itself is removed — while the storage stays for a neighbour the tombstone is
    // what keeps the former path from silently reclaiming it.
    let keep_own = shared && is_tombstone;
    if !keep_own && registry.paths.remove(project_text).is_some() {
        removed_paths += 1;
    }
    if !shared {
        let identity_text = path_text(&identity)?;
        let before = registry.paths.len();
        registry.paths.retain(|path, bound| {
            path != identity_text && bound.as_deref() != Some(identity.as_path())
        });
        removed_paths += before - registry.paths.len();
    }
    let data_lock = if shared { None } else { existing.as_ref().map(lock).transpose()? };
    if removed_paths > 0 {
        registry_directory.persist(&registry.bytes()?)?;
    }
    // A binding preview may leave the directory seeded with only its lock file; that is
    // still removed, while `had_data` reports published memory contents.
    let had_data = !shared
        && existing
            .as_ref()
            .map(|directory| directory.open(DATA_FILE, filesystem::OpenMode::Read))
            .transpose()?
            .flatten()
            .is_some();
    if data_lock.is_some() {
        // On Unix the lock's open file does not block the recursive delete, and the guard
        // is held until it finishes: releasing it earlier would let a live store grab the
        // lock and recreate the directory between unlock and unlink. Windows cannot remove
        // a tree that holds an open handle, so the lock is dropped there first.
        #[cfg(not(unix))]
        drop(data_lock);
        fs::remove_dir_all(&directory)?;
    }
    Ok(PurgeReport { removed_paths, had_data, shared })
}

/// Canonical form when the folder still exists, the former path otherwise: the vanished
/// project keeps its records addressable exactly the way `source_path` lets a moved
/// source stay previewable.
fn former_path(path: &Path) -> Result<PathBuf, Error> {
    match fs::canonicalize(path) {
        Ok(path) if path.is_dir() => Ok(path),
        Ok(_) => Err(Error::InvalidInput("project must be a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            validate_path(path)?;
            Ok(path.to_owned())
        }
        Err(error) => Err(error.into()),
    }
}

/// Both spellings of every living project path: settings and registry keys meet at the
/// canonical form, while a folder that vanished mid-life keeps the recorded one.
fn living_paths(living: &[PathBuf]) -> std::collections::BTreeSet<PathBuf> {
    living
        .iter()
        .flat_map(|path| [path.clone(), fs::canonicalize(path).unwrap_or_else(|_| path.clone())])
        .collect()
}

fn is_living(living: &std::collections::BTreeSet<PathBuf>, path: &Path) -> bool {
    living.contains(path)
        || fs::canonicalize(path).is_ok_and(|canonical| living.contains(&canonical))
}

/// The registry publication is short (`apply` holds the lock only while it prepares and
/// persists), so waiting briefly for it beats turning the whole cascade into a user retry.
/// The data-directory lock is never waited on: a live MCP store holds it for the duration
/// of the session.
fn registry_lock(directory: &filesystem::Directory) -> Result<File, Error> {
    let deadline = std::time::Instant::now() + REGISTRY_WAIT;
    loop {
        match lock(directory) {
            Err(Error::Busy) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            result => return result,
        }
    }
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
