//! Memory browser: disk access runs off the egui thread, one request at a time.
use crate::supervisor::{Command, SaveStatus, Snapshot};
use eframe::egui;
use hub_telegram::memory::{POLICY, Request, View, entry_text, read};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use tokio::sync::oneshot;

mod context;
mod lifecycle;

type ReadResult = (u64, Result<View, String>);
#[derive(Default)]
pub struct MemoryView {
    admin: lifecycle::AdminUi,
    project: String,
    query: String,
    key: String,
    revision: u32,
    offset: usize,
    generation: u64,
    known_raw: Vec<String>,
    known: Vec<String>,
    catalog: Option<Receiver<Vec<String>>>,
    read_started: Option<std::time::Instant>,
    result: Option<View>,
    error: Option<String>,
    save_error: Option<String>,
    pending: Option<Receiver<ReadResult>>,
    picking: Option<Receiver<Option<PathBuf>>>,
    saving: Option<oneshot::Receiver<Result<(), String>>>,
}
impl MemoryView {
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
        if self.project.trim().is_empty() {
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
        let project = hub_core::workspace::expand_home(&self.project, home);
        let base = home.join(".agent-hub/memory");
        let (send, receive) = mpsc::channel();
        self.pending = Some(receive);
        self.read_started = Some(std::time::Instant::now());
        std::thread::spawn(move || {
            let _ = send.send((generation, read(&base, &project, &request)));
        });
    }
    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.catalog {
            match rx.try_recv() {
                Ok(known) => {
                    self.known = known;
                    self.catalog = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => self.catalog = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
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
        if let Some(rx) = &self.picking {
            match rx.try_recv() {
                Ok(path) => {
                    self.picking = None;
                    if let Some(path) = path {
                        self.project = path.display().to_string();
                        self.invalidate();
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.picking = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rx) = &mut self.saving {
            match rx.try_recv() {
                Ok(result) => {
                    self.saving = None;
                    self.save_error = result.err();
                }
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.saving = None;
                    self.save_error = Some("Результат сохранения неизвестен".into());
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if self.pending.is_some()
            || self.picking.is_some()
            || self.saving.is_some()
            || self.catalog.is_some()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
    fn controls(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        home: &Path,
        send: &mut impl FnMut(Command),
    ) {
        if let Some(saved) = &snapshot.saved {
            let mut enabled = saved.memory.enabled;
            if ui
                .add_enabled(
                    self.saving.is_none() && snapshot.save_status == SaveStatus::Idle,
                    egui::Checkbox::new(&mut enabled, "Подключать память к агентам по умолчанию"),
                )
                .changed()
            {
                let (reply, rx) = oneshot::channel();
                self.saving = Some(rx);
                send(Command::SetMemory { enabled, revision: snapshot.save_revision, reply });
            }
        } else {
            ui.label("Сначала сохраните настройки приложения для управления подключением.");
        }
        ui.small(POLICY);
        if self.saving.is_some() {
            ui.label("Сохранение…");
        }
        if let Some(error) = &self.save_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        for topic in &snapshot.topics {
            if topic.state == hub_telegram::hub::TopicState::Running {
                ui.small(format!(
                    "Тема {}: подключение активного процесса — {}",
                    topic.key.thread.0,
                    match topic.memory_enabled {
                        Some(true) => "включено",
                        Some(false) => "выключено",
                        None => "неизвестно",
                    }
                ));
            }
        }
        let previous = self.project.clone();
        ui.horizontal(|ui| {
            ui.label("Каталог проекта:");
            ui.text_edit_singleline(&mut self.project);
            if ui.add_enabled(self.picking.is_none(), egui::Button::new("Выбрать папку…")).clicked()
            {
                let (tx, rx) = mpsc::channel();
                self.picking = Some(rx);
                std::thread::spawn(move || {
                    let _ = tx.send(rfd::FileDialog::new().pick_folder());
                });
            }
        });
        let mut known: Vec<String> = snapshot
            .topics
            .iter()
            .map(|topic| topic.session.cwd.as_path().display().to_string())
            .collect();
        if let Some(saved) = &snapshot.saved {
            for project in &saved.projects {
                known.push(project.cwd.clone());
            }
        }
        known.sort();
        known.dedup();
        if known != self.known_raw && self.catalog.is_none() {
            self.known_raw.clone_from(&known);
            let home = home.to_path_buf();
            let (tx, rx) = mpsc::channel();
            self.catalog = Some(rx);
            std::thread::spawn(move || {
                let mut paths: Vec<_> = known
                    .into_iter()
                    .map(|raw| {
                        let path = hub_core::workspace::expand_home(&raw, &home);
                        std::fs::canonicalize(&path).unwrap_or(path).display().to_string()
                    })
                    .collect();
                paths.sort();
                paths.dedup();
                let _ = tx.send(paths);
            });
        }
        egui::ComboBox::from_id_salt("memory_projects").selected_text("Известные проекты").show_ui(
            ui,
            |ui| {
                for path in &self.known {
                    ui.selectable_value(&mut self.project, path.clone(), path.as_str());
                }
            },
        );
        if self.project != previous {
            self.invalidate();
        }
    }
    fn search_controls(&mut self, ui: &mut egui::Ui) -> Option<Request> {
        let mut request = None;
        let can_browse = self.pending.is_none() && !self.project.trim().is_empty();
        if self.project.trim().is_empty() {
            ui.label("Сначала выберите каталог проекта выше, затем нажмите «Найти / обновить».");
        }
        ui.horizontal(|ui| {
            ui.label("Поиск:");
            if ui.text_edit_singleline(&mut self.query).changed() {
                self.invalidate_result();
            }
            if ui.add_enabled(can_browse, egui::Button::new("Найти / обновить")).clicked()
            {
                request = Some(Request::List { query: self.query.clone(), offset: self.offset });
            }
        });
        ui.horizontal(|ui| {
            ui.label("Ключ:");
            if ui.text_edit_singleline(&mut self.key).changed() {
                self.revision = 0;
            }
            ui.add(egui::DragValue::new(&mut self.revision).range(0..=u32::MAX));
            ui.small("0 = последняя ревизия");
            if ui.add_enabled(can_browse && !self.key.trim().is_empty(), egui::Button::new("Открыть запись"))
                .on_disabled_hover_text("Выберите каталог проекта и введите ключ записи. Для просмотра списка нажмите «Найти / обновить».")
                .clicked()
            {
                request = Some(Request::Get {
                    key: self.key.clone(),
                    revision: (self.revision > 0).then_some(self.revision),
                });
            }
        });
        request
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
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        home: &Path,
        send: &mut impl FnMut(Command),
    ) {
        self.poll(ui.ctx());
        super::theme::page_heading(
            ui,
            "Память",
            "Записи проекта и источники без обращения к модели.",
        );
        self.controls(ui, snapshot, home, send);
        let mut request = self.search_controls(ui);
        if let Some(next) = self.admin.controls(
            ui,
            self.generation,
            self.pending.is_none() && !self.project.trim().is_empty(),
            &self.query,
        ) {
            request = Some(next);
        }
        self.progress(ui);
        egui::ScrollArea::vertical().show(ui, |ui| match &self.result {
            Some(View::List(page)) => {
                ui.label(format!("Проект: {}", page.project.display()));
                ui.label(format!("Хранилище: {}", page.directory.display()));
                ui.label(format!(
                    "Ключей: {}/500 · байт: {}/8388608 · найдено: {}",
                    page.key_count, page.bytes, page.matched_count
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
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            self.pending.is_none() && page.offset > 0,
                            egui::Button::new("←"),
                        )
                        .clicked()
                    {
                        self.offset = page.offset.saturating_sub(20);
                        request =
                            Some(Request::List { query: self.query.clone(), offset: self.offset });
                    }
                    if ui
                        .add_enabled(
                            self.pending.is_none()
                                && page.offset + page.entries.len() < page.matched_count,
                            egui::Button::new("→"),
                        )
                        .clicked()
                    {
                        self.offset = page.offset + 20;
                        request =
                            Some(Request::List { query: self.query.clone(), offset: self.offset });
                    }
                });
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
            None => {
                ui.label("Выберите проект и нажмите «Найти / обновить».");
            }
        });
        if let Some(request) = request {
            self.load(home, request);
        }
    }
}

pub(super) fn scope(ui: &mut egui::Ui, current: Option<bool>) -> Option<bool> {
    let mut selected = current;
    egui::ComboBox::from_label("Память проекта")
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
    fn successful_toggle_preserves_failed_read_warning() {
        let (tx, rx) = oneshot::channel();
        let mut view = MemoryView {
            error: Some("read failed".into()),
            saving: Some(rx),
            result: Some(View::Notice("old data".into())),
            ..Default::default()
        };
        tx.send(Ok(())).unwrap();
        view.poll(&egui::Context::default());
        assert_eq!(view.error.as_deref(), Some("read failed"));
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
                |ui| view.show(ui, &Snapshot::default(), Path::new("/"), &mut |_| {}),
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
                egui::Shape::Text(text) if text.galley.text() == "→" => {
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
                |ui| view.show(ui, &Snapshot::default(), Path::new("/"), &mut |_| {}),
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
