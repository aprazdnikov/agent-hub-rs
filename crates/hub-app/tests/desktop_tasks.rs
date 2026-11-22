#[cfg(test)]
mod tests {
    use hub_app::{desktop_tasks::request, topics::FileTopics};
    use hub_core::{
        domain::BackendKind,
        tasks::{TaskStatus, service::TaskService},
    };
    use hub_telegram::hub::tasks::Request;
    #[test]
    fn local_management_is_persistent_revisioned_and_project_scoped() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let other = root.path().join("other");
        std::fs::create_dir(&other).unwrap();
        let file = root.path().join("topics.json");
        let service = TaskService::new(Box::new(FileTopics::new(file.clone())));
        let apply = |project: &std::path::Path, op| {
            request(&service, project, BackendKind::Qwen, op, root.path().to_str(), root.path())
        };
        let snapshot =
            apply(&project, Request::Create { title: "Local".into(), prompt: "Inspect".into() })
                .unwrap();
        let id = snapshot.tasks[0].id;
        apply(&project, Request::Status { id, revision: 1, status: TaskStatus::Deferred }).unwrap();
        assert!(
            apply(&project, Request::Assign { id, revision: 1, backend: BackendKind::Codex })
                .is_err()
        );
        assert!(apply(&other, Request::Get(id)).is_err());
        assert!(apply(&other, Request::List).unwrap().tasks.is_empty());
        apply(
            &project,
            Request::SaveRules {
                revision: 0, enabled: true, text: "Русский язык".into()
            },
        )
        .unwrap();
        drop(service);
        let restored = TaskService::new(Box::new(FileTopics::new(file)));
        let snapshot =
            request(&restored, &project, BackendKind::Qwen, Request::List, None, root.path())
                .unwrap();
        assert_eq!(snapshot.tasks[0].status, TaskStatus::Deferred);
        assert_eq!(snapshot.rules.text, "Русский язык");
        assert!(snapshot.tasks[0].runs.is_empty());
    }
    #[test]
    fn delete_checks_scope_revision_activity_and_removes_only_delivered_copy() {
        use hub_core::tasks::{ResultFile, RunState, service::Access};
        use hub_telegram::hub::tasks::parse;
        let data = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let topics = data.path().join("topics.json");
        let service = TaskService::new(Box::new(FileTopics::new(topics.clone())));
        let apply = |path, op| request(&service, path, BackendKind::Qwen, op, None, project.path());
        let id = apply(
            project.path(),
            Request::Create { title: "Report".into(), prompt: "Inspect".into() },
        )
        .unwrap()
        .tasks[0]
            .id;
        let delete = |revision| Request::Delete { id, revision };
        assert!(parse("delete 1 1").is_err());
        assert!(parse("delete 1 1 confirm").is_ok());
        assert!(apply(data.path(), delete(1)).is_err());
        assert!(apply(project.path(), delete(2)).is_err());
        let key = "aa".repeat(16);
        let name = "report.txt";
        let dir = data.path().join("task-files").join(&key);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), "copy").unwrap();
        std::fs::write(project.path().join(name), "original").unwrap();
        service
            .access(Access::Internal, |book, _| {
                book.begin(id, 1, None)?;
                book.record_file(
                    id,
                    1,
                    ResultFile { key, name: name.into(), caption: String::new(), bytes: 4 },
                )
            })
            .unwrap();
        assert!(apply(project.path(), delete(2)).is_err());
        service
            .access(Access::Internal, |book, _| book.finish(id, 1, RunState::Completed))
            .unwrap();
        service
            .access(Access::Internal, |book, _| {
                book.enqueue(id, 3, Some("More".into()), false, None)
            })
            .unwrap();
        assert!(apply(project.path(), delete(4)).is_err());
        apply(project.path(), Request::Unqueue { id, revision: 4 }).unwrap();
        apply(project.path(), delete(5)).unwrap();
        assert!(!dir.exists());
        assert!(project.path().join(name).is_file());
        let restored = TaskService::new(Box::new(FileTopics::new(topics)));
        assert!(restored.read().unwrap().get(id).is_err());
        assert!(
            restored
                .access(Access::Edit, |book, _| book.create(
                    project.path().into(),
                    "qwen",
                    "Another",
                    "Inspect"
                ))
                .unwrap()
                > id
        );
    }
    #[test]
    fn configured_root_is_enforced_even_without_telegram_credentials() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let service = TaskService::new(Box::new(FileTopics::new(root.path().join("topics.json"))));
        assert!(
            request(
                &service,
                outside.path(),
                BackendKind::Qwen,
                Request::Create { title: "No".into(), prompt: "Read".into() },
                root.path().to_str(),
                root.path()
            )
            .is_err()
        );
        assert!(service.read().unwrap().tasks().is_empty());
    }
    #[test]
    fn attention_lists_failed_and_review_tasks_across_projects_and_obeys_root() {
        use hub_app::desktop_tasks::{Attention, index};
        use hub_core::tasks::{RunState, service::Access};
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("one");
        let second = root.path().join("two");
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(&second).unwrap();
        let service = TaskService::new(Box::new(FileTopics::new(root.path().join("topics.json"))));
        let projects = [&project, &second, &outside.path().to_path_buf()];
        for (n, path) in projects.iter().enumerate() {
            let t = request(
                &service,
                path,
                BackendKind::Codex,
                Request::Create { title: format!("task {n}"), prompt: "read".into() },
                None,
                root.path(),
            )
            .unwrap();
            let id = t.tasks[0].id;
            service
                .access(Access::Internal, |b, _| {
                    let run = b.begin(id, 1, None)?;
                    b.finish(id, run, if n == 0 { RunState::Failed } else { RunState::Completed })
                })
                .unwrap();
        }
        let rows = index(&service, root.path().to_str(), root.path()).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].reason, Some(Attention::AgentFailed));
        assert_eq!(rows[1].reason, Some(Attention::Review));
        let id = rows[1].id;
        service
            .access(Access::Edit, |b, _| {
                b.set_status(id, b.get(id)?.revision, TaskStatus::Accepted)
            })
            .unwrap();
        assert!(index(&service, root.path().to_str(), root.path()).unwrap()[1].reason.is_none());
    }
}
