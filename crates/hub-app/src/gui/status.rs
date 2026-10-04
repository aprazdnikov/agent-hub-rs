//! The «Статус» tab.

use eframe::egui;

use hub_core::domain::BackendKind;

use crate::gui::look::{active, agent_line, agent_title, status_text};
use crate::supervisor::{BotStatus, Command, Snapshot};

pub fn show(ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
    ui.heading("Бот");
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
    ui.separator();
    egui::Grid::new("status").num_columns(2).show(ui, |ui| {
        ui.label("agent-hub");
        ui.label(env!("CARGO_PKG_VERSION"));
        ui.end_row();
        for kind in BackendKind::ALL {
            let state =
                snapshot.agents.iter().find(|agent| agent.kind == kind).map(|agent| &agent.state);
            ui.label(agent_title(kind));
            ui.label(agent_line(state));
            ui.end_row();
        }
        ui.label("Активных сессий");
        ui.label(active(snapshot).to_string());
        ui.end_row();
        ui.label("Тем");
        ui.label(snapshot.topics.len().to_string());
        ui.end_row();
    });
}
