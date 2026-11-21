//! Durable tasks use the same actor port as Telegram, never a second file writer.
use crate::supervisor::{BotStatus, Command, Snapshot};
use eframe::egui;
use hub_core::{
    domain::{BackendKind, TopicKey},
    tasks::{RunState, Task},
};
use hub_telegram::hub::tasks::Request;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

type Scope = (TopicKey, PathBuf, BackendKind);
type Reply = oneshot::Receiver<Result<Vec<Task>, String>>;
#[derive(Default)]
pub struct TasksView {
    selected: Option<TopicKey>,
    scope: Option<Scope>,
    tasks: Vec<Task>,
    pending: Option<(Scope, bool, Reply)>,
    refreshed: Option<Instant>,
    title: String,
    prompt: String,
    next: std::collections::BTreeMap<u64, String>,
    error: Option<String>,
}
impl TasksView {
    fn request(&mut self, request: Request, send: &mut impl FnMut(Command)) {
        let Some((key, project, backend)) = self.scope.clone() else { return };
        if self.pending.is_some() {
            return;
        }
        let creating = matches!(request, Request::Create { .. });
        let (reply, result) = oneshot::channel();
        send(Command::Tasks { key, project: project.clone(), backend, request, reply });
        self.pending = Some(((key, project, backend), creating, result));
        self.error = None;
    }
    fn poll(&mut self) {
        let Some((scope, creating, mut receiver)) = self.pending.take() else { return };
        match receiver.try_recv() {
            Ok(result) if self.scope.as_ref() == Some(&scope) => {
                self.refreshed = Some(Instant::now());
                match result {
                    Ok(tasks) => {
                        self.next.retain(|id, _| {
                            self.tasks.iter().any(|old| {
                                old.id == *id
                                    && tasks
                                        .iter()
                                        .any(|new| new.id == *id && new.revision == old.revision)
                            })
                        });
                        if creating {
                            self.title.clear();
                            self.prompt.clear();
                        }
                        self.tasks = tasks;
                        self.error = None;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            Ok(_) => {}
            Err(oneshot::error::TryRecvError::Empty) => {
                self.pending = Some((scope, creating, receiver));
            }
            Err(oneshot::error::TryRecvError::Closed) => {
                self.error = Some("Бот остановлен или запрос не принят. Обновите список.".into());
                self.refreshed = Some(Instant::now());
            }
        }
    }
    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        super::theme::page_heading(
            ui,
            "Задачи",
            "Ручной запуск и история попыток. После перезапуска работа не возобновляется автоматически.",
        );
        if !matches!(snapshot.bot, BotStatus::Running { .. }) {
            self.scope = None;
            self.pending = None;
            self.tasks.clear();
            self.refreshed = None;
            ui.label("Для просмотра и запуска задач запустите бота. Сохранённые задачи останутся на диске.");
            return;
        }
        egui::ComboBox::from_id_salt("task_topic")
            .selected_text(self.selected.map_or("Выберите тему".into(), |key| {
                format!("Тема #{}", key.thread.0)
            }))
            .show_ui(ui, |ui| {
                for topic in &snapshot.topics {
                    ui.selectable_value(
                        &mut self.selected,
                        Some(topic.key),
                        format!(
                            "{} · {} · {}",
                            topic.title.as_deref().unwrap_or("Без названия"),
                            topic.session.backend.name(),
                            topic.session.cwd.as_path().display()
                        ),
                    );
                }
            });
        let scope = self
            .selected
            .and_then(|key| snapshot.topics.iter().find(|topic| topic.key == key))
            .map(|topic| {
                (topic.key, topic.session.cwd.as_path().to_path_buf(), topic.session.backend)
            });
        if scope != self.scope {
            self.scope = scope;
            self.title.clear();
            self.prompt.clear();
            self.tasks.clear();
            self.error = None;
            self.pending = None;
            self.refreshed = None;
            self.next.clear();
        }
        let Some((key, project, backend)) = self.scope.clone() else {
            ui.label("Выберите существующую тему. Каталог и агент задачи берутся из неё.");
            return;
        };
        ui.label(format!("{} · {}", backend.name(), project.display()));
        self.poll();
        if self.pending.is_none()
            && self.refreshed.is_none_or(|time| time.elapsed() > Duration::from_secs(3))
        {
            self.request(Request::List, send);
        }
        ui.ctx().request_repaint_after(Duration::from_millis(250));
        if let Some(error) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        let busy = self.pending.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            if ui.button("Обновить задачи").clicked() {
                self.request(Request::List, send);
            }
            ui.collapsing("Создать задачу", |ui| {
                ui.label("Название");
                ui.add(egui::TextEdit::singleline(&mut self.title).char_limit(256));
                ui.label("Поручение и ограничения");
                ui.add(
                    egui::TextEdit::multiline(&mut self.prompt).desired_rows(4).char_limit(16384),
                );
                if ui
                    .add_enabled(
                        !self.title.trim().is_empty() && !self.prompt.trim().is_empty(),
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
        ui.separator();
        let topic_running = snapshot
            .topics
            .iter()
            .any(|topic| topic.key == key && topic.state == hub_telegram::hub::TopicState::Running);
        let tasks = self.tasks.clone();
        egui::ScrollArea::vertical().show(ui, |ui| {
            for task in &tasks {
                self.card(ui, task, backend, busy || topic_running, send);
            }
        });
    }
    fn card(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        backend: BackendKind,
        busy: bool,
        send: &mut impl FnMut(Command),
    ) {
        let mut next = self.next.get(&task.id).cloned().unwrap_or_default();
        ui.push_id(task.id, |ui| {
            ui.heading(format!("#{} · {} · r{}", task.id, task.title, task.revision));
            ui.label(format!("Агент: {}", task.backend));
            ui.label(&task.prompt);
            for run in &task.runs {
                ui.collapsing(
                    format!("Попытка {} — {}", run.id, run.state.label()),
                    |ui| {
                        ui.label(&run.step);
                        if !run.diagnostic.is_empty() {
                            ui.label(&run.diagnostic);
                        }
                        ui.add(
                            egui::Label::new(if run.result.is_empty() {
                                "Сохранённого текста результата нет"
                            } else {
                                &run.result
                            })
                            .selectable(true),
                        );
                    },
                );
            }
            let active = task.runs.last().is_some_and(|run| run.state == RunState::Running);
            if active {
                ui.label("Выполняется. Остановить можно в теме Telegram или во вкладке «Темы».");
            } else {
                ui.add_enabled_ui(!busy && task.backend == backend.name(), |ui| {
                    if task.runs.is_empty() {
                        if ui.button("Запустить задачу в выбранной теме").clicked()
                        {
                            self.request(
                                Request::Start { id: task.id, revision: task.revision, next: None },
                                send,
                            );
                        }
                    } else {
                        ui.label(
                            "Перед продолжением проверьте прежние эффекты. Укажите следующий шаг:",
                        );
                        ui.add(
                            egui::TextEdit::multiline(&mut next).desired_rows(2).char_limit(16384),
                        );
                        if ui
                            .add_enabled(
                                !next.trim().is_empty(),
                                egui::Button::new("Продолжить новым запуском"),
                            )
                            .clicked()
                        {
                            self.request(
                                Request::Start {
                                    id: task.id,
                                    revision: task.revision,
                                    next: Some(next.clone()),
                                },
                                send,
                            );
                        }
                    }
                });
                if task.backend != backend.name() {
                    ui.small("Для запуска выберите тему с агентом этой задачи.");
                }
            }
            ui.small("Завершение запуска не означает приёмку результата пользователем.");
            ui.separator();
        });
        self.next.insert(task.id, next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::domain::{AbsolutePath, ChatId, ThreadId, TopicSession};
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
    #[test]
    fn stopped_bot_does_not_dispatch_and_topic_change_discards_stale_reply() {
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let other = TopicKey { chat: key.chat, thread: ThreadId(8) };
        let mut view = TasksView { selected: Some(key), ..Default::default() };
        let mut commands = Vec::new();
        frame(&mut view, &Snapshot::default(), &mut commands);
        assert!(commands.is_empty());
        let snapshot = Snapshot {
            bot: BotStatus::Running { username: "bot".into() },
            topics: vec![topic(key), topic(other)],
            ..Default::default()
        };
        frame(&mut view, &snapshot, &mut commands);
        let Command::Tasks { key: sent, request: Request::List, reply, .. } =
            commands.pop().unwrap()
        else {
            panic!("expected list")
        };
        assert_eq!(sent, key);
        let mut book = hub_core::tasks::TaskBook::default();
        book.create(std::env::temp_dir(), "qwen", "Old scope", "Read").unwrap();
        reply.send(Ok(book.tasks().to_vec())).unwrap();
        view.selected = Some(other);
        frame(&mut view, &snapshot, &mut commands);
        assert!(view.tasks.is_empty());
        assert_eq!(view.scope.as_ref().unwrap().0, other);
    }
    #[test]
    fn start_command_uses_visible_revision_and_selected_scope() {
        let key = TopicKey { chat: ChatId(-100), thread: ThreadId(7) };
        let project = std::env::temp_dir();
        let mut view = TasksView {
            scope: Some((key, project.clone(), BackendKind::Qwen)),
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
            scope: Some((key, std::env::temp_dir().join("old"), BackendKind::Qwen)),
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
            scope: Some((key, std::env::temp_dir(), BackendKind::Qwen)),
            tasks: book.tasks().to_vec(),
            ..Default::default()
        };
        view.next.insert(id, "Old continuation".into());
        let mut commands = Vec::new();
        view.request(Request::List, &mut |command| commands.push(command));
        let Command::Tasks { reply, .. } = commands.pop().unwrap() else { panic!("task request") };
        book.begin(id, 1, None).unwrap();
        reply.send(Ok(book.tasks().to_vec())).unwrap();
        view.poll();
        assert!(!view.next.contains_key(&id), "a different revision needs a new step");
    }
}
