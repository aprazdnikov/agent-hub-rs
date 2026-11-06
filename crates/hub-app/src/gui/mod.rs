pub mod app;
mod autostart;
mod folder;
mod log;
pub mod look;
pub mod scale;
mod settings;
mod status;
mod theme;
mod topics;
pub mod tray;

pub use settings::home_or_root;
pub use theme::configure as configure_theme;
#[cfg(target_os = "linux")]
pub use theme::linux::follow_system as follow_system_theme;
