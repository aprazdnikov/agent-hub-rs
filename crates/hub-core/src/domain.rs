use std::path::{Path, PathBuf};

use rust_decimal::Decimal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChatId(pub i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThreadId(pub i32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MessageId(pub i32);

/// One forum topic of one Telegram chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TopicKey {
    pub chat: ChatId,
    pub thread: ThreadId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionId(String);

impl SessionId {
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    Claude,
}

impl BackendKind {
    pub const ALL: [Self; 1] = [Self::Claude];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.name().eq_ignore_ascii_case(raw))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AbsolutePath(PathBuf);

impl AbsolutePath {
    #[must_use]
    pub fn new(path: PathBuf) -> Option<Self> {
        path.is_absolute().then_some(Self(path))
    }

    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopicSession {
    pub backend: BackendKind,
    pub cwd: AbsolutePath,
    pub session: Option<SessionId>,
}

impl TopicSession {
    #[must_use]
    pub fn fresh(backend: BackendKind, cwd: AbsolutePath) -> Self {
        Self { backend, cwd, session: None }
    }

    #[must_use]
    pub fn with_session(self, session: Option<SessionId>) -> Self {
        Self { session, ..self }
    }
}

/// A tool call the agent wants to make, awaiting a human decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRequest {
    pub tool: String,
    pub summary: String,
}

/// A tool call the agent made, shown to the user as one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUse {
    pub tool: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied {
    pub reason: String,
}

impl Denied {
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self { reason: reason.into() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    Denied(Denied),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Selection {
    Single,
    Multiple,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// A clarifying question, answered by an option label or free text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    text: String,
    header: String,
    options: Vec<QuestionOption>,
    selection: Selection,
}

impl Question {
    #[must_use]
    pub fn new(
        text: String,
        header: String,
        options: Vec<QuestionOption>,
        selection: Selection,
    ) -> Option<Self> {
        (!text.trim().is_empty()).then_some(Self { text, header, options, selection })
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn header(&self) -> &str {
        &self.header
    }

    #[must_use]
    pub fn options(&self) -> &[QuestionOption] {
        &self.options
    }

    #[must_use]
    pub fn selection(&self) -> Selection {
        self.selection
    }
}

/// Answer keyed by question text; multi-select labels are joined with ", ".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionAnswer {
    pub question: String,
    pub answer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuestionsOutcome {
    Answered(Vec<QuestionAnswer>),
    Denied(Denied),
}

/// A file the agent asks to deliver; `path` is as the agent wrote it, not yet checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingFile {
    pub path: String,
    pub caption: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDelivery {
    Delivered,
    Denied(Denied),
}

/// Image formats the model accepts inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageMediaType {
    Jpeg,
    Png,
    Gif,
    Webp,
}

impl ImageMediaType {
    #[must_use]
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Gif => "image/gif",
            Self::Webp => "image/webp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub media: ImageMediaType,
    pub data: Vec<u8>,
}

/// One user turn: text plus images sent inline to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    text: String,
    images: Vec<Image>,
}

impl Prompt {
    #[must_use]
    pub fn new(text: String, images: Vec<Image>) -> Option<Self> {
        (!text.trim().is_empty() || !images.is_empty()).then_some(Self { text, images })
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn images(&self) -> &[Image] {
        &self.images
    }
}

/// One agent turn ended; `background` tasks keep running and report in later turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    pub session: SessionId,
    pub turns: u32,
    pub cost: Option<Decimal>,
    pub background: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    SessionStarted(SessionId),
    AssistantText(String),
    ToolCall(ToolUse),
    Finished(Finished),
    Failed(String),
    BackgroundAbandoned(Vec<String>),
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rstest::rstest;

    use super::*;

    fn absolute() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" })
    }

    #[rstest]
    #[case("", None)]
    #[case("   ", None)]
    #[case(" abc ", Some("abc"))]
    fn session_id_is_non_empty(#[case] raw: &str, #[case] expected: Option<&str>) {
        assert_eq!(SessionId::parse(raw).as_ref().map(SessionId::as_str), expected);
    }

    #[rstest]
    #[case("claude", Some(BackendKind::Claude))]
    #[case("Claude", Some(BackendKind::Claude))]
    #[case("gpt", None)]
    fn backend_kind_parses_case_insensitively(
        #[case] raw: &str,
        #[case] expected: Option<BackendKind>,
    ) {
        assert_eq!(BackendKind::parse(raw), expected);
    }

    #[test]
    fn relative_cwd_is_rejected() {
        assert!(AbsolutePath::new(PathBuf::from("rel")).is_none());
        assert_eq!(
            AbsolutePath::new(absolute()).map(|p| p.as_path().to_path_buf()),
            Some(absolute())
        );
    }

    #[test]
    fn prompt_needs_text_or_images() {
        assert!(Prompt::new("  ".to_owned(), Vec::new()).is_none());
        let image = Image { media: ImageMediaType::Png, data: vec![1] };
        let prompt = Prompt::new(String::new(), vec![image]).unwrap();
        assert_eq!(prompt.images().len(), 1);
        assert_eq!(Prompt::new("hi".to_owned(), Vec::new()).unwrap().text(), "hi");
    }

    #[test]
    fn question_needs_text() {
        assert!(
            Question::new(" ".to_owned(), String::new(), Vec::new(), Selection::Single).is_none()
        );
    }

    #[test]
    fn with_session_replaces_only_session() {
        let cwd = AbsolutePath::new(absolute()).unwrap();
        let session = TopicSession::fresh(BackendKind::Claude, cwd.clone());
        let resumed = session.with_session(SessionId::parse("s"));
        assert_eq!(resumed.cwd, cwd);
        assert_eq!(resumed.session, SessionId::parse("s"));
    }

    #[test]
    fn image_media_types_are_api_mime_types() {
        let mimes: Vec<_> =
            [ImageMediaType::Jpeg, ImageMediaType::Png, ImageMediaType::Gif, ImageMediaType::Webp]
                .into_iter()
                .map(ImageMediaType::mime)
                .collect();
        assert_eq!(mimes, ["image/jpeg", "image/png", "image/gif", "image/webp"]);
    }
}
