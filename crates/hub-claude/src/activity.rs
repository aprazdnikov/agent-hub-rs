//! What the CLI session is still doing, to know when closing it loses nothing.
//!
//! Only backgrounded tasks matter: foreground subagents finish inside their turn, while a
//! background task reports in a turn the CLI starts on its own after the task ends.

use std::collections::HashSet;

use crate::wire::{Incoming, System, TaskPatch, User};

// Statuses after which a task no longer runs.
const TERMINAL_STATUSES: [&str; 4] = ["completed", "failed", "stopped", "killed"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The agent is working on a turn.
    Busy,
    /// Idle, but background tasks are still running.
    Background,
    /// Tasks reported; the CLI is about to start a turn with their results.
    Settling,
    /// Nothing left; the session can be closed.
    Idle,
}

#[derive(Debug, Default)]
pub struct SessionActivity {
    // Sent prompts the CLI has not taken in yet.
    waiting: HashSet<String>,
    // A turn with our prompts is in progress.
    answering: bool,
    // Running background tasks: id and description, in start order.
    tasks: Vec<(String, String)>,
    // A background task ended; its report turn has not begun.
    report_due: bool,
    // A report turn started by the CLI is in progress.
    reporting: bool,
}

impl SessionActivity {
    pub fn sent(&mut self, prompt: String) {
        self.waiting.insert(prompt);
    }

    #[must_use]
    pub fn awaiting_result(&self) -> bool {
        !self.waiting.is_empty() || self.answering
    }

    #[must_use]
    pub fn background(&self) -> Vec<String> {
        self.tasks.iter().map(|(_, description)| description.clone()).collect()
    }

    #[must_use]
    pub fn phase(&self) -> Phase {
        if self.awaiting_result() || self.reporting {
            Phase::Busy
        } else if !self.tasks.is_empty() {
            Phase::Background
        } else if self.report_due {
            // Also covers a task killed without a report: the settling window closes it.
            Phase::Settling
        } else {
            Phase::Idle
        }
    }

    pub fn observe(&mut self, message: &Incoming) {
        match message {
            Incoming::User(User { uuid: Some(uuid) }) if self.waiting.contains(uuid) => {
                self.waiting.remove(uuid);
                self.answering = true;
            }
            Incoming::System(System::TaskStarted { task_id, description, is_backgrounded })
                if *is_backgrounded != Some(false) =>
            {
                self.tasks.push((task_id.clone(), description.clone()));
            }
            Incoming::System(System::TaskNotification { task_id }) => self.task_ended(task_id),
            // Arrives before the notification, so it alone must not close the session.
            Incoming::System(System::TaskUpdated { task_id, patch }) if is_terminal(patch) => {
                self.task_ended(task_id);
            }
            Incoming::Result(outcome) if outcome.injected() => self.reporting = false,
            Incoming::Result(_) => self.answering = false,
            Incoming::Assistant(_) | Incoming::System(System::Init { .. })
                if self.report_due && !self.answering =>
            {
                self.report_due = false;
                self.reporting = true;
            }
            Incoming::User(_)
            | Incoming::Assistant(_)
            | Incoming::System(_)
            | Incoming::ControlRequest { .. }
            | Incoming::ControlResponse { .. }
            | Incoming::ControlCancelRequest { .. }
            | Incoming::Other => {}
        }
    }

    fn task_ended(&mut self, task_id: &str) {
        if let Some(position) = self.tasks.iter().position(|(id, _)| id == task_id) {
            self.tasks.remove(position);
            self.report_due = true;
        }
    }
}

fn is_terminal(patch: &TaskPatch) -> bool {
    patch.status.as_deref().is_some_and(|status| TERMINAL_STATUSES.contains(&status))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::wire::parse_line;

    fn message(value: &Value) -> Incoming {
        parse_line(&value.to_string()).unwrap()
    }

    fn started(id: &str, description: &str, background: bool) -> Incoming {
        message(&json!({"type": "system", "subtype": "task_started", "task_id": id,
                        "description": description, "is_backgrounded": background}))
    }

    fn notified(id: &str) -> Incoming {
        message(&json!({"type": "system", "subtype": "task_notification", "task_id": id}))
    }

    fn completed(id: &str) -> Incoming {
        message(&json!({"type": "system", "subtype": "task_updated", "task_id": id,
                        "patch": {"status": "completed"}}))
    }

    fn result(injected: bool) -> Incoming {
        let origin = if injected { json!({"kind": "task-notification"}) } else { Value::Null };
        message(&json!({"type": "result", "subtype": "success", "is_error": false,
                        "num_turns": 1, "session_id": "s", "origin": origin}))
    }

    fn text() -> Incoming {
        message(&json!({"type": "assistant",
                        "message": {"content": [{"type": "text", "text": "BG-DONE"}]}}))
    }

    fn init() -> Incoming {
        message(&json!({"type": "system", "subtype": "init", "session_id": "s"}))
    }

    fn accepted(id: &str) -> Incoming {
        message(&json!({"type": "user", "uuid": id, "message": {"role": "user", "content": "…"}}))
    }

    fn activity(messages: &[Incoming]) -> SessionActivity {
        let mut activity = SessionActivity::default();
        activity.sent("p1".to_owned());
        activity.observe(&accepted("p1"));
        for message in messages {
            activity.observe(message);
        }
        activity
    }

    #[test]
    fn plain_turn_is_busy_until_its_result() {
        let mut activity = activity(&[]);
        assert_eq!(activity.phase(), Phase::Busy);
        activity.observe(&result(false));
        assert_eq!(activity.phase(), Phase::Idle);
    }

    #[test]
    fn foreground_subagent_does_not_keep_session_open() {
        let activity = activity(&[
            started("a", "sub", false),
            completed("a"),
            notified("a"),
            text(),
            result(false),
        ]);
        assert_eq!(activity.phase(), Phase::Idle);
    }

    #[test]
    fn background_task_keeps_session_open_until_injected_turn_ends() {
        let mut activity = activity(&[started("b", "sleep 8", true), result(false)]);
        assert_eq!(activity.phase(), Phase::Background);
        assert_eq!(activity.background(), ["sleep 8"]);

        activity.observe(&notified("b"));
        assert_eq!(activity.phase(), Phase::Settling);

        activity.observe(&init());
        assert_eq!(activity.phase(), Phase::Busy);

        activity.observe(&text());
        activity.observe(&result(true));
        assert_eq!(activity.phase(), Phase::Idle);
    }

    #[test]
    fn task_update_before_notification_waits_for_the_injected_turn() {
        let mut activity = activity(&[started("b", "sleep", true), result(false), completed("b")]);
        assert_eq!(activity.phase(), Phase::Settling);
        activity.observe(&notified("b"));
        activity.observe(&text());
        activity.observe(&result(true));
        assert_eq!(activity.phase(), Phase::Idle);
    }

    #[test]
    fn background_task_finishing_during_a_turn_still_expects_its_report() {
        let activity = activity(&[started("b", "sleep", true), completed("b"), result(false)]);
        assert_eq!(activity.phase(), Phase::Settling);
    }

    #[test]
    fn task_ending_during_an_injected_turn_expects_another_one() {
        let mut activity = activity(&[
            started("b1", "one", true),
            started("b2", "two", true),
            result(false),
            notified("b1"),
            text(),
        ]);
        activity.observe(&notified("b2"));
        activity.observe(&result(true));
        assert_eq!(activity.phase(), Phase::Settling);
    }

    #[test]
    fn one_injected_turn_may_report_several_tasks() {
        let mut activity = activity(&[
            started("b1", "one", true),
            started("b2", "two", true),
            result(false),
            notified("b1"),
        ]);
        assert_eq!(activity.phase(), Phase::Background);
        activity.observe(&notified("b2"));
        activity.observe(&text());
        activity.observe(&result(true));
        assert_eq!(activity.phase(), Phase::Idle);
    }

    #[test]
    fn message_sent_while_background_runs_is_busy_until_its_result() {
        let mut activity = activity(&[started("b", "sleep", true), result(false)]);
        activity.sent("p2".to_owned());
        assert_eq!(activity.phase(), Phase::Busy);
        activity.observe(&accepted("p2"));
        activity.observe(&result(false));
        assert_eq!(activity.phase(), Phase::Background);
    }

    #[test]
    fn message_merged_into_the_running_turn_ends_with_it() {
        let mut activity = activity(&[]);
        activity.sent("p2".to_owned());
        activity.observe(&accepted("p2"));
        activity.observe(&result(false));
        assert_eq!(activity.phase(), Phase::Idle);
    }

    #[test]
    fn message_not_yet_accepted_keeps_session_busy_after_a_result() {
        let mut activity = activity(&[]);
        activity.sent("p2".to_owned());
        activity.observe(&result(false));
        assert_eq!(activity.phase(), Phase::Busy);
    }

    #[test]
    fn injected_result_does_not_answer_a_human_prompt() {
        let mut activity = activity(&[started("b", "sleep", true), result(false), notified("b")]);
        activity.sent("p2".to_owned());
        activity.observe(&result(true));
        assert_eq!(activity.phase(), Phase::Busy);
    }

    #[test]
    fn running_background_tasks_are_listed() {
        assert_eq!(activity(&[started("b", "sleep", true)]).background(), ["sleep"]);
    }

    #[test]
    fn awaiting_result_until_the_prompt_is_answered() {
        let mut activity = activity(&[]);
        assert!(activity.awaiting_result());
        activity.observe(&result(false));
        assert!(!activity.awaiting_result());
    }
}
