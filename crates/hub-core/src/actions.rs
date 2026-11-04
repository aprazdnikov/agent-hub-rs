//! Locally configured shell actions. Telegram only refers to these saved definitions.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::workspace::expand_home;

pub const MAX_ACTIONS: usize = 32;

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionDraft {
    pub id: String,
    pub name: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cwd: String,
}

impl fmt::Debug for ActionDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActionDraft")
            .field("id", &self.id)
            .field("command", &"***")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Action {
    id: String,
    name: String,
    command: String,
    cwd: PathBuf,
}

impl fmt::Debug for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Action")
            .field("id", &self.id)
            .field("command", &"***")
            .finish_non_exhaustive()
    }
}

impl Action {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub fn command(&self) -> &str {
        &self.command
    }
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    #[must_use]
    pub fn to_draft(&self) -> ActionDraft {
        ActionDraft {
            id: self.id.clone(),
            name: self.name.clone(),
            command: self.command.clone(),
            cwd: self.cwd.display().to_string(),
        }
    }
}

pub fn parse(drafts: &[ActionDraft], home: &Path) -> Result<Vec<Action>, String> {
    if drafts.len() > MAX_ACTIONS {
        return Err(format!("Не более {MAX_ACTIONS} действий"));
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    drafts
        .iter()
        .enumerate()
        .map(|(index, draft)| {
            let error = |reason: &str| format!("Действие {}: {reason}", index + 1);
            let id = draft.id.trim();
            let name = draft.name.trim();
            if id.is_empty()
                || id.len() > 48
                || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(error("ID: 1–48 латинских букв, цифр, «-» или «_»"));
            }
            if !ids.insert(id) {
                return Err(error("ID уже используется"));
            }
            if name.is_empty() || name.chars().count() > 80 || name.chars().any(char::is_control) {
                return Err(error("название: 1–80 символов, без переводов строк"));
            }
            if !names.insert(name) {
                return Err(error("название уже используется"));
            }
            if draft.command.trim().is_empty()
                || draft.command.len() > 16_384
                || draft.command.contains('\0')
            {
                return Err(error("укажите команду, не более 16 КиБ, без NUL"));
            }
            let cwd = if draft.cwd.trim().is_empty() {
                home.to_path_buf()
            } else {
                expand_home(draft.cwd.trim(), home)
            };
            if !cwd.is_absolute() {
                return Err(error("каталог: абсолютный путь или ~/…"));
            }
            Ok(Action {
                id: id.to_owned(),
                name: name.to_owned(),
                command: draft.command.clone(),
                cwd,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> ActionDraft {
        ActionDraft {
            id: "vpn".to_owned(),
            name: "Рабочий VPN".to_owned(),
            command: "echo ready".to_owned(),
            cwd: String::new(),
        }
    }

    #[test]
    fn validates_identity_command_and_directory() {
        let home = std::env::temp_dir();
        for invalid in [
            ActionDraft { id: "bad id".to_owned(), ..draft() },
            ActionDraft { name: "  ".to_owned(), ..draft() },
            ActionDraft { name: "line\nbreak".to_owned(), ..draft() },
            ActionDraft { command: " ".to_owned(), ..draft() },
            ActionDraft { command: "echo\0oops".to_owned(), ..draft() },
            ActionDraft { cwd: "relative".to_owned(), ..draft() },
        ] {
            assert!(parse(&[invalid], &home).is_err());
        }
        assert!(parse(&[draft(), draft()], &home).is_err());
        assert!(
            parse(&[draft(), ActionDraft { id: "other".to_owned(), ..draft() }], &home).is_err()
        );
        assert!(parse(&vec![draft(); MAX_ACTIONS + 1], &home).is_err());
        let actions = parse(&[draft()], &home).unwrap();
        assert_eq!(actions.first().unwrap().cwd(), home);
        let actions = parse(&[ActionDraft { cwd: "~/vpn".to_owned(), ..draft() }], &home).unwrap();
        assert_eq!(actions.first().unwrap().cwd(), home.join("vpn"));
    }
}
