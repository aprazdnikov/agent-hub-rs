use super::{
    CleanupPolicy, CleanupSummary, Error, MAX_BYTES, MAX_REVISIONS, MemoryReader, MemoryStatus,
    SCHEMA_VERSION, Snapshot, SnapshotToken, StatusFilter, Store, SystemTime, Tombstone,
    UNIX_EPOCH, initial_incarnation,
};
use crate::{AdminAction, AdminPreview, EntryDetails, MemoryOverview};
use std::collections::BTreeMap;

pub(super) fn now() -> Result<i64, Error> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Corrupt("system clock predates Unix epoch"))?
        .as_secs()
        .try_into()
        .map_err(|_| Error::Corrupt("timestamp overflow"))
}
impl Snapshot {
    pub(super) fn token(&self) -> SnapshotToken {
        SnapshotToken { incarnation: self.incarnation.clone(), generation: self.generation }
    }
    pub(super) fn status(&self, key: &str) -> MemoryStatus {
        self.metadata.get(key).map_or(MemoryStatus::Active, |meta| meta.status)
    }
    pub(super) fn encoded(&self) -> Result<Vec<u8>, Error> {
        serde_json::to_vec(self).map_err(|_| Error::Corrupt("could not serialize memory"))
    }
    fn remove_revision(&mut self, key: &str, revision: u32) {
        if let Some(history) = self.entries.get_mut(key) {
            history.retain(|entry| entry.revision != revision);
        }
        self.metadata.entry(key.to_owned()).or_default().pruned = true;
    }
    pub(super) fn enforce_retention(&mut self) -> Result<(), Error> {
        if self.policy == CleanupPolicy::Manual {
            return Ok(());
        }
        let mut removed = 0;
        for (key, history) in &mut self.entries {
            if history.len() > MAX_REVISIONS {
                let count = history.len() - MAX_REVISIONS;
                history.drain(..count);
                removed += count;
                self.metadata.entry(key.clone()).or_default().pruned = true;
            }
        }
        let mut candidates = Vec::new();
        for (key, history) in &self.entries {
            for entry in history.iter().take(history.len().saturating_sub(1)) {
                candidates.push((entry.created_at, key.clone(), entry.revision));
            }
        }
        candidates.sort();
        let timestamp = now()?;
        // Reserve the final generation's serialized width before judging the byte quota.
        let generation = self.generation;
        self.generation =
            generation.checked_add(1).ok_or(Error::LimitExceeded("generation range"))?;
        let original = self.clone();
        let candidate = |count: usize| {
            let mut result = original.clone();
            for (_, key, revision) in candidates.iter().take(count) {
                result.remove_revision(key, *revision);
            }
            if removed + count > 0 {
                result.last_cleanup = Some(CleanupSummary {
                    removed_revisions: removed + count,
                    performed_at: timestamp,
                });
            }
            result
        };
        let mut low = 0;
        let mut high = candidates.len();
        while low < high {
            let mid = low + (high - low) / 2;
            if candidate(mid).encoded()?.len() as u64 > MAX_BYTES {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        let mut result = candidate(low);
        if result.encoded()?.len() as u64 > MAX_BYTES {
            return Err(Error::LimitExceeded(
                "8 MiB per project; latest revisions cannot be removed automatically",
            ));
        }
        result.generation = generation;
        *self = result;
        Ok(())
    }
    fn apply_action(&mut self, action: &AdminAction) -> Result<(Vec<String>, usize), Error> {
        let mut removed_keys = Vec::new();
        let mut removed = 0;
        match action {
            AdminAction::SetPolicy(policy) => self.policy = *policy,
            AdminAction::SetStatus { key, status } => {
                if !self.entries.contains_key(key) {
                    return Err(Error::NotFound { key: key.clone(), revision: None });
                }
                self.metadata.entry(key.clone()).or_default().status = *status;
            }
            AdminAction::Prune { revisions } => {
                for (key, numbers) in revisions {
                    let history = self
                        .entries
                        .get(key)
                        .ok_or_else(|| Error::NotFound { key: key.clone(), revision: None })?;
                    let latest = history.last().ok_or(Error::Corrupt("empty history"))?.revision;
                    let unique: std::collections::BTreeSet<_> = numbers.iter().collect();
                    if unique.len() != numbers.len()
                        || numbers
                            .iter()
                            .any(|r| *r == latest || !history.iter().any(|e| e.revision == *r))
                    {
                        return Err(Error::InvalidInput(
                            "select existing historical revisions, never the latest",
                        ));
                    }
                    for revision in numbers {
                        self.remove_revision(key, *revision);
                        removed += 1;
                    }
                }
            }
            AdminAction::Delete { keys } => {
                for key in keys {
                    let history = self
                        .entries
                        .remove(key)
                        .ok_or_else(|| Error::NotFound { key: key.clone(), revision: None })?;
                    let revision = history.last().ok_or(Error::Corrupt("empty history"))?.revision;
                    let meta = self.metadata.remove(key).unwrap_or_default();
                    self.tombstones
                        .insert(key.clone(), Tombstone { revision, provenance: meta.provenance });
                    removed += history.len();
                    removed_keys.push(key.clone());
                }
            }
        }
        if removed > 0 {
            self.last_cleanup =
                Some(CleanupSummary { removed_revisions: removed, performed_at: now()? });
        }
        self.validate(&self.project)?;
        Ok((removed_keys, removed))
    }
}
impl Store {
    pub fn preview(&self, action: AdminAction) -> Result<AdminPreview, Error> {
        let mut snapshot = self.load()?;
        self.check_incarnation(&snapshot)?;
        let token = snapshot.token();
        let before_bytes = snapshot.encoded()?.len() as u64;
        let (removed_keys, removed_revisions) = snapshot.apply_action(&action)?;
        snapshot.schema_version = SCHEMA_VERSION;
        snapshot.generation =
            snapshot.generation.checked_add(1).ok_or(Error::LimitExceeded("generation range"))?;
        let after_bytes = snapshot.encoded()?.len() as u64;
        if after_bytes > MAX_BYTES {
            return Err(Error::LimitExceeded("8 MiB per project"));
        }
        Ok(AdminPreview {
            token,
            action,
            removed_keys,
            removed_revisions,
            before_bytes,
            after_bytes,
        })
    }
    pub fn apply(&self, preview: AdminPreview) -> Result<SnapshotToken, Error> {
        let AdminPreview { token, action, .. } = preview;
        let _lock = self.acquire_lock()?;
        let mut snapshot = self.load()?;
        self.check_incarnation(&snapshot)?;
        if snapshot.token() != token {
            return Err(Error::StalePreview);
        }
        snapshot.apply_action(&action)?;
        self.persist_snapshot(&mut snapshot)?;
        Ok(snapshot.token())
    }
}
impl MemoryReader {
    pub fn details(&self, key: &str, revision: Option<u32>) -> Result<EntryDetails, Error> {
        let store = self.store()?.ok_or_else(|| Error::NotFound { key: key.into(), revision })?;
        store.details(key, revision)
    }
    pub fn overview(
        &self,
        query: &str,
        offset: usize,
        limit: usize,
        filter: StatusFilter,
    ) -> Result<MemoryOverview, Error> {
        let (page, snapshot) = self.inspect_filtered(query, offset, limit, filter)?;
        Ok(MemoryOverview {
            query: query.to_owned(),
            filter,
            token: snapshot.as_ref().map_or_else(
                || SnapshotToken { incarnation: initial_incarnation(&self.project), generation: 0 },
                Snapshot::token,
            ),
            policy: snapshot.as_ref().map_or(CleanupPolicy::Manual, |s| s.policy),
            statuses: snapshot.as_ref().map_or_else(BTreeMap::new, |s| {
                page.entries.iter().map(|entry| (entry.key.clone(), s.status(&entry.key))).collect()
            }),
            revision_count: snapshot.as_ref().map_or(0, |s| s.entries.values().map(Vec::len).sum()),
            page,
            tombstone_count: snapshot.as_ref().map_or(0, |s| s.tombstones.len()),
            last_cleanup: snapshot.and_then(|s| s.last_cleanup),
        })
    }
}

impl Store {
    pub fn details(&self, key: &str, revision: Option<u32>) -> Result<EntryDetails, Error> {
        self.load()?.details(key, revision)
    }
}
impl Snapshot {
    pub(super) fn details(&self, key: &str, revision: Option<u32>) -> Result<EntryDetails, Error> {
        let history =
            self.entries.get(key).ok_or_else(|| Error::NotFound { key: key.into(), revision })?;
        let entry = match revision {
            Some(number) => history.iter().find(|e| e.revision == number),
            None => history.last(),
        }
        .cloned()
        .ok_or_else(|| Error::RevisionUnavailable {
            key: key.into(),
            revision: revision.unwrap_or(0),
            available: history.iter().map(|entry| entry.revision).collect(),
            pruned: self.metadata.get(key).is_some_and(|meta| meta.pruned),
        })?;
        Ok(EntryDetails {
            entry,
            status: self.status(key),
            revisions: history.iter().map(|e| e.revision).collect(),
            history_pruned: self.metadata.get(key).is_some_and(|m| m.pruned),
        })
    }
}
