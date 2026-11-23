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
use hub_core::domain::{BackendKind, ToolRequest};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;

#[derive(Default)]
pub(super) struct Inbox {
    project: Option<PathBuf>,
    details: Option<(Uuid, Uuid)>,
}

/// Spec «Входящие»: collapsed permission summary shows up to 3 visual rows.
const SUMMARY_LINES: usize = 3;

/// Visual rows a monospace permission summary takes at the width `decision`
/// renders it — a single long command line wraps into several rows, so the
/// collapse decision must count rows, not logical lines.
fn summary_rows(ui: &egui::Ui, summary: &str) -> usize {
    let mut job = egui::WidgetText::from(egui::RichText::new(summary).monospace()).into_layout_job(
        ui.style(),
        egui::FontSelection::Default,
        egui::Align::Min,
    );
    Arc::make_mut(&mut job).wrap.max_width = ui.available_width();
    ui.fonts_mut(|fonts| fonts.layout_job(Arc::unwrap_or_clone(job))).rows.len()
}

fn clamp_lines(summary: &str) -> String {
    let mut lines = summary.lines();
    let head = lines.by_ref().take(SUMMARY_LINES).collect::<Vec<_>>().join("\n");
    if lines.next().is_some() { format!("{head}…") } else { head }
}

/// Request as rendered: collapsed permission cards show the logically clamped
/// summary so the visual `max_rows` clamp never hides a revealed tail from tests.
fn shown_request(request: &Interaction, collapsed: bool) -> Cow<'_, Interaction> {
    match &request.kind {
        Kind::Permission(tool) if collapsed => Cow::Owned(Interaction {
            id: request.id,
            kind: Kind::Permission(ToolRequest {
                tool: tool.tool.clone(),
                summary: clamp_lines(&tool.summary),
            }),
        }),
        Kind::Permission(_) | Kind::Questions(_) => Cow::Borrowed(request),
    }
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
                    let (glyph, chip_label, tone) = match &request.kind {
                        Kind::Questions(_) => ("?", "Вопрос", Tone::Accent),
                        Kind::Permission(_) => ("!", "Разрешение", Tone::Warning),
                    };
                    let title = row.map_or_else(
                        || Cow::Owned(format!("Задача #{}", live.id)),
                        |r| Cow::Borrowed(r.title.as_str()),
                    );
                    theme::card(ui).show(ui, |ui| {
                        let collapsible = matches!(&request.kind, Kind::Permission(tool)
                            if summary_rows(ui, &tool.summary) > SUMMARY_LINES);
                        let collapsed = collapsible && inbox.details != Some(key);
                        let shown = shown_request(request, collapsed);
                        card_header(
                            ui,
                            Header {
                                project,
                                path: &live.project,
                                row,
                                title: &title,
                                glyph,
                                tone,
                                end: HeaderEnd::Chip(chip_label),
                            },
                        );
                        ui.add_space(6.0);
                        panel.decision(ui, live, &shown, collapsed.then_some(SUMMARY_LINES), send);
                        if collapsible
                            && ui
                                .link(if collapsed {
                                    "Подробнее"
                                } else {
                                    "Свернуть"
                                })
                                .clicked()
                        {
                            inbox.details = collapsed.then_some(key);
                            ui.ctx().request_repaint();
                        }
                    });
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
                let (glyph, tone) = match reason {
                    Attention::Review => ("√", Tone::Accent),
                    Attention::CheckFailed | Attention::AgentFailed | Attention::Interrupted => {
                        ("!", Tone::Error)
                    }
                    Attention::Git | Attention::Queue => ("!", Tone::Warning),
                };
                let summary = if row.summary.is_empty() { reason.label() } else { &row.summary };
                ui.push_id((&row.project, row.id), |ui| {
                    theme::card(ui)
                        .show(ui, |ui| {
                            let clicked = card_header(
                                ui,
                                Header {
                                    project,
                                    path: &row.project,
                                    row: Some(row),
                                    title: &row.title,
                                    glyph,
                                    tone,
                                    end: HeaderEnd::Action(action),
                                },
                            );
                            review_chips(ui, row, reason);
                            ui.add(egui::Label::new(egui::RichText::new(summary).weak()).wrap());
                            clicked
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

struct Header<'a> {
    project: &'a str,
    path: &'a Path,
    row: Option<&'a TaskRow>,
    title: &'a str,
    glyph: &'a str,
    tone: Tone,
    end: HeaderEnd<'a>,
}

enum HeaderEnd<'a> {
    Chip(&'a str),
    Action(&'a str),
}

fn icon_plate(ui: &mut egui::Ui, glyph: &str, tone: Tone) {
    let (bg, fg) = theme::tone_colors(ui, tone);
    egui::Frame::new().fill(bg).corner_radius(8).show(ui, |ui| {
        // A fixed box, not just a minimum: `centered_and_justified` inside a
        // horizontal row otherwise stretches the frame across the whole card.
        ui.allocate_ui_with_layout(
            egui::vec2(26.0, 26.0),
            egui::Layout::centered_and_justified(egui::Direction::TopDown),
            |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(glyph).strong().color(fg))
                        .selectable(false),
                );
            },
        );
    });
}

fn card_header(ui: &mut egui::Ui, header: Header<'_>) -> bool {
    let Header { project, path, row, title, glyph, tone, end } = header;
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
    let details = |ui: &mut egui::Ui| {
        ui.spacing_mut().item_spacing.y = 3.0;
        ui.add(egui::Label::new(egui::RichText::new(title).strong()).wrap());
        ui.add(egui::Label::new(egui::RichText::new(&metadata).small().weak()).truncate())
            .on_hover_text(path.display().to_string());
    };
    let grow = |ui: &mut egui::Ui| {
        ui.set_width(ui.available_width());
        details(ui);
    };
    match end {
        HeaderEnd::Chip(label) => {
            ui.horizontal(|ui| {
                icon_plate(ui, glyph, tone);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    theme::chip(ui, tone, label);
                    ui.vertical(grow);
                });
            });
            false
        }
        HeaderEnd::Action(action) => {
            let narrow = ui.available_width() < 440.0;
            if narrow {
                ui.horizontal(|ui| {
                    icon_plate(ui, glyph, tone);
                    ui.vertical(grow);
                });
                ui.button(action).clicked()
            } else {
                ui.horizontal(|ui| {
                    icon_plate(ui, glyph, tone);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let clicked = ui.button(action).clicked();
                        ui.vertical(grow);
                        clicked
                    })
                    .inner
                })
                .inner
            }
        }
    }
}

impl TasksView {
    pub(in crate::gui) fn show_attention(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) -> bool {
        self.local_panel.sync(ui, snapshot);
        if self.inbox.details.is_some_and(|key| !has_request(snapshot, key)) {
            self.inbox.details = None;
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
            let is_review = |item: &Item| matches!(item, Item::Task(_, Attention::Review));
            let review_count = visible.iter().filter(|item| is_review(item)).count();
            for ((results, heading), count) in
                [(false, "Ждут вашего ответа"), (true, "Результаты на проверку")]
                    .into_iter()
                    .zip([visible.len() - review_count, review_count])
            {
                if count == 0 {
                    continue;
                }
                ui.label(egui::RichText::new(format!("{heading} · {count}")).strong().weak());
                ui.separator();
                for item in visible.iter().copied().filter(|item| is_review(item) == results) {
                    ui.add_space(6.0);
                    let project = project_label(item.project(), &projects);
                    if let Some(row) =
                        item.show(ui, &project, &mut self.inbox, &mut self.local_panel, send)
                    {
                        target = Some(row);
                    }
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
