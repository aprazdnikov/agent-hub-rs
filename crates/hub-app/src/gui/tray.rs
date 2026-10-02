//! The tray icon: health colour, tooltip and the menu.

use std::sync::mpsc;

use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{
    BadIcon, Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};

use crate::gui::look::{Health, health, tooltip, update_item};
use crate::logging::Repaint;
use crate::supervisor::{BotStatus, Snapshot};
use crate::updater::UpdateState;

const SIZE: u32 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    Open,
    Toggle,
    Quit,
}

#[derive(Debug, thiserror::Error)]
pub enum TrayError {
    #[error("иконка трея: {0}")]
    Icon(#[from] BadIcon),
    #[error("меню трея: {0}")]
    Menu(#[from] tray_icon::menu::Error),
    #[error("трей недоступен: {0}")]
    Tray(#[from] tray_icon::Error),
}

pub struct Tray {
    icon: TrayIcon,
    toggle: MenuItem,
    update: MenuItem,
    health: Health,
}

const GREEN: [u8; 3] = [46, 160, 67];
const GREY: [u8; 3] = [140, 140, 140];
const RED: [u8; 3] = [210, 50, 45];

/// RGBA of a filled disc on a transparent square of side `SIZE`.
fn disc(rgb: [u8; 3]) -> Vec<u8> {
    let radius = f64::from(SIZE) / 2.0;
    let [red, green, blue] = rgb;
    (0..SIZE * SIZE)
        .flat_map(|index| {
            let column = f64::from(index % SIZE) + 0.5;
            let row = f64::from(index / SIZE) + 0.5;
            let inside = (column - radius).hypot(row - radius) <= radius - 1.0;
            [red, green, blue, if inside { u8::MAX } else { 0 }]
        })
        .collect()
}

fn icon(health: Health) -> Result<Icon, BadIcon> {
    let rgb = match health {
        Health::Connected => GREEN,
        Health::Stopped => GREY,
        Health::Failed => RED,
    };
    Icon::from_rgba(disc(rgb), SIZE, SIZE)
}

/// The window icon: RGBA pixels and the side length.
#[must_use]
pub fn app_icon() -> (Vec<u8>, u32) {
    (disc(GREEN), SIZE)
}

impl Tray {
    pub fn create(repaint: Repaint) -> Result<(Self, mpsc::Receiver<TrayAction>), TrayError> {
        let open = MenuItem::new("Открыть", true, None);
        let toggle = MenuItem::new("Запустить бота", false, None);
        let update = MenuItem::new("Обновлений нет", false, None);
        let quit = MenuItem::new("Выход", true, None);
        let menu = Menu::new();
        menu.append_items(&[&open, &toggle, &update, &PredefinedMenuItem::separator(), &quit])?;
        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("agent-hub")
            // Left click opens the window; the menu stays on the right button, except on macOS
            // where a left click on a status item conventionally shows the menu.
            .with_menu_on_left_click(cfg!(target_os = "macos"))
            .with_icon(icon(Health::Stopped)?)
            .build()?;
        let (sender, actions) = mpsc::channel();
        let ids: [(MenuId, TrayAction); 4] = [
            (open.id().clone(), TrayAction::Open),
            (toggle.id().clone(), TrayAction::Toggle),
            // Installing is a button in the window, so the item only brings the window up.
            (update.id().clone(), TrayAction::Open),
            (quit.id().clone(), TrayAction::Quit),
        ];
        let menu_sender = sender.clone();
        let menu_repaint = repaint.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some((_, action)) = ids.iter().find(|(id, _)| *id == event.id) {
                // The window is gone only while the process exits.
                let _ = menu_sender.send(*action);
                menu_repaint();
            }
        }));
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            let open = match event {
                TrayIconEvent::DoubleClick { .. } => true,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } => cfg!(not(target_os = "macos")),
                // A foreign non-exhaustive enum: hover and other buttons do nothing.
                _other => false,
            };
            if open {
                // The window is gone only while the process exits.
                let _ = sender.send(TrayAction::Open);
                repaint();
            }
        }));
        Ok((Self { icon, toggle, update, health: Health::Stopped }, actions))
    }

    pub fn show(&mut self, snapshot: &Snapshot, updates: &UpdateState) {
        let health = health(&snapshot.bot);
        if health != self.health {
            match icon(health) {
                Ok(icon) => {
                    if let Err(error) = self.icon.set_icon(Some(icon)) {
                        tracing::warn!(%error, "tray icon not updated");
                    }
                }
                Err(error) => tracing::warn!(%error, "tray icon not drawn"),
            }
            self.health = health;
        }
        if let Err(error) = self.icon.set_tooltip(Some(tooltip(snapshot))) {
            tracing::warn!(%error, "tray tooltip not updated");
        }
        let (text, enabled) = match &snapshot.bot {
            BotStatus::Running { .. } | BotStatus::Starting => ("Остановить бота", true),
            BotStatus::Stopped | BotStatus::Failed(_) => ("Запустить бота", true),
            BotStatus::Unconfigured => ("Запустить бота", false),
        };
        self.toggle.set_text(text);
        self.toggle.set_enabled(enabled);
        let (text, enabled) = update_item(updates);
        self.update.set_text(text);
        self.update.set_enabled(enabled);
    }
}
