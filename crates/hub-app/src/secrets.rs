//! The bot token lives in the OS credential store, never in a file.

const SERVICE: &str = "agent-hub";
const ACCOUNT: &str = "telegram-token";

/// The store's own message; it never carries the secret itself.
#[derive(Debug, thiserror::Error)]
#[error("системное хранилище ключей недоступно: {0}")]
pub struct SecretError(String);

pub trait Secrets: Send + Sync {
    fn read(&self) -> Result<Option<String>, SecretError>;
    fn write(&self, token: &str) -> Result<(), SecretError>;
}

pub struct Keyring;

fn entry() -> Result<keyring::Entry, SecretError> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(|error| SecretError(error.to_string()))
}

impl Secrets for Keyring {
    fn read(&self) -> Result<Option<String>, SecretError> {
        match entry()?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecretError(error.to_string())),
        }
    }

    fn write(&self, token: &str) -> Result<(), SecretError> {
        entry()?.set_password(token).map_err(|error| SecretError(error.to_string()))
    }
}

#[cfg(test)]
#[derive(Default)]
pub struct MemorySecrets(std::sync::Mutex<Option<String>>);

#[cfg(test)]
impl Secrets for MemorySecrets {
    fn read(&self) -> Result<Option<String>, SecretError> {
        Ok(self.0.lock().unwrap().clone())
    }

    fn write(&self, token: &str) -> Result<(), SecretError> {
        *self.0.lock().unwrap() = Some(token.to_owned());
        Ok(())
    }
}
