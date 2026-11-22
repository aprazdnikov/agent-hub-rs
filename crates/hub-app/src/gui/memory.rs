//! Memory browser: disk access runs off the egui thread, one request at a time.
use eframe::egui;
use hub_telegram::memory::{Request, View, entry_text, read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

mod context;
mod lifecycle;

type ReadResult = (u64, Result<View, String>);
#[derive(Default)]
pub struct MemoryView {
    admin: lifecycle::AdminUi,
    project: PathBuf,
    refresh_on_open: bool,
    query: String,
    key: String,
    revision: u32,
    offset: usize,
    generation: u64,
    read_started: Option<std::time::Instant>,
    result: Option<View>,
    error: Option<String>,
    pending: Option<Receiver<ReadResult>>,
}
impl MemoryView {
    pub(super) fn open_project(&mut self, project: PathBuf) {
        if self.project != project {
            self.project = project;
            self.query.clear();
            self.key.clear();
            self.revision = 0;
            self.invalidate();
        }
        self.refresh_on_open = true;
    }

    fn refresh(&mut self, home: &Path) {
        if self.refresh_on_open && self.pending.is_none() {
            self.refresh_on_open = false;
            self.load(home, Request::List { query: self.query.clone(), offset: 0 });
        }
    }

    fn invalidate(&mut self) {
        self.admin.clear_context();
        self.invalidate_result();
    }
    fn invalidate_result(&mut self) {
        self.admin.clear();
        self.generation = self.generation.wrapping_add(1);
        self.result = None;
        self.error = None;
        self.offset = 0;
    }
    fn load(&mut self, home: &Path, request: Request) {
        if self.project.as_os_str().is_empty() {
            self.error = Some("Сначала выберите каталог проекта.".into());
            return;
        }
        if matches!(&request, Request::Get { key, .. } if key.trim().is_empty()) {
            self.error = Some("Введите ключ записи или выберите запись из списка.".into());
            return;
        }
        if self.pending.is_some() {
            return;
        }
        let request = match request {
            Request::Get { key, revision } => {
                Request::Admin(Box::new(hub_telegram::memory::lifecycle::Action::Details {
                    key,
                    revision,
                }))
            }
            request => request,
        };
        let generation = self.generation;
        let project = self.project.clone();
        let base = home.join(".agent-hub/memory");
        let (send, receive) = mpsc::channel();
        self.pending = Some(receive);
        self.read_started = Some(std::time::Instant::now());
        std::thread::spawn(move || {
            let _ = send.send((generation, read(&base, &project, &request)));
        });
    }
    fn poll(&mut self, ctx: &egui::Context) {
        if self.pending.is_some()
            && self.read_started.is_some_and(|at| at.elapsed() > std::time::Duration::from_secs(10))
        {
            self.error =
                Some("Чтение занимает более 10 с; ожидается завершение системной операции".into());
        }
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok((generation, result)) => {
                    self.pending = None;
                    if generation == self.generation {
                        match result {
                            Ok(view) => {
                                if let View::List(page) = &view {
                                    self.offset = page.offset;
                                }
                                if let View::Admin(panel) = &view
                                    && let hub_telegram::memory::lifecycle::Panel::ContextReady(
                                        packet,
                                    ) = panel.as_ref()
                                {
                                    if !self.admin.context_matches(packet) {
                                        self.error = Some("Поручение изменилось. Соберите пакет заново перед копированием.".into());
                                        self.result = None;
                                        return;
                                    }
                                    ctx.copy_text(packet.text().to_owned());
                                }
                                self.result = Some(view);
                                self.error = None;
                            }
                            Err(error) => self.error = Some(error),
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.error = Some("Чтение прервано".into());
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.pending.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    fn search_controls(&mut self, ui: &mut egui::Ui) -> Option<Request> {
        let mut request = None;
        let can_browse = self.pending.is_none() && !self.project.as_os_str().is_empty();
        ui.horizontal_wrapped(|ui| {
            ui.label("Поиск");
            if ui.text_edit_singleline(&mut self.query).changed() {
                self.invalidate_result();
            }
            if ui.add_enabled(can_browse, egui::Button::new("Найти / обновить")).clicked()
            {
                request = Some(Request::List { query: self.query.clone(), offset: self.offset });
            }
        });
        request
    }

    fn tools(&mut self, ui: &mut egui::Ui) -> Option<Request> {
        let mut request = None;
        let can_browse = self.pending.is_none() && !self.project.as_os_str().is_empty();
        ui.horizontal_wrapped(|ui| {
            ui.label("Ключ");
            if ui.text_edit_singleline(&mut self.key).changed() {
                self.revision = 0;
            }
            ui.add(egui::DragValue::new(&mut self.revision).range(0..=u32::MAX));
            ui.small("0 = последняя ревизия");
            if ui
                .add_enabled(
                    can_browse && !self.key.trim().is_empty(),
                    egui::Button::new("Открыть запись"),
                )
                .on_disabled_hover_text("Введите ключ записи или выберите запись из списка.")
                .clicked()
            {
                request = Some(Request::Get {
                    key: self.key.clone(),
                    revision: (self.revision > 0).then_some(self.revision),
                });
            }
        });
        self.admin.controls(ui, self.generation, can_browse, &self.query).or(request)
    }
    fn progress(&self, ui: &mut egui::Ui) {
        if self.pending.is_some() {
            ui.label("Загрузка…");
        }
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
            if self.result.is_some() {
                ui.small("Ниже показан предыдущий результат. Запрошенные данные не получены.");
            }
        }
    }
    pub fn show(&mut self, ui: &mut egui::Ui, home: &Path) {
        self.poll(ui.ctx());
        self.refresh(home);
        let name = self.project.file_name().unwrap_or(self.project.as_os_str()).to_string_lossy();
        ui.heading(format!("Память · {name}")).on_hover_text(self.project.display().to_string());
        ui.add_space(8.0);
        let mut request = self.search_controls(ui);
        let tools = ui.collapsing("Инструменты памяти", |ui| self.tools(ui));
        if let Some(next) = tools.body_returned.flatten() {
            request = Some(next);
        }
        self.progress(ui);
        if self.result.as_ref().is_some_and(|view| !matches!(view, View::List(_)))
            && ui
                .add_enabled(self.pending.is_none(), egui::Button::new("К списку записей"))
                .clicked()
        {
            request = Some(Request::List { query: self.query.clone(), offset: self.offset });
        }
        match &self.result {
            Some(View::List(page)) => {
                ui.weak(format!("Найдено записей: {}", page.matched_count)).on_hover_text(format!(
                    "{} · {}/500 ключей · {}/8388608 байт",
                    page.directory.display(),
                    page.key_count,
                    page.bytes
                ));
                if !page.exists {
                    ui.label("Хранилище ещё не создано.");
                } else if page.entries.is_empty() {
                    ui.label("Записи не найдены.");
                }
                for entry in &page.entries {
                    if ui
                        .add_enabled(
                            self.pending.is_none(),
                            egui::Button::new(format!(
                                "{} · r{} · {}",
                                entry.key, entry.revision, entry.title
                            )),
                        )
                        .clicked()
                    {
                        self.key.clone_from(&entry.key);
                        self.revision = 0;
                        request = Some(Request::Get { key: entry.key.clone(), revision: None });
                    }
                }
                if page.offset > 0 || page.entries.len() < page.matched_count {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                self.pending.is_none() && page.offset > 0,
                                egui::Button::new("Назад"),
                            )
                            .clicked()
                        {
                            self.offset = page.offset.saturating_sub(20);
                            request = Some(Request::List {
                                query: self.query.clone(),
                                offset: self.offset,
                            });
                        }
                        if ui
                            .add_enabled(
                                self.pending.is_none()
                                    && page.offset + page.entries.len() < page.matched_count,
                                egui::Button::new("Далее"),
                            )
                            .clicked()
                        {
                            self.offset = page.offset + 20;
                            request = Some(Request::List {
                                query: self.query.clone(),
                                offset: self.offset,
                            });
                        }
                    });
                }
            }
            Some(View::ExportReady { .. }) => {
                ui.label("Экспорт подготовлен для Telegram.");
            }
            Some(View::Admin(panel)) => {
                ui.add_enabled_ui(self.pending.is_none(), |ui| {
                    if let Some(next) = self.admin.panel(ui, panel) {
                        request = Some(next);
                    }
                });
            }
            Some(View::Entry(entry)) => {
                ui.label(entry_text(entry));
            }
            Some(View::Notice(text)) => {
                ui.label(text);
            }
            None => {}
        }
        if let Some(request) = request {
            self.load(home, request);
        }
    }
}

pub(super) fn scope(ui: &mut egui::Ui, label: &str, current: Option<bool>) -> Option<bool> {
    let mut selected = current;
    egui::ComboBox::from_label(label)
        .selected_text(hub_core::options::memory_label(current))
        .show_ui(ui, |ui| {
            for mode in [None, Some(true), Some(false)] {
                ui.selectable_value(&mut selected, mode, hub_core::options::memory_label(mode));
            }
        });
    selected
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_project_does_not_start_a_disk_read() {
        let mut view = MemoryView::default();
        view.load(Path::new("/"), Request::Get { key: String::new(), revision: None });
        assert!(view.pending.is_none());
        assert_eq!(view.error.as_deref(), Some("Сначала выберите каталог проекта."));
    }

    #[test]
    fn empty_key_does_not_start_a_disk_read() {
        let mut view = MemoryView { project: "/project".into(), ..Default::default() };
        view.load(Path::new("/"), Request::Get { key: String::new(), revision: None });
        assert!(view.pending.is_none());
        assert_eq!(
            view.error.as_deref(),
            Some("Введите ключ записи или выберите запись из списка.")
        );
    }

    #[test]
    fn switching_projects_during_a_read_loads_only_the_selected_project() {
        let home = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let mut view =
            MemoryView { project: home.path().into(), pending: Some(rx), ..Default::default() };
        view.open_project(target.path().to_owned());
        view.refresh(home.path());
        assert!(view.refresh_on_open);
        tx.send((0, Ok(View::Notice("old data".into())))).unwrap();
        view.poll(&egui::Context::default());
        view.refresh(home.path());
        let (_, result) =
            view.pending.as_ref().unwrap().recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(
            matches!(result.unwrap(), View::List(page) if page.project == std::fs::canonicalize(target.path()).unwrap())
        );
    }

    #[test]
    fn pending_navigation_does_not_advance_visible_page() {
        let ctx = egui::Context::default();
        let (_tx, rx) = mpsc::channel();
        let mut view = MemoryView {
            pending: Some(rx),
            result: Some(View::List(hub_memory::MemoryPage {
                bound_projects: Vec::new(),
                offset: 0,
                project: PathBuf::from("/repo"),
                directory: PathBuf::from("/store"),
                exists: true,
                bytes: 1,
                key_count: 60,
                matched_count: 60,
                entries: Vec::new(),
            })),
            ..Default::default()
        };
        let frame = |view: &mut MemoryView, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 900.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| view.show(ui, Path::new("/")),
            );
            output.textures_delta.clear();
            output
        };
        frame(&mut view, Vec::new());
        let output = frame(&mut view, Vec::new());
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "Далее" => {
                    Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                }
                _ => None,
            })
            .unwrap();
        for pressed in [true, false] {
            frame(
                &mut view,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        assert_eq!(view.offset, 0);
    }

    #[test]
    fn context_selection_survives_search_but_not_project_change() {
        let ctx = egui::Context::default();
        let mut view = MemoryView {
            project: "/repo".into(),
            result: Some(View::Admin(Box::new(hub_telegram::memory::lifecycle::Panel::Details(
                hub_memory::EntryDetails {
                    entry: hub_memory::Entry {
                        key: "pilot".into(),
                        revision: 1,
                        kind: hub_memory::EntryKind::Progress,
                        title: "Pilot".into(),
                        body: "body".into(),
                        sources: vec!["test".into()],
                        author: "test".into(),
                        created_at: 0,
                    },
                    status: hub_memory::MemoryStatus::Active,
                    revisions: vec![1],
                    history_pruned: false,
                },
            )))),
            ..Default::default()
        };
        let frame = |view: &mut MemoryView, events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1200.0, 1800.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| view.show(ui, Path::new("/")),
            );
            output.textures_delta.clear();
            output
        };
        frame(&mut view, Vec::new());
        let output = frame(&mut view, Vec::new());
        let tools = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == "Инструменты памяти" => {
                    Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                }
                _ => None,
            })
            .unwrap();
        for pressed in [true, false] {
            frame(
                &mut view,
                vec![
                    egui::Event::PointerMoved(tools),
                    egui::Event::PointerButton {
                        pos: tools,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        let output = frame(&mut view, Vec::new());
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text)
                    if text.galley.text() == "Добавить ревизию в пакет контекста" =>
                {
                    Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                }
                _ => None,
            })
            .unwrap();
        for pressed in [true, false] {
            frame(
                &mut view,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        view.invalidate_result();
        let output = frame(&mut view, Vec::new());
        assert!(output.shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text().contains("Пакет контекста следующему агенту (1/20)"))));
        view.project = "/other".into();
        view.invalidate();
        let output = frame(&mut view, Vec::new());
        assert!(output.shapes.iter().any(|shape|matches!(&shape.shape,egui::Shape::Text(text) if text.galley.text().contains("Пакет контекста следующему агенту (0/20)"))));
    }

    #[test]
    fn changed_project_discards_old_result() {
        let mut view = MemoryView::default();
        let (tx, rx) = mpsc::channel();
        view.pending = Some(rx);
        view.invalidate();
        tx.send((0, Ok(View::Notice("old project".into())))).unwrap();
        view.poll(&egui::Context::default());
        assert!(view.result.is_none());
    }
}
