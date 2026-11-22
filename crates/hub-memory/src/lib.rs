//! Durable project memory shared by execution backends.

mod cli;
mod model;
mod protocol;
mod stdio;
mod store;

pub use cli::run_cli;

#[must_use]
pub fn default_base() -> Option<std::path::PathBuf> {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".agent-hub/memory"))
}

pub use model::{Entry, EntryKind, Error, SearchHit, WriteRequest};
pub use stdio::serve;
pub use store::{MemoryPage, MemoryReader, Store};

mod lifecycle;
pub use lifecycle::{
    AdminAction, AdminPreview, CleanupPolicy, CleanupSummary, EntryDetails, MemoryOverview,
    MemoryStatus, SnapshotToken, StatusFilter,
};

pub use store::portable::{
    ImportChoice, ImportConflict, ImportItem, ImportKind, ImportPreview, ImportRecord,
    MemoryExport, VerifiedExport,
};

pub use store::bindings::{BindingMode, BindingPreview};

mod project_cli;
pub use project_cli::run_project_cli;

mod presentation;

pub use store::context::{ContextBrief, ContextPacket, ContextRequest, ContextSelection};

/// Explicitly selected, versioned procedure; never injected into agent sessions.
pub const CONTINUE_WORK_V1: &str =
    include_str!("../../../docs/guide/procedures/continue-work-v1.md");
