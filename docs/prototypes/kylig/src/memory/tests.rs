use super::*;
use crate::topics::Topics;

struct Frame {
    context: egui::Context,
    time: f64,
    width: f32,
}

impl Default for Frame {
    fn default() -> Self {
        let context = egui::Context::default();
        context.all_styles_mut(|style| style.animation_time = 0.0);
        kylig_icons::install_weights(&context, &[kylig_icons::Weight::Regular]);
        Self { context, time: 0.0, width: 430.0 }
    }
}

impl Frame {
    fn draw(
        &mut self,
        events: Vec<egui::Event>,
        draw: &mut impl FnMut(&mut egui::Ui),
    ) -> egui::FullOutput {
        self.time += 0.1;
        let mut output = self.context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(self.width, 1600.0),
                )),
                time: Some(self.time),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| draw(ui));
            },
        );
        output.textures_delta.clear();
        output
    }
    fn click(&mut self, label: &str, mut draw: impl FnMut(&mut egui::Ui)) {
        self.draw(Vec::new(), &mut draw);
        let output = self.draw(Vec::new(), &mut draw);
        let pos = output
            .shapes
            .iter()
            .find_map(|s| {
                if let egui::Shape::Text(text) = &s.shape
                    && text.galley.text() == label
                {
                    Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("Missing control {label}"));
        for pressed in [true, false] {
            self.draw(
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                &mut draw,
            );
        }
    }
    fn settle(&mut self, mut draw: impl FnMut(&mut egui::Ui)) -> egui::FullOutput {
        self.time += 2.0;
        self.draw(Vec::new(), &mut draw)
    }
}

#[test]
fn selecting_a_revision_copies_that_exact_version() {
    let mut memory = Memory::default();
    let mut frame = Frame::default();
    frame.click("Проверка перед выпуском", |ui| memory.show(ui));
    assert!(memory.view.wait.busy());
    frame.settle(|ui| memory.show(ui));
    frame.click("r1", |ui| memory.show(ui));
    frame.click("Добавить в пакет контекста", |ui| memory.show(ui));
    frame.click("Открыть пакет", |ui| memory.show(ui));
    frame.click("Собрать пакет", |ui| memory.show(ui));
    frame.settle(|ui| memory.show(ui));
    frame.click("Проверить и скопировать", |ui| memory.show(ui));
    let output = frame.settle(|ui| memory.show(ui));
    assert!(output.platform_output.commands.iter().any(|command|
        matches!(command, egui::OutputCommand::CopyText(packet) if packet.contains("release-check@1")
            && packet.contains("приложить журнал") && !packet.contains("пакеты для Linux и Windows"))
    ));
}

#[test]
fn changing_projects_cancels_the_previous_read() {
    let mut memory = Memory::default();
    let mut frame = Frame::default();
    frame.click("Проверка перед выпуском", |ui| memory.show(ui));
    memory.open_project("sandbox");
    frame.settle(|ui| memory.show(ui));
    assert!(memory.view.detail.is_none());
    assert!(memory.data.store("sandbox").unwrap().entries.is_empty());
}

#[test]
fn cancelling_topic_deletion_preserves_the_row() {
    let mut topics = Topics::default();
    let mut frame = Frame::default();
    frame.click("Проверка обновления", |ui| topics.show(ui));
    frame.click("Удалить тему…", |ui| topics.show(ui));
    frame.click("Отмена", |ui| topics.show(ui));
    frame.click("Удалить тему…", |ui| topics.show(ui));
    frame.click("Удалить тему", |ui| topics.show(ui));
    let output = frame.settle(|ui| topics.show(ui));
    assert!(output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Text(text)
        if text.galley.text().contains("Тема #73 удалена"))));
    assert!(!output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Text(text)
        if text.galley.text() == "Проверка обновления")));
}

#[test]
fn restoring_a_generation_imports_the_snapshot_into_the_selected_project() {
    let mut memory = Memory::default();
    let mut frame = Frame::default();
    frame.click("Управление", |ui| memory.show(ui));
    frame.click("Новое поколение", |ui| memory.show(ui));
    frame.click("Проверка перед выпуском", |ui| memory.show(ui));
    frame.click("Проверить новое поколение", |ui| memory.show(ui));
    frame.click("Подтвердить", |ui| memory.show(ui));
    frame.settle(|ui| memory.show(ui));
    let latest = memory.data.store("agent-hub").unwrap();
    assert_eq!(latest.generation, 2);
    assert!(latest.entry("release-check").is_none());
    memory.open_project("sandbox");
    frame.click("Перейти к импорту", |ui| memory.show(ui));
    frame.click("Последний снимок", |ui| memory.show(ui));
    frame.click("Проверить импорт", |ui| memory.show(ui));
    frame.click("Подтвердить", |ui| memory.show(ui));
    frame.settle(|ui| memory.show(ui));
    assert_eq!(memory.data.store("sandbox").unwrap().entries.len(), 4);
    assert!(memory.data.store("agent-hub").unwrap().entry("release-check").is_none());
}

#[test]
fn exact_lookup_and_record_management_return_to_the_selected_revision() {
    let mut memory = Memory::default();
    let mut frame = Frame::default();
    frame.click("По ключу", |ui| memory.show(ui));
    memory.view.key = "release-check".into();
    memory.view.revision = 1;
    frame.click("Открыть запись", |ui| memory.show(ui));
    frame.settle(|ui| memory.show(ui));
    frame.click("Управление записью", |ui| memory.show(ui));
    frame.click("В архив", |ui| memory.show(ui));
    frame.click("Отмена", |ui| memory.show(ui));
    frame.click("Содержание", |ui| memory.show(ui));
    assert_eq!(memory.view.detail, Some(("release-check".into(), 1)));
    assert!(memory.view.confirm.is_none());
}

#[test]
fn cleanup_confirmation_fits_the_window_and_only_applies_when_confirmed() {
    for width in [320.0_f32, 430.0, 1600.0] {
        let mut memory = Memory::default();
        let mut frame = Frame { width, ..Default::default() };
        frame.click("Управление", |ui| memory.show(ui));
        frame.click("Включить автоочистку", |ui| memory.show(ui));
        frame.settle(|ui| memory.show(ui));
        let rect = frame.context.memory(|m| m.area_rect(m.top_modal_layer().unwrap().id)).unwrap();
        let expected = (width - 32.0).min(580.0);
        assert!(
            (rect.width() - expected).abs() < 5.0 && rect.left() >= 12.0,
            "Окно {width}: {rect:?}"
        );
        frame.click("Отмена", |ui| memory.show(ui));
        assert!(memory.view.confirm.is_none());
        assert!(!memory.data.store("agent-hub").unwrap().automatic);
        frame.click("Включить автоочистку", |ui| memory.show(ui));
        frame.click("Включить", |ui| memory.show(ui));
        frame.settle(|ui| memory.show(ui));
        assert!(memory.data.store("agent-hub").unwrap().automatic);
    }
}
