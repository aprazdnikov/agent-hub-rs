use eframe::egui;

use crate::widgets::{AGENTS, choice_chips};

const PHASES: &[&str] = &[
    "Подготовка",
    "Выполняется",
    "Ожидает разрешения",
    "Ожидает ответа",
    "Фоновые задачи",
    "Завершается",
    "Останавливается",
    "Завершено",
    "Остановлено",
    "Ошибка",
    "Нет данных",
];
pub(super) const MEMORY_MODES: &[&str] = &["Наследовать", "Включена", "Выключена"];

#[derive(Clone, Copy)]
pub(super) struct Topic {
    pub id: u16,
    pub name: &'static str,
    pub project: &'static str,
    pub backend: &'static str,
    pub phase: &'static str,
    pub running: bool,
    pub model: &'static str,
    pub session: Option<&'static str>,
    pub memory: &'static str,
    pub event: &'static str,
}

impl Topic {
    pub fn state(self) -> &'static str {
        if self.running { "Работает" } else { "Свободна" }
    }
    pub fn path(self) -> String {
        format!("/demo/projects/{}", self.project)
    }
    pub fn link(self) -> String {
        format!("https://t.me/c/1000000000/{}", self.id)
    }
}

pub(super) fn samples() -> Vec<Topic> {
    vec![
        Topic {
            id: 42,
            name: "Подготовка релиза",
            project: "agent-hub",
            backend: "Codex",
            phase: "Выполняется",
            running: true,
            model: "gpt-5.4",
            session: Some("demo-codex-0042"),
            memory: "Наследовать",
            event: "Агент проверяет сборку пакета для Linux.",
        },
        Topic {
            id: 57,
            name: "Руководство пользователя",
            project: "docs-site",
            backend: "Claude Code",
            phase: "Ожидает ответа",
            running: true,
            model: "Sonnet",
            session: Some("demo-claude-0057"),
            memory: "Включена",
            event: "Агент уточняет, для какой платформы подготовить инструкцию.",
        },
        Topic {
            id: 73,
            name: "Проверка обновления",
            project: "agent-hub",
            backend: "Codex",
            phase: "Завершено",
            running: false,
            model: "gpt-5.4",
            session: Some("demo-codex-0073"),
            memory: "Наследовать",
            event: "Проверка обновления завершилась успешно.",
        },
        Topic {
            id: 1,
            name: "Общая",
            project: "sandbox",
            backend: "Qwen Code",
            phase: "Нет данных",
            running: false,
            model: "По умолчанию",
            session: None,
            memory: "Выключена",
            event: "Ожидается первое поручение.",
        },
    ]
}

#[derive(Default, PartialEq, Eq)]
pub(super) struct Filter {
    pub query: String,
    backend: Option<&'static str>,
    state: Option<&'static str>,
    phase: Option<&'static str>,
}

impl Filter {
    pub fn matches(&self, topic: Topic) -> bool {
        let query = self.query.trim().to_lowercase();
        self.backend.is_none_or(|value| value == topic.backend)
            && self.state.is_none_or(|value| value == topic.state())
            && self.phase.is_none_or(|value| value == topic.phase)
            && (topic.name.to_lowercase().contains(&query) || topic.path().contains(&query))
    }
    pub fn controls(&mut self, ui: &mut egui::Ui) {
        let mut backend = self.backend.unwrap_or("Все агенты");
        ui.label("Агент");
        choice_chips(ui, &mut backend, std::iter::once("Все агенты").chain(AGENTS));
        self.backend = (backend != "Все агенты").then_some(backend);
        ui.horizontal_wrapped(|ui| {
            for (label, all, selected, options) in [
                ("Состояние", "Все состояния", &mut self.state, &["Работает", "Свободна"][..]),
                ("Этап", "Все этапы", &mut self.phase, PHASES),
            ] {
                egui::ComboBox::from_label(label).selected_text(selected.unwrap_or(all)).show_ui(
                    ui,
                    |ui| {
                        ui.selectable_value(selected, None, all);
                        for &option in options {
                            ui.selectable_value(selected, Some(option), option);
                        }
                    },
                );
            }
        });
    }
}
