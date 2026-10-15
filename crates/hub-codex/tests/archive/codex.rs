use std::fs;
use std::path::Path;

use hub_agent::archive::{ArchiveError, SessionArchive};
use hub_codex::archive::CodexArchive;
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};

const ID: &str = "019a0b1c-2d3e-7f50-8172-a5b6c7d8e9f0";

fn rel(raw: &str) -> RelPath {
    RelPath::parse(raw).unwrap()
}

fn write(root: &Path, raw: &str) {
    let path = rel(raw).under(root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "{\"type\":\"session_meta\"}\n").unwrap();
}

fn id() -> SessionId {
    SessionId::parse(ID).unwrap()
}

fn cwd() -> AbsolutePath {
    AbsolutePath::new(std::env::temp_dir()).unwrap()
}

fn archive(root: &Path) -> CodexArchive {
    CodexArchive::new(root.to_path_buf(), "0.160.0".to_owned())
}

fn live_name() -> String {
    format!("rollout-2026-10-09T15-30-12-{ID}.jsonl")
}

#[test]
fn export_copies_every_rollout_of_the_session_and_nothing_else() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), &format!("sessions/2026/10/09/{}", live_name()));
    write(
        root.path(),
        &format!("sessions/2026/10/10/rollout-2026-10-10T09-00-00-{ID}_2.jsonl.zst"),
    );
    write(root.path(), "sessions/2026/10/09/rollout-2026-10-09T15-30-12-other.jsonl");
    write(root.path(), "auth.json");
    write(root.path(), "state_5.sqlite");
    let dest = tempfile::tempdir().unwrap();

    let exported = archive(root.path()).export(&id(), &cwd(), dest.path()).unwrap();

    assert_eq!(
        exported.files,
        [
            rel(&format!("sessions/2026/10/09/{}", live_name())),
            rel(&format!("sessions/2026/10/10/rollout-2026-10-10T09-00-00-{ID}_2.jsonl.zst")),
        ]
    );
    assert_eq!(exported.data, BackendData::Codex);
    assert!(
        !dest.path().join("auth.json").exists() && !dest.path().join("state_5.sqlite").exists()
    );
    assert!(
        !dest.path().join("sessions/2026/10/09/rollout-2026-10-09T15-30-12-other.jsonl").exists()
    );
}

#[test]
fn an_archived_session_names_the_unarchive_command() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), &format!("archived_sessions/{}", live_name()));
    let dest = tempfile::tempdir().unwrap();
    let exported = archive(root.path()).export(&id(), &cwd(), dest.path());
    assert!(matches!(exported, Err(ArchiveError::Archived(ref archived)) if archived == ID));
    assert!(matches!(
        archive(root.path()).present(&id(), &cwd(), &BackendData::Codex),
        Err(ArchiveError::Archived(_))
    ));
    assert_eq!(
        ArchiveError::Archived(ID.to_owned()).to_string(),
        format!("Сессия в архиве Codex: выполните `codex unarchive {ID}`")
    );
}

#[test]
fn a_missing_session_is_not_found_and_not_present() {
    let root = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(
        archive(root.path()).export(&id(), &cwd(), dest.path()),
        Err(ArchiveError::NotFound)
    ));
    assert!(!archive(root.path()).present(&id(), &cwd(), &BackendData::Codex).unwrap());
}

#[test]
fn import_files_rollouts_by_their_date() {
    let snapshot = tempfile::tempdir().unwrap();
    let name = live_name();
    write(snapshot.path(), &format!("sessions/2026/10/09/{name}"));
    let fresh = tempfile::tempdir().unwrap();
    let files = [rel(&format!("sessions/2026/10/09/{name}"))];

    archive(fresh.path())
        .import(&id(), &BackendData::Codex, &files, snapshot.path(), &cwd())
        .unwrap();

    assert!(rel(&format!("sessions/2026/10/09/{name}")).under(fresh.path()).is_file());
    assert!(archive(fresh.path()).present(&id(), &cwd(), &BackendData::Codex).unwrap());
}

#[test]
fn import_refuses_foreign_or_misdated_paths() {
    let snapshot = tempfile::tempdir().unwrap();
    write(snapshot.path(), "auth.json");
    write(snapshot.path(), &format!("sessions/2026/01/01/{}", live_name()));
    write(snapshot.path(), "sessions/2026/10/09/rollout-2026-10-09T15-30-12-other.jsonl");
    let fresh = tempfile::tempdir().unwrap();
    for file in [
        "auth.json",
        &format!("sessions/2026/01/01/{}", live_name()),
        "sessions/2026/10/09/rollout-2026-10-09T15-30-12-other.jsonl",
    ] {
        let result = archive(fresh.path()).import(
            &id(),
            &BackendData::Codex,
            &[rel(file)],
            snapshot.path(),
            &cwd(),
        );
        assert!(matches!(result, Err(ArchiveError::Malformed(_))), "{file}");
    }
    assert_eq!(fs::read_dir(fresh.path()).unwrap().count(), 0);
}

#[test]
fn import_needs_a_rollout_of_this_session_among_the_files() {
    let snapshot = tempfile::tempdir().unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let result =
        archive(fresh.path()).import(&id(), &BackendData::Codex, &[], snapshot.path(), &cwd());
    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    assert_eq!(fs::read_dir(fresh.path()).unwrap().count(), 0);
}

#[test]
fn import_refuses_when_a_rollout_already_exists_elsewhere() {
    let snapshot = tempfile::tempdir().unwrap();
    let name = live_name();
    write(snapshot.path(), &format!("sessions/2026/10/09/{name}"));
    let files = [rel(&format!("sessions/2026/10/09/{name}"))];
    for existing in [
        format!("sessions/2026/10/11/rollout-2026-10-11T01-00-00-{ID}.jsonl"),
        format!("archived_sessions/{name}"),
    ] {
        let fresh = tempfile::tempdir().unwrap();
        write(fresh.path(), &existing);
        let result = archive(fresh.path()).import(
            &id(),
            &BackendData::Codex,
            &files,
            snapshot.path(),
            &cwd(),
        );
        assert!(matches!(result, Err(ArchiveError::Ambiguous)), "{existing}");
        assert!(!rel(&format!("sessions/2026/10/09/{name}")).under(fresh.path()).exists());
    }
}

#[test]
fn import_refuses_another_backend() {
    let snapshot = tempfile::tempdir().unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let result =
        archive(fresh.path()).import(&id(), &BackendData::Qwen, &[], snapshot.path(), &cwd());
    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
}

#[cfg(unix)]
#[test]
fn a_linked_sessions_directory_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    write(elsewhere.path(), &format!("2026/10/09/{}", live_name()));
    std::os::unix::fs::symlink(elsewhere.path(), root.path().join("sessions")).unwrap();
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(
        archive(root.path()).export(&id(), &cwd(), dest.path()),
        Err(ArchiveError::Link(_))
    ));
    assert!(matches!(
        archive(root.path()).present(&id(), &cwd(), &BackendData::Codex),
        Err(ArchiveError::Link(_))
    ));
}

#[cfg(unix)]
#[test]
fn a_linked_rollout_is_skipped() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "secret.jsonl");
    let dir = root.path().join("sessions/2026/10/09");
    fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.jsonl"), dir.join(live_name())).unwrap();
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(
        archive(root.path()).export(&id(), &cwd(), dest.path()),
        Err(ArchiveError::NotFound)
    ));
}
