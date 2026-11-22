//! Task review tabs and annotations against the saved patch of a particular attempt.
use eframe::egui;
use hub_core::tasks::{Run, Task};

#[derive(Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    #[default]
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
    scroll: bool,
}
impl View {
    pub(super) fn keep_position(&mut self) {
        self.scroll = false;
    }
    pub(super) fn select(&mut self, tab: Tab, task: Option<u64>) {
        *self = Self { task, tab, scroll: true, ..Default::default() };
    }
    pub(super) fn show(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        next: &str,
        editable: bool,
    ) -> Option<String> {
        let run = task.runs.last()?;
        if self.task != Some(task.id) {
            *self = Self { task: Some(task.id), ..Default::default() };
        }
        if self.run != Some(run.id) {
            self.anchor = None;
            self.note.clear();
            self.run = Some(run.id);
        }
        let response = ui
            .horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Changes, "Изменения");
                ui.selectable_value(&mut self.tab, Tab::Checks, "Проверки");
                ui.selectable_value(
                    &mut self.tab,
                    Tab::History,
                    format!("История попыток ({})", task.runs.len()),
                );
            })
            .response;
        if self.scroll {
            response.scroll_to_me(Some(egui::Align::Min));
            self.scroll = false;
        }
        match self.tab {
            Tab::Changes => self.changes(ui, task, run, next, editable),
            Tab::Checks => {
                report(ui, run);
                None
            }
            Tab::History => {
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
                None
            }
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
