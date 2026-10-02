//! The «Лог» tab.

use std::path::Path;

use eframe::egui;
use tracing::Level;

use crate::gui::folder;
use crate::logging::LogLine;

const LEVELS: [Level; 4] = [Level::ERROR, Level::WARN, Level::INFO, Level::DEBUG];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogAction {
    UseChat(i64),
    AddUser(u64),
}

pub struct LogView {
    level: Level,
    follow: bool,
}

impl Default for LogView {
    fn default() -> Self {
        Self { level: Level::INFO, follow: true }
    }
}

impl LogView {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        lines: &[LogLine],
        folder_path: &Path,
    ) -> Option<LogAction> {
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("level").selected_text(self.level.as_str()).show_ui(
                ui,
                |ui| {
                    for level in LEVELS {
                        ui.selectable_value(&mut self.level, level, level.as_str());
                    }
                },
            );
            ui.checkbox(&mut self.follow, "Автопрокрутка");
            if ui.button("Открыть папку логов").clicked()
                && let Err(error) = folder::open(folder_path)
            {
                tracing::warn!(%error, "log folder not opened");
            }
        });
        ui.separator();
        let mut action = None;
        egui::ScrollArea::vertical().auto_shrink(false).stick_to_bottom(self.follow).show(
            ui,
            |ui| {
                for line in lines.iter().filter(|line| line.level <= self.level) {
                    ui.horizontal_wrapped(|ui| {
                        let text = format!("{} {:5} {}", line.time, line.level.as_str(), line.text);
                        match line.rejected {
                            Some(rejected) => {
                                ui.colored_label(ui.visuals().warn_fg_color, text);
                                if let Some(chat) = rejected.chat
                                    && ui.small_button("Использовать chat_id").clicked()
                                {
                                    action = Some(LogAction::UseChat(chat));
                                }
                                if let Some(user) = rejected.user
                                    && ui.small_button("Добавить user_id").clicked()
                                {
                                    action = Some(LogAction::AddUser(user));
                                }
                            }
                            None => {
                                ui.monospace(text);
                            }
                        }
                    });
                }
            },
        );
        action
    }
}
