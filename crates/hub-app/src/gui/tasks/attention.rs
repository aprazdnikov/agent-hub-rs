use super::{Command, Snapshot, TasksView, result::Tab};
use crate::desktop_tasks::Attention;
use crate::local_execution::{Kind, Live};
use eframe::egui;
use hub_core::tasks::Task;
use std::{collections::BTreeSet, path::Path};

fn jobs(snapshot: &Snapshot) -> impl Iterator<Item = &Live> {
    snapshot.local_jobs.iter().chain(
        snapshot
            .local
            .iter()
            .filter(|live| !snapshot.local_jobs.iter().any(|row| row.token == live.token)),
    )
}
fn pending(snapshot: &Snapshot) -> impl Iterator<Item = &Live> {
    jobs(snapshot).filter(|j| j.active && j.interaction.is_some())
}
pub(in crate::gui) fn attention_count(snapshot: &Snapshot) -> usize {
    pending(snapshot).count()
        + snapshot.task_index.as_ref().map_or(0, |rows| {
            rows.iter()
                .filter(|row| row.reason.is_some() && !pending(snapshot).any(|j| j.id == row.id))
                .count()
        })
}
fn attention_row(ui: &mut egui::Ui, project: &Path, id: u64, title: &str, action: &str) -> bool {
    let width = ui.available_width().min(820.0);
    egui::Frame::group(ui.style())
        .fill(ui.visuals().panel_fill)
        .inner_margin(12)
        .show(ui, |ui| {
            ui.set_width((width - 26.0).max(0.0));
            let details = |ui: &mut egui::Ui| {
                let name = project.file_name().and_then(|v| v.to_str()).unwrap_or("Проект");
                ui.small(format!("{name}   ·   #{id}"))
                    .on_hover_text(project.display().to_string());
                ui.add(egui::Label::new(egui::RichText::new(title).strong()).truncate())
                    .on_hover_text(title);
            };
            if width < 460.0 {
                details(ui);
                ui.button(action).clicked()
            } else {
                ui.horizontal(|ui| {
                    let title_width = ui.available_width() - 190.0;
                    ui.vertical(|ui| {
                        ui.set_width(title_width);
                        details(ui);
                    });
                    ui.button(action).clicked()
                })
                .inner
            }
        })
        .inner
}
impl TasksView {
    fn navigate(&mut self, project: &Path, id: Option<u64>, tab: Tab) {
        self.local_mode = Some(true);
        self.local_project.choose(project);
        self.focused = id;
        self.query.clear();
        self.filter = hub_core::tasks::TaskFilter::Active;
        self.result_view.select(tab, id);
    }
    pub(in crate::gui) fn show_attention(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
    ) -> bool {
        ui.heading("Требуют внимания");
        if let Err(error) = &snapshot.task_index {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let mut opened = false;
        egui::ScrollArea::vertical().id_salt("attention_items").show(ui, |ui| {
            for live in pending(snapshot) {
                let Some(interaction) = &live.interaction else { continue };
                let reason = match &interaction.kind {
                    Kind::Permission(tool) => format!("Разрешить {}", tool.tool),
                    Kind::Questions(_) => "Ответить агенту".to_owned(),
                };
                let title = snapshot
                    .task_index
                    .iter()
                    .flatten()
                    .find(|row| row.id == live.id)
                    .map_or(live.status.as_str(), |row| row.title.as_str());
                if attention_row(ui, &live.project, live.id, title, &reason) {
                    self.navigate(&live.project, Some(live.id), Tab::Changes);
                    self.result_view.keep_position();
                    self.reveal_interaction = true;
                    opened = true;
                }
            }
            for row in snapshot.task_index.iter().flatten() {
                let Some(reason) = row.reason else { continue };
                if pending(snapshot).any(|j| j.id == row.id) {
                    continue;
                }
                if attention_row(ui, &row.project, row.id, &row.title, reason.label()) {
                    let tab = match reason {
                        Attention::CheckFailed
                        | Attention::AgentFailed
                        | Attention::Interrupted => Tab::Checks,
                        Attention::Review => Tab::Summary,
                        Attention::Queue | Attention::Git => Tab::Changes,
                    };
                    self.navigate(&row.project, Some(row.id), tab);
                    opened = true;
                }
            }
            if snapshot.task_index.is_ok() && attention_count(snapshot) == 0 {
                ui.label("Вопросов, запросов разрешения и результатов для проверки пока нет.");
                if ui.button("Открыть задачи").clicked() {
                    opened = true;
                }
            }
        });
        opened
    }
    pub(super) fn project_navigation(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot) {
        let projects: BTreeSet<_> =
            snapshot.task_index.iter().flatten().map(|row| &row.project).collect();
        if projects.is_empty() {
            return;
        }
        let mut draw = |ui: &mut egui::Ui| {
            ui.strong("Проекты");
            for path in &projects {
                let label = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or_else(|| path.to_str().unwrap_or("Проект"));
                if ui
                    .selectable_label(
                        self.scope
                            .as_ref()
                            .is_some_and(|scope| scope.1.as_path() == path.as_path()),
                        label,
                    )
                    .on_hover_text(path.display().to_string())
                    .clicked()
                {
                    self.navigate(path, None, Tab::Changes);
                }
            }
        };
        if ui.available_width() >= 900.0 {
            egui::Panel::left("task_projects").default_size(150.0).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("projects_list").show(ui, &mut draw);
            });
        } else {
            ui.collapsing("Список проектов", &mut draw);
        }
    }
    pub(super) fn task_navigation(
        &mut self,
        ui: &mut egui::Ui,
        tasks: &[&Task],
        snapshot: &Snapshot,
    ) {
        if tasks.len() <= 1 {
            return;
        }
        let mut draw = |ui: &mut egui::Ui| {
            for task in tasks {
                let live = jobs(snapshot).find(|live| live.active && live.id == task.id);
                let reason = live
                    .and_then(|j| j.interaction.as_ref())
                    .map(|request| match &request.kind {
                        Kind::Permission(_) => "Ждёт разрешения",
                        Kind::Questions(_) => "Ждёт ответа",
                    })
                    .or_else(|| {
                        snapshot
                            .task_index
                            .as_ref()
                            .ok()?
                            .iter()
                            .find(|row| row.id == task.id)?
                            .reason
                            .map(Attention::label)
                    });
                let state = reason.unwrap_or_else(|| {
                    if live.is_some() || task.active() {
                        "Выполняется"
                    } else if task.queue.is_some() {
                        "В очереди"
                    } else {
                        task.status.label()
                    }
                });
                if ui
                    .selectable_label(
                        self.focused == Some(task.id),
                        format!("#{}  {}\n{} · {}", task.id, task.title, task.backend, state),
                    )
                    .clicked()
                {
                    self.focused = Some(task.id);
                    self.result_view.select(Tab::Summary, Some(task.id));
                }
            }
        };
        if ui.available_width() >= 720.0 {
            egui::Panel::left("tasks_list").default_size(180.0).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("task_rows").show(ui, &mut draw);
            });
        } else {
            egui::ComboBox::from_id_salt("which_task")
                .selected_text("Выбрать задачу")
                .show_ui(ui, draw);
        }
    }
    pub(super) fn local_panels(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        let selected = jobs(snapshot).find(|live| self.focused == Some(live.id));
        if let Some(live) =
            selected.filter(|live| snapshot.local.as_ref().is_none_or(|l| l.token != live.token))
        {
            let mut focused = snapshot.clone();
            focused.local = Some(live.clone());
            self.local_panel.show(ui, &focused, send);
        } else {
            self.local_panel.show(ui, snapshot, send);
        }
    }
}
