use tokio::process::Command;

/// Without it every CLI process flashes a console window under the desktop app.
#[cfg(windows)]
pub(crate) fn hide_window(command: &mut Command) {
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub(crate) fn hide_window(_command: &mut Command) {}
