//! Shared native styling. Both palettes are installed once; egui follows OS changes.

#[cfg(target_os = "linux")]
pub mod linux;

use eframe::egui::{self, Color32, FontId, Stroke, TextStyle, Theme};

pub fn configure(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::System);
    for theme in [Theme::Light, Theme::Dark] {
        ctx.style_mut_of(theme, |style| {
            let (canvas, surface, text, muted, border, accent, selected) = match theme {
                Theme::Light => (
                    Color32::from_rgb(243, 245, 248),
                    Color32::WHITE,
                    Color32::from_rgb(32, 43, 60),
                    Color32::from_rgb(94, 109, 131),
                    Color32::from_rgb(215, 224, 236),
                    Color32::from_rgb(43, 84, 177),
                    Color32::from_rgb(227, 236, 255),
                ),
                Theme::Dark => (
                    Color32::from_rgb(27, 35, 48),
                    Color32::from_rgb(34, 45, 60),
                    Color32::from_rgb(225, 232, 244),
                    Color32::from_rgb(164, 179, 202),
                    Color32::from_rgb(63, 79, 102),
                    Color32::from_rgb(156, 187, 255),
                    Color32::from_rgb(46, 66, 99),
                ),
            };
            style.text_styles.insert(TextStyle::Heading, FontId::proportional(24.0));
            style.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
            style.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
            style.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
            style.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
            style.spacing.item_spacing = egui::vec2(10.0, 10.0);
            style.spacing.button_padding = egui::vec2(12.0, 7.0);
            style.spacing.interact_size.y = 32.0;
            style.spacing.combo_width = 200.0;
            style.visuals.panel_fill = surface;
            style.visuals.window_fill = surface;
            style.visuals.extreme_bg_color = canvas;
            style.visuals.faint_bg_color = canvas;
            style.visuals.text_edit_bg_color = Some(canvas);
            style.visuals.weak_text_color = Some(muted);
            style.visuals.selection.bg_fill = selected;
            style.visuals.selection.stroke = Stroke::new(1.0, accent);
            style.visuals.hyperlink_color = accent;
            style.visuals.window_stroke = Stroke::new(1.0, border);
            style.visuals.window_corner_radius = 10.into();
            style.visuals.menu_corner_radius = 8.into();
            let widgets = &mut style.visuals.widgets;
            for widget in [
                &mut widgets.noninteractive,
                &mut widgets.inactive,
                &mut widgets.hovered,
                &mut widgets.active,
                &mut widgets.open,
            ] {
                widget.bg_fill = surface;
                widget.weak_bg_fill = surface;
                widget.bg_stroke = Stroke::new(1.0, border);
                widget.fg_stroke = Stroke::new(1.0, text);
                widget.corner_radius = 6.into();
                widget.expansion = 0.0;
            }
            widgets.hovered.bg_fill = selected;
            widgets.hovered.weak_bg_fill = selected;
            widgets.hovered.bg_stroke = Stroke::new(1.0, accent);
            widgets.active.bg_fill = selected;
            widgets.active.weak_bg_fill = selected;
            widgets.active.bg_stroke = Stroke::new(2.0, accent);
            widgets.open.bg_fill = selected;
            widgets.open.weak_bg_fill = selected;
        });
    }
}

pub fn page_heading(ui: &mut egui::Ui, title: &str, description: &str) {
    ui.heading(title);
    ui.label(egui::RichText::new(description).weak());
    ui.add_space(20.0);
}

pub fn surface(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::group(ui.style()).fill(ui.visuals().panel_fill).inner_margin(20).corner_radius(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_theme_changes_keep_custom_styles_and_system_preference() {
        let ctx = egui::Context::default();
        // Even a previously forced preference must follow the OS at startup.
        ctx.set_theme(Theme::Dark);
        configure(&ctx);
        let mut surfaces = Vec::new();
        for theme in [Theme::Light, Theme::Dark, Theme::Light] {
            let mut output = ctx.run_ui(
                egui::RawInput { system_theme: Some(theme), ..Default::default() },
                |ui| {
                    assert_eq!(ui.visuals().dark_mode, theme == Theme::Dark);
                    surfaces.push(ui.visuals().panel_fill);
                    ui.label("Настройки");
                },
            );
            output.textures_delta.clear();
            assert_eq!(
                ctx.options(|options| options.theme_preference),
                egui::ThemePreference::System
            );
        }
        assert_eq!(surfaces, [Color32::WHITE, Color32::from_rgb(34, 45, 60), Color32::WHITE]);
    }
}
