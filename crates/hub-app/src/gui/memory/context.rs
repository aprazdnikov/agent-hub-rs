use super::{Request, Tab, egui, theme};
use hub_memory::{ContextBrief, ContextPacket, ContextRequest, ContextSelection, Entry};
use hub_telegram::memory::lifecycle::Action;
use kylig_ui::{Button, TextArea, TextField};
use std::collections::BTreeMap;

pub(super) struct ContextDraft {
    selected: BTreeMap<String, u32>,
    procedure_open: bool,
    brief: ContextBrief,
    sources: String,
    budget: usize,
}
impl Default for ContextDraft {
    fn default() -> Self {
        Self {
            selected: BTreeMap::new(),
            procedure_open: false,
            brief: ContextBrief::default(),
            sources: String::new(),
            budget: 16384,
        }
    }
}
impl ContextDraft {
    pub(super) fn apply_change(&mut self, action: &hub_memory::AdminAction) {
        match action {
            hub_memory::AdminAction::Delete { keys } => {
                self.selected.retain(|key, _| !keys.contains(key));
            }
            hub_memory::AdminAction::Prune { revisions } => self
                .selected
                .retain(|key, r| !revisions.get(key).is_some_and(|removed| removed.contains(r))),
            hub_memory::AdminAction::SetStatus { .. } | hub_memory::AdminAction::SetPolicy(_) => {}
        }
    }
    pub(super) fn select(&mut self, ui: &mut egui::Ui, entry: &Entry, tab: &mut Tab) -> bool {
        let already = self.selected.get(&entry.key) == Some(&entry.revision);
        if already {
            ui.horizontal_wrapped(|ui| {
                ui.weak("Эта ревизия в пакете");
                if ui.add(Button::new("Открыть пакет")).clicked() {
                    *tab = Tab::Context;
                }
            });
        } else if ui
            .add_enabled(
                self.selected.len() < 20 || self.selected.contains_key(&entry.key),
                Button::new("Добавить в пакет контекста").primary(),
            )
            .clicked()
        {
            self.selected.insert(entry.key.clone(), entry.revision);
            return true;
        }
        false
    }
    pub(super) fn controls(&mut self, ui: &mut egui::Ui, tab: &mut Tab) -> Option<Request> {
        theme::card(ui).show(ui, |ui| {
            ui.strong(format!("Ревизии для следующего агента · {}/20", self.selected.len()));
            if self.selected.is_empty() {
                ui.label("Откройте запись и добавьте нужную ревизию.");
                if ui.add(Button::new("Выбрать записи")).clicked() {
                    *tab = Tab::Records;
                }
            }
            self.selected.retain(|key, revision| {
                ui.horizontal_wrapped(|ui| {
                    ui.monospace(format!("{key} · r{revision}"));
                    !ui.small_button("Убрать").clicked()
                })
                .inner
            });
        });
        ui.strong("Поручение");
        let width = ui.available_width().min(620.0);
        for (label, value) in [
            ("Цель", &mut self.brief.goal),
            ("Ограничения", &mut self.brief.constraints),
            ("Состояние", &mut self.brief.state),
            ("Следующий шаг", &mut self.brief.next_step),
        ] {
            ui.add(TextField::new(value).label(label).id_salt(label).max_chars(4096).width(width));
        }
        ui.add(
            TextArea::new(&mut self.sources)
                .label("Источники поручения")
                .placeholder("По одному источнику на строке (1–8)")
                .rows(2)
                .max_chars(16384)
                .width(width),
        );
        ui.weak("Пакет содержит полный текст ревизий. Источники не проверяются, разрешения не переносятся.");
        let build = ui
            .horizontal_wrapped(|ui| {
                ui.label("Бюджет, байт");
                ui.add(egui::DragValue::new(&mut self.budget).range(1024..=65536));
                ui.add_enabled(!self.selected.is_empty(), Button::new("Собрать пакет").primary())
                    .clicked()
            })
            .inner;
        build.then(|| Request::Admin(Box::new(Action::Context(self.input()))))
    }
    pub(super) fn procedure(&mut self, ui: &mut egui::Ui) -> bool {
        procedure(ui, &mut self.procedure_open)
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
        theme::card(ui).show(ui, |ui| {
            ui.strong("Предпросмотр пакета");
            ui.weak(format!(
                "{} байт · ревизий — {}",
                packet.text().len(),
                packet.selections().len()
            ));
            for warning in packet.warnings() {
                ui.label(warning.as_str());
            }
            ui.add(egui::Label::new(packet.text()).selectable(true));
        });
        if ready {
            ui.label("Пакет скопирован. Вставьте его обычным сообщением выбранному агенту. Свежесть проверена на момент копирования.");
        }
        ui.add(Button::new("Проверить и скопировать").primary())
            .clicked()
            .then(|| Request::Admin(Box::new(Action::ValidateContext(packet.clone()))))
    }
}

fn procedure(ui: &mut egui::Ui, open: &mut bool) -> bool {
    if ui.add(Button::new("Открыть процедуру продолжения работы")).clicked()
    {
        *open = true;
    }
    let mut close = false;
    let mut copied = false;
    let height = (ui.ctx().content_rect().height() - 200.0).max(60.0);
    theme::dialog(ui, "memory-procedure", open, "Продолжение работы").show(ui.ctx(), |ui| {
        egui::ScrollArea::vertical().max_height(height).show(ui, |ui| {
            ui.add(egui::Label::new(hub_memory::CONTINUE_WORK_V1).selectable(true));
        });
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::new("Скопировать процедуру").primary()).clicked()
            {
                ui.ctx().copy_text(hub_memory::CONTINUE_WORK_V1.to_owned());
                copied = true;
                close = true;
            }
            if ui.add(Button::new("Закрыть")).clicked() {
                close = true;
            }
        });
    });
    if close {
        *open = false;
    }
    copied
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
            procedure_open: false,
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
            output.shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text()=="Проверить и скопировать"))
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
        let open = std::cell::Cell::new(false);
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
                |ui| {
                    let mut value = open.get();
                    procedure(ui, &mut value);
                    open.set(value);
                },
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
        click(position(&closed, "Открыть процедуру продолжения работы").unwrap());
        // Disable animation to make the expanded content immediately available in tests.
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        frame(vec![]);
        let opened = frame(vec![]);
        let copied = click(position(&opened, "Скопировать процедуру").unwrap());
        assert!(copied.platform_output.commands.iter().any(|command| matches!(command,
            egui::OutputCommand::CopyText(text) if text == hub_memory::CONTINUE_WORK_V1)));
    }
}
