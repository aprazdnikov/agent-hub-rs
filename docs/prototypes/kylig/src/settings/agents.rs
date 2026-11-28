//! Agent-specific editors share the same in-memory draft as the general role settings.

use eframe::egui;
use kylig_ui::{DepthSelect, Switch};

use super::{choice, field, secret};
use crate::widgets::{AGENTS, agent_choice, card, choice_chips};

pub(super) const GENERAL: &str = "Общие";

pub(super) fn modes(name: &str) -> &'static [&'static str] {
    match name {
        "Claude Code" => &["default", "plan", "acceptEdits", "bypassPermissions"],
        "Codex" => &["read-only", "workspace-write", "danger-full-access"],
        "Qwen Code" => &["default", "plan", "auto-edit", "yolo"],
        "Hermes" => &["default", "accept_edits", "dont_ask", "yolo"],
        "Pi" => &["off", "minimal", "low", "medium", "high", "xhigh", "max"],
        _ => &[],
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Backend {
    name: &'static str,
    command: String,
    model: String,
    mode: &'static str,
    approval: &'static str,
    // Budget for Claude, endpoint for Qwen, profile name for Hermes.
    parameter: String,
    key: String,
}

impl Backend {
    fn new(name: &'static str) -> Self {
        let mode = match name {
            "Codex" => "workspace-write",
            "Pi" => "medium",
            _ => "default",
        };
        let approval = if name == "Codex" { "on-request" } else { "default" };
        Self {
            name,
            mode,
            approval,
            command: String::new(),
            model: String::new(),
            parameter: String::new(),
            key: String::new(),
        }
    }

    fn show(&mut self, ui: &mut egui::Ui) {
        let executable = match self.name {
            "Claude Code" => "claude",
            "Codex" => "codex",
            "Qwen Code" => "qwen",
            "Hermes" => "hermes",
            "OpenCode" => "opencode",
            _ => "pi",
        };
        field(ui, &format!("Путь к {executable}"), &mut self.command, "Поиск команды в PATH");
        field(ui, "Модель", &mut self.model, "Из настроек агента");
        match self.name {
            "Claude Code" => {
                choice(ui, "Режим разрешений", &mut self.mode, modes(self.name));
                field(ui, "Лимит на задачу, $", &mut self.parameter, "Без лимита");
            }
            "Codex" => {
                choice(ui, "Песочница", &mut self.mode, modes(self.name));
                choice(ui, "Одобрения", &mut self.approval, &["untrusted", "on-request", "never"]);
                secret(ui, "API-ключ OpenAI", &mut self.key, "Вход через codex login");
            }
            "Qwen Code" => {
                choice(ui, "Одобрения", &mut self.mode, modes(self.name));
                field(ui, "Базовый URL API", &mut self.parameter, "Из настроек Qwen");
                secret(ui, "API-ключ Qwen", &mut self.key, "Из настроек Qwen");
            }
            "Hermes" => {
                field(ui, "Профиль", &mut self.parameter, "Активный профиль Hermes");
                choice(ui, "Одобрения", &mut self.mode, modes(self.name));
                ui.weak("Вход и провайдер настраиваются в hermes setup.");
            }
            "Pi" => {
                choice(ui, "Уровень размышлений", &mut self.mode, modes(self.name));
                choice(ui, "Одобрения", &mut self.approval, &["default", "yolo"]);
                ui.weak("В режиме default хаб запрашивает подтверждение для bash, edit и write.");
            }
            _ => {
                ui.weak("Вход и провайдеры настраиваются в opencode auth login.");
            }
        }
        if matches!(self.mode, "danger-full-access" | "bypassPermissions" | "yolo")
            || matches!(self.approval, "never" | "yolo")
        {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "Этот режим снимает ограничения на действия агента.",
            );
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Agents {
    pub(super) default: &'static str,
    implementer: &'static str,
    reviewer: &'static str,
    memory: bool,
    approval_seconds: String,
    background_seconds: String,
    backends: Vec<Backend>,
}

impl Default for Agents {
    fn default() -> Self {
        Self {
            default: "Claude Code",
            implementer: "Claude Code",
            reviewer: "Codex",
            memory: true,
            approval_seconds: "600".into(),
            background_seconds: "3600".into(),
            backends: AGENTS.into_iter().map(Backend::new).collect(),
        }
    }
}

impl Agents {
    pub(super) fn issue(&self) -> Option<(&'static str, &'static str)> {
        if [&self.approval_seconds, &self.background_seconds]
            .into_iter()
            .any(|v| v.parse::<u64>().map_or(true, |n| n == 0))
        {
            return Some((GENERAL, "Укажите время ожидания в целых положительных секундах."));
        }
        self.backends.iter().find_map(|backend| {
            let invalid = match backend.name {
                "Claude Code"
                    if !backend.parameter.is_empty()
                        && backend
                            .parameter
                            .parse::<f64>()
                            .map_or(true, |n| !n.is_finite() || n <= 0.0) =>
                {
                    "Лимит на задачу должен быть положительным числом."
                }
                "Qwen Code"
                    if !backend.parameter.is_empty()
                        && !backend.parameter.starts_with("https://")
                        && !backend.parameter.starts_with("http://") =>
                {
                    "Адрес API Qwen должен начинаться с https:// или http://."
                }
                _ if backend.key.contains(char::is_whitespace) => "Удалите пробелы из API-ключа.",
                _ => return None,
            };
            Some((backend.name, invalid))
        })
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui, page: &mut &'static str) {
        self.editor_selector(ui, page);
        ui.add_space(8.0);
        if *page == GENERAL {
            card(ui, |ui| {
                ui.strong("Распределение ролей");
                agent_choice(ui, "Агент для новых тем", &mut self.default);
                agent_choice(ui, "Исполнитель по умолчанию", &mut self.implementer);
                agent_choice(ui, "Ревьюер по умолчанию", &mut self.reviewer);
                if self.implementer == self.reviewer {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "Исполнитель будет проверять свою работу.",
                    );
                }
            });
            card(ui, |ui| {
                ui.strong("Выполнение задач");
                field(ui, "Ожидание подтверждения, с", &mut self.approval_seconds, "");
                field(ui, "Фоновые задачи, с", &mut self.background_seconds, "");
                ui.add(Switch::new(&mut self.memory).label("Подключать память к агентам"));
            });
        } else if let Some(backend) = self.backends.iter_mut().find(|b| b.name == *page) {
            card(ui, |ui| ui.push_id(backend.name, |ui| backend.show(ui)));
        }
    }

    fn editor_selector(&self, ui: &mut egui::Ui, page: &mut &'static str) {
        let width = ui.available_width();
        let wide = width >= 570.0;
        let stack_width = if wide { (width * 0.47).min(420.0) } else { width };
        let render = |ui: &mut egui::Ui| {
            let mut index = self.backends.iter().position(|b| b.name == *page).map_or(0, |i| i + 1);
            let card_width = (stack_width - 48.0 - ui.spacing().item_spacing.x * 1.5).max(1.0);
            DepthSelect::new(&mut index, self.backends.len() + 1)
                .id_salt("agent-editor-stack")
                .visible(3)
                .offset(9.0)
                .looping(true)
                .wheel(false)
                .card_radius(10.0)
                .card_size(egui::vec2(card_width, 92.0))
                .card_fill(ui.visuals().widgets.inactive.weak_bg_fill)
                .show(ui, |ui, index| {
                    ui.style_mut().interaction.selectable_labels = false;
                    let backend = index.checked_sub(1).and_then(|i| self.backends.get(i));
                    let name = backend.map_or(GENERAL, |b| b.name);
                    let icon = if backend.is_some() {
                        kylig_icons::TERMINAL_WINDOW
                    } else {
                        kylig_icons::SLIDERS_HORIZONTAL
                    };
                    ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
                    ui.horizontal(|ui| {
                        ui.add(
                            kylig_icons::Icon::new(icon)
                                .size(20.0)
                                .color(ui.visuals().selection.stroke.color),
                        );
                        ui.add(
                            egui::Label::new(egui::RichText::new(name).strong().size(22.0))
                                .truncate(),
                        );
                    });
                    let summary = match name {
                        GENERAL => "Роли и время ожидания",
                        "Claude Code" => "Разрешения и бюджет",
                        "Codex" => "Песочница и одобрения",
                        "Qwen Code" => "Подключение API",
                        "Hermes" => "Профиль и одобрения",
                        "OpenCode" => "Команда запуска и модель",
                        _ => "Модель и размышления",
                    };
                    ui.add(egui::Label::new(egui::RichText::new(summary).weak()).truncate());
                });
            *page =
                index.checked_sub(1).and_then(|i| self.backends.get(i)).map_or(GENERAL, |b| b.name);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                ui.strong("Выбрать редактор");
                choice_chips(ui, page, std::iter::once(GENERAL).chain(AGENTS));
            });
        };
        if wide {
            ui.horizontal(render);
        } else {
            ui.vertical(render);
        }
    }
}
