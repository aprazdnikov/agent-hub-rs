//! The human on the other side: approves tools, answers questions, receives files.

use futures::future::BoxFuture;
use hub_core::domain::{
    Decision, FileDelivery, OutgoingFile, Question, QuestionsOutcome, ToolRequest,
};

/// Boxed futures keep the trait object-safe, so the backend does not depend on the messenger.
pub trait UserChannel: Send + Sync {
    fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision>;
    fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome>;
    fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery>;
}
