//! One Qwen conversation: turns, prompts handed over mid-turn, and the end of the conversation.
//!
//! During a turn Qwen asks for queued prompts with `craft/drainMidTurnQueue`. That request is
//! answered in another task, so it asks this loop through `Drain`, and the loop answers at once
//! from the inbox. Prompts that arrive after the last drain start the next turn.

use std::collections::VecDeque;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_agent::conversation::Conversation;
use hub_agent::rpc::{Notification, RpcClient, RpcError};
use hub_core::domain::{AgentEvent, Prompt, SessionId};
use tokio::sync::{mpsc, oneshot};

use crate::protocol::{
    Call, MAX_DRAIN_ITEMS, StopReason, TurnTracker, cancel_params, parse_stop_reason, prompt_params,
};

/// Qwen waits 2 s for a drain answer and stops asking after three misses in a row.
pub const DRAIN_DEADLINE: Duration = Duration::from_millis(1500);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_REQUESTS: usize = 4;

/// The part of an ACP session a conversation needs.
pub trait Agent: Send + Sync {
    fn prompt<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<StopReason, RpcError>>;
    fn cancel(&self) -> BoxFuture<'_, Result<(), RpcError>>;
}

pub struct RpcAgent {
    client: RpcClient,
    session: SessionId,
}

impl RpcAgent {
    #[must_use]
    pub fn new(client: RpcClient, session: SessionId) -> Self {
        Self { client, session }
    }
}

impl Agent for RpcAgent {
    fn prompt<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<StopReason, RpcError>> {
        Box::pin(async move {
            let params = prompt_params(&self.session, prompt);
            // A turn lasts as long as Qwen works; `/stop` and the process exit end it.
            let result = self.client.request_untimed(Call::SessionPrompt.method(), params).await?;
            parse_stop_reason(&result)
        })
    }

    fn cancel(&self) -> BoxFuture<'_, Result<(), RpcError>> {
        Box::pin(async move {
            self.client
                .notify_with(Call::SessionCancel.method(), cancel_params(&self.session))
                .await
        })
    }
}

type Handover = oneshot::Sender<Vec<Prompt>>;

/// The request handler's way to ask the conversation loop for the prompts waiting in the inbox.
#[derive(Clone)]
pub struct Drain {
    requests: mpsc::Sender<Handover>,
}

pub struct DrainRequests(mpsc::Receiver<Handover>);

#[must_use]
pub fn drain_channel() -> (Drain, DrainRequests) {
    let (requests, received) = mpsc::channel(DRAIN_REQUESTS);
    (Drain { requests }, DrainRequests(received))
}

impl Drain {
    /// The prompts the conversation hands over; none if it is gone or did not answer in time.
    pub async fn take(&self) -> Vec<Prompt> {
        let (reply, handed) = oneshot::channel();
        let asked = async {
            self.requests.send(reply).await.ok()?;
            handed.await.ok()
        };
        match tokio::time::timeout(DRAIN_DEADLINE, asked).await {
            Ok(Some(prompts)) => prompts,
            // The conversation ended; nothing is waiting for this turn any more.
            Ok(None) => Vec::new(),
            Err(_) => {
                tracing::warn!("qwen drain not answered in time; prompts wait for the next turn");
                Vec::new()
            }
        }
    }
}

pub struct Turns<'a, A> {
    pub agent: &'a A,
    pub notifications: &'a mut mpsc::UnboundedReceiver<Notification>,
    pub drains: &'a mut DrainRequests,
}

/// Runs turns until none is running and nothing waits, or until /stop.
///
/// Returns the inbox, so prompts that arrive while the conversation closes are not lost.
pub async fn converse<A: Agent>(
    turns: Turns<'_, A>,
    mut tracker: TurnTracker,
    prompt: Prompt,
    mut inbox: mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> (mpsc::Receiver<Prompt>, Result<(), RpcError>) {
    // Prompts taken for a drain whose handler had already given up; they go first.
    let mut held = VecDeque::new();
    let outcome = relay(turns, &mut tracker, prompt, &mut inbox, &mut held, conversation).await;
    if !held.is_empty() {
        // The conversation ended (`/stop` or a failed turn) before they could start a turn.
        tracing::warn!(prompts = held.len(), "qwen conversation ended; held prompts dropped");
    }
    (inbox, outcome)
}

async fn relay<A: Agent>(
    turns: Turns<'_, A>,
    tracker: &mut TurnTracker,
    prompt: Prompt,
    inbox: &mut mpsc::Receiver<Prompt>,
    held: &mut VecDeque<Prompt>,
    conversation: &Conversation,
) -> Result<(), RpcError> {
    let Turns { agent, notifications, drains } = turns;
    let mut next = Some(prompt);
    while let Some(prompt) = next.take() {
        let turn = agent.prompt(&prompt);
        tokio::pin!(turn);
        let mut updates_open = true;
        let stop = loop {
            tokio::select! {
                // A stop wins over anything that arrived at the same moment, which then stays
                // in the inbox; every update Qwen sent before its reply is read before the reply.
                biased;
                () = conversation.cancel.cancelled() => {
                    cancel(agent).await;
                    return Ok(());
                }
                Some(handover) = drains.0.recv() => hand_over(handover, inbox, held),
                update = notifications.recv(), if updates_open => match update {
                    Some(update) => {
                        for event in tracker.translate(&update) {
                            emit(conversation, event).await;
                        }
                    }
                    // The reader is gone; the turn itself ends with `Closed`.
                    None => updates_open = false,
                },
                stop = &mut turn => break stop?,
            }
        };
        for event in tracker.finish(stop) {
            emit(conversation, event).await;
        }
        if conversation.cancel.is_cancelled() {
            return Ok(());
        }
        next = held.pop_front().or_else(|| inbox.try_recv().ok());
    }
    Ok(())
}

/// Answers a drain from the held prompts, then the inbox, up to what Qwen accepts at once.
fn hand_over(handover: Handover, inbox: &mut mpsc::Receiver<Prompt>, held: &mut VecDeque<Prompt>) {
    let from_held = held.len().min(MAX_DRAIN_ITEMS);
    let room = MAX_DRAIN_ITEMS.saturating_sub(from_held);
    let taken: Vec<Prompt> = held
        .drain(..from_held)
        .chain(std::iter::from_fn(|| inbox.try_recv().ok()).take(room))
        .collect();
    if let Err(late) = handover.send(taken) {
        tracing::warn!(
            prompts = late.len(),
            "qwen drain handler gave up; prompts start the next turn"
        );
        for prompt in late.into_iter().rev() {
            held.push_front(prompt);
        }
    }
}

async fn cancel<A: Agent>(agent: &A) {
    match tokio::time::timeout(CANCEL_TIMEOUT, agent.cancel()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "qwen did not take session/cancel"),
        Err(_) => tracing::warn!("qwen input stalled on session/cancel"),
    }
}

pub(crate) async fn emit(conversation: &Conversation, event: AgentEvent) {
    // Nobody listening means the topic is gone; there is no one left to tell.
    let _ = conversation.events.send(event).await;
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use hub_agent::conversation::Limits;
    use hub_core::domain::{Finished, Usage};
    use serde_json::json;
    use tokio::task::JoinHandle;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::testing::Scripted;

    type Ends = mpsc::UnboundedSender<Result<StopReason, RpcError>>;

    struct FakeAgent {
        calls: Mutex<Vec<String>>,
        ends: tokio::sync::Mutex<mpsc::UnboundedReceiver<Result<StopReason, RpcError>>>,
    }

    impl FakeAgent {
        fn new() -> (Self, Ends) {
            let (ends, ended) = mpsc::unbounded_channel();
            (Self { calls: Mutex::new(Vec::new()), ends: tokio::sync::Mutex::new(ended) }, ends)
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Agent for FakeAgent {
        fn prompt<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<StopReason, RpcError>> {
            self.calls.lock().unwrap().push(format!("prompt:{}", prompt.text()));
            Box::pin(
                async move { self.ends.lock().await.recv().await.unwrap_or(Err(RpcError::Closed)) },
            )
        }

        fn cancel(&self) -> BoxFuture<'_, Result<(), RpcError>> {
            self.calls.lock().unwrap().push("cancel".to_owned());
            Box::pin(async { Ok(()) })
        }
    }

    struct Harness {
        agent: Arc<FakeAgent>,
        ends: Ends,
        notify: mpsc::UnboundedSender<Notification>,
        inbox: mpsc::Sender<Prompt>,
        drain: Drain,
        cancel: CancellationToken,
        events: mpsc::Receiver<AgentEvent>,
        done: JoinHandle<(mpsc::Receiver<Prompt>, Result<(), RpcError>)>,
    }

    fn prompt(text: &str) -> Prompt {
        Prompt::new(text.to_owned(), Vec::new()).unwrap()
    }

    fn session_id() -> SessionId {
        SessionId::parse("s-1").unwrap()
    }

    fn start() -> Harness {
        let (agent, ends) = FakeAgent::new();
        let agent = Arc::new(agent);
        let (notify, mut notifications) = mpsc::unbounded_channel();
        let (inbox, inbox_out) = mpsc::channel(32);
        let (events_in, events) = mpsc::channel(64);
        let (drain, mut drains) = drain_channel();
        let cancel = CancellationToken::new();
        let conversation = Conversation {
            channel: Arc::new(Scripted::default()),
            events: events_in,
            cancel: cancel.clone(),
            limits: Limits::new(Duration::from_secs(1)),
        };
        let done = tokio::spawn({
            let agent = Arc::clone(&agent);
            async move {
                let turns = Turns {
                    agent: agent.as_ref(),
                    notifications: &mut notifications,
                    drains: &mut drains,
                };
                converse(
                    turns,
                    TurnTracker::new(session_id()),
                    prompt("hi"),
                    inbox_out,
                    &conversation,
                )
                .await
            }
        });
        Harness { agent, ends, notify, inbox, drain, cancel, events, done }
    }

    fn said(text: &str) -> Notification {
        Notification {
            method: "session/update".to_owned(),
            params: json!({"sessionId": "s-1", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}}),
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

    fn drained(events: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        std::iter::from_fn(|| events.try_recv().ok()).collect()
    }

    fn texts(prompts: &[Prompt]) -> Vec<String> {
        prompts.iter().map(|prompt| prompt.text().to_owned()).collect()
    }

    #[tokio::test]
    async fn a_turn_relays_text_and_finishes() {
        let mut harness = start();
        harness.notify.send(said("При")).unwrap();
        harness.notify.send(said("вет")).unwrap();
        eventually("started", || harness.agent.calls().len() == 1).await;
        harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        let finished = AgentEvent::Finished(Finished {
            session: session_id(),
            usage: Usage::Qwen { tokens: None },
            background: 0,
        });
        assert_eq!(
            drained(&mut harness.events),
            [AgentEvent::AssistantText("Привет".to_owned()), finished]
        );
    }

    #[tokio::test]
    async fn a_prompt_during_a_turn_goes_through_the_drain() {
        let harness = start();
        eventually("started", || harness.agent.calls().len() == 1).await;
        harness.inbox.send(prompt("ещё")).await.unwrap();
        assert_eq!(texts(&harness.drain.take().await), ["ещё"]);
        harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!((outcome, harness.agent.calls()), (Ok(()), vec!["prompt:hi".to_owned()]));
    }

    #[tokio::test]
    async fn a_prompt_after_the_last_drain_starts_the_next_turn() {
        let harness = start();
        eventually("started", || harness.agent.calls().len() == 1).await;
        harness.inbox.send(prompt("потом")).await.unwrap();
        harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
        eventually("next turn", || harness.agent.calls().len() == 2).await;
        harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(harness.agent.calls(), ["prompt:hi", "prompt:потом"]);
    }

    #[tokio::test]
    async fn a_drain_hands_over_at_most_ten_prompts() {
        let harness = start();
        eventually("started", || harness.agent.calls().len() == 1).await;
        for n in 1..=12 {
            harness.inbox.send(prompt(&n.to_string())).await.unwrap();
        }
        assert_eq!(
            texts(&harness.drain.take().await),
            ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]
        );
        for turns in 2..=3 {
            harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
            eventually("next turn", || harness.agent.calls().len() == turns).await;
        }
        harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(harness.agent.calls(), ["prompt:hi", "prompt:11", "prompt:12"]);
    }

    #[test]
    fn prompts_for_a_drain_that_gave_up_are_kept() {
        let (reply, handed) = oneshot::channel();
        drop(handed);
        let (sender, mut inbox) = mpsc::channel(4);
        sender.try_send(prompt("a")).unwrap();
        let mut held = VecDeque::from([prompt("раньше")]);
        hand_over(reply, &mut inbox, &mut held);
        assert_eq!(held.iter().map(Prompt::text).collect::<Vec<_>>(), ["раньше", "a"]);
    }

    #[tokio::test]
    async fn a_drain_without_a_conversation_is_empty() {
        let (drain, requests) = drain_channel();
        drop(requests);
        assert!(drain.take().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn an_unanswered_drain_gives_up_before_qwen_does() {
        let (drain, _requests) = drain_channel();
        let started = tokio::time::Instant::now();
        assert!(drain.take().await.is_empty());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn stop_cancels_the_turn_and_keeps_the_inbox() {
        let harness = start();
        eventually("started", || harness.agent.calls().len() == 1).await;
        harness.cancel.cancel();
        let (mut inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(harness.agent.calls(), ["prompt:hi", "cancel"]);
        harness.inbox.send(prompt("после стопа")).await.unwrap();
        assert_eq!(inbox.recv().await.map(|p| p.text().to_owned()), Some("после стопа".to_owned()));
    }

    #[tokio::test]
    async fn stop_wins_over_a_prompt_that_arrived_at_the_same_moment() {
        for _ in 0..20 {
            let harness = start();
            eventually("started", || harness.agent.calls().len() == 1).await;
            harness.inbox.send(prompt("одновременно")).await.unwrap();
            harness.cancel.cancel();
            let (mut inbox, outcome) = harness.done.await.unwrap();
            assert_eq!(outcome, Ok(()));
            assert!(!harness.agent.calls().contains(&"prompt:одновременно".to_owned()));
            assert_eq!(
                inbox.recv().await.map(|p| p.text().to_owned()),
                Some("одновременно".to_owned())
            );
        }
    }

    #[tokio::test]
    async fn closed_updates_do_not_end_a_turn_that_still_answers() {
        let harness = start();
        eventually("started", || harness.agent.calls().len() == 1).await;
        drop(harness.notify);
        harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
    }

    #[tokio::test]
    async fn a_failed_prompt_ends_the_conversation_with_its_error() {
        let harness = start();
        let error = RpcError::Remote { code: -32603, message: "Internal error: quota".to_owned() };
        harness.ends.send(Err(error.clone())).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Err(error));
    }
}
