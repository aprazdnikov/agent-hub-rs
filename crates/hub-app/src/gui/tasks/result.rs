//! Task review tabs and annotations against the saved patch of a particular attempt.
use crate::gui::theme::{self, Tone, bubble, chip, tone_colors};
use eframe::egui;
use hub_core::tasks::{Run, RunState, Task};
use jiff::{Timestamp, Zoned, tz::TimeZone};
use std::collections::BTreeSet;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    #[default]
    Chat,
    Parameters,
    Summary,
    Changes,
    Checks,
    Files,
    History,
}
#[derive(Default)]
pub(super) struct View {
    task: Option<u64>,
    tab: Tab,
    anchor: Option<String>,
    note: String,
    run: Option<u32>,
    history_attempt: Option<u32>,
    file_error: Option<String>,
    preview: Option<FilePreview>,
    auto_previewed: bool,
    expanded: BTreeSet<u32>,
    /// The «…» menu landed on Parameters and wants the next-run context open; consumed there.
    context_open: bool,
}
struct FilePreview {
    key: String,
    name: String,
    reply: std::sync::mpsc::Receiver<Result<String, String>>,
    content: Option<Result<String, String>>,
}

impl FilePreview {
    fn open(
        file: &hub_core::tasks::ResultFile,
        path: std::path::PathBuf,
        ctx: egui::Context,
    ) -> std::io::Result<Self> {
        let (send, reply) = std::sync::mpsc::channel();
        std::thread::Builder::new().name("task-file-preview".into()).spawn(move || {
            let result = read_preview(&path).map_err(|err| err.to_string());
            let _ = send.send(result);
            ctx.request_repaint();
        })?;
        Ok(Self { key: file.key.clone(), name: file.name.clone(), reply, content: None })
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        if self.content.is_none() {
            self.content = match self.reply.try_recv() {
                Ok(result) => Some(result),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Не удалось прочитать файл.".into()))
                }
            };
        }
        match &self.content {
            None => {
                ui.add(kylig_ui::ProgressRing::indeterminate().size(18.0));
            }
            Some(Err(error)) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            Some(Ok(text)) => {
                ui.horizontal(|ui| {
                    ui.weak("Содержимое файла");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        copy_button(ui, text, "Скопировать файл");
                    });
                });
                egui::ScrollArea::vertical()
                    .id_salt(("file-preview", &self.key, &self.name))
                    .max_height(240.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(egui::RichText::new(text).monospace())
                                .wrap()
                                .selectable(true),
                        );
                    });
            }
        }
    }
}

const PREVIEW_BYTE_LIMIT: u64 = 256 * 1024;

fn read_preview(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let folder =
        path.parent().ok_or_else(|| std::io::Error::other("Каталог вложения недоступен."))?;
    if !std::fs::symlink_metadata(folder)?.is_dir() || !std::fs::symlink_metadata(path)?.is_file() {
        return Err(std::io::Error::other("Сохранённый файл недоступен."));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(PREVIEW_BYTE_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > PREVIEW_BYTE_LIMIT {
        return Err(std::io::Error::other(
            "Файл больше 256 КиБ. Откройте его во внешнем приложении.",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|_| std::io::Error::other("Файл содержит данные в другой кодировке."))
}

impl View {
    pub(super) fn select(&mut self, tab: Tab, task: Option<u64>) {
        *self = Self { task, tab, ..Default::default() };
    }
    /// Called right after `select`, which resets the flag; records the one-shot request.
    pub(super) fn request_context_open(&mut self) {
        self.context_open = true;
    }
    /// Consumes the pending request so only the frame it was made in forces the section open.
    pub(super) fn take_context_open(&mut self) -> bool {
        std::mem::take(&mut self.context_open)
    }
    pub(super) fn tab(&mut self, task: &Task) -> Tab {
        if self.task != Some(task.id) {
            self.select(Tab::Chat, Some(task.id));
        }
        self.tab
    }
    pub(super) fn review_run(task: &Task) -> Option<&Run> {
        task.runs.iter().rev().find(|run| {
            run.state != RunState::Running
                || !run.result.is_empty()
                || !run.files.is_empty()
                || !run.diff.is_empty()
                || !run.workflow_report.is_empty()
        })
    }
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        next: &str,
        editable: bool,
    ) -> Option<String> {
        let run = Self::review_run(task)?;
        if self.run != Some(run.id) {
            self.file_error = None;
            self.preview = None;
            self.auto_previewed = false;
            self.anchor = None;
            self.note.clear();
            self.run = Some(run.id);
        }
        self.tab_bar(ui, task);
        ui.add_space(8.0);
        theme::card(ui)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                match self.tab {
                    Tab::Summary => {
                        summary(ui, run);
                        None
                    }
                    Tab::Changes => self.changes(ui, task, run, next, editable),
                    Tab::Checks => {
                        report(ui, run);
                        None
                    }
                    Tab::Files => {
                        if run.files.is_empty() {
                            ui.weak("Сохранённых файлов нет.");
                        } else {
                            self.files(ui, run);
                        }
                        None
                    }
                    Tab::History => {
                        self.history(ui, task);
                        None
                    }
                    // Chat and Parameters render their own screens; review bodies exist only for these five.
                    Tab::Chat | Tab::Parameters => None,
                }
            })
            .inner
    }
    fn tab_bar(&mut self, ui: &mut egui::Ui, task: &Task) {
        const SECTIONS: [(Tab, &str); 5] = [
            (Tab::Summary, "Сводка"),
            (Tab::Changes, "Изменения"),
            (Tab::Checks, "Проверки"),
            (Tab::Files, "Файлы"),
            (Tab::History, "История"),
        ];
        if ui.available_width() > 500.0 {
            let mut index = SECTIONS.iter().position(|(tab, _)| *tab == self.tab).unwrap_or(0);
            kylig_ui::Tabs::new(&mut index, SECTIONS.map(|(_, label)| label))
                .id_salt(("review", task.id))
                .variant(kylig_ui::TabsVariant::Underline)
                .show(ui);
            if let Some((tab, _)) = SECTIONS.get(index) {
                self.tab = *tab;
            }
        } else {
            let choices = SECTIONS.map(|(tab, label)| kylig_ui::SelectItem::new(tab, label));
            ui.add(
                kylig_ui::Select::new(&mut self.tab, &choices)
                    .id_salt(("review.compact", task.id))
                    .options(kylig_ui::TextFieldOptions {
                        animate: ui.style().animation_time > 0.0,
                        ..Default::default()
                    })
                    .label("Раздел результата")
                    .width(ui.available_width()),
            );
        }
    }

    fn history(&mut self, ui: &mut egui::Ui, task: &Task) {
        let Some(latest) = task.runs.last() else { return };
        let caption = |attempt: &Run| {
            format!(
                "Попытка {} · {}",
                attempt.id,
                display_agent(attempt.backend.as_deref().unwrap_or(&task.backend))
            )
        };
        let mut id = self.history_attempt.unwrap_or(latest.id);
        if task.runs.len() > 1 {
            let choices: Vec<_> = task
                .runs
                .iter()
                .rev()
                .map(|run| theme::Choice::new(run.id, caption(run), run.state.label()))
                .collect();
            if let Some(value) = theme::detailed_select(ui, "Попытка", "", &id, &choices, 300.0)
            {
                id = *value;
            }
            self.history_attempt = (id != latest.id).then_some(id);
        } else {
            ui.strong(caption(latest));
        }
        let attempt = task.runs.iter().find(|run| run.id == id).unwrap_or(latest);
        ui.push_id(attempt.id, |ui| {
            ui.small(attempt.state.label());
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.weak("Поручение");
                copy_button(ui, &attempt.step, "Скопировать поручение");
            });
            ui.add(egui::Label::new(&attempt.step).selectable(true).wrap());
            if let Some(described) = attempt.described_rules() {
                ui.label(described);
            }
            if !attempt.diagnostic.is_empty() {
                ui.colored_label(ui.visuals().error_fg_color, &attempt.diagnostic);
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.weak("Итог попытки");
                if !attempt.result.is_empty() {
                    copy_button(ui, &attempt.result, "Скопировать результат");
                }
            });
            ui.add(
                egui::Label::new(if attempt.result.is_empty() {
                    "Сохранённого ответа нет"
                } else {
                    &attempt.result
                })
                .wrap()
                .selectable(true),
            );
            self.files(ui, attempt);
            if !attempt.workflow_report.is_empty() {
                report(ui, attempt);
            }
            if !attempt.diff.is_empty() {
                ui.collapsing("Сохранённые изменения", |ui| {
                    ui.monospace(&attempt.diff);
                });
            }
        });
    }
    pub(super) fn transcript(ui: &mut egui::Ui, task: &Task, view: &mut Self) {
        ui.add_space(8.0);
        for run in &task.runs {
            ui.push_id((run.id, "user"), |ui| user_bubble(ui, &run.step, run.started_at));
            ui.add_space(6.0);
            ui.push_id((run.id, "agent"), |ui| agent_bubble(ui, task, run, view));
            ui.add_space(6.0);
        }
    }
    fn files(&mut self, ui: &mut egui::Ui, run: &Run) {
        if run.files.is_empty() {
            return;
        }
        for file in &run.files {
            ui.horizontal_wrapped(|ui| {
                ui.add(kylig_icons::Icon::new(kylig_icons::FILE_TEXT).size(18.0));
                ui.strong(&file.name);
                ui.weak(format!("{} байт", file.bytes));
                let openable = std::path::Path::new(&file.name)
                    .extension()
                    .and_then(|v| v.to_str())
                    .is_some_and(|ext| {
                        ["txt", "pdf", "png", "jpg", "jpeg", "webp"]
                            .iter()
                            .any(|v| ext.eq_ignore_ascii_case(v))
                    });
                let text_file = std::path::Path::new(&file.name)
                    .extension()
                    .and_then(|v| v.to_str())
                    .is_some_and(|ext| {
                        ["txt", "md", "log", "csv", "json", "diff", "patch"]
                            .iter()
                            .any(|v| ext.eq_ignore_ascii_case(v))
                    });
                let expanded =
                    self.preview.as_ref().is_some_and(|p| p.key == file.key && p.name == file.name);
                let mut preview = text_file
                    && ui
                        .add(kylig_ui::Button::new(if expanded {
                            "Свернуть файл"
                        } else {
                            "Прочитать файл"
                        }))
                        .clicked();
                if preview && expanded {
                    self.preview = None;
                    preview = false;
                }
                if !self.auto_previewed
                    && self.tab == Tab::Files
                    && run.files.len() == 1
                    && text_file
                    && file.bytes <= PREVIEW_BYTE_LIMIT
                    && !expanded
                {
                    self.auto_previewed = true;
                    preview = true;
                }
                let open = openable && ui.add(kylig_ui::Button::new("Открыть файл")).clicked();
                let reveal = ui.add(kylig_ui::Button::new("В папке")).clicked();
                if open || reveal || preview {
                    self.file_error = crate::dirs::AppDirs::locate()
                        .ok_or_else(|| "Каталог данных недоступен".to_owned())
                        .and_then(|dirs| {
                            let folder = dirs.task_files().join(&file.key);
                            let path = folder.join(&file.name);
                            if preview {
                                self.preview = Some(
                                    FilePreview::open(file, path, ui.ctx().clone())
                                        .map_err(|e| e.to_string())?,
                                );
                                return Ok(());
                            }
                            if !path.is_file() {
                                return Err("Сохранённый файл отсутствует на диске".into());
                            }
                            super::super::folder::open(if open { &path } else { &folder })
                                .map_err(|e| e.to_string())
                        })
                        .err();
                }
            });
            if !file.caption.is_empty() {
                ui.label(&file.caption);
            }
            if let Some(preview) =
                self.preview.as_mut().filter(|p| p.key == file.key && p.name == file.name)
            {
                preview.show(ui);
            }
        }
        if let Some(error) = &self.file_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    }
    fn changes(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        run: &Run,
        next: &str,
        editable: bool,
    ) -> Option<String> {
        if run.diff.is_empty() {
            ui.weak("Для этой попытки снимок изменений отсутствует.");
            return None;
        }
        ui.horizontal(|ui| {
            ui.strong(format!("Попытка {}", run.id));
            copy_button(ui, &run.diff, "Скопировать изменения");
        });
        let mut step = None;
        let width = ui.available_width();
        egui::ScrollArea::both().id_salt(("patch", task.id, run.id)).max_height(300.0).show(
            ui,
            |ui| {
                let mut cursor = Lines::default();
                for (i, line) in run.diff.lines().enumerate() {
                    if let Some((path, side, number)) = cursor.step(line) {
                        let anchor =
                            format!("{path}:{number} ({side}), попытка {}\n{line}", run.id);
                        let tone = if line.starts_with('+') { Tone::Accent } else { Tone::Error };
                        let text = egui::RichText::new(format!("{number:>4}  {line}"))
                            .monospace()
                            .color(tone_colors(ui, tone).1);
                        ui.push_id(i, |ui| {
                            let selected = self.anchor.as_ref() == Some(&anchor);
                            if ui
                                .selectable_label(selected, text)
                                .on_hover_text(format!("Добавить замечание к строке {number}"))
                                .clicked()
                            {
                                self.anchor = (!selected).then(|| anchor.clone());
                                self.note.clear();
                            }
                            if self.anchor.as_ref() == Some(&anchor) {
                                ui.add(
                                    kylig_ui::TextArea::new(&mut self.note)
                                        .id_salt("annotation")
                                        .label(format!("Замечание к строке {number}"))
                                        .max_chars(2048)
                                        .rows(2)
                                        .width(width),
                                );
                                let draft = annotate(next, &anchor, &self.note);
                                if ui
                                    .add_enabled(
                                        editable && draft.is_some(),
                                        kylig_ui::Button::new("Добавить в следующий шаг"),
                                    )
                                    .clicked()
                                {
                                    step = draft;
                                    self.anchor = None;
                                    self.note.clear();
                                } else if draft.is_none() && !self.note.trim().is_empty() {
                                    ui.label("Следующий шаг с замечанием превышает 16 КиБ.");
                                }
                            }
                        });
                    } else {
                        ui.add(
                            egui::Label::new(egui::RichText::new(line).monospace().weak())
                                .selectable(true),
                        );
                    }
                }
            },
        );
        step
    }
}
// Agent answers longer than this many lines start collapsed in the chat (design spec «Чат задачи»).
const RESULT_CLAMP_LINES: usize = 8;
// Height of the transparent→surface band fading the clamped body into the bubble (mockup: 26px).
const RESULT_FADE_HEIGHT: f32 = 26.0;

/// `HH:MM` in the system zone; `None` (old books, recovered runs) hides the time entirely.
pub(super) fn format_time(ts: Option<Timestamp>) -> Option<String> {
    let zoned = ts?.to_zoned(TimeZone::system());
    Some(zoned.strftime("%H:%M").to_string())
}

/// The calendar date is only worth a tooltip when the run happened on another day.
fn time_hover_date(ts: Option<Timestamp>) -> Option<String> {
    let zoned = ts?.to_zoned(TimeZone::system());
    (zoned.date() != Zoned::now().date()).then(|| zoned.strftime("%d.%m.%Y").to_string())
}

fn display_agent(backend: &str) -> &str {
    hub_core::domain::BackendKind::parse(backend)
        .map_or(backend, |kind| crate::gui::look::agent_title(kind))
}

fn bubble_actions(ui: &mut egui::Ui, ts: Option<Timestamp>, copy: Option<&str>, flush: bool) {
    let actions = |ui: &mut egui::Ui| {
        if let Some(text) = copy {
            copy_button(ui, text, "Скопировать сообщение");
        }
        time_text(ui, ts);
    };
    if flush {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), actions);
    } else {
        actions(ui);
    }
}

fn time_text(ui: &mut egui::Ui, ts: Option<Timestamp>) {
    if let Some(time) = format_time(ts) {
        let response =
            ui.add(egui::Label::new(egui::RichText::new(time).small().weak()).selectable(false));
        if let Some(date) = time_hover_date(ts) {
            response.on_hover_text(date);
        }
    }
}

/// Measure with an independent child UI so measuring never adds a blank row to the transcript.
fn chat_bubble(ui: &mut egui::Ui, user: bool, mut contents: impl FnMut(&mut egui::Ui, bool)) {
    let row = ui.available_rect_before_wrap();
    let cap = (row.width() * if user { 0.76 } else { 0.84 }).min(600.0);
    let mut measure = ui.new_child(
        egui::UiBuilder::new()
            .id_salt("bubble_measure")
            .invisible()
            .max_rect(egui::Rect::from_min_size(row.min, egui::vec2(cap, f32::INFINITY))),
    );
    contents(&mut measure, false);
    let width = measure.min_rect().width().max(150.0).min(cap);
    let left = if user { row.right() - width } else { row.left() };
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
            egui::pos2(left, row.top()),
            egui::vec2(width, f32::INFINITY),
        )),
        |ui| contents(ui, true),
    );
}

fn copy_button(ui: &mut egui::Ui, text: &str, label: &str) {
    let id = ui.next_auto_id().with("copied");
    let now = ui.input(|input| input.time);
    let copied = ui.data(|d| d.get_temp::<f64>(id)).is_some_and(|until| now < until);
    let (icon, label) = if copied {
        (kylig_icons::CHECK, "Скопировано")
    } else {
        (kylig_icons::COPY, label)
    };
    if ui
        .add(
            kylig_ui::IconButton::new(icon, label)
                .size(24.0)
                .icon_size(14.0)
                .variant(kylig_ui::ButtonVariant::Ghost),
        )
        .clicked()
    {
        ui.ctx().copy_text(text.to_owned());
        ui.data_mut(|d| d.insert_temp(id, now + 1.5));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(1500));
    }
}

fn user_bubble(ui: &mut egui::Ui, step: &str, started_at: Option<Timestamp>) {
    chat_bubble(ui, true, |ui, flush| user_contents(ui, step, started_at, flush));
}

fn user_contents(ui: &mut egui::Ui, step: &str, started_at: Option<Timestamp>, flush_time: bool) {
    bubble(ui, true).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            ui.add(
                kylig_icons::Icon::new(kylig_icons::USER)
                    .size(15.0)
                    .color(ui.visuals().hyperlink_color),
            );
            ui.label(egui::RichText::new("Вы").strong().small());
            bubble_actions(ui, started_at, Some(step), flush_time);
        });
        ui.add(egui::Label::new(step).wrap().selectable(true));
    });
}

fn agent_bubble(ui: &mut egui::Ui, task: &Task, run: &Run, view: &mut View) {
    chat_bubble(ui, false, |ui, flush| agent_contents(ui, task, run, view, flush));
}

fn agent_contents(ui: &mut egui::Ui, task: &Task, run: &Run, view: &mut View, flush_time: bool) {
    bubble(ui, false).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            ui.add(
                kylig_icons::Icon::new(kylig_icons::ROBOT)
                    .size(15.0)
                    .color(ui.visuals().hyperlink_color),
            );
            ui.label(
                egui::RichText::new(display_agent(run.backend.as_deref().unwrap_or(&task.backend)))
                    .strong()
                    .small(),
            )
            .on_hover_text(run.state.label());
            bubble_actions(
                ui,
                run.finished_at,
                (!run.result.is_empty()).then_some(run.result.as_str()),
                flush_time,
            );
        });
        if !run.result.is_empty() {
            let long = run.result.lines().count() > RESULT_CLAMP_LINES;
            let shown_full = !long || view.expanded.contains(&run.id);
            let body = if shown_full {
                run.result.clone()
            } else {
                run.result.lines().take(RESULT_CLAMP_LINES).collect::<Vec<_>>().join("\n")
            };
            let response = ui.add(egui::Label::new(body).wrap().selectable(true));
            if long && !shown_full {
                let height = RESULT_FADE_HEIGHT.min(response.rect.height());
                let band = egui::Rect::from_min_size(
                    egui::pos2(response.rect.left(), response.rect.bottom() - height),
                    egui::vec2(response.rect.width(), height),
                );
                let surface = ui.visuals().panel_fill;
                ui.painter().add(egui::Shape::gradient_rect(
                    band,
                    egui::Direction::TopDown,
                    [egui::Color32::TRANSPARENT, surface],
                ));
            }
            if long && ui.link(if shown_full { "Свернуть" } else { "Развернуть" }).clicked()
            {
                if shown_full {
                    view.expanded.remove(&run.id);
                } else {
                    view.expanded.insert(run.id);
                }
            }
        }
        if !run.diagnostic.is_empty() {
            ui.colored_label(tone_colors(ui, Tone::Error).1, &run.diagnostic);
        }
        let checks = &run.workflow_report.checks;
        let latest = Some(run) == View::review_run(task);
        if !run.files.is_empty() || !checks.is_empty() || latest {
            ui.horizontal_wrapped(|ui| {
                if latest && ui.link("Открыть результат").clicked() {
                    view.select(Tab::Summary, Some(task.id));
                }
                if !checks.is_empty() {
                    let passed = checks.iter().filter(|check| check.passed).count();
                    let label = format!("Проверки {passed}/{}", checks.len());
                    if latest {
                        if ui.link(label).clicked() {
                            view.select(Tab::Checks, Some(task.id));
                        }
                    } else {
                        chip(ui, Tone::Neutral, &label);
                    }
                }
                if !run.files.is_empty() {
                    if latest {
                        if ui.link(format!("Файлов · {}", run.files.len())).clicked() {
                            view.select(Tab::Files, Some(task.id));
                        }
                    } else {
                        chip(ui, Tone::Neutral, &format!("Файлов · {}", run.files.len()));
                    }
                }
            });
        }
    });
}

// The «Изменения · N …» tab keeps Russian agreement for the count (mockup: «3 файла»).
fn file_count_word(count: usize) -> &'static str {
    match count % 100 {
        11..=14 => "файлов",
        _ => match count % 10 {
            1 => "файл",
            2..=4 => "файла",
            _ => "файлов",
        },
    }
}

fn annotate(next: &str, anchor: &str, note: &str) -> Option<String> {
    if note.trim().is_empty() {
        return None;
    }
    let note = format!("Исправь замечание к {anchor}\n{}", note.trim());
    let step = if next.trim().is_empty() { note } else { format!("{next}\n\n{note}") };
    (step.len() <= 16 * 1024).then_some(step)
}
fn summary(ui: &mut egui::Ui, run: &Run) {
    ui.horizontal(|ui| {
        ui.strong(format!("Попытка {}", run.id));
        if !run.result.is_empty() {
            copy_button(ui, &run.result, "Скопировать результат");
        }
    });
    ui.add(
        egui::Label::new(if run.result.is_empty() { run.state.label() } else { &run.result })
            .wrap()
            .selectable(true),
    );
    if !run.diagnostic.is_empty() {
        ui.colored_label(ui.visuals().error_fg_color, &run.diagnostic);
    }
    let checks = &run.workflow_report.checks;
    let changed = run.diff.lines().filter(|line| line.starts_with("diff --git ")).count();
    if run.files.is_empty() && checks.is_empty() && changed == 0 {
        return;
    }
    ui.add_space(6.0);
    ui.horizontal_wrapped(|ui| {
        if changed > 0 {
            ui.weak(format!("Изменения · {changed} {}", file_count_word(changed)));
        }
        if !run.files.is_empty() {
            chip(ui, Tone::Neutral, &format!("Файлов · {}", run.files.len()));
        }
        if !checks.is_empty() {
            let passed = checks.iter().filter(|check| check.passed).count();
            chip(ui, Tone::Neutral, &format!("Проверки · {passed}/{}", checks.len()));
        }
    });
}
fn report(ui: &mut egui::Ui, run: &Run) {
    if !run.diagnostic.is_empty() {
        ui.label(&run.diagnostic);
    }
    if run.workflow_report.is_empty() {
        ui.weak("Результатов автоматических проверок пока нет.");
    } else {
        if let Some(command) = run.check_command() {
            ui.monospace(command);
        }
        for (index, check) in run.workflow_report.checks.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.add(kylig_ui::Badge::dot().severity(if check.passed {
                    kylig_ui::Severity::Success
                } else {
                    kylig_ui::Severity::Error
                }));
                ui.label(format!(
                    "Проверка {} · {}",
                    index + 1,
                    if check.passed { "прошла" } else { "не прошла" }
                ));
                if !check.output.is_empty() {
                    copy_button(ui, &check.output, "Скопировать вывод проверки");
                }
            });
            ui.add(
                egui::Label::new(egui::RichText::new(&check.output).monospace())
                    .selectable(true)
                    .wrap(),
            );
        }
        if !run.workflow_report.review.is_empty() {
            ui.strong("Ревью");
            ui.label(&run.workflow_report.review);
        }
    }
}

#[derive(Default)]
struct Lines<'a> {
    old_path: &'a str,
    new_path: &'a str,
    old: (u64, u64),
    new: (u64, u64),
}
fn range(text: &str) -> Option<(u64, u64)> {
    let (start, count) = text.split_once(',').unwrap_or((text, "1"));
    let start: u64 = start.parse().ok()?;
    let count: u64 = count.parse().ok()?;
    start.checked_add(count)?;
    Some((start, count))
}
impl<'a> Lines<'a> {
    fn step(&mut self, line: &'a str) -> Option<(&'a str, &'static str, u64)> {
        if let Some(hunk) = line.strip_prefix("@@ -") {
            let (old, new) = hunk.split_once(" +")?;
            self.old = range(old)?;
            self.new = range(new.split_whitespace().next()?)?;
        } else if line.starts_with("diff --git ") {
            *self = Self::default();
        } else if self.new.1 == 0 && self.old.1 == 0 {
            if let Some(path) = line.strip_prefix("--- ") {
                self.old_path = path.trim_end().strip_prefix("a/").unwrap_or(path.trim_end());
            }
            if let Some(path) = line.strip_prefix("+++ ") {
                self.new_path = path.trim_end().strip_prefix("b/").unwrap_or(path.trim_end());
            }
        } else {
            let n = self.new.0;
            match line.as_bytes().first()? {
                b'+' if self.new.1 > 0 => {
                    self.new.0 += 1;
                    self.new.1 -= 1;
                    return Some((self.new_path, "после", n));
                }
                b'-' if self.old.1 > 0 => {
                    let old = self.old.0;
                    self.old.0 += 1;
                    self.old.1 -= 1;
                    return Some((self.old_path, "до", old));
                }
                b' ' if self.new.1 > 0 && self.old.1 > 0 => {
                    self.old.0 += 1;
                    self.old.1 -= 1;
                    self.new.0 += 1;
                    self.new.1 -= 1;
                }
                _ => {}
            }
        }
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::tasks::{
        ResultFile, TaskBook,
        workflow::{CheckResult, WorkflowReport},
    };
    #[test]
    fn diff_headers_inside_a_hunk_are_annotated_as_content() {
        let mut lines = Lines::default();
        for line in
            ["diff --git a/lib b/lib", "--- a/lib", "+++ b/lib", "@@ -7,2 +9,2 @@ fn", " context"]
        {
            assert!(lines.step(line).is_none());
        }
        assert_eq!(lines.step("--- a/old"), Some(("lib", "до", 8)));
        assert_eq!(lines.step("+++ b/new"), Some(("lib", "после", 10)));
        assert!(lines.step("untracked").is_none());
        let step = annotate("Проверь", "lib:10 (после)\n+++ b/new", "Пустая строка").unwrap();
        assert!(
            step.starts_with("Проверь\n\n")
                && step.contains("lib:10")
                && step.ends_with("Пустая строка")
        );
        assert!(annotate(&"я".repeat(8192), "lib:10", "Ещё").is_none());
    }

    fn ts() -> Timestamp {
        Timestamp::from_second(1_700_000_000).unwrap()
    }

    fn tabs_fixture() -> Task {
        let mut book = TaskBook::default();
        let id =
            book.create(std::env::temp_dir(), "qwen", "Вкладки результата", "Проверь").unwrap();
        let run =
            book.begin(id, 1, None, ts(), &hub_core::settings::AgentRoles::default()).unwrap();
        book.record(id, run, "Итог попытки").unwrap();
        book.finish(id, run, RunState::Completed, ts()).unwrap();
        let mut task = book.get(id).unwrap().clone();
        let run = &mut task.runs[0];
        run.diff = "diff --git a/lib b/lib\n--- a/lib\n+++ b/lib\n@@ -1,1 +1,2 @@\n контекст\n+новая строка\n"
            .to_owned();
        run.files = vec![ResultFile {
            key: "f".repeat(32),
            name: "report.txt".to_owned(),
            caption: String::new(),
            bytes: 12,
        }];
        run.workflow_report = WorkflowReport {
            checks: vec![
                CheckResult { passed: true, output: "ok".to_owned() },
                CheckResult { passed: false, output: "fail".to_owned() },
            ],
            review: String::new(),
        };
        task
    }

    fn frame(
        ctx: &egui::Context,
        view: &mut View,
        task: &Task,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                view.show(ui, task, "Следующий шаг", true);
            },
        );
        output.textures_delta.clear();
        output
    }

    fn labels(output: &egui::FullOutput) -> Vec<&str> {
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect()
    }

    fn click(ctx: &egui::Context, view: &mut View, task: &Task, text: &str) {
        frame(ctx, view, task, vec![]);
        let output = frame(ctx, view, task, vec![]);
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(label) if label.galley.text() == text => {
                    Some(label.galley.rect.translate(label.pos.to_vec2()).center())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing text: {text}"));
        for pressed in [true, false] {
            frame(
                ctx,
                view,
                task,
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
    fn attachment_preview_copies_the_stored_text() {
        let ctx = egui::Context::default();
        kylig_icons::install_weights(&ctx, &[kylig_icons::Weight::Regular]);
        let task = tabs_fixture();
        let mut view = View::default();
        view.select(Tab::Files, Some(task.id));
        let _ = frame(&ctx, &mut view, &task, vec![]);
        let folder = tempfile::tempdir().unwrap();
        let file = &task.runs[0].files[0];
        let path = folder.path().join(&file.name);
        let report = "Результаты проверки\n2 теста пройдены";
        std::fs::write(&path, report).unwrap();
        view.preview = Some(FilePreview::open(file, path, ctx.clone()).unwrap());
        let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let copy = loop {
            let out = frame(&ctx, &mut view, &task, vec![]);
            if labels(&out).contains(&report) {
                break out
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text() == kylig_icons::COPY => {
                            Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                        }
                        _ => None,
                    })
                    .expect("copy action for the loaded file");
            }
            assert!(std::time::Instant::now() < until);
            std::thread::yield_now();
        };
        let pointer = |pressed| egui::Event::PointerButton {
            pos: copy,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&ctx, &mut view, &task, vec![egui::Event::PointerMoved(copy), pointer(true)]);
        let output = frame(&ctx, &mut view, &task, vec![pointer(false)]);
        assert!(output.platform_output.commands.iter().any(
            |command| matches!(command, egui::OutputCommand::CopyText(body) if body == report)
        ));
    }

    #[test]
    fn review_tabs_switch_sections_one_at_a_time() {
        let task = tabs_fixture();
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let mut view = View::default();
        view.select(Tab::Summary, Some(task.id));
        frame(&ctx, &mut view, &task, vec![]);
        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        for tab in ["Сводка", "Изменения", "Проверки", "Файлы", "История"]
        {
            assert!(texts.contains(&tab), "missing tab {tab}");
        }
        assert!(texts.contains(&"Итог попытки"), "Сводка shows the saved result");
        assert!(
            !texts.iter().any(|text| text.contains("новая строка")),
            "diff lines stay hidden outside Изменения"
        );
        assert!(!texts.contains(&"report.txt"), "files stay hidden outside Файлы");
        assert!(
            !texts.iter().any(|text| text.starts_with("Проверка 1")),
            "checks stay hidden outside Проверки"
        );
        click(&ctx, &mut view, &task, "Файлы");
        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        assert!(texts.contains(&"report.txt"), "Файлы lists the saved files");
        assert!(!texts.contains(&"Итог попытки"), "only one section at a time");
        assert!(
            !texts.iter().any(|text| text.contains("новая строка")),
            "Файлы does not draw the diff"
        );
        click(&ctx, &mut view, &task, "Изменения");
        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        assert!(
            texts.iter().any(|text| text.contains("новая строка")),
            "Изменения shows the annotated diff"
        );
        assert!(!texts.contains(&"report.txt"), "Изменения does not draw the file list");
        assert_eq!(view.tab(&task), Tab::Changes);
        click(&ctx, &mut view, &task, "Проверки");
        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        assert!(texts.contains(&"Проверка 1 · прошла"));
        assert!(texts.contains(&"Проверка 2 · не прошла"));
        assert!(
            !texts.iter().any(|text| text.contains("новая строка")),
            "Проверки does not draw the diff"
        );
        click(&ctx, &mut view, &task, "История");
        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        assert!(texts.iter().any(|text| text.starts_with("Попытка 1 ·")));
        assert!(
            texts.contains(&"report.txt") && texts.contains(&"Итог попытки"),
            "history opens the latest attempt"
        );
        click(&ctx, &mut view, &task, "Сводка");
        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        assert!(texts.contains(&"Итог попытки"), "Сводка returns with the result label");
    }

    #[test]
    fn history_opens_the_latest_attempt_and_can_show_an_earlier_one() {
        let mut task = tabs_fixture();
        let mut later = task.runs.last().unwrap().clone();
        later.id += 1;
        later.step = "Второй шаг".into();
        later.result = "Второй ответ".into();
        task.runs.push(later);
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let mut view = View::default();
        view.select(Tab::History, Some(task.id));
        let output = frame(&ctx, &mut view, &task, vec![]);
        assert!(labels(&output).contains(&"Второй ответ"));
        click(&ctx, &mut view, &task, "Попытка 2 · Qwen Code");
        click(&ctx, &mut view, &task, "Попытка 1 · Qwen Code");
        let output = frame(&ctx, &mut view, &task, vec![]);
        assert!(labels(&output).contains(&"Итог попытки"));
        assert!(!labels(&output).contains(&"Второй ответ"));
    }

    #[test]
    fn narrow_review_exposes_all_sections_in_a_menu() {
        let task = tabs_fixture();
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let mut view = View::default();
        view.select(Tab::Summary, Some(task.id));
        let mut render = |events: Vec<egui::Event>| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(390.0, 740.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    view.show(ui, &task, "", true);
                },
            );
            output.textures_delta.clear();
            output
        };
        let output = render(vec![]);
        assert!(
            labels(&output).contains(&"Раздел результата") && !labels(&output).contains(&"История")
        );
        for target in ["Сводка", "Проверки"] {
            let output = render(vec![]);
            let position = output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == target => {
                        Some(t.visual_bounding_rect().center())
                    }
                    _ => None,
                })
                .expect("visible menu choice");
            for pressed in [true, false] {
                render(vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
        }
        assert!(labels(&render(vec![])).contains(&"Проверка 2 · не прошла"));
    }

    #[test]
    fn history_names_the_agents_frozen_at_start_and_not_the_shared_role_wording() {
        use hub_core::domain::BackendKind;
        use hub_core::settings::AgentRoles;
        use hub_core::tasks::{ProjectRules, workflow::WorkflowChoice};
        let mut book = TaskBook::default();
        let project = std::env::temp_dir().join("agent-hub-gui-frozen-history");
        let id = book.create(project.clone(), "hermes", "История", "Проверь").unwrap();
        book.configure_rules(
            project,
            0,
            ProjectRules {
                revision: 0,
                enabled: true,
                text: "Правила".into(),
                git: None,
                workflow: Some(WorkflowChoice::Inherit { command: "cargo test".into() }),
            },
        )
        .unwrap();
        // The attempt starts while Hermes is the shared implementer; settings may move on.
        let hermes = AgentRoles { implementer: BackendKind::Hermes, reviewer: BackendKind::Claude };
        let run = book.begin(id, 1, None, ts(), &hermes).unwrap();
        book.record(id, run, "Итог").unwrap();
        book.finish(id, run, RunState::Completed, ts()).unwrap();
        let task = book.get(id).unwrap().clone();
        let ctx = egui::Context::default();
        let mut view = View::default();
        view.select(Tab::History, Some(task.id));
        frame(&ctx, &mut view, &task, vec![]);

        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        assert!(
            texts.iter().any(|text| text.contains("для hermes") && text.contains("ревью claude")),
            "the frozen agents must be named: {texts:?}"
        );
        assert!(
            !texts.iter().any(|text| text.contains("по общим ролям")),
            "history never renders the unresolved shared-role wording: {texts:?}"
        );
    }
}
