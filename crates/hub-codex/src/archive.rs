//! Codex keeps a thread as `sessions/YYYY/MM/DD/rollout-<timestamp>-<id>[_<n>].jsonl[.zst]`;
//! `thread/resume` finds the file by id and re-creates its `SQLite` row, so only files move.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use hub_agent::archive::{
    ArchiveError, Entry, Exported, SessionArchive, collect_tree, copy_plain, entry_under,
    export_files, file_stem, place_all,
};
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};

const SESSIONS: &str = "sessions";
const ARCHIVED: &str = "archived_sessions";

/// `$CODEX_HOME`, or `~/.codex` when it is unset or empty. A relative value (Codex resolves it
/// against its own cwd, which differs per topic) is refused.
///
/// # Errors
/// `Unsupported` for a relative value.
pub fn storage_root(codex_home: Option<&OsStr>, home: &Path) -> Result<PathBuf, ArchiveError> {
    match codex_home.filter(|dir| !dir.is_empty()).map(PathBuf::from) {
        Some(dir) if dir.is_absolute() => Ok(dir),
        Some(_) => Err(ArchiveError::Unsupported("CODEX_HOME: нужен абсолютный путь".to_owned())),
        None => Ok(home.join(".codex")),
    }
}

/// `rollout-<timestamp>-<id>.jsonl`, with an optional `_<n>` and `.zst`.
fn is_rollout_of(name: &str, id: &str) -> bool {
    let stem = name.strip_suffix(".jsonl.zst").or_else(|| name.strip_suffix(".jsonl"));
    stem.and_then(|stem| stem.strip_prefix("rollout-"))
        .is_some_and(|rest| rest.ends_with(&format!("-{id}")) || rest.contains(&format!("-{id}_")))
}

/// `["YYYY", "MM", "DD"]` from `rollout-YYYY-MM-DDT...`.
fn day_of(name: &str) -> Option<[&str; 3]> {
    let date = name.strip_prefix("rollout-")?.get(..10)?;
    let mut parts = date.split('-');
    let day = [parts.next()?, parts.next()?, parts.next()?];
    let digits =
        |part: &str, len: usize| part.len() == len && part.bytes().all(|b| b.is_ascii_digit());
    let [year, month, date] = day;
    (digits(year, 4) && digits(month, 2) && digits(date, 2)).then_some(day)
}

pub struct CodexArchive {
    root: PathBuf,
    version: String,
}

impl CodexArchive {
    #[must_use]
    pub fn new(root: PathBuf, version: String) -> Self {
        Self { root, version }
    }

    /// Rollouts of `id` below `top`; a linked or non-directory `top` is refused.
    fn rollouts(&self, top: &str, id: &str) -> Result<Vec<RelPath>, ArchiveError> {
        let top = RelPath::parse(top)?;
        match entry_under(&self.root, &top)? {
            Entry::Dir => {}
            Entry::Missing => return Ok(Vec::new()),
            Entry::Link => return Err(ArchiveError::Link(top.to_string())),
            Entry::File | Entry::Other => {
                return Err(ArchiveError::Malformed(format!("{top}: не каталог")));
            }
        }
        Ok(collect_tree(&self.root, &top)?
            .into_iter()
            .filter(|file| file.parts().next_back().is_some_and(|name| is_rollout_of(name, id)))
            .collect())
    }

    /// Live rollouts; none live but some archived is `Archived`.
    fn live(&self, id: &str) -> Result<Vec<RelPath>, ArchiveError> {
        let live = self.rollouts(SESSIONS, id)?;
        if live.is_empty() && !self.rollouts(ARCHIVED, id)?.is_empty() {
            return Err(ArchiveError::Archived(id.to_owned()));
        }
        Ok(live)
    }
}

/// Where the snapshot's `file` goes: `sessions/YYYY/MM/DD/<name>` with the date of the name.
fn target_of(file: &RelPath, id: &str) -> Result<RelPath, ArchiveError> {
    let parts: Vec<&str> = file.parts().collect();
    if let [SESSIONS, year, month, date, name] = parts.as_slice()
        && is_rollout_of(name, id)
        && day_of(name) == Some([*year, *month, *date])
    {
        return Ok(RelPath::from_parts(&[SESSIONS, year, month, date, name])?);
    }
    Err(ArchiveError::Malformed(format!("{file}: не файл этой сессии Codex")))
}

impl SessionArchive for CodexArchive {
    fn export(
        &self,
        session: &SessionId,
        _cwd: &AbsolutePath,
        dest: &Path,
    ) -> Result<Exported, ArchiveError> {
        let id = file_stem(session)?;
        let files = self.live(id)?;
        if files.is_empty() {
            return Err(ArchiveError::NotFound);
        }
        // Only what import can put back: a rollout outside its dated directory would make an
        // unrestorable snapshot.
        files.iter().try_for_each(|file| target_of(file, id).map(drop))?;
        export_files(&self.root, &files, dest)?;
        Ok(Exported { files, data: BackendData::Codex, agent_version: self.version.clone() })
    }

    fn present(
        &self,
        session: &SessionId,
        _cwd: &AbsolutePath,
        _data: &BackendData,
    ) -> Result<bool, ArchiveError> {
        Ok(!self.live(file_stem(session)?)?.is_empty())
    }

    fn import(
        &self,
        session: &SessionId,
        data: &BackendData,
        files: &[RelPath],
        src: &Path,
        _cwd: &AbsolutePath,
    ) -> Result<(), ArchiveError> {
        let id = file_stem(session)?;
        match data {
            BackendData::Codex => {}
            BackendData::Claude { .. } | BackendData::Qwen | BackendData::Hermes { .. } => {
                return Err(ArchiveError::Malformed("снимок другого бэкенда".to_owned()));
            }
        }
        let pairs = files
            .iter()
            .map(|file| Ok((file.clone(), target_of(file, id)?)))
            .collect::<Result<Vec<_>, ArchiveError>>()?;
        let Some((_, presence)) = pairs.last() else {
            return Err(ArchiveError::Malformed("в снимке нет rollout-файла Codex".to_owned()));
        };
        if !self.rollouts(SESSIONS, id)?.is_empty() || !self.rollouts(ARCHIVED, id)?.is_empty() {
            return Err(ArchiveError::Ambiguous);
        }
        place_all(src, &self.root, &pairs, presence, copy_plain)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain("rollout-2026-10-09T15-30-12-abc.jsonl", true)]
    #[case::compressed("rollout-2026-10-09T15-30-12-abc.jsonl.zst", true)]
    #[case::numbered("rollout-2026-10-09T15-30-12-abc_2.jsonl", true)]
    #[case::other_id("rollout-2026-10-09T15-30-12-abcd.jsonl", false)]
    #[case::suffix_id("rollout-2026-10-09T15-30-12-xabc.jsonl", false)]
    #[case::not_rollout("abc.jsonl", false)]
    #[case::wrong_extension("rollout-2026-10-09T15-30-12-abc.json", false)]
    fn rollouts_match_by_id(#[case] name: &str, #[case] expected: bool) {
        assert_eq!(is_rollout_of(name, "abc"), expected);
    }

    #[rstest]
    #[case("rollout-2026-10-09T15-30-12-abc.jsonl", Some(["2026", "10", "09"]))]
    #[case("rollout-26-10-09T15-30-12-abc.jsonl", None)]
    #[case("rollout-x", None)]
    fn rollout_dates_come_from_the_name(#[case] name: &str, #[case] expected: Option<[&str; 3]>) {
        assert_eq!(day_of(name), expected);
    }

    #[test]
    fn codex_home_overrides_the_home_default() {
        let home = std::env::temp_dir();
        let custom = home.join("c");
        assert_eq!(storage_root(Some(custom.as_os_str()), &home).unwrap(), custom);
        assert_eq!(storage_root(Some(OsStr::new("")), &home).unwrap(), home.join(".codex"));
        assert_eq!(storage_root(None, &home).unwrap(), home.join(".codex"));
    }

    #[test]
    fn a_relative_codex_home_is_refused() {
        let home = std::env::temp_dir();
        assert!(matches!(
            storage_root(Some(OsStr::new("c")), &home),
            Err(ArchiveError::Unsupported(_))
        ));
    }
}
