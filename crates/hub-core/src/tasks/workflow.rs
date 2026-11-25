//! A trusted project check and a separate reviewer, selected on the host.
use crate::domain::BackendKind;
use crate::settings::AgentRoles;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPreset {
    pub implementer: BackendKind,
    pub command: String,
    pub reviewer: BackendKind,
}
impl WorkflowPreset {
    pub(super) fn validate(&self) -> Result<(), String> {
        validate_command(&self.command)
    }
    /// One line naming the agents and the check this preset runs.
    #[must_use]
    pub(super) fn describe(&self) -> String {
        format!(
            "Исправить и проверить для {}: {} → ревью {}.",
            self.implementer.name(),
            self.command,
            self.reviewer.name()
        )
    }
}

fn validate_command(command: &str) -> Result<(), String> {
    if command.trim().is_empty() || command.len() > 2048 || command.contains('\0') {
        return Err("Введите локальную команду проверки до 2048 байт.".into());
    }
    Ok(())
}

/// How a project chooses its check: shared roles, or its own pair of agents.
/// `deny_unknown_fields` keeps a half-filled preset from collapsing into `Inherit`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum WorkflowChoice {
    Inherit { command: String },
    Custom(WorkflowPreset),
}
impl WorkflowChoice {
    #[must_use]
    pub fn command(&self) -> &str {
        match self {
            Self::Inherit { command } => command,
            Self::Custom(preset) => &preset.command,
        }
    }
    pub(super) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Inherit { command } => validate_command(command),
            Self::Custom(preset) => preset.validate(),
        }
    }
}

impl super::ProjectRules {
    /// The preset this project actually runs: inherited roles carry the project's own command.
    #[must_use]
    pub fn effective_workflow<'a>(
        &'a self,
        roles: &'a AgentRoles,
    ) -> Option<Cow<'a, WorkflowPreset>> {
        match &self.workflow {
            None => None,
            Some(WorkflowChoice::Custom(preset)) => Some(Cow::Borrowed(preset)),
            Some(WorkflowChoice::Inherit { command }) => Some(Cow::Owned(WorkflowPreset {
                implementer: roles.implementer,
                command: command.clone(),
                reviewer: roles.reviewer,
            })),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{ProjectRules, TaskBook};

    fn rules(raw: &str) -> Result<ProjectRules, serde_json::Error> {
        serde_json::from_str(&format!(
            r#"{{"revision":1,"enabled":true,"text":"Правила","workflow":{raw}}}"#
        ))
    }

    fn preset() -> WorkflowPreset {
        WorkflowPreset {
            implementer: BackendKind::Codex,
            command: "cargo test".into(),
            reviewer: BackendKind::Claude,
        }
    }

    fn project(workflow: Option<WorkflowChoice>) -> ProjectRules {
        ProjectRules { revision: 1, enabled: false, text: String::new(), git: None, workflow }
    }

    #[test]
    fn workflow_choice_absent_field_is_off_and_stays_unwritten() {
        let rules = serde_json::from_str::<ProjectRules>(
            r#"{"revision":1,"enabled":true,"text":"Правила"}"#,
        )
        .unwrap();
        assert_eq!(rules.workflow, None);
        assert!(!serde_json::to_string(&rules).unwrap().contains("workflow"));
    }

    #[test]
    fn workflow_choice_command_only_object_is_inherit_both_ways() {
        let choice = rules(r#"{"command":"cargo test"}"#).unwrap().workflow.unwrap();
        assert_eq!(choice, WorkflowChoice::Inherit { command: "cargo test".into() });
        assert_eq!(choice.command(), "cargo test");
        let encoded = serde_json::to_string(&choice).unwrap();
        assert_eq!(encoded, r#"{"command":"cargo test"}"#);
        assert_eq!(serde_json::from_str::<WorkflowChoice>(&encoded).unwrap(), choice);
    }

    #[test]
    fn workflow_choice_legacy_preset_object_stays_custom_both_ways() {
        let choice = rules(r#"{"implementer":"codex","command":"cargo test","reviewer":"claude"}"#)
            .unwrap()
            .workflow
            .unwrap();
        assert_eq!(choice, WorkflowChoice::Custom(preset()));
        assert_eq!(choice.command(), "cargo test");
        let encoded = serde_json::to_string(&choice).unwrap();
        for key in ["implementer", "command", "reviewer"] {
            assert!(encoded.contains(key), "{key} lost in {encoded}");
        }
        assert_eq!(serde_json::from_str::<WorkflowChoice>(&encoded).unwrap(), choice);
    }

    #[test]
    fn workflow_choice_denormalized_values_break_the_book() {
        for raw in [
            "{}",
            "42",
            r#""off""#,
            r#"{"implementer":"codex"}"#,
            r#"{"command":"cargo test","reviewer":"claude"}"#,
            r#"{"command":42}"#,
        ] {
            assert!(rules(raw).is_err(), "expected a parse error for workflow {raw}");
            let book = format!(
                // Revision 1+ so the only reason the book can fail is the workflow itself.
                r#"{{"version":1,"next_id":1,"tasks":[],"project_rules":[{{"project":"/tmp","rules":{{"revision":1,"enabled":false,"text":"x","workflow":{raw}}}}}]}}"#
            );
            assert!(TaskBook::decode(book.as_bytes()).is_err(), "expected a broken book for {raw}");
        }
        // Positive control: the bare-command inherit form is a valid book value.
        let ok = r#"{"version":1,"next_id":1,"tasks":[],"project_rules":[{"project":"/tmp","rules":{"revision":1,"enabled":false,"text":"x","workflow":{"command":"cargo test"}}}]}"#;
        assert!(TaskBook::decode(ok.as_bytes()).is_ok(), "a valid inherit form must decode");
    }

    #[test]
    fn effective_workflow_is_the_only_resolution_point() {
        let roles = AgentRoles { implementer: BackendKind::Qwen, reviewer: BackendKind::Hermes };
        assert_eq!(project(None).effective_workflow(&roles), None);
        let own = project(Some(WorkflowChoice::Custom(preset())));
        assert!(matches!(
            own.effective_workflow(&roles),
            Some(Cow::Borrowed(resolved)) if resolved == &preset()
        ));
        let inherited = project(Some(WorkflowChoice::Inherit { command: "cargo test".into() }));
        assert_eq!(
            inherited.effective_workflow(&roles),
            Some(Cow::Owned(WorkflowPreset {
                implementer: BackendKind::Qwen,
                command: "cargo test".into(),
                reviewer: BackendKind::Hermes,
            }))
        );
    }

    #[test]
    fn workflow_choice_describe_resolves_shared_roles() {
        let rules = project(Some(WorkflowChoice::Inherit { command: "cargo test".into() }));
        let shared = AgentRoles { implementer: BackendKind::Qwen, reviewer: BackendKind::Hermes };
        let plain = rules.describe();
        assert!(plain.contains("по общим ролям"), "{plain}");
        assert!(plain.contains("cargo test"), "{plain}");
        let resolved = rules.describe_with_roles(&shared);
        assert!(resolved.contains("для qwen"), "{resolved}");
        assert!(resolved.contains("ревью hermes"), "{resolved}");
        let off = project(None);
        assert!(!off.describe_with_roles(&shared).contains("Исправить и проверить"));
        assert!(!off.describe().contains("Исправить и проверить"));
        // A new shared role shows up in the report without re-saving the project rules.
        let moved = rules.describe_with_roles(&AgentRoles {
            implementer: BackendKind::Opencode,
            reviewer: BackendKind::Claude,
        });
        assert!(moved.contains("для opencode") && moved.contains("ревью claude"), "{moved}");
    }

    #[test]
    fn workflow_choice_validation_follows_the_mode() {
        for command in ["", "   ", "\0", &"x".repeat(2049)] {
            let inherited = project(Some(WorkflowChoice::Inherit { command: command.to_owned() }));
            assert!(
                inherited.validate().is_err(),
                "command {command:?} must be rejected for inheritance"
            );
        }
        assert!(
            project(Some(WorkflowChoice::Inherit { command: "cargo test".into() }))
                .validate()
                .is_ok()
        );
        assert!(project(None).validate().is_ok());
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckResult {
    pub passed: bool,
    pub output: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowReport {
    pub checks: Vec<CheckResult>,
    pub review: String,
}
impl WorkflowReport {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.checks.is_empty() && self.review.is_empty()
    }
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.checks.len() > 2
            || self.checks.iter().any(|c| c.output.len() > 16 * 1024)
            || self.review.len() > super::MAX_RESULT
        {
            return Err("Превышен размер отчёта о проверке".into());
        }
        Ok(())
    }
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts: Vec<_> = self
            .checks
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "Проверка {} · {}\n{}",
                    i + 1,
                    if c.passed { "прошла" } else { "не прошла" },
                    c.output
                )
            })
            .collect();
        if !self.review.is_empty() {
            parts.push(format!("Ревью:\n{}", self.review));
        }
        parts.join("\n\n")
    }
}
