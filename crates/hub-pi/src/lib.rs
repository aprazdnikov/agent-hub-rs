//! `Pi` integration, isolated from the other agent backends.
//!
//! Pi is not an ACP agent: it runs its own JSON-lines RPC mode, mapped in [`protocol`] and
//! carried by [`backend`]. Approvals and hub tools reach the child through the bridge extension
//! in [`requests`].

mod link;
mod protocol;
mod requests;

pub mod backend;
pub mod version;
