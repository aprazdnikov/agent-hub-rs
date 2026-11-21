//! Human administration types; none are exposed as agent MCP tools.
use crate::{Entry, MemoryPage};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    #[default]
    Active,
    Stale,
    Archived,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusFilter {
    #[default]
    Active,
    Stale,
    Archived,
    All,
}
impl StatusFilter {
    pub(crate) fn includes(self, status: MemoryStatus) -> bool {
        matches!(
            (self, status),
            (Self::All, _)
                | (Self::Active, MemoryStatus::Active)
                | (Self::Stale, MemoryStatus::Stale)
                | (Self::Archived, MemoryStatus::Archived)
        )
    }
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPolicy {
    #[default]
    Manual,
    Automatic,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotToken {
    pub incarnation: String,
    pub generation: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct EntryDetails {
    #[serde(flatten)]
    pub entry: Entry,
    pub status: MemoryStatus,
    pub revisions: Vec<u32>,
    pub history_pruned: bool,
}
#[derive(Clone, Debug)]
pub struct MemoryOverview {
    pub query: String,
    pub filter: StatusFilter,
    pub token: SnapshotToken,
    pub policy: CleanupPolicy,
    pub page: MemoryPage,
    pub tombstone_count: usize,
    pub last_cleanup: Option<CleanupSummary>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupSummary {
    pub removed_revisions: usize,
    pub performed_at: i64,
}
#[derive(Clone, Debug)]
pub enum AdminAction {
    SetPolicy(CleanupPolicy),
    SetStatus { key: String, status: MemoryStatus },
    Prune { revisions: BTreeMap<String, Vec<u32>> },
    Delete { keys: Vec<String> },
}
#[derive(Clone, Debug)]
pub struct AdminPreview {
    pub token: SnapshotToken,
    pub action: AdminAction,
    pub removed_keys: Vec<String>,
    pub removed_revisions: usize,
    pub before_bytes: u64,
    pub after_bytes: u64,
}
