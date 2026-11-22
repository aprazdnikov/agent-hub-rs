//! Async boundary for locally authorized Git operations, implemented by the desktop host.
use futures::future::BoxFuture;
use hub_core::tasks::git::Preparation;
use std::path::PathBuf;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug)]
pub struct Request {
    pub project: PathBuf,
    pub preparation: Preparation,
    /// Read-only launch check; continuation permits the agent's own uncommitted edits.
    pub verify_only: bool,
    pub continuation: bool,
}
pub trait GitRunner: Send + Sync + 'static {
    fn run(
        &self,
        request: Request,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<String, String>>;
}
