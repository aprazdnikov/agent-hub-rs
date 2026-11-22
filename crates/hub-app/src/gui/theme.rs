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
                    Color32::from_rgb(248, 249, 247),
                    Color32::WHITE,
                    Color32::from_rgb(28, 37, 33),
                    Color32::from_rgb(84, 100, 92),
                    Color32::from_rgb(213, 221, 215),
                    Color32::from_rgb(13, 100, 79),
                    Color32::from_rgb(220, 238, 229),
                ),
                Theme::Dark => (
                    Color32::from_rgb(23, 27, 25),
                    Color32::from_rgb(31, 37, 34),
                    Color32::from_rgb(230, 236, 231),
                    Color32::from_rgb(169, 185, 175),
                    Color32::from_rgb(65, 78, 70),
                    Color32::from_rgb(142, 218, 185),
                    Color32::from_rgb(43, 68, 55),
                ),
            };
            style.text_styles.insert(TextStyle::Heading, FontId::proportional(26.0));
            style.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
            style.text_styles.insert(TextStyle::Button, FontId::proportional(14.0));
            style.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
            style.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(12.0, 7.0);
            style.spacing.interact_size.y = 32.0;
            style.spacing.combo_width = 160.0;
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

/// Keep the full button hit area and its inner margin in every interaction state.
pub(super) fn tab(ui: &mut egui::Ui, selected: bool, text: &str) -> egui::Response {
    let response = ui.add(egui::Button::selectable(selected, text).frame_when_inactive(false));
    if selected {
        ui.painter().hline(
            response.rect.x_range().shrink(4.0),
            response.rect.bottom() + 1.0,
            Stroke::new(2.0, ui.visuals().hyperlink_color),
        );
    }
    response
}

pub(super) fn primary_button(ui: &mut egui::Ui, enabled: bool, text: &str) -> egui::Response {
    let text = egui::RichText::new(text).color(ui.visuals().panel_fill);
    let button = egui::Button::new(text).fill(ui.visuals().hyperlink_color);
    ui.add_enabled(enabled, button)
}

pub fn surface(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::group(ui.style()).fill(ui.visuals().panel_fill).inner_margin(20).corner_radius(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_label_stays_inside_its_hover_and_focus_ring() {
        let ctx = egui::Context::default();
        configure(&ctx);
        let mut rect = egui::Rect::NOTHING;
        for pass in 0..4 {
            let events = if pass == 1 {
                vec![egui::Event::PointerMoved(rect.center())]
            } else if pass == 2 {
                vec![egui::Event::PointerGone]
            } else {
                Vec::new()
            };
            let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
                let response = tab(ui, false, "Темы Telegram");
                rect = response.rect;
                if pass == 1 {
                    assert!(response.hovered());
                    response.request_focus();
                }
                if pass == 3 {
                    assert!(response.has_focus());
                }
            });
            output.textures_delta.clear();
            let label = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == "Темы Telegram" => {
                        Some(text.galley.rect.translate(text.pos.to_vec2()))
                    }
                    _ => None,
                })
                .unwrap();
            assert!(rect.shrink2(egui::vec2(8.0, 4.0)).contains_rect(label));
        }
    }

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
        assert_eq!(surfaces, [Color32::WHITE, Color32::from_rgb(31, 37, 34), Color32::WHITE]);
    }
}
