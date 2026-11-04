//! Short-lived project choices. The hub owns filesystem checks and binding persistence.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use hub_core::domain::{AbsolutePath, TopicKey, TopicSession};
use hub_core::projects::{Project, RecentProjects};
use hub_core::questions::Button;
use hub_core::settings::Settings;
use hub_core::topics::Topics;
use tokio::time::Instant;

use crate::inbound::Press;
use crate::messenger::Outgoing;
use crate::telegram::allowed;

pub const PREFIX: &str = "project:";
const TTL: Duration = Duration::from_mins(15);
const LIMIT: usize = 256;
pub const STALE: &str = "Кнопка устарела. Откройте /projects заново.";

#[derive(Clone)]
pub enum Stage {
    Preview,
    Apply(AbsolutePath),
    Cancel,
}

#[derive(Clone)]
pub struct Ticket {
    pub key: TopicKey,
    pub project: Project,
    pub stage: Stage,
    current: Option<TopicSession>,
    root: PathBuf,
    created: Instant,
}

pub struct Projects {
    tickets: HashMap<String, Ticket>,
    pub recent: RecentProjects,
}

impl Projects {
    pub fn new(recent: RecentProjects) -> Self {
        Self { tickets: HashMap::new(), recent }
    }

    fn button(&mut self, text: String, ticket: Ticket) -> Button {
        self.tickets.retain(|_, saved| saved.created.elapsed() < TTL);
        if self.tickets.len() >= LIMIT {
            let oldest = self
                .tickets
                .iter()
                .min_by_key(|(_, ticket)| ticket.created)
                .map(|(id, _)| id.clone());
            if let Some(id) = oldest {
                self.tickets.remove(&id);
            }
        }
        let data = format!("{PREFIX}{}", uuid::Uuid::new_v4().simple());
        self.tickets.insert(data.clone(), ticket);
        Button { text, data }
    }

    pub fn invalidate(&mut self, key: TopicKey) {
        self.tickets.retain(|_, ticket| ticket.key != key);
    }

    pub fn menu(
        &mut self,
        key: TopicKey,
        settings: &Settings,
        current: Option<&TopicSession>,
    ) -> Outgoing {
        self.invalidate(key);
        let mut ordered: Vec<_> = self
            .recent
            .ids()
            .iter()
            .filter_map(|id| settings.projects.iter().find(|project| project.id() == id))
            .collect();
        ordered.extend(
            settings
                .projects
                .iter()
                .filter(|project| !self.recent.ids().iter().any(|id| id == project.id())),
        );
        let keyboard = ordered
            .into_iter()
            .map(|project| {
                let recent = self.recent.ids().iter().any(|id| id == project.id());
                vec![self.button(
                    format!(
                        "{}{} · {}",
                        if recent { "🕘 " } else { "" },
                        project.name(),
                        project.backend().name()
                    ),
                    Ticket {
                        key,
                        project: project.clone(),
                        stage: Stage::Preview,
                        current: current.cloned(),
                        root: settings.workspace_root.clone(),
                        created: Instant::now(),
                    },
                )]
            })
            .collect();
        let text = if settings.projects.is_empty() {
            "Проектов пока нет. Добавьте их в «Настройки → Проекты» на ПК."
        } else {
            "📁 Выберите проект. Затем можно проверить каталог и подтвердить сброс контекста.\n🕘 — последние выбранные проекты. Кнопки действуют 15 минут."
        };
        Outgoing::plain(text.to_owned()).with_keyboard(keyboard)
    }

    pub fn take(
        &mut self,
        press: &Press,
        settings: &Settings,
        topics: &Topics,
    ) -> Result<Ticket, &'static str> {
        if !allowed(&settings.telegram, press.chat, Some(press.user)) {
            return Err("Нет доступа");
        }
        let ticket = self.tickets.get(&press.data).ok_or(STALE)?;
        if ticket.created.elapsed() >= TTL
            || Some(ticket.key.chat) != press.chat
            || !settings.projects.contains(&ticket.project)
            || ticket.root != settings.workspace_root
            || topics.get(&ticket.key) != ticket.current.as_ref()
        {
            return Err(STALE);
        }
        self.tickets.remove(&press.data).ok_or(STALE)
    }

    pub fn confirm(&mut self, ticket: Ticket, cwd: AbsolutePath, settings: &Settings) -> Outgoing {
        let actions: Vec<_> = ticket
            .project
            .actions()
            .iter()
            .filter_map(|id| settings.actions.iter().find(|action| action.id() == id))
            .map(hub_core::actions::Action::name)
            .collect();
        let mut text = format!(
            "📁 {}\nКаталог: {}\nБэкенд: {}\n\nВыбор создаст новую сессию и сбросит текущий контекст этой темы.",
            ticket.project.name(),
            cwd.as_path().display(),
            ticket.project.backend().name()
        );
        if !actions.is_empty() {
            text.push_str("\n\nСвязанные действия: ");
            text.push_str(&actions.join(", "));
            text.push_str(". Запуск вручную через /actions.");
        }
        let apply = self.button(
            "Выбрать и сбросить контекст".to_owned(),
            Ticket { stage: Stage::Apply(cwd), created: Instant::now(), ..ticket.clone() },
        );
        let cancel = self.button("Отмена".to_owned(), Ticket { stage: Stage::Cancel, ..ticket });
        Outgoing::plain(text).with_keyboard(vec![vec![apply], vec![cancel]])
    }
}

pub async fn send_card(sender: crate::sender::Sender, key: TopicKey, message: Outgoing) {
    use crate::messenger::Target;
    use hub_core::render::{TELEGRAM_TEXT_LIMIT, split_message};

    let mut chunks = split_message(&message.text, TELEGRAM_TEXT_LIMIT);
    let Some(last) = chunks.pop() else { return };
    for chunk in chunks {
        if sender.one(Target::Topic(key), Outgoing::plain(chunk)).await.is_none() {
            return;
        }
    }
    sender.one(Target::Topic(key), Outgoing::plain(last).with_keyboard(message.keyboard)).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sender::Sender;
    use crate::testing::{Call, FakeMessenger};
    use hub_core::domain::{ChatId, ThreadId, UserId};
    use hub_core::projects::ProjectDraft;
    use hub_core::settings::Draft;
    use std::sync::Arc;

    fn settings() -> Settings {
        Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: std::env::temp_dir().display().to_string(),
            projects: vec![
                ProjectDraft {
                    id: "one".to_owned(),
                    name: "Первый".to_owned(),
                    cwd: "one".to_owned(),
                    backend: "claude".to_owned(),
                    actions: vec![],
                    checks: Vec::new(),
                },
                ProjectDraft {
                    id: "two".to_owned(),
                    name: "Второй".to_owned(),
                    cwd: "two".to_owned(),
                    backend: "codex".to_owned(),
                    actions: vec![],
                    checks: Vec::new(),
                },
            ],
            ..Draft::default()
        }
        .parse(&std::env::temp_dir())
        .unwrap()
    }
    fn key() -> TopicKey {
        TopicKey { chat: ChatId(-100), thread: ThreadId(7) }
    }
    fn press(data: String) -> Press {
        Press {
            callback: "c".to_owned(),
            user: UserId(1),
            chat: Some(ChatId(-100)),
            message: None,
            html: None,
            data,
        }
    }
    fn button(book: &mut Projects, settings: &Settings) -> Press {
        let menu = book.menu(key(), settings, None);
        press(menu.keyboard.first().unwrap().first().unwrap().data.clone())
    }

    #[test]
    fn unauthorized_click_does_not_consume_the_one_shot_choice() {
        let settings = settings();
        let mut book = Projects::new(RecentProjects::default());
        let original = button(&mut book, &settings);
        for invalid in [
            Press { user: UserId(2), ..original.clone() },
            Press { chat: Some(ChatId(-200)), ..original.clone() },
        ] {
            assert!(book.take(&invalid, &settings, &Topics::new()).is_err());
        }
        assert!(book.take(&original, &settings, &Topics::new()).is_ok());
        assert!(book.take(&original, &settings, &Topics::new()).is_err());
    }

    #[test]
    fn changes_to_profile_workspace_or_binding_reject_old_choices() {
        let original = settings();
        for change in 0..3 {
            let mut book = Projects::new(RecentProjects::default());
            let press = button(&mut book, &original);
            let mut settings = original.clone();
            let mut topics = Topics::new();
            match change {
                0 => {
                    settings.projects.clear();
                }
                1 => {
                    settings.workspace_root.push("other");
                }
                _ => {
                    topics.insert(
                        key(),
                        TopicSession::fresh(
                            hub_core::domain::BackendKind::Codex,
                            AbsolutePath::new(std::env::temp_dir()).unwrap(),
                        ),
                    );
                }
            }
            assert!(book.take(&press, &settings, &topics).is_err());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn expired_choices_are_rejected_and_ticket_storage_is_bounded() {
        let settings = settings();
        let mut book = Projects::new(RecentProjects::default());
        let press = button(&mut book, &settings);
        tokio::time::advance(Duration::from_mins(15)).await;
        assert!(book.take(&press, &settings, &Topics::new()).is_err());
        for thread in 1..300 {
            book.menu(TopicKey { thread: ThreadId(thread), ..key() }, &settings, None);
        }
        assert!(book.tickets.len() <= 256);
    }

    #[test]
    fn recent_projects_come_first_and_deleted_ids_are_ignored() {
        let mut recent = RecentProjects::default();
        recent.remember("two");
        recent.remember("removed");
        let mut book = Projects::new(recent);
        let menu = book.menu(key(), &settings(), None);
        let labels: Vec<_> =
            menu.keyboard.iter().flatten().map(|button| button.text.as_str()).collect();
        assert_eq!(labels, ["🕘 Второй · codex", "Первый · claude"]);
    }

    #[tokio::test]
    async fn long_cards_keep_full_details_and_put_buttons_on_the_last_chunk() {
        let messenger = Arc::new(FakeMessenger::default());
        let text = "п".repeat(6000);
        let message = Outgoing::plain(text.clone()).with_keyboard(vec![vec![Button {
            text: "Confirm".to_owned(),
            data: "confirm".to_owned(),
        }]]);
        send_card(Sender::new(messenger.clone()), key(), message).await;
        let sent: Vec<_> = messenger
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Send { message, .. } => Some(message),
                _ => None,
            })
            .collect();
        assert!(sent.len() > 1);
        assert_eq!(sent.iter().map(|message| message.text.as_str()).collect::<String>(), text);
        assert!(sent.iter().all(|message| message.text.chars().count() <= 4096));
        assert!(sent.iter().take(sent.len() - 1).all(|message| message.keyboard.is_empty()));
        assert!(!sent.last().unwrap().keyboard.is_empty());
    }
    #[tokio::test]
    async fn failed_details_delivery_does_not_expose_confirmation_buttons() {
        let messenger = Arc::new(FakeMessenger::with_sends(vec![Err(
            crate::messenger::SendError::Failed("offline".to_owned()),
        )]));
        let message = Outgoing::plain("p".repeat(6000)).with_keyboard(vec![vec![Button {
            text: "Confirm".to_owned(),
            data: "confirm".to_owned(),
        }]]);
        send_card(Sender::new(messenger.clone()), key(), message).await;
        assert_eq!(messenger.send_attempts().len(), 1);
        assert!(messenger.last_keyboard().is_none());
    }
}
