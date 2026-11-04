//! Author-confirmed handoffs. Source leases outlive cancelled drafts until cleanup finishes.

use std::{collections::HashMap, panic::AssertUnwindSafe, sync::Arc, time::Duration};

use futures::FutureExt;
use hub_core::{
    domain::{BackendKind, MessageId, ThreadId, TopicKey, TopicSession, UserId},
    handoff::Summary,
    progress::TaskProgress,
    questions::Button,
    render::format_finished,
    settings::Settings,
    topics::Topics,
};
use tokio::{sync::mpsc, task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;

use crate::{
    handoff_summary::{Prepared, SummaryRun},
    hub::{Agents, HubMessage},
    inbound::{Content, HANDOFF_EDIT_PROMPT, Inbound, Press},
    messenger::{Outgoing, Target},
    progress::Progress,
    sender::Sender,
    telegram::allowed,
};

pub const PREFIX: &str = "handoff:";
const TTL: Duration = Duration::from_mins(15);
const IO_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_DRAFTS: usize = 32;
const MAX_JOBS: usize = 2;
const STALE: &str = "Сводка устарела. Откройте /handoff заново.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Choosing,
    Offered,
    Generating,
    Publishing,
    Ready,
    Prompting,
    Editing,
    Creating,
}

struct Draft {
    key: TopicKey,
    user: UserId,
    source_message: MessageId,
    session: TopicSession,
    settings: Arc<Settings>,
    recipient: Option<BackendKind>,
    summary: Option<Summary>,
    footer: String,
    created: Instant,
    revision: u32,
    phase: Phase,
    message: Option<MessageId>,
    prompt: Option<MessageId>,
    task: Option<JoinHandle<()>>,
    cancelled: bool,
    progress: Progress,
}

impl Draft {
    fn current(&self, settings: &Settings, topics: &Topics) -> bool {
        self.created.elapsed() < TTL
            && self.settings.as_ref() == settings
            && topics.get(&self.key) == Some(&self.session)
    }

    fn button(&self, id: &str, label: &str, operation: &str) -> Button {
        Button {
            text: label.to_owned(),
            data: format!("{PREFIX}{id}:{}:{operation}", self.revision),
        }
    }

    fn card(&self, id: &str, phase: Phase) -> Outgoing {
        let cancel = self.button(id, "Отмена", "cancel");
        match phase {
            Phase::Choosing => {
                let mut rows: Vec<_> = BackendKind::ALL.into_iter().filter(|backend| *backend != self.session.backend)
                    .map(|backend| vec![self.button(id, backend.name(), backend.name())]).collect();
                rows.push(vec![cancel]);
                Outgoing::plain("Кому передать задачу? После проверки сводки получатель начнёт новую сессию в новой теме, в том же каталоге. Исходная тема сохранится.".to_owned()).with_keyboard(rows)
            }
            Phase::Offered => Outgoing::plain(format!(
                "Передача: {} → {}.\n\nТекущий агент подготовит сводку отдельным ходом. Это расходует его лимит или оплачиваемые токены. Сначала вы проверите и при необходимости исправите текст; новая тема появится только после подтверждения.\nЧерновик действует 15 минут.",
                self.session.backend.name(), self.recipient.map_or("?", BackendKind::name)
            )).with_keyboard(vec![vec![self.button(id, "Сформировать сводку", "generate"), cancel]]),
            Phase::Generating => Outgoing::plain("📋 Формирую сводку из истории текущего агента… до 3 минут. Отмена и /stop прекращают подготовку; исходная тема сохраняется.".to_owned())
                .with_keyboard(vec![vec![cancel]]),
            Phase::Ready => Outgoing::plain(format!(
                "📋 Проверьте сводку для {}:\n\n{}\n\n{}\nПосле подтверждения получатель продолжит задачу в новой теме. Исходная тема сохранится. Черновик действует 15 минут с открытия /handoff.",
                self.recipient.map_or("?", BackendKind::name), self.summary.as_ref().map_or("", Summary::text), self.footer
            )).with_keyboard(vec![vec![self.button(id, "Передать и продолжить", "send"), self.button(id, "Исправить", "edit"), cancel]]),
            Phase::Publishing | Phase::Prompting | Phase::Editing | Phase::Creating => Outgoing::plain("Дождитесь завершения операции.".to_owned()),
        }
    }
}

impl Drop for Draft {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

struct Job {
    key: TopicKey,
    session: TopicSession,
    cancel: CancellationToken,
    progress: Progress,
    _task: JoinHandle<()>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub struct Done {
    id: String,
    revision: u32,
    outcome: Outcome,
}
enum Outcome {
    Published { message: Option<MessageId>, phase: Phase },
    Generated(Result<Prepared, String>),
    Prompted(Option<MessageId>),
    Created(Result<ThreadId, String>),
}

pub struct Transfer {
    pub source: TopicKey,
    pub destination: TopicKey,
    pub original: TopicSession,
    pub recipient: BackendKind,
    pub summary: Summary,
}

pub struct Handoffs<A> {
    drafts: HashMap<String, Draft>,
    jobs: HashMap<String, Job>,
    agents: Arc<A>,
    sender: Sender,
    mailbox: mpsc::Sender<HubMessage>,
}

impl<A: Agents> Handoffs<A> {
    pub fn new(agents: Arc<A>, sender: Sender, mailbox: mpsc::Sender<HubMessage>) -> Self {
        Self { drafts: HashMap::new(), jobs: HashMap::new(), agents, sender, mailbox }
    }
    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty() && self.jobs.is_empty()
    }
    pub fn is_idle(&self) -> bool {
        self.jobs.is_empty()
    }
    pub fn creating(&self) -> bool {
        self.drafts.values().any(|draft| draft.phase == Phase::Creating)
    }
    pub fn busy(&self, key: TopicKey) -> bool {
        self.jobs.values().any(|job| job.key == key)
            || self.drafts.values().any(|draft| draft.key == key && draft.phase == Phase::Creating)
    }
    pub fn session_busy(&self, session: &TopicSession) -> bool {
        self.jobs.values().any(|job| same_session(&job.session, session))
            || self.drafts.values().any(|draft| {
                draft.phase == Phase::Creating && same_session(&draft.session, session)
            })
    }
    pub fn progress(&self, key: TopicKey) -> Option<TaskProgress> {
        self.jobs.values().find(|job| job.key == key).map(|job| job.progress.snapshot()).or_else(
            || {
                self.drafts
                    .values()
                    .find(|draft| draft.key == key && draft.phase == Phase::Creating)
                    .map(|draft| draft.progress.snapshot())
            },
        )
    }
    pub fn stop(&mut self, key: TopicKey) -> bool {
        let mut stopped = false;
        for job in self.jobs.values().filter(|job| job.key == key) {
            job.progress.stopping();
            job.cancel.cancel();
            stopped = true;
        }
        for draft in self
            .drafts
            .values_mut()
            .filter(|draft| draft.key == key && draft.phase == Phase::Creating)
        {
            draft.cancelled = true;
            stopped = true;
        }
        stopped
    }
    pub fn shutdown(&mut self) {
        self.drafts.clear();
        for job in self.jobs.values() {
            job.progress.stopping();
            job.cancel.cancel();
        }
    }
    pub fn refresh(&mut self, settings: &Settings, topics: &Topics) {
        let stale: Vec<_> = self
            .drafts
            .iter()
            .filter(|(_, draft)| !draft.current(settings, topics))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.discard(&id);
        }
    }
    pub fn invalidate(&mut self, key: TopicKey) {
        let ids: Vec<_> = self
            .drafts
            .iter()
            .filter(|(_, draft)| draft.key == key)
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.discard(&id);
        }
        for job in self.jobs.values().filter(|job| job.key == key) {
            job.cancel.cancel();
        }
    }
    pub fn invalidate_session(&mut self, session: &TopicSession) {
        let ids: Vec<_> = self
            .drafts
            .iter()
            .filter(|(_, draft)| same_session(&draft.session, session))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.discard(&id);
        }
        for job in self.jobs.values().filter(|job| same_session(&job.session, session)) {
            job.cancel.cancel();
        }
    }
    fn discard(&mut self, id: &str) {
        self.drafts.remove(id);
        if let Some(job) = self.jobs.get(id) {
            job.progress.stopping();
            job.cancel.cancel();
        }
    }
    pub fn source(&self, press: &Press) -> Option<TopicKey> {
        let id = press.data.strip_prefix(PREFIX)?.split(':').next()?;
        self.drafts.get(id).map(|draft| draft.key)
    }

    pub fn open(
        &mut self,
        inbound: &Inbound,
        session: TopicSession,
        settings: Arc<Settings>,
        recipient: Option<BackendKind>,
    ) {
        let (Some(key), Some(user)) = (inbound.key(), inbound.user) else { return };
        if !allowed(&settings.telegram, Some(key.chat), Some(user)) {
            return;
        }
        if session.session.is_none() {
            self.notice(key, "Сначала обсудите задачу в этой теме: у текущего агента ещё нет истории для сводки.".to_owned());
            return;
        }
        if recipient == Some(session.backend) {
            self.notice(key, "Выберите другого агента через /handoff.".to_owned());
            return;
        }
        self.invalidate(key);
        if self.drafts.len() >= MAX_DRAFTS {
            self.notice(
                key,
                "Слишком много сводок. Завершите одну из передач и повторите /handoff.".to_owned(),
            );
            return;
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let phase = if recipient.is_some() { Phase::Offered } else { Phase::Choosing };
        self.drafts.insert(
            id.clone(),
            Draft {
                key,
                user,
                source_message: inbound.message,
                session,
                settings,
                recipient,
                summary: None,
                footer: String::new(),
                created: Instant::now(),
                revision: 0,
                phase,
                message: None,
                prompt: None,
                task: None,
                cancelled: false,
                progress: Progress::new(),
            },
        );
        self.publish(&id, phase);
    }

    pub fn press(
        &mut self,
        press: Press,
        settings: &Settings,
        topics: &Topics,
        refusal: Option<&str>,
    ) {
        self.refresh(settings, topics);
        let Some(raw) = press.data.strip_prefix(PREFIX) else { return };
        let mut parts = raw.split(':');
        let (Some(id), Some(revision), Some(operation), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            self.answer(press.callback, STALE);
            return;
        };
        let valid = self.drafts.get(id).is_some_and(|draft| {
            allowed(&settings.telegram, press.chat, Some(press.user))
                && press.user == draft.user
                && press.chat == Some(draft.key.chat)
                && press.message.is_some()
                && press.message == draft.message
                && revision.parse::<u32>().ok() == Some(draft.revision)
        });
        if !valid {
            self.answer(press.callback, STALE);
            return;
        }
        let Some(draft) = self.drafts.get(id) else { return };
        let phase = draft.phase;
        if operation == "cancel" && phase != Phase::Creating {
            self.finish(id, "Передача отменена. Исходная тема сохранена.");
            self.discard(id);
            self.answer(press.callback, "Отменено");
            return;
        }
        if matches!(operation, "generate" | "send")
            && let Some(refusal) = refusal
        {
            self.answer(press.callback, refusal);
            return;
        }
        if phase == Phase::Choosing
            && let Some(recipient) = BackendKind::parse(operation)
        {
            if recipient == draft.session.backend {
                self.answer(press.callback, "Выберите другого агента");
                return;
            }
            if let Some(draft) = self.drafts.get_mut(id) {
                draft.recipient = Some(recipient);
                draft.revision += 1;
            }
            self.publish(id, Phase::Offered);
            self.answer(press.callback, "Получатель выбран");
        } else if phase == Phase::Offered && operation == "generate" {
            if self.jobs.len() >= MAX_JOBS {
                self.answer(press.callback, "Уже готовятся две сводки. Попробуйте позже.");
                return;
            }
            self.generate(id);
            self.answer(press.callback, "Готовлю сводку");
        } else if phase == Phase::Ready && operation == "edit" {
            self.prompt(id);
            self.answer(press.callback, "Ответьте на сообщение об исправлении");
        } else if phase == Phase::Ready && operation == "send" {
            self.create(id);
            self.answer(press.callback, "Создаю новую тему");
        } else {
            self.answer(press.callback, "Дождитесь завершения текущей операции.");
        }
    }

    fn generate(&mut self, id: &str) {
        if let Some(source) = self.drafts.get(id).map(|draft| draft.session.clone()) {
            let peers: Vec<_> = self
                .drafts
                .iter()
                .filter(|(other, draft)| {
                    other.as_str() != id && same_session(&draft.session, &source)
                })
                .map(|(id, _)| id.clone())
                .collect();
            for peer in peers {
                self.discard(&peer);
            }
        }
        let Some(draft) = self.drafts.get_mut(id) else { return };
        let mut settings = Settings::clone(&draft.settings);
        let Ok(parameters) = draft.session.preferences.resolve(draft.session.backend, &settings)
        else {
            let key = draft.key;
            self.notice(
                key,
                "Проверьте настройки текущего агента и повторите /handoff.".to_owned(),
            );
            return;
        };
        parameters.apply(&mut settings);
        draft.phase = Phase::Generating;
        let cancel = CancellationToken::new();
        let progress = Progress::new();
        let run = SummaryRun {
            agents: self.agents.clone(),
            session: draft.session.clone(),
            settings: Arc::new(settings),
            cancel: cancel.clone(),
            progress: progress.clone(),
        };
        let (job_id, revision, mailbox) = (id.to_owned(), draft.revision, self.mailbox.clone());
        let task = tokio::spawn(async move {
            let result =
                AssertUnwindSafe(run.execute()).catch_unwind().await.unwrap_or_else(|_| {
                    Err("Не удалось подготовить сводку. Исходная сессия сохранена.".to_owned())
                });
            let _ = mailbox
                .send(HubMessage::HandoffDone(Done {
                    id: job_id,
                    revision,
                    outcome: Outcome::Generated(result),
                }))
                .await;
        });
        self.jobs.insert(
            id.to_owned(),
            Job { key: draft.key, session: draft.session.clone(), cancel, progress, _task: task },
        );
        // Do not replace the card asynchronously here: a very fast result could be
        // overwritten by a delayed progress edit. The original cancel button stays valid.
        let key = draft.key;
        self.notice(
            key,
            "📋 Текущий агент готовит сводку… до 3 минут. /stop или «Отмена» прервут подготовку."
                .to_owned(),
        );
    }

    pub fn complete(
        &mut self,
        done: Done,
        settings: &Settings,
        topics: &Topics,
    ) -> Option<Transfer> {
        let cancelled = self.jobs.get(&done.id).is_some_and(|job| job.cancel.is_cancelled());
        if matches!(&done.outcome, Outcome::Generated(_)) {
            self.jobs.remove(&done.id);
        }
        self.refresh(settings, topics);
        let draft = self.drafts.get_mut(&done.id)?;
        if draft.revision != done.revision {
            return None;
        }
        draft.task = None;
        let outcome = if cancelled && matches!(&done.outcome, Outcome::Generated(_)) {
            Outcome::Generated(Err("Подготовка сводки отменена.".to_owned()))
        } else {
            done.outcome
        };
        match outcome {
            Outcome::Published { message: Some(message), phase } => {
                draft.message = Some(message);
                draft.phase = phase;
            }
            Outcome::Prompted(Some(prompt)) => {
                draft.prompt = Some(prompt);
                draft.phase = Phase::Editing;
            }
            Outcome::Published { message: None, .. } | Outcome::Prompted(None) => {
                let key = draft.key;
                self.discard(&done.id);
                self.notice(
                    key,
                    "Не удалось показать сводку. Исходная сессия сохранена. Повторите /handoff."
                        .to_owned(),
                );
            }
            Outcome::Generated(Ok(prepared)) => {
                draft.summary = Some(prepared.summary);
                draft.footer = format_finished(&prepared.finished);
                draft.revision += 1;
                self.publish(&done.id, Phase::Ready);
            }
            Outcome::Generated(Err(error)) | Outcome::Created(Err(error)) => {
                let key = draft.key;
                self.finish(&done.id, &error);
                self.discard(&done.id);
                self.notice(key, error);
            }
            Outcome::Created(Ok(thread)) => {
                let destination = TopicKey { chat: draft.key.chat, thread };
                if draft.cancelled {
                    let key = draft.key;
                    self.discard(&done.id);
                    self.notice(key, format!("Передача отменена; задача не запущена. Создана пустая {}. Исходная тема сохранена.", topic_reference(destination)));
                    return None;
                }
                self.finish(&done.id, "Сводка подтверждена. Исходная тема сохранена.");
                let draft = self.drafts.remove(&done.id)?;
                return Some(Transfer {
                    source: draft.key,
                    destination,
                    original: draft.session.clone(),
                    recipient: draft.recipient?,
                    summary: draft.summary.clone()?,
                });
            }
        }
        None
    }

    fn publish(&mut self, id: &str, phase: Phase) {
        let Some(draft) = self.drafts.get_mut(id) else { return };
        draft.phase = Phase::Publishing;
        let (id, revision, key, previous, card) =
            (id.to_owned(), draft.revision, draft.key, draft.message, draft.card(id, phase));
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
            let message = tokio::time::timeout(IO_TIMEOUT, send).await.ok().flatten();
            let _ = mailbox
                .send(HubMessage::HandoffDone(Done {
                    id,
                    revision,
                    outcome: Outcome::Published { message, phase },
                }))
                .await;
        }));
    }

    fn prompt(&mut self, id: &str) {
        let Some(draft) = self.drafts.get_mut(id) else { return };
        draft.phase = Phase::Prompting;
        let (id, revision, key, source) =
            (id.to_owned(), draft.revision, draft.key, draft.source_message);
        let (sender, mailbox) = (self.sender.clone(), self.mailbox.clone());
        draft.task = Some(tokio::spawn(async move {
            let prompt = Outgoing::plain(HANDOFF_EDIT_PROMPT.to_owned())
                .with_force_reply()
                .replying_to(source);
            let message = tokio::time::timeout(
                IO_TIMEOUT,
                sender.messenger().send(Target::Topic(key), prompt),
            )
            .await
            .ok()
            .and_then(Result::ok);
            let _ = mailbox
                .send(HubMessage::HandoffDone(Done {
                    id,
                    revision,
                    outcome: Outcome::Prompted(message),
                }))
                .await;
        }));
    }

    pub fn correction(&mut self, inbound: &Inbound) -> bool {
        if !inbound.handoff_reply {
            return false;
        }
        let Some(key) = inbound.key() else { return true };
        let id = self
            .drafts
            .iter()
            .find(|(_, draft)| {
                draft.key == key
                    && Some(draft.user) == inbound.user
                    && draft.phase == Phase::Editing
                    && draft.prompt == inbound.reply_to
            })
            .map(|(id, _)| id.clone());
        let Some(id) = id else {
            self.notice(key, STALE.to_owned());
            return true;
        };
        let summary = match &inbound.content {
            Content::Text(text) => Summary::parse(text),
            _ => Err("Ответьте полным текстом сводки."),
        };
        match summary {
            Ok(summary) => {
                if let Some(draft) = self.drafts.get_mut(&id) {
                    draft.summary = Some(summary);
                    draft.prompt = None;
                    draft.revision += 1;
                }
                self.publish(&id, Phase::Ready);
            }
            Err(error) => self.notice(key, error.to_owned()),
        }
        true
    }

    fn create(&mut self, id: &str) {
        let Some(draft) = self.drafts.get_mut(id) else { return };
        let Some(recipient) = draft.recipient else { return };
        draft.phase = Phase::Creating;
        draft.progress.executing();
        let (id, revision, chat) = (id.to_owned(), draft.revision, draft.key.chat);
        let (sender, mailbox) = (self.sender.clone(), self.mailbox.clone());
        draft.task = Some(tokio::spawn(async move {
            let name = format!("Передача в {}", recipient.name());
            let result = match tokio::time::timeout(IO_TIMEOUT, sender.messenger().create_topic(chat, name)).await {
                Ok(Ok(thread)) => Ok(thread),
                Ok(Err(_)) | Err(_) => Err("Не удалось подтвердить создание темы. Проверьте право бота управлять темами и список тем: при потере ответа могла появиться пустая тема. Автоматического повтора нет. Исходная сессия сохранена; повторить передачу можно через /handoff.".to_owned()),
            };
            let _ = mailbox
                .send(HubMessage::HandoffDone(Done {
                    id,
                    revision,
                    outcome: Outcome::Created(result),
                }))
                .await;
        }));
    }

    fn finish(&self, id: &str, text: &str) {
        if let Some(draft) = self.drafts.get(id)
            && let Some(message) = draft.message
        {
            let (sender, chat, text) = (self.sender.clone(), draft.key.chat, text.to_owned());
            tokio::spawn(async move {
                let _ = tokio::time::timeout(
                    IO_TIMEOUT,
                    sender.messenger().edit_card(chat, message, Outgoing::plain(text)),
                )
                .await;
            });
        }
    }
    fn answer(&self, callback: String, text: &str) {
        let (sender, text) = (self.sender.clone(), text.to_owned());
        tokio::spawn(async move {
            let _ =
                tokio::time::timeout(IO_TIMEOUT, sender.messenger().answer(callback, Some(text)))
                    .await;
        });
    }
    pub fn notice(&self, key: TopicKey, text: String) {
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let _ = tokio::time::timeout(
                IO_TIMEOUT,
                sender.messenger().send(Target::Topic(key), Outgoing::plain(text)),
            )
            .await;
        });
    }
}

fn same_session(left: &TopicSession, right: &TopicSession) -> bool {
    left.backend == right.backend && left.session.is_some() && left.session == right.session
}

pub fn topic_reference(key: TopicKey) -> String {
    if (-1_997_852_516_352..=-1_000_000_000_001).contains(&key.chat.0) && key.thread.0 > 0 {
        let channel = -key.chat.0 - 1_000_000_000_000;
        format!("тема https://t.me/c/{channel}/{}", key.thread.0)
    } else {
        format!("тема #{}", key.thread.0)
    }
}
