//! The «Настройки» tab: a draft of every setting, saved only when it parses.

use std::path::{Path, PathBuf};

use eframe::egui;
use hub_core::domain::BackendKind;
use hub_core::settings::{
    ApiEndpoint, Approval, CodexField, Draft, Field, FieldError, PermissionMode, QwenApproval,
    QwenField, Sandbox, Settings, UpdateCheck,
};

use crate::gui::log::LogAction;
use crate::gui::look::{add_user, telegram_changed, use_chat};
use crate::supervisor::Command;

/// Which secret fields are shown in clear text.
#[derive(Default)]
struct Revealed {
    token: bool,
    codex_key: bool,
    qwen_key: bool,
}

#[derive(Default)]
pub struct SettingsForm {
    draft: Draft,
    synced: Option<Draft>,
    revealed: Revealed,
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

    /// Parses the draft once: the field errors and, when it parses, the settings.
    fn check(&self, home: &Path) -> (Vec<FieldError>, Option<Settings>) {
        match self.draft.parse(home) {
            Ok(settings) if settings.workspace_root.is_dir() => (Vec::new(), Some(settings)),
            Ok(settings) => (
                vec![FieldError {
                    field: Field::WorkspaceRoot,
                    message: "Такого каталога нет".to_owned(),
                }],
                Some(settings),
            ),
            Err(errors) => (errors, None),
        }
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        home: &Path,
        sessions: usize,
        send: &mut impl FnMut(Command),
    ) {
        let (errors, parsed) = self.check(home);
        let endpoint = parsed.as_ref().and_then(|settings| settings.qwen.endpoint.as_ref());
        let changed = self.synced.as_ref().is_none_or(|synced| *synced != self.draft);
        egui::Panel::bottom("settings_actions").show(ui, |ui| {
            ui.horizontal(|ui| {
                let save =
                    ui.add_enabled(errors.is_empty() && changed, egui::Button::new("Сохранить"));
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
        });
        egui::ScrollArea::vertical().show(ui, |ui| {
            self.telegram(ui, &errors);
            ui.separator();
            self.workspace(ui, &errors);
            ui.separator();
            self.agent(ui, &errors);
            ui.separator();
            self.codex(ui, &errors);
            ui.separator();
            self.qwen(ui, &errors, endpoint);
            ui.separator();
            self.timeouts(ui, &errors);
        });
        self.confirm(ui.ctx(), sessions, send);
    }

    fn telegram(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Telegram");
        ui.label("Изменение этих полей перезапускает бота и прерывает работающие сессии.");
        ui.horizontal(|ui| {
            ui.label("Токен бота");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.token).password(!self.revealed.token),
            );
            ui.checkbox(&mut self.revealed.token, "показать");
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
                    .password(!self.revealed.codex_key)
                    .hint_text("вход через codex login"),
            );
            ui.checkbox(&mut self.revealed.codex_key, "показать");
        });
        messages(ui, errors, Field::Codex(CodexField::ApiKey));
        ui.label(
            "Без ключа используется вход `codex login`. Ключ хранится в системном хранилище \
                 ключей и не заменяет вход по подписке ChatGPT.",
        );
    }

    fn qwen(&mut self, ui: &mut egui::Ui, errors: &[FieldError], endpoint: Option<&ApiEndpoint>) {
        ui.heading("Qwen");
        ui.horizontal(|ui| {
            ui.label("Путь к qwen");
            ui.add(egui::TextEdit::singleline(&mut self.draft.qwen.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.qwen.cli)
            {
                self.draft.qwen.cli = path;
            }
        });
        messages(ui, errors, Field::Qwen(QwenField::Cli));
        ui.horizontal(|ui| {
            ui.label("Модель");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.qwen.model)
                    .hint_text("из настроек Qwen"),
            );
        });
        messages(ui, errors, Field::Qwen(QwenField::Model));
        ui.horizontal(|ui| {
            ui.label("Одобрения");
            egui::ComboBox::from_id_salt("qwen_approval")
                .selected_text(self.draft.qwen.approval.wire())
                .show_ui(ui, |ui| {
                    for approval in QwenApproval::ALL {
                        ui.selectable_value(
                            &mut self.draft.qwen.approval,
                            approval,
                            approval.wire(),
                        );
                    }
                });
        });
        if self.draft.qwen.approval == QwenApproval::Yolo {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "yolo: Qwen выполняет любые инструменты без подтверждения.",
            );
        }
        ui.horizontal(|ui| {
            ui.label("Base URL");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.qwen.base_url)
                    .hint_text("собственная настройка qwen"),
            );
        });
        messages(ui, errors, Field::Qwen(QwenField::BaseUrl));
        ui.horizontal(|ui| {
            ui.label("API-ключ");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.qwen.api_key)
                    .password(!self.revealed.qwen_key)
                    .hint_text("собственная настройка qwen"),
            );
            ui.checkbox(&mut self.revealed.qwen_key, "показать");
        });
        messages(ui, errors, Field::Qwen(QwenField::ApiKey));
        if endpoint.is_some_and(ApiEndpoint::is_cleartext_remote) {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "http:// к другому компьютеру: ключ уходит в сеть открытым текстом.",
            );
        }
        ui.label(
            "Без адреса и ключа Qwen использует собственную настройку (`qwen` → /auth, \
             ~/.qwen/settings.json). Адрес OpenAI-совместимый; ключ хранится в системном \
             хранилище ключей и передаётся только процессу qwen.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[test]
    fn visible_save_dispatches_edited_settings() {
        let home = home_or_root();
        let draft = Draft {
            token: "123:test-token".to_owned(),
            chat: "0".to_owned(),
            users: "1".to_owned(),
            workspace_root: home.display().to_string(),
            ..Draft::default()
        };
        let mut form = SettingsForm::default();
        form.sync(Some(&draft));
        form.apply(LogAction::UseChat(-100));
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(640.0, 360.0));
        let mut commands = Vec::new();
        let mut frame = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput { screen_rect: Some(screen), events, ..Default::default() },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        form.show(ui, &home, 0, &mut |command| commands.push(command));
                    });
                },
            );
            output.textures_delta.clear();
            output
        };
        let output = frame(Vec::new());
        let save = output
            .shapes
            .iter()
            .find_map(|clipped| {
                if let egui::Shape::Text(text) = &clipped.shape
                    && text.galley.text() == "Сохранить"
                {
                    Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                } else {
                    None
                }
            })
            .expect("save button is painted");
        assert!(screen.contains(save));
        for pressed in [true, false] {
            frame(vec![
                egui::Event::PointerMoved(save),
                egui::Event::PointerButton {
                    pos: save,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        assert!(matches!(commands.as_slice(), [Command::Save(saved)] if saved.chat == "-100"));
    }

    #[rstest]
    #[case(egui::vec2(960.0, 640.0))]
    #[case(egui::vec2(640.0, 360.0))]
    fn settings_actions_remain_visible(#[case] size: egui::Vec2) {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
        let mut form = SettingsForm::default();
        for _ in 0..2 {
            let mut output = ctx.run_ui(
                egui::RawInput { screen_rect: Some(screen), ..Default::default() },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        form.show(ui, Path::new("/"), 0, &mut |_| {});
                    });
                },
            );
            output.textures_delta.clear();
            for label in ["Сохранить", "Отменить изменения"] {
                assert!(
                    output.shapes.iter().any(|clipped| {
                        if let egui::Shape::Text(text) = &clipped.shape {
                            let rect = text.galley.rect.translate(text.pos.to_vec2());
                            text.galley.text() == label
                                && screen.contains_rect(rect)
                                && clipped.clip_rect.contains_rect(rect)
                        } else {
                            false
                        }
                    }),
                    "{label} is clipped at {size:?}"
                );
            }
        }
    }
}
