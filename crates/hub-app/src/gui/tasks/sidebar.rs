//! Project and task navigation shared by the inbox, chats and project memory.

use super::{Page, Snapshot, TasksView, attention, result::Tab};
use crate::desktop_tasks::{Attention, TaskRow};
use crate::gui::theme::{self, Tone};
use crate::local_execution::Kind;
use eframe::egui;
use hub_core::domain::AbsolutePath;
use hub_core::tasks::{Task, TaskFilter, TaskStatus};
use hub_core::workspace::{candidate, expand_home};
use hub_telegram::paths::workspace_root;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub(in crate::gui) enum SidebarAction {
    Tasks,
    Memory(PathBuf),
}

#[derive(Default)]
pub(super) struct State {
    query: String,
    branches: BTreeMap<PathBuf, Branch>,
    catalog: Catalog,
    home: Option<PathBuf>,
    picker: Option<mpsc::Receiver<Option<PathBuf>>>,
    error: Option<String>,
    drawer: bool,
}

#[derive(Default)]
struct Branch {
    expanded: Option<bool>,
    last_task: Option<u64>,
    filter: TaskFilter,
}

#[derive(Clone, Default, PartialEq, Eq)]
struct Sources {
    root: Option<PathBuf>,
    folders: BTreeMap<PathBuf, String>,
}

impl Sources {
    fn collect(
        snapshot: &Snapshot,
        remembered: impl Iterator<Item = PathBuf>,
        home: &Path,
    ) -> Self {
        let saved = snapshot.saved.as_ref();
        let root = saved
            .map(|draft| draft.workspace_root.trim())
            .filter(|value| !value.is_empty())
            .map(|value| expand_home(value, home));
        let mut folders: BTreeMap<_, _> = remembered
            .chain(snapshot.topics.iter().map(|topic| topic.session.cwd.as_path().to_owned()))
            .chain(snapshot.task_index.iter().flatten().map(|row| row.project.clone()))
            .map(|path| (path, String::new()))
            .collect();
        for project in saved.into_iter().flat_map(|draft| &draft.projects) {
            let path =
                candidate(root.as_deref().unwrap_or(Path::new("")), home, Some(&project.cwd));
            if path.is_absolute() {
                folders.insert(path, project.name.clone());
            }
        }
        Self { root, folders }
    }

    fn root(&self) -> Result<Option<AbsolutePath>, String> {
        self.root
            .as_deref()
            .map(|root| workspace_root(root).map_err(|error| error.to_string()))
            .transpose()
    }
}

fn checked_project(path: &Path, root: Option<&AbsolutePath>) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err("Укажите абсолютный путь к проекту.".to_owned());
    }
    let path = workspace_root(path).map_err(|error| error.to_string())?;
    if root.is_some_and(|root| !path.as_path().starts_with(root.as_path())) {
        return Err("Каталог находится вне настроенного корня проектов.".to_owned());
    }
    Ok(path.as_path().to_owned())
}

type Projects = BTreeMap<PathBuf, String>;

impl Sources {
    fn resolve(&self) -> Result<Projects, String> {
        let root = self.root()?;
        let mut projects = Projects::new();
        for (candidate, name) in &self.folders {
            if let Ok(path) = checked_project(candidate, root.as_ref()) {
                let label = projects.entry(path.clone()).or_insert_with(|| {
                    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
                });
                if !name.is_empty() {
                    label.clone_from(name);
                }
            }
        }
        Ok(projects)
    }
}

#[derive(Default)]
struct Catalog {
    sources: Sources,
    checked: Option<Instant>,
    pending: Option<mpsc::Receiver<(Sources, Result<Projects, String>)>>,
    projects: Projects,
    error: Option<String>,
}

impl Catalog {
    fn update(&mut self, sources: Sources, ctx: &egui::Context) {
        if sources != self.sources {
            if self.sources.root == sources.root {
                self.projects.retain(|path, _| sources.folders.contains_key(path));
            } else {
                self.projects.clear();
                self.pending = None;
            }
            self.sources = sources;
            self.error = None;
            self.checked = None;
        }
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok((scanned, result)) => {
                    self.pending = None;
                    if scanned == self.sources {
                        self.checked = Some(Instant::now());
                        match result {
                            Ok(projects) => {
                                self.projects = projects;
                                self.error = None;
                            }
                            Err(error) => {
                                self.projects.clear();
                                self.error = Some(error);
                            }
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.error = Some("Не удалось загрузить проекты.".into());
                    self.checked = Some(Instant::now());
                }
            }
        }
        if self.sources.folders.is_empty()
            || self.checked.is_some_and(|at| at.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        let sources = self.sources.clone();
        let ctx = ctx.clone();
        let (tx, rx) = mpsc::channel();
        match std::thread::Builder::new().name("project-catalog".into()).spawn(move || {
            let projects = sources.resolve();
            let _ = tx.send((sources, projects));
            ctx.request_repaint();
        }) {
            Ok(_) => self.pending = Some(rx),
            Err(error) => {
                self.error = Some(format!("Не удалось загрузить проекты: {error}"));
                self.checked = Some(Instant::now());
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Current<'a> {
    project: Option<&'a Path>,
    focused: Option<u64>,
    loaded: Option<&'a [Task]>,
    in_tasks: bool,
}

struct TaskLink<'a> {
    id: u64,
    title: &'a str,
    backend: &'a str,
    prompt: &'a str,
    status: TaskStatus,
    archived: bool,
    task: Option<&'a Task>,
    reason: Option<Attention>,
}

impl<'a> TaskLink<'a> {
    fn from_index(row: &'a TaskRow) -> Self {
        Self {
            id: row.id,
            title: &row.title,
            backend: &row.backend,
            prompt: &row.prompt,
            status: row.status,
            archived: row.archived,
            task: None,
            reason: row.reason,
        }
    }

    fn matches(&self, query: &str, project_matches: bool, filter: TaskFilter) -> bool {
        let by_id = query.strip_prefix('#').and_then(|id| id.parse().ok()) == Some(self.id);
        filter.includes(self.status, self.archived)
            && (project_matches
                || by_id
                || self.title.to_lowercase().contains(query)
                || self.prompt.to_lowercase().contains(query))
    }

    fn mark(&self, project: &Path, snapshot: &Snapshot) -> (Mark, &'static str) {
        if let Some(live) = attention::jobs(snapshot)
            .find(|live| live.active && live.project == project && live.id == self.id)
        {
            return match live.interaction.as_ref().map(|request| &request.kind) {
                Some(Kind::Questions(_)) => (Mark::Question, "Ждёт ответа"),
                Some(Kind::Permission(_)) => (Mark::Warning, "Ждёт разрешения"),
                None => (Mark::Running, "Выполняется"),
            };
        }
        if self.archived {
            return (Mark::Idle, "В архиве");
        }
        if self.task.is_some_and(Task::active) {
            return (Mark::Running, "Выполняется");
        }
        match self.reason {
            Some(Attention::Review) => (Mark::Review, "На проверке"),
            Some(reason) => (Mark::Warning, reason.label()),
            None if self.task.is_some_and(|task| task.queue.is_some()) => (Mark::Idle, "В очереди"),
            None => (Mark::Idle, self.status.label()),
        }
    }
}

fn task_links<'a>(
    snapshot: &'a Snapshot,
    current: Current<'a>,
) -> BTreeMap<&'a Path, Vec<TaskLink<'a>>> {
    let mut groups: BTreeMap<&Path, Vec<TaskLink<'_>>> = BTreeMap::new();
    let mut reasons = BTreeMap::new();
    for row in snapshot.task_index.iter().flatten() {
        if current.loaded.is_some() && current.project == Some(row.project.as_path()) {
            reasons.insert(row.id, row.reason);
        } else {
            groups.entry(&row.project).or_default().push(TaskLink::from_index(row));
        }
    }
    if let (Some(project), Some(tasks)) = (current.project, current.loaded) {
        groups.insert(
            project,
            tasks
                .iter()
                .map(|task| TaskLink {
                    id: task.id,
                    title: &task.title,
                    backend: &task.backend,
                    prompt: &task.prompt,
                    status: task.status,
                    archived: task.archived,
                    task: Some(task),
                    reason: reasons.get(&task.id).copied().flatten(),
                })
                .collect(),
        );
    }
    groups
}

enum Intent {
    Chat(PathBuf, Option<u64>),
    Memory(PathBuf),
    Rules(PathBuf),
    New(Option<PathBuf>),
    Filter(PathBuf, TaskFilter),
}

#[derive(Clone, Copy)]
enum Mark {
    Idle,
    Running,
    Review,
    Question,
    Warning,
}

// The mockup (sidebar-review.html) fades opacity 1→0.35 over a 1.6s period;
// repainting every 500ms keeps the dot moving without a busy frame loop.
const PULSE_PERIOD_SECS: f32 = 1.6;
const PULSE_MIN_OPACITY: f32 = 0.35;
const PULSE_REPAINT: Duration = Duration::from_millis(500);

fn pulse(ui: &egui::Ui) -> f32 {
    let Ok(elapsed) = Duration::try_from_secs_f64(ui.ctx().input(|input| input.time).max(0.0))
    else {
        return 1.0;
    };
    let phase = (elapsed.as_secs_f32() * std::f32::consts::TAU / PULSE_PERIOD_SECS).cos();
    PULSE_MIN_OPACITY + (1.0 - PULSE_MIN_OPACITY) * (0.5 + 0.5 * phase)
}

impl Mark {
    fn paint(self, ui: &egui::Ui, center: egui::Pos2) {
        let visuals = ui.visuals();
        match self {
            Self::Idle => {
                ui.painter().circle_filled(
                    center,
                    4.0,
                    visuals.widgets.noninteractive.bg_stroke.color,
                );
            }
            Self::Running => {
                ui.painter().circle_filled(
                    center,
                    4.0,
                    visuals.hyperlink_color.gamma_multiply(pulse(ui)),
                );
                ui.ctx().request_repaint_after(PULSE_REPAINT);
            }
            Self::Review => {
                ui.painter().circle_stroke(
                    center,
                    3.0,
                    egui::Stroke::new(2.0, theme::review_color(ui)),
                );
            }
            Self::Question => {
                ui.painter().circle_filled(center, 4.0, visuals.hyperlink_color);
            }
            Self::Warning => {
                ui.painter().circle_filled(center, 4.0, theme::tone_colors(ui, Tone::Error).1);
            }
        }
    }
}

fn row_button(
    ui: &mut egui::Ui,
    link: &TaskLink<'_>,
    project: &Path,
    snapshot: &Snapshot,
    selected: bool,
) -> bool {
    let (mark, state) = link.mark(project, snapshot);
    let accent = ui.visuals().hyperlink_color;
    let title = egui::RichText::new(link.title);
    let title = if selected { title.color(accent).strong() } else { title };
    let width = ui.available_width();
    let text = egui::WidgetText::from(title).into_galley_impl(
        ui.ctx(),
        ui.style(),
        egui::text::TextWrapping {
            max_width: (width - 2.0 * ui.spacing().button_padding.x - 24.0).max(1.0),
            max_rows: 2,
            ..Default::default()
        },
        egui::TextStyle::Button.into(),
        egui::Align::Center,
    );
    let mark_id = egui::Id::new("task-state");
    let icon = egui::Atom::custom(mark_id, egui::vec2(11.0, 16.0));
    let mut detail = format!("#{} · {}", link.id, link.backend);
    if !state.is_empty() {
        detail.push_str(" · ");
        detail.push_str(state);
    }
    let button = egui::Button::selectable(selected, (icon, text, egui::Atom::grow()))
        .gap(6.0)
        .min_size(egui::vec2(width, 28.0))
        .atom_ui(ui);
    if let Some(rect) = button.rect(mark_id) {
        mark.paint(ui, rect.center());
    }
    button.response.on_hover_text(format!("{}\n{detail}", link.title)).clicked()
}

fn project_menu(
    ui: &mut egui::Ui,
    project: &Path,
    name: &str,
    filter: TaskFilter,
    intent: &mut Option<Intent>,
) {
    let menu = ui.add(egui::Button::new("...").frame_when_inactive(false));
    egui::Popup::menu(&menu).show(|ui| {
        ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
        if ui.button("Новая задача").clicked() {
            *intent = Some(Intent::New(Some(project.to_owned())));
            ui.close();
        }
        if ui.button("Память проекта").clicked() {
            *intent = Some(Intent::Memory(project.to_owned()));
            ui.close();
        }
        if ui.button("Правила проекта").clicked() {
            *intent = Some(Intent::Rules(project.to_owned()));
            ui.close();
        }
        ui.separator();
        egui::containers::menu::SubMenuButton::from_button(
            egui::Button::new("Фильтр задач").right_text(">"),
        )
        .ui(ui, |ui| {
            ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
            for choice in [TaskFilter::Active, TaskFilter::All, TaskFilter::Archived]
                .into_iter()
                .chain(TaskStatus::ALL.map(TaskFilter::Status))
            {
                if ui.add(egui::Button::selectable(filter == choice, choice.label())).clicked() {
                    *intent = Some(Intent::Filter(project.to_owned(), choice));
                    ui.close();
                }
            }
        });
    });
    let label = format!("Действия проекта «{name}»");
    menu.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label));
    menu.on_hover_text(label);
}

const BADGE_RESERVE: f32 = 40.0;

struct ProjectRow<'a> {
    path: &'a Path,
    name: &'a str,
    expanded: bool,
    filter: TaskFilter,
}

impl ProjectRow<'_> {
    fn header(
        &self,
        ui: &mut egui::Ui,
        selected: bool,
        searching: bool,
        attention: usize,
        intent: &mut Option<Intent>,
    ) -> bool {
        let toggle = ui.add_enabled(
            !searching,
            egui::Button::new("").frame_when_inactive(false).min_size(egui::vec2(25.0, 29.0)),
        );
        let mut arrow = toggle.clone();
        arrow.rect = egui::Rect::from_center_size(toggle.rect.center(), egui::vec2(11.0, 10.0));
        egui::collapsing_header::paint_default_icon(ui, f32::from(self.expanded), &arrow);
        let label =
            format!("{} {}", if self.expanded { "Свернуть" } else { "Развернуть" }, self.name);
        toggle.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, !searching, &label)
        });
        let toggled = toggle.on_hover_text(label).clicked();
        // The chip (9px padding each side plus a few small digits) joins the fixed menu slot.
        let badge = f32::from(attention > 0) * BADGE_RESERVE;
        let width = (ui.available_width() - 37.0 - badge).max(20.0);
        let text = egui::RichText::new(self.name).strong();
        let text = if selected { text.color(ui.visuals().hyperlink_color) } else { text };
        let button =
            egui::Button::new((text, egui::Atom::grow())).truncate().frame_when_inactive(false);
        if ui
            .add_sized([width, 29.0], button)
            .on_hover_text(self.path.display().to_string())
            .clicked()
        {
            *intent = Some(Intent::Chat(self.path.to_owned(), None));
        }
        if attention > 0 {
            theme::chip(ui, Tone::Accent, &attention.to_string());
        }
        project_menu(ui, self.path, self.name, self.filter, intent);
        toggled
    }
}

impl State {
    pub(super) fn projects(&self) -> impl Iterator<Item = (&Path, &str)> {
        self.catalog.projects.iter().map(|(path, name)| (path.as_path(), name.as_str()))
    }

    fn pick(&mut self, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        let root = self.catalog.sources.root.clone();
        match std::thread::Builder::new().name("project-folder".into()).spawn(move || {
            let dialog = rfd::FileDialog::new().set_title("Открыть проект");
            let dialog = if let Some(root) = root { dialog.set_directory(root) } else { dialog };
            let _ = tx.send(dialog.pick_folder());
            ctx.request_repaint();
        }) {
            Ok(_) => self.picker = Some(rx),
            Err(error) => self.error = Some(format!("Не удалось открыть выбор папки: {error}")),
        }
    }

    fn picked(&mut self, ctx: &egui::Context) -> Option<PathBuf> {
        let receiver = self.picker.as_ref()?;
        match receiver.try_recv() {
            Ok(path) => {
                self.picker = None;
                path
            }
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(150));
                None
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.picker = None;
                self.error = Some("Не удалось выбрать папку проекта.".into());
                None
            }
        }
    }

    fn tree(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        current: Current<'_>,
    ) -> Option<Intent> {
        let query = self.query.trim().to_lowercase();
        let groups = task_links(snapshot, current);
        let mut intent = None;
        let mut matched = false;
        let first = self.catalog.projects.keys().next();
        for (project, name) in &self.catalog.projects {
            let active = current.project == Some(project.as_path());
            let project_matches = query.is_empty() || name.to_lowercase().contains(&query);
            let branch = self.branches.get(project);
            let filter = branch.map_or(TaskFilter::Active, |b| b.filter);
            let project_links = groups.get(project.as_path()).map_or(&[][..], Vec::as_slice);
            let links: Vec<_> = project_links
                .iter()
                .filter(|link| link.matches(&query, project_matches, filter))
                .collect();
            if !project_matches && links.is_empty() {
                continue;
            }
            matched = true;
            let expanded = !query.is_empty()
                || branch
                    .and_then(|b| b.expanded)
                    .unwrap_or(active || (current.project.is_none() && first == Some(project)));
            ui.push_id(project, |ui| {
                let row = ProjectRow { path: project, name, expanded, filter };
                let attention_items = attention::project_attention(snapshot, project);
                if ui
                    .horizontal(|ui| {
                        row.header(
                            ui,
                            active && current.in_tasks,
                            !query.is_empty(),
                            attention_items,
                            &mut intent,
                        )
                    })
                    .inner
                {
                    self.branches.entry(project.clone()).or_default().expanded = Some(!expanded);
                }
                if expanded {
                    egui::Frame::new()
                        .inner_margin(egui::Margin { left: 14, ..egui::Margin::ZERO })
                        .show(ui, |ui| {
                            if filter != TaskFilter::Active {
                                ui.weak(filter.label());
                            }
                            if links.is_empty() {
                                if query.is_empty()
                                    && filter == TaskFilter::Active
                                    && !project_links.is_empty()
                                {
                                    if ui
                                        .add(
                                            egui::Button::new("Показать архив")
                                                .frame_when_inactive(false),
                                        )
                                        .clicked()
                                    {
                                        intent = Some(Intent::Filter(
                                            project.clone(),
                                            TaskFilter::Archived,
                                        ));
                                    }
                                } else {
                                    ui.weak(if query.is_empty() && filter == TaskFilter::Active {
                                        "Задач пока нет"
                                    } else {
                                        "Задачи не найдены"
                                    });
                                }
                            }
                            for link in links {
                                let selected =
                                    active && current.in_tasks && current.focused == Some(link.id);
                                if ui
                                    .push_id(link.id, |ui| {
                                        row_button(ui, link, project, snapshot, selected)
                                    })
                                    .inner
                                {
                                    intent = Some(Intent::Chat(project.clone(), Some(link.id)));
                                }
                            }
                        });
                }
            });
            ui.add_space(3.0);
        }
        if !matched {
            ui.weak(if self.catalog.projects.is_empty() && self.catalog.pending.is_some() {
                "Загружаем проекты…"
            } else if self.catalog.projects.is_empty() {
                "Проектов пока нет"
            } else {
                "Задачи не найдены"
            });
        }
        intent
    }

    fn contents(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        current: Current<'_>,
    ) -> Option<Intent> {
        ui.spacing_mut().button_padding = egui::vec2(8.0, 5.0);
        ui.spacing_mut().item_spacing = egui::vec2(3.0, 3.0);
        ui.spacing_mut().interact_size.y = 29.0;
        ui.strong("Проекты");
        ui.add_space(6.0);
        ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .id_salt("tree-search")
                .desired_width(f32::INFINITY)
                .char_limit(256)
                .margin(egui::Margin::symmetric(8, 5))
                .hint_text("Найти задачу"),
        )
        .on_hover_text("Название проекта или задачи, текст поручения, #номер задачи");
        ui.add_space(6.0);
        let max_height = (ui.available_height() - 88.0).max(52.0);
        let mut intent = egui::ScrollArea::vertical()
            .id_salt("tree-projects")
            .max_height(max_height)
            .auto_shrink([false, true])
            .show(ui, |ui| self.tree(ui, snapshot, current))
            .inner;
        ui.add_space(8.0);
        if ui.button("+  Новая задача").clicked() {
            let project = current
                .project
                .filter(|path| self.catalog.projects.contains_key(*path))
                .or_else(|| self.catalog.projects.keys().next().map(PathBuf::as_path));
            intent = Some(Intent::New(project.map(Path::to_owned)));
        }
        if ui
            .add_enabled(
                self.picker.is_none(),
                egui::Button::new("Открыть папку…").frame_when_inactive(false),
            )
            .clicked()
        {
            self.pick(ui.ctx());
        }
        if let Some(error) = self
            .error
            .as_ref()
            .or(self.catalog.error.as_ref())
            .or(snapshot.task_index.as_ref().err())
        {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        intent
    }
}

impl TasksView {
    fn select_project_task(&mut self, project: &Path, id: Option<u64>, tab: Tab) {
        if let (Some((_, previous, _)), Some(focused)) = (&self.scope, self.focused)
            && self.tasks.iter().any(|task| task.id == focused && task.project == *previous)
        {
            self.sidebar.branches.entry(previous.clone()).or_default().last_task = Some(focused);
        }
        let same_topic = self.local_mode == Some(false)
            && self.scope.as_ref().is_some_and(|(key, path, _)| {
                key.is_some() && *key == self.selected && path == project
            });
        if !same_topic {
            self.local_mode = Some(true);
            self.local_project.choose(project);
        }
        self.leave_review();
        self.page = Page::Task;
        let branch = self.sidebar.branches.entry(project.to_owned()).or_default();
        branch.expanded = Some(true);
        self.focused = id.or(branch.last_task);
        branch.last_task = self.focused;
        self.result_view.select(tab, self.focused);
    }

    pub(super) fn navigate(&mut self, project: &Path, id: Option<u64>, tab: Tab) {
        self.sidebar.query.clear();
        self.sidebar.branches.entry(project.to_owned()).or_default().filter = TaskFilter::Active;
        self.select_project_task(project, id, tab);
    }

    fn apply_navigation(&mut self, intent: &Intent) -> Result<Option<SidebarAction>, String> {
        let path = match intent {
            Intent::Chat(p, _) | Intent::Memory(p) | Intent::Rules(p) | Intent::New(Some(p)) => p,
            Intent::Filter(path, filter) => {
                let branch = self.sidebar.branches.entry(path.clone()).or_default();
                branch.filter = *filter;
                branch.expanded = Some(true);
                return Ok(None);
            }
            Intent::New(None) => {
                self.leave_review();
                self.page = Page::New;
                return Ok(Some(SidebarAction::Tasks));
            }
        };
        let root = self.sidebar.catalog.sources.root()?;
        let project = checked_project(path, root.as_ref())?;
        let task = if let Intent::Chat(_, task) = intent { *task } else { None };
        self.select_project_task(&project, task, Tab::Chat);
        match intent {
            Intent::Memory(_) => return Ok(Some(SidebarAction::Memory(project))),
            Intent::Rules(_) => self.page = Page::Rules,
            Intent::New(_) => self.page = Page::New,
            Intent::Filter(..) | Intent::Chat(..) => {}
        }
        Ok(Some(SidebarAction::Tasks))
    }

    pub(in crate::gui) fn project_navigation(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        in_tasks: bool,
    ) -> Option<SidebarAction> {
        let project = self.scope.as_ref().map(|(_, path, _)| path.as_path());
        let local = self.local_project.opened();
        self.sidebar.catalog.update(
            Sources::collect(
                snapshot,
                self.sidebar
                    .branches
                    .keys()
                    .cloned()
                    .chain(project.into_iter().chain(local).map(Path::to_owned)),
                self.sidebar.home.get_or_insert_with(crate::gui::home_or_root),
            ),
            ui.ctx(),
        );
        let selected = if self.local_mode == Some(true) { local } else { project.or(local) };
        let current = Current {
            project: selected,
            focused: self.focused,
            loaded: (selected == project
                && project.is_some()
                && self.refreshed.is_some()
                && (self.error.is_none() || !self.tasks.is_empty()))
            .then_some(self.tasks.as_slice()),
            in_tasks: in_tasks && self.page == Page::Task,
        };
        let picked = self.sidebar.picked(ui.ctx()).map(|project| Intent::Chat(project, None));
        let frame = egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .inner_margin(egui::Margin::symmetric(10, 12));
        let intent = if ui.available_width() < 660.0 {
            egui::Panel::top("compact-project-navigation")
                .frame(
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .inner_margin(egui::Margin::symmetric(9, 3)),
                )
                .show(ui, |ui| {
                    if ui.button("Проекты").clicked() {
                        self.sidebar.drawer = true;
                    }
                });
            if self.sidebar.drawer {
                let rect = ui.available_rect_before_wrap();
                let id = egui::Id::new("project-drawer");
                let area = egui::Modal::default_area(id).anchor(
                    egui::Align2::LEFT_TOP,
                    egui::vec2(0.0, rect.top() - ui.ctx().content_rect().top()),
                );
                let modal = egui::Modal::new(id).frame(frame).area(area).show(ui.ctx(), |ui| {
                    ui.set_width((rect.width() - 40.0).clamp(140.0, 280.0));
                    ui.set_max_height((rect.height() - 32.0).max(160.0));
                    self.sidebar.contents(ui, snapshot, current)
                });
                if modal.should_close() {
                    self.sidebar.drawer = false;
                }
                modal.inner
            } else {
                None
            }
        } else {
            self.sidebar.drawer = false;
            egui::Panel::left("task-project-navigation")
                .default_size(226.0)
                .size_range(176.0..=320.0)
                .frame(frame)
                .show(ui, |ui| self.sidebar.contents(ui, snapshot, current))
                .inner
        };
        match self.apply_navigation(&picked.or(intent)?) {
            Ok(action) => {
                self.sidebar.error = None;
                if action.is_some() {
                    self.sidebar.drawer = false;
                }
                action
            }
            Err(error) => {
                self.sidebar.error = Some(error);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_execution::{Interaction, Live};
    use hub_core::domain::{BackendKind, Question, QuestionOption, Selection};
    use hub_core::options::OptionsDraft;
    use hub_core::projects::ProjectDraft;
    use hub_core::settings::Draft;
    use hub_core::tasks::TaskBook;
    use uuid::Uuid;

    fn settings(root: &Path) -> Draft {
        Draft {
            workspace_root: root.display().to_string(),
            projects: vec![ProjectDraft {
                options: OptionsDraft::default(),
                id: "atlas".into(),
                name: "Атлас".into(),
                cwd: "Atlas".into(),
                backend: "codex".into(),
                actions: Vec::new(),
                checks: Vec::new(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn discovered_folders_and_memory_obey_the_current_root() {
        let root = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let named = root.path().join("Atlas");
        let opened = root.path().join("Empty");
        std::fs::create_dir(&named).unwrap();
        std::fs::create_dir(&opened).unwrap();
        let snapshot = Snapshot { saved: Some(settings(root.path())), ..Default::default() };
        let sources = Sources::collect(
            &snapshot,
            [opened.clone(), elsewhere.path().to_owned()].into_iter(),
            root.path(),
        );
        let projects = sources.resolve().unwrap();
        let named = checked_project(&named, None).unwrap();
        let opened = checked_project(&opened, None).unwrap();
        assert_eq!(projects.get(&named).map(String::as_str), Some("Атлас"));
        assert_eq!(projects.get(&opened).map(String::as_str), Some("Empty"));
        assert_eq!(projects.len(), 2);
        let mut view = TasksView::default();
        view.sidebar.catalog.sources = sources;
        assert!(view.apply_navigation(&Intent::Memory(elsewhere.path().to_owned())).is_err());
        assert_eq!(
            view.apply_navigation(&Intent::Memory(named.clone())),
            Ok(Some(SidebarAction::Memory(named)))
        );
        let (_tx, rx) = mpsc::channel();
        view.sidebar.catalog.pending = Some(rx);
        view.sidebar.catalog.projects = projects;
        view.sidebar.catalog.update(
            Sources { root: Some(elsewhere.path().to_owned()), ..Default::default() },
            &egui::Context::default(),
        );
        assert!(view.sidebar.projects().next().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_redirected_outside_the_root_is_rechecked_before_opening_memory() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let mut view = TasksView::default();
        view.sidebar.catalog.sources.root = Some(root.path().to_owned());
        assert!(view.apply_navigation(&Intent::Chat(project.clone(), None)).is_ok());
        std::fs::remove_dir(&project).unwrap();
        std::os::unix::fs::symlink(outside.path(), &project).unwrap();
        assert!(view.apply_navigation(&Intent::Memory(project)).is_err());
    }

    struct Screen {
        ctx: egui::Context,
        view: TasksView,
        snapshot: Snapshot,
        width: f32,
        last: Option<SidebarAction>,
    }

    impl Screen {
        fn themed(snapshot: Snapshot) -> Self {
            let ctx = egui::Context::default();
            crate::gui::configure_theme(&ctx);
            Self { ctx, view: TasksView::default(), snapshot, width: 1024.0, last: None }
        }

        fn frame(
            &mut self,
            events: Vec<egui::Event>,
            time: f64,
            system_theme: Option<egui::Theme>,
        ) -> egui::FullOutput {
            let mut output = self.ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(self.width, if self.width < 660.0 { 360.0 } else { 650.0 }),
                    )),
                    events,
                    time: Some(time),
                    system_theme,
                    ..Default::default()
                },
                |ui| {
                    if let Some(action) = self.view.project_navigation(ui, &self.snapshot, true) {
                        self.last = Some(action);
                    }
                },
            );
            output.textures_delta.clear();
            output
        }

        fn draw(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
            text_labels(&self.frame(events, 0.0, None))
        }

        fn wait_until(&mut self, needle: &str, time: f64) -> egui::FullOutput {
            let until = Instant::now() + Duration::from_secs(3);
            loop {
                let output = self.frame(Vec::new(), time, Some(egui::Theme::Light));
                if text_labels(&output).iter().any(|(text, _)| text == needle) {
                    let _ = self.frame(Vec::new(), time, Some(egui::Theme::Light));
                    return self.frame(Vec::new(), time, Some(egui::Theme::Light));
                }
                assert!(Instant::now() < until, "missing {needle:?}");
            }
        }

        fn click(&mut self, label: &str, index: usize) {
            let until = Instant::now() + Duration::from_secs(3);
            let pos = loop {
                let labels = self.draw(Vec::new());
                if let Some((_, pos)) = labels.iter().filter(|(text, _)| text == label).nth(index) {
                    break *pos;
                }
                assert!(Instant::now() < until, "missing {label:?} in {labels:?}");
                std::thread::sleep(Duration::from_millis(2));
            };
            // Give newly opened panels a full frame to register their hit targets.
            self.draw(vec![egui::Event::PointerMoved(pos)]);
            for pressed in [true, false] {
                self.draw(vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]);
            }
        }
    }

    #[test]
    fn tree_search_archive_and_project_menu_target_the_right_folder() {
        let root = tempfile::tempdir().unwrap();
        let atlas = root.path().join("Atlas");
        let hub = root.path().join("Hub");
        for path in [&atlas, &hub] {
            std::fs::create_dir(path).unwrap();
        }
        let atlas = checked_project(&atlas, None).unwrap();
        let hub = checked_project(&hub, None).unwrap();
        let mut book = TaskBook::default();
        let first = book.create(hub.clone(), "codex", "Первая", "Собрать").unwrap();
        let archived = book.create(hub.clone(), "codex", "Старый отчёт", "Искомое слово").unwrap();
        book.set_archived(archived, 1, true).unwrap();
        let snapshot = Snapshot {
            saved: Some(settings(root.path())),
            task_index: Ok(book.tasks().iter().map(TaskRow::from).collect()),
            ..Default::default()
        };
        let mut screen = Screen {
            ctx: egui::Context::default(),
            view: TasksView::default(),
            snapshot,
            width: 1024.0,
            last: None,
        };
        screen.view.navigate(&atlas, None, Tab::Chat);
        screen.view.set_scope(Some((None, atlas.clone(), BackendKind::Codex)));
        screen.view.refreshed = Some(Instant::now());
        screen.click("Найти задачу", 0);
        screen.draw(vec![egui::Event::Text("СОБРАТЬ".into())]);
        screen.click("Первая", 0);
        assert_eq!(screen.last, Some(SidebarAction::Tasks));
        assert_eq!(screen.view.focused, Some(first));
        screen.view.set_scope(Some((None, hub.clone(), BackendKind::Codex)));
        screen.view.tasks = book.tasks().to_vec();
        screen.view.refreshed = Some(Instant::now());
        screen.view.sidebar.query.clear();
        screen.click("...", 1);
        screen.click("Фильтр задач", 0);
        screen.click("Архив", 0);
        assert_eq!(screen.view.sidebar.branches[&hub].filter, TaskFilter::Archived);
        screen.click("Старый отчёт", 0);
        assert_eq!(screen.view.focused, Some(archived));
        screen.click("...", 0);
        screen.click("Память проекта", 0);
        assert_eq!(screen.last, Some(SidebarAction::Memory(atlas.clone())));
        assert_eq!(screen.view.local_project.opened(), Some(atlas.as_path()));
        screen.click("Hub", 0);
        assert_eq!(screen.view.focused, Some(archived));
    }

    #[test]
    fn a_picked_empty_folder_remains_available_for_project_memory() {
        let folder = tempfile::tempdir().unwrap();
        let path = checked_project(folder.path(), None).unwrap();
        let mut screen = Screen {
            ctx: egui::Context::default(),
            view: TasksView::default(),
            snapshot: Snapshot::default(),
            width: 960.0,
            last: None,
        };
        assert!(screen.draw(Vec::new()).iter().any(|(text, _)| text == "Открыть папку…"));
        let (tx, rx) = mpsc::channel();
        screen.view.sidebar.picker = Some(rx);
        tx.send(Some(path.clone())).unwrap();
        screen.draw(Vec::new());
        assert_eq!(screen.last, Some(SidebarAction::Tasks));
        screen.click("...", 0);
        screen.click("Память проекта", 0);
        assert_eq!(screen.last, Some(SidebarAction::Memory(path)));
    }

    fn text_labels(output: &egui::FullOutput) -> Vec<(String, egui::Pos2)> {
        output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    Some((
                        text.galley.text().to_owned(),
                        text.galley.rect.translate(text.pos.to_vec2()).center(),
                    ))
                } else {
                    None
                }
            })
            .collect()
    }

    fn dots(output: &egui::FullOutput) -> Vec<egui::epaint::CircleShape> {
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Circle(circle) => Some(*circle),
                _ => None,
            })
            .collect()
    }

    fn repaint_delay(output: &egui::FullOutput) -> Duration {
        output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map_or(Duration::MAX, |viewport| viewport.repaint_delay)
    }

    fn live_running(project: &Path, id: u64) -> Live {
        Live {
            token: Uuid::new_v4(),
            project: project.to_owned(),
            id,
            run: Some(1),
            active: true,
            status: "Выполняется".into(),
            interaction: None,
        }
    }

    fn live_question(project: &Path, id: u64) -> Live {
        let options = ["Да", "Нет"]
            .map(|label| QuestionOption { label: label.into(), description: String::new() });
        Live {
            token: Uuid::new_v4(),
            project: project.to_owned(),
            id,
            run: Some(1),
            active: true,
            status: "Ожидает ответа".into(),
            interaction: Some(Interaction {
                id: Uuid::new_v4(),
                kind: Kind::Questions(vec![
                    Question::new(
                        "Продолжать?".into(),
                        String::new(),
                        options.into(),
                        Selection::Multiple,
                    )
                    .unwrap(),
                ]),
            }),
        }
    }

    #[test]
    fn marks_use_status_colors() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("Atlas");
        std::fs::create_dir(&project).unwrap();
        let project = checked_project(&project, None).unwrap();
        let mut book = TaskBook::default();
        let running = book.create(project.clone(), "codex", "Рабочая задача", "Пилить").unwrap();
        let question =
            book.create(project.clone(), "codex", "Задача с вопросом", "Уточнить").unwrap();
        let review =
            book.create(project.clone(), "codex", "Задача на проверке", "Собрать").unwrap();
        let warning =
            book.create(project.clone(), "codex", "Задача с ошибкой", "Сломалось").unwrap();
        let idle = book.create(project.clone(), "codex", "Спокойная задача", "Потом").unwrap();
        let rows = vec![
            TaskRow::from(book.get(running).unwrap()),
            TaskRow::from(book.get(question).unwrap()),
            TaskRow { reason: Some(Attention::Review), ..TaskRow::from(book.get(review).unwrap()) },
            TaskRow {
                reason: Some(Attention::AgentFailed),
                ..TaskRow::from(book.get(warning).unwrap())
            },
            TaskRow::from(book.get(idle).unwrap()),
        ];
        let snapshot = Snapshot {
            saved: Some(settings(root.path())),
            task_index: Ok(rows),
            local_jobs: vec![live_running(&project, running), live_question(&project, question)],
            ..Default::default()
        };
        let mut screen = Screen::themed(snapshot);
        let output = screen.wait_until("Рабочая задача", 0.0);
        let dots = dots(&output);
        let accent = egui::Color32::from_rgb(13, 100, 79);
        let border = egui::Color32::from_rgb(213, 221, 215);
        let ring = egui::Color32::from_rgb(183, 121, 31);
        let error = egui::Color32::from_rgb(148, 49, 47);
        assert_eq!(dots.len(), 5, "one dot per task row");
        let size = |value: f32, expected: f32| (value - expected).abs() < 0.01;
        assert_eq!(
            dots.iter().filter(|dot| size(dot.radius, 4.0) && dot.stroke.width < 0.01).count(),
            4,
            "four filled dots are 8px across"
        );
        assert_eq!(
            dots.iter().filter(|dot| dot.fill == accent && size(dot.radius, 4.0)).count(),
            2,
            "running and question use the accent dot"
        );
        assert!(dots.iter().any(|dot| dot.fill == border), "idle uses the border color");
        assert!(dots.iter().any(|dot| dot.fill == error), "warning uses the error color");
        assert!(
            dots.iter().any(|dot| size(dot.radius, 3.0)
                && dot.fill == egui::Color32::TRANSPARENT
                && size(dot.stroke.width, 2.0)
                && dot.stroke.color == ring),
            "review is a 2px ring in the review color"
        );
    }

    #[test]
    fn running_dot_pulses_and_schedules_repaint_only_while_running() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("Atlas");
        std::fs::create_dir(&project).unwrap();
        let project = checked_project(&project, None).unwrap();
        let mut book = TaskBook::default();
        let running = book.create(project.clone(), "codex", "Рабочая задача", "Пилить").unwrap();
        let question =
            book.create(project.clone(), "codex", "Задача с вопросом", "Уточнить").unwrap();
        let rows = vec![
            TaskRow::from(book.get(running).unwrap()),
            TaskRow::from(book.get(question).unwrap()),
        ];
        let mut screen = Screen::themed(Snapshot {
            saved: Some(settings(root.path())),
            task_index: Ok(rows),
            local_jobs: vec![live_running(&project, running), live_question(&project, question)],
            ..Default::default()
        });
        let accent = egui::Color32::from_rgb(13, 100, 79);
        let settled = screen.wait_until("Рабочая задача", 0.0);
        assert!(
            dots(&settled).iter().any(|dot| dot.fill == accent),
            "the pulse starts at full opacity"
        );
        // A frame starved by a busy machine can finalize after its own 500 ms deadline;
        // the pulse re-schedules every frame, so retry before trusting one wall-clock sample.
        let mut delay = repaint_delay(&settled);
        for _ in 0..5 {
            if Duration::from_millis(400) <= delay && delay <= Duration::from_millis(500) {
                break;
            }
            delay = repaint_delay(&screen.wait_until("Рабочая задача", 0.0));
        }
        assert!(
            Duration::from_millis(400) <= delay && delay <= Duration::from_millis(500),
            "a running task schedules the next pulse frame, got {delay:?}"
        );
        let faded = screen.wait_until("Рабочая задача", 0.8);
        assert!(
            dots(&faded).iter().any(|dot| (70..=110).contains(&dot.fill.a())),
            "half a period later the running dot is dimmed near the mockup's 0.35"
        );
        screen.snapshot.local_jobs.retain(|job| job.interaction.is_some());
        let calm = screen.wait_until("Задача с вопросом", 0.0);
        assert_eq!(
            repaint_delay(&calm),
            Duration::MAX,
            "without running tasks nothing schedules a repaint"
        );
    }

    #[test]
    fn project_badge_counts_attention_items() {
        let root = tempfile::tempdir().unwrap();
        let atlas = root.path().join("Atlas");
        let hub = root.path().join("Hub");
        for path in [&atlas, &hub] {
            std::fs::create_dir(path).unwrap();
        }
        let atlas = checked_project(&atlas, None).unwrap();
        let hub = checked_project(&hub, None).unwrap();
        let mut book = TaskBook::default();
        let ask = book.create(atlas.clone(), "codex", "Отчёт по очереди", "Собрать").unwrap();
        let done = book.create(atlas.clone(), "codex", "Поиск по каталогу", "Собрать").unwrap();
        let calm = book.create(hub.clone(), "codex", "Спокойная задача", "Собрать").unwrap();
        let rows = vec![
            TaskRow::from(book.get(ask).unwrap()),
            TaskRow { reason: Some(Attention::Review), ..TaskRow::from(book.get(done).unwrap()) },
            TaskRow::from(book.get(calm).unwrap()),
        ];
        let snapshot = Snapshot {
            saved: Some(settings(root.path())),
            task_index: Ok(rows),
            local_jobs: vec![live_question(&atlas, ask)],
            ..Default::default()
        };
        let mut screen = Screen::themed(snapshot);
        let output = screen.wait_until("Атлас", 0.0);
        let labels = text_labels(&output);
        let header_y = |name: &str| {
            labels.iter().find(|(text, _)| text == name).map(|(_, pos)| pos.y).unwrap()
        };
        let badges: Vec<_> = labels.iter().filter(|(text, _)| text == "2").collect();
        assert_eq!(badges.len(), 1, "the attention chip appears once");
        assert!(
            (badges[0].1.y - header_y("Атлас")).abs() <= 6.0,
            "the chip sits in the header of the project with attention"
        );
        assert!(
            !labels.iter().any(|(text, pos)| matches!(text.as_str(), "1" | "2" | "3")
                && (pos.y - header_y("Hub")).abs() <= 6.0),
            "the clean project shows no badge"
        );
        // The chip must not push the project menu out of the row.
        screen.click("...", 0);
        let menu = screen.draw(Vec::new());
        assert!(
            menu.iter().any(|(text, _)| text == "Новая задача"),
            "menu opens next to the badge"
        );
    }

    #[test]
    fn compact_tree_opens_and_dismisses_after_task_selection() {
        let project = tempfile::tempdir().unwrap();
        let mut book = TaskBook::default();
        let id = book
            .create(
                checked_project(project.path(), None).unwrap(),
                "codex",
                "Доступная задача",
                "Проверить",
            )
            .unwrap();
        book.set_archived(id, 1, true).unwrap();
        let snapshot = Snapshot {
            task_index: Ok(vec![TaskRow::from(book.get(id).unwrap())]),
            ..Default::default()
        };
        let mut screen = Screen {
            ctx: egui::Context::default(),
            view: TasksView::default(),
            snapshot,
            width: 640.0,
            last: None,
        };
        assert!(!screen.draw(Vec::new()).iter().any(|(label, _)| label == "Доступная задача"));
        screen.click("Проекты", 0);
        screen.click("Показать архив", 0);
        assert!(screen.view.sidebar.drawer);
        screen.click("Доступная задача", 0);
        assert_eq!(screen.last, Some(SidebarAction::Tasks));
        assert_eq!(screen.view.focused, Some(id));
        assert!(!screen.view.sidebar.drawer);
    }
}
