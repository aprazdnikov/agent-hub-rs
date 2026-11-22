//! One owner of the task book and persistence. Clients submit whole synchronous transactions.
use super::TaskBook;
use std::sync::{Arc, Mutex};

pub trait TaskStorage: Send + 'static {
    fn load_tasks(&mut self) -> Result<TaskBook, String>;
    fn save_tasks(&mut self, book: &TaskBook) -> Result<(), String>;
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Edit,
    Internal,
}
struct State {
    book: Result<TaskBook, String>,
    storage: Box<dyn TaskStorage>,
    busy: bool,
}
pub struct TaskService {
    state: Mutex<State>,
}
pub struct BusyLease {
    service: Arc<TaskService>,
}
impl Drop for BusyLease {
    fn drop(&mut self) {
        if let Ok(mut state) = self.service.state.lock() {
            state.busy = false;
        }
    }
}
impl TaskService {
    #[must_use]
    pub fn new(mut storage: Box<dyn TaskStorage>) -> Self {
        let book = storage.load_tasks().and_then(|mut book| {
            if book.recover() {
                storage.save_tasks(&book)?;
            }
            Ok(book)
        });
        Self { state: Mutex::new(State { book, storage, busy: false }) }
    }
    /// Read a small projection without cloning run histories or changing the store.
    pub fn inspect<T>(&self, project: impl FnOnce(&TaskBook) -> T) -> Result<T, String> {
        let state = self.state.lock().map_err(|_| "Сервис задач недоступен")?;
        state.book.as_ref().map(project).map_err(Clone::clone)
    }
    pub fn read(&self) -> Result<TaskBook, String> {
        self.state
            .lock()
            .map_err(|_| "Сервис задач недоступен; перезапустите приложение".to_owned())?
            .book
            .clone()
    }
    pub fn reserve(self: &Arc<Self>) -> Result<BusyLease, String> {
        let mut state = self.state.lock().map_err(|_| "Сервис задач недоступен")?;
        state.book.as_ref().map_err(Clone::clone)?;
        if state.busy {
            return Err("Дождитесь завершения подготовки или проверки Git.".into());
        }
        state.busy = true;
        Ok(BusyLease { service: self.clone() })
    }
    pub fn access<T>(
        &self,
        access: Access,
        operation: impl FnOnce(&mut TaskBook, bool) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut state =
            self.state.lock().map_err(|_| "Сервис задач недоступен; перезапустите приложение")?;
        if access == Access::Edit && state.busy {
            return Err("Дождитесь завершения подготовки или проверки Git.".into());
        }
        let mut book = state.book.clone()?;
        let result = operation(&mut book, state.busy)?;
        if state.book.as_ref().is_ok_and(|previous| *previous == book) {
            return Ok(result);
        }
        if access == Access::Read {
            return Err("Чтение задач не должно менять данные".into());
        }
        book.encode()?; // Known validation/capacity refusals have no storage effect.
        if let Err(error) = state.storage.save_tasks(&book) {
            let error = format!(
                "{error}. Операции задач заблокированы до перезапуска приложения; проверьте предыдущие эффекты."
            );
            state.book = Err(error.clone());
            return Err(error);
        }
        state.book = Ok(book);
        Ok(result)
    }
    pub fn recover(&self) -> Result<(), String> {
        self.access(Access::Internal, |book, _| {
            book.recover();
            Ok(())
        })
    }
}

#[derive(Clone, Debug)]
pub struct Pending {
    pub id: u64,
    pub project: std::path::PathBuf,
    pub revision: u64,
    pub isolated: bool,
    pub origin: Option<super::queue::Origin>,
}

impl TaskService {
    /// Poll the FIFO without cloning the task book or an attempt's retained output.
    pub fn pending(&self, connected: bool) -> Option<Pending> {
        let state = self.state.lock().ok()?;
        if state.busy {
            return None;
        }
        let (task, q) = state.book.as_ref().ok()?.next_queued(connected)?;
        Some(Pending {
            id: task.id,
            project: task.project.clone(),
            revision: q.revision,
            isolated: q.isolated,
            origin: q.origin,
        })
    }

    pub fn pause_pending(&self, pending: &Pending, error: &str) -> Result<(), String> {
        self.access(Access::Internal, |book, _| {
            book.pause_queue(pending.id, pending.revision, error)
        })
    }
}
