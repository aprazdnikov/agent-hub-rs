//! Secrets live in the OS credential store, never in a file.

const SERVICE: &str = "agent-hub";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Secret {
    TelegramToken,
    OpenAiKey,
}

impl Secret {
    const fn account(self) -> &'static str {
        match self {
            Self::TelegramToken => "telegram-token",
            Self::OpenAiKey => "openai-api-key",
        }
    }
}

/// The store's own message; it never carries the secret itself.
#[derive(Debug, thiserror::Error)]
#[error("системное хранилище ключей недоступно: {0}")]
pub struct SecretError(String);

pub trait Secrets: Send + Sync {
    fn read(&self, secret: Secret) -> Result<Option<String>, SecretError>;
    /// `None` removes the secret.
    fn write(&self, secret: Secret, value: Option<&str>) -> Result<(), SecretError>;
}

pub struct Keyring;

fn entry(secret: Secret) -> Result<keyring::Entry, SecretError> {
    keyring::Entry::new(SERVICE, secret.account()).map_err(|error| SecretError(error.to_string()))
}

impl Secrets for Keyring {
    fn read(&self, secret: Secret) -> Result<Option<String>, SecretError> {
        match entry(secret)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecretError(error.to_string())),
        }
    }

    fn write(&self, secret: Secret, value: Option<&str>) -> Result<(), SecretError> {
        let entry = entry(secret)?;
        let written = match value {
            Some(value) => entry.set_password(value),
            None => match entry.delete_credential() {
                Err(keyring::Error::NoEntry) => Ok(()),
                other => other,
            },
        };
        written.map_err(|error| SecretError(error.to_string()))
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemorySecrets(std::sync::Mutex<std::collections::HashMap<Secret, String>>);

#[cfg(test)]
impl Secrets for MemorySecrets {
    fn read(&self, secret: Secret) -> Result<Option<String>, SecretError> {
        Ok(self.0.lock().unwrap().get(&secret).cloned())
    }

    fn write(&self, secret: Secret, value: Option<&str>) -> Result<(), SecretError> {
        let mut stored = self.0.lock().unwrap();
        match value {
            Some(value) => stored.insert(secret, value.to_owned()),
            None => stored.remove(&secret),
        };
        Ok(())
    }
}
