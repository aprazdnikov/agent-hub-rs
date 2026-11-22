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
    fn open(&mut self) {
        self.opened = None;
        if !std::path::Path::new(&self.path).is_absolute() {
            self.error = Some("Укажите абсолютный путь к каталогу проекта.".into());
            return;
        }
        match hub_telegram::paths::workspace_root(std::path::Path::new(&self.path)) {
            Ok(path) => {
                let path = path.as_path().to_path_buf();
                self.path = path.display().to_string();
                self.opened = Some(path);
                self.error = None;
            }
            _ => self.error = Some("Каталог недоступен. Проверьте путь и права доступа.".into()),
        }
    }
    fn show(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) -> Option<Scope> {
        if let Some(receiver) = &self.picking {
            match receiver.try_recv() {
                Ok(path) => {
                    self.picking = None;
                    if let Some(path) = path {
                        self.path = path.display().to_string();
                        self.open();
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.picking = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        ui.label("Каталог проекта");
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.path)
                    .id_salt("local_task_project")
                    .hint_text("Абсолютный путь к проекту"),
            );
            if ui.add_enabled(self.picking.is_none(), egui::Button::new("Выбрать папку…")).clicked()
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
        let backend = self.backend.get_or_insert_with(|| {
            snapshot.saved.as_ref().map_or(BackendKind::Qwen, |draft| draft.default_backend)
        });
        ui.horizontal(|ui| {
            ui.label("Агент для новых задач");
            egui::ComboBox::from_id_salt("local_task_backend")
                .selected_text(backend.name())
                .show_ui(ui, |ui| {
                    for candidate in BackendKind::ALL {
                        ui.selectable_value(backend, candidate, candidate.name());
                    }
                });
        });
        ui.small("Запуск и управление доступны одновременно здесь и в Telegram. Вопросы и разрешения появятся над выбором проекта.");
        self.opened.clone().map(|project| (None, project, *backend))
    }
}
impl TasksView {
    pub(super) fn choose_context(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
    ) -> Option<Scope> {
        let local =
            self.local_mode.get_or_insert(!matches!(snapshot.bot, BotStatus::Running { .. }));
        egui::ComboBox::from_id_salt("task_mode")
            .selected_text(if *local { "Локально" } else { "В теме Telegram" })
            .show_ui(ui, |ui| {
                ui.selectable_value(local, true, "Локально");
                ui.selectable_value(local, false, "В теме Telegram");
            });
        if *local {
            return self.local_project.show(ui, snapshot);
        }
        if !matches!(snapshot.bot, BotStatus::Running { .. }) {
            ui.label("Бот остановлен. Выберите «Локально» для управления задачами без Telegram.");
            return None;
        }
        ui.label("Тема для запуска — проект и агент");
        egui::ComboBox::from_id_salt("task_topic")
            .selected_text(self.selected.map_or("Выберите тему".into(), |key| {
                format!("Тема #{}", key.thread.0)
            }))
            .show_ui(ui, |ui| {
                for topic in &snapshot.topics {
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
        self.selected.and_then(|key| snapshot.topics.iter().find(|topic| topic.key == key)).map(
            |topic| {
                (Some(topic.key), topic.session.cwd.as_path().to_path_buf(), topic.session.backend)
            },
        )
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
