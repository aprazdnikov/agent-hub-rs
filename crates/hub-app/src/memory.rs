//! Adapter from the memory menu to the single settings writer.
use crate::supervisor::{Command, Snapshot};
use futures::future::BoxFuture;
use hub_telegram::memory::MemoryControl;
use tokio::sync::{mpsc, oneshot, watch};

pub struct Control {
    pub commands: mpsc::Sender<Command>,
    pub snapshot: watch::Receiver<Snapshot>,
}
impl MemoryControl for Control {
    fn set_enabled(&self, enabled: bool) -> BoxFuture<'static, Result<(), String>> {
        let commands = self.commands.clone();
        let revision = self.snapshot.borrow().save_revision;
        Box::pin(async move {
            let (reply, received) = oneshot::channel();
            commands
                .send(Command::SetMemory { enabled, revision, reply })
                .await
                .map_err(|_| "Приложение остановлено")?;
            tokio::time::timeout(std::time::Duration::from_secs(20), received)
                .await
                .map_err(|_| {
                    "Сохранение ещё выполняется; проверьте состояние перед повтором".to_owned()
                })?
                .map_err(|_| "Результат сохранения неизвестен".to_owned())?
        })
    }
}
