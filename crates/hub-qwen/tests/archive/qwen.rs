use std::fs;
use std::path::Path;

use hub_agent::archive::{ArchiveError, SessionArchive};
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};
use hub_qwen::archive::{Case, QwenArchive, sanitize};
use serde_json::Value;

const ID: &str = "4f1c2d3e-0b50-4172-8394-a5b6c7d8e9f0";

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

fn cwd(name: &str) -> AbsolutePath {
    AbsolutePath::new(std::env::temp_dir().join(name)).unwrap()
}

fn san(cwd: &AbsolutePath) -> String {
    sanitize(cwd.as_path().to_str().unwrap(), Case::native())
}

fn archive(root: &Path) -> QwenArchive {
    QwenArchive::new(root.to_path_buf(), "0.25.0".to_owned())
}

fn record(cwd: &AbsolutePath) -> String {
    serde_json::json!({"uuid": "u-1", "sessionId": ID, "type": "user", "cwd": cwd.as_path(), "version": "0.25.0"})
        .to_string()
}

fn chat(root: &Path, cwd: &AbsolutePath) -> std::path::PathBuf {
    rel(&format!("projects/{}/chats/{ID}.jsonl", san(cwd))).under(root)
}

fn first_cwd(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap();
    let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    first.get("cwd").and_then(Value::as_str).unwrap().to_owned()
}

/// A `~/.qwen` with the session in directory `name`, its extras, another session and secrets.
fn qwen_home_for(name: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let project = format!("projects/{}", san(&cwd(name)));
    write(
        root.path(),
        &format!("{project}/chats/{ID}.jsonl"),
        &format!("{}\nnot json\n", record(&cwd(name))),
    );
    write(root.path(), &format!("{project}/chats/{ID}.pr.json"), "{}");
    write(root.path(), &format!("{project}/chats/{ID}.runtime.json"), "{\"workDir\":\"old\"}");
    write(root.path(), &format!("{project}/chats/{ID}.worktree.json"), "{}");
    write(root.path(), &format!("{project}/chats/other.jsonl"), "{}\n");
    write(
        root.path(),
        &format!("{project}/subagents/{ID}/agent-1.jsonl"),
        &format!("{}\n", record(&cwd(name))),
    );
    write(root.path(), &format!("todos/{ID}.json"), "{\"todos\":[]}");
    write(root.path(), "settings.json", "{\"apiKey\":\"secret\"}");
    write(root.path(), "oauth_creds.json", "secret");
    root
}

fn qwen_home() -> tempfile::TempDir {
    qwen_home_for("shop")
}

fn exported_files(project: &str) -> Vec<RelPath> {
    vec![
        rel(&format!("projects/{project}/chats/{ID}.jsonl")),
        rel(&format!("projects/{project}/chats/{ID}.pr.json")),
        rel(&format!("projects/{project}/subagents/{ID}/agent-1.jsonl")),
        rel(&format!("todos/{ID}.json")),
    ]
}

/// Exports the session of `root` from directory `name` into a fresh snapshot.
fn export_from(root: &Path, name: &str) -> (tempfile::TempDir, Vec<RelPath>) {
    let snapshot = tempfile::tempdir().unwrap();
    let files = archive(root).export(&id(), &cwd(name), snapshot.path()).unwrap().files;
    (snapshot, files)
}

#[test]
fn export_takes_the_chat_its_extras_and_todos_but_no_secrets() {
    let root = qwen_home();
    let dest = tempfile::tempdir().unwrap();

    let exported = archive(root.path()).export(&id(), &cwd("shop"), dest.path()).unwrap();

    assert_eq!(exported.files, exported_files(&san(&cwd("shop"))));
    assert_eq!(exported.data, BackendData::Qwen);
    for secret in ["settings.json", "oauth_creds.json"] {
        assert!(!dest.path().join(secret).exists(), "{secret}");
    }
}

#[test]
fn a_session_of_another_project_is_not_found() {
    let root = qwen_home();
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(
        archive(root.path()).export(&id(), &cwd("other"), dest.path()),
        Err(ArchiveError::NotFound)
    ));
    assert!(archive(root.path()).present(&id(), &cwd("shop"), &BackendData::Qwen).unwrap());
    assert!(!archive(root.path()).present(&id(), &cwd("other"), &BackendData::Qwen).unwrap());
}

#[test]
fn the_same_directory_gets_the_files_unchanged() {
    let root = qwen_home();
    let (snapshot, files) = export_from(root.path(), "shop");
    let fresh = tempfile::tempdir().unwrap();

    archive(fresh.path())
        .import(&id(), &BackendData::Qwen, &files, snapshot.path(), &cwd("shop"))
        .unwrap();

    for file in &files {
        assert_eq!(
            fs::read(file.under(fresh.path())).unwrap(),
            fs::read(file.under(snapshot.path())).unwrap()
        );
    }
}

#[test]
fn a_new_directory_gets_the_files_under_its_project_with_cwd_rewritten() {
    let root = qwen_home();
    let (snapshot, files) = export_from(root.path(), "shop");
    let fresh = tempfile::tempdir().unwrap();
    let target = cwd("moved");

    archive(fresh.path())
        .import(&id(), &BackendData::Qwen, &files, snapshot.path(), &target)
        .unwrap();

    assert!(archive(fresh.path()).present(&id(), &target, &BackendData::Qwen).unwrap());
    let text = fs::read_to_string(chat(fresh.path(), &target)).unwrap();
    let mut lines = text.lines();
    let first: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(first["cwd"], Value::from(target.as_path().to_str().unwrap()));
    assert_eq!((first["sessionId"].as_str(), first["uuid"].as_str()), (Some(ID), Some("u-1")));
    assert_eq!(lines.next(), Some("not json"));
    let agent =
        rel(&format!("projects/{}/subagents/{ID}/agent-1.jsonl", san(&target))).under(fresh.path());
    assert!(fs::read_to_string(agent).unwrap().contains("moved"));
    assert!(!rel(&format!("projects/{}", san(&cwd("shop")))).under(fresh.path()).exists());
}

#[test]
fn restoring_into_another_directory_keeps_the_existing_todos() {
    let root = qwen_home();
    let (snapshot, files) = export_from(root.path(), "shop");
    let todos = rel(&format!("todos/{ID}.json")).under(root.path());
    fs::write(&todos, "{\"todos\":[\"still mine\"]}").unwrap();
    let target = cwd("moved");

    archive(root.path())
        .import(&id(), &BackendData::Qwen, &files, snapshot.path(), &target)
        .unwrap();

    assert_eq!(fs::read_to_string(&todos).unwrap(), "{\"todos\":[\"still mine\"]}");
    assert!(archive(root.path()).present(&id(), &target, &BackendData::Qwen).unwrap());
    assert_eq!(
        first_cwd(&chat(root.path(), &cwd("shop"))),
        cwd("shop").as_path().to_str().unwrap()
    );
}

#[test]
fn directories_that_sanitize_alike_are_told_apart_by_the_recorded_cwd() {
    let (dash, under) = (cwd("a-b"), cwd("a_b"));
    assert_eq!(san(&dash), san(&under));
    let root = qwen_home_for("a-b");
    let (snapshot, files) = export_from(root.path(), "a-b");

    let occupied = archive(root.path()).present(&id(), &under, &BackendData::Qwen);
    assert!(
        matches!(occupied, Err(ArchiveError::Occupied { ref cwd, .. }) if cwd == dash.as_path().to_str().unwrap())
    );
    assert_eq!(
        occupied.unwrap_err().to_string(),
        format!("В Qwen уже есть сессия {ID} для каталога {}", dash.as_path().display())
    );

    for file in &files {
        fs::remove_file(file.under(root.path())).unwrap();
    }
    assert!(!archive(root.path()).present(&id(), &under, &BackendData::Qwen).unwrap());
    archive(root.path())
        .import(&id(), &BackendData::Qwen, &files, snapshot.path(), &under)
        .unwrap();

    assert_eq!(first_cwd(&chat(root.path(), &under)), under.as_path().to_str().unwrap());
    assert!(archive(root.path()).present(&id(), &under, &BackendData::Qwen).unwrap());
    assert!(matches!(
        archive(root.path()).present(&id(), &dash, &BackendData::Qwen),
        Err(ArchiveError::Occupied { .. })
    ));
}

#[test]
fn import_never_writes_a_second_copy_of_the_chat() {
    let (dash, under) = (cwd("a-b"), cwd("a_b"));
    let root = qwen_home_for("a-b");
    let (snapshot, files) = export_from(root.path(), "a-b");
    let before = fs::read(chat(root.path(), &dash)).unwrap();

    let same =
        archive(root.path()).import(&id(), &BackendData::Qwen, &files, snapshot.path(), &dash);
    let other =
        archive(root.path()).import(&id(), &BackendData::Qwen, &files, snapshot.path(), &under);

    assert!(matches!(same, Err(ArchiveError::Ambiguous)));
    assert!(matches!(other, Err(ArchiveError::Occupied { .. })));
    assert_eq!(fs::read(chat(root.path(), &dash)).unwrap(), before);
}

#[test]
fn import_refuses_files_that_are_not_this_session() {
    let root = qwen_home();
    let (snapshot, mut files) = export_from(root.path(), "shop");
    write(snapshot.path(), "settings.json", "planted");
    files.push(rel("settings.json"));
    let fresh = tempfile::tempdir().unwrap();
    let result = archive(fresh.path()).import(
        &id(),
        &BackendData::Qwen,
        &files,
        snapshot.path(),
        &cwd("shop"),
    );
    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    let mixed = vec![
        rel(&format!("projects/a/chats/{ID}.jsonl")),
        rel(&format!("projects/b/chats/{ID}.pr.json")),
    ];
    let result = archive(fresh.path()).import(
        &id(),
        &BackendData::Qwen,
        &mixed,
        snapshot.path(),
        &cwd("shop"),
    );
    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    assert_eq!(fs::read_dir(fresh.path()).unwrap().count(), 0);
}

#[test]
fn import_requires_this_sessions_chat_itself() {
    let root = qwen_home();
    let (snapshot, files) = export_from(root.path(), "shop");
    let fresh = tempfile::tempdir().unwrap();
    let without_chat: Vec<RelPath> = files
        .iter()
        .filter(|file| Path::new(file.as_str()).extension() != Some("jsonl".as_ref()))
        .cloned()
        .collect();
    let result = archive(fresh.path()).import(
        &id(),
        &BackendData::Qwen,
        &without_chat,
        snapshot.path(),
        &cwd("shop"),
    );
    assert!(matches!(result, Err(ArchiveError::Malformed(_))));

    let foreign = serde_json::json!({"sessionId": "another", "cwd": cwd("shop").as_path()});
    fs::write(files.first().unwrap().under(snapshot.path()), format!("{foreign}\n")).unwrap();
    let result = archive(fresh.path()).import(
        &id(),
        &BackendData::Qwen,
        &files,
        snapshot.path(),
        &cwd("shop"),
    );
    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    assert_eq!(fs::read_dir(fresh.path()).unwrap().count(), 0);
}

#[test]
fn a_snapshot_of_another_backend_is_refused() {
    let root = qwen_home();
    let (snapshot, files) = export_from(root.path(), "shop");
    let fresh = tempfile::tempdir().unwrap();
    let result = archive(fresh.path()).import(
        &id(),
        &BackendData::Codex,
        &files,
        snapshot.path(),
        &cwd("shop"),
    );
    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    assert_eq!(fs::read_dir(fresh.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn linked_todos_or_projects_are_refused() {
    let root = qwen_home();
    let (snapshot, files) = export_from(root.path(), "shop");
    let fresh = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), fresh.path().join("projects")).unwrap();
    let result = archive(fresh.path()).import(
        &id(),
        &BackendData::Qwen,
        &files,
        snapshot.path(),
        &cwd("moved"),
    );
    assert!(matches!(result, Err(ArchiveError::Link(_))));
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing written outside the root"
    );
}

#[cfg(unix)]
#[test]
fn a_linked_todos_directory_is_refused_and_nothing_is_written() {
    let root = qwen_home();
    let (snapshot, files) = export_from(root.path(), "shop");
    let fresh = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), fresh.path().join("todos")).unwrap();

    let result = archive(fresh.path()).import(
        &id(),
        &BackendData::Qwen,
        &files,
        snapshot.path(),
        &cwd("moved"),
    );

    assert!(matches!(result, Err(ArchiveError::Link(_))));
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing written outside the root"
    );
    assert!(!fresh.path().join("projects").exists(), "nothing written inside the root");
}

#[cfg(unix)]
#[test]
fn a_linked_todos_file_is_refused_and_left_alone() {
    let root = qwen_home();
    let (snapshot, files) = export_from(root.path(), "shop");
    let fresh = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("mine.json");
    fs::write(&target, "mine").unwrap();
    fs::create_dir(fresh.path().join("todos")).unwrap();
    std::os::unix::fs::symlink(&target, fresh.path().join(format!("todos/{ID}.json"))).unwrap();

    let result = archive(fresh.path()).import(
        &id(),
        &BackendData::Qwen,
        &files,
        snapshot.path(),
        &cwd("moved"),
    );

    assert!(matches!(result, Err(ArchiveError::Link(_))));
    assert_eq!(fs::read_to_string(&target).unwrap(), "mine");
    assert!(!fresh.path().join("projects").exists(), "nothing written inside the root");
}

#[test]
fn rewriting_keeps_line_breaks_as_they_were() {
    let root = qwen_home();
    let (snapshot, files) = export_from(root.path(), "shop");
    let source = files.first().unwrap().under(snapshot.path());
    fs::write(&source, format!("{}\r\n\r\nlast", record(&cwd("shop")))).unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let target = cwd("moved");

    archive(fresh.path())
        .import(&id(), &BackendData::Qwen, &files, snapshot.path(), &target)
        .unwrap();

    let text = fs::read_to_string(chat(fresh.path(), &target)).unwrap();
    assert_eq!(text, format!("{}\r\n\r\nlast", record(&target)));
}
