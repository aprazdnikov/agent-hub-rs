//! Pure parsing of bot commands and their arguments.

use crate::domain::{BackendKind, TopicSession};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Help,
    New,
    Cwd,
    Reset,
    Stop,
    Status,
    Backend,
}

impl Command {
    pub const ALL: [Self; 7] =
        [Self::New, Self::Backend, Self::Cwd, Self::Reset, Self::Stop, Self::Status, Self::Help];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Help => "help",
            Self::New => "new",
            Self::Cwd => "cwd",
            Self::Reset => "reset",
            Self::Stop => "stop",
            Self::Status => "status",
            Self::Backend => "backend",
        }
    }

    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Help => "эта справка",
            Self::New => "новая сессия в этой теме (сброс контекста)",
            Self::Cwd => "сменить рабочую директорию (сброс контекста)",
            Self::Reset => "начать разговор заново в той же директории",
            Self::Stop => "прервать текущую задачу",
            Self::Status => "состояние сессии",
            Self::Backend => "сменить агента в этой теме (сброс контекста)",
        }
    }

    #[must_use]
    pub const fn arguments(self) -> &'static str {
        match self {
            Self::New => " [агент] [путь]",
            Self::Backend => " [агент]",
            Self::Cwd => " <путь>",
            Self::Help | Self::Reset | Self::Stop | Self::Status => "",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("start") {
            Some(Self::Help)
        } else {
            Self::ALL.into_iter().find(|command| name.eq_ignore_ascii_case(command.name()))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input<'a> {
    Command {
        command: Command,
        args: Vec<&'a str>,
    },
    /// A command this bot does not know or one addressed to another bot; ignored.
    Unknown,
    Text,
}

#[must_use]
pub fn parse_input<'a>(text: &'a str, bot: &str) -> Input<'a> {
    let Some(body) = text.strip_prefix('/') else {
        return Input::Text;
    };
    let mut words = body.split_whitespace();
    let Some(head) = words.next() else {
        return Input::Text;
    };
    let (name, mention) =
        head.split_once('@').map_or((head, None), |(name, bot)| (name, Some(bot)));
    if mention.is_some_and(|mention| !mention.eq_ignore_ascii_case(bot)) {
        return Input::Unknown;
    }
    Command::parse(name)
        .map_or(Input::Unknown, |command| Input::Command { command, args: words.collect() })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSessionArgs {
    pub backend: BackendKind,
    pub cwd: Option<String>,
}

/// `/new [backend] [path]`; a first word that is not a backend name starts the path.
#[must_use]
pub fn parse_new_args(args: &[&str], default: BackendKind) -> NewSessionArgs {
    match args.split_first() {
        None => NewSessionArgs { backend: default, cwd: None },
        Some((first, rest)) => match BackendKind::parse(first) {
            Some(backend) => NewSessionArgs { backend, cwd: join_path_args(rest) },
            None => NewSessionArgs { backend: default, cwd: join_path_args(args) },
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendDecision {
    Show(TopicSession),
    Unknown(String),
    AlreadySelected(TopicSession),
    Switch(TopicSession),
}

/// `/backend [name]`: no name shows the current session.
#[must_use]
pub fn decide_backend(args: &[&str], current: &TopicSession) -> BackendDecision {
    match args {
        [] => BackendDecision::Show(current.clone()),
        [name] => match BackendKind::parse(name) {
            None => BackendDecision::Unknown((*name).to_owned()),
            Some(kind) if kind == current.backend => {
                BackendDecision::AlreadySelected(current.clone())
            }
            // One agent cannot continue another's session.
            Some(kind) => BackendDecision::Switch(TopicSession::fresh(kind, current.cwd.clone())),
        },
        [_, _, ..] => BackendDecision::Unknown(args.join(" ")),
    }
}

#[must_use]
pub fn join_path_args(args: &[&str]) -> Option<String> {
    let joined = args.join(" ");
    let trimmed = joined.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::domain::{AbsolutePath, SessionId, TopicSession};

    #[test]
    fn menu_registry_is_unique_valid_and_parsed() {
        let mut names = std::collections::HashSet::new();
        for command in Command::ALL {
            let name = command.name();
            assert!(names.insert(name));
            assert!((1..=32).contains(&name.len()));
            assert!(name.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_'));
            assert!((1..=256).contains(&command.description().chars().count()));
            assert_eq!(Command::parse(name), Some(command));
        }
        assert_eq!(names.len(), 7);
    }

    #[rstest]
    #[case(&[], BackendKind::Codex, None)]
    #[case(&["claude"], BackendKind::Claude, None)]
    #[case(&["Claude", "shop/backend"], BackendKind::Claude, Some("shop/backend"))]
    #[case(&["codex", "shop"], BackendKind::Codex, Some("shop"))]
    #[case(&["qwen"], BackendKind::Qwen, None)]
    #[case(&["QWEN", "shop"], BackendKind::Qwen, Some("shop"))]
    #[case(&["shop/backend"], BackendKind::Codex, Some("shop/backend"))]
    #[case(&["my", "dir"], BackendKind::Codex, Some("my dir"))]
    fn new_args_fall_back_to_the_default_backend(
        #[case] args: &[&str],
        #[case] backend: BackendKind,
        #[case] cwd: Option<&str>,
    ) {
        assert_eq!(
            parse_new_args(args, BackendKind::Codex),
            NewSessionArgs { backend, cwd: cwd.map(str::to_owned) }
        );
    }

    fn current() -> TopicSession {
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        TopicSession::fresh(BackendKind::Claude, cwd).with_session(SessionId::parse("s-1"))
    }

    #[test]
    fn backend_without_a_name_shows_the_session() {
        assert_eq!(decide_backend(&[], &current()), BackendDecision::Show(current()));
    }

    #[test]
    fn same_backend_is_already_selected() {
        assert_eq!(
            decide_backend(&["CLAUDE"], &current()),
            BackendDecision::AlreadySelected(current())
        );
    }

    #[test]
    fn other_backend_keeps_the_directory_and_drops_the_session() {
        let switched = TopicSession::fresh(BackendKind::Codex, current().cwd);
        assert_eq!(decide_backend(&["codex"], &current()), BackendDecision::Switch(switched));
    }

    #[test]
    fn qwen_is_a_backend_to_switch_to() {
        let switched = TopicSession::fresh(BackendKind::Qwen, current().cwd);
        assert_eq!(decide_backend(&["qwen"], &current()), BackendDecision::Switch(switched));
    }

    #[rstest]
    #[case(&["hermes"], None)]
    #[case(&["HERMES", "my", "dir"], Some("my dir"))]
    fn hermes_new_args_select_backend(#[case] args: &[&str], #[case] cwd: Option<&str>) {
        let parsed = parse_new_args(args, BackendKind::Claude);
        assert_eq!((parsed.backend.name(), parsed.cwd.as_deref()), ("hermes", cwd));
    }

    #[test]
    fn hermes_backend_switch_resets_context() {
        let decision = decide_backend(&["Hermes"], &current());
        assert!(matches!(
            decision,
            BackendDecision::Switch(session)
                if session.backend.name() == "hermes"
                    && session.cwd == current().cwd
                    && session.session.is_none()
        ));
    }

    #[test]
    fn hermes_backend_already_selected_keeps_session() {
        let session = TopicSession::fresh(BackendKind::Hermes, current().cwd)
            .with_session(SessionId::parse("h-1"));
        assert_eq!(
            decide_backend(&["HERMES"], &session),
            BackendDecision::AlreadySelected(session)
        );
    }

    #[test]
    fn hermes_default_is_used_for_a_new_path() {
        assert_eq!(
            parse_new_args(&["shop"], BackendKind::Hermes),
            NewSessionArgs { backend: BackendKind::Hermes, cwd: Some("shop".to_owned()) }
        );
    }

    #[rstest]
    #[case(&["gpt"], "gpt")]
    #[case(&["codex", "now"], "codex now")]
    fn unknown_backend_names_what_was_typed(#[case] args: &[&str], #[case] name: &str) {
        assert_eq!(decide_backend(args, &current()), BackendDecision::Unknown(name.to_owned()));
    }

    #[test]
    fn path_args_are_joined() {
        assert_eq!(join_path_args(&[]), None);
        assert_eq!(join_path_args(&["a", "b"]), Some("a b".to_owned()));
    }

    #[rstest]
    #[case("/new shop backend", Input::Command { command: Command::New, args: vec!["shop", "backend"] })]
    #[case("/backend codex", Input::Command { command: Command::Backend, args: vec!["codex"] })]
    #[case("/start", Input::Command { command: Command::Help, args: vec![] })]
    #[case("/STOP", Input::Command { command: Command::Stop, args: vec![] })]
    #[case("/cwd@agent_hub_bot x", Input::Command { command: Command::Cwd, args: vec!["x"] })]
    #[case("/cwd@Agent_Hub_Bot x", Input::Command { command: Command::Cwd, args: vec!["x"] })]
    #[case("/cwd@other_bot x", Input::Unknown)]
    #[case("/unknown", Input::Unknown)]
    #[case("hello /new", Input::Text)]
    #[case("/", Input::Text)]
    fn input_is_classified(#[case] text: &str, #[case] expected: Input<'_>) {
        assert_eq!(parse_input(text, "agent_hub_bot"), expected);
    }
}
