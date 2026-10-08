//! One Codex conversation: turns, prompts sent mid-turn, and the end of the conversation.
//!
//! Prompts sent meanwhile join the running turn. One the turn rejects waits, in order with the
//! others, for that turn to complete and then starts the next turn; a turn is never started
//! while another runs.

use std::collections::VecDeque;

use futures::future::BoxFuture;
use hub_agent::conversation::Conversation;
use hub_core::domain::{AgentEvent, Prompt, SessionId};
use tokio::sync::mpsc;

use crate::protocol::{
    Call, Translation, TurnId, TurnTracker, parse_turn_id, steer_params, turn_params,
};
use hub_agent::rpc::{Notification, RpcClient, RpcError};

/// The part of an app-server thread a conversation needs.
pub trait Thread: Send + Sync {
    fn start_turn<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<TurnId, RpcError>>;
    fn steer<'a>(
        &'a self,
        turn: &'a TurnId,
        prompt: &'a Prompt,
    ) -> BoxFuture<'a, Result<(), RpcError>>;
}

pub struct RpcThread {
    client: RpcClient,
    thread: SessionId,
}

impl RpcThread {
    #[must_use]
    pub fn new(client: RpcClient, thread: SessionId) -> Self {
        Self { client, thread }
    }
}

impl Thread for RpcThread {
    fn start_turn<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<TurnId, RpcError>> {
        Box::pin(async move {
            let params = turn_params(&self.thread, prompt);
            parse_turn_id(&self.client.request(Call::TurnStart.method(), params).await?)
        })
    }

    fn steer<'a>(
        &'a self,
        turn: &'a TurnId,
        prompt: &'a Prompt,
    ) -> BoxFuture<'a, Result<(), RpcError>> {
        Box::pin(async move {
            let params = steer_params(&self.thread, turn, prompt);
            self.client.request(Call::TurnSteer.method(), params).await.map(drop)
        })
    }
}

/// Relays a thread until no turn runs and nothing is queued, or until /stop.
///
/// Returns the inbox, so prompts that arrive while the conversation closes are not lost.
pub async fn converse<T: Thread>(
    thread: &T,
    notifications: &mut mpsc::UnboundedReceiver<Notification>,
    mut tracker: TurnTracker,
    prompt: Prompt,
    mut inbox: mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> (mpsc::Receiver<Prompt>, Result<(), RpcError>) {
    let outcome =
        relay(thread, notifications, &mut tracker, prompt, &mut inbox, conversation).await;
    (inbox, outcome)
}

async fn relay<T: Thread>(
    thread: &T,
    notifications: &mut mpsc::UnboundedReceiver<Notification>,
    tracker: &mut TurnTracker,
    prompt: Prompt,
    inbox: &mut mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> Result<(), RpcError> {
    let mut queued = VecDeque::new();
    let mut active = tokio::select! {
        biased;
        () = conversation.cancel.cancelled() => return Ok(()),
        turn = thread.start_turn(&prompt) => Some(turn?),
    };
    let mut inbox_open = true;
    while active.is_some() || !queued.is_empty() {
        tokio::select! {
            // A stop must win over a prompt that arrived at the same moment, which then stays
            // in the inbox.
            biased;
            () = conversation.cancel.cancelled() => return Ok(()),
            next = inbox.recv(), if inbox_open => match next {
                Some(prompt) => {
                    queued.push_back(prompt);
                    active = deliver(thread, active, &mut queued).await?;
                }
                None => inbox_open = false,
            },
            notification = notifications.recv() => {
                let notification = notification.ok_or(RpcError::Closed)?;
                let Translation { events, completed } = tracker.translate(&notification)?;
                for event in events {
                    emit(conversation, event).await;
                }
                if completed.is_some() && completed == active {
                    active = deliver(thread, None, &mut queued).await?;
                }
            }
        }
    }
    Ok(())
}

/// Hands `queued` to the running turn, or to a new one if none runs; returns that turn.
///
/// A rejected steer leaves the prompt and everything after it queued: the turn may still be
/// running, so only its `turn/completed` makes starting another one safe.
async fn deliver<T: Thread>(
    thread: &T,
    active: Option<TurnId>,
    queued: &mut VecDeque<Prompt>,
) -> Result<Option<TurnId>, RpcError> {
    let active = match active {
        Some(turn) => turn,
        None => match queued.pop_front() {
            None => return Ok(None),
            Some(prompt) => thread.start_turn(&prompt).await?,
        },
    };
    while let Some(next) = queued.front() {
        match thread.steer(&active, next).await {
            Ok(()) => {
                queued.pop_front();
            }
            Err(RpcError::Remote { .. }) => break,
            Err(other @ (RpcError::Closed | RpcError::Protocol(_) | RpcError::Timeout { .. })) => {
                return Err(other);
            }
        }
    }
    Ok(Some(active))
}

async fn emit(conversation: &Conversation, event: AgentEvent) {
    // Nobody listening means the topic is gone; there is no one left to tell.
    let _ = conversation.events.send(event).await;
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use hub_agent::conversation::Limits;
    use hub_core::domain::{Finished, Usage};
    use serde_json::json;
    use tokio::task::JoinHandle;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::testing::Scripted;

    #[derive(Default)]
    struct FakeThread {
        calls: Mutex<Vec<String>>,
        steers: Mutex<VecDeque<Result<(), RpcError>>>,
        turns: Mutex<u32>,
    }

    impl FakeThread {
        fn rejecting(times: usize) -> Self {
            let rejected = RpcError::Remote { code: -32000, message: "no active turn".to_owned() };
            Self { steers: Mutex::new(vec![Err(rejected); times].into()), ..Self::default() }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Thread for FakeThread {
        fn start_turn<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<TurnId, RpcError>> {
            let mut turns = self.turns.lock().unwrap();
            *turns += 1;
            self.calls.lock().unwrap().push(format!("start:{}", prompt.text()));
            let id = TurnId::new(format!("u-{turns}"));
            Box::pin(async move { Ok(id) })
        }

        fn steer<'a>(
            &'a self,
            turn: &'a TurnId,
            prompt: &'a Prompt,
        ) -> BoxFuture<'a, Result<(), RpcError>> {
            self.calls.lock().unwrap().push(format!("steer:{}:{}", turn.as_str(), prompt.text()));
            let outcome = self.steers.lock().unwrap().pop_front().unwrap_or(Ok(()));
            Box::pin(async move { outcome })
        }
    }

    struct Harness {
        thread: Arc<FakeThread>,
        notify: mpsc::UnboundedSender<Notification>,
        inbox: mpsc::Sender<Prompt>,
        cancel: CancellationToken,
        events: mpsc::Receiver<AgentEvent>,
        done: JoinHandle<(mpsc::Receiver<Prompt>, Result<(), RpcError>)>,
    }

    fn prompt(text: &str) -> Prompt {
        Prompt::new(text.to_owned(), Vec::new()).unwrap()
    }

    fn thread_id() -> SessionId {
        SessionId::parse("t-1").unwrap()
    }

    fn start(thread: FakeThread) -> Harness {
        let thread = Arc::new(thread);
        let (notify, mut notifications) = mpsc::unbounded_channel();
        let (inbox, inbox_out) = mpsc::channel(8);
        let (events_in, events) = mpsc::channel(64);
        let cancel = CancellationToken::new();
        let conversation = Conversation {
            channel: Arc::new(Scripted::default()),
            events: events_in,
            cancel: cancel.clone(),
            limits: Limits::new(Duration::from_secs(1)),
        };
        let done = tokio::spawn({
            let thread = Arc::clone(&thread);
            async move {
                let tracker = TurnTracker::new(thread_id());
                converse(
                    thread.as_ref(),
                    &mut notifications,
                    tracker,
                    prompt("hi"),
                    inbox_out,
                    &conversation,
                )
                .await
            }
        });
        Harness { thread, notify, inbox, cancel, events, done }
    }

    fn completed(turn: &str) -> Notification {
        Notification {
            method: "turn/completed".to_owned(),
            params: json!({"turn": {"id": turn, "status": "completed"}}),
        }
    }

    fn said(text: &str) -> Notification {
        Notification {
            method: "item/completed".to_owned(),
            params: json!({"item": {"type": "agentMessage", "text": text}}),
        }
    }

    async fn eventually(what: &str, check: impl Fn() -> bool) {
        for _ in 0..400 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    fn drain(events: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        std::iter::from_fn(|| events.try_recv().ok()).collect()
    }

    #[tokio::test]
    async fn a_turn_relays_text_and_finishes() {
        let mut harness = start(FakeThread::default());
        harness.notify.send(said("Привет")).unwrap();
        harness.notify.send(completed("u-1")).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(harness.thread.calls(), ["start:hi"]);
        let finished = AgentEvent::Finished(Finished {
            session: thread_id(),
            usage: Usage::Codex { tokens: None },
            background: 0,
        });
        assert_eq!(
            drain(&mut harness.events),
            [AgentEvent::AssistantText("Привет".to_owned()), finished]
        );
    }

    #[tokio::test]
    async fn a_prompt_during_a_turn_steers_it() {
        let harness = start(FakeThread::default());
        harness.inbox.send(prompt("ещё")).await.unwrap();
        eventually("steered", || harness.thread.calls().len() == 2).await;
        harness.notify.send(completed("u-1")).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(harness.thread.calls(), ["start:hi", "steer:u-1:ещё"]);
    }

    #[tokio::test]
    async fn a_rejected_steer_waits_for_the_turn_and_starts_the_next() {
        let harness = start(FakeThread::rejecting(1));
        harness.inbox.send(prompt("поздно")).await.unwrap();
        eventually("steer tried", || harness.thread.calls().len() == 2).await;
        harness.notify.send(completed("u-1")).unwrap();
        eventually("next turn", || harness.thread.calls().len() == 3).await;
        harness.notify.send(completed("u-2")).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(harness.thread.calls(), ["start:hi", "steer:u-1:поздно", "start:поздно"]);
    }

    #[tokio::test]
    async fn rejected_prompts_keep_their_order() {
        let harness = start(FakeThread::rejecting(2));
        harness.inbox.send(prompt("a")).await.unwrap();
        eventually("a tried", || harness.thread.calls().len() == 2).await;
        harness.inbox.send(prompt("b")).await.unwrap();
        eventually("a tried again", || harness.thread.calls().len() == 3).await;
        harness.notify.send(completed("u-1")).unwrap();
        eventually("a started, b steered", || harness.thread.calls().len() == 5).await;
        harness.notify.send(completed("u-2")).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(
            harness.thread.calls(),
            ["start:hi", "steer:u-1:a", "steer:u-1:a", "start:a", "steer:u-2:b"]
        );
    }

    #[tokio::test]
    async fn completion_of_another_turn_is_not_the_end() {
        let mut harness = start(FakeThread::default());
        harness.notify.send(completed("u-9")).unwrap();
        harness.notify.send(said("всё ещё работаю")).unwrap();
        harness.notify.send(completed("u-1")).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert!(
            drain(&mut harness.events)
                .contains(&AgentEvent::AssistantText("всё ещё работаю".to_owned()))
        );
    }

    #[tokio::test]
    async fn closed_notifications_are_a_closed_transport() {
        let harness = start(FakeThread::default());
        drop(harness.notify);
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Err(RpcError::Closed));
    }

    #[tokio::test]
    async fn stop_ends_the_conversation_at_once_and_keeps_the_inbox() {
        let harness = start(FakeThread::default());
        eventually("started", || harness.thread.calls().len() == 1).await;
        harness.cancel.cancel();
        let (mut inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        harness.inbox.send(prompt("после стопа")).await.unwrap();
        assert_eq!(inbox.recv().await.map(|p| p.text().to_owned()), Some("после стопа".to_owned()));
    }

    #[tokio::test]
    async fn stop_wins_over_a_prompt_that_arrived_at_the_same_moment() {
        for _ in 0..20 {
            let harness = start(FakeThread::default());
            eventually("started", || harness.thread.calls().len() == 1).await;
            harness.inbox.send(prompt("одновременно")).await.unwrap();
            harness.cancel.cancel();
            let (mut inbox, outcome) = harness.done.await.unwrap();
            assert_eq!(outcome, Ok(()));
            assert_eq!(harness.thread.calls(), ["start:hi"]);
            assert_eq!(
                inbox.recv().await.map(|p| p.text().to_owned()),
                Some("одновременно".to_owned())
            );
        }
    }

    #[tokio::test]
    async fn a_failed_turn_ends_the_conversation_with_its_reason() {
        let mut harness = start(FakeThread::default());
        let failed = Notification {
            method: "turn/completed".to_owned(),
            params: json!({"turn": {"id": "u-1", "status": "failed", "error": {"message": "quota"}}}),
        };
        harness.notify.send(failed).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(drain(&mut harness.events), [AgentEvent::Failed("quota".to_owned())]);
    }
}
