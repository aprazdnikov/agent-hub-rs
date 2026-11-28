//! Settings and first-run flows. All drafts, receipts, commands and reports live in this session.

mod actions;
mod agents;
mod application;
mod diagnostics;
mod profiles;
mod setup;

use eframe::egui;
use kylig_ui::{Button, Select, SelectItem, SourceItem, SourceList, SourceSection, TextField};

use crate::widgets::{Wait, action, card, dialog, title, workspace_panes};

#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
enum Section {
    #[default]
    Telegram,
    Workspace,
    Agents,
    Projects,
    Actions,
    Speech,
    Application,
    Diagnostics,
}

impl Section {
    const ALL: [Self; 8] = [
        Self::Telegram,
        Self::Workspace,
        Self::Agents,
        Self::Projects,
        Self::Actions,
        Self::Speech,
        Self::Application,
        Self::Diagnostics,
    ];

    const fn title(self) -> &'static str {
        match self {
            Self::Telegram => "Telegram и доступ",
            Self::Workspace => "Рабочие каталоги",
            Self::Agents => "Агенты",
            Self::Projects => "Проекты",
            Self::Actions => "Действия",
            Self::Speech => "Голосовые сообщения",
            Self::Application => "Запуск и обновления",
            Self::Diagnostics => "Диагностика",
        }
    }

    const fn hint(self) -> &'static str {
        match self {
            Self::Telegram => "Бот и участники, которым разрешено запускать задачи.",
            Self::Workspace => "Хранилище проектов и снимков сессий.",
            Self::Agents => "Роли, модели и разрешения для запуска.",
            Self::Projects => "Каталоги, агенты, команды и проверки ресурсов проектов.",
            Self::Actions => "Именованные команды для запуска через /actions в Telegram.",
            Self::Speech => "Распознавание аудио в голосовых задачах.",
            Self::Application => "Оформление, запуск при входе и обновления приложения.",
            Self::Diagnostics => "Подключения, доступность агентов и журнал приложения.",
        }
    }
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.add(
        TextField::new(value)
            .id_salt(label)
            .label(label)
            .placeholder(hint)
            .width(ui.available_width().min(520.0)),
    );
}

fn secret(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    ui.add(
        TextField::new(value)
            .id_salt(label)
            .label(label)
            .placeholder(hint)
            .password(true)
            .width(ui.available_width().min(520.0)),
    );
}

fn choice(ui: &mut egui::Ui, label: &str, current: &mut &'static str, values: &[&'static str]) {
    let items: Vec<_> = values.iter().map(|&v| SelectItem::new(v, v)).collect();
    ui.add(
        Select::new(current, &items)
            .id_salt(label)
            .label(label)
            .search(false)
            .width(ui.available_width().min(380.0)),
    );
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Telegram {
    token: String,
    chat: String,
    users: String,
}

impl Telegram {
    fn fixture() -> Self {
        Self {
            token: "123456:demo_only_agent_hub_token".into(),
            chat: "-1002300".into(),
            users: "73011".into(),
        }
    }

    fn local(&self) -> bool {
        [&self.token, &self.chat, &self.users].into_iter().all(|s| s.trim().is_empty())
    }

    fn issue(&self) -> Option<&'static str> {
        if self.local() {
            return None;
        }
        if self.token.trim().is_empty() || self.token.trim().contains(char::is_whitespace) {
            return Some("Укажите токен бота без пробелов.");
        }
        if self.chat.trim().parse::<i64>().is_err() {
            return Some("Укажите chat_id группы целым числом.");
        }
        let mut users = self
            .users
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .peekable();
        if users.peek().is_none() || users.any(|s| s.parse::<u64>().is_err()) {
            return Some("Укажите числовые user_id участников через запятую или пробел.");
        }
        None
    }

    fn fields(&mut self, ui: &mut egui::Ui) {
        secret(ui, "Токен бота", &mut self.token, "Токен от BotFather");
        field(ui, "chat_id группы", &mut self.chat, "−100…");
        field(ui, "Разрешённые user_id", &mut self.users, "73011, 73012");
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Speech {
    provider: &'static str,
    whisper: String,
    model: String,
    ffmpeg: String,
    url: String,
    api_model: String,
    key: String,
}

impl Default for Speech {
    fn default() -> Self {
        Self {
            provider: "Выключено",
            whisper: "whisper-cli".into(),
            model: String::new(),
            ffmpeg: "ffmpeg".into(),
            url: "https://api.openai.com/v1".into(),
            api_model: "whisper-1".into(),
            key: String::new(),
        }
    }
}

impl Speech {
    fn show(&mut self, ui: &mut egui::Ui) {
        choice(
            ui,
            "Способ распознавания",
            &mut self.provider,
            &["Выключено", "Whisper.cpp", "API"],
        );
        match self.provider {
            "Whisper.cpp" => {
                ui.weak("Аудио обрабатывается на этом компьютере.");
                field(ui, "Путь к whisper-cli", &mut self.whisper, "whisper-cli");
                field(ui, "Файл модели Whisper.cpp", &mut self.model, "~/Models/ggml-base.bin");
                field(ui, "Путь к ffmpeg", &mut self.ffmpeg, "ffmpeg");
            }
            "API" => {
                ui.weak("Аудио отправляется выбранному провайдеру.");
                field(ui, "Базовый URL API", &mut self.url, "https://api.openai.com/v1");
                field(ui, "Модель распознавания", &mut self.api_model, "whisper-1");
                secret(ui, "API-ключ распознавания", &mut self.key, "Ключ провайдера");
            }
            _ => {
                ui.weak("Распознавание голосовых сообщений выключено.");
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Draft {
    telegram: Telegram,
    progress: &'static str,
    workspace: String,
    snapshots: String,
    agents: agents::Agents,
    profiles: profiles::Profiles,
    speech: Speech,
    updates: bool,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            telegram: Telegram::fixture(),
            progress: "Компактный",
            workspace: "~/Projects".into(),
            snapshots: "~/.agent-hub".into(),
            agents: agents::Agents::default(),
            profiles: profiles::Profiles::default(),
            speech: Speech::default(),
            updates: true,
        }
    }
}

struct Issue {
    location: IssueLocation,
    text: &'static str,
}

enum IssueLocation {
    Section(Section),
    Agent(&'static str),
    Project(u64),
    Action(u64),
}

impl Draft {
    fn issue(&self) -> Option<Issue> {
        let (section, text) = if let Some(issue) = self.telegram.issue() {
            (Section::Telegram, issue)
        } else if self.workspace.trim().is_empty() {
            (Section::Workspace, "Укажите корень рабочих каталогов.")
        } else if let Some((agent, text)) = self.agents.issue() {
            return Some(Issue { location: IssueLocation::Agent(agent), text });
        } else if let Some(issue) = self.profiles.issue() {
            return Some(issue);
        } else if self.speech.provider == "Whisper.cpp" && self.speech.model.trim().is_empty() {
            (Section::Speech, "Выберите файл модели Whisper.cpp.")
        } else if self.speech.provider == "API"
            && (self.speech.key.trim().is_empty()
                || self.speech.api_model.trim().is_empty()
                || !self.speech.url.starts_with("http"))
        {
            (Section::Speech, "Для API нужны адрес, модель и ключ распознавания.")
        } else {
            return None;
        };
        Some(Issue { location: IssueLocation::Section(section), text })
    }
}

enum Confirmation {
    Restart,
    Delete { id: u64, name: String, cwd: String, typed: String },
}

pub(crate) struct Settings {
    section: Section,
    agent_page: &'static str,
    project: Option<u64>,
    actions: actions::Editor,
    saved: Draft,
    draft: Draft,
    pending: Option<Draft>,
    confirmation: Option<Confirmation>,
    save: Wait,
    notice: &'static str,
    sequence: u64,
    diagnostics: diagnostics::Diagnostics,
    application: application::Application,
    setup: setup::Setup,
}

impl Default for Settings {
    fn default() -> Self {
        let saved = Draft::default();
        Self {
            section: Section::default(),
            agent_page: agents::GENERAL,
            project: None,
            actions: actions::Editor::default(),
            draft: saved.clone(),
            saved,
            pending: None,
            confirmation: None,
            save: Wait::default(),
            notice: "",
            sequence: 5,
            diagnostics: diagnostics::Diagnostics::default(),
            application: application::Application::default(),
            setup: setup::Setup::default(),
        }
    }
}

impl Settings {
    pub(crate) fn open_agents(&mut self) {
        self.section = Section::Agents;
    }

    pub(crate) fn open_actions(&mut self) {
        self.section = Section::Actions;
    }

    pub(crate) fn open_log(&mut self) {
        self.section = Section::Diagnostics;
        self.diagnostics.open_log();
    }

    pub(crate) fn tray_bot(&mut self, ui: &mut egui::Ui) {
        self.poll_save(ui);
        self.diagnostics.bot(ui, self.saved.telegram.local());
    }

    pub(crate) fn tray_update(&mut self, ui: &mut egui::Ui) -> bool {
        let clicked = self.application.tray_update(ui);
        if clicked {
            self.section = Section::Application;
        }
        clicked
    }

    pub(crate) fn setup(&mut self, ui: &mut egui::Ui) -> bool {
        if !self.setup.show(ui) {
            return false;
        }
        let setup = std::mem::take(&mut self.setup);
        self.saved.workspace = setup.root;
        self.saved.agents.default = setup.agent;
        self.saved.telegram = setup.telegram;
        self.draft = self.saved.clone();
        self.pending = None;
        self.save = Wait::default();
        self.confirmation = None;
        self.notice = "";
        self.diagnostics.saved(!self.saved.telegram.local());
        self.diagnostics.record("Первичная настройка завершена");
        true
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) {
        if self.section == Section::Diagnostics && self.diagnostics.showing_log() {
            self.poll_save(ui);
            ui.horizontal_wrapped(|ui| {
                if ui.link("‹ Диагностика").clicked() {
                    self.diagnostics.close_log();
                }
                ui.heading("Журнал");
            });
            if let Some(event) = self.diagnostics.log(ui) {
                self.apply_log_action(event);
            }
            return;
        }
        if ui.available_width() < 780.0 {
            let items = Section::ALL.map(|s| SelectItem::new(s, s.title()));
            ui.horizontal_wrapped(|ui| {
                ui.heading("Настройки");
                ui.add(
                    Select::new(&mut self.section, &items)
                        .search(false)
                        .id_salt("settings-sections")
                        .width(ui.available_width().min(230.0)),
                );
            });
            if self.draft != self.saved || self.save.busy() || !self.notice.is_empty() {
                self.save_bar(ui);
            }
            ui.add_space(8.0);
            self.page(ui);
        } else {
            title(ui, "Настройки", "");
            self.save_bar(ui);
            ui.add_space(8.0);
            workspace_panes(ui, "settings-panes", |ui, sidebar| {
                if sidebar {
                    let group = |label, pages: &[Section]| {
                        SourceSection::new(label)
                            .collapsible(false)
                            .items(pages.iter().map(|&s| SourceItem::new(s, s.title())))
                    };
                    let sections = [
                        group(
                            "Подключения",
                            &[Section::Telegram, Section::Workspace, Section::Agents],
                        ),
                        group(
                            "Рабочие процессы",
                            &[Section::Projects, Section::Actions, Section::Speech],
                        ),
                        group("Приложение", &[Section::Application, Section::Diagnostics]),
                    ];
                    SourceList::new(&mut self.section, &sections)
                        .id_salt("settings-navigation")
                        .show(ui);
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt(("settings-body", self.section))
                        .auto_shrink([false, false])
                        .show(ui, |ui| self.page(ui));
                }
            });
        }
        self.dialog(ui);
    }

    fn begin_save(&mut self, ui: &egui::Ui) {
        self.pending = Some(self.draft.clone());
        self.notice = "";
        self.save.start(ui, "Сохраняем настройки…");
    }

    fn poll_save(&mut self, ui: &mut egui::Ui) {
        if self.save.draw(ui)
            && let Some(receipt) = self.pending.take()
        {
            if self.saved.telegram != receipt.telegram {
                self.diagnostics.saved(!receipt.telegram.local());
            }
            self.saved = receipt;
            self.diagnostics.record("Настройки сохранены");
            self.notice = "Настройки сохранены";
        }
    }

    fn save_bar(&mut self, ui: &mut egui::Ui) {
        self.poll_save(ui);
        let dirty = self.draft != self.saved;
        let issue = self.draft.issue();
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    dirty && issue.is_none() && !self.save.busy(),
                    Button::new("Сохранить").primary(),
                )
                .clicked()
            {
                if self.saved.telegram != self.draft.telegram && self.diagnostics.running() {
                    self.confirmation = Some(Confirmation::Restart);
                } else {
                    self.begin_save(ui);
                }
            }
            if ui.add_enabled(dirty, Button::new("Отменить изменения")).clicked() {
                self.draft = self.saved.clone();
                self.notice = "";
            }
            ui.weak(if dirty {
                "Есть несохранённые изменения"
            } else if self.notice.is_empty() {
                "Все изменения сохранены"
            } else {
                self.notice
            });
        });
        if let Some(issue) = issue {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(ui.visuals().error_fg_color, issue.text);
                if action(ui, "Перейти к ошибке") {
                    match issue.location {
                        IssueLocation::Section(section) => self.section = section,
                        IssueLocation::Agent(agent) => {
                            self.section = Section::Agents;
                            self.agent_page = agent;
                        }
                        IssueLocation::Action(id) => {
                            self.open_actions();
                            self.actions.select(id);
                        }
                        IssueLocation::Project(id) => {
                            self.section = Section::Projects;
                            self.project = Some(id);
                        }
                    }
                }
            });
        }
    }

    fn page(&mut self, ui: &mut egui::Ui) {
        title(ui, self.section.title(), self.section.hint());
        ui.push_id(self.section, |ui| match self.section {
            Section::Telegram => {
                card(ui, |ui| {
                    self.draft.telegram.fields(ui);
                    ui.weak("Для локальной работы оставьте все три поля пустыми.");
                    choice(
                        ui,
                        "Прогресс задач в Telegram",
                        &mut self.draft.progress,
                        &["Компактный", "Подробный"],
                    );
                    if self.draft.telegram.local() && action(ui, "Подставить демоданные Telegram")
                    {
                        self.draft.telegram = Telegram::fixture();
                    } else if !self.draft.telegram.local()
                        && action(ui, "Очистить настройки Telegram")
                    {
                        self.draft.telegram = Telegram::default();
                    }
                });
            }
            Section::Workspace => {
                card(ui, |ui| {
                    field(ui, "Корень рабочих каталогов", &mut self.draft.workspace, "~/Projects");
                    field(ui, "Каталог снимков", &mut self.draft.snapshots, "~/.agent-hub");
                });
            }
            Section::Agents => self.draft.agents.show(ui, &mut self.agent_page),
            Section::Projects => {
                if let Some(id) = self.draft.profiles.projects(
                    ui,
                    &mut self.project,
                    &mut self.sequence,
                    self.draft.agents.default,
                ) {
                    if let Some(saved) = self.saved.profiles.projects.iter().find(|p| p.id == id) {
                        self.confirmation = Some(Confirmation::Delete {
                            id,
                            name: saved.name.clone(),
                            cwd: saved.cwd.clone(),
                            typed: String::new(),
                        });
                    } else {
                        self.draft.profiles.projects.retain(|p| p.id != id);
                    }
                }
            }
            Section::Actions => self.actions.show(ui, &mut self.draft.profiles, &mut self.sequence),
            Section::Speech => {
                card(ui, |ui| self.draft.speech.show(ui));
            }
            Section::Application => self.application.show(ui, &mut self.draft.updates),
            Section::Diagnostics => {
                if let Some(event) =
                    self.diagnostics.show(ui, &self.saved, self.saved != self.draft)
                {
                    self.apply_log_action(event);
                }
            }
        });
    }

    fn apply_log_action(&mut self, event: diagnostics::LogAction) {
        match event {
            diagnostics::LogAction::UseChat => self.draft.telegram.chat = "-1002301".into(),
            diagnostics::LogAction::AddUser => {
                if !self.draft.telegram.users.split(',').any(|v| v.trim() == "73012") {
                    if !self.draft.telegram.users.trim().is_empty() {
                        self.draft.telegram.users.push_str(", ");
                    }
                    self.draft.telegram.users.push_str("73012");
                }
            }
        }
        self.section = Section::Telegram;
    }

    fn dialog(&mut self, ui: &egui::Ui) {
        let Some(confirmation) = &mut self.confirmation else { return };
        let mut open = true;
        let heading = match &confirmation {
            Confirmation::Restart => "Перезапуск бота",
            Confirmation::Delete { .. } => "Удалить проект?",
        };
        let result = dialog(ui, "settings-confirm", &mut open, heading).show(ui.ctx(), |ui| {
                let (label, enabled) = match confirmation {
                    Confirmation::Restart => {
                        ui.label("Новые параметры доступа прервут текущие сессии бота.");
                        ("Сохранить и перезапустить", true)
                    }
                    Confirmation::Delete { name, cwd, typed, .. } => {
                        ui.strong(name.as_str());
                        ui.label(format!("Каталог проекта • {cwd}"));
                        ui.label("Из списка настроек будет удалён демонстрационный профиль. В приложении эта операция также удаляет задачи, правила и память проекта.");
                        field(ui, "Введите название проекта для подтверждения", typed, name);
                        ("Удалить проект", typed == name)
                    }
                };
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(enabled, Button::new(label).primary()).clicked() { Some(true) }
                    else if action(ui, "Отмена") { Some(false) } else { None }
                }).inner
            }).flatten();
        if result == Some(true) {
            match confirmation {
                Confirmation::Restart => self.begin_save(ui),
                Confirmation::Delete { id, .. } => {
                    self.saved.profiles.projects.retain(|p| p.id != *id);
                    self.draft.profiles.projects.retain(|p| p.id != *id);
                    if let Some(pending) = &mut self.pending {
                        pending.profiles.projects.retain(|p| p.id != *id);
                    }
                    self.notice = "Профиль проекта удалён";
                    self.diagnostics.record("Профиль проекта удалён из хаба");
                }
            }
        }
        if !open || result.is_some() {
            self.confirmation = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Screen {
        context: egui::Context,
        time: f64,
        width: f32,
    }

    impl Screen {
        fn new(width: f32) -> Self {
            let context = egui::Context::default();
            crate::palette::configure(&context);
            context.all_styles_mut(|style| style.spacing.item_spacing = egui::vec2(12.0, 12.0));
            kylig_icons::install(&context);
            Self { context, time: 0.0, width }
        }

        fn draw(&mut self, settings: &mut Settings, events: Vec<egui::Event>) -> egui::FullOutput {
            self.time += 0.12;
            let mut output = self.context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(self.width, 1800.0),
                    )),
                    time: Some(self.time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::CentralPanel::default().show(ui, |ui| settings.show(ui));
                },
            );
            output.textures_delta.clear();
            output
        }

        fn click(&mut self, settings: &mut Settings, label: &str) {
            self.draw(settings, Vec::new());
            let pos = self
                .draw(settings, Vec::new())
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
                .unwrap_or_else(|| panic!("Control missing: {label}"));
            assert!(pos.x < self.width, "Control outside the window: {label}");
            for pressed in [true, false] {
                self.draw(
                    settings,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: egui::Modifiers::NONE,
                        },
                    ],
                );
            }
        }

        fn visible(&mut self, settings: &mut Settings, label: &str) -> bool {
            self.draw(settings, Vec::new()).shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == label)
            })
        }
    }

    #[test]
    fn action_preview_can_stop_and_finish_without_changing_settings() {
        for width in [400.0, 1350.0] {
            let mut settings = Settings::default();
            settings.open_actions();
            let mut screen = Screen::new(width);
            screen.click(&mut settings, "Меню /actions");
            screen.click(&mut settings, "Версия Git");
            assert!(screen.visible(&mut settings, "Выполняется демозапуск…"));
            screen.click(&mut settings, "Остановить · Версия Git");
            assert!(screen.visible(&mut settings, "Демозапуск остановлен"));
            screen.click(&mut settings, "Статус репозитория");
            screen.time += 2.0;
            assert!(screen.visible(&mut settings, "Демозапуск завершён"));
            assert!(screen.visible(&mut settings, "git status --short --branch"));
            assert!(settings.draft == settings.saved);
        }
    }

    #[test]
    fn action_links_can_be_saved_and_removal_can_be_reverted() {
        let mut settings = Settings::default();
        settings.open_actions();
        let mut screen = Screen::new(420.0);
        screen.click(&mut settings, "Версия Git");
        let linked = |draft: &Draft| {
            draft.profiles.projects.iter().any(|p| p.name == "agent-hub" && p.actions.contains(&4))
        };
        screen.click(&mut settings, "agent-hub");
        assert!(linked(&settings.draft));
        screen.click(&mut settings, "Сохранить");
        screen.time += 2.0;
        screen.draw(&mut settings, vec![]);
        assert!(linked(&settings.saved));
        screen.click(&mut settings, "Убрать действие");
        assert!(!linked(&settings.draft));
        screen.click(&mut settings, "Отменить изменения");
        assert!(linked(&settings.draft));
        assert!(screen.visible(&mut settings, "Версия Git"));
        screen.click(&mut settings, "Добавить действие");
        screen.click(&mut settings, "‹ Все команды");
        screen.click(&mut settings, "Перейти к ошибке");
        assert!(screen.visible(&mut settings, "Название кнопки в Telegram"));
    }

    #[test]
    fn journal_opens_on_its_own_and_returns_to_diagnostics() {
        for width in [360.0, 1200.0] {
            let mut settings = Settings { section: Section::Diagnostics, ..Default::default() };
            let mut screen = Screen::new(width);
            screen.click(&mut settings, "Журнал");
            assert!(screen.visible(&mut settings, "agent-hub.log"));
            assert!(!screen.visible(&mut settings, "Настройки"));
            screen.click(&mut settings, "‹ Диагностика");
            assert!(screen.visible(&mut settings, "Состояние"));
            assert!(!screen.visible(&mut settings, "agent-hub.log"));
        }
    }

    #[test]
    fn editor_switches_directly_without_dirtying_the_draft() {
        for width in [360.0, 1200.0] {
            let mut settings = Settings { section: Section::Agents, ..Default::default() };
            let mut screen = Screen::new(width);
            for (agent, executable) in [
                ("Pi", "pi"),
                ("Qwen Code", "qwen"),
                ("OpenCode", "opencode"),
                ("Claude Code", "claude"),
                ("Codex", "codex"),
                ("Hermes", "hermes"),
            ] {
                screen.click(&mut settings, agent);
                assert!(screen.visible(&mut settings, &format!("Путь к {executable}")));
            }
            screen.click(&mut settings, agents::GENERAL);
            assert!(screen.visible(&mut settings, "Агент для новых тем"));
            assert!(settings.draft == settings.saved);
        }
    }

    #[test]
    fn keyboard_cycles_the_focused_agent_stack() {
        let mut settings = Settings { section: Section::Agents, ..Default::default() };
        let mut screen = Screen::new(360.0);
        screen.click(&mut settings, "Роли и время ожидания");
        screen.draw(
            &mut settings,
            vec![egui::Event::Key {
                key: egui::Key::ArrowUp,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(screen.visible(&mut settings, "Путь к claude"));
    }

    #[test]
    fn switching_editors_preserves_fields_and_targets_validation() {
        let mut settings = Settings { section: Section::Agents, ..Default::default() };
        let mut screen = Screen::new(360.0);
        screen.click(&mut settings, "Qwen Code");
        screen.click(&mut settings, "Из настроек Qwen");
        screen.draw(&mut settings, vec![egui::Event::Text("invalid-host".into())]);
        screen.click(&mut settings, "Hermes");
        screen.click(&mut settings, "Перейти к ошибке");
        assert_eq!(settings.agent_page, "Qwen Code");
        assert!(screen.visible(&mut settings, "invalid-host"));
    }

    #[test]
    fn invalid_project_reopens_its_own_editor() {
        for width in [360.0, 1200.0] {
            let mut settings = Settings { section: Section::Projects, ..Default::default() };
            let mut screen = Screen::new(width);
            screen.click(&mut settings, "Добавить проект");
            let new_project = settings.project;
            assert!(screen.visible(&mut settings, "Новый проект"));
            screen.click(&mut settings, "docs-site");
            assert_ne!(settings.project, new_project);
            screen.click(&mut settings, "Перейти к ошибке");
            assert_eq!(settings.project, new_project);
            screen.click(&mut settings, "Удалить проект…");
            assert!(settings.draft == settings.saved);
            assert!(settings.confirmation.is_none());
        }
    }
}
