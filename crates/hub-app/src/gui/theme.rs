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

/// Semantic color pair for status surfaces; resolved per active theme.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Tone {
    Accent,
    Warning,
    Error,
    Neutral,
}

/// warning bg `58,49,32` / fg `229,192,123` in dark, `251,240,220` / `138,90,0` in light.
pub(super) fn warning(ui: &egui::Ui) -> (Color32, Color32) {
    if ui.visuals().dark_mode {
        (Color32::from_rgb(58, 49, 32), Color32::from_rgb(229, 192, 123))
    } else {
        (Color32::from_rgb(251, 240, 220), Color32::from_rgb(138, 90, 0))
    }
}

/// review ring `229,192,123` in dark, `183,121,31` in light.
pub(super) fn review_color(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode {
        Color32::from_rgb(229, 192, 123)
    } else {
        Color32::from_rgb(183, 121, 31)
    }
}

/// (bg, fg) for a status tone; error fg duplicates `tasks/result.rs` diff colors until Task 8 migrates it.
pub(super) fn tone_colors(ui: &egui::Ui, tone: Tone) -> (Color32, Color32) {
    let visuals = ui.visuals();
    match tone {
        Tone::Accent => (visuals.selection.bg_fill, visuals.hyperlink_color),
        Tone::Warning => warning(ui),
        Tone::Error => (
            visuals.extreme_bg_color,
            if visuals.dark_mode {
                Color32::from_rgb(253, 166, 160)
            } else {
                Color32::from_rgb(148, 49, 47)
            },
        ),
        Tone::Neutral => (visuals.extreme_bg_color, visuals.weak_text_color()),
    }
}

/// Status capsule: small text on the tone background.
pub(super) fn chip(ui: &mut egui::Ui, tone: Tone, text: &str) -> egui::Response {
    let (bg, fg) = tone_colors(ui, tone);
    egui::Frame::new()
        .fill(bg)
        .corner_radius(egui::CornerRadius::same(255))
        .inner_margin(egui::Margin::symmetric(9, 2))
        .show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(text).small().color(fg)).selectable(false))
        })
        .inner
}

pub(super) fn card(ui: &egui::Ui) -> egui::Frame {
    egui::Frame::new()
        .fill(ui.visuals().panel_fill)
        .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
        .corner_radius(10)
        .inner_margin(egui::Margin::symmetric(14, 12))
}

pub(super) fn bubble(ui: &egui::Ui, user: bool) -> egui::Frame {
    // The 4px corner at the sender's lower edge reads as the bubble tail (design spec «Чат задачи»).
    let corner_radius = if user {
        egui::CornerRadius { nw: 12, ne: 12, sw: 12, se: 4 }
    } else {
        egui::CornerRadius { nw: 12, ne: 12, sw: 4, se: 12 }
    };
    let frame = egui::Frame::new()
        .corner_radius(corner_radius)
        .inner_margin(egui::Margin::symmetric(12, 9));
    if user {
        frame.fill(ui.visuals().selection.bg_fill)
    } else {
        frame
            .fill(ui.visuals().panel_fill)
            .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
    }
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
    fn bubble_frames_have_a_small_tail_corner_at_the_sender_side() {
        let ctx = egui::Context::default();
        configure(&ctx);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let user = bubble(ui, true);
            assert_eq!(user.corner_radius, egui::CornerRadius { nw: 12, ne: 12, sw: 12, se: 4 });
            let agent = bubble(ui, false);
            assert_eq!(agent.corner_radius, egui::CornerRadius { nw: 12, ne: 12, sw: 4, se: 12 });
        });
        output.textures_delta.clear();
    }

    #[test]
    fn status_tokens_have_dark_variants_and_chips_render() {
        let ctx = egui::Context::default();
        configure(&ctx);
        let mut warning_pairs = Vec::new();
        let mut error_fgs = Vec::new();
        let mut review_colors = Vec::new();
        for theme in [Theme::Light, Theme::Dark] {
            let mut output = ctx.run_ui(
                egui::RawInput { system_theme: Some(theme), ..Default::default() },
                |ui| {
                    let (bg, fg) = tone_colors(ui, Tone::Warning);
                    assert_ne!(bg, fg);
                    warning_pairs.push((bg, fg));
                    assert_ne!(tone_colors(ui, Tone::Accent).0, tone_colors(ui, Tone::Accent).1);
                    assert_ne!(tone_colors(ui, Tone::Neutral).0, tone_colors(ui, Tone::Neutral).1);
                    error_fgs.push(tone_colors(ui, Tone::Error).1);
                    review_colors.push(review_color(ui));
                    assert!(chip(ui, Tone::Warning, "Нужна проверка").rect.width() > 0.0);
                    card(ui).show(ui, |ui| {
                        ui.label("карточка");
                    });
                    bubble(ui, true).show(ui, |ui| {
                        ui.label("вы");
                    });
                    bubble(ui, false).show(ui, |ui| {
                        ui.label("агент");
                    });
                },
            );
            output.textures_delta.clear();
        }
        assert_ne!(warning_pairs[0], warning_pairs[1], "warning token needs a dark variant");
        assert_ne!(error_fgs[0], error_fgs[1], "error token needs a dark variant");
        assert_ne!(review_colors[0], review_colors[1], "review ring needs a dark variant");
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
