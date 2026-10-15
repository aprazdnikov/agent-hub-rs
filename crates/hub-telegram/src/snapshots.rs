//! Snapshots on disk: `<dir>/snapshots/<name>/{manifest.json, agent/…}`. A snapshot is
//! written in a private staging directory and renamed into place, so it is whole or absent.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use hub_agent::archive::{
    ArchiveError, Entry, entry_under, open_under, private_dirs, private_file, restrict_dir,
};
use hub_core::snapshot::{MANIFEST_LIMIT, Manifest, ManifestError, RelPath, SnapshotName};

const SNAPSHOTS: &str = "snapshots";
const MANIFEST: &str = "manifest.json";
const AGENT: &str = "agent";
const STAGING_PREFIX: &str = ".tmp-";
/// Staging older than this was left by a crash; a live save finishes in minutes.
const STALE_STAGING: Duration = Duration::from_hours(24);

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("Снимок {0} уже существует")]
    Exists(SnapshotName),
    #[error("Снимок {0} не найден")]
    Missing(SnapshotName),
    #[error("Снимок {name} повреждён: {reason}")]
    Corrupt { name: SnapshotName, reason: String },
    #[error("Манифест снимка не записан: {0}")]
    Manifest(#[from] ManifestError),
    #[error("Каталог снимков недоступен: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Archive(#[from] ArchiveError),
}

#[derive(Debug)]
pub struct Loaded {
    pub manifest: Manifest,
    /// The snapshot's `agent/` directory, which `manifest.files` are relative to.
    pub agent: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SnapshotStore {
    root: PathBuf,
}

/// Removes the staging directory unless it was renamed into place.
struct Staging {
    path: PathBuf,
    committed: bool,
}

impl Staging {
    fn create(root: &Path) -> io::Result<Self> {
        let path = root.join(format!("{STAGING_PREFIX}{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir(&path)?;
        let staging = Self { path, committed: false };
        restrict_dir(&staging.path)?;
        Ok(staging)
    }

    fn commit(mut self, target: &Path) -> io::Result<()> {
        fs::rename(&self.path, target)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.committed
            && let Err(error) = fs::remove_dir_all(&self.path)
        {
            tracing::warn!(kind = ?error.kind(), "snapshot staging left behind");
        }
    }
}

impl SnapshotStore {
    #[must_use]
    pub fn new(dir: &Path) -> Self {
        Self { root: dir.join(SNAPSHOTS) }
    }

    /// Writes a snapshot whole or not at all.
    ///
    /// # Errors
    /// `Exists` for a taken name, `Manifest` if the manifest would not read back, `Archive`
    /// for what `fill` refused, `Io` for the store itself. Staging is removed on any error.
    pub fn save(
        &self,
        name: &SnapshotName,
        fill: impl FnOnce(&Path) -> Result<Manifest, ArchiveError>,
    ) -> Result<Manifest, SnapshotError> {
        private_dirs(&self.root)?;
        restrict_dir(&self.root)?;
        self.sweep();
        let target = self.root.join(name.as_str());
        if fs::symlink_metadata(&target).is_ok() {
            return Err(SnapshotError::Exists(name.clone()));
        }
        let staging = Staging::create(&self.root)?;
        let agent = staging.path.join(AGENT);
        private_dirs(&agent)?;
        let manifest = fill(&agent)?;
        if manifest.name != *name {
            return Err(corrupt(name, "имя в манифесте другое".to_owned()));
        }
        // Fails before anything is published if the manifest would not read back.
        let text = manifest.dump()?;
        let mut out = private_file(&staging.path.join(MANIFEST))?;
        out.write_all(text.as_bytes())?;
        out.sync_all()?;
        drop(out);
        // A concurrent save of the same name is refused by the hub; this narrows what is left.
        if fs::symlink_metadata(&target).is_ok() {
            return Err(SnapshotError::Exists(name.clone()));
        }
        staging.commit(&target)?;
        sync_dir(&self.root);
        tracing::info!(snapshot = name.as_str(), files = manifest.files.len(), "snapshot saved");
        Ok(manifest)
    }

    /// Readable snapshots, newest first; corrupt and service entries are skipped.
    ///
    /// # Errors
    /// `Io` if the snapshots directory exists but cannot be read.
    pub fn list(&self) -> Result<Vec<Manifest>, SnapshotError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut manifests: Vec<Manifest> = entries
            .filter_map(|entry| {
                let raw = entry.ok()?.file_name().to_str()?.to_owned();
                if raw.starts_with('.') {
                    return None;
                }
                let name = SnapshotName::parse(&raw).ok()?;
                self.read(&name)
                    .map_err(|_| tracing::warn!(snapshot = %name, "snapshot skipped"))
                    .ok()
            })
            .collect();
        manifests.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.name.cmp(&b.name)));
        Ok(manifests)
    }

    /// A snapshot whose directory, manifest and every listed file were checked.
    ///
    /// # Errors
    /// `Missing` for an absent snapshot, `Corrupt` for one that cannot be trusted.
    pub fn load(&self, name: &SnapshotName) -> Result<Loaded, SnapshotError> {
        let manifest = self.read(name)?;
        let snapshot = self.root.join(name.as_str());
        let agent_rel = RelPath::parse(AGENT).map_err(|error| corrupt(name, error.to_string()))?;
        match entry_under(&snapshot, &agent_rel).map_err(|error| refused(name, error))? {
            Entry::Dir => {}
            Entry::Missing | Entry::File | Entry::Link | Entry::Other => {
                return Err(corrupt(name, "agent/ не каталог".to_owned()));
            }
        }
        let agent = snapshot.join(AGENT);
        for file in &manifest.files {
            match entry_under(&agent, file).map_err(|error| refused(name, error))? {
                Entry::File => {}
                Entry::Missing | Entry::Dir | Entry::Link | Entry::Other => {
                    return Err(corrupt(name, format!("нет обычного файла {file}")));
                }
            }
        }
        Ok(Loaded { manifest, agent })
    }

    fn read(&self, name: &SnapshotName) -> Result<Manifest, SnapshotError> {
        let dir = self.root.join(name.as_str());
        match fs::symlink_metadata(&dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(SnapshotError::Missing(name.clone()));
            }
            Err(error) => return Err(error.into()),
            Ok(metadata) if !metadata.file_type().is_dir() => {
                return Err(corrupt(name, "не каталог".to_owned()));
            }
            Ok(_) => {}
        }
        let manifest_rel =
            RelPath::parse(MANIFEST).map_err(|error| corrupt(name, error.to_string()))?;
        let file = open_under(&dir, &manifest_rel).map_err(|error| refused(name, error))?;
        // One byte over the limit is enough to tell "too large" from "truncated".
        let limit = u64::try_from(MANIFEST_LIMIT).unwrap_or(u64::MAX).saturating_add(1);
        let mut bytes = Vec::new();
        file.take(limit).read_to_end(&mut bytes)?;
        if bytes.len() > MANIFEST_LIMIT {
            return Err(corrupt(name, "manifest.json слишком большой".to_owned()));
        }
        let raw = String::from_utf8(bytes)
            .map_err(|_| corrupt(name, "manifest.json не в UTF-8".to_owned()))?;
        let manifest = Manifest::parse(&raw).map_err(|error| corrupt(name, error.to_string()))?;
        if manifest.name != *name {
            return Err(corrupt(name, format!("в манифесте имя {}", manifest.name)));
        }
        Ok(manifest)
    }

    /// Staging directories a crash left behind; failures only cost disk space.
    fn sweep(&self) {
        let Ok(entries) = fs::read_dir(&self.root) else { return };
        for entry in entries.flatten() {
            let stale =
                entry.file_name().to_str().is_some_and(|name| name.starts_with(STAGING_PREFIX))
                    && entry
                        .metadata()
                        .and_then(|metadata| metadata.modified())
                        .ok()
                        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                        .is_some_and(|age| age > STALE_STAGING);
            if stale && let Err(error) = fs::remove_dir_all(entry.path()) {
                tracing::warn!(kind = ?error.kind(), "stale snapshot staging left behind");
            }
        }
    }
}

fn corrupt(name: &SnapshotName, reason: String) -> SnapshotError {
    SnapshotError::Corrupt { name: name.clone(), reason }
}

/// A path check that refused the snapshot's content is corruption; a real I/O error stays one.
fn refused(name: &SnapshotName, error: ArchiveError) -> SnapshotError {
    match error {
        ArchiveError::Io(error) => SnapshotError::Io(error),
        ArchiveError::NotFound
        | ArchiveError::Ambiguous
        | ArchiveError::Archived(_)
        | ArchiveError::Occupied { .. }
        | ArchiveError::Link(_)
        | ArchiveError::Unsupported(_)
        | ArchiveError::Malformed(_) => corrupt(name, error.to_string()),
    }
}

/// Makes the rename durable; a failure only weakens durability, so it is logged.
#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Err(error) = File::open(dir).and_then(|dir| dir.sync_all()) {
        tracing::warn!(kind = ?error.kind(), "snapshot directory not synced");
    }
}

/// Elsewhere the filesystem gives no handle to sync a directory.
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}
