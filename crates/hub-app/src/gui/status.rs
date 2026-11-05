//! The «Статус» tab.

use eframe::egui;

use hub_core::domain::BackendKind;

use crate::gui::look::{active, agent_line, agent_title, status_text};
use crate::supervisor::{BotStatus, Command, Snapshot};

pub fn show(ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        super::theme::page_heading(ui, "Статус", "Подключение к Telegram и готовность агентов.");
        super::theme::surface(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("Telegram-бот");
            ui.label(status_text(&snapshot.bot));
            ui.horizontal(|ui| {
                let (start, stop) = match &snapshot.bot {
                    BotStatus::Unconfigured => (false, false),
                    BotStatus::Stopped | BotStatus::Failed(_) => (true, false),
                    BotStatus::Starting | BotStatus::Running { .. } => (false, true),
                };
                if ui.add_enabled(start, egui::Button::new("▶ Запустить")).clicked() {
                    send(Command::Start);
                }
                if ui.add_enabled(stop, egui::Button::new("⏹ Остановить")).clicked() {
                    send(Command::Stop);
                }
            });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(format!("Активных сессий: {}", active(snapshot)));
                ui.separator();
                ui.label(format!("Тем: {}", snapshot.topics.len()));
            });
        });
        ui.add_space(18.0);
        super::theme::surface(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("Агенты");
            ui.add_space(8.0);
            egui::Grid::new("status_agents").num_columns(2).spacing([28.0, 16.0]).show(ui, |ui| {
                for kind in BackendKind::ALL {
                    let state = snapshot
                        .agents
                        .iter()
                        .find(|agent| agent.kind == kind)
                        .map(|agent| &agent.state);
                    ui.strong(agent_title(kind));
                    ui.add(egui::Label::new(agent_line(state)).wrap());
                    ui.end_row();
                }
            });
        });
        ui.add_space(12.0);
        ui.weak(format!("agent-hub {}", env!("CARGO_PKG_VERSION")));
    });
}
