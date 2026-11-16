//! Versioned, bounded export/import. All incoming content remains untrusted data.
use super::{
    Arc, BTreeMap, Deserialize, Digest, Error, File, MAX_BYTES, MemoryReader, Path, PathBuf, Read,
    SCHEMA_VERSION, Serialize, Sha256, Snapshot, SnapshotToken, Store, fs, validate_key,
};
use std::io::Write;

const MAX_IMPORT_BYTES: u64 = 16 * 1024 * 1024;
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryExport {
    format_version: u32,
    snapshot: Arc<Snapshot>,
}
#[derive(Clone, Debug)]
pub struct VerifiedExport {
    token: SnapshotToken,
    digest: String,
    path: PathBuf,
    temporary: Option<Arc<tempfile::TempDir>>,
}
#[derive(Clone, Debug)]
pub enum ImportChoice {
    KeepLocal,
    CopyAs(String),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportKind {
    New,
    Identical,
    Continued,
    Conflict,
    Copied(String),
}
#[derive(Clone, Debug)]
pub struct ImportItem {
    pub key: String,
    pub kind: ImportKind,
}
#[derive(Clone, Debug)]
pub struct ImportPreview {
    pub token: SnapshotToken,
    pub source: PathBuf,
    pub target: PathBuf,
    pub items: Vec<ImportItem>,
    pub after_bytes: u64,
    local: Arc<Snapshot>,
    export: MemoryExport,
    choices: BTreeMap<String, ImportChoice>,
}
pub(super) fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().fold(String::with_capacity(64), |mut text, byte| {
        use std::fmt::Write;
        let _ = write!(text, "{byte:02x}");
        text
    })
}
impl MemoryExport {
    pub fn decode(reader: impl Read) -> Result<Self, Error> {
        let mut bytes = Vec::new();
        reader.take(MAX_IMPORT_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_IMPORT_BYTES {
            return Err(Error::LimitExceeded("16 MiB import"));
        }
        let export: Self =
            serde_json::from_slice(&bytes).map_err(|_| Error::Corrupt("invalid export JSON"))?;
        if export.format_version != 1 || export.snapshot.schema_version != SCHEMA_VERSION {
            return Err(Error::UnsupportedVersion(export.format_version));
        }
        export.snapshot.validate(&export.snapshot.project)?;
        Ok(export)
    }
    pub fn write_temporary(&self) -> Result<VerifiedExport, Error> {
        let directory = Arc::new(tempfile::tempdir()?);
        let mut receipt = self.write_new(&directory.path().join("memory-export.json"))?;
        receipt.temporary = Some(directory);
        Ok(receipt)
    }
    pub fn write_new(&self, path: &Path) -> Result<VerifiedExport, Error> {
        let bytes =
            serde_json::to_vec(self).map_err(|_| Error::Corrupt("could not serialize export"))?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let receipt = VerifiedExport {
            token: self.snapshot.token(),
            digest: digest(&bytes),
            path: path.to_owned(),
            temporary: None,
        };
        receipt.verify()?;
        Ok(receipt)
    }
}
impl VerifiedExport {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn verify(&self) -> Result<(), Error> {
        let metadata = fs::symlink_metadata(&self.path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::UnsafePath(self.path.clone()));
        }
        let mut bytes = Vec::new();
        File::open(&self.path)?.take(MAX_IMPORT_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_IMPORT_BYTES || digest(&bytes) != self.digest {
            return Err(Error::Corrupt("verified export changed"));
        }
        Ok(())
    }
}
impl MemoryReader {
    pub fn export(&self) -> Result<MemoryExport, Error> {
        let mut snapshot =
            self.store()?.ok_or(Error::InvalidInput("project has no memory to export"))?.load()?;
        snapshot.schema_version = SCHEMA_VERSION;
        Ok(MemoryExport { format_version: 1, snapshot: Arc::new(snapshot) })
    }
}
fn fingerprint(snapshot: &Snapshot, key: &str) -> Result<String, Error> {
    let value = (
        &snapshot.project,
        key,
        snapshot.entries.get(key),
        snapshot.metadata.get(key).cloned().unwrap_or_default(),
        snapshot.tombstones.get(key),
    );
    Ok(digest(
        &serde_json::to_vec(&value).map_err(|_| Error::Corrupt("invalid import fingerprint"))?,
    ))
}
fn classification(local: &Snapshot, incoming: &Snapshot, key: &str) -> ImportKind {
    if incoming.tombstones.contains_key(key) {
        return if local.tombstones.get(key) == incoming.tombstones.get(key) {
            ImportKind::Identical
        } else if local.entries.contains_key(key) || local.tombstones.contains_key(key) {
            ImportKind::Conflict
        } else {
            ImportKind::New
        };
    }
    if local.tombstones.contains_key(key) {
        return ImportKind::Conflict;
    }
    let Some(ours) = local.entries.get(key) else {
        return ImportKind::New;
    };
    let Some(theirs) = incoming.entries.get(key) else {
        return ImportKind::Conflict;
    };
    let ours_meta = local.metadata.get(key).cloned().unwrap_or_default();
    let theirs_meta = incoming.metadata.get(key).cloned().unwrap_or_default();
    if ours_meta != theirs_meta {
        return ImportKind::Conflict;
    }
    let common: Vec<_> = theirs
        .iter()
        .filter_map(|entry| ours.iter().find(|e| e.revision == entry.revision).map(|e| (e, entry)))
        .collect();
    if common.is_empty() || common.iter().any(|(a, b)| a != b) {
        return ImportKind::Conflict;
    }
    let latest = ours.last().map_or(0, |e| e.revision);
    if theirs.iter().all(|e| e.revision <= latest) {
        if theirs.iter().all(|e| ours.iter().any(|o| o == e)) {
            ImportKind::Identical
        } else {
            ImportKind::Conflict
        }
    } else if theirs.iter().any(|e| e.revision <= latest && !ours.iter().any(|o| o == e)) {
        ImportKind::Conflict
    } else {
        ImportKind::Continued
    }
}
impl Snapshot {
    fn import(
        &mut self,
        export: &MemoryExport,
        choices: &BTreeMap<String, ImportChoice>,
    ) -> Result<Vec<ImportItem>, Error> {
        let incoming = &export.snapshot;
        let keys: std::collections::BTreeSet<_> =
            incoming.entries.keys().chain(incoming.tombstones.keys()).cloned().collect();
        if choices.keys().any(|key| !keys.contains(key)) {
            return Err(Error::InvalidInput("import choice refers to unknown key"));
        }
        let mut items = Vec::new();
        let mut targets = std::collections::BTreeSet::new();
        for key in &keys {
            let key = key.clone();
            let hash = fingerprint(incoming, &key)?;
            if self.metadata.values().any(|m| m.provenance.as_ref() == Some(&hash))
                || self.tombstones.values().any(|t| t.provenance.as_ref() == Some(&hash))
            {
                items.push(ImportItem { key, kind: ImportKind::Identical });
                continue;
            }
            let mut kind = classification(self, incoming, &key);
            if let Some(ImportChoice::CopyAs(target)) = choices.get(&key) {
                validate_key(target)?;
                if keys.contains(target)
                    || !targets.insert(target.clone())
                    || self.entries.contains_key(target)
                    || self.tombstones.contains_key(target)
                    || incoming.tombstones.contains_key(&key)
                {
                    return Err(Error::InvalidInput(
                        "copy target must be a unique unused key for a live record",
                    ));
                }
                let mut history = incoming
                    .entries
                    .get(&key)
                    .cloned()
                    .ok_or(Error::Corrupt("missing incoming history"))?;
                for entry in &mut history {
                    entry.key.clone_from(target);
                }
                self.entries.insert(target.clone(), history);
                let mut meta = incoming.metadata.get(&key).cloned().unwrap_or_default();
                meta.provenance = Some(hash);
                self.metadata.insert(target.clone(), meta);
                kind = ImportKind::Copied(target.clone());
            } else if !matches!(choices.get(&key), Some(ImportChoice::KeepLocal)) {
                match kind {
                    ImportKind::New => {
                        if !targets.insert(key.clone()) {
                            return Err(Error::InvalidInput("duplicate import target"));
                        }
                        if let Some(history) = incoming.entries.get(&key) {
                            self.entries.insert(key.clone(), history.clone());
                            self.metadata.insert(
                                key.clone(),
                                incoming.metadata.get(&key).cloned().unwrap_or_default(),
                            );
                        } else if let Some(tombstone) = incoming.tombstones.get(&key) {
                            self.tombstones.insert(key.clone(), tombstone.clone());
                        }
                    }
                    ImportKind::Continued => {
                        let ours = self
                            .entries
                            .get_mut(&key)
                            .ok_or(Error::Corrupt("missing local history"))?;
                        let latest = ours.last().map_or(0, |e| e.revision);
                        if let Some(theirs) = incoming.entries.get(&key) {
                            ours.extend(theirs.iter().filter(|e| e.revision > latest).cloned());
                        }
                    }
                    ImportKind::Identical | ImportKind::Conflict | ImportKind::Copied(_) => {}
                }
            }
            items.push(ImportItem { key, kind });
        }
        self.validate(&self.project)?;
        Ok(items)
    }
}
impl Store {
    pub fn preview_import(
        &self,
        export: MemoryExport,
        choices: BTreeMap<String, ImportChoice>,
    ) -> Result<ImportPreview, Error> {
        let mut snapshot = self.load()?;
        self.check_incarnation(&snapshot)?;
        let token = snapshot.token();
        let local = Arc::new(snapshot.clone());
        let items = snapshot.import(&export, &choices)?;
        snapshot.schema_version = SCHEMA_VERSION;
        snapshot.generation =
            snapshot.generation.checked_add(1).ok_or(Error::LimitExceeded("generation range"))?;
        let after_bytes = snapshot.encoded()?.len() as u64;
        if after_bytes > MAX_BYTES {
            return Err(Error::LimitExceeded("8 MiB per project; import does not auto-prune"));
        }
        Ok(ImportPreview {
            token,
            local,
            source: export.snapshot.project.clone(),
            target: self.binding.cwd.clone(),
            items,
            after_bytes,
            export,
            choices,
        })
    }
    pub fn apply_import(&self, preview: ImportPreview) -> Result<SnapshotToken, Error> {
        let ImportPreview { token, export, choices, .. } = preview;
        let _lock = self.acquire_lock()?;
        let mut snapshot = self.load()?;
        self.check_incarnation(&snapshot)?;
        if snapshot.token() != token {
            return Err(Error::StalePreview);
        }
        let before = snapshot.encoded()?;
        snapshot.import(&export, &choices)?;
        if before != snapshot.encoded()? {
            self.persist_snapshot(&mut snapshot)?;
        }
        Ok(snapshot.token())
    }
    pub fn reset(
        &self,
        token: SnapshotToken,
        keep: Vec<String>,
        export: VerifiedExport,
    ) -> Result<SnapshotToken, Error> {
        let _lock = self.acquire_lock()?;
        let mut snapshot = self.load()?;
        self.check_incarnation(&snapshot)?;
        if snapshot.token() != token || export.token != token {
            return Err(Error::StalePreview);
        }
        export.verify()?;
        let mut current = snapshot.clone();
        current.schema_version = SCHEMA_VERSION;
        let exported = MemoryExport { format_version: 1, snapshot: Arc::new(current) };
        let bytes = serde_json::to_vec(&exported)
            .map_err(|_| Error::Corrupt("could not verify current export"))?;
        if digest(&bytes) != export.digest {
            return Err(Error::Corrupt("export does not contain the current complete snapshot"));
        }
        let count = keep.len();
        let keys: std::collections::BTreeSet<_> = keep.into_iter().collect();
        if keys.len() != count || keys.iter().any(|key| !snapshot.entries.contains_key(key)) {
            return Err(Error::InvalidInput("select existing unique keys"));
        }
        snapshot.entries.retain(|key, _| keys.contains(key));
        snapshot.metadata.retain(|key, _| keys.contains(key));
        snapshot.tombstones.clear();
        snapshot.incarnation = uuid::Uuid::new_v4().to_string();
        self.persist_snapshot(&mut snapshot)?;
        drop(export);
        drop(token);
        Ok(snapshot.token())
    }
}

#[derive(Clone, Debug)]
pub struct ImportRecord<'a> {
    pub history: &'a [crate::Entry],
    pub status: Option<crate::MemoryStatus>,
    pub deleted_revision: Option<u32>,
    pub history_pruned: bool,
}
#[derive(Clone, Debug)]
pub struct ImportConflict<'a> {
    pub local: ImportRecord<'a>,
    pub incoming: ImportRecord<'a>,
}
impl ImportPreview {
    #[must_use]
    pub fn source_export(&self) -> MemoryExport {
        self.export.clone()
    }
    #[must_use]
    pub fn choices(&self) -> BTreeMap<String, ImportChoice> {
        self.choices.clone()
    }
    pub fn conflict(&self, key: &str) -> Result<ImportConflict<'_>, Error> {
        if !self.items.iter().any(|item| item.key == key) {
            return Err(Error::InvalidInput("key is absent from import preview"));
        }
        Ok(ImportConflict {
            local: self.local.import_record(key),
            incoming: self.export.snapshot.import_record(key),
        })
    }
}
impl Snapshot {
    fn import_record(&self, key: &str) -> ImportRecord<'_> {
        ImportRecord {
            history: self.entries.get(key).map_or(&[], Vec::as_slice),
            status: self.entries.contains_key(key).then(|| self.status(key)),
            deleted_revision: self.tombstones.get(key).map(|dead| dead.revision),
            history_pruned: self.metadata.get(key).is_some_and(|meta| meta.pruned),
        }
    }
}
