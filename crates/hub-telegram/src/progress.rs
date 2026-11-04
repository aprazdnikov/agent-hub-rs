//! A bounded snapshot per run. Drop guards update it without spawning tasks or sending messages.

use hub_core::{
    domain::AgentEvent,
    progress::{TaskProgress, WaitKind},
};
use std::time::Instant;
use tokio::sync::watch;

#[derive(Clone)]
pub struct Progress(watch::Sender<TaskProgress>);
impl Progress {
    pub fn new() -> Self {
        Self(watch::Sender::new(TaskProgress::new(Instant::now())))
    }
    pub fn snapshot(&self) -> TaskProgress {
        self.0.borrow().clone()
    }
    pub async fn finished(&self) {
        let mut changes = self.0.subscribe();
        loop {
            if changes.borrow_and_update().is_finished() {
                return;
            }
            if changes.changed().await.is_err() {
                return;
            }
        }
    }
    pub fn executing(&self) {
        self.0.send_modify(|p| p.executing(Instant::now()));
    }
    pub fn observe(&self, event: &AgentEvent) {
        self.0.send_modify(|p| p.observe(event, Instant::now()));
    }
    pub fn stopping(&self) {
        self.0.send_modify(|p| p.stopping(Instant::now()));
    }
    pub fn finish(&self, cancelled: bool) {
        self.0.send_modify(|p| p.finish(cancelled, Instant::now()));
    }
    pub fn wait(&self, kind: WaitKind) -> Wait {
        self.0.send_modify(|p| p.wait_started(kind, Instant::now()));
        Wait { progress: self.clone(), kind }
    }
}
pub struct Wait {
    progress: Progress,
    kind: WaitKind,
}
impl Drop for Wait {
    fn drop(&mut self) {
        self.progress.0.send_modify(|p| p.wait_ended(self.kind, Instant::now()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::progress::Phase;

    #[test]
    fn progress_guards_count_parallel_waits_and_ignore_late_drops() {
        let progress = Progress::new();
        progress.executing();
        let answer = progress.wait(WaitKind::Answer);
        let first = progress.wait(WaitKind::Approval);
        let second = progress.wait(WaitKind::Approval);
        drop(second);
        assert_eq!(progress.snapshot().phase(), Phase::AwaitingApproval);
        drop(first);
        assert_eq!(progress.snapshot().phase(), Phase::AwaitingAnswer);
        progress.stopping();
        progress.finish(true);
        let stopped = progress.snapshot();
        drop(answer);
        assert_eq!(progress.snapshot(), stopped);
    }

    #[tokio::test]
    async fn progress_cancellation_drops_a_wait_without_a_mailbox_slot() {
        let progress = Progress::new();
        progress.executing();
        let other = progress.clone();
        let (opened, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _wait = other.wait(WaitKind::Answer);
            opened.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        assert_eq!(progress.snapshot().phase(), Phase::AwaitingAnswer);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(progress.snapshot().phase(), Phase::Running);
    }
}
