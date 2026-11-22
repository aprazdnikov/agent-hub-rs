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
mod sidebar;
mod workspace;
pub(super) use sidebar::SidebarAction;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

type Scope = (Option<TopicKey>, PathBuf, BackendKind);
type Reply = oneshot::Receiver<Result<TaskSnapshot, String>>;
struct Pending {
    scope: Scope,
    effect: Effect,
    reply: Reply,
}
enum Effect {
    List,
    Edit,
    SaveRules,
    Delete(u64),
    Create { start_with_rules: Option<u64>, open: bool },
    StartAfterPrompt { id: u64, rules_revision: u64, review: bool },
    Review { id: u64, follow: bool },
}
#[derive(Default, PartialEq, Eq)]
enum Page {
    #[default]
    Task,
    New,
    Rules,
}
struct Rework {
    task: u64,
    focus: bool,
}
struct PromptDraft {
    original: String,
    text: String,
}
#[derive(Default)]
pub struct TasksView {
    sidebar: sidebar::State,
    inbox: attention::Inbox,
    result_view: result::View,
    review_from_inbox: bool,
    inbox_return: bool,
    reworking: Option<Rework>,
    page: Page,
    focused: Option<u64>,
    confirm_delete: Option<(u64, u64)>,
    local_mode: Option<bool>,
    local_panel: local::Panel,
    local_block: LocalBlock,
    local_project: context::LocalProject,
    rules: rules::Editor,
    git_busy: bool,
    git_branches: BTreeMap<u64, String>,
    selected: Option<TopicKey>,
    scope: Option<Scope>,
    tasks: Vec<Task>,
    pending: Option<Pending>,
    refreshed: Option<Instant>,
    title: String,
    prompt: String,
    next: BTreeMap<u64, String>,
    prompt_drafts: BTreeMap<(PathBuf, u64), PromptDraft>,
    parked_drafts: BTreeMap<(PathBuf, u64), (Task, String)>,
    assignees: BTreeMap<u64, (u64, BackendKind)>,
    error: Option<String>,
    read_error: Option<String>,
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
// Changing the executor or memory policy preserves the message being composed.
// Normalize legacy ownership on copies so future Run fields participate in equality too.
fn only_execution_preferences_changed(old: &Task, new: &Task) -> bool {
    if new.revision <= old.revision || (old.backend == new.backend && old.memory == new.memory) {
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
    before.memory = after.memory;
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
fn draft_matches(old: &Task, new: &Task) -> bool {
    old.id == new.id
        && old.project == new.project
        && (old.revision == new.revision
            || old.runs.last().is_some_and(|run| {
                run.state == hub_core::tasks::RunState::Running
                    && new.runs.last().is_some_and(|last| last.id == run.id)
            })
            || only_execution_preferences_changed(old, new)
            || only_git_diagnostic_changed(old, new))
}
impl TasksView {
    fn mutation_pending(&self) -> bool {
        self.pending.as_ref().is_some_and(|p| !matches!(p.effect, Effect::List))
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
        let Some(scope) = self.scope.clone() else { return };
        let effect = match request {
            Request::List | Request::Search { .. } => Effect::List,
            Request::SaveRules { .. } | Request::SaveProjectRules { .. } => Effect::SaveRules,
            Request::Delete { id, .. } => Effect::Delete(id),
            Request::Create { .. } => Effect::Create { start_with_rules: None, open: true },
            _ => Effect::Edit,
        };
        self.dispatch(scope, request, effect, send);
    }
    fn dispatch(
        &mut self,
        scope: Scope,
        request: Request,
        effect: Effect,
        send: &mut impl FnMut(Command),
    ) {
        if self.mutation_pending() || (matches!(effect, Effect::List) && self.pending.is_some()) {
            return;
        }
        // A user action supersedes a background read. Mutations retain their original
        // target and remain serialized even when the user chooses another project.
        let (reply, receiver) = oneshot::channel();
        let (key, project, backend) = scope.clone();
        if let Some(key) = key {
            send(Command::Tasks { key, project, backend, request, reply });
        } else {
            send(Command::LocalTasks { project, backend, request, reply });
        }
        if !matches!(effect, Effect::List) {
            self.error = None;
        }
        self.pending = Some(Pending { scope, effect, reply: receiver });
    }
    pub(in crate::gui) fn update(&mut self, send: &mut impl FnMut(Command)) {
        let Some(mut pending) = self.pending.take() else { return };
        let result = match pending.reply.try_recv() {
            Ok(result) => result,
            Err(oneshot::error::TryRecvError::Empty) => {
                self.pending = Some(pending);
                return;
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                Err("Запрос не принят или сервис остановлен. Повторите действие.".into())
            }
        };
        let current = self.scope.as_ref() == Some(&pending.scope);
        if current {
            self.refreshed = Some(Instant::now());
        }
        let snapshot = match result {
            Ok(snapshot) => snapshot,
            Err(error) => {
                if current && matches!(pending.effect, Effect::List) {
                    self.read_error = Some(error);
                } else if current {
                    self.error = Some(error);
                } else if !matches!(pending.effect, Effect::List) {
                    self.error = Some(format!("{} · {error}", pending.scope.1.display()));
                }
                return;
            }
        };
        let saved_task = snapshot
            .tasks
            .iter()
            .find(|task| {
                task.project == pending.scope.1
                    && match pending.effect {
                        Effect::Create { .. } => true,
                        Effect::StartAfterPrompt { id, .. } => task.id == id,
                        _ => false,
                    }
            })
            .map(|task| (task.id, task.revision));
        if current {
            self.read_error = None;
            self.apply_snapshot(snapshot, &pending.effect);
        }
        match pending.effect {
            Effect::Create { start_with_rules, open } => {
                let Some((id, revision)) = saved_task else {
                    self.error = Some("Сервис не вернул созданную задачу. Обновите проект.".into());
                    return;
                };
                if current {
                    self.title.clear();
                    self.prompt.clear();
                    if open {
                        self.focused = Some(id);
                        self.page = Page::Task;
                        self.result_view.select(result::Tab::Chat, Some(id));
                    }
                }
                if let Some(rules_revision) = start_with_rules {
                    self.dispatch(
                        pending.scope,
                        Request::StartChecked { id, revision, next: None, rules_revision },
                        Effect::Edit,
                        send,
                    );
                }
            }
            Effect::StartAfterPrompt { id, rules_revision, review } => {
                let Some((_, revision)) = saved_task else {
                    self.error =
                        Some("Сервис не подтвердил изменение поручения. Обновите проект.".into());
                    return;
                };
                let effect =
                    if review { Effect::Review { id, follow: true } } else { Effect::Edit };
                self.dispatch(
                    pending.scope,
                    Request::StartChecked { id, revision, rules_revision, next: None },
                    effect,
                    send,
                );
            }
            Effect::Review { id, follow: true } if current && self.focused == Some(id) => {
                self.inbox_return = self.review_from_inbox;
                self.review_from_inbox = false;
                self.reworking = None;
                self.result_view.select(result::Tab::Chat, Some(id));
            }
            _ => {}
        }
    }
    fn apply_snapshot(&mut self, snapshot: TaskSnapshot, effect: &Effect) {
        self.git_busy = snapshot.git_busy;
        if matches!(effect, Effect::SaveRules) {
            self.rules.accept(snapshot.rules);
        } else {
            self.rules.observe(snapshot.rules);
        }
        let tasks = if matches!(effect, Effect::List) {
            snapshot.tasks
        } else {
            let mut merged = self.tasks.clone();
            if let Effect::Delete(id) = effect {
                merged.retain(|task| task.id != *id);
                self.git_branches.remove(id);
                self.next.remove(id);
            }
            for task in snapshot.tasks {
                if let Some(old) = merged.iter_mut().find(|old| old.id == task.id) {
                    *old = task;
                } else {
                    merged.push(task);
                }
            }
            merged
        };
        self.next.retain(|id, _| {
            self.tasks
                .iter()
                .find(|old| old.id == *id)
                .is_some_and(|old| tasks.iter().any(|new| draft_matches(old, new)))
        });
        for task in &tasks {
            if let Some((old, draft)) = self.parked_drafts.remove(&(task.project.clone(), task.id))
                && draft_matches(&old, task)
            {
                self.next.insert(task.id, draft);
            }
        }
        if matches!(effect, Effect::List)
            && let Some((_, project, _)) = &self.scope
        {
            // A full read also tells us which tasks were deleted while the project was closed.
            self.parked_drafts.retain(|(path, _), _| path != project);
        }
        self.prompt_drafts.retain(|(path, id), draft| {
            if self.scope.as_ref().is_none_or(|scope| scope.1 != *path) {
                return true;
            }
            match tasks.iter().find(|task| task.id == *id) {
                Some(task) => task.runs.is_empty() && draft.text != task.prompt,
                None => {
                    !matches!(effect, Effect::List)
                        && !matches!(effect,Effect::Delete(removed) if removed == id)
                }
            }
        });
        self.assignees.retain(|id, (revision, _)| {
            tasks.iter().any(|task| task.id == *id && task.revision == *revision)
        });
        self.tasks = tasks;
    }
    pub(in crate::gui) fn take_inbox_return(&mut self) -> bool {
        std::mem::take(&mut self.inbox_return)
    }
    pub(in crate::gui) fn leave_review(&mut self) {
        self.review_from_inbox = false;
        self.inbox_return = false;
        self.reworking = None;
        match self.pending.as_mut().map(|p| &mut p.effect) {
            Some(Effect::Create { open, .. }) => *open = false,
            Some(Effect::Review { follow, .. }) => *follow = false,
            Some(Effect::StartAfterPrompt { review, .. }) => *review = false,
            _ => {}
        }
    }
    fn set_scope(&mut self, scope: Option<Scope>) {
        if scope == self.scope {
            return;
        }
        let same_project =
            self.scope.as_ref().zip(scope.as_ref()).is_some_and(|(old, new)| old.1 == new.1);
        if !same_project {
            for task in std::mem::take(&mut self.tasks) {
                if let Some(draft) = self.next.remove(&task.id).filter(|text| !text.is_empty()) {
                    self.parked_drafts.insert((task.project.clone(), task.id), (task, draft));
                }
            }
            self.next.clear();
            self.confirm_delete = None;
            self.git_branches.clear();
            self.git_busy = false;
            self.rules = rules::Editor::default();
            self.assignees.clear();
        }
        if !same_project && self.scope.is_some() {
            self.title.clear();
            self.prompt.clear();
        }
        self.scope = scope;
        self.error = None;
        self.read_error = None;
        if !self.mutation_pending() {
            self.pending = None;
        }
        self.refreshed = None;
    }
    fn refresh(&mut self, send: &mut impl FnMut(Command)) {
        let seconds = if self.tasks.iter().any(Task::active) { 1 } else { 3 };
        if self.pending.is_none()
            && self.refreshed.is_none_or(|time| time.elapsed() > Duration::from_secs(seconds))
        {
            self.request(Request::Search { filter: TaskFilter::All, query: String::new() }, send);
        }
    }
    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        self.local_block = if snapshot.local.as_ref().is_some_and(|live| live.active) {
            LocalBlock::Job
        } else {
            LocalBlock::Free
        };
        let scope = self.resolved_scope(snapshot);
        self.set_scope(scope);
        self.update(send);
        self.refresh(send);
        ui.ctx().request_repaint_after(Duration::from_millis(250));
        egui::Frame::new().inner_margin(egui::Margin::symmetric(12, 4)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(error) = self.error.as_ref().or(self.read_error.as_ref()) {
                egui::ScrollArea::vertical()
                    .id_salt("task_error")
                    .min_scrolled_height(0.0)
                    .max_height(54.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(error).color(ui.visuals().error_fg_color),
                            )
                            .wrap()
                            .selectable(true),
                        );
                    });
            }
            if self.page == Page::Rules
                && let Some((_, project, _)) = self.scope.clone()
            {
                egui::ScrollArea::vertical().id_salt("task_project_rules").show(ui, |ui| {
                    if ui.button("Назад").clicked() {
                        self.page = Page::Task;
                    }
                    ui.heading("Правила проекта");
                    ui.weak(project.display().to_string());
                    let saving = self
                        .pending
                        .as_ref()
                        .is_some_and(|p| matches!(p.effect, Effect::SaveRules));
                    let blocked = self.mutation_pending() || self.git_busy;
                    if let Some(request) = self.rules.show(ui, &project, saving, blocked) {
                        self.request(request, send);
                    }
                });
                return;
            }
            let selected = self
                .tasks
                .iter()
                .position(|task| Some(task.id) == self.focused)
                .or_else(|| self.tasks.iter().position(|task| !task.archived));
            if self.page != Page::New
                && let (Some(index), Some((_, _, backend))) = (selected, self.scope.as_ref())
            {
                let tasks = std::mem::take(&mut self.tasks);
                if let Some(task) = tasks.get(index) {
                    self.focused = Some(task.id);
                    self.card(ui, task, *backend, snapshot, send);
                }
                self.tasks = tasks;
            } else if self.page != Page::New
                && self.focused.is_some()
                && self.pending.is_some()
                && self.tasks.is_empty()
            {
                ui.spinner();
            } else {
                egui::ScrollArea::vertical().id_salt("new_task_page").show(ui, |ui| {
                    self.new_task(ui, snapshot, send);
                });
            }
        });
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
            let current = (task.id, task.revision);
            if self.confirm_delete == Some(current) {
                ui.label(format!(
                    "Удалить задачу #{} «{}» вместе с перепиской и вложениями?",
                    task.id, task.title
                ));
                ui.horizontal(|ui| {
                    if ui.button("Отмена").clicked() {
                        self.confirm_delete = None;
                    }
                    if ui.button("Удалить безвозвратно").clicked() {
                        self.confirm_delete = None;
                        self.request(
                            Request::Delete { id: task.id, revision: task.revision },
                            send,
                        );
                    }
                });
            } else if ui
                .add_enabled(task.queue.is_none(), egui::Button::new("Удалить задачу"))
                .on_disabled_hover_text("Сначала снимите задачу с очереди")
                .clicked()
            {
                self.confirm_delete = Some(current);
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
        ui.strong("Агент и память");
        ui.add_enabled_ui(editable && task.queue.is_none() && !task.active(), |ui| {
            let enabled = super::memory::scope(ui, "Память задачи", task.memory);
            if enabled != task.memory {
                self.request(
                    Request::Memory { id: task.id, revision: task.revision, enabled },
                    send,
                );
            }
        });
        if !self.local_context() && backend.name() != task.backend {
            ui.small(format!("Агент выбранной темы · {}", backend.name()));
        }
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
            "Сначала восстановите задачу в разделе «Управление задачей»."
        } else if matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred) {
            "Сначала выберите «Запланирована» в разделе «Управление задачей»."
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
    ) {
        let first = task.runs.is_empty();
        if let Some(q) = &task.queue {
            ui.label(q.error.clone().unwrap_or_else(|| format!("В очереди · №{}", q.sequence)));
            if ui.button("Снять с очереди").clicked() {
                self.request(Request::Unqueue { id: task.id, revision: task.revision }, send);
            }
            return;
        }
        if !task.active()
            && !task.archived
            && matches!(task.status, TaskStatus::Planned | TaskStatus::NeedsReview)
        {
            let valid_step =
                !next.trim().is_empty() && next.len() <= 16384 && (!first || next == task.prompt);
            let ready = valid_step && !self.rules.dirty() && !self.mutation_pending();
            if first && next != task.prompt {
                ui.small("Сохраните поручение в чате перед постановкой в очередь.");
            } else if !valid_step {
                ui.small("Чтобы поставить продолжение в очередь, введите следующий шаг в чате (до 16 КиБ).");
            }
            ui.horizontal_wrapped(|ui| {
                for (isolated, label) in
                    [(false, "В очередь"), (true, "В очередь в отдельном worktree")]
                {
                    if (!first && isolated != task.isolated)
                        || (isolated && self.rules.saved.git.is_some())
                    {
                        continue;
                    }
                    if ui.add_enabled(ready, egui::Button::new(label)).clicked() {
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
    }
    fn launch_block(
        &self,
        task: &Task,
        backend: BackendKind,
        available: &Availability,
        next: &str,
    ) -> Option<&'static str> {
        let first = task.runs.is_empty();
        if task.runs.last().is_some_and(|run| run.state == hub_core::tasks::RunState::Running) {
            Some("Агент работает. Следующее сообщение можно отправить после завершения.")
        } else if self.local_context() && self.local_block == LocalBlock::Job {
            Some(
                "Локальная работа выполняется. Дождитесь завершения или остановите её в параметрах задачи.",
            )
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
            Some("Восстановите задачу в параметрах задачи.")
        } else if matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred) {
            Some("Верните задачу в работу в параметрах задачи.")
        } else if self.assignees.get(&task.id).is_some_and(|(revision, agent)| {
            *revision == task.revision && agent.name() != task.backend
        }) {
            Some("Нажмите «Назначить» в параметрах задачи, чтобы сохранить выбранного агента.")
        } else if !self.local_context() && task.backend != backend.name() {
            Some("Выберите в параметрах тему проекта с назначенным агентом.")
        } else if next.trim().is_empty() {
            Some(if first {
                "Введите поручение агенту."
            } else {
                "Введите сообщение агенту."
            })
        } else if next.len() > 16384 {
            Some("Сократите сообщение до 16 КиБ.")
        } else if !available.launch {
            Some("Тема занята. Дождитесь завершения текущей работы или остановите её.")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::domain::{AbsolutePath, ChatId, ThreadId, TopicSession};
    use hub_core::tasks::RunState;
    use hub_telegram::hub::{TopicState, TopicView};
    fn ts() -> jiff::Timestamp {
        jiff::Timestamp::from_second(1_700_000_000).unwrap()
    }
    fn test_project() -> PathBuf {
        hub_telegram::paths::workspace_root(&std::env::temp_dir()).unwrap().as_path().to_owned()
    }

    fn topic(key: TopicKey) -> TopicView {
        TopicView {
            key,
            title: Some("Test".into()),
            session: TopicSession::fresh(
                BackendKind::Qwen,
                AbsolutePath::new(test_project()).unwrap(),
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
    fn sized_frame(
        ctx: &egui::Context,
        view: &mut TasksView,
        snapshot: &Snapshot,
        screen: egui::Rect,
        events: Vec<egui::Event>,
        commands: &mut Vec<Command>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput { screen_rect: Some(screen), events, ..Default::default() },
            |ui| {
                view.show(ui, snapshot, &mut |command| commands.push(command));
            },
        );
        output.textures_delta.clear();
        output
    }
    fn input_frame(
        ctx: &egui::Context,
        view: &mut TasksView,
        snapshot: &Snapshot,
        events: Vec<egui::Event>,
        commands: &mut Vec<Command>,
    ) -> egui::FullOutput {
        sized_frame(
            ctx,
            view,
            snapshot,
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 900.0)),
            events,
            commands,
        )
    }
    fn labels(output: &egui::FullOutput) -> Vec<&str> {
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect()
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
    fn review_fixture() -> (hub_core::tasks::TaskBook, u64, Snapshot, TasksView) {
        let project = test_project();
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(project.clone(), "qwen", "Проверка чата", "Исходный вопрос").unwrap();
        book.begin(id, 1, None, ts()).unwrap();
        book.record(id, 1, "Ответ агента").unwrap();
        book.finish(id, 1, RunState::Completed, ts()).unwrap();
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key)],
            task_index: Ok(book.tasks().iter().map(crate::desktop_tasks::TaskRow::from).collect()),
            ..Default::default()
        };
        let view = TasksView {
            scope: Some((Some(key), project, BackendKind::Qwen)),
            selected: Some(key),
            tasks: book.tasks().to_vec(),
            focused: Some(id),
            refreshed: Some(Instant::now()),
            ..Default::default()
        };
        (book, id, snapshot, view)
    }

    fn looks_like_clock(text: &str) -> bool {
        let bytes = text.as_bytes();
        bytes.len() == 5
            && bytes[0].is_ascii_digit()
            && bytes[1].is_ascii_digit()
            && bytes[2] == b':'
            && bytes[3].is_ascii_digit()
            && bytes[4].is_ascii_digit()
    }

    fn has_clock(labels: &[&str]) -> bool {
        labels.iter().any(|label| looks_like_clock(label))
    }

    fn text_rect_containing(output: &egui::FullOutput, needle: &str) -> Option<egui::Rect> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.text().contains(needle) => {
                Some(text.galley.rect.translate(text.pos.to_vec2()))
            }
            _ => None,
        })
    }

    fn clock_rects(output: &egui::FullOutput) -> Vec<egui::Rect> {
        let mut clocks: Vec<(f32, egui::Rect)> = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if looks_like_clock(text.galley.text()) => {
                    Some((text.pos.y, text.galley.rect.translate(text.pos.to_vec2())))
                }
                _ => None,
            })
            .collect();
        clocks.sort_by(|a, b| a.0.total_cmp(&b.0));
        clocks.into_iter().map(|(_, rect)| rect).collect()
    }

    fn gradient_fades(output: &egui::FullOutput) -> Vec<(egui::Rect, [egui::Color32; 2])> {
        output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Mesh(mesh) if mesh.vertices.len() >= 4 => {
                    let bounds = egui::Rect::from_points(
                        &mesh.vertices.iter().map(|vertex| vertex.pos).collect::<Vec<_>>(),
                    );
                    let colors = [mesh.vertices[0].color, mesh.vertices[2].color];
                    (!bounds.is_negative()).then_some((bounds, colors))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn clamped_agent_answer_fades_and_expanding_removes_the_fade() {
        let (_, _, snapshot, mut view) = review_fixture();
        view.tasks[0].runs[0].result = (1..=12)
            .map(|n| format!("Строка {n} — развёрнутый абзац ответа агента для проверки затухания"))
            .collect::<Vec<_>>()
            .join("\n");
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        let out = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        let surface = ctx.style_of(ctx.theme()).visuals.panel_fill;
        let body = text_rect_containing(&out, "Строка 8").unwrap();
        assert!(body.width() > 200.0, "long lines make the clamped body wide");
        let fades: Vec<(egui::Rect, [egui::Color32; 2])> = gradient_fades(&out)
            .into_iter()
            .filter(|(bounds, _)| (bounds.bottom() - body.bottom()).abs() <= 2.0)
            .collect();
        assert_eq!(fades.len(), 1, "one gradient band over the clamped body bottom");
        assert_eq!(
            fades[0].1,
            [egui::Color32::TRANSPARENT, surface],
            "transparent fades to surface"
        );
        assert!(fades[0].0.height() <= 27.0, "the band stays inside the mockup's 26px");
        click_text(&ctx, &mut view, &snapshot, "Развернуть", &mut commands);
        let out = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(gradient_fades(&out).is_empty(), "an expanded answer has no fade band");
    }

    #[test]
    fn bubble_time_sits_at_the_right_edge_of_its_header() {
        let (_, _, snapshot, mut view) = review_fixture();
        view.tasks[0].runs[0].started_at = Some(ts());
        view.tasks[0].runs[0].finished_at = Some(ts());
        view.tasks[0].runs[0].result = (1..=12)
            .map(|n| format!("Строка {n} — развёрнутый абзац ответа агента для проверки затухания"))
            .collect::<Vec<_>>()
            .join("\n");
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        let out = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        let clocks = clock_rects(&out);
        assert_eq!(clocks.len(), 2, "one clock per bubble header");
        let step = text_rect_containing(&out, "Исходный вопрос").unwrap();
        let body = text_rect_containing(&out, "Строка 8").unwrap();
        assert!(
            (clocks[0].max.x - step.max.x).abs() <= 2.0,
            "the user clock sits flush with the step text edge: {clocks:?} {step:?}"
        );
        assert!(
            (clocks[1].max.x - body.max.x).abs() <= 2.0,
            "the agent clock sits flush with the answer body edge: {clocks:?} {body:?}"
        );
    }

    #[test]
    fn long_agent_answer_collapses_and_expands_by_click() {
        let (_, _, snapshot, mut view) = review_fixture();
        view.tasks[0].runs[0].result =
            (1..=12).map(|n| format!("Строка {n}")).collect::<Vec<_>>().join("\n");
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        let out = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(labels(&out).contains(&"Развернуть"), "long answers start collapsed");
        assert!(!labels(&out).contains(&"Свернуть"));
        assert!(labels(&out).iter().any(|label| label.contains("Строка 8")));
        assert!(!labels(&out).iter().any(|label| label.contains("Строка 12")));
        click_text(&ctx, &mut view, &snapshot, "Развернуть", &mut commands);
        let out = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(labels(&out).contains(&"Свернуть"), "clicking expands and offers to collapse");
        assert!(labels(&out).iter().any(|label| label.contains("Строка 12")));
        click_text(&ctx, &mut view, &snapshot, "Свернуть", &mut commands);
        let out = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(labels(&out).contains(&"Развернуть"), "collapsing returns the clamped body");
        assert!(!labels(&out).iter().any(|label| label.contains("Строка 12")));
    }

    #[test]
    fn bubbles_show_role_and_time_only_when_known() {
        let (_, _, snapshot, mut view) = review_fixture();
        view.tasks[0].runs[0].started_at = None;
        view.tasks[0].runs[0].finished_at = None;
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        let out = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        let texts = labels(&out);
        assert!(texts.contains(&"Вы"), "user bubble header names the role");
        assert!(texts.contains(&"qwen"), "agent bubble header names the backend");
        assert!(!has_clock(&texts), "unknown times must not render");
        drop(texts);
        view.tasks[0].runs[0].started_at = Some(ts());
        view.tasks[0].runs[0].finished_at = Some(ts());
        let out = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        let texts = labels(&out);
        assert!(has_clock(&texts), "known dates render local HH:MM");
    }

    #[test]
    fn result_link_only_on_latest_reviewable_run() {
        let (mut book, id, snapshot, mut view) = review_fixture();
        let revision = view.tasks[0].revision;
        let second = book.begin(id, revision, Some("Второй шаг"), ts()).unwrap();
        book.record(id, second, "Ответ второго запуска").unwrap();
        book.finish(id, second, RunState::Completed, ts()).unwrap();
        view.tasks = book.tasks().to_vec();
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        let out = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        let texts = labels(&out);
        assert_eq!(
            texts.iter().filter(|label| **label == "Результат →").count(),
            1,
            "only the latest reviewable run links to its result"
        );
    }

    #[test]
    fn chat_survives_compact_window_with_inline_result() {
        let (_, _, snapshot, mut view) = review_fixture();
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 300.0));
        let out = sized_frame(&ctx, &mut view, &snapshot, screen, vec![], &mut commands);
        let texts = labels(&out);
        assert!(texts.contains(&"Открыть результат"), "inline result mode survives compact mode");
        assert!(
            texts.contains(&"Вы") && texts.contains(&"qwen"),
            "bubble headers survive compact mode"
        );
    }

    #[test]
    fn composer_field_grows_with_text_up_to_eight_lines() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let (_, _, snapshot, mut view) = review_fixture();
        let mut commands = Vec::new();
        let style = ctx.style_of(ctx.theme());
        let panel_fill = style.visuals.panel_fill;
        let body_font = style.text_styles.get(&egui::TextStyle::Body).cloned().expect("body font");
        let composer_height = |view: &mut TasksView, commands: &mut Vec<Command>| {
            let output = input_frame(&ctx, view, &snapshot, vec![], commands);
            let mut frames = output.shapes.iter().filter_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if rect.fill == panel_fill
                        && (rect.stroke.width - 1.0).abs() < 0.01
                        && rect.corner_radius == egui::CornerRadius::same(12) =>
                {
                    Some(rect.rect)
                }
                _ => None,
            });
            let frame = frames.next().expect("composer frame");
            assert!(frames.next().is_none(), "exactly one composer frame");
            frame.height()
        };
        click_text(&ctx, &mut view, &snapshot, "Сообщение агенту", &mut commands);
        let one = composer_height(&mut view, &mut commands);
        let body_line = ctx.fonts_mut(|fonts| fonts.row_height(&body_font));
        input_frame(
            &ctx,
            &mut view,
            &snapshot,
            vec![egui::Event::Text("первая\nвторая\nтретья".into())],
            &mut commands,
        );
        let three = composer_height(&mut view, &mut commands);
        assert!(three > one, "the field grows with the text: {one} -> {three}");
        input_frame(
            &ctx,
            &mut view,
            &snapshot,
            vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                },
                egui::Event::Text(
                    (1..=20).map(|n| format!("строка {n}")).collect::<Vec<_>>().join("\n"),
                ),
            ],
            &mut commands,
        );
        let many = composer_height(&mut view, &mut commands);
        {
            for n in [1usize, 2, 3, 4, 8, 9, 20] {
                input_frame(
                    &ctx,
                    &mut view,
                    &snapshot,
                    vec![
                        egui::Event::Key {
                            key: egui::Key::A,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::COMMAND,
                        },
                        egui::Event::Text(
                            (1..=n).map(|k| format!("с{k}")).collect::<Vec<_>>().join("\n"),
                        ),
                    ],
                    &mut commands,
                );
                eprintln!("rows={n} height={}", composer_height(&mut view, &mut commands));
            }
            eprintln!("one={one} three={three} many={many} body={body_line}");
        }
        assert!(many > three, "eight lines is taller than three");
        assert!(
            many <= body_line * 8.0 + 22.0 + 2.0,
            "the composer stops growing at eight lines: {many} > {}",
            body_line * 8.0 + 22.0 + 2.0
        );
    }

    #[test]
    fn review_accepts_and_returns_to_the_inbox_only_after_confirmation() {
        let (mut book, id, snapshot, mut view) = review_fixture();
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        view.result_view.select(result::Tab::Summary, Some(id));
        view.review_from_inbox = true;
        for succeeds in [false, true] {
            click_text(&ctx, &mut view, &snapshot, "Принять результат", &mut commands);
            let Command::Tasks {
                request: Request::Status { id: requested, revision, status: TaskStatus::Accepted },
                reply,
                ..
            } = commands.pop().unwrap()
            else {
                panic!("accept")
            };
            assert_eq!(requested, id);
            assert!(!view.take_inbox_return());
            if succeeds {
                book.set_status(id, revision, TaskStatus::Accepted).unwrap();
                reply
                    .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..Default::default() }))
                    .unwrap();
            } else {
                reply.send(Err("Не удалось сохранить результат".into())).unwrap();
            }
            view.update(&mut |c| commands.push(c));
            assert_eq!(view.take_inbox_return(), succeeds);
            assert!(!view.take_inbox_return(), "navigation is a one-shot event");
            if !succeeds {
                assert_eq!(view.tasks[0].status, TaskStatus::NeedsReview);
                assert!(view.error.is_some());
                assert_eq!(view.result_view.tab(&view.tasks[0]), result::Tab::Summary);
            }
        }
    }

    #[test]
    fn review_feedback_uses_the_chat_draft_and_waits_for_the_checked_start() {
        let (mut book, id, snapshot, mut view) = review_fixture();
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        view.result_view.select(result::Tab::Summary, Some(id));
        view.review_from_inbox = true;
        click_text(&ctx, &mut view, &snapshot, "На доработку", &mut commands);
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        input_frame(
            &ctx,
            &mut view,
            &snapshot,
            vec![egui::Event::Text("Проверьте итог".into())],
            &mut commands,
        );
        assert_eq!(view.next.get(&id).map(String::as_str), Some("Проверьте итог"));
        for succeeds in [false, true] {
            click_text(&ctx, &mut view, &snapshot, "Отправить", &mut commands);
            let Command::Tasks {
                request: Request::StartChecked { id: requested, revision, next, .. },
                reply,
                ..
            } = commands.pop().unwrap()
            else {
                panic!("feedback")
            };
            assert_eq!((requested, next.as_deref()), (id, Some("Проверьте итог")));
            assert!(!view.take_inbox_return());
            if succeeds {
                book.begin(id, revision, next.as_deref(), ts()).unwrap();
                reply
                    .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..Default::default() }))
                    .unwrap();
            } else {
                reply.send(Err("Тема занята".into())).unwrap();
            }
            view.update(&mut |c| commands.push(c));
            assert_eq!(view.take_inbox_return(), succeeds);
            if !succeeds {
                assert_eq!(view.next.get(&id), next.as_ref());
            }
        }
    }

    #[test]
    fn leaving_review_cancels_navigation_on_a_late_acknowledgement() {
        let (mut book, id, snapshot, mut view) = review_fixture();
        let ctx = egui::Context::default();
        let mut commands = vec![];
        view.result_view.select(result::Tab::Summary, Some(id));
        view.review_from_inbox = true;
        click_text(&ctx, &mut view, &snapshot, "Принять результат", &mut commands);
        let Command::Tasks { request: Request::Status { revision, .. }, reply, .. } =
            commands.pop().unwrap()
        else {
            panic!("accept")
        };
        view.leave_review();
        book.set_status(id, revision, TaskStatus::Accepted).unwrap();
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..Default::default() }))
            .unwrap();
        view.update(&mut |c| commands.push(c));
        assert!(!view.take_inbox_return());
    }

    #[test]
    fn new_task_derives_a_valid_title_and_starts_in_its_original_topic_after_switching() {
        let (mut book, old, snapshot, mut view) = review_fixture();
        let other = tempfile::tempdir().unwrap();
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        view.page = Page::New;
        view.prompt = format!("{}\nПроверьте проект", "🌿".repeat(70));
        view.rules.accept(hub_core::tasks::ProjectRules { revision: 5, ..Default::default() });
        click_text(&ctx, &mut view, &snapshot, "Начать задачу", &mut commands);
        let Command::Tasks {
            key,
            project,
            backend,
            request: Request::Create { title, prompt },
            reply,
        } = commands.pop().unwrap()
        else {
            panic!("create")
        };
        assert_eq!(title, "🌿".repeat(64));
        assert_eq!(prompt, view.prompt);
        let created = book.create(project.clone(), backend.name(), &title, &prompt).unwrap();
        assert_ne!(created, old);
        view.navigate(other.path(), None, result::Tab::Chat);
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        reply
            .send(Ok(TaskSnapshot {
                tasks: vec![book.get(created).unwrap().clone()],
                ..Default::default()
            }))
            .unwrap();
        view.update(&mut |c| commands.push(c));
        let Command::Tasks {
            key: sent,
            project: cwd,
            request: Request::StartChecked { id, revision, rules_revision, next: None },
            ..
        } = commands.pop().unwrap()
        else {
            panic!("checked start")
        };
        assert_eq!((sent, cwd, id, revision, rules_revision), (key, project, created, 1, 5));
        assert_ne!(view.focused, Some(created));
        assert!(view.tasks.is_empty());
        assert!(commands.is_empty());
    }

    #[test]
    fn drafts_return_to_their_own_task_after_project_switches() {
        let (book, id, snapshot, mut view) = review_fixture();
        let other = tempfile::tempdir().unwrap();
        let project = view.tasks[0].project.clone();
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        click_text(&ctx, &mut view, &snapshot, "Сообщение агенту", &mut commands);
        input_frame(
            &ctx,
            &mut view,
            &snapshot,
            vec![egui::Event::Text("Черновик A".into())],
            &mut commands,
        );
        view.navigate(other.path(), Some(id), result::Tab::Chat);
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        let Command::LocalTasks { project: other_path, reply, .. } = commands.pop().unwrap() else {
            panic!("second project")
        };
        let mut other_book = hub_core::tasks::TaskBook::default();
        let other_id = other_book.create(other_path, "qwen", "Соседний проект", "Шаг").unwrap();
        other_book.begin(other_id, 1, None, ts()).unwrap();
        other_book.finish(other_id, 1, RunState::Completed, ts()).unwrap();
        assert_eq!(id, other_id, "distinct projects with the same id must remain isolated");
        reply
            .send(Ok(TaskSnapshot { tasks: other_book.tasks().to_vec(), ..Default::default() }))
            .unwrap();
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(view.next[&other_id].is_empty());
        view.next.insert(other_id, "Черновик B".into());
        view.navigate(&project, Some(id), result::Tab::Chat);
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        let Command::LocalTasks { reply, .. } = commands.pop().unwrap() else {
            panic!("first project")
        };
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..Default::default() }))
            .unwrap();
        let output = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert_eq!(view.next[&id], "Черновик A");
        assert!(labels(&output).contains(&"Черновик A"));
        assert!(!labels(&output).contains(&"Черновик B"));
    }

    fn saved_prompt_fixture() -> (hub_core::tasks::TaskBook, u64, Snapshot, TasksView) {
        let (_, _, snapshot, mut view) = review_fixture();
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(test_project(), "qwen", "Тест", "Исходное поручение").unwrap();
        view.tasks = book.tasks().to_vec();
        (book, id, snapshot, view)
    }

    fn replace_prompt(
        ctx: &egui::Context,
        view: &mut TasksView,
        snapshot: &Snapshot,
        prompt: &str,
        commands: &mut Vec<Command>,
    ) {
        click_text(ctx, view, snapshot, "Исходное поручение", commands);
        input_frame(
            ctx,
            view,
            snapshot,
            vec![
                egui::Event::Key {
                    key: egui::Key::A,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                },
                egui::Event::Text(prompt.into()),
            ],
            commands,
        );
    }

    #[test]
    fn first_prompt_saves_without_starting_and_receives_its_own_revision() {
        let (mut book, id, snapshot, mut view) = saved_prompt_fixture();
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        replace_prompt(&ctx, &mut view, &snapshot, "Новое поручение", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Сохранить", &mut commands);
        let Command::Tasks {
            request: Request::EditPrompt { id: sent, revision, prompt },
            reply,
            ..
        } = commands.pop().unwrap()
        else {
            panic!("save prompt")
        };
        assert_eq!((sent, revision, prompt.as_str()), (id, 1, "Новое поручение"));
        book.edit_prompt(id, revision, &prompt).unwrap();
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..Default::default() }))
            .unwrap();
        let output = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(labels(&output).contains(&"Новое поручение"));
        assert!(!labels(&output).contains(&"Сохранить"));
        assert!(commands.is_empty() && view.tasks[0].runs.is_empty());
        click_text(&ctx, &mut view, &snapshot, "Запустить", &mut commands);
        assert!(matches!(
            commands.pop(),
            Some(Command::Tasks {
                request: Request::StartChecked { revision: 2, next: None, .. },
                ..
            })
        ));
    }

    #[test]
    fn stale_first_prompt_keeps_both_versions_and_launches_only_the_confirmed_edit() {
        let (mut book, id, snapshot, mut view) = saved_prompt_fixture();
        let ctx = egui::Context::default();
        let mut commands = Vec::new();
        replace_prompt(&ctx, &mut view, &snapshot, "Мой текст", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Запустить", &mut commands);
        let Command::Tasks {
            request: Request::EditPrompt { id: sent, revision, prompt },
            reply,
            ..
        } = commands.pop().unwrap()
        else {
            panic!("edited start")
        };
        book.edit_prompt(id, revision, "Текст другой сессии").unwrap();
        reply.send(Err(book.edit_prompt(sent, revision, &prompt).unwrap_err())).unwrap();
        view.update(&mut |c| commands.push(c));
        assert!(commands.is_empty(), "a failed save never starts a run");
        view.request(Request::List, &mut |c| commands.push(c));
        let Command::Tasks { reply, .. } = commands.pop().unwrap() else { panic!("refresh") };
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..Default::default() }))
            .unwrap();
        let output = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(
            labels(&output).contains(&"Мой текст")
                && labels(&output).contains(&"Текст другой сессии")
        );
        click_text(&ctx, &mut view, &snapshot, "Запустить", &mut commands);
        assert!(commands.is_empty(), "conflicts need an explicit choice");
        click_text(&ctx, &mut view, &snapshot, "Использовать мой текст", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Запустить", &mut commands);
        let Command::Tasks {
            key,
            project,
            request: Request::EditPrompt { revision: 2, prompt, .. },
            reply,
            ..
        } = commands.pop().unwrap()
        else {
            panic!("resolved edit")
        };
        let other = tempfile::tempdir().unwrap();
        view.navigate(other.path(), None, result::Tab::Chat);
        input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        book.edit_prompt(id, 2, &prompt).unwrap();
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..Default::default() }))
            .unwrap();
        view.update(&mut |c| commands.push(c));
        assert!(
            matches!(commands.pop(),Some(Command::Tasks {key:actual,project:path,request:Request::StartChecked{id:sent,revision:3,next:None,..},..}) if actual==key && path==project && sent==id)
        );
        assert!(view.tasks.is_empty(), "the new project's chat must not receive the old task");
    }

    #[test]
    fn chat_shows_the_effective_memory_source_for_the_selected_target() {
        let (_, _, mut snapshot, mut view) = review_fixture();
        let ctx = egui::Context::default();
        snapshot.topics[0].session.preferences.memory = Some(false);
        view.tasks[0].memory = Some(true);
        let mut commands = Vec::new();
        let out = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(labels(&out).contains(&"Память включена · задача"));
        view.tasks[0].memory = None;
        let out = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(labels(&out).contains(&"Память выключена · тема Telegram"));
        view.local_mode = Some(true);
        view.local_project.choose(&test_project());
        let out = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(labels(&out).contains(&"Память включена · общие настройки"));
    }

    #[test]
    fn transcript_stays_above_pinned_controls_in_small_windows() {
        for (width, height) in [(960.0, 640.0), (640.0, 360.0)] {
            let (_, _, snapshot, mut view) = review_fixture();
            let ctx = egui::Context::default();
            crate::gui::configure_theme(&ctx);
            view.next.insert(1, "Черновик сообщения".into());
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
            let render = |view: &mut TasksView| {
                let mut out = ctx.run_ui(
                    egui::RawInput { screen_rect: Some(screen), ..Default::default() },
                    |ui| {
                        egui::Panel::top("app_navigation").exact_size(48.0).show(ui, |_| {});
                        view.project_navigation(ui, &snapshot, true);
                        view.show(ui, &snapshot, &mut |_| {});
                    },
                );
                out.textures_delta.clear();
                out
            };
            let _ = render(&mut view);
            let baseline = render(&mut view);
            let bounds = |out: &egui::FullOutput, label: &str| {
                let (rect, clip) = out
                    .shapes
                    .iter()
                    .rev()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text() == label => {
                            Some((text.galley.rect.translate(text.pos.to_vec2()), shape.clip_rect))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| panic!("missing {label} at {width}×{height}"));
                assert!(
                    clip.expand(1.0).contains_rect(rect),
                    "clipped {label} at {width}×{height}: {rect:?} in {clip:?}"
                );
                rect
            };
            let send = bounds(&baseline, "Отправить");
            view.tasks[0].runs[0].result = "Длинная история чата\n".repeat(90);
            let _ = render(&mut view);
            let output = render(&mut view);
            assert_eq!(bounds(&output, "Отправить"), send);
            let result = bounds(&output, "Открыть результат");
            let draft = bounds(&output, "Черновик сообщения");
            assert!(send.bottom() <= screen.bottom() && send.bottom() > height - 80.0);
            let transcript = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == "Результат →" => {
                        Some(shape.clip_rect)
                    }
                    _ => None,
                })
                .unwrap();
            assert!(
                transcript.bottom() <= result.top().min(draft.top()),
                "history overlaps controls at {width}×{height}"
            );
            let crumb = format!("{}  /  #1", test_project().file_name().unwrap().to_string_lossy());
            assert!(
                (bounds(&output, "Проверка чата").left() - bounds(&output, &crumb).left()).abs()
                    < 2.0
            );
        }
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
        let rows = book.tasks().iter().map(TaskRow::from).collect();
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
        let tasks_x = x("Второе");
        let crumb =
            format!("{}  /  #{}", project.as_path().file_name().unwrap().to_string_lossy(), first);
        assert!(project_x <= tasks_x && tasks_x < x(&crumb));
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
        view.page = Page::New;
        click_text(&ctx, &mut view, &snapshot, "Сохранить черновик", &mut commands);
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
        click_text(&ctx, &mut view, &snapshot, "Запустить", &mut commands);
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
        let project = test_project();
        let mut book = TaskBook::default();
        let id = book.create(project.clone(), "qwen", "Task", "Read").unwrap();
        let mut rules = book.rules(&project);
        rules.git = Some(GitPolicy { remote: "origin".into(), base: "Dev".into() });
        book.configure_rules(project.clone(), 0, rules).unwrap();
        book.prepare_git(id, 1, "task/first").unwrap();
        book.finish_git(id, 2, Ok("a".repeat(40))).unwrap();
        let run = book.begin(id, 3, None, ts()).unwrap();
        book.finish(id, run, RunState::Completed, ts()).unwrap();
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
        view.update(&mut |c| commands.push(c));
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
        let project = test_project();
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
        click_text(&ctx, &mut view, &snapshot, "Параметры", &mut commands);
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
        let project = test_project();
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
        click_text(&ctx, &mut view, &snapshot, "Запустить", &mut commands);
        assert!(commands.is_empty());
        click_text(&ctx, &mut view, &snapshot, "Параметры", &mut commands);
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
        click_text(&ctx, &mut view, &snapshot, "В чат", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Запустить", &mut commands);
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
        other_topic.session.cwd = AbsolutePath::new(test_project().join("other-project")).unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), test_project(), BackendKind::Qwen)),
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
        view.page = Page::Rules;
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
        book.create(test_project(), "qwen", "Task", "Read").unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), test_project(), BackendKind::Qwen)),
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
        click_text(&ctx, &mut view, &snapshot, "Параметры", &mut commands);
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
        click_text(&ctx, &mut view, &snapshot, "Назад", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "В чат", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Запустить", &mut commands);
        assert!(commands.is_empty());
        click_text(&ctx, &mut view, &snapshot, "Параметры", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Правила проекта", &mut commands);
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
        click_text(&ctx, &mut view, &snapshot, "Назад", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "В чат", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Запустить", &mut commands);
        assert!(matches!(
            commands.pop(),
            Some(Command::Tasks { request: Request::StartChecked { rules_revision: 2, .. }, .. })
        ));
    }
    #[test]
    fn running_task_shows_assignment_control_but_does_not_allow_mutation() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(test_project(), "qwen", "Task", "Read").unwrap();
        book.begin(id, 1, None, ts()).unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), test_project(), BackendKind::Qwen)),
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
        click_text(&ctx, &mut view, &snapshot, "Параметры", &mut commands);
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
        let id = book.create(test_project(), "qwen", "Task", "Read").unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), test_project(), BackendKind::Qwen)),
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
        click_text(&ctx, &mut view, &snapshot, "Параметры", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "qwen", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "codex", &mut commands);
        assert!(commands.is_empty(), "choosing an agent is only a draft");
        click_text(&ctx, &mut view, &snapshot, "В чат", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Запустить", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Параметры", &mut commands);
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
        view.update(&mut |c| commands.push(c));
        assert_eq!(view.tasks[0].backend, "codex");
        assert_eq!(view.scope.as_ref().unwrap().2, BackendKind::Qwen);
        click_text(&ctx, &mut view, &snapshot, "В чат", &mut commands);
        let output = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Выберите в параметрах тему проекта с назначенным агентом.")));
        assert!(commands.is_empty());
    }
    #[test]
    fn task_chat_sends_focused_draft_and_displays_the_active_reply_in_order() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let project = test_project();
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(project.clone(), "qwen", "Task", "Первый вопрос").unwrap();
        book.begin(id, 1, None, ts()).unwrap();
        book.record(id, 1, "Первый ответ").unwrap();
        book.finish(id, 1, RunState::Completed, ts()).unwrap();
        let revision = book.get(id).unwrap().revision;
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), project, BackendKind::Qwen)),
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
        let draft = "Второй вопрос";
        view.next.insert(id, draft.into());
        click_text(&ctx, &mut view, &snapshot, draft, &mut commands);
        input_frame(
            &ctx,
            &mut view,
            &snapshot,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            }],
            &mut commands,
        );
        let Command::Tasks {
            request: Request::StartChecked { next, revision: visible, .. },
            reply,
            ..
        } = commands.pop().expect("keyboard submission")
        else {
            panic!("checked start")
        };
        assert_eq!(visible, revision);
        assert_eq!(next.as_deref(), Some(draft));
        reply.send(Err("Сервис занят".into())).unwrap();
        view.update(&mut |c| commands.push(c));
        assert_eq!(view.next[&id], draft);
        click_text(&ctx, &mut view, &snapshot, "Отправить", &mut commands);
        let Command::Tasks { reply, .. } = commands.pop().unwrap() else { panic!("retry") };
        let run = book.begin(id, revision, Some(draft), ts()).unwrap();
        book.record(id, run, "Промежуточный ответ").unwrap();
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..Default::default() }))
            .unwrap();
        let output = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        let positions: Vec<_> = ["Первый вопрос", "Первый ответ", draft, "Промежуточный ответ"]
            .iter()
            .map(|label| {
                output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.text() == *label => Some(text.pos.y),
                        _ => None,
                    })
                    .expect("visible chat message")
            })
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(view.next[&id].is_empty());
        assert!(view.tasks[0].active());
        view.next.insert(id, "Черновик во время работы".into());
        view.request(Request::List, &mut |command| commands.push(command));
        let Command::Tasks { reply, .. } = commands.pop().unwrap() else { panic!("refresh") };
        book.finish(id, run, RunState::Completed, ts()).unwrap();
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..Default::default() }))
            .unwrap();
        view.update(&mut |c| commands.push(c));
        assert_eq!(view.next[&id], "Черновик во время работы");
    }

    #[test]
    fn task_card_orders_executor_step_preview_and_secondary_actions() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(test_project(), "codex", "Task", "Read").unwrap();
        book.begin(id, 1, None, ts()).unwrap();
        book.record(id, 1, "Готовый результат").unwrap();
        book.finish(id, 1, RunState::Completed, ts()).unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), test_project(), BackendKind::Qwen)),
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

        assert_eq!(
            labels(&output).iter().filter(|label| **label == "Открыть результат").count(),
            1
        );
        assert!(!labels(&output).contains(&"Управление задачей"));
        click_text(&ctx, &mut view, &snapshot, "Отправить", &mut commands);
        assert!(commands.is_empty(), "unassigned task must not launch");
        click_text(&ctx, &mut view, &snapshot, "Параметры", &mut commands);
        let output = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        for section in ["Агент и память", "Контекст следующего запуска", "Управление задачей"]
        {
            assert!(labels(&output).contains(&section), "missing {section}");
        }
        click_text(&ctx, &mut view, &snapshot, "В чат", &mut commands);
        click_text(&ctx, &mut view, &snapshot, "Открыть результат", &mut commands);
        let output = input_frame(&ctx, &mut view, &snapshot, vec![], &mut commands);
        for section in [
            "Готовый результат",
            "Изменения",
            "Проверки",
            "История попыток (1)",
            "Принять результат",
            "На доработку",
        ] {
            assert!(labels(&output).contains(&section), "missing {section}");
        }
    }
    #[test]
    fn changing_task_memory_preserves_the_composer_draft() {
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(test_project(), "codex", "Task", "Read").unwrap();
        let before = book.get(id).unwrap().clone();
        book.set_memory(id, before.revision, Some(true)).unwrap();
        assert!(only_execution_preferences_changed(&before, book.get(id).unwrap()));
        book.set_status(id, before.revision + 1, TaskStatus::Deferred).unwrap();
        assert!(!only_execution_preferences_changed(&before, book.get(id).unwrap()));
    }
    #[test]
    fn transfer_preserves_typed_step_across_same_project_topic_and_assignment() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let receiving = TopicKey { chat: key.chat, thread: ThreadId(8) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(test_project(), "codex", "Task", "Read").unwrap();
        book.begin(id, 1, None, ts()).unwrap();
        book.record(id, 1, "Previous result").unwrap();
        book.finish(id, 1, RunState::Completed, ts()).unwrap();
        let mut raw = serde_json::to_value(&book).unwrap();
        raw["tasks"][0]["runs"][0].as_object_mut().unwrap().remove("backend");
        let mut book =
            hub_core::tasks::TaskBook::decode(&serde_json::to_vec(&raw).unwrap()).unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), test_project(), BackendKind::Codex)),
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
        click_text(&ctx, &mut view, &snapshot, "Параметры", &mut commands);
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
        view.update(&mut |c| commands.push(c));
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
        let id = book.create(test_project(), "codex", "Task", "Read").unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), test_project(), BackendKind::Qwen)),
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
        click_text(&ctx, &mut view, &snapshot, "Параметры", &mut commands);
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
        view.update(&mut |c| commands.push(c));
        assert_eq!(view.tasks[0].backend, "codex");
        assert_eq!(view.tasks[0].revision, 1);
    }
    #[test]
    fn gui_preview_shows_the_exact_prompt_without_dispatching_a_request() {
        let ctx = egui::Context::default();
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(test_project(), "qwen", "Task", "Read\n  source").unwrap();
        let expected = book.get(id).unwrap().preview(None).unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), test_project(), BackendKind::Qwen)),
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
        click_text(&ctx, &mut view, &snapshot, "Параметры", &mut commands);
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
        book.create(test_project(), "qwen", "Existing", "Read").unwrap();
        let mut view = TasksView {
            selected: Some(key),
            scope: Some((Some(key), test_project(), BackendKind::Qwen)),
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
        view.page = Page::New;
        view.request(Request::List, &mut |command| commands.push(command));
        let Command::Tasks { reply: stale_read, .. } = commands.pop().unwrap() else {
            panic!("list")
        };
        let available = view.availability(&view.tasks[0], BackendKind::Qwen, false);
        assert!(available.edit && available.launch, "refresh must not disable task buttons");
        click_text(&ctx, &mut view, &snapshot, "Сохранить черновик", &mut commands);
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
        let id = book.create(test_project(), "qwen", "New task", "Check").unwrap();
        reply
            .send(Ok(TaskSnapshot {
                tasks: vec![book.get(id).unwrap().clone()],
                ..TaskSnapshot::default()
            }))
            .unwrap();
        view.update(&mut |c| commands.push(c));
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
                scope: Some((Some(key), test_project(), BackendKind::Qwen)),
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
            view.page = Page::New;
            if field == "Название черновика" {
                click_text(&ctx, &mut view, &snapshot, "Название задачи", &mut commands);
            }
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
        let id = book.create(test_project(), "qwen", "Task", "Read").unwrap();
        let mut view = TasksView::default();
        let available = view.availability(book.get(id).unwrap(), BackendKind::Qwen, true);
        assert!(available.edit && !available.launch);
        book.begin(id, 1, None, ts()).unwrap();
        assert!(!view.availability(book.get(id).unwrap(), BackendKind::Qwen, false).edit);
        book.finish(id, 1, RunState::Completed, ts()).unwrap();
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let (sender, receiver) = oneshot::channel();
        view.pending = Some(Pending {
            scope: (Some(key), test_project(), BackendKind::Qwen),
            effect: Effect::Edit,
            reply: receiver,
        });
        let available = view.availability(book.get(id).unwrap(), BackendKind::Qwen, false);
        assert!(!available.edit && !available.launch);
        drop(sender);
    }
    #[test]
    fn workflow_failure_keeps_visible_status_and_form_until_acknowledged() {
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let mut book = hub_core::tasks::TaskBook::default();
        let id = book.create(test_project(), "qwen", "Task", "Read").unwrap();
        let mut view = TasksView {
            scope: Some((Some(key), test_project(), BackendKind::Qwen)),
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
        view.update(&mut |c| commands.push(c));
        assert_eq!(view.tasks[0].status, TaskStatus::Planned);
        assert_eq!(view.title, "Draft");
        assert!(view.error.as_ref().unwrap().contains("Нет доступа"));
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
        book.create(test_project(), "qwen", "Old scope", "Read").unwrap();
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
        let project = test_project();
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
            scope: Some((Some(key), test_project().join("old"), BackendKind::Qwen)),
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
        let id = book.create(test_project(), "qwen", "Task", "First").unwrap();
        let mut view = TasksView {
            scope: Some((Some(key), test_project(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            ..Default::default()
        };
        view.next.insert(id, "Old continuation".into());
        let mut commands = Vec::new();
        view.request(Request::List, &mut |command| commands.push(command));
        let Command::Tasks { reply, .. } = commands.pop().unwrap() else { panic!("task request") };
        book.begin(id, 1, None, ts()).unwrap();
        reply
            .send(Ok(TaskSnapshot { tasks: book.tasks().to_vec(), ..TaskSnapshot::default() }))
            .unwrap();
        view.update(&mut |c| commands.push(c));
        assert!(!view.next.contains_key(&id), "a different revision needs a new step");
    }
}
