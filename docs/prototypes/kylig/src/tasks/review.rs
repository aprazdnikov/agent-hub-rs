use super::chat::copy_button;
use super::{Effect, Page, Stage, Task, Tasks};
use crate::widgets::{action, card, primary};
use eframe::egui;
use kylig_ui::{Badge, Select, SelectItem, Severity, Tabs, TabsVariant, TextArea};

const SECTIONS: [&str; 5] = ["Сводка", "Изменения", "Проверки", "Файлы", "История"];
const SOURCE: &str = "src/tasks/search.rs";

impl Tasks {
    pub(super) fn review(&mut self, ui: &mut egui::Ui, id: usize) {
        if ui.available_width() > 500.0 {
            Tabs::new(&mut self.review_tab, SECTIONS)
                .id_salt(("review", id))
                .variant(TabsVariant::Underline)
                .show(ui);
        } else {
            let sections = SECTIONS
                .into_iter()
                .enumerate()
                .map(|(i, s)| SelectItem::new(i, s))
                .collect::<Vec<_>>();
            ui.add(
                Select::new(&mut self.review_tab, &sections)
                    .label("Раздел результата")
                    .id_salt("review.compact")
                    .width(ui.available_width()),
            );
        }
        ui.add_space(8.0);
        let Some(task) = self.entry(id) else { return };
        let stage = task.stage;
        match self.review_tab {
            0 => summary(ui, task),
            1 => self.diff(ui, id),
            2 => checks(ui, task),
            3 => files(ui, task),
            _ => {
                if let Some(task) = self.entry_mut(id) {
                    history(ui, task);
                }
            }
        }
        ui.add_space(8.0);
        if self.entry(id).is_some_and(|task| task.ops.archived || task.ops.queue.is_some()) {
            return;
        }
        if stage == Stage::Failed {
            if primary(ui, "Повторить запуск") {
                self.start(ui, "Восстанавливаем соединение с агентом…", Effect::Run(id), false);
            }
        } else if stage == Stage::Review {
            ui.horizontal_wrapped(|ui| {
                if primary(ui, "Принять результат") {
                    self.start(ui, "Принимаем результат…", Effect::Accept(id), self.from_inbox);
                }
                if action(ui, "На доработку") {
                    self.page = Page::Chat;
                }
            });
        }
    }

    fn diff(&mut self, ui: &mut egui::Ui, id: usize) {
        if id != 37 {
            empty(ui, "Изменений в файлах нет.");
            return;
        }
        card(ui, |ui| {
            ui.monospace(SOURCE);
            ui.separator();
            ui.weak("@@ 12,1 → 12,1 @@");
            ui.label(
                egui::RichText::new("− title.contains(query)")
                    .monospace()
                    .color(ui.visuals().error_fg_color),
            );
            let mut open = self.line_note.is_some();
            if ui
                .selectable_label(
                    open,
                    egui::RichText::new("+ title.to_lowercase().contains(&query.to_lowercase())")
                        .monospace(),
                )
                .on_hover_text("Добавить замечание к строке 12")
                .clicked()
            {
                open = !open;
            }
            if open {
                let note = self.line_note.get_or_insert_with(String::new);
                ui.add(
                    TextArea::new(note)
                        .label("Замечание к строке 12")
                        .rows(2)
                        .width(ui.available_width()),
                );
                if ui
                    .add_enabled_ui(!note.trim().is_empty(), |ui| {
                        action(ui, "Добавить в следующий шаг")
                    })
                    .inner
                {
                    let annotation = format!("{SOURCE}:12 — {}", note.trim());
                    if let Some(task) = self.entry_mut(id) {
                        if !task.composer.is_empty() {
                            task.composer.push('\n');
                        }
                        task.composer.push_str(&annotation);
                    }
                    self.line_note = None;
                    self.page = Page::Chat;
                }
            } else {
                self.line_note = None;
            }
        });
    }
}

fn summary(ui: &mut egui::Ui, task: &Task) {
    card(ui, |ui| {
        ui.strong(format!("Попытка {}", task.attempts.len()));
        ui.label(match task.stage {
            Stage::Failed => "Исполнитель потерял соединение. Результат не получен.",
            Stage::Stopped => "Пользователь отклонил запрос. Попытка остановлена.",
            Stage::Draft | Stage::Permission | Stage::Question => "Результата пока нет.",
            Stage::Planned | Stage::Accepted | Stage::Review => task.summary(),
        });
        if task.id == 37 {
            ui.weak("Изменён 1 файл   /   Проверки 2 из 2");
        }
    });
}

fn checks(ui: &mut egui::Ui, task: &Task) {
    if task.stage == Stage::Failed {
        card(ui, |ui| {
            ui.strong("Агент недоступен");
            ui.label("Соединение прервалось до запуска сборки. Повторите попытку.");
        });
    } else if task.id == 37 {
        card(ui, |ui| {
            for check in ["Поиск по кириллице", "Пустой поисковый запрос"]
            {
                ui.horizontal_wrapped(|ui| {
                    ui.add(Badge::dot().severity(Severity::Success));
                    ui.label(check);
                    ui.weak("Пройдена");
                });
            }
            ui.separator();
            ui.monospace("cargo test -p hub-core search");
            ui.weak("2 passed, 0 failed");
        });
    } else {
        empty(ui, "Для этой попытки проверки не заданы.");
    }
}

fn files(ui: &mut egui::Ui, task: &Task) {
    if task.id != 37 {
        empty(ui, "Агент не приложил файлы к результату.");
        return;
    }
    card(ui, |ui| {
        let report = "test empty_query ... ok\ntest cyrillic_case ... ok";
        ui.horizontal(|ui| {
            ui.add(kylig_icons::Icon::new(kylig_icons::FILE_TEXT).size(18.0));
            ui.strong("search-tests.txt");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                copy_button(ui, report, "Скопировать отчёт");
            });
        });
        ui.weak("Отчёт о проверках");
        ui.separator();
        ui.add(egui::Label::new(egui::RichText::new(report).monospace()).wrap().selectable(true));
    });
}

fn history(ui: &mut egui::Ui, task: &mut Task) {
    let Some(latest) = task.attempts.len().checked_sub(1) else {
        empty(ui, "У задачи пока нет попыток.");
        return;
    };
    let mut selected = task.review_attempt.unwrap_or(latest);
    card(ui, |ui| {
        if latest > 0 {
            let options: Vec<_> = task
                .attempts
                .iter()
                .enumerate()
                .rev()
                .map(|(i, attempt)| {
                    SelectItem::new(i, format!("Попытка {} · {}", i + 1, attempt.agent))
                })
                .collect();
            ui.add(
                Select::new(&mut selected, &options)
                    .id_salt(("review.attempt", task.id))
                    .width(ui.available_width().min(300.0)),
            );
            task.review_attempt = (selected != latest).then_some(selected);
        } else if let Some(attempt) = task.attempts.first() {
            ui.strong(format!("Попытка 1 · {}", attempt.agent));
        }
        if let Some(attempt) = task.attempts.get(selected) {
            ui.add_space(6.0);
            ui.weak("Поручение");
            ui.add(egui::Label::new(&attempt.step).wrap().selectable(true));
            ui.separator();
            ui.weak("Итог попытки");
            ui.add(egui::Label::new(&attempt.outcome).wrap().selectable(true));
        }
    });
}

fn empty(ui: &mut egui::Ui, text: &str) {
    card(ui, |ui| {
        ui.weak(text);
    });
}
