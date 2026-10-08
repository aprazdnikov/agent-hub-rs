//! Pure presentation decisions shared by the window and the tray.

use hub_codex::protocol::CodexAuth;
use hub_core::domain::BackendKind;
use hub_core::settings::Draft;
use hub_qwen::backend::QwenAuth;
use hub_telegram::hub::TopicState;

use crate::supervisor::{AgentAuth, AgentState, BotStatus, Snapshot};
use crate::updater::UpdateState;

const SHORT_SESSION: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Health {
    Connected,
    Stopped,
    Failed,
}

#[must_use]
pub fn health(status: &BotStatus) -> Health {
    match status {
        BotStatus::Running { .. } => Health::Connected,
        BotStatus::Unconfigured | BotStatus::Stopped | BotStatus::Starting => Health::Stopped,
        BotStatus::Failed(_) => Health::Failed,
    }
}

#[must_use]
pub fn active(snapshot: &Snapshot) -> usize {
    snapshot.topics.iter().filter(|topic| topic.state == TopicState::Running).count()
}

#[must_use]
pub fn tooltip(snapshot: &Snapshot) -> String {
    match &snapshot.bot {
        BotStatus::Running { .. } => {
            format!("agent-hub — подключён · активных сессий: {}", active(snapshot))
        }
        BotStatus::Starting => "agent-hub — подключение…".to_owned(),
        BotStatus::Stopped => "agent-hub — остановлен".to_owned(),
        BotStatus::Unconfigured => "agent-hub — не настроен".to_owned(),
        BotStatus::Failed(_) => "agent-hub — ошибка".to_owned(),
    }
}

#[must_use]
pub fn status_text(status: &BotStatus) -> String {
    match status {
        BotStatus::Unconfigured => "Не настроен: заполните вкладку «Настройки»".to_owned(),
        BotStatus::Stopped => "Остановлен".to_owned(),
        BotStatus::Starting => "Подключение…".to_owned(),
        BotStatus::Running { username } => format!("Подключён как @{username}"),
        BotStatus::Failed(reason) => format!("Ошибка: {reason}"),
    }
}

#[must_use]
pub const fn agent_title(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Claude => "Claude Code",
        BackendKind::Codex => "Codex",
        BackendKind::Qwen => "Qwen Code",
        BackendKind::Hermes => "Hermes",
    }
}

#[must_use]
pub fn agent_line(state: Option<&AgentState>) -> String {
    match state {
        None => "—".to_owned(),
        Some(AgentState::Ready { version, auth: None }) => version.clone(),
        Some(AgentState::Ready { version, auth: Some(auth) }) => {
            format!("{version} · {}", auth_text(*auth))
        }
        Some(AgentState::Unavailable(reason)) => format!("недоступен: {reason}"),
    }
}

const fn auth_text(auth: AgentAuth) -> &'static str {
    match auth {
        AgentAuth::Codex(CodexAuth::ApiKey) => "вход: API-ключ",
        AgentAuth::Codex(CodexAuth::ChatGpt) => "вход: ChatGPT",
        AgentAuth::Codex(CodexAuth::Other) => "вход: другой способ",
        AgentAuth::Codex(CodexAuth::NotRequired) => "вход: не требуется",
        AgentAuth::Codex(CodexAuth::Missing) => "вход: не выполнен — `codex login` или API-ключ",
        AgentAuth::Qwen(QwenAuth::HubKey) => "ключ из настроек хаба",
        AgentAuth::Qwen(QwenAuth::OwnSetup) => "собственная настройка qwen",
        AgentAuth::Hermes => "собственная настройка Hermes (вход не проверен)",
    }
}

#[must_use]
pub fn short_session(id: &str) -> String {
    match id.char_indices().nth(SHORT_SESSION) {
        Some((end, _)) => format!("{}…", id.get(..end).unwrap_or(id)),
        None => id.to_owned(),
    }
}

pub fn use_chat(draft: &mut Draft, chat: i64) {
    draft.chat = chat.to_string();
}

pub fn add_user(draft: &mut Draft, user: u64) {
    let user = user.to_string();
    let present =
        draft.users.split(|c: char| c == ',' || c.is_whitespace()).any(|part| part == user);
    if present {
        return;
    }
    draft.users = if draft.users.trim().is_empty() {
        user
    } else {
        format!("{}, {user}", draft.users.trim_end())
    };
}

/// Saving these restarts the bot and interrupts running sessions.
#[must_use]
pub fn telegram_changed(saved: Option<&Draft>, draft: &Draft) -> bool {
    saved.is_some_and(|saved| {
        saved.token != draft.token || saved.chat != draft.chat || saved.users != draft.users
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerAction {
    Install,
    Restart,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub text: String,
    pub action: Option<BannerAction>,
}

#[must_use]
pub fn banner(state: &UpdateState) -> Option<Banner> {
    let (text, action) = match state {
        UpdateState::Idle => return None,
        UpdateState::Available(version) => {
            (format!("Доступна версия {version}"), Some(BannerAction::Install))
        }
        UpdateState::Installing(version) => (format!("Устанавливается версия {version}…"), None),
        UpdateState::Installed(version) => (
            format!("Версия {version} установлена — перезапустите приложение"),
            Some(BannerAction::Restart),
        ),
        UpdateState::Failed(reason) => {
            (format!("Обновление не установлено: {reason}"), Some(BannerAction::Install))
        }
    };
    Some(Banner { text, action })
}

/// The tray item's text and whether it is clickable (it opens the window).
#[must_use]
pub fn update_item(state: &UpdateState) -> (String, bool) {
    match state {
        UpdateState::Idle | UpdateState::Failed(_) => ("Обновлений нет".to_owned(), false),
        UpdateState::Available(version) | UpdateState::Installing(version) => {
            (format!("Доступно обновление v{version}"), true)
        }
        UpdateState::Installed(version) => (format!("Перезапустите для v{version}"), true),
    }
}

#[cfg(test)]
mod tests {
    use hub_codex::protocol::CodexAuth;
    use hub_core::domain::{AbsolutePath, BackendKind, ChatId, ThreadId, TopicKey, TopicSession};
    use hub_qwen::backend::QwenAuth;
    use hub_telegram::hub::TopicView;
    use rstest::rstest;

    use super::*;
    use crate::supervisor::{AgentAuth, AgentState};

    fn view(thread: i32, state: TopicState) -> TopicView {
        let root = if cfg!(windows) { r"C:\p" } else { "/p" };
        TopicView {
            key: TopicKey { chat: ChatId(-100), thread: ThreadId(thread) },
            title: None,
            session: TopicSession::fresh(
                BackendKind::Claude,
                AbsolutePath::new(root.into()).unwrap(),
            ),
            state,
        }
    }

    #[rstest]
    #[case(BotStatus::Running { username: "b".to_owned() }, Health::Connected)]
    #[case(BotStatus::Starting, Health::Stopped)]
    #[case(BotStatus::Stopped, Health::Stopped)]
    #[case(BotStatus::Unconfigured, Health::Stopped)]
    #[case(BotStatus::Failed("x".to_owned()), Health::Failed)]
    fn status_maps_to_health(#[case] status: BotStatus, #[case] expected: Health) {
        assert_eq!(health(&status), expected);
    }

    #[test]
    fn tooltip_counts_running_sessions() {
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "b".to_owned() },
            topics: vec![view(1, TopicState::Running), view(2, TopicState::Waiting)],
            ..Snapshot::default()
        };
        assert_eq!(tooltip(&snapshot), "agent-hub — подключён · активных сессий: 1");
        assert_eq!(tooltip(&Snapshot::default()), "agent-hub — не настроен");
    }

    #[rstest]
    #[case("abc", "abc")]
    #[case("0123456789abcdef", "01234567…")]
    fn session_ids_are_shortened(#[case] id: &str, #[case] expected: &str) {
        assert_eq!(short_session(id), expected);
    }

    #[rstest]
    #[case("", 5, "5")]
    #[case("1, 2", 5, "1, 2, 5")]
    #[case("1, 5", 5, "1, 5")]
    #[case("15", 5, "15, 5")]
    fn rejected_user_is_added_once(#[case] users: &str, #[case] user: u64, #[case] expected: &str) {
        let mut draft = Draft { users: users.to_owned(), ..Draft::default() };
        add_user(&mut draft, user);
        assert_eq!(draft.users, expected);
    }

    #[test]
    fn rejected_chat_replaces_the_chat() {
        let mut draft = Draft { chat: "-1".to_owned(), ..Draft::default() };
        use_chat(&mut draft, -100);
        assert_eq!(draft.chat, "-100");
    }

    #[rstest]
    #[case(Draft { token: "2:b".to_owned(), ..Draft::default() }, true)]
    #[case(Draft { chat: "-5".to_owned(), ..Draft::default() }, true)]
    #[case(Draft { users: "9".to_owned(), ..Draft::default() }, true)]
    #[case(Draft { model: "opus".to_owned(), ..Draft::default() }, false)]
    fn telegram_fields_are_detected(#[case] draft: Draft, #[case] expected: bool) {
        assert_eq!(telegram_changed(Some(&Draft::default()), &draft), expected);
        assert!(!telegram_changed(None, &draft));
    }

    fn v(raw: &str) -> crate::updates::Version {
        crate::updates::Version::parse(raw).unwrap()
    }

    #[rstest]
    #[case(UpdateState::Idle, None)]
    #[case(UpdateState::Available(v("0.2.0")), Some(("Доступна версия 0.2.0", Some(BannerAction::Install))))]
    #[case(UpdateState::Installing(v("0.2.0")), Some(("Устанавливается версия 0.2.0…", None)))]
    #[case(UpdateState::Installed(v("0.2.0")), Some(("Версия 0.2.0 установлена — перезапустите приложение", Some(BannerAction::Restart))))]
    #[case(UpdateState::Failed("сеть".to_owned()), Some(("Обновление не установлено: сеть", Some(BannerAction::Install))))]
    fn update_banner(
        #[case] state: UpdateState,
        #[case] expected: Option<(&str, Option<BannerAction>)>,
    ) {
        let shown = banner(&state).map(|banner| (banner.text, banner.action));
        assert_eq!(shown, expected.map(|(text, action)| (text.to_owned(), action)));
    }

    #[rstest]
    #[case(UpdateState::Idle, ("Обновлений нет", false))]
    #[case(UpdateState::Available(v("0.2.0")), ("Доступно обновление v0.2.0", true))]
    #[case(UpdateState::Installed(v("0.2.0")), ("Перезапустите для v0.2.0", true))]
    fn update_menu_item(#[case] state: UpdateState, #[case] expected: (&str, bool)) {
        let (text, enabled) = update_item(&state);
        assert_eq!((text.as_str(), enabled), expected);
    }

    #[rstest]
    #[case(None, "—")]
    #[case(Some(AgentState::Ready { version: "2.1.287".to_owned(), auth: None }), "2.1.287")]
    #[case(Some(AgentState::Ready { version: "0.160.0".to_owned(), auth: Some(AgentAuth::Codex(CodexAuth::ChatGpt)) }), "0.160.0 · вход: ChatGPT")]
    #[case(Some(AgentState::Ready { version: "0.160.0".to_owned(), auth: Some(AgentAuth::Codex(CodexAuth::ApiKey)) }), "0.160.0 · вход: API-ключ")]
    #[case(Some(AgentState::Ready { version: "0.160.0".to_owned(), auth: Some(AgentAuth::Codex(CodexAuth::Missing)) }), "0.160.0 · вход: не выполнен — `codex login` или API-ключ")]
    #[case(Some(AgentState::Ready { version: "0.25.0".to_owned(), auth: Some(AgentAuth::Qwen(QwenAuth::HubKey)) }), "0.25.0 · ключ из настроек хаба")]
    #[case(Some(AgentState::Ready { version: "0.25.0".to_owned(), auth: Some(AgentAuth::Qwen(QwenAuth::OwnSetup)) }), "0.25.0 · собственная настройка qwen")]
    #[case(Some(AgentState::Unavailable("Codex не найден".to_owned())), "недоступен: Codex не найден")]
    #[case(Some(AgentState::Ready { version: "0.21.5".to_owned(), auth: Some(AgentAuth::Hermes) }), "0.21.5 · собственная настройка Hermes (вход не проверен)")]
    fn agent_lines(#[case] state: Option<AgentState>, #[case] expected: &str) {
        assert_eq!(agent_line(state.as_ref()), expected);
    }

    #[test]
    fn agents_have_titles() {
        assert_eq!(agent_title(BackendKind::Claude), "Claude Code");
        assert_eq!(agent_title(BackendKind::Codex), "Codex");
        assert_eq!(agent_title(BackendKind::Qwen), "Qwen Code");
        assert_eq!(agent_title(BackendKind::Hermes), "Hermes");
    }
}
