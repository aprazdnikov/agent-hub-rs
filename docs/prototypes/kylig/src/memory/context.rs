use super::{Browser, Job, Store, Tab, action, card, egui, primary};
use kylig_ui::{TextArea, TextField};
use std::{collections::BTreeMap, fmt::Write as _};

const PROCEDURE: &str = include_str!("../../../../guide/procedures/continue-work-v1.md");

pub(super) struct ContextDraft {
    pub selected: BTreeMap<String, u32>,
    pub preview: Option<String>,
    procedure_open: bool,
    goal: String,
    constraints: String,
    state: String,
    next: String,
    sources: String,
    budget: usize,
}

impl Default for ContextDraft {
    fn default() -> Self {
        Self {
            selected: BTreeMap::new(),
            preview: None,
            procedure_open: false,
            goal: "Подготовить выпуск проекта".into(),
            constraints: "Сохранить совместимость настроек.".into(),
            state: "Сборка готова к проверке.".into(),
            next: "Выполнить проверку установки.".into(),
            sources: "Задача о выпуске, тема #42".into(),
            budget: 16384,
        }
    }
}

impl ContextDraft {
    pub fn packet(&self, store: &Store, project: &str) -> Result<String, String> {
        if !(1..=20).contains(&self.selected.len()) {
            return Err("Выберите от 1 до 20 ревизий из списка записей.".into());
        }
        if [&self.goal, &self.constraints, &self.state, &self.next]
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 4096)
        {
            return Err("Заполните цель, ограничения, состояние и следующий шаг. Каждое поле вмещает до 4096 байт.".into());
        }
        let sources: Vec<_> =
            self.sources.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
        if !(1..=8).contains(&sources.len()) || sources.iter().any(|s| s.len() > 2048) {
            return Err(
                "Укажите от 1 до 8 источников, по одному на строке, до 2048 байт каждый.".into()
            );
        }
        let mut text = format!(
            "Поручение · {project}\nЦель — {}\nОграничения — {}\nСостояние — {}\nСледующий шаг — {}\nИсточники — {}\n",
            self.goal.trim(),
            self.constraints.trim(),
            self.state.trim(),
            self.next.trim(),
            sources.join("\n")
        );
        for (key, revision) in &self.selected {
            let entry = store
                .entry(key)
                .ok_or_else(|| format!("Запись {key} удалена. Исключите её из пакета."))?;
            let body = entry
                .revisions
                .get(revision)
                .ok_or_else(|| format!("Ревизия {key} · r{revision} удалена. Выберите другую."))?;
            let _ = write!(
                text,
                "\n{} · {key}@{revision} · {}\n{body}\nИсточник — {}\n",
                entry.title,
                entry.status.label(),
                entry.source
            );
        }
        if text.len() > self.budget {
            return Err(format!(
                "Пакету нужно {} байт. Увеличьте бюджет или уберите запись.",
                text.len()
            ));
        }
        Ok(text)
    }
}

impl Browser {
    pub(super) fn context_panel(&mut self, ui: &mut egui::Ui, store: &Store, project: &str) {
        card(ui, |ui| {
            ui.strong("Ревизии для следующего агента");
            if self.context.selected.is_empty() {
                ui.label("Откройте запись и добавьте нужную ревизию.");
                if action(ui, "Выбрать записи") {
                    self.tab = Tab::Records;
                    self.detail = None;
                }
            }
            let mut remove = None;
            for (key, revision) in &self.context.selected {
                ui.horizontal_wrapped(|ui| {
                    ui.monospace(format!("{key} · r{revision}"));
                    if ui.small_button("Убрать").clicked() {
                        remove = Some(key.clone());
                    }
                });
            }
            if let Some(key) = remove {
                self.context.selected.remove(&key);
                self.context.preview = None;
            }
        });
        ui.strong("Поручение");
        for (label, value) in [
            ("Цель", &mut self.context.goal),
            ("Ограничения", &mut self.context.constraints),
            ("Состояние", &mut self.context.state),
            ("Следующий шаг", &mut self.context.next),
        ] {
            let width = ui.available_width().min(620.0);
            if ui.add(TextField::new(value).label(label).id_salt(label).width(width)).changed() {
                self.context.preview = None;
            }
        }
        if ui
            .add(
                TextArea::new(&mut self.context.sources)
                    .label("Источники поручения")
                    .placeholder("По одному источнику на строке")
                    .rows(2)
                    .width(ui.available_width().min(620.0)),
            )
            .changed()
        {
            self.context.preview = None;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Бюджет, байт");
            if ui.add(egui::DragValue::new(&mut self.context.budget).range(1024..=65536)).changed()
            {
                self.context.preview = None;
            }
            if primary(ui, "Собрать пакет") {
                self.start(ui, Job::Context, "Собираем пакет контекста…");
            }
        });
        if let Some(packet) = &self.context.preview {
            let valid = self.context.packet(store, project).as_ref() == Ok(packet);
            card(ui, |ui| {
                ui.strong("Предпросмотр пакета");
                ui.weak(format!(
                    "{} байт · ревизий — {}",
                    packet.len(),
                    self.context.selected.len()
                ));
                ui.label(packet);
            });
            if valid {
                if primary(ui, "Проверить и скопировать") {
                    self.start(ui, Job::CopyContext, "Проверяем свежесть пакета…");
                }
            } else {
                ui.label("Записи изменились. Соберите пакет заново.");
            }
        }
        self.procedure(ui);
    }

    fn procedure(&mut self, ui: &mut egui::Ui) {
        if action(ui, "Открыть процедуру продолжения работы") {
            self.context.procedure_open = true;
        }
        if self.context.procedure_open {
            let rect = ui.ctx().content_rect();
            let mut open = true;
            crate::widgets::dialog(ui, "memory-procedure", &mut open, "Продолжение работы").show(
                ui.ctx(),
                |ui| {
                    egui::ScrollArea::vertical()
                        .max_height((rect.height() - 200.0).max(60.0))
                        .show(ui, |ui| {
                            ui.label(PROCEDURE);
                        });
                    ui.horizontal_wrapped(|ui| {
                        if primary(ui, "Скопировать процедуру") {
                            ui.ctx().copy_text(PROCEDURE.into());
                            self.notice = Some((
                                kylig_ui::Severity::Success,
                                "Процедура скопирована.".into(),
                            ));
                            self.context.procedure_open = false;
                        }
                        if action(ui, "Закрыть") {
                            self.context.procedure_open = false;
                        }
                    });
                },
            );
            if !open {
                self.context.procedure_open = false;
            }
        }
    }
}
