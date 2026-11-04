//! Per-topic model and execution mode, without credentials or transport configuration.

use crate::domain::BackendKind;
use crate::settings::{Approval, PermissionMode, QwenApproval, Sandbox, Settings};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OptionsDraft {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mode: String,
}
impl OptionsDraft {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.model.is_empty() && self.mode.is_empty()
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    model: Option<String>,
    mode: Option<Mode>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Claude(PermissionMode),
    Codex { sandbox: Sandbox, approval: Approval },
    Qwen(QwenApproval),
}
impl Mode {
    #[must_use]
    pub fn all(backend: BackendKind) -> Vec<Self> {
        match backend {
            BackendKind::Claude => PermissionMode::ALL.into_iter().map(Self::Claude).collect(),
            BackendKind::Codex => Sandbox::ALL
                .into_iter()
                .flat_map(|sandbox| {
                    Approval::ALL.into_iter().map(move |approval| Self::Codex { sandbox, approval })
                })
                .collect(),
            BackendKind::Qwen => QwenApproval::ALL.into_iter().map(Self::Qwen).collect(),
            BackendKind::Hermes | BackendKind::Opencode => vec![],
        }
    }
    #[must_use]
    pub fn wire(self) -> String {
        match self {
            Self::Claude(mode) => mode.wire().to_owned(),
            Self::Codex { sandbox, approval } => format!("{}/{}", sandbox.wire(), approval.wire()),
            Self::Qwen(mode) => mode.wire().to_owned(),
        }
    }
    pub fn parse(backend: BackendKind, raw: &str) -> Result<Option<Self>, String> {
        if raw.trim().is_empty() {
            return Ok(None);
        }
        if raw.len() > 128 {
            return Err("Слишком длинное название режима".to_owned());
        }
        Self::all(backend)
            .into_iter()
            .find(|mode| mode.wire() == raw.trim())
            .map(Some)
            .ok_or_else(|| format!("Режим «{raw}» не поддерживается для {}", backend.name()))
    }
    #[must_use]
    pub fn bypasses_approval(self) -> bool {
        matches!(
            self,
            Self::Claude(PermissionMode::BypassPermissions)
                | Self::Codex { approval: Approval::Never, .. }
                | Self::Qwen(QwenApproval::Yolo)
        )
    }
}
impl Options {
    pub fn parse(backend: BackendKind, draft: &OptionsDraft) -> Result<Self, String> {
        let model = draft.model.trim();
        if model.len() > 256
            || model.starts_with('-')
            || model.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return Err(
                "ID модели: до 256 байт, без пробелов, управляющих символов и начального «-»"
                    .to_owned(),
            );
        }
        Ok(Self {
            model: (!model.is_empty()).then(|| model.to_owned()),
            mode: Mode::parse(backend, &draft.mode)?,
        })
    }
    #[must_use]
    pub fn to_draft(&self) -> OptionsDraft {
        OptionsDraft {
            model: self.model.clone().unwrap_or_default(),
            mode: self.mode.map(Mode::wire).unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Preferences {
    pub overrides: Options,
    project: Option<ProjectOptions>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProjectOptions {
    id: String,
    name: String,
    options: Options,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreferencesDraft {
    #[serde(default)]
    overrides: OptionsDraft,
    project: Option<ProjectDraft>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectDraft {
    id: String,
    name: String,
    options: OptionsDraft,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Topic,
    Project(String),
    Global,
}
impl Source {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Topic => "тема".to_owned(),
            Self::Project(name) => format!("проект «{name}»"),
            Self::Global => "общие настройки".to_owned(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    pub backend: BackendKind,
    pub model: Option<String>,
    pub mode: Option<Mode>,
    pub model_source: Source,
    pub mode_source: Source,
}
impl Preferences {
    #[must_use]
    pub fn for_project(id: &str, name: &str, options: Options) -> Self {
        Self {
            overrides: Options::default(),
            project: Some(ProjectOptions { id: id.to_owned(), name: name.to_owned(), options }),
        }
    }
    pub fn parse(backend: BackendKind, draft: Option<PreferencesDraft>) -> Result<Self, String> {
        let Some(draft) = draft else {
            return Ok(Self::default());
        };
        let overrides = Options::parse(backend, &draft.overrides)?;
        let project = draft
            .project
            .map(|project| {
                if project.id.is_empty()
                    || project.id.len() > 48
                    || !project
                        .id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                    || project.name.trim().is_empty()
                    || project.name.chars().count() > 80
                    || project.name.chars().any(char::is_control)
                {
                    return Err("Некорректный источник настроек проекта".to_owned());
                }
                Ok(ProjectOptions {
                    id: project.id,
                    name: project.name,
                    options: Options::parse(backend, &project.options)?,
                })
            })
            .transpose()?;
        Ok(Self { overrides, project })
    }
    #[must_use]
    pub fn to_draft(&self) -> Option<PreferencesDraft> {
        if *self == Self::default() {
            return None;
        }
        Some(PreferencesDraft {
            overrides: self.overrides.to_draft(),
            project: self.project.as_ref().map(|p| ProjectDraft {
                id: p.id.clone(),
                name: p.name.clone(),
                options: p.options.to_draft(),
            }),
        })
    }
    pub fn resolve(&self, backend: BackendKind, settings: &Settings) -> Result<Effective, String> {
        // Validate the backend even for in-memory values copied across a binding change.
        let supported = Mode::all(backend);
        if [self.overrides.mode, self.project.as_ref().and_then(|p| p.options.mode)]
            .into_iter()
            .flatten()
            .any(|mode| !supported.contains(&mode))
        {
            return Err(format!("Режим настроек принадлежит другому бэкенду: {}", backend.name()));
        }
        Self::parse(backend, self.to_draft())?;
        let (mut model, mut mode) = match backend {
            BackendKind::Claude => {
                (settings.claude.model.clone(), Some(Mode::Claude(settings.claude.permission_mode)))
            }
            BackendKind::Codex => (
                settings.codex.model.clone(),
                Some(Mode::Codex {
                    sandbox: settings.codex.sandbox,
                    approval: settings.codex.approval,
                }),
            ),
            BackendKind::Qwen => {
                (settings.qwen.model.clone(), Some(Mode::Qwen(settings.qwen.approval)))
            }
            BackendKind::Hermes => (settings.hermes.model.clone(), None),
            BackendKind::Opencode => (settings.opencode.model.clone(), None),
        };
        let mut model_source = Source::Global;
        let mut execution_source = Source::Global;
        if let Some(project) = &self.project {
            if let Some(value) = &project.options.model {
                model = Some(value.clone());
                model_source = Source::Project(project.name.clone());
            }
            if let Some(value) = project.options.mode {
                mode = Some(value);
                execution_source = Source::Project(project.name.clone());
            }
        }
        if let Some(value) = &self.overrides.model {
            model = Some(value.clone());
            model_source = Source::Topic;
        }
        if let Some(value) = self.overrides.mode {
            mode = Some(value);
            execution_source = Source::Topic;
        }
        Ok(Effective { backend, model, mode, model_source, mode_source: execution_source })
    }
}
impl Effective {
    pub fn apply(&self, settings: &mut Settings) {
        match self.backend {
            BackendKind::Claude => settings.claude.model.clone_from(&self.model),
            BackendKind::Codex => settings.codex.model.clone_from(&self.model),
            BackendKind::Qwen => settings.qwen.model.clone_from(&self.model),
            BackendKind::Hermes => settings.hermes.model.clone_from(&self.model),
            BackendKind::Opencode => settings.opencode.model.clone_from(&self.model),
        }
        match self.mode {
            Some(Mode::Claude(mode)) => settings.claude.permission_mode = mode,
            Some(Mode::Codex { sandbox, approval }) => {
                settings.codex.sandbox = sandbox;
                settings.codex.approval = approval;
            }
            Some(Mode::Qwen(mode)) => settings.qwen.approval = mode,
            None => {}
        }
    }
    #[must_use]
    pub fn describe(&self) -> String {
        let model =
            self.model.as_deref().unwrap_or("выбор CLI/сохранённой сессии (не подтверждён)");
        let mode = self.mode.map_or_else(
            || match self.backend {
                BackendKind::Hermes => "default (фиксированный)".to_owned(),
                _ => "из конфигурации CLI/сессии".to_owned(),
            },
            Mode::wire,
        );
        format!(
            "Модель: {model} · {}\nРежим: {mode} · {}",
            self.model_source.label(),
            self.mode_source.label()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Draft;
    fn settings() -> Settings {
        let root = std::env::temp_dir();
        let mut draft = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            ..Draft::default()
        };
        draft.codex.model = "global-model".to_owned();
        draft.parse(&root).unwrap()
    }
    fn options(model: &str, mode: &str) -> Options {
        Options::parse(
            BackendKind::Codex,
            &OptionsDraft { model: model.to_owned(), mode: mode.to_owned() },
        )
        .unwrap()
    }
    #[test]
    fn options_resolve_each_field_from_topic_project_then_global() {
        let settings = settings();
        let mut preferences = Preferences::for_project(
            "p",
            "Project",
            options("project-model", "read-only/untrusted"),
        );
        preferences.overrides = options("topic-model", "");
        let effective = preferences.resolve(BackendKind::Codex, &settings).unwrap();
        assert_eq!(effective.model.as_deref(), Some("topic-model"));
        assert_eq!(effective.mode.unwrap().wire(), "read-only/untrusted");
        assert_eq!(effective.model_source, Source::Topic);
        assert_eq!(effective.mode_source, Source::Project("Project".to_owned()));
        preferences.overrides = options("", "workspace-write/on-request");
        let effective = preferences.resolve(BackendKind::Codex, &settings).unwrap();
        assert_eq!(effective.model.as_deref(), Some("project-model"));
        assert_eq!(effective.mode_source, Source::Topic);
        let global = Preferences::default().resolve(BackendKind::Codex, &settings).unwrap();
        assert_eq!(global.model.as_deref(), Some("global-model"));
        let mut frozen = settings.clone();
        effective.apply(&mut frozen);
        assert_eq!(frozen.codex.model.as_deref(), Some("project-model"));
        assert_eq!(frozen.codex.sandbox, Sandbox::WorkspaceWrite);
    }
    #[test]
    fn options_validate_backend_modes_and_bounded_model_ids() {
        for backend in BackendKind::ALL {
            for mode in Mode::all(backend) {
                assert_eq!(Mode::parse(backend, &mode.wire()).unwrap(), Some(mode));
            }
            assert!(
                Options::parse(
                    backend,
                    &OptionsDraft { model: "--bad".to_owned(), mode: String::new() }
                )
                .is_err()
            );
            for model in ["two words".to_owned(), "x\0y".to_owned(), "x".repeat(257)] {
                assert!(
                    Options::parse(backend, &OptionsDraft { model, mode: String::new() }).is_err()
                );
            }
        }
        assert_eq!(Mode::all(BackendKind::Codex).len(), 9);
        assert!(Mode::parse(BackendKind::Hermes, "plan").is_err());
        assert!(Mode::parse(BackendKind::Opencode, "default").is_err());
        assert!(Mode::parse(BackendKind::Qwen, "bypassPermissions").is_err());
        assert!(Mode::parse(BackendKind::Codex, "workspace-write").is_err());
    }
    #[test]
    fn options_reject_cross_backend_modes_even_with_identical_wire_names() {
        let settings = settings();
        let source = Options::parse(
            BackendKind::Claude,
            &OptionsDraft { model: String::new(), mode: "default".to_owned() },
        )
        .unwrap();
        let preferences = Preferences { overrides: source.clone(), ..Preferences::default() };
        assert!(preferences.resolve(BackendKind::Qwen, &settings).is_err());
        let project = Preferences::for_project("p", "Project", source);
        assert!(project.resolve(BackendKind::Qwen, &settings).is_err());
    }

    #[test]
    fn options_preferences_round_trip_without_keys_and_reject_wrong_backend() {
        let mut original =
            Preferences::for_project("p", "Project", options("model", "read-only/on-request"));
        original.overrides = options("topic", "");
        let raw = serde_json::to_string(&original.to_draft()).unwrap();
        let restored =
            Preferences::parse(BackendKind::Codex, serde_json::from_str(&raw).unwrap()).unwrap();
        assert_eq!(restored, original);
        assert!(Preferences::parse(BackendKind::Claude, original.to_draft()).is_err());
        assert!(Preferences::default().to_draft().is_none());
        assert!(!raw.contains("token"));
        assert!(!raw.contains("api_key"));
    }
}
