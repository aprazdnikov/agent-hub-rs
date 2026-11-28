use super::{Effect, Page, Speaker, Stage, Task, Tasks};
use crate::widgets::{action, primary, remaining_height};
use eframe::egui::{self, Align, Frame, Layout, RichText};
use kylig_ui::{Button, ButtonVariant, IconButton};

const SEND: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Enter);

impl Tasks {
    pub(super) fn chat(&mut self, ui: &mut egui::Ui, id: usize) {
        let Some(task) = self.entry(id) else { return };
        let stage = task.stage;
        if stage != Stage::Draft
            && let Some(tab) = transcript(ui, task)
        {
            self.review_tab = tab;
            self.page = Page::Review;
            return;
        }
        if task.ops.archived || task.ops.queue.is_some() {
            if action(ui, "Управление задачей") {
                self.page = Page::Context;
            }
            return;
        }
        if stage.waiting() {
            self.reply(ui, id);
            return;
        }
        if stage == Stage::Failed {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "Соединение с исполнителем прервалось до начала сборки.",
            );
            if primary(ui, "Повторить запуск") {
                self.start(ui, "Повторяем запуск…", Effect::Run(id), false);
            }
            return;
        }
        if stage == Stage::Accepted {
            return;
        }
        let rules = self.rules.entry(self.project).or_default();
        let blocked = if rules.saved != rules.draft {
            Some("Сохраните правила проекта перед запуском.")
        } else if rules.saved.git
            && stage == Stage::Draft
            && self.entry(id).is_some_and(|t| t.ops.prepared.is_none())
        {
            Some("Подготовьте ветку Git в параметрах задачи.")
        } else {
            None
        };
        let Some(task) = self.entry_mut(id) else { return };
        let draft = stage == Stage::Draft;
        let field = if draft { &mut task.prompt } else { &mut task.composer };
        let footer_top = ui.cursor().top();
        ui.add_space(6.0);
        let submit = composer(ui, id, field, draft, blocked);
        let footer_height = ui.cursor().top() - footer_top;
        let height_id = ui.make_persistent_id(("chat-footer-height", id));
        if !draft && ui.data(|data| data.get_temp::<f32>(height_id)) != Some(footer_height) {
            ui.data_mut(|data| data.insert_temp(height_id, footer_height));
            ui.ctx().request_repaint();
        }
        match submit {
            Some(Submit::Send) => {
                if !draft {
                    let message = std::mem::take(&mut task.composer);
                    task.message(Speaker::User, message.trim());
                }
                self.start(ui, "Агент выполняет поручение…", Effect::Run(id), self.from_inbox);
            }
            Some(Submit::Save) => {
                self.start(ui, "Сохраняем поручение…", Effect::SaveDraft(id), false);
            }
            None => {}
        }
    }
}

fn transcript(ui: &mut egui::Ui, task: &Task) -> Option<usize> {
    let has_composer = !task.ops.archived
        && task.ops.queue.is_none()
        && !task.stage.waiting()
        && !matches!(task.stage, Stage::Accepted | Stage::Failed);
    let reserved = if has_composer {
        let id = ui.make_persistent_id(("chat-footer-height", task.id));
        ui.data(|data| data.get_temp::<f32>(id)).unwrap_or(148.0)
    } else if task.stage == Stage::Accepted {
        0.0
    } else {
        130.0
    };
    let height = (remaining_height(ui) - reserved).max(100.0);
    let mut review_tab = None;
    egui::ScrollArea::vertical()
        .id_salt(("chat.thread", task.id))
        .max_height(height)
        .stick_to_bottom(true)
        .auto_shrink([false, !has_composer])
        .show(ui, |ui| {
            ui.add_space(8.0);
            bubble(ui, Speaker::User, &task.prompt, |_| {});
            for (index, message) in task.chat.iter().enumerate() {
                ui.add_space(6.0);
                bubble(ui, message.speaker, &message.text, |ui| {
                    if matches!(message.speaker, Speaker::Agent(_))
                        && matches!(task.stage, Stage::Review | Stage::Accepted)
                        && index + 1 == task.chat.len()
                    {
                        ui.add_space(2.0);
                        ui.horizontal_wrapped(|ui| {
                            if ui.link("Открыть результат").clicked() {
                                review_tab = Some(0);
                            }
                            if task.id == 37 && ui.link("Проверки 2/2").clicked() {
                                review_tab = Some(2);
                            }
                        });
                    }
                });
            }
            ui.add_space(8.0);
        });
    review_tab
}

fn bubble(ui: &mut egui::Ui, speaker: Speaker, text: &str, footer: impl FnOnce(&mut egui::Ui)) {
    let row = ui.available_rect_before_wrap();
    let (name, icon, cap, accent_corner, fill) = match speaker {
        Speaker::User => (
            "Вы",
            kylig_icons::USER,
            0.76,
            egui::CornerRadius { se: 3, ..egui::CornerRadius::same(15) },
            ui.visuals().selection.bg_fill,
        ),
        Speaker::Agent(name) => (
            name,
            kylig_icons::ROBOT,
            0.84,
            egui::CornerRadius { sw: 3, ..egui::CornerRadius::same(15) },
            ui.visuals().panel_fill,
        ),
    };
    let width = (row.width() * cap).min(600.0);
    let body_font = egui::TextStyle::Body.resolve(ui.style());
    let text_width = ui.fonts_mut(|fonts| {
        fonts
            .layout(text.to_owned(), body_font, egui::Color32::WHITE, (width - 28.0).max(1.0))
            .size()
            .x
    });
    let width = width.min((text_width + 30.0).max(150.0));
    let left = if speaker == Speaker::User { row.right() - width } else { row.left() };
    let rect =
        egui::Rect::from_min_size(egui::pos2(left, row.top()), egui::vec2(width, f32::INFINITY));
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(rect).layout(Layout::top_down(Align::Min)),
        |ui| {
            Frame::new()
                .fill(fill)
                .stroke(ui.visuals().window_stroke)
                .corner_radius(accent_corner)
                .inner_margin(egui::Margin::symmetric(13, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 3.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 5.0;
                        ui.add(
                            kylig_icons::Icon::new(icon)
                                .size(15.0)
                                .color(ui.visuals().hyperlink_color),
                        );
                        ui.label(RichText::new(name).strong().small());
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            copy_button(ui, text, "Скопировать сообщение");
                        });
                    });
                    ui.add(egui::Label::new(text).selectable(true).wrap());
                    footer(ui);
                });
        },
    );
}

pub(super) fn copy_button(ui: &mut egui::Ui, text: &str, label: &str) {
    let id = ui.next_auto_id().with("clipboard");
    let now = ui.input(|input| input.time);
    let copied = ui.data(|data| data.get_temp::<f64>(id)).is_some_and(|until| now < until);
    let (icon, tooltip) = if copied {
        (kylig_icons::CHECK, "Скопировано")
    } else {
        (kylig_icons::COPY, label)
    };
    if ui
        .add(
            IconButton::new(icon, tooltip).size(24.0).icon_size(14.0).variant(ButtonVariant::Ghost),
        )
        .clicked()
    {
        ui.ctx().copy_text(text.to_owned());
        ui.data_mut(|data| data.insert_temp(id, now + 1.5));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(1500));
    }
}

enum Submit {
    Send,
    Save,
}

fn composer(
    ui: &mut egui::Ui,
    id: usize,
    field: &mut String,
    draft: bool,
    blocked: Option<&str>,
) -> Option<Submit> {
    let editor = ui.make_persistent_id(("task.composer", id));
    let focused = ui.memory(|memory| memory.has_focus(editor));
    let shortcut =
        ui.is_enabled() && focused && ui.input_mut(|input| input.consume_shortcut(&SEND));
    let stroke = if focused { ui.visuals().selection.stroke } else { ui.visuals().window_stroke };
    Frame::new()
        .fill(ui.visuals().panel_fill)
        .corner_radius(12)
        .stroke(stroke)
        .inner_margin(egui::Margin::symmetric(12, 9))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let max_height = ui.text_style_height(&egui::TextStyle::Body) * 8.0 + 8.0;
            egui::ScrollArea::vertical()
                .id_salt(("composer.input", id))
                .max_height(max_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(field)
                            .id(editor)
                            .desired_width(f32::INFINITY)
                            .desired_rows(1)
                            .frame(Frame::NONE)
                            .hint_text(if draft {
                                "Что поручить агенту?"
                            } else {
                                "Сообщение агенту…"
                            }),
                    );
                });
            ui.separator();
            let valid = !field.trim().is_empty() && field.len() <= 16384;
            let send = valid && blocked.is_none();
            let mut submit = None;
            ui.horizontal_wrapped(|ui| {
                let button =
                    Button::new(if draft { "Запустить" } else { "Отправить" })
                        .icon(kylig_icons::PAPER_PLANE_TILT)
                        .primary();
                if (ui.add_enabled(send, button).clicked() || shortcut) && send {
                    submit = Some(Submit::Send);
                }
                if draft && ui.add_enabled(valid, Button::new("Сохранить").ghost()).clicked()
                {
                    submit = Some(Submit::Save);
                }
                ui.label(RichText::new(ui.ctx().format_shortcut(&SEND)).weak().small());
            });
            if let Some(hint) = blocked {
                ui.weak(hint);
            } else if field.len() > 16384 {
                ui.colored_label(ui.visuals().error_fg_color, "Сократите сообщение до 16 КиБ.");
            }
            submit
        })
        .inner
}
