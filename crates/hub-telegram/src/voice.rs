//! Short-lived, author-bound voice drafts. Only confirmation returns a task to the hub.

use std::{collections::HashMap, panic::AssertUnwindSafe, sync::Arc, time::Duration};

use futures::FutureExt;
use hub_core::{
    domain::{MessageId, TopicKey, TopicSession, UserId},
    questions::Button,
    settings::Settings,
    speech::SpeechProvider,
};
use tokio::{sync::mpsc, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::{
    hub::HubMessage,
    inbound::{Content, Inbound, Press, VOICE_EDIT_PROMPT},
    messenger::{Outgoing, Target},
    sender::Sender,
    telegram::allowed,
    transcription::{
        MAX_AUDIO_BYTES, MAX_DURATION, MAX_TEXT_UNITS, SpeechError, TIMEOUT, Transcriber,
    },
};

pub const PREFIX: &str = "voice:";
const TTL: Duration = Duration::from_mins(10);
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_DRAFTS: usize = 32;
const MAX_RUNNING: usize = 2;
const STALE: &str = "Черновик устарел. Отправьте голосовое заново или напишите задачу текстом.";
const FALLBACK: &str =
    "Не удалось распознать голосовое. Попробуйте ещё раз или напишите задачу текстом.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Recognizing,
    Publishing,
    Ready,
    Prompting,
    Editing,
}

struct Draft {
    key: TopicKey,
    author: UserId,
    source: MessageId,
    settings: Arc<Settings>,
    session: Option<TopicSession>,
    created: Instant,
    phase: Phase,
    revision: u32,
    text: String,
    preview: Option<MessageId>,
    prompt: Option<MessageId>,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl Draft {
    fn current(&self, settings: &Settings, session: Option<&TopicSession>) -> bool {
        self.created.elapsed() < TTL
            && self.settings.as_ref() == settings
            && self.session.as_ref() == session
    }

    fn card(&self, id: &str) -> Outgoing {
        let button = |label: &str, operation: &str| Button {
            text: label.to_owned(),
            data: format!("{PREFIX}{id}:{}:{operation}", self.revision),
        };
        Outgoing::plain(format!(
            "🎤 Проверьте распознанную задачу:\n\n{}\n\nОтправка — только после подтверждения автора. Черновик действует 10 минут.",
            self.text
        ))
        .with_keyboard(vec![vec![
            button("Отправить", "send"),
            button("Исправить", "edit"),
            button("Отмена", "cancel"),
        ]])
    }
}

impl Drop for Draft {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

pub struct Done {
    id: String,
    revision: u32,
    outcome: Outcome,
}

enum Outcome {
    Recognized(Result<String, SpeechError>),
    Published(Option<MessageId>),
    Prompted(Option<MessageId>),
}

pub struct Voices {
    drafts: HashMap<String, Draft>,
    transcriber: Arc<dyn Transcriber>,
    sender: Sender,
    mailbox: mpsc::Sender<HubMessage>,
}

impl Voices {
    pub fn new(
        transcriber: Arc<dyn Transcriber>,
        sender: Sender,
        mailbox: mpsc::Sender<HubMessage>,
    ) -> Self {
        Self { drafts: HashMap::new(), transcriber, sender, mailbox }
    }

    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty()
    }

    pub fn refresh(&mut self, settings: &Settings, topics: &hub_core::topics::Topics) {
        self.drafts.retain(|_, draft| draft.current(settings, topics.get(&draft.key)));
    }

    pub fn invalidate(&mut self, key: TopicKey) {
        self.drafts.retain(|_, draft| draft.key != key);
    }

    pub fn shutdown(&mut self) {
        self.drafts.clear();
    }

    pub fn start(
        &mut self,
        inbound: &Inbound,
        settings: Arc<Settings>,
        session: Option<TopicSession>,
    ) {
        let (Some(key), Some(author), Content::Voice { file, duration }) =
            (inbound.key(), inbound.user, &inbound.content)
        else {
            return;
        };
        if !allowed(&settings.telegram, Some(key.chat), Some(author)) {
            return;
        }
        let refusal = if settings.speech.provider == SpeechProvider::Disabled {
            Some(SpeechError::Disabled.to_string())
        } else if *duration > MAX_DURATION
            || file.size.is_some_and(|size| size > MAX_AUDIO_BYTES as u64)
        {
            Some("Голосовое должно быть не длиннее 3 минут и не больше 10 МиБ. Отправьте короткое сообщение или текст.".to_owned())
        } else if self.drafts.values().any(|draft| draft.key == key && draft.author == author) {
            Some("В этой теме уже есть ваш голосовой черновик. Подтвердите или отмените его; время ожидания — до 10 минут.".to_owned())
        } else if self.drafts.values().filter(|draft| draft.phase == Phase::Recognizing).count()
            >= MAX_RUNNING
        {
            Some(
                "Уже распознаются два сообщения. Попробуйте чуть позже или отправьте текст."
                    .to_owned(),
            )
        } else if self.drafts.len() >= MAX_DRAFTS {
            Some(
                "Слишком много голосовых черновиков. Завершите один из них или отправьте текст."
                    .to_owned(),
            )
        } else {
            None
        };
        if let Some(text) = refusal {
            self.notice(key, text);
            return;
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let cancel = CancellationToken::new();
        let (sender, transcriber, mailbox) =
            (self.sender.clone(), self.transcriber.clone(), self.mailbox.clone());
        let (job_id, file, speech, stop) =
            (id.clone(), file.id.clone(), settings.speech.clone(), cancel.clone());
        let task = tokio::spawn(async move {
            let work = async {
                let bytes = sender.messenger().fetch_limited(file, MAX_AUDIO_BYTES).await
                    .map_err(|_| SpeechError::Failed("Не удалось загрузить голосовое в пределах 10 МиБ. Отправьте короткое сообщение или текст.".to_owned()))?;
                if bytes.is_empty() || bytes.len() > MAX_AUDIO_BYTES {
                    return Err(SpeechError::Failed(FALLBACK.to_owned()));
                }
                transcriber.transcribe(speech, bytes, stop.clone()).await
            };
            let result =
                match tokio::time::timeout(TIMEOUT, AssertUnwindSafe(work).catch_unwind()).await {
                    Ok(Ok(result)) => result,
                    Ok(Err(_)) => Err(SpeechError::Failed(FALLBACK.to_owned())),
                    Err(_) => {
                        stop.cancel();
                        Err(SpeechError::Timeout)
                    }
                };
            let _ = mailbox
                .send(HubMessage::VoiceDone(Done {
                    id: job_id,
                    revision: 0,
                    outcome: Outcome::Recognized(result),
                }))
                .await;
        });
        self.drafts.insert(
            id,
            Draft {
                key,
                author,
                source: inbound.message,
                settings,
                session,
                created: Instant::now(),
                phase: Phase::Recognizing,
                revision: 0,
                text: String::new(),
                preview: None,
                prompt: None,
                cancel,
                task: Some(task),
            },
        );
        self.notice(
            key,
            "🎤 Распознаю голосовое… до 3 минут. Затем покажу текст для подтверждения.".to_owned(),
        );
    }

    pub fn complete(&mut self, done: Done, settings: &Settings, topics: &hub_core::topics::Topics) {
        self.refresh(settings, topics);
        let Some(draft) = self.drafts.get_mut(&done.id) else { return };
        if draft.revision != done.revision {
            return;
        }
        draft.task = None;
        match done.outcome {
            Outcome::Recognized(result) => match result.and_then(|text| valid_text(&text)) {
                Ok(text) => {
                    draft.text = text;
                    self.publish(&done.id);
                }
                Err(error) => {
                    let key = draft.key;
                    self.drafts.remove(&done.id);
                    self.notice(
                        key,
                        format!("⚠️ {error}\nМожно отправить задачу обычным текстом."),
                    );
                }
            },
            Outcome::Published(Some(message)) => {
                draft.preview = Some(message);
                draft.phase = Phase::Ready;
            }
            Outcome::Prompted(Some(message)) => {
                draft.prompt = Some(message);
                draft.phase = Phase::Editing;
            }
            Outcome::Published(None) | Outcome::Prompted(None) => {
                let key = draft.key;
                self.drafts.remove(&done.id);
                self.notice(key, "Не удалось показать голосовой черновик. Отправьте голосовое заново или напишите текст.".to_owned());
            }
        }
    }

    fn publish(&mut self, id: &str) {
        let Some(draft) = self.drafts.get_mut(id) else { return };
        draft.phase = Phase::Publishing;
        let (id, revision, key, previous, card) =
            (id.to_owned(), draft.revision, draft.key, draft.preview, draft.card(id));
        let (sender, mailbox) = (self.sender.clone(), self.mailbox.clone());
        draft.task = Some(tokio::spawn(async move {
            let send = async {
                if let Some(message) = previous {
                    sender
                        .messenger()
                        .edit_card(key.chat, message, card)
                        .await
                        .ok()
                        .map(|()| message)
                } else {
                    sender.messenger().send(Target::Topic(key), card).await.ok()
                }
            };
            let message = tokio::time::timeout(DELIVERY_TIMEOUT, send).await.ok().flatten();
            let _ = mailbox
                .send(HubMessage::VoiceDone(Done {
                    id,
                    revision,
                    outcome: Outcome::Published(message),
                }))
                .await;
        }));
    }

    /// A reply to any voice edit prompt is consumed, including expired and restarted drafts.
    pub fn correction(&mut self, inbound: &Inbound) -> bool {
        if !inbound.voice_reply {
            return false;
        }
        let Some(key) = inbound.key() else { return true };
        let id = self
            .drafts
            .iter()
            .find(|(_, draft)| {
                draft.key == key
                    && Some(draft.author) == inbound.user
                    && draft.phase == Phase::Editing
                    && draft.prompt == inbound.reply_to
            })
            .map(|(id, _)| id.clone());
        let Some(id) = id else {
            self.notice(key, STALE.to_owned());
            return true;
        };
        let result = match &inbound.content {
            Content::Text(text) => valid_text(text),
            _ => {
                Err(SpeechError::Failed("Ответьте текстом на сообщение об исправлении.".to_owned()))
            }
        };
        match result {
            Ok(text) => {
                if let Some(draft) = self.drafts.get_mut(&id) {
                    draft.text = text;
                    draft.prompt = None;
                    draft.revision += 1;
                }
                self.publish(&id);
            }
            Err(error) => self.notice(key, error.to_string()),
        }
        true
    }

    pub fn press(
        &mut self,
        press: Press,
        settings: &Settings,
        topics: &hub_core::topics::Topics,
    ) -> Option<(TopicKey, String)> {
        self.refresh(settings, topics);
        let mut parts = press.data.strip_prefix(PREFIX)?.split(':');
        let (Some(id), Some(revision), Some(operation), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            self.answer(press.callback, STALE);
            return None;
        };
        let valid = self.drafts.get(id).is_some_and(|draft| {
            allowed(&settings.telegram, press.chat, Some(press.user))
                && press.user == draft.author
                && press.chat == Some(draft.key.chat)
                && press.message.is_some()
                && press.message == draft.preview
                && revision.parse::<u32>().ok() == Some(draft.revision)
        });
        if !valid {
            self.answer(press.callback, STALE);
            return None;
        }
        if operation == "cancel" {
            if let Some(draft) = self.drafts.remove(id) {
                self.finish(&draft, "Голосовой черновик отменён.");
            }
            self.answer(press.callback, "Отменено");
        } else if operation == "send"
            && self.drafts.get(id).is_some_and(|draft| draft.phase == Phase::Ready)
        {
            let draft = self.drafts.remove(id)?;
            self.finish(&draft, "✅ Голосовая задача подтверждена.");
            self.answer(press.callback, "Подтверждено");
            return Some((draft.key, draft.text.clone()));
        } else if operation == "edit"
            && self.drafts.get(id).is_some_and(|draft| draft.phase == Phase::Ready)
        {
            self.prompt(id);
            self.answer(press.callback, "Ответьте на сообщение об исправлении");
        } else {
            self.answer(press.callback, "Дождитесь нового предпросмотра.");
        }
        None
    }

    fn prompt(&mut self, id: &str) {
        let Some(draft) = self.drafts.get_mut(id) else { return };
        draft.phase = Phase::Prompting;
        let (id, revision, key, source) = (id.to_owned(), draft.revision, draft.key, draft.source);
        let (sender, mailbox) = (self.sender.clone(), self.mailbox.clone());
        draft.task = Some(tokio::spawn(async move {
            let prompt = Outgoing::plain(VOICE_EDIT_PROMPT.to_owned())
                .with_force_reply()
                .replying_to(source);
            let message = tokio::time::timeout(
                DELIVERY_TIMEOUT,
                sender.messenger().send(Target::Topic(key), prompt),
            )
            .await
            .ok()
            .and_then(Result::ok);
            let _ = mailbox
                .send(HubMessage::VoiceDone(Done {
                    id,
                    revision,
                    outcome: Outcome::Prompted(message),
                }))
                .await;
        }));
    }

    fn finish(&self, draft: &Draft, text: &str) {
        if let Some(message) = draft.preview {
            let (sender, key, text) = (self.sender.clone(), draft.key, text.to_owned());
            tokio::spawn(async move {
                let _ = tokio::time::timeout(
                    DELIVERY_TIMEOUT,
                    sender.messenger().edit_card(key.chat, message, Outgoing::plain(text)),
                )
                .await;
            });
        }
    }

    fn answer(&self, callback: String, text: &str) {
        let (sender, text) = (self.sender.clone(), text.to_owned());
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                DELIVERY_TIMEOUT,
                sender.messenger().answer(callback, Some(text)),
            )
            .await;
        });
    }

    fn notice(&self, key: TopicKey, text: String) {
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                DELIVERY_TIMEOUT,
                sender.messenger().send(Target::Topic(key), Outgoing::plain(text)),
            )
            .await;
        });
    }
}

fn valid_text(text: &str) -> Result<String, SpeechError> {
    let text = text.trim();
    if text.is_empty() {
        Err(SpeechError::Failed(
            "В записи не удалось распознать речь. Отправьте голосовое ещё раз или напишите текст."
                .to_owned(),
        ))
    } else if text.encode_utf16().count() > MAX_TEXT_UNITS {
        Err(SpeechError::Failed("Распознанный текст длиннее 3000 символов. Разделите задачу на несколько сообщений или напишите текст.".to_owned()))
    } else {
        Ok(text.to_owned())
    }
}
