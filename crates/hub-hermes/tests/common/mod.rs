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
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp.py");
        // JSON strings are Python string literals for these ASCII fixture paths.
        let script = format!(
            "#!/usr/bin/env python3\nimport runpy, sys\nsys.argv[1:1] = [{case:?}, {root}]\nrunpy.run_path({fixture}, run_name='__main__')\n",
            root = serde_json::to_string(&directory.path().display().to_string()).unwrap(),
            fixture = serde_json::to_string(&fixture.display().to_string()).unwrap(),
        );
        std::fs::write(&cli, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
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
