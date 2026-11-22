use super::{Command, Snapshot, TasksView, local, result::Tab};
use crate::{
    desktop_tasks::{Attention, TaskRow, excerpt},
    gui::look::agent_title,
    local_execution::{Interaction, Kind, Live},
};
use eframe::egui;
use hub_core::domain::BackendKind;
use std::{
    borrow::Cow,
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Default)]
pub(super) struct Inbox {
    project: Option<PathBuf>,
    expanded: Option<(Uuid, Uuid)>,
}

pub(super) fn jobs(snapshot: &Snapshot) -> impl Iterator<Item = &Live> {
    snapshot.local_jobs.iter().chain(snapshot.local.iter().filter(|live| {
        !snapshot.local_jobs.iter().any(|row| {
            row.token == live.token || (row.id == live.id && row.project == live.project)
        })
    }))
}

pub(super) fn has_request(snapshot: &Snapshot, (token, id): (Uuid, Uuid)) -> bool {
    jobs(snapshot).any(|job| {
        job.active && job.token == token && job.interaction.as_ref().is_some_and(|r| r.id == id)
    })
}

#[derive(Clone, Copy)]
enum Item<'a> {
    Request(&'a Live, &'a Interaction, Option<&'a TaskRow>),
    Task(&'a TaskRow, Attention),
}

fn items(snapshot: &Snapshot) -> impl Iterator<Item = Item<'_>> {
    let rows = snapshot.task_index.as_deref().unwrap_or_default();
    let requests = jobs(snapshot).filter(|job| job.active).filter_map(|job| {
        let request = job.interaction.as_ref()?;
        let row = rows.iter().find(|row| row.id == job.id && row.project == job.project);
        Some(Item::Request(job, request, row))
    });
    let tasks = rows.iter().filter_map(|row| {
        let reason = row.reason?;
        // A live attempt may be newer than the last read of the persisted task index.
        let running =
            jobs(snapshot).any(|j| j.active && j.id == row.id && j.project == row.project);
        (!running).then_some(Item::Task(row, reason))
    });
    requests.chain(tasks)
}

pub(in crate::gui) fn attention_count(snapshot: &Snapshot) -> usize {
    items(snapshot).count()
}

type Projects<'a> = BTreeMap<&'a Path, Cow<'a, str>>;

fn project_name(path: &Path) -> Cow<'_, str> {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy()
}

fn project_label<'a>(path: &'a Path, projects: &Projects<'a>) -> Cow<'a, str> {
    let name = projects.get(path).cloned().unwrap_or_else(|| project_name(path));
    if projects.iter().any(|(other, label)| *other != path && label == &name) {
        path.to_string_lossy()
    } else {
        name
    }
}

impl Inbox {
    fn heading(&mut self, ui: &mut egui::Ui, projects: &Projects<'_>) {
        let width = ui.available_width().min(1040.0);
        let filter = |ui: &mut egui::Ui| {
            let selected =
                self.project.as_deref().map_or(Cow::Borrowed("Все проекты"), |project| {
                    project_label(project, projects)
                });
            egui::ComboBox::from_id_salt("inbox_project")
                .width((width * 0.42).min(205.0))
                .selected_text(selected.as_ref())
                .truncate()
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.project, None, "Все проекты");
                    for &project in projects.keys() {
                        ui.selectable_value(
                            &mut self.project,
                            Some(project.to_path_buf()),
                            project_label(project, projects).as_ref(),
                        )
                        .on_hover_text(project.display().to_string());
                    }
                });
        };
        ui.horizontal_wrapped(|ui| {
            ui.heading("Входящие");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), filter);
        });
        ui.add_space(8.0);
    }
}

impl<'a> Item<'a> {
    fn project(self) -> &'a Path {
        match self {
            Self::Request(job, ..) => &job.project,
            Self::Task(row, _) => &row.project,
        }
    }

    fn show(
        self,
        ui: &mut egui::Ui,
        project: &str,
        inbox: &mut Inbox,
        panel: &mut local::Panel,
        send: &mut impl FnMut(Command),
    ) -> Option<(&'a TaskRow, Tab)> {
        match self {
            Self::Request(live, request, row) => {
                let key = (live.token, request.id);
                ui.push_id(key, |ui| {
                    let (action, reason) = match &request.kind {
                        Kind::Questions(questions) => {
                            let summary = questions.split_first().map_or_else(
                                || live.status.clone(),
                                |(first, rest)| {
                                    let text = excerpt(first.text());
                                    if rest.is_empty() {
                                        text
                                    } else {
                                        format!("{text} · ещё {}", rest.len())
                                    }
                                },
                            );
                            ("Ответить", summary)
                        }
                        Kind::Permission(tool) => {
                            let tool_name = hub_core::render::truncate(tool.tool.trim(), 40);
                            let details = excerpt(&tool.summary);
                            (
                                "Посмотреть запрос",
                                if details.is_empty() {
                                    tool_name
                                } else {
                                    format!("{tool_name} · {details}")
                                },
                            )
                        }
                    };
                    let title = row.map_or_else(
                        || Cow::Owned(format!("Задача #{}", live.id)),
                        |r| Cow::Borrowed(r.title.as_str()),
                    );
                    let expanded = inbox.expanded == Some(key);
                    if row_header(
                        ui,
                        project,
                        &live.project,
                        row,
                        &title,
                        (!expanded).then_some((reason.as_str(), action)),
                    ) {
                        inbox.expanded = Some(key);
                        ui.ctx().request_repaint();
                    }
                    if expanded {
                        ui.add_space(4.0);
                        panel.decision(ui, live, request, send);
                        if ui.button("Свернуть").clicked() {
                            inbox.expanded = None;
                            ui.ctx().request_repaint();
                        }
                    }
                });
                None
            }
            Self::Task(row, reason) => {
                let (action, tab) = match reason {
                    Attention::Review => ("Проверить", Tab::Summary),
                    Attention::CheckFailed => ("Открыть проверку", Tab::Checks),
                    Attention::AgentFailed | Attention::Interrupted => {
                        ("Разобрать запуск", Tab::Checks)
                    }
                    Attention::Git => ("Проверить Git", Tab::Parameters),
                    Attention::Queue => ("Открыть очередь", Tab::Parameters),
                };
                let summary = if row.summary.is_empty() { reason.label() } else { &row.summary };
                ui.push_id((&row.project, row.id), |ui| {
                    row_header(
                        ui,
                        project,
                        &row.project,
                        Some(row),
                        &row.title,
                        Some((summary, action)),
                    )
                })
                .inner
                .then_some((row, tab))
            }
        }
    }
}

fn row_header(
    ui: &mut egui::Ui,
    project: &str,
    path: &Path,
    row: Option<&TaskRow>,
    title: &str,
    preview: Option<(&str, &str)>,
) -> bool {
    let details = |ui: &mut egui::Ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        let metadata = row.map_or_else(
            || project.to_owned(),
            |r| {
                let agent = match BackendKind::parse(&r.backend) {
                    Some(kind) => agent_title(kind),
                    None => r.backend.as_str(),
                };
                format!("{project} · {agent}")
            },
        );
        ui.add(egui::Label::new(egui::RichText::new(metadata).small().weak()).truncate())
            .on_hover_text(path.display().to_string());
        ui.add(egui::Label::new(egui::RichText::new(title).strong()).wrap());
        if let Some((reason, _)) = preview {
            ui.add(egui::Label::new(egui::RichText::new(reason).weak()).wrap());
        }
    };
    let width = ui.available_width();
    let Some((_, action)) = preview else {
        ui.vertical(details);
        return false;
    };
    if width < 440.0 {
        ui.vertical(details);
        return ui.button(action).clicked();
    }
    ui.allocate_ui_with_layout(
        egui::vec2(width, ui.spacing().interact_size.y),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            let clicked = ui.button(action).clicked();
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                details(ui);
            });
            clicked
        },
    )
    .inner
}

impl TasksView {
    pub(in crate::gui) fn show_attention(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) -> bool {
        self.local_panel.sync(ui, snapshot);
        if self.inbox.expanded.is_some_and(|key| !has_request(snapshot, key)) {
            self.inbox.expanded = None;
        }
        ui.set_max_width(ui.available_width().min(1040.0));
        let projects: Projects<'_> = snapshot
            .task_index
            .iter()
            .flatten()
            .map(|r| r.project.as_path())
            .chain(jobs(snapshot).map(|j| j.project.as_path()))
            .chain(self.scope.iter().map(|scope| scope.1.as_path()))
            .map(|path| (path, project_name(path)))
            .chain(self.sidebar.projects().map(|(path, name)| (path, Cow::Borrowed(name))))
            .collect();
        self.inbox.heading(ui, &projects);
        self.local_panel.feedback(ui, self.inbox.project.as_deref(), self.inbox.expanded);
        if let Err(error) = &snapshot.task_index {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Не удалось загрузить задачи. {error}"),
            );
        }
        let visible: Vec<_> = items(snapshot)
            .filter(|item| self.inbox.project.as_deref().is_none_or(|p| p == item.project()))
            .collect();
        let mut target = None;
        let mut open_chat = false;
        egui::ScrollArea::vertical().id_salt(("inbox_items", &self.inbox.project)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (results, heading) in
                [(false, "Ждут вашего ответа"), (true, "Результаты на проверку")]
            {
                let mut group = visible
                    .iter()
                    .copied()
                    .filter(|item| matches!(item, Item::Task(_, Attention::Review)) == results)
                    .peekable();
                if group.peek().is_none() {
                    continue;
                }
                ui.label(egui::RichText::new(heading).strong().weak());
                ui.separator();
                for item in group {
                    ui.add_space(3.0);
                    let project = project_label(item.project(), &projects);
                    if let Some(row) =
                        item.show(ui, &project, &mut self.inbox, &mut self.local_panel, send)
                    {
                        target = Some(row);
                    }
                    ui.add_space(3.0);
                    ui.separator();
                }
                ui.add_space(10.0);
            }
            if visible.is_empty() && snapshot.task_index.is_ok() {
                ui.label(if self.inbox.project.is_some() {
                    "В этом проекте всё разобрано."
                } else {
                    "Всё разобрано."
                });
                ui.horizontal_wrapped(|ui| {
                    if self.inbox.project.is_some() && ui.button("Показать все проекты").clicked()
                    {
                        self.inbox.project = None;
                    }
                    open_chat = ui
                        .button(if self.focused.is_some() {
                            "Вернуться в чат"
                        } else {
                            "Открыть задачи"
                        })
                        .clicked();
                });
            }
        });
        if let Some((row, tab)) = target {
            self.navigate(&row.project, Some(row.id), tab);
            self.review_from_inbox = true;
            return true;
        }
        if open_chat {
            self.leave_review();
            self.page = super::Page::Task;
            self.result_view.select(Tab::Chat, self.focused);
        }
        open_chat
    }

    pub(super) fn selected_live<'a>(&self, snapshot: &'a Snapshot) -> Option<&'a Live> {
        let project = &self.scope.as_ref()?.1;
        let id = self.focused?;
        jobs(snapshot)
            .filter(|live| &live.project == project && live.id == id)
            .max_by_key(|live| (live.active, live.run))
    }

    pub(super) fn local_panels(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        self.local_panel.sync(ui, snapshot);
        if let Some(live) = self.selected_live(snapshot) {
            self.local_panel.show_job(ui, live, send);
        }
    }
}

#[cfg(test)]
mod tests;
