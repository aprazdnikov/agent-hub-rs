use std::{
    collections::BTreeMap,
    fs::{self, File, TryLockError},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    CleanupPolicy, CleanupSummary, Entry, Error, MemoryStatus, SearchHit, SnapshotToken,
    StatusFilter, WriteRequest,
};
pub(crate) mod bindings;
pub(crate) mod context;
mod lifecycle;
pub(crate) mod portable;

const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_KEYS: usize = 500;
const MAX_REVISIONS: usize = 32;
const SCHEMA_VERSION: u32 = 2;
const DATA_FILE: &str = "memory.json";
const LOCK_FILE: &str = ".lock";

/// A project-scoped store. Clones and independent processes read the same durable state.
#[derive(Clone)]
pub struct Store {
    directory: Arc<filesystem::Directory>,
    project: PathBuf,
    actor: String,
    incarnation: String,
    binding: bindings::Location,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    project: PathBuf,
    #[serde(default)]
    incarnation: String,
    #[serde(default)]
    generation: u64,
    #[serde(default)]
    policy: CleanupPolicy,
    #[serde(default, deserialize_with = "unique_map")]
    metadata: BTreeMap<String, Metadata>,
    #[serde(default, deserialize_with = "unique_map")]
    tombstones: BTreeMap<String, Tombstone>,
    #[serde(default)]
    last_cleanup: Option<CleanupSummary>,
    #[serde(deserialize_with = "unique_map")]
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
        let binding = bindings::Location::resolve(base, &project)?;
        let directory = binding.directory()?;
        let project_id = directory
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(Error::Corrupt("invalid project ID"))?;
        let mut store = Self {
            directory: Arc::new(filesystem::Directory::create(&binding.base, project_id)?),
            project: binding.identity.clone(),
            binding,
            actor: actor.to_owned(),
            incarnation: String::new(),
        };
        store.directory.open(LOCK_FILE, filesystem::OpenMode::Create)?;
        store.incarnation = store.load()?.incarnation;
        Ok(store)
    }

    /// Atomically appends a revision, or returns the original result of an identical retry.
    pub fn write(&self, request: WriteRequest) -> Result<Entry, Error> {
        validate_fields(&request.key, &request.title, &request.body, &request.sources)?;
        let _lock = self.acquire_lock()?;
        let mut snapshot = self.load()?;
        self.check_incarnation(&snapshot)?;
        if snapshot.tombstones.contains_key(&request.key) {
            return Err(Error::Deleted);
        }
        if snapshot.status(&request.key) == MemoryStatus::Archived {
            return Err(Error::Archived);
        }
        if let Some(existing) = snapshot.entries.get(&request.key) {
            if let Some(retry) = request
                .expected_revision
                .checked_add(1)
                .and_then(|revision| existing.iter().find(|entry| entry.revision == revision))
                .filter(|entry| same_payload(entry, &request))
            {
                return Ok(retry.clone());
            }
            let actual = existing.last().map_or(0, |entry| entry.revision);
            ensure_revision(&request, actual)?;
            if existing.len() >= MAX_REVISIONS && snapshot.policy == CleanupPolicy::Manual {
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
        snapshot.enforce_retention()?;
        self.persist_snapshot(&mut snapshot)?;
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
        self.search_filtered(query, limit, StatusFilter::Active)
    }

    pub fn search_filtered(
        &self,
        query: &str,
        limit: usize,
        filter: StatusFilter,
    ) -> Result<Vec<SearchHit>, Error> {
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
                .filter(|entry| filter.includes(snapshot.status(&entry.key)))
                .filter(|entry| {
                    let text = format!(
                        "{}\n{}\n{}\n{}",
                        entry.key,
                        entry.title,
                        entry.body,
                        entry.sources.join("\n")
                    )
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

    fn check_incarnation(&self, snapshot: &Snapshot) -> Result<(), Error> {
        if self.incarnation != snapshot.incarnation {
            return Err(Error::Recreated);
        }
        Ok(())
    }

    fn persist_snapshot(&self, snapshot: &mut Snapshot) -> Result<(), Error> {
        snapshot.schema_version = SCHEMA_VERSION;
        snapshot.generation = snapshot
            .generation
            .checked_add(1)
            .ok_or(Error::LimitExceeded("snapshot generation range"))?;
        snapshot.validate(&self.project)?;
        let bytes = serde_json::to_vec(snapshot)
            .map_err(|_| Error::Corrupt("could not serialize memory"))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Error::LimitExceeded("8 MiB per project"));
        }
        if let Some(file) = self.directory.open(DATA_FILE, filesystem::OpenMode::Read)? {
            let mut original = Vec::new();
            file.take(MAX_BYTES + 1).read_to_end(&mut original)?;
            let header: Header = serde_json::from_slice(&original)
                .map_err(|_| Error::Corrupt("invalid schema header"))?;
            if header.schema_version == 1 {
                self.directory.backup(&original)?;
            }
        }
        self.directory.persist(&bytes)
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
        Ok(self.load_with_size()?.0)
    }

    fn load_with_size(&self) -> Result<(Snapshot, u64), Error> {
        self.binding.check()?;
        let Some(file) = self.directory.open(DATA_FILE, filesystem::OpenMode::Read)? else {
            return Ok((
                Snapshot {
                    schema_version: SCHEMA_VERSION,
                    project: self.project.clone(),
                    entries: BTreeMap::new(),
                    incarnation: initial_incarnation(&self.project),
                    generation: 0,
                    policy: CleanupPolicy::Manual,
                    metadata: BTreeMap::new(),
                    tombstones: BTreeMap::new(),
                    last_cleanup: None,
                },
                0,
            ));
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
        if !matches!(header.schema_version, 1 | SCHEMA_VERSION) {
            return Err(Error::UnsupportedVersion(header.schema_version));
        }
        let mut snapshot: Snapshot = serde_json::from_slice(&bytes)
            .map_err(|_| Error::Corrupt("invalid JSON or schema fields"))?;
        if snapshot.schema_version == 1 {
            snapshot.incarnation = initial_incarnation(&self.project);
            let hash = Sha256::digest(&bytes);
            let mut prefix = [0u8; 8];
            for (target, byte) in prefix.iter_mut().zip(hash.iter()) {
                *target = *byte;
            }
            snapshot.generation = u64::from_be_bytes(prefix) >> 1;
        }
        snapshot.validate(&self.project)?;
        Ok((snapshot, bytes.len() as u64))
    }
}

impl Snapshot {
    fn validate(&self, project: &Path) -> Result<(), Error> {
        if self.project != project {
            return Err(Error::ProjectMismatch);
        }
        if self.incarnation.len() > 128 || self.incarnation.is_empty() {
            return Err(Error::Corrupt("invalid incarnation"));
        }
        if self.tombstones.len() > 4096 {
            return Err(Error::LimitExceeded("4096 tombstones"));
        }
        for (key, dead) in &self.tombstones {
            validate_key(key)?;
            if dead.revision == 0 || self.entries.contains_key(key) {
                return Err(Error::Corrupt("invalid tombstone"));
            }
        }
        if self.metadata.keys().any(|key| !self.entries.contains_key(key)) {
            return Err(Error::Corrupt("orphan entry metadata"));
        }
        if self.entries.len() > MAX_KEYS {
            return Err(Error::Corrupt("key count exceeds the schema limit"));
        }
        for (key, history) in &self.entries {
            if history.is_empty() || history.len() > MAX_REVISIONS {
                return Err(Error::Corrupt("invalid revision count"));
            }
            let mut previous = 0;
            for (index, entry) in history.iter().enumerate() {
                if key != &entry.key
                    || entry.revision <= previous
                    || (self.schema_version == 1
                        && usize::try_from(entry.revision).ok() != index.checked_add(1))
                {
                    return Err(Error::Corrupt("entry key or revision sequence is invalid"));
                }
                previous = entry.revision;
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

fn unique_map<'de, D, T>(deserializer: D) -> Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Unique<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Unique<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("unique keys")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, T>()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate key"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Unique(std::marker::PhantomData))
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Metadata {
    #[serde(default)]
    status: MemoryStatus,
    #[serde(default)]
    pruned: bool,
    #[serde(default)]
    provenance: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct Tombstone {
    revision: u32,
    provenance: Option<String>,
}

mod filesystem;

/// A read-only view that never creates a store or changes its permissions.
#[derive(Clone, Debug)]
pub struct MemoryReader {
    project: PathBuf,
    directory: PathBuf,
    binding: bindings::Location,
}

#[derive(Clone, Debug)]
pub struct MemoryPage {
    pub bound_projects: Vec<PathBuf>,
    pub offset: usize,
    pub project: PathBuf,
    pub directory: PathBuf,
    pub exists: bool,
    pub bytes: u64,
    pub key_count: usize,
    pub matched_count: usize,
    pub entries: Vec<SearchHit>,
}

impl MemoryReader {
    pub fn new(base: &Path, project: &Path) -> Result<Self, Error> {
        let project = fs::canonicalize(project)?;
        if !project.is_dir() {
            return Err(Error::InvalidInput("project must be an existing directory"));
        }
        let binding = bindings::Location::resolve(base, &project)?;
        Ok(Self { directory: binding.directory()?, project, binding })
    }

    fn store(&self) -> Result<Option<Store>, Error> {
        self.binding.check()?;
        Ok(filesystem::Directory::existing(&self.directory)?.map(|directory| Store {
            directory: Arc::new(directory),
            project: self.binding.identity.clone(),
            binding: self.binding.clone(),
            actor: "viewer".into(),
            incarnation: String::new(),
        }))
    }

    pub fn get(&self, key: &str, revision: Option<u32>) -> Result<Entry, Error> {
        validate_key(key)?;
        self.store()?
            .ok_or_else(|| Error::NotFound { key: key.to_owned(), revision })?
            .get(key, revision)
    }

    pub fn inspect(&self, query: &str, offset: usize, limit: usize) -> Result<MemoryPage, Error> {
        self.inspect_filtered(query, offset, limit, StatusFilter::Active)
    }

    fn inspect_filtered(
        &self,
        query: &str,
        offset: usize,
        limit: usize,
        filter: StatusFilter,
    ) -> Result<MemoryPage, Error> {
        if query.len() > 512 || !(1..=20).contains(&limit) {
            return Err(Error::InvalidInput("query must be <=512 bytes and limit 1..20"));
        }
        let mut page = MemoryPage {
            bound_projects: self.binding.projects()?,
            offset,
            project: self.project.clone(),
            directory: self.directory.clone(),
            exists: false,
            bytes: 0,
            key_count: 0,
            matched_count: 0,
            entries: Vec::new(),
        };
        let Some(store) = self.store()? else { return Ok(page) };
        let (snapshot, bytes) = store.load_with_size()?;
        page.exists = bytes > 0;
        page.bytes = bytes;
        page.key_count = snapshot.entries.len();
        let terms: Vec<_> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut entries: Vec<_> = snapshot
            .entries
            .values()
            .filter_map(|history| history.last())
            .filter(|entry| filter.includes(snapshot.status(&entry.key)))
            .filter(|entry| {
                let text = format!(
                    "{}\n{}\n{}\n{}",
                    entry.key,
                    entry.title,
                    entry.body,
                    entry.sources.join("\n")
                )
                .to_lowercase();
                terms.iter().all(|term| text.contains(term))
            })
            .collect();
        entries.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.key.cmp(&b.key)));
        page.matched_count = entries.len();
        page.entries = entries
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|entry| SearchHit {
                key: entry.key.clone(),
                revision: entry.revision,
                kind: entry.kind,
                title: entry.title.clone(),
                excerpt: entry.body.chars().take(400).collect(),
                created_at: entry.created_at,
            })
            .collect();
        Ok(page)
    }
}

fn initial_incarnation(project: &Path) -> String {
    portable::digest(project.as_os_str().as_encoded_bytes())
}
