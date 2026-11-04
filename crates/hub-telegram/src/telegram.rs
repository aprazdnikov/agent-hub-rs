//! The Bot API through teloxide: update conversion, the allowlist guard, the `Messenger`
//! implementation and the long-polling listener.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
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
use crate::inbound::{
    Attachment, Content, FileRef, HANDOFF_EDIT_PROMPT, Inbound, Press, VOICE_EDIT_PROMPT,
};
pub use crate::menu::{MenuError, synchronize_commands};
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
        reply_to: message.reply_to_message().map(|reply| MessageId(reply.id.0)),
        voice_reply: message.reply_to_message().is_some_and(|reply| {
            reply.from.as_ref().is_some_and(|author| author.is_bot)
                && reply.text().is_some_and(|text| text.starts_with(VOICE_EDIT_PROMPT))
        }),
        handoff_reply: message.reply_to_message().is_some_and(|reply| {
            reply.from.as_ref().is_some_and(|author| author.is_bot)
                && reply.text().is_some_and(|text| text.starts_with(HANDOFF_EDIT_PROMPT))
        }),
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
    if let Some(name) = message.forum_topic_edited().and_then(|edited| edited.name.as_ref()) {
        return Content::TopicRenamed { name: name.clone() };
    }
    if let Some(voice) = message.voice() {
        return Content::Voice {
            file: reference(&voice.file, None),
            duration: voice.duration.duration(),
        };
    }
    if message.video_note().is_some() {
        return Content::VideoNote;
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
    let topics = topic_bot(token.expose())?;
    let messenger = Arc::new(TelegramMessenger { bot: bot.clone(), files, topics });
    Ok(Connection { bot, messenger, username })
}

pub struct TelegramMessenger {
    bot: Bot,
    /// The same bot with a longer timeout for file transfers.
    files: Bot,
    /// Topic creation cannot be safely repeated after an uncertain response.
    topics: Bot,
}

fn topic_bot(token: &str) -> Result<Bot, StartError> {
    let client = teloxide::net::default_reqwest_settings()
        .timeout(Duration::from_secs(15))
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| StartError::Network("Не удалось настроить создание тем".to_owned()))?;
    Ok(Bot::with_client(token, client))
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

fn download_too_large() -> SendError {
    SendError::Rejected("Файл превышает допустимый размер".to_owned())
}

async fn collect_limited<B: AsRef<[u8]>, E>(
    chunks: impl Stream<Item = Result<B, E>>,
    limit: usize,
) -> Result<Vec<u8>, SendError> {
    let mut chunks = std::pin::pin!(chunks);
    let mut bytes = Vec::new();
    while let Some(chunk) = chunks.next().await {
        // Download errors can contain the file URL, including the bot token.
        let chunk = chunk.map_err(|_| SendError::Failed("Не удалось скачать файл".to_owned()))?;
        let chunk = chunk.as_ref();
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(download_too_large());
        }
        bytes.extend_from_slice(chunk);
    }
    Ok(bytes)
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
    fn create_topic(
        &self,
        chat: ChatId,
        name: String,
    ) -> BoxFuture<'_, Result<ThreadId, SendError>> {
        Box::pin(async move {
            let topic = self.topics.create_forum_topic(tg::ChatId(chat.0), name).await.map_err(
                |error| match error {
                    RequestError::RetryAfter(seconds) => SendError::RetryAfter(seconds.duration()),
                    RequestError::Api(_) => SendError::Rejected(
                        "Не удалось создать тему; проверьте право бота управлять темами".to_owned(),
                    ),
                    // Transport and parse errors can expose the token URL or response body.
                    _ => SendError::Failed("Не удалось получить ответ о создании темы".to_owned()),
                },
            )?;
            let id = topic.thread_id.0.0;
            if id <= 0 {
                return Err(SendError::Failed("Telegram вернул некорректный ID темы".to_owned()));
            }
            Ok(ThreadId(id))
        })
    }

    fn edit_card(
        &self,
        chat: ChatId,
        message: MessageId,
        card: Outgoing,
    ) -> BoxFuture<'_, Result<(), SendError>> {
        Box::pin(async move {
            if card.force_reply || card.reply_to.is_some() {
                return Err(SendError::Rejected(
                    "Запрос ответа можно добавить только к новому сообщению".to_owned(),
                ));
            }
            let request = self
                .bot
                .edit_message_text(tg::ChatId(chat.0), tg::MessageId(message.0), card.text)
                .reply_markup(markup(card.keyboard));
            let request = match card.format {
                Format::Html => request.parse_mode(ParseMode::Html),
                Format::Plain => request,
            };
            match request.await {
                Ok(_) | Err(RequestError::Api(teloxide::ApiError::MessageNotModified)) => Ok(()),
                Err(error) => Err(sent(error)),
            }
        })
    }
    fn send(
        &self,
        target: Target,
        message: Outgoing,
    ) -> BoxFuture<'_, Result<MessageId, SendError>> {
        Box::pin(async move {
            let Outgoing { text, format, keyboard, force_reply, reply_to } = message;
            if force_reply && !keyboard.is_empty() {
                return Err(SendError::Rejected(
                    "Запрос ответа несовместим с кнопками сообщения".to_owned(),
                ));
            }
            let request = match target {
                Target::Topic(key) => {
                    let request = self
                        .bot
                        .send_message(tg::ChatId(key.chat.0), text)
                        .message_thread_id(thread(key));
                    match reply_to {
                        Some(message) => request.reply_parameters(
                            ReplyParameters::new(tg::MessageId(message.0))
                                .allow_sending_without_reply(),
                        ),
                        None => request,
                    }
                }
                Target::Reply { chat, message } => {
                    self.bot.send_message(tg::ChatId(chat.0), text).reply_parameters(
                        ReplyParameters::new(tg::MessageId(message.0))
                            .allow_sending_without_reply(),
                    )
                }
            };
            let request = match format {
                Format::Html => request.parse_mode(ParseMode::Html),
                Format::Plain => request,
            };
            let request = if force_reply {
                request.reply_markup(tg::ForceReply::new().selective())
            } else if keyboard.is_empty() {
                request
            } else {
                request.reply_markup(markup(keyboard))
            };
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

    fn fetch_limited(
        &self,
        file: String,
        limit: usize,
    ) -> BoxFuture<'_, Result<Vec<u8>, SendError>> {
        Box::pin(async move {
            let file = self.files.get_file(FileId(file)).await.map_err(|error| match error {
                RequestError::RetryAfter(seconds) => SendError::RetryAfter(seconds.duration()),
                _ => SendError::Failed("Не удалось получить файл".to_owned()),
            })?;
            if usize::try_from(file.size).map_or(true, |size| size > limit) {
                return Err(download_too_large());
            }
            collect_limited(self.files.download_file_stream(&file.path), limit).await
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

    #[rstest::rstest]
    #[case::created(
        json!({"ok": true, "result": {"message_thread_id": 41, "name": "Передача → Codex", "icon_color": 7_322_096}}),
        Ok(ThreadId(41))
    )]
    #[case::zero_thread(
        json!({"ok": true, "result": {"message_thread_id": 0, "name": "topic", "icon_color": 7_322_096}}),
        Err(SendError::Failed("Telegram вернул некорректный ID темы".to_owned()))
    )]
    #[case::negative_thread(
        json!({"ok": true, "result": {"message_thread_id": -7, "name": "topic", "icon_color": 7_322_096}}),
        Err(SendError::Failed("Telegram вернул некорректный ID темы".to_owned()))
    )]
    #[case::rejected(
        json!({"ok": false, "error_code": 400, "description": "secret https://api.telegram.org/bot1:secret/createForumTopic"}),
        Err(SendError::Rejected("Не удалось создать тему; проверьте право бота управлять темами".to_owned()))
    )]
    #[case::rate_limited(
        json!({"ok": false, "error_code": 429, "description": "Too Many Requests: retry after 2", "parameters": {"retry_after": 2}}),
        Err(SendError::RetryAfter(Duration::from_secs(2)))
    )]
    #[case::malformed(
        json!({"ok": true, "result": "secret https://api.telegram.org/bot1:secret/createForumTopic"}),
        Err(SendError::Failed("Не удалось получить ответ о создании темы".to_owned()))
    )]
    #[tokio::test]
    async fn topic_creation_sends_one_request_and_returns_a_safe_result(
        #[case] response: serde_json::Value,
        #[case] expected: Result<ThreadId, SendError>,
    ) {
        use http_body_util::{BodyExt, Full};
        use hyper::body::Bytes;
        use hyper_util::rt::TokioIo;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let recorded = Arc::clone(&recorded);
                let response = response.clone();
                let service = hyper::service::service_fn(
                    move |request: hyper::Request<hyper::body::Incoming>| {
                        let recorded = Arc::clone(&recorded);
                        let response = response.clone();
                        async move {
                            let path = request.uri().path().to_ascii_lowercase();
                            let bytes = request.into_body().collect().await.unwrap().to_bytes();
                            let params: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                            recorded.lock().unwrap().push((path, params));
                            Ok::<_, std::convert::Infallible>(hyper::Response::new(Full::new(
                                Bytes::from(response.to_string()),
                            )))
                        }
                    },
                );
                let _ = hyper::server::conn::http1::Builder::new()
                    .keep_alive(false)
                    .serve_connection(TokioIo::new(stream), service)
                    .await;
            }
        });
        let bot =
            topic_bot("1:test").unwrap().set_api_url(format!("http://{address}/").parse().unwrap());
        let messenger = TelegramMessenger { files: bot.clone(), topics: bot.clone(), bot };
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            messenger.create_topic(ChatId(-100), "Передача → Codex".to_owned()),
        )
        .await
        .unwrap();
        server.abort();
        assert_eq!(
            (result, requests.lock().unwrap().clone()),
            (
                expected,
                vec![(
                    "/bot1:test/createforumtopic".to_owned(),
                    json!({
                        "chat_id": -100, "name": "Передача → Codex"
                    })
                )]
            )
        );
    }

    #[tokio::test]
    async fn topic_creation_transport_failure_does_not_expose_the_token_or_url() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let bot = topic_bot("1:secret")
            .unwrap()
            .set_api_url(format!("http://{address}/").parse().unwrap());
        let messenger = TelegramMessenger { files: bot.clone(), topics: bot.clone(), bot };
        assert_eq!(
            messenger.create_topic(ChatId(-100), "topic".to_owned()).await,
            Err(SendError::Failed("Не удалось получить ответ о создании темы".to_owned()))
        );
    }

    #[rstest::rstest]
    #[case(true)]
    #[case(false)]
    #[tokio::test]
    async fn card_edit_sends_keyboard_atomically_and_accepts_unchanged_message(
        #[case] active: bool,
    ) {
        use http_body_util::{BodyExt, Full};
        use hyper::body::Bytes;
        use hyper_util::rt::TokioIo;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let rows = if active {
            vec![vec![Button { text: "Stop".into(), data: "stop".into() }]]
        } else {
            vec![]
        };
        let expected = serde_json::to_value(markup(rows.clone())).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let service = hyper::service::service_fn(
                move |request: hyper::Request<hyper::body::Incoming>| {
                    let expected = expected.clone();
                    async move {
                        assert!(request.uri().path().to_lowercase().ends_with("editmessagetext"));
                        let bytes = request.into_body().collect().await.unwrap().to_bytes();
                        let params: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                        assert_eq!(params["text"], "status");
                        assert_eq!(params["reply_markup"], expected);
                        let body = json!({"ok": false, "error_code": 400, "description": "Bad Request: message is not modified: specified new message content and reply markup are exactly the same as a current content and reply markup of the message"});
                        Ok::<_, std::convert::Infallible>(hyper::Response::new(Full::new(
                            Bytes::from(body.to_string()),
                        )))
                    }
                },
            );
            hyper::server::conn::http1::Builder::new()
                .keep_alive(false)
                .serve_connection(TokioIo::new(stream), service)
                .await
                .unwrap();
        });
        let bot = Bot::new("1:test").set_api_url(format!("http://{address}/").parse().unwrap());
        let messenger = TelegramMessenger { files: bot.clone(), topics: bot.clone(), bot };
        assert_eq!(
            messenger
                .edit_card(
                    ChatId(-100),
                    MessageId(1),
                    Outgoing::plain("status".into()).with_keyboard(rows)
                )
                .await,
            Ok(())
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn result_can_be_sent_after_original_command_message_is_deleted() {
        use http_body_util::{BodyExt, Full};
        use hyper::body::Bytes;
        use hyper_util::rt::TokioIo;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let service = hyper::service::service_fn(
                |request: hyper::Request<hyper::body::Incoming>| async move {
                    let bytes = request.into_body().collect().await.unwrap().to_bytes();
                    let params: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    let allowed = params
                        .get("reply_parameters")
                        .and_then(|p| p.get("allow_sending_without_reply"))
                        .and_then(serde_json::Value::as_bool)
                        == Some(true);
                    let body = if allowed {
                        json!({"ok": true, "result": {"message_id": 2, "date": 0, "chat": {"id": -100, "type": "supergroup"}, "text": "done"}})
                    } else {
                        json!({"ok": false, "error_code": 400, "description": "Bad Request: message to be replied not found"})
                    };
                    Ok::<_, std::convert::Infallible>(hyper::Response::new(Full::new(Bytes::from(
                        body.to_string(),
                    ))))
                },
            );
            hyper::server::conn::http1::Builder::new()
                .keep_alive(false)
                .serve_connection(TokioIo::new(stream), service)
                .await
                .unwrap();
        });
        let bot = Bot::new("1:test").set_api_url(format!("http://{address}/").parse().unwrap());
        let messenger = TelegramMessenger { files: bot.clone(), topics: bot.clone(), bot };
        let result = messenger
            .send(
                Target::Reply { chat: ChatId(-100), message: MessageId(1) },
                Outgoing::plain("done".to_owned()),
            )
            .await;
        assert_eq!(result, Ok(MessageId(2)));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn correction_prompt_sends_selective_force_reply_in_its_topic() {
        use http_body_util::{BodyExt, Full};
        use hyper::body::Bytes;
        use hyper_util::rt::TokioIo;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let service = hyper::service::service_fn(
                |request: hyper::Request<hyper::body::Incoming>| async move {
                    let bytes = request.into_body().collect().await.unwrap().to_bytes();
                    let params: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    assert_eq!(
                        params,
                        json!({
                            "chat_id": -100,
                            "message_thread_id": 7,
                            "text": VOICE_EDIT_PROMPT,
                            "reply_parameters": {"message_id": 8, "allow_sending_without_reply": true},
                            "reply_markup": {"force_reply": true, "selective": true}
                        })
                    );
                    let body = json!({"ok": true, "result": {"message_id": 9, "date": 0, "chat": {"id": -100, "type": "supergroup"}, "text": VOICE_EDIT_PROMPT}});
                    Ok::<_, std::convert::Infallible>(hyper::Response::new(Full::new(Bytes::from(
                        body.to_string(),
                    ))))
                },
            );
            hyper::server::conn::http1::Builder::new()
                .keep_alive(false)
                .serve_connection(TokioIo::new(stream), service)
                .await
                .unwrap();
        });
        let bot = Bot::new("1:test").set_api_url(format!("http://{address}/").parse().unwrap());
        let messenger = TelegramMessenger { files: bot.clone(), topics: bot.clone(), bot };
        assert!(
            messenger
                .send(
                    Target::Topic(TopicKey { chat: ChatId(-100), thread: ThreadId(7) }),
                    Outgoing::plain(VOICE_EDIT_PROMPT.to_owned())
                        .with_force_reply()
                        .replying_to(MessageId(8)),
                )
                .await
                .is_ok()
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn force_reply_cannot_be_combined_with_an_inline_keyboard() {
        // An unreachable local endpoint ensures rejected requests have no network effects.
        let bot = Bot::new("1:test").set_api_url("http://127.0.0.1:1/".parse().unwrap());
        let messenger = TelegramMessenger { files: bot.clone(), topics: bot.clone(), bot };
        let outgoing = Outgoing::plain("reply".to_owned())
            .with_force_reply()
            .with_keyboard(vec![vec![Button { text: "Confirm".into(), data: "confirm".into() }]]);
        assert!(matches!(
            messenger
                .send(Target::Reply { chat: ChatId(-100), message: MessageId(1) }, outgoing)
                .await,
            Err(SendError::Rejected(_))
        ));
    }

    #[tokio::test]
    async fn force_reply_cannot_be_added_to_an_existing_card() {
        let bot = Bot::new("1:test").set_api_url("http://127.0.0.1:1/".parse().unwrap());
        let messenger = TelegramMessenger { files: bot.clone(), topics: bot.clone(), bot };
        assert!(matches!(
            messenger
                .edit_card(
                    ChatId(-100),
                    MessageId(1),
                    Outgoing::plain("reply".to_owned()).with_force_reply()
                )
                .await,
            Err(SendError::Rejected(_))
        ));
    }

    #[rstest::rstest]
    #[case::empty(vec![], 0, Ok(vec![]))]
    #[case::exact(vec![vec![1, 2], vec![3]], 3, Ok(vec![1, 2, 3]))]
    #[case::overflow(vec![vec![1, 2], vec![3, 4]], 3, Err(download_too_large()))]
    #[case::zero(vec![vec![1]], 0, Err(download_too_large()))]
    #[tokio::test]
    async fn download_stream_enforces_actual_byte_limit(
        #[case] chunks: Vec<Vec<u8>>,
        #[case] limit: usize,
        #[case] expected: Result<Vec<u8>, SendError>,
    ) {
        let stream = futures::stream::iter(chunks.into_iter().map(Ok::<_, &str>));
        assert_eq!(collect_limited(stream, limit).await, expected);
    }

    #[tokio::test]
    async fn download_stops_polling_as_soon_as_limit_is_exceeded() {
        let polled = std::sync::atomic::AtomicUsize::new(0);
        let stream =
            futures::stream::iter([Ok::<_, &str>(vec![1, 2]), Ok(vec![3])]).inspect(|_| {
                polled.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            });
        assert!(collect_limited(stream, 1).await.is_err());
        assert_eq!(polled.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn download_errors_do_not_expose_file_urls_or_bot_tokens() {
        let stream = futures::stream::iter([Err::<Vec<u8>, _>(
            "request failed at https://api.telegram.org/file/bot1:secret/voice.ogg",
        )]);
        assert_eq!(
            collect_limited(stream, 10).await,
            Err(SendError::Failed("Не удалось скачать файл".to_owned()))
        );
    }

    #[rstest::rstest]
    #[case::exact(3, Some(vec![1, 2, 3]), Ok(vec![1, 2, 3]))]
    #[case::lying_metadata(1, Some(vec![1, 2, 3, 4]), Err(download_too_large()))]
    #[case::oversized_metadata(4, None, Err(download_too_large()))]
    #[tokio::test]
    async fn limited_fetch_checks_metadata_and_downloaded_bytes(
        #[case] size: u32,
        #[case] download: Option<Vec<u8>>,
        #[case] expected: Result<Vec<u8>, SendError>,
    ) {
        use http_body_util::Full;
        use hyper::body::Bytes;
        use hyper_util::rt::TokioIo;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let metadata = json!({"ok": true, "result": {"file_id": "voice", "file_unique_id": "v", "file_size": size, "file_path": "voice.ogg"}});
            let responses =
                std::iter::once(("/bot1:test/GetFile", metadata.to_string().into_bytes()))
                    .chain(download.map(|bytes| ("/file/bot1:test/voice.ogg", bytes)));
            for (path, body) in responses {
                let (stream, _) = listener.accept().await.unwrap();
                let service = hyper::service::service_fn(
                    move |request: hyper::Request<hyper::body::Incoming>| {
                        assert!(request.uri().path().eq_ignore_ascii_case(path));
                        let body = body.clone();
                        async move {
                            Ok::<_, std::convert::Infallible>(hyper::Response::new(Full::new(
                                Bytes::from(body),
                            )))
                        }
                    },
                );
                hyper::server::conn::http1::Builder::new()
                    .keep_alive(false)
                    .serve_connection(TokioIo::new(stream), service)
                    .await
                    .unwrap();
            }
        });
        let bot = Bot::new("1:test").set_api_url(format!("http://{address}/").parse().unwrap());
        let messenger = TelegramMessenger { files: bot.clone(), topics: bot.clone(), bot };
        assert_eq!(messenger.fetch_limited("voice".to_owned(), 3).await, expected);
        server.await.unwrap();
    }

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
        assert_eq!(
            voice.content,
            Content::Voice {
                file: FileRef { id: "v".to_owned(), name: None, size: Some(3) },
                duration: Duration::from_secs(1),
            }
        );
    }

    #[test]
    fn video_note_is_distinct_from_voice() {
        let video_note = convert(&topic(&json!({
            "video_note": {"file_id": "v", "file_unique_id": "vu", "duration": 2, "length": 320, "file_size": 3}
        })));
        assert_eq!(video_note.content, Content::VideoNote);
    }

    #[test]
    fn reply_message_id_is_preserved() {
        let reply = convert(&topic(&json!({
            "text": "corrected",
            "reply_to_message": {"message_id": 4, "date": 0, "chat": {"id": -100, "type": "supergroup"}, "text": "prompt"}
        })));
        assert_eq!(reply.reply_to, Some(MessageId(4)));
    }

    #[rstest::rstest]
    #[case::bot_prompt(true, VOICE_EDIT_PROMPT, true)]
    #[case::human_forgery(false, VOICE_EDIT_PROMPT, false)]
    #[case::ordinary_bot_message(true, "ordinary message", false)]
    fn voice_correction_reply_is_recognised_only_for_a_bot_prompt(
        #[case] is_bot: bool,
        #[case] text: &str,
        #[case] expected: bool,
    ) {
        let reply = convert(&topic(&json!({
            "text": "corrected",
            "reply_to_message": {"message_id": 4, "date": 0, "chat": {"id": -100, "type": "supergroup"}, "from": {"id": 2, "is_bot": is_bot, "first_name": "bot"}, "text": text}
        })));
        assert_eq!(reply.voice_reply, expected);
    }

    #[rstest::rstest]
    #[case::bot_prompt(
        true,
        "📋 Исправление сводки\nОтветьте на это сообщение полным исправленным текстом. Затем подтвердите передачу.",
        true
    )]
    #[case::human_forgery(
        false,
        "📋 Исправление сводки\nОтветьте на это сообщение полным исправленным текстом. Затем подтвердите передачу.",
        false
    )]
    #[case::ordinary_bot_message(true, "ordinary message", false)]
    #[case::voice_prompt(true, VOICE_EDIT_PROMPT, false)]
    #[case::bot_prompt_with_context(
        true,
        "📋 Исправление сводки\nОтветьте на это сообщение полным исправленным текстом. Затем подтвердите передачу.\nЧерновик #1",
        true
    )]
    fn handoff_correction_reply_is_recognised_only_for_its_bot_prompt(
        #[case] is_bot: bool,
        #[case] text: &str,
        #[case] expected: bool,
    ) {
        let reply = convert(&topic(&json!({
            "text": "corrected",
            "reply_to_message": {"message_id": 4, "date": 0, "chat": {"id": -100, "type": "supergroup"}, "from": {"id": 2, "is_bot": is_bot, "first_name": "bot"}, "text": text}
        })));
        assert_eq!(reply.handoff_reply, expected);
    }

    #[test]
    fn topic_creation_is_recognised() {
        let created = convert(&topic(&json!({
            "forum_topic_created": {"name": "backend", "icon_color": 0}
        })));
        assert_eq!(created.content, Content::TopicCreated { name: "backend".to_owned() });
    }

    #[test]
    fn topic_rename_is_recognised() {
        let renamed = convert(&topic(&json!({
            "forum_topic_edited": {"name": "Новое имя"}
        })));
        assert_eq!(renamed.content, Content::TopicRenamed { name: "Новое имя".to_owned() });
        let icon_only = convert(&topic(&json!({
            "forum_topic_edited": {"icon_custom_emoji_id": "123"}
        })));
        assert_eq!(icon_only.content, Content::Ignored);
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
