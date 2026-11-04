//! Owner-bound previews for topic parameters; the hub commits validated choices.

use crate::{inbound::Press, messenger::Outgoing, sender::Sender, telegram::allowed};
use hub_core::{
    domain::{BackendKind, ThreadId, TopicKey, TopicSession, UserId},
    options::{Mode, Options as Overrides, Preferences},
    questions::Button,
    settings::Settings,
    topics::Topics,
};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::watch, task::JoinHandle, time::Instant};

pub const PREFIX: &str = "option:";
pub const STALE: &str = "Выбор устарел. Откройте /model или /mode заново.";
const TTL: Duration = Duration::from_mins(15);
const LIMIT: usize = 256;
#[derive(Clone, Copy)]
pub enum Field {
    Model,
    Mode,
}
#[derive(Clone)]
pub struct Context {
    pub key: TopicKey,
    pub user: UserId,
    pub current: Option<TopicSession>,
    pub session: TopicSession,
    pub settings: Arc<Settings>,
}
#[derive(Clone)]
pub enum Action {
    Preview(Preferences),
    Apply(TopicSession),
    Cancel,
}
#[derive(Clone)]
pub struct Ticket {
    pub context: Context,
    pub action: Action,
    created: Instant,
}
#[derive(Default)]
pub struct Reply {
    pub message: Option<Outgoing>,
    pub callback: Option<String>,
    pub answer: Option<String>,
}
#[derive(Default)]
pub struct Choices {
    tickets: HashMap<String, Ticket>,
    deliveries: HashMap<TopicKey, JoinHandle<()>>,
}
impl Choices {
    pub fn available(&mut self, key: TopicKey) -> bool {
        self.deliveries.retain(|_, task| !task.is_finished());
        self.deliveries.len() < 4 && !self.deliveries.contains_key(&key)
    }
    pub fn invalidate(&mut self, key: TopicKey) {
        self.tickets.retain(|_, ticket| ticket.context.key != key);
    }
    fn button(&mut self, text: String, context: Context, action: Action) -> Button {
        self.tickets.retain(|_, ticket| ticket.created.elapsed() < TTL);
        if self.tickets.len() >= LIMIT
            && let Some(oldest) =
                self.tickets.iter().min_by_key(|(_, t)| t.created).map(|(key, _)| key.clone())
        {
            self.tickets.remove(&oldest);
        }
        let data = format!("{PREFIX}{}:{}", context.key.thread.0, uuid::Uuid::new_v4().simple());
        self.tickets.insert(data.clone(), Ticket { context, action, created: Instant::now() });
        Button { text, data }
    }
    pub fn menu(&mut self, context: Context, field: Field, summary: &str) -> Outgoing {
        self.invalidate(context.key);
        let mut text = format!("⚙️ {}\n{summary}\n\n", context.session.backend.name());
        let mut rows = vec![];
        match field {
            Field::Model => text.push_str("/model <ID> — задать модель\n/model inherit — наследовать проект/общие настройки\nДоступность ID проверяет установленный CLI при запуске."),
            Field::Mode => {
                let modes = Mode::all(context.session.backend);
                if modes.is_empty() { text.push_str("Изменение режима для этого бэкенда не поддерживается."); }
                else {
                    text.push_str("Выберите режим; затем проверьте и подтвердите изменение.\n/mode inherit — наследовать проект/общие настройки.");
                    for mode in modes {
                        let mut preferences = context.session.preferences.clone();
                        let mut draft = preferences.overrides.to_draft(); draft.mode = mode.wire();
                        if let Ok(options) = Overrides::parse(context.session.backend, &draft) {
                            preferences.overrides = options;
                            rows.push(vec![self.button(mode.wire(), context.clone(), Action::Preview(preferences))]);
                        }
                    }
                }
            }
        }
        let mut inherit = context.session.preferences.clone();
        let mut draft = inherit.overrides.to_draft();
        match field {
            Field::Model => draft.model.clear(),
            Field::Mode => draft.mode.clear(),
        }
        if let Ok(options) = Overrides::parse(context.session.backend, &draft) {
            inherit.overrides = options;
            if inherit != context.session.preferences {
                rows.push(vec![self.button(
                    "Наследовать".to_owned(),
                    context,
                    Action::Preview(inherit),
                )]);
            }
        }
        Outgoing::plain(text).with_keyboard(rows)
    }
    pub fn change(
        &mut self,
        context: Context,
        field: Field,
        args: &[&str],
    ) -> Result<Outgoing, String> {
        let raw = match (field, args) {
            (_, [value]) => (*value).to_owned(),
            (Field::Mode, [sandbox, approval]) if context.session.backend == BackendKind::Codex => {
                format!("{sandbox}/{approval}")
            }
            _ => {
                return Err("Используйте /model <ID|inherit> или /mode <режим|inherit>.".to_owned());
            }
        };
        let mut preferences = context.session.preferences.clone();
        let mut draft = preferences.overrides.to_draft();
        let value = if raw == "inherit" { String::new() } else { raw };
        match field {
            Field::Model => draft.model = value,
            Field::Mode => draft.mode = value,
        }
        preferences.overrides = Overrides::parse(context.session.backend, &draft)?;
        self.preview(context, preferences)
    }
    pub fn preview(
        &mut self,
        context: Context,
        preferences: Preferences,
    ) -> Result<Outgoing, String> {
        self.invalidate(context.key);
        let effective = preferences.resolve(context.session.backend, &context.settings)?;
        if preferences == context.session.preferences {
            return Ok(Outgoing::plain(format!(
                "Эти настройки уже выбраны.\n{}",
                effective.describe()
            )));
        }
        let previous =
            context.session.preferences.resolve(context.session.backend, &context.settings)?;
        let fresh = context.session.backend == BackendKind::Qwen
            && context.session.session.is_some()
            && (previous.model != effective.model || previous.mode != effective.mode);
        let mut next = context.session.clone();
        next.preferences = preferences;
        if fresh {
            next.session = None;
        }
        let consequence = if fresh {
            "Для Qwen нужна новая сессия. После подтверждения текущий контекст будет сброшен."
        } else if next.session.is_some() {
            "История разговора сохранится. Параметры применятся при следующем запуске."
        } else {
            "Параметры применятся при следующем запуске новой сессии."
        };
        let mut text = format!("⚙️ Новые параметры\n{}\n\n{consequence}", effective.describe());
        if effective.mode.is_some_and(Mode::bypasses_approval) {
            text.push_str("\n⚠️ Этот режим разрешает действия без запросов подтверждения.");
        }
        let apply = self.button(
            if fresh {
                "Применить — новая сессия"
            } else {
                "Применить"
            }
            .to_owned(),
            context.clone(),
            Action::Apply(next),
        );
        let cancel = self.button("Отмена".to_owned(), context, Action::Cancel);
        Ok(Outgoing::plain(text).with_keyboard(vec![vec![apply], vec![cancel]]))
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
        if ticket.context.user != press.user {
            return Err("Подтверждение доступно автору выбора.");
        }
        if ticket.created.elapsed() >= TTL
            || Some(ticket.context.key.chat) != press.chat
            || *ticket.context.settings != *settings
            || topics.get(&ticket.context.key) != ticket.context.current.as_ref()
        {
            return Err(STALE);
        }
        self.tickets.remove(&press.data).ok_or(STALE)
    }
    pub fn emit(
        &mut self,
        key: TopicKey,
        user: UserId,
        mut settings: watch::Receiver<Arc<Settings>>,
        sender: Sender,
        reply: Reply,
    ) {
        if !self.available(key) || !allowed(&settings.borrow().telegram, Some(key.chat), Some(user))
        {
            return;
        }
        let identity = settings.borrow_and_update().telegram.clone();
        let task = tokio::spawn(async move {
            let mut changed = settings.clone();
            let send = async {
                if settings.borrow().telegram != identity {
                    return;
                }
                if let Some(callback) = reply.callback {
                    sender.answer(callback, reply.answer.as_deref()).await;
                }
                if settings.borrow().telegram != identity {
                    return;
                }
                if let Some(message) = reply.message {
                    crate::projects::send_card(sender, key, message).await;
                }
            };
            tokio::select! { biased; _ = changed.changed() => {}, _ = tokio::time::timeout(Duration::from_secs(65), send) => {} }
        });
        self.deliveries.insert(key, task);
    }
    pub fn shutdown(&mut self) {
        for (_, task) in self.deliveries.drain() {
            task.abort();
        }
        self.tickets.clear();
    }
}
impl Drop for Choices {
    fn drop(&mut self) {
        self.shutdown();
    }
}
#[must_use]
pub fn key(press: &Press) -> Option<TopicKey> {
    let (thread, id) = press.data.strip_prefix(PREFIX)?.split_once(':')?;
    let thread = thread.parse::<i32>().ok().filter(|thread| *thread > 0)?;
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(TopicKey { chat: press.chat?, thread: ThreadId(thread) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::{
        domain::{AbsolutePath, BackendKind, ChatId, SessionId, ThreadId},
        settings::Draft,
    };
    fn context(backend: BackendKind) -> Context {
        let root = std::env::temp_dir();
        let settings = Arc::new(
            Draft {
                token: "1:a".to_owned(),
                chat: "-100".to_owned(),
                users: "1 2".to_owned(),
                workspace_root: root.display().to_string(),
                ..Draft::default()
            }
            .parse(&root)
            .unwrap(),
        );
        let session = TopicSession::fresh(backend, AbsolutePath::new(root).unwrap())
            .with_session(SessionId::parse("old"));
        Context {
            key: TopicKey { chat: ChatId(-100), thread: ThreadId(7) },
            user: UserId(1),
            current: Some(session.clone()),
            session,
            settings,
        }
    }
    fn press(context: &Context, data: String) -> Press {
        Press {
            callback: "c".to_owned(),
            user: context.user,
            chat: Some(context.key.chat),
            message: None,
            html: None,
            data,
        }
    }
    #[test]
    fn options_preview_preserves_history_or_explicitly_requests_qwen_reset() {
        let mut choices = Choices::default();
        for backend in BackendKind::ALL {
            let context = context(backend);
            let preview = choices.change(context.clone(), Field::Model, &["new-model"]).unwrap();
            let button = preview
                .keyboard
                .iter()
                .flatten()
                .find(|button| button.text.starts_with("Применить"))
                .unwrap();
            assert_eq!(button.text.contains("новая сессия"), backend == BackendKind::Qwen);
            let ticket = choices
                .take(
                    &press(&context, button.data.clone()),
                    &context.settings,
                    &Topics::from([(context.key, context.session.clone())]),
                )
                .unwrap();
            let Action::Apply(next) = ticket.action else { panic!("apply") };
            assert_eq!(next.session.is_some(), backend != BackendKind::Qwen);
            assert_eq!(next.preferences.overrides.to_draft().model, "new-model");
        }
    }
    #[test]
    fn options_choices_are_owner_bound_one_shot_and_reject_changed_configuration() {
        let mut choices = Choices::default();
        let context = context(BackendKind::Claude);
        let topics = Topics::from([(context.key, context.session.clone())]);
        let preview = choices.change(context.clone(), Field::Model, &["new-model"]).unwrap();
        let data = preview.keyboard[0][0].data.clone();
        let mut foreign = press(&context, data.clone());
        foreign.user = UserId(2);
        assert!(choices.take(&foreign, &context.settings, &topics).is_err());
        let click = press(&context, data);
        assert!(choices.take(&click, &context.settings, &topics).is_ok());
        assert!(choices.take(&click, &context.settings, &topics).is_err());
        let preview = choices.change(context.clone(), Field::Model, &["other"]).unwrap();
        let mut changed = (*context.settings).clone();
        changed.claude.model = Some("changed".to_owned());
        assert!(
            choices
                .take(&press(&context, preview.keyboard[0][0].data.clone()), &changed, &topics)
                .is_err()
        );
    }
    #[test]
    fn options_inherit_changes_source_even_when_the_effective_value_is_equal() {
        for backend in [BackendKind::Claude, BackendKind::Qwen] {
            let mut context = context(backend);
            let settings = Arc::make_mut(&mut context.settings);
            settings.claude.model = Some("model".to_owned());
            settings.qwen.model = Some("model".to_owned());
            context.session.preferences.overrides = Overrides::parse(
                backend,
                &hub_core::options::OptionsDraft { model: "model".to_owned(), mode: String::new() },
            )
            .unwrap();
            context.current = Some(context.session.clone());
            let mut choices = Choices::default();
            let preview = choices.change(context.clone(), Field::Model, &["inherit"]).unwrap();
            assert!(!preview.keyboard.is_empty());
            let ticket = choices
                .take(
                    &press(&context, preview.keyboard[0][0].data.clone()),
                    &context.settings,
                    &Topics::from([(context.key, context.session.clone())]),
                )
                .unwrap();
            let Action::Apply(next) = ticket.action else { panic!("apply") };
            assert!(next.preferences.overrides.to_draft().model.is_empty());
            assert!(next.session.is_some());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn options_tickets_expire_and_remain_bounded_across_topics() {
        let mut choices = Choices::default();
        let original = context(BackendKind::Claude);
        let preview = choices.change(original.clone(), Field::Model, &["model"]).unwrap();
        let click = press(&original, preview.keyboard[0][0].data.clone());
        tokio::time::advance(TTL).await;
        assert!(
            choices
                .take(
                    &click,
                    &original.settings,
                    &Topics::from([(original.key, original.session.clone())])
                )
                .is_err()
        );
        for thread in 1..=300 {
            let mut context = original.clone();
            context.key.thread = ThreadId(thread);
            choices.change(context, Field::Model, &["model"]).unwrap();
        }
        assert!(choices.tickets.len() <= LIMIT);
        assert!(!choices.tickets.contains_key(&click.data));
    }

    #[tokio::test]
    async fn options_delivery_is_bounded_and_cancelled_on_reconfiguration_or_shutdown() {
        let context = context(BackendKind::Claude);
        let (settings, rx) = watch::channel(context.settings.clone());
        let mut choices = Choices::default();
        let mut holds = vec![];
        let mut messengers = vec![];
        for thread in 1..=4 {
            let messenger = Arc::new(crate::testing::FakeMessenger::default());
            holds.push(messenger.delay_next_send());
            let key = TopicKey { thread: ThreadId(thread), ..context.key };
            choices.emit(
                key,
                context.user,
                rx.clone(),
                Sender::new(messenger.clone()),
                Reply { message: Some(Outgoing::plain("menu".to_owned())), ..Reply::default() },
            );
            messengers.push(messenger);
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while messengers.iter().any(|m| m.send_attempts().is_empty()) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!choices.available(context.key));
        settings.send_modify(|settings| {
            Arc::make_mut(settings).claude.model = Some("changed".to_owned());
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            while !choices.available(context.key) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(holds);
        assert!(messengers.iter().all(|m| m.sent_texts().is_empty()));
        let messenger = Arc::new(crate::testing::FakeMessenger::default());
        let hold = messenger.delay_next_send();
        choices.emit(
            context.key,
            context.user,
            rx,
            Sender::new(messenger.clone()),
            Reply { message: Some(Outgoing::plain("menu".to_owned())), ..Reply::default() },
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            while messenger.send_attempts().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        choices.shutdown();
        drop(hold);
        tokio::task::yield_now().await;
        assert!(messenger.sent_texts().is_empty());
        assert!(choices.deliveries.is_empty());
    }
}
