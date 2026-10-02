//! Where the application keeps its files.

use std::path::PathBuf;

use directories::ProjectDirs;

pub struct AppDirs {
    config: PathBuf,
    data: PathBuf,
}

impl AppDirs {
    #[must_use]
    pub fn locate() -> Option<Self> {
        ProjectDirs::from("", "", "agent-hub").map(|dirs| Self {
            config: dirs.config_dir().to_path_buf(),
            data: dirs.data_dir().to_path_buf(),
        })
    }

    #[must_use]
    pub fn settings(&self) -> PathBuf {
        self.config.join("settings.toml")
    }

    #[must_use]
    pub fn topics(&self) -> PathBuf {
        self.data.join("topics.json")
    }

    #[must_use]
    pub fn logs(&self) -> PathBuf {
        self.data.join("logs")
    }

    #[must_use]
    pub fn lock(&self) -> PathBuf {
        self.data.join("agent-hub.lock")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_live_in_config_and_data_directories() {
        let dirs = AppDirs { config: PathBuf::from("c"), data: PathBuf::from("d") };
        assert_eq!(dirs.settings(), PathBuf::from("c").join("settings.toml"));
        assert_eq!(dirs.topics(), PathBuf::from("d").join("topics.json"));
        assert_eq!(dirs.logs(), PathBuf::from("d").join("logs"));
        assert_eq!(dirs.lock(), PathBuf::from("d").join("agent-hub.lock"));
    }
}
