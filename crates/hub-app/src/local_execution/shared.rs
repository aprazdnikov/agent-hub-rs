use super::{Answer, Control, ExecutionSettings, Live, Request, Runtime};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

pub struct SharedRuntime(Mutex<Option<Runtime>>);
impl SharedRuntime {
    #[must_use]
    pub fn new(runtime: Runtime) -> Self {
        Self(Mutex::new(Some(runtime)))
    }
    fn access<T>(&self, operation: impl FnOnce(&mut Runtime) -> T) -> Result<T, String> {
        let mut guard =
            self.0.lock().map_err(|_| "Исполнитель недоступен; перезапустите приложение")?;
        let runtime = guard.as_mut().ok_or("Приложение завершает работу")?;
        Ok(operation(runtime))
    }
    pub async fn shutdown(&self) {
        let runtime = self.0.lock().ok().and_then(|mut guard| guard.take());
        if let Some(mut runtime) = runtime {
            runtime.shutdown().await;
        }
    }
}
impl Control for SharedRuntime {
    fn busy(&self) -> bool {
        self.access(|runtime| runtime.busy()).unwrap_or(true)
    }
    fn snapshot(&self) -> Option<Live> {
        self.access(|runtime| runtime.snapshot()).ok().flatten()
    }
    fn snapshot_for(&self, project: &Path, id: u64) -> Option<Live> {
        self.access(|runtime| runtime.snapshot_for(project, id)).ok().flatten()
    }
    fn dispatch(
        &self,
        project: &Path,
        request: Request,
        settings: Arc<ExecutionSettings>,
    ) -> Result<(), String> {
        self.access(|runtime| runtime.dispatch(project, request, settings))?
    }
    fn stop(&self, token: Uuid) -> Result<(), String> {
        self.access(|runtime| runtime.stop(token))?
    }
    fn respond(&self, token: Uuid, id: Uuid, answer: Answer) -> Result<(), String> {
        self.access(|runtime| runtime.respond(token, id, answer))?
    }
}
