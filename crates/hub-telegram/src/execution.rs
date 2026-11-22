//! Transport-neutral control of one durable task execution.
use crate::hub::tasks::Request;
use hub_core::{
    domain::{Question, ToolRequest},
    settings::ExecutionSettings,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Permission(ToolRequest),
    Questions(Vec<Question>),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interaction {
    pub id: Uuid,
    pub kind: Kind,
}
#[derive(Debug, Clone)]
pub enum Answer {
    Allow,
    Deny,
    Text(Vec<String>),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Live {
    pub token: Uuid,
    pub project: PathBuf,
    pub id: u64,
    /// Durable attempt created by this operation; absent for Git/preflight failures.
    pub run: Option<u32>,
    pub active: bool,
    pub status: String,
    pub interaction: Option<Interaction>,
}

pub trait Control: Send + Sync + 'static {
    fn busy(&self) -> bool;
    fn snapshot(&self) -> Option<Live>;
    fn snapshots(&self) -> Vec<Live> {
        self.snapshot().into_iter().collect()
    }
    fn capacity(&self, _isolated: bool) -> bool {
        !self.busy()
    }
    fn snapshot_for(&self, project: &Path, id: u64) -> Option<Live> {
        self.snapshot().filter(|live| live.id == id && live.project == project)
    }
    fn dispatch(
        &self,
        project: &Path,
        request: Request,
        settings: Arc<ExecutionSettings>,
    ) -> Result<(), String>;
    fn stop(&self, token: Uuid) -> Result<(), String>;
    fn stop_preflight(&self, token: Uuid) -> Result<(), String> {
        if self
            .snapshots()
            .iter()
            .any(|live| live.token == token && live.active && live.run.is_none())
        {
            self.stop(token)
        } else {
            Err("Подготовка уже завершена".into())
        }
    }
    fn respond(&self, token: Uuid, id: Uuid, answer: Answer) -> Result<(), String>;
}
