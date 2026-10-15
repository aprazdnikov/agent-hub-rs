//! The file work of `/save` and `/restore`, off the hub task: the agent CLI is checked, files
//! copied and Hermes run in blocking threads. The hub owns the decisions: it reserves the
//! agent session before any agent file is touched and applies the result in one step.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Datelike, Timelike};
use hub_agent::archive::{ArchiveError, Exported, SessionArchive};
use hub_core::domain::{AbsolutePath, BackendKind, SessionId, TopicKey, TopicSession};
use hub_core::snapshot::{BackendData, Manifest, Minute, RelPath, SnapshotName};
use hub_core::topics::Topics;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinError;

use crate::hub::{Agents, HubMessage, SnapshotDone};
use crate::paths::resolve_cwd;
use crate::snapshots::{Loaded, SnapshotStore};
use crate::texts;

/// One agent session; at most one snapshot job works on it at a time.
pub type Reservation = (BackendKind, SessionId);

/// Errors are the finished text for the topic (`⚠️ …` or the internal-error notice).
pub enum Done {
    Saved {
        key: TopicKey,
        name: SnapshotName,
        reservation: Reservation,
        outcome: Result<Manifest, String>,
    },
    /// `/restore` loaded its snapshot; the hub grants the session (`true`) or refuses and
    /// tells the topic itself (`false`).
    Loaded { key: TopicKey, reservation: Reservation, reply: oneshot::Sender<bool> },
    Restored {
        key: TopicKey,
        /// `None` when the job failed before the hub granted the session, or crashed and
        /// cannot tell: the hub then releases whatever the topic holds.
        reservation: Option<Reservation>,
        outcome: Result<Restoration, String>,
    },
}

pub struct Restoration {
    pub name: SnapshotName,
    pub session: TopicSession,
    pub imported: bool,
}

#[must_use]
pub fn local_minute() -> Minute {
    let now = chrono::Local::now();
    Minute {
        year: now.year(),
        month: now.month(),
        day: now.day(),
        hour: now.hour(),
        minute: now.minute(),
    }
}

/// UTC `YYYY-MM-DDTHH:MM:SSZ`, the manifest's `created_at`.
#[must_use]
pub fn utc_now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Topics other than `key` bound to the same agent session.
#[must_use]
pub fn holders(topics: &Topics, key: TopicKey, session: &TopicSession) -> Vec<TopicKey> {
    topics
        .iter()
        .filter(|(other, bound)| {
            **other != key
                && bound.backend == session.backend
                && bound.session.is_some()
                && bound.session == session.session
        })
        .map(|(other, _)| *other)
        .collect()
}

/// Runs a job in its own task and watches it: if the task dies (a panic in a backend), the
/// hub still gets `crashed`, so no reservation stays locked until a restart.
pub fn supervise(
    job: impl Future<Output = ()> + Send + 'static,
    mailbox: mpsc::Sender<HubMessage>,
    crashed: Done,
) {
    let task = tokio::spawn(job);
    tokio::spawn(async move {
        if let Err(error) = task.await {
            tracing::error!(panic = error.is_panic(), "snapshot job crashed");
            report(&mailbox, crashed).await;
        }
    });
}

fn internal(error: &JoinError) -> String {
    tracing::error!(panic = error.is_panic(), "snapshot file work crashed");
    texts::INTERNAL_ERROR.to_owned()
}

async fn report(mailbox: &mpsc::Sender<HubMessage>, done: Done) {
    // The hub is gone only during shutdown; a restore is then simply repeated later.
    let _ = mailbox.send(HubMessage::Snapshot(SnapshotDone(done))).await;
}

pub struct SaveJob<A> {
    pub key: TopicKey,
    pub name: SnapshotName,
    pub title: Option<String>,
    pub session: TopicSession,
    pub id: SessionId,
    pub created_at: String,
    pub store: SnapshotStore,
    pub home: PathBuf,
    pub agents: Arc<A>,
    pub mailbox: mpsc::Sender<HubMessage>,
}

impl<A: Agents> SaveJob<A> {
    /// The hub has already reserved `(session.backend, id)` for this job.
    pub async fn run(self) {
        let (key, name, mailbox) = (self.key, self.name.clone(), self.mailbox.clone());
        let reservation = (self.session.backend, self.id.clone());
        let outcome = self.save().await;
        // Error texts can carry agent or storage paths: the user sees them, the log does not.
        if outcome.is_err() {
            tracing::warn!(
                snapshot = name.as_str(),
                backend = reservation.0.name(),
                "snapshot not saved"
            );
        }
        report(&mailbox, Done::Saved { key, name, reservation, outcome }).await;
    }

    async fn save(self) -> Result<Manifest, String> {
        let Self {
            key: _key,
            name,
            title,
            session,
            id,
            created_at,
            store,
            home,
            agents,
            mailbox: _mailbox,
        } = self;
        let archive = agents
            .archive(session.backend, &home)
            .await
            .map_err(|reason| texts::warning(&reason))?;
        tokio::task::spawn_blocking(move || {
            store.save(&name, |agent| {
                let Exported { files, data, agent_version } =
                    archive.export(&id, &session.cwd, agent)?;
                if data.backend() != session.backend {
                    return Err(ArchiveError::Malformed(
                        "архив вернул данные другого бэкенда".to_owned(),
                    ));
                }
                Ok(Manifest {
                    name: name.clone(),
                    created_at,
                    title,
                    cwd: session.cwd.clone(),
                    session: id.clone(),
                    agent_version,
                    data,
                    files,
                })
            })
        })
        .await
        .map_err(|error| internal(&error))?
        .map_err(|error| texts::warning(&error))
    }
}

pub struct RestoreJob<A> {
    pub key: TopicKey,
    pub name: SnapshotName,
    /// An explicit directory already resolved inside the root; `None` — the snapshot's own.
    pub cwd: Option<AbsolutePath>,
    pub root: AbsolutePath,
    pub store: SnapshotStore,
    pub home: PathBuf,
    pub agents: Arc<A>,
    pub mailbox: mpsc::Sender<HubMessage>,
}

/// What phase one found: everything `present`/`import` need.
struct Plan {
    session: SessionId,
    data: BackendData,
    files: Vec<RelPath>,
    agent: PathBuf,
    cwd: AbsolutePath,
}

impl<A: Agents> RestoreJob<A> {
    pub async fn run(self) {
        let Self { key, name, cwd, root, store, home, agents, mailbox } = self;
        let plan = match load(&name, cwd, root, store, home.clone()).await {
            Ok(plan) => plan,
            Err(reason) => return finish(&mailbox, key, &name, None, Err(reason)).await,
        };
        let reservation = (plan.data.backend(), plan.session.clone());
        let (reply, granted) = oneshot::channel();
        report(&mailbox, Done::Loaded { key, reservation: reservation.clone(), reply }).await;
        // `false` or a hub gone away: the hub already told the topic, nothing was touched.
        if !matches!(granted.await, Ok(true)) {
            return;
        }
        let outcome = apply(name.clone(), plan, &*agents, &home).await;
        finish(&mailbox, key, &name, Some(reservation), outcome).await;
    }
}

async fn finish(
    mailbox: &mpsc::Sender<HubMessage>,
    key: TopicKey,
    name: &SnapshotName,
    reservation: Option<Reservation>,
    outcome: Result<Restoration, String>,
) {
    // Error texts can carry agent or storage paths: the user sees them, the log does not.
    if let Ok(restoration) = &outcome {
        tracing::info!(
            snapshot = name.as_str(),
            backend = restoration.session.backend.name(),
            imported = restoration.imported,
            "snapshot restored"
        );
    } else {
        tracing::warn!(
            snapshot = name.as_str(),
            backend = reservation.as_ref().map_or("—", |(backend, _)| backend.name()),
            "snapshot not restored"
        );
    }
    report(mailbox, Done::Restored { key, reservation, outcome }).await;
}

/// Phase one: the snapshot is checked and its directory resolved inside the root; no agent
/// file is touched yet.
async fn load(
    name: &SnapshotName,
    cwd: Option<AbsolutePath>,
    root: AbsolutePath,
    store: SnapshotStore,
    home: PathBuf,
) -> Result<Plan, String> {
    let wanted = name.clone();
    tokio::task::spawn_blocking(move || -> Result<Plan, String> {
        let Loaded { manifest, agent } =
            store.load(&wanted).map_err(|error| texts::warning(&error))?;
        let Manifest {
            name: _name,
            created_at: _created_at,
            title: _title,
            cwd: saved,
            session,
            agent_version: _agent_version,
            data,
            files,
        } = manifest;
        let cwd = match cwd {
            Some(cwd) => cwd,
            None => resolve_cwd(&root, &home, Some(&saved.as_path().display().to_string()))
                .map_err(|error| texts::warning(&error))?,
        };
        Ok(Plan { session, data, files, agent, cwd })
    })
    .await
    .map_err(|error| internal(&error))?
}

/// Phase two, with the session reserved: nothing is imported if the agent still has it.
async fn apply<A: Agents>(
    name: SnapshotName,
    plan: Plan,
    agents: &A,
    home: &Path,
) -> Result<Restoration, String> {
    let Plan { session, data, files, agent, cwd } = plan;
    let backend = data.backend();
    let archive: Arc<dyn SessionArchive> =
        agents.archive(backend, home).await.map_err(|reason| texts::warning(&reason))?;
    let (id, target) = (session.clone(), cwd.clone());
    let imported = tokio::task::spawn_blocking(move || -> Result<bool, ArchiveError> {
        if archive.present(&id, &target, &data)? {
            return Ok(false);
        }
        archive.import(&id, &data, &files, &agent, &target)?;
        Ok(true)
    })
    .await
    .map_err(|error| internal(&error))?
    .map_err(|error| texts::warning(&error))?;
    Ok(Restoration {
        name,
        session: TopicSession { backend, cwd, session: Some(session) },
        imported,
    })
}

#[cfg(test)]
mod tests {
    use hub_core::domain::{AbsolutePath, BackendKind, ChatId, SessionId, ThreadId};

    use super::*;

    fn key(thread: i32) -> TopicKey {
        TopicKey { chat: ChatId(-100), thread: ThreadId(thread) }
    }

    fn session(backend: BackendKind, id: Option<&str>) -> TopicSession {
        TopicSession::fresh(backend, AbsolutePath::new(std::env::temp_dir()).unwrap())
            .with_session(id.and_then(SessionId::parse))
    }

    #[test]
    fn holders_are_other_topics_with_the_same_agent_session() {
        let moved = session(BackendKind::Claude, Some("s-1"));
        let topics = Topics::from([
            (key(1), moved.clone()),
            (key(2), session(BackendKind::Claude, Some("s-1"))),
            (key(3), session(BackendKind::Codex, Some("s-1"))),
            (key(4), session(BackendKind::Claude, Some("s-2"))),
            (key(5), session(BackendKind::Claude, None)),
        ]);
        assert_eq!(holders(&topics, key(1), &moved), [key(2)]);
    }

    #[test]
    fn created_at_is_utc_to_the_second() {
        let stamp = utc_now();
        assert_eq!(stamp.len(), "2026-10-09T15:30:12Z".len());
        assert!(chrono::NaiveDateTime::parse_from_str(&stamp, "%Y-%m-%dT%H:%M:%SZ").is_ok());
    }
}
