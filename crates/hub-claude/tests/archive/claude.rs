use std::fs;
use std::io;
use std::path::Path;

use hub_agent::archive::{ArchiveError, SessionArchive};
use hub_claude::archive::ClaudeArchive;
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};

const ID: &str = "0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0";

fn rel(raw: &str) -> RelPath {
    RelPath::parse(raw).unwrap()
}

fn write(root: &Path, raw: &str, text: &str) {
    let path = rel(raw).under(root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn id() -> SessionId {
    SessionId::parse(ID).unwrap()
}

fn cwd() -> AbsolutePath {
    AbsolutePath::new(std::env::temp_dir()).unwrap()
}

fn archive(root: &Path) -> ClaudeArchive {
    ClaudeArchive::new(root.to_path_buf(), "2.1.292".to_owned())
}

/// A `~/.claude` with the session, its side directory, another session and credentials.
fn claude_home() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), &format!("projects/-work-shop/{ID}.jsonl"), "{\"type\":\"user\"}\n");
    write(root.path(), &format!("projects/-work-shop/{ID}/subagents/agent-1.jsonl"), "{}\n");
    write(root.path(), &format!("projects/-work-shop/{ID}/tool-results/t.txt"), "out");
    write(root.path(), "projects/-work-shop/other.jsonl", "{}\n");
    write(root.path(), ".credentials.json", "secret");
    root
}

#[test]
fn export_copies_the_transcript_and_its_directory_only() {
    let root = claude_home();
    let dest = tempfile::tempdir().unwrap();

    let exported = archive(root.path()).export(&id(), &cwd(), dest.path()).unwrap();

    assert_eq!(
        exported.files,
        [
            rel(&format!("projects/-work-shop/{ID}.jsonl")),
            rel(&format!("projects/-work-shop/{ID}/subagents/agent-1.jsonl")),
            rel(&format!("projects/-work-shop/{ID}/tool-results/t.txt")),
        ]
    );
    assert_eq!(exported.data, BackendData::Claude { project: rel("-work-shop") });
    assert_eq!(exported.agent_version, "2.1.292");
    assert!(
        rel(&format!("projects/-work-shop/{ID}/tool-results/t.txt")).under(dest.path()).is_file()
    );
    assert!(!dest.path().join(".credentials.json").exists());
    assert!(!rel("projects/-work-shop/other.jsonl").under(dest.path()).exists());
}

#[test]
fn a_missing_session_is_not_found() {
    let root = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(
        archive(root.path()).export(&id(), &cwd(), dest.path()),
        Err(ArchiveError::NotFound)
    ));
    let data = BackendData::Claude { project: rel("-work-shop") };
    assert!(!archive(root.path()).present(&id(), &cwd(), &data).unwrap());
}

#[test]
fn two_copies_are_ambiguous() {
    let root = claude_home();
    write(root.path(), &format!("projects/-elsewhere/{ID}.jsonl"), "{}\n");
    let dest = tempfile::tempdir().unwrap();
    let data = BackendData::Claude { project: rel("-work-shop") };
    assert!(matches!(
        archive(root.path()).present(&id(), &cwd(), &data),
        Err(ArchiveError::Ambiguous)
    ));
    assert!(matches!(
        archive(root.path()).export(&id(), &cwd(), dest.path()),
        Err(ArchiveError::Ambiguous)
    ));
}

#[test]
fn import_puts_the_session_back_and_never_overwrites() {
    let root = claude_home();
    let snapshot = tempfile::tempdir().unwrap();
    let exported = archive(root.path()).export(&id(), &cwd(), snapshot.path()).unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let target = archive(fresh.path());

    assert!(!target.present(&id(), &cwd(), &exported.data).unwrap());
    target.import(&id(), &exported.data, &exported.files, snapshot.path(), &cwd()).unwrap();
    assert!(target.present(&id(), &cwd(), &exported.data).unwrap());
    assert_eq!(
        fs::read_to_string(rel(&format!("projects/-work-shop/{ID}.jsonl")).under(fresh.path()))
            .unwrap(),
        "{\"type\":\"user\"}\n"
    );

    let again = target.import(&id(), &exported.data, &exported.files, snapshot.path(), &cwd());
    assert!(
        matches!(again, Err(ArchiveError::Io(ref error)) if error.kind() == io::ErrorKind::AlreadyExists)
    );
    assert!(rel(&format!("projects/-work-shop/{ID}.jsonl")).under(fresh.path()).is_file());
}

#[test]
fn import_refuses_paths_outside_the_session() {
    let snapshot = tempfile::tempdir().unwrap();
    write(snapshot.path(), &format!("projects/-work-shop/{ID}.jsonl"), "{}\n");
    write(snapshot.path(), ".credentials.json", "planted");
    let fresh = tempfile::tempdir().unwrap();
    let data = BackendData::Claude { project: rel("-work-shop") };
    for files in [
        vec![rel(&format!("projects/-work-shop/{ID}.jsonl")), rel(".credentials.json")],
        vec![rel(&format!("projects/-other/{ID}.jsonl"))],
        vec![rel(&format!("projects/-work-shop/{ID}/x"))],
    ] {
        let result = archive(fresh.path()).import(&id(), &data, &files, snapshot.path(), &cwd());
        assert!(matches!(result, Err(ArchiveError::Malformed(_))), "{files:?}");
    }
    assert_eq!(fs::read_dir(fresh.path()).unwrap().count(), 0);
}

#[test]
fn import_refuses_another_backends_snapshot() {
    let snapshot = tempfile::tempdir().unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let result =
        archive(fresh.path()).import(&id(), &BackendData::Codex, &[], snapshot.path(), &cwd());
    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
}

#[cfg(unix)]
#[test]
fn a_linked_transcript_is_skipped() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "t.jsonl", "{}\n");
    fs::create_dir_all(root.path().join("projects").join("-work-shop")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("t.jsonl"),
        rel(&format!("projects/-work-shop/{ID}.jsonl")).under(root.path()),
    )
    .unwrap();
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(
        archive(root.path()).export(&id(), &cwd(), dest.path()),
        Err(ArchiveError::NotFound)
    ));
}

#[cfg(unix)]
#[test]
fn a_linked_projects_directory_is_refused_on_export_and_import() {
    let outside = claude_home();
    let root = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path().join("projects"), root.path().join("projects"))
        .unwrap();
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(
        archive(root.path()).export(&id(), &cwd(), dest.path()),
        Err(ArchiveError::Link(_))
    ));
    assert_eq!(fs::read_dir(dest.path()).unwrap().count(), 0);

    let snapshot = tempfile::tempdir().unwrap();
    let exported = archive(outside.path()).export(&id(), &cwd(), snapshot.path()).unwrap();
    let target = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), target.path().join("projects")).unwrap();
    let result = archive(target.path()).import(
        &id(),
        &exported.data,
        &exported.files,
        snapshot.path(),
        &cwd(),
    );
    assert!(matches!(result, Err(ArchiveError::Link(_))));
    assert_eq!(
        fs::read_dir(elsewhere.path()).unwrap().count(),
        0,
        "nothing written outside the root"
    );
}

#[test]
fn import_refuses_a_second_copy_in_another_project() {
    let root = claude_home();
    let snapshot = tempfile::tempdir().unwrap();
    let exported = archive(root.path()).export(&id(), &cwd(), snapshot.path()).unwrap();
    let target = tempfile::tempdir().unwrap();
    write(target.path(), &format!("projects/-elsewhere/{ID}.jsonl"), "{}\n");

    let result = archive(target.path()).import(
        &id(),
        &exported.data,
        &exported.files,
        snapshot.path(),
        &cwd(),
    );

    assert!(matches!(result, Err(ArchiveError::Ambiguous)));
    assert!(!rel("projects/-work-shop").under(target.path()).exists(), "nothing written");
}

#[test]
fn import_needs_the_transcript_itself_not_a_nested_namesake() {
    let snapshot = tempfile::tempdir().unwrap();
    let nested = format!("projects/-work-shop/{ID}/x/{ID}.jsonl");
    write(snapshot.path(), &nested, "{}\n");
    let fresh = tempfile::tempdir().unwrap();
    let data = BackendData::Claude { project: rel("-work-shop") };

    let result =
        archive(fresh.path()).import(&id(), &data, &[rel(&nested)], snapshot.path(), &cwd());

    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    assert_eq!(fs::read_dir(fresh.path()).unwrap().count(), 0);
}
