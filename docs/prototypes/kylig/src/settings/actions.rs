//! Edit reusable commands and try the Telegram menu using the current draft.
use eframe::egui;
use kylig_ui::{Button, Chip, ListItem, Segmented, TextArea};

use super::{field, profiles::Profiles};
use crate::widgets::{Wait, action, card, remaining_height};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Action {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) command: String,
    cwd: String,
}

impl Action {
    pub(super) fn new(id: u64, name: &str, command: &str, cwd: &str) -> Self {
        Self { id, name: name.into(), command: command.into(), cwd: cwd.into() }
    }

    pub(super) fn complete(&self) -> bool {
        !self.name.trim().is_empty() && !self.command.trim().is_empty()
    }

    fn title(&self) -> &str {
        if self.name.trim().is_empty() { "Новое действие" } else { self.name.trim() }
    }

    fn directory(&self) -> &str {
        if self.cwd.trim().is_empty() { "~" } else { self.cwd.trim() }
    }
}

#[derive(Default, PartialEq, Eq, Clone, Copy)]
enum Tab {
    #[default]
    Edit,
    Menu,
}

#[derive(Default)]
pub(super) struct Editor {
    tab: Tab,
    selected: Option<u64>,
    // A run reflects the command as it appeared when the button was pressed.
    preview: Option<Action>,
    wait: Wait,
    stopped: bool,
}

impl Editor {
    pub(super) fn select(&mut self, id: u64) {
        self.tab = Tab::Edit;
        self.selected = Some(id);
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui, profiles: &mut Profiles, sequence: &mut u64) {
        ui.horizontal_wrapped(|ui| {
            Segmented::new(&mut self.tab, [(Tab::Edit, "Команды"), (Tab::Menu, "Меню /actions")])
                .id_salt("action-views")
                .show(ui);
            if self.tab == Tab::Edit && action(ui, "Добавить действие") {
                *sequence += 1;
                profiles.actions.push(Action::new(*sequence, "", "", ""));
                self.select(*sequence);
            }
        });
        ui.add_space(6.0);
        if self.tab == Tab::Menu {
            self.menu(ui, &profiles.actions);
            return;
        }
        let Some(first) = profiles.actions.first() else {
            ui.weak("Пока нет действий. Добавьте первую команду.");
            self.selected = None;
            return;
        };
        if !profiles.actions.iter().any(|a| Some(a.id) == self.selected) {
            self.selected = None;
        }
        let wide = ui.available_width() >= 680.0;
        if wide {
            self.selected.get_or_insert(first.id);
            let list_width = (ui.available_width() * 0.32).min(320.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(list_width);
                    egui::ScrollArea::vertical()
                        .id_salt("action-list")
                        .max_height(remaining_height(ui).max(80.0))
                        .show(ui, |ui| self.list(ui, &profiles.actions));
                });
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    self.detail(ui, profiles);
                });
            });
        } else if self.selected.is_some() {
            if ui.link("‹ Все команды").clicked() {
                self.selected = None;
            }
            self.detail(ui, profiles);
        } else {
            self.list(ui, &profiles.actions);
        }
    }

    fn list(&mut self, ui: &mut egui::Ui, actions: &[Action]) {
        for item in actions {
            let command =
                item.command.lines().next().filter(|s| !s.is_empty()).unwrap_or("Введите команду");
            if ListItem::new(item.title())
                .id_salt(item.id)
                .icon(kylig_icons::TERMINAL_WINDOW)
                .subtitle(command)
                .selected(self.selected == Some(item.id))
                .chevron(true)
                .show(ui)
                .response
                .clicked()
            {
                self.select(item.id);
            }
        }
    }

    fn detail(&mut self, ui: &mut egui::Ui, profiles: &mut Profiles) {
        let Some(item) = profiles.actions.iter_mut().find(|a| Some(a.id) == self.selected) else {
            return;
        };
        let id = item.id;
        let remove = ui
            .push_id(id, |ui| {
                card(ui, |ui| {
                    field(ui, "Название действия", &mut item.name, "Название кнопки в Telegram");
                    ui.add(
                        TextArea::new(&mut item.command)
                            .label("Команда")
                            .id_salt("command")
                            .rows(3)
                            .width(ui.available_width()),
                    );
                    field(ui, "Рабочий каталог", &mut item.cwd, "Домашний каталог");
                    ui.strong("Связанные проекты");
                    ui.horizontal_wrapped(|ui| {
                        for project in &mut profiles.projects {
                            let linked = project.actions.contains(&id);
                            if Chip::new(project.title())
                                .selected(linked)
                                .show(ui)
                                .response
                                .clicked()
                            {
                                if linked {
                                    project.actions.retain(|a| *a != id);
                                } else {
                                    project.actions.push(id);
                                }
                            }
                        }
                    });
                    ui.separator();
                    ui.add(Button::new("Убрать действие").ghost()).clicked()
                })
            })
            .inner;
        if remove {
            profiles.actions.retain(|a| a.id != id);
            for project in &mut profiles.projects {
                project.actions.retain(|a| *a != id);
            }
            self.selected = None;
        }
    }

    fn menu(&mut self, ui: &mut egui::Ui, actions: &[Action]) {
        card(ui, |ui| {
            ui.heading("Предпросмотр Telegram");
            ui.weak("Кнопки из текущих настроек. Запуск и остановка моделируются в макете.");
            if actions.is_empty() {
                ui.label("В меню пока нет команд.");
            }
            for item in actions {
                let running =
                    self.wait.busy() && self.preview.as_ref().is_some_and(|p| p.id == item.id);
                let button = if running {
                    Button::new(format!("Остановить · {}", item.title())).icon(kylig_icons::STOP)
                } else {
                    Button::new(item.title()).icon(kylig_icons::PLAY)
                };
                if ui
                    .add_enabled(running || (item.complete() && !self.wait.busy()), button)
                    .clicked()
                {
                    if running {
                        self.stopped = true;
                        self.wait = Wait::default();
                    } else {
                        self.preview = Some(item.clone());
                        self.stopped = false;
                        self.wait.start(ui, "Выполняется демозапуск…");
                    }
                }
            }
            if let Some(run) = &self.preview {
                ui.separator();
                self.wait.draw(ui);
                if !self.wait.busy() {
                    ui.strong(if self.stopped {
                        "Демозапуск остановлен"
                    } else {
                        "Демозапуск завершён"
                    });
                }
                ui.label(run.title());
                ui.weak(format!("Рабочий каталог · {}", run.directory()));
                ui.monospace(&run.command);
            }
        });
    }
}
