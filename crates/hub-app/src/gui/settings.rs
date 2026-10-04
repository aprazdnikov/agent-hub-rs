//! The «Настройки» tab: a draft of every setting, saved only when it parses.

use std::path::{Path, PathBuf};

use eframe::egui;
use hub_core::domain::BackendKind;
use hub_core::settings::{
    Approval, CodexField, Draft, Field, FieldError, PermissionMode, Sandbox, UpdateCheck,
};

use crate::gui::log::LogAction;
use crate::gui::look::{add_user, telegram_changed, use_chat};
use crate::supervisor::Command;

#[derive(Default)]
pub struct SettingsForm {
    draft: Draft,
    synced: Option<Draft>,
    reveal: bool,
    reveal_key: bool,
    confirming: bool,
}

fn errors_for(errors: &[FieldError], field: Field) -> impl Iterator<Item = &str> {
    errors.iter().filter(move |error| error.field == field).map(|error| error.message.as_str())
}

fn messages(ui: &mut egui::Ui, errors: &[FieldError], field: Field) {
    for message in errors_for(errors, field) {
        ui.colored_label(ui.visuals().error_fg_color, message);
    }
}

fn pick_folder(start: &str) -> Option<String> {
    let dialog = rfd::FileDialog::new();
    let dialog = if start.is_empty() { dialog } else { dialog.set_directory(start) };
    dialog.pick_folder().map(|path| path.display().to_string())
}

fn pick_file(start: &str) -> Option<String> {
    let dialog = rfd::FileDialog::new();
    let start = Path::new(start).parent().map(Path::to_path_buf).unwrap_or_default();
    let dialog = if start.as_os_str().is_empty() { dialog } else { dialog.set_directory(start) };
    dialog.pick_file().map(|path| path.display().to_string())
}

impl SettingsForm {
    /// Adopts newly saved settings unless the user is in the middle of editing.
    pub fn sync(&mut self, saved: Option<&Draft>) {
        if self.synced.as_ref() == saved {
            return;
        }
        let pristine = self
            .synced
            .as_ref()
            .map_or(self.draft == Draft::default(), |synced| *synced == self.draft);
        if pristine {
            self.draft = saved.cloned().unwrap_or_default();
        }
        self.synced = saved.cloned();
    }

    pub fn apply(&mut self, action: LogAction) {
        match action {
            LogAction::UseChat(chat) => use_chat(&mut self.draft, chat),
            LogAction::AddUser(user) => add_user(&mut self.draft, user),
        }
    }

    fn errors(&self, home: &Path) -> Vec<FieldError> {
        match self.draft.parse(home) {
            Ok(settings) if settings.workspace_root.is_dir() => Vec::new(),
            Ok(_) => vec![FieldError {
                field: Field::WorkspaceRoot,
                message: "Такого каталога нет".to_owned(),
            }],
            Err(errors) => errors,
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        home: &Path,
        sessions: usize,
        send: &mut impl FnMut(Command),
    ) {
        let errors = self.errors(home);
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.telegram(ui, &errors);
            ui.separator();
            self.workspace(ui, &errors);
            ui.separator();
            self.agent(ui, &errors);
            ui.separator();
            self.codex(ui, &errors);
            ui.separator();
            self.timeouts(ui, &errors);
        });
        ui.separator();
        let changed = self.synced.as_ref().is_none_or(|synced| *synced != self.draft);
        ui.horizontal(|ui| {
            let save = ui.add_enabled(errors.is_empty() && changed, egui::Button::new("Сохранить"));
            if save.clicked() {
                if telegram_changed(self.synced.as_ref(), &self.draft) && sessions > 0 {
                    self.confirming = true;
                } else {
                    send(Command::Save(Box::new(self.draft.clone())));
                }
            }
            if ui.add_enabled(changed, egui::Button::new("Отменить изменения")).clicked()
            {
                self.draft = self.synced.clone().unwrap_or_default();
            }
        });
        self.confirm(ui.ctx(), sessions, send);
    }

    fn telegram(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Telegram");
        ui.label("Изменение этих полей перезапускает бота и прерывает работающие сессии.");
        ui.horizontal(|ui| {
            ui.label("Токен бота");
            ui.add(egui::TextEdit::singleline(&mut self.draft.token).password(!self.reveal));
            ui.checkbox(&mut self.reveal, "показать");
        });
        messages(ui, errors, Field::Token);
        ui.horizontal(|ui| {
            ui.label("chat_id группы");
            ui.text_edit_singleline(&mut self.draft.chat);
        });
        messages(ui, errors, Field::Chat);
        ui.horizontal(|ui| {
            ui.label("Разрешённые user_id");
            ui.add(egui::TextEdit::singleline(&mut self.draft.users).hint_text("111, 222"));
        });
        messages(ui, errors, Field::Users);
    }

    fn workspace(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Рабочие каталоги");
        ui.horizontal(|ui| {
            ui.label("Корень");
            ui.text_edit_singleline(&mut self.draft.workspace_root);
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_folder(&self.draft.workspace_root)
            {
                self.draft.workspace_root = path;
            }
        });
        messages(ui, errors, Field::WorkspaceRoot);
    }

    fn agent(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Агенты");
        ui.horizontal(|ui| {
            ui.label("Агент по умолчанию");
            egui::ComboBox::from_id_salt("default_backend")
                .selected_text(self.draft.default_backend.name())
                .show_ui(ui, |ui| {
                    for kind in BackendKind::ALL {
                        ui.selectable_value(&mut self.draft.default_backend, kind, kind.name());
                    }
                });
        });
        ui.label("Используется в /new без имени агента; /backend меняет агента в теме.");
        ui.separator();
        ui.heading("Claude Code");
        ui.horizontal(|ui| {
            ui.label("Путь к claude");
            ui.add(egui::TextEdit::singleline(&mut self.draft.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.cli)
            {
                self.draft.cli = path;
            }
        });
        messages(ui, errors, Field::Cli);
        ui.horizontal(|ui| {
            ui.label("Модель");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.model)
                    .hint_text("из настроек Claude Code"),
            );
        });
        messages(ui, errors, Field::Model);
        ui.horizontal(|ui| {
            ui.label("Режим разрешений");
            egui::ComboBox::from_id_salt("permission_mode")
                .selected_text(self.draft.permission_mode.wire())
                .show_ui(ui, |ui| {
                    for mode in PermissionMode::ALL {
                        ui.selectable_value(&mut self.draft.permission_mode, mode, mode.wire());
                    }
                });
        });
        if self.draft.permission_mode == PermissionMode::BypassPermissions {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "bypassPermissions: агент выполняет любые инструменты без подтверждения.",
            );
        }
        ui.horizontal(|ui| {
            ui.label("Лимит на задачу, $");
            ui.add(egui::TextEdit::singleline(&mut self.draft.budget).hint_text("без лимита"));
        });
        messages(ui, errors, Field::Budget);
    }

    fn codex(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Codex");
        ui.horizontal(|ui| {
            ui.label("Путь к codex");
            ui.add(egui::TextEdit::singleline(&mut self.draft.codex.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.codex.cli)
            {
                self.draft.codex.cli = path;
            }
        });
        messages(ui, errors, Field::Codex(CodexField::Cli));
        ui.horizontal(|ui| {
            ui.label("Модель");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.codex.model)
                    .hint_text("из настроек Codex"),
            );
        });
        messages(ui, errors, Field::Codex(CodexField::Model));
        ui.horizontal(|ui| {
            ui.label("Песочница");
            egui::ComboBox::from_id_salt("codex_sandbox")
                .selected_text(self.draft.codex.sandbox.wire())
                .show_ui(ui, |ui| {
                    for sandbox in Sandbox::ALL {
                        ui.selectable_value(&mut self.draft.codex.sandbox, sandbox, sandbox.wire());
                    }
                });
        });
        if self.draft.codex.sandbox == Sandbox::DangerFullAccess {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "danger-full-access: команды Codex выполняются без песочницы ОС.",
            );
        }
        ui.horizontal(|ui| {
            ui.label("Одобрения");
            egui::ComboBox::from_id_salt("codex_approval")
                .selected_text(self.draft.codex.approval.wire())
                .show_ui(ui, |ui| {
                    for approval in Approval::ALL {
                        ui.selectable_value(
                            &mut self.draft.codex.approval,
                            approval,
                            approval.wire(),
                        );
                    }
                });
        });
        if self.draft.codex.approval == Approval::Never {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "never: Codex ничего не спрашивает перед действиями.",
            );
        }
        ui.horizontal(|ui| {
            ui.label("API-ключ OpenAI");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.codex.api_key)
                    .password(!self.reveal_key)
                    .hint_text("вход через codex login"),
            );
            ui.checkbox(&mut self.reveal_key, "показать");
        });
        messages(ui, errors, Field::Codex(CodexField::ApiKey));
        ui.label(
            "Без ключа используется вход `codex login`. Ключ хранится в системном хранилище \
                 ключей и не заменяет вход по подписке ChatGPT.",
        );
    }

    fn timeouts(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Таймауты и обновления");
        ui.horizontal(|ui| {
            ui.label("Ожидание подтверждения, с");
            ui.text_edit_singleline(&mut self.draft.approval_timeout);
        });
        messages(ui, errors, Field::ApprovalTimeout);
        ui.horizontal(|ui| {
            ui.label("Фоновые задачи, с");
            ui.text_edit_singleline(&mut self.draft.background_timeout);
        });
        messages(ui, errors, Field::BackgroundTimeout);
        let mut check = self.draft.updates == UpdateCheck::Enabled;
        if ui.checkbox(&mut check, "Проверять обновления").changed() {
            self.draft.updates = if check { UpdateCheck::Enabled } else { UpdateCheck::Disabled };
        }
    }

    fn confirm(&mut self, ctx: &egui::Context, sessions: usize, send: &mut impl FnMut(Command)) {
        if !self.confirming {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("restart")).show(ctx, |ui| {
            ui.label(format!(
                "Сохранение перезапустит бота и прервёт активные сессии ({sessions}). Продолжить?"
            ));
            ui.horizontal(|ui| {
                if ui.button("Сохранить и перезапустить").clicked() {
                    send(Command::Save(Box::new(self.draft.clone())));
                    self.confirming = false;
                }
                if ui.button("Отмена").clicked() {
                    self.confirming = false;
                }
            });
        });
        if modal.should_close() {
            self.confirming = false;
        }
    }
}

#[must_use]
pub fn home_or_root() -> PathBuf {
    directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()).unwrap_or_default()
}
