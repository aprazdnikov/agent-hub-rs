use std::{io, path::PathBuf};

use serde::{Deserialize, Serialize};

/// The purpose of an explicitly saved memory entry.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Context,
    Decision,
    Progress,
    Artifact,
}

/// A new entry revision, guarded by the revision the caller read.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WriteRequest {
    pub key: String,
    pub expected_revision: u32,
    pub kind: EntryKind,
    pub title: String,
    pub body: String,
    pub sources: Vec<String>,
}

/// An immutable, attributed revision of an entry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub key: String,
    pub revision: u32,
    pub kind: EntryKind,
    pub title: String,
    pub body: String,
    pub sources: Vec<String>,
    pub author: String,
    pub created_at: i64,
}

/// A bounded preview; use `Store::get` to retrieve the complete entry.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SearchHit {
    pub key: String,
    pub revision: u32,
    pub kind: EntryKind,
    pub title: String,
    pub excerpt: String,
    pub created_at: i64,
}

/// Failures contain metadata and safe diagnostics, never entry contents.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    ContextInput(&'static str),
    #[error("context package requires {required} bytes, budget is {limit}")]
    ContextBudget { required: usize, limit: usize },
    #[error("project binding publication is uncertain for '{}': {cause}; check the destination before retrying", target.display())]
    BindingPublicationUncertain { target: PathBuf, cause: Box<Error> },
    #[error("project memory binding changed or was moved; reopen the project in a new session")]
    BindingChanged,
    #[error(
        "destination already has memory or a binding; use export/import to resolve it explicitly"
    )]
    BindingOccupied,
    #[error("memory store was recreated; start a new session")]
    Recreated,
    #[error("memory preview is stale; refresh before confirming")]
    StalePreview,
    #[error("memory entry is archived; activate it before writing")]
    Archived,
    #[error("memory key was deleted; import under a new key")]
    Deleted,
    #[error(
        "memory entry '{key}' revision {revision} unavailable; available: {available:?}; history pruned: {pruned}"
    )]
    RevisionUnavailable { key: String, revision: u32, available: Vec<u32>, pruned: bool },
    #[error("invalid memory input: {0}")]
    InvalidInput(&'static str),
    #[error("memory entry '{key}' revision {revision:?} was not found")]
    NotFound { key: String, revision: Option<u32> },
    #[error(
        "memory entry '{key}' changed: expected revision {expected}, current revision {actual}"
    )]
    Conflict { key: String, expected: u32, actual: u32 },
    #[error("memory store is busy; retry the operation")]
    Busy,
    #[error("memory limit exceeded: {0}")]
    LimitExceeded(&'static str),
    #[error("memory store is invalid: {0}")]
    Corrupt(&'static str),
    #[error("unsupported memory schema version {0}")]
    UnsupportedVersion(u32),
    #[error("unsafe memory path: {}", .0.display())]
    UnsafePath(PathBuf),
    #[error("memory store belongs to a different project")]
    ProjectMismatch,
    #[error("memory storage I/O failed: {0}")]
    Io(#[from] io::Error),
}
