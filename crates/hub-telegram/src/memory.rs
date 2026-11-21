//! Model-free memory browsing and a narrow application-settings port.
use crate::hub::HubMessage;
use crate::inbound::Press;
use crate::messenger::{Outgoing, Target};
use crate::sender::Sender;
use futures::future::BoxFuture;
use hub_core::domain::{TopicKey, UserId};
use hub_core::questions::Button;
use hub_core::settings::Settings;
use hub_memory::{Entry, Error, MemoryPage, MemoryReader};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

mod admin;
pub mod context;
pub mod lifecycle;
mod portable;

pub const PREFIX: &str = "memory:";
pub const POLICY: &str = "Для всех проектов и агентов этого приложения. Применяется при следующем запуске процесса, включая продолжение сессии. Работающие процессы сохраняют прежнее подключение. Ручные CLI не затрагиваются; данные не удаляются.";

pub trait MemoryControl: Send + Sync {
    fn set_enabled(&self, enabled: bool) -> BoxFuture<'static, Result<(), String>>;
}

#[derive(Clone, Debug)]
pub enum Request {
    Admin(Box<lifecycle::Action>),
    ExportConfirm(bool),
    ExportSend(bool),
    ResetSelection {
        token: hub_memory::SnapshotToken,
        keep: Vec<String>,
        receipt: hub_memory::VerifiedExport,
    },
    List {
        query: String,
        offset: usize,
    },
    Get {
        key: String,
        revision: Option<u32>,
    },
    Confirm(bool),
    Set(bool),
}
#[derive(Clone, Debug)]
pub enum View {
    ExportReady {
        receipt: hub_memory::VerifiedExport,
        token: hub_memory::SnapshotToken,
        keep: Vec<String>,
        reset: bool,
    },
    Admin(Box<lifecycle::Panel>),
    List(MemoryPage),
    Entry(Entry),
    Notice(String),
}

pub fn read(
    base: &std::path::Path,
    cwd: &std::path::Path,
    request: &Request,
) -> Result<View, String> {
    if let Request::Admin(action) = request {
        return lifecycle::run(base, cwd, action).map(|panel| View::Admin(Box::new(panel)));
    }
    let reader = MemoryReader::new(base, cwd).map_err(memory_error)?;
    match request {
        Request::List { query, offset } => {
            reader.inspect(query, *offset, 20).map(View::List).map_err(memory_error)
        }
        Request::Get { key, revision } => read_entry(&reader, key, *revision).map(View::Entry),
        Request::Admin(_)
        | Request::Confirm(_)
        | Request::Set(_)
        | Request::ExportConfirm(_)
        | Request::ExportSend(_)
        | Request::ResetSelection { .. } => Err("Ожидалась операция чтения".into()),
    }
}

fn read_entry(reader: &MemoryReader, key: &str, revision: Option<u32>) -> Result<Entry, String> {
    match reader.get(key, revision) {
        Err(Error::NotFound { revision: Some(requested), .. }) => match reader.details(key, None) {
            Ok(latest) => Err(format!(
                "У записи «{key}» нет ревизии {requested}. Доступны ревизии {}. Чтобы открыть последнюю, укажите 0 в GUI или не указывайте ревизию в /memory get.",
                latest.revisions.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
            )),
            Err(error) => Err(memory_error(error)),
        },
        result => result.map_err(memory_error),
    }
}

fn memory_error(error: Error) -> String {
    error.user_message()
}

pub fn parse(args: &str) -> Result<Request, String> {
    let mut parts = args.split_whitespace();
    match parts.next() {
        Some("procedure") if parts.next().is_none() => {
            Ok(Request::Admin(Box::new(lifecycle::Action::Procedure)))
        }
        Some("context") => {
            let form = args.trim_start().strip_prefix("context").unwrap_or("").trim();
            if form.is_empty() {
                Ok(Request::Admin(Box::new(lifecycle::Action::ContextHelp)))
            } else {
                context::parse(form)
                    .map(|input| Request::Admin(Box::new(lifecycle::Action::Context(input))))
            }
        }
        None => Ok(Request::List { query: String::new(), offset: 0 }),
        Some("search") => {
            Ok(Request::List { query: parts.collect::<Vec<_>>().join(" "), offset: 0 })
        }
        Some("get") => {
            let key = parts.next().ok_or("Укажите ключ")?.to_owned();
            let revision =
                parts.next().map(str::parse::<u32>).transpose().map_err(|_| "Неверная ревизия")?;
            if parts.next().is_some() || revision == Some(0) {
                return Err("Неверные аргументы".into());
            }
            Ok(Request::Get { key, revision })
        }
        Some("export") if parts.next().is_none() => Ok(Request::ExportConfirm(false)),
        Some("reset") if parts.next().is_none() => Ok(Request::ExportConfirm(true)),
        _ => admin::parse_admin(args),
    }
}

#[must_use]
pub fn entry_text(entry: &Entry) -> String {
    format!(
        "{} · r{} · {:?}\n{}\nАвтор: {} · Unix time: {}\n\n{}\n\nИсточники:\n{}\n\nАвтор и источники — метаданные, не подтверждение одобрения человеком.",
        entry.key,
        entry.revision,
        entry.kind,
        entry.title,
        entry.author,
        entry.created_at,
        entry.body,
        entry.sources.join("\n")
    )
}

#[derive(Clone)]
struct Binding {
    key: TopicKey,
    user: UserId,
    cwd: PathBuf,
    request: Request,
}
pub struct Done {
    epoch: u64,
    binding: Binding,
    result: Result<View, String>,
}
pub struct Browser {
    sender: Sender,
    mailbox: mpsc::Sender<HubMessage>,
    base: PathBuf,
    control: Option<Arc<dyn MemoryControl>>,
    bindings: HashMap<String, (Binding, std::time::Instant)>,
    admin_draft: Option<(Binding, lifecycle::Panel, std::time::Instant)>,
    imports: HashMap<(TopicKey, UserId), (PathBuf, std::time::Instant)>,
    busy: bool,
    epoch: u64,
    active: Option<Binding>,
    cancel: tokio_util::sync::CancellationToken,
}
impl Browser {
    #[must_use]
    pub fn new(
        sender: Sender,
        mailbox: mpsc::Sender<HubMessage>,
        base: PathBuf,
        control: Option<Arc<dyn MemoryControl>>,
    ) -> Self {
        Self {
            sender,
            mailbox,
            base,
            control,
            bindings: HashMap::new(),
            imports: HashMap::new(),
            admin_draft: None,
            busy: false,
            epoch: 0,
            active: None,
            cancel: tokio_util::sync::CancellationToken::new(),
        }
    }
    pub fn menu(&mut self, key: TopicKey, user: Option<UserId>, cwd: PathBuf, args: &str) {
        let Some(user) = user else { return };
        self.bindings.retain(|_, (_, time)| time.elapsed() < std::time::Duration::from_mins(10));
        if self
            .admin_draft
            .as_ref()
            .is_some_and(|(_, _, time)| time.elapsed() >= std::time::Duration::from_mins(10))
        {
            self.admin_draft = None;
        }
        if self.edit_draft(key, user, &cwd, args) {
            return;
        }
        self.imports.retain(|_, (_, time)| time.elapsed() < std::time::Duration::from_mins(10));
        if args.trim() == "cancel" {
            self.cancel_active(key, user);
            if self
                .admin_draft
                .as_ref()
                .is_some_and(|(binding, _, _)| binding.key == key && binding.user == user)
            {
                self.admin_draft = None;
            }
            self.imports.remove(&(key, user));
            self.bindings.retain(|_, (binding, _)| binding.key != key || binding.user != user);
            self.say(key, "Ожидание импорта и подтверждения отменены.".into());
            return;
        }
        if args.trim() == "import" {
            if self.imports.len() >= 64 {
                self.say(key, "Слишком много ожидающих импортов. Повторите позже.".into());
                return;
            }
            self.imports.insert((key, user), (cwd, std::time::Instant::now()));
            self.say(key,"Пришлите один JSON-файл экспорта до 16 МиБ в эту тему в течение 10 минут. После проверки будет предварительный просмотр. Отмена: /memory cancel".into());
            return;
        }
        match parse(args) {
            Ok(request) => self.start(Binding { key, user, cwd, request }),
            Err(error) => self.say(key, error),
        }
    }
    fn begin(&mut self, binding: &Binding) -> u64 {
        self.busy = true;
        self.epoch = self.epoch.wrapping_add(1);
        self.active = Some(binding.clone());
        self.cancel = tokio_util::sync::CancellationToken::new();
        self.epoch
    }
    fn cancel_active(&mut self, key: TopicKey, user: UserId) {
        if self.active.as_ref().is_some_and(|binding| binding.key == key && binding.user == user) {
            self.epoch = self.epoch.wrapping_add(1);
            self.cancel.cancel();
            self.active = None;
        }
    }
    pub fn refresh(&mut self, settings: &Settings, topics: &hub_core::topics::Topics) {
        let valid = |binding: &Binding| {
            settings.telegram.chat == binding.key.chat
                && settings.telegram.users.contains(binding.user)
                && topics
                    .get(&binding.key)
                    .is_some_and(|session| session.cwd.as_path() == binding.cwd)
        };
        self.bindings.retain(|_, (binding, time)| {
            valid(binding) && time.elapsed() < std::time::Duration::from_mins(10)
        });
        if self.admin_draft.as_ref().is_some_and(|(binding, _, time)| {
            !valid(binding) || time.elapsed() >= std::time::Duration::from_mins(10)
        }) {
            self.admin_draft = None;
        }
        self.imports.retain(|(key, user), (cwd, time)| {
            settings.telegram.chat == key.chat
                && settings.telegram.users.contains(*user)
                && topics.get(key).is_some_and(|session| session.cwd.as_path() == cwd)
                && time.elapsed() < std::time::Duration::from_mins(10)
        });
        if let Some(binding) = self.active.as_ref().filter(|binding| !valid(binding)).cloned() {
            self.cancel_active(binding.key, binding.user);
        }
    }
    fn say(&self, key: TopicKey, text: String) {
        let sender = self.sender.clone();
        tokio::spawn(async move {
            sender.text(Target::Topic(key), &text).await;
        });
    }
    fn button(&mut self, text: String, binding: Binding) -> Button {
        if self.bindings.len() >= 256 {
            self.bindings.clear();
        }
        let id = format!("{PREFIX}{}", uuid::Uuid::new_v4());
        self.bindings.insert(id.clone(), (binding, std::time::Instant::now()));
        Button { text, data: id }
    }
    pub fn press(
        &mut self,
        press: Press,
        settings: &Settings,
        cwd: impl Fn(TopicKey) -> Option<PathBuf>,
    ) {
        let binding = self
            .bindings
            .get(&press.data)
            .filter(|(_, time)| time.elapsed() < std::time::Duration::from_mins(10))
            .map(|(binding, _)| binding.clone())
            .filter(|b| {
                press.thread == Some(b.key.thread)
                    && press.chat == Some(b.key.chat)
                    && press.user == b.user
                    && settings.telegram.chat == b.key.chat
                    && settings.telegram.users.contains(press.user)
                    && cwd(b.key).as_ref() == Some(&b.cwd)
            });
        let sender = self.sender.clone();
        let valid = binding.is_some();
        tokio::spawn(async move {
            sender
                .answer(
                    press.callback,
                    if valid {
                        None
                    } else {
                        Some("Меню устарело. Откройте /memory")
                    },
                )
                .await;
        });
        if let Some(binding) = binding {
            self.start(binding);
        }
    }
    fn start(&mut self, mut binding: Binding) {
        if let Request::Get { key, revision } = &binding.request {
            binding.request =
                admin::admin(lifecycle::Action::Details { key: key.clone(), revision: *revision });
        }
        if self.busy {
            self.say(binding.key, "Операция памяти выполняется; повторите позже".into());
            return;
        }
        let epoch = self.begin(&binding);
        let base = self.base.clone();
        let mailbox = self.mailbox.clone();
        let control = self.control.clone();
        tokio::spawn(async move {
            let result = match &binding.request {
                Request::ExportConfirm(_) => Ok(View::Notice("Экспорт содержит память проекта и станет доступен участникам группы. Подтвердите отправку файла в эту тему.".into())),
                Request::ExportSend(reset) => portable::prepare_export(&base,&binding.cwd,*reset).await,
                Request::ResetSelection{token,keep,receipt} => Ok(View::Admin(Box::new(lifecycle::Panel::Reset{token:token.clone(),keep:keep.clone(),receipt:receipt.clone()}))),
                Request::Confirm(_) => {
                    Ok(View::Notice(format!("Подтвердите изменение.\n{POLICY}")))
                }
                Request::Set(enabled) => match control {
                    Some(control) => control
                        .set_enabled(*enabled)
                        .await
                        .map(|()| View::Notice(format!("Настройка сохранена.\n{POLICY}"))),
                    None => Err("Управление настройками недоступно".into()),
                },
                Request::List { .. } | Request::Get { .. } | Request::Admin(_) => {
                    read_async(base, binding.cwd.clone(), binding.request.clone()).await
                }
            };
            let _ = mailbox.send(HubMessage::MemoryDone(Done { epoch, binding, result })).await;
        });
    }
    fn deliver_export(
        &mut self,
        binding: Binding,
        receipt: hub_memory::VerifiedExport,
        token: hub_memory::SnapshotToken,
        keep: Vec<String>,
        reset: bool,
    ) {
        self.busy = true;
        let epoch = self.epoch;
        let cancel = self.cancel.clone();
        let sender = self.sender.clone();
        let mailbox = self.mailbox.clone();
        tokio::spawn(async move {
            let sent = tokio::select! {
                biased;
                ()=cancel.cancelled()=> {let _=mailbox.send(HubMessage::MemoryDone(Done{binding,result:Err("Экспорт отменён".into()),epoch})).await;return;},
                result=tokio::time::timeout(std::time::Duration::from_secs(30),sender.document(binding.key,receipt.path().to_path_buf(),"Экспорт памяти проекта. Авторы и источники не подтверждают согласие пользователя."))=>result,
            };
            let result=match sent {
                    Ok(Ok(())) if reset=>Ok(View::Admin(Box::new(lifecycle::Panel::Reset{token,keep,receipt}))),
                    Ok(Ok(()))=>Ok(View::Notice("Экспорт отправлен. Сохраните файл в надёжном месте.".into())),
                    Ok(Err(error))=>Err(error.to_string()),
                    Err(_)=>Err("Отправка экспорта превысила 30 с; доставка неизвестна, пересоздание не разрешено.".into()),
                };
            let _ = mailbox.send(HubMessage::MemoryDone(Done { epoch, binding, result })).await;
        });
    }
    fn list_card(
        &mut self,
        page: &MemoryPage,
        binding: &Binding,
        settings: &Settings,
        keyboard: &mut Vec<Vec<Button>>,
    ) -> String {
        for entry in &page.entries {
            keyboard.push(vec![self.button(
                format!("{} · r{}", entry.key, entry.revision),
                Binding {
                    request: Request::Get { key: entry.key.clone(), revision: None },
                    ..binding.clone()
                },
            )]);
        }
        if let Request::List { query, offset } = &binding.request {
            let mut row = Vec::new();
            if *offset > 0 {
                row.push(self.button(
                    "←".into(),
                    Binding {
                        request: Request::List {
                            query: query.clone(),
                            offset: offset.saturating_sub(20),
                        },
                        ..binding.clone()
                    },
                ));
            }
            if offset + page.entries.len() < page.matched_count {
                row.push(self.button(
                    "→".into(),
                    Binding {
                        request: Request::List { query: query.clone(), offset: offset + 20 },
                        ..binding.clone()
                    },
                ));
            }
            if !row.is_empty() {
                keyboard.push(row);
            }
        }
        keyboard.push(vec![
            self.button(
                if settings.memory.enabled {
                    "Отключить для всех проектов"
                } else {
                    "Включить для всех проектов"
                }
                .into(),
                Binding { request: Request::Confirm(!settings.memory.enabled), ..binding.clone() },
            ),
        ]);
        format!(
            "Память\nПроект: {}\nХранилище: {}\nКлючей: {}/500 · байт: {}/8388608\nНайдено: {}\nНастройка подключения: {}\n{}\n{}\nПоиск: /memory search ТЕКСТ\nОткрыть: /memory get КЛЮЧ [РЕВИЗИЯ]",
            page.project.display(),
            page.directory.display(),
            page.key_count,
            page.bytes,
            page.matched_count,
            if settings.memory.enabled { "включено" } else { "выключено" },
            POLICY,
            if page.exists { "" } else { "Хранилище ещё не создано." }
        )
    }

    fn retain_preview(&mut self, binding: &Binding, result: &Result<View, String>) {
        if let Ok(View::Admin(panel)) = result
            && matches!(
                panel.as_ref(),
                lifecycle::Panel::Import { .. } | lifecycle::Panel::Reset { .. }
            )
        {
            // One transfer draft across the browser bounds retained export data and temporary files.
            self.bindings.retain(|_, (binding, _)| !binding.request.carries_export());
            self.admin_draft = Some((binding.clone(), *panel.clone(), std::time::Instant::now()));
        }
        self.active = None;
        self.bindings.retain(|_, (old, time)| {
            time.elapsed() < std::time::Duration::from_mins(10)
                && (old.key != binding.key || old.user != binding.user)
        });
    }
    pub fn finish(
        &mut self,
        done: Done,
        settings: &Settings,
        cwd: Option<&PathBuf>,
        active_memory: Option<bool>,
    ) {
        self.busy = false;
        let Done { epoch, binding, result } = done;
        if epoch != self.epoch {
            return;
        }
        if cwd != Some(&binding.cwd)
            || settings.telegram.chat != binding.key.chat
            || !settings.telegram.users.contains(binding.user)
        {
            return;
        }
        if let Ok(View::Admin(panel)) = &result
            && let lifecycle::Panel::ContextReady(packet) = panel.as_ref()
        {
            self.deliver_context(binding, packet.clone());
            return;
        }
        if let Ok(View::ExportReady { receipt, token, keep, reset }) = result {
            self.deliver_export(binding, receipt, token, keep, reset);
            return;
        }
        self.retain_preview(&binding, &result);
        let mut keyboard = Vec::new();
        let mut text = match result {
            Err(error) => format!("Ошибка памяти: {error}"),
            Ok(View::Notice(text)) => {
                if let Request::ExportConfirm(reset) = binding.request {
                    keyboard.push(vec![self.button(
                        "Отправить экспорт в эту тему".into(),
                        Binding { request: Request::ExportSend(reset), ..binding.clone() },
                    )]);
                }
                if let Request::Confirm(enabled) = binding.request {
                    keyboard.push(vec![self.button(
                        "Подтвердить для всех проектов".into(),
                        Binding { request: Request::Set(enabled), ..binding.clone() },
                    )]);
                }
                text
            }
            Ok(View::ExportReady { .. }) => return,
            Ok(View::Admin(panel)) => self.admin_card(&panel, &binding, &mut keyboard),
            Ok(View::List(page)) => self.list_card(&page, &binding, settings, &mut keyboard),
            Ok(View::Entry(entry)) => {
                if entry.revision > 1 {
                    keyboard.push(vec![self.button(
                        format!("Ревизия {}", entry.revision - 1),
                        Binding {
                            request: Request::Get {
                                key: entry.key.clone(),
                                revision: Some(entry.revision - 1),
                            },
                            ..binding.clone()
                        },
                    )]);
                }
                keyboard.push(vec![self.button(
                    "Последняя ревизия".into(),
                    Binding {
                        request: Request::Get { key: entry.key.clone(), revision: None },
                        ..binding.clone()
                    },
                )]);
                entry_text(&entry)
            }
        };
        text.push_str(match active_memory {
            Some(true) => "\nПодключение текущего процесса: включено.",
            Some(false) => "\nПодключение текущего процесса: выключено.",
            None => "\nПодключение текущего процесса: нет данных (процесс не запущен или состояние неизвестно).",
        });
        keyboard.push(vec![self.button(
            "Управление памятью проекта".into(),
            Binding {
                request: admin::admin(lifecycle::Action::Overview {
                    filter: hub_memory::StatusFilter::All,
                    query: String::new(),
                    offset: 0,
                }),
                ..binding.clone()
            },
        )]);
        keyboard.push(vec![self.button(
            "Обновить список".into(),
            Binding {
                request: Request::List { query: String::new(), offset: 0 },
                ..binding.clone()
            },
        )]);
        let sender = self.sender.clone();
        tokio::spawn(async move {
            sender.text(Target::Topic(binding.key), &text).await;
            sender
                .one(
                    Target::Topic(binding.key),
                    Outgoing::plain("Память: действия".into()).with_keyboard(keyboard),
                )
                .await;
        });
    }
}
impl Done {
    #[must_use]
    pub fn key(&self) -> TopicKey {
        self.binding.key
    }
}

// A timed-out blocking filesystem operation keeps its permit until it really ends.
static READ_SLOT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
async fn read_async(base: PathBuf, cwd: PathBuf, request: Request) -> Result<View, String> {
    let permit = READ_SLOT.try_acquire().map_err(|_| "Чтение памяти занято; повторите позже")?;
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        read(&base, &cwd, &request)
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), task)
        .await
        .map_err(|_| "Чтение памяти превысило 10 с".to_owned())?
        .map_err(|_| "Не удалось прочитать память".to_owned())?
}

impl Request {
    fn carries_export(&self) -> bool {
        matches!(self, Self::ResetSelection { .. })
            || matches!(self,Self::Admin(action) if matches!(action.as_ref(),lifecycle::Action::Import{..}|lifecycle::Action::Compare{..}|lifecycle::Action::ReopenImport(_)|lifecycle::Action::ApplyImport(_)|lifecycle::Action::Reset{..}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stale_or_foreign_buttons_never_read_or_change_settings() {
        use hub_core::domain::{ChatId, ThreadId};
        let root = std::env::temp_dir();
        let settings = hub_core::settings::Draft {
            token: "1:a".into(),
            chat: "-100".into(),
            users: "1,2".into(),
            workspace_root: root.display().to_string(),
            ..Default::default()
        }
        .parse(&root)
        .unwrap();
        let messenger = Arc::new(crate::testing::FakeMessenger::default());
        let (mailbox, _rx) = mpsc::channel(2);
        let mut browser = Browser::new(Sender::new(messenger), mailbox, root.clone(), None);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(1) };
        let button = browser.button(
            "test".into(),
            Binding { key, user: UserId(1), cwd: root.clone(), request: Request::Set(false) },
        );
        for (user, chat, cwd, thread) in [
            (UserId(2), Some(key.chat), Some(root.clone()), Some(key.thread)),
            (UserId(1), Some(ChatId(-200)), Some(root.clone()), Some(key.thread)),
            (UserId(1), Some(key.chat), None, Some(key.thread)),
            (UserId(1), Some(key.chat), Some(root.clone()), Some(ThreadId(2))),
        ] {
            browser.press(
                Press {
                    thread,
                    callback: "cb".into(),
                    user,
                    chat,
                    message: None,
                    html: None,
                    data: button.data.clone(),
                },
                &settings,
                |_| cwd.clone(),
            );
            assert!(!browser.busy);
        }
    }

    #[tokio::test]
    async fn old_results_after_project_change_are_not_sent() {
        use hub_core::domain::{ChatId, ThreadId};
        let root = std::env::temp_dir();
        let settings = hub_core::settings::Draft {
            token: "1:a".into(),
            chat: "-100".into(),
            users: "1".into(),
            workspace_root: root.display().to_string(),
            ..Default::default()
        }
        .parse(&root)
        .unwrap();
        let messenger = Arc::new(crate::testing::FakeMessenger::default());
        let (mailbox, _rx) = mpsc::channel(2);
        let mut browser = Browser::new(Sender::new(messenger.clone()), mailbox, root.clone(), None);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(1) };
        browser.finish(
            Done {
                epoch: 0,
                binding: Binding {
                    key,
                    user: UserId(1),
                    cwd: root,
                    request: Request::List { query: String::new(), offset: 0 },
                },
                result: Ok(View::Notice("private".into())),
            },
            &settings,
            None,
            None,
        );
        tokio::task::yield_now().await;
        assert!(messenger.calls().is_empty());
    }

    #[tokio::test]
    async fn menu_reports_active_connection_separately_from_saved_policy() {
        use hub_core::domain::{ChatId, ThreadId};
        let root = std::env::temp_dir();
        let mut settings = hub_core::settings::Draft {
            token: "1:a".into(),
            chat: "-100".into(),
            users: "1".into(),
            workspace_root: root.display().to_string(),
            ..Default::default()
        }
        .parse(&root)
        .unwrap();
        settings.memory.enabled = false;
        let messenger = Arc::new(crate::testing::FakeMessenger::default());
        let (mailbox, _rx) = mpsc::channel(2);
        let mut browser = Browser::new(Sender::new(messenger.clone()), mailbox, root.clone(), None);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(1) };
        browser.finish(
            Done {
                epoch: 0,
                binding: Binding {
                    key,
                    user: UserId(1),
                    cwd: root.clone(),
                    request: Request::List { query: String::new(), offset: 0 },
                },
                result: Ok(View::List(MemoryPage {
                    bound_projects: Vec::new(),
                    project: root.clone(),
                    directory: root.clone(),
                    offset: 0,
                    exists: false,
                    bytes: 0,
                    key_count: 0,
                    matched_count: 0,
                    entries: Vec::new(),
                })),
            },
            &settings,
            Some(&root),
            Some(true),
        );
        tokio::task::yield_now().await;
        let text =
            messenger.send_attempts().into_iter().map(|m| m.text).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Настройка подключения: выключено"));
        assert!(text.contains("Подключение текущего процесса: включено"));
    }

    #[test]
    fn parses_explicit_revisions_and_rejects_bad_input() {
        assert!(matches!(parse("get key 2"), Ok(Request::Get { revision: Some(2), .. })));
        assert!(parse("get key 0").is_err());
        assert!(parse("get key 2 extra").is_err());
        assert!(
            matches!(parse("search my key"), Ok(Request::List { query, .. }) if query == "my key")
        );
    }
}
