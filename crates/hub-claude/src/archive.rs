//! Claude Code keeps a session as `projects/<P>/<id>.jsonl` plus `projects/<P>/<id>/`
//! (subagents, tool results). `<P>` encodes the cwd with a hash for long paths, so it is
//! found, not computed; Claude resumes an id from any project.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use hub_agent::archive::{
    ArchiveError, Entry, Exported, SessionArchive, collect_tree, copy_plain, entry_under,
    export_files, file_stem, place_all,
};
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};

const PROJECTS: &str = "projects";

/// `$CLAUDE_CONFIG_DIR`, or `~/.claude` when it is unset or empty.
#[must_use]
pub fn storage_root(config_dir: Option<&OsStr>, home: &Path) -> PathBuf {
    match config_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home.join(".claude"),
    }
}

pub struct ClaudeArchive {
    root: PathBuf,
    version: String,
}

impl ClaudeArchive {
    #[must_use]
    pub fn new(root: PathBuf, version: String) -> Self {
        Self { root, version }
    }

    /// Project directories holding `<stem>.jsonl` as a regular file, sorted; a linked
    /// `projects/` is refused, linked project directories and transcripts are skipped.
    fn projects_with(&self, stem: &str) -> Result<Vec<String>, ArchiveError> {
        let projects = RelPath::parse(PROJECTS)?;
        match entry_under(&self.root, &projects)? {
            Entry::Dir => {}
            Entry::Missing => return Ok(Vec::new()),
            Entry::Link => return Err(ArchiveError::Link(projects.to_string())),
            Entry::File | Entry::Other => {
                return Err(ArchiveError::Malformed(format!("{projects}: не каталог")));
            }
        }
        let mut found = Vec::new();
        for entry in fs::read_dir(projects.under(&self.root))? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            let Ok(transcript) = RelPath::from_parts(&[PROJECTS, &name, &format!("{stem}.jsonl")])
            else {
                continue;
            };
            if entry.file_type()?.is_dir() && entry_under(&self.root, &transcript)? == Entry::File {
                found.push(name);
            }
        }
        found.sort();
        Ok(found)
    }
}

impl SessionArchive for ClaudeArchive {
    fn export(
        &self,
        session: &SessionId,
        _cwd: &AbsolutePath,
        dest: &Path,
    ) -> Result<Exported, ArchiveError> {
        let stem = file_stem(session)?;
        let project = match self.projects_with(stem)?.as_slice() {
            [] => return Err(ArchiveError::NotFound),
            [project] => RelPath::parse(project)?,
            [_, _, ..] => return Err(ArchiveError::Ambiguous),
        };
        let transcript =
            RelPath::from_parts(&[PROJECTS, project.as_str(), &format!("{stem}.jsonl")])?;
        let side = RelPath::from_parts(&[PROJECTS, project.as_str(), stem])?;
        let files: Vec<RelPath> =
            std::iter::once(transcript).chain(collect_tree(&self.root, &side)?).collect();
        export_files(&self.root, &files, dest)?;
        Ok(Exported {
            files,
            data: BackendData::Claude { project },
            agent_version: self.version.clone(),
        })
    }

    fn present(
        &self,
        session: &SessionId,
        _cwd: &AbsolutePath,
        _data: &BackendData,
    ) -> Result<bool, ArchiveError> {
        match self.projects_with(file_stem(session)?)?.as_slice() {
            [] => Ok(false),
            [_] => Ok(true),
            [_, _, ..] => Err(ArchiveError::Ambiguous),
        }
    }

    fn import(
        &self,
        session: &SessionId,
        data: &BackendData,
        files: &[RelPath],
        src: &Path,
        _cwd: &AbsolutePath,
    ) -> Result<(), ArchiveError> {
        let stem = file_stem(session)?;
        let project = match data {
            BackendData::Claude { project } => project.as_str(),
            BackendData::Codex | BackendData::Qwen | BackendData::Hermes { .. } => {
                return Err(ArchiveError::Malformed("снимок другого бэкенда".to_owned()));
            }
        };
        let transcript = format!("{stem}.jsonl");
        let pairs = files
            .iter()
            .map(|file| {
                let parts: Vec<&str> = file.parts().collect();
                let ours = match parts.as_slice() {
                    [PROJECTS, dir, name] => *dir == project && *name == transcript,
                    [PROJECTS, dir, id, _, ..] => *dir == project && *id == stem,
                    _ => false,
                };
                if ours {
                    Ok((file.clone(), file.clone()))
                } else {
                    Err(ArchiveError::Malformed(format!("{file}: не файл этой сессии Claude")))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !files.iter().any(|file| file.parts().next_back() == Some(transcript.as_str())) {
            return Err(ArchiveError::Malformed("в снимке нет транскрипта Claude".to_owned()));
        }
        place_all(src, &self.root, &pairs, copy_plain)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn config_dir_overrides_the_home_default() {
        let home = Path::new("home");
        assert_eq!(storage_root(Some(&OsString::from("custom")), home), PathBuf::from("custom"));
        assert_eq!(storage_root(Some(&OsString::new()), home), home.join(".claude"));
        assert_eq!(storage_root(None, home), home.join(".claude"));
    }
}
