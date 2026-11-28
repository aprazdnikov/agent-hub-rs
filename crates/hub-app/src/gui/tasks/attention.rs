use super::{Command, Snapshot, TasksView, local, result::Tab};
use crate::{
    desktop_tasks::{Attention, TaskRow},
    gui::{
        look::agent_title,
        theme::{self, Tone},
    },
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

/// Attention items of one project; feeds the sidebar badge.
pub(super) fn project_attention(snapshot: &Snapshot, project: &Path) -> usize {
    items(snapshot).filter(|item| item.project() == project).count()
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
    fn empty(&mut self, ui: &mut egui::Ui) -> bool {
        theme::card(ui)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.heading("Входящие разобраны");
                ui.label("Здесь появятся новые вопросы и готовые результаты.");
                ui.horizontal_wrapped(|ui| {
                    if self.project.is_some()
                        && ui.add(kylig_ui::Button::new("Показать все проекты")).clicked()
                    {
                        self.project = None;
                    }
                    ui.add(kylig_ui::Button::new("К задачам проекта")).clicked()
                })
                .inner
            })
            .inner
    }

    fn heading(&mut self, ui: &mut egui::Ui, projects: &Projects<'_>) {
        ui.heading("Входящие");
        ui.weak("Вопросы агентов и результаты, которые ждут вашего решения.");
        ui.add_space(12.0);
        let choices = std::iter::once(theme::Choice::new(None, "Все проекты", ""))
            .chain(projects.iter().map(|(&project, name)| {
                theme::Choice::new(Some(project.to_owned()), name.to_string(), "")
                    .with_path(&project.to_string_lossy())
            }))
            .collect::<Vec<_>>();
        if let Some(value) =
            theme::detailed_select(ui, "Проект", "Все проекты", &self.project, &choices, 360.0)
        {
            self.project.clone_from(value);
        }
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
        panel: &mut local::Panel,
        send: &mut impl FnMut(Command),
    ) -> Option<(&'a TaskRow, Tab)> {
        match self {
            Self::Request(live, request, row) => {
                let key = (live.token, request.id);
                ui.push_id(key, |ui| {
                    let (status, severity) = match &request.kind {
                        Kind::Questions(_) => ("Вопрос", kylig_ui::Severity::Info),
                        Kind::Permission(_) => ("Разрешение", kylig_ui::Severity::Warning),
                    };
                    let title = row.map_or_else(
                        || Cow::Owned(format!("Задача #{}", live.id)),
                        |r| Cow::Borrowed(r.title.as_str()),
                    );
                    theme::card(ui).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        card_header(
                            ui,
                            live.id,
                            project,
                            &live.project,
                            row,
                            &title,
                            (status, severity),
                        );
                        ui.add_space(6.0);
                        panel.decision(ui, live, request, send);
                    });
                });
                None
            }
            Self::Task(row, reason) => {
                let (action, tab) = match reason {
                    Attention::Review => ("Проверить результат", Tab::Summary),
                    Attention::CheckFailed => ("Открыть проверку", Tab::Checks),
                    Attention::AgentFailed | Attention::Interrupted => {
                        ("Разобрать запуск", Tab::Checks)
                    }
                    Attention::Git => ("Проверить Git", Tab::Parameters),
                    Attention::Queue => ("Открыть очередь", Tab::Parameters),
                };
                let (status, severity) = match reason {
                    Attention::Review => ("На проверке", kylig_ui::Severity::Info),
                    Attention::CheckFailed | Attention::AgentFailed => {
                        (reason.label(), kylig_ui::Severity::Error)
                    }
                    Attention::Git => ("Подготовка Git", kylig_ui::Severity::Warning),
                    Attention::Queue | Attention::Interrupted => {
                        (reason.label(), kylig_ui::Severity::Warning)
                    }
                };
                let summary = if row.summary.is_empty() { reason.label() } else { &row.summary };
                ui.push_id((&row.project, row.id), |ui| {
                    theme::card(ui)
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            card_header(
                                ui,
                                row.id,
                                project,
                                &row.project,
                                Some(row),
                                &row.title,
                                (status, severity),
                            );
                            ui.add(egui::Label::new(summary).wrap());
                            review_chips(ui, row, reason);
                            ui.add_space(4.0);
                            let button = kylig_ui::Button::new(action);
                            ui.add(if reason == Attention::Review {
                                button.primary()
                            } else {
                                button
                            })
                            .clicked()
                        })
                        .inner
                })
                .inner
                .then_some((row, tab))
            }
        }
    }
}

fn review_chips(ui: &mut egui::Ui, row: &TaskRow, reason: Attention) {
    if reason != Attention::Review || (row.checks.is_none() && row.files == 0) {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        if let Some((passed, total)) = row.checks {
            theme::chip(ui, Tone::Neutral, &format!("Проверки · {passed}/{total}"));
        }
        if row.files > 0 {
            theme::chip(ui, Tone::Neutral, &format!("Файлов · {}", row.files));
        }
    });
}

fn card_header(
    ui: &mut egui::Ui,
    id: u64,
    project: &str,
    path: &Path,
    row: Option<&TaskRow>,
    title: &str,
    (status, severity): (&str, kylig_ui::Severity),
) {
    ui.horizontal_wrapped(|ui| {
        ui.add(kylig_ui::Badge::dot().severity(severity));
        ui.small(status);
        let mut metadata = format!("#{id}   {project}");
        if let Some(row) = row {
            let agent = BackendKind::parse(&row.backend)
                .map_or(row.backend.as_str(), |kind| agent_title(kind));
            metadata.push_str(" / ");
            metadata.push_str(agent);
        }
        ui.weak(metadata).on_hover_text(path.display().to_string());
    });
    ui.add(egui::Label::new(egui::RichText::new(title).size(18.0).strong()).wrap());
}

impl TasksView {
    pub(in crate::gui) fn show_attention(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) -> bool {
        self.refresh_projects(ui.ctx(), snapshot);
        self.local_panel.sync(ui, snapshot);
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
        self.confirmation(ui, self.inbox.project.as_deref());
        let visible: Vec<_> = items(snapshot)
            .filter(|item| self.inbox.project.as_deref().is_none_or(|p| p == item.project()))
            .collect();
        let inline: Vec<(Uuid, Uuid)> = visible
            .iter()
            .filter_map(|item| match item {
                Item::Request(live, request, _) => Some((live.token, request.id)),
                Item::Task(..) => None,
            })
            .collect();
        self.local_panel.feedback(ui, self.inbox.project.as_deref(), &inline);
        if let Err(error) = &snapshot.task_index {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("Не удалось загрузить задачи. {error}"),
            );
        }
        let mut target = None;
        let mut open_chat = false;
        egui::ScrollArea::vertical().id_salt(("inbox_items", &self.inbox.project)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let (requests, results): (Vec<_>, Vec<_>) =
                visible.iter().copied().partition(|item| matches!(item, Item::Request(..)));
            for (is_request, group, heading) in
                [(true, &requests, "Ждут вашего ответа"), (false, &results, "Результаты и ошибки")]
            {
                if group.is_empty() {
                    continue;
                }
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(heading).size(18.0).strong());
                    ui.weak(group.len().to_string());
                });
                let mut card = |ui: &mut egui::Ui, item: Item<'_>| {
                    ui.add_space(6.0);
                    let project = project_label(item.project(), &projects);
                    if let Some((row, tab)) = item.show(ui, &project, &mut self.local_panel, send) {
                        target = Some((row.project.clone(), row.id, tab));
                    }
                };
                if is_request && group.len() == 2 && ui.available_width() >= 780.0 {
                    ui.columns(2, |columns| {
                        for (ui, item) in columns.iter_mut().zip(group) {
                            card(ui, *item);
                        }
                    });
                } else {
                    for &item in group {
                        card(ui, item);
                    }
                }
            }
            if visible.is_empty() && snapshot.task_index.is_ok() {
                ui.add_space(16.0);
                open_chat = self.inbox.empty(ui);
            }
        });
        if let Some((project, id, tab)) = target {
            self.navigate(&project, Some(id), tab);
            self.review_from_inbox = true;
            return true;
        }
        if open_chat {
            if let Some(project) = self.inbox.project.clone() {
                self.navigate(&project, None, Tab::Chat);
            }
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
