//! A trusted project check and a separate reviewer, selected on the host.
use crate::domain::BackendKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowPreset {
    pub implementer: BackendKind,
    pub command: String,
    pub reviewer: BackendKind,
}
impl WorkflowPreset {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.command.trim().is_empty()
            || self.command.len() > 2048
            || self.command.contains('\0')
        {
            return Err("Введите локальную команду проверки до 2048 байт.".into());
        }
        Ok(())
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
