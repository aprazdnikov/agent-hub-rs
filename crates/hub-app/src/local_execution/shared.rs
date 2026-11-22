use super::{Answer, Control, ExecutionSettings, Live, Request, Runtime};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

/// The second lane is available only when both workers use separate worktrees.
pub struct SharedRuntime(Mutex<Option<Vec<Runtime>>>);
impl SharedRuntime {
    #[must_use]
    pub fn new(runtime: Runtime) -> Self {
        let second =
            Runtime::new(runtime.service.clone(), runtime.executor.clone(), runtime.git.clone());
        Self(Mutex::new(Some(vec![runtime, second])))
    }
    fn access<T>(&self, operation: impl FnOnce(&mut [Runtime]) -> T) -> Result<T, String> {
        let mut guard =
            self.0.lock().map_err(|_| "Исполнитель недоступен; перезапустите приложение")?;
        Ok(operation(guard.as_mut().ok_or("Приложение завершает работу")?))
    }
    pub async fn shutdown(&self) {
        let runtimes = self.0.lock().ok().and_then(|mut guard| guard.take());
        if let Some(runtimes) = runtimes {
            futures::future::join_all(
                runtimes.into_iter().map(|mut r| async move { r.shutdown().await }),
            )
            .await;
        }
    }
}
fn capacity(lanes: &[Runtime], isolated: bool) -> bool {
    lanes.iter().any(|r| !r.busy())
        && lanes.iter().filter(|r| r.busy()).all(|r| isolated && r.isolated)
}
impl Control for SharedRuntime {
    fn busy(&self) -> bool {
        self.access(|r| r.iter().any(Runtime::busy)).unwrap_or(true)
    }
    fn capacity(&self, isolated: bool) -> bool {
        self.access(|r| capacity(r, isolated)).unwrap_or(false)
    }
    fn snapshots(&self) -> Vec<Live> {
        self.access(|r| r.iter().filter_map(Runtime::snapshot).collect()).unwrap_or_default()
    }
    fn snapshot(&self) -> Option<Live> {
        self.snapshots().into_iter().max_by_key(|v| (v.active, v.interaction.is_some()))
    }
    fn snapshot_for(&self, project: &Path, id: u64) -> Option<Live> {
        self.access(|r| {
            r.iter().filter_map(|r| r.snapshot_for(project, id)).max_by_key(|v| (v.active, v.run))
        })
        .ok()
        .flatten()
    }
    fn dispatch(
        &self,
        project: &Path,
        request: Request,
        settings: Arc<ExecutionSettings>,
    ) -> Result<(), String> {
        self.access(|lanes| {
            let first = lanes.first().ok_or("Исполнитель недоступен")?;
            let isolation = match &request {
                Request::RunQueued { id, .. } => {
                    first.service.read()?.get(*id)?.queue.as_ref().is_some_and(|q| q.isolated)
                }
                Request::StartChecked { id, .. } => first.service.read()?.get(*id)?.isolated,
                _ => false,
            };
            if !capacity(lanes, isolation) {
                return Err("Исполнитель занят".into());
            }
            lanes
                .iter_mut()
                .find(|r| !r.busy())
                .ok_or("Исполнитель занят")?
                .dispatch(project, request, settings)
        })?
    }
    fn stop(&self, token: Uuid) -> Result<(), String> {
        self.access(|r| {
            r.iter()
                .find(|r| r.job.as_ref().is_some_and(|j| j.token == token))
                .ok_or("Запуск завершён")?
                .stop(token)
        })?
    }
    fn stop_preflight(&self, token: Uuid) -> Result<(), String> {
        self.access(|lanes| {
            for lane in lanes.iter() {
                // Hold the watch read lock while cancelling: the worker publishes run before
                // first polling the CLI, so a stale Git stop cannot cancel a running agent.
                let view = lane.view.borrow();
                if view
                    .as_ref()
                    .is_some_and(|live| live.active && live.token == token && live.run.is_none())
                {
                    return lane.stop(token);
                }
            }
            Err("Подготовка уже завершена".into())
        })?
    }
    fn respond(&self, token: Uuid, id: Uuid, answer: Answer) -> Result<(), String> {
        self.access(|r| {
            r.iter()
                .find(|r| r.job.as_ref().is_some_and(|j| j.token == token))
                .ok_or("Запуск завершён")?
                .respond(token, id, answer)
        })?
    }
}
