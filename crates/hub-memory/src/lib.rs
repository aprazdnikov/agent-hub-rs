//! Durable project memory shared by execution backends.

mod cli;
mod model;
mod protocol;
mod stdio;
mod store;

pub use cli::run_cli;
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
