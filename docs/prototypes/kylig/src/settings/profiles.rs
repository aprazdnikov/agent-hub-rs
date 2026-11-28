//! Profile and action drafts use stable per-session IDs so renames keep their links.

use eframe::egui;
use kylig_ui::{Checkbox, ListItem};

use super::{Issue, IssueLocation, actions::Action, agents, choice, field};
use crate::widgets::{action, agent_choice, card, remaining_height};

#[derive(Clone, PartialEq, Eq)]
enum Check {
    Http { name: String, url: String, status: u16 },
    Tcp { name: String, host: String, port: u16 },
}

impl Check {
    fn show(&mut self, ui: &mut egui::Ui) {
        match self {
            Self::Http { name, url, status } => {
                field(ui, "Название проверки HTTP", name, "Веб-сервис");
                field(ui, "URL", url, "https://example.org/health");
                ui.horizontal_wrapped(|ui| {
                    ui.label("Ожидаемый HTTP-статус");
                    ui.add(egui::DragValue::new(status).range(200..=599));
                });
            }
            Self::Tcp { name, host, port } => {
                field(ui, "Название проверки TCP", name, "Сервер");
                field(ui, "Хост", host, "localhost");
                ui.horizontal_wrapped(|ui| {
                    ui.label("Порт");
                    ui.add(egui::DragValue::new(port).range(1..=65535));
                });
            }
        }
    }

    fn incomplete(&self) -> bool {
        match self {
            Self::Http { name, url, .. } => {
                name.trim().is_empty()
                    || !(url.starts_with("http://") || url.starts_with("https://"))
            }
            Self::Tcp { name, host, .. } => name.trim().is_empty() || host.trim().is_empty(),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Profile {
    pub(super) id: u64,
    pub(super) name: String,
    pub(super) cwd: String,
    backend: &'static str,
    model: String,
    mode: &'static str,
    pub(super) actions: Vec<u64>,
    checks: Vec<Check>,
}

impl Profile {
    pub(super) fn title(&self) -> &str {
        if self.name.trim().is_empty() { "Новый проект" } else { &self.name }
    }

    fn issue(&self) -> Option<&'static str> {
        if self.name.trim().is_empty() || self.cwd.trim().is_empty() {
            Some("Для проекта нужны название и рабочий каталог.")
        } else if self.checks.iter().any(Check::incomplete) {
            Some("Проверьте название и адрес в проверках ресурсов проекта.")
        } else {
            None
        }
    }

    fn new(id: u64, name: &str, cwd: String, backend: &'static str) -> Self {
        Self {
            id,
            name: name.into(),
            cwd,
            backend,
            model: String::new(),
            mode: "Общие настройки",
            actions: Vec::new(),
            checks: Vec::new(),
        }
    }

    fn checks(&mut self, ui: &mut egui::Ui) {
        ui.strong("Проверки ресурсов для /doctor");
        let mut remove = None;
        for (index, check) in self.checks.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                check.show(ui);
                if action(ui, "Убрать проверку") {
                    remove = Some(index);
                }
            });
        }
        if let Some(index) = remove {
            self.checks.remove(index);
        }
        ui.horizontal_wrapped(|ui| {
            if action(ui, "Добавить HTTP") {
                self.checks.push(Check::Http {
                    name: String::new(),
                    url: String::new(),
                    status: 200,
                });
            }
            if action(ui, "Добавить TCP") {
                self.checks.push(Check::Tcp {
                    name: String::new(),
                    host: String::new(),
                    port: 443,
                });
            }
        });
    }

    fn show(&mut self, ui: &mut egui::Ui, actions: &[Action]) {
        field(ui, "Название проекта", &mut self.name, "Название");
        field(ui, "Каталог проекта", &mut self.cwd, "~/Projects/проект");
        let previous = self.backend;
        agent_choice(ui, "Агент проекта", &mut self.backend);
        if previous != self.backend {
            self.model.clear();
            self.mode = "Общие настройки";
        }
        field(ui, "Модель", &mut self.model, "Из общих настроек");
        let modes = agents::modes(self.backend);
        if !modes.is_empty() {
            let options: Vec<_> =
                std::iter::once("Общие настройки").chain(modes.iter().copied()).collect();
            choice(ui, "Режим", &mut self.mode, &options);
        }
        ui.strong("Связанные действия");
        if actions.is_empty() {
            ui.weak("Добавьте команду в разделе «Действия».");
        }
        for action in actions {
            let mut linked = self.actions.contains(&action.id);
            if ui.add(Checkbox::new(&mut linked, &action.name)).changed() {
                self.actions.retain(|id| *id != action.id);
                if linked {
                    self.actions.push(action.id);
                }
            }
        }
        self.checks(ui);
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct Profiles {
    pub(super) projects: Vec<Profile>,
    pub(super) actions: Vec<Action>,
}

impl Default for Profiles {
    fn default() -> Self {
        Self {
            projects: [(1, "agent-hub"), (2, "docs-site"), (3, "sandbox")]
                .into_iter()
                .map(|(id, name)| Profile::new(id, name, format!("~/Projects/{name}"), "Codex"))
                .collect(),
            actions: vec![
                Action::new(4, "Версия Git", "git --version", ""),
                Action::new(
                    5,
                    "Статус репозитория",
                    "git status --short --branch",
                    "~/Projects/agent-hub",
                ),
            ],
        }
    }
}

impl Profiles {
    pub(super) fn issue(&self) -> Option<Issue> {
        for project in &self.projects {
            if let Some(text) = project.issue() {
                return Some(Issue { location: IssueLocation::Project(project.id), text });
            }
        }
        if let Some(action) = self.actions.iter().find(|a| !a.complete()) {
            return Some(Issue {
                location: IssueLocation::Action(action.id),
                text: "Укажите название и команду каждого действия.",
            });
        }
        None
    }

    pub(super) fn projects(
        &mut self,
        ui: &mut egui::Ui,
        selected: &mut Option<u64>,
        sequence: &mut u64,
        backend: &'static str,
    ) -> Option<u64> {
        if action(ui, "Добавить проект") {
            *sequence += 1;
            self.projects.push(Profile::new(*sequence, "", String::new(), backend));
            *selected = Some(*sequence);
        }
        let Some(first) = self.projects.first() else {
            *selected = None;
            ui.weak("Профилей проектов пока нет.");
            return None;
        };
        if !self.projects.iter().any(|p| *selected == Some(p.id)) {
            *selected = Some(first.id);
        }
        if ui.available_width() >= 680.0 {
            let list_width = (ui.available_width() * 0.31).min(285.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(list_width);
                    let height = remaining_height(ui).max(100.0);
                    egui::ScrollArea::vertical()
                        .id_salt("project-list")
                        .max_height(height)
                        .show(ui, |ui| self.project_list(ui, selected));
                });
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    self.project_detail(ui, *selected)
                })
                .inner
            })
            .inner
        } else {
            self.project_list(ui, selected);
            self.project_detail(ui, *selected)
        }
    }

    fn project_list(&self, ui: &mut egui::Ui, selected: &mut Option<u64>) {
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for project in &self.projects {
                let issue = project.issue();
                let mut row = ListItem::new(project.title())
                    .id_salt(project.id)
                    .icon(kylig_icons::FOLDER)
                    .subtitle(if project.cwd.is_empty() { project.backend } else { &project.cwd })
                    .chevron(true)
                    .selected(*selected == Some(project.id));
                if issue.is_some() {
                    row = row
                        .icon(kylig_icons::WARNING_CIRCLE)
                        .icon_color(ui.visuals().error_fg_color);
                }
                let response = row.show(ui).response;
                if response.clicked() {
                    *selected = Some(project.id);
                }
                if let Some(message) = issue {
                    response.on_hover_text(message);
                }
            }
        });
    }

    fn project_detail(&mut self, ui: &mut egui::Ui, selected: Option<u64>) -> Option<u64> {
        let project = self.projects.iter_mut().find(|p| Some(p.id) == selected)?;
        ui.push_id(project.id, |ui| {
            card(ui, |ui| {
                ui.heading(project.title());
                project.show(ui, &self.actions);
                ui.separator();
                action(ui, "Удалить проект…").then_some(project.id)
            })
        })
        .inner
    }
}
