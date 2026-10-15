use std::fs;
use std::io::Write;
use std::path::Path;

use hub_agent::archive::{ArchiveError, private_dirs, private_file};
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{
    BackendData, FILES_LIMIT, MANIFEST_LIMIT, Manifest, ManifestError, RelPath, SnapshotName,
};
use hub_telegram::snapshots::{SnapshotError, SnapshotStore};

fn name(raw: &str) -> SnapshotName {
    SnapshotName::parse(raw).unwrap()
}

fn manifest(raw: &str, created_at: &str) -> Manifest {
    Manifest {
        name: name(raw),
        created_at: created_at.to_owned(),
        title: None,
        cwd: AbsolutePath::new(std::env::temp_dir()).unwrap(),
        session: SessionId::parse("s-1").unwrap(),
        agent_version: "0.160.0".to_owned(),
        data: BackendData::Codex,
        files: vec![RelPath::parse("fake/s-1.txt").unwrap()],
    }
}

/// What an archive does: writes its file into `agent/` and describes it.
fn fill(raw: &str, created_at: &str) -> impl FnOnce(&Path) -> Result<Manifest, ArchiveError> {
    let manifest = manifest(raw, created_at);
    move |agent: &Path| {
        private_dirs(&agent.join("fake"))?;
        private_file(&agent.join("fake").join("s-1.txt"))?.write_all(b"history")?;
        Ok(manifest)
    }
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn a_saved_snapshot_loads_back_with_its_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());

    let saved = store.save(&name("snap"), fill("snap", "2026-10-09T15:30:12Z")).unwrap();
    let loaded = store.load(&name("snap")).unwrap();

    assert_eq!(loaded.manifest, saved);
    assert_eq!(fs::read_to_string(loaded.agent.join("fake").join("s-1.txt")).unwrap(), "history");
    assert_eq!(entries(&dir.path().join("snapshots")), ["snap"]);
    assert_eq!(entries(&dir.path().join("snapshots").join("snap")), ["agent", "manifest.json"]);
}

#[test]
fn a_failed_export_leaves_no_trace() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    let result = store.save(&name("snap"), |agent: &Path| {
        fs::write(agent.join("partial"), "x")?;
        Err(ArchiveError::NotFound)
    });
    assert!(matches!(result, Err(SnapshotError::Archive(ArchiveError::NotFound))));
    assert!(entries(&dir.path().join("snapshots")).is_empty());
}

#[test]
fn a_taken_name_is_refused_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    store.save(&name("snap"), fill("snap", "2026-10-09T15:30:12Z")).unwrap();
    let again = store.save(&name("snap"), fill("snap", "2026-10-10T00:00:00Z"));
    assert!(matches!(again, Err(SnapshotError::Exists(_))));
    assert_eq!(store.load(&name("snap")).unwrap().manifest.created_at, "2026-10-09T15:30:12Z");
    assert_eq!(SnapshotError::Exists(name("snap")).to_string(), "Снимок snap уже существует");
}

#[test]
fn the_list_is_newest_first_and_skips_what_is_not_a_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    assert!(store.list().unwrap().is_empty());
    store.save(&name("old"), fill("old", "2026-10-01T10:00:00Z")).unwrap();
    store.save(&name("new"), fill("new", "2026-10-09T10:00:00Z")).unwrap();
    let root = dir.path().join("snapshots");
    fs::create_dir(root.join("broken")).unwrap();
    fs::write(root.join("broken").join("manifest.json"), "{garbage").unwrap();
    fs::create_dir(root.join(".tmp-leftover")).unwrap();
    fs::create_dir(root.join("Bad Name")).unwrap();
    fs::write(root.join("stray.txt"), "x").unwrap();

    let names: Vec<String> =
        store.list().unwrap().into_iter().map(|m| m.name.as_str().to_owned()).collect();

    assert_eq!(names, ["new", "old"]);
}

#[test]
fn loading_checks_name_presence_and_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    assert!(matches!(store.load(&name("nope")), Err(SnapshotError::Missing(_))));
    assert_eq!(SnapshotError::Missing(name("nope")).to_string(), "Снимок nope не найден");

    store.save(&name("snap"), fill("snap", "2026-10-09T15:30:12Z")).unwrap();
    let root = dir.path().join("snapshots");
    fs::rename(root.join("snap"), root.join("renamed")).unwrap();
    assert!(matches!(store.load(&name("renamed")), Err(SnapshotError::Corrupt { .. })));

    fs::remove_file(root.join("renamed").join("agent").join("fake").join("s-1.txt")).unwrap();
    fs::rename(root.join("renamed"), root.join("snap")).unwrap();
    assert!(matches!(store.load(&name("snap")), Err(SnapshotError::Corrupt { .. })));
}

/// `fill` for a manifest of `files` existing files, optionally padded to an exact JSON size.
fn fill_many(
    raw: &str,
    count: usize,
    size: Option<usize>,
) -> impl FnOnce(&Path) -> Result<Manifest, ArchiveError> {
    let files: Vec<RelPath> =
        (0..count).map(|i| RelPath::parse(&format!("fake/f{i}")).unwrap()).collect();
    let base =
        Manifest { files, title: Some(String::new()), ..manifest(raw, "2026-10-09T15:30:12Z") };
    let manifest = match size {
        Some(size) => {
            let padding = size - base.dump().unwrap().len();
            Manifest { title: Some("a".repeat(padding)), ..base }
        }
        None => base,
    };
    move |agent: &Path| {
        private_dirs(&agent.join("fake"))?;
        for file in manifest.files.iter().take(FILES_LIMIT) {
            private_file(&file.under(agent))?;
        }
        Ok(manifest)
    }
}

#[test]
fn the_size_limit_holds_on_save_and_load_alike() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    store.save(&name("big"), fill_many("big", 1, Some(MANIFEST_LIMIT))).unwrap();
    assert_eq!(
        fs::metadata(dir.path().join("snapshots").join("big").join("manifest.json")).unwrap().len(),
        u64::try_from(MANIFEST_LIMIT).unwrap()
    );
    assert_eq!(store.load(&name("big")).unwrap().manifest.files.len(), 1);

    let over = store.save(&name("bigger"), fill_many("bigger", 1, Some(MANIFEST_LIMIT + 1)));
    assert!(matches!(over, Err(SnapshotError::Manifest(ManifestError::TooLarge(_)))));
    assert_eq!(entries(&dir.path().join("snapshots")), ["big"]);
}

#[test]
fn the_files_limit_holds_on_save_and_load_alike() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    store.save(&name("many"), fill_many("many", FILES_LIMIT, None)).unwrap();
    assert_eq!(store.load(&name("many")).unwrap().manifest.files.len(), FILES_LIMIT);

    let over = store.save(&name("more"), fill_many("more", FILES_LIMIT + 1, None));
    assert!(matches!(
        over,
        Err(SnapshotError::Manifest(ManifestError::Field { field: "files", .. }))
    ));
    assert_eq!(entries(&dir.path().join("snapshots")), ["many"]);
}

#[cfg(unix)]
#[test]
fn a_linked_parent_inside_a_snapshot_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    store.save(&name("snap"), fill("snap", "2026-10-09T15:30:12Z")).unwrap();
    let agent = dir.path().join("snapshots").join("snap").join("agent");
    fs::create_dir_all(outside.path().join("fake")).unwrap();
    fs::write(outside.path().join("fake").join("s-1.txt"), "planted").unwrap();
    fs::remove_dir_all(agent.join("fake")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("fake"), agent.join("fake")).unwrap();

    assert!(matches!(store.load(&name("snap")), Err(SnapshotError::Corrupt { .. })));
}

#[cfg(unix)]
#[test]
fn snapshots_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    SnapshotStore::new(dir.path())
        .save(&name("snap"), fill("snap", "2026-10-09T15:30:12Z"))
        .unwrap();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let snap = dir.path().join("snapshots").join("snap");
    assert_eq!(
        [
            mode(&dir.path().join("snapshots")),
            mode(&snap),
            mode(&snap.join("agent")),
            mode(&snap.join("manifest.json")),
            mode(&snap.join("agent").join("fake").join("s-1.txt")),
        ],
        [0o700, 0o700, 0o700, 0o600, 0o600]
    );
}

#[cfg(unix)]
#[test]
fn a_linked_snapshot_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    SnapshotStore::new(elsewhere.path())
        .save(&name("snap"), fill("snap", "2026-10-09T15:30:12Z"))
        .unwrap();
    fs::create_dir_all(dir.path().join("snapshots")).unwrap();
    std::os::unix::fs::symlink(
        elsewhere.path().join("snapshots").join("snap"),
        dir.path().join("snapshots").join("snap"),
    )
    .unwrap();
    assert!(matches!(store.load(&name("snap")), Err(SnapshotError::Corrupt { .. })));
    assert!(store.list().unwrap().is_empty());
}
