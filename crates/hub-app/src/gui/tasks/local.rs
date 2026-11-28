use super::Command;
use crate::gui::theme;
use crate::local_execution::{Answer, Interaction, Kind, Live};
use eframe::egui;
use hub_core::domain::{Question, Selection};
use std::sync::Arc;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;
use uuid::Uuid;

const MAX_ANSWER: usize = 16 * 1024;

#[derive(Default)]
pub(super) struct Panel {
    editors: BTreeMap<(Uuid, Uuid), Editor>,
}

#[derive(Default)]
struct Editor {
    project: PathBuf,
    task: u64,
    answers: Vec<QuestionDraft>,
    sent: bool,
    permission_details: bool,
    pending: Option<oneshot::Receiver<Result<(), String>>>,
    error: Option<String>,
    feedback_until: Option<Instant>,
}

#[derive(Default)]
struct QuestionDraft {
    choices: BTreeSet<usize>,
    text: String,
}

impl Panel {
    pub(super) fn sync(&mut self, ui: &egui::Ui, snapshot: &super::Snapshot) {
        self.editors.retain(|key, editor| {
            editor.poll(ui);
            if let Some(until) = editor.feedback_until {
                let remaining = until.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    editor.feedback_until = None;
                } else {
                    ui.ctx().request_repaint_after(remaining);
                }
            }
            editor.pending.is_some()
                || editor.feedback_until.is_some()
                || super::attention::has_request(snapshot, *key)
        });
    }

    /// Global feedback rows, skipped for requests whose card renders inline
    /// (`inline` keys) — the card already shows sent/error state.
    pub(super) fn feedback(
        &self,
        ui: &mut egui::Ui,
        project: Option<&Path>,
        inline: &[(Uuid, Uuid)],
    ) {
        for (key, editor) in &self.editors {
            if inline.contains(key)
                || editor.feedback_until.is_none()
                || project.is_some_and(|p| p != editor.project)
            {
                continue;
            }
            let message = format!(
                "Задача #{} · {}",
                editor.task,
                editor.error.as_deref().unwrap_or("Ответ получен.")
            );
            if editor.error.is_some() {
                ui.colored_label(ui.visuals().error_fg_color, message);
            } else {
                ui.weak(message);
            }
        }
    }

    pub(super) fn show_job(
        &mut self,
        ui: &mut egui::Ui,
        live: &Live,
        send: &mut impl FnMut(Command),
    ) {
        ui.push_id(live.token, |ui| {
            theme::card(ui).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(&live.status).on_hover_text(format!(
                        "В теме проекта за этой задачей можно следить командой /task follow {}",
                        live.id,
                    ));
                    if live.active
                        && ui.add(kylig_ui::Button::new("Остановить локальную работу")).clicked()
                    {
                        send(Command::LocalStop(live.token));
                    }
                });
                if let Some(request) = &live.interaction {
                    self.decision(ui, live, request, send);
                }
            });
        });
    }

    pub(super) fn decision(
        &mut self,
        ui: &mut egui::Ui,
        live: &Live,
        request: &Interaction,
        send: &mut impl FnMut(Command),
    ) {
        if !live.active {
            return;
        }
        let key = (live.token, request.id);
        let editor = self.editors.entry(key).or_insert_with(|| Editor {
            project: live.project.clone(),
            task: live.id,
            ..Default::default()
        });
        ui.push_id(key, |ui| {
            if let Some(error) = &editor.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            let response = ui
                .add_enabled_ui(!editor.sent, |ui| match &request.kind {
                    Kind::Permission(tool) => {
                        ui.strong(format!("Разрешить действие «{}»?", tool.tool));
                        let clamp = !editor.permission_details
                            && summary_rows(ui, &tool.summary) > SUMMARY_LINES;
                        let summary =
                            if clamp { clamp_lines(&tool.summary) } else { tool.summary.clone() };
                        egui::ScrollArea::vertical()
                            .id_salt("permission_details")
                            .max_height(160.0)
                            .show(ui, |ui| {
                                let mut job = egui::WidgetText::from(
                                    egui::RichText::new(summary).monospace(),
                                )
                                .into_layout_job(
                                    ui.style(),
                                    egui::FontSelection::Default,
                                    egui::Align::Min,
                                );
                                if clamp {
                                    Arc::make_mut(&mut job).wrap.max_rows = SUMMARY_LINES;
                                }
                                ui.add(egui::Label::new(job).wrap().selectable(true));
                            });
                        if ui
                            .link(if editor.permission_details {
                                "Скрыть подробности"
                            } else {
                                "Подробнее о запросе"
                            })
                            .clicked()
                        {
                            editor.permission_details = !editor.permission_details;
                        }
                        if editor.permission_details {
                            ui.label(format!(
                                "Разрешение на один вызов инструмента «{}» для задачи #{}.",
                                tool.tool, live.id
                            ));
                            ui.monospace(live.project.display().to_string());
                        }
                        ui.add_space(4.0);
                        ui.horizontal_wrapped(|ui| {
                            if theme::primary_button(ui, true, "Разрешить один раз").clicked()
                            {
                                Some(Answer::Allow)
                            } else if ui.add(kylig_ui::Button::new("Отклонить")).clicked()
                            {
                                Some(Answer::Deny)
                            } else {
                                None
                            }
                        })
                        .inner
                    }
                    Kind::Questions(questions) => editor.questions(ui, questions),
                })
                .inner;
            if let Some(answer) = response {
                let (reply, result) = oneshot::channel();
                send(Command::LocalAnswer { token: live.token, id: request.id, answer, reply });
                editor.pending = Some(result);
                editor.error = None;
                editor.feedback_until = None;
                editor.sent = true;
                ui.ctx().request_repaint();
            }
            if editor.sent {
                ui.horizontal(|ui| {
                    if editor.pending.is_some() {
                        ui.add(kylig_ui::ProgressRing::indeterminate().size(16.0));
                        ui.small("Отправляем ответ…");
                    } else {
                        ui.small("Ответ получен.");
                    }
                });
            }
        });
    }
}

impl Editor {
    fn poll(&mut self, ui: &egui::Ui) {
        let Some(pending) = &mut self.pending else { return };
        let error = match pending.try_recv() {
            Ok(result) => result.err(),
            Err(oneshot::error::TryRecvError::Closed) => {
                Some("Ответ не доставлен. Повторите отправку.".into())
            }
            Err(oneshot::error::TryRecvError::Empty) => {
                ui.ctx().request_repaint_after(Duration::from_millis(100));
                return;
            }
        };
        self.sent = error.is_none();
        self.feedback_until =
            Some(Instant::now() + Duration::from_secs(if self.sent { 5 } else { 15 }));
        self.error = error;
        self.pending = None;
    }

    fn questions(&mut self, ui: &mut egui::Ui, questions: &[Question]) -> Option<Answer> {
        self.answers.resize_with(questions.len(), QuestionDraft::default);
        egui::ScrollArea::vertical().id_salt("request_questions").max_height(220.0).show(
            ui,
            |ui| {
                for (index, (question, draft)) in
                    questions.iter().zip(&mut self.answers).enumerate()
                {
                    ui.push_id(index, |ui| draft.show(ui, question));
                }
            },
        );
        let answers: Vec<_> =
            self.answers.iter().zip(questions).map(|(draft, q)| draft.answer(q)).collect();
        let oversized = answers.iter().any(|answer| answer.len() > MAX_ANSWER);
        if oversized {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "Ответ превышает 16 КиБ. Сократите его перед отправкой.",
            );
        }
        let ready = !answers.is_empty()
            && !oversized
            && answers.iter().all(|answer| !answer.trim().is_empty());
        ui.horizontal_wrapped(|ui| {
            let label = if questions.len() == 1 {
                "Отправить ответ"
            } else {
                "Отправить ответы"
            };
            if theme::primary_button(ui, ready, label).clicked() {
                Some(Answer::Text(answers))
            } else if ui.add(kylig_ui::Button::new("Отказаться")).clicked() {
                Some(Answer::Deny)
            } else {
                None
            }
        })
        .inner
    }
}

impl QuestionDraft {
    fn show(&mut self, ui: &mut egui::Ui, question: &Question) {
        if !question.header().is_empty() {
            ui.weak(question.header());
        }
        ui.add(egui::Label::new(egui::RichText::new(question.text()).strong()).wrap());
        if !question.options().is_empty() {
            if question.selection() == Selection::Multiple {
                ui.small("Можно выбрать несколько вариантов.");
            }
            ui.horizontal_wrapped(|ui| {
                for (index, option) in question.options().iter().enumerate() {
                    let selected = self.choices.contains(&index);
                    if ui
                        .add(egui::Button::selectable(selected, &option.label).wrap())
                        .on_hover_text(&option.description)
                        .clicked()
                    {
                        if question.selection() == Selection::Single {
                            self.choices.clear();
                            self.choices.insert(index);
                            self.text.clone_from(&option.label);
                        } else if selected {
                            self.choices.remove(&index);
                        } else {
                            self.choices.insert(index);
                        }
                    }
                }
            });
        }
        if ui
            .add(
                kylig_ui::TextArea::new(&mut self.text)
                    .id_salt("answer")
                    .rows(1)
                    .max_rows(Some(4))
                    .width(ui.available_width())
                    .placeholder("Ответ или свой вариант"),
            )
            .changed()
            && question.selection() == Selection::Single
        {
            self.choices.clear();
        }
    }

    fn answer(&self, question: &Question) -> String {
        if question.selection() == Selection::Single {
            return self.text.trim().to_owned();
        }
        self.choices
            .iter()
            .filter_map(|&index| question.options().get(index).map(|option| option.label.as_str()))
            .chain((!self.text.trim().is_empty()).then_some(self.text.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Spec «Входящие»: collapsed permission summary shows up to 3 visual rows.
const SUMMARY_LINES: usize = 3;

/// Visual rows a monospace permission summary takes at the width `decision`
/// renders it — a single long command line wraps into several rows, so the
/// collapse decision must count rows, not logical lines.
fn summary_rows(ui: &egui::Ui, summary: &str) -> usize {
    let mut job = egui::WidgetText::from(egui::RichText::new(summary).monospace()).into_layout_job(
        ui.style(),
        egui::FontSelection::Default,
        egui::Align::Min,
    );
    Arc::make_mut(&mut job).wrap.max_width = ui.available_width();
    ui.fonts_mut(|fonts| fonts.layout_job(Arc::unwrap_or_clone(job))).rows.len()
}

fn clamp_lines(summary: &str) -> String {
    let mut lines = summary.lines();
    let head = lines.by_ref().take(SUMMARY_LINES).collect::<Vec<_>>().join("\n");
    if lines.next().is_some() { format!("{head}…") } else { head }
}
