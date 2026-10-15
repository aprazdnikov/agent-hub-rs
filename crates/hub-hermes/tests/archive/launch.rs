//! The Rust side of the script protocol, through a checked-in fake Python.

use std::fs;
use std::path::{Path, PathBuf};

use hub_agent::archive::{ArchiveError, SessionArchive};
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};
use hub_hermes::archive::{HermesArchive, SESSION_FILE};

struct Fake {
    dir: tempfile::TempDir,
    cli: PathBuf,
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

/// A directory with `python3` → the fake Python and the `mode` it answers in. Under the target
/// directory, so a fixture can be hard-linked into it (same file system as the checkout).
fn fake_dir(mode: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    std::os::unix::fs::symlink(fixture("fake-python"), dir.path().join("python3")).unwrap();
    fs::write(dir.path().join("mode"), mode).unwrap();
    dir
}

/// A `hermes` console script whose shebang names the fake Python.
fn fake(mode: &str) -> Fake {
    let dir = fake_dir(mode);
    let cli = dir.path().join("hermes");
    let python = dir.path().join("python3");
    fs::write(&cli, format!("#!{}\nfrom hermes_cli.main import main\n", python.display())).unwrap();
    Fake { dir, cli }
}

/// The published POSIX launcher (`exec <python> … "$@"`). A hard link, never a file written
/// here: executing a file another test thread may still hold open for writing fails (ETXTBSY).
fn fake_launcher(mode: &str) -> Fake {
    let dir = fake_dir(mode);
    let cli = dir.path().join("hermes");
    fs::hard_link(fixture("fake-launcher"), &cli).unwrap();
    Fake { dir, cli }
}

impl Fake {
    fn archive(&self, profile: Option<&str>) -> HermesArchive {
        HermesArchive::new(&self.cli, profile.map(str::to_owned), "0.21.5".to_owned()).unwrap()
    }

    fn argv(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join("argv.txt"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn id() -> SessionId {
    SessionId::parse("s-1").unwrap()
}

fn cwd() -> AbsolutePath {
    AbsolutePath::new(std::env::temp_dir()).unwrap()
}

fn hermes_data() -> BackendData {
    BackendData::Hermes { profile: None }
}

#[test]
fn present_runs_the_script_with_a_list_of_arguments() {
    let fake = fake("present");
    assert!(fake.archive(Some("work")).present(&id(), &cwd(), &hermes_data()).unwrap());
    let argv = fake.argv();
    assert_eq!(argv.first().map(String::as_str), Some("-I"));
    assert!(argv.get(1).is_some_and(|script| script.ends_with("agent_hub_snapshot.py")));
    assert_eq!(argv.get(2).map(String::as_str), Some("present"));
    for arg in ["--schema=31", "--session=s-1", "--profile=work"] {
        assert!(argv.iter().any(|given| given == arg), "{arg} in {argv:?}");
    }
}

#[test]
fn the_published_launcher_runs_the_script_as_a_module_argument() {
    let fake = fake_launcher("present");
    assert!(fake.archive(None).present(&id(), &cwd(), &hermes_data()).unwrap());
    let argv = fake.argv();
    assert_eq!(
        argv.get(..3),
        Some(["--run-module", "cProfile", "-o"].map(str::to_owned).as_slice())
    );
    assert!(argv.get(3).is_some_and(|stats| stats.ends_with("profile.out")), "{argv:?}");
    assert!(argv.get(4).is_some_and(|script| script.ends_with("agent_hub_snapshot.py")));
    assert_eq!(argv.get(5).map(String::as_str), Some("present"));
}

#[test]
fn a_profile_that_looks_like_a_flag_stays_one_argument() {
    let fake = fake("absent");
    assert!(!fake.archive(Some("--evil")).present(&id(), &cwd(), &hermes_data()).unwrap());
    assert!(fake.argv().iter().any(|arg| arg == "--profile=--evil"));
}

#[test]
fn absent_sessions_are_not_present() {
    let fake = fake("absent");
    assert!(!fake.archive(None).present(&id(), &cwd(), &hermes_data()).unwrap());
    assert!(!fake.argv().iter().any(|arg| arg.starts_with("--profile")));
}

#[test]
fn export_lists_the_one_session_file() {
    let fake = fake("export");
    let dest = tempfile::tempdir().unwrap();
    let exported = fake.archive(Some("work")).export(&id(), &cwd(), dest.path()).unwrap();
    assert_eq!(exported.files, [RelPath::parse(SESSION_FILE).unwrap()]);
    assert_eq!(exported.data, BackendData::Hermes { profile: Some("work".to_owned()) });
    assert_eq!(exported.agent_version, "0.21.5");
    assert!(dest.path().join(SESSION_FILE).is_file());
}

#[test]
fn a_missing_session_is_not_found() {
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(
        fake("missing").archive(None).export(&id(), &cwd(), dest.path()),
        Err(ArchiveError::NotFound)
    ));
}

#[test]
fn another_schema_names_the_hermes_version() {
    let error = fake("schema").archive(None).present(&id(), &cwd(), &hermes_data()).unwrap_err();
    assert_eq!(error.to_string(), "Hermes 0.21.5 не поддерживается для переноса");
}

#[test]
fn no_result_file_is_a_failure_not_a_guess() {
    assert!(matches!(
        fake("silent").archive(None).present(&id(), &cwd(), &hermes_data()),
        Err(ArchiveError::Unsupported(_))
    ));
}

#[test]
fn a_result_that_is_not_json_is_malformed() {
    assert!(matches!(
        fake("garbage").archive(None).present(&id(), &cwd(), &hermes_data()),
        Err(ArchiveError::Malformed(_))
    ));
}

fn snapshot_with_session_file() -> tempfile::TempDir {
    let snapshot = tempfile::tempdir().unwrap();
    fs::write(snapshot.path().join(SESSION_FILE), "{}").unwrap();
    snapshot
}

#[test]
fn import_passes_the_new_directory() {
    let fake = fake("imported");
    let snapshot = snapshot_with_session_file();
    let files = [RelPath::parse(SESSION_FILE).unwrap()];
    fake.archive(None).import(&id(), &hermes_data(), &files, snapshot.path(), &cwd()).unwrap();
    let argv = fake.argv();
    assert_eq!(argv.get(2).map(String::as_str), Some("import"));
    let dir = format!("--cwd={}", cwd().as_path().display());
    assert!(argv.contains(&dir), "{argv:?}");
    let input = format!("--in={}", snapshot.path().join(SESSION_FILE).display());
    assert!(argv.contains(&input), "{argv:?}");
}

#[test]
fn import_refuses_a_session_hermes_already_has() {
    let snapshot = snapshot_with_session_file();
    let files = [RelPath::parse(SESSION_FILE).unwrap()];
    assert!(matches!(
        fake("exists").archive(None).import(&id(), &hermes_data(), &files, snapshot.path(), &cwd()),
        Err(ArchiveError::Ambiguous)
    ));
}

#[test]
fn import_accepts_only_the_session_file() {
    let fake = fake("imported");
    let snapshot = snapshot_with_session_file();
    for files in [
        vec![],
        vec![RelPath::parse("other.json").unwrap()],
        vec![RelPath::parse(SESSION_FILE).unwrap(), RelPath::parse("other.json").unwrap()],
    ] {
        assert!(matches!(
            fake.archive(None).import(&id(), &hermes_data(), &files, snapshot.path(), &cwd()),
            Err(ArchiveError::Malformed(_))
        ));
    }
    assert!(!fake.dir.path().join("argv.txt").exists(), "the script must not run");
}

#[test]
fn import_requires_the_listed_file_to_exist() {
    let fake = fake("imported");
    let empty = tempfile::tempdir().unwrap();
    let files = [RelPath::parse(SESSION_FILE).unwrap()];
    assert!(matches!(
        fake.archive(None).import(&id(), &hermes_data(), &files, empty.path(), &cwd()),
        Err(ArchiveError::Malformed(_))
    ));
}

#[test]
fn import_refuses_a_linked_session_file() {
    let fake = fake("imported");
    let snapshot = tempfile::tempdir().unwrap();
    let elsewhere = snapshot_with_session_file();
    std::os::unix::fs::symlink(
        elsewhere.path().join(SESSION_FILE),
        snapshot.path().join(SESSION_FILE),
    )
    .unwrap();
    let files = [RelPath::parse(SESSION_FILE).unwrap()];
    assert!(matches!(
        fake.archive(None).import(&id(), &hermes_data(), &files, snapshot.path(), &cwd()),
        Err(ArchiveError::Link(_))
    ));
}

#[test]
fn import_refuses_another_backends_snapshot() {
    let fake = fake("imported");
    let snapshot = snapshot_with_session_file();
    let files = [RelPath::parse(SESSION_FILE).unwrap()];
    assert!(matches!(
        fake.archive(None).import(&id(), &BackendData::Codex, &files, snapshot.path(), &cwd()),
        Err(ArchiveError::Malformed(_))
    ));
}
