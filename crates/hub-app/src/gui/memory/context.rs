use super::{Request, egui};
use hub_memory::{ContextBrief, ContextPacket, ContextRequest, ContextSelection, Entry};
use hub_telegram::memory::lifecycle::Action;
use std::collections::BTreeMap;

pub(super) struct ContextDraft {
    selected: BTreeMap<String, u32>,
    brief: ContextBrief,
    sources: String,
    budget: usize,
}
impl Default for ContextDraft {
    fn default() -> Self {
        Self {
            selected: BTreeMap::new(),
            brief: ContextBrief::default(),
            sources: String::new(),
            budget: 16384,
        }
    }
}
impl ContextDraft {
    pub(super) fn select(&mut self, ui: &mut egui::Ui, entry: &Entry) {
        let already = self.selected.get(&entry.key) == Some(&entry.revision);
        if ui
            .add_enabled(
                !already && (self.selected.len() < 20 || self.selected.contains_key(&entry.key)),
                egui::Button::new(if already {
                    "Эта ревизия уже в пакете"
                } else {
                    "Добавить ревизию в пакет контекста"
                }),
            )
            .clicked()
        {
            self.selected.insert(entry.key.clone(), entry.revision);
        }
        ui.small("Поля поручения и сборка — в блоке «Пакет контекста следующему агенту» выше.");
    }
    pub(super) fn controls(&mut self, ui: &mut egui::Ui, enabled: bool) -> Option<Request> {
        procedure(ui);
        let mut action = None;
        ui.collapsing(format!("Пакет контекста следующему агенту ({}/20)",self.selected.len()),|ui| {
            ui.add_enabled_ui(enabled,|ui| {
                ui.label("Откройте нужные записи и добавьте их ревизии. Пакет содержит полные тексты; источники не проверяются, разрешения не переносятся.");
                let mut remove=None;
                for (key,revision) in &self.selected {
                    ui.horizontal(|ui| { ui.label(format!("{key}@{revision}")); if ui.small_button("Убрать").clicked(){remove=Some(key.clone());} });
                }
                if let Some(key)=remove { self.selected.remove(&key); }
                for (label,value) in [("Цель",&mut self.brief.goal),("Ограничения",&mut self.brief.constraints),("Состояние",&mut self.brief.state),("Следующий шаг",&mut self.brief.next_step)] {
                    ui.label(label);
                    ui.add(egui::TextEdit::multiline(value).desired_rows(2).char_limit(4096));
                }
                ui.label("Источники поручения: по одному на строке, например ссылка на сообщение пользователя или документ (1–8). Это указания, а не подтверждение согласия.");
                ui.add(egui::TextEdit::multiline(&mut self.sources).desired_rows(2).char_limit(16384));
                ui.horizontal(|ui| { ui.label("Бюджет, байт UTF-8:"); ui.add(egui::DragValue::new(&mut self.budget).range(1024..=65536)); });
                if ui.add_enabled(!self.selected.is_empty(),egui::Button::new("Собрать пакет для проверки")).clicked() {
                    action=Some(Request::Admin(Box::new(Action::Context(self.input()))));
                }
            });
        });
        action
    }
    fn input(&self) -> ContextRequest {
        let mut brief = self.brief.clone();
        brief.sources =
            self.sources.lines().filter(|s| !s.trim().is_empty()).map(str::to_owned).collect();
        ContextRequest {
            brief,
            selections: self
                .selected
                .iter()
                .map(|(key, revision)| ContextSelection { key: key.clone(), revision: *revision })
                .collect(),
            max_bytes: self.budget,
        }
    }
    pub(super) fn matches(&self, packet: &ContextPacket) -> bool {
        packet.matches_request(&self.input())
    }
    pub(super) fn preview(
        &self,
        ui: &mut egui::Ui,
        packet: &ContextPacket,
        ready: bool,
    ) -> Option<Request> {
        if !self.matches(packet) {
            ui.label("Поручение, состав или бюджет изменились. Соберите пакет заново перед копированием.");
            return None;
        }
        ui.label(hub_telegram::memory::context::summary(packet));
        let mut action = None;
        if ready {
            ui.label("Пакет скопирован. Вставьте его обычным сообщением выбранному агенту. Свежесть проверена на момент копирования.");
        }
        if ui.button("Проверить свежесть и скопировать пакет").clicked()
        {
            action = Some(Request::Admin(Box::new(Action::ValidateContext(packet.clone()))));
        }
        ui.collapsing("Полный текст пакета", |ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(packet.text()).monospace()).selectable(true),
            );
        });
        action
    }
}

fn procedure(ui: &mut egui::Ui) {
    ui.collapsing("Процедура: продолжить работу по памяти", |ui| {
        ui.label("Шаблон continue-work@1. Передайте выбранному агенту вместе с конкретной задачей. Копирование не запускает работу и не меняет память.");
        ui.add(egui::Label::new(hub_memory::CONTINUE_WORK_V1).selectable(true));
        if ui.button("Скопировать процедуру").clicked() {
            ui.ctx().copy_text(hub_memory::CONTINUE_WORK_V1.to_owned());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editing_constraints_blocks_copy_until_packet_is_rebuilt() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join("memory");
        hub_memory::Store::open(&base, root.path(), "test")
            .unwrap()
            .write(hub_memory::WriteRequest {
                key: "pilot".into(),
                expected_revision: 0,
                kind: hub_memory::EntryKind::Progress,
                title: "Pilot".into(),
                body: "body".into(),
                sources: vec!["test".into()],
            })
            .unwrap();
        let reader = hub_memory::MemoryReader::new(&base, root.path()).unwrap();
        let mut draft = ContextDraft {
            selected: BTreeMap::from([("pilot".into(), 1)]),
            brief: ContextBrief {
                goal: "goal".into(),
                constraints: "original".into(),
                state: "state".into(),
                next_step: "next".into(),
                sources: vec![],
            },
            sources: "user: task".into(),
            budget: 16384,
        };
        let packet = reader.context(&draft.input()).unwrap();
        let ctx = egui::Context::default();
        let frame = |draft: &ContextDraft, packet: &ContextPacket| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 1800.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    draft.preview(ui, packet, false);
                },
            );
            output.textures_delta.clear();
            output.shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text()=="Проверить свежесть и скопировать пакет"))
        };
        frame(&draft, &packet);
        assert!(frame(&draft, &packet));
        draft.brief.constraints = "Не удалять данные".into();
        assert!(!frame(&draft, &packet), "edited constraints must block old packet copying");
        let rebuilt = reader.context(&draft.input()).unwrap();
        assert!(frame(&draft, &rebuilt));
        assert!(rebuilt.text().contains("Не удалять данные"));
        draft.selected.clear();
        assert!(!frame(&draft, &rebuilt), "removing a record must invalidate the preview");
    }
    #[test]
    fn procedure_copy_requires_expansion_and_copies_exact_text() {
        let ctx = egui::Context::default();
        let frame = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 3000.0),
                    )),
                    events,
                    ..Default::default()
                },
                procedure,
            );
            output.textures_delta.clear();
            output
        };
        let position = |output: &egui::FullOutput, label: &str| {
            output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                }
                _ => None,
            })
        };
        let click = |pos| {
            for pressed in [true, false] {
                let output = frame(vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::default(),
                    },
                ]);
                if !pressed {
                    return output;
                }
            }
            unreachable!()
        };
        frame(vec![]);
        let closed = frame(vec![]);
        assert!(position(&closed, "Скопировать процедуру").is_none());
        assert!(closed.platform_output.commands.is_empty());
        click(position(&closed, "Процедура: продолжить работу по памяти").unwrap());
        // Disable animation to make the expanded content immediately available in tests.
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        frame(vec![]);
        let opened = frame(vec![]);
        let copied = click(position(&opened, "Скопировать процедуру").unwrap());
        assert!(copied.platform_output.commands.iter().any(|command| matches!(command,
            egui::OutputCommand::CopyText(text) if text == hub_memory::CONTINUE_WORK_V1)));
    }
}
