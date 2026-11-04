//! Saved project defaults; selecting one copies its values into a new topic session.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::actions::Action;
use crate::domain::BackendKind;

pub const MAX_PROJECTS: usize = 32;
const MAX_RECENT: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectDraft {
    pub id: String,
    pub name: String,
    pub cwd: String,
    pub backend: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    id: String,
    name: String,
    cwd: String,
    backend: BackendKind,
    actions: Vec<String>,
}

impl Project {
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub fn cwd(&self) -> &str {
        &self.cwd
    }
    #[must_use]
    pub fn backend(&self) -> BackendKind {
        self.backend
    }
    #[must_use]
    pub fn actions(&self) -> &[String] {
        &self.actions
    }
    #[must_use]
    pub fn to_draft(&self) -> ProjectDraft {
        ProjectDraft {
            id: self.id.clone(),
            name: self.name.clone(),
            cwd: self.cwd.clone(),
            backend: self.backend.name().to_owned(),
            actions: self.actions.clone(),
        }
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub fn parse(drafts: &[ProjectDraft], actions: &[Action]) -> Result<Vec<Project>, String> {
    if drafts.len() > MAX_PROJECTS {
        return Err(format!("Не более {MAX_PROJECTS} проектов"));
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    drafts
        .iter()
        .enumerate()
        .map(|(index, draft)| {
            let error = |reason: &str| format!("Проект {}: {reason}", index + 1);
            let id = draft.id.trim();
            let name = draft.name.trim();
            let cwd = draft.cwd.trim();
            if !valid_id(id) || !ids.insert(id) {
                return Err(error(
                    "ID должен быть уникальным: 1–48 латинских букв, цифр, «-» или «_»",
                ));
            }
            if name.is_empty()
                || name.chars().count() > 80
                || name.chars().any(char::is_control)
                || !names.insert(name)
            {
                return Err(error(
                    "название должно быть уникальным: 1–80 символов без переводов строк",
                ));
            }
            if cwd.is_empty() || cwd.len() > 4096 || cwd.chars().any(char::is_control) {
                return Err(error("укажите каталог, не более 4096 байт, без управляющих символов"));
            }
            let backend =
                BackendKind::parse(&draft.backend).ok_or_else(|| error("неизвестный бэкенд"))?;
            let mut linked = HashSet::new();
            for action in &draft.actions {
                if !linked.insert(action) || !actions.iter().any(|saved| saved.id() == action) {
                    return Err(error("связанное действие отсутствует или указано дважды"));
                }
            }
            Ok(Project {
                id: id.to_owned(),
                name: name.to_owned(),
                cwd: cwd.to_owned(),
                backend,
                actions: draft.actions.clone(),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecentProjects {
    ids: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct RecentFile {
    version: u32,
    ids: Vec<String>,
}

impl RecentProjects {
    #[must_use]
    pub fn ids(&self) -> &[String] {
        &self.ids
    }
    pub fn remember(&mut self, id: &str) {
        self.ids.retain(|saved| saved != id);
        self.ids.insert(0, id.to_owned());
        self.ids.truncate(MAX_RECENT);
    }
    pub fn parse(raw: &str) -> Result<Self, String> {
        let file: RecentFile = serde_json::from_str(raw).map_err(|error| error.to_string())?;
        let mut seen = HashSet::new();
        if file.version != 1
            || file.ids.len() > MAX_RECENT
            || file.ids.iter().any(|id| !valid_id(id) || !seen.insert(id))
        {
            return Err("Некорректная версия или список последних проектов".to_owned());
        }
        Ok(Self { ids: file.ids })
    }
    pub fn dump(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(&RecentFile { version: 1, ids: self.ids.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> ProjectDraft {
        ProjectDraft {
            id: "project-1".to_owned(),
            name: "Бэкенд".to_owned(),
            cwd: "проект с пробелами".to_owned(),
            backend: "codex".to_owned(),
            actions: vec![],
        }
    }

    #[test]
    fn profiles_keep_identity_and_defaults() {
        let projects = parse(&[draft()], &[]).unwrap();
        let project = projects.first().unwrap();
        assert_eq!(project.to_draft(), draft());
        assert_eq!(project.backend(), BackendKind::Codex);
        assert!(parse(&[], &[]).unwrap().is_empty());
    }

    #[test]
    fn invalid_or_duplicate_profiles_are_rejected() {
        for invalid in [
            ProjectDraft { id: "bad id".to_owned(), ..draft() },
            ProjectDraft { name: "\n".to_owned(), ..draft() },
            ProjectDraft { cwd: " ".to_owned(), ..draft() },
            ProjectDraft { backend: "unknown".to_owned(), ..draft() },
            ProjectDraft { actions: vec!["missing".to_owned()], ..draft() },
        ] {
            assert!(parse(&[invalid], &[]).is_err());
        }
        assert!(parse(&[draft(), draft()], &[]).is_err());
        assert!(
            parse(&[draft(), ProjectDraft { id: "other".to_owned(), ..draft() }], &[]).is_err()
        );
        assert!(parse(&vec![draft(); MAX_PROJECTS + 1], &[]).is_err());
    }

    #[test]
    fn recent_projects_are_unique_bounded_and_survive_reload() {
        let mut recent = RecentProjects::default();
        for id in ["a", "b", "c", "d", "e", "f", "c"] {
            recent.remember(id);
        }
        assert_eq!(recent.ids(), ["c", "f", "e", "d", "b"]);
        assert_eq!(RecentProjects::parse(&recent.dump().unwrap()).unwrap(), recent);
        for raw in [
            "bad",
            r#"{"version":2,"ids":[]}"#,
            r#"{"version":1,"ids":["a","a"]}"#,
            r#"{"version":1,"ids":["a","b","c","d","e","f"]}"#,
            r#"{"version":1,"ids":["bad id"]}"#,
        ] {
            assert!(RecentProjects::parse(raw).is_err(), "{raw}");
        }
    }
}
