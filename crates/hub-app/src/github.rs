//! The real update source (GitHub Releases) and installer (`self-replace`).

use std::io::Write;

use futures::future::BoxFuture;

use crate::updater::{CHECK_TIMEOUT, Installer, Source, UpdateError};

// Fixed at build time: the update source is not configurable, so it cannot be redirected.
const LATEST: &str = "https://api.github.com/repos/aprazdnikov/agent-hub-rs/releases/latest";

pub struct GitHub {
    client: reqwest::Client,
}

impl GitHub {
    pub fn new() -> Result<Self, UpdateError> {
        reqwest::Client::builder()
            .user_agent(concat!("agent-hub/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CHECK_TIMEOUT)
            .build()
            .map(|client| Self { client })
            .map_err(|error| UpdateError::Check(error.to_string()))
    }
}

impl Source for GitHub {
    fn latest(&self) -> BoxFuture<'_, Result<String, UpdateError>> {
        Box::pin(async move {
            let check = |error: reqwest::Error| UpdateError::Check(error.to_string());
            self.client
                .get(LATEST)
                .header(reqwest::header::ACCEPT, "application/vnd.github+json")
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(check)?
                .text()
                .await
                .map_err(check)
        })
    }

    fn download(&self, url: String, limit: u64) -> BoxFuture<'_, Result<Vec<u8>, UpdateError>> {
        Box::pin(async move {
            let failed = |error: reqwest::Error| UpdateError::Download(error.to_string());
            let too_large = || UpdateError::Download(format!("файл больше {limit} байт"));
            let mut response = self
                .client
                .get(url)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(failed)?;
            if response.content_length().is_some_and(|length| length > limit) {
                return Err(too_large());
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(failed)? {
                bytes.extend_from_slice(&chunk);
                if u64::try_from(bytes.len()).map_or(true, |length| length > limit) {
                    return Err(too_large());
                }
            }
            Ok(bytes)
        })
    }
}

pub struct SelfReplace;

impl Installer for SelfReplace {
    fn install(&self, binary: Vec<u8>) -> Result<(), UpdateError> {
        let replace = |error: std::io::Error| UpdateError::Replace(error.to_string());
        let current = std::env::current_exe().map_err(replace)?;
        let directory = current
            .parent()
            .ok_or_else(|| UpdateError::Replace("у программы нет каталога".to_owned()))?;
        // Next to the executable, so the final rename stays on one filesystem.
        let mut file = tempfile::NamedTempFile::new_in(directory).map_err(replace)?;
        file.write_all(&binary).map_err(replace)?;
        file.as_file().sync_all().map_err(replace)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(file.path(), std::fs::Permissions::from_mode(0o755))
                .map_err(replace)?;
        }
        self_replace::self_replace(file.path()).map_err(replace)
    }
}
