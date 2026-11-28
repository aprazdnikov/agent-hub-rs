use super::{Effect, Page, Rules, Speaker, Stage, Task, Tasks};
use crate::widgets::{action, card, primary};
use eframe::egui;
use kylig_ui::{Button, TextField};
use std::time::Duration;

#[derive(Clone, Copy)]
pub(super) enum QueueMode {
    Shared,
    Worktree,
}

pub(super) struct Queue {
    mode: QueueMode,
    ready_at: f64,
}

pub(super) struct Controls {
    pub(super) agent: &'static str,
    pub(super) archived: bool,
    pub(super) queue: Option<Queue>,
    branch: String,
    pub(super) prepared: Option<String>,
    worktree: bool,
}

impl Controls {
    pub(super) fn new(id: usize, agent: &'static str) -> Self {
        Self {
            agent,
            archived: false,
            queue: None,
            branch: format!("task/{id}"),
            prepared: None,
            worktree: false,
        }
    }
}

pub(super) enum Change {
    Assign(&'static str),
    Prepare(String),
    Enqueue(Queue),
    Unqueue,
    Archive(bool),
    Delete,
    Reopen,
}

impl Tasks {
    pub(crate) fn begin_new(&mut self) {
        self.page = Page::New;
        self.compact_projects = false;
        self.from_inbox = false;
        self.notice = None;
    }

    pub(super) fn switch_archive(&mut self, archive: bool) {
        let browsing = self.compact_projects;
        self.archive = archive;
        self.select_project(self.project);
        self.compact_projects = browsing;
    }

    pub(super) fn poll_queue(&mut self, ui: &egui::Ui) {
        let now = ui.input(|i| i.time);
        let next = self
            .entries
            .iter()
            .filter_map(|t| t.ops.queue.as_ref().map(|q| (t.id, q.ready_at)))
            .min_by(|(_, a), (_, b)| a.total_cmp(b));
        let Some((id, ready_at)) = next else { return };
        if ready_at > now {
            ui.ctx().request_repaint_after(Duration::from_secs_f64(ready_at - now));
            return;
        }
        if let Some(task) = self.entry_mut(id)
            && let Some(queue) = task.ops.queue.take()
        {
            task.ops.worktree |= matches!(queue.mode, QueueMode::Worktree);
            if !task.composer.is_empty() {
                let step = std::mem::take(&mut task.composer);
                task.message(Speaker::User, &step);
            }
        }
        self.start(ui, &format!("Выполняем задачу #{id} из очереди…"), Effect::Run(id), false);
    }

    pub(super) fn manage(&mut self, ui: &mut egui::Ui, id: usize, rules: &Rules) {
        let deleting = self.deleting == Some(id);
        let dirty = self.rules.get(&self.project).is_some_and(|r| r.saved != r.draft);
        let Some(task) = self.entry_mut(id) else { return };
        let locked = task.ops.archived || task.stage.waiting() || task.stage == Stage::Accepted;
        let queued = task.ops.queue.is_some();
        let mut change = git_controls(ui, task, rules, dirty);
        let mut confirm = deleting;
        card(ui, |ui| {
            ui.strong("Управление задачей");
            if let Some(queue) = &task.ops.queue {
                ui.label(match queue.mode {
                    QueueMode::Shared => "В очереди. Ожидает свободного исполнителя.",
                    QueueMode::Worktree => {
                        "В очереди. Для запуска будет создана отдельная рабочая копия."
                    }
                });
                if action(ui, "Снять с очереди") {
                    change = Some(Change::Unqueue);
                }
            } else if !locked {
                let first = task.attempts.is_empty();
                let has_step = first || !task.composer.trim().is_empty();
                let valid =
                    !dirty && has_step && (!rules.git || !first || task.ops.prepared.is_some());
                if !has_step {
                    ui.weak("Для продолжения в очереди введите следующий шаг в чате.");
                }
                ui.horizontal_wrapped(|ui| {
                    for (mode, label) in [
                        (QueueMode::Shared, "В очередь"),
                        (QueueMode::Worktree, "В очередь в отдельном worktree"),
                    ] {
                        if matches!(mode, QueueMode::Worktree) && (!first || rules.git) {
                            continue;
                        }
                        if ui.add_enabled(valid, Button::new(label)).clicked() {
                            change = Some(Change::Enqueue(Queue {
                                mode,
                                ready_at: ui.input(|i| i.time) + 10.0,
                            }));
                        }
                    }
                });
            }
            if !queued && !task.stage.waiting() {
                if task.stage == Stage::Accepted
                    && !task.ops.archived
                    && action(ui, "Вернуть в работу")
                {
                    change = Some(Change::Reopen);
                }
                ui.horizontal_wrapped(|ui| {
                    if action(
                        ui,
                        if task.ops.archived {
                            "Восстановить из архива"
                        } else {
                            "В архив"
                        },
                    ) {
                        change = Some(Change::Archive(!task.ops.archived));
                    }
                    if !deleting && action(ui, "Удалить задачу…") {
                        confirm = true;
                    }
                });
                if deleting {
                    ui.label(format!("Удалить задачу #{id} вместе с чатом и историей попыток?"));
                    ui.horizontal_wrapped(|ui| {
                        if primary(ui, "Удалить задачу") {
                            change = Some(Change::Delete);
                        }
                        if action(ui, "Отмена") {
                            confirm = false;
                        }
                    });
                }
            }
        });
        self.deleting = confirm.then_some(id);
        if let Some(change) = change {
            let label = match change {
                Change::Prepare(_) => "Подготавливаем рабочую ветку…",
                Change::Enqueue(_) => "Ставим задачу в очередь…",
                Change::Unqueue => "Снимаем задачу с очереди…",
                Change::Delete => "Удаляем задачу…",
                Change::Archive(_) | Change::Reopen | Change::Assign(_) => {
                    "Сохраняем изменения задачи…"
                }
            };
            self.start(ui, label, Effect::Manage(id, change), false);
        }
    }

    pub(super) fn apply_change(&mut self, id: usize, change: Change) -> String {
        let Some(task) = self.entry_mut(id) else {
            return format!("Задача #{id} удалена.");
        };
        match change {
            Change::Assign(agent) => {
                task.agent = agent;
                format!("Задача #{id} передана {agent}.")
            }
            Change::Prepare(branch) => {
                let notice = format!("Ветка {branch} готова к запуску.");
                task.ops.prepared = Some(branch);
                notice
            }
            Change::Enqueue(queue) => {
                task.ops.queue = Some(queue);
                format!("Задача #{id} в очереди.")
            }
            Change::Unqueue => {
                task.ops.queue = None;
                format!("Задача #{id} снята с очереди.")
            }
            Change::Reopen => {
                task.stage = Stage::Planned;
                format!("Задача #{id} возвращена в работу.")
            }
            Change::Archive(archive) => {
                task.ops.archived = archive;
                self.select_project(self.project);
                if archive {
                    format!("Задача #{id} перенесена в архив.")
                } else {
                    format!("Задача #{id} восстановлена.")
                }
            }
            Change::Delete => {
                self.entries.retain(|task| task.id != id);
                self.deleting = None;
                self.select_project(self.project);
                format!("Задача #{id} удалена.")
            }
        }
    }
}

fn git_controls(ui: &mut egui::Ui, task: &mut Task, rules: &Rules, dirty: bool) -> Option<Change> {
    let id = task.id;
    let mut change = None;
    if rules.git || task.ops.prepared.is_some() || task.ops.worktree {
        card(ui, |ui| {
            ui.strong("Подготовка Git");
            if task.ops.worktree {
                ui.monospace(format!("hub/task-{id}   /   .agent-hub-worktrees/{id}"));
            }
            if let Some(branch) = &task.ops.prepared {
                ui.label(format!("Подготовлена ветка {branch}"));
            }
            if rules.git
                && task.stage == Stage::Draft
                && task.ops.queue.is_none()
                && !task.ops.archived
            {
                ui.weak(format!("База {}/{}", rules.remote, rules.base));
                ui.add(
                    TextField::new(&mut task.ops.branch)
                        .label("Новая рабочая ветка")
                        .width(ui.available_width()),
                );
                let branch = task.ops.branch.trim();
                let valid = !branch.is_empty()
                    && !branch.contains(char::is_whitespace)
                    && !branch.contains("..")
                    && !branch.starts_with('-');
                if !valid {
                    ui.weak("Введите имя ветки Git без пробелов и двух точек подряд.");
                }
                if ui
                    .add_enabled(
                        valid && !dirty && task.ops.prepared.as_deref() != Some(branch),
                        Button::new("Подготовить ветку Git"),
                    )
                    .clicked()
                {
                    change = Some(Change::Prepare(branch.to_owned()));
                }
            }
        });
    }

    change
}
