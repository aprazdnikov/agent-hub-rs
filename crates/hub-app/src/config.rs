//! `settings.toml` plus the token from the keyring, parsed by the same code as the form.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use hub_core::settings::{ApiKey, Draft, FieldError, Keys, Settings, SettingsFile};

use crate::atomic::write_atomic;
use crate::error::StoreError;
use crate::secrets::{Secret, Secrets};

#[derive(Debug)]
pub enum Loaded {
    Missing,
    Ready(Settings),
    /// The file is readable but the form still has errors, e.g. no token in the keyring yet.
    Incomplete {
        draft: Draft,
        errors: Vec<FieldError>,
    },
}

pub trait SettingsStore: Send + 'static {
    fn save(&self, settings: &Settings) -> Result<(), StoreError>;
}

pub struct FileSettings {
    path: PathBuf,
    secrets: Box<dyn Secrets>,
}

impl FileSettings {
    #[must_use]
    pub fn new(path: PathBuf, secrets: Box<dyn Secrets>) -> Self {
        Self { path, secrets }
    }

    pub fn load(&self, home: &Path) -> Result<Loaded, StoreError> {
        let raw = match fs::read_to_string(&self.path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Loaded::Missing),
            Err(error) => return Err(StoreError::Read { path: self.path.clone(), error }),
        };
        let corrupt = |reason: String| StoreError::Corrupt { path: self.path.clone(), reason };
        let file: SettingsFile =
            toml::from_str(&raw).map_err(|error| corrupt(error.message().to_owned()))?;
        let keys = Keys {
            token: self.secrets.read(Secret::TelegramToken)?.unwrap_or_default(),
            api_key: self.secrets.read(Secret::OpenAiKey)?.unwrap_or_default(),
        };
        let draft = file.to_draft(keys).map_err(|error| corrupt(error.message))?;
        Ok(match draft.parse(home) {
            Ok(settings) => Loaded::Ready(settings),
            Err(errors) => Loaded::Incomplete { draft, errors },
        })
    }
}

impl SettingsStore for FileSettings {
    fn save(&self, settings: &Settings) -> Result<(), StoreError> {
        let write = |error| StoreError::Write { path: self.path.clone(), error };
        let text = toml::to_string_pretty(&SettingsFile::from_settings(settings))
            .map_err(|error| write(io::Error::other(error)))?;
        // The secrets go first: a file pointing at a token that was never stored is worse.
        self.secrets.write(Secret::TelegramToken, Some(settings.telegram.token.expose()))?;
        self.secrets
            .write(Secret::OpenAiKey, settings.codex.api_key.as_ref().map(ApiKey::expose))?;
        write_atomic(&self.path, text.as_bytes()).map_err(write)
    }
}

#[cfg(test)]
mod tests {
    use hub_core::settings::Field;

    use super::*;
    use crate::secrets::MemorySecrets;

    fn draft(root: &Path) -> Draft {
        Draft {
            token: "123:secret-token".to_owned(),
            chat: "-100".to_owned(),
            users: "1, 2".to_owned(),
            workspace_root: root.display().to_string(),
            model: "opus".to_owned(),
            ..Draft::default()
        }
    }

    fn store(dir: &Path) -> FileSettings {
        FileSettings::new(dir.join("settings.toml"), Box::new(MemorySecrets::default()))
    }

    #[test]
    fn missing_file_means_no_settings() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(store(dir.path()).load(dir.path()), Ok(Loaded::Missing)));
    }

    #[test]
    fn saved_settings_load_back() {
        let dir = tempfile::tempdir().unwrap();
        let settings = draft(dir.path()).parse(dir.path()).unwrap();
        let store = store(dir.path());
        store.save(&settings).unwrap();
        match store.load(dir.path()).unwrap() {
            Loaded::Ready(loaded) => assert_eq!(loaded, settings),
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn token_is_not_written_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let settings = draft(dir.path()).parse(dir.path()).unwrap();
        store(dir.path()).save(&settings).unwrap();
        let text = std::fs::read_to_string(dir.path().join("settings.toml")).unwrap();
        assert!(!text.contains("secret-token"));
        assert!(text.contains("-100"));
    }

    #[test]
    fn missing_token_leaves_the_form_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        let settings = draft(dir.path()).parse(dir.path()).unwrap();
        store(dir.path()).save(&settings).unwrap();
        let fresh = store(dir.path());
        match fresh.load(dir.path()).unwrap() {
            Loaded::Incomplete { draft, errors } => {
                assert_eq!(draft.chat, "-100");
                assert_eq!(errors.iter().map(|e| e.field).collect::<Vec<_>>(), [Field::Token]);
            }
            other => panic!("expected Incomplete, got {other:?}"),
        }
    }

    #[test]
    fn corrupt_settings_are_reported_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "telegram = 5").unwrap();
        let error = store(dir.path()).load(dir.path()).unwrap_err();
        assert!(matches!(error, StoreError::Corrupt { .. }));
        assert!(error.to_string().contains("settings.toml"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "telegram = 5");
    }

    #[test]
    fn api_key_goes_to_the_keyring_not_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut form = draft(dir.path());
        form.codex.api_key = "sk-secret".to_owned();
        let settings = form.parse(dir.path()).unwrap();
        let store = store(dir.path());
        store.save(&settings).unwrap();
        let text = std::fs::read_to_string(dir.path().join("settings.toml")).unwrap();
        assert!(!text.contains("sk-secret"));
        match store.load(dir.path()).unwrap() {
            Loaded::Ready(loaded) => {
                assert_eq!(loaded.codex.api_key.as_ref().map(ApiKey::expose), Some("sk-secret"));
            }
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn cleared_api_key_is_removed_from_the_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let mut form = draft(dir.path());
        form.codex.api_key = "sk-secret".to_owned();
        store.save(&form.parse(dir.path()).unwrap()).unwrap();
        form.codex.api_key = String::new();
        store.save(&form.parse(dir.path()).unwrap()).unwrap();
        match store.load(dir.path()).unwrap() {
            Loaded::Ready(loaded) => assert_eq!(loaded.codex.api_key, None),
            other => panic!("expected Ready, got {other:?}"),
        }
    }
}
