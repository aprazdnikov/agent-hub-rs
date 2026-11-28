//! First-run setup submits the same validated settings draft as the editor.

use super::{
    Button, Command, Draft, Path, ProgressRing, SettingsForm, Snapshot, egui, navigation,
    path_field, pick_folder, telegram_fields, theme,
};
use crate::gui::look::agent_title;
use crate::supervisor::SaveStatus;
use kylig_ui::Segmented;

#[derive(PartialEq, Eq)]
enum Step {
    Mode,
    Configure,
    Saving,
    Ready,
}

pub(super) struct Wizard {
    step: Step,
    local: bool,
    candidate: Draft,
    error: Option<String>,
}

enum Navigation {
    Stay,
    Submit,
    Settings,
    Inbox,
}

impl Wizard {
    fn mode(&mut self, ui: &mut egui::Ui) -> Navigation {
        Segmented::new(&mut self.local, [(true, "На компьютере"), (false, "С Telegram")])
            .id_salt("setup-mode")
            .show(ui);
        ui.label(if self.local {
            "Задачи, проекты и память доступны в окне Agent Hub."
        } else {
            "Бот принимает задачи из тем группы и отправляет ответы агентов в Telegram."
        });
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::new("Продолжить").primary()).clicked() {
                if self.candidate.workspace_root.trim().is_empty() {
                    "~/Projects".clone_into(&mut self.candidate.workspace_root);
                }
                self.step = Step::Configure;
            }
            if ui.add(Button::new("Открыть настройки").ghost()).clicked() {
                Navigation::Settings
            } else {
                Navigation::Stay
            }
        })
        .inner
    }

    fn configure(&mut self, ui: &mut egui::Ui, home: &Path, saving: bool) -> Navigation {
        let draft = &mut self.candidate;
        path_field(ui, "Рабочий каталог", &mut draft.workspace_root, "~/Projects", pick_folder);
        ui.weak("Выберите существующую папку, в которой находятся ваши проекты.");
        navigation::agent_choice(ui, "Основной агент", &mut draft.default_backend);
        if !self.local {
            telegram_fields(ui, draft, &[]);
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.horizontal_wrapped(|ui| {
            let label = if self.error.is_some() {
                "Повторить настройку"
            } else {
                "Завершить настройку"
            };
            if ui.add_enabled(!saving, Button::new(label).primary()).clicked() {
                let mut candidate = self.candidate.clone();
                if self.local {
                    candidate.token.clear();
                    candidate.chat.clear();
                    candidate.users.clear();
                }
                if !self.local && candidate.token.trim().is_empty() {
                    self.error = Some("Укажите токен бота для подключения Telegram.".into());
                } else if let Err(errors) = candidate.parse(home) {
                    self.error = errors.into_iter().next().map(|error| error.message);
                } else {
                    self.candidate = candidate;
                    self.step = Step::Saving;
                    return Navigation::Submit;
                }
            }
            if ui.add(Button::new("Назад")).clicked() {
                self.step = Step::Mode;
                self.error = None;
            }
            Navigation::Stay
        })
        .inner
    }

    fn poll_save(&mut self, snapshot: &Snapshot, saving: bool) {
        if self.step == Step::Saving {
            if snapshot.saved.as_ref() == Some(&self.candidate) {
                self.step = Step::Ready;
            } else if !saving {
                self.step = Step::Configure;
                self.error = Some(snapshot.notice.clone().unwrap_or_else(|| {
                    "Сохранить настройки не удалось. Повторите попытку.".into()
                }));
            }
        }
    }

    fn draw(
        &mut self,
        ui: &mut egui::Ui,
        home: &Path,
        snapshot: &Snapshot,
        saving: bool,
    ) -> Navigation {
        match self.step {
            Step::Mode => self.mode(ui),
            Step::Configure => self.configure(ui, home, saving),
            Step::Saving if snapshot.save_status == SaveStatus::Uncertain => {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    "Результат сохранения неизвестен. Проверьте настройки после перезапуска.",
                );
                if ui.add(Button::new("Открыть настройки")).clicked() {
                    Navigation::Settings
                } else {
                    Navigation::Stay
                }
            }
            Step::Saving => {
                ui.horizontal(|ui| {
                    ui.add(ProgressRing::indeterminate().size(20.0));
                    ui.label("Сохраняем настройки…");
                });
                Navigation::Stay
            }
            Step::Ready => {
                ui.strong(if self.local {
                    "Локальный режим готов"
                } else {
                    "Настройки Telegram сохранены"
                });
                ui.label(format!(
                    "{}  •  {}",
                    self.candidate.workspace_root,
                    agent_title(self.candidate.default_backend)
                ));
                if ui.add(Button::new("Открыть входящие").primary()).clicked() {
                    Navigation::Inbox
                } else {
                    Navigation::Stay
                }
            }
        }
    }
}

impl SettingsForm {
    pub(in crate::gui) fn open_setup(&mut self) {
        if self.setup.is_some() || self.synced.is_some() {
            return;
        }
        self.setup = Some(Wizard {
            step: Step::Mode,
            local: [&self.draft.token, &self.draft.chat, &self.draft.users]
                .iter()
                .all(|s| s.trim().is_empty()),
            candidate: self.draft.clone(),
            error: None,
        });
    }

    pub(in crate::gui) fn take_setup_complete(&mut self) -> bool {
        std::mem::take(&mut self.setup_complete)
    }

    pub(super) fn setup_page(
        &mut self,
        ui: &mut egui::Ui,
        home: &Path,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        let saving = self.save_progress == super::SaveProgress::Saving;
        let Some(setup) = self.setup.as_mut() else { return };
        let action = egui::ScrollArea::vertical()
            .id_salt("first-run")
            .show(ui, |ui| {
                setup.poll_save(snapshot, saving);
                ui.heading("Первый запуск");
                ui.weak(match setup.step {
                    Step::Mode => "Выберите, где работать с агентами.",
                    Step::Configure | Step::Saving => {
                        "Укажите рабочий каталог и настройки подключения."
                    }
                    Step::Ready => "Рабочее пространство настроено.",
                });
                ui.add_space(12.0);
                theme::card(ui)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        setup.draw(ui, home, snapshot, saving)
                    })
                    .inner
            })
            .inner;
        match action {
            Navigation::Submit => {
                self.draft = setup.candidate.clone();
                self.submitted = Some(self.draft.clone());
                send(Command::Save(Box::new(self.draft.clone())));
            }
            Navigation::Inbox => {
                self.setup_complete = true;
                self.setup = None;
            }
            Navigation::Settings => self.setup = None,
            Navigation::Stay => {}
        }
    }
}
