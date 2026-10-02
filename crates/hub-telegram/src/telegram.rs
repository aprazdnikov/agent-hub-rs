//! The Bot API through teloxide: update conversion, the allowlist guard, the `Messenger`
//! implementation and the long-polling listener.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_core::domain::{ChatId, MessageId, ThreadId, TopicKey, UserId};
use hub_core::questions::Button;
use hub_core::settings::{TelegramSettings, Token};
use teloxide::dispatching::{Dispatcher, ShutdownToken, UpdateFilterExt};
use teloxide::net::Download;
use teloxide::payloads::{
    AnswerCallbackQuerySetters, EditMessageReplyMarkupSetters, EditMessageTextSetters,
    SendChatActionSetters, SendDocumentSetters, SendMessageSetters,
};
use teloxide::prelude::{Bot, CallbackQuery, Message, Requester, Update};
use teloxide::types::{
    self as tg, ChatAction, FileId, InlineKeyboardButton, InlineKeyboardMarkup, InputFile,
    ParseMode, ReplyParameters,
};
use teloxide::utils::render::RenderMessageTextHelper;
use teloxide::{ApiError, RequestError};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::hub::HubMessage;
use crate::inbound::{Attachment, Content, FileRef, Inbound, Press};
use crate::messenger::{Format, Messenger, Outgoing, SendError, Target};

// Uploads and downloads of large files take far longer than ordinary requests.
const FILE_TIMEOUT: Duration = Duration::from_mins(2);

#[must_use]
pub fn convert(message: &Message) -> Inbound {
    // Replies in a plain group also carry a thread id; only forum topics count.
    let thread =
        message.thread_id.filter(|_| message.is_topic_message).map(|thread| ThreadId(thread.0.0));
    Inbound {
        chat: ChatId(message.chat.id.0),
        thread,
        user: message.from.as_ref().map(|user| UserId(user.id.0)),
        message: MessageId(message.id.0),
        album: message.media_group_id().map(|group| group.0.clone()),
        content: content(message),
    }
}

fn content(message: &Message) -> Content {
    let media = |attachment| Content::Media {
        caption: message.caption().unwrap_or_default().to_owned(),
        attachment,
    };
    if let Some(created) = message.forum_topic_created() {
        return Content::TopicCreated { name: created.name.clone() };
    }
    if message.voice().is_some() || message.video_note().is_some() {
        return Content::Voice;
    }
    if let Some(photo) = message.photo().and_then(<[tg::PhotoSize]>::last) {
        return media(Attachment::Photo(reference(&photo.file, None)));
    }
    let file = message
        .document()
        .map(|document| (&document.file, document.file_name.clone()))
        .or_else(|| message.audio().map(|audio| (&audio.file, audio.file_name.clone())))
        .or_else(|| message.video().map(|video| (&video.file, video.file_name.clone())));
    if let Some((file, name)) = file {
        return media(Attachment::File(reference(file, name)));
    }
    match message.text() {
        Some(text) => Content::Text(text.to_owned()),
        None => Content::Ignored,
    }
}

fn reference(file: &tg::FileMeta, name: Option<String>) -> FileRef {
    FileRef { id: file.id.0.clone(), name, size: Some(u64::from(file.size)) }
}

#[must_use]
pub fn pressed(query: &CallbackQuery) -> Press {
    let message = query.regular_message();
    Press {
        callback: query.id.0.clone(),
        user: UserId(query.from.id.0),
        chat: message.map(|message| ChatId(message.chat.id.0)),
        message: message.map(|message| MessageId(message.id.0)),
        html: message.and_then(RenderMessageTextHelper::html_text),
        data: query.data.clone().unwrap_or_default(),
    }
}

#[must_use]
pub fn allowed(telegram: &TelegramSettings, chat: Option<ChatId>, user: Option<UserId>) -> bool {
    chat == Some(telegram.chat) && user.is_some_and(|user| telegram.users.contains(user))
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("Telegram не принял токен бота")]
    InvalidToken,
    #[error("Telegram недоступен: {0}")]
    Network(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatAccess {
    Visible,
    Unseen,
}

/// A chat the bot cannot see yet must not stop the start: on first setup the chat id is still
/// unknown, and the bot has to run to log it from a `rejected update`.
pub fn chat_access(result: Result<(), RequestError>) -> Result<ChatAccess, StartError> {
    match result {
        Ok(()) => Ok(ChatAccess::Visible),
        Err(RequestError::Api(ApiError::ChatNotFound)) => Ok(ChatAccess::Unseen),
        Err(other) => Err(StartError::Network(other.to_string())),
    }
}

pub struct Connection {
    pub bot: Bot,
    pub messenger: Arc<TelegramMessenger>,
    pub username: String,
}

/// Checks the token and the chat before the hub starts.
pub async fn connect(token: &Token, chat: ChatId) -> Result<Connection, StartError> {
    let bot = Bot::new(token.expose());
    let client = teloxide::net::default_reqwest_settings()
        .timeout(FILE_TIMEOUT)
        .build()
        .map_err(|error| StartError::Network(error.to_string()))?;
    let files = Bot::with_client(token.expose(), client);
    let me = bot.get_me().await.map_err(|error| match error {
        RequestError::Api(ApiError::InvalidToken) => StartError::InvalidToken,
        other => StartError::Network(other.to_string()),
    })?;
    match chat_access(bot.get_chat(tg::ChatId(chat.0)).await.map(drop))? {
        ChatAccess::Visible => {}
        ChatAccess::Unseen => {
            tracing::warn!(
                chat_id = chat.0,
                "the bot does not see this chat: add it to the group and check chat_id"
            );
        }
    }
    let username = me.user.username.clone().unwrap_or_default();
    let messenger = Arc::new(TelegramMessenger { bot: bot.clone(), files });
    Ok(Connection { bot, messenger, username })
}

pub struct TelegramMessenger {
    bot: Bot,
    /// The same bot with a longer timeout for file transfers.
    files: Bot,
}

fn sent(error: RequestError) -> SendError {
    match error {
        RequestError::RetryAfter(seconds) => SendError::RetryAfter(seconds.duration()),
        RequestError::Api(api) => SendError::Rejected(api.to_string()),
        other => SendError::Failed(other.to_string()),
    }
}

fn failed(error: impl std::fmt::Display) -> SendError {
    SendError::Failed(error.to_string())
}

fn markup(keyboard: Vec<Vec<Button>>) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(keyboard.into_iter().map(|row| {
        row.into_iter()
            .map(|button| InlineKeyboardButton::callback(button.text, button.data))
            .collect::<Vec<_>>()
    }))
}

fn thread(key: TopicKey) -> tg::ThreadId {
    tg::ThreadId(tg::MessageId(key.thread.0))
}

impl Messenger for TelegramMessenger {
    fn send(
        &self,
        target: Target,
        message: Outgoing,
    ) -> BoxFuture<'_, Result<MessageId, SendError>> {
        Box::pin(async move {
            let Outgoing { text, format, keyboard } = message;
            let request = match target {
                Target::Topic(key) => self
                    .bot
                    .send_message(tg::ChatId(key.chat.0), text)
                    .message_thread_id(thread(key)),
                Target::Reply { chat, message } => self
                    .bot
                    .send_message(tg::ChatId(chat.0), text)
                    .reply_parameters(ReplyParameters::new(tg::MessageId(message.0))),
            };
            let request = match format {
                Format::Html => request.parse_mode(ParseMode::Html),
                Format::Plain => request,
            };
            let request =
                if keyboard.is_empty() { request } else { request.reply_markup(markup(keyboard)) };
            request.await.map(|sent| MessageId(sent.id.0)).map_err(sent)
        })
    }

    fn edit(
        &self,
        chat: ChatId,
        message: MessageId,
        text: String,
        format: Format,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            let request =
                self.bot.edit_message_text(tg::ChatId(chat.0), tg::MessageId(message.0), text);
            let request = match format {
                Format::Html => request.parse_mode(ParseMode::Html),
                Format::Plain => request,
            };
            request.await.map(drop).map_err(sent)
        })
    }

    fn edit_keyboard(
        &self,
        chat: ChatId,
        message: MessageId,
        keyboard: Vec<Vec<Button>>,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            self.bot
                .edit_message_reply_markup(tg::ChatId(chat.0), tg::MessageId(message.0))
                .reply_markup(markup(keyboard))
                .await
                .map(drop)
                .map_err(sent)
        })
    }

    fn answer(
        &self,
        callback: String,
        text: Option<String>,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            let request = self.bot.answer_callback_query(tg::CallbackQueryId(callback));
            let request = match text {
                Some(text) => request.text(text),
                None => request,
            };
            request.await.map(drop).map_err(sent)
        })
    }

    fn document(
        &self,
        key: TopicKey,
        path: PathBuf,
        caption: String,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            let request = self
                .files
                .send_document(tg::ChatId(key.chat.0), InputFile::file(path))
                .message_thread_id(thread(key));
            let request = if caption.is_empty() { request } else { request.caption(caption) };
            request.await.map(drop).map_err(sent)
        })
    }

    fn typing(&self, key: TopicKey) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            self.bot
                .send_chat_action(tg::ChatId(key.chat.0), ChatAction::Typing)
                .message_thread_id(thread(key))
                .await
                .map(drop)
                .map_err(sent)
        })
    }

    fn fetch(&self, file: String) -> BoxFuture<'_, Result<Vec<u8>, SendError>> {
        Box::pin(async move {
            let file = self.files.get_file(FileId(file)).await.map_err(sent)?;
            let mut bytes = Vec::new();
            self.files.download_file(&file.path, &mut bytes).await.map_err(failed)?;
            Ok(bytes)
        })
    }

    fn save(
        &self,
        file: String,
        mut target: tokio::fs::File,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            let file = self.files.get_file(FileId(file)).await.map_err(sent)?;
            self.files.download_file(&file.path, &mut target).await.map_err(failed)?;
            target.flush().await.map_err(failed)
        })
    }
}

pub struct Listener {
    stop: ShutdownToken,
    task: JoinHandle<()>,
}

impl Listener {
    /// Stops long polling and waits for in-flight handlers.
    pub async fn stop(self) {
        match self.stop.shutdown() {
            Ok(done) => done.await,
            // Not polling yet (still dropping a webhook) or already finished: nothing to drain.
            Err(_idle) => self.task.abort(),
        }
        match self.task.await {
            Ok(()) => {}
            Err(error) if error.is_cancelled() => {}
            Err(error) => tracing::error!(%error, "telegram listener crashed"),
        }
    }
}

struct Guard {
    telegram: TelegramSettings,
    hub: mpsc::Sender<HubMessage>,
}

#[must_use]
pub fn listen(bot: Bot, telegram: TelegramSettings, hub: mpsc::Sender<HubMessage>) -> Listener {
    let guard = Arc::new(Guard { telegram, hub });
    let handler = teloxide::dptree::entry()
        .branch(Update::filter_message().endpoint(on_message))
        .branch(Update::filter_callback_query().endpoint(on_press));
    let mut dispatcher = Dispatcher::builder(bot, handler)
        .dependencies(teloxide::dptree::deps![guard])
        .default_handler(|_update| async {})
        .build();
    let stop = dispatcher.shutdown_token();
    let task = tokio::spawn(async move { dispatcher.dispatch().await });
    Listener { stop, task }
}

async fn on_message(message: Message, guard: Arc<Guard>) -> Result<(), RequestError> {
    let inbound = convert(&message);
    if allowed(&guard.telegram, Some(inbound.chat), inbound.user) {
        forward(&guard, HubMessage::Inbound(inbound)).await;
    } else {
        tracing::warn!(
            chat_id = inbound.chat.0,
            user_id = inbound.user.map(|user| user.0),
            "rejected update"
        );
    }
    Ok(())
}

async fn on_press(query: CallbackQuery, guard: Arc<Guard>) -> Result<(), RequestError> {
    let press = pressed(&query);
    if allowed(&guard.telegram, press.chat, Some(press.user)) {
        forward(&guard, HubMessage::Press(press)).await;
    } else {
        tracing::warn!(
            chat_id = press.chat.map(|chat| chat.0),
            user_id = press.user.0,
            "rejected update"
        );
    }
    Ok(())
}

async fn forward(guard: &Guard, message: HubMessage) {
    if guard.hub.send(message).await.is_err() {
        tracing::warn!("update dropped: the hub has stopped");
    }
}

#[cfg(test)]
mod tests {
    use hub_core::domain::{ChatId, ThreadId, UserId};
    use hub_core::settings::Draft;
    use serde_json::json;

    use super::*;

    fn message(extra: &serde_json::Value) -> Message {
        let mut base = json!({
            "message_id": 5,
            "date": 1_700_000_000,
            "chat": {"id": -100, "type": "supergroup", "title": "g", "is_forum": true},
            "from": {"id": 1, "is_bot": false, "first_name": "u"},
        });
        base.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(base).unwrap()
    }

    fn topic(extra: &serde_json::Value) -> Message {
        let mut fields = json!({"message_thread_id": 7, "is_topic_message": true});
        fields.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        message(&fields)
    }

    #[test]
    fn topic_text_message_is_converted() {
        let inbound = convert(&topic(&json!({"text": "/new x"})));
        assert_eq!(inbound.key(), Some(TopicKey { chat: ChatId(-100), thread: ThreadId(7) }));
        assert_eq!(inbound.user, Some(UserId(1)));
        assert_eq!(inbound.content, Content::Text("/new x".to_owned()));
    }

    #[test]
    fn reply_thread_outside_a_topic_is_not_a_topic() {
        let inbound = convert(&message(&json!({"message_thread_id": 9, "text": "hi"})));
        assert_eq!(inbound.key(), None);
    }

    #[test]
    fn largest_photo_becomes_an_album_attachment() {
        let photo = convert(&topic(&json!({
            "caption": "что это?",
            "media_group_id": "g1",
            "photo": [
                {"file_id": "small", "file_unique_id": "s", "width": 90, "height": 90, "file_size": 10},
                {"file_id": "large", "file_unique_id": "l", "width": 900, "height": 900, "file_size": 100}
            ]
        })));
        assert_eq!(photo.album.as_deref(), Some("g1"));
        assert_eq!(
            photo.content,
            Content::Media {
                caption: "что это?".to_owned(),
                attachment: Attachment::Photo(FileRef {
                    id: "large".to_owned(),
                    name: None,
                    size: Some(100)
                }),
            }
        );
    }

    #[test]
    fn document_becomes_a_file_attachment() {
        let document = convert(&topic(&json!({
            "document": {"file_id": "d", "file_unique_id": "du", "file_name": "a.pdf", "file_size": 5}
        })));
        assert_eq!(
            document.content,
            Content::Media {
                caption: String::new(),
                attachment: Attachment::File(FileRef {
                    id: "d".to_owned(),
                    name: Some("a.pdf".to_owned()),
                    size: Some(5)
                }),
            }
        );
    }

    #[test]
    fn voice_is_recognised() {
        let voice = convert(&topic(&json!({
            "voice": {"file_id": "v", "file_unique_id": "vu", "duration": 1, "mime_type": "audio/ogg", "file_size": 3}
        })));
        assert_eq!(voice.content, Content::Voice);
    }

    #[test]
    fn topic_creation_is_recognised() {
        let created = convert(&topic(&json!({
            "forum_topic_created": {"name": "backend", "icon_color": 0}
        })));
        assert_eq!(created.content, Content::TopicCreated { name: "backend".to_owned() });
    }

    #[test]
    fn an_unseen_chat_does_not_stop_the_start() {
        let unseen = chat_access(Err(RequestError::Api(ApiError::ChatNotFound)));
        assert!(matches!(unseen, Ok(ChatAccess::Unseen)));
        assert!(matches!(chat_access(Ok(())), Ok(ChatAccess::Visible)));
        let rejected = chat_access(Err(RequestError::Api(ApiError::InvalidToken)));
        assert!(matches!(rejected, Err(StartError::Network(_))));
    }

    #[test]
    fn only_allowed_users_in_the_chat_pass() {
        let telegram = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1, 2".to_owned(),
            workspace_root: "~/p".to_owned(),
            ..Draft::default()
        }
        .parse(&std::env::temp_dir())
        .unwrap()
        .telegram;
        let chat = Some(ChatId(-100));
        assert!(allowed(&telegram, chat, Some(UserId(2))));
        assert!(!allowed(&telegram, chat, Some(UserId(3))));
        assert!(!allowed(&telegram, Some(ChatId(-1)), Some(UserId(1))));
        assert!(!allowed(&telegram, chat, None));
    }
}
