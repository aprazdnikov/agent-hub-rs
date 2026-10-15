//! Session snapshots: names, agent-relative paths and `manifest.json` format version 1.
//! Pure: the store and the archives do the I/O.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::{AbsolutePath, BackendKind, SessionId};

pub const MANIFEST_VERSION: u32 = 1;
pub const NAME_LIMIT: usize = 64;
pub const FILES_LIMIT: usize = 10_000;
/// The largest `manifest.json`, in bytes; `dump` refuses what `parse` would refuse.
pub const MANIFEST_LIMIT: usize = 1024 * 1024;
/// Device names Windows reserves for any extension: such directories cannot be created there.
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("Имя снимка пустое")]
    Empty,
    #[error("Имя снимка длиннее {NAME_LIMIT} символов")]
    TooLong,
    #[error("Имя снимка не может начинаться или заканчиваться точкой")]
    Dot,
    #[error("Имя снимка зарезервировано в Windows")]
    Reserved,
    #[error("В имени снимка допустимы a-z, 0-9, «.», «_» и «-»; встретился «{0}»")]
    Character(char),
}

/// A snapshot directory name: `[a-z0-9._-]{1,64}`, no leading or trailing dot, no device name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SnapshotName(String);

impl SnapshotName {
    pub fn parse(raw: &str) -> Result<Self, NameError> {
        if raw.is_empty() {
            return Err(NameError::Empty);
        }
        if let Some(bad) =
            raw.chars().find(|c| !matches!(c, 'a'..='z' | '0'..='9' | '.' | '_' | '-'))
        {
            return Err(NameError::Character(bad));
        }
        // Only ASCII is left, so bytes are characters.
        if raw.len() > NAME_LIMIT {
            return Err(NameError::TooLong);
        }
        if raw.starts_with('.') || raw.ends_with('.') {
            return Err(NameError::Dot);
        }
        if RESERVED.contains(&raw.split('.').next().unwrap_or(raw)) {
            return Err(NameError::Reserved);
        }
        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SnapshotName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The local wall-clock minute a default name is stamped with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Minute {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

/// `<title>-YYYYMMDD-HHMM`: Cyrillic transliterated, any other run outside `[a-z0-9]` becomes
/// one `-`; a title that leaves nothing falls back to `fallback` (the backend name).
#[must_use]
pub fn default_name(title: Option<&str>, fallback: &str, at: Minute) -> SnapshotName {
    let Minute { year, month, day, hour, minute } = at;
    let suffix = format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}");
    let room = NAME_LIMIT.saturating_sub(suffix.len() + 1);
    let slug = [title.unwrap_or_default(), fallback]
        .into_iter()
        .map(|text| slug(text, room))
        .find(|slug| !slug.is_empty())
        .unwrap_or_default();
    // Only [a-z0-9-], ends with a digit, no dot and at most NAME_LIMIT long: a valid name.
    SnapshotName(if slug.is_empty() { suffix } else { format!("{slug}-{suffix}") })
}

fn slug(text: &str, room: usize) -> String {
    let raw = text.chars().flat_map(char::to_lowercase).fold(String::new(), |mut out, c| {
        match latin(c) {
            Some(latin) => out.push_str(latin),
            None if c.is_ascii_lowercase() || c.is_ascii_digit() => out.push(c),
            None if out.ends_with('-') => {}
            None => out.push('-'),
        }
        out
    });
    let cut: String = raw.trim_start_matches('-').chars().take(room).collect();
    cut.trim_end_matches('-').to_owned()
}

/// Russian letters in the common passport-style transliteration.
const fn latin(c: char) -> Option<&'static str> {
    Some(match c {
        'а' => "a",
        'б' => "b",
        'в' => "v",
        'г' => "g",
        'д' => "d",
        'е' | 'ё' | 'э' => "e",
        'ж' => "zh",
        'з' => "z",
        'и' => "i",
        'й' | 'ы' => "y",
        'к' => "k",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'у' => "u",
        'ф' => "f",
        'х' => "kh",
        'ц' => "ts",
        'ч' => "ch",
        'ш' => "sh",
        'щ' => "shch",
        'ъ' | 'ь' => "",
        'ю' => "yu",
        'я' => "ya",
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("путь «{0}» должен быть относительным, без пустых частей, «.» и «..»")]
    Shape(String),
    #[error("путь «{0}» содержит «\\», «:» или управляющий символ")]
    Character(String),
}

/// A `/`-separated path inside a snapshot's `agent/` or an agent's storage root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

impl RelPath {
    pub fn parse(raw: &str) -> Result<Self, PathError> {
        if raw.chars().any(|c| c == '\\' || c == ':' || c.is_control()) {
            return Err(PathError::Character(raw.to_owned()));
        }
        if raw.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
            return Err(PathError::Shape(raw.to_owned()));
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn from_parts(parts: &[&str]) -> Result<Self, PathError> {
        Self::parse(&parts.join("/"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn parts(&self) -> std::str::Split<'_, char> {
        self.0.split('/')
    }

    #[must_use]
    pub fn under(&self, base: &Path) -> PathBuf {
        self.parts().fold(base.to_path_buf(), |path, part| path.join(part))
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a backend needs, besides the files, to put a session back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendData {
    /// The `projects/<P>` directory Claude kept the transcript in (one path component).
    Claude {
        project: RelPath,
    },
    Codex,
    Qwen,
    /// The profile the session was exported from; `None` means Hermes's own choice.
    Hermes {
        profile: Option<String>,
    },
}

impl BackendData {
    #[must_use]
    pub const fn backend(&self) -> BackendKind {
        match self {
            Self::Claude { .. } => BackendKind::Claude,
            Self::Codex => BackendKind::Codex,
            Self::Qwen => BackendKind::Qwen,
            Self::Hermes { .. } => BackendKind::Hermes,
        }
    }
}

/// `manifest.json` of one snapshot; the backend is `data.backend()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: SnapshotName,
    /// UTC `YYYY-MM-DDTHH:MM:SSZ`: sorts chronologically as text.
    pub created_at: String,
    pub title: Option<String>,
    pub cwd: AbsolutePath,
    pub session: SessionId,
    pub agent_version: String,
    pub data: BackendData,
    /// Paths inside the snapshot's `agent/` directory.
    pub files: Vec<RelPath>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest.json не является JSON нужной формы: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ожидалась версия манифеста {MANIFEST_VERSION}, найдена {0}")]
    Version(u32),
    #[error("поле {field}: {reason}")]
    Field { field: &'static str, reason: String },
    #[error("путь {} не в UTF-8", .0.display())]
    NonUtf8Path(PathBuf),
    #[error("manifest.json занимает {0} байт, больше {MANIFEST_LIMIT}")]
    TooLarge(usize),
}

#[derive(Deserialize)]
struct Versioned {
    version: u32,
}

#[derive(Serialize, Deserialize)]
struct ManifestFile {
    version: u32,
    name: String,
    created_at: String,
    title: Option<String>,
    backend: String,
    cwd: String,
    session_id: String,
    agent_version: String,
    backend_data: DataFile,
    files: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum DataFile {
    Claude { project: String },
    Codex {},
    Qwen {},
    Hermes { profile: Option<String> },
}

fn bad(field: &'static str, reason: impl Into<String>) -> ManifestError {
    ManifestError::Field { field, reason: reason.into() }
}

impl Manifest {
    /// The version is checked first, so a newer format is reported as such, not as garbage.
    pub fn parse(raw: &str) -> Result<Self, ManifestError> {
        if raw.len() > MANIFEST_LIMIT {
            return Err(ManifestError::TooLarge(raw.len()));
        }
        let Versioned { version } = serde_json::from_str(raw)?;
        if version != MANIFEST_VERSION {
            return Err(ManifestError::Version(version));
        }
        let ManifestFile {
            version: _version,
            name,
            created_at,
            title,
            backend,
            cwd,
            session_id,
            agent_version,
            backend_data,
            files,
        } = serde_json::from_str(raw)?;
        let name = SnapshotName::parse(&name).map_err(|error| bad("name", error.to_string()))?;
        let cwd = AbsolutePath::new(PathBuf::from(cwd))
            .ok_or_else(|| bad("cwd", "нужен абсолютный путь"))?;
        let session = SessionId::parse(&session_id).ok_or_else(|| bad("session_id", "пусто"))?;
        let data = match backend_data {
            DataFile::Claude { project } => {
                let project = RelPath::parse(&project)
                    .map_err(|error| bad("backend_data.project", error.to_string()))?;
                if project.parts().count() != 1 {
                    return Err(bad("backend_data.project", "ожидалось одно имя каталога"));
                }
                BackendData::Claude { project }
            }
            DataFile::Codex {} => BackendData::Codex,
            DataFile::Qwen {} => BackendData::Qwen,
            DataFile::Hermes { profile } => BackendData::Hermes { profile },
        };
        if BackendKind::parse(&backend) != Some(data.backend()) {
            return Err(bad("backend", format!("«{backend}» не совпадает с backend_data")));
        }
        if files.len() > FILES_LIMIT {
            return Err(bad("files", format!("больше {FILES_LIMIT} файлов")));
        }
        let files = files
            .iter()
            .map(|file| RelPath::parse(file))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| bad("files", error.to_string()))?;
        Ok(Self { name, created_at, title, cwd, session, agent_version, data, files })
    }

    /// The same limits as `parse`, so a snapshot that is published can always be read back.
    pub fn dump(&self) -> Result<String, ManifestError> {
        let Self { name, created_at, title, cwd, session, agent_version, data, files } = self;
        if files.len() > FILES_LIMIT {
            return Err(bad("files", format!("больше {FILES_LIMIT} файлов")));
        }
        let path = cwd.as_path();
        let file = ManifestFile {
            version: MANIFEST_VERSION,
            name: name.as_str().to_owned(),
            created_at: created_at.clone(),
            title: title.clone(),
            backend: data.backend().name().to_owned(),
            cwd: path
                .to_str()
                .ok_or_else(|| ManifestError::NonUtf8Path(path.to_path_buf()))?
                .to_owned(),
            session_id: session.as_str().to_owned(),
            agent_version: agent_version.clone(),
            backend_data: match data {
                BackendData::Claude { project } => {
                    DataFile::Claude { project: project.as_str().to_owned() }
                }
                BackendData::Codex => DataFile::Codex {},
                BackendData::Qwen => DataFile::Qwen {},
                BackendData::Hermes { profile } => DataFile::Hermes { profile: profile.clone() },
            },
            files: files.iter().map(|file| file.as_str().to_owned()).collect(),
        };
        let text = serde_json::to_string_pretty(&file)?;
        if text.len() > MANIFEST_LIMIT {
            return Err(ManifestError::TooLarge(text.len()));
        }
        Ok(text)
    }
}
#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rstest::rstest;
    use serde_json::json;

    use super::*;

    fn cwd() -> String {
        if cfg!(windows) { r"C:\work\shop".to_owned() } else { "/work/shop".to_owned() }
    }

    const AT: Minute = Minute { year: 2026, month: 10, day: 9, hour: 15, minute: 30 };

    #[rstest]
    #[case::typical("fix-tests-20261009-1530")]
    #[case::single("a")]
    #[case::punctuation("a.b_c-1")]
    #[case::longest(&"a".repeat(64))]
    #[case::device_prefix("console")]
    fn valid_names_parse(#[case] raw: &str) {
        assert_eq!(
            SnapshotName::parse(raw).map(|name| name.as_str().to_owned()),
            Ok(raw.to_owned())
        );
    }

    #[rstest]
    #[case::empty("", NameError::Empty)]
    #[case::upper("Fix", NameError::Character('F'))]
    #[case::space("a b", NameError::Character(' '))]
    #[case::slash("a/b", NameError::Character('/'))]
    #[case::backslash("a\\b", NameError::Character('\\'))]
    #[case::cyrillic("имя", NameError::Character('и'))]
    #[case::too_long(&"a".repeat(65), NameError::TooLong)]
    #[case::dots("..", NameError::Dot)]
    #[case::hidden(".hidden", NameError::Dot)]
    #[case::trailing("name.", NameError::Dot)]
    #[case::device("con", NameError::Reserved)]
    #[case::device_with_extension("nul.txt", NameError::Reserved)]
    #[case::port("com1", NameError::Reserved)]
    fn invalid_names_are_refused(#[case] raw: &str, #[case] expected: NameError) {
        assert_eq!(SnapshotName::parse(raw), Err(expected));
    }

    #[rstest]
    #[case::cyrillic(Some("Починить тесты"), "claude", "pochinit-testy-20261009-1530")]
    #[case::no_title(None, "claude", "claude-20261009-1530")]
    #[case::symbols_only(Some("  *** "), "codex", "codex-20261009-1530")]
    #[case::mixed(Some("Ёлка: v2.0!"), "qwen", "elka-v2-0-20261009-1530")]
    #[case::digraphs(Some("Щука и Хек"), "hermes", "shchuka-i-khek-20261009-1530")]
    #[case::soft_signs(Some("Объём"), "claude", "obem-20261009-1530")]
    fn default_names_are_transliterated(
        #[case] title: Option<&str>,
        #[case] fallback: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(default_name(title, fallback, AT).as_str(), expected);
    }

    #[test]
    fn long_titles_are_cut_to_the_limit() {
        let name = default_name(Some(&"я".repeat(100)), "claude", AT);
        assert_eq!(SnapshotName::parse(name.as_str()), Ok(name.clone()));
        assert!(name.as_str().len() <= NAME_LIMIT && name.as_str().ends_with("-20261009-1530"));
    }

    #[rstest]
    #[case::nested("projects/-work-shop/abc.jsonl")]
    #[case::single("hermes-session.json")]
    #[case::dotted_name("chats/abc.runtime.json")]
    fn relative_paths_parse(#[case] raw: &str) {
        assert_eq!(RelPath::parse(raw).map(|path| path.as_str().to_owned()), Ok(raw.to_owned()));
    }

    #[rstest]
    #[case::empty("")]
    #[case::absolute("/etc/passwd")]
    #[case::double_slash("a//b")]
    #[case::trailing_slash("a/")]
    #[case::current("a/./b")]
    #[case::parent("a/../b")]
    #[case::only_parent("..")]
    fn malformed_paths_are_refused(#[case] raw: &str) {
        assert_eq!(RelPath::parse(raw), Err(PathError::Shape(raw.to_owned())));
    }

    #[rstest]
    #[case::backslash("a\\..\\b")]
    #[case::drive("C:/Windows")]
    #[case::stream("file.txt:stream")]
    #[case::nul("a\u{0}b")]
    #[case::newline("a\nb")]
    fn paths_with_platform_syntax_are_refused(#[case] raw: &str) {
        assert_eq!(RelPath::parse(raw), Err(PathError::Character(raw.to_owned())));
    }

    #[test]
    fn relative_paths_join_under_a_base() {
        let path = RelPath::from_parts(&["projects", "p", "id.jsonl"]).unwrap();
        assert_eq!(
            path.under(Path::new("root")),
            Path::new("root").join("projects").join("p").join("id.jsonl")
        );
    }

    fn manifest(data: BackendData) -> Manifest {
        Manifest {
            name: SnapshotName::parse("fix-tests-20261009-1530").unwrap(),
            created_at: "2026-10-09T15:30:12Z".to_owned(),
            title: Some("Починить тесты".to_owned()),
            cwd: AbsolutePath::new(PathBuf::from(cwd())).unwrap(),
            session: SessionId::parse("0b1c-id").unwrap(),
            agent_version: "2.1.292".to_owned(),
            data,
            files: vec![RelPath::parse("projects/-work-shop/0b1c-id.jsonl").unwrap()],
        }
    }

    #[rstest]
    #[case::claude(BackendData::Claude { project: RelPath::parse("-work-shop").unwrap() })]
    #[case::codex(BackendData::Codex)]
    #[case::qwen(BackendData::Qwen)]
    #[case::hermes(BackendData::Hermes { profile: Some("work".to_owned()) })]
    #[case::hermes_default(BackendData::Hermes { profile: None })]
    fn manifests_round_trip(#[case] data: BackendData) {
        let original = manifest(data);
        assert_eq!(Manifest::parse(&original.dump().unwrap()).unwrap(), original);
    }

    #[test]
    fn manifest_shape_matches_the_spec() {
        let text = manifest(BackendData::Claude { project: RelPath::parse("-work-shop").unwrap() })
            .dump()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value,
            json!({
                "version": 1,
                "name": "fix-tests-20261009-1530",
                "created_at": "2026-10-09T15:30:12Z",
                "title": "Починить тесты",
                "backend": "claude",
                "cwd": cwd(),
                "session_id": "0b1c-id",
                "agent_version": "2.1.292",
                "backend_data": {"kind": "claude", "project": "-work-shop"},
                "files": ["projects/-work-shop/0b1c-id.jsonl"],
            })
        );
    }

    fn raw(overrides: serde_json::Value) -> String {
        let mut base = json!({
            "version": 1, "name": "snap", "created_at": "2026-10-09T15:30:12Z", "title": null,
            "backend": "codex", "cwd": cwd(), "session_id": "t-1", "agent_version": "0.160.0",
            "backend_data": {"kind": "codex"}, "files": ["sessions/2026/10/09/rollout-x-t-1.jsonl"],
        });
        if let (Some(base), serde_json::Value::Object(overrides)) =
            (base.as_object_mut(), overrides)
        {
            base.extend(overrides);
        }
        base.to_string()
    }

    #[test]
    fn minimal_manifest_parses_without_a_title() {
        let parsed = Manifest::parse(&raw(json!({}))).unwrap();
        assert_eq!((parsed.title, parsed.data), (None, BackendData::Codex));
    }

    #[test]
    fn unknown_manifest_version_is_reported_before_the_shape() {
        assert!(matches!(Manifest::parse(r#"{"version": 2}"#), Err(ManifestError::Version(2))));
    }

    #[rstest]
    #[case::backend_mismatch(json!({"backend": "claude"}), "backend")]
    #[case::unknown_backend(json!({"backend": "gpt"}), "backend")]
    #[case::bad_name(json!({"name": "../x"}), "name")]
    #[case::relative_cwd(json!({"cwd": "shop"}), "cwd")]
    #[case::empty_session(json!({"session_id": " "}), "session_id")]
    #[case::traversal(json!({"files": ["../../.ssh/id_ed25519"]}), "files")]
    #[case::absolute_file(json!({"files": ["/etc/passwd"]}), "files")]
    #[case::nested_project(
        json!({"backend": "claude", "backend_data": {"kind": "claude", "project": "a/b"}}),
        "backend_data.project"
    )]
    fn malformed_manifests_name_the_field(
        #[case] overrides: serde_json::Value,
        #[case] expected: &str,
    ) {
        assert!(matches!(
            Manifest::parse(&raw(overrides)),
            Err(ManifestError::Field { field, .. }) if field == expected
        ));
    }

    /// A manifest whose JSON is exactly `len` bytes, padded through the title.
    fn manifest_of_len(len: usize) -> Manifest {
        let base = Manifest { title: Some(String::new()), ..manifest(BackendData::Codex) };
        let padding = len - base.dump().unwrap().len();
        Manifest { title: Some("a".repeat(padding)), ..base }
    }

    #[test]
    fn dump_and_parse_share_the_size_limit() {
        let at_limit = manifest_of_len(MANIFEST_LIMIT);
        let text = at_limit.dump().unwrap();
        assert_eq!(text.len(), MANIFEST_LIMIT);
        assert_eq!(Manifest::parse(&text).unwrap(), at_limit);
        let over = Manifest { title: at_limit.title.map(|title| format!("{title}a")), ..at_limit };
        assert!(
            matches!(over.dump(), Err(ManifestError::TooLarge(len)) if len == MANIFEST_LIMIT + 1)
        );
        assert!(matches!(Manifest::parse(&format!("{text} ")), Err(ManifestError::TooLarge(_))));
    }

    #[test]
    fn dump_refuses_more_files_than_parse_accepts() {
        let file = RelPath::parse("f").unwrap();
        let at_limit =
            Manifest { files: vec![file.clone(); FILES_LIMIT], ..manifest(BackendData::Codex) };
        assert_eq!(Manifest::parse(&at_limit.dump().unwrap()).unwrap().files.len(), FILES_LIMIT);
        let over = Manifest { files: vec![file; FILES_LIMIT + 1], ..manifest(BackendData::Codex) };
        assert!(matches!(over.dump(), Err(ManifestError::Field { field: "files", .. })));
    }

    #[test]
    fn too_many_files_are_refused() {
        let files: Vec<String> = (0..=FILES_LIMIT).map(|i| format!("f{i}")).collect();
        assert!(matches!(
            Manifest::parse(&raw(json!({ "files": files }))),
            Err(ManifestError::Field { field: "files", .. })
        ));
    }
}
