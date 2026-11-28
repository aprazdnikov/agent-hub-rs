//! Application events and setup actions inside Diagnostics.

use std::path::Path;

use eframe::egui::{self, Color32};
use kylig_ui::{Button, Select, SelectItem, Switch, TextField};
use tracing::Level;

use crate::gui::{folder, theme};
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
    search: String,
    folder_error: Option<String>,
}

impl Default for LogView {
    fn default() -> Self {
        Self { level: Level::INFO, follow: true, search: String::new(), folder_error: None }
    }
}

impl LogView {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        lines: &[LogLine],
        folder_path: &Path,
    ) -> Option<LogAction> {
        self.filters(ui);
        let query = self.search.trim().to_lowercase();
        let rows: Vec<_> = lines
            .iter()
            .filter(|row| {
                row.level <= self.level
                    && (query.is_empty()
                        || row.time.to_ascii_lowercase().contains(&query)
                        || row.level.as_str().to_ascii_lowercase().contains(&query)
                        || row.text.to_lowercase().contains(&query))
            })
            .collect();
        let compact = ui.available_width() < 450.0;
        let resume = ui
            .horizontal_wrapped(|ui| {
                let changed =
                    ui.add(Switch::new(&mut self.follow).label("Автопрокрутка")).changed();
                if ui
                    .add_enabled(!rows.is_empty(), Button::new("Копировать"))
                    .on_hover_text("Скопировать показанные строки")
                    .clicked()
                {
                    ui.ctx().copy_text(
                        rows.iter()
                            .map(|row| {
                                format!("{}  {:<5}  {}", row.time, row.level.as_str(), row.text)
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    );
                }
                let label = if compact { "Папка" } else { "Папка журналов" };
                if ui
                    .add(Button::new(label))
                    .on_hover_text(folder_path.display().to_string())
                    .clicked()
                {
                    self.folder_error =
                        folder::open(folder_path).err().map(|error| error.to_string());
                }
                changed && self.follow
            })
            .inner;
        if let Some(error) = &self.folder_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        console(ui, &rows, lines.len(), &mut self.follow, resume)
    }

    fn filters(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let level_width = (width * 0.32).clamp(100.0, 170.0);
        ui.horizontal_top(|ui| {
            ui.add(
                TextField::new(&mut self.search)
                    .id_salt("journal-search")
                    .label("Поиск в журнале")
                    .placeholder("Текст или ID")
                    .clearable(true)
                    .width((width - level_width - ui.spacing().item_spacing.x).max(50.0)),
            );
            let levels = LEVELS.map(|level| SelectItem::new(level, level.as_str()));
            ui.add(
                Select::new(&mut self.level, &levels)
                    .label("Уровень")
                    .id_salt("journal-level")
                    .search(false)
                    .width(level_width),
            );
        });
    }
}

const CONSOLE_BG: Color32 = Color32::from_rgb(18, 25, 25);
const CONSOLE_BAR: Color32 = Color32::from_rgb(34, 44, 43);
const CONSOLE_TEXT: Color32 = Color32::from_rgb(229, 238, 234);
const CONSOLE_MUTED: Color32 = Color32::from_rgb(159, 182, 174);

fn draw_line(ui: &mut egui::Ui, row: &LogLine) -> Option<LogAction> {
    let clock = row.time.split_once('T').and_then(|(_, t)| t.get(..8)).unwrap_or(&row.time);
    let color = match row.level {
        Level::ERROR => Color32::from_rgb(255, 153, 147),
        Level::WARN => Color32::from_rgb(242, 201, 118),
        Level::INFO => Color32::from_rgb(132, 216, 186),
        Level::DEBUG | Level::TRACE => Color32::from_rgb(167, 181, 231),
    };
    let mut line = egui::text::LayoutJob::default();
    let style = |color| egui::TextFormat::simple(egui::FontId::monospace(14.0), color);
    line.append(clock, 0.0, style(CONSOLE_MUTED));
    line.append(&format!("  {:<5}  ", row.level.as_str()), 0.0, style(color));
    line.append(&row.text, 0.0, style(CONSOLE_TEXT));
    ui.add(egui::Label::new(line).wrap().selectable(true)).on_hover_text(&row.time);
    let rejected = row.rejected?;
    ui.horizontal_wrapped(|ui| {
        // Each action needs its own hit rectangle even when the row wraps.
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        if let Some(chat) = rejected.chat
            && ui.link("Использовать chat_id").clicked()
        {
            return Some(LogAction::UseChat(chat));
        }
        if let Some(user) = rejected.user
            && ui.link("Добавить user_id").clicked()
        {
            return Some(LogAction::AddUser(user));
        }
        None
    })
    .inner
}

fn console(
    ui: &mut egui::Ui,
    rows: &[&LogLine],
    total: usize,
    follow: &mut bool,
    resume: bool,
) -> Option<LogAction> {
    let mut action = None;
    egui::Frame::new().fill(CONSOLE_BG).corner_radius(9.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::Frame::new()
            .fill(CONSOLE_BAR)
            .corner_radius(egui::CornerRadius { nw: 9, ne: 9, sw: 0, se: 0 })
            .inner_margin(egui::Margin::symmetric(14, 9))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("agent-hub.log").monospace().color(CONSOLE_TEXT));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("{} / {total}   UTC", rows.len()))
                                .monospace()
                                .color(CONSOLE_MUTED),
                        );
                    });
                });
            });
        egui::Frame::new().inner_margin(egui::Margin::symmetric(14, 12)).show(ui, |ui| {
            *ui.visuals_mut() = egui::Visuals::dark();
            ui.visuals_mut().hyperlink_color = Color32::from_rgb(132, 216, 186);
            let height = theme::remaining_height(ui).max(76.0);
            let scroll = egui::ScrollArea::vertical()
                .id_salt("log-lines")
                .max_height(height)
                .min_scrolled_height(height)
                .auto_shrink([false, false])
                .stick_to_bottom(*follow)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 7.0;
                    if rows.is_empty() {
                        ui.colored_label(
                            CONSOLE_MUTED,
                            if total == 0 {
                                "Здесь появятся события приложения."
                            } else {
                                "Совпадений нет. Измените поиск или уровень сообщений."
                            },
                        );
                    }
                    for row in rows {
                        if let Some(picked) = draw_line(ui, row) {
                            action = Some(picked);
                        }
                    }
                    if resume {
                        ui.scroll_to_cursor_animation(
                            Some(egui::Align::BOTTOM),
                            egui::style::ScrollAnimation::none(),
                        );
                    }
                });
            let remaining = scroll.content_size.y
                - scroll.inner_rect.height()
                - scroll.state.clamped_offset().y;
            if remaining > 1.0 && !resume {
                *follow = false;
            }
        });
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::Rejected;

    #[test]
    fn filtered_journal_copies_the_full_timestamp_and_uses_the_matching_id() {
        let ctx = egui::Context::default();
        crate::gui::theme::configure(&ctx);
        let mut view = LogView { search: "оТкЛ".into(), ..Default::default() };
        let lines = [
            LogLine {
                time: "2026-04-05T01:02:03Z".into(),
                level: Level::WARN,
                text: "Запрос отклонён".into(),
                rejected: Some(Rejected { chat: Some(-51), user: Some(701) }),
            },
            LogLine {
                time: "2026-04-05T01:02:04Z".into(),
                level: Level::INFO,
                text: "Доставка завершена".into(),
                rejected: None,
            },
        ];
        let mut frame = |events| {
            let mut action = None;
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(400.0, 720.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default()
                        .show(ui, |ui| action = view.show(ui, &lines, Path::new("logs")));
                },
            );
            output.textures_delta.clear();
            (output, action)
        };
        let mut click = |label| {
            frame(Vec::new());
            let (out, _) = frame(Vec::new());
            let pos = out
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => {
                        Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                    }
                    _ => None,
                })
                .unwrap();
            let press = |pressed| egui::Event::PointerButton {
                pos,
                pressed,
                button: egui::PointerButton::Primary,
                modifiers: egui::Modifiers::NONE,
            };
            frame(vec![egui::Event::PointerMoved(pos)]);
            frame(vec![press(true)]);
            frame(vec![press(false)])
        };
        let (output, _) = click("Копировать");
        assert!(output.platform_output.commands.iter().any(|c| matches!(c, egui::OutputCommand::CopyText(s) if s == "2026-04-05T01:02:03Z  WARN   Запрос отклонён")));
        assert_eq!(click("Использовать chat_id").1, Some(LogAction::UseChat(-51)));
        assert_eq!(click("Добавить user_id").1, Some(LogAction::AddUser(701)));
    }
}
