//! Saved project profiles and resource checks.

use super::{
    BackendKind, Button, CheckDraft, Field, FieldError, MAX_CHECKS, MAX_PROJECTS, PendingDelete,
    ProgressRing, ProjectDraft, Select, SelectItem, SettingsForm, egui, input, messages,
    navigation, path_field, pick_folder, text_field, theme,
};
use hub_core::actions::ActionDraft;

fn project_options(ui: &mut egui::Ui, project: &mut ProjectDraft) {
    let mut backend = BackendKind::parse(&project.backend).unwrap_or(BackendKind::Claude);
    let previous_backend = backend;
    navigation::agent_choice(ui, "Агент проекта", &mut backend);
    if previous_backend != backend {
        backend.name().clone_into(&mut project.backend);
        project.options = hub_core::options::OptionsDraft::default();
    }
    text_field(ui, "Модель", input(&mut project.options.model).placeholder("Из общих настроек"));
    let modes =
        BackendKind::parse(&project.backend).map(hub_core::options::Mode::all).unwrap_or_default();
    if modes.is_empty() {
        ui.label("Режим задаётся самим CLI.");
    } else {
        let items: Vec<_> = std::iter::once(SelectItem::new(String::new(), "Общие настройки"))
            .chain(modes.into_iter().map(|mode| {
                let wire = mode.wire();
                SelectItem::new(wire.clone(), wire)
            }))
            .collect();
        ui.add(
            Select::new(&mut project.options.mode, &items)
                .id_salt("project-mode")
                .label("Режим")
                .search(false)
                .width(ui.available_width().min(380.0)),
        );
    }
}

fn project_checks(ui: &mut egui::Ui, checks: &mut Vec<CheckDraft>) {
    ui.strong("Проверки ресурсов для /doctor");
    ui.weak("HTTP(S) проверяет ответ на GET без редиректов и авторизации. TCP проверяет подключение к порту.");
    let mut remove = None;
    for (index, check) in checks.iter_mut().enumerate() {
        ui.push_id(("check", index), |ui| {
            match check {
                CheckDraft::Http { name, url, expected } => {
                    text_field(ui, "Название проверки HTTP", input(name).placeholder("Веб-сервис"));
                    text_field(ui, "URL", input(url).placeholder("https://example.org/health"));
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Ожидаемый HTTP-статус");
                        ui.add(egui::DragValue::new(expected).range(200..=599));
                    });
                    ui.weak("Укажите адрес без логина, пароля, query и fragment.");
                }
                CheckDraft::Tcp { name, host, port } => {
                    text_field(ui, "Название проверки TCP", input(name).placeholder("Сервер"));
                    text_field(ui, "Хост", input(host).placeholder("localhost"));
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Порт");
                        ui.add(egui::DragValue::new(port).range(1..=65535));
                    });
                }
            }
            if ui.add(Button::new("Убрать проверку")).clicked() {
                remove = Some(index);
            }
        });
    }
    if let Some(index) = remove {
        checks.remove(index);
    }
    ui.horizontal_wrapped(|ui| {
        if ui.add_enabled(checks.len() < MAX_CHECKS, Button::new("Добавить HTTP")).clicked()
        {
            checks.push(CheckDraft::Http {
                name: String::new(),
                url: String::new(),
                expected: 200,
            });
        }
        if ui.add_enabled(checks.len() < MAX_CHECKS, Button::new("Добавить TCP")).clicked()
        {
            checks.push(CheckDraft::Tcp { name: String::new(), host: String::new(), port: 443 });
        }
        ui.label(format!("{}/{MAX_CHECKS}", checks.len()));
    });
}

// Keep the valid prefix for uniqueness checks so each erroneous profile gets its own warning.
fn project_issues(
    projects: &[ProjectDraft],
    actions: &[hub_core::actions::Action],
) -> Vec<Option<String>> {
    let mut valid = Vec::with_capacity(projects.len());
    projects
        .iter()
        .map(|project| {
            valid.push(project.clone());
            if let Err(error) = hub_core::projects::parse(&valid, actions) {
                valid.pop();
                // The core numbers profiles within the slice; the row already identifies this profile.
                let reason = error.split_once(": ").map_or(error.as_str(), |(_, reason)| reason);
                Some(reason.to_owned())
            } else {
                None
            }
        })
        .collect()
}

pub(super) fn title<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() { fallback } else { value.trim() }
}

impl SettingsForm {
    pub(super) fn projects(
        &mut self,
        ui: &mut egui::Ui,
        errors: &[FieldError],
        home: &std::path::Path,
    ) {
        if ui
            .add_enabled(
                self.draft.projects.len() < MAX_PROJECTS,
                Button::new("Добавить проект").icon(kylig_icons::PLUS),
            )
            .clicked()
        {
            let id = uuid::Uuid::new_v4().simple().to_string();
            self.draft.projects.push(ProjectDraft {
                id: id.clone(),
                name: String::new(),
                cwd: String::new(),
                backend: self.draft.default_backend.name().to_owned(),
                options: hub_core::options::OptionsDraft::default(),
                actions: Vec::new(),
                checks: Vec::new(),
            });
            self.project = Some(id);
        }
        messages(ui, errors, Field::Projects);
        if self.project_deletion.is_some() {
            ui.horizontal(|ui| {
                ui.add(ProgressRing::indeterminate().size(18.0));
                ui.label("Удаление проекта…");
            });
        }
        match &self.project_notice {
            Some(Ok(message)) => {
                ui.label(message);
            }
            Some(Err(message)) => {
                ui.colored_label(ui.visuals().error_fg_color, message);
            }
            None => {}
        }
        if !self.draft.projects.iter().any(|p| Some(&p.id) == self.project.as_ref()) {
            self.project = self.draft.projects.first().map(|p| p.id.clone());
        }
        if self.project.is_none() {
            ui.weak("Профилей проектов пока нет.");
            return;
        }
        let issues = if errors.iter().any(|error| error.field == Field::Projects) {
            let actions = hub_core::actions::parse(&self.draft.actions, home).unwrap_or_default();
            project_issues(&self.draft.projects, &actions)
        } else {
            Vec::new()
        };
        if ui.available_width() >= 680.0 {
            let list_width = (ui.available_width() * 0.31).min(285.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(list_width);
                    egui::ScrollArea::vertical()
                        .id_salt("project-list")
                        .max_height(theme::remaining_height(ui).max(100.0))
                        .show(ui, |ui| self.project_list(ui, &issues));
                });
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    self.project_detail(ui);
                });
            });
        } else {
            self.project_list(ui, &issues);
            self.project_detail(ui);
        }
    }

    fn project_list(&mut self, ui: &mut egui::Ui, issues: &[Option<String>]) {
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for (index, project) in self.draft.projects.iter().enumerate() {
                let issue = issues.get(index).and_then(Option::as_deref);
                let mut row = kylig_ui::ListItem::new(title(&project.name, "Новый проект"))
                    .id_salt(&project.id)
                    .icon(kylig_icons::FOLDER)
                    .subtitle(title(&project.cwd, &project.backend))
                    .selected(Some(&project.id) == self.project.as_ref())
                    .chevron(true);
                if issue.is_some() {
                    row = row
                        .icon(kylig_icons::WARNING_CIRCLE)
                        .icon_color(ui.visuals().error_fg_color);
                }
                let response = row.show(ui).response;
                if response.clicked() {
                    self.project = Some(project.id.clone());
                }
                if let Some(issue) = issue {
                    response.on_hover_text(issue);
                }
            }
        });
    }

    fn project_detail(&mut self, ui: &mut egui::Ui) {
        let Some(project) =
            self.draft.projects.iter_mut().find(|p| Some(&p.id) == self.project.as_ref())
        else {
            return;
        };
        let id = project.id.clone();
        let saved = self.synced.as_ref().and_then(|s| s.projects.iter().find(|p| p.id == id));
        let label = if saved.is_some() {
            "Удалить проект…"
        } else {
            "Удалить проект"
        };
        let remove = ui
            .push_id(&id, |ui| {
                theme::card(ui)
                    .show(ui, |ui| {
                        ui.heading(title(&project.name, "Новый проект"));
                        project_fields(ui, project, &self.draft.actions);
                        ui.separator();
                        ui.add_enabled(self.project_deletion.is_none(), Button::new(label).ghost())
                            .clicked()
                    })
                    .inner
            })
            .inner;
        if remove {
            if let Some(saved) = saved {
                self.deleting_project = Some(PendingDelete {
                    id,
                    name: saved.name.clone(),
                    cwd: saved.cwd.clone(),
                    typed: String::new(),
                });
            } else {
                self.draft.projects.retain(|p| p.id != id);
            }
        }
    }
}

fn project_fields(ui: &mut egui::Ui, project: &mut ProjectDraft, actions: &[ActionDraft]) {
    let width = ui.available_width();
    ui.set_min_width(width);
    text_field(ui, "Название проекта", input(&mut project.name).placeholder("Название"));
    path_field(ui, "Каталог проекта", &mut project.cwd, "~/Projects/проект", pick_folder);
    project_options(ui, project);
    ui.strong("Связанные действия");
    if actions.is_empty() {
        ui.weak("Добавьте команду в разделе «Действия».");
    }
    for action in actions {
        let mut linked = project.actions.contains(&action.id);
        if ui
            .add(kylig_ui::Checkbox::new(&mut linked, title(&action.name, "Новое действие")))
            .changed()
        {
            project.actions.retain(|id| id != &action.id);
            if linked {
                project.actions.push(action.id.clone());
            }
        }
    }
    project.actions.retain(|id| {
        actions.iter().any(|action| &action.id == id)
            || !ui.add(Button::new(format!("Убрать отсутствующее действие {id}"))).clicked()
    });
    project_checks(ui, &mut project.checks);
    ui.small("Режим проверок настраивается на вкладке „Правила“ страницы „Задачи“.");
    ui.small("Пустые параметры наследуют общие настройки. Смена агента сбрасывает модель и режим профиля. Изменения профиля применяются к новым сессиям.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_warnings_follow_the_offending_rows() {
        let valid = ProjectDraft {
            id: "app".into(),
            name: "App".into(),
            cwd: "~/Projects/app".into(),
            backend: "codex".into(),
            options: hub_core::options::OptionsDraft::default(),
            actions: vec![],
            checks: vec![],
        };
        let projects = [
            ProjectDraft { cwd: String::new(), ..valid.clone() },
            valid.clone(),
            ProjectDraft { id: "other".into(), ..valid.clone() },
            ProjectDraft { id: "docs".into(), name: "Docs".into(), ..valid },
        ];
        let issues = project_issues(&projects, &[]);
        assert!(matches!(issues.as_slice(), [Some(path), None, Some(duplicate), None]
            if path.contains("каталог") && duplicate.contains("уникальным")));
    }
}
