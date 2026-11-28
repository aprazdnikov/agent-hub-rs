use super::{Effect, Page, Project, Stage, Task, Tasks};
use crate::widgets::{action, card, primary, title};
use eframe::egui;
use kylig_ui::{Badge, Select, SelectItem, TextField};

impl Tasks {
    pub(crate) fn inbox(&mut self, ui: &mut egui::Ui) -> bool {
        title(ui, "Входящие", "Вопросы агентов и результаты, которые ждут вашего решения.");
        self.feedback(ui);
        let mut navigate = false;
        ui.add_enabled_ui(!self.wait.busy(), |ui| {
            let projects = std::iter::once(SelectItem::new(None, "Все проекты"))
                .chain(Project::ALL.map(|p| SelectItem::new(Some(p), p.name())))
                .collect::<Vec<_>>();
            ui.add(
                Select::new(&mut self.inbox.project, &projects)
                    .id_salt("inbox.project")
                    .width(190.0),
            );
            let (requests, results): (Vec<_>, Vec<_>) = self
                .entries
                .iter()
                .filter(|task| {
                    task.attention() && self.inbox.project.is_none_or(|p| p == task.project)
                })
                .map(|task| (task.id, task.stage))
                .partition(|(_, stage)| stage.waiting());
            if requests.is_empty() && results.is_empty() {
                ui.add_space(16.0);
                card(ui, |ui| {
                    ui.heading("Входящие разобраны");
                    ui.label("Здесь появятся новые вопросы и готовые результаты.");
                    if action(ui, "К задачам проекта") {
                        if let Some(project) = self.inbox.project {
                            self.select_project(project);
                        }
                        navigate = true;
                    }
                });
                return;
            }
            if !requests.is_empty() {
                section(ui, "Ждут вашего ответа", requests.len());
                if let [(left, _), (right, _)] = requests.as_slice()
                    && ui.available_width() >= 780.0
                {
                    ui.columns(2, |columns| {
                        if let [left_ui, right_ui] = columns {
                            self.request_card(left_ui, *left);
                            self.request_card(right_ui, *right);
                        }
                    });
                } else {
                    for (id, _) in requests {
                        self.request_card(ui, id);
                    }
                }
            }
            if !results.is_empty() {
                section(ui, "Результаты и ошибки", results.len());
                for (id, _) in results {
                    if let Some(task) = self.entry(id)
                        && result_card(ui, task)
                    {
                        let failed = task.stage == Stage::Failed;
                        self.open(id, Page::Review, true);
                        if failed {
                            self.review_tab = 2;
                        }
                        navigate = true;
                    }
                }
            }
        });
        navigate
    }

    pub(super) fn request_card(&mut self, ui: &mut egui::Ui, id: usize) {
        ui.push_id(("request", id), |ui| {
            card(ui, |ui| {
                let Some(task) = self.entry(id) else { return };
                header(ui, task);
                ui.add_space(6.0);
                if let Some(request) = task.stage.request() {
                    ui.label(request);
                }
                self.reply(ui, id);
            });
        });
    }

    pub(super) fn reply(&mut self, ui: &mut egui::Ui, id: usize) {
        let Some(task) = self.entry(id) else { return };
        let decision = match task.stage {
            Stage::Question => question(ui, &mut self.inbox.answer),
            Stage::Permission => permission(ui, &mut self.inbox.permission_details),
            Stage::Accepted
            | Stage::Planned
            | Stage::Draft
            | Stage::Failed
            | Stage::Review
            | Stage::Stopped => None,
        };
        if let Some((reply, proceed)) = decision {
            self.start(
                ui,
                "Передаём решение агенту…",
                Effect::Decision { id, reply, proceed },
                false,
            );
        }
    }
}

fn section(ui: &mut egui::Ui, text: &str, count: usize) {
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(text).size(18.0).strong());
        ui.weak(count.to_string());
    });
    ui.add_space(4.0);
}

fn header(ui: &mut egui::Ui, task: &Task) {
    ui.horizontal_wrapped(|ui| {
        ui.add(Badge::dot().severity(task.stage.severity()));
        ui.small(task.stage.label());
        ui.weak(format!("#{}   {} / {}", task.id, task.project.name(), task.agent));
    });
    ui.label(egui::RichText::new(&task.name).size(18.0).strong());
}

fn question(ui: &mut egui::Ui, answer: &mut String) -> Option<(String, bool)> {
    ui.horizontal_wrapped(|ui| {
        for option in ["Все задачи", "В очереди", "Завершённые"] {
            if ui.selectable_label(answer == option, option).clicked() {
                option.clone_into(answer);
            }
        }
    });
    ui.add(
        TextField::new(answer)
            .id_salt("reply")
            .placeholder("Ответ или свой вариант")
            .width(ui.available_width()),
    );
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled_ui(!answer.trim().is_empty(), |ui| primary(ui, "Отправить ответ")).inner
        {
            return Some((answer.trim().to_owned(), true));
        }
        action(ui, "Отказаться").then(|| ("Отказываюсь отвечать.".to_owned(), false))
    })
    .inner
}

fn permission(ui: &mut egui::Ui, expanded: &mut bool) -> Option<(String, bool)> {
    ui.monospace("npm run check:links");
    if ui
        .link(if *expanded {
            "Скрыть подробности"
        } else {
            "Подробнее о запросе"
        })
        .clicked()
    {
        *expanded = !*expanded;
    }
    if *expanded {
        ui.label("Агент запрашивает разрешение на один запуск команды в каталоге docs-site.");
    }
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        if primary(ui, "Разрешить один раз") {
            return Some(("Проверка ссылок разрешена.".to_owned(), true));
        }
        action(ui, "Отклонить").then(|| ("Запуск команды отклонён.".to_owned(), false))
    })
    .inner
}

fn result_card(ui: &mut egui::Ui, task: &Task) -> bool {
    let mut open = false;
    ui.push_id(task.id, |ui| {
        card(ui, |ui| {
            header(ui, task);
            let failed = task.stage == Stage::Failed;
            ui.label(if failed {
                "Исполнитель потерял соединение до начала сборки."
            } else {
                task.summary()
            });
            if task.id == 37 {
                ui.weak("2 проверки пройдены   /   1 файл с изменениями");
            }
            ui.add_space(4.0);
            open = if failed {
                action(ui, "Разобрать запуск")
            } else {
                primary(ui, "Проверить результат")
            };
        });
    });
    open
}
