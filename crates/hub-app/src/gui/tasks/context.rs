use super::{BackendKind, BotStatus, Scope, Snapshot, TasksView};
use crate::gui::theme::{self, Choice};
use eframe::egui;
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};

#[derive(Default)]
pub(super) struct LocalProject {
    path: String,
    editing: bool,
    opened: Option<PathBuf>,
    backend: Option<BackendKind>,
    picking: Option<mpsc::Receiver<Option<PathBuf>>>,
    error: Option<String>,
}
impl LocalProject {
    pub(super) fn opened(&self) -> Option<&std::path::Path> {
        self.opened.as_deref()
    }
    fn ensure_backend(&mut self, snapshot: &Snapshot) -> BackendKind {
        *self.backend.get_or_insert_with(|| {
            snapshot.saved.as_ref().map_or(BackendKind::Qwen, |draft| draft.default_backend)
        })
    }
    fn scope(&mut self, snapshot: &Snapshot) -> Option<Scope> {
        let backend = self.ensure_backend(snapshot);
        self.opened.clone().map(|project| (None, project, backend))
    }
    pub(super) fn choose(&mut self, path: &std::path::Path) {
        self.editing = false;
        self.picking = None;
        self.path = path.display().to_string();
        self.open();
    }
    fn open(&mut self) {
        let path = Path::new(&self.path);
        let result = if path.is_absolute() {
            hub_telegram::paths::workspace_root(path)
                .map(|path| path.as_path().to_owned())
                .map_err(|_| "Каталог недоступен. Проверьте путь и права доступа.")
        } else {
            Err("Укажите абсолютный путь к каталогу проекта.")
        };
        match result {
            Ok(path) => {
                self.path = path.display().to_string();
                self.opened = Some(path);
                self.error = None;
                self.editing = false;
                self.picking = None;
            }
            Err(message) => {
                self.error = Some(message.into());
                if !self.editing {
                    self.opened = None;
                }
            }
        }
    }
    fn poll(&mut self, ui: &mut egui::Ui) {
        if let Some(receiver) = &self.picking {
            match receiver.try_recv() {
                Ok(path) => {
                    self.picking = None;
                    if let Some(path) = path {
                        self.path = path.display().to_string();
                        self.error = None;
                        if !self.editing {
                            self.open();
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.picking = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    fn directory(&mut self, ui: &mut egui::Ui) {
        let Some(current) = &self.opened else {
            self.directory_fields(ui);
            return;
        };
        if ui
            .add(kylig_ui::Button::new("Сменить каталог…").icon(kylig_icons::FOLDER_OPEN).ghost())
            .on_hover_text(current.display().to_string())
            .clicked()
        {
            self.path = current.display().to_string();
            self.error = None;
            self.editing = true;
        }
        if self.editing {
            let mut open = true;
            theme::dialog(ui, "change-task-directory", &mut open, "Сменить каталог проекта")
                .show(ui.ctx(), |ui| self.directory_fields(ui));
            if !open {
                self.cancel_edit();
            }
        }
    }

    fn cancel_edit(&mut self) {
        if let Some(path) = &self.opened {
            self.path = path.display().to_string();
        }
        self.editing = false;
        self.error = None;
        self.picking = None;
    }

    fn directory_fields(&mut self, ui: &mut egui::Ui) {
        ui.add(
            kylig_ui::TextField::new(&mut self.path)
                .id_salt("local_task_project")
                .label("Путь к каталогу")
                .placeholder("Абсолютный путь к проекту")
                .width(ui.available_width().min(540.0)),
        );
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.picking.is_none(),
                    kylig_ui::Button::new("Выбрать папку…").icon(kylig_icons::FOLDER_OPEN),
                )
                .clicked()
            {
                let (tx, rx) = mpsc::channel();
                self.picking = Some(rx);
                let context = ui.ctx().clone();
                let start = self.opened.clone();
                std::thread::spawn(move || {
                    let dialog = rfd::FileDialog::new().set_title("Каталог проекта");
                    let dialog =
                        if let Some(dir) = start { dialog.set_directory(dir) } else { dialog };
                    let _ = tx.send(dialog.pick_folder());
                    context.request_repaint();
                });
            }
            if ui
                .add_enabled(
                    self.picking.is_none(),
                    kylig_ui::Button::new("Открыть проект").primary(),
                )
                .clicked()
            {
                self.open();
            }
            if self.editing && ui.add(kylig_ui::Button::new("Отмена").ghost()).clicked() {
                self.cancel_edit();
            }
            if self.picking.is_some() {
                ui.add(kylig_ui::ProgressRing::indeterminate().size(18.0))
                    .on_hover_text("Ожидание выбора папки");
            }
        });
    }
}
// The «Проект» selector in the compact row: the picked path lands in `choice`
// for the migration pass; `current` labels and highlights the active project.
fn project_combo(
    ui: &mut egui::Ui,
    projects: Vec<(PathBuf, String)>,
    current: Option<&Path>,
    choice: &mut Option<PathBuf>,
) {
    let items = projects
        .into_iter()
        .map(|(path, name)| {
            let detail = path.display().to_string();
            Choice::new(Some(path), name, "").with_path(&detail)
        })
        .collect::<Vec<_>>();
    let selected = current.map(Path::to_owned);
    if let Some(value) =
        theme::detailed_select(ui, "Проект", "Выберите проект", &selected, &items, 420.0)
    {
        choice.clone_from(value);
    }
}

fn mode_choice(ui: &mut egui::Ui, local: &mut bool, telegram: bool) {
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(local, true, "Локально");
        ui.add_enabled_ui(telegram, |ui| ui.selectable_value(local, false, "В теме Telegram"))
            .inner
            .on_disabled_hover_text("Для этого проекта нет подключённой темы Telegram.");
    });
}

fn topic_choice(
    ui: &mut egui::Ui,
    selected: &mut Option<super::TopicKey>,
    topics: &[&hub_telegram::hub::TopicView],
) {
    let items = topics
        .iter()
        .map(|topic| {
            let label = topic.title.clone().unwrap_or_else(|| format!("#{}", topic.key.thread.0));
            let agent = crate::gui::look::agent_title(topic.session.backend);
            let detail = if topic.title.is_some() {
                format!("#{} · {agent}", topic.key.thread.0)
            } else {
                agent.to_owned()
            };
            Choice::new(Some(topic.key), label, detail)
                .with_path(&topic.session.cwd.as_path().to_string_lossy())
        })
        .collect::<Vec<_>>();
    if let Some(value) =
        theme::detailed_select(ui, "Тема проекта", "Выберите тему", selected, &items, 420.0)
    {
        *selected = *value;
    }
    if let Some(topic) = topics.iter().find(|topic| Some(topic.key) == *selected) {
        ui.weak(format!(
            "Исполнитель темы — {}",
            crate::gui::look::agent_title(topic.session.backend)
        ));
    }
}

/// What the selector row changed during one frame. Named fields keep the two
/// optional paths from being transposed — mixing them silently loses a project.
struct RowChanges {
    previous: Option<bool>,
    choice: Option<PathBuf>,
    anchor: Option<PathBuf>,
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
    fn creating_context(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        self.local_project.poll(ui);
        let mut agent = self.local_project.ensure_backend(snapshot);
        let projects: Vec<(PathBuf, String)> = self
            .sidebar
            .projects()
            .map(|(path, name)| (path.to_owned(), name.to_owned()))
            .collect();
        let current = self.scope.as_ref().map(|s| s.1.clone());
        let project = self.resolved_scope(snapshot).map(|(_, path, _)| path);
        let topics: Vec<_> = snapshot
            .topics
            .iter()
            .filter(|topic| project.as_ref().is_none_or(|path| topic.session.cwd.as_path() == path))
            .collect();
        let telegram = matches!(snapshot.bot, BotStatus::Running { .. }) && !topics.is_empty();
        let previous = self.local_mode;
        let mut mode = *self.local_mode.get_or_insert(true);
        let mut choice: Option<PathBuf> = None;
        if !projects.is_empty() {
            project_combo(ui, projects, current.as_deref(), &mut choice);
        }
        if mode {
            self.local_project.directory(ui);
        }
        mode_choice(ui, &mut mode, telegram);
        if mode {
            crate::gui::settings::agent_choice(ui, "Исполнитель", &mut agent);
        } else if matches!(snapshot.bot, BotStatus::Running { .. }) {
            topic_choice(ui, &mut self.selected, &topics);
        } else {
            ui.label("Бот остановлен. Выберите «Локально» для управления задачами без Telegram.");
        }
        self.local_project.backend = Some(agent);
        self.local_mode = Some(mode);
        self.apply_context_selection(snapshot, RowChanges { previous, choice, anchor: project });
    }
    // The migration anchors on the project the scope resolved to before the row mutated
    // `local_mode` — the same pre-flip value the non-creating path captures, never a
    // scope re-resolved in the new mode (which may resolve to nothing). When the pick
    // itself flips the mode (a chosen project has no topic), the anchor becomes the
    // picked path, so the flip guard re-anchors what the user just chose.
    fn apply_context_selection(&mut self, snapshot: &Snapshot, changes: RowChanges) {
        let RowChanges { previous, choice, mut anchor } = changes;
        if let Some(path) = choice {
            self.local_project.choose(&path);
            if self.local_mode == Some(false) {
                self.selected =
                    snapshot.topics.iter().find(|t| t.session.cwd.as_path() == path).map(|t| t.key);
                if self.selected.is_none() {
                    self.local_mode = Some(true);
                }
            }
            anchor = Some(path);
        }
        let mode = self.local_mode == Some(true);
        if previous == Some(!mode)
            && let Some(project) = anchor
        {
            if mode {
                self.local_project.choose(&project);
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
    }
    pub(super) fn context_controls(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        creating: bool,
    ) {
        if creating {
            self.creating_context(ui, snapshot);
            return;
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
        mode_choice(ui, local, telegram);
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
            return;
        }
        if !matches!(snapshot.bot, BotStatus::Running { .. }) {
            ui.label("Бот остановлен. Выберите «Локально» для управления задачами без Telegram.");
            return;
        }
        topic_choice(ui, &mut self.selected, &topics);
    }
    pub(super) fn local_context(&self) -> bool {
        self.scope.as_ref().is_some_and(|scope| scope.0.is_none())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn picker_frame(
        ctx: &egui::Context,
        width: f32,
        events: Vec<egui::Event>,
        mut draw: impl FnMut(&mut egui::Ui),
    ) -> egui::FullOutput {
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let mut output = ctx.run_ui(
            egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(788.0, 540.0),
                )),
                ..Default::default()
            },
            |ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin { left: 100, ..egui::Margin::ZERO })
                    .show(ui, |ui| {
                        ui.set_width(width);
                        draw(ui);
                    });
            },
        );
        output.textures_delta.clear();
        output
    }

    fn text_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
        matching_rect(output, |text| text == label).unwrap_or_else(|| panic!("Missing {label}"))
    }

    fn matching_rect(
        output: &egui::FullOutput,
        matches: impl Fn(&str) -> bool,
    ) -> Option<egui::Rect> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if matches(text.galley.text()) => {
                Some(text.galley.rect.translate(text.pos.to_vec2()))
            }
            _ => None,
        })
    }

    fn click(render: &mut impl FnMut(Vec<egui::Event>) -> egui::FullOutput, pos: egui::Pos2) {
        for pressed in [true, false] {
            render(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    pressed,
                    button: egui::PointerButton::Primary,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        render(vec![]);
    }

    #[test]
    fn project_menu_keeps_names_and_long_paths_inside_narrow_rows() {
        let short = PathBuf::from("/work/backend");
        let long = PathBuf::from(
            "/home/developer/very-long-project-workspace/projects/команды/активные-проекты/Tochka/marketplaces",
        );
        for width in [190.0, 320.0, 420.0] {
            let ctx = egui::Context::default();
            crate::gui::theme::configure(&ctx);
            let mut choice = None;
            let mut render = |events| {
                picker_frame(&ctx, width, events, |ui| {
                    project_combo(
                        ui,
                        vec![
                            (short.clone(), "backend".into()),
                            (long.clone(), "marketplaces".into()),
                        ],
                        Some(&short),
                        &mut choice,
                    );
                })
            };
            let field = text_rect(&render(vec![]), "backend");
            click(&mut render, field.center());
            let output = render(vec![]);
            let name = text_rect(&output, "marketplaces");
            let path = matching_rect(&output, |text| {
                text.starts_with('…') && text.ends_with("/marketplaces")
            })
            .expect("the shortened path should show the project directory");
            assert!(!output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text().starts_with('…') && text.galley.elided)
            }), "the compact project path still loses its last directory");
            assert!(
                path.left() >= 100.0 && path.right() <= 100.0 + width + 2.0,
                "path outside a {width}-point field: {path:?}"
            );
            assert!(
                name.bottom() <= path.top(),
                "the path must have a separate line under its project name"
            );
            click(&mut render, name.center());
            assert_eq!(choice.as_ref(), Some(&long));
        }
    }

    #[test]
    fn topic_menu_keeps_long_titles_and_paths_in_bounds_and_picks_by_key() {
        use hub_core::domain::{AbsolutePath, ChatId, ThreadId, TopicKey, TopicSession};
        use hub_telegram::hub::{TopicState, TopicView};
        let ctx = egui::Context::default();
        crate::gui::theme::configure(&ctx);
        let path = std::env::temp_dir().join("Projects/очень-длинный-каталог/marketplaces");
        let title = "Планирование релиза и проверка изменений для проекта marketplaces";
        let topics = [7, 9].map(|thread| TopicView {
            key: TopicKey { chat: ChatId(-100), thread: ThreadId(thread) },
            title: Some(title.into()),
            session: TopicSession::fresh(
                BackendKind::Qwen,
                AbsolutePath::new(path.clone()).unwrap(),
            ),
            state: TopicState::Waiting,
            memory_enabled: None,
            parameters: String::new(),
            progress: hub_core::progress::TaskProgress::default(),
        });
        let mut selected = Some(topics[0].key);
        let mut render = |events| {
            picker_frame(&ctx, 190.0, events, |ui| {
                topic_choice(ui, &mut selected, &[&topics[0], &topics[1]]);
            })
        };
        let field = text_rect(&render(vec![]), title);
        assert!(field.right() <= 290.0);
        click(&mut render, field.center());
        let output = render(vec![]);
        let details = [7, 9].map(|id| {
            let prefix = format!("#{id} · Qwen Code · …");
            matching_rect(&output, |text| {
                text.starts_with(&prefix) && text.ends_with("/marketplaces")
            })
            .expect("missing compact topic path")
        });
        for row in details {
            assert!(row.left() >= 100.0 && row.right() <= 292.0, "{row:?}");
        }
        assert!(details[0].bottom() < details[1].top());
        let key = |key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        render(vec![key(egui::Key::End)]);
        render(vec![]);
        render(vec![key(egui::Key::Enter)]);
        assert_eq!(selected, Some(topics[1].key));
    }

    #[test]
    fn cancelling_a_directory_edit_preserves_the_open_project_and_discards_pending_picker() {
        let current = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let mut project = LocalProject::default();
        project.choose(current.path());
        let original = project.opened.clone();
        project.editing = true;
        project.path = current.path().join("missing").display().to_string();
        project.open();
        assert!(project.editing && project.error.is_some());
        assert_eq!(project.opened, original);
        let (reply, pending) = mpsc::channel();
        project.picking = Some(pending);
        project.cancel_edit();
        assert!(reply.send(Some(target.path().to_owned())).is_err());
        assert_eq!(project.opened, original);
        assert!(!project.editing && project.error.is_none());
        assert_eq!(Some(Path::new(&project.path)), original.as_deref());
        project.editing = true;
        project.path = target.path().display().to_string();
        project.open();
        assert!(!project.editing);
        let expected = hub_telegram::paths::workspace_root(target.path()).unwrap();
        assert_eq!(project.opened(), Some(expected.as_path()));
    }

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
