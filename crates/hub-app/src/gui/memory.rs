//! Project memory. Reads, previews and mutations use the same backend as Telegram.
use crate::gui::theme;
use eframe::egui;
use hub_memory::{AdminAction, ContextPacket, EntryDetails, MemoryOverview, StatusFilter};
use hub_telegram::memory::lifecycle::{Action, Panel};
use hub_telegram::memory::{Request, View, read};
use kylig_ui::{Button, InfoBar, ProgressRing, Severity};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

mod context;
mod lifecycle;
mod records;

type ReadResult = (u64, Result<View, String>);

#[derive(Default, Clone, Copy, PartialEq)]
enum Tab {
    #[default]
    Records,
    Context,
    Manage,
}

#[derive(Default)]
pub struct MemoryView {
    admin: lifecycle::AdminUi,
    tab: Tab,
    records: records::Records,
    context: context::ContextDraft,
    project: PathBuf,
    workspace_root: Option<PathBuf>,
    project_picker: Option<Receiver<Option<PathBuf>>>,
    refresh_on_open: bool,
    reopen_revision: Option<(String, Option<u32>)>,
    generation: u64,
    read_started: Option<std::time::Instant>,
    overview: Option<MemoryOverview>,
    detail: Option<EntryDetails>,
    packet: Option<(ContextPacket, bool)>,
    error: Option<String>,
    notice: Option<String>,
    pending: Option<Receiver<ReadResult>>,
    effect: Option<Effect>,
    last_export: Option<PathBuf>,
    progress_label: &'static str,
    reveal_progress: bool,
}

enum Effect {
    Export(PathBuf),
    Refresh,
    Reset,
    Update(AdminAction),
}

fn request(action: Action) -> Request {
    Request::Admin(Box::new(action))
}

impl MemoryView {
    pub(super) fn set_workspace_root(&mut self, root: Option<PathBuf>) {
        if self.workspace_root != root {
            self.workspace_root = root;
            self.project_picker = None;
            self.open_project(PathBuf::new());
        }
    }

    pub(super) fn open_project(&mut self, project: PathBuf) {
        if self.project != project {
            self.project = project;
            self.tab = Tab::Records;
            self.reopen_revision = None;
            self.project_picker = None;
            self.records = records::Records::default();
            self.admin = lifecycle::AdminUi::default();
            self.context = context::ContextDraft::default();
            self.generation = self.generation.wrapping_add(1);
            self.overview = None;
            self.detail = None;
            self.packet = None;
            self.error = None;
            self.notice = None;
        }
        self.refresh_on_open = !self.project.as_os_str().is_empty();
    }

    pub(super) fn project_picker(
        &mut self,
        ui: &mut egui::Ui,
        known: &std::collections::BTreeMap<PathBuf, String>,
    ) {
        ui.heading("Память проекта");
        ui.weak("Записи агентов, история ревизий и передача контекста.");
        ui.add_space(12.0);
        let mut projects = known.clone();
        if !self.project.as_os_str().is_empty() {
            projects.entry(self.project.clone()).or_default();
        }
        if self.project.as_os_str().is_empty()
            && self.project_picker.is_none()
            && let Some(path) = projects.keys().next()
        {
            self.open_project(path.clone());
        }
        let name = |path: &Path, label: &str| {
            if label.is_empty() {
                path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
            } else {
                label.to_owned()
            }
        };
        let choices: Vec<_> = projects
            .into_iter()
            .map(|(path, label)| {
                let title = name(&path, &label);
                let detail = path.display().to_string();
                theme::Choice::new(path, title, "").with_path(&detail)
            })
            .collect();
        ui.horizontal_wrapped(|ui| {
            if let Some(value) = theme::detailed_select(
                ui,
                "Проект",
                "Выберите проект",
                &self.project,
                &choices,
                360.0,
            ) {
                self.open_project(value.clone());
            }
            if ui
                .add_enabled(self.project_picker.is_none(), Button::new("Выбрать папку…"))
                .clicked()
            {
                let (tx, rx) = mpsc::channel();
                self.project_picker = Some(rx);
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let _ =
                        tx.send(rfd::FileDialog::new().set_title("Каталог проекта").pick_folder());
                    ctx.request_repaint();
                });
            }
        });
        if let Some(picker) = &self.project_picker {
            match picker.try_recv() {
                Ok(path) => {
                    self.project_picker = None;
                    if let Some(path) = path {
                        match super::tasks::check_memory_project(
                            &path,
                            self.workspace_root.as_deref(),
                        ) {
                            Ok(path) => self.open_project(path),
                            Err(error) => self.error = Some(error),
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => self.project_picker = None,
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    fn refresh(&mut self, home: &Path) {
        if self.refresh_on_open && self.pending.is_none() {
            self.refresh_on_open = false;
            self.load(home, self.records.list(0));
        } else if self.pending.is_none()
            && let Some((key, revision)) = self.reopen_revision.take()
        {
            self.load(home, request(Action::Details { key, revision }));
        }
    }

    fn load(&mut self, home: &Path, mut next: Request) {
        if self.project.as_os_str().is_empty() {
            self.error = Some("Сначала выберите каталог проекта.".into());
            return;
        }
        if let Request::Get { key, revision } = next {
            if key.trim().is_empty() {
                self.error = Some("Введите ключ записи или выберите запись из списка.".into());
                return;
            }
            next = request(Action::Details { key, revision });
        }
        if self.pending.is_some() {
            return;
        }
        let Request::Admin(action) = &next else {
            return;
        };
        self.progress_label = match action.as_ref() {
            Action::Overview { .. } => "Читаем память проекта…",
            Action::Details { .. } => "Читаем ревизию записи…",
            Action::Context(_) => "Собираем пакет контекста…",
            Action::ValidateContext(_) => "Проверяем свежесть пакета…",
            Action::Export(_) | Action::PrepareReset(_) => "Сохраняем экспорт памяти…",
            Action::ImportFile(_)
            | Action::Import { .. }
            | Action::Compare { .. }
            | Action::ReopenImport(_) => "Проверяем импорт…",
            Action::Bind { .. } | Action::Preview(_) => "Готовим предварительный просмотр…",
            _ => "Применяем изменения…",
        };
        self.effect = match action.as_ref() {
            Action::Export(path) => Some(Effect::Export(path.clone())),
            Action::Apply(p) => Some(Effect::Update(p.action.clone())),
            Action::ApplyImport(_) => Some(Effect::Refresh),
            Action::Reset { .. } | Action::ApplyBinding(_) => Some(Effect::Reset),
            _ => None,
        };
        let generation = self.generation;
        let project = self.project.clone();
        let root = self.workspace_root.clone();
        let base = hub_memory::base_in(home);
        let (send, receive) = mpsc::channel();
        self.pending = Some(receive);
        self.reveal_progress = true;
        self.read_started = Some(std::time::Instant::now());
        self.error = None;
        std::thread::spawn(move || {
            // Recheck at use time: a folder from the catalogue may have been moved or relinked.
            let result = super::tasks::check_memory_project(&project, root.as_deref())
                .and_then(|project| read(&base, &project, &next));
            let _ = send.send((generation, result));
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.pending else {
            return;
        };
        match rx.try_recv() {
            Ok((generation, result)) => {
                self.pending = None;
                if generation == self.generation {
                    match result {
                        Ok(View::Admin(panel)) => self.received(ctx, *panel),
                        Ok(_) => self.error = Some("Неизвестный ответ хранилища.".into()),
                        Err(error) => self.error = Some(error),
                    }
                }
                self.effect = None;
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.pending = None;
                self.error = Some("Чтение прервано".into());
            }
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(75));
            }
        }
    }

    fn received(&mut self, ctx: &egui::Context, panel: Panel) {
        self.error = None;
        match panel {
            Panel::Overview(overview) => {
                if std::mem::take(&mut self.records.report_refresh) {
                    self.notice = Some(format!(
                        "Список обновлён. Записей в проекте — {}.",
                        overview.page.key_count
                    ));
                }
                self.overview = Some(overview);
            }
            Panel::Details(detail) => {
                if self.notice.is_none() {
                    self.notice = Some(format!(
                        "Открыта запись {} · r{}",
                        detail.entry.key, detail.entry.revision
                    ));
                }
                self.detail = Some(detail);
            }
            Panel::Context(packet) => {
                self.notice = Some("Пакет готов к проверке.".into());
                self.packet = Some((packet, false));
            }
            Panel::ContextReady(packet) => {
                if self.context.matches(&packet) {
                    ctx.copy_text(packet.text().to_owned());
                    self.packet = Some((packet, true));
                    self.notice = Some("Проверенный пакет скопирован.".into());
                } else {
                    self.packet = None;
                    self.error = Some(
                        "Поручение изменилось. Соберите пакет заново перед копированием.".into(),
                    );
                }
            }
            Panel::Confirm(preview) => self.admin.confirm_preview(preview),
            Panel::Notice(text) => {
                self.notice = Some(text);
                if let Some(effect) = self.effect.take() {
                    match effect {
                        Effect::Export(path) => {
                            self.last_export = Some(path);
                            return;
                        }
                        Effect::Reset => self.context = context::ContextDraft::default(),
                        Effect::Update(action) => {
                            self.context.apply_change(&action);
                            if let Some(detail) = &self.detail {
                                let key = &detail.entry.key;
                                let version = detail.entry.revision;
                                match action {
                                    AdminAction::SetStatus { key: changed, .. }
                                        if changed == *key =>
                                    {
                                        self.reopen_revision = Some((changed, Some(version)));
                                    }
                                    AdminAction::Prune { revisions }
                                        if revisions.contains_key(key) =>
                                    {
                                        let kept = !revisions
                                            .get(key)
                                            .is_some_and(|pruned| pruned.contains(&version));
                                        self.reopen_revision =
                                            Some((key.clone(), kept.then_some(version)));
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Effect::Refresh => {}
                    }
                    self.overview = None;
                    self.packet = None;
                    self.detail = None;
                    self.admin.completed();
                    self.refresh_on_open = true;
                }
            }
            panel => {
                if let Panel::Reset { receipt, .. } = &panel {
                    self.last_export = Some(receipt.path().to_owned());
                }
                self.admin.receive(panel);
            }
        }
    }

    fn progress(&mut self, ui: &mut egui::Ui) {
        if self.pending.is_some() {
            let row = ui.horizontal_wrapped(|ui| {
                ui.add(ProgressRing::indeterminate().size(20.0));
                ui.strong(self.progress_label);
                if self.read_started.is_some_and(|at| at.elapsed().as_secs() >= 10) {
                    ui.weak("Операция занимает более 10 с.");
                }
            });
            if std::mem::take(&mut self.reveal_progress)
                && !ui.clip_rect().contains_rect(row.response.rect)
            {
                row.response.scroll_to_me(None);
            }
        }
        for (severity, message) in
            [(Severity::Error, &mut self.error), (Severity::Success, &mut self.notice)]
        {
            if let Some(text) = message {
                let mut open = true;
                InfoBar::new(severity, text.as_str()).open(&mut open).show(ui);
                if !open {
                    *message = None;
                }
            }
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, home: &Path) {
        self.poll(ui.ctx());
        self.refresh(home);
        self.progress(ui);
        if self.project.as_os_str().is_empty() {
            ui.label("Выберите каталог проекта, чтобы открыть его память.");
            return;
        }
        let picked = self.admin.poll_picker(ui, self.generation);
        let next = ui
            .add_enabled_ui(self.pending.is_none() && !self.admin.picking(), |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (tab, label) in [
                        (Tab::Records, "Записи"),
                        (Tab::Context, "Пакет контекста"),
                        (Tab::Manage, "Управление"),
                    ] {
                        ui.selectable_value(&mut self.tab, tab, label);
                    }
                });
                ui.separator();
                match self.tab {
                    Tab::Records => {
                        if let Some(details) = &self.detail {
                            if ui.add(Button::new("‹ К списку записей")).clicked() {
                                self.detail = None;
                                self.notice = None;
                                (!self.records.exact).then(|| {
                                    self.records
                                        .list(self.overview.as_ref().map_or(0, |o| o.page.offset))
                                })
                            } else {
                                self.records.detail(
                                    ui,
                                    details,
                                    &mut self.context,
                                    &mut self.tab,
                                    &mut self.notice,
                                )
                            }
                        } else {
                            self.records.show(
                                ui,
                                self.overview.as_ref(),
                                &mut self.tab,
                                &mut self.admin,
                            )
                        }
                    }
                    Tab::Context => {
                        let build = self.context.controls(ui, &mut self.tab);
                        if self
                            .packet
                            .take_if(|(packet, _)| !self.context.matches(packet))
                            .is_some()
                        {
                            self.notice = None;
                        }
                        if self.tab == Tab::Records {
                            self.detail = None;
                            self.records.exact = false;
                        }
                        let copy = self
                            .packet
                            .as_ref()
                            .and_then(|(packet, ready)| self.context.preview(ui, packet, *ready));
                        if self.context.procedure(ui) {
                            self.notice = Some("Процедура скопирована.".into());
                        }
                        build.or(copy)
                    }
                    Tab::Manage => self.admin.show(
                        ui,
                        self.generation,
                        &self.project,
                        self.overview.as_ref(),
                        self.last_export.as_deref(),
                    ),
                }
            })
            .inner;
        if let Some(action) = self.admin.confirmation(ui).map(request).or(picked).or(next) {
            self.notice = None;
            self.load(home, action);
            ui.ctx().request_repaint();
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
mod tests;
