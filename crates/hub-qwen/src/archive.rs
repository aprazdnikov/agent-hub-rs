//! Qwen keeps a session under `projects/<sanitized cwd>/` and loads it only when the first
//! record's `cwd` hashes to the project. Sanitizing is lossy (`/a-b` and `/a_b` share a
//! directory), so the recorded `cwd`, not the directory name, decides what a chat belongs to.

use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use hub_agent::archive::{
    ArchiveError, Entry, Exported, SessionArchive, collect_tree, copy_plain, entry_under,
    export_files, file_stem, open_under, place_all,
};
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};
use serde::de::{Deserialize, Deserializer, MapAccess, Visitor};
use serde_json::Value;
use serde_json::value::RawValue;

const PROJECTS: &str = "projects";
const CHATS: &str = "chats";
const SUBAGENTS: &str = "subagents";
const TODOS: &str = "todos";
const CWD: &str = "cwd";
/// Sidecars that hold the old directory and would point the session back at it.
const STALE: [&str; 2] = [".runtime.json", ".worktree.json"];
/// The first record is read only up to this many bytes.
const FIRST_RECORD_LIMIT: u64 = 1024 * 1024;

/// How Qwen compares working directories: it lowercases them on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    Sensitive,
    Insensitive,
}

impl Case {
    /// The rule of the platform the hub runs on.
    #[must_use]
    pub const fn native() -> Self {
        if cfg!(windows) { Self::Insensitive } else { Self::Sensitive }
    }

    fn fold(self, text: &str) -> String {
        match self {
            Self::Sensitive => text.to_owned(),
            Self::Insensitive => text.to_lowercase(),
        }
    }
}

/// What import does with the `cwd` of the records it copies.
#[derive(Debug, Clone, Copy)]
enum Records<'a> {
    /// The snapshot was taken in this very directory: bytes as they are.
    AsRecorded,
    /// Every top-level string `cwd` becomes this directory.
    MovedTo(&'a str),
}

/// `$QWEN_RUNTIME_DIR`, else `$QWEN_HOME`, else `~/.qwen`; `~` expands as in Qwen, a relative
/// value (Qwen resolves it against its own cwd) is refused.
///
/// # Errors
/// `Unsupported` for a relative or non-UTF-8 value.
pub fn storage_root(
    runtime: Option<&OsStr>,
    qwen_home: Option<&OsStr>,
    home: &Path,
) -> Result<PathBuf, ArchiveError> {
    let Some(raw) = [runtime, qwen_home].into_iter().flatten().find(|raw| !raw.is_empty()) else {
        return Ok(home.join(".qwen"));
    };
    let unsupported = || {
        ArchiveError::Unsupported(
            "QWEN_RUNTIME_DIR/QWEN_HOME: нужен абсолютный путь или ~".to_owned(),
        )
    };
    let raw = raw.to_str().ok_or_else(unsupported)?;
    let path = match raw.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => rest
            .split(['/', '\\'])
            .filter(|part| !part.is_empty())
            .fold(home.to_path_buf(), |path, part| path.join(part)),
        Some(_) | None => PathBuf::from(raw),
    };
    if path.is_absolute() { Ok(path) } else { Err(unsupported()) }
}

/// Qwen's `sanitizeCwd`: lowercase where paths are case-insensitive, then every UTF-16 unit
/// outside `[A-Za-z0-9]` → `-`.
#[must_use]
pub fn sanitize(cwd: &str, case: Case) -> String {
    case.fold(cwd)
        .encode_utf16()
        .map(|unit| match char::from_u32(u32::from(unit)) {
            Some(c) if c.is_ascii_alphanumeric() => c,
            Some(_) | None => '-',
        })
        .collect()
}

/// Qwen compares project roots by `sha256(cwd)`, lowercased on Windows.
fn same_dir(recorded: &str, cwd: &str, case: Case) -> bool {
    case.fold(recorded) == case.fold(cwd)
}

/// A JSON object's members in written order, each value as its exact text, so a rewrite
/// keeps key order, number precision and duplicates as Qwen wrote them.
struct Members<'a>(Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for Members<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Object;

        impl<'de> Visitor<'de> for Object {
            type Value = Members<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut members = Vec::new();
                while let Some(member) = map.next_entry()? {
                    members.push(member);
                }
                Ok(Members(members))
            }
        }

        deserializer.deserialize_map(Object)
    }
}

/// The record with every top-level string `cwd` replaced and the rest kept verbatim; a line
/// that is not a JSON object with such a `cwd` is returned as written.
fn rewrite_cwd(line: &str, cwd: &str) -> String {
    let Ok(Members(members)) = serde_json::from_str::<Members<'_>>(line) else {
        return line.to_owned();
    };
    let is_cwd = |key: &str, value: &RawValue| key == CWD && value.get().starts_with('"');
    if !members.iter().any(|(key, value)| is_cwd(key, value)) {
        return line.to_owned();
    }
    let new = Value::from(cwd).to_string();
    let body: Vec<String> = members
        .iter()
        .map(|(key, value)| {
            let text = if is_cwd(key, value) { new.as_str() } else { value.get() };
            format!("{}:{text}", Value::from(key.as_str()))
        })
        .collect();
    format!("{{{}}}", body.join(","))
}

/// Copies the records line by line with `cwd` rewritten; line breaks (`\n`, `\r\n`, none at
/// the end) stay as they were. A line that is not UTF-8 fails the copy.
fn rewrite_lines(from: File, out: &mut File, cwd: &str) -> io::Result<()> {
    let mut reader = BufReader::new(from);
    let mut line = String::new();
    while reader.read_line(&mut line)? > 0 {
        let record = line.trim_end_matches(['\n', '\r']);
        let (record, end) = line.split_at(record.len());
        write!(out, "{}{end}", rewrite_cwd(record, cwd))?;
        line.clear();
    }
    Ok(())
}

/// `sessionId` and `cwd` of the chat's first record.
fn first_record(root: &Path, chat: &RelPath) -> Result<(String, String), ArchiveError> {
    let mut line = String::new();
    BufReader::new(open_under(root, chat)?.take(FIRST_RECORD_LIMIT)).read_line(&mut line)?;
    let record: Value = serde_json::from_str(&line)
        .map_err(|_| ArchiveError::Malformed(format!("{chat}: первая запись не JSON")))?;
    let field = |name: &str| record.get(name).and_then(Value::as_str).map(str::to_owned);
    match (field("sessionId"), field(CWD)) {
        (Some(id), Some(cwd)) => Ok((id, cwd)),
        (None, _) | (_, None) => {
            Err(ArchiveError::Malformed(format!("{chat}: в первой записи нет sessionId или cwd")))
        }
    }
}

fn cwd_text(cwd: &AbsolutePath) -> Result<&str, ArchiveError> {
    cwd.as_path()
        .to_str()
        .ok_or_else(|| ArchiveError::Unsupported("рабочий каталог не в UTF-8".to_owned()))
}

fn chat_of(project: &str, stem: &str) -> Result<RelPath, ArchiveError> {
    Ok(RelPath::from_parts(&[PROJECTS, project, CHATS, &format!("{stem}.jsonl")])?)
}

fn is_records(file: &RelPath) -> bool {
    Path::new(file.as_str()).extension() == Some(OsStr::new("jsonl"))
}

/// The refusal for a path that is there but is not a regular file.
fn not_a_file(chat: &RelPath, entry: Entry) -> ArchiveError {
    match entry {
        Entry::Link => ArchiveError::Link(chat.to_string()),
        Entry::Missing | Entry::File | Entry::Dir | Entry::Other => {
            ArchiveError::Malformed(format!("{chat}: не обычный файл"))
        }
    }
}

pub struct QwenArchive {
    root: PathBuf,
    version: String,
}

impl QwenArchive {
    #[must_use]
    pub fn new(root: PathBuf, version: String) -> Self {
        Self { root, version }
    }

    /// The chat and its sidecars `<stem>.*`, minus the stale ones; regular files only.
    fn chat_files(&self, project: &str, stem: &str) -> Result<Vec<RelPath>, ArchiveError> {
        let chats = RelPath::from_parts(&[PROJECTS, project, CHATS])?;
        let prefix = format!("{stem}.");
        let mut found = Vec::new();
        for entry in fs::read_dir(chats.under(&self.root))? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            let wanted =
                name.starts_with(&prefix) && !STALE.iter().any(|stale| name.ends_with(stale));
            // `DirEntry::file_type` does not follow links.
            if wanted && entry.file_type()?.is_file() {
                found.push(RelPath::from_parts(&[PROJECTS, project, CHATS, &name])?);
            }
        }
        found.sort();
        Ok(found)
    }
}

impl SessionArchive for QwenArchive {
    fn export(
        &self,
        session: &SessionId,
        cwd: &AbsolutePath,
        dest: &Path,
    ) -> Result<Exported, ArchiveError> {
        let stem = file_stem(session)?;
        let project = sanitize(cwd_text(cwd)?, Case::native());
        let chat = chat_of(&project, stem)?;
        match entry_under(&self.root, &chat)? {
            Entry::File => {}
            Entry::Missing => return Err(ArchiveError::NotFound),
            entry @ (Entry::Dir | Entry::Link | Entry::Other) => {
                return Err(not_a_file(&chat, entry));
            }
        }
        let todo = RelPath::from_parts(&[TODOS, &format!("{stem}.json")])?;
        let todos =
            if entry_under(&self.root, &todo)? == Entry::File { vec![todo] } else { Vec::new() };
        let subagents = RelPath::from_parts(&[PROJECTS, &project, SUBAGENTS, stem])?;
        let files: Vec<RelPath> = self
            .chat_files(&project, stem)?
            .into_iter()
            .chain(collect_tree(&self.root, &subagents)?)
            .chain(todos)
            .collect();
        export_files(&self.root, &files, dest)?;
        Ok(Exported { files, data: BackendData::Qwen, agent_version: self.version.clone() })
    }

    /// Resumable here only if the chat's first record names this session and this very cwd.
    fn present(
        &self,
        session: &SessionId,
        cwd: &AbsolutePath,
        _data: &BackendData,
    ) -> Result<bool, ArchiveError> {
        let stem = file_stem(session)?;
        let target = cwd_text(cwd)?;
        let chat = chat_of(&sanitize(target, Case::native()), stem)?;
        match entry_under(&self.root, &chat)? {
            Entry::Missing => Ok(false),
            Entry::File => {
                let (recorded_id, recorded_cwd) = first_record(&self.root, &chat)?;
                if recorded_id.eq_ignore_ascii_case(stem)
                    && same_dir(&recorded_cwd, target, Case::native())
                {
                    Ok(true)
                } else {
                    Err(ArchiveError::Occupied { id: stem.to_owned(), cwd: recorded_cwd })
                }
            }
            entry @ (Entry::Dir | Entry::Link | Entry::Other) => Err(not_a_file(&chat, entry)),
        }
    }

    fn import(
        &self,
        session: &SessionId,
        data: &BackendData,
        files: &[RelPath],
        src: &Path,
        cwd: &AbsolutePath,
    ) -> Result<(), ArchiveError> {
        match data {
            BackendData::Qwen => {}
            BackendData::Claude { .. } | BackendData::Codex | BackendData::Hermes { .. } => {
                return Err(ArchiveError::Malformed("снимок другого бэкенда".to_owned()));
            }
        }
        let stem = file_stem(session)?;
        let target = cwd_text(cwd)?;
        let new = sanitize(target, Case::native());
        let (todo, prefix) = (format!("{stem}.json"), format!("{stem}."));
        let foreign =
            |file: &RelPath| ArchiveError::Malformed(format!("{file}: не файл этой сессии Qwen"));
        let mut old: Option<&str> = None;
        let mut pairs = Vec::new();
        for file in files {
            let parts: Vec<&str> = file.parts().collect();
            let (project, target_path) = match parts.as_slice() {
                [TODOS, name] if *name == todo => {
                    // Keyed by the id alone, outside any project: an existing one is the same
                    // session's and stays as it is.
                    match entry_under(&self.root, file)? {
                        Entry::Missing => (None, file.clone()),
                        Entry::File => continue,
                        entry @ (Entry::Dir | Entry::Link | Entry::Other) => {
                            return Err(not_a_file(file, entry));
                        }
                    }
                }
                [PROJECTS, project, CHATS, name]
                    if name.starts_with(&prefix)
                        && !STALE.iter().any(|stale| name.ends_with(stale)) =>
                {
                    (Some(*project), RelPath::from_parts(&[PROJECTS, &new, CHATS, name])?)
                }
                [PROJECTS, project, SUBAGENTS, id, rest @ ..]
                    if *id == stem && !rest.is_empty() =>
                {
                    let tail: Vec<&str> = [PROJECTS, new.as_str(), SUBAGENTS, stem]
                        .into_iter()
                        .chain(rest.iter().copied())
                        .collect();
                    (Some(*project), RelPath::from_parts(&tail)?)
                }
                _ => return Err(foreign(file)),
            };
            match (project, old) {
                (None, _) => {}
                (Some(project), None) => old = Some(project),
                (Some(project), Some(seen)) if project == seen => {}
                (Some(_), Some(_)) => return Err(foreign(file)),
            }
            pairs.push((file.clone(), target_path));
        }
        let chat = old.map(|project| chat_of(project, stem)).transpose()?;
        let Some(chat) = chat.filter(|chat| files.contains(chat)) else {
            return Err(ArchiveError::Malformed("в снимке нет чата Qwen".to_owned()));
        };
        let (recorded_id, recorded_cwd) = first_record(src, &chat)?;
        if !recorded_id.eq_ignore_ascii_case(stem) {
            return Err(ArchiveError::Malformed("чат снимка принадлежит другой сессии".to_owned()));
        }
        // One chat per id and project: never a second copy beside the one Qwen already has.
        if self.present(session, cwd, data)? {
            return Err(ArchiveError::Ambiguous);
        }
        let records = if same_dir(&recorded_cwd, target, Case::native()) {
            Records::AsRecorded
        } else {
            Records::MovedTo(target)
        };
        let presence = chat_of(&new, stem)?;
        place_all(src, &self.root, &pairs, &presence, |file, from, out| match records {
            Records::MovedTo(cwd) if is_records(file) => rewrite_lines(from, out, cwd),
            Records::AsRecorded | Records::MovedTo(_) => copy_plain(file, from, out),
        })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::unix("/home/me/shop", Case::Sensitive, "-home-me-shop")]
    #[case::windows(r"C:\Work\Shop", Case::Insensitive, "c--work-shop")]
    #[case::cyrillic("/tmp/проект", Case::Sensitive, "-tmp-------")]
    #[case::astral("/a😀", Case::Sensitive, "-a--")]
    #[case::dash_and_underscore_collide("/work/a_b", Case::Sensitive, "-work-a-b")]
    fn cwd_is_sanitized_like_qwen(#[case] cwd: &str, #[case] case: Case, #[case] expected: &str) {
        assert_eq!(sanitize(cwd, case), expected);
    }

    #[rstest]
    #[case::same("/work/a-b", "/work/a-b", Case::Sensitive, true)]
    #[case::collision("/work/a-b", "/work/a_b", Case::Sensitive, false)]
    #[case::case_on_unix("/Work", "/work", Case::Sensitive, false)]
    #[case::case_on_windows(r"C:\Work", r"c:\work", Case::Insensitive, true)]
    fn directories_are_compared_like_qwen(
        #[case] recorded: &str,
        #[case] cwd: &str,
        #[case] case: Case,
        #[case] expected: bool,
    ) {
        assert_eq!(same_dir(recorded, cwd, case), expected);
    }

    #[rstest]
    #[case::rewritten(r#"{"cwd":"/old","uuid":"u"}"#, r#"{"cwd":"/new","uuid":"u"}"#)]
    #[case::order_and_numbers_kept(
        r#"{"uuid":"u","cwd": "/old","n":0.30000000000000004,"big":123456789012345678901234567890,"m":{"cwd":"/old"}}"#,
        r#"{"uuid":"u","cwd":"/new","n":0.30000000000000004,"big":123456789012345678901234567890,"m":{"cwd":"/old"}}"#
    )]
    #[case::no_cwd(r#"{"uuid":"u"}"#, r#"{"uuid":"u"}"#)]
    #[case::not_a_string(r#"{"cwd":5}"#, r#"{"cwd":5}"#)]
    #[case::not_an_object(r#"["cwd","/old"]"#, r#"["cwd","/old"]"#)]
    #[case::trailing_garbage(r#"{"cwd":"/old"} x"#, r#"{"cwd":"/old"} x"#)]
    #[case::garbage("not json", "not json")]
    #[case::empty("", "")]
    fn only_a_top_level_string_cwd_is_rewritten(#[case] line: &str, #[case] expected: &str) {
        assert_eq!(rewrite_cwd(line, "/new"), expected);
    }

    #[test]
    fn a_target_with_quotes_is_written_as_a_json_string() {
        let line = rewrite_cwd(r#"{"cwd":"/old"}"#, r#"C:\a "b""#);
        assert_eq!(line, r#"{"cwd":"C:\\a \"b\""}"#);
    }

    #[test]
    fn runtime_dir_wins_then_qwen_home_then_the_default() {
        let home = std::env::temp_dir();
        let absolute = home.join("runtime");
        let raw = absolute.as_os_str();
        assert_eq!(storage_root(Some(raw), Some(OsStr::new("~/q")), &home).unwrap(), absolute);
        assert_eq!(
            storage_root(Some(OsStr::new("")), Some(OsStr::new("~/q")), &home).unwrap(),
            home.join("q")
        );
        assert_eq!(storage_root(None, None, &home).unwrap(), home.join(".qwen"));
        assert_eq!(storage_root(None, Some(OsStr::new("~")), &home).unwrap(), home);
        assert!(matches!(
            storage_root(Some(OsStr::new("rel")), None, &home),
            Err(ArchiveError::Unsupported(_))
        ));
        assert!(matches!(
            storage_root(None, Some(OsStr::new("~user/q")), &home),
            Err(ArchiveError::Unsupported(_))
        ));
    }
}
