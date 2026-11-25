//! Task review tabs and annotations against the saved patch of a particular attempt.
use crate::gui::theme::{Tone, bubble, chip, tab, tone_colors};
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
    file_error: Option<String>,
    expanded: BTreeSet<u32>,
    /// The «…» menu landed on Parameters and wants the next-run context open; consumed there.
    context_open: bool,
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
            self.anchor = None;
            self.note.clear();
            self.run = Some(run.id);
        }
        if let Some(target) = self.tab_bar(ui, task, run) {
            self.tab = target;
        }
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
    }
    /// The section tabs from the spec «Экран результата»; clicks move `tab` directly, so the
    /// row keeps annotations, the line note and the diff anchor that `select` would reset.
    fn tab_bar(&self, ui: &mut egui::Ui, task: &Task, run: &Run) -> Option<Tab> {
        let checks = &run.workflow_report.checks;
        let passed = checks.iter().filter(|check| check.passed).count();
        let diff_files = run.diff.lines().filter(|line| line.starts_with("diff --git ")).count();
        let tabs = [
            (Tab::Summary, "Сводка".to_owned()),
            (Tab::Changes, format!("Изменения · {diff_files} {}", file_count_word(diff_files))),
            (Tab::Checks, format!("Проверки · {passed}/{}", checks.len())),
            (Tab::Files, format!("Файлы · {}", run.files.len())),
            (Tab::History, format!("История · {}", task.runs.len())),
        ];
        let mut chosen = None;
        ui.horizontal_wrapped(|ui| {
            for (target, label) in &tabs {
                if tab(ui, self.tab == *target, label).clicked() {
                    chosen = Some(*target);
                }
            }
        });
        chosen
    }
    fn history(&mut self, ui: &mut egui::Ui, task: &Task) {
        for attempt in task.runs.iter().rev() {
            ui.collapsing(
                format!(
                    "Попытка {} · {} · {}",
                    attempt.id,
                    attempt.state.label(),
                    attempt.backend.as_deref().unwrap_or(&task.backend)
                ),
                |ui| {
                    ui.label(&attempt.step);
                    if let Some(rules) = &attempt.rules {
                        ui.label(rules.describe());
                    }
                    if !attempt.diagnostic.is_empty() {
                        ui.label(&attempt.diagnostic);
                    }
                    ui.label(if attempt.result.is_empty() {
                        "Сохранённого ответа нет"
                    } else {
                        &attempt.result
                    });
                    self.files(ui, attempt);
                    if !attempt.workflow_report.is_empty() {
                        report(ui, attempt);
                    }
                    if !attempt.diff.is_empty() {
                        ui.collapsing("Сохранённые изменения", |ui| {
                            ui.monospace(&attempt.diff);
                        });
                    }
                },
            );
        }
    }
    pub(super) fn transcript(ui: &mut egui::Ui, task: &Task, view: &mut Self) {
        for run in &task.runs {
            user_bubble(ui, &run.step, run.started_at);
            ui.add_space(12.0);
            agent_bubble(ui, task, run, view);
            ui.add_space(12.0);
        }
    }
    fn files(&mut self, ui: &mut egui::Ui, run: &Run) {
        if run.files.is_empty() {
            return;
        }
        for file in &run.files {
            ui.horizontal_wrapped(|ui| {
                ui.label(&file.name);
                ui.weak(format!("{} байт", file.bytes));
                let openable = std::path::Path::new(&file.name)
                    .extension()
                    .and_then(|v| v.to_str())
                    .is_some_and(|ext| {
                        ["txt", "pdf", "png", "jpg", "jpeg", "webp"]
                            .iter()
                            .any(|v| ext.eq_ignore_ascii_case(v))
                    });
                let open = openable && ui.button("Открыть файл").clicked();
                let reveal = ui.button("В папке").clicked();
                if open || reveal {
                    self.file_error = crate::dirs::AppDirs::locate()
                        .ok_or_else(|| "Каталог данных недоступен".to_owned())
                        .and_then(|dirs| {
                            let folder = dirs.task_files().join(&file.key);
                            let path = folder.join(&file.name);
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
        egui::ScrollArea::both().id_salt(("patch", task.id, run.id)).max_height(300.0).show(
            ui,
            |ui| {
                let mut cursor = Lines::default();
                for (i, line) in run.diff.lines().enumerate() {
                    if let Some((path, side, n)) = cursor.step(line) {
                        let anchor = format!("{path}:{n} ({side}), попытка {}\n{line}", run.id);
                        let tone = if line.starts_with('+') { Tone::Accent } else { Tone::Error };
                        let color = tone_colors(ui, tone).1;
                        let text =
                            egui::RichText::new(format!("{n:>4}  {line}")).monospace().color(color);
                        ui.push_id(i, |ui| {
                            if ui
                                .selectable_label(self.anchor.as_ref() == Some(&anchor), text)
                                .on_hover_text("Добавить замечание к строке")
                                .clicked()
                            {
                                self.anchor = Some(anchor);
                            }
                        });
                    } else {
                        ui.monospace(line);
                    }
                }
            },
        );
        if let Some(anchor) = &self.anchor {
            ui.label(anchor);
            ui.add(
                egui::TextEdit::multiline(&mut self.note)
                    .char_limit(2048)
                    .desired_rows(2)
                    .hint_text("Замечание к строке"),
            );
            let step = annotate(next, anchor, &self.note);
            if ui
                .add_enabled(
                    editable && step.is_some(),
                    egui::Button::new("Добавить в следующий шаг"),
                )
                .clicked()
            {
                self.anchor = None;
                self.note.clear();
                return step;
            }
            if step.is_none() && !self.note.trim().is_empty() {
                ui.label("Следующий шаг с замечанием превышает 16 КиБ.");
            }
        }
        None
    }
}
// Agent answers longer than this many lines start collapsed in the chat (design spec «Чат задачи»).
const RESULT_CLAMP_LINES: usize = 8;
// Height of the transparent→surface band fading the clamped body into the bubble (mockup: 26px).
const RESULT_FADE_HEIGHT: f32 = 26.0;

fn state_tone(state: RunState) -> Tone {
    match state {
        RunState::Completed => Tone::Accent,
        RunState::Running => Tone::Neutral,
        RunState::Failed | RunState::Interrupted => Tone::Error,
    }
}

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

/// Small weak clock text; `flush` pushes it to the right edge of the enclosing row
/// (design spec «время справа»), a no-op when no timestamp is known.
fn time_label(ui: &mut egui::Ui, ts: Option<Timestamp>, flush: bool) {
    if flush && format_time(ts).is_some() {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| time_text(ui, ts));
    } else {
        time_text(ui, ts);
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

fn user_bubble(ui: &mut egui::Ui, step: &str, started_at: Option<Timestamp>) {
    let row = ui.available_rect_before_wrap();
    let cap = row.width() * 0.78;
    // Measure the natural bubble width against the cap with a packed header, then draw
    // at exactly that width so the time can be flush right without widening the bubble:
    // frames only shrink-wrap while laid out left-to-right. The measuring copy stays
    // invisible (egui makes it disabled too) so it paints nothing and cannot steal
    // hover, tooltips or text-cursor state from the drawn bubble.
    let width = ui
        .scope_builder(
            egui::UiBuilder::new().invisible().max_rect(egui::Rect::from_min_size(
                ui.cursor().min,
                egui::vec2(cap, f32::INFINITY),
            )),
            |ui| user_contents(ui, step, started_at, false),
        )
        .response
        .rect
        .width();
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
            egui::pos2(row.max.x - width, row.min.y),
            egui::vec2(width, f32::INFINITY),
        )),
        |ui| user_contents(ui, step, started_at, true),
    );
}

fn user_contents(ui: &mut egui::Ui, step: &str, started_at: Option<Timestamp>, flush_time: bool) {
    bubble(ui, true).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.strong("Вы");
            time_label(ui, started_at, flush_time);
        });
        ui.add(egui::Label::new(step).wrap().selectable(true));
    });
}

fn agent_bubble(ui: &mut egui::Ui, task: &Task, run: &Run, view: &mut View) {
    let row = ui.available_rect_before_wrap();
    let cap = row.width() * 0.86;
    let top = row.min.y;
    let width = ui
        .scope_builder(
            egui::UiBuilder::new().invisible().max_rect(egui::Rect::from_min_size(
                ui.cursor().min,
                egui::vec2(cap, f32::INFINITY),
            )),
            |ui| agent_contents(ui, task, run, view, false),
        )
        .response
        .rect
        .width();
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
            egui::pos2(row.min.x, top),
            egui::vec2(width, f32::INFINITY),
        )),
        |ui| agent_contents(ui, task, run, view, true),
    );
}

fn agent_contents(ui: &mut egui::Ui, task: &Task, run: &Run, view: &mut View, flush_time: bool) {
    bubble(ui, false).show(ui, |ui| {
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::HOVER);
            let accent = ui.visuals().hyperlink_color;
            ui.painter().circle_filled(dot.center(), 4.0, accent);
            ui.strong(run.backend.as_deref().unwrap_or(&task.backend));
            chip(ui, state_tone(run.state), run.state.label());
            time_label(ui, run.finished_at, flush_time);
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
            ui.horizontal(|ui| {
                if !run.files.is_empty() {
                    chip(ui, Tone::Neutral, &format!("Файлов · {}", run.files.len()));
                }
                if !checks.is_empty() {
                    let passed = checks.iter().filter(|check| check.passed).count();
                    chip(ui, Tone::Neutral, &format!("Проверки · {passed}/{}", checks.len()));
                }
                if latest && ui.link("Результат ›").clicked() {
                    view.select(Tab::Summary, Some(task.id));
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
    ui.add(
        egui::Label::new(if run.result.is_empty() { run.state.label() } else { &run.result })
            .wrap()
            .selectable(true),
    );
    if !run.diagnostic.is_empty() {
        ui.colored_label(ui.visuals().error_fg_color, &run.diagnostic);
    }
    let checks = &run.workflow_report.checks;
    if run.files.is_empty() && checks.is_empty() {
        return;
    }
    ui.add_space(6.0);
    ui.horizontal(|ui| {
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
        if let Some(choice) = run.rules.as_ref().and_then(|rules| rules.workflow.as_ref()) {
            ui.monospace(choice.command());
        }
        for (index, check) in run.workflow_report.checks.iter().enumerate() {
            ui.strong(format!(
                "Проверка {} · {}",
                index + 1,
                if check.passed { "прошла" } else { "не прошла" }
            ));
            ui.monospace(&check.output);
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
        let run = book.begin(id, 1, None, ts()).unwrap();
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
    fn review_tabs_switch_sections_one_at_a_time() {
        let task = tabs_fixture();
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let mut view = View::default();
        view.select(Tab::Summary, Some(task.id));
        frame(&ctx, &mut view, &task, vec![]);
        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        for tab in ["Сводка", "Изменения · 1 файл", "Проверки · 1/2", "Файлы · 1", "История · 1"]
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
        click(&ctx, &mut view, &task, "Файлы · 1");
        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        assert!(texts.contains(&"report.txt"), "Файлы lists the saved files");
        assert!(!texts.contains(&"Итог попытки"), "only one section at a time");
        assert!(
            !texts.iter().any(|text| text.contains("новая строка")),
            "Файлы does not draw the diff"
        );
        click(&ctx, &mut view, &task, "Изменения · 1 файл");
        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        assert!(
            texts.iter().any(|text| text.contains("новая строка")),
            "Изменения shows the annotated diff"
        );
        assert!(!texts.contains(&"report.txt"), "Изменения does not draw the file list");
        assert_eq!(view.tab(&task), Tab::Changes);
        click(&ctx, &mut view, &task, "Проверки · 1/2");
        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        assert!(texts.contains(&"Проверка 1 · прошла"));
        assert!(texts.contains(&"Проверка 2 · не прошла"));
        assert!(
            !texts.iter().any(|text| text.contains("новая строка")),
            "Проверки does not draw the diff"
        );
        click(&ctx, &mut view, &task, "История · 1");
        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        assert!(texts.iter().any(|text| text.starts_with("Попытка 1 ·")));
        assert!(!texts.contains(&"report.txt"), "history bodies stay collapsed");
        click(&ctx, &mut view, &task, "Сводка");
        let out = frame(&ctx, &mut view, &task, vec![]);
        let texts = labels(&out);
        assert!(texts.contains(&"Итог попытки"), "Сводка returns with the result label");
    }
}
