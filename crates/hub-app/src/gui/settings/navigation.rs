//! Presentation-only navigation; the draft remains shared by every page.

use eframe::egui;
use hub_core::domain::BackendKind;
use hub_core::settings::{Field, FieldError};
use kylig_ui::{
    Chip, DepthSelect, Segmented, Select, SelectItem, SourceItem, SourceList, SourceSection,
    TabsOptions,
};

use crate::gui::look::agent_title;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Section {
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
    pub const ALL: [Self; 8] = [
        Self::Telegram,
        Self::Workspace,
        Self::Agents,
        Self::Projects,
        Self::Actions,
        Self::Speech,
        Self::Application,
        Self::Diagnostics,
    ];

    pub const fn title(self) -> &'static str {
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

    pub const fn description(self) -> &'static str {
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

    pub const fn for_field(field: Field) -> Self {
        match field {
            Field::Token | Field::Chat | Field::Users => Self::Telegram,
            Field::WorkspaceRoot | Field::SnapshotsDir => Self::Workspace,
            Field::Projects => Self::Projects,
            Field::Actions => Self::Actions,
            Field::Speech(_) => Self::Speech,
            Field::Cli
            | Field::Model
            | Field::PermissionMode
            | Field::Budget
            | Field::ApprovalTimeout
            | Field::BackgroundTimeout
            | Field::DefaultBackend
            | Field::Role(_)
            | Field::Codex(_)
            | Field::Qwen(_)
            | Field::Hermes(_)
            | Field::Opencode(_)
            | Field::Pi(_) => Self::Agents,
        }
    }

    pub fn label(self, errors: &[FieldError]) -> String {
        let count = errors.iter().filter(|error| Self::for_field(error.field) == self).count();
        if count == 0 { self.title().to_owned() } else { format!("{}  ({count})", self.title()) }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AgentPage {
    #[default]
    General,
    Backend(BackendKind),
}

impl AgentPage {
    pub const fn for_field(field: Field) -> Option<Self> {
        let page = match field {
            Field::Cli | Field::Model | Field::PermissionMode | Field::Budget => {
                Self::Backend(BackendKind::Claude)
            }
            Field::Codex(_) => Self::Backend(BackendKind::Codex),
            Field::Qwen(_) => Self::Backend(BackendKind::Qwen),
            Field::Hermes(_) => Self::Backend(BackendKind::Hermes),
            Field::Opencode(_) => Self::Backend(BackendKind::Opencode),
            Field::Pi(_) => Self::Backend(BackendKind::Pi),
            Field::ApprovalTimeout
            | Field::BackgroundTimeout
            | Field::DefaultBackend
            | Field::Role(_) => Self::General,
            Field::Token
            | Field::Chat
            | Field::Users
            | Field::WorkspaceRoot
            | Field::SnapshotsDir
            | Field::Projects
            | Field::Actions
            | Field::Speech(_) => return None,
        };
        Some(page)
    }

    pub const fn title(self) -> &'static str {
        match self {
            Self::General => "Общие",
            Self::Backend(kind) => agent_title(kind),
        }
    }

    const fn summary(self) -> &'static str {
        match self {
            Self::General => "Роли и время ожидания",
            Self::Backend(BackendKind::Claude) => "Разрешения и бюджет",
            Self::Backend(BackendKind::Codex) => "Песочница и одобрения",
            Self::Backend(BackendKind::Qwen) => "Подключение API",
            Self::Backend(BackendKind::Hermes) => "Профиль и одобрения",
            Self::Backend(BackendKind::Opencode) => "Команда запуска и модель",
            Self::Backend(BackendKind::Pi) => "Модель и размышления",
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        let pages = std::iter::once(Self::General)
            .chain(BackendKind::ALL.map(Self::Backend))
            .collect::<Vec<_>>();
        let width = ui.available_width();
        let wide = width >= 570.0;
        let stack_width = if wide { (width * 0.47).min(420.0) } else { width };
        let render = |ui: &mut egui::Ui| {
            let mut index = pages.iter().position(|page| page == self).unwrap_or_default();
            let card_width = (stack_width - 48.0 - ui.spacing().item_spacing.x * 1.5).max(1.0);
            DepthSelect::new(&mut index, pages.len())
                .id_salt("agent-editor-stack")
                .visible(3)
                .offset(9.0)
                .looping(true)
                .wheel(false)
                .card_radius(10.0)
                .card_size(egui::vec2(card_width, 92.0))
                .card_fill(ui.visuals().widgets.inactive.weak_bg_fill)
                .show(ui, |ui, index| {
                    let Some(page) = pages.get(index) else { return };
                    ui.style_mut().interaction.selectable_labels = false;
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
                    ui.horizontal(|ui| {
                        let icon = if *page == Self::General {
                            kylig_icons::SLIDERS_HORIZONTAL
                        } else {
                            kylig_icons::TERMINAL_WINDOW
                        };
                        ui.add(
                            kylig_icons::Icon::new(icon)
                                .size(20.0)
                                .color(ui.visuals().selection.stroke.color),
                        );
                        ui.add(
                            egui::Label::new(egui::RichText::new(page.title()).strong().size(22.0))
                                .truncate(),
                        );
                    });
                    ui.add(egui::Label::new(egui::RichText::new(page.summary()).weak()).truncate());
                });
            if let Some(page) = pages.get(index) {
                *self = *page;
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.strong("Выбрать редактор");
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                    for &page in &pages {
                        let count = errors
                            .iter()
                            .filter(|error| Self::for_field(error.field) == Some(page))
                            .count();
                        let label = if count == 0 {
                            page.title().into()
                        } else {
                            format!("{} ({count})", page.title())
                        };
                        if Chip::new(label).selected(*self == page).show(ui).response.clicked() {
                            *self = page;
                        }
                    }
                });
            });
        };
        if wide {
            ui.horizontal(render);
        } else {
            ui.vertical(render);
        }
    }
}

pub fn agent_choice(ui: &mut egui::Ui, label: &str, selected: &mut BackendKind) {
    ui.push_id(label, |ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.label(label);
        if ui.available_width() >= 450.0 {
            Segmented::new(selected, BackendKind::ALL.map(|kind| (kind, agent_title(kind))))
                .options(TabsOptions { padding: 10.0, ..Default::default() })
                .show(ui);
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                for kind in BackendKind::ALL {
                    if Chip::new(agent_title(kind))
                        .selected(*selected == kind)
                        .show(ui)
                        .response
                        .clicked()
                    {
                        *selected = kind;
                    }
                }
            });
        }
    });
}

pub fn compact(ui: &mut egui::Ui, section: &mut Section, errors: &[FieldError]) {
    ui.horizontal_wrapped(|ui| {
        ui.heading("Настройки");
        let items = Section::ALL.map(|page| SelectItem::new(page, page.label(errors)));
        ui.add(
            Select::new(section, &items)
                .id_salt("settings-sections")
                .search(false)
                .width(ui.available_width().min(230.0)),
        );
    });
}

pub fn sidebar(ui: &mut egui::Ui, section: &mut Section, errors: &[FieldError]) {
    let group = |label, pages: &[Section]| {
        SourceSection::new(label).collapsible(false).items(pages.iter().map(|&page| {
            let item = SourceItem::new(page, page.title());
            let count =
                errors.iter().filter(|error| Section::for_field(error.field) == page).count();
            if count > 0 { item.badge(count) } else { item }
        }))
    };
    let sections = [
        group("Подключения", &[Section::Telegram, Section::Workspace, Section::Agents]),
        group("Рабочие процессы", &[Section::Projects, Section::Actions, Section::Speech]),
        group("Приложение", &[Section::Application, Section::Diagnostics]),
    ];
    SourceList::new(section, &sections).id_salt("settings-navigation").show(ui);
}
