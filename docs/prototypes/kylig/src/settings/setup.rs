use eframe::egui;
use kylig_ui::Segmented;

use super::{Telegram, choice, field};
use crate::widgets::{Wait, action, agent_choice, card, primary, title};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Step {
    #[default]
    Mode,
    Configure,
    Ready,
}

pub(super) struct Setup {
    step: Step,
    local: bool,
    pub(super) root: String,
    pub(super) agent: &'static str,
    pub(super) telegram: Telegram,
    wait: Wait,
    network: &'static str,
    error: Option<&'static str>,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            step: Step::Mode,
            local: true,
            root: "~/Projects".into(),
            agent: "Claude Code",
            telegram: Telegram::default(),
            wait: Wait::default(),
            network: "Доступна",
            error: None,
        }
    }
}

impl Setup {
    pub(super) fn show(&mut self, ui: &mut egui::Ui) -> bool {
        title(
            ui,
            "Первый запуск",
            match self.step {
                Step::Mode => "Выберите, где работать с агентами.",
                Step::Configure => "Укажите рабочий каталог и настройки подключения.",
                Step::Ready => "Рабочее пространство настроено.",
            },
        );
        card(ui, |ui| match self.step {
            Step::Mode => {
                Segmented::new(&mut self.local, [(true, "На компьютере"), (false, "С Telegram")])
                    .show(ui);
                ui.label(if self.local {
                    "Задачи, проекты и память доступны в окне Agent Hub."
                } else {
                    "Бот принимает задачи из тем группы и отправляет ответы агентов в Telegram."
                });
                if primary(ui, "Продолжить") {
                    self.step = Step::Configure;
                }
                false
            }
            Step::Configure => {
                if self.wait.draw(ui) {
                    if !self.local && self.network == "Недоступна" {
                        self.error =
                            Some("Истекло время ожидания Telegram. Повторите подключение.");
                        self.network = "Доступна";
                    } else {
                        self.step = Step::Ready;
                        self.error = None;
                    }
                }
                ui.add_enabled_ui(!self.wait.busy(), |ui| {
                    field(ui, "Рабочий каталог", &mut self.root, "~/Projects");
                    agent_choice(ui, "Основной агент", &mut self.agent);
                    if !self.local {
                        self.telegram.fields(ui);
                        if action(ui, "Подставить демоданные") {
                            self.telegram = Telegram::fixture();
                        }
                        choice(
                            ui,
                            "Доступность сети в макете",
                            &mut self.network,
                            &["Доступна", "Недоступна"],
                        );
                    }
                    if let Some(error) = self.error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    }
                    ui.horizontal_wrapped(|ui| {
                        if primary(
                            ui,
                            if self.error.is_some() {
                                "Повторить настройку"
                            } else {
                                "Завершить настройку"
                            },
                        ) {
                            self.error = if self.root.trim().is_empty() {
                                Some("Укажите рабочий каталог.")
                            } else if !self.local && self.telegram.local() {
                                Some("Заполните параметры Telegram или подставьте демоданные.")
                            } else if self.local {
                                None
                            } else {
                                self.telegram.issue()
                            };
                            if self.error.is_none() {
                                if self.local {
                                    self.telegram = Telegram::default();
                                }
                                self.wait.start(ui, "Проверяем настройки рабочего пространства…");
                            }
                        }
                        if action(ui, "Назад") {
                            self.step = Step::Mode;
                            self.error = None;
                        }
                    });
                });
                false
            }
            Step::Ready => {
                ui.strong(if self.local {
                    "Локальный режим готов"
                } else {
                    "Бот подключён"
                });
                ui.label(format!("{}  •  {}", self.root, self.agent));
                primary(ui, "Открыть входящие")
            }
        })
    }
}
