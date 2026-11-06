//! Window zoom is independent of bot configuration and multiplies the system DPI.

use std::io::Read;
use std::path::{Path, PathBuf};

use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scale(u16);

impl Default for Scale {
    fn default() -> Self {
        Self(100)
    }
}

impl Scale {
    #[must_use]
    pub fn new(percent: u16) -> Option<Self> {
        ((75..=150).contains(&percent) && percent.is_multiple_of(5)).then_some(Self(percent))
    }

    #[must_use]
    pub fn percent(self) -> u16 {
        self.0
    }

    pub fn apply(self, ctx: &egui::Context) {
        ctx.options_mut(|options| options.zoom_with_keyboard = false);
        ctx.set_zoom_factor(f32::from(self.0) / 100.0);
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        #[derive(Deserialize)]
        struct File {
            scale: u16,
        }
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(format!("Не удалось прочитать {}: {error}", path.display())),
        };
        let mut text = String::new();
        file.take(4097)
            .read_to_string(&mut text)
            .map_err(|error| format!("Не удалось прочитать {}: {error}", path.display()))?;
        if text.len() > 4096 {
            return Err(format!("Файл {} слишком велик", path.display()));
        }
        let parsed: File = toml::from_str(&text)
            .map_err(|error| format!("Некорректный файл {}: {error}", path.display()))?;
        Self::new(parsed.scale).ok_or_else(|| format!("Некорректный масштаб в {}", path.display()))
    }

    pub fn save(self, path: &Path) -> std::io::Result<()> {
        #[derive(Serialize)]
        struct File {
            scale: u16,
        }
        let text = toml::to_string(&File { scale: self.0 }).map_err(std::io::Error::other)?;
        crate::atomic::write_atomic(path, text.as_bytes())
    }
}

#[derive(Default)]
pub struct Control {
    path: Option<PathBuf>,
    saved: Scale,
    draft: Scale,
    error: Option<String>,
    warning: Option<String>,
}

impl Control {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        let (saved, warning) = match Scale::load(&path) {
            Ok(saved) => (saved, None),
            Err(error) => (Scale::default(), Some(error)),
        };
        Self { path: Some(path), saved, draft: saved, error: None, warning }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        ui.strong("Масштаб интерфейса");
        let mut percent = self.draft.percent();
        ui.horizontal_wrapped(|ui| {
            if ui.add(egui::Slider::new(&mut percent, 75..=150).step_by(5.0).suffix("%")).changed()
                && let Some(scale) = Scale::new(percent)
            {
                self.draft = scale;
                self.draft.apply(ui.ctx());
            }
            if ui.button("Сбросить до 100%").clicked() {
                self.draft = Scale::default();
                self.draft.apply(ui.ctx());
            }
        });
        ui.horizontal_wrapped(|ui| {
            let changed = self.draft != self.saved;
            if ui
                .add_enabled(
                    changed || self.warning.is_some(),
                    egui::Button::new("Сохранить масштаб"),
                )
                .clicked()
            {
                match self.path.as_ref().map(|path| self.draft.save(path)) {
                    Some(Ok(())) => {
                        self.saved = self.draft;
                        self.error = None;
                        self.warning = None;
                    }
                    Some(Err(error)) => {
                        self.error = Some(format!("Не удалось сохранить масштаб: {error}"));
                    }
                    None => {
                        self.error = Some("Не указан путь к настройкам масштаба".to_owned());
                    }
                }
            }
            if ui.add_enabled(changed, egui::Button::new("Отменить масштаб")).clicked()
            {
                self.draft = self.saved;
                self.draft.apply(ui.ctx());
                self.error = None;
            }
        });
        if let Some(warning) = &self.warning {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("{warning}. Используется 100%. Сохранение заменит этот файл."),
            );
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.weak("Относительно системного масштаба. Сохраняется отдельно от настроек бота.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_invalid_scale_uses_safe_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.toml");
        assert_eq!(Scale::load(&path).unwrap().percent(), 100);
        for text in ["scale = 74", "scale = 151", "scale = 101", "scale = -1", "scale = 'bad'"] {
            std::fs::write(&path, text).unwrap();
            assert!(Scale::load(&path).is_err(), "{text}");
        }
    }

    #[test]
    fn oversized_or_unreadable_file_is_reported_without_modification() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.toml");
        let content = "x".repeat(5000);
        std::fs::write(&path, &content).unwrap();
        assert!(Scale::load(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        assert!(Scale::load(dir.path()).is_err());
    }

    #[test]
    fn valid_scale_survives_restart_without_bot_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/ui.toml");
        for percent in [75, 100, 150] {
            Scale::new(percent).unwrap().save(&path).unwrap();
            assert_eq!(Scale::load(&path).unwrap().percent(), percent);
        }
        assert!(!dir.path().join("settings.toml").exists());
    }
    fn frame(
        ctx: &egui::Context,
        control: &mut Control,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 360.0),
                )),
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| control.show(ui));
            },
        );
        output.textures_delta.clear();
        output
    }

    fn click(ctx: &egui::Context, control: &mut Control, label: &str) {
        let output = frame(ctx, control, vec![]);
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                }
                _ => None,
            })
            .unwrap();
        for pressed in [true, false] {
            frame(
                ctx,
                control,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
    }

    #[test]
    fn corrupt_file_is_visible_and_retained_until_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.toml");
        std::fs::write(&path, "scale = 500").unwrap();
        let mut control = Control::new(path.clone());
        let ctx = egui::Context::default();
        let output = frame(&ctx, &mut control, vec![]);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text().contains("Сохранение заменит этот файл"))));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "scale = 500");
        click(&ctx, &mut control, "Сохранить масштаб");
        assert_eq!(Scale::load(&path).unwrap().percent(), 100);
        assert!(control.warning.is_none());
    }

    #[test]
    fn reset_cancel_and_save_work_without_bot_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.toml");
        Scale::new(125).unwrap().save(&path).unwrap();
        let mut control = Control::new(path.clone());
        let ctx = egui::Context::default();
        control.saved.apply(&ctx);
        frame(&ctx, &mut control, vec![]);
        click(&ctx, &mut control, "Сбросить до 100%");
        frame(&ctx, &mut control, vec![]);
        assert!((ctx.zoom_factor() - 1.0).abs() < f32::EPSILON);
        assert_eq!(Scale::load(&path).unwrap().percent(), 125);
        click(&ctx, &mut control, "Отменить масштаб");
        frame(&ctx, &mut control, vec![]);
        assert!((ctx.zoom_factor() - 1.25).abs() < f32::EPSILON);
        click(&ctx, &mut control, "Сбросить до 100%");
        click(&ctx, &mut control, "Сохранить масштаб");
        assert_eq!(Scale::load(&path).unwrap().percent(), 100);
        assert!(!dir.path().join("settings.toml").exists());
    }

    #[test]
    fn keyboard_zoom_shortcuts_do_not_change_the_saved_scale() {
        let ctx = egui::Context::default();
        let mut control = Control::default();
        Scale::new(125).unwrap().apply(&ctx);
        frame(&ctx, &mut control, vec![]);
        for key in [egui::Key::Plus, egui::Key::Equals, egui::Key::Minus, egui::Key::Num0] {
            frame(
                &ctx,
                &mut control,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                }],
            );
            frame(&ctx, &mut control, vec![]);
            assert!((ctx.zoom_factor() - 1.25).abs() < f32::EPSILON, "{key:?}");
        }
    }

    #[test]
    fn scale_multiplies_dpi_and_survives_theme_changes() {
        let ctx = egui::Context::default();
        Scale::new(150).unwrap().apply(&ctx);
        for (dpi, theme, expected) in
            [(2.0, egui::Theme::Light, 3.0), (1.5, egui::Theme::Dark, 2.25)]
        {
            let mut input = egui::RawInput { system_theme: Some(theme), ..Default::default() };
            input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().native_pixels_per_point =
                Some(dpi);
            let mut output = ctx.run_ui(input, |ui| {
                ui.label("test");
            });
            output.textures_delta.clear();
            assert!((ctx.pixels_per_point() - expected).abs() < f32::EPSILON);
        }
    }
}
