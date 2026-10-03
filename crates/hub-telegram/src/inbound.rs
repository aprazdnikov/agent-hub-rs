//! Telegram updates reduced to what the hub acts on, independent of the Bot API library.

use hub_core::domain::{ChatId, MessageId, ThreadId, TopicKey, UserId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRef {
    pub id: String,
    pub name: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    /// Telegram re-encodes stored photos as JPEG; the model sees them inline.
    Photo(FileRef),
    /// Documents, audio and video land in the session's uploads directory.
    File(FileRef),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Media { caption: String, attachment: Attachment },
    Voice,
    TopicCreated { name: String },
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub chat: ChatId,
    /// Set only for messages inside a forum topic.
    pub thread: Option<ThreadId>,
    pub user: Option<UserId>,
    pub message: MessageId,
    pub album: Option<String>,
    pub content: Content,
}

impl Inbound {
    #[must_use]
    pub fn key(&self) -> Option<TopicKey> {
        self.thread.map(|thread| TopicKey { chat: self.chat, thread })
    }
}

/// A button press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Press {
    pub callback: String,
    pub user: UserId,
    pub chat: Option<ChatId>,
    pub message: Option<MessageId>,
    /// The pressed message as Telegram HTML, to append a verdict to.
    pub html: Option<String>,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub message: MessageId,
    pub file: FileRef,
}

/// One user turn before its attachments are downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub text: String,
    pub photos: Vec<Upload>,
    pub files: Vec<Upload>,
}

impl Turn {
    #[must_use]
    pub fn text(text: String) -> Self {
        Self { text, photos: Vec::new(), files: Vec::new() }
    }

    /// One turn from a message or an album: captions joined, attachments in message order.
    #[must_use]
    pub fn from_parts(parts: &[Inbound]) -> Self {
        let mut photos = Vec::new();
        let mut files = Vec::new();
        let mut texts = Vec::new();
        for part in parts {
            match &part.content {
                Content::Text(text) => texts.push(text.as_str()),
                Content::Media { caption, attachment } => {
                    texts.push(caption.as_str());
                    match attachment {
                        Attachment::Photo(file) => {
                            photos.push(Upload { message: part.message, file: file.clone() });
                        }
                        Attachment::File(file) => {
                            files.push(Upload { message: part.message, file: file.clone() });
                        }
                    }
                }
                Content::Voice | Content::TopicCreated { .. } | Content::Ignored => {}
            }
        }
        let text = texts.into_iter().filter(|text| !text.is_empty()).collect::<Vec<_>>().join("\n");
        Self { text, photos, files }
    }

    #[must_use]
    pub fn has_attachments(&self) -> bool {
        !self.photos.is_empty() || !self.files.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(message: i32, caption: &str, attachment: Attachment) -> Inbound {
        Inbound {
            chat: ChatId(-100),
            thread: Some(ThreadId(7)),
            user: Some(UserId(1)),
            message: MessageId(message),
            album: Some("g".to_owned()),
            content: Content::Media { caption: caption.to_owned(), attachment },
        }
    }

    fn file(id: &str, name: Option<&str>) -> FileRef {
        FileRef { id: id.to_owned(), name: name.map(str::to_owned), size: Some(10) }
    }

    #[test]
    fn key_needs_a_topic() {
        let mut inbound = part(1, "", Attachment::Photo(file("p", None)));
        assert_eq!(inbound.key(), Some(TopicKey { chat: ChatId(-100), thread: ThreadId(7) }));
        inbound.thread = None;
        assert_eq!(inbound.key(), None);
    }

    #[test]
    fn album_joins_captions_and_keeps_attachment_order() {
        let parts = [
            part(1, "первая", Attachment::Photo(file("p1", None))),
            part(2, "", Attachment::File(file("d1", Some("a.pdf")))),
            part(3, "третья", Attachment::Photo(file("p2", None))),
        ];
        let turn = Turn::from_parts(&parts);
        assert_eq!(turn.text, "первая\nтретья");
        assert_eq!(
            turn.photos.iter().map(|u| (u.message, u.file.id.as_str())).collect::<Vec<_>>(),
            [(MessageId(1), "p1"), (MessageId(3), "p2")]
        );
        assert_eq!(
            turn.files.iter().map(|u| u.file.name.as_deref()).collect::<Vec<_>>(),
            [Some("a.pdf")]
        );
        assert!(turn.has_attachments());
        assert!(!Turn::text("hi".to_owned()).has_attachments());
    }
}
