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
}
