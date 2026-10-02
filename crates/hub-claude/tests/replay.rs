//! Real CLI transcripts replayed through activity tracking and event translation.

#[cfg(test)]
mod tests {
    use hub_claude::activity::{Phase, SessionActivity};
    use hub_claude::tracker::SessionTracker;
    use hub_claude::wire::{Incoming, User, parse_line};
    use hub_core::domain::AgentEvent;
    use rstest::rstest;

    fn replay(name: &str) -> (Vec<AgentEvent>, Phase) {
        let path = format!("{}/tests/fixtures/{name}.jsonl", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(path).unwrap();
        let messages: Vec<Incoming> = text.lines().map(|line| parse_line(line).unwrap()).collect();
        let prompt = messages
            .iter()
            .find_map(|message| match message {
                Incoming::User(User { uuid: Some(uuid) }) => Some(uuid.clone()),
                _ => None,
            })
            .unwrap();
        let mut activity = SessionActivity::default();
        let mut tracker = SessionTracker::default();
        activity.sent(prompt);
        let events = messages
            .iter()
            .flat_map(|message| {
                activity.observe(message);
                tracker.translate(message, activity.background().len())
            })
            .collect();
        (events, activity.phase())
    }

    #[rstest]
    #[case("approval")]
    #[case("question")]
    #[case("sendfile")]
    #[case("background")]
    fn transcript_replays_to_idle(#[case] name: &str) {
        let (events, phase) = replay(name);
        assert_eq!(phase, Phase::Idle);
        insta::assert_debug_snapshot!(format!("replay_{name}"), events);
    }
}
