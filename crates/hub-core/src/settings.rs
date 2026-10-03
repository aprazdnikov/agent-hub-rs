//! Settings: the GUI form draft, the TOML file shape, and one parser for both.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::domain::{ChatId, UserId};
use crate::workspace::expand_home;

pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_mins(10);
pub const DEFAULT_BACKGROUND_TIMEOUT: Duration = Duration::from_mins(30);
// Keeps `Instant + timeout` far from overflow; longer waits are not meaningful for a chat.
pub const MAX_TIMEOUT: Duration = Duration::from_hours(24);

#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(***)")
    }
}

/// Non-empty allowlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Users(BTreeSet<UserId>);

impl Users {
    #[must_use]
    pub fn contains(&self, user: UserId) -> bool {
        self.0.contains(&user)
    }

    pub fn iter(&self) -> impl Iterator<Item = UserId> + '_ {
        self.0.iter().copied()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    BypassPermissions,
}

impl PermissionMode {
    pub const ALL: [Self; 4] =
        [Self::Default, Self::AcceptEdits, Self::Plan, Self::BypassPermissions];

    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::BypassPermissions => "bypassPermissions",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.wire() == raw)
    }
}

/// Positive spending cap per task, in USD.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget(Decimal);

impl Budget {
    #[must_use]
    pub fn amount(self) -> Decimal {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateCheck {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TelegramSettings {
    pub token: Token,
    pub chat: ChatId,
    pub users: Users,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSettings {
    pub cli: Option<PathBuf>,
    pub model: Option<String>,
    pub permission_mode: PermissionMode,
    pub budget: Option<Budget>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub approval: Duration,
    pub background: Duration,
}

/// Parsed settings. `workspace_root` is absolute; whether it exists is checked by the shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub telegram: TelegramSettings,
    pub workspace_root: PathBuf,
    pub claude: ClaudeSettings,
    pub timeouts: Timeouts,
    pub updates: UpdateCheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Token,
    Chat,
    Users,
    WorkspaceRoot,
    Cli,
    Model,
    PermissionMode,
    Budget,
    ApprovalTimeout,
    BackgroundTimeout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: Field,
    pub message: String,
}

/// The settings form as typed by the user.
#[derive(Clone, PartialEq, Eq)]
pub struct Draft {
    pub token: String,
    pub chat: String,
    pub users: String,
    pub workspace_root: String,
    pub cli: String,
    pub model: String,
    pub permission_mode: PermissionMode,
    pub budget: String,
    pub approval_timeout: String,
    pub background_timeout: String,
    pub updates: UpdateCheck,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            token: String::new(),
            chat: String::new(),
            users: String::new(),
            workspace_root: String::new(),
            cli: String::new(),
            model: String::new(),
            permission_mode: PermissionMode::Default,
            budget: String::new(),
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT.as_secs().to_string(),
            background_timeout: DEFAULT_BACKGROUND_TIMEOUT.as_secs().to_string(),
            updates: UpdateCheck::Enabled,
        }
    }
}

impl fmt::Debug for Draft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            token: _token,
            chat,
            users,
            workspace_root,
            cli,
            model,
            permission_mode,
            budget,
            approval_timeout,
            background_timeout,
            updates,
        } = self;
        f.debug_struct("Draft")
            .field("token", &"***")
            .field("chat", chat)
            .field("users", users)
            .field("workspace_root", workspace_root)
            .field("cli", cli)
            .field("model", model)
            .field("permission_mode", permission_mode)
            .field("budget", budget)
            .field("approval_timeout", approval_timeout)
            .field("background_timeout", background_timeout)
            .field("updates", updates)
            .finish()
    }
}

impl Draft {
    /// Parses every field and reports all errors at once, in form order.
    pub fn parse(&self, home: &Path) -> Result<Settings, Vec<FieldError>> {
        let mut errors = Vec::new();
        let token = check(&mut errors, Field::Token, parse_token(&self.token));
        let chat = check(&mut errors, Field::Chat, parse_chat(&self.chat));
        let users = check(&mut errors, Field::Users, parse_users(&self.users));
        let root = check(&mut errors, Field::WorkspaceRoot, parse_root(&self.workspace_root, home));
        let cli = check(&mut errors, Field::Cli, parse_cli(&self.cli, home));
        let model = check(&mut errors, Field::Model, parse_model(&self.model));
        let budget = check(&mut errors, Field::Budget, parse_budget(&self.budget));
        let approval =
            check(&mut errors, Field::ApprovalTimeout, parse_seconds(&self.approval_timeout));
        let background =
            check(&mut errors, Field::BackgroundTimeout, parse_seconds(&self.background_timeout));
        let (
            Some(token),
            Some(chat),
            Some(users),
            Some(workspace_root),
            Some(cli),
            Some(model),
            Some(budget),
            Some(approval),
            Some(background),
        ) = (token, chat, users, root, cli, model, budget, approval, background)
        else {
            return Err(errors);
        };
        Ok(Settings {
            telegram: TelegramSettings { token, chat, users },
            workspace_root,
            claude: ClaudeSettings { cli, model, permission_mode: self.permission_mode, budget },
            timeouts: Timeouts { approval, background },
            updates: self.updates,
        })
    }

    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            token: settings.telegram.token.expose().to_owned(),
            chat: settings.telegram.chat.0.to_string(),
            users: settings
                .telegram
                .users
                .iter()
                .map(|user| user.0.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            workspace_root: settings.workspace_root.display().to_string(),
            cli: settings
                .claude
                .cli
                .as_ref()
                .map(|cli| cli.display().to_string())
                .unwrap_or_default(),
            model: settings.claude.model.clone().unwrap_or_default(),
            permission_mode: settings.claude.permission_mode,
            budget: settings
                .claude
                .budget
                .map(|budget| budget.amount().to_string())
                .unwrap_or_default(),
            approval_timeout: settings.timeouts.approval.as_secs().to_string(),
            background_timeout: settings.timeouts.background.as_secs().to_string(),
            updates: settings.updates,
        }
    }
}

fn check<T>(errors: &mut Vec<FieldError>, field: Field, result: Result<T, String>) -> Option<T> {
    result.map_err(|message| errors.push(FieldError { field, message })).ok()
}

fn parse_token(raw: &str) -> Result<Token, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Укажите токен бота от @BotFather".to_owned());
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err("Токен не должен содержать пробелов".to_owned());
    }
    Ok(Token(trimmed.to_owned()))
}

fn parse_chat(raw: &str) -> Result<ChatId, String> {
    raw.trim()
        .parse()
        .map(ChatId)
        .map_err(|_| "Ожидается целое число, например -1001234567890".to_owned())
}

fn parse_users(raw: &str) -> Result<Users, String> {
    let users = raw
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().map(UserId).map_err(|_| format!("«{part}» — не user id")))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if users.is_empty() {
        return Err("Нужен хотя бы один user id".to_owned());
    }
    Ok(Users(users))
}

fn parse_root(raw: &str, home: &Path) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("Укажите корень рабочих директорий".to_owned());
    }
    absolute(trimmed, home)
}

fn parse_cli(raw: &str, home: &Path) -> Result<Option<PathBuf>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() { Ok(None) } else { absolute(trimmed, home).map(Some) }
}

fn absolute(raw: &str, home: &Path) -> Result<PathBuf, String> {
    let path = expand_home(raw, home);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err("Нужен абсолютный путь или путь от ~".to_owned())
    }
}

/// Empty means the model from the operator's own agent settings.
fn parse_model(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.chars().any(char::is_whitespace) {
        return Err("Имя модели не должно содержать пробелов".to_owned());
    }
    Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
}

fn parse_budget(raw: &str) -> Result<Option<Budget>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let amount = Decimal::from_str(trimmed)
        .map_err(|_| "Ожидается число, например 5 или 2.50".to_owned())?;
    if amount <= Decimal::ZERO {
        return Err("Лимит должен быть больше нуля".to_owned());
    }
    Ok(Some(Budget(amount)))
}

fn parse_seconds(raw: &str) -> Result<Duration, String> {
    let max = MAX_TIMEOUT.as_secs();
    match raw.trim().parse::<u64>() {
        Ok(seconds) if (1..=max).contains(&seconds) => Ok(Duration::from_secs(seconds)),
        Ok(_) | Err(_) => Err(format!("Ожидается целое число секунд от 1 до {max}")),
    }
}

/// `settings.toml`; the token is kept in the OS keyring, not here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsFile {
    pub telegram: TelegramFile,
    pub workspace: WorkspaceFile,
    #[serde(default)]
    pub claude: ClaudeFile,
    #[serde(default)]
    pub timeouts: TimeoutsFile,
    #[serde(default)]
    pub updates: UpdatesFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramFile {
    pub chat: i64,
    pub users: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceFile {
    pub root: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeFile {
    pub cli: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub budget: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeoutsFile {
    pub approval_seconds: Option<u64>,
    pub background_seconds: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatesFile {
    pub check: Option<bool>,
}

impl SettingsFile {
    #[must_use]
    pub fn from_settings(settings: &Settings) -> Self {
        Self {
            telegram: TelegramFile {
                chat: settings.telegram.chat.0,
                users: settings.telegram.users.iter().map(|user| user.0).collect(),
            },
            workspace: WorkspaceFile { root: settings.workspace_root.display().to_string() },
            claude: ClaudeFile {
                cli: settings.claude.cli.as_ref().map(|cli| cli.display().to_string()),
                model: settings.claude.model.clone(),
                permission_mode: Some(settings.claude.permission_mode.wire().to_owned()),
                budget: settings.claude.budget.map(|budget| budget.amount().to_string()),
            },
            timeouts: TimeoutsFile {
                approval_seconds: Some(settings.timeouts.approval.as_secs()),
                background_seconds: Some(settings.timeouts.background.as_secs()),
            },
            updates: UpdatesFile {
                check: Some(match settings.updates {
                    UpdateCheck::Enabled => true,
                    UpdateCheck::Disabled => false,
                }),
            },
        }
    }

    /// The file as a form draft, so file and form share one parser.
    pub fn to_draft(&self, token: String) -> Result<Draft, FieldError> {
        let permission_mode = match self.claude.permission_mode.as_deref() {
            None => PermissionMode::Default,
            Some(raw) => PermissionMode::parse(raw).ok_or_else(|| FieldError {
                field: Field::PermissionMode,
                message: format!("Неизвестный режим «{raw}»"),
            })?,
        };
        let defaults = Draft::default();
        Ok(Draft {
            token,
            chat: self.telegram.chat.to_string(),
            users: self.telegram.users.iter().map(u64::to_string).collect::<Vec<_>>().join(", "),
            workspace_root: self.workspace.root.clone(),
            cli: self.claude.cli.clone().unwrap_or_default(),
            model: self.claude.model.clone().unwrap_or_default(),
            permission_mode,
            budget: self.claude.budget.clone().unwrap_or_default(),
            approval_timeout: self
                .timeouts
                .approval_seconds
                .map_or(defaults.approval_timeout, |seconds| seconds.to_string()),
            background_timeout: self
                .timeouts
                .background_seconds
                .map_or(defaults.background_timeout, |seconds| seconds.to_string()),
            updates: match self.updates.check {
                Some(false) => UpdateCheck::Disabled,
                Some(true) | None => UpdateCheck::Enabled,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rstest::rstest;

    use super::*;

    fn home() -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\Users\me" } else { "/home/me" })
    }

    fn draft() -> Draft {
        Draft {
            token: "123:abc".to_owned(),
            chat: "-1001234567890".to_owned(),
            users: "111, 222".to_owned(),
            workspace_root: "~/Projects".to_owned(),
            ..Draft::default()
        }
    }

    #[test]
    fn minimal_draft_uses_defaults() {
        let settings = draft().parse(&home()).unwrap();

        assert_eq!(settings.telegram.token.expose(), "123:abc");
        assert_eq!(settings.telegram.chat, ChatId(-1_001_234_567_890));
        assert_eq!(settings.telegram.users.iter().collect::<Vec<_>>(), [UserId(111), UserId(222)]);
        assert_eq!(settings.workspace_root, home().join("Projects"));
        assert_eq!(settings.timeouts.approval, DEFAULT_APPROVAL_TIMEOUT);
        assert_eq!(settings.timeouts.background, DEFAULT_BACKGROUND_TIMEOUT);
        assert_eq!(settings.claude.permission_mode, PermissionMode::Default);
        assert_eq!(settings.claude.model, None);
        assert_eq!(settings.claude.budget, None);
        assert_eq!(settings.claude.cli, None);
        assert_eq!(settings.updates, UpdateCheck::Enabled);
    }

    #[test]
    fn optional_values_are_parsed() {
        let settings = Draft {
            permission_mode: PermissionMode::AcceptEdits,
            model: " claude-opus-5-5 ".to_owned(),
            budget: "2.50".to_owned(),
            approval_timeout: "30".to_owned(),
            background_timeout: "7200".to_owned(),
            cli: "~/bin/claude".to_owned(),
            updates: UpdateCheck::Disabled,
            ..draft()
        }
        .parse(&home())
        .unwrap();

        assert_eq!(settings.claude.permission_mode, PermissionMode::AcceptEdits);
        assert_eq!(settings.claude.model.as_deref(), Some("claude-opus-5-5"));
        assert_eq!(settings.claude.budget.map(Budget::amount), Some("2.50".parse().unwrap()));
        assert_eq!(settings.timeouts.approval, Duration::from_secs(30));
        assert_eq!(settings.timeouts.background, Duration::from_hours(2));
        assert_eq!(settings.claude.cli, Some(home().join("bin/claude")));
        assert_eq!(settings.updates, UpdateCheck::Disabled);
    }

    #[rstest]
    #[case(Draft { token: String::new(), ..draft() }, Field::Token)]
    #[case(Draft { token: "12 3".to_owned(), ..draft() }, Field::Token)]
    #[case(Draft { chat: "abc".to_owned(), ..draft() }, Field::Chat)]
    #[case(Draft { users: " , ".to_owned(), ..draft() }, Field::Users)]
    #[case(Draft { users: "1, x".to_owned(), ..draft() }, Field::Users)]
    #[case(Draft { workspace_root: "relative".to_owned(), ..draft() }, Field::WorkspaceRoot)]
    #[case(Draft { cli: "claude".to_owned(), ..draft() }, Field::Cli)]
    #[case(Draft { model: "a b".to_owned(), ..draft() }, Field::Model)]
    #[case(Draft { budget: "-1".to_owned(), ..draft() }, Field::Budget)]
    #[case(Draft { budget: "NaN".to_owned(), ..draft() }, Field::Budget)]
    #[case(Draft { approval_timeout: "0".to_owned(), ..draft() }, Field::ApprovalTimeout)]
    #[case(Draft { background_timeout: "-5".to_owned(), ..draft() }, Field::BackgroundTimeout)]
    #[case(Draft { approval_timeout: "86401".to_owned(), ..draft() }, Field::ApprovalTimeout)]
    #[case(Draft { background_timeout: u64::MAX.to_string(), ..draft() }, Field::BackgroundTimeout)]
    fn invalid_values_are_reported_per_field(#[case] draft: Draft, #[case] field: Field) {
        let errors = draft.parse(&home()).unwrap_err();
        assert_eq!(errors.iter().map(|error| error.field).collect::<Vec<_>>(), [field]);
        assert!(errors.iter().all(|error| !error.message.is_empty()));
    }

    #[test]
    fn timeout_of_a_day_is_accepted() {
        let settings = Draft { background_timeout: "86400".to_owned(), ..draft() }.parse(&home());
        assert_eq!(settings.unwrap().timeouts.background, MAX_TIMEOUT);
    }

    #[test]
    fn all_errors_are_reported_at_once() {
        let errors = Draft::default().parse(&home()).unwrap_err();
        let fields: Vec<_> = errors.iter().map(|error| error.field).collect();
        assert_eq!(fields, [Field::Token, Field::Chat, Field::Users, Field::WorkspaceRoot]);
    }

    #[test]
    fn draft_round_trips_through_settings() {
        let settings = Draft { budget: "5".to_owned(), model: "m".to_owned(), ..draft() }
            .parse(&home())
            .unwrap();
        assert_eq!(Draft::from_settings(&settings).parse(&home()).unwrap(), settings);
    }

    #[test]
    fn file_round_trips_through_draft() {
        let settings = draft().parse(&home()).unwrap();
        let file = SettingsFile::from_settings(&settings);
        let restored = file.to_draft(settings.telegram.token.expose().to_owned()).unwrap();
        assert_eq!(restored.parse(&home()).unwrap(), settings);
    }

    #[test]
    fn file_with_unknown_permission_mode_is_rejected() {
        let mut file = SettingsFile::from_settings(&draft().parse(&home()).unwrap());
        file.claude.permission_mode = Some("yolo".to_owned());
        assert_eq!(file.to_draft(String::new()).unwrap_err().field, Field::PermissionMode);
    }

    #[test]
    fn debug_output_hides_token() {
        let settings = draft().parse(&home()).unwrap();
        assert!(!format!("{settings:?}").contains("123:abc"));
        assert!(!format!("{:?}", draft()).contains("123:abc"));
    }

    #[test]
    fn permission_modes_have_cli_names() {
        let names: Vec<_> = PermissionMode::ALL.into_iter().map(PermissionMode::wire).collect();
        assert_eq!(names, ["default", "acceptEdits", "plan", "bypassPermissions"]);
        assert_eq!(PermissionMode::parse("plan"), Some(PermissionMode::Plan));
        assert_eq!(PermissionMode::parse("yolo"), None);
    }
}
