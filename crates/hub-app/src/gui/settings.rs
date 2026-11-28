//! The «Настройки» tab: a draft of every setting, saved only when it parses.

mod actions;
mod navigation;
mod projects;
mod setup;

use crate::gui::theme;
use kylig_ui::{Button, ProgressRing, Select, SelectItem, Switch, TextField};
pub(super) use navigation::agent_choice;
use navigation::{AgentPage, Section};

use std::path::{Path, PathBuf};

use eframe::egui;
use hub_core::actions::{ActionDraft, MAX_ACTIONS};
use hub_core::doctor::{CheckDraft, MAX_CHECKS};
use hub_core::domain::BackendKind;
use hub_core::projects::{MAX_PROJECTS, ProjectDraft};
use hub_core::settings::{
    ApiEndpoint, Approval, CodexField, Draft, Field, FieldError, HermesApproval, HermesField,
    OpencodeField, PermissionMode, PiApproval, PiField, PiThinking, ProgressMode, QwenApproval,
    QwenField, Sandbox, Settings, UpdateCheck,
};
use hub_core::speech::SpeechProvider;

use crate::gui::log::LogAction;
use crate::gui::look::{add_user, telegram_changed, use_chat};
use crate::supervisor::{Command, Snapshot};

pub(super) enum SystemPage {
    Diagnostics { unsaved: bool },
    Updates,
}

#[derive(Default, PartialEq, Eq)]
enum SaveProgress {
    #[default]
    Idle,
    Saving,
    Confirmed,
}

#[derive(Default)]
pub struct SettingsForm {
    section: Section,
    agent_page: AgentPage,
    autostart: Option<super::autostart::Control>,
    scale: super::scale::Control,
    draft: Draft,
    synced: Option<Draft>,
    project: Option<String>,
    actions: actions::Editor,
    setup: Option<setup::Wizard>,
    setup_complete: bool,
    confirming: bool,
    save_progress: SaveProgress,
    submitted: Option<Draft>,
    deleting_project: Option<PendingDelete>,
    project_deletion: Option<(String, tokio::sync::oneshot::Receiver<Result<String, String>>)>,
    project_notice: Option<Result<String, String>>,
}

/// One open «Удалить проект…» modal: the profile being removed and the confirmation input.
#[derive(Clone, Default)]
struct PendingDelete {
    id: String,
    name: String,
    cwd: String,
    typed: String,
}

/// Preview of the cascade size: matches exactly the shapes the supervisor will forget,
/// so the modal counter and the completion summary cannot disagree.
fn project_task_count(snapshot: &Snapshot, draft: &Draft, home: &Path, cwd: &str) -> usize {
    let root = hub_core::workspace::expand_home(&draft.workspace_root, home);
    let shapes = crate::supervisor::task_path_shapes(&root, home, cwd);
    snapshot
        .task_index
        .as_ref()
        .map_or(0, |rows| rows.iter().filter(|row| shapes.contains(&row.project)).count())
}

fn errors_for(errors: &[FieldError], field: Field) -> impl Iterator<Item = &str> {
    errors.iter().filter(move |error| error.field == field).map(|error| error.message.as_str())
}

fn messages(ui: &mut egui::Ui, errors: &[FieldError], field: Field) {
    for message in errors_for(errors, field) {
        ui.colored_label(ui.visuals().error_fg_color, message);
    }
}

fn input(value: &mut String) -> TextField<'_> {
    TextField::new(value)
}

fn text_field(ui: &mut egui::Ui, label: &str, field: TextField<'_>) {
    ui.add(field.id_salt(label).label(label).width(ui.available_width().min(520.0)));
}

fn telegram_fields(ui: &mut egui::Ui, draft: &mut Draft, errors: &[FieldError]) {
    text_field(
        ui,
        "Токен бота",
        input(&mut draft.token).password(true).placeholder("Токен от BotFather"),
    );
    messages(ui, errors, Field::Token);
    text_field(ui, "chat_id группы", input(&mut draft.chat).placeholder("−100…"));
    messages(ui, errors, Field::Chat);
    text_field(ui, "Разрешённые user_id", input(&mut draft.users).placeholder("73011, 73012"));
    messages(ui, errors, Field::Users);
}

fn choice<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    heading: &str,
    current: &mut T,
    options: impl IntoIterator<Item = T>,
    label: impl Fn(T) -> &'static str,
) {
    let items: Vec<_> =
        options.into_iter().map(|item| SelectItem::new(item, label(item))).collect();
    ui.add(
        Select::new(current, &items)
            .id_salt(heading)
            .label(heading)
            .search(false)
            .width(ui.available_width().min(380.0)),
    );
}

fn path_field(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    hint: &str,
    choose: fn(&str) -> Option<String>,
) {
    ui.push_id(label, |ui| {
        let width = (ui.available_width() - 48.0 - ui.spacing().item_spacing.x).clamp(60.0, 520.0);
        ui.horizontal_wrapped(|ui| {
            ui.add(input(value).id_salt("path").label(label).placeholder(hint).width(width));
            if ui.add(kylig_ui::IconButton::new(kylig_icons::FOLDER_OPEN, "Выбрать…")).clicked()
                && let Some(selected) = choose(value)
            {
                *value = selected;
            }
        });
    });
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
        if saving {
            self.save_progress = SaveProgress::Saving;
        } else if self.save_progress == SaveProgress::Saving {
            self.save_progress = SaveProgress::Idle;
        }
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
        if let Some((id, mut receipt)) = self.project_deletion.take() {
            match receipt.try_recv() {
                Ok(Ok(summary)) => {
                    // The profile is gone from saved settings; drop the card from the draft.
                    self.draft.projects.retain(|saved| saved.id != id);
                    self.project_notice = Some(Ok(summary));
                }
                Ok(Err(error)) => self.project_notice = Some(Err(error)),
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    self.project_deletion = Some((id, receipt));
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.project_notice = Some(Err(
                        "Связь с ядром потеряна; повторите удаление после перезапуска приложения."
                            .to_owned(),
                    ));
                }
            }
        }
    }
    /// Adopts newly saved settings unless the user is in the middle of editing.
    pub fn sync(&mut self, saved: Option<&Draft>) {
        if self.synced.as_ref() == saved {
            return;
        }
        if saved.is_some() && self.submitted.as_ref() == saved {
            self.save_progress = SaveProgress::Confirmed;
        } else if self.save_progress == SaveProgress::Confirmed {
            self.save_progress = SaveProgress::Idle;
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
        if let (Some(previous), Some(next)) = (&self.synced, saved) {
            // A boolean has no third conflicting value: if locally changed and externally
            // changed from the same baseline, both edits selected the same value.
            if self.draft.memory == previous.memory {
                self.draft.memory = next.memory;
            }
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

    pub(super) fn open_application(&mut self) {
        self.section = Section::Application;
    }

    pub(super) fn application_open(&self) -> bool {
        self.section == Section::Application && self.setup.is_none()
    }

    pub(super) fn diagnostics_open(&self) -> bool {
        self.section == Section::Diagnostics && self.setup.is_none()
    }

    /// Parses the draft once: the field errors and, when it parses, the settings.
    fn check(&self, home: &Path) -> (Vec<FieldError>, Option<Settings>) {
        match self.draft.parse(home) {
            Ok(settings) => (Vec::new(), Some(settings)),
            Err(errors) => (errors, None),
        }
    }

    #[cfg(test)]
    fn show(
        &mut self,
        ui: &mut egui::Ui,
        home: &Path,
        sessions: usize,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        self.show_with_system_controls(ui, home, sessions, snapshot, send, |_, _, _| None);
    }

    pub(super) fn show_with_system_controls<S: FnMut(Command)>(
        &mut self,
        ui: &mut egui::Ui,
        home: &Path,
        sessions: usize,
        snapshot: &Snapshot,
        send: &mut S,
        mut system_controls: impl FnMut(&mut egui::Ui, SystemPage, &mut S) -> Option<LogAction>,
    ) {
        if self.setup.is_some() {
            self.setup_page(ui, home, snapshot, send);
            return;
        }
        let (errors, parsed) = self.check(home);
        let wide = ui.available_width() >= 780.0;
        let compact = ui.available_height() < 280.0;
        if wide {
            ui.heading("Настройки");
            ui.add_space(12.0);
        } else {
            navigation::compact(ui, &mut self.section, &errors);
        }
        if wide || self.has_unsaved() || self.save_progress != SaveProgress::Idle {
            self.save_bar(ui, &errors, sessions, home, snapshot, send);
        }
        if !compact {
            ui.add_space(8.0);
        }
        let mut render = |ui: &mut egui::Ui, sidebar: bool| {
            if sidebar {
                navigation::sidebar(ui, &mut self.section, &errors);
            } else {
                let width = ui.available_width();
                egui::ScrollArea::vertical()
                    .id_salt(("settings_content", self.section))
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width((width - ui.spacing().scroll.allocated_width()).max(1.0));
                        if !compact {
                            ui.heading(self.section.title());
                            ui.weak(self.section.description());
                            ui.add_space(12.0);
                        }
                        if self.diagnostics_open() {
                            let page = SystemPage::Diagnostics { unsaved: self.has_unsaved() };
                            if let Some(action) = system_controls(ui, page, send) {
                                self.apply(action);
                            }
                            return;
                        }
                        if self.section == Section::Agents {
                            self.agent_page.show(ui, &errors);
                            ui.add_space(8.0);
                        }
                        ui.push_id((self.section, self.agent_page), |ui| {
                            if self.section == Section::Application {
                                self.application(ui, |ui| {
                                    system_controls(ui, SystemPage::Updates, send);
                                });
                            } else if matches!(self.section, Section::Projects | Section::Actions)
                                || (self.section == Section::Agents
                                    && self.agent_page == AgentPage::General)
                            {
                                self.page(ui, &errors, parsed.as_ref(), home);
                            } else {
                                theme::card(ui).show(ui, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    self.page(ui, &errors, parsed.as_ref(), home);
                                });
                            }
                        });
                    });
            }
        };
        if wide {
            theme::workspace_panes(ui, "settings-panes", render);
        } else {
            render(ui, false);
        }
        self.confirm(ui, sessions, send);
        self.confirm_delete_project(ui, snapshot, home, send);
    }

    fn page(
        &mut self,
        ui: &mut egui::Ui,
        errors: &[FieldError],
        parsed: Option<&Settings>,
        home: &Path,
    ) {
        match self.section {
            Section::Telegram => self.telegram(ui, errors),
            Section::Workspace => self.workspace(ui, errors),
            Section::Projects => self.projects(ui, errors, home),
            Section::Actions => self.actions(ui, errors),
            Section::Speech => self.speech(ui, errors),
            // These pages render live system controls outside the draft editor.
            Section::Diagnostics | Section::Application => {}
            Section::Agents => match self.agent_page {
                AgentPage::General => {
                    theme::card(ui).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        self.agent(ui, errors);
                    });
                    theme::card(ui).show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        self.timeouts(ui, errors);
                        ui.add(
                            Switch::new(&mut self.draft.memory.enabled)
                                .label("Подключать память к агентам по умолчанию"),
                        );
                        ui.small(hub_telegram::memory::POLICY);
                    });
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
                AgentPage::Backend(BackendKind::Pi) => self.pi(ui, errors),
            },
        }
    }

    fn save_bar(
        &mut self,
        ui: &mut egui::Ui,
        errors: &[FieldError],
        sessions: usize,
        home: &Path,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        let changed = self.has_unsaved();
        let compact = ui.available_height() < 250.0;
        let mut bar = |ui: &mut egui::Ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        errors.is_empty() && changed && self.save_progress != SaveProgress::Saving,
                        Button::new("Сохранить").primary(),
                    )
                    .clicked()
                {
                    if telegram_changed(self.synced.as_ref(), &self.draft)
                        && (sessions > 0
                            || matches!(snapshot.bot, crate::supervisor::BotStatus::Running { .. }))
                    {
                        self.confirming = true;
                    } else {
                        self.submitted = Some(self.draft.clone());
                        send(Command::Save(Box::new(self.draft.clone())));
                    }
                }
                if ui.add_enabled(changed, Button::new("Отменить изменения")).clicked()
                {
                    self.draft = self.synced.clone().unwrap_or_default();
                    self.confirming = false;
                    if self.save_progress == SaveProgress::Confirmed {
                        self.save_progress = SaveProgress::Idle;
                    }
                }
                if self.save_progress == SaveProgress::Saving
                    && snapshot.save_status != crate::supervisor::SaveStatus::Uncertain
                {
                    ui.add(ProgressRing::indeterminate().size(18.0));
                    ui.weak("Сохраняем настройки…");
                } else if !compact {
                    ui.weak(if changed {
                        "Есть несохранённые изменения"
                    } else if self.save_progress == SaveProgress::Confirmed {
                        "Настройки сохранены"
                    } else {
                        "Все изменения сохранены"
                    });
                }
                if compact && let Some(error) = errors.first() {
                    let label = format!("Ошибки ({})", errors.len());
                    if ui.add(Button::new(label).ghost()).on_hover_text(&error.message).clicked() {
                        self.open_error(error.field, home);
                    }
                }
            });
            if !compact && let Some(error) = errors.first() {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, &error.message);
                    let label = format!("Исправить ошибки ({})", errors.len());
                    if ui.add(Button::new(label).ghost()).on_hover_text(&error.message).clicked() {
                        self.open_error(error.field, home);
                    }
                });
            }
        };
        if compact {
            egui::Panel::bottom("settings_actions").frame(egui::Frame::NONE).show(ui, bar);
        } else {
            bar(ui);
        }
    }

    fn open_error(&mut self, field: Field, home: &Path) {
        self.section = Section::for_field(field);
        if let Some(page) = AgentPage::for_field(field) {
            self.agent_page = page;
        } else if field == Field::Actions {
            // The core validates both individual rows and cross-row uniqueness.
            let id = (1..=self.draft.actions.len()).find_map(|n| {
                let rows = self.draft.actions.get(..n)?;
                hub_core::actions::parse(rows, home).err()?;
                Some(rows.last()?.id.clone())
            });
            self.actions.edit(id);
        } else if field == Field::Projects {
            let actions = hub_core::actions::parse(&self.draft.actions, home).unwrap_or_default();
            self.project = (1..=self.draft.projects.len()).find_map(|n| {
                let rows = self.draft.projects.get(..n)?;
                hub_core::projects::parse(rows, &actions).err()?;
                Some(rows.last()?.id.clone())
            });
        }
    }

    fn speech(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        choice(
            ui,
            "Способ распознавания",
            &mut self.draft.speech.provider,
            SpeechProvider::ALL,
            SpeechProvider::label,
        );
        ui.weak("Смена способа распознавания сохраняет настройки обоих провайдеров.");
        match self.draft.speech.provider {
            SpeechProvider::Disabled => {
                ui.weak("Распознавание голосовых сообщений выключено.");
            }
            SpeechProvider::Local => {
                ui.label("Установите Whisper.cpp и ffmpeg самостоятельно и скачайте модель. Аудио распознаётся на этом компьютере.");
                for (label, path, hint) in [
                    ("Путь к whisper-cli", &mut self.draft.speech.whisper_path, "whisper-cli"),
                    (
                        "Файл модели Whisper.cpp",
                        &mut self.draft.speech.model_path,
                        "~/Models/ggml-base.bin",
                    ),
                    ("Путь к ffmpeg", &mut self.draft.speech.ffmpeg_path, "ffmpeg"),
                ] {
                    path_field(ui, label, path, hint, pick_file);
                }
                ui.small("Для команд допустимо имя из PATH или полный путь; для модели нужен полный путь.");
            }
            SpeechProvider::Api => {
                ui.label("Аудио отправляется на указанный API-адрес. Провайдер может взимать плату за распознавание.");
                text_field(
                    ui,
                    "Базовый URL API",
                    input(&mut self.draft.speech.api_url).placeholder("https://api.openai.com/v1"),
                );
                ui.small("Базовый URL без /audio/transcriptions. HTTPS; HTTP — только localhost или loopback IP.");
                text_field(
                    ui,
                    "Модель распознавания",
                    input(&mut self.draft.speech.api_model).placeholder("whisper-1"),
                );
                text_field(
                    ui,
                    "API-ключ распознавания",
                    input(&mut self.draft.speech.api_key)
                        .password(true)
                        .placeholder("Ключ провайдера"),
                );
                ui.small("Отдельный ключ хранится в системном хранилище ключей.");
            }
        }
        for error in errors.iter().filter(|error| matches!(error.field, Field::Speech(_))) {
            ui.colored_label(ui.visuals().error_fg_color, &error.message);
        }
    }

    fn telegram(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        telegram_fields(ui, &mut self.draft, errors);
        ui.weak("Для локальной работы оставьте все три поля Telegram пустыми.");
        ui.small("Изменение токена, chat_id или user_id перезапускает бота и останавливает сессии и пользовательские действия.");
        choice(
            ui,
            "Прогресс задач в Telegram",
            &mut self.draft.progress_mode,
            [ProgressMode::Compact, ProgressMode::Detailed],
            |mode| match mode {
                ProgressMode::Compact => "Компактный",
                ProgressMode::Detailed => "Подробный",
            },
        );
        ui.weak("Компактный прогресс объединяет события в одной карточке. Режим применяется к следующим запускам.");
        if [&self.draft.token, &self.draft.chat, &self.draft.users]
            .iter()
            .any(|s| !s.trim().is_empty())
            && ui.add(Button::new("Очистить настройки Telegram")).clicked()
        {
            self.draft.token.clear();
            self.draft.chat.clear();
            self.draft.users.clear();
        }
    }

    fn workspace(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        path_field(
            ui,
            "Корень рабочих каталогов",
            &mut self.draft.workspace_root,
            "~/Projects",
            pick_folder,
        );
        messages(ui, errors, Field::WorkspaceRoot);
        path_field(
            ui,
            "Каталог снимков",
            &mut self.draft.snapshots_dir,
            "~/.agent-hub",
            pick_folder,
        );
        messages(ui, errors, Field::SnapshotsDir);
    }

    fn agent(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.strong("Распределение ролей");
        agent_choice(ui, "Агент для новых тем", &mut self.draft.default_backend);
        ui.label("Используется в /new без имени агента; /backend меняет агента в теме.");
        messages(ui, errors, Field::DefaultBackend);
        for (label, role) in [
            ("Исполнитель по умолчанию", &mut self.draft.roles.implementer),
            ("Ревьюер по умолчанию", &mut self.draft.roles.reviewer),
        ] {
            agent_choice(ui, label, role);
        }
        ui.label(
            "Используется проектами в режиме „Наследовать общие роли“. Команда проверки остаётся своей у проекта.",
        );
        messages(ui, errors, Field::Role("implementer"));
        messages(ui, errors, Field::Role("reviewer"));
        if self.draft.roles.implementer == self.draft.roles.reviewer {
            let tone = super::theme::tone_colors(ui, super::theme::Tone::Warning);
            ui.colored_label(
                tone.1,
                "Исполнитель и ревьюер совпадают: проверка станет самопроверкой.",
            );
        }
    }

    fn claude(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        path_field(ui, "Путь к claude", &mut self.draft.cli, "из PATH", pick_file);
        messages(ui, errors, Field::Cli);
        text_field(
            ui,
            "Модель",
            input(&mut self.draft.model).placeholder("из настроек Claude Code"),
        );
        messages(ui, errors, Field::Model);
        choice(
            ui,
            "Режим разрешений",
            &mut self.draft.permission_mode,
            PermissionMode::ALL,
            PermissionMode::wire,
        );
        if self.draft.permission_mode == PermissionMode::BypassPermissions {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "bypassPermissions: агент выполняет любые инструменты без подтверждения.",
            );
        }
        text_field(
            ui,
            "Лимит на задачу, $",
            input(&mut self.draft.budget).placeholder("без лимита"),
        );
        messages(ui, errors, Field::Budget);
    }

    fn codex(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        path_field(ui, "Путь к codex", &mut self.draft.codex.cli, "из PATH", pick_file);
        messages(ui, errors, Field::Codex(CodexField::Cli));
        text_field(
            ui,
            "Модель",
            input(&mut self.draft.codex.model).placeholder("из настроек Codex"),
        );
        messages(ui, errors, Field::Codex(CodexField::Model));
        choice(ui, "Песочница", &mut self.draft.codex.sandbox, Sandbox::ALL, Sandbox::wire);
        if self.draft.codex.sandbox == Sandbox::DangerFullAccess {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "danger-full-access: команды Codex выполняются без песочницы ОС.",
            );
        }
        choice(ui, "Одобрения", &mut self.draft.codex.approval, Approval::ALL, Approval::wire);
        if self.draft.codex.approval == Approval::Never {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "never: Codex ничего не спрашивает перед действиями.",
            );
        }
        text_field(
            ui,
            "API-ключ OpenAI",
            input(&mut self.draft.codex.api_key)
                .password(true)
                .placeholder("вход через codex login"),
        );
        messages(ui, errors, Field::Codex(CodexField::ApiKey));
        ui.label(
            "Без ключа используется вход `codex login`. Ключ хранится в системном хранилище \
                 ключей и не заменяет вход по подписке ChatGPT.",
        );
    }

    fn qwen(&mut self, ui: &mut egui::Ui, errors: &[FieldError], endpoint: Option<&ApiEndpoint>) {
        path_field(ui, "Путь к qwen", &mut self.draft.qwen.cli, "из PATH", pick_file);
        messages(ui, errors, Field::Qwen(QwenField::Cli));
        text_field(ui, "Модель", input(&mut self.draft.qwen.model).placeholder("из настроек Qwen"));
        messages(ui, errors, Field::Qwen(QwenField::Model));
        choice(
            ui,
            "Одобрения",
            &mut self.draft.qwen.approval,
            QwenApproval::ALL,
            QwenApproval::wire,
        );
        if self.draft.qwen.approval == QwenApproval::Yolo {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "yolo: Qwen выполняет любые инструменты без подтверждения.",
            );
        }
        text_field(
            ui,
            "Базовый URL API",
            input(&mut self.draft.qwen.base_url).placeholder("собственная настройка qwen"),
        );
        messages(ui, errors, Field::Qwen(QwenField::BaseUrl));
        text_field(
            ui,
            "API-ключ Qwen",
            input(&mut self.draft.qwen.api_key)
                .password(true)
                .placeholder("собственная настройка qwen"),
        );
        messages(ui, errors, Field::Qwen(QwenField::ApiKey));
        if endpoint.is_some_and(ApiEndpoint::is_cleartext_remote) {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "http:// к другому компьютеру: ключ уходит в сеть открытым текстом.",
            );
        }
        ui.label(
            "Без адреса и ключа Qwen использует собственную настройку (`qwen` › /auth, \
             ~/.qwen/settings.json). Адрес OpenAI-совместимый; ключ хранится в системном \
             хранилище ключей и передаётся только процессу qwen.",
        );
    }

    fn hermes(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        path_field(ui, "Путь к hermes", &mut self.draft.hermes.cli, "из PATH", pick_file);
        messages(ui, errors, Field::Hermes(HermesField::Cli));
        text_field(
            ui,
            "Профиль",
            input(&mut self.draft.hermes.profile).placeholder("активный профиль Hermes"),
        );
        messages(ui, errors, Field::Hermes(HermesField::Profile));
        text_field(
            ui,
            "Модель",
            input(&mut self.draft.hermes.model).placeholder("из настроек Hermes"),
        );
        messages(ui, errors, Field::Hermes(HermesField::Model));
        choice(
            ui,
            "Одобрения",
            &mut self.draft.hermes.approval,
            HermesApproval::ALL,
            HermesApproval::wire,
        );
        if self.draft.hermes.approval == HermesApproval::Yolo {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "yolo: Hermes выполняет опасные команды без подтверждения.",
            );
        }
        ui.label(
            "Режим применяется при каждом запуске сессии, включая восстановленную. Чувствительные \
             пути и правила запрета Hermes спрашивает всегда.",
        );
        ui.label("Вход и провайдер настраиваются в Hermes (`hermes setup`). Хаб запускает `hermes acp`, не меняя профиль и его секреты. Новые сообщения во время работы ждут следующего хода.");
    }

    fn opencode(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        path_field(ui, "Путь к opencode", &mut self.draft.opencode.cli, "из PATH", pick_file);
        messages(ui, errors, Field::Opencode(OpencodeField::Cli));
        text_field(
            ui,
            "Модель",
            input(&mut self.draft.opencode.model)
                .placeholder("провайдер/модель из настроек OpenCode"),
        );
        messages(ui, errors, Field::Opencode(OpencodeField::Model));
        ui.label("Вход и провайдеры настраиваются в OpenCode (`opencode auth login`). Хаб запускает `opencode acp`, не меняя его конфигурацию и секреты. Новые сообщения во время работы ждут следующего хода.");
    }

    fn pi(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        path_field(ui, "Путь к pi", &mut self.draft.pi.cli, "из PATH", pick_file);
        messages(ui, errors, Field::Pi(PiField::Cli));
        text_field(
            ui,
            "Модель",
            input(&mut self.draft.pi.model).placeholder("провайдер/модель из настроек Pi"),
        );
        messages(ui, errors, Field::Pi(PiField::Model));
        choice(
            ui,
            "Уровень размышлений",
            &mut self.draft.pi.thinking,
            PiThinking::ALL,
            PiThinking::label,
        );
        messages(ui, errors, Field::Pi(PiField::Thinking));
        ui.label(
            "Режим — это уровень размышлений. Модель объявляет свои уровни сама; недоступный \
             уровень хаб отклоняет до начала хода и показывает, какие доступны.",
        );
        choice(ui, "Одобрения", &mut self.draft.pi.approval, PiApproval::ALL, PiApproval::wire);
        messages(ui, errors, Field::Pi(PiField::Approval));
        if self.draft.pi.approval == PiApproval::Yolo {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "yolo: Pi редактирует файлы и выполняет команды без подтверждения.",
            );
        } else {
            ui.label("Pi сам ничего не спрашивает: хаб подтверждает bash, edit и write.");
        }
        ui.label(
            "Подписки и ключи остаются у Pi (`pi auth login`, `~/.pi/agent/auth.json`) — хаб их \
             не читает. Через мост хаба агент получает `send_file` и `ask_user`, а сообщение, \
             пришедшее во время работы, уточняет текущий ход.",
        );
    }

    fn timeouts(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.strong("Выполнение задач");
        text_field(ui, "Ожидание подтверждения, с", input(&mut self.draft.approval_timeout));
        messages(ui, errors, Field::ApprovalTimeout);
        text_field(ui, "Фоновые задачи, с", input(&mut self.draft.background_timeout));
        messages(ui, errors, Field::BackgroundTimeout);
    }

    fn application(&mut self, ui: &mut egui::Ui, updates: impl FnOnce(&mut egui::Ui)) {
        theme::card(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("Оформление");
            ui.weak(if ui.visuals().dark_mode {
                "Тёмная тема"
            } else {
                "Светлая тема"
            });
            self.scale.show(ui);
        });
        if let Some(control) = &mut self.autostart {
            theme::card(ui).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                control.show(ui);
            });
        }
        theme::card(ui).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.strong("Обновления");
            let mut check = self.draft.updates == UpdateCheck::Enabled;
            if ui.add(Switch::new(&mut check).label("Автоматически проверять обновления")).changed()
            {
                self.draft.updates =
                    if check { UpdateCheck::Enabled } else { UpdateCheck::Disabled };
            }
            updates(ui);
        });
        if self.synced.is_none() && ui.add(Button::new("Мастер первого запуска")).clicked()
        {
            self.open_setup();
        }
    }

    fn confirm(&mut self, ui: &egui::Ui, sessions: usize, send: &mut impl FnMut(Command)) {
        if !self.confirming {
            return;
        }
        let mut open = true;
        theme::dialog(ui, "restart", &mut open, "Перезапустить бота?").show(ui.ctx(), |ui| {
            if sessions == 0 {
                ui.label("Новые параметры доступа перезапустят бота. Продолжить?");
            } else {
                ui.label(format!("Сохранение перезапустит бота и прервёт активные сессии ({sessions}). Продолжить?"));
            }
            ui.horizontal_wrapped(|ui| {
                if ui.add(Button::new("Сохранить и перезапустить").primary()).clicked()
                {
                    self.submitted = Some(self.draft.clone());
                    send(Command::Save(Box::new(self.draft.clone())));
                    self.confirming = false;
                }
                if ui.add(Button::new("Отмена")).clicked() {
                    self.confirming = false;
                }
            });
        });
        if !open {
            self.confirming = false;
        }
    }

    /// Danger-тон и точный ввод названия: каскад необратим, молча он не запускается.
    fn confirm_delete_project(
        &mut self,
        ui: &egui::Ui,
        snapshot: &Snapshot,
        home: &Path,
        send: &mut impl FnMut(Command),
    ) {
        let Some(pending) = self.deleting_project.clone() else { return };
        let saved = self.synced.as_ref().unwrap_or(&self.draft);
        let tasks = project_task_count(snapshot, saved, home, &pending.cwd);
        let topics = snapshot
            .topics
            .iter()
            .filter(|view| view.session.preferences.project_id() == Some(pending.id.as_str()))
            .count();
        let mut typed = pending.typed.clone();
        let mut open = true;
        theme::dialog(ui, "delete-project", &mut open, "Удаление проекта").show(ui.ctx(), |ui| {
            let available = (ui.ctx().content_rect().height() - 180.0).max(60.0);
            egui::ScrollArea::vertical().id_salt("delete-impact").max_height(available).show(
                ui,
                |ui| {
                    ui.strong(format!("Удалить проект «{}» из хаба?", pending.name));
                    ui.label(format!("Каталог: {}", pending.cwd));
                    ui.label("Из хаба удаляются:");
                    ui.label("· профиль настроек и запись в списке последних проектов;");
                    ui.label(format!("· задачи и правила проекта (сейчас: {tasks});"));
                    ui.label("· память проекта в хранилище хаба;");
                    ui.label(format!(
                        "· привязка к темам (сейчас: {topics}) — сессии тем сохранятся."
                    ));
                    ui.label(format!("Каталог проекта {} не удаляется.", pending.cwd));
                    ui.label("Темы в Telegram и файлы проекта остаются на месте.");
                },
            );
            ui.add(
                input(&mut typed)
                    .id_salt("delete-project-confirm")
                    .label("Название для подтверждения")
                    .placeholder(&pending.name)
                    .width(ui.available_width()),
            );
            ui.horizontal_wrapped(|ui| {
                if ui.add(Button::new("Отмена")).clicked() {
                    self.deleting_project = None;
                }
                if ui
                    .add_enabled(
                        typed == pending.name,
                        Button::new("Удалить безвозвратно").danger(),
                    )
                    .clicked()
                {
                    let (reply, receipt) = tokio::sync::oneshot::channel();
                    self.project_deletion = Some((pending.id.clone(), receipt));
                    self.project_notice = None;
                    self.deleting_project = None;
                    send(Command::DeleteProject { id: pending.id.clone(), reply });
                }
            });
        });
        if !open {
            self.deleting_project = None;
        }
        if let Some(current) = &mut self.deleting_project {
            current.typed = typed;
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
    use hub_core::settings::AgentRoles;

    fn render(
        ctx: &egui::Context,
        form: &mut SettingsForm,
        size: egui::Vec2,
        events: Vec<egui::Event>,
        commands: &mut Vec<Command>,
    ) -> egui::FullOutput {
        let snapshot = Snapshot { saved: form.synced.clone(), ..Snapshot::default() };
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
                    form.show(ui, &home_or_root(), 0, &snapshot, &mut |command| {
                        commands.push(command);
                    });
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
        let position = (0..32)
            .find_map(|_| {
                let output = render(ctx, form, size, Vec::new(), commands);
                let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => {
                        let rect = text.galley.rect.translate(text.pos.to_vec2());
                        shape.clip_rect.contains_rect(rect).then_some(rect.center())
                    }
                    _ => None,
                });
                if pos.is_none() {
                    render(
                        ctx,
                        form,
                        size,
                        vec![
                            egui::Event::PointerMoved(egui::pos2(650.0, 700.0)),
                            egui::Event::MouseWheel {
                                source: egui::MouseWheelSource::Unknown,
                                unit: egui::MouseWheelUnit::Point,
                                delta: egui::vec2(0.0, -120.0),
                                phase: egui::TouchPhase::Move,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                        commands,
                    );
                }
                pos
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

    // Role buttons repeat the same agent names under each role heading.
    fn click_option(
        ctx: &egui::Context,
        form: &mut SettingsForm,
        heading: &str,
        label: &str,
        commands: &mut Vec<Command>,
    ) {
        let size = egui::vec2(960.0, 900.0);
        let output = render(ctx, form, size, vec![], commands);
        let mut labels = output.shapes.iter().filter_map(|shape| match &shape.shape {
            egui::Shape::Text(t) => Some(t),
            _ => None,
        });
        labels.find(|t| t.galley.text() == heading).expect("role heading");
        let option = labels.find(|t| t.galley.text() == label).expect("role option");
        let pos = option.galley.rect.translate(option.pos.to_vec2()).center();
        for pressed in [true, false] {
            render(
                ctx,
                form,
                size,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
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
    fn delete_project_modal_requires_the_typed_name_and_sends_the_command() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = SettingsForm::default();
        let mut saved = Draft {
            token: "123:test-token".to_owned(),
            chat: "0".to_owned(),
            users: "1".to_owned(),
            workspace_root: home_or_root().display().to_string(),
            ..Draft::default()
        };
        saved.projects = vec![ProjectDraft {
            options: hub_core::options::OptionsDraft::default(),
            id: "alfa".to_owned(),
            name: "Альфа".to_owned(),
            cwd: "/projects/alfa".to_owned(),
            backend: "codex".to_owned(),
            actions: vec![],
            checks: Vec::new(),
        }];
        form.sync(Some(&saved));
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Проекты", &mut commands);
        click_label(&ctx, &mut form, "Удалить проект…", &mut commands);
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), Vec::new(), &mut commands);
        assert!(has_label(&output, "Удалить безвозвратно"), "modal must open on click");
        assert!(has_label(&output, "Каталог проекта /projects/alfa не удаляется."));
        assert!(commands.is_empty());
        // Without the typed name the destructive button stays inert.
        click_label(&ctx, &mut form, "Удалить безвозвратно", &mut commands);
        assert!(commands.is_empty());
        form.deleting_project.as_mut().unwrap().typed = "альфа".to_owned();
        click_label(&ctx, &mut form, "Удалить безвозвратно", &mut commands);
        assert!(commands.is_empty(), "a different case is not the exact name");
        form.deleting_project.as_mut().unwrap().typed = "Альфа".to_owned();
        click_label(&ctx, &mut form, "Удалить безвозвратно", &mut commands);
        let command = commands.pop().expect("delete command sent");
        let Command::DeleteProject { id, reply } = command else {
            panic!("expected DeleteProject, got another command");
        };
        assert_eq!(id, "alfa");
        assert_eq!(form.draft.projects.len(), 1, "the card waits for the Ok reply");
        reply.send(Ok("Проект «Альфа» удалён".to_owned())).unwrap();
        form.poll(&ctx);
        assert!(form.draft.projects.is_empty(), "Ok reply removes the saved card");
        assert!(form.project_notice.as_ref().is_some_and(Result::is_ok));
    }

    #[test]
    fn external_memory_toggle_preserves_other_unsaved_fields() {
        let mut form = SettingsForm::default();
        let original = Draft::default();
        form.sync(Some(&original));
        form.draft.model = "local-edit".into();
        let mut external = original;
        external.memory.enabled = false;
        form.sync(Some(&external));
        assert!(!form.draft.memory.enabled);
        assert_eq!(form.draft.model, "local-edit");
    }

    #[test]
    fn focused_agent_stack_keeps_keyboard_focus_while_cycling() {
        let ctx = egui::Context::default();
        theme::configure(&ctx);
        let mut form = configured_form();
        form.section = Section::Agents;
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Роли и время ожидания", &mut commands);
        let size = egui::vec2(960.0, 900.0);
        for (count, backend) in [("2 / 7", BackendKind::Claude), ("3 / 7", BackendKind::Codex)] {
            for pressed in [true, false] {
                render(
                    &ctx,
                    &mut form,
                    size,
                    vec![egui::Event::Key {
                        key: egui::Key::ArrowUp,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    &mut commands,
                );
            }
            assert_eq!(form.agent_page, AgentPage::Backend(backend));
            assert!(has_label(&render(&ctx, &mut form, size, vec![], &mut commands), count));
        }
        assert!(!form.has_unsaved());
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
    fn roles_section_renders_both_roles_and_saves_the_choice() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = SettingsForm::default();
        form.sync(Some(&Draft {
            token: "123:test-token".to_owned(),
            chat: "0".to_owned(),
            users: "1".to_owned(),
            workspace_root: home_or_root().display().to_string(),
            default_backend: BackendKind::Qwen,
            ..Draft::default()
        }));
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Агенты", &mut commands);
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), vec![], &mut commands);
        assert!(has_label(&output, "Распределение ролей"));
        assert!(has_label(&output, "Исполнитель по умолчанию"));
        assert!(has_label(&output, "Ревьюер по умолчанию"));
        assert!(has_label(&output, "Codex"));
        assert!(has_label(&output, "Claude Code"));
        assert!(commands.is_empty());
        assert!(!form.has_unsaved(), "a freshly synced draft is not dirty");
        click_option(&ctx, &mut form, "Исполнитель по умолчанию", "OpenCode", &mut commands);
        assert_eq!(form.draft.roles.implementer, BackendKind::Opencode);
        assert!(form.has_unsaved(), "picking a role must dirty the draft");
        click_option(&ctx, &mut form, "Ревьюер по умолчанию", "Hermes", &mut commands);
        assert_eq!(form.draft.roles.reviewer, BackendKind::Hermes);
        click_label(&ctx, &mut form, "Сохранить", &mut commands);
        assert!(
            matches!(
                commands.as_slice(),
                [Command::Save(saved)]
                    if saved.roles
                        == AgentRoles {
                            implementer: BackendKind::Opencode,
                            reviewer: BackendKind::Hermes,
                        }
            ),
            "roles must reach the save command"
        );
    }

    #[test]
    fn equal_roles_warn_about_self_review() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = SettingsForm::default();
        form.sync(Some(&Draft {
            workspace_root: home_or_root().display().to_string(),
            roles: AgentRoles { implementer: BackendKind::Codex, reviewer: BackendKind::Codex },
            ..Draft::default()
        }));
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Агенты", &mut commands);
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), vec![], &mut commands);
        assert!(
            has_label(&output, "Исполнитель и ревьюер совпадают: проверка станет самопроверкой."),
            "equal roles must warn"
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
        assert!(has_label(&output, "Все изменения сохранены"));
        assert_eq!(form.draft, form.synced.unwrap());
    }

    #[test]
    fn clear_telegram_settings_only_clears_the_telegram_draft() {
        let ctx = egui::Context::default();
        theme::configure(&ctx);
        let mut form = configured_form();
        let saved = form.draft.clone();
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Очистить настройки Telegram", &mut commands);
        let expected =
            Draft { token: String::new(), chat: String::new(), users: String::new(), ..saved };
        assert_eq!(form.draft, expected);
        assert!(form.has_unsaved());
        assert!(commands.is_empty());
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
            |ui| form.show(ui, Path::new("/"), 0, &Snapshot::default(), &mut |_| {}),
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
                    form.show(ui, &std::env::temp_dir(), 0, &Snapshot::default(), &mut |_| {
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
                    form.projects(ui, &[], &home_or_root());
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
        click(&ctx, &mut form, "Убрать проверку");
        assert_eq!(form.draft.projects.first().unwrap().checks.len(), 1);
        click(&ctx, &mut form, "Убрать проверку");
        assert!(form.draft.projects.first().unwrap().checks.is_empty());
        let first = form.draft.projects.first().unwrap().id.clone();
        click(&ctx, &mut form, "VPN");
        assert_eq!(form.draft.projects.first().unwrap().actions, ["vpn"]);
        click(&ctx, &mut form, "Добавить проект");
        let second = form.draft.projects.last().unwrap().id.clone();
        assert_ne!(first, second);
        click(&ctx, &mut form, "Удалить проект");
        assert_eq!(form.draft.projects.first().unwrap().id, first);
        assert!(!form.draft.projects.iter().any(|p| p.id == second));
        click(&ctx, &mut form, "Добавить проект");
        assert_ne!(form.draft.projects.last().unwrap().id, first);
        // Plan §3: the project card must point at where the check mode is configured.
        assert!(
            has_label(
                &frame(&ctx, &mut form, Vec::new()),
                "Режим проверок настраивается на вкладке „Правила“ страницы „Задачи“."
            ),
            "the profile description lacks the rules-tab hint"
        );
    }

    #[test]
    fn first_run_waits_for_a_saved_local_draft_before_opening_the_inbox() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = SettingsForm::default();
        form.open_setup();
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Продолжить", &mut commands);
        click_label(&ctx, &mut form, "Завершить настройку", &mut commands);
        let [Command::Save(draft)] = commands.as_slice() else {
            panic!("expected a single setup save")
        };
        assert!(draft.parse(&home_or_root()).is_ok());
        assert!(draft.token.is_empty());
        let saved = draft.clone();
        form.set_saving(true);
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), vec![], &mut commands);
        assert!(has_label(&output, "Сохраняем настройки…"));
        assert!(!has_label(&output, "Открыть входящие"));
        form.sync(Some(&saved));
        form.set_saving(false);
        click_label(&ctx, &mut form, "Открыть входящие", &mut commands);
        assert!(form.take_setup_complete());
        assert!(!form.take_setup_complete());
        assert!(!form.has_unsaved());
    }

    #[test]
    fn failed_first_run_keeps_the_configuration_for_retry() {
        let ctx = egui::Context::default();
        theme::configure(&ctx);
        let mut form = SettingsForm::default();
        form.open_setup();
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Продолжить", &mut commands);
        click_label(&ctx, &mut form, "Завершить настройку", &mut commands);
        let Some(Command::Save(first)) = commands.pop() else { panic!("expected setup save") };
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), vec![], &mut commands);
        assert!(has_label(&output, "Сохранить настройки не удалось. Повторите попытку."));
        assert!(!form.take_setup_complete());
        click_label(&ctx, &mut form, "Повторить настройку", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::Save(retry)] if retry == &first));
    }

    #[test]
    fn action_editor_updates_links_on_projects_by_id() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = configured_form();
        form.section = Section::Actions;
        form.draft.actions.push(ActionDraft {
            id: "one".into(),
            name: "Сборка".into(),
            command: "make".into(),
            cwd: String::new(),
        });
        form.draft.projects.push(ProjectDraft {
            id: "a".into(),
            name: "Альфа".into(),
            cwd: "~/alpha".into(),
            actions: vec![],
            backend: "codex".into(),
            checks: vec![],
            options: hub_core::options::OptionsDraft::default(),
        });
        form.actions.edit(Some("one".into()));
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Альфа", &mut commands);
        assert_eq!(form.draft.projects.first().unwrap().actions, ["one"]);
        click_label(&ctx, &mut form, "Убрать действие", &mut commands);
        assert!(form.draft.actions.is_empty());
        assert!(form.draft.projects.first().unwrap().actions.is_empty());
    }

    #[test]
    fn hidden_duplicate_action_opens_its_own_editor() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = configured_form();
        form.draft.actions = ["one", "two"]
            .map(|id| ActionDraft {
                id: id.to_string(),
                name: "Сборка".into(),
                command: format!("echo {id}"),
                cwd: String::new(),
            })
            .to_vec();
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Исправить ошибки (1)", &mut commands);
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), vec![], &mut commands);
        assert!(has_label(&output, "Название действия"));
        assert!(has_label(&output, "echo two"));
        assert!(commands.is_empty());
    }

    #[test]
    fn actions_menu_previews_the_draft_without_dispatching_a_command() {
        let ctx = egui::Context::default();
        super::super::theme::configure(&ctx);
        let mut form = configured_form();
        form.section = Section::Actions;
        form.draft.actions.push(ActionDraft {
            id: "version".into(),
            name: "Версия".into(),
            command: "git --version".into(),
            cwd: String::new(),
        });
        let mut commands = Vec::new();
        click_label(&ctx, &mut form, "Меню /actions", &mut commands);
        click_label(&ctx, &mut form, "Версия", &mut commands);
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), vec![], &mut commands);
        assert!(has_label(&output, "git --version"));
        click_label(&ctx, &mut form, "Остановить · Версия", &mut commands);
        let output = render(&ctx, &mut form, egui::vec2(960.0, 900.0), vec![], &mut commands);
        assert!(has_label(&output, "Предпросмотр остановлен"));
        click_label(&ctx, &mut form, "Версия", &mut commands);
        let time = ctx.input(|input| input.time) + 2.0;
        let mut output =
            ctx.run_ui(egui::RawInput { time: Some(time), ..Default::default() }, |ui| {
                form.actions(ui, &[]);
            });
        output.textures_delta.clear();
        assert!(has_label(&output, "Предпросмотр завершён"));
        assert!(commands.is_empty());
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
        click(&ctx, &mut form, "Убрать действие");
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
                    form.show(ui, Path::new("/"), 0, &Snapshot::default(), &mut |_| {});
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
    #[case(BackendKind::Hermes, "Одобрения")]
    #[case(BackendKind::Opencode, "Путь к opencode")]
    #[case(BackendKind::Pi, "Путь к pi")]
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
                        form.show(ui, &home, 0, &Snapshot::default(), &mut |command| {
                            commands.push(command);
                        });
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
                            source: egui::MouseWheelSource::Unknown,
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
                        form.show(ui, Path::new("/"), 0, &Snapshot::default(), &mut |_| {});
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
