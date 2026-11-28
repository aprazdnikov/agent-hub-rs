//! Local simulations of the controls applied outside the settings draft.

use eframe::egui;
use kylig_ui::Switch;

use super::choice;
use crate::widgets::{Wait, action, card, dialog, primary};

#[derive(Default, PartialEq, Eq)]
enum Update {
    #[default]
    Idle,
    Available,
    Installed,
    Current,
    Failed,
}

#[derive(Clone, Copy)]
enum Operation {
    Check,
    Install,
    Restart,
    Inspect,
    Autostart,
}

pub(super) struct Application {
    zoom: u16,
    autostart: bool,
    registration: &'static str,
    update: Update,
    fixture: &'static str,
    operation: Option<Operation>,
    wait: Wait,
    confirming: bool,
}

impl Default for Application {
    fn default() -> Self {
        Self {
            zoom: 100,
            autostart: false,
            registration: "Выключен",
            update: Update::Idle,
            fixture: "Штатная работа",
            operation: None,
            wait: Wait::default(),
            confirming: false,
        }
    }
}

impl Application {
    fn start(&mut self, ui: &egui::Ui, operation: Operation) {
        self.operation = Some(operation);
        let label = match operation {
            Operation::Check => "Проверяем обновления…",
            Operation::Install => "Устанавливаем обновление…",
            Operation::Restart => "Перезапускаем макет…",
            Operation::Inspect => "Проверяем регистрацию…",
            Operation::Autostart => "Меняем регистрацию…",
        };
        self.wait.start(ui, label);
    }

    fn finish(&mut self, operation: Operation) {
        let failed = self.fixture == "Ошибка операции";
        self.fixture = "Штатная работа";
        match operation {
            Operation::Check | Operation::Install if failed => self.update = Update::Failed,
            Operation::Check => self.update = Update::Available,
            Operation::Install => self.update = Update::Installed,
            Operation::Restart => self.update = Update::Current,
            Operation::Autostart | Operation::Inspect => {
                if failed {
                    self.registration = "Ошибка обращения к ОС. Повторите проверку.";
                } else {
                    if matches!(operation, Operation::Autostart) {
                        self.autostart = !self.autostart;
                    }
                    self.registration =
                        if self.autostart { "Включён" } else { "Выключен" };
                }
            }
        }
    }

    fn poll(&mut self, ui: &mut egui::Ui) {
        if self.wait.draw(ui)
            && let Some(operation) = self.operation.take()
        {
            self.finish(operation);
        }
    }

    pub(super) fn tray_update(&mut self, ui: &mut egui::Ui) -> bool {
        self.poll(ui);
        if self.wait.busy() {
            return false;
        }
        match self.update {
            Update::Idle => {
                ui.weak("Обновления ещё не проверялись.");
                false
            }
            Update::Current => {
                ui.weak("Установлена последняя версия.");
                false
            }
            Update::Available => action(ui, "Доступно обновление"),
            Update::Installed => action(ui, "Обновление готово к перезапуску"),
            Update::Failed => action(ui, "Ошибка проверки обновлений"),
        }
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui, updates: &mut bool) {
        self.poll(ui);
        card(ui, |ui| {
            ui.strong("Оформление");
            ui.weak(if ui.visuals().dark_mode {
                "Тёмная тема"
            } else {
                "Светлая тема"
            });
            ui.label("Масштаб интерфейса");
            if ui
                .add(egui::Slider::new(&mut self.zoom, 75..=150).step_by(5.0).suffix("%"))
                .changed()
            {
                ui.ctx().set_zoom_factor(f32::from(self.zoom) / 100.0);
            }
            if action(ui, "Сбросить до 100%") {
                self.zoom = 100;
                ui.ctx().set_zoom_factor(1.0);
            }
        });
        ui.add_enabled_ui(!self.wait.busy(), |ui| {
            card(ui, |ui| {
                ui.strong("Автозапуск приложения");
                ui.label(self.registration);
                ui.horizontal_wrapped(|ui| {
                    if action(ui, if self.autostart { "Выключить" } else { "Включить" })
                    {
                        self.start(ui, Operation::Autostart);
                    }
                    if action(ui, "Проверить состояние") {
                        self.start(ui, Operation::Inspect);
                    }
                });
            });
            card(ui, |ui| {
                ui.strong("Обновления");
                ui.add(Switch::new(updates).label("Автоматически проверять обновления"));
                let (message, button, operation) = match self.update {
                    Update::Idle => {
                        ("Обновления ещё не проверялись.", "Проверить обновления", Operation::Check)
                    }
                    Update::Available => {
                        ("Доступна новая версия макета.", "Установить", Operation::Install)
                    }
                    Update::Installed => (
                        "Обновление установлено. Требуется перезапуск.",
                        "Перезапустить",
                        Operation::Restart,
                    ),
                    Update::Current => {
                        ("Установлена последняя версия.", "Проверить снова", Operation::Check)
                    }
                    Update::Failed => {
                        ("Сервер обновлений недоступен.", "Повторить проверку", Operation::Check)
                    }
                };
                ui.label(message);
                if action(ui, button) {
                    if matches!(operation, Operation::Restart) {
                        self.confirming = true;
                    } else {
                        self.start(ui, operation);
                    }
                }
            });
            choice(
                ui,
                "Следующая системная операция",
                &mut self.fixture,
                &["Штатная работа", "Ошибка операции"],
            );
        });
        if self.confirming {
            let mut open = true;
            let result =
                dialog(ui, "confirm-update-restart", &mut open, "Перезапустить приложение?")
                    .show(ui.ctx(), |ui| {
                        ui.label("Перезапуск прервёт активные сессии.");
                        ui.horizontal_wrapped(|ui| {
                            if primary(ui, "Перезапустить") {
                                Some(true)
                            } else if action(ui, "Отмена") {
                                Some(false)
                            } else {
                                None
                            }
                        })
                        .inner
                    })
                    .flatten();
            if result == Some(true) {
                self.start(ui, Operation::Restart);
            }
            self.confirming = open && result.is_none();
        }
    }
}
