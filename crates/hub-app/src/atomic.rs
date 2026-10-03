//! Whole-file replacement that never leaves a half-written file behind.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "путь без каталога"))?;
    fs::create_dir_all(directory)?;
    // The temporary file must share the target's filesystem for the rename to be atomic.
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_missing_directories_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a").join("b.toml");
        write_atomic(&path, b"x = 1").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"x = 1");
    }

    #[test]
    fn replaces_and_leaves_no_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("topics.json");
        write_atomic(&path, b"old").unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
