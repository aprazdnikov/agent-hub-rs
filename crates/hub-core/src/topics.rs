//! `topics.json`: topic → session bindings, format version 1 shared with the Python version.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::options::{Preferences, PreferencesDraft};
use serde::{Deserialize, Serialize};

use crate::domain::{
    AbsolutePath, BackendKind, ChatId, SessionId, ThreadId, TopicKey, TopicSession,
};

const FORMAT_VERSION: u32 = 1;

pub type Topics = BTreeMap<TopicKey, TopicSession>;
/// Separate metadata: a rename does not change the topic's agent session.
pub type Titles = BTreeMap<TopicKey, String>;

#[derive(Serialize, Deserialize)]
struct TitlesFile {
    version: u32,
    titles: Vec<TitleEntry>,
}

#[derive(Serialize, Deserialize)]
struct TitleEntry {
    chat_id: i64,
    thread_id: i32,
    title: String,
}

/// Reads the optional `topics.titles.json` sidecar, not the session bindings file.
pub fn parse_titles(raw: &str) -> Result<Titles, CorruptState> {
    let file: TitlesFile = serde_json::from_str(raw)?;
    if file.version != FORMAT_VERSION {
        return Err(CorruptState::Version(file.version));
    }
    let mut titles = Titles::new();
    for (index, TitleEntry { chat_id, thread_id, title }) in file.titles.into_iter().enumerate() {
        let key = TopicKey { chat: ChatId(chat_id), thread: ThreadId(thread_id) };
        if titles.insert(key, title).is_some() {
            return Err(CorruptState::Entry {
                index, reason: "повторяющийся ключ темы"
            });
        }
    }
    Ok(titles)
}

pub fn dump_titles(titles: &Titles) -> Result<String, serde_json::Error> {
    let titles = titles
        .iter()
        .map(|(key, title)| TitleEntry {
            chat_id: key.chat.0,
            thread_id: key.thread.0,
            title: title.clone(),
        })
        .collect();
    serde_json::to_string_pretty(&TitlesFile { version: FORMAT_VERSION, titles })
}

#[derive(Debug, thiserror::Error)]
pub enum CorruptState {
    #[error("файл не является JSON нужной формы")]
    Json(#[from] serde_json::Error),
    #[error("неподдерживаемая версия формата: {0}")]
    Version(u32),
    #[error("тема №{index}: {reason}")]
    Entry { index: usize, reason: &'static str },
}

#[derive(Debug, thiserror::Error)]
pub enum DumpError {
    #[error("Некорректные настройки темы: {0}")]
    Preferences(String),
    #[error("не удалось сериализовать привязки тем")]
    Json(#[from] serde_json::Error),
    #[error("путь {} не в UTF-8", .0.display())]
    NonUtf8Path(PathBuf),
}

#[derive(Serialize, Deserialize)]
struct StateFile {
    version: u32,
    topics: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preferences: Option<PreferencesDraft>,
    #[serde(rename = "chat_id")]
    chat: i64,
    #[serde(rename = "thread_id")]
    thread: i32,
    backend: String,
    cwd: String,
    #[serde(rename = "session_id")]
    session: Option<String>,
}

pub fn parse_state(raw: &str) -> Result<Topics, CorruptState> {
    let file: StateFile = serde_json::from_str(raw)?;
    if !matches!(file.version, 1 | 2) {
        return Err(CorruptState::Version(file.version));
    }
    if file.version == 1 && file.topics.iter().any(|entry| entry.preferences.is_some()) {
        return Err(CorruptState::Entry {
            index: 0, reason: "настройкам темы нужна версия 2"
        });
    }
    file.topics
        .into_iter()
        .enumerate()
        .map(|(index, entry)| {
            parse_entry(entry).map_err(|reason| CorruptState::Entry { index, reason })
        })
        .collect()
}

fn parse_entry(entry: Entry) -> Result<(TopicKey, TopicSession), &'static str> {
    let Entry { chat, thread, backend, cwd, session, preferences } = entry;
    let backend = BackendKind::parse(&backend).ok_or("неизвестный бэкенд")?;
    let preferences =
        Preferences::parse(backend, preferences).map_err(|_| "некорректные настройки темы")?;
    let cwd = AbsolutePath::new(PathBuf::from(cwd)).ok_or("cwd должен быть абсолютным путём")?;
    let session = match session {
        None => None,
        Some(raw) => Some(SessionId::parse(&raw).ok_or("пустой session_id")?),
    };
    Ok((
        TopicKey { chat: ChatId(chat), thread: ThreadId(thread) },
        TopicSession { preferences, backend, cwd, session },
    ))
}

pub fn dump_state(topics: &Topics) -> Result<String, DumpError> {
    let entries = topics
        .iter()
        .map(|(key, session)| {
            Preferences::parse(session.backend, session.preferences.to_draft())
                .map_err(DumpError::Preferences)?;
            let cwd = session.cwd.as_path();
            Ok(Entry {
                preferences: session.preferences.to_draft(),
                chat: key.chat.0,
                thread: key.thread.0,
                backend: session.backend.name().to_owned(),
                cwd: cwd
                    .to_str()
                    .ok_or_else(|| DumpError::NonUtf8Path(cwd.to_path_buf()))?
                    .to_owned(),
                session: session.session.as_ref().map(|id| id.as_str().to_owned()),
            })
        })
        .collect::<Result<Vec<_>, DumpError>>()?;
    let version =
        if entries.iter().any(|entry| entry.preferences.is_some()) { 2 } else { FORMAT_VERSION };
    Ok(serde_json::to_string_pretty(&StateFile { version, topics: entries })?)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use rstest::rstest;

    use super::*;

    fn absolute(name: &str) -> PathBuf {
        PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" }).join(name)
    }

    fn session(name: &str, id: Option<&str>) -> TopicSession {
        TopicSession {
            preferences: crate::options::Preferences::default(),
            backend: BackendKind::Claude,
            cwd: AbsolutePath::new(absolute(name)).unwrap(),
            session: id.and_then(SessionId::parse),
        }
    }

    fn json_path(name: &str) -> String {
        serde_json::to_string(&absolute(name).display().to_string()).unwrap()
    }

    #[test]
    fn topic_options_use_v2_only_when_present_and_survive_restart() {
        use crate::options::{Options, OptionsDraft, Preferences};
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut topic = session("project", Some("saved"));
        let old = dump_state(&Topics::from([(key, topic.clone())])).unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&old).unwrap()["version"], 1);
        let options = Options::parse(
            topic.backend,
            &OptionsDraft { model: "model".to_owned(), mode: "plan".to_owned() },
        )
        .unwrap();
        topic.preferences = Preferences::for_project("p", "Project", options);
        let topics = Topics::from([(key, topic)]);
        let raw = dump_state(&topics).unwrap();
        assert_eq!(serde_json::from_str::<serde_json::Value>(&raw).unwrap()["version"], 2);
        assert_eq!(parse_state(&raw).unwrap(), topics);
        let mut invalid: serde_json::Value = serde_json::from_str(&raw).unwrap();
        invalid["topics"][0]["backend"] = "codex".into();
        assert!(parse_state(&invalid.to_string()).is_err());
        invalid["version"] = 1.into();
        assert!(parse_state(&invalid.to_string()).is_err());
    }
    #[test]
    fn empty_state_round_trips() {
        assert_eq!(parse_state(&dump_state(&Topics::new()).unwrap()).unwrap(), Topics::new());
    }

    #[test]
    fn titles_round_trip_independently_of_bindings() {
        let titles = Titles::from([
            (TopicKey { chat: ChatId(-100), thread: ThreadId(7) }, "Тесты 🦀".to_owned()),
            (TopicKey { chat: ChatId(-200), thread: ThreadId(7) }, "Другой чат".to_owned()),
        ]);
        let text = dump_titles(&titles).unwrap();
        assert!(text.contains("Тесты 🦀"));
        assert_eq!(parse_titles(&text).unwrap(), titles);
        assert_eq!(parse_titles(&dump_titles(&Titles::new()).unwrap()).unwrap(), Titles::new());
        let bindings = dump_state(&Topics::new()).unwrap();
        assert!(!bindings.contains("titles"));
    }

    #[rstest]
    #[case("broken")]
    #[case(r#"{"version": 2, "titles": []}"#)]
    #[case(r#"{"version": 1, "titles": [{"chat_id": 1, "thread_id": 7, "title": 123}]}"#)]
    #[case(r#"{"version": 1, "titles": [{"chat_id": 1, "thread_id": 7, "title": "one"}, {"chat_id": 1, "thread_id": 7, "title": "two"}]}"#)]
    fn invalid_titles_are_rejected(#[case] raw: &str) {
        assert!(parse_titles(raw).is_err());
    }

    #[test]
    fn sessions_round_trip() {
        let topics = Topics::from([
            (TopicKey { chat: ChatId(-100_123), thread: ThreadId(42) }, session("a", Some("abc"))),
            (TopicKey { chat: ChatId(1), thread: ThreadId(2) }, session("b", None)),
        ]);
        assert_eq!(parse_state(&dump_state(&topics).unwrap()).unwrap(), topics);
    }

    #[test]
    fn hermes_topic_state_round_trips_with_version_one() {
        let raw = format!(
            r#"{{"version":1,"topics":[{{"chat_id":1,"thread_id":2,"backend":"hermes","cwd":{},"session_id":"h-1"}}]}}"#,
            json_path("hermes")
        );
        let topics = parse_state(&raw).unwrap();
        let dumped: serde_json::Value =
            serde_json::from_str(&dump_state(&topics).unwrap()).unwrap();
        assert_eq!(dumped, serde_json::from_str::<serde_json::Value>(&raw).unwrap());
    }

    #[test]
    fn opencode_topic_state_round_trips_with_version_one() {
        let raw = format!(
            r#"{{"version":1,"topics":[{{"chat_id":1,"thread_id":2,"backend":"opencode","cwd":{},"session_id":"o-1"}}]}}"#,
            json_path("opencode")
        );
        let topics = parse_state(&raw).unwrap();
        let dumped: serde_json::Value =
            serde_json::from_str(&dump_state(&topics).unwrap()).unwrap();
        assert_eq!(dumped, serde_json::from_str::<serde_json::Value>(&raw).unwrap());
    }

    #[test]
    fn python_written_state_is_read() {
        let raw = format!(
            r#"{{
  "version": 1,
  "topics": [
    {{
      "chat_id": -100123,
      "thread_id": 42,
      "backend": "claude",
      "cwd": {},
      "session_id": "abc"
    }}
  ]
}}"#,
            json_path("проект")
        );
        let topics = parse_state(&raw).unwrap();
        assert_eq!(
            topics.get(&TopicKey { chat: ChatId(-100_123), thread: ThreadId(42) }),
            Some(&session("проект", Some("abc")))
        );
    }

    #[test]
    fn dump_keeps_non_ascii_readable() {
        let topics = Topics::from([(
            TopicKey { chat: ChatId(1), thread: ThreadId(2) },
            session("проект", None),
        )]);
        assert!(dump_state(&topics).unwrap().contains("проект"));
    }

    #[rstest]
    #[case("not json".to_owned())]
    #[case(r#"{"version": 999, "topics": []}"#.to_owned())]
    #[case(r#"{"version": 1, "topics": {}}"#.to_owned())]
    #[case(r#"{"version": 1, "topics": [{"chat_id": "1"}]}"#.to_owned())]
    #[case(format!(
        r#"{{"version": 1, "topics": [{{"chat_id": 1, "thread_id": 2, "backend": "gpt", "cwd": {}, "session_id": null}}]}}"#,
        json_path("a")
    ))]
    #[case(
        r#"{"version": 1, "topics": [{"chat_id": 1, "thread_id": 2, "backend": "claude", "cwd": "rel", "session_id": null}]}"#
            .to_owned()
    )]
    #[case(format!(
        r#"{{"version": 1, "topics": [{{"chat_id": 1, "thread_id": 2, "backend": "claude", "cwd": {}, "session_id": " "}}]}}"#,
        json_path("a")
    ))]
    fn corrupt_state_is_rejected(#[case] raw: String) {
        assert!(parse_state(&raw).is_err());
    }
}
