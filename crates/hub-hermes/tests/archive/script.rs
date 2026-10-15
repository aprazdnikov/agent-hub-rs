//! The logic of `archive.py` against stand-ins for Hermes's modules. Needs `python3`: skipped
//! with a message without it, but a failure under CI (`CI` set) so CI cannot skip silently.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

fn crate_path(raw: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(raw)
}

/// Puts the stubs first on `sys.path` and runs the script as `__main__`; `-B` keeps
/// `__pycache__` out of the checkout.
const HARNESS: &str = "import runpy, sys\n\
stubs, script = sys.argv[1], sys.argv[2]\n\
sys.path.insert(0, stubs)\n\
sys.argv = [script] + sys.argv[3:]\n\
runpy.run_path(script, run_name='__main__')\n";

fn python_available(test: &str) -> bool {
    let found = Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    assert!(found || std::env::var_os("CI").is_none(), "python3 is required under CI ({test})");
    if !found {
        eprintln!("python3 not found: {test} skipped");
    }
    found
}

fn schema_db(home: &Path, version: u32) {
    fs::create_dir_all(home).unwrap();
    let status = Command::new("python3")
        .args([
            "-I",
            "-c",
            "import sqlite3, sys\nc = sqlite3.connect(sys.argv[1])\nc.execute('create table schema_version (version int)')\nc.execute('insert into schema_version values (?)', (int(sys.argv[2]),))\nc.commit()\n",
        ])
        .arg(home.join("state.db"))
        .arg(version.to_string())
        .status()
        .unwrap();
    assert!(status.success());
}

/// Runs the embedded script against the stubs and returns its result file.
fn run(root: &Path, args: &[&str]) -> Value {
    let result = root.join(format!("result-{}.json", file_name_of(args)));
    let status = Command::new("python3")
        .args(["-I", "-B", "-c", HARNESS])
        .arg(crate_path("tests/fixtures/stub_hermes"))
        .arg(crate_path("src/archive.py"))
        .args(args)
        .arg(format!("--result={}", result.display()))
        .arg("--schema=31")
        .env("STUB_HERMES_ROOT", root)
        .env_remove("HERMES_HOME")
        .status()
        .unwrap();
    assert!(status.success());
    serde_json::from_str(&fs::read_to_string(result).unwrap()).unwrap()
}

/// Distinct result names per call inside one test.
fn file_name_of(args: &[&str]) -> String {
    args.join("-").replace(['/', '\\', ':', '.', '='], "_")
}

fn sessions(home: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(home.join("sessions.json")).unwrap()).unwrap()
}

fn seeded_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    schema_db(root.path(), 31);
    let sessions = json!({
        "s-0": {"id": "s-0", "source": "acp", "cwd": "/old", "model_config": "{\"cwd\": \"/old\", \"provider\": \"p\"}", "messages": [{"role": "user", "content": "hi"}]},
        "s-1": {"id": "s-1", "source": "acp", "parent_session_id": "s-0", "lineage": ["s-0", "s-1"], "cwd": "/old", "model_config": {"cwd": "/old"}, "messages": []},
    });
    fs::write(root.path().join("sessions.json"), sessions.to_string()).unwrap();
    root
}

fn export_to(root: &Path) -> PathBuf {
    let out = root.join("out.json");
    let exported = run(root, &["export", "--session=s-1", &format!("--out={}", out.display())]);
    assert_eq!(exported, json!({"ok": true, "found": true, "segments": 2}));
    out
}

#[test]
fn a_lineage_moves_to_another_profile_with_the_new_directory() {
    if !python_available("a_lineage_moves_to_another_profile_with_the_new_directory") {
        return;
    }
    let root = seeded_root();
    let out = export_to(root.path());
    let work = root.path().join("profiles").join("work");
    schema_db(&work, 31);
    let input = format!("--in={}", out.display());
    let imported =
        run(root.path(), &["import", "--session=s-1", "--profile=work", &input, "--cwd=/new"]);
    assert_eq!(imported, json!({"ok": true, "imported": 2, "skipped": 0}));

    let moved = sessions(&work);
    assert_eq!(moved["s-0"]["cwd"], "/new");
    assert_eq!(moved["s-0"]["model_config"], json!({"cwd": "/new", "provider": "p"}));
    assert_eq!(moved["s-1"]["model_config"], json!({"cwd": "/new"}));
    assert_eq!(moved["s-0"]["source"], "acp");
    assert!(moved["s-1"].get("segments").is_none());

    let present = run(root.path(), &["present", "--session=s-1", "--profile=work"]);
    assert_eq!(present, json!({"ok": true, "present": true}));
}

#[test]
fn a_session_hermes_already_has_is_refused_and_nothing_is_written() {
    if !python_available("a_session_hermes_already_has_is_refused_and_nothing_is_written") {
        return;
    }
    let root = seeded_root();
    let out = export_to(root.path());
    let before = fs::read(root.path().join("sessions.json")).unwrap();
    let input = format!("--in={}", out.display());
    let refused = run(root.path(), &["import", "--session=s-1", &input, "--cwd=/new"]);
    assert_eq!(refused["error"], "exists");
    assert_eq!(fs::read(root.path().join("sessions.json")).unwrap(), before);
}

#[test]
fn a_payload_without_the_session_is_refused() {
    if !python_available("a_payload_without_the_session_is_refused") {
        return;
    }
    let root = seeded_root();
    let out = root.path().join("other.json");
    fs::write(&out, json!({"id": "s-9", "segments": [{"id": "s-9"}]}).to_string()).unwrap();
    let input = format!("--in={}", out.display());
    let refused = run(root.path(), &["import", "--session=s-1", &input, "--cwd=/new"]);
    assert_eq!(refused["error"], "payload");
}

#[test]
fn an_unknown_session_is_not_found() {
    if !python_available("an_unknown_session_is_not_found") {
        return;
    }
    let root = seeded_root();
    let out = root.path().join("none.json");
    let exported =
        run(root.path(), &["export", "--session=s-9", &format!("--out={}", out.display())]);
    assert_eq!(exported, json!({"ok": true, "found": false}));
    assert!(!out.exists());
}

#[test]
fn the_sticky_active_profile_is_honoured() {
    if !python_available("the_sticky_active_profile_is_honoured") {
        return;
    }
    let root = seeded_root();
    let work = root.path().join("profiles").join("work");
    schema_db(&work, 31);
    fs::write(root.path().join("active_profile"), "work\n").unwrap();
    assert_eq!(
        run(root.path(), &["present", "--session=s-1"]),
        json!({"ok": true, "present": false})
    );
}

#[test]
fn another_schema_or_a_missing_profile_is_refused() {
    if !python_available("another_schema_or_a_missing_profile_is_refused") {
        return;
    }
    let old = tempfile::tempdir().unwrap();
    schema_db(old.path(), 30);
    assert_eq!(run(old.path(), &["present", "--session=s-1"])["error"], "schema");
    let root = seeded_root();
    assert_eq!(
        run(root.path(), &["present", "--session=s-1", "--profile=ghost"])["error"],
        "profile"
    );
}
