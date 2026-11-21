use std::{
    error::Error as StdError,
    fs,
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
};

use hub_memory::{EntryKind, Error, Store, WriteRequest};
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn StdError>>;

struct Fixture {
    root: TempDir,
    base: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Result<Self, Box<dyn StdError>> {
        let root = tempfile::tempdir()?;
        let base = root.path().join("memory");
        let project = root.path().join("repo");
        fs::create_dir(&project)?;
        Ok(Self { root, base, project })
    }

    fn open(&self, actor: &str) -> Result<Store, Error> {
        Store::open(&self.base, &self.project, actor)
    }

    fn directory(&self) -> Result<PathBuf, Box<dyn StdError>> {
        fs::read_dir(&self.base)?
            .next()
            .ok_or("missing project directory")?
            .map(|entry| entry.path())
            .map_err(Into::into)
    }

    fn data(&self) -> Result<PathBuf, Box<dyn StdError>> {
        Ok(self.directory()?.join("memory.json"))
    }
}

fn request(key: &str, expected_revision: u32, body: &str) -> WriteRequest {
    WriteRequest {
        key: key.to_owned(),
        expected_revision,
        kind: EntryKind::Decision,
        title: "Project decision".to_owned(),
        body: body.to_owned(),
        sources: vec!["telegram:topic/42".to_owned()],
    }
}

#[test]
fn revisions_survive_restarting_and_keep_authorship() -> TestResult {
    let fixture = Fixture::new()?;
    let first = fixture.open("claude")?.write(request("architecture", 0, "Initial decision"))?;
    let second = fixture.open("codex")?.write(request("architecture", 1, "Revised decision"))?;
    let reopened = fixture.open("reader")?;
    assert_eq!(
        (
            reopened.get("architecture", Some(1))?,
            reopened.get("architecture", None)?,
            first.author.as_str(),
            second.author.as_str(),
            second.revision
        ),
        (first.clone(), second.clone(), "claude", "codex", 2)
    );
    assert!(first.created_at > 0);
    Ok(())
}

#[test]
fn duplicate_write_is_idempotent_and_stale_change_conflicts() -> TestResult {
    let fixture = Fixture::new()?;
    let initial = request("decision", 0, "The chosen approach");
    let stored = fixture.open("first")?.write(initial.clone())?;
    let retry = fixture.open("retry")?.write(initial)?;
    assert_eq!(stored, retry);
    assert!(matches!(
        fixture.open("other")?.write(request("decision", 0, "Different approach")),
        Err(Error::Conflict { actual: 1, expected: 0, .. })
    ));
    Ok(())
}

#[test]
fn independent_handles_do_not_lose_updates() -> TestResult {
    let fixture = Fixture::new()?;
    let first = fixture.open("one")?;
    let second = fixture.open("two")?;
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [(first, "one"), (second, "two")]
        .into_iter()
        .map(|(store, key)| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                for _ in 0..100 {
                    match store.write(request(key, 0, key)) {
                        Err(Error::Busy) => thread::sleep(std::time::Duration::from_millis(1)),
                        result => return result,
                    }
                }
                Err(Error::Busy)
            })
        })
        .collect();
    for worker in workers {
        worker.join().map_err(|_| "writer panicked")??;
    }
    assert_eq!(fixture.open("reader")?.search("", 20)?.len(), 2);
    Ok(())
}

#[test]
fn cloned_handles_share_persisted_state() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    let cloned = store.clone();
    let entry = store.write(request("shared", 0, "Saved"))?;
    assert_eq!(cloned.get("shared", None)?, entry);
    Ok(())
}

#[test]
fn search_uses_latest_revisions_all_terms_and_bounded_unicode_excerpt() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    store.write(request("first", 0, "obsolete keyword"))?;
    store.write(request("first", 1, &format!("CURRENT {}", "я".repeat(500))))?;
    store.write(request("second", 0, "unrelated text"))?;
    let hits = store.search("CURRENT TELEGRAM", 20)?;
    assert_eq!(
        hits.iter()
            .map(|hit| (hit.key.as_str(), hit.revision, hit.excerpt.chars().count()))
            .collect::<Vec<_>>(),
        vec![("first", 2, 400)]
    );
    assert!(store.search("obsolete", 20)?.is_empty());
    Ok(())
}

#[test]
fn separate_projects_are_isolated_and_canonical_aliases_share_memory() -> TestResult {
    let fixture = Fixture::new()?;
    let other = fixture.root.path().join("other");
    fs::create_dir(&other)?;
    fixture.open("one")?.write(request("private", 0, "Project one"))?;
    assert!(matches!(
        Store::open(&fixture.base, &other, "two")?.get("private", None),
        Err(Error::NotFound { .. })
    ));
    assert_eq!(
        Store::open(&fixture.base, &fixture.project.join("."), "three")?.get("private", None)?.body,
        "Project one"
    );
    Ok(())
}

#[test]
fn invalid_requests_and_queries_are_rejected() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    for invalid_key in ["", "../escape", "space key", "кириллица"] {
        assert!(matches!(
            store.write(request(invalid_key, 0, "body")),
            Err(Error::InvalidInput(_))
        ));
    }
    let mut invalid = request("entry", 0, "body");
    invalid.sources.clear();
    assert!(matches!(store.write(invalid), Err(Error::InvalidInput(_))));
    assert!(matches!(
        store.write(request("entry", 0, &"x".repeat(16_385))),
        Err(Error::InvalidInput(_))
    ));
    assert!(matches!(store.search(&"x".repeat(513), 10), Err(Error::InvalidInput(_))));
    assert!(matches!(store.search("", 0), Err(Error::InvalidInput(_))));
    assert!(matches!(store.search("", 21), Err(Error::InvalidInput(_))));
    assert!(matches!(fixture.open(&"x".repeat(129)), Err(Error::InvalidInput(_))));
    Ok(())
}

#[test]
fn revision_limit_preserves_history() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    for revision in 0..32 {
        store.write(request("history", revision, &revision.to_string()))?;
    }
    assert!(matches!(
        store.write(request("history", 32, "overflow")),
        Err(Error::LimitExceeded(_))
    ));
    assert_eq!(store.get("history", Some(1))?.body, "0");
    Ok(())
}

#[test]
fn invalid_json_and_oversize_files_are_rejected_without_overwrite() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    store.write(request("entry", 0, "Original"))?;
    fs::write(fixture.data()?, b"not json; SECRET_BODY")?;
    let error = store.get("entry", None).err().ok_or("corrupt file was accepted")?;
    assert!(matches!(error, Error::Corrupt(_)));
    assert!(!format!("{error:?}").contains("SECRET_BODY"));
    assert!(store.write(request("entry", 1, "Replace")).is_err());
    fs::OpenOptions::new().write(true).open(fixture.data()?)?.set_len(8 * 1024 * 1024 + 1)?;
    assert!(matches!(store.search("", 20), Err(Error::LimitExceeded(_))));
    Ok(())
}

#[test]
fn unsupported_schema_and_wrong_project_are_rejected() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    store.write(request("entry", 0, "Saved"))?;
    let original: serde_json::Value = serde_json::from_slice(&fs::read(fixture.data()?)?)?;
    let mut altered = original.clone();
    altered.as_object_mut().ok_or("expected object")?.insert("schema_version".into(), 99.into());
    fs::write(fixture.data()?, serde_json::to_vec(&altered)?)?;
    assert!(matches!(store.get("entry", None), Err(Error::UnsupportedVersion(99))));
    let mut altered = original;
    altered
        .as_object_mut()
        .ok_or("expected object")?
        .insert("project".into(), "another-project".into());
    fs::write(fixture.data()?, serde_json::to_vec(&altered)?)?;
    assert!(matches!(store.get("entry", None), Err(Error::ProjectMismatch)));
    Ok(())
}

#[cfg(unix)]
#[test]
fn managed_symlinks_are_rejected_and_files_are_private() -> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    store.write(request("entry", 0, "Saved"))?;
    assert_eq!(
        (
            fs::metadata(&fixture.base)?.permissions().mode() & 0o777,
            fs::metadata(fixture.directory()?)?.permissions().mode() & 0o777,
            fs::metadata(fixture.data()?)?.permissions().mode() & 0o777
        ),
        (0o700, 0o700, 0o600)
    );
    let outside = fixture.root.path().join("outside");
    fs::write(&outside, "untouched")?;
    fs::remove_file(fixture.data()?)?;
    symlink(&outside, fixture.data()?)?;
    assert!(matches!(store.get("entry", None), Err(Error::UnsafePath(_))));
    assert!(matches!(store.write(request("entry", 0, "Replaced")), Err(Error::UnsafePath(_))));
    Ok(())
}

#[cfg(unix)]
#[test]
fn lock_contention_returns_busy_immediately() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    let lock =
        fs::OpenOptions::new().read(true).write(true).open(fixture.directory()?.join(".lock"))?;
    lock.try_lock()?;
    assert!(matches!(store.write(request("entry", 0, "Saved")), Err(Error::Busy)));
    Ok(())
}

#[cfg(unix)]
#[test]
fn opening_during_a_write_does_not_require_the_writer_lock() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    let entry = store.write(request("entry", 0, "Saved"))?;
    let lock =
        fs::OpenOptions::new().read(true).write(true).open(fixture.directory()?.join(".lock"))?;
    lock.try_lock()?;
    assert_eq!(fixture.open("second")?.get("entry", None)?, entry);
    Ok(())
}

#[cfg(unix)]
#[test]
fn existing_base_directory_permissions_are_preserved() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new()?;
    fs::create_dir(&fixture.base)?;
    fs::set_permissions(&fixture.base, fs::Permissions::from_mode(0o755))?;
    fixture.open("one")?.write(request("entry", 0, "Saved"))?;
    assert_eq!(
        (
            fs::metadata(&fixture.base)?.permissions().mode() & 0o777,
            fs::metadata(fixture.directory()?)?.permissions().mode() & 0o777
        ),
        (0o755, 0o700)
    );
    Ok(())
}

#[test]
fn explicit_storage_location_may_be_inside_project() -> TestResult {
    let fixture = Fixture::new()?;
    let store = Store::open(&fixture.base, fixture.root.path(), "one")?;
    let entry = store.write(request("entry", 0, "Saved"))?;
    assert_eq!(store.get("entry", None)?, entry);
    Ok(())
}

#[test]
fn interrupted_staging_file_is_replaced_without_accumulation() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    let staging = fixture.directory()?.join(".memory.tmp");
    fs::write(&staging, "interrupted partial write")?;
    store.write(request("entry", 0, "Saved"))?;
    assert!(!staging.exists());
    assert_eq!(fs::read_dir(fixture.directory()?)?.count(), 2);
    Ok(())
}

#[cfg(unix)]
#[test]
fn existing_symlink_ancestor_is_resolved_before_creating_managed_paths() -> TestResult {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new()?;
    let alias = fixture.root.path().join("alias");
    symlink(fixture.root.path(), &alias)?;
    let store = Store::open(&alias.join("new-memory"), &fixture.project, "one")?;
    let entry = store.write(request("entry", 0, "Saved"))?;
    assert_eq!(store.get("entry", None)?, entry);
    Ok(())
}

#[test]
fn persisted_invalid_entry_fields_are_rejected() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    store.write(request("entry", 0, "Saved"))?;
    let original: serde_json::Value = serde_json::from_slice(&fs::read(fixture.data()?)?)?;
    for (field, value) in [
        ("key", serde_json::json!("other-key")),
        ("revision", serde_json::json!(0)),
        ("sources", serde_json::json!([])),
        ("author", serde_json::json!("")),
        ("created_at", serde_json::json!(-1)),
        ("body", serde_json::json!("x".repeat(16_385))),
    ] {
        let mut corrupt = original.clone();
        corrupt
            .pointer_mut("/entries/entry/0")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or("missing entry")?
            .insert(field.into(), value);
        fs::write(fixture.data()?, serde_json::to_vec(&corrupt)?)?;
        assert!(
            matches!(store.get("entry", None), Err(Error::Corrupt(_))),
            "accepted invalid {field}"
        );
    }
    Ok(())
}

#[test]
fn key_limit_refuses_growth_without_losing_existing_entries() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    let original = store.write(request("entry", 0, "Saved"))?;
    let entries: serde_json::Map<String, serde_json::Value> = (0..500)
        .map(|index| {
            let mut entry = original.clone();
            entry.key = format!("entry-{index}");
            (entry.key.clone(), serde_json::json!([entry]))
        })
        .collect();
    fs::write(
        fixture.data()?,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "project": fs::canonicalize(&fixture.project)?, "entries": entries,
        }))?,
    )?;
    assert!(matches!(store.write(request("overflow", 0, "New")), Err(Error::LimitExceeded(_))));
    assert_eq!(store.get("entry-0", Some(1))?.body, "Saved");
    Ok(())
}

#[test]
fn byte_limit_refuses_growth_and_preserves_last_snapshot() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    let mut original = store.write(request("entry", 0, "Saved"))?;
    original.body = "x".repeat(16_384);
    original.sources = vec!["s".repeat(2048); 8];
    let entries: serde_json::Map<String, serde_json::Value> = (0..250)
        .map(|index| {
            let mut entry = original.clone();
            entry.key = format!("entry-{index}");
            (entry.key.clone(), serde_json::json!([entry]))
        })
        .collect();
    fs::write(
        fixture.data()?,
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1, "project": fs::canonicalize(&fixture.project)?, "entries": entries,
        }))?,
    )?;
    let mut refused = false;
    for index in 0..10 {
        let before = fs::read(fixture.data()?)?;
        let mut next = request(&format!("next-{index}"), 0, &original.body);
        next.sources.clone_from(&original.sources);
        match store.write(next) {
            Ok(_) => {}
            Err(Error::LimitExceeded(_)) => {
                assert!(fs::read(fixture.data()?)? == before);
                refused = true;
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    assert!(refused, "8 MiB capacity was not enforced");
    Ok(())
}

#[test]
fn retry_remains_idempotent_after_later_updates() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("one")?;
    let first = store.write(request("entry", 0, "First"))?;
    let latest = store.write(request("entry", 1, "Later"))?;
    assert_eq!(
        (store.write(request("entry", 0, "First"))?, store.get("entry", None)?),
        (first, latest)
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn base_project_and_lock_symlinks_are_rejected() -> TestResult {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new()?;
    let outside = fixture.root.path().join("outside");
    fs::create_dir(&outside)?;
    symlink(&outside, &fixture.base)?;
    assert!(matches!(fixture.open("one"), Err(Error::UnsafePath(_))));
    fs::remove_file(&fixture.base)?;
    let store = fixture.open("one")?;
    let directory = fixture.directory()?;
    let lock = directory.join(".lock");
    fs::remove_file(&lock)?;
    symlink(outside.join("outside-lock"), &lock)?;
    assert!(matches!(store.write(request("entry", 0, "Saved")), Err(Error::UnsafePath(_))));
    fs::remove_file(&lock)?;
    fs::remove_dir(&directory)?;
    symlink(&outside, &directory)?;
    assert!(matches!(fixture.open("one"), Err(Error::UnsafePath(_))));
    Ok(())
}

#[test]
fn inspection_does_not_create_storage_and_finds_keys() -> TestResult {
    let fixture = Fixture::new()?;
    let reader = hub_memory::MemoryReader::new(&fixture.base, &fixture.project)?;
    let empty = reader.inspect("", 0, 20)?;
    assert!(!empty.exists);
    assert!(!fixture.base.exists());
    let store = fixture.open("writer")?;
    store.write(request("unique-key", 0, "first"))?;
    store.write(request("unique-key", 1, "second"))?;
    let page = reader.inspect("unique-key", 0, 20)?;
    assert_eq!((page.key_count, page.matched_count, page.entries.len()), (1, 1, 1));
    assert_eq!(page.bytes, fs::metadata(fixture.data()?)?.len());
    assert_eq!(reader.get("unique-key", Some(1))?.body, "first");
    assert_eq!(store.search("unique-key", 20)?.len(), 1);
    assert!(reader.inspect("", 1, 20)?.entries.is_empty());
    fs::write(fixture.data()?, "broken")?;
    assert!(matches!(reader.inspect("", 0, 20), Err(Error::Corrupt(_))));
    Ok(())
}

#[test]
fn inspection_pages_cover_all_keys_without_duplicates() -> TestResult {
    let fixture = Fixture::new()?;
    let store = fixture.open("writer")?;
    for number in 0..25 {
        store.write(request(&format!("key-{number:02}"), 0, "body"))?;
    }
    let reader = hub_memory::MemoryReader::new(&fixture.base, &fixture.project)?;
    let mut keys = reader.inspect("key-", 0, 20)?.entries;
    keys.extend(reader.inspect("key-", 20, 20)?.entries);
    let mut names: Vec<_> = keys.into_iter().map(|e| e.key).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 25);
    Ok(())
}

#[cfg(unix)]
#[test]
fn inspection_rejects_symlink_and_preserves_read_permissions() -> TestResult {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let fixture = Fixture::new()?;
    fixture.open("writer")?.write(request("key", 0, "body"))?;
    let reader = hub_memory::MemoryReader::new(&fixture.base, &fixture.project)?;
    let data = fixture.data()?;
    fs::set_permissions(&data, fs::Permissions::from_mode(0o400))?;
    reader.get("key", None)?;
    assert_eq!(fs::metadata(&data)?.permissions().mode() & 0o777, 0o400);
    fs::remove_file(&data)?;
    symlink(fixture.root.path().join("missing"), &data)?;
    assert!(matches!(reader.inspect("", 0, 20), Err(Error::UnsafePath(_))));
    Ok(())
}
