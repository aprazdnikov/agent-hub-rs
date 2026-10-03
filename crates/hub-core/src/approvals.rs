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

/// Open approval requests; `R` is whatever delivers the decision back to the asker.
#[derive(Debug)]
pub struct ApprovalRegistry<R> {
    pending: HashMap<ApprovalId, R>,
}

impl<R> Default for ApprovalRegistry<R> {
    fn default() -> Self {
        Self { pending: HashMap::new() }
    }
}

impl<R> ApprovalRegistry<R> {
    pub fn open(&mut self, id: ApprovalId, responder: R) {
        self.pending.insert(id, responder);
    }

    pub fn close(&mut self, id: &ApprovalId) {
        self.pending.remove(id);
    }

    /// The responder and decision; `None` when the request is gone (answered, timed out, stopped).
    pub fn resolve(&mut self, answer: &ApprovalAnswer) -> Option<(R, Decision)> {
        self.pending.remove(&answer.id).map(|responder| (responder, answer.verdict.decision()))
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
        assert_eq!(registry.resolve(&answer), Some(("responder", Decision::Allowed)));
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
    fn deny_carries_reason() {
        assert_eq!(Verdict::Deny.decision(), Decision::Denied(Denied::new(DENIED_BY_USER)));
    }
}
