//! Throwaway offscreen renderer: drives the real `HubApp` GUI with fictional
//! demo data and rasterizes two 960x640 PNG screenshots without a display.

mod demo;
mod raster;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail};
use eframe::App as _;
use eframe::egui::{self, Pos2, Rect, Theme, Vec2};
use hub_app::gui::app::{AppSetup, Exit, HubApp};
use hub_app::logging::LogBuffer;
use hub_app::supervisor::Command;
use hub_app::updater::{UpdateCommand, UpdateState};
use tokio::sync::{mpsc, watch};

const WIDTH: u32 = 960;
const HEIGHT: u32 = 640;
const SIZE: Vec2 = egui::vec2(WIDTH as f32, HEIGHT as f32);
/// The sidebar panel is ~226pt wide; sidebar-only labels live left of this.
const SIDEBAR_LIMIT: f32 = 240.0;

struct Harness {
    app: HubApp,
    ctx: egui::Context,
    frame: eframe::Frame,
    commands: mpsc::Receiver<Command>,
    demo: demo::Demo,
}

impl Harness {
    fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SIZE)),
            system_theme: Some(Theme::Light),
            events,
            ..egui::RawInput::default()
        };
        let Self { app, ctx, frame, .. } = self;
        let mut output = ctx.run_ui(input, |ui| app.ui(ui, frame));
        // The atlas is read straight from FontsView; no backend textures exist.
        output.textures_delta.clear();
        self.drain_commands();
        output
    }

    /// Stand in for the core: answer task reads with the demo book.
    fn drain_commands(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            match command {
                Command::LocalTasks { project, backend: _backend, request: _request, reply } => {
                    let snapshot = self.demo.task_snapshot(&project);
                    let _ = reply.send(Ok(snapshot));
                }
                Command::Tasks { key: _key, project, backend: _backend, request: _request, reply } => {
                    let snapshot = self.demo.task_snapshot(&project);
                    let _ = reply.send(Ok(snapshot));
                }
                // Foreign enum with many variants; the demo flow triggers none of them.
                _ => {}
            }
        }
    }

    fn text_rect(output: &egui::FullOutput, label: &str, max_x: Option<f32>) -> Option<Rect> {
        output.shapes.iter().find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) if text.galley.text() == label => {
                let rect = text.galley.rect.translate(text.pos.to_vec2());
                match max_x {
                    Some(limit) if rect.center().x > limit => None,
                    Some(_) | None => Some(rect),
                }
            }
            _ => None,
        })
    }

    fn text_pos(output: &egui::FullOutput, label: &str, max_x: Option<f32>) -> Option<Pos2> {
        Self::text_rect(output, label, max_x).map(|rect| rect.center())
    }

    fn click_at(&mut self, pos: Pos2) {
        self.frame(vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            self.frame(vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
    }

    fn click(&mut self, label: &str, max_x: Option<f32>) -> anyhow::Result<()> {
        let output = self.frame(Vec::new());
        let pos = Self::text_pos(&output, label, max_x).ok_or_else(|| {
            let labels: Vec<&str> = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) => Some(text.galley.text()),
                    _ => None,
                })
                .collect();
            anyhow!("label not found on screen: {label}; visible: {labels:?}")
        })?;
        self.click_at(pos);
        Ok(())
    }

    fn settle(&mut self, frames: usize) {
        for _ in 0..frames {
            self.frame(Vec::new());
        }
    }

    fn save_png(&mut self, path: &Path) -> anyhow::Result<()> {
        let output = self.frame(Vec::new());
        let primitives = self.ctx.tessellate(output.shapes, 1.0);
        let atlas = self.ctx.fonts(|fonts| epaint::FontsView::image(fonts));
        let background = self.ctx.style_of(Theme::Light).visuals.extreme_bg_color;
        let mut fb = raster::Framebuffer::new(WIDTH as usize, HEIGHT as usize, background);
        raster::rasterize(&primitives, &atlas, &mut fb)?;
        let rgb = fb.into_rgb8();
        let image = image::RgbImage::from_raw(WIDTH, HEIGHT, rgb)
            .ok_or_else(|| anyhow!("framebuffer size does not match the screenshot size"))?;
        image.save(path)?;
        println!("wrote {}", path.display());
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("demo-root");
    let demo = demo::build(&root)?;

    let (commands, command_rx) = mpsc::channel(64);
    let (_snapshot_tx, snapshot_rx) = watch::channel(demo.snapshot.clone());
    let (_update_tx, update_rx) = watch::channel(UpdateState::Idle);
    let (update_commands, _update_rx) = mpsc::channel::<UpdateCommand>(4);
    let exit = Arc::new(Mutex::new(Exit::Staying));
    let temp = manifest.join("tmp");
    std::fs::create_dir_all(&temp)?;

    let app = HubApp::new(AppSetup {
        autostart: Err("render harness: autostart unavailable".to_owned()),
        initial_window: hub_app::autostart::Window::Visible,
        commands,
        snapshot: snapshot_rx,
        logs: LogBuffer::default(),
        tray: None,
        home: temp.clone(),
        logs_dir: temp.clone(),
        ui_settings: temp.join("ui.toml"),
        updates: update_rx,
        update_commands,
        exit,
    });

    let ctx = egui::Context::default();
    hub_app::gui::configure_theme(&ctx);
    ctx.set_pixels_per_point(1.0);
    ctx.all_styles_mut(|style| style.animation_time = 0.0);

    let mut harness = Harness {
        app,
        ctx,
        frame: eframe::Frame::_new_kittest(),
        commands: command_rx,
        demo,
    };

    // The sidebar catalog resolves on a background thread; wait for it.
    let mut ready = false;
    for _ in 0..60 {
        let output = harness.frame(Vec::new());
        if Harness::text_pos(&output, "Atlas", Some(SIDEBAR_LIMIT)).is_some() {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if !ready {
        bail!("sidebar catalog did not resolve within the warmup budget");
    }

    // inbox.png: expand the collapsed Витрина branch, pick the «В очереди» chip.
    let output = harness.frame(Vec::new());
    let vitrina = Harness::text_rect(&output, "Витрина", Some(SIDEBAR_LIMIT))
        .ok_or_else(|| anyhow!("Витрина project row missing from the sidebar"))?;
    // The expand toggle is the 25pt button left of the name button; its center
    // sits 22.5pt left of the name galley's left edge (measured from the layout).
    harness.click_at(egui::pos2(vitrina.left() - 22.5, vitrina.center().y));
    harness.settle(2);
    let output = harness.frame(Vec::new());
    if Harness::text_pos(&output, "Перевод формы заказа", Some(SIDEBAR_LIMIT)).is_none() {
        bail!("Витрина branch did not expand after the toggle click");
    }
    harness.click("В очереди", None)?;
    harness.settle(2);
    let screenshots = manifest.join("../../site/screenshots");
    harness.save_png(&screenshots.join("inbox.png"))?;

    // task-chat.png: Tasks tab, select Витрина, open «Поиск по каталогу».
    harness.click("Задачи", None)?;
    harness.click("Витрина", Some(SIDEBAR_LIMIT))?;
    harness.settle(3);
    harness.click("Поиск по каталогу", Some(SIDEBAR_LIMIT))?;
    harness.settle(3);
    harness.save_png(&screenshots.join("task-chat.png"))?;

    Ok(())
}
