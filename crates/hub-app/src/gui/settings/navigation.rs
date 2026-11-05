//! Presentation-only navigation; the draft remains shared by every page.

use eframe::egui;
use hub_core::domain::BackendKind;
use hub_core::settings::{Field, FieldError};

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
}

impl Section {
    pub const ALL: [Self; 7] = [
        Self::Telegram,
        Self::Workspace,
        Self::Agents,
        Self::Projects,
        Self::Actions,
        Self::Speech,
        Self::Application,
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
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Telegram => {
                "Подключение бота и пользователи, которым разрешено запускать задачи."
            }
            Self::Workspace => "Папка для работы агентов и место хранения снимков сессий.",
            Self::Agents => "Общие параметры и настройки каждого агента.",
            Self::Projects => "Готовые профили каталогов, агентов и проверок ресурсов.",
            Self::Actions => "Команды для ручного запуска из Telegram.",
            Self::Speech => "Распознавание аудио для постановки задач агентам.",
            Self::Application => "Системная тема, автозапуск и проверка новых версий.",
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
            | Field::Codex(_)
            | Field::Qwen(_)
            | Field::Hermes(_)
            | Field::Opencode(_) => Self::Agents,
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
            Field::ApprovalTimeout | Field::BackgroundTimeout | Field::DefaultBackend => {
                Self::General
            }
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

    pub fn show(&mut self, ui: &mut egui::Ui, errors: &[FieldError]) {
        ui.horizontal_wrapped(|ui| {
            for page in std::iter::once(Self::General).chain(BackendKind::ALL.map(Self::Backend)) {
                let count = errors
                    .iter()
                    .filter(|error| Self::for_field(error.field) == Some(page))
                    .count();
                let label = if count == 0 {
                    page.title().to_owned()
                } else {
                    format!("{} ({count})", page.title())
                };
                ui.selectable_value(self, page, label);
            }
        });
    }
}

pub fn show(ui: &mut egui::Ui, section: &mut Section, errors: &[FieldError]) {
    if ui.available_width() < 760.0 {
        ui.horizontal(|ui| {
            ui.strong("Настройки");
            egui::ComboBox::from_id_salt("settings_section")
                .selected_text(section.label(errors))
                .width(240.0)
                .show_ui(ui, |ui| {
                    for page in Section::ALL {
                        ui.selectable_value(section, page, page.label(errors));
                    }
                });
        });
        ui.add_space(12.0);
    } else {
        egui::Panel::left("settings_navigation")
            .exact_size(218.0)
            .resizable(false)
            .frame(egui::Frame::new().fill(ui.visuals().panel_fill).inner_margin(16))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("settings_navigation_scroll").show(ui, |ui| {
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Настройки").size(20.0).strong());
                    ui.add_space(22.0);
                    for page in Section::ALL {
                        if matches!(page, Section::Projects | Section::Application) {
                            ui.add_space(16.0);
                            ui.weak(if page == Section::Projects {
                                "Рабочие процессы"
                            } else {
                                "Приложение"
                            });
                            ui.add_space(4.0);
                        }
                        let count = errors
                            .iter()
                            .filter(|error| Section::for_field(error.field) == page)
                            .count();
                        let label = egui::RichText::new(page.title());
                        let label = if count > 0 {
                            label.color(ui.visuals().error_fg_color)
                        } else {
                            label
                        };
                        ui.horizontal(|ui| {
                            if ui
                                .add_sized(
                                    [ui.available_width() - 24.0, 34.0],
                                    egui::Button::selectable(*section == page, "").left_text(label),
                                )
                                .clicked()
                            {
                                *section = page;
                            }
                            if count > 0 {
                                ui.colored_label(ui.visuals().error_fg_color, count.to_string())
                                    .on_hover_text("Есть поля, которые нужно исправить");
                            }
                        });
                    }
                });
            });
    }
}
