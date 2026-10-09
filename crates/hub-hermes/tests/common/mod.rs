//! Process fixtures shared by the public API tests.

use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct Fixture {
    directory: tempfile::TempDir,
    pub cli: PathBuf,
}

impl Fixture {
    pub fn new(case: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("hermes-fixture");
        let launcher = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/hermes-fixture");
        // Writing an executable per test races with parallel spawns (ETXTBSY on Linux), so
        // every test runs the same checked-in launcher and passes its case as plain data.
        std::fs::write(directory.path().join("case"), case).unwrap();
        std::os::unix::fs::symlink(&launcher, &cli).unwrap();
        Self { directory, cli }
    }

    pub fn root(&self) -> &Path {
        self.directory.path()
    }

    pub fn records(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root().join("wire.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}
