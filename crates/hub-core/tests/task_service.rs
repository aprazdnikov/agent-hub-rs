#[cfg(test)]
mod tests {
    use hub_core::tasks::{
        RunState, TaskBook,
        service::{Access, TaskService, TaskStorage},
    };
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Default)]
    struct Store {
        book: Arc<Mutex<TaskBook>>,
        fail: Arc<std::sync::atomic::AtomicBool>,
    }
    impl TaskStorage for Store {
        fn load_tasks(&mut self) -> Result<TaskBook, String> {
            Ok(self.book.lock().unwrap().clone())
        }
        fn save_tasks(&mut self, book: &TaskBook) -> Result<(), String> {
            if self.fail.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("disk failed".into());
            }
            *self.book.lock().unwrap() = book.clone();
            Ok(())
        }
    }
    #[test]
    fn clients_share_one_serialized_book_and_preserve_concurrent_changes() {
        let service = Arc::new(TaskService::new(Box::new(Store::default())));
        let id = service
            .access(Access::Edit, |book, _| {
                book.create(std::env::temp_dir(), "qwen", "Task", "Read")
            })
            .unwrap();
        service.access(Access::Internal, |book, _| book.begin(id, 1, None)).unwrap();
        let other = service.clone();
        let writer = std::thread::spawn(move || {
            for n in 0..20 {
                other
                    .access(Access::Edit, |book, _| {
                        book.create(std::env::temp_dir(), "qwen", &format!("Other {n}"), "Read")
                    })
                    .unwrap();
            }
        });
        for n in 0..20 {
            service
                .access(Access::Internal, |book, _| book.record(id, 1, &format!("Result {n}")))
                .unwrap();
        }
        writer.join().unwrap();
        service
            .access(Access::Internal, |book, _| book.finish(id, 1, RunState::Completed))
            .unwrap();
        let book = service.read().unwrap();
        assert_eq!(book.tasks().len(), 21);
        assert_eq!(book.get(id).unwrap().runs[0].result, "Result 19");
    }
    #[test]
    fn busy_lease_and_storage_failure_are_shared_by_both_clients() {
        let store = Store::default();
        let service = Arc::new(TaskService::new(Box::new(store.clone())));
        let lease = service.reserve().unwrap();
        assert!(
            service
                .access(Access::Edit, |book, _| book.create(
                    std::env::temp_dir(),
                    "qwen",
                    "Task",
                    "Read"
                ))
                .is_err()
        );
        assert!(service.read().is_ok());
        drop(lease);
        store.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            service
                .access(Access::Edit, |book, _| book.create(
                    std::env::temp_dir(),
                    "qwen",
                    "Task",
                    "Read"
                ))
                .is_err()
        );
        assert!(service.read().is_err());
        assert!(store.book.lock().unwrap().tasks().is_empty());
    }
}
