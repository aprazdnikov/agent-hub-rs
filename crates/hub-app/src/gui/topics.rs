//! The «Темы» tab.

use eframe::egui;
use hub_core::domain::TopicKey;
use hub_telegram::hub::TopicState;

use crate::gui::look::short_session;
use crate::supervisor::{Command, Snapshot};

#[derive(Default)]
pub struct TopicsView {
    resetting: Option<TopicKey>,
}

impl TopicsView {
    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        if snapshot.topics.is_empty() {
            ui.label("Тем пока нет: они появляются, когда бот запущен и в группе есть сообщения.");
            return;
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("topics").num_columns(6).striped(true).show(ui, |ui| {
                for header in ["Тема", "Бэкенд", "Каталог", "Сессия", "Статус", ""]
                {
                    ui.strong(header);
                }
                ui.end_row();
                for topic in &snapshot.topics {
                    let name =
                        topic.title.clone().unwrap_or_else(|| format!("#{}", topic.key.thread.0));
                    ui.label(name);
                    ui.label(topic.session.backend.name());
                    ui.label(topic.session.cwd.as_path().display().to_string());
                    match &topic.session.session {
                        Some(id) => {
                            let response = ui
                                .add(
                                    egui::Label::new(short_session(id.as_str()))
                                        .sense(egui::Sense::click()),
                                )
                                .on_hover_text("Скопировать полный session_id");
                            if response.clicked() {
                                ui.ctx().copy_text(id.as_str().to_owned());
                            }
                        }
                        None => {
                            ui.label("—");
                        }
                    }
                    let running = topic.state == TopicState::Running;
                    ui.label(if running { "работает" } else { "свободна" });
                    ui.horizontal(|ui| {
                        if ui.add_enabled(running, egui::Button::new("Stop")).clicked() {
                            send(Command::StopTopic(topic.key));
                        }
                        if ui.add_enabled(!running, egui::Button::new("Reset")).clicked() {
                            self.resetting = Some(topic.key);
                        }
                    });
                    ui.end_row();
                }
            });
        });
        self.confirm(ui.ctx(), send);
    }

    fn confirm(&mut self, ctx: &egui::Context, send: &mut impl FnMut(Command)) {
        let Some(key) = self.resetting else { return };
        let modal = egui::Modal::new(egui::Id::new("reset")).show(ctx, |ui| {
            ui.label("Сбросить контекст темы? Агент начнёт следующую задачу с чистого листа.");
            ui.horizontal(|ui| {
                if ui.button("Сбросить").clicked() {
                    send(Command::ResetTopic(key));
                    self.resetting = None;
                }
                if ui.button("Отмена").clicked() {
                    self.resetting = None;
                }
            });
        });
        if modal.should_close() {
            self.resetting = None;
        }
    }
}
