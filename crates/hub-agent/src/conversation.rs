//! What a backend needs to run one conversation: the human, the event sink, cancellation, limits.

use std::sync::Arc;
use std::time::Duration;

use hub_core::domain::AgentEvent;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::channel::UserChannel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// How long background tasks may run after the agent's answer.
    pub background: Duration,
    /// After a task reports, the CLI starts a turn of its own within this window.
    pub settle: Duration,
    pub initialize: Duration,
}

impl Limits {
    #[must_use]
    pub const fn new(background: Duration) -> Self {
        Self { background, settle: Duration::from_secs(30), initialize: Duration::from_mins(1) }
    }
}

pub struct Conversation {
    pub channel: Arc<dyn UserChannel>,
    pub events: mpsc::Sender<AgentEvent>,
    pub cancel: CancellationToken,
    pub limits: Limits,
}
