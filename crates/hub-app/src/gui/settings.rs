//! The «Настройки» tab: a draft of every setting, saved only when it parses.

use std::path::{Path, PathBuf};

use eframe::egui;
use hub_core::actions::{ActionDraft, MAX_ACTIONS};
use hub_core::doctor::{CheckDraft, MAX_CHECKS};
use hub_core::domain::BackendKind;
use hub_core::projects::{MAX_PROJECTS, ProjectDraft};
use hub_core::settings::{
    ApiEndpoint, Approval, CodexField, Draft, Field, FieldError, HermesField, OpencodeField,
    PermissionMode, ProgressMode, QwenApproval, QwenField, Sandbox, Settings, UpdateCheck,
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
    autostart: Option<super::autostart::Control>,
    draft: Draft,
    synced: Option<Draft>,
    revealed: Revealed,
    confirming: bool,
}

fn project_options(ui: &mut egui::Ui, project: &mut ProjectDraft) {
    let previous_backend = project.backend.clone();
    egui::ComboBox::from_id_salt("project-backend").selected_text(&project.backend).show_ui(
        ui,
        |ui| {
            for backend in BackendKind::ALL {
                ui.selectable_value(
                    &mut project.backend,
                    backend.name().to_owned(),
                    backend.name(),
                );
            }
        },
    );
    if previous_backend != project.backend {
        project.options = hub_core::options::OptionsDraft::default();
    }
    ui.horizontal(|ui| {
        ui.label("Модель");
        ui.add(
            egui::TextEdit::singleline(&mut project.options.model)
                .hint_text("пусто — общие настройки"),
        );
    });
    let modes =
        BackendKind::parse(&project.backend).map(hub_core::options::Mode::all).unwrap_or_default();
    if modes.is_empty() {
        ui.label("Режим задаётся самим CLI.");
    } else {
        egui::ComboBox::from_id_salt("project-mode")
            .selected_text(if project.options.mode.is_empty() {
                "Режим: общие настройки"
            } else {
                &project.options.mode
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut project.options.mode, String::new(), "Общие настройки");
                for mode in modes {
                    let wire = mode.wire();
                    ui.selectable_value(&mut project.options.mode, wire.clone(), wire);
                }
            });
    }
}

fn project_checks(ui: &mut egui::Ui, checks: &mut Vec<CheckDraft>) {
    ui.separator();
    ui.label("Проверки ресурсов через /doctor");
    ui.small("HTTP(S): GET без редиректов и авторизации. TCP: только подключение к порту.");
    let mut remove = None;
    for (index, check) in checks.iter_mut().enumerate() {
        ui.push_id(("check", index), |ui| {
            ui.group(|ui| {
                let name = match check {
                    CheckDraft::Http { name, .. } | CheckDraft::Tcp { name, .. } => name,
                };
                ui.horizontal(|ui| {
                    ui.label("Название проверки");
                    ui.text_edit_singleline(name);
                    if ui.button("Удалить проверку").clicked() {
                        remove = Some(index);
                    }
                });
                match check {
                    CheckDraft::Http { url, expected, .. } => {
                        ui.horizontal(|ui| {
                            ui.label("URL");
                            ui.add(
                                egui::TextEdit::singleline(url).hint_text("https://service/health"),
                            );
                        });
                        ui.horizontal(|ui| {
                            ui.label("Ожидаемый HTTP-статус");
                            ui.add(egui::DragValue::new(expected).range(200..=599));
                        });
                        ui.small(
                            "Без логина, пароля, query и fragment. Не указывайте секреты в адресе.",
                        );
                    }
                    CheckDraft::Tcp { host, port, .. } => {
                        ui.horizontal(|ui| {
                            ui.label("Хост или IP");
                            ui.text_edit_singleline(host);
                        });
                        ui.horizontal(|ui| {
                            ui.label("TCP-порт");
                            ui.add(egui::DragValue::new(port).range(1..=65535));
                        });
                    }
                }
            });
        });
    }
    if let Some(index) = remove {
        checks.remove(index);
    }
    ui.horizontal(|ui| {
        if ui.add_enabled(checks.len() < MAX_CHECKS, egui::Button::new("Добавить HTTP")).clicked()
        {
            checks.push(CheckDraft::Http {
                name: String::new(),
                url: String::new(),
                expected: 200,
            });
        }
        if ui.add_enabled(checks.len() < MAX_CHECKS, egui::Button::new("Добавить TCP")).clicked()
        {
            checks.push(CheckDraft::Tcp { name: String::new(), host: String::new(), port: 443 });
        }
        ui.label(format!("{}/{MAX_CHECKS}", checks.len()));
    });
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
    pub fn with_autostart(
        registration: Result<std::sync::Arc<dyn crate::autostart::Registration>, String>,
    ) -> Self {
        Self { autostart: Some(super::autostart::Control::new(registration)), ..Self::default() }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(control) = &mut self.autostart {
            control.poll(ctx);
        }
    }
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
            if let Some(control) = &mut self.autostart {
                control.show(ui);
                ui.separator();
            }
            self.telegram(ui, &errors);
            ui.separator();
            self.workspace(ui, &errors);
            ui.separator();
            self.projects(ui, &errors);
            ui.separator();
            self.actions(ui, &errors);
            ui.separator();
            self.agent(ui, &errors);
            ui.separator();
            self.codex(ui, &errors);
            ui.separator();
            self.qwen(ui, &errors, endpoint);
            ui.separator();
            self.hermes(ui, &errors);
            ui.separator();
            self.opencode(ui, &errors);
            ui.separator();
            self.timeouts(ui, &errors);
        });
        self.confirm(ui.ctx(), sessions, send);
    }

    fn projects(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Проекты");
        ui.label("Сохраните каталог, бэкенд и параметры, затем выберите проект через /projects в Telegram.");
        ui.label("Пустые параметры наследуют общие настройки. Смена бэкенда сбрасывает модель и режим профиля.");
        ui.label("Связанные действия запускаются вручную через /actions. Изменения профиля не меняют существующие сессии.");
        let mut remove = None;
        for (index, project) in self.draft.projects.iter_mut().enumerate() {
            ui.push_id(egui::Id::new(&project.id), |ui| {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Название");
                        ui.text_edit_singleline(&mut project.name);
                        if ui.button("Удалить проект").clicked() {
                            remove = Some(index);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Каталог");
                        ui.add(
                            egui::TextEdit::singleline(&mut project.cwd)
                                .hint_text("путь внутри корня рабочих каталогов"),
                        );
                        if ui.button("Выбрать…").clicked()
                            && let Some(path) = pick_folder(&project.cwd)
                        {
                            project.cwd = path;
                        }
                    });
                    project_options(ui, project);
                    if !self.draft.actions.is_empty() {
                        ui.label("Связанные действия (например, VPN)");
                    }
                    for action in &self.draft.actions {
                        let mut checked = project.actions.contains(&action.id);
                        if ui.checkbox(&mut checked, &action.name).changed() {
                            project.actions.retain(|id| id != &action.id);
                            if checked {
                                project.actions.push(action.id.clone());
                            }
                        }
                    }
                    project_checks(ui, &mut project.checks);
                    // A removed action must be explicitly unlinked, not silently assigned to a new one.
                    let missing: Vec<_> = project
                        .actions
                        .iter()
                        .filter(|id| !self.draft.actions.iter().any(|action| action.id == **id))
                        .cloned()
                        .collect();
                    for id in missing {
                        if ui.button(format!("Убрать отсутствующее действие {id}")).clicked()
                        {
                            project.actions.retain(|saved| saved != &id);
                        }
                    }
                })
            });
        }
        if let Some(index) = remove {
            self.draft.projects.remove(index);
        }
        if ui
            .add_enabled(
                self.draft.projects.len() < MAX_PROJECTS,
                egui::Button::new("Добавить проект"),
            )
            .clicked()
        {
            self.draft.projects.push(ProjectDraft {
                options: hub_core::options::OptionsDraft::default(),
                id: uuid::Uuid::new_v4().simple().to_string(),
                name: String::new(),
                cwd: String::new(),
                backend: self.draft.default_backend.name().to_owned(),
                actions: Vec::new(),
                checks: Vec::new(),
            });
        }
        messages(ui, errors, Field::Projects);
    }

    fn actions(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Действия");
        ui.label("Назовите команду и запускайте её кнопкой через /actions в Telegram.");
        ui.label("Команды выполняются от вашего пользователя. Без ввода пароля и терминала; вывод отправляется в группу.");
        ui.label("Постоянные команды работают до остановки. Остановка бота или выход из приложения останавливает их.");
        ui.label(if cfg!(windows) {
            "Оболочка: cmd.exe /D /S /C"
        } else {
            "Оболочка: /bin/sh -c"
        });
        let mut remove = None;
        for (index, action) in self.draft.actions.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Название");
                        ui.text_edit_singleline(&mut action.name);
                        if ui.button("Удалить").clicked() {
                            remove = Some(index);
                        }
                    });
                    ui.label("Команда");
                    ui.add(
                        egui::TextEdit::multiline(&mut action.command)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                    ui.horizontal(|ui| {
                        ui.label("Каталог");
                        ui.add(
                            egui::TextEdit::singleline(&mut action.cwd)
                                .hint_text("пусто — домашний каталог"),
                        );
                        if ui.button("Выбрать…").clicked()
                            && let Some(path) = pick_folder(&action.cwd)
                        {
                            action.cwd = path;
                        }
                    });
                });
            });
        }
        if let Some(index) = remove {
            self.draft.actions.remove(index);
        }
        if ui
            .add_enabled(
                self.draft.actions.len() < MAX_ACTIONS,
                egui::Button::new("Добавить действие"),
            )
            .clicked()
        {
            self.draft.actions.push(ActionDraft {
                id: uuid::Uuid::new_v4().simple().to_string(),
                ..ActionDraft::default()
            });
        }
        messages(ui, errors, Field::Actions);
    }

    fn telegram(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Telegram");
        ui.label("Изменение токена, chat_id или user_id перезапускает бота и останавливает сессии и пользовательские действия.");
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
        ui.horizontal(|ui| {
            ui.label("Прогресс задач");
            ui.selectable_value(&mut self.draft.progress_mode, ProgressMode::Compact, "Компактный");
            ui.selectable_value(&mut self.draft.progress_mode, ProgressMode::Detailed, "Подробный");
        });
        ui.label("Компактный: одна карточка вместо сообщений об инструментах. Применяется к следующим запускам без перезапуска бота.");
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
        ui.horizontal(|ui| {
            ui.label("Каталог снимков");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.snapshots_dir).hint_text("~/.agent-hub"),
            );
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_folder(&self.draft.snapshots_dir)
            {
                self.draft.snapshots_dir = path;
            }
        });
        messages(ui, errors, Field::SnapshotsDir);
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

    fn hermes(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Hermes");
        ui.horizontal(|ui| {
            ui.label("Путь к hermes");
            ui.add(egui::TextEdit::singleline(&mut self.draft.hermes.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.hermes.cli)
            {
                self.draft.hermes.cli = path;
            }
        });
        messages(ui, errors, Field::Hermes(HermesField::Cli));
        ui.horizontal(|ui| {
            ui.label("Профиль");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.hermes.profile)
                    .hint_text("активный профиль Hermes"),
            );
        });
        messages(ui, errors, Field::Hermes(HermesField::Profile));
        ui.horizontal(|ui| {
            ui.label("Модель");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.hermes.model)
                    .hint_text("из настроек Hermes"),
            );
        });
        messages(ui, errors, Field::Hermes(HermesField::Model));
        ui.label("Вход и провайдер настраиваются в Hermes (`hermes setup`). Хаб запускает `hermes acp`, не меняя профиль и его секреты. Новые сообщения во время работы ждут следующего хода.");
    }

    fn opencode(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("OpenCode");
        ui.horizontal(|ui| {
            ui.label("Путь к opencode");
            ui.add(egui::TextEdit::singleline(&mut self.draft.opencode.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.opencode.cli)
            {
                self.draft.opencode.cli = path;
            }
        });
        messages(ui, errors, Field::Opencode(OpencodeField::Cli));
        ui.horizontal(|ui| {
            ui.label("Модель");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.opencode.model)
                    .hint_text("провайдер/модель из настроек OpenCode"),
            );
        });
        messages(ui, errors, Field::Opencode(OpencodeField::Model));
        ui.label("Вход и провайдеры настраиваются в OpenCode (`opencode auth login`). Хаб запускает `opencode acp`, не меняя его конфигурацию и секреты. Новые сообщения во время работы ждут следующего хода.");
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

    #[test]
    fn autostart_applies_without_saving_an_incomplete_telegram_form() {
        use crate::autostart::{Action, Registration, State};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Fake(Arc<AtomicBool>);
        impl Registration for Fake {
            fn apply(&self, action: Action) -> Result<State, String> {
                match action {
                    Action::Enable => self.0.store(true, Ordering::SeqCst),
                    Action::Disable => self.0.store(false, Ordering::SeqCst),
                    Action::Inspect => {}
                }
                Ok(if self.0.load(Ordering::SeqCst) { State::Enabled } else { State::Disabled })
            }
        }
        let enabled = Arc::new(AtomicBool::new(false));
        let mut form = SettingsForm::with_autostart(Ok(Arc::new(Fake(enabled.clone()))));
        let ctx = egui::Context::default();
        let mut frame = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 900.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    form.show(ui, &std::env::temp_dir(), 0, &mut |_| {
                        panic!("autostart must not save bot settings")
                    });
                },
            );
            output.textures_delta.clear();
            output
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let output = loop {
            let output = frame(vec![]);
            if output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(t) if t.galley.text() == "Выключен в настройках ОС")) { break output; }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        };
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(t) if t.galley.text() == "Включить" => {
                    Some(t.galley.rect.translate(t.pos.to_vec2()).center())
                }
                _ => None,
            })
            .unwrap();
        for pressed in [true, false] {
            frame(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        while !enabled.load(Ordering::SeqCst) {
            frame(vec![]);
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!form.check(&std::env::temp_dir()).0.is_empty());
    }
    use rstest::rstest;

    #[test]
    fn project_rows_keep_stable_ids_and_support_linked_actions() {
        fn frame(
            ctx: &egui::Context,
            form: &mut SettingsForm,
            events: Vec<egui::Event>,
        ) -> egui::FullOutput {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(960.0, 1200.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    form.projects(ui, &[]);
                },
            );
            output.textures_delta.clear();
            output
        }
        fn click(ctx: &egui::Context, form: &mut SettingsForm, label: &str) {
            frame(ctx, form, Vec::new());
            let output = frame(ctx, form, Vec::new());
            let position = output
                .shapes
                .iter()
                .find_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape
                        && text.galley.text() == label
                    {
                        Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                    } else {
                        None
                    }
                })
                .unwrap();
            for pressed in [true, false] {
                frame(
                    ctx,
                    form,
                    vec![
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        }
        let ctx = egui::Context::default();
        let mut form = SettingsForm::default();
        form.draft.actions.push(ActionDraft {
            id: "vpn".to_owned(),
            name: "VPN".to_owned(),
            command: "echo vpn".to_owned(),
            cwd: String::new(),
        });
        click(&ctx, &mut form, "Добавить проект");
        click(&ctx, &mut form, "Добавить HTTP");
        assert!(matches!(
            form.draft.projects.first().unwrap().checks.first(),
            Some(CheckDraft::Http { expected: 200, .. })
        ));
        click(&ctx, &mut form, "Добавить TCP");
        assert!(matches!(
            form.draft.projects.first().unwrap().checks.last(),
            Some(CheckDraft::Tcp { port: 443, .. })
        ));
        click(&ctx, &mut form, "Удалить проверку");
        assert_eq!(form.draft.projects.first().unwrap().checks.len(), 1);
        click(&ctx, &mut form, "Удалить проверку");
        assert!(form.draft.projects.first().unwrap().checks.is_empty());
        let first = form.draft.projects.first().unwrap().id.clone();
        click(&ctx, &mut form, "VPN");
        assert_eq!(form.draft.projects.first().unwrap().actions, ["vpn"]);
        click(&ctx, &mut form, "Добавить проект");
        let second = form.draft.projects.last().unwrap().id.clone();
        assert_ne!(first, second);
        click(&ctx, &mut form, "Удалить проект");
        assert_eq!(form.draft.projects.first().unwrap().id, second);
        click(&ctx, &mut form, "Добавить проект");
        assert_ne!(form.draft.projects.last().unwrap().id, first);
    }

    #[test]
    fn action_rows_can_be_added_and_removed() {
        fn draw(
            ctx: &egui::Context,
            form: &mut SettingsForm,
            events: Vec<egui::Event>,
        ) -> egui::FullOutput {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(960.0, 1200.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| form.actions(ui, &[]));
                },
            );
            output.textures_delta.clear();
            output
        }
        fn click(ctx: &egui::Context, form: &mut SettingsForm, label: &str) {
            let output = draw(ctx, form, Vec::new());
            let position = output
                .shapes
                .iter()
                .find_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape
                        && text.galley.text() == label
                    {
                        Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                    } else {
                        None
                    }
                })
                .unwrap();
            for pressed in [true, false] {
                draw(
                    ctx,
                    form,
                    vec![
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        }
        let ctx = egui::Context::default();
        let mut form = SettingsForm::default();
        click(&ctx, &mut form, "Добавить действие");
        click(&ctx, &mut form, "Добавить действие");
        assert_eq!(form.draft.actions.len(), 2);
        let ids = form
            .draft
            .actions
            .iter()
            .map(|action| &action.id)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ids.len(), 2);
        click(&ctx, &mut form, "Удалить");
        assert_eq!(form.draft.actions.len(), 1);
    }

    #[test]
    fn snapshots_directory_is_visible() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 4000.0));
        let mut form = SettingsForm::default();
        let mut output =
            ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    form.show(ui, Path::new("/"), 0, &mut |_| {});
                });
            });
        output.textures_delta.clear();
        assert!(output.shapes.iter().any(|clipped| matches!(
            &clipped.shape,
            egui::Shape::Text(text) if text.galley.text() == "Каталог снимков"
        )));
    }

    #[test]
    fn hermes_settings_are_visible() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 4000.0));
        let mut form = SettingsForm::default();
        let mut output =
            ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    form.show(ui, Path::new("/"), 0, &mut |_| {});
                });
            });
        output.textures_delta.clear();
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|clipped| {
                if let egui::Shape::Text(text) = &clipped.shape {
                    Some(text.galley.text().to_owned())
                } else {
                    None
                }
            })
            .collect();
        assert!(labels.iter().any(|label| label == "Hermes"), "Hermes section missing");
        assert!(labels.iter().any(|label| label == "Профиль"), "Hermes profile missing");
        assert!(labels.iter().any(|label| label == "OpenCode"), "OpenCode section missing");
        assert!(labels.iter().any(|label| label == "Путь к opencode"), "OpenCode cli missing");
    }

    #[test]
    fn visible_save_dispatches_edited_settings() {
        let home = home_or_root();
        let draft = Draft {
            token: "123:test-token".to_owned(),
            chat: "0".to_owned(),
            users: "1".to_owned(),
            workspace_root: home.display().to_string(),
            actions: vec![ActionDraft {
                id: "vpn".to_owned(),
                name: "Рабочий VPN".to_owned(),
                command: "echo test".to_owned(),
                cwd: String::new(),
            }],
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
        assert!(
            matches!(commands.as_slice(), [Command::Save(saved)] if saved.chat == "-100" && saved.actions == draft.actions)
        );
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
