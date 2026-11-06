//! The «Настройки» tab: a draft of every setting, saved only when it parses.

mod navigation;

use navigation::{AgentPage, Section};

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
use hub_core::speech::SpeechProvider;

use crate::gui::log::LogAction;
use crate::gui::look::{add_user, telegram_changed, use_chat};
use crate::supervisor::Command;

/// Which secret fields are shown in clear text.
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "Each secret has an independent visibility toggle"
)]
struct Revealed {
    token: bool,
    codex_key: bool,
    qwen_key: bool,
    speech_key: bool,
}

#[derive(Default)]
pub struct SettingsForm {
    section: Section,
    agent_page: AgentPage,
    autostart: Option<super::autostart::Control>,
    scale: super::scale::Control,
    draft: Draft,
    synced: Option<Draft>,
    revealed: Revealed,
    confirming: bool,
    saving: bool,
    submitted: Option<Draft>,
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
        ui.add(input(&mut project.options.model).hint_text("пусто — общие настройки"));
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
                    ui.add(input(name));
                    if ui.button("Удалить проверку").clicked() {
                        remove = Some(index);
                    }
                });
                match check {
                    CheckDraft::Http { url, expected, .. } => {
                        ui.horizontal(|ui| {
                            ui.label("URL");
                            ui.add(input(url).hint_text("https://service/health"));
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
                            ui.add(input(host));
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

fn input(value: &mut String) -> egui::TextEdit<'_> {
    egui::TextEdit::singleline(value).margin(egui::vec2(10.0, 7.0))
}

fn field_row(ui: &mut egui::Ui, label: &str, contents: impl FnOnce(&mut egui::Ui)) {
    if ui.available_width() < 560.0 {
        ui.vertical(|ui| {
            ui.label(label);
            ui.horizontal_wrapped(contents);
        });
    } else {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(180.0, 32.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_width(180.0);
                    ui.label(label);
                },
            );
            contents(ui);
        });
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
    pub fn set_saving(&mut self, saving: bool) {
        if saving && self.submitted.is_none() {
            self.submitted = Some(self.draft.clone());
        } else if !saving {
            self.submitted = None;
        }
        self.saving = saving;
    }

    pub fn has_unsaved(&self) -> bool {
        self.synced.as_ref().is_none_or(|saved| *saved != self.draft)
    }

    pub fn with_autostart(
        registration: Result<std::sync::Arc<dyn crate::autostart::Registration>, String>,
    ) -> Self {
        Self { autostart: Some(super::autostart::Control::new(registration)), ..Self::default() }
    }

    pub fn set_scale_path(&mut self, path: PathBuf) {
        self.scale = super::scale::Control::new(path);
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
        let pristine = match (self.submitted.as_ref(), saved) {
            (Some(submitted), Some(saved)) if submitted == saved => self.draft == *submitted,
            _ => self
                .synced
                .as_ref()
                .map_or(self.draft == Draft::default(), |synced| *synced == self.draft),
        };
        if pristine {
            self.draft = saved.cloned().unwrap_or_default();
        }
        self.synced = saved.cloned();
    }

    pub fn apply(&mut self, action: LogAction) {
        self.section = Section::Telegram;
        match action {
            LogAction::UseChat(chat) => use_chat(&mut self.draft, chat),
            LogAction::AddUser(user) => add_user(&mut self.draft, user),
        }
    }

    /// Parses the draft once: the field errors and, when it parses, the settings.
    fn check(&self, home: &Path) -> (Vec<FieldError>, Option<Settings>) {
        match self.draft.parse(home) {
            Ok(settings) => (Vec::new(), Some(settings)),
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
        let compact = ui.available_height() < 280.0;
        self.save_bar(ui, &errors, sessions, send);
        navigation::show(ui, &mut self.section, &errors);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().inner_margin(if compact {
                egui::Margin::same(8)
            } else {
                egui::Margin::symmetric(24, 16)
            }))
            .show(ui, |ui| {
                let content_width = (ui.available_width() - 12.0).min(800.0);
                egui::ScrollArea::both()
                    .id_salt(("settings_content", self.section, self.agent_page))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(content_width);
                        if !compact {
                            ui.heading(self.section.title())
                                .on_hover_text(self.section.description());
                        }
                        if !compact && content_width >= 600.0 {
                            ui.label(egui::RichText::new(self.section.description()).weak());
                            ui.add_space(20.0);
                        } else if !compact {
                            ui.add_space(8.0);
                        }
                        if self.section == Section::Agents {
                            self.agent_page.show(ui, &errors);
                            ui.add_space(16.0);
                        }
                        super::theme::surface(ui).inner_margin(if compact { 8 } else { 20 }).show(
                            ui,
                            |ui| {
                                ui.set_width((ui.available_width()).max(280.0));
                                ui.spacing_mut().text_edit_width =
                                    (ui.available_width() - 260.0).clamp(140.0, 320.0);
                                ui.push_id((self.section, self.agent_page), |ui| {
                                    self.page(ui, &errors, parsed.as_ref());
                                });
                            },
                        );
                    });
            });
        self.confirm(ui.ctx(), sessions, send);
    }

    fn page(&mut self, ui: &mut egui::Ui, errors: &[FieldError], parsed: Option<&Settings>) {
        match self.section {
            Section::Telegram => self.telegram(ui, errors),
            Section::Workspace => self.workspace(ui, errors),
            Section::Projects => self.projects(ui, errors),
            Section::Actions => self.actions(ui, errors),
            Section::Speech => self.speech(ui, errors),
            Section::Application => self.application(ui),
            Section::Agents => match self.agent_page {
                AgentPage::General => {
                    self.agent(ui);
                    ui.add_space(16.0);
                    self.timeouts(ui, errors);
                }
                AgentPage::Backend(BackendKind::Claude) => self.claude(ui, errors),
                AgentPage::Backend(BackendKind::Codex) => self.codex(ui, errors),
                AgentPage::Backend(BackendKind::Qwen) => self.qwen(
                    ui,
                    errors,
                    parsed.and_then(|settings| settings.qwen.endpoint.as_ref()),
                ),
                AgentPage::Backend(BackendKind::Hermes) => self.hermes(ui, errors),
                AgentPage::Backend(BackendKind::Opencode) => self.opencode(ui, errors),
            },
        }
    }

    fn save_bar(
        &mut self,
        ui: &mut egui::Ui,
        errors: &[FieldError],
        sessions: usize,
        send: &mut impl FnMut(Command),
    ) {
        let changed = self.has_unsaved();
        let compact = ui.available_height() < 280.0;
        egui::Panel::bottom("settings_actions")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(20, 12)),
            )
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let save = ui.add_enabled(
                        errors.is_empty() && changed && !self.saving,
                        egui::Button::new("Сохранить").fill(ui.visuals().selection.bg_fill),
                    );
                    if save.clicked() {
                        if telegram_changed(self.synced.as_ref(), &self.draft) && sessions > 0 {
                            self.confirming = true;
                        } else {
                            self.submitted = Some(self.draft.clone());
                            send(Command::Save(Box::new(self.draft.clone())));
                        }
                    }
                    if ui.add_enabled(changed, egui::Button::new("Отменить изменения")).clicked()
                    {
                        self.draft = self.synced.clone().unwrap_or_default();
                        self.confirming = false;
                    }
                    if compact {
                        if !errors.is_empty()
                            && ui.button(format!("Ошибки ({})", errors.len())).clicked()
                            && let Some(error) = errors.first()
                        {
                            self.section = Section::for_field(error.field);
                            if let Some(page) = AgentPage::for_field(error.field) {
                                self.agent_page = page;
                            }
                        }
                    } else {
                        ui.add_space(8.0);
                        ui.weak(if changed {
                            "Есть несохранённые изменения"
                        } else {
                            "Нет несохранённых изменений"
                        });
                    }
                });
                if !compact
                    && !errors.is_empty()
                    && ui.button(format!("Исправить ошибки ({})", errors.len())).clicked()
                    && let Some(error) = errors.first()
                {
                    self.section = Section::for_field(error.field);
                    if let Some(page) = AgentPage::for_field(error.field) {
                        self.agent_page = page;
                    }
                }
            });
    }

    fn speech(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.strong("Способ распознавания");
        egui::ComboBox::from_id_salt("speech_provider")
            .selected_text(self.draft.speech.provider.label())
            .show_ui(ui, |ui| {
                for provider in SpeechProvider::ALL {
                    ui.selectable_value(
                        &mut self.draft.speech.provider,
                        provider,
                        provider.label(),
                    );
                }
            });
        ui.label("Смена способа распознавания сохраняет настройки обоих провайдеров.");
        match self.draft.speech.provider {
            SpeechProvider::Disabled => {
                ui.label("Распознавание голосовых сообщений выключено.");
            }
            SpeechProvider::Local => {
                ui.label("Установите Whisper.cpp и ffmpeg самостоятельно и скачайте модель. Аудио распознаётся на этом компьютере.");
                for (id, label, path) in [
                    ("speech_whisper", "Путь к whisper-cli", &mut self.draft.speech.whisper_path),
                    ("speech_model", "Файл модели Whisper.cpp", &mut self.draft.speech.model_path),
                    ("speech_ffmpeg", "Путь к ffmpeg", &mut self.draft.speech.ffmpeg_path),
                ] {
                    ui.push_id(id, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(label);
                            ui.add(input(path));
                            if ui.button("Выбрать…").clicked()
                                && let Some(selected) = pick_file(path)
                            {
                                *path = selected;
                            }
                        });
                    });
                }
                ui.small("Для команд допустимо имя из PATH или полный путь; для модели нужен полный путь.");
            }
            SpeechProvider::Api => {
                ui.label("Аудио отправляется на указанный API-адрес. Провайдер может взимать плату за распознавание.");
                field_row(ui, "API URL", |ui| {
                    ui.add(
                        input(&mut self.draft.speech.api_url)
                            .hint_text("https://api.openai.com/v1"),
                    );
                });
                ui.small("Базовый URL без /audio/transcriptions. HTTPS; HTTP — только localhost или loopback IP.");
                field_row(ui, "Модель распознавания", |ui| {
                    ui.add(input(&mut self.draft.speech.api_model));
                });
                field_row(ui, "API-ключ распознавания", |ui| {
                    ui.add(
                        input(&mut self.draft.speech.api_key).password(!self.revealed.speech_key),
                    );
                    ui.checkbox(&mut self.revealed.speech_key, "показать");
                });
                ui.small("Отдельный ключ хранится в системном хранилище ключей.");
            }
        }
        for error in errors.iter().filter(|error| matches!(error.field, Field::Speech(_))) {
            ui.colored_label(ui.visuals().error_fg_color, &error.message);
        }
    }

    fn projects(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.strong("Профили проектов");
        ui.label("Сохраните каталог, бэкенд и параметры, затем выберите проект через /projects в Telegram.");
        ui.label("Пустые параметры наследуют общие настройки. Смена бэкенда сбрасывает модель и режим профиля.");
        ui.label("Связанные действия запускаются вручную через /actions. Изменения профиля не меняют существующие сессии.");
        let mut remove = None;
        for (index, project) in self.draft.projects.iter_mut().enumerate() {
            ui.push_id(egui::Id::new(&project.id), |ui| {
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        ui.label("Название");
                        ui.add(input(&mut project.name));
                        if ui.button("Удалить проект").clicked() {
                            remove = Some(index);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Каталог");
                        ui.add(
                            input(&mut project.cwd)
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
        ui.strong("Пользовательские команды");
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
                        ui.add(input(&mut action.name));
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
                        ui.add(input(&mut action.cwd).hint_text("пусто — домашний каталог"));
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
        ui.strong("Подключение");
        field_row(ui, "Токен бота", |ui| {
            ui.add(input(&mut self.draft.token).password(!self.revealed.token));
            ui.checkbox(&mut self.revealed.token, "показать");
        });
        messages(ui, errors, Field::Token);
        field_row(ui, "chat_id группы", |ui| {
            ui.add(input(&mut self.draft.chat));
        });
        messages(ui, errors, Field::Chat);
        field_row(ui, "Разрешённые user_id", |ui| {
            ui.add(input(&mut self.draft.users).hint_text("111, 222"));
        });
        messages(ui, errors, Field::Users);
        ui.small("Изменение токена, chat_id или user_id перезапускает бота и останавливает сессии и пользовательские действия.");
        field_row(ui, "Прогресс задач", |ui| {
            ui.selectable_value(&mut self.draft.progress_mode, ProgressMode::Compact, "Компактный");
            ui.selectable_value(&mut self.draft.progress_mode, ProgressMode::Detailed, "Подробный");
        });
        ui.label("Компактный: одна карточка вместо сообщений об инструментах. Применяется к следующим запускам без перезапуска бота.");
    }

    fn workspace(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.strong("Каталоги");
        field_row(ui, "Корень", |ui| {
            ui.add(input(&mut self.draft.workspace_root));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_folder(&self.draft.workspace_root)
            {
                self.draft.workspace_root = path;
            }
        });
        messages(ui, errors, Field::WorkspaceRoot);
        field_row(ui, "Каталог снимков", |ui| {
            ui.add(input(&mut self.draft.snapshots_dir).hint_text("~/.agent-hub"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_folder(&self.draft.snapshots_dir)
            {
                self.draft.snapshots_dir = path;
            }
        });
        messages(ui, errors, Field::SnapshotsDir);
    }

    fn agent(&mut self, ui: &mut egui::Ui) {
        ui.strong("Новые темы");
        field_row(ui, "Агент по умолчанию", |ui| {
            egui::ComboBox::from_id_salt("default_backend")
                .selected_text(self.draft.default_backend.name())
                .show_ui(ui, |ui| {
                    for kind in BackendKind::ALL {
                        ui.selectable_value(&mut self.draft.default_backend, kind, kind.name());
                    }
                });
        });
        ui.label("Используется в /new без имени агента; /backend меняет агента в теме.");
    }

    fn claude(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Claude Code");
        field_row(ui, "Путь к claude", |ui| {
            ui.add(input(&mut self.draft.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.cli)
            {
                self.draft.cli = path;
            }
        });
        messages(ui, errors, Field::Cli);
        field_row(ui, "Модель", |ui| {
            ui.add(input(&mut self.draft.model).hint_text("из настроек Claude Code"));
        });
        messages(ui, errors, Field::Model);
        field_row(ui, "Режим разрешений", |ui| {
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
        field_row(ui, "Лимит на задачу, $", |ui| {
            ui.add(input(&mut self.draft.budget).hint_text("без лимита"));
        });
        messages(ui, errors, Field::Budget);
    }

    fn codex(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("Codex");
        field_row(ui, "Путь к codex", |ui| {
            ui.add(input(&mut self.draft.codex.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.codex.cli)
            {
                self.draft.codex.cli = path;
            }
        });
        messages(ui, errors, Field::Codex(CodexField::Cli));
        field_row(ui, "Модель", |ui| {
            ui.add(input(&mut self.draft.codex.model).hint_text("из настроек Codex"));
        });
        messages(ui, errors, Field::Codex(CodexField::Model));
        field_row(ui, "Песочница", |ui| {
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
        field_row(ui, "Одобрения", |ui| {
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
        field_row(ui, "API-ключ OpenAI", |ui| {
            ui.add(
                input(&mut self.draft.codex.api_key)
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
        field_row(ui, "Путь к qwen", |ui| {
            ui.add(input(&mut self.draft.qwen.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.qwen.cli)
            {
                self.draft.qwen.cli = path;
            }
        });
        messages(ui, errors, Field::Qwen(QwenField::Cli));
        field_row(ui, "Модель", |ui| {
            ui.add(input(&mut self.draft.qwen.model).hint_text("из настроек Qwen"));
        });
        messages(ui, errors, Field::Qwen(QwenField::Model));
        field_row(ui, "Одобрения", |ui| {
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
        field_row(ui, "Base URL", |ui| {
            ui.add(input(&mut self.draft.qwen.base_url).hint_text("собственная настройка qwen"));
        });
        messages(ui, errors, Field::Qwen(QwenField::BaseUrl));
        field_row(ui, "API-ключ", |ui| {
            ui.add(
                input(&mut self.draft.qwen.api_key)
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
        field_row(ui, "Путь к hermes", |ui| {
            ui.add(input(&mut self.draft.hermes.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.hermes.cli)
            {
                self.draft.hermes.cli = path;
            }
        });
        messages(ui, errors, Field::Hermes(HermesField::Cli));
        field_row(ui, "Профиль", |ui| {
            ui.add(input(&mut self.draft.hermes.profile).hint_text("активный профиль Hermes"));
        });
        messages(ui, errors, Field::Hermes(HermesField::Profile));
        field_row(ui, "Модель", |ui| {
            ui.add(input(&mut self.draft.hermes.model).hint_text("из настроек Hermes"));
        });
        messages(ui, errors, Field::Hermes(HermesField::Model));
        ui.label("Вход и провайдер настраиваются в Hermes (`hermes setup`). Хаб запускает `hermes acp`, не меняя профиль и его секреты. Новые сообщения во время работы ждут следующего хода.");
    }

    fn opencode(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.heading("OpenCode");
        field_row(ui, "Путь к opencode", |ui| {
            ui.add(input(&mut self.draft.opencode.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.opencode.cli)
            {
                self.draft.opencode.cli = path;
            }
        });
        messages(ui, errors, Field::Opencode(OpencodeField::Cli));
        field_row(ui, "Модель", |ui| {
            ui.add(
                input(&mut self.draft.opencode.model)
                    .hint_text("провайдер/модель из настроек OpenCode"),
            );
        });
        messages(ui, errors, Field::Opencode(OpencodeField::Model));
        ui.label("Вход и провайдеры настраиваются в OpenCode (`opencode auth login`). Хаб запускает `opencode acp`, не меняя его конфигурацию и секреты. Новые сообщения во время работы ждут следующего хода.");
    }

    fn timeouts(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.strong("Таймауты");
        field_row(ui, "Ожидание подтверждения, с", |ui| {
            ui.add(input(&mut self.draft.approval_timeout));
        });
        messages(ui, errors, Field::ApprovalTimeout);
        field_row(ui, "Фоновые задачи, с", |ui| {
            ui.add(input(&mut self.draft.background_timeout));
        });
        messages(ui, errors, Field::BackgroundTimeout);
    }

    fn application(&mut self, ui: &mut egui::Ui) {
        self.scale.show(ui);
        ui.add_space(20.0);
        ui.strong("Оформление");
        ui.label("Тема меняется автоматически вслед за настройками системы.");
        ui.weak(if ui.visuals().dark_mode {
            "Сейчас используется тёмная тема"
        } else {
            "Сейчас используется светлая тема"
        });
        ui.add_space(20.0);
        if let Some(control) = &mut self.autostart {
            control.show(ui);
            ui.add_space(20.0);
        }
        ui.strong("Обновления");
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
                    self.submitted = Some(self.draft.clone());
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

    fn render(
        ctx: &egui::Context,
        form: &mut SettingsForm,
        size: egui::Vec2,
        events: Vec<egui::Event>,
        commands: &mut Vec<Command>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                    form.show(ui, &home_or_root(), 0, &mut |command| commands.push(command));
                });
            },
        );
        output.textures_delta.clear();
        output
    }

    fn has_label(output: &egui::FullOutput, label: &str) -> bool {
        output.shapes.iter().any(|shape| {
            matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.text() == label)
        })
    }

    fn click_label(
        ctx: &egui::Context,
        form: &mut SettingsForm,
        label: &str,
        commands: &mut Vec<Command>,
    ) {
        let size = egui::vec2(960.0, 900.0);
        render(ctx, form, size, Vec::new(), commands);
        let output = render(ctx, form, size, Vec::new(), commands);
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == label => {
                    Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing label: {label}"));
        for pressed in [true, false] {
            render(
                ctx,
                form,
                size,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                commands,
            );
        }
    }

    fn configured_form() -> SettingsForm {
        let mut form = SettingsForm::default();
        form.sync(Some(&Draft {
            token: "123:test-token".to_owned(),
            chat: "0".to_owned(),
            users: "1".to_owned(),
            workspace_root: home_or_root().display().to_string(),
            ..Draft::default()
        }));
        form
    }

    #[test]
    fn undo_history_does_not_cross_agent_editors() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = configured_form();
        form.draft.model = "claude-original".into();
        form.draft.codex.model = "codex-original".into();
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Агенты", &mut commands);
        click_label(&ctx, &mut form, "Claude Code", &mut commands);
        click_label(&ctx, &mut form, "claude-original", &mut commands);
        let command = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: command,
        };
        render(
            &ctx,
            &mut form,
            egui::vec2(960.0, 900.0),
            vec![key(egui::Key::A), egui::Event::Text("claude-edited".into())],
            &mut commands,
        );
        assert_eq!(form.draft.model, "claude-edited");
        click_label(&ctx, &mut form, "Codex", &mut commands);
        click_label(&ctx, &mut form, "codex-original", &mut commands);
        render(&ctx, &mut form, egui::vec2(960.0, 900.0), vec![key(egui::Key::Z)], &mut commands);
        assert_eq!(form.draft.codex.model, "codex-original");
    }

    #[test]
    fn navigation_preserves_edits_and_save_includes_hidden_pages() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = configured_form();
        let mut commands = Vec::new();
        form.apply(LogAction::UseChat(-100));
        click_label(&ctx, &mut form, "Агенты", &mut commands);
        click_label(&ctx, &mut form, "Codex", &mut commands);
        form.draft.codex.model = "custom-model".into();
        click_label(&ctx, &mut form, "Рабочие каталоги", &mut commands);
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), vec![], &mut commands);
        assert!(has_label(&output, "Каталог снимков"));
        assert!(!has_label(&output, "Токен бота"));
        click_label(&ctx, &mut form, "Сохранить", &mut commands);
        assert!(
            matches!(commands.as_slice(), [Command::Save(saved)] if saved.chat == "-100" && saved.codex.model == "custom-model")
        );
    }

    #[test]
    fn hidden_agent_error_blocks_save_and_opens_the_right_editor() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = configured_form();
        form.draft.qwen.base_url = "not-a-url".into();
        let mut commands = Vec::new();
        let count = form.check(&home_or_root()).0.len();
        assert!(count > 0);
        click_label(&ctx, &mut form, "Сохранить", &mut commands);
        assert!(commands.is_empty());
        click_label(&ctx, &mut form, &format!("Исправить ошибки ({count})"), &mut commands);
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), vec![], &mut commands);
        assert!(has_label(&output, "Путь к qwen"));
        assert!(!has_label(&output, "Путь к claude"));
    }

    #[test]
    fn cancel_during_save_keeps_the_restored_old_draft_after_save_finishes() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = configured_form();
        let old = form.draft.clone();
        form.draft.model = "first".to_owned();
        let submitted = form.draft.clone();
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Сохранить", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::Save(_)]));
        form.set_saving(true);
        click_label(&ctx, &mut form, "Отменить изменения", &mut commands);
        assert_eq!(form.draft, old);
        form.sync(Some(&submitted));
        form.set_saving(false);
        assert_eq!(form.draft, old);
        assert!(form.has_unsaved());
    }

    #[test]
    fn submitted_draft_is_tracked_even_if_save_finishes_before_saving_status() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = configured_form();
        let old = form.draft.clone();
        form.draft.model = "first".to_owned();
        let submitted = form.draft.clone();
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Сохранить", &mut commands);
        click_label(&ctx, &mut form, "Отменить изменения", &mut commands);
        form.sync(Some(&submitted));
        assert_eq!(form.draft, old);
        assert!(form.has_unsaved());
    }

    #[test]
    fn unchanged_submitted_draft_becomes_the_saved_baseline() {
        let mut form = configured_form();
        form.draft.model = "first".to_owned();
        let submitted = form.draft.clone();
        form.set_saving(true);
        form.sync(Some(&submitted));
        form.set_saving(false);
        assert_eq!(form.draft, submitted);
        assert!(!form.has_unsaved());
    }

    #[test]
    fn editing_while_a_save_is_running_keeps_the_newer_draft() {
        let mut form = configured_form();
        form.draft.model = "first".to_owned();
        let saved_in_flight = form.draft.clone();
        form.set_saving(true);
        form.draft.model = "second".to_owned();
        form.sync(Some(&saved_in_flight));
        form.set_saving(false);
        assert_eq!(form.draft.model, "second");
        assert!(form.has_unsaved());
    }

    #[test]
    fn save_button_is_disabled_while_a_save_is_in_progress() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = configured_form();
        form.draft.model = "changed".to_owned();
        form.set_saving(true);
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Сохранить", &mut commands);
        assert!(commands.is_empty());
    }

    #[test]
    fn parsing_form_does_not_probe_workspace_filesystem() {
        let mut form = configured_form();
        form.draft.workspace_root =
            std::env::temp_dir().join("missing-workspace-for-form").display().to_string();
        assert!(form.check(&home_or_root()).0.is_empty());
    }

    #[test]
    fn unsaved_indicator_tracks_draft_against_saved_settings() {
        let mut form = SettingsForm::default();
        let draft = Draft::default();
        form.sync(Some(&draft));
        assert!(!form.has_unsaved());
        form.draft.chat = "-200".into();
        assert!(form.has_unsaved());
    }

    #[test]
    fn cancel_reverts_edits_across_sections_and_log_action_returns_to_telegram() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = configured_form();
        let mut commands = Vec::new();
        form.draft.codex.model = "edited".into();
        click_label(&ctx, &mut form, "Проекты", &mut commands);
        form.apply(LogAction::UseChat(-100));
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), vec![], &mut commands);
        assert!(has_label(&output, "Токен бота"));
        click_label(&ctx, &mut form, "Отменить изменения", &mut commands);
        click_label(&ctx, &mut form, "Сохранить", &mut commands);
        assert!(commands.is_empty());
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), vec![], &mut commands);
        assert!(has_label(&output, "Нет несохранённых изменений"));
        assert_eq!(form.draft, form.synced.unwrap());
    }

    #[test]
    fn initial_page_only_shows_telegram_fields() {
        let ctx = egui::Context::default();
        let mut form = SettingsForm::default();
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 4000.0),
                )),
                ..Default::default()
            },
            |ui| form.show(ui, Path::new("/"), 0, &mut |_| {}),
        );
        output.textures_delta.clear();
        let labels: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect();
        assert!(labels.contains(&"Токен бота"));
        assert!(!labels.contains(&"Каталог снимков"), "unrelated workspace editor is visible");
        assert!(!labels.contains(&"Путь к claude"), "unrelated agent editor is visible");
    }

    #[test]
    fn speech_settings_show_provider_specific_configuration_without_losing_it() {
        let ctx = egui::Context::default();
        let mut form = SettingsForm::default();
        form.draft.speech.api_key = "sk-hidden-speech".to_owned();
        form.draft.speech.model_path = "/models/whisper.bin".to_owned();
        for (provider, expected) in [
            (SpeechProvider::Disabled, "Распознавание голосовых сообщений выключено."),
            (SpeechProvider::Local, "Файл модели Whisper.cpp"),
            (SpeechProvider::Api, "API-ключ распознавания"),
        ] {
            form.draft.speech.provider = provider;
            let before = form.draft.speech.clone();
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 1000.0),
                    )),
                    ..Default::default()
                },
                |ui| form.speech(ui, &[]),
            );
            output.textures_delta.clear();
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.text() == expected)));
            assert!(!output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.text().contains("sk-hidden-speech"))));
            assert_eq!(form.draft.speech, before);
        }
    }

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
        form.section = Section::Application;
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
        let mut form = SettingsForm { section: Section::Workspace, ..SettingsForm::default() };
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

    #[rstest]
    #[case(BackendKind::Claude, "Путь к claude")]
    #[case(BackendKind::Codex, "Путь к codex")]
    #[case(BackendKind::Qwen, "Путь к qwen")]
    #[case(BackendKind::Hermes, "Профиль")]
    #[case(BackendKind::Opencode, "Путь к opencode")]
    fn selected_agent_settings_are_visible(#[case] backend: BackendKind, #[case] expected: &str) {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = SettingsForm {
            section: Section::Agents,
            agent_page: AgentPage::Backend(backend),
            ..SettingsForm::default()
        };
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), Vec::new(), &mut Vec::new());
        assert!(has_label(&output, expected), "missing editor: {expected}");
        if backend != BackendKind::Claude {
            assert!(!has_label(&output, "Путь к claude"));
        }
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
        super::super::theme::configure(&ctx);
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

    #[test]
    fn scale_controls_work_at_maximum_zoom_and_minimum_window() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = SettingsForm { section: Section::Application, ..SettingsForm::default() };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ui.toml");
        super::super::scale::Scale::new(125).unwrap().save(&path).unwrap();
        form.set_scale_path(path.clone());
        let mut commands = Vec::new();
        // The top tabs occupy about 50 points after zooming a 640 × 360 window to 150%.
        let size = egui::vec2(640.0 / 1.5, 360.0 / 1.5 - 50.0);
        for _ in 0..2 {
            render(&ctx, &mut form, size, vec![], &mut commands);
        }
        for label in ["Сбросить до 100%", "Сохранить масштаб"] {
            let mut found = None;
            for _ in 0..20 {
                let output = render(&ctx, &mut form, size, vec![], &mut commands);
                found = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => {
                        let rect = text.galley.rect.translate(text.pos.to_vec2());
                        shape.clip_rect.contains_rect(rect).then_some(rect.center())
                    }
                    _ => None,
                });
                if found.is_some() {
                    break;
                }
                render(
                    &ctx,
                    &mut form,
                    size,
                    vec![
                        egui::Event::PointerMoved(egui::pos2(200.0, 75.0)),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: egui::vec2(0.0, -20.0),
                            phase: egui::TouchPhase::Move,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    &mut commands,
                );
            }
            let position = found.unwrap_or_else(|| panic!("{label} is not reachable"));
            for pressed in [true, false] {
                render(
                    &ctx,
                    &mut form,
                    size,
                    vec![
                        egui::Event::PointerMoved(position),
                        egui::Event::PointerButton {
                            pos: position,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                    &mut commands,
                );
            }
        }
        assert_eq!(super::super::scale::Scale::load(&path).unwrap().percent(), 100);
        assert!(commands.is_empty());
    }

    #[rstest]
    #[case(egui::vec2(960.0, 640.0))]
    #[case(egui::vec2(640.0, 360.0))]
    fn settings_actions_remain_visible(#[case] size: egui::Vec2) {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
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
