#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1080.0, 780.0])
            .with_min_inner_size([380.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Agent Hub · Интерактивный макет",
        options,
        Box::new(|cc| Ok(Box::new(hub_ui_prototype::Prototype::new(cc)))),
    )
}

#[cfg(target_arch = "wasm32")]
fn main() {}
