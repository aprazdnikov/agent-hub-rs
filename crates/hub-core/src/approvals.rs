//! Pending human decisions on tool calls, keyed by an opaque id carried in button data.

use std::collections::HashMap;

use crate::domain::{Decision, Denied};
use crate::ids::hex;

pub const CALLBACK_PREFIX: &str = "ap";
pub const DENIED_BY_USER: &str = "Пользователь запретил этот вызов";

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ApprovalId(String);

impl ApprovalId {
    #[must_use]
    pub fn from_random(bytes: [u8; 8]) -> Self {
        Self(hex(&bytes))
    }

    fn parse(raw: &str) -> Option<Self> {
        (!raw.is_empty()).then(|| Self(raw.to_owned()))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    Deny,
}

impl Verdict {
    const fn code(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    fn parse(code: &str) -> Option<Self> {
        [Self::Allow, Self::Deny].into_iter().find(|verdict| verdict.code() == code)
    }

    #[must_use]
    pub fn decision(self) -> Decision {
        match self {
            Self::Allow => Decision::Allowed,
            Self::Deny => Decision::Denied(Denied::new(DENIED_BY_USER)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalAnswer {
    pub id: ApprovalId,
    pub verdict: Verdict,
}

#[must_use]
pub fn callback_data(id: &ApprovalId, verdict: Verdict) -> String {
    format!("{CALLBACK_PREFIX}:{}:{}", verdict.code(), id.0)
}

#[must_use]
pub fn parse_callback_data(data: &str) -> Option<ApprovalAnswer> {
    match data.split(':').collect::<Vec<_>>().as_slice() {
        [prefix, verdict, id] if *prefix == CALLBACK_PREFIX => {
            Some(ApprovalAnswer { verdict: Verdict::parse(verdict)?, id: ApprovalId::parse(id)? })
        }
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Answered(Verdict),
    TimedOut,
}

#[derive(Debug)]
enum ApprovalState<R> {
    Pending(R),
    Finished(ApprovalOutcome),
}

/// Approval state until the asker takes the outcome or cancels. `R` only wakes the asker;
/// the registry retains the authoritative outcome even if notification is delayed or lost.
/// Callers must serialize access to this registry (including `resolve`, `finish`, and `close`).
#[derive(Debug)]
pub struct ApprovalRegistry<R> {
    requests: HashMap<ApprovalId, ApprovalState<R>>,
}

impl<R> Default for ApprovalRegistry<R> {
    fn default() -> Self {
        Self { requests: HashMap::new() }
    }
}

impl<R> ApprovalRegistry<R> {
    pub fn open(&mut self, id: ApprovalId, responder: R) {
        self.requests.insert(id, ApprovalState::Pending(responder));
    }

    pub fn close(&mut self, id: &ApprovalId) {
        self.requests.remove(id);
    }

    /// Commits a button answer and returns its notifier only to the winning handler.
    /// A successful notification is not evidence that the asker received it.
    pub fn resolve(&mut self, answer: &ApprovalAnswer) -> Option<R> {
        self.complete(&answer.id, ApprovalOutcome::Answered(answer.verdict))
    }

    /// Takes the committed outcome, committing a timeout only if no answer won first.
    /// An absent (closed) request is also denied. No tombstones remain after this call.
    pub fn finish(&mut self, id: &ApprovalId) -> ApprovalOutcome {
        self.complete(id, ApprovalOutcome::TimedOut);
        match self.requests.remove(id) {
            Some(ApprovalState::Finished(outcome)) => outcome,
            Some(ApprovalState::Pending(_)) | None => ApprovalOutcome::TimedOut,
        }
    }

    /// The only Pending -> Finished transition. The caller's lock makes the first
    /// transition authoritative for both button handlers and the timeout path.
    fn complete(&mut self, id: &ApprovalId, outcome: ApprovalOutcome) -> Option<R> {
        let state = self.requests.get_mut(id)?;
        match state {
            ApprovalState::Pending(_) => {
                match std::mem::replace(state, ApprovalState::Finished(outcome)) {
                    ApprovalState::Pending(responder) => Some(responder),
                    ApprovalState::Finished(_) => None,
                }
            }
            ApprovalState::Finished(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn id() -> ApprovalId {
        ApprovalId::from_random([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef])
    }

    #[rstest]
    fn callback_data_round_trips(#[values(Verdict::Allow, Verdict::Deny)] verdict: Verdict) {
        let data = callback_data(&id(), verdict);
        assert!(data.len() <= 64, "Telegram callback_data limit");
        assert_eq!(parse_callback_data(&data), Some(ApprovalAnswer { id: id(), verdict }));
    }

    #[rstest]
    #[case("")]
    #[case("ap")]
    #[case("ap:allow:")]
    #[case("ap:maybe:x")]
    #[case("xx:allow:x")]
    #[case("ap:allow:x:y")]
    fn malformed_callback_data_is_rejected(#[case] data: &str) {
        assert_eq!(parse_callback_data(data), None);
    }

    #[test]
    fn resolve_delivers_decision_once() {
        let mut registry = ApprovalRegistry::default();
        registry.open(id(), "responder");

        let answer = ApprovalAnswer { id: id(), verdict: Verdict::Allow };
        assert_eq!(registry.resolve(&answer), Some("responder"));
        assert_eq!(registry.resolve(&answer), None);
    }

    #[test]
    fn closed_request_cannot_be_resolved() {
        let mut registry = ApprovalRegistry::default();
        registry.open(id(), ());
        registry.close(&id());

        assert_eq!(registry.resolve(&ApprovalAnswer { id: id(), verdict: Verdict::Allow }), None);
    }

    #[test]
    fn committed_answer_survives_timeout_before_notification() {
        let mut registry = ApprovalRegistry::default();
        registry.open(id(), "notifier");
        let notifier = registry.resolve(&ApprovalAnswer { id: id(), verdict: Verdict::Allow });

        // The button handler has not notified the waiter yet; the timer fires first.
        assert_eq!(registry.finish(&id()), ApprovalOutcome::Answered(Verdict::Allow));
        assert!(notifier.is_some());
        assert!(registry.requests.is_empty());
    }

    #[test]
    fn timeout_prevents_a_button_from_changing_the_result() {
        let mut registry = ApprovalRegistry::default();
        registry.open(id(), ());

        assert_eq!(registry.finish(&id()), ApprovalOutcome::TimedOut);
        assert_eq!(registry.resolve(&ApprovalAnswer { id: id(), verdict: Verdict::Allow }), None);
    }

    #[test]
    fn close_discards_an_unconsumed_answer() {
        let mut registry = ApprovalRegistry::default();
        registry.open(id(), ());
        registry.resolve(&ApprovalAnswer { id: id(), verdict: Verdict::Allow });
        registry.close(&id());

        assert!(registry.requests.is_empty());
    }

    #[test]
    fn competing_threads_only_commit_one_answer() {
        use std::sync::{Barrier, Mutex};

        let mut registry = ApprovalRegistry::default();
        registry.open(id(), ());
        let registry = Mutex::new(registry);
        let start = Barrier::new(2);
        let accepted = std::thread::scope(|scope| {
            let attempt = |verdict| {
                start.wait();
                let accepted = registry
                    .lock()
                    .unwrap()
                    .resolve(&ApprovalAnswer { id: id(), verdict })
                    .is_some();
                accepted.then_some(verdict)
            };
            let allow = scope.spawn(move || attempt(Verdict::Allow));
            let deny = scope.spawn(move || attempt(Verdict::Deny));
            [allow.join().unwrap(), deny.join().unwrap()]
        });

        let ([Some(winner), None] | [None, Some(winner)]) = accepted else {
            panic!("exactly one button must be accepted");
        };
        assert_eq!(registry.lock().unwrap().finish(&id()), ApprovalOutcome::Answered(winner));
    }

    #[test]
    fn deny_carries_reason() {
        assert_eq!(Verdict::Deny.decision(), Decision::Denied(Denied::new(DENIED_BY_USER)));
    }
}
