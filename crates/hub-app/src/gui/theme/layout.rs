use eframe::egui;
use kylig_ui::{Dialog, Pane, SplitView};

pub(crate) fn remaining_height(ui: &egui::Ui) -> f32 {
    (ui.clip_rect().bottom().min(ui.max_rect().bottom()) - ui.cursor().top()).max(0.0)
}

pub(crate) fn workspace_panes(
    ui: &mut egui::Ui,
    id: &'static str,
    mut render: impl FnMut(&mut egui::Ui, bool),
) {
    let size = egui::vec2(ui.available_width(), remaining_height(ui).max(120.0));
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
        SplitView::new(id)
            .pane(Pane::new(260.0).min(195.0).max(480.0))
            .pane(Pane::fill().min(400.0))
            .show(ui, |ui, index| {
                let sidebar = index == 0;
                let mut margin = egui::Margin::ZERO;
                if sidebar {
                    margin.right = 14;
                } else {
                    margin.left = 16;
                }
                egui::Frame::new().inner_margin(margin).show(ui, |ui| render(ui, sidebar));
            });
    });
}

pub(crate) fn dialog<'a>(
    ui: &egui::Ui,
    id: &'static str,
    open: &'a mut bool,
    heading: &str,
) -> Dialog<'a> {
    // Kylig adds 20 points of padding on each side. Keep another 16 outside the dialog.
    let width = (ui.ctx().content_rect().width() - 72.0).clamp(1.0, 540.0);
    Dialog::new(id, open).title(heading).width(width)
}
