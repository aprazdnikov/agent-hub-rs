use super::{
    Binding, Browser, Done, HubMessage, MemoryReader, Request, View, admin, lifecycle,
    memory_error, read_async,
};
use crate::inbound::{Attachment, Content, Inbound};
use hub_memory::{MemoryExport, StatusFilter};
use lifecycle::Action;

pub(super) async fn prepare_export(
    base: &std::path::Path,
    cwd: &std::path::Path,
    reset: bool,
) -> Result<View, String> {
    let base = base.to_owned();
    let cwd = cwd.to_owned();
    let (receipt, token, keep) = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let reader = MemoryReader::new(&base, &cwd).map_err(memory_error)?;
        let receipt =
            reader.export().map_err(memory_error)?.write_temporary().map_err(memory_error)?;
        let token = reader.overview("", 0, 20, StatusFilter::All).map_err(memory_error)?.token;
        let mut keep = Vec::new();
        let mut offset = 0;
        loop {
            let page =
                reader.overview("", offset, 20, StatusFilter::All).map_err(memory_error)?.page;
            keep.extend(page.entries.into_iter().map(|e| e.key));
            offset += 20;
            if offset >= page.matched_count {
                break;
            }
        }
        Ok((receipt, token, keep))
    })
    .await
    .map_err(|_| "Экспорт прерван")??;
    Ok(View::ExportReady { receipt, token, keep, reset })
}

impl Browser {
    pub fn attachment(&mut self, inbound: &Inbound, cwd: &std::path::Path) -> bool {
        let (Some(key), Some(user)) = (inbound.key(), inbound.user) else {
            return false;
        };
        let Some((expected, time)) = self.imports.get(&(key, user)) else {
            return false;
        };
        if !matches!(inbound.content, Content::Media { .. }) {
            return false;
        }
        if expected != cwd || time.elapsed() >= std::time::Duration::from_mins(10) {
            self.imports.remove(&(key, user));
            self.say(key,"Импорт устарел или проект сменился. Повторите /memory import; файл агенту не передан.".into());
            return true;
        }
        let Content::Media { attachment: Attachment::File(file), .. } = &inbound.content else {
            self.say(key, "Для импорта нужен JSON-файл, не фотография.".into());
            return true;
        };
        if inbound.album.is_some() || file.size.is_some_and(|size| size > 16 * 1024 * 1024) {
            self.say(key, "Пришлите один файл до 16 МиБ.".into());
            return true;
        }
        if self.busy {
            self.say(key, "Операция памяти выполняется. Пришлите файл после её завершения.".into());
            return true;
        }
        self.imports.remove(&(key, user));
        let initial = Binding {
            key,
            user,
            cwd: cwd.to_owned(),
            request: Request::List { query: String::new(), offset: 0 },
        };
        let epoch = self.begin(&initial);
        let sender = self.sender.clone();
        let mailbox = self.mailbox.clone();
        let base = self.base.clone();
        let cwd = cwd.to_owned();
        let file = file.clone();
        tokio::spawn(async move {
            let mut binding = Binding {
                key,
                user,
                cwd: cwd.clone(),
                request: Request::List { query: String::new(), offset: 0 },
            };
            let result = async {
                let bytes = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    sender.messenger().fetch_limited(file.id, 16 * 1024 * 1024),
                )
                .await
                .map_err(|_| "Загрузка импорта превысила 30 с")?
                .map_err(|e| e.to_string())?;
                let export =
                    tokio::task::spawn_blocking(move || MemoryExport::decode(bytes.as_slice()))
                        .await
                        .map_err(|_| "Чтение импорта прервано")?
                        .map_err(memory_error)?;
                binding.request = admin::admin(Action::Import {
                    export,
                    choices: std::collections::BTreeMap::new(),
                });
                read_async(base, cwd, binding.request.clone()).await
            }
            .await;
            let _ = mailbox.send(HubMessage::MemoryDone(Done { epoch, binding, result })).await;
        });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sender::Sender;
    use crate::{inbound::FileRef, testing::FakeMessenger};
    use hub_core::domain::TopicKey;
    use hub_core::domain::{ChatId, MessageId, ThreadId, UserId};
    use std::sync::Arc;
    #[tokio::test]
    async fn attachment_requires_explicit_matching_import_and_uses_preview() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("memory");
        let source = root.path().join("source");
        let target = root.path().join("target");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&target).unwrap();
        let store = hub_memory::Store::open(&base, &source, "test").unwrap();
        store
            .write(hub_memory::WriteRequest {
                key: "sample".into(),
                expected_revision: 0,
                kind: hub_memory::EntryKind::Progress,
                title: "title".into(),
                body: "body".into(),
                sources: vec!["test".into()],
            })
            .unwrap();
        let export =
            MemoryReader::new(&base, &source).unwrap().export().unwrap().write_temporary().unwrap();
        let mut fake = FakeMessenger::default();
        fake.files.insert("file".into(), std::fs::read(export.path()).unwrap());
        let (send, mut receive) = tokio::sync::mpsc::channel(4);
        let mut browser = Browser::new(Sender::new(Arc::new(fake)), send, base.clone(), None);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(1) };
        let mut message = Inbound {
            chat: key.chat,
            thread: Some(key.thread),
            user: Some(UserId(1)),
            message: MessageId(1),
            reply_to: None,
            voice_reply: false,
            handoff_reply: false,
            album: None,
            content: Content::Media {
                caption: String::new(),
                attachment: Attachment::File(FileRef {
                    id: "file".into(),
                    name: Some("memory.json".into()),
                    size: None,
                }),
            },
        };
        assert!(!browser.attachment(&message, &target));
        browser.menu(key, Some(UserId(1)), target.clone(), "import");
        message.user = Some(UserId(2));
        assert!(!browser.attachment(&message, &target));
        message.user = Some(UserId(1));
        assert!(browser.attachment(&message, &target));
        let done = tokio::time::timeout(std::time::Duration::from_secs(5), receive.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(done, HubMessage::MemoryDone(Done { result: Ok(View::Admin(_)), .. })));
        assert!(MemoryReader::new(&base, &target).unwrap().get("sample", None).is_err());
    }
    #[tokio::test]
    async fn cancel_and_project_change_do_not_import() {
        let root = tempfile::tempdir().unwrap();
        let (send, _receive) = tokio::sync::mpsc::channel(4);
        let mut browser = Browser::new(
            Sender::new(Arc::new(FakeMessenger::default())),
            send,
            root.path().join("memory"),
            None,
        );
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(1) };
        browser.menu(key, Some(UserId(1)), root.path().into(), "import");
        browser.menu(key, Some(UserId(1)), root.path().into(), "cancel");
        assert!(browser.imports.is_empty());
        assert!(!browser.busy);
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use crate::{
        sender::Sender,
        testing::{Call, FakeMessenger},
    };
    use hub_core::domain::{ChatId, ThreadId, TopicKey, UserId};
    use std::sync::Arc;
    #[tokio::test]
    async fn cancel_during_export_preparation_never_sends_document_or_recreates_confirmation() {
        check_invalidation(false).await;
    }
    #[tokio::test]
    async fn project_roundtrip_does_not_restore_old_export_request() {
        check_invalidation(true).await;
    }
    async fn check_invalidation(change_project: bool) {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("memory");
        let store = hub_memory::Store::open(&base, root.path(), "test").unwrap();
        store
            .write(hub_memory::WriteRequest {
                key: "key".into(),
                expected_revision: 0,
                kind: hub_memory::EntryKind::Progress,
                title: "title".into(),
                body: "private".into(),
                sources: vec!["test".into()],
            })
            .unwrap();
        let settings = hub_core::settings::Draft {
            token: "1:a".into(),
            chat: "-100".into(),
            users: "1".into(),
            workspace_root: root.path().display().to_string(),
            ..Default::default()
        }
        .parse(root.path())
        .unwrap();
        let fake = Arc::new(FakeMessenger::default());
        let (send, mut receive) = tokio::sync::mpsc::channel(4);
        let mut browser = Browser::new(Sender::new(fake.clone()), send, base, None);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(1) };
        let cwd = root.path().to_path_buf();
        browser.start(Binding {
            key,
            user: UserId(1),
            cwd: cwd.clone(),
            request: Request::ExportSend(true),
        });
        if change_project {
            let other = root.path().join("other");
            std::fs::create_dir(&other).unwrap();
            let mut topics = hub_core::topics::Topics::from([(
                key,
                hub_core::domain::TopicSession::fresh(
                    hub_core::domain::BackendKind::Codex,
                    crate::paths::workspace_root(&other).unwrap(),
                ),
            )]);
            browser.refresh(&settings, &topics);
            topics.insert(
                key,
                hub_core::domain::TopicSession::fresh(
                    hub_core::domain::BackendKind::Codex,
                    crate::paths::workspace_root(&cwd).unwrap(),
                ),
            );
            browser.refresh(&settings, &topics);
        } else {
            browser.menu(key, Some(UserId(1)), cwd.clone(), "cancel");
        }
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), receive.recv())
            .await
            .unwrap()
            .unwrap();
        let HubMessage::MemoryDone(done) = message else { panic!("unexpected message") };
        browser.finish(done, &settings, Some(&cwd), None);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!fake.calls().iter().any(|call| matches!(call, Call::Document { .. })));
        assert!(browser.bindings.is_empty());
        assert!(browser.admin_draft.is_none());
    }
}
