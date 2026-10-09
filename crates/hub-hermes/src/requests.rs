//! Session-scoped permissions. Persistent allow options are never selected.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hub_agent::channel::UserChannel;
use hub_agent::rpc::{Handler, RequestError};
use hub_core::domain::{Decision, SessionId, ToolRequest, ToolUse};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::protocol::tool_use;

const HUMAN_TIMEOUT: Duration = Duration::from_mins(5);

#[derive(Clone)]
pub struct Gate {
    session: Arc<Mutex<Option<SessionId>>>,
    cancel: CancellationToken,
}

impl Gate {
    pub fn new(cancel: CancellationToken) -> Self {
        Self { session: Arc::new(Mutex::new(None)), cancel }
    }

    pub fn bind(&self, session: SessionId) {
        *self.session.lock().unwrap_or_else(PoisonError::into_inner) = Some(session);
    }

    pub fn close(&self) {
        self.cancel.cancel();
        self.session.lock().unwrap_or_else(PoisonError::into_inner).take();
    }

    pub fn handler(&self, channel: Arc<dyn UserChannel>) -> Handler {
        let gate = self.clone();
        Arc::new(move |method, params| {
            let gate = gate.clone();
            let channel = Arc::clone(&channel);
            Box::pin(async move { gate.answer(channel.as_ref(), &method, &params).await })
        })
    }

    async fn answer(
        &self,
        channel: &dyn UserChannel,
        method: &str,
        params: &Value,
    ) -> Result<Value, RequestError> {
        if method != "session/request_permission" {
            return Err(RequestError::Unsupported(method.to_owned()));
        }
        let session = self.session.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if session.as_ref().map(SessionId::as_str)
            != params.get("sessionId").and_then(Value::as_str)
            || session.is_none()
        {
            return Err(RequestError::Malformed(
                "permission request for an unopened or different session".to_owned(),
            ));
        }
        let Permission { allow, deny, tool } = Permission::parse(params)?;
        let cancelled = || json!({"outcome": {"outcome": "cancelled"}});
        if self.cancel.is_cancelled() {
            return Ok(cancelled());
        }
        let decision = tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Ok(cancelled()),
            decision = tokio::time::timeout(HUMAN_TIMEOUT, channel.request(tool)) => decision,
        };
        if self.cancel.is_cancelled() {
            return Ok(cancelled());
        }
        let option = match decision {
            Ok(Decision::Allowed) => allow,
            Ok(Decision::Denied(_)) => deny,
            Err(_) => {
                tracing::warn!("Hermes permission answer deadline expired; rejecting once");
                deny
            }
        };
        Ok(json!({"outcome": {"outcome": "selected", "optionId": option}}))
    }
}

struct Permission {
    allow: String,
    deny: String,
    tool: ToolRequest,
}

impl Permission {
    fn parse(params: &Value) -> Result<Self, RequestError> {
        let malformed = || {
            RequestError::Malformed("permission requires unique offered allow_once and reject_once options and a toolCall".to_owned())
        };
        let options = params.get("options").and_then(Value::as_array).ok_or_else(malformed)?;
        let mut ids = HashSet::new();
        let mut allow = None;
        let mut deny = None;
        for option in options {
            let id = option
                .get("optionId")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(malformed)?;
            if !ids.insert(id) {
                return Err(malformed());
            }
            match option.get("kind").and_then(Value::as_str) {
                Some("allow_once") if allow.is_none() => allow = Some(id),
                Some("reject_once") if deny.is_none() => deny = Some(id),

                Some("allow_always" | "reject_always") => {}
                Some(_) | None => return Err(malformed()),
            }
        }
        let call = params.get("toolCall").filter(|call| call.is_object()).ok_or_else(malformed)?;
        call.get("toolCallId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(malformed)?;
        let ToolUse { tool, summary } = tool_use(call);
        Ok(Self {
            allow: allow.ok_or_else(malformed)?.to_owned(),
            deny: deny.ok_or_else(malformed)?.to_owned(),
            tool: ToolRequest { tool, summary },
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use hub_core::domain::{Denied, FileDelivery, OutgoingFile, Question, QuestionsOutcome};
    use rstest::rstest;

    struct Human {
        decision: Decision,
        called: std::sync::atomic::AtomicBool,
        hold: bool,
    }

    impl UserChannel for Human {
        fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
            self.called.store(true, std::sync::atomic::Ordering::Relaxed);
            Box::pin(async move {
                if self.hold {
                    std::future::pending::<()>().await;
                }
                self.decision.clone()
            })
        }
        fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            Box::pin(async { QuestionsOutcome::Denied(Denied::new("not supported natively")) })
        }
        fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            Box::pin(async { FileDelivery::Delivered })
        }
    }

    fn human(decision: Decision, hold: bool) -> Human {
        Human { decision, called: std::sync::atomic::AtomicBool::new(false), hold }
    }

    fn offered() -> Value {
        json!({"sessionId": "s-1", "toolCall": {"toolCallId": "t", "title": "dangerous command", "kind": "execute"}, "options": [
            {"optionId": "forever", "kind": "allow_always"},
            {"optionId": "yes", "kind": "allow_once"},
            {"optionId": "no", "kind": "reject_once"},
        ]})
    }

    fn gate(cancel: CancellationToken) -> Gate {
        let gate = Gate::new(cancel);
        gate.bind(SessionId::parse("s-1").unwrap());
        gate
    }

    #[rstest]
    #[case(Decision::Allowed, "yes")]
    #[case(Decision::Denied(Denied::new("denied")), "no")]
    #[tokio::test]
    async fn chooses_once_only_by_offered_kind(#[case] decision: Decision, #[case] option: &str) {
        let gate = gate(CancellationToken::new());
        let human = human(decision, false);
        assert_eq!(
            gate.answer(&human, "session/request_permission", &offered()).await,
            Ok(json!({"outcome": {"outcome": "selected", "optionId": option}}))
        );
    }

    #[tokio::test]
    async fn cancellation_beats_a_ready_approval_without_asking() {
        let cancel = CancellationToken::new();
        let gate = gate(cancel.clone());
        let human = human(Decision::Allowed, false);
        cancel.cancel();
        assert_eq!(
            gate.answer(&human, "session/request_permission", &offered()).await,
            Ok(json!({"outcome": {"outcome": "cancelled"}}))
        );
        // request() is not polled when cancellation already won.
        assert!(!human.called.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test(start_paused = true)]
    async fn approval_wait_is_bounded_and_rejects_once_on_timeout() {
        let gate = gate(CancellationToken::new());
        let human = human(Decision::Allowed, true);
        assert_eq!(
            gate.answer(&human, "session/request_permission", &offered()).await,
            Ok(json!({"outcome": {"outcome": "selected", "optionId": "no"}}))
        );
    }

    #[tokio::test]
    async fn cancel_interrupts_a_pending_human_answer() {
        let cancel = CancellationToken::new();
        let gate = gate(cancel.clone());
        let human = human(Decision::Allowed, true);
        let params = offered();
        let answer = gate.answer(&human, "session/request_permission", &params);
        tokio::pin!(answer);
        tokio::select! {
            result = &mut answer => assert!(result.is_err()),
            () = async { while !human.called.load(std::sync::atomic::Ordering::Relaxed) { tokio::task::yield_now().await; } } => {},
        }
        cancel.cancel();
        assert_eq!(answer.await, Ok(json!({"outcome": {"outcome": "cancelled"}})));
    }

    #[tokio::test]
    async fn closed_gate_cannot_approve_late_requests() {
        let gate = gate(CancellationToken::new());
        let human = human(Decision::Allowed, false);
        gate.close();
        assert!(matches!(
            gate.answer(&human, "session/request_permission", &offered()).await,
            Err(RequestError::Malformed(_))
        ));
        assert!(!human.called.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[rstest]
    #[case("craft/drainMidTurnQueue")]
    #[case("session/request_user_input")]
    #[case("fs/read_text_file")]
    #[tokio::test]
    async fn invented_extensions_are_not_advertised_or_answered(#[case] method: &str) {
        let gate = gate(CancellationToken::new());
        let human = human(Decision::Allowed, false);
        assert_eq!(
            gate.answer(&human, method, &json!({})).await,
            Err(RequestError::Unsupported(method.to_owned()))
        );
    }

    #[test]
    fn duplicate_option_ids_are_rejected() {
        let mut params = offered();
        let options = params.get_mut("options").and_then(Value::as_array_mut).unwrap();
        options.push(json!({"optionId": "yes", "kind": "allow_always"}));
        assert!(matches!(Permission::parse(&params), Err(RequestError::Malformed(_))));
    }

    #[test]
    fn missing_tool_call_id_is_not_an_approval_request() {
        let params = json!({"toolCall": {}, "options": [
            {"optionId": "yes", "kind": "allow_once"},
            {"optionId": "no", "kind": "reject_once"},
        ]});
        assert!(matches!(Permission::parse(&params), Err(RequestError::Malformed(_))));
    }
}
