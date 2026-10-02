//! Opens a directory in the system file manager.

use std::io;
use std::path::Path;
use std::process::Command;

pub fn open(path: &Path) -> io::Result<()> {
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    // The file manager outlives us; we do not wait for it.
    Command::new(program).arg(path).spawn().map(drop)
}
