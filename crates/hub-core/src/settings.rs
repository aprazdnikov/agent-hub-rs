//! Settings: the GUI form draft, the TOML file shape, and one parser for both.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::domain::{BackendKind, ChatId, UserId};
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

/// What the OS lets commands started by Codex touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sandbox {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl Sandbox {
    pub const ALL: [Self; 3] = [Self::ReadOnly, Self::WorkspaceWrite, Self::DangerFullAccess];

    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|sandbox| sandbox.wire() == raw)
    }
}

/// When Codex asks the human before acting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    Untrusted,
    OnRequest,
    Never,
}

impl Approval {
    pub const ALL: [Self; 3] = [Self::Untrusted, Self::OnRequest, Self::Never];

    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|approval| approval.wire() == raw)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// A key is one non-empty word; surrounding whitespace is dropped.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        (!trimmed.is_empty() && !trimmed.chars().any(char::is_whitespace))
            .then(|| Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

/// When Qwen asks the human before acting; `auto` (Qwen's own classifier) is not offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QwenApproval {
    Plan,
    Default,
    AutoEdit,
    Yolo,
}

impl QwenApproval {
    pub const ALL: [Self; 4] = [Self::Plan, Self::Default, Self::AutoEdit, Self::Yolo];

    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Default => "default",
            Self::AutoEdit => "auto-edit",
            Self::Yolo => "yolo",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|approval| approval.wire() == raw)
    }
}

/// An OpenAI-compatible endpoint for Qwen; the address and the key exist only together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiEndpoint {
    base_url: String,
    key: ApiKey,
}

impl ApiEndpoint {
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub fn key(&self) -> &ApiKey {
        &self.key
    }

    /// Plain `http://` to another machine sends the key in clear text; the form warns about it.
    #[must_use]
    pub fn is_cleartext_remote(&self) -> bool {
        let Some(rest) = self.base_url.strip_prefix("http://") else {
            return false;
        };
        let authority = rest.split_once('/').map_or(rest, |(authority, _)| authority);
        !["localhost", "127.0.0.1", "[::1]"].into_iter().any(|host| {
            authority == host
                || authority.strip_prefix(host).is_some_and(|port| port.starts_with(':'))
        })
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSettings {
    pub cli: Option<PathBuf>,
    pub model: Option<String>,
    pub sandbox: Sandbox,
    pub approval: Approval,
    /// Used only when Codex has no login yet or is logged in with a key.
    pub api_key: Option<ApiKey>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QwenSettings {
    pub cli: Option<PathBuf>,
    pub model: Option<String>,
    pub approval: QwenApproval,
    /// None: Qwen uses its own setup (`/auth`, `~/.qwen/settings.json`, environment).
    pub endpoint: Option<ApiEndpoint>,
}

/// Overrides only; authentication and provider setup remain with the Hermes operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HermesSettings {
    pub cli: Option<PathBuf>,
    pub profile: Option<String>,
    pub model: Option<String>,
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
    pub default_backend: BackendKind,
    pub claude: ClaudeSettings,
    pub codex: CodexSettings,
    pub qwen: QwenSettings,
    pub hermes: HermesSettings,
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
    DefaultBackend,
    Codex(CodexField),
    Qwen(QwenField),
    Hermes(HermesField),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HermesField {
    Cli,
    Profile,
    Model,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QwenField {
    Cli,
    Model,
    Approval,
    BaseUrl,
    ApiKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexField {
    Cli,
    Model,
    Sandbox,
    Approval,
    ApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    pub field: Field,
    pub message: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct CodexDraft {
    pub cli: String,
    pub model: String,
    pub sandbox: Sandbox,
    pub approval: Approval,
    pub api_key: String,
}

impl Default for CodexDraft {
    fn default() -> Self {
        Self {
            cli: String::new(),
            model: String::new(),
            sandbox: Sandbox::WorkspaceWrite,
            approval: Approval::OnRequest,
            api_key: String::new(),
        }
    }
}

impl fmt::Debug for CodexDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { cli, model, sandbox, approval, api_key: _api_key } = self;
        f.debug_struct("CodexDraft")
            .field("cli", cli)
            .field("model", model)
            .field("sandbox", sandbox)
            .field("approval", approval)
            .field("api_key", &"***")
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct QwenDraft {
    pub cli: String,
    pub model: String,
    pub approval: QwenApproval,
    pub base_url: String,
    pub api_key: String,
}

impl Default for QwenDraft {
    fn default() -> Self {
        Self {
            cli: String::new(),
            model: String::new(),
            approval: QwenApproval::Default,
            base_url: String::new(),
            api_key: String::new(),
        }
    }
}

impl fmt::Debug for QwenDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { cli, model, approval, base_url, api_key: _api_key } = self;
        f.debug_struct("QwenDraft")
            .field("cli", cli)
            .field("model", model)
            .field("approval", approval)
            .field("base_url", base_url)
            .field("api_key", &"***")
            .finish()
    }
}

/// The settings form as typed by the user.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HermesDraft {
    pub cli: String,
    pub profile: String,
    pub model: String,
}

impl HermesDraft {
    fn parse(&self, home: &Path, errors: &mut Vec<FieldError>) -> Option<HermesSettings> {
        let Self { cli, profile, model } = self;
        let cli = check(errors, Field::Hermes(HermesField::Cli), parse_cli(cli, home));
        let profile = check(errors, Field::Hermes(HermesField::Profile), parse_profile(profile));
        let model = check(errors, Field::Hermes(HermesField::Model), parse_model(model));
        Some(HermesSettings { cli: cli?, profile: profile?, model: model? })
    }
}

/// The settings form as typed by the user.
#[derive(Clone, PartialEq, Eq)]
pub struct Draft {
    pub token: String,
    pub chat: String,
    pub users: String,
    pub workspace_root: String,
    pub default_backend: BackendKind,
    pub cli: String,
    pub model: String,
    pub permission_mode: PermissionMode,
    pub budget: String,
    pub codex: CodexDraft,
    pub qwen: QwenDraft,
    pub hermes: HermesDraft,
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
            default_backend: BackendKind::Claude,
            cli: String::new(),
            model: String::new(),
            permission_mode: PermissionMode::Default,
            budget: String::new(),
            codex: CodexDraft::default(),
            qwen: QwenDraft::default(),
            hermes: HermesDraft::default(),
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
            default_backend,
            cli,
            model,
            permission_mode,
            budget,
            codex,
            qwen,
            hermes,
            approval_timeout,
            background_timeout,
            updates,
        } = self;
        f.debug_struct("Draft")
            .field("token", &"***")
            .field("chat", chat)
            .field("users", users)
            .field("workspace_root", workspace_root)
            .field("default_backend", default_backend)
            .field("cli", cli)
            .field("model", model)
            .field("permission_mode", permission_mode)
            .field("budget", budget)
            .field("codex", codex)
            .field("qwen", qwen)
            .field("hermes", hermes)
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
        let codex_cli =
            check(&mut errors, Field::Codex(CodexField::Cli), parse_cli(&self.codex.cli, home));
        let codex_model =
            check(&mut errors, Field::Codex(CodexField::Model), parse_model(&self.codex.model));
        let api_key = check(
            &mut errors,
            Field::Codex(CodexField::ApiKey),
            parse_api_key(&self.codex.api_key),
        );
        let qwen_cli =
            check(&mut errors, Field::Qwen(QwenField::Cli), parse_cli(&self.qwen.cli, home));
        let qwen_model =
            check(&mut errors, Field::Qwen(QwenField::Model), parse_model(&self.qwen.model));
        let endpoint = parse_endpoint(&self.qwen.base_url, &self.qwen.api_key)
            .map_err(|error| errors.push(error))
            .ok();
        let hermes = self.hermes.parse(home, &mut errors);
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
            Some(codex_cli),
            Some(codex_model),
            Some(api_key),
            Some(qwen_cli),
            Some(qwen_model),
            Some(endpoint),
            Some(hermes),
            Some(approval),
            Some(background),
        ) = (
            token,
            chat,
            users,
            root,
            cli,
            model,
            budget,
            codex_cli,
            codex_model,
            api_key,
            qwen_cli,
            qwen_model,
            endpoint,
            hermes,
            approval,
            background,
        )
        else {
            return Err(errors);
        };
        Ok(Settings {
            telegram: TelegramSettings { token, chat, users },
            workspace_root,
            default_backend: self.default_backend,
            claude: ClaudeSettings { cli, model, permission_mode: self.permission_mode, budget },
            codex: CodexSettings {
                cli: codex_cli,
                model: codex_model,
                sandbox: self.codex.sandbox,
                approval: self.codex.approval,
                api_key,
            },
            qwen: QwenSettings {
                cli: qwen_cli,
                model: qwen_model,
                approval: self.qwen.approval,
                endpoint,
            },
            hermes,
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
            default_backend: settings.default_backend,
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
            codex: CodexDraft {
                cli: settings
                    .codex
                    .cli
                    .as_ref()
                    .map(|cli| cli.display().to_string())
                    .unwrap_or_default(),
                model: settings.codex.model.clone().unwrap_or_default(),
                sandbox: settings.codex.sandbox,
                approval: settings.codex.approval,
                api_key: settings
                    .codex
                    .api_key
                    .as_ref()
                    .map(|key| key.expose().to_owned())
                    .unwrap_or_default(),
            },
            qwen: QwenDraft {
                cli: settings
                    .qwen
                    .cli
                    .as_ref()
                    .map(|cli| cli.display().to_string())
                    .unwrap_or_default(),
                model: settings.qwen.model.clone().unwrap_or_default(),
                approval: settings.qwen.approval,
                base_url: settings
                    .qwen
                    .endpoint
                    .as_ref()
                    .map(|endpoint| endpoint.base_url().to_owned())
                    .unwrap_or_default(),
                api_key: settings
                    .qwen
                    .endpoint
                    .as_ref()
                    .map(|endpoint| endpoint.key().expose().to_owned())
                    .unwrap_or_default(),
            },
            hermes: HermesDraft {
                cli: settings
                    .hermes
                    .cli
                    .as_ref()
                    .map(|cli| cli.display().to_string())
                    .unwrap_or_default(),
                profile: settings.hermes.profile.clone().unwrap_or_default(),
                model: settings.hermes.model.clone().unwrap_or_default(),
            },
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

/// Empty means the operator's default Hermes profile.
fn parse_profile(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.chars().any(char::is_whitespace) {
        return Err("Имя профиля не должно содержать пробелов".to_owned());
    }
    Ok((!trimmed.is_empty()).then(|| trimmed.to_owned()))
}

/// Empty means no key: Codex uses its own login.
fn parse_api_key(raw: &str) -> Result<Option<ApiKey>, String> {
    if raw.trim().is_empty() {
        return Ok(None);
    }
    ApiKey::parse(raw).map(Some).ok_or_else(|| "Ключ не должен содержать пробелов".to_owned())
}

const BASE_URL_SHAPE: &str = "Ожидается адрес http:// или https://, например https://dashscope-intl.aliyuncs.com/compatible-mode/v1";

/// Empty means Qwen's own setup; anything else is an `http(s)://` address without spaces.
fn parse_base_url(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let rest = trimmed.strip_prefix("https://").or_else(|| trimmed.strip_prefix("http://"));
    match rest {
        Some(rest) if !rest.is_empty() && !trimmed.chars().any(char::is_whitespace) => {
            Ok(Some(trimmed.to_owned()))
        }
        Some(_) | None => Err(BASE_URL_SHAPE.to_owned()),
    }
}

fn parse_endpoint(base_url: &str, key: &str) -> Result<Option<ApiEndpoint>, FieldError> {
    let error = |field, message: String| FieldError { field: Field::Qwen(field), message };
    let base_url =
        parse_base_url(base_url).map_err(|message| error(QwenField::BaseUrl, message))?;
    let key = parse_api_key(key).map_err(|message| error(QwenField::ApiKey, message))?;
    match (base_url, key) {
        (Some(base_url), Some(key)) => Ok(Some(ApiEndpoint { base_url, key })),
        (None, None) => Ok(None),
        (Some(_), None) => {
            Err(error(QwenField::ApiKey, "Укажите API-ключ для этого адреса".to_owned()))
        }
        (None, Some(_)) => {
            Err(error(QwenField::BaseUrl, "Укажите base URL для этого ключа".to_owned()))
        }
    }
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
    pub agents: AgentsFile,
    #[serde(default)]
    pub claude: ClaudeFile,
    #[serde(default)]
    pub codex: CodexFile,
    #[serde(default)]
    pub qwen: QwenFile,
    #[serde(default)]
    pub hermes: HermesFile,
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
pub struct AgentsFile {
    pub default: Option<String>,
}

/// The API key is kept in the OS keyring, not here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodexFile {
    pub cli: Option<String>,
    pub model: Option<String>,
    pub sandbox: Option<String>,
    pub approval: Option<String>,
}

/// The API key is kept in the OS keyring, not here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QwenFile {
    pub cli: Option<String>,
    pub model: Option<String>,
    pub approval: Option<String>,
    pub base_url: Option<String>,
}

/// Hermes overrides; credentials remain in the operator's own Hermes setup.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HermesFile {
    pub cli: Option<String>,
    pub profile: Option<String>,
    pub model: Option<String>,
}

/// Secrets the file does not hold, read from the keyring by the caller.
pub struct Keys {
    pub token: String,
    pub api_key: String,
    pub qwen_key: String,
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
            agents: AgentsFile { default: Some(settings.default_backend.name().to_owned()) },
            claude: ClaudeFile {
                cli: settings.claude.cli.as_ref().map(|cli| cli.display().to_string()),
                model: settings.claude.model.clone(),
                permission_mode: Some(settings.claude.permission_mode.wire().to_owned()),
                budget: settings.claude.budget.map(|budget| budget.amount().to_string()),
            },
            codex: CodexFile {
                cli: settings.codex.cli.as_ref().map(|cli| cli.display().to_string()),
                model: settings.codex.model.clone(),
                sandbox: Some(settings.codex.sandbox.wire().to_owned()),
                approval: Some(settings.codex.approval.wire().to_owned()),
            },
            qwen: QwenFile {
                cli: settings.qwen.cli.as_ref().map(|cli| cli.display().to_string()),
                model: settings.qwen.model.clone(),
                approval: Some(settings.qwen.approval.wire().to_owned()),
                base_url: settings
                    .qwen
                    .endpoint
                    .as_ref()
                    .map(|endpoint| endpoint.base_url().to_owned()),
            },
            hermes: HermesFile {
                cli: settings.hermes.cli.as_ref().map(|cli| cli.display().to_string()),
                profile: settings.hermes.profile.clone(),
                model: settings.hermes.model.clone(),
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
    pub fn to_draft(&self, keys: Keys) -> Result<Draft, FieldError> {
        let Keys { token, api_key, qwen_key } = keys;
        let permission_mode = choice(
            self.claude.permission_mode.as_deref(),
            PermissionMode::Default,
            PermissionMode::parse,
            Field::PermissionMode,
            "Неизвестный режим",
        )?;
        let default_backend = choice(
            self.agents.default.as_deref(),
            BackendKind::Claude,
            BackendKind::parse,
            Field::DefaultBackend,
            "Неизвестный бэкенд",
        )?;
        let sandbox = choice(
            self.codex.sandbox.as_deref(),
            Sandbox::WorkspaceWrite,
            Sandbox::parse,
            Field::Codex(CodexField::Sandbox),
            "Неизвестная песочница",
        )?;
        let approval = choice(
            self.codex.approval.as_deref(),
            Approval::OnRequest,
            Approval::parse,
            Field::Codex(CodexField::Approval),
            "Неизвестный режим одобрений",
        )?;
        let qwen_approval = choice(
            self.qwen.approval.as_deref(),
            QwenApproval::Default,
            QwenApproval::parse,
            Field::Qwen(QwenField::Approval),
            "Неизвестный режим одобрений",
        )?;
        let defaults = Draft::default();
        Ok(Draft {
            token,
            chat: self.telegram.chat.to_string(),
            users: self.telegram.users.iter().map(u64::to_string).collect::<Vec<_>>().join(", "),
            workspace_root: self.workspace.root.clone(),
            default_backend,
            cli: self.claude.cli.clone().unwrap_or_default(),
            model: self.claude.model.clone().unwrap_or_default(),
            permission_mode,
            budget: self.claude.budget.clone().unwrap_or_default(),
            codex: CodexDraft {
                cli: self.codex.cli.clone().unwrap_or_default(),
                model: self.codex.model.clone().unwrap_or_default(),
                sandbox,
                approval,
                api_key,
            },
            qwen: QwenDraft {
                cli: self.qwen.cli.clone().unwrap_or_default(),
                model: self.qwen.model.clone().unwrap_or_default(),
                approval: qwen_approval,
                base_url: self.qwen.base_url.clone().unwrap_or_default(),
                api_key: qwen_key,
            },
            hermes: HermesDraft {
                cli: self.hermes.cli.clone().unwrap_or_default(),
                profile: self.hermes.profile.clone().unwrap_or_default(),
                model: self.hermes.model.clone().unwrap_or_default(),
            },
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

fn choice<T>(
    raw: Option<&str>,
    default: T,
    parse: fn(&str) -> Option<T>,
    field: Field,
    unknown: &str,
) -> Result<T, FieldError> {
    match raw {
        None => Ok(default),
        Some(raw) => {
            parse(raw).ok_or_else(|| FieldError { field, message: format!("{unknown} «{raw}»") })
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rstest::rstest;

    use super::*;
    use crate::domain::BackendKind;

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
        assert_eq!(settings.default_backend, BackendKind::Claude);
        assert_eq!(settings.codex.cli, None);
        assert_eq!(settings.codex.model, None);
        assert_eq!(settings.codex.sandbox, Sandbox::WorkspaceWrite);
        assert_eq!(settings.codex.approval, Approval::OnRequest);
        assert_eq!(settings.codex.api_key, None);
        assert_eq!(settings.qwen.cli, None);
        assert_eq!(settings.qwen.model, None);
        assert_eq!(settings.qwen.approval, QwenApproval::Default);
        assert_eq!(settings.qwen.endpoint, None);
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
        let keys = Keys {
            token: settings.telegram.token.expose().to_owned(),
            api_key: String::new(),
            qwen_key: String::new(),
        };
        let restored = file.to_draft(keys).unwrap();
        assert_eq!(restored.parse(&home()).unwrap(), settings);
    }

    #[test]
    fn file_with_unknown_permission_mode_is_rejected() {
        let mut file = SettingsFile::from_settings(&draft().parse(&home()).unwrap());
        file.claude.permission_mode = Some("yolo".to_owned());
        let keys = Keys { token: String::new(), api_key: String::new(), qwen_key: String::new() };
        assert_eq!(file.to_draft(keys).unwrap_err().field, Field::PermissionMode);
    }

    #[test]
    fn hermes_file_section_and_default_are_supported() {
        let text = "[telegram]\nchat = -100\nusers = [1]\n\
                    [workspace]\nroot = \"~/w\"\n\
                    [agents]\ndefault = \"hermes\"\n\
                    [hermes]\ncli = \"~/bin/hermes\"\nprofile = \"work\"\nmodel = \"test-model\"\n";
        let file: SettingsFile = toml::from_str(text).unwrap();
        let keys =
            Keys { token: "1:a".to_owned(), api_key: String::new(), qwen_key: String::new() };
        let settings = file.to_draft(keys).unwrap().parse(&home()).unwrap();
        assert_eq!(
            (settings.default_backend.name(), settings.hermes),
            (
                "hermes",
                HermesSettings {
                    cli: Some(home().join("bin/hermes")),
                    profile: Some("work".to_owned()),
                    model: Some("test-model".to_owned()),
                }
            )
        );
    }

    #[test]
    fn hermes_file_rejects_a_spaced_profile() {
        let text = "[telegram]\nchat = -100\nusers = [1]\n\
                    [workspace]\nroot = \"~/w\"\n\
                    [hermes]\nprofile = \"bad profile\"\n";
        let file: SettingsFile = toml::from_str(text).unwrap();
        let keys =
            Keys { token: "1:a".to_owned(), api_key: String::new(), qwen_key: String::new() };
        assert_eq!(
            file.to_draft(keys).unwrap().parse(&home()).unwrap_err(),
            [FieldError {
                field: Field::Hermes(HermesField::Profile),
                message: "Имя профиля не должно содержать пробелов".to_owned(),
            }]
        );
    }

    #[test]
    fn hermes_defaults_preserve_the_operators_setup() {
        assert_eq!(
            draft().parse(&home()).unwrap().hermes,
            HermesSettings { cli: None, profile: None, model: None }
        );
    }

    #[test]
    fn hermes_draft_and_file_round_trip() {
        let input = Draft {
            default_backend: BackendKind::Hermes,
            hermes: HermesDraft {
                cli: " ~/bin/hermes ".to_owned(),
                profile: " work ".to_owned(),
                model: " test-model ".to_owned(),
            },
            ..draft()
        };
        let settings = input.parse(&home()).unwrap();
        let text = toml::to_string(&SettingsFile::from_settings(&settings)).unwrap();
        let file: SettingsFile = toml::from_str(&text).unwrap();
        let keys = Keys { token: input.token, api_key: String::new(), qwen_key: String::new() };
        assert_eq!(
            (
                Draft::from_settings(&settings).parse(&home()).unwrap(),
                file.to_draft(keys).unwrap().parse(&home()).unwrap(),
            ),
            (settings.clone(), settings)
        );
    }

    #[rstest]
    #[case::relative_cli(HermesDraft { cli: "hermes".to_owned(), profile: String::new(), model: String::new() }, HermesField::Cli)]
    #[case::spaced_profile(HermesDraft { cli: String::new(), profile: "bad profile".to_owned(), model: String::new() }, HermesField::Profile)]
    #[case::spaced_model(HermesDraft { cli: String::new(), profile: String::new(), model: "bad model".to_owned() }, HermesField::Model)]
    fn hermes_invalid_values_report_their_field(
        #[case] hermes: HermesDraft,
        #[case] field: HermesField,
    ) {
        let errors = Draft { hermes, ..draft() }.parse(&home()).unwrap_err();
        assert_eq!(
            errors.iter().map(|error| error.field).collect::<Vec<_>>(),
            [Field::Hermes(field)]
        );
    }

    #[test]
    fn hermes_errors_are_aggregated_in_form_order() {
        let errors = Draft {
            hermes: HermesDraft {
                cli: "relative".to_owned(),
                profile: "bad profile".to_owned(),
                model: "bad model".to_owned(),
            },
            ..draft()
        }
        .parse(&home())
        .unwrap_err();
        assert_eq!(
            errors.iter().map(|error| error.field).collect::<Vec<_>>(),
            [
                Field::Hermes(HermesField::Cli),
                Field::Hermes(HermesField::Profile),
                Field::Hermes(HermesField::Model)
            ]
        );
    }

    #[rstest]
    #[case("api_key")]
    #[case("provider")]
    fn hermes_file_rejects_unsupported_fields(#[case] name: &str) {
        let text = format!(
            "[telegram]\nchat = -100\nusers = [1]\n[workspace]\nroot = \"~/w\"\n[hermes]\n{name} = \"x\"\n"
        );
        assert!(toml::from_str::<SettingsFile>(&text).is_err());
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

    fn codex_draft() -> Draft {
        Draft {
            default_backend: BackendKind::Codex,
            codex: CodexDraft {
                cli: "~/bin/codex".to_owned(),
                model: " gpt-5.5-codex ".to_owned(),
                sandbox: Sandbox::ReadOnly,
                approval: Approval::Untrusted,
                api_key: " sk-test ".to_owned(),
            },
            ..draft()
        }
    }

    #[test]
    fn codex_values_are_parsed() {
        let settings = codex_draft().parse(&home()).unwrap();

        assert_eq!(settings.default_backend, BackendKind::Codex);
        assert_eq!(settings.codex.cli, Some(home().join("bin/codex")));
        assert_eq!(settings.codex.model.as_deref(), Some("gpt-5.5-codex"));
        assert_eq!(settings.codex.sandbox, Sandbox::ReadOnly);
        assert_eq!(settings.codex.approval, Approval::Untrusted);
        assert_eq!(settings.codex.api_key.as_ref().map(ApiKey::expose), Some("sk-test"));
    }

    #[rstest]
    #[case(CodexDraft { cli: "codex".to_owned(), ..CodexDraft::default() }, CodexField::Cli)]
    #[case(CodexDraft { model: "a b".to_owned(), ..CodexDraft::default() }, CodexField::Model)]
    #[case(CodexDraft { api_key: "sk a".to_owned(), ..CodexDraft::default() }, CodexField::ApiKey)]
    fn invalid_codex_values_are_reported(#[case] codex: CodexDraft, #[case] field: CodexField) {
        let errors = Draft { codex, ..draft() }.parse(&home()).unwrap_err();
        assert_eq!(
            errors.iter().map(|error| error.field).collect::<Vec<_>>(),
            [Field::Codex(field)]
        );
    }

    #[test]
    fn codex_file_round_trips_through_draft_without_the_key() {
        let settings = codex_draft().parse(&home()).unwrap();
        let file = SettingsFile::from_settings(&settings);
        let keys = Keys {
            token: settings.telegram.token.expose().to_owned(),
            api_key: "sk-test".to_owned(),
            qwen_key: String::new(),
        };
        assert_eq!(file.to_draft(keys).unwrap().parse(&home()).unwrap(), settings);
        assert!(!toml::to_string(&file).unwrap().contains("sk-test"));
    }

    #[test]
    fn old_file_without_agents_and_codex_reads_with_defaults() {
        let file: SettingsFile = toml::from_str(
            "[telegram]
chat = -100
users = [1]

[workspace]
root = \"~/w\"
",
        )
        .unwrap();
        let keys =
            Keys { token: "1:a".to_owned(), api_key: String::new(), qwen_key: String::new() };
        let draft = file.to_draft(keys).unwrap();
        assert_eq!(draft.default_backend, BackendKind::Claude);
        assert_eq!(draft.codex, CodexDraft::default());
        assert_eq!(draft.qwen, QwenDraft::default());
        assert_eq!(draft.hermes, HermesDraft::default());
    }

    #[rstest]
    #[case(
        "[agents]
default = \"gpt\"
",
        Field::DefaultBackend
    )]
    #[case(
        "[codex]
sandbox = \"yolo\"
",
        Field::Codex(CodexField::Sandbox)
    )]
    #[case(
        "[codex]
approval = \"always\"
",
        Field::Codex(CodexField::Approval)
    )]
    #[case(
        "[qwen]
approval = \"auto\"
",
        Field::Qwen(QwenField::Approval)
    )]
    fn file_with_unknown_choice_is_rejected(#[case] extra: &str, #[case] field: Field) {
        let text = format!(
            "[telegram]
chat = -100
users = [1]

[workspace]
root = \"~/w\"

{extra}"
        );
        let file: SettingsFile = toml::from_str(&text).unwrap();
        let keys = Keys { token: String::new(), api_key: String::new(), qwen_key: String::new() };
        assert_eq!(file.to_draft(keys).unwrap_err().field, field);
    }

    fn qwen_draft() -> Draft {
        Draft {
            default_backend: BackendKind::Qwen,
            qwen: QwenDraft {
                cli: "~/bin/qwen".to_owned(),
                model: " qwen3-coder-plus ".to_owned(),
                approval: QwenApproval::AutoEdit,
                base_url: " https://dashscope-intl.aliyuncs.com/compatible-mode/v1 ".to_owned(),
                api_key: " sk-qwen ".to_owned(),
            },
            ..draft()
        }
    }

    #[test]
    fn qwen_values_are_parsed() {
        let settings = qwen_draft().parse(&home()).unwrap();
        assert_eq!(
            (
                settings.default_backend,
                settings.qwen.cli,
                settings.qwen.model.as_deref(),
                settings.qwen.approval,
                settings
                    .qwen
                    .endpoint
                    .as_ref()
                    .map(|e| (e.base_url().to_owned(), e.key().expose().to_owned())),
            ),
            (
                BackendKind::Qwen,
                Some(home().join("bin/qwen")),
                Some("qwen3-coder-plus"),
                QwenApproval::AutoEdit,
                Some((
                    "https://dashscope-intl.aliyuncs.com/compatible-mode/v1".to_owned(),
                    "sk-qwen".to_owned()
                )),
            )
        );
    }

    #[rstest]
    #[case::relative_cli(QwenDraft { cli: "qwen".to_owned(), ..QwenDraft::default() }, QwenField::Cli)]
    #[case::spaced_model(QwenDraft { model: "a b".to_owned(), ..QwenDraft::default() }, QwenField::Model)]
    #[case::not_http(QwenDraft { base_url: "ftp://x".to_owned(), api_key: "k".to_owned(), ..QwenDraft::default() }, QwenField::BaseUrl)]
    #[case::bare_scheme(QwenDraft { base_url: "https://".to_owned(), api_key: "k".to_owned(), ..QwenDraft::default() }, QwenField::BaseUrl)]
    #[case::spaced_key(QwenDraft { base_url: "https://x/v1".to_owned(), api_key: "sk a".to_owned(), ..QwenDraft::default() }, QwenField::ApiKey)]
    #[case::url_without_key(QwenDraft { base_url: "https://x/v1".to_owned(), ..QwenDraft::default() }, QwenField::ApiKey)]
    #[case::key_without_url(QwenDraft { api_key: "sk-q".to_owned(), ..QwenDraft::default() }, QwenField::BaseUrl)]
    fn invalid_qwen_values_are_reported(#[case] qwen: QwenDraft, #[case] field: QwenField) {
        let errors = Draft { qwen, ..draft() }.parse(&home()).unwrap_err();
        assert_eq!(
            errors.iter().map(|error| error.field).collect::<Vec<_>>(),
            [Field::Qwen(field)]
        );
    }

    #[test]
    fn qwen_file_round_trips_through_draft_without_the_key() {
        let settings = qwen_draft().parse(&home()).unwrap();
        let file = SettingsFile::from_settings(&settings);
        let keys = Keys {
            token: settings.telegram.token.expose().to_owned(),
            api_key: String::new(),
            qwen_key: "sk-qwen".to_owned(),
        };
        assert_eq!(file.to_draft(keys).unwrap().parse(&home()).unwrap(), settings);
        let text = toml::to_string(&file).unwrap();
        assert!(!text.contains("sk-qwen") && text.contains("[qwen]"));
    }

    #[test]
    fn debug_output_hides_qwen_key() {
        let settings = qwen_draft().parse(&home()).unwrap();
        assert!(!format!("{settings:?}").contains("sk-qwen"));
        assert!(!format!("{:?}", qwen_draft()).contains("sk-qwen"));
    }

    #[test]
    fn qwen_approvals_have_wire_names() {
        let names: Vec<_> = QwenApproval::ALL.into_iter().map(QwenApproval::wire).collect();
        assert_eq!(names, ["plan", "default", "auto-edit", "yolo"]);
        assert_eq!(QwenApproval::parse("auto"), None);
    }

    #[rstest]
    #[case("https://api.example.com/v1", false)]
    #[case("http://localhost:11434/v1", false)]
    #[case("http://127.0.0.1/v1", false)]
    #[case("http://[::1]:8000", false)]
    #[case("http://192.168.1.5:8000/v1", true)]
    #[case("http://localhost.example.com/v1", true)]
    fn cleartext_to_another_machine_is_detected(#[case] base_url: &str, #[case] expected: bool) {
        let endpoint =
            ApiEndpoint { base_url: base_url.to_owned(), key: ApiKey::parse("k").unwrap() };
        assert_eq!(endpoint.is_cleartext_remote(), expected);
    }

    #[test]
    fn debug_output_hides_api_key() {
        let settings = codex_draft().parse(&home()).unwrap();
        assert!(!format!("{settings:?}").contains("sk-test"));
        assert!(!format!("{:?}", codex_draft()).contains("sk-test"));
    }

    #[test]
    fn codex_choices_have_wire_names() {
        let sandboxes: Vec<_> = Sandbox::ALL.into_iter().map(Sandbox::wire).collect();
        assert_eq!(sandboxes, ["read-only", "workspace-write", "danger-full-access"]);
        let approvals: Vec<_> = Approval::ALL.into_iter().map(Approval::wire).collect();
        assert_eq!(approvals, ["untrusted", "on-request", "never"]);
        assert_eq!(Sandbox::parse("read-only"), Some(Sandbox::ReadOnly));
        assert_eq!(Approval::parse("yolo"), None);
    }
}
