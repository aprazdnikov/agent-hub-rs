//! Pure parsing of bot commands and their arguments.

use crate::domain::BackendKind;

pub const DEFAULT_BACKEND: BackendKind = BackendKind::Claude;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Help,
    New,
    Cwd,
    Reset,
    Stop,
    Status,
}

impl Command {
    fn parse(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "start" | "help" => Some(Self::Help),
            "new" => Some(Self::New),
            "cwd" => Some(Self::Cwd),
            "reset" => Some(Self::Reset),
            "stop" => Some(Self::Stop),
            "status" => Some(Self::Status),
            _ => None,
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
pub fn parse_new_args(args: &[&str]) -> NewSessionArgs {
    match args.split_first() {
        None => NewSessionArgs { backend: DEFAULT_BACKEND, cwd: None },
        Some((first, rest)) => match BackendKind::parse(first) {
            Some(backend) => NewSessionArgs { backend, cwd: join_path_args(rest) },
            None => NewSessionArgs { backend: DEFAULT_BACKEND, cwd: join_path_args(args) },
        },
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

    #[rstest]
    #[case(&[], None)]
    #[case(&["claude"], None)]
    #[case(&["Claude", "shop/backend"], Some("shop/backend"))]
    #[case(&["shop/backend"], Some("shop/backend"))]
    #[case(&["my", "dir"], Some("my dir"))]
    fn new_args(#[case] args: &[&str], #[case] cwd: Option<&str>) {
        assert_eq!(
            parse_new_args(args),
            NewSessionArgs { backend: BackendKind::Claude, cwd: cwd.map(str::to_owned) }
        );
    }

    #[test]
    fn path_args_are_joined() {
        assert_eq!(join_path_args(&[]), None);
        assert_eq!(join_path_args(&["a", "b"]), Some("a b".to_owned()));
    }

    #[rstest]
    #[case("/new shop backend", Input::Command { command: Command::New, args: vec!["shop", "backend"] })]
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
