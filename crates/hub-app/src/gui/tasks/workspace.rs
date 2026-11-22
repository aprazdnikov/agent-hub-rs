//! Chat, result review and execution parameters for the selected durable task.
use super::{
    Availability, BackendKind, Command, Effect, LocalBlock, Page, Request, Rework, Scope, Snapshot,
    Task, TaskStatus, TasksView, result,
};
use crate::gui::{look::agent_title, theme::primary_button};
use eframe::egui;
use egui::{Align, Layout, RichText};
use hub_core::tasks::RunState;

fn project_name(project: &std::path::Path) -> std::borrow::Cow<'_, str> {
    project.file_name().unwrap_or(project.as_os_str()).to_string_lossy()
}

fn task_title(title: &str, prompt: &str) -> String {
    if !title.trim().is_empty() {
        return title.trim().to_owned();
    }
    let line = prompt.trim().lines().next().unwrap_or_default();
    line.char_indices()
        .take(70)
        .take_while(|(i, c)| i + c.len_utf8() <= 256)
        .map(|(_, c)| c)
        .collect()
}

fn memory_description(task: &Task, scope: &Scope, snapshot: &Snapshot) -> String {
    let topic_mode = snapshot
        .topics
        .iter()
        .find(|topic| Some(topic.key) == scope.0)
        .and_then(|topic| topic.session.preferences.memory);
    let (enabled, source) = if let Some(on) = task.memory {
        (on, "задача")
    } else if let Some(on) = topic_mode {
        (on, "тема Telegram")
    } else {
        (
            snapshot
                .saved
                .as_ref()
                .map_or_else(hub_core::settings::MemorySettings::default, |draft| draft.memory)
                .enabled,
            "общие настройки",
        )
    };
    format!("Память {} · {source}", if enabled { "включена" } else { "выключена" })
}

fn task_state(task: &Task) -> &'static str {
    if task.archived {
        "В архиве"
    } else if task.active() {
        "Выполняется"
    } else if task.queue.is_some() {
        "В очереди"
    } else if matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred) {
        task.status.label()
    } else if let Some(run) =
        task.runs.last().filter(|run| matches!(run.state, RunState::Failed | RunState::Interrupted))
    {
        run.state.label()
    } else {
        task.status.label()
    }
}

fn topic_running(scope: &Scope, snapshot: &Snapshot) -> bool {
    snapshot.topics.iter().any(|topic| {
        Some(topic.key) == scope.0 && topic.state == hub_telegram::hub::TopicState::Running
    })
}

// Bottom panels reserve space for actions. Restrict both painting and pointer input
// to the remaining rectangle, even when a short window has only one visible line.
fn scroll_body(
    ui: &mut egui::Ui,
    id: &str,
    stick_to_bottom: bool,
    contents: impl FnOnce(&mut egui::Ui),
) {
    let rect = ui.available_rect_before_wrap().intersect(ui.clip_rect());
    if rect.is_positive() {
        ui.scope(|ui| {
            ui.set_clip_rect(rect);
            egui::ScrollArea::vertical()
                .id_salt(id)
                .auto_shrink([false, false])
                .min_scrolled_height(0.0)
                .max_height(rect.height())
                .stick_to_bottom(stick_to_bottom)
                .show(ui, contents);
        });
    }
}

fn shortcut(ui: &mut egui::Ui, editor: egui::Id) -> bool {
    ui.memory(|memory| memory.has_focus(editor))
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter))
}

impl TasksView {
    pub(super) fn new_task(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        if !self.tasks.is_empty() && ui.button("В чат").clicked() {
            self.leave_review();
            self.page = Page::Task;
            return;
        }
        ui.heading("Новая задача");
        ui.add_space(4.0);
        self.context_controls(ui, snapshot, true);
        let scope = self.resolved_scope(snapshot);
        self.set_scope(scope.clone());
        self.refresh(send);
        let creating =
            self.pending.as_ref().is_some_and(|p| matches!(p.effect, Effect::Create { .. }));
        ui.add_space(6.0);
        ui.label("Что поручить агенту");
        let editor = ui.make_persistent_id(("task_create_prompt", scope.as_ref().map(|s| &s.1)));
        let submit = shortcut(ui, editor);
        ui.add_enabled(
            !creating,
            egui::TextEdit::multiline(&mut self.prompt)
                .id(editor)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .char_limit(16384),
        );
        ui.collapsing("Название задачи", |ui| {
            ui.add_enabled(
                !creating,
                egui::TextEdit::singleline(&mut self.title)
                    .id_salt(("task_create_title", scope.as_ref().map(|s| &s.1)))
                    .hint_text("По умолчанию — первая строка поручения")
                    .char_limit(256),
            );
        });
        let title = task_title(&self.title, &self.prompt);
        let valid = !self.prompt.trim().is_empty()
            && !title.is_empty()
            && title.len() <= 256
            && self.prompt.len() <= 16384;
        let save = scope.is_some() && valid && !self.mutation_pending();
        let blocked = if scope.is_none() {
            Some("Выберите каталог проекта или тему Telegram.")
        } else if self.prompt.len() > 16384 {
            Some("Сократите поручение до 16 КиБ.")
        } else if title.len() > 256 {
            Some("Сократите название до 256 байт.")
        } else if self.rules.dirty() {
            Some("Сохраните правила проекта перед запуском.")
        } else if self.rules.saved.git.is_some() {
            Some("Сохраните черновик и подготовьте ветку Git в параметрах задачи.")
        } else if self.git_busy {
            Some("Дождитесь завершения операции Git.")
        } else if self.local_context() && self.local_block == LocalBlock::Job {
            Some("Локальный агент занят. Можно сохранить черновик.")
        } else if scope.as_ref().is_some_and(|s| topic_running(s, snapshot)) {
            Some("Тема занята. Можно сохранить черновик.")
        } else if self.refreshed.is_none() {
            Some("Загружаем правила проекта…")
        } else {
            None
        };
        if let Some(reason) = blocked {
            ui.small(reason);
        }
        let start = save && blocked.is_none();
        let choice = ui
            .horizontal_wrapped(|ui| {
                let launch =
                    primary_button(ui, start, "Начать задачу").clicked() || (start && submit);
                let save_draft =
                    ui.add_enabled(save, egui::Button::new("Сохранить черновик")).clicked();
                (launch || save_draft).then_some(launch)
            })
            .inner;
        if let (Some(start), Some(scope)) = (choice, scope) {
            self.dispatch(
                scope,
                Request::Create { title, prompt: self.prompt.clone() },
                Effect::Create {
                    start_with_rules: start.then_some(self.rules.saved.revision),
                    open: true,
                },
                send,
            );
        }
    }

    pub(super) fn card(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        backend: BackendKind,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        let Some(scope) = self.scope.as_ref() else { return };
        let available = self.availability(task, backend, topic_running(scope, snapshot));
        let key = (task.project.clone(), task.id);
        let mut next = if task.runs.is_empty() {
            self.prompt_drafts
                .get(&key)
                .map_or_else(|| task.prompt.clone(), |draft| draft.text.clone())
        } else {
            self.next.remove(&task.id).unwrap_or_default()
        };
        ui.push_id((&task.project, task.id), |ui| match self.result_view.tab(task) {
            result::Tab::Chat => {
                self.chat(ui, task, &available, &mut next, snapshot, send);
            }
            result::Tab::Parameters => {
                egui::ScrollArea::vertical().id_salt("task_parameters").show(ui, |ui| {
                    self.parameters(ui, task, &available, &next, snapshot, send);
                });
            }
            _ => self.review(ui, task, &mut next, &available, send),
        });
        if task.runs.is_empty() {
            if next == task.prompt {
                self.prompt_drafts.remove(&key);
            } else {
                self.prompt_drafts
                    .entry(key)
                    .or_insert_with(|| super::PromptDraft {
                        original: task.prompt.clone(),
                        text: String::new(),
                    })
                    .text = next;
            }
        } else {
            self.next.insert(task.id, next);
        }
    }

    fn breadcrumb(&self, ui: &mut egui::Ui, task: &Task) {
        let folder = project_name(&task.project);
        let name = self
            .sidebar
            .projects()
            .find(|(path, _)| *path == task.project)
            .map_or(folder.as_ref(), |(_, name)| name);
        ui.small(format!("{name}  /  #{}", task.id)).on_hover_text(format!(
            "{} · редакция {}",
            task.project.display(),
            task.revision
        ));
    }

    fn chat(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        available: &Availability,
        next: &mut String,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        let compact = ui.available_height() < 360.0;
        self.chat_header(ui, task, snapshot, compact);
        let needs_answer = self
            .selected_live(snapshot)
            .is_some_and(|live| live.active && live.interaction.is_some());
        let has_result =
            result::View::review_run(task).is_some() || task.status == TaskStatus::NeedsReview;
        let body_line = ui.text_style_height(&egui::TextStyle::Body);
        let button_height = ui.spacing().interact_size.y.max(
            ui.text_style_height(&egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.y,
        );
        let inline_result = has_result && compact && !needs_answer;
        let result_space = if has_result && !inline_result {
            button_height + ui.spacing().item_spacing.y
        } else {
            0.0
        };
        let composer_height = if needs_answer {
            (ui.available_height() * 0.60).clamp(130.0, 380.0)
        } else {
            body_line * 2.0 + button_height + 40.0
        };
        egui::Panel::bottom(ui.id().with("composer"))
            .exact_size(composer_height + result_space)
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show(ui, |ui| {
                if has_result && !inline_result && ui.button("Открыть результат").clicked()
                {
                    self.result_view.select(result::Tab::Summary, Some(task.id));
                }
                ui.separator();
                if needs_answer {
                    egui::ScrollArea::vertical()
                        .id_salt("chat_decision")
                        .show(ui, |ui| self.local_panels(ui, snapshot, send));
                } else {
                    self.composer(ui, task, available, next, inline_result, send);
                }
            });
        scroll_body(ui, "task_chat", true, |ui| {
            if task.runs.is_empty() {
                let key = (task.project.clone(), task.id);
                if let Some(draft) = self
                    .prompt_drafts
                    .get_mut(&key)
                    .filter(|draft| draft.original != task.prompt && next != &task.prompt)
                {
                    ui.strong("Поручение изменилось в другой сессии");
                    ui.label(&task.prompt);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("Использовать мой текст").clicked() {
                            draft.original.clone_from(&task.prompt);
                        }
                        if ui.button("Загрузить сохранённое").clicked() {
                            next.clone_from(&task.prompt);
                        }
                    });
                } else {
                    ui.weak("Переписка появится после первого запуска.");
                }
            } else {
                result::View::transcript(ui, task);
            }
        });
    }

    fn chat_header(&mut self, ui: &mut egui::Ui, task: &Task, snapshot: &Snapshot, compact: bool) {
        let Some(scope) = self.scope.as_ref() else { return };
        if compact {
            ui.spacing_mut().item_spacing.y = 3.0;
        }
        self.breadcrumb(ui, task);
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("Параметры").clicked() {
                    self.result_view.select(result::Tab::Parameters, Some(task.id));
                }
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    ui.add(
                        egui::Label::new(RichText::new(&task.title).heading().strong()).truncate(),
                    )
                    .on_hover_text(&task.title);
                });
            });
        });
        let agent = match BackendKind::parse(&task.backend) {
            Some(agent) => agent_title(agent),
            None => &task.backend,
        };
        let target = scope.0.map_or_else(
            || "локально".to_owned(),
            |key| {
                let name =
                    snapshot.topics.iter().find(|t| t.key == key).and_then(|t| t.title.as_deref());
                name.map_or_else(
                    || format!("Telegram · #{}", key.thread.0),
                    |s| format!("Telegram · {s}"),
                )
            },
        );
        let location = format!("{agent} · {target} · {}", task_state(task));
        let memory = memory_description(task, scope, snapshot);
        if compact {
            ui.horizontal_wrapped(|ui| {
                ui.small(location);
                ui.small(memory);
            });
        } else {
            ui.add(egui::Label::new(RichText::new(&location).weak()).truncate())
                .on_hover_text(location);
            ui.small(memory);
        }
        ui.separator();
    }

    fn composer(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        available: &Availability,
        next: &mut String,
        inline_result: bool,
        send: &mut impl FnMut(Command),
    ) {
        let Some(backend) = self.scope.as_ref().map(|scope| scope.2) else { return };
        let first = task.runs.is_empty();
        let can_edit_prompt = available.edit
            && task.queue.is_none()
            && !task.archived
            && !matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred);
        let submit = self.message_field(ui, task, next, !first || can_edit_prompt);
        let dirty = first && next != &task.prompt;
        let conflict = dirty
            && self
                .prompt_drafts
                .get(&(task.project.clone(), task.id))
                .is_some_and(|draft| draft.original != task.prompt);
        let blocked = if conflict {
            Some("Сравните изменения поручения в чате.")
        } else if task.queue.is_some() {
            Some("Задача в очереди. Управление — в параметрах.")
        } else {
            self.launch_block(task, backend, available, next)
        };
        let button = if first { "Запустить" } else { "Отправить" };
        let clicked = ui
            .horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let clicked = primary_button(ui, blocked.is_none(), button)
                        .on_disabled_hover_text(blocked.unwrap_or_default())
                        .clicked();
                    if dirty
                        && ui
                            .add_enabled(
                                can_edit_prompt
                                    && !conflict
                                    && !next.trim().is_empty()
                                    && next.len() <= 16384,
                                egui::Button::new("Сохранить"),
                            )
                            .clicked()
                    {
                        self.request(
                            Request::EditPrompt {
                                id: task.id,
                                revision: task.revision,
                                prompt: next.clone(),
                            },
                            send,
                        );
                    }
                    if inline_result && ui.button("Открыть результат").clicked() {
                        self.result_view.select(result::Tab::Summary, Some(task.id));
                    }
                    let note = match blocked {
                        Some("Введите сообщение агенту.") | None if !first => {
                            if cfg!(target_os = "macos") {
                                "⌘+Enter для отправки"
                            } else {
                                "Ctrl+Enter для отправки"
                            }
                        }
                        other => other.unwrap_or_default(),
                    };
                    if !note.is_empty() {
                        ui.add_sized(
                            [ui.available_width(), ui.spacing().interact_size.y],
                            egui::Label::new(RichText::new(note).small().weak())
                                .halign(Align::Min)
                                .wrap(),
                        )
                        .on_hover_text(note);
                    }
                    clicked
                })
                .inner
            })
            .inner;
        if blocked.is_none()
            && (clicked || submit)
            && let Some(scope) = self.scope.clone()
        {
            self.start_message(scope, task, next, send);
        }
    }

    fn start_message(
        &mut self,
        scope: Scope,
        task: &Task,
        next: &str,
        send: &mut impl FnMut(Command),
    ) {
        let first = task.runs.is_empty();
        let dirty = first && next != task.prompt;
        let review = self.reworking.as_ref().is_some_and(|r| r.task == task.id);
        let rules_revision = self.rules.saved.revision;
        let (request, effect) = if dirty {
            (
                Request::EditPrompt {
                    id: task.id,
                    revision: task.revision,
                    prompt: next.to_owned(),
                },
                Effect::StartAfterPrompt { id: task.id, rules_revision, review },
            )
        } else {
            (
                Request::StartChecked {
                    rules_revision,
                    id: task.id,
                    revision: task.revision,
                    next: (!first).then(|| next.to_owned()),
                },
                if review { Effect::Review { id: task.id, follow: true } } else { Effect::Edit },
            )
        };
        self.dispatch(scope, request, effect, send);
    }

    fn message_field(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        next: &mut String,
        editable: bool,
    ) -> bool {
        let editor = ui.make_persistent_id("task_message");
        let submit = shortcut(ui, editor);
        let max_height = ui.text_style_height(&egui::TextStyle::Body) * 2.0 + 8.0;
        egui::ScrollArea::vertical()
            .id_salt("message_field")
            .min_scrolled_height(0.0)
            .max_height(max_height)
            .show(ui, |ui| {
                let hint = if self.reworking.as_ref().is_some_and(|r| r.task == task.id) {
                    "Что нужно доработать?"
                } else if task.runs.is_empty() {
                    "Что поручить агенту"
                } else {
                    "Сообщение агенту"
                };
                let field = ui.add_enabled(
                    editable && !self.mutation_pending(),
                    egui::TextEdit::multiline(next)
                        .id(editor)
                        .desired_width(f32::INFINITY)
                        .desired_rows(2)
                        .char_limit(16384)
                        .hint_text(hint),
                );
                if let Some(r) = &mut self.reworking
                    && r.task == task.id
                    && std::mem::take(&mut r.focus)
                {
                    field.request_focus();
                }
            });
        submit
    }

    fn parameters(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        available: &Availability,
        next: &str,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        let return_to_inbox = self.review_from_inbox && self.reworking.is_none();
        if ui.button(if return_to_inbox { "Во входящие" } else { "В чат" }).clicked()
        {
            if return_to_inbox {
                self.leave_review();
                self.inbox_return = true;
            }
            self.result_view.select(result::Tab::Chat, Some(task.id));
        }
        self.breadcrumb(ui, task);
        ui.heading("Параметры задачи");
        ui.strong(&task.title);
        self.context_controls(ui, snapshot, false);
        if self.resolved_scope(snapshot) != self.scope {
            return;
        }
        ui.separator();
        if let Some((_, _, backend)) = self.scope.as_ref() {
            self.executor_controls(ui, task, *backend, available.edit, send);
        }
        if let Some(scope) = self.scope.as_ref() {
            ui.small(memory_description(task, scope, snapshot));
        }
        self.git_controls(ui, task, send);
        if self.selected_live(snapshot).is_some_and(|live| live.active) {
            self.local_panels(ui, snapshot, send);
        }
        if let Some(plan) =
            self.rules.saved.workflow.as_ref().filter(|p| p.implementer.name() == task.backend)
        {
            ui.label(format!(
                "{} → {} → ревью {}",
                plan.implementer.name(),
                plan.command,
                plan.reviewer.name()
            ));
        }
        if ui.button("Правила проекта").clicked() {
            self.page = Page::Rules;
        }
        ui.collapsing("Контекст следующего запуска", |ui| {
            ui.label(format!("Поручение хаба · r{} · {}", task.revision, task.backend));
            ui.small("Каждый запуск создаёт новую CLI-сессию с сохранёнными правилами и контекстом задачи.");
            let mut preview;
            let task = if task.runs.is_empty() && next != task.prompt {
                preview = task.clone();
                next.clone_into(&mut preview.prompt);
                &preview
            } else { task };
            match task.preview_with_rules((!task.runs.is_empty()).then_some(next), &self.rules.saved) {
                Ok(text) => { ui.add(egui::Label::new(text).wrap().selectable(true)); }
                Err(error) => { ui.label(error); }
            }
        });
        self.show_queue_controls(ui, task, next, send);
        ui.collapsing("Управление задачей", |ui| {
            self.workflow_controls(ui, task, !available.edit, send);
        });
    }

    fn review(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        next: &mut String,
        available: &Availability,
        send: &mut impl FnMut(Command),
    ) {
        if ui.button(if self.review_from_inbox { "Во входящие" } else { "В чат" }).clicked()
        {
            self.inbox_return = self.review_from_inbox;
            self.review_from_inbox = false;
            self.result_view.select(result::Tab::Chat, Some(task.id));
        }
        self.breadcrumb(ui, task);
        ui.heading(&task.title);
        if task.status == TaskStatus::NeedsReview && !task.archived {
            egui::Panel::bottom(ui.id().with("review_actions"))
                .default_size(54.0)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    ui.add_space(6.0);
                    ui.horizontal_wrapped(|ui| {
                        if primary_button(ui, available.edit, "Принять результат").clicked()
                            && let Some(scope) = self.scope.clone()
                        {
                            self.dispatch(
                                scope,
                                Request::Status {
                                    id: task.id,
                                    revision: task.revision,
                                    status: TaskStatus::Accepted,
                                },
                                Effect::Review { id: task.id, follow: true },
                                send,
                            );
                        }
                        if ui
                            .add_enabled(available.edit, egui::Button::new("На доработку"))
                            .clicked()
                        {
                            self.result_view.select(result::Tab::Chat, Some(task.id));
                            self.reworking = Some(Rework { task: task.id, focus: true });
                        }
                    });
                });
        } else {
            ui.weak(task_state(task));
        }
        scroll_body(ui, "review_body", false, |ui| {
            if result::View::review_run(task).is_none() {
                ui.weak("Сохранённых попыток пока нет.");
            }
            if let Some(annotated) = self.result_view.show(ui, task, next, available.edit) {
                *next = annotated;
            }
        });
    }
}
