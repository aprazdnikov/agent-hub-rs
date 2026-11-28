use eframe::egui;
use kylig_ui::{Button, Card, CardOptions, Chip, ProgressRing, Segmented, TabsOptions};

#[path = "../../../../crates/hub-app/src/gui/theme/layout.rs"]
mod layout;
pub(crate) use layout::{dialog, remaining_height, workspace_panes};

pub(crate) const AGENTS: [&str; 6] =
    ["Claude Code", "Codex", "Qwen Code", "Hermes", "OpenCode", "Pi"];

pub(crate) fn choice_chips(
    ui: &mut egui::Ui,
    selected: &mut &'static str,
    options: impl IntoIterator<Item = &'static str>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
        for name in options {
            if Chip::new(name).selected(*selected == name).show(ui).response.clicked() {
                *selected = name;
            }
        }
    });
}

pub(crate) fn agent_choice(ui: &mut egui::Ui, label: &str, selected: &mut &'static str) {
    ui.push_id(label, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(label);
        if ui.available_width() >= 450.0 {
            Segmented::new(selected, AGENTS.map(|name| (name, name)))
                .options(TabsOptions { padding: 10.0, ..Default::default() })
                .show(ui);
        } else {
            choice_chips(ui, selected, AGENTS);
        }
    });
}

pub(crate) fn title(ui: &mut egui::Ui, heading: &str, subtitle: &str) {
    ui.heading(heading);
    if !subtitle.is_empty() {
        ui.weak(subtitle);
    }
    ui.add_space(12.0);
}

pub(crate) fn card<R>(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    Card::new()
        .options(CardOptions {
            radius: 10.0,
            padding: if ui.available_width() < 420.0 { 18.0 } else { 24.0 },
            fill: Some(ui.visuals().panel_fill),
            border: Some(ui.visuals().widgets.noninteractive.bg_stroke.color),
            shadow: egui::epaint::Shadow::NONE,
            ..Default::default()
        })
        .width(ui.available_width())
        .show(ui, contents)
        .inner
}

pub(crate) fn primary(ui: &mut egui::Ui, label: &str) -> bool {
    ui.add(Button::new(label).primary()).clicked()
}

pub(crate) fn action(ui: &mut egui::Ui, label: &str) -> bool {
    ui.add(Button::new(label)).clicked()
}

#[derive(Default)]
pub(crate) struct Wait {
    pending: Option<(f64, String)>,
    reveal: bool,
}

impl Wait {
    pub(crate) fn start(&mut self, ui: &egui::Ui, label: &str) {
        self.reveal = true;
        self.pending = Some((ui.input(|i| i.time) + 1.35, label.to_owned()));
        ui.ctx().request_repaint();
    }

    pub(crate) fn busy(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn draw(&mut self, ui: &mut egui::Ui) -> bool {
        let Some((end, label)) = &self.pending else { return false };
        if ui.input(|i| i.time) >= *end {
            self.pending = None;
            return true;
        }
        let row = ui.horizontal_wrapped(|ui| {
            ui.add(ProgressRing::indeterminate().size(20.0));
            ui.strong(label);
        });
        if std::mem::take(&mut self.reveal) && !ui.clip_rect().contains_rect(row.response.rect) {
            row.response.scroll_to_me(None);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        false
    }
}
