//! One-shot action buttons and the lifetime of user-started processes.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures::{FutureExt, future::BoxFuture};
use hub_core::actions::{Action, MAX_ACTIONS};
use hub_core::domain::ChatId;
use hub_core::questions::Button;
use hub_core::settings::Settings;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::hub::HubMessage;
use crate::inbound::Press;
use crate::messenger::{Outgoing, Target};
use crate::sender::Sender;
use crate::telegram::allowed;

pub const PREFIX: &str = "action:";
const TICKET_TTL: Duration = Duration::from_mins(15);
const MAX_TICKETS: usize = 256;
const STALE: &str = "Кнопка устарела. Откройте /actions заново.";

pub trait ActionRunner: Send + Sync + 'static {
    /// Runs a saved definition until it exits or is cancelled; returns a bounded user-facing result.
    fn run(&self, action: Action, cancel: CancellationToken) -> BoxFuture<'static, String>;
}

enum Operation {
    Start(Action),
    Stop { id: String, run: String },
}

struct Ticket {
    operation: Operation,
    target: Target,
    created: Instant,
}

struct Running {
    action: Action,
    run: String,
    cancel: CancellationToken,
    started: Instant,
}

pub struct Done {
    id: String,
    run: String,
    name: String,
    target: Target,
    result: String,
}

pub struct Actions {
    runner: Arc<dyn ActionRunner>,
    tickets: HashMap<String, Ticket>,
    running: HashMap<String, Running>,
}

impl Actions {
    #[must_use]
    pub fn new(runner: Arc<dyn ActionRunner>) -> Self {
        Self { runner, tickets: HashMap::new(), running: HashMap::new() }
    }

    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.running.is_empty()
    }

    pub fn shutdown(&self) {
        for running in self.running.values() {
            running.cancel.cancel();
        }
    }

    fn ticket(&mut self, operation: Operation, target: Target) -> String {
        self.tickets.retain(|_, ticket| ticket.created.elapsed() < TICKET_TTL);
        if self.tickets.len() >= MAX_TICKETS {
            let oldest = self
                .tickets
                .iter()
                .min_by_key(|(_, ticket)| ticket.created)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.tickets.remove(&oldest);
            }
        }
        let id = format!("{PREFIX}{}", uuid::Uuid::new_v4().simple());
        self.tickets.insert(id.clone(), Ticket { operation, target, created: Instant::now() });
        id
    }

    #[must_use]
    pub fn states(&self, ids: &[String], configured: &[Action]) -> Vec<String> {
        ids.iter()
            .map(|id| {
                let configured = configured.iter().find(|action| action.id() == id);
                let running = self.running.get(id);
                let name = configured
                    .map(Action::name)
                    .or_else(|| running.map(|run| run.action.name()))
                    .unwrap_or(id);
                let state = match running {
                    Some(run) if run.cancel.is_cancelled() => "останавливается",
                    Some(run) if configured != Some(&run.action) => "работает по прежней команде",
                    Some(_) => "работает",
                    None => "не запущен через бот",
                };
                format!("{name}: {state}")
            })
            .collect()
    }

    pub fn menu(&mut self, configured: &[Action], target: Target, sender: Sender) {
        let mut actions = configured.to_vec();
        for running in self.running.values() {
            if !actions.iter().any(|action| action.id() == running.action.id()) {
                actions.push(running.action.clone());
            }
        }
        let mut text = "⚙️ Пользовательские действия\n".to_owned();
        let mut keyboard = Vec::new();
        for action in actions {
            let (label, operation) = match self.running.get(action.id()) {
                Some(running) => {
                    let state = if running.cancel.is_cancelled() {
                        "останавливается"
                    } else {
                        "работает"
                    };
                    (
                        format!(
                            "⏹ {} · {state} {} с",
                            running.action.name(),
                            running.started.elapsed().as_secs()
                        ),
                        Operation::Stop { id: action.id().to_owned(), run: running.run.clone() },
                    )
                }
                None => (format!("▶ {}", action.name()), Operation::Start(action)),
            };
            keyboard.push(vec![Button { text: label, data: self.ticket(operation, target) }]);
        }
        if keyboard.is_empty() {
            text.push_str(
                "\nДействий пока нет. Добавьте название и команду в «Настройки → Действия» на ПК.",
            );
        } else {
            text.push_str("\n\nКнопки одноразовые, действуют 15 минут. Обновить список: /actions.");
        }
        tokio::spawn(async move {
            sender.one(target, Outgoing::plain(text).with_keyboard(keyboard)).await;
        });
    }

    pub fn press(
        &mut self,
        press: Press,
        settings: &Settings,
        sender: Sender,
        mailbox: mpsc::Sender<HubMessage>,
    ) {
        if !allowed(&settings.telegram, press.chat, Some(press.user)) {
            return;
        }
        let ticket = self.tickets.get(&press.data).filter(|ticket| {
            ticket.created.elapsed() < TICKET_TTL && Some(chat(ticket.target)) == press.chat
        });
        if ticket.is_none() {
            answer(sender, press.callback, STALE);
            return;
        }
        let Some(ticket) = self.tickets.remove(&press.data) else { return };
        match ticket.operation {
            Operation::Start(action) => {
                if !settings.actions.contains(&action) {
                    answer(sender, press.callback, STALE);
                } else if self.running.contains_key(action.id()) {
                    answer(
                        sender,
                        press.callback,
                        "Это действие уже выполняется. Откройте /actions для остановки.",
                    );
                } else if self.running.len() >= MAX_ACTIONS {
                    answer(
                        sender,
                        press.callback,
                        "Слишком много работающих действий. Сначала остановите одно из них.",
                    );
                } else {
                    tracing::info!(
                        action_id = action.id(),
                        user_id = press.user.0,
                        "starting custom action"
                    );
                    answer(sender.clone(), press.callback, "Запускаю. Управление: /actions");
                    self.start(action, ticket.target, sender, mailbox);
                }
            }
            Operation::Stop { id, run } => {
                if let Some(running) = self.running.get(&id).filter(|running| running.run == run) {
                    tracing::info!(
                        action_id = id,
                        user_id = press.user.0,
                        "stopping custom action"
                    );
                    running.cancel.cancel();
                    answer(sender, press.callback, "Останавливаю…");
                } else {
                    answer(sender, press.callback, STALE);
                }
            }
        }
    }

    fn start(
        &mut self,
        action: Action,
        target: Target,
        sender: Sender,
        mailbox: mpsc::Sender<HubMessage>,
    ) {
        let run = uuid::Uuid::new_v4().to_string();
        let cancel = CancellationToken::new();
        self.running.insert(
            action.id().to_owned(),
            Running {
                action: action.clone(),
                run: run.clone(),
                cancel: cancel.clone(),
                started: Instant::now(),
            },
        );
        let runner = Arc::clone(&self.runner);
        tokio::spawn(async move {
            // Delivery is independent: a Telegram outage must not prevent cancellation or execution.
            let notice_sender = sender.clone();
            let notice = format!("▶ {}: запуск команды. Остановить: /actions", action.name());
            tokio::spawn(async move {
                notice_sender.text(target, &notice).await;
            });
            let future = async { runner.run(action.clone(), cancel).await };
            let result = AssertUnwindSafe(future)
                .catch_unwind()
                .await
                .unwrap_or_else(|_| "❌ Ошибка выполнения действия".to_owned());
            let done = Done {
                id: action.id().to_owned(),
                name: action.name().to_owned(),
                run,
                target,
                result,
            };
            // A closed mailbox means shutdown has already completed; there is no state left to update.
            let _ = mailbox.send(HubMessage::ActionDone(done)).await;
        });
    }

    pub fn complete(&mut self, done: Done, sender: Sender) {
        if self.running.get(&done.id).is_some_and(|running| running.run == done.run) {
            self.running.remove(&done.id);
            tracing::info!(action_id = done.id, "custom action finished");
            tokio::spawn(async move {
                sender.text(done.target, &format!("{}\n{}", done.name, done.result)).await;
            });
        }
    }
}

impl Drop for Actions {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn chat(target: Target) -> ChatId {
    match target {
        Target::Topic(key) => key.chat,
        Target::Reply { chat, .. } => chat,
    }
}

fn answer(sender: Sender, callback: String, text: &'static str) {
    tokio::spawn(async move {
        sender.answer(callback, Some(text)).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeMessenger;
    use hub_core::actions::ActionDraft;
    use hub_core::domain::{MessageId, UserId};
    use hub_core::settings::Draft;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Runner(AtomicUsize);
    impl ActionRunner for Runner {
        fn run(&self, _action: Action, cancel: CancellationToken) -> BoxFuture<'static, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                cancel.cancelled().await;
                "stopped".to_owned()
            })
        }
    }

    fn settings(command: &str) -> Settings {
        let home = std::env::temp_dir();
        Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: home.display().to_string(),
            actions: vec![ActionDraft {
                id: "vpn".to_owned(),
                name: "Рабочий VPN".to_owned(),
                command: command.to_owned(),
                cwd: String::new(),
            }],
            ..Draft::default()
        }
        .parse(&home)
        .unwrap()
    }

    async fn button(
        book: &mut Actions,
        settings: &Settings,
        messenger: &Arc<FakeMessenger>,
    ) -> String {
        let previous = messenger.last_keyboard().map(|(id, _)| id);
        book.menu(
            &settings.actions,
            Target::Reply { chat: ChatId(-100), message: MessageId(1) },
            Sender::new(messenger.clone()),
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while messenger.last_keyboard().map(|(id, _)| id) == previous {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        messenger.last_keyboard().unwrap().1.first().unwrap().first().unwrap().data.clone()
    }

    fn press(data: &str, user: u64) -> Press {
        Press {
            callback: uuid::Uuid::new_v4().to_string(),
            user: UserId(user),
            chat: Some(ChatId(-100)),
            message: Some(MessageId(1)),
            html: None,
            data: data.to_owned(),
        }
    }

    async fn finish(
        book: &mut Actions,
        inbox: &mut mpsc::Receiver<HubMessage>,
        messenger: &Arc<FakeMessenger>,
    ) {
        let done =
            tokio::time::timeout(Duration::from_secs(2), inbox.recv()).await.unwrap().unwrap();
        let HubMessage::ActionDone(done) = done else {
            panic!("expected action completion");
        };
        book.complete(done, Sender::new(messenger.clone()));
    }

    #[tokio::test]
    async fn duplicate_press_starts_once_and_removed_action_can_be_stopped() {
        let runner = Arc::new(Runner::default());
        let mut book = Actions::new(runner.clone());
        let messenger = Arc::new(FakeMessenger::default());
        let settings = settings("echo private-command");
        let (mailbox, mut inbox) = mpsc::channel(10);
        let start = button(&mut book, &settings, &messenger).await;
        let other_menu_start = button(&mut book, &settings, &messenger).await;
        for data in [&start, &start, &other_menu_start] {
            book.press(press(data, 1), &settings, Sender::new(messenger.clone()), mailbox.clone());
        }
        assert_eq!(book.running.len(), 1);
        assert_eq!(book.states(&["vpn".to_owned()], &settings.actions), ["Рабочий VPN: работает"]);
        let changed = super::tests::settings("echo different");
        assert_eq!(
            book.states(&["vpn".to_owned()], &changed.actions),
            ["Рабочий VPN: работает по прежней команде"]
        );
        let mut removed = settings.clone();
        removed.actions.clear();
        let stop = button(&mut book, &removed, &messenger).await;
        book.press(press(&stop, 1), &removed, Sender::new(messenger.clone()), mailbox);
        assert_eq!(
            book.states(&["vpn".to_owned()], &settings.actions),
            ["Рабочий VPN: останавливается"]
        );
        finish(&mut book, &mut inbox, &messenger).await;
        assert!(book.is_idle());
        assert_eq!(
            book.states(&["vpn".to_owned()], &settings.actions),
            ["Рабочий VPN: не запущен через бот"]
        );
        assert_eq!(runner.0.load(Ordering::SeqCst), 1);
        assert!(!messenger.sent_texts().join("\n").contains("private-command"));
    }

    #[tokio::test]
    async fn unauthorized_and_changed_definition_buttons_cannot_run_commands() {
        let mut book = Actions::new(Arc::new(Runner::default()));
        let messenger = Arc::new(FakeMessenger::default());
        let settings = settings("echo old");
        let (mailbox, mut inbox) = mpsc::channel(10);
        let start = button(&mut book, &settings, &messenger).await;
        book.press(press(&start, 999), &settings, Sender::new(messenger.clone()), mailbox.clone());
        assert!(book.is_idle());
        book.press(
            press(&start, 1),
            &super::tests::settings("echo new"),
            Sender::new(messenger.clone()),
            mailbox.clone(),
        );
        assert!(book.is_idle());
        let start = button(&mut book, &settings, &messenger).await;
        book.press(press(&start, 1), &settings, Sender::new(messenger.clone()), mailbox);
        book.shutdown();
        finish(&mut book, &mut inbox, &messenger).await;
        assert!(book.is_idle());
    }

    #[tokio::test(start_paused = true)]
    async fn expired_buttons_and_ticket_store_are_bounded() {
        let mut book = Actions::new(Arc::new(Runner::default()));
        let messenger = Arc::new(FakeMessenger::default());
        let settings = settings("echo test");
        let start = button(&mut book, &settings, &messenger).await;
        tokio::time::advance(TICKET_TTL).await;
        let (mailbox, _) = mpsc::channel(10);
        book.press(press(&start, 1), &settings, Sender::new(messenger), mailbox);
        assert!(book.is_idle());
        for _ in 0..MAX_TICKETS + 10 {
            book.ticket(
                Operation::Start(settings.actions.first().unwrap().clone()),
                Target::Reply { chat: ChatId(-100), message: MessageId(1) },
            );
        }
        assert_eq!(book.tickets.len(), MAX_TICKETS);
    }

    #[tokio::test]
    async fn old_stop_button_does_not_cancel_a_new_run() {
        let mut book = Actions::new(Arc::new(Runner::default()));
        let messenger = Arc::new(FakeMessenger::default());
        let settings = settings("echo test");
        let (mailbox, mut inbox) = mpsc::channel(10);
        let start = button(&mut book, &settings, &messenger).await;
        book.press(press(&start, 1), &settings, Sender::new(messenger.clone()), mailbox.clone());
        let old_stop = button(&mut book, &settings, &messenger).await;
        book.shutdown();
        finish(&mut book, &mut inbox, &messenger).await;
        let start = button(&mut book, &settings, &messenger).await;
        book.press(press(&start, 1), &settings, Sender::new(messenger.clone()), mailbox.clone());
        book.press(press(&old_stop, 1), &settings, Sender::new(messenger.clone()), mailbox);
        assert!(!book.running.get("vpn").unwrap().cancel.is_cancelled());
        book.shutdown();
        finish(&mut book, &mut inbox, &messenger).await;
    }
}
