//! `topics.json`: topic → session bindings, format version 1 shared with the Python version.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::domain::{
    AbsolutePath, BackendKind, ChatId, SessionId, ThreadId, TopicKey, TopicSession,
};

const FORMAT_VERSION: u32 = 1;

pub type Topics = BTreeMap<TopicKey, TopicSession>;

#[derive(Debug, thiserror::Error)]
pub enum CorruptState {
    #[error("файл не является JSON нужной формы")]
    Json(#[from] serde_json::Error),
    #[error("ожидалась версия формата {FORMAT_VERSION}, найдена {0}")]
    Version(u32),
    #[error("тема №{index}: {reason}")]
    Entry { index: usize, reason: &'static str },
}

#[derive(Debug, thiserror::Error)]
pub enum DumpError {
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
    if file.version != FORMAT_VERSION {
        return Err(CorruptState::Version(file.version));
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
    let Entry { chat, thread, backend, cwd, session } = entry;
    let backend = BackendKind::parse(&backend).ok_or("неизвестный бэкенд")?;
    let cwd = AbsolutePath::new(PathBuf::from(cwd)).ok_or("cwd должен быть абсолютным путём")?;
    let session = match session {
        None => None,
        Some(raw) => Some(SessionId::parse(&raw).ok_or("пустой session_id")?),
    };
    Ok((
        TopicKey { chat: ChatId(chat), thread: ThreadId(thread) },
        TopicSession { backend, cwd, session },
    ))
}

pub fn dump_state(topics: &Topics) -> Result<String, DumpError> {
    let entries = topics
        .iter()
        .map(|(key, session)| {
            let cwd = session.cwd.as_path();
            Ok(Entry {
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
    Ok(serde_json::to_string_pretty(&StateFile { version: FORMAT_VERSION, topics: entries })?)
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
            backend: BackendKind::Claude,
            cwd: AbsolutePath::new(absolute(name)).unwrap(),
            session: id.and_then(SessionId::parse),
        }
    }

    fn json_path(name: &str) -> String {
        serde_json::to_string(&absolute(name).display().to_string()).unwrap()
    }

    #[test]
    fn empty_state_round_trips() {
        assert_eq!(parse_state(&dump_state(&Topics::new()).unwrap()).unwrap(), Topics::new());
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
