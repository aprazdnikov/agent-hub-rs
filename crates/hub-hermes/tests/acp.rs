#![cfg(unix)]
#![allow(clippy::unwrap_used)]
#[path = "acp/backend.rs"]
mod backend;
mod common;
#[path = "acp/version.rs"]
mod version;
