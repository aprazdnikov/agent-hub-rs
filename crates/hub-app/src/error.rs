//! Failures of the files the application owns.

use std::io;
use std::path::PathBuf;

use crate::secrets::SecretError;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("не удалось прочитать {}: {error}", path.display())]
    Read { path: PathBuf, error: io::Error },
    #[error("{} повреждён: {reason}", path.display())]
    Corrupt { path: PathBuf, reason: String },
    #[error("не удалось записать {}: {error}", path.display())]
    Write { path: PathBuf, error: io::Error },
    #[error(transparent)]
    Secret(#[from] SecretError),
}
