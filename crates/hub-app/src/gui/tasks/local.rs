use super::{Command, Snapshot};
use crate::local_execution::{Answer, Kind};
use eframe::egui;
#[derive(Default)]
pub(super) struct Panel {
    request: Option<uuid::Uuid>,
    answers: Vec<String>,
    sent: bool,
    pending: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
    error: Option<String>,
}
impl Panel {
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        if let Some(mut pending) = self.pending.take() {
            match pending.try_recv() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    self.error = Some(error);
                    self.sent = false;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.error = Some("Ответ не доставлен. Повторите отправку.".into());
                    self.sent = false;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    self.pending = Some(pending);
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        let Some(live) = &snapshot.local else { return };
        ui.group(|ui| {
            ui.strong(format!("Общая задача #{} · {}", live.id, live.project.display()));
            ui.label(&live.status);
            ui.add(
                egui::Label::new(format!(
                    "В Telegram: /task follow {} в теме этого проекта",
                    live.id
                ))
                .selectable(true),
            );
            if live.active && ui.button("Остановить локальную работу").clicked()
            {
                send(Command::LocalStop(live.token));
            }
            let current = live.interaction.as_ref().map(|request| request.id);
            if current != self.request {
                self.request = current;
                self.answers.clear();
                self.sent = false;
                self.pending = None;
                self.error = None;
            }
            let Some(request) = &live.interaction else { return };
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            let mut response = None;
            ui.add_enabled_ui(!self.sent && live.active, |ui| match &request.kind {
                Kind::Permission(tool) => {
                    ui.strong(format!("Разрешить действие: {}", tool.tool));
                    egui::ScrollArea::vertical()
                        .id_salt(("local_permission", request.id))
                        .max_height(180.0)
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(&tool.summary).selectable(true));
                        });
                    ui.horizontal(|ui| {
                        if ui.button("Разрешить один раз").clicked() {
                            response = Some(Answer::Allow);
                        }
                        if ui.button("Отклонить").clicked() {
                            response = Some(Answer::Deny);
                        }
                    });
                }
                Kind::Questions(questions) => {
                    response = self.questions(ui, request.id, questions);
                }
            });
            if let Some(answer) = response {
                let (reply, result) = tokio::sync::oneshot::channel();
                send(Command::LocalAnswer { token: live.token, id: request.id, answer, reply });
                self.pending = Some(result);
                self.error = None;
                self.sent = true;
            }
            if self.sent {
                ui.small("Ответ отправлен. Ожидаем продолжения работы.");
            }
        });
    }
    fn questions(
        &mut self,
        ui: &mut egui::Ui,
        id: uuid::Uuid,
        questions: &[hub_core::domain::Question],
    ) -> Option<Answer> {
        let mut response = None;
        self.answers.resize(questions.len(), String::new());
        egui::ScrollArea::vertical().id_salt(("local_questions", id)).max_height(300.0).show(
            ui,
            |ui| {
                for (index, (question, answer)) in
                    questions.iter().zip(&mut self.answers).enumerate()
                {
                    ui.label(question.text());
                    for option in question.options() {
                        if ui.button(&option.label).on_hover_text(&option.description).clicked() {
                            if question.selection() == hub_core::domain::Selection::Multiple
                                && !answer.is_empty()
                            {
                                answer.push_str(", ");
                                answer.push_str(&option.label);
                            } else {
                                answer.clone_from(&option.label);
                            }
                        }
                    }
                    ui.add(
                        egui::TextEdit::multiline(answer)
                            .id_salt(("local_answer", id, index))
                            .desired_rows(2)
                            .char_limit(16384)
                            .hint_text("Ответ или свой вариант"),
                    );
                }
            },
        );
        if self.answers.iter().any(|answer| answer.len() > 16384) {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "Ответ превышает 16 КиБ. Сократите его перед отправкой.",
            );
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.answers
                        .iter()
                        .all(|answer| !answer.trim().is_empty() && answer.len() <= 16384),
                    egui::Button::new("Отправить ответы"),
                )
                .clicked()
            {
                response = Some(Answer::Text(self.answers.clone()));
            }
            if ui.button("Отказаться отвечать").clicked() {
                response = Some(Answer::Deny);
            }
        });
        response
    }
}
