//! A scripted human for tests.

use std::sync::Mutex;

use futures::future::BoxFuture;
use hub_agent::channel::UserChannel;
use hub_core::domain::{
    Decision, FileDelivery, OutgoingFile, Question, QuestionsOutcome, ToolRequest,
};

/// A human who answers every request the same scripted way and remembers what was asked.
pub struct Scripted {
    pub decision: Decision,
    pub outcome: QuestionsOutcome,
    pub delivery: FileDelivery,
    pub requests: Mutex<Vec<ToolRequest>>,
    pub questions: Mutex<Vec<Question>>,
}

impl Default for Scripted {
    fn default() -> Self {
        Self {
            decision: Decision::Allowed,
            outcome: QuestionsOutcome::Answered(Vec::new()),
            delivery: FileDelivery::Delivered,
            requests: Mutex::new(Vec::new()),
            questions: Mutex::new(Vec::new()),
        }
    }
}

impl UserChannel for Scripted {
    fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision> {
        self.requests.lock().unwrap().push(tool);
        Box::pin(async { self.decision.clone() })
    }

    fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        self.questions.lock().unwrap().extend(questions);
        Box::pin(async { self.outcome.clone() })
    }

    fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(async { self.delivery.clone() })
    }
}
