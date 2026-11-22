use super::{BackendKind, BotStatus, Scope, Snapshot, TasksView};
use eframe::egui;
use std::{path::PathBuf, sync::mpsc};

#[derive(Default)]
pub(super) struct LocalProject {
    path: String,
    opened: Option<PathBuf>,
    backend: Option<BackendKind>,
    picking: Option<mpsc::Receiver<Option<PathBuf>>>,
    error: Option<String>,
}
impl LocalProject {
    pub(super) fn opened(&self) -> Option<&std::path::Path> {
        self.opened.as_deref()
    }
    fn scope(&mut self, snapshot: &Snapshot) -> Option<Scope> {
        let backend = *self.backend.get_or_insert_with(|| {
            snapshot.saved.as_ref().map_or(BackendKind::Qwen, |draft| draft.default_backend)
        });
        self.opened.clone().map(|project| (None, project, backend))
    }
    pub(super) fn choose(&mut self, path: &std::path::Path) {
        self.path = path.display().to_string();
        self.opened = None;
        if !path.is_absolute() {
            self.error = Some("Укажите абсолютный путь к каталогу проекта.".into());
            return;
        }
        match hub_telegram::paths::workspace_root(path) {
            Ok(path) => {
                let path = path.as_path().to_path_buf();
                self.path = path.display().to_string();
                self.opened = Some(path);
                self.error = None;
            }
            _ => self.error = Some("Каталог недоступен. Проверьте путь и права доступа.".into()),
        }
    }
    fn open(&mut self) {
        self.choose(&PathBuf::from(&self.path));
    }
    fn show(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        if let Some(receiver) = &self.picking {
            match receiver.try_recv() {
                Ok(path) => {
                    self.picking = None;
                    if let Some(path) = path {
                        self.choose(&path);
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.picking = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        let mut backend = *self.backend.get_or_insert_with(|| {
            snapshot.saved.as_ref().map_or(BackendKind::Qwen, |draft| draft.default_backend)
        });
        let current = self.opened.clone().map(|path| {
            let name = path.file_name().and_then(|v| v.to_str()).unwrap_or("Проект");
            (format!("Проект · {name}"), path)
        });
        let mut controls = |ui: &mut egui::Ui| {
            ui.label("Каталог проекта");
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.path)
                        .id_salt("local_task_project")
                        .hint_text("Абсолютный путь к проекту"),
                );
                if ui
                    .add_enabled(self.picking.is_none(), egui::Button::new("Выбрать папку…"))
                    .clicked()
                {
                    let (tx, rx) = mpsc::channel();
                    self.picking = Some(rx);
                    let context = ui.ctx().clone();
                    std::thread::spawn(move || {
                        let _ = tx.send(rfd::FileDialog::new().pick_folder());
                        context.request_repaint();
                    });
                }
                if ui.button("Открыть проект").clicked() {
                    self.open();
                }
            });
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Агент для новых задач");
                egui::ComboBox::from_id_salt("local_task_backend")
                    .selected_text(backend.name())
                    .show_ui(ui, |ui| {
                        for candidate in BackendKind::ALL {
                            ui.selectable_value(&mut backend, candidate, candidate.name());
                        }
                    });
            });
        };
        if let Some((label, path)) = current {
            egui::CollapsingHeader::new(label)
                .id_salt("local_project_settings")
                .show(ui, controls)
                .header_response
                .on_hover_text(path.display().to_string());
        } else {
            controls(ui);
        }
        self.backend = Some(backend);
    }
}
impl TasksView {
    pub(super) fn resolved_scope(&mut self, snapshot: &Snapshot) -> Option<Scope> {
        let local = *self.local_mode.get_or_insert_with(|| {
            self.selected.is_none() || !matches!(snapshot.bot, BotStatus::Running { .. })
        });
        if local {
            return self.local_project.scope(snapshot);
        }
        if !matches!(snapshot.bot, BotStatus::Running { .. }) {
            return None;
        }
        let topic = snapshot.topics.iter().find(|topic| Some(topic.key) == self.selected)?;
        Some((Some(topic.key), topic.session.cwd.as_path().to_owned(), topic.session.backend))
    }
    fn project_choice(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        let projects: Vec<_> = self.sidebar.projects().collect();
        if !projects.is_empty() {
            let current = self.scope.as_ref().map(|s| s.1.as_path());
            let label = projects
                .iter()
                .find(|(path, _)| current == Some(*path))
                .map_or("Выберите проект", |(_, name)| *name);
            let mut choice = None;
            ui.horizontal_wrapped(|ui| {
                ui.label("Проект");
                egui::ComboBox::from_id_salt("new_task_project").selected_text(label).show_ui(
                    ui,
                    |ui| {
                        for (path, name) in projects {
                            if ui
                                .selectable_label(current == Some(path), name)
                                .on_hover_text(path.display().to_string())
                                .clicked()
                            {
                                choice = Some(path.to_owned());
                            }
                        }
                    },
                );
            });
            if let Some(path) = choice {
                self.local_project.choose(&path);
                if self.local_mode == Some(false) {
                    self.selected = snapshot
                        .topics
                        .iter()
                        .find(|t| t.session.cwd.as_path() == path)
                        .map(|t| t.key);
                    if self.selected.is_none() {
                        self.local_mode = Some(true);
                    }
                }
            }
        }
    }
    pub(super) fn context_controls(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        creating: bool,
    ) {
        if creating {
            self.project_choice(ui, snapshot);
        }
        let project = self.resolved_scope(snapshot).map(|(_, path, _)| path);
        let topics: Vec<_> = snapshot
            .topics
            .iter()
            .filter(|topic| project.as_ref().is_none_or(|path| topic.session.cwd.as_path() == path))
            .collect();
        let telegram = matches!(snapshot.bot, BotStatus::Running { .. }) && !topics.is_empty();
        let previous = self.local_mode;
        let local = self.local_mode.get_or_insert(true);
        ui.horizontal_wrapped(|ui| {
            ui.label("Где запустить");
            egui::ComboBox::from_id_salt("task_mode")
                .selected_text(if *local { "Локально" } else { "В теме Telegram" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(local, true, "Локально");
                    ui.add_enabled_ui(telegram, |ui| {
                        ui.selectable_value(local, false, "В теме Telegram");
                    })
                    .response
                    .on_disabled_hover_text("Для этого проекта нет подключённой темы Telegram.");
                });
        });
        if previous == Some(!*local)
            && let Some(project) = project.as_deref()
        {
            if *local {
                self.local_project.choose(project);
            } else if !snapshot.topics.iter().any(|topic| {
                Some(topic.key) == self.selected && topic.session.cwd.as_path() == project
            }) {
                self.selected = snapshot
                    .topics
                    .iter()
                    .find(|topic| topic.session.cwd.as_path() == project)
                    .map(|t| t.key);
            }
        }
        if *local {
            if creating {
                self.local_project.show(ui, snapshot);
            }
            return;
        }
        if !matches!(snapshot.bot, BotStatus::Running { .. }) {
            ui.label("Бот остановлен. Выберите «Локально» для управления задачами без Telegram.");
            return;
        }
        ui.label("Тема Telegram");
        egui::ComboBox::from_id_salt("task_topic")
            .selected_text(
                snapshot.topics.iter().find(|topic| Some(topic.key) == self.selected).map_or_else(
                    || "Выберите тему".to_owned(),
                    |topic| {
                        topic.title.clone().unwrap_or_else(|| format!("#{}", topic.key.thread.0))
                    },
                ),
            )
            .show_ui(ui, |ui| {
                for topic in topics {
                    ui.selectable_value(
                        &mut self.selected,
                        Some(topic.key),
                        format!(
                            "{} · {} · {}",
                            topic.title.as_deref().unwrap_or("Без названия"),
                            topic.session.backend.name(),
                            topic.session.cwd.as_path().display()
                        ),
                    );
                }
            });
    }
    pub(super) fn local_context(&self) -> bool {
        self.scope.as_ref().is_some_and(|scope| scope.0.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_project_switch_clears_previous_target() {
        let project = tempfile::tempdir().unwrap();
        let mut selection =
            LocalProject { path: project.path().display().to_string(), ..Default::default() };
        selection.open();
        assert!(selection.opened.is_some());
        selection.path = project.path().join("missing").display().to_string();
        selection.open();
        assert!(selection.error.is_some());
        assert!(
            selection.opened.is_none(),
            "a failed switch must not keep the old mutation target"
        );
    }
    #[test]
    fn relative_project_path_is_not_opened() {
        let mut selection = LocalProject { path: ".".into(), ..Default::default() };
        selection.open();
        assert!(selection.opened.is_none());
        assert!(selection.error.is_some());
    }
}
