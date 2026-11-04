//! Saved project diagnostics, independent of sessions and action execution.

use futures::future::BoxFuture;
use hub_core::{doctor::Report, projects::Project, settings::Settings};
use std::sync::Arc;

pub trait DoctorRunner: Send + Sync + 'static {
    fn run(
        &self,
        project: Project,
        settings: Arc<Settings>,
    ) -> BoxFuture<'static, Result<Report, String>>;
}

use crate::{
    actions::Actions, hub::HubMessage, inbound::Press, messenger::Outgoing, sender::Sender,
    telegram::allowed,
};
use futures::FutureExt;
use hub_core::{
    domain::{TopicKey, UserId},
    questions::Button,
};
use std::{collections::HashMap, panic::AssertUnwindSafe, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle, time::Instant};

pub const PREFIX: &str = "doctor:";
const TTL: Duration = Duration::from_mins(15);
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_TICKETS: usize = 256;
const MAX_RUNNING: usize = 2;
const STALE: &str = "Кнопка устарела. Откройте /doctor заново.";

#[derive(Clone, Copy)]
enum Operation {
    Check,
    Actions,
}

#[derive(Clone)]
struct Ticket {
    key: TopicKey,
    project: Project,
    settings: Arc<Settings>,
    operation: Operation,
    created: Instant,
}

struct Running {
    ticket: Ticket,
    user: UserId,
    run: String,
    task: JoinHandle<()>,
}
pub struct Done {
    key: TopicKey,
    run: String,
    result: Result<Report, String>,
}

pub struct Doctors {
    runner: Arc<dyn DoctorRunner>,
    tickets: HashMap<String, Ticket>,
    running: HashMap<TopicKey, Running>,
}

impl Doctors {
    #[must_use]
    pub fn new(runner: Arc<dyn DoctorRunner>) -> Self {
        Self { runner, tickets: HashMap::new(), running: HashMap::new() }
    }

    fn button(&mut self, text: &str, ticket: Ticket) -> Button {
        self.tickets.retain(|_, saved| saved.created.elapsed() < TTL);
        if self.tickets.len() >= MAX_TICKETS {
            let oldest = self
                .tickets
                .iter()
                .min_by_key(|(_, saved)| saved.created)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                self.tickets.remove(&id);
            }
        }
        let data = format!("{PREFIX}{}", uuid::Uuid::new_v4().simple());
        self.tickets.insert(data.clone(), ticket);
        Button { text: text.to_owned(), data }
    }

    pub fn menu(&mut self, key: TopicKey, settings: &Arc<Settings>) -> Outgoing {
        self.tickets.retain(|_, ticket| ticket.key != key);
        let keyboard = settings
            .projects
            .iter()
            .map(|project| {
                vec![self.button(
                    project.name(),
                    Ticket {
                        key,
                        project: project.clone(),
                        settings: settings.clone(),
                        operation: Operation::Check,
                        created: Instant::now(),
                    },
                )]
            })
            .collect();
        let text = if settings.projects.is_empty() {
            "Проектов пока нет. Добавьте их в «Настройки → Проекты» на ПК."
        } else {
            "🩺 Выберите проект для проверки. Контекст темы сохранится.\nКнопки действуют 15 минут."
        };
        Outgoing::plain(text.to_owned()).with_keyboard(keyboard)
    }

    fn take(&mut self, press: &Press, settings: &Settings) -> Result<Ticket, &'static str> {
        if !allowed(&settings.telegram, press.chat, Some(press.user)) {
            return Err("Нет доступа");
        }
        let ticket = self.tickets.get(&press.data).ok_or(STALE)?;
        if ticket.created.elapsed() >= TTL
            || press.chat != Some(ticket.key.chat)
            || ticket.settings.as_ref() != settings
        {
            return Err(STALE);
        }
        self.tickets.remove(&press.data).ok_or(STALE)
    }

    /// Returns a target only for the separate action menu; never starts a process.
    pub fn press(
        &mut self,
        press: Press,
        settings: &Settings,
        sender: Sender,
        mailbox: mpsc::Sender<HubMessage>,
    ) -> Option<TopicKey> {
        let ticket = match self.take(&press, settings) {
            Ok(ticket) => ticket,
            Err(error) => {
                answer(sender, press.callback, error);
                return None;
            }
        };
        match ticket.operation {
            Operation::Actions => {
                answer(sender, press.callback, "Открываю действия");
                Some(ticket.key)
            }
            Operation::Check => {
                if self.running.contains_key(&ticket.key) {
                    answer(sender, press.callback, "Проверка в этой теме уже выполняется.");
                } else if self.running.len() >= MAX_RUNNING {
                    answer(
                        sender,
                        press.callback,
                        "Уже выполняются две проверки. Повторите /doctor позже.",
                    );
                } else {
                    answer(sender, press.callback, "Проверяю… до 10 с");
                    self.start(ticket, press.user, mailbox);
                }
                None
            }
        }
    }

    fn start(&mut self, ticket: Ticket, user: UserId, mailbox: mpsc::Sender<HubMessage>) {
        let run = uuid::Uuid::new_v4().to_string();
        let key = ticket.key;
        let runner = self.runner.clone();
        let project = ticket.project.clone();
        let settings = ticket.settings.clone();
        let done_run = run.clone();
        let task = tokio::spawn(async move {
            let future = async { runner.run(project, settings).await };
            let result = match tokio::time::timeout(
                TIMEOUT,
                AssertUnwindSafe(future).catch_unwind(),
            )
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err("Ошибка выполнения проверки".to_owned()),
                Err(_) => Err("Проверка не завершилась за 10 с. Повторите позже.".to_owned()),
            };
            let _ = mailbox.send(HubMessage::DoctorDone(Done { key, run: done_run, result })).await;
        });
        self.running.insert(key, Running { ticket, user, run, task });
    }

    pub fn complete(
        &mut self,
        done: &Done,
        settings: &Settings,
        actions: &Actions,
        sender: Sender,
    ) {
        if self.running.get(&done.key).is_none_or(|running| running.run != done.run) {
            return;
        }
        let Some(running) = self.running.remove(&done.key) else { return };
        if !allowed(&settings.telegram, Some(done.key.chat), Some(running.user)) {
            return;
        }
        let ticket = running.ticket;
        let card = if ticket.settings.as_ref() == settings {
            let states = actions.states(ticket.project.actions(), &settings.actions);
            let text = render(&ticket.project, &done.result, &states);
            let repeat = self.button(
                "Повторить",
                Ticket { operation: Operation::Check, created: Instant::now(), ..ticket.clone() },
            );
            let actions = self.button(
                "Действия",
                Ticket { operation: Operation::Actions, created: Instant::now(), ..ticket },
            );
            Outgoing::plain(text).with_keyboard(vec![vec![repeat, actions]])
        } else {
            Outgoing::plain(
                "Настройки изменились во время проверки. Откройте /doctor заново.".to_owned(),
            )
        };
        tokio::spawn(crate::projects::send_card(sender, done.key, card));
    }

    pub fn shutdown(&mut self) {
        for (_, running) in self.running.drain() {
            running.task.abort();
        }
        self.tickets.clear();
    }
}
impl Drop for Doctors {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn answer(sender: Sender, callback: String, text: &'static str) {
    tokio::spawn(async move {
        sender.answer(callback, Some(text)).await;
    });
}

fn render(project: &Project, report: &Result<Report, String>, states: &[String]) -> String {
    let mut text = format!(
        "🩺 {}\nПроверено: {} UTC\n",
        project.name(),
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")
    );
    match report {
        Ok(report) => {
            let show = |label: &str, result: &Result<String, String>| match result {
                Ok(value) => format!("\n✅ {label}: {value}"),
                Err(error) => format!("\n❌ {label}: {error}"),
            };
            text.push_str(&show("Каталог", &report.directory));
            text.push_str(&show(
                &format!("CLI {} (наличие файла)", project.backend().name()),
                &report.cli,
            ));
            text.push_str("\n\nРесурсы:");
            if report.resources.is_empty() {
                text.push_str(
                    " проверки не заданы. Добавьте HTTP/TCP в «Настройки → Проекты» на ПК.",
                );
            }
            for (name, result) in &report.resources {
                text.push_str(&show(name, result));
            }
        }
        Err(error) => {
            text.push_str("\n❌ ");
            text.push_str(error);
        }
    }
    text.push_str("\n\nСвязанные процессы (сейчас):");
    if states.is_empty() {
        text.push_str(" не заданы");
    }
    for state in states {
        text.push('\n');
        text.push_str(state);
    }
    text.push_str("\n\nРаботающий процесс не подтверждает подключение VPN. Ошибка ресурса может быть связана с сетью, VPN или самим сервисом.");
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{FakeActionRunner, FakeDoctorRunner, FakeMessenger};
    use hub_core::{
        domain::{ChatId, ThreadId},
        projects::ProjectDraft,
        settings::Draft,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Pending {
        calls: AtomicUsize,
        active: Arc<AtomicUsize>,
    }
    struct Guard(Arc<AtomicUsize>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::SeqCst);
        }
    }
    impl DoctorRunner for Pending {
        fn run(
            &self,
            _project: Project,
            _settings: Arc<Settings>,
        ) -> BoxFuture<'static, Result<Report, String>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.active.fetch_add(1, Ordering::SeqCst);
            let guard = Guard(self.active.clone());
            Box::pin(async move {
                let _guard = guard;
                std::future::pending().await
            })
        }
    }
    fn settings() -> Arc<Settings> {
        let root = std::env::temp_dir();
        Arc::new(
            Draft {
                token: "1:a".to_owned(),
                chat: "-100".to_owned(),
                users: "1".to_owned(),
                workspace_root: root.display().to_string(),
                projects: vec![ProjectDraft {
                    id: "p".to_owned(),
                    name: "Project".to_owned(),
                    cwd: ".".to_owned(),
                    backend: "codex".to_owned(),
                    actions: vec![],
                    checks: vec![],
                }],
                ..Draft::default()
            }
            .parse(&root)
            .unwrap(),
        )
    }
    fn key(thread: i32) -> TopicKey {
        TopicKey { chat: ChatId(-100), thread: ThreadId(thread) }
    }
    fn button(book: &mut Doctors, settings: &Arc<Settings>, thread: i32) -> Press {
        let menu = book.menu(key(thread), settings);
        Press {
            callback: "c".to_owned(),
            user: UserId(1),
            chat: Some(ChatId(-100)),
            message: None,
            html: None,
            data: menu.keyboard.first().unwrap().first().unwrap().data.clone(),
        }
    }
    async fn done(inbox: &mut mpsc::Receiver<HubMessage>) -> Done {
        let HubMessage::DoctorDone(done) = inbox.recv().await.unwrap() else {
            panic!("expected doctor result")
        };
        done
    }

    #[tokio::test]
    async fn authorization_and_settings_are_checked_before_consuming_a_button() {
        let settings = settings();
        let mut book = Doctors::new(Arc::new(FakeDoctorRunner));
        let original = button(&mut book, &settings, 1);
        for press in [
            Press { user: UserId(2), ..original.clone() },
            Press { chat: Some(ChatId(-200)), ..original.clone() },
        ] {
            assert!(book.take(&press, &settings).is_err());
        }
        let mut changed = (*settings).clone();
        changed.projects.clear();
        assert!(book.take(&original, &changed).is_err());
        assert!(book.take(&original, &settings).is_ok());
        assert!(book.take(&original, &settings).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn buttons_expire_and_storage_is_bounded() {
        let settings = settings();
        let mut book = Doctors::new(Arc::new(FakeDoctorRunner));
        let original = button(&mut book, &settings, 1);
        tokio::time::advance(TTL).await;
        assert!(book.take(&original, &settings).is_err());
        for thread in 1..300 {
            book.menu(key(thread), &settings);
        }
        assert_eq!(book.tickets.len(), MAX_TICKETS);
    }

    #[tokio::test(start_paused = true)]
    async fn duplicate_and_parallel_requests_are_bounded_and_timeout_cancels_work() {
        let settings = settings();
        let runner = Arc::new(Pending::default());
        let mut book = Doctors::new(runner.clone());
        let messenger = Arc::new(FakeMessenger::default());
        let sender = Sender::new(messenger);
        let (mailbox, mut inbox) = mpsc::channel(10);
        let press = button(&mut book, &settings, 1);
        book.press(press.clone(), &settings, sender.clone(), mailbox.clone());
        book.press(press, &settings, sender.clone(), mailbox.clone());
        for thread in [1, 2, 3] {
            let press = button(&mut book, &settings, thread);
            book.press(press, &settings, sender.clone(), mailbox.clone());
        }
        assert_eq!(book.running.len(), 2);
        tokio::task::yield_now().await;
        assert_eq!(runner.calls.load(Ordering::SeqCst), 2);
        tokio::time::advance(TIMEOUT).await;
        let first = done(&mut inbox).await;
        assert!(first.result.as_ref().unwrap_err().contains("10 с"));
        let second = done(&mut inbox).await;
        let actions = Actions::new(Arc::new(FakeActionRunner));
        book.complete(&first, &settings, &actions, sender.clone());
        book.complete(&second, &settings, &actions, sender);
        assert!(book.running.is_empty());
        assert_eq!(runner.active.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn repeat_and_actions_are_separate_one_shot_choices() {
        let settings = settings();
        let mut book = Doctors::new(Arc::new(FakeDoctorRunner));
        let messenger = Arc::new(FakeMessenger::default());
        let sender = Sender::new(messenger.clone());
        let (mailbox, mut inbox) = mpsc::channel(10);
        let press = button(&mut book, &settings, 1);
        book.press(press.clone(), &settings, sender.clone(), mailbox.clone());
        let actions = Actions::new(Arc::new(FakeActionRunner));
        book.complete(&done(&mut inbox).await, &settings, &actions, sender.clone());
        let repeat = book
            .tickets
            .iter()
            .find(|(_, t)| matches!(t.operation, Operation::Check))
            .unwrap()
            .0
            .clone();
        let open_actions = book
            .tickets
            .iter()
            .find(|(_, t)| matches!(t.operation, Operation::Actions))
            .unwrap()
            .0
            .clone();
        assert_eq!(
            book.press(
                Press { data: open_actions.clone(), ..press.clone() },
                &settings,
                sender.clone(),
                mailbox.clone()
            ),
            Some(key(1))
        );
        assert_eq!(
            book.press(
                Press { data: open_actions, ..press.clone() },
                &settings,
                sender.clone(),
                mailbox.clone()
            ),
            None
        );
        assert!(book.running.is_empty());
        assert!(actions.is_idle());
        book.press(Press { data: repeat, ..press }, &settings, sender.clone(), mailbox);
        assert_eq!(book.running.len(), 1);
        book.complete(&done(&mut inbox).await, &settings, &actions, sender);
        tokio::task::yield_now().await;
        let text = messenger.sent_texts().join("\n");
        assert!(text.contains("test directory"));
        assert!(text.contains("UTC"));
        assert!(text.contains("не подтверждает подключение VPN"));
    }

    #[tokio::test]
    async fn changed_settings_discard_report_and_shutdown_aborts_jobs() {
        let settings = settings();
        let mut book = Doctors::new(Arc::new(FakeDoctorRunner));
        let messenger = Arc::new(FakeMessenger::default());
        let sender = Sender::new(messenger.clone());
        let (mailbox, mut inbox) = mpsc::channel(10);
        let press = button(&mut book, &settings, 1);
        book.press(press, &settings, sender.clone(), mailbox.clone());
        let mut changed = (*settings).clone();
        changed.projects.clear();
        book.complete(
            &done(&mut inbox).await,
            &changed,
            &Actions::new(Arc::new(FakeActionRunner)),
            sender.clone(),
        );
        tokio::task::yield_now().await;
        assert!(!messenger.sent_texts().join("\n").contains("test directory"));
        assert!(book.tickets.is_empty());
        let runner = Arc::new(Pending::default());
        let mut book = Doctors::new(runner.clone());
        let press = button(&mut book, &settings, 1);
        book.press(press, &settings, sender, mailbox);
        tokio::task::yield_now().await;
        assert_eq!(runner.active.load(Ordering::SeqCst), 1);
        book.shutdown();
        tokio::task::yield_now().await;
        assert_eq!(runner.active.load(Ordering::SeqCst), 0);
        assert!(book.running.is_empty());
        assert!(book.tickets.is_empty());
    }
}
