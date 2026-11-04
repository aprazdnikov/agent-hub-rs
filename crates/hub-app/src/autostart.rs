//! Per-user OS registration, independent of bot settings and credentials.

use std::{ffi::OsStr, path::Path, sync::Arc};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(target_os = "macos", all(test, unix)))]
mod macos;
#[cfg(any(target_os = "windows", test))]
mod windows;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Inspect,
    Enable,
    Disable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Enabled,
    Disabled,
    NeedsAttention(String),
}

pub trait Registration: Send + Sync {
    fn apply(&self, action: Action) -> Result<State, String>;
}

pub fn system(program: &Path) -> Result<Arc<dyn Registration>, String> {
    #[cfg(target_os = "linux")]
    {
        linux::registration(program)
    }
    #[cfg(target_os = "windows")]
    {
        windows::registration(program)
    }
    #[cfg(target_os = "macos")]
    {
        macos::registration(program)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        let _ = program;
        Err("Автозапуск недоступен на этой ОС".to_owned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    Manual,
    Login,
}
impl Launch {
    #[must_use]
    pub fn from_args(args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Self {
        if args.into_iter().any(|arg| arg.as_ref() == "--autostart") {
            Self::Login
        } else {
            Self::Manual
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    Visible,
    Minimized,
    Hidden,
}
impl Window {
    #[must_use]
    pub fn initial(launch: Launch, tray: bool, configured: bool) -> Self {
        if launch == Launch::Manual || !tray || !configured {
            Self::Visible
        } else if cfg!(target_os = "linux") {
            Self::Minimized
        } else {
            Self::Hidden
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_flag_and_window_fallback_keep_the_app_reachable() {
        assert_eq!(Launch::from_args(["agent-hub"]), Launch::Manual);
        assert_eq!(Launch::from_args(["agent-hub", "--autostart"]), Launch::Login);
        assert_eq!(Window::initial(Launch::Manual, true, true), Window::Visible);
        assert_eq!(Window::initial(Launch::Login, false, true), Window::Visible);
        assert_eq!(Window::initial(Launch::Login, true, false), Window::Visible);
        assert_eq!(
            Window::initial(Launch::Login, true, true),
            if cfg!(target_os = "linux") { Window::Minimized } else { Window::Hidden }
        );
    }
}
