//! Shared native styling. Both palettes are installed once; egui follows OS changes.

#[cfg(target_os = "linux")]
pub mod linux;

mod choice;
pub(super) use choice::{Choice, detailed_select};
mod layout;
pub(super) use layout::{dialog, remaining_height, workspace_panes};
mod palette;
pub use palette::configure;

#[cfg(test)]
use egui::Theme;
use egui::{Color32, Stroke};

/// A keyboard-focusable section selector with the same label for screen readers.
pub(super) fn nav_chip(
    ui: &mut egui::Ui,
    selected: bool,
    text: &str,
    icon: &str,
) -> egui::Response {
    let response = kylig_ui::Chip::new(text)
        .icon(icon)
        .selected(selected)
        .color(ui.visuals().hyperlink_color)
        .show(ui)
        .response;
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::Button, ui.is_enabled(), selected, text)
    });
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect,
            16,
            Stroke::new(1.5, ui.visuals().hyperlink_color),
            egui::StrokeKind::Inside,
        );
    }
    response
}

pub(super) fn primary_button(ui: &mut egui::Ui, enabled: bool, text: &str) -> egui::Response {
    ui.add_enabled(enabled, kylig_ui::Button::new(text).primary())
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

/// (bg, fg) for a status tone; also the single source for diff line colors in `tasks/result.rs`.
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
        .inner_margin(CHIP_MARGIN)
        .show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(text).small().color(fg)).selectable(false))
        })
        .inner
}

const CHIP_MARGIN: egui::Margin = egui::Margin::symmetric(9, 2);

pub(super) fn card(ui: &egui::Ui) -> kylig_ui::Card {
    kylig_ui::Card::new()
        .options(kylig_ui::CardOptions {
            radius: 10.0,
            padding: if ui.available_width() < 420.0 { 18.0 } else { 24.0 },
            fill: Some(ui.visuals().panel_fill),
            border: Some(ui.visuals().widgets.noninteractive.bg_stroke.color),
            shadow: egui::epaint::Shadow::NONE,
            ..Default::default()
        })
        .width(ui.available_width())
}

pub(super) fn bubble(ui: &egui::Ui, user: bool) -> egui::Frame {
    let radius = egui::CornerRadius::same(15);
    let (corners, fill) = if user {
        (egui::CornerRadius { se: 3, ..radius }, ui.visuals().selection.bg_fill)
    } else {
        (egui::CornerRadius { sw: 3, ..radius }, ui.visuals().panel_fill)
    };
    egui::Frame::new()
        .fill(fill)
        .stroke(ui.visuals().window_stroke)
        .corner_radius(corners)
        .inner_margin(egui::Margin::symmetric(13, 8))
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
                let response = nav_chip(ui, false, "Темы Telegram", kylig_icons::CHATS);
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
            assert_eq!(user.corner_radius, egui::CornerRadius { nw: 15, ne: 15, sw: 15, se: 3 });
            let agent = bubble(ui, false);
            assert_eq!(agent.corner_radius, egui::CornerRadius { nw: 15, ne: 15, sw: 3, se: 15 });
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
                    let width = ui.available_width();
                    let rendered = card(ui).show(ui, |ui| ui.label("карточка"));
                    assert!((rendered.response.rect.width() - width).abs() < 1.0);
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
    fn gui_labels_use_only_covered_glyphs() {
        // Labels must work with the bundled fonts even when no system fonts are available.
        let ctx = egui::Context::default();
        configure(&ctx);
        let used = [
            '…', '›', '‹', '√', '⏵', '⏹', '!', '?', '·', '№', '⌘', '«', '»', '–', '—', '•', '○',
            '„', '“',
        ];
        let drawn: String = used.iter().copied().collect();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label(drawn.as_str());
        });
        output.textures_delta.clear();
        let font = egui::FontId::proportional(14.0);
        for ch in used {
            assert!(
                ctx.fonts_mut(|fonts| fonts.has_glyph(&font, ch)),
                "label glyph {ch} (U+{:04X}) is missing from the proportional font",
                u32::from(ch),
            );
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
