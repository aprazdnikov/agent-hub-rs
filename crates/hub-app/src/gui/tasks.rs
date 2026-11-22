//! Desktop and Telegram task management share one service and persistence owner.
use crate::supervisor::{BotStatus, Command, Snapshot};
use eframe::egui;
use hub_core::{
    domain::{BackendKind, TopicKey},
    tasks::{Task, TaskFilter, TaskStatus},
};
use hub_telegram::hub::tasks::{Request, Snapshot as TaskSnapshot};
mod attention;
pub(super) use attention::attention_count;
mod context;
mod git;
mod local;
mod result;
mod rules;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

type Scope = (Option<TopicKey>, PathBuf, BackendKind);
type Reply = oneshot::Receiver<Result<TaskSnapshot, String>>;
#[derive(Default)]
pub struct TasksView {
    result_view: result::View,
    focused: Option<u64>,
    reveal_interaction: bool,
    local_mode: Option<bool>,
    local_panel: local::Panel,
    local_block: LocalBlock,
    local_project: context::LocalProject,
    rules: rules::Editor,
    saving_rules: bool,
    git_busy: bool,
    git_branches: std::collections::BTreeMap<u64, String>,
    selected: Option<TopicKey>,
    scope: Option<Scope>,
    tasks: Vec<Task>,
    pending: Option<(Scope, bool, bool, Reply)>,
    refreshed: Option<Instant>,
    title: String,
    prompt: String,
    next: std::collections::BTreeMap<u64, String>,
    assignees: std::collections::BTreeMap<u64, (u64, BackendKind)>,
    error: Option<String>,
    query: String,
    filter: TaskFilter,
}
#[derive(Default, PartialEq, Eq)]
enum LocalBlock {
    #[default]
    Free,
    Job,
}
struct Availability {
    edit: bool,
    launch: bool,
}
// Assignment changes metadata, not the continuation the user is composing.
// Normalize legacy ownership on copies so future Run fields participate in equality too.
fn only_assignment_changed(old: &Task, new: &Task) -> bool {
    if new.revision <= old.revision || old.backend == new.backend {
        return false;
    }
    let mut before = old.clone();
    let mut after = new.clone();
    for run in &mut before.runs {
        run.backend.get_or_insert_with(|| old.backend.clone());
    }
    for run in &mut after.runs {
        run.backend.get_or_insert_with(|| new.backend.clone());
    }
    before.revision = after.revision;
    before.backend.clone_from(&after.backend);
    before == after
}
fn only_git_diagnostic_changed(old: &Task, new: &Task) -> bool {
    if new.revision <= old.revision {
        return false;
    }
    let mut before = old.clone();
    let (Some(previous), Some(current)) = (&mut before.git, &new.git) else { return false };
    previous.message.clone_from(&current.message);
    before.revision = new.revision;
    before == *new
}
impl TasksView {
    fn mutation_pending(&self) -> bool {
        self.pending.as_ref().is_some_and(|(_, _, listing, _)| !listing)
    }
    fn availability(&self, task: &Task, backend: BackendKind, topic_running: bool) -> Availability {
        let edit = !self.mutation_pending() && !self.git_busy && !task.active();
        Availability {
            edit,
            launch: edit
                && !topic_running
                && !task.archived
                && !matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)
                && (self.local_context() || task.backend == backend.name())
                && !(self.local_context() && self.local_block != LocalBlock::Free),
        }
    }
    fn request(&mut self, request: Request, send: &mut impl FnMut(Command)) {
        let Some((key, project, backend)) = self.scope.clone() else { return };
        let creating = matches!(request, Request::Create { .. });
        let listing = matches!(request, Request::List | Request::Search { .. });
        if self.mutation_pending() || (listing && self.pending.is_some()) {
            return;
        }
        // A user action supersedes an in-flight read. Dropping its receiver below
        // prevents that older snapshot from replacing the acknowledged mutation.
        // Mutations still serialize; repeated refresh requests are coalesced.
        let (reply, result) = oneshot::channel();
        self.saving_rules =
            matches!(request, Request::SaveRules { .. } | Request::SaveProjectRules { .. });
        if let Some(key) = key {
            send(Command::Tasks { key, project: project.clone(), backend, request, reply });
        } else {
            send(Command::LocalTasks { project: project.clone(), backend, request, reply });
        }
        self.pending = Some(((key, project, backend), creating, listing, result));
        self.error = None;
    }
    fn poll(&mut self) {
        let Some((scope, creating, listing, mut receiver)) = self.pending.take() else { return };
        match receiver.try_recv() {
            Ok(result) if self.scope.as_ref() == Some(&scope) => {
                self.refreshed = Some(Instant::now());
                let saving_rules = self.saving_rules;
                self.saving_rules = false;
                match result {
                    Ok(snapshot) => {
                        self.git_busy = snapshot.git_busy;
                        if saving_rules {
                            self.rules.accept(snapshot.rules);
                        } else {
                            self.rules.observe(snapshot.rules);
                        }
                        let mut tasks = snapshot.tasks;
                        if !listing {
                            let mut merged = self.tasks.clone();
                            for task in tasks {
                                if let Some(old) = merged.iter_mut().find(|old| old.id == task.id) {
                                    *old = task;
                                } else {
                                    merged.push(task);
                                }
                            }
                            tasks = merged;
                        }
                        self.next.retain(|id, _| {
                            self.tasks.iter().any(|old| {
                                old.id == *id
                                    && tasks.iter().any(|new| {
                                        new.id == *id
                                            && (new.revision == old.revision
                                                || only_assignment_changed(old, new)
                                                || only_git_diagnostic_changed(old, new))
                                    })
                            })
                        });
                        if creating {
                            self.title.clear();
                            self.prompt.clear();
                        }
                        self.assignees.retain(|id, (revision, _)| {
                            tasks.iter().any(|task| task.id == *id && task.revision == *revision)
                        });
                        self.tasks = tasks;
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Ok(_) => {}
            Err(oneshot::error::TryRecvError::Empty) => {
                self.pending = Some((scope, creating, listing, receiver));
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                self.saving_rules = false;
                self.error =
                    Some("Запрос не принят или сервис остановлен. Обновите список.".into());
                self.refreshed = Some(Instant::now());
            }
        }
    }
    fn clear_project(&mut self) {
        self.git_branches.clear();
        self.git_busy = false;
        self.rules = rules::Editor::default();
        self.tasks.clear();
        self.next.clear();
        self.assignees.clear();
    }
    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        egui::ScrollArea::vertical().id_salt("task_workspace").show(ui, |ui| {
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(12, 0))
                .show(ui, |ui| self.show_project(ui, snapshot, send));
        });
    }
    fn show_project(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        ui.horizontal_wrapped(|ui| {
            ui.heading("Задачи");
            self.choose_mode(ui, snapshot);
        });
        if self.reveal_interaction {
            ui.scroll_to_cursor(Some(egui::Align::Min));
            self.reveal_interaction = false;
        }
        self.local_panels(ui, snapshot, send);
        self.local_block = if snapshot.local.as_ref().is_some_and(|live| live.active) {
            LocalBlock::Job
        } else {
            LocalBlock::Free
        };
        let scope = self.choose_context(ui, snapshot);
        if scope != self.scope {
            let same_project =
                self.scope.as_ref().zip(scope.as_ref()).is_some_and(|(old, new)| old.1 == new.1);
            if !same_project {
                self.clear_project();
            }
            self.saving_rules = false;
            self.scope = scope;
            self.title.clear();
            self.prompt.clear();
            self.error = None;
            self.pending = None;
            self.refreshed = None;
            self.query.clear();
            self.filter = TaskFilter::Active;
        }
        let Some((key, project, backend)) = self.scope.clone() else {
            ui.label("Выберите и откройте каталог проекта либо тему Telegram.");
            return;
        };
        if key.is_some() {
            ui.weak(format!("{} · {}", backend.name(), project.display()));
        }
        self.poll();
        if self.pending.is_none()
            && self.refreshed.is_none_or(|time| time.elapsed() > Duration::from_secs(3))
        {
            self.request(Request::Search { filter: TaskFilter::All, query: String::new() }, send);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(250));
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let busy = self.mutation_pending();
        if let Some(request) =
            self.rules.show(ui, &project, self.saving_rules, busy || self.git_busy)
        {
            self.request(request, send);
        }
        self.toolbar(ui, busy, send);
        let topic_running = snapshot.topics.iter().any(|topic| {
            Some(topic.key) == key && topic.state == hub_telegram::hub::TopicState::Running
        });
        let snapshot_tasks = std::mem::take(&mut self.tasks);
        let tasks: Vec<_> =
            snapshot_tasks.iter().filter(|task| self.filter.matches(task, &self.query)).collect();
        if tasks.is_empty() {
            ui.label("Задачи по выбранным условиям не найдены.");
        }
        self.task_navigation(ui, &tasks, snapshot);
        if let Some(task) =
            tasks.iter().find(|task| Some(task.id) == self.focused).or(tasks.first())
        {
            self.focused = Some(task.id);
            self.card(ui, task, backend, topic_running, send);
        }
        self.tasks = snapshot_tasks;
    }
    fn toolbar(&mut self, ui: &mut egui::Ui, busy: bool, send: &mut impl FnMut(Command)) {
        // A background list refresh must not disable text editors: egui surrenders
        // their focus when disabled. Freeze the draft only while it is being saved.
        let creating = self.pending.as_ref().is_some_and(|(_, creating, _, _)| *creating);
        ui.collapsing("Создать задачу", |ui| {
            ui.add_enabled_ui(!creating, |ui| {
                ui.label("Название");
                ui.add(
                    egui::TextEdit::singleline(&mut self.title)
                        .id_salt(("task_create_title", self.scope.as_ref().map(|scope| &scope.1)))
                        .char_limit(256),
                );
                ui.label("Поручение и ограничения");
                ui.add(
                    egui::TextEdit::multiline(&mut self.prompt)
                        .id_salt(("task_create_prompt", self.scope.as_ref().map(|scope| &scope.1)))
                        .desired_rows(4)
                        .char_limit(16384),
                );
                if ui
                    .add_enabled(
                        !busy && !self.title.trim().is_empty() && !self.prompt.trim().is_empty(),
                        egui::Button::new("Сохранить задачу без запуска"),
                    )
                    .clicked()
                {
                    self.request(
                        Request::Create { title: self.title.clone(), prompt: self.prompt.clone() },
                        send,
                    );
                }
            });
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Поиск по задачам")
                    .desired_width(190.0)
                    .char_limit(256),
            )
            .on_hover_text("Поиск по названию и поручению");
            egui::ComboBox::from_id_salt("task_filter").selected_text(self.filter.label()).show_ui(
                ui,
                |ui| {
                    for filter in [TaskFilter::Active, TaskFilter::All, TaskFilter::Archived]
                        .into_iter()
                        .chain(TaskStatus::ALL.map(TaskFilter::Status))
                    {
                        ui.selectable_value(&mut self.filter, filter, filter.label());
                    }
                },
            );
            if ui.add_enabled(!busy, egui::Button::new("Обновить").frame(false)).clicked() {
                self.request(
                    Request::Search { filter: TaskFilter::All, query: String::new() },
                    send,
                );
            }
        });
        ui.separator();
    }
    fn workflow_controls(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        busy: bool,
        send: &mut impl FnMut(Command),
    ) {
        let mut status = task.status;
        ui.add_enabled_ui(!busy && !task.active(), |ui| {
            ui.add_enabled_ui(!task.archived, |ui| {
                egui::ComboBox::from_id_salt("task_status")
                    .selected_text(task.status.label())
                    .show_ui(ui, |ui| {
                        for candidate in TaskStatus::ALL {
                            if matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)
                                && candidate != task.status
                                && candidate != TaskStatus::Planned
                            {
                                continue;
                            }
                            ui.selectable_value(&mut status, candidate, candidate.label());
                        }
                    });
                if status != task.status {
                    self.request(
                        Request::Status { id: task.id, revision: task.revision, status },
                        send,
                    );
                }
            });
            if ui
                .button(if task.archived {
                    "Восстановить из архива"
                } else {
                    "В архив"
                })
                .clicked()
            {
                self.request(
                    Request::Archive {
                        id: task.id,
                        revision: task.revision,
                        archived: !task.archived,
                    },
                    send,
                );
            }
        });
    }
    fn executor_controls(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        backend: BackendKind,
        editable: bool,
        send: &mut impl FnMut(Command),
    ) {
        ui.strong("1. Исполнитель");
        ui.label(format!(
            "Назначен: {}. {}: {}.",
            task.backend,
            if self.local_context() {
                "Агент для новых задач"
            } else {
                "Агент выбранной темы"
            },
            backend.name()
        ));
        let Some(assigned) = BackendKind::parse(&task.backend) else {
            ui.label("Неизвестный агент задачи. Обновите список.");
            return;
        };
        let mut selected = self
            .assignees
            .get(&task.id)
            .filter(|(revision, _)| *revision == task.revision)
            .map_or(assigned, |(_, agent)| *agent);
        let editable = editable
            && !task.archived
            && !matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred);
        ui.horizontal(|ui| {
            ui.label("Агент");
            ui.add_enabled_ui(editable, |ui| {
                egui::ComboBox::from_id_salt("task_assignee")
                    .selected_text(selected.name())
                    .show_ui(ui, |ui| {
                        for agent in BackendKind::ALL {
                            ui.selectable_value(&mut selected, agent, agent.name());
                        }
                    });
            });
            if ui
                .add_enabled(editable && selected != assigned, egui::Button::new("Назначить"))
                .clicked()
            {
                self.request(
                    Request::Assign { id: task.id, revision: task.revision, backend: selected },
                    send,
                );
            }
        });
        if selected == assigned {
            self.assignees.remove(&task.id);
        } else {
            self.assignees.insert(task.id, (task.revision, selected));
        }
        ui.small(if task.active() {
            "Сменить агента можно после завершения или остановки текущего запуска."
        } else if task.archived {
            "Сначала восстановите задачу в разделе «Статус и архив»."
        } else if matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred) {
            "Сначала выберите «Запланирована» в разделе «Статус и архив»."
        } else if self.mutation_pending() {
            "Дождитесь сохранения текущего изменения."
        } else if selected != assigned {
            "Нажмите «Назначить», чтобы сохранить выбор. Это не запускает работу."
        } else {
            "Чтобы сменить исполнителя, выберите другого агента и нажмите «Назначить»."
        });
        ui.add_space(8.0);
    }
    fn show_queue_controls(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        next: &str,
        send: &mut impl FnMut(Command),
    ) -> bool {
        let first = task.runs.is_empty();
        if let Some(q) = &task.queue {
            ui.label(q.error.clone().unwrap_or_else(|| format!("В очереди · №{}", q.sequence)));
            if ui.button("Снять с очереди").clicked() {
                self.request(Request::Unqueue { id: task.id, revision: task.revision }, send);
            }
            return true;
        }
        if !task.active()
            && !self.rules.dirty()
            && !self.mutation_pending()
            && !task.archived
            && matches!(task.status, TaskStatus::Planned | TaskStatus::NeedsReview)
            && (first || !next.trim().is_empty())
        {
            ui.horizontal_wrapped(|ui| {
                for (isolated, label) in
                    [(false, "В очередь"), (true, "В очередь в отдельном worktree")]
                {
                    if (!first && isolated != task.isolated)
                        || (isolated && self.rules.saved.git.is_some())
                    {
                        continue;
                    }
                    if ui.button(label).clicked() {
                        self.request(
                            Request::Queue {
                                id: task.id,
                                revision: task.revision,
                                isolated,
                                next: (!first).then(|| next.to_owned()),
                                origin: None,
                            },
                            send,
                        );
                    }
                }
            });
        }
        false
    }
    fn launch_block(
        &self,
        task: &Task,
        backend: BackendKind,
        available: &Availability,
        next: &str,
    ) -> Option<&'static str> {
        let first = task.runs.is_empty();
        if self.local_context() && self.local_block == LocalBlock::Job {
            Some("Локальная работа выполняется. Дождитесь завершения или остановите её выше.")
        } else if self.mutation_pending() {
            Some("Сохраняем изменения. Дождитесь подтверждения.")
        } else if self.git_busy {
            Some("Подготовка или проверка Git выполняется. Дождитесь результата.")
        } else if self.rules.saved.git.as_ref().is_some_and(|policy| {
            !task.git.as_ref().is_some_and(|git| {
                git.state == hub_core::tasks::git::PreparationState::Ready && git.policy == *policy
            })
        }) {
            Some("Сначала подготовьте Git по сохранённым правилам проекта.")
        } else if self.rules.dirty() {
            Some("Сохраните правила проекта или загрузите сохранённую редакцию перед запуском.")
        } else if task.archived {
            Some("Восстановите задачу в разделе «Статус и архив» ниже.")
        } else if matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred) {
            Some("Верните задачу в работу в разделе «Статус и архив» ниже.")
        } else if self.assignees.get(&task.id).is_some_and(|(revision, agent)| {
            *revision == task.revision && agent.name() != task.backend
        }) {
            Some("Нажмите «Назначить» в шаге 1, чтобы сохранить выбранного агента.")
        } else if !self.local_context() && task.backend != backend.name() {
            Some("Выберите сверху тему этого проекта с назначенным агентом.")
        } else if !first && next.trim().is_empty() {
            Some("Заполните следующий шаг в шаге 2.")
        } else if !available.launch {
            Some("Тема занята. Дождитесь завершения текущей работы или остановите её.")
        } else {
            None
        }
    }
    fn launch_controls(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        backend: BackendKind,
        available: &Availability,
        next: &mut String,
        send: &mut impl FnMut(Command),
    ) {
        let first = task.runs.is_empty();
        ui.strong(if first { "2. Поручение" } else { "2. Следующий шаг" });
        if first {
            ui.add(egui::Label::new(&task.prompt).selectable(true));
        } else {
            ui.collapsing("Исходное поручение", |ui| {
                ui.add(egui::Label::new(&task.prompt).selectable(true));
            });
            ui.label("Что агент должен сделать дальше? Прежний результат добавится автоматически.");
            ui.add_enabled(
                available.edit,
                egui::TextEdit::multiline(next)
                    .desired_rows(3)
                    .char_limit(16384)
                    .hint_text("Например: проверь предыдущий результат и исправь найденные ошибки"),
            );
        }
        ui.add_space(8.0);
        ui.strong("3. Проверка и запуск");
        let workflow = self
            .rules
            .saved
            .workflow
            .as_ref()
            .filter(|plan| plan.implementer.name() == task.backend);
        if let Some(plan) = workflow {
            ui.label(format!(
                "{} → {} → ревью {}",
                plan.implementer.name(),
                plan.command,
                plan.reviewer.name()
            ));
        }
        let automated = workflow.is_some();
        ui.small("Проверьте контекст, затем запустите работу отдельной кнопкой.");
        ui.collapsing("Контекст следующего запуска", |ui| {
            ui.label(format!("Поручение хаба · r{} · {}", task.revision, task.backend));
            ui.small("Каждый запуск создаёт свежую CLI-сессию. Передаются сохранённые правила и контекст задачи; разрешения не переносятся.");
            match task.preview_with_rules(if first { None } else { Some(next.as_str()) }, &self.rules.saved) {
                Ok(text) => { ui.add(egui::Label::new(text).selectable(true)); }
                Err(error) => { ui.label(error); }
            }
        });
        if self.show_queue_controls(ui, task, next, send) {
            return;
        }
        let blocked = self.launch_block(task, backend, available, next);
        if let Some(reason) = blocked {
            ui.label(reason);
        }
        if ui
            .add_enabled(
                blocked.is_none(),
                egui::Button::new(if automated {
                    "Исправить и проверить"
                } else if self.local_context() {
                    if first {
                        "Запустить локально"
                    } else {
                        "Продолжить локально"
                    }
                } else if first {
                    "Запустить задачу в выбранной теме"
                } else {
                    "Продолжить новым запуском"
                }),
            )
            .clicked()
        {
            self.request(
                Request::StartChecked {
                    rules_revision: self.rules.saved.revision,
                    id: task.id,
                    revision: task.revision,
                    next: (!first).then(|| next.clone()),
                },
                send,
            );
        }
    }
    fn card(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        backend: BackendKind,
        topic_running: bool,
        send: &mut impl FnMut(Command),
    ) {
        let available = self.availability(task, backend, topic_running);
        let mut next = self.next.get(&task.id).cloned().unwrap_or_default();
        ui.push_id(task.id, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.weak(format!("#{} · r{}", task.id, task.revision));
                ui.label(format!("Агент · {}", task.backend));
                ui.colored_label(ui.visuals().hyperlink_color, task.status.label());
                if task.archived {
                    ui.weak("В архиве");
                }
            });
            ui.heading(&task.title);
            ui.add_space(8.0);
            let annotation = self.result_view.show(ui, task, &next, available.edit);
            let reveal_next = annotation.is_some();
            if let Some(step) = annotation {
                next = step;
            }

            if task.status == TaskStatus::NeedsReview
                && !task.archived
                && ui
                    .add_enabled(
                        available.edit,
                        egui::Button::new(
                            egui::RichText::new("Принять результат").color(ui.visuals().panel_fill),
                        )
                        .fill(ui.visuals().hyperlink_color),
                    )
                    .clicked()
            {
                self.request(
                    Request::Status {
                        id: task.id,
                        revision: task.revision,
                        status: TaskStatus::Accepted,
                    },
                    send,
                );
            }
            ui.add_space(8.0);
            let has_draft = !next.trim().is_empty();
            let mut launch = |ui: &mut egui::Ui| {
                self.git_controls(ui, task, send);
                self.executor_controls(ui, task, backend, available.edit, send);
                if task.active() {
                    if !task.git.as_ref().is_some_and(|git| {
                        git.state == hub_core::tasks::git::PreparationState::Preparing
                    }) {
                        ui.label(if self.local_context() {
                            "Выполняется. Кнопка остановки находится над выбором проекта."
                        } else {
                            "Выполняется. Остановить можно в теме Telegram или во вкладке «Темы»."
                        });
                    }
                } else {
                    self.launch_controls(ui, task, backend, &available, &mut next, send);
                }
            };
            if task.runs.last().is_some_and(|run| !run.result.is_empty()) && !task.active() {
                egui::CollapsingHeader::new("Подготовить следующий запуск")
                    .id_salt(("continue", task.id, task.runs.last().map(|run| run.id)))
                    .default_open(has_draft)
                    .open(reveal_next.then_some(true))
                    .show(ui, &mut launch);
            } else {
                ui.group(launch);
            }
            ui.collapsing("Статус и архив", |ui| {
                self.workflow_controls(ui, task, !available.edit, send);
            });
            ui.separator();
        });
        self.next.insert(task.id, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::domain::{AbsolutePath, ChatId, ThreadId, TopicSession};
    use hub_core::tasks::RunState;
    use hub_telegram::hub::{TopicState, TopicView};
    fn topic(key: TopicKey) -> TopicView {
        TopicView {
            key,
            title: Some("Test".into()),
            session: TopicSession::fresh(
                BackendKind::Qwen,
                AbsolutePath::new(std::env::temp_dir()).unwrap(),
            ),
            state: TopicState::Waiting,
            memory_enabled: None,
            parameters: String::new(),
            progress: hub_core::progress::TaskProgress::default(),
        }
    }
    fn frame(view: &mut TasksView, snapshot: &Snapshot, commands: &mut Vec<Command>) {
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            view.show(ui, snapshot, &mut |command| commands.push(command));
        });
        output.textures_delta.clear();
    }
    fn input_frame(
        ctx: &egui::Context,
        view: &mut TasksView,
        snapshot: &Snapshot,
        events: Vec<egui::Event>,
        commands: &mut Vec<Command>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 900.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                view.show(ui, snapshot, &mut |command| commands.push(command));
            },
        );
        output.textures_delta.clear();
        output
    }
    fn click_text(
        ctx: &egui::Context,
        view: &mut TasksView,
        snapshot: &Snapshot,
        text: &str,
        commands: &mut Vec<Command>,
    ) {
        input_frame(ctx, view, snapshot, vec![], commands);
        let output = input_frame(ctx, view, snapshot, vec![], commands);
        let position = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(label) if label.galley.text() == text => {
                    Some(label.galley.rect.translate(label.pos.to_vec2()).center())
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing text: {text}"));
        for pressed in [true, false] {
            input_frame(
                ctx,
                view,
                snapshot,
                vec![
                    egui::Event::PointerMoved(position),
                    egui::Event::PointerButton {
                        pos: position,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                commands,
            );
        }
    }
    #[test]
    fn attention_opens_the_requested_parallel_permission() {
        use crate::local_execution::{Answer, Interaction, Kind, Live};
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let cwd = std::env::temp_dir();
        let job = |id| Live {
            token: uuid::Uuid::new_v4(),
            project: cwd.clone(),
            id,
            run: Some(1),
            active: true,
            status: "Ожидание".into(),
            interaction: Some(Interaction {
                id: uuid::Uuid::new_v4(),
                kind: Kind::Permission(hub_core::domain::ToolRequest {
                    tool: format!("Tool{id}"),
                    summary: "Review".into(),
                }),
            }),
        };
        let first = job(1);
        let second = job(2);
        let snapshot = Snapshot {
            local: Some(first.clone()),
            local_jobs: vec![first, second.clone()],
            ..Default::default()
        };
        assert_eq!(attention_count(&snapshot), 2);
        let mut view = TasksView::default();
        let mut opened = false;
        let mut attention_frame = |events| {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    events,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1000.0, 900.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    opened |= view.show_attention(ui, &snapshot);
                },
            );
            output.textures_delta.clear();
            output
        };
        let _ = attention_frame(vec![]);
        let output = attention_frame(vec![]);
        let point = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text().contains("Разрешить Tool2") => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            })
            .unwrap();
        for pressed in [true, false] {
            let _ = attention_frame(vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        assert!(opened);
        let mut commands = vec![];
        click_text(&ctx, &mut view, &snapshot, "Разрешить один раз", &mut commands);
        assert!(
            commands
                .iter()
                .any(|cmd| matches!(cmd,Command::LocalAnswer{token,id,answer:Answer::Allow,..}
            if *token==second.token && Some(*id)==second.interaction.as_ref().map(|i|i.id)))
        );
    }
    #[test]
    fn wide_task_workspace_places_project_list_before_tasks_and_details() {
        use crate::desktop_tasks::TaskRow;
        let ctx = egui::Context::default();
        let dir = tempfile::tempdir().unwrap();
        let project = hub_telegram::paths::workspace_root(dir.path()).unwrap();
        let mut book = hub_core::tasks::TaskBook::default();
        let first = book.create(project.as_path().to_owned(), "codex", "Первое", "Работа").unwrap();
        book.create(project.as_path().to_owned(), "qwen", "Второе", "Ещё работа").unwrap();
        let rows = book
            .tasks()
            .iter()
            .map(|t| TaskRow {
                id: t.id,
                project: t.project.clone(),
                title: t.title.clone(),
                reason: None,
            })
            .collect();
        let snapshot = Snapshot { task_index: Ok(rows), ..Default::default() };
        let mut view = TasksView {
            tasks: book.tasks().to_vec(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        view.local_project.choose(project.as_path());
        view.scope = Some((None, project.as_path().to_owned(), BackendKind::Qwen));
        view.focused = Some(first);
        let mut render = || {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1440.0, 1000.0),
                    )),
                    ..Default::default()
                },
                |ui| {
                    view.project_navigation(ui, &snapshot, true);
                    view.show(ui, &snapshot, &mut |_| {});
                },
            );
            output.textures_delta.clear();
            output
        };
        let _ = render();
        let output = render();
        let x = |label: &str| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => Some(text.pos.x),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("missing: {label}"))
        };
        let project_x = x("Проекты");
        let tasks_x = x("#1  Первое\ncodex · Запланирована");
        assert!(project_x < tasks_x && tasks_x < x("#1 · r1"));
    }
    #[test]
    fn failed_permission_delivery_allows_retry_or_deny() {
        use crate::local_execution::{Answer, Interaction, Kind, Live};
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let mut view = TasksView::default();
        let mut commands = Vec::new();
        let id = uuid::Uuid::new_v4();
        let token = uuid::Uuid::new_v4();
        let snapshot = Snapshot {
            local: Some(Live {
                run: None,
                token,
                project: std::env::temp_dir(),
                id: 1,
                active: true,
                status: "Ожидание".into(),
                interaction: Some(Interaction {
                    id,
                    kind: Kind::Permission(hub_core::domain::ToolRequest {
                        tool: "Read".into(),
                        summary: "README".into(),
                    }),
                }),
            }),
            ..Default::default()
        };
        click_text(&ctx, &mut view, &snapshot, "Разрешить один раз", &mut commands);
        let Some(Command::LocalAnswer { reply, answer: Answer::Allow, .. }) = commands.pop() else {
            panic!("permission reply")
        };
        reply.send(Err("Доставка не удалась".into())).unwrap();
        click_text(&ctx, &mut view, &snapshot, "Отклонить", &mut commands);
        assert!(
            matches!(commands.pop(),Some(Command::LocalAnswer{id:actual,token:job,answer:Answer::Deny,..}) if actual==id && job==token)
        );
    }

    #[test]
    fn oversized_unicode_answer_is_not_dispatched() {
        use crate::local_execution::{Interaction, Kind, Live};
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let mut view = TasksView::default();
        let mut commands = Vec::new();
        let question = hub_core::domain::Question::new(
            "Вопрос?".into(),
            String::new(),
            vec![],
            hub_core::domain::Selection::Single,
        )
        .unwrap();
        let snapshot = Snapshot {
            local: Some(Live {
                run: None,
                token: uuid::Uuid::new_v4(),
                project: std::env::temp_dir(),
                id: 1,
                active: true,
                status: "Ожидание".into(),
                interaction: Some(Interaction {
                    id: uuid::Uuid::new_v4(),
                    kind: Kind::Questions(vec![question]),
                }),
            }),
            ..Default::default()
        };
        click_text(&ctx, &mut view, &snapshot, "Ответ или свой вариант", &mut commands);
        input_frame(
            &ctx,
            &mut view,
            &snapshot,
            vec![egui::Event::Text("Я".repeat(9000))],
            &mut commands,
        );
        click_text(&ctx, &mut view, &snapshot, "Отправить ответы", &mut commands);
        assert!(commands.is_empty(), "byte overflow must leave editing and deny available");
        click_text(&ctx, &mut view, &snapshot, "Отказаться отвечать", &mut commands);
        assert!(matches!(
            commands.pop(),
            Some(Command::LocalAnswer { answer: crate::local_execution::Answer::Deny, .. })
        ));
    }

    #[test]
    fn unconfigured_gui_opens_project_and_creates_task_without_a_topic() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let project = tempfile::tempdir().unwrap();
        let mut view = TasksView::default();
        let snapshot = Snapshot::default();
        let mut commands = Vec::new();
        click_text(&ctx, &mut view, &snapshot, "Абсолютный путь к проекту", &mut commands);
        input_frame(
            &ctx,
            &mut view,
            &snapshot,
            vec![egui::Event::Text(project.path().display().to_string())],
            &mut commands,
        );
        click_text(&ctx, &mut view, &snapshot, "Открыть проект", &mut commands);
        let Command::LocalTasks {
            project: selected, request: Request::Search { .. }, reply, ..
        } = commands.pop().unwrap()
        else {
            panic!("local read")
        };
        assert!(selected.is_dir());
        reply.send(Ok(TaskSnapshot::default())).unwrap();
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        view.title = "Локальная задача".into();
        view.prompt = "Прочитать README".into();
        click_text(&ctx, &mut view, &snapshot, "Создать задачу", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Сохранить задачу без запуска", &mut commands);
        let Command::LocalTasks {
            project: selected,
            backend,
            request: Request::Create { title, prompt },
            reply,
        } = commands.pop().unwrap()
        else {
            panic!("local create")
        };
        let mut book = hub_core::tasks::TaskBook::default();
        book.create(selected, backend.name(), &title, &prompt).unwrap();
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..Default::default() }))
            .unwrap();
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert_eq!(view.tasks.len(), 1);
        assert!(view.local_context());
        assert!(view.tasks[0].runs.is_empty());
        assert!(commands.is_empty());
        let running =
            Snapshot { bot: BotStatus::Running { username: "bot".into() }, ..Default::default() };
        input_frame(&ctx, &mut view, &running, vec![], &mut commands);
        assert!(view.local_context(), "bot connection must not switch the user's editing mode");
        click_text(&ctx, &mut view, &snapshot, "Запустить локально", &mut commands);
        assert!(matches!(
            commands.pop(),
            Some(Command::LocalTasks {
                request: Request::StartChecked { id: 1, revision: 1, .. },
                ..
            })
        ));
    }
    #[test]
    fn git_check_failure_preserves_the_continuation_draft() {
        use hub_core::tasks::{TaskBook, git::GitPolicy};
        let project = std::env::temp_dir();
        let mut book = TaskBook::default();
        let id = book.create(project.clone(), "qwen", "Task", "Read").unwrap();
        let mut rules = book.rules(&project);
        rules.git = Some(GitPolicy { remote: "origin".into(), base: "Dev".into() });
        book.configure_rules(project.clone(), 0, rules).unwrap();
        book.prepare_git(id, 1, "task/first").unwrap();
        book.finish_git(id, 2, Ok("a".repeat(40))).unwrap();
        let run = book.begin(id, 3, None).unwrap();
        book.finish(id, run, RunState::Completed).unwrap();
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut view = TasksView {
            scope: Some((Some(key), project.clone(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            ..Default::default()
        };
        view.next.insert(id, "Продолжить после проверки результатов".into());
        book.note_git(id, 5, "Выбрана другая ветка").unwrap();
        let mut commands = Vec::new();
        view.request(Request::List, &mut |command| commands.push(command));
        let Command::Tasks { reply, .. } = commands.pop().unwrap() else { panic!("list") };
        reply
            .send(Ok(TaskSnapshot {
                tasks: book.tasks().to_vec(),
                rules: book.rules(&project),
                git_busy: false,
            }))
            .unwrap();
        view.poll();
        assert_eq!(
            view.next.get(&id).map(String::as_str),
            Some("Продолжить после проверки результатов")
        );
    }
    #[test]
    fn ready_git_task_can_be_prepared_again_with_a_new_branch_before_first_run() {
        use hub_core::tasks::{TaskBook, git::GitPolicy};
        let ctx = egui::Context::default();
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let project = std::env::temp_dir();
        let mut book = TaskBook::default();
        let id = book.create(project.clone(), "qwen", "Task", "Read").unwrap();
        let mut rules = book.rules(&project);
        rules.git = Some(GitPolicy { remote: "origin".into(), base: "Dev".into() });
        book.configure_rules(project.clone(), 0, rules).unwrap();
        book.prepare_git(id, 1, "task/first").unwrap();
        book.finish_git(id, 2, Ok("a".repeat(40))).unwrap();
        book.note_git(id, 3, "Ветка изменилась после подготовки").unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), project.clone(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        view.rules.accept(book.rules(&project));
        view.git_branches.insert(id, "task/retry".into());
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key)],
            ..Default::default()
        };
        let mut commands = Vec::new();
        click_text(&ctx, &mut view, &snapshot, "Подготовить ветку Git", &mut commands);
        assert!(
            matches!(commands.pop(), Some(Command::Tasks { request: Request::PrepareGitChecked { revision: 4, branch, .. }, .. }) if branch == "task/retry")
        );
    }
    #[test]
    fn git_preparation_is_explicit_and_enables_a_separate_launch() {
        use hub_core::tasks::{TaskBook, git::GitPolicy};
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let project = std::env::temp_dir();
        let mut book = TaskBook::default();
        let id = book.create(project.clone(), "qwen", "Task", "Read").unwrap();
        let mut rules = book.rules(&project);
        rules.git = Some(GitPolicy { remote: "origin".into(), base: "Dev".into() });
        book.configure_rules(project.clone(), 0, rules).unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), project.clone(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        view.rules.accept(book.rules(&project));
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key)],
            ..Default::default()
        };
        let mut commands = Vec::new();
        click_text(&ctx, &mut view, &snapshot, "Запустить задачу в выбранной теме", &mut commands);
        assert!(commands.is_empty());
        click_text(&ctx, &mut view, &snapshot, "Подготовить ветку Git", &mut commands);
        let Command::Tasks {
            request:
                Request::PrepareGitChecked { id: requested, revision: 1, branch, rules_revision: 1 },
            reply,
            ..
        } = commands.pop().unwrap()
        else {
            panic!("prepare")
        };
        assert_eq!(requested, id);
        assert_eq!(branch, "task/1");
        book.prepare_git(id, 1, &branch).unwrap();
        reply
            .send(Ok(TaskSnapshot {
                tasks: book.tasks().to_vec(),
                rules: book.rules(&project),
                git_busy: true,
            }))
            .unwrap();
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(view.git_busy);
        book.finish_git(id, 2, Ok("a".repeat(40))).unwrap();
        view.request(Request::List, &mut |command| commands.push(command));
        let Command::Tasks { reply, .. } = commands.pop().unwrap() else { panic!("refresh") };
        reply
            .send(Ok(TaskSnapshot {
                tasks: book.tasks().to_vec(),
                rules: book.rules(&project),
                git_busy: false,
            }))
            .unwrap();
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(commands.is_empty(), "preparation must not automatically launch the agent");
        click_text(&ctx, &mut view, &snapshot, "Запустить задачу в выбранной теме", &mut commands);
        assert!(matches!(
            commands.pop(),
            Some(Command::Tasks {
                request: Request::StartChecked { revision: 3, rules_revision: 1, .. },
                ..
            })
        ));
    }
    #[test]
    fn project_rules_editor_has_separate_undo_state_for_each_project() {
        use hub_core::tasks::ProjectRules;
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let other = TopicKey { chat: key.chat, thread: ThreadId(8) };
        let mut other_topic = topic(other);
        other_topic.session.cwd =
            AbsolutePath::new(std::env::temp_dir().join("other-project")).unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), std::env::temp_dir(), BackendKind::Qwen)),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        view.rules.accept(ProjectRules {
            workflow: None,
            git: None,
            revision: 1,
            enabled: true,
            text: "Project A".into(),
        });
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key), other_topic],
            ..Default::default()
        };
        let mut commands = Vec::new();
        click_text(&ctx, &mut view, &snapshot, "Правила проекта", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Project A", &mut commands);
        let first_editor = ctx.memory(egui::Memory::focused).unwrap();
        input_frame(
            &ctx,
            &mut view,
            &snapshot,
            vec![egui::Event::Text("Draft A".into())],
            &mut commands,
        );
        view.selected = Some(other);
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        let Command::Tasks { reply, .. } = commands.pop().unwrap() else { panic!("refresh") };
        reply
            .send(Ok(TaskSnapshot {
                git_busy: false,
                tasks: vec![],
                rules: ProjectRules {
                    workflow: None,
                    git: None,
                    revision: 1,
                    enabled: true,
                    text: "Project B".into(),
                },
            }))
            .unwrap();
        click_text(&ctx, &mut view, &snapshot, "Project B", &mut commands);
        let second_editor = ctx.memory(egui::Memory::focused).unwrap();
        assert_ne!(first_editor, second_editor, "Undo history must not be shared across projects");
        input_frame(
            &ctx,
            &mut view,
            &snapshot,
            vec![egui::Event::Key {
                key: egui::Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers { ctrl: true, command: true, ..Default::default() },
            }],
            &mut commands,
        );
        assert!(!view.rules.dirty(), "Undo must not restore another project's draft");
    }
    #[test]
    fn project_rules_typing_survives_refresh_and_launch_requires_saved_revision() {
        use hub_core::tasks::ProjectRules;
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        book.create(std::env::temp_dir(), "qwen", "Task", "Read").unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), std::env::temp_dir(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        let rules = ProjectRules {
            workflow: None,
            git: None,
            revision: 1,
            enabled: true,
            text: "Rule".into(),
        };
        view.rules.accept(rules.clone());
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key)],
            ..Default::default()
        };
        let mut commands = Vec::new();
        click_text(&ctx, &mut view, &snapshot, "Правила проекта", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Rule", &mut commands);
        let focus = ctx.memory(egui::Memory::focused).unwrap();
        view.refreshed = None;
        input_frame(&ctx, &mut view, &snapshot, vec![egui::Event::Text("A".into())], &mut commands);
        let Command::Tasks { request: Request::Search { .. }, reply, .. } = commands.pop().unwrap()
        else {
            panic!("refresh")
        };
        reply
            .send(Ok(TaskSnapshot { git_busy: false, tasks: book.tasks().to_vec(), rules }))
            .unwrap();
        input_frame(&ctx, &mut view, &snapshot, vec![egui::Event::Text("B".into())], &mut commands);
        assert_eq!(ctx.memory(egui::Memory::focused), Some(focus));
        assert!(view.rules.dirty());
        click_text(&ctx, &mut view, &snapshot, "Запустить задачу в выбранной теме", &mut commands);
        assert!(commands.is_empty());
        click_text(&ctx, &mut view, &snapshot, "Сохранить правила", &mut commands);
        let Command::Tasks {
            request: Request::SaveRules { revision: 1, enabled: true, text },
            reply,
            ..
        } = commands.pop().unwrap()
        else {
            panic!("save rules")
        };
        assert!(text.contains("AB"));
        assert!(view.rules.dirty(), "not acknowledged yet");
        reply
            .send(Ok(TaskSnapshot {
                git_busy: false,
                tasks: vec![],
                rules: ProjectRules { workflow: None, git: None, revision: 2, enabled: true, text },
            }))
            .unwrap();
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(!view.rules.dirty());
        click_text(&ctx, &mut view, &snapshot, "Запустить задачу в выбранной теме", &mut commands);
        assert!(matches!(
            commands.pop(),
            Some(Command::Tasks { request: Request::StartChecked { rules_revision: 2, .. }, .. })
        ));
    }
    #[test]
    fn running_task_shows_assignment_control_but_does_not_allow_mutation() {
        let ctx = egui::Context::default();
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(std::env::temp_dir(), "qwen", "Task", "Read").unwrap();
        book.begin(id, 1, None).unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), std::env::temp_dir(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key)],
            ..Default::default()
        };
        let mut commands = Vec::new();
        click_text(&ctx, &mut view, &snapshot, "Назначить", &mut commands);
        assert!(commands.is_empty());
        assert_eq!(view.tasks, book.tasks());
    }
    #[test]
    fn agent_can_be_assigned_directly_without_changing_topic() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(std::env::temp_dir(), "qwen", "Task", "Read").unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), std::env::temp_dir(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key)],
            ..Default::default()
        };
        let mut commands = Vec::new();
        click_text(&ctx, &mut view, &snapshot, "qwen", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "codex", &mut commands);
        assert!(commands.is_empty(), "choosing an agent is only a draft");
        click_text(&ctx, &mut view, &snapshot, "Запустить задачу в выбранной теме", &mut commands);
        assert!(commands.is_empty(), "cannot launch with an unsaved agent choice");
        click_text(&ctx, &mut view, &snapshot, "Назначить", &mut commands);
        let Command::Tasks {
            backend: BackendKind::Qwen,
            request: Request::Assign { id: assigned, revision: 1, backend: BackendKind::Codex },
            reply,
            ..
        } = commands.pop().unwrap()
        else {
            panic!("direct assignment")
        };
        assert_eq!(assigned, id);
        book.assign(id, 1, "codex").unwrap();
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..TaskSnapshot::default() }))
            .unwrap();
        view.poll();
        assert_eq!(view.tasks[0].backend, "codex");
        assert_eq!(view.scope.as_ref().unwrap().2, BackendKind::Qwen);
        let output = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Выберите сверху тему этого проекта с назначенным агентом.")));
        assert!(commands.is_empty());
    }
    #[test]
    fn task_card_orders_executor_step_preview_and_secondary_actions() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(std::env::temp_dir(), "codex", "Task", "Read").unwrap();
        book.begin(id, 1, None).unwrap();
        book.record(id, 1, "Готовый результат").unwrap();
        book.finish(id, 1, RunState::Completed).unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), std::env::temp_dir(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key)],
            ..Default::default()
        };
        let mut commands = Vec::new();
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        let output = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(output.shapes.iter().any(
            |s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text() == "Готовый результат")
        ));
        assert!(!output.shapes.iter().any(
            |s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.text() == "1. Исполнитель")
        ));
        click_text(&ctx, &mut view, &snapshot, "Подготовить следующий запуск", &mut commands);
        let output = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        let labels = [
            "История попыток (1)",
            "1. Исполнитель",
            "2. Следующий шаг",
            "3. Проверка и запуск",
            "Статус и архив",
        ];
        let positions: Vec<_> = labels
            .iter()
            .map(|label| {
                output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text() == *label => Some(text.pos.y),
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("missing workflow section: {label}"))
            })
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Выберите сверху тему этого проекта с назначенным агентом.")));
        click_text(&ctx, &mut view, &snapshot, "Продолжить новым запуском", &mut commands);
        assert!(commands.is_empty(), "unassigned task must not launch");
    }
    #[test]
    fn transfer_preserves_typed_step_across_same_project_topic_and_assignment() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let receiving = TopicKey { chat: key.chat, thread: ThreadId(8) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(std::env::temp_dir(), "codex", "Task", "Read").unwrap();
        book.begin(id, 1, None).unwrap();
        book.record(id, 1, "Previous result").unwrap();
        book.finish(id, 1, RunState::Completed).unwrap();
        let mut raw = serde_json::to_value(&book).unwrap();
        raw["tasks"][0]["runs"][0].as_object_mut().unwrap().remove("backend");
        let mut book =
            hub_core::tasks::TaskBook::decode(&serde_json::to_vec(&raw).unwrap()).unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), std::env::temp_dir(), BackendKind::Codex)),
            tasks: book.tasks().to_vec(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        view.next.insert(id, "Проверить результат".into());
        let mut original = topic(key);
        original.session.backend = BackendKind::Codex;
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![original, topic(receiving)],
            ..Default::default()
        };
        let mut commands = Vec::new();
        click_text(&ctx, &mut view, &snapshot, "Проверить результат", &mut commands);
        input_frame(
            &ctx,
            &mut view,
            &snapshot,
            vec![egui::Event::Text("новый шаг ".into())],
            &mut commands,
        );
        let draft = view.next[&id].clone();
        assert!(draft.contains("новый шаг"));
        view.selected = Some(receiving);
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert_eq!(view.next.get(&id), Some(&draft), "same-project topic change lost the step");
        let Command::Tasks { reply, .. } = commands.pop().unwrap() else { panic!("refresh") };
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..TaskSnapshot::default() }))
            .unwrap();
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        click_text(&ctx, &mut view, &snapshot, "codex", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "qwen", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Назначить", &mut commands);
        let Command::Tasks { request: Request::Assign { .. }, reply, .. } = commands.pop().unwrap()
        else {
            panic!("assign")
        };
        book.assign(id, 3, "qwen").unwrap();
        reply
            .send(Ok(TaskSnapshot {
                tasks: vec![book.get(id).unwrap().clone()],
                ..TaskSnapshot::default()
            }))
            .unwrap();
        view.poll();
        assert_eq!(view.next.get(&id), Some(&draft), "assignment lost the step");
        assert!(
            view.tasks[0].preview(view.next.get(&id).map(String::as_str)).unwrap().contains(&draft)
        );
    }
    #[test]
    fn transfer_button_is_explicit_and_failed_assignment_keeps_previous_agent() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(std::env::temp_dir(), "codex", "Task", "Read").unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), std::env::temp_dir(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key)],
            ..Default::default()
        };
        let mut commands = Vec::new();
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(commands.is_empty(), "rendering a different topic agent must not assign it");
        click_text(&ctx, &mut view, &snapshot, "codex", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "qwen", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Назначить", &mut commands);
        let Command::Tasks {
            request: Request::Assign { id: assigned, revision: 1, backend: BackendKind::Qwen },
            reply,
            ..
        } = commands.pop().unwrap()
        else {
            panic!("assign")
        };
        assert_eq!(assigned, id);
        reply.send(Err("Не удалось сохранить".into())).unwrap();
        view.poll();
        assert_eq!(view.tasks[0].backend, "codex");
        assert_eq!(view.tasks[0].revision, 1);
    }
    #[test]
    fn gui_preview_shows_the_exact_prompt_without_dispatching_a_request() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(std::env::temp_dir(), "qwen", "Task", "Read\n  source").unwrap();
        let expected = book.get(id).unwrap().preview(None).unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), std::env::temp_dir(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key)],
            ..Default::default()
        };
        let mut commands = Vec::new();
        click_text(&ctx, &mut view, &snapshot, "Контекст следующего запуска", &mut commands);
        let output = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == expected)));
        assert!(commands.is_empty());
        assert_eq!(view.tasks, book.tasks());
    }
    #[test]
    fn refresh_keeps_buttons_usable_and_user_action_supersedes_stale_read() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        book.create(std::env::temp_dir(), "qwen", "Existing", "Read").unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), std::env::temp_dir(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            title: "New task".into(),
            prompt: "Check".into(),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key)],
            ..Default::default()
        };
        let mut commands = Vec::new();
        click_text(&ctx, &mut view, &snapshot, "Создать задачу", &mut commands);
        view.request(Request::List, &mut |command| commands.push(command));
        let Command::Tasks { reply: stale_read, .. } = commands.pop().unwrap() else {
            panic!("list")
        };
        let available = view.availability(&view.tasks[0], BackendKind::Qwen, false);
        assert!(available.edit && available.launch, "refresh must not disable task buttons");
        click_text(&ctx, &mut view, &snapshot, "Сохранить задачу без запуска", &mut commands);
        let Command::Tasks { request: Request::Create { .. }, reply, .. } =
            commands.pop().expect("save button must work during refresh")
        else {
            panic!("create")
        };
        assert!(
            stale_read.send(Ok(TaskSnapshot::default())).is_err(),
            "superseded read must not overwrite mutation"
        );
        view.request(
            Request::Create { title: "Duplicate".into(), prompt: "No".into() },
            &mut |command| commands.push(command),
        );
        view.request(Request::List, &mut |command| commands.push(command));
        assert!(commands.is_empty(), "pending mutation blocks duplicate actions and refresh");
        let id = book.create(std::env::temp_dir(), "qwen", "New task", "Check").unwrap();
        reply
            .send(Ok(TaskSnapshot {
                tasks: vec![book.get(id).unwrap().clone()],
                ..TaskSnapshot::default()
            }))
            .unwrap();
        view.poll();
        assert_eq!(view.tasks.len(), 2);
        assert!(view.title.is_empty());
    }
    #[test]
    fn automatic_refresh_preserves_creation_focus_cursor_and_typing() {
        for field in ["Название черновика", "Текст поручения"] {
            let ctx = egui::Context::default();
            ctx.all_styles_mut(|style| style.animation_time = 0.0);
            let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
            let mut view = TasksView {
                selected: Some(key),
                scope: Some((Some(key), std::env::temp_dir(), BackendKind::Qwen)),
                title: "Название черновика".into(),
                prompt: "Текст поручения".into(),
                refreshed: Some(Instant::now()),
                ..Default::default()
            };
            let snapshot = Snapshot {
                bot: BotStatus::Running { username: "bot".into() },
                topics: vec![topic(key)],
                ..Default::default()
            };
            let mut commands = Vec::new();
            click_text(&ctx, &mut view, &snapshot, "Создать задачу", &mut commands);
            click_text(&ctx, &mut view, &snapshot, field, &mut commands);
            let focus = ctx.memory(egui::Memory::focused).expect("text field focused");
            input_frame(
                &ctx,
                &mut view,
                &snapshot,
                vec![egui::Event::Text("1".into())],
                &mut commands,
            );
            let before = if field == "Название черновика" {
                view.title.clone()
            } else {
                view.prompt.clone()
            };
            assert!(before.contains('1'));
            view.refreshed = Some(Instant::now().checked_sub(Duration::from_secs(4)).unwrap());
            input_frame(
                &ctx,
                &mut view,
                &snapshot,
                vec![egui::Event::Text("2".into())],
                &mut commands,
            );
            assert_eq!(
                ctx.memory(egui::Memory::focused),
                Some(focus),
                "background refresh stole focus"
            );
            let Command::Tasks { request: Request::Search { .. }, reply, .. } =
                commands.pop().unwrap()
            else {
                panic!("refresh")
            };
            assert!(commands.is_empty());
            input_frame(
                &ctx,
                &mut view,
                &snapshot,
                vec![egui::Event::Text("3".into())],
                &mut commands,
            );
            reply.send(Ok(TaskSnapshot::default())).unwrap();
            input_frame(
                &ctx,
                &mut view,
                &snapshot,
                vec![egui::Event::Text("4".into())],
                &mut commands,
            );
            let after = if field == "Название черновика" {
                &view.title
            } else {
                &view.prompt
            };
            assert_eq!(
                *after,
                before.replace('1', "1234"),
                "cursor must stay at the insertion point"
            );
            assert_eq!(ctx.memory(egui::Memory::focused), Some(focus));
        }
    }
    #[test]
    fn occupied_topic_only_blocks_launch_not_review_of_an_inactive_task() {
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(std::env::temp_dir(), "qwen", "Task", "Read").unwrap();
        let mut view = TasksView::default();
        let available = view.availability(book.get(id).unwrap(), BackendKind::Qwen, true);
        assert!(available.edit && !available.launch);
        book.begin(id, 1, None).unwrap();
        assert!(!view.availability(book.get(id).unwrap(), BackendKind::Qwen, false).edit);
        book.finish(id, 1, RunState::Completed).unwrap();
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let (sender, receiver) = oneshot::channel();
        view.pending =
            Some(((Some(key), std::env::temp_dir(), BackendKind::Qwen), false, false, receiver));
        let available = view.availability(book.get(id).unwrap(), BackendKind::Qwen, false);
        assert!(!available.edit && !available.launch);
        drop(sender);
    }
    #[test]
    fn workflow_failure_keeps_visible_status_and_form_until_acknowledged() {
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(std::env::temp_dir(), "qwen", "Task", "Read").unwrap();
        let mut view = TasksView {
            scope: Some((Some(key), std::env::temp_dir(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            title: "Draft".into(),
            prompt: "Text".into(),
            ..Default::default()
        };
        let mut commands = Vec::new();
        view.request(Request::Status { id, revision: 1, status: TaskStatus::Accepted }, &mut |c| {
            commands.push(c);
        });
        view.request(Request::Archive { id, revision: 1, archived: true }, &mut |c| {
            commands.push(c);
        });
        assert_eq!(commands.len(), 1);
        assert_eq!(view.tasks[0].status.label(), "Запланирована");
        let Command::Tasks { reply, .. } = commands.pop().unwrap() else { panic!("task") };
        reply.send(Err("Нет доступа к файлу".into())).unwrap();
        view.poll();
        assert_eq!(view.tasks[0].status, TaskStatus::Planned);
        assert_eq!(view.title, "Draft");
        assert!(view.error.as_ref().unwrap().contains("Нет доступа"));
    }
    #[test]
    fn workflow_scope_change_resets_search_and_filter() {
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), std::env::temp_dir().join("old"), BackendKind::Qwen)),
            query: "private search".into(),
            filter: TaskFilter::Archived,
            ..Default::default()
        };
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key)],
            ..Default::default()
        };
        frame(&mut view, &snapshot, &mut Vec::new());
        assert!(view.query.is_empty());
        assert_eq!(view.filter, TaskFilter::Active);
    }
    #[test]
    fn stopped_bot_does_not_dispatch_and_topic_change_discards_stale_reply() {
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let other = TopicKey { chat: key.chat, thread: ThreadId(8) };
        let mut view = TasksView { selected: Some(key), ..Default::default() };
        let mut commands = Vec::new();
        frame(&mut view, &Snapshot::default(), &mut commands);
        assert!(commands.is_empty());
        view.local_mode = Some(false); // Explicitly choose Telegram after opening in local mode.
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key), topic(other)],
            ..Default::default()
        };
        frame(&mut view, &snapshot, &mut commands);
        let Command::Tasks {
            key: sent,
            request: Request::Search { filter: TaskFilter::All, .. },
            reply,
            ..
        } = commands.pop().unwrap()
        else {
            panic!("expected list")
        };
        assert_eq!(sent, key);
        let mut book = hub_core::tasks::TaskBook::default();
        book.create(std::env::temp_dir(), "qwen", "Old scope", "Read").unwrap();
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..TaskSnapshot::default() }))
            .unwrap();
        view.selected = Some(other);
        frame(&mut view, &snapshot, &mut commands);
        assert!(view.tasks.is_empty());
        assert_eq!(view.scope.as_ref().unwrap().0, Some(other));
    }
    #[test]
    fn start_command_uses_visible_revision_and_selected_scope() {
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let project = std::env::temp_dir();
        let mut view = TasksView {
            scope: Some((Some(key), project.clone(), BackendKind::Qwen)),
            ..Default::default()
        };
        let mut commands = Vec::new();
        view.request(
            Request::Start { id: 2, revision: 9, next: Some("Verify effects".into()) },
            &mut |command| commands.push(command),
        );
        view.request(Request::Start { id: 2, revision: 9, next: None }, &mut |command| {
            commands.push(command);
        });
        assert_eq!(commands.len(), 1, "pending request must not be sent twice");
        assert!(
            matches!(commands.pop().unwrap(),Command::Tasks {key:sent,project:cwd,backend:BackendKind::Qwen,request:Request::Start{id:2,revision:9,next:Some(_)},..} if sent==key&&cwd==project)
        );
    }
    #[test]
    fn managed_task_creation_draft_is_cleared_when_scope_changes() {
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), std::env::temp_dir().join("old"), BackendKind::Qwen)),
            title: "private title".into(),
            prompt: "private prompt".into(),
            ..Default::default()
        };
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key)],
            ..Default::default()
        };
        frame(&mut view, &snapshot, &mut Vec::new());
        assert!(view.title.is_empty() && view.prompt.is_empty());
    }
    #[test]
    fn managed_task_new_revision_clears_previous_continuation_step() {
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(std::env::temp_dir(), "qwen", "Task", "First").unwrap();
        let mut view = TasksView {
            scope: Some((Some(key), std::env::temp_dir(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            ..Default::default()
        };
        view.next.insert(id, "Old continuation".into());
        let mut commands = Vec::new();
        view.request(Request::List, &mut |command| commands.push(command));
        let Command::Tasks { reply, .. } = commands.pop().unwrap() else { panic!("task request") };
        book.begin(id, 1, None).unwrap();
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..TaskSnapshot::default() }))
            .unwrap();
        view.poll();
        assert!(!view.next.contains_key(&id), "a different revision needs a new step");
    }
}
