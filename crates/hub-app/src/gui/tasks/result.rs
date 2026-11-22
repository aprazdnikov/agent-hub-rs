//! Task review tabs and annotations against the saved patch of a particular attempt.
use crate::gui::theme::{Tone, bubble, chip, tone_colors};
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
}
impl View {
    pub(super) fn select(&mut self, tab: Tab, task: Option<u64>) {
        *self = Self { task, tab, ..Default::default() };
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
        let section = self.tab;
        if matches!(section, Tab::Checks | Tab::Changes | Tab::History) {
            self.tab = Tab::Summary;
        }
        if self.run != Some(run.id) {
            self.file_error = None;
            self.anchor = None;
            self.note.clear();
            self.run = Some(run.id);
        }
        ui.add(
            egui::Label::new(if run.result.is_empty() { run.state.label() } else { &run.result })
                .wrap()
                .selectable(true),
        );
        if !run.diagnostic.is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, &run.diagnostic);
        }
        ui.add_space(6.0);
        let annotation = egui::CollapsingHeader::new("Изменения")
            .id_salt(("result_diff", &task.project, task.id, run.id))
            .open((section == Tab::Changes).then_some(true))
            .show(ui, |ui| self.changes(ui, task, run, next, editable))
            .body_returned
            .flatten();
        let checks = &run.workflow_report.checks;
        let count = checks.iter().filter(|check| check.passed).count();
        let checks_label = if checks.is_empty() {
            "Проверки".to_owned()
        } else {
            format!("Проверки · пройдено {count} из {}", checks.len())
        };
        egui::CollapsingHeader::new(checks_label)
            .id_salt(("result_checks", &task.project, task.id, run.id))
            .open((section == Tab::Checks).then_some(true))
            .show(ui, |ui| report(ui, run));
        if !run.files.is_empty() {
            ui.collapsing(format!("Файлы ({})", run.files.len()), |ui| self.files(ui, run));
        }
        egui::CollapsingHeader::new(format!("История попыток ({})", task.runs.len()))
            .id_salt(("result_history", &task.project, task.id))
            .open((section == Tab::History).then_some(true))
            .show(ui, |ui| {
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
            });
        annotation
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
                        let color = match (line.starts_with('+'), ui.visuals().dark_mode) {
                            (true, true) => egui::Color32::from_rgb(133, 218, 165),
                            (true, false) => egui::Color32::from_rgb(28, 102, 59),
                            (false, true) => egui::Color32::from_rgb(253, 166, 160),
                            (false, false) => egui::Color32::from_rgb(148, 49, 47),
                        };
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

fn time_label(ui: &mut egui::Ui, ts: Option<Timestamp>) {
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
    let contents = |ui: &mut egui::Ui| {
        bubble(ui, true).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.strong("Вы");
                time_label(ui, started_at);
            });
            ui.add(egui::Label::new(step).wrap().selectable(true));
        });
    };
    // Measure the natural bubble width against the cap, then draw the same contents
    // flush with the right edge: frames only shrink-wrap while laid out left-to-right.
    // The measuring copy stays invisible and disabled so it paints nothing and cannot
    // steal hover, tooltips or text-cursor state from the drawn bubble.
    let width = ui
        .scope_builder(
            egui::UiBuilder::new().invisible().disabled().max_rect(egui::Rect::from_min_size(
                ui.cursor().min,
                egui::vec2(cap, f32::INFINITY),
            )),
            contents,
        )
        .response
        .rect
        .width();
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
            egui::pos2(row.max.x - width, row.min.y),
            egui::vec2(width, f32::INFINITY),
        )),
        contents,
    );
}

fn agent_bubble(ui: &mut egui::Ui, task: &Task, run: &Run, view: &mut View) {
    let width = ui.available_width();
    ui.scope_builder(
        egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(width * 0.86, f32::INFINITY),
        )),
        |ui| {
            bubble(ui, false).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::HOVER);
                    let accent = ui.visuals().hyperlink_color;
                    ui.painter().circle_filled(dot.center(), 4.0, accent);
                    ui.strong(run.backend.as_deref().unwrap_or(&task.backend));
                    chip(ui, state_tone(run.state), run.state.label());
                    time_label(ui, run.finished_at);
                });
                if !run.result.is_empty() {
                    let long = run.result.lines().count() > RESULT_CLAMP_LINES;
                    let shown_full = !long || view.expanded.contains(&run.id);
                    let body = if shown_full {
                        run.result.clone()
                    } else {
                        run.result.lines().take(RESULT_CLAMP_LINES).collect::<Vec<_>>().join("\n")
                    };
                    ui.add(egui::Label::new(body).wrap().selectable(true));
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
                            chip(
                                ui,
                                Tone::Neutral,
                                &format!("Проверки · {passed}/{}", checks.len()),
                            );
                        }
                        if latest && ui.link("Результат →").clicked() {
                            view.select(Tab::Summary, Some(task.id));
                        }
                    });
                }
            });
        },
    );
}

fn annotate(next: &str, anchor: &str, note: &str) -> Option<String> {
    if note.trim().is_empty() {
        return None;
    }
    let note = format!("Исправь замечание к {anchor}\n{}", note.trim());
    let step = if next.trim().is_empty() { note } else { format!("{next}\n\n{note}") };
    (step.len() <= 16 * 1024).then_some(step)
}
fn report(ui: &mut egui::Ui, run: &Run) {
    if !run.diagnostic.is_empty() {
        ui.label(&run.diagnostic);
    }
    if run.workflow_report.is_empty() {
        ui.weak("Результатов автоматических проверок пока нет.");
    } else {
        if let Some(preset) = run.rules.as_ref().and_then(|rules| rules.workflow.as_ref()) {
            ui.monospace(&preset.command);
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
}
