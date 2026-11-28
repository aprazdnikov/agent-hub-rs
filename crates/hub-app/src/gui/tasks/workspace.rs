//! Chat, result review and execution parameters for the selected durable task.
use super::{
    Availability, BackendKind, Command, Effect, LocalBlock, Page, QueueBlock, Request, Rework,
    Scope, Snapshot, Task, TaskStatus, TasksView, result,
};
use crate::gui::{
    look::{agent_title, topic_url},
    theme::{Tone, primary_button, tone_colors},
};
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

fn task_badge(
    task: &Task,
    live: Option<&crate::local_execution::Live>,
) -> (&'static str, kylig_ui::Severity) {
    use crate::local_execution::Kind;
    use kylig_ui::Severity;
    if let Some(request) = live.filter(|job| job.active).and_then(|job| job.interaction.as_ref()) {
        return match request.kind {
            Kind::Questions(_) => ("Вопрос", Severity::Info),
            Kind::Permission(_) => ("Разрешение", Severity::Warning),
        };
    }
    let severity = if task.archived || task.queue.is_some() {
        Severity::Info
    } else if task.status == TaskStatus::Accepted {
        Severity::Success
    } else if task.status == TaskStatus::Deferred {
        Severity::Warning
    } else {
        match task.runs.last().map(|run| run.state) {
            Some(RunState::Failed) => Severity::Error,
            Some(RunState::Interrupted) => Severity::Warning,
            Some(RunState::Running | RunState::Completed) | None => Severity::Info,
        }
    };
    let label = if task.runs.is_empty()
        && !task.archived
        && task.queue.is_none()
        && task.status == TaskStatus::Planned
    {
        "Черновик"
    } else {
        task_state(task)
    };
    (label, severity)
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
    ui.is_enabled()
        && ui.memory(|memory| memory.has_focus(editor))
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter))
}

// Composer frame from chat.html: 1px border, radius 12, surface fill, mockup padding.
const COMPOSER_FRAME_MARGIN: egui::Margin = egui::Margin::symmetric(12, 9);
// Spec «Чат задачи»: below this chat height bubbles, the header and composer switch to compact.
const COMPACT_CHAT_HEIGHT: f32 = 360.0;
const COMPOSER_FRAME_EXTRA: f32 = 20.0; // Both vertical margins and frame borders
const COMPOSER_FIELD_MARGIN: egui::Margin = egui::Margin::symmetric(4, 2);

fn ui_font(ui: &egui::Ui, style: &egui::TextStyle) -> egui::FontId {
    ui.style()
        .text_styles
        .get(style)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(ui.text_style_height(style)))
}

// Height of the composer field for a draft at a known width: the wrapped galley
// between one and eight body lines plus the field's inner margins, matching the
// ScrollArea clamp in `message_field`.
fn composer_field_height(ui: &egui::Ui, draft: &str, width: f32) -> f32 {
    let body_line = ui.text_style_height(&egui::TextStyle::Body);
    let job = egui::text::LayoutJob::simple(
        draft.to_owned(),
        ui_font(ui, &egui::TextStyle::Body),
        egui::Color32::PLACEHOLDER,
        width.max(48.0),
    );
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    (galley.size().y + 2.0 * f32::from(COMPOSER_FIELD_MARGIN.top))
        .clamp(body_line + 8.0, body_line * 8.0 + 8.0)
}

fn task_heading(ui: &mut egui::Ui, task: &Task) {
    ui.add(egui::Label::new(RichText::new(&task.title).heading().strong()).wrap());
}

struct ComposerGuards {
    dirty: bool,
    conflict: bool,
    blocked: Option<&'static str>,
}

impl TasksView {
    pub(super) fn new_task(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        if !self.tasks.is_empty() && ui.link("В чат").clicked() {
            self.leave_review();
            self.page = Page::Task;
            return;
        }
        ui.heading("Новая задача");
        ui.weak("Выберите исполнителя и опишите ожидаемый результат.");
        ui.add_space(12.0);
        crate::gui::theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            self.new_task_form(ui, snapshot, send);
        });
    }

    fn new_task_form(
        &mut self,
        ui: &mut egui::Ui,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        self.context_controls(ui, snapshot, true);
        let scope = self.resolved_scope(snapshot);
        self.set_scope(scope.clone());
        self.refresh(send);
        let creating =
            self.pending.as_ref().is_some_and(|p| matches!(p.effect, Effect::Create { .. }));
        let editor_key = ui.make_persistent_id(("new-prompt-focus", scope.as_ref().map(|s| &s.1)));
        let submit = ui
            .data(|data| data.get_temp::<egui::Id>(editor_key))
            .is_some_and(|editor| shortcut(ui, editor));
        let response = ui.add_enabled(
            !creating,
            kylig_ui::TextArea::new(&mut self.prompt)
                .id_salt(("task_create_prompt", scope.as_ref().map(|s| &s.1)))
                .label("Что поручить агенту")
                .placeholder("Опишите задачу и условия готовности")
                .rows(3)
                .max_rows(Some(8))
                .width(ui.available_width()),
        );
        ui.data_mut(|data| data.insert_temp(editor_key, response.id));
        ui.add_enabled(
            !creating,
            kylig_ui::TextField::new(&mut self.title)
                .id_salt(("task_create_title", scope.as_ref().map(|s| &s.1)))
                .label("Название (необязательно)")
                .width(ui.available_width())
                .placeholder("Первая строка поручения"),
        );
        let title = task_title(&self.title, &self.prompt);
        let valid = !self.prompt.trim().is_empty()
            && !title.is_empty()
            && title.len() <= 256
            && self.prompt.len() <= 16384;
        let save = scope.is_some() && valid && !self.mutation_pending();
        let blocked = self.create_block(scope.as_ref(), snapshot, &title);
        let start = save && blocked.is_none();
        let choice = ui
            .horizontal_wrapped(|ui| {
                let launch = primary_button(ui, start, "Начать задачу")
                    .on_disabled_hover_text(blocked.unwrap_or_default())
                    .clicked()
                    || (start && submit);
                let save_draft = ui
                    .add_enabled(save, kylig_ui::Button::new("Сохранить черновик").ghost())
                    .clicked();
                if scope.is_some() && ui.link("Правила проекта").clicked() {
                    self.open_rules();
                }
                (launch || save_draft).then_some(launch)
            })
            .inner;
        if let Some(reason) = blocked {
            ui.small(reason);
        }
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

    // The single reason «Начать задачу» is held back, in the original chain order.
    // Draft saving is gated separately by `save`.
    fn create_block(
        &self,
        scope: Option<&Scope>,
        snapshot: &Snapshot,
        title: &str,
    ) -> Option<&'static str> {
        if scope.is_none() {
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
        } else if scope.is_some_and(|s| topic_running(s, snapshot)) {
            Some("Тема занята. Можно сохранить черновик.")
        } else if self.refreshed.is_none() {
            Some("Загружаем правила проекта…")
        } else {
            None
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
        ui.push_id((&task.project, task.id), |ui| {
            if self.review_from_inbox && ui.link("Во входящие").clicked() {
                self.leave_review();
                self.inbox_return = true;
            }
            self.chat_header(ui, task, &available, next.as_str(), snapshot, send);
            match self.result_view.tab(task) {
                result::Tab::Chat => self.chat(ui, task, &available, &mut next, snapshot, send),
                result::Tab::Parameters => {
                    egui::ScrollArea::vertical().id_salt("task_parameters").show(ui, |ui| {
                        self.parameters(ui, task, &available, &next, snapshot, send);
                    });
                }
                result::Tab::Summary
                | result::Tab::Changes
                | result::Tab::Checks
                | result::Tab::Files
                | result::Tab::History => self.review(ui, task, &mut next, &available, send),
            }
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
        let compact = ui.available_height() < COMPACT_CHAT_HEIGHT;

        let needs_answer = self
            .selected_live(snapshot)
            .is_some_and(|live| live.active && live.interaction.is_some());
        let has_result =
            result::View::review_run(task).is_some() || task.status == TaskStatus::NeedsReview;
        // On wide screens the header has a result tab. Keep the shortcut with the
        // composer only when the compact header hides those tabs.
        let inline_result = has_result && compact && !needs_answer;
        let management =
            task.archived || task.queue.is_some() || task.status == TaskStatus::Deferred;
        let locked = management || task.status == TaskStatus::Accepted;
        let composer_height = if locked {
            if management { 42.0 } else { 0.0 }
        } else if needs_answer {
            (ui.available_height() * 0.60).clamp(130.0, 380.0)
        } else {
            let field = composer_field_height(ui, next, ui.available_width() - 28.0);
            let actions = kylig_ui::ButtonOptions::default().height;
            field
                + actions
                + COMPOSER_FRAME_EXTRA
                + ui.spacing().item_spacing.y * 4.0
                + ui.text_style_height(&egui::TextStyle::Small)
                + 8.0
        };
        let draft = task.runs.is_empty() && !locked && !needs_answer;
        if draft {
            let key = (task.project.clone(), task.id);
            if let Some(draft) = self
                .prompt_drafts
                .get_mut(&key)
                .filter(|draft| draft.original != task.prompt && next != &task.prompt)
            {
                ui.strong("Поручение изменилось в другой сессии");
                ui.label(&task.prompt);
                ui.horizontal_wrapped(|ui| {
                    if ui.add(kylig_ui::Button::new("Использовать мой текст")).clicked()
                    {
                        draft.original.clone_from(&task.prompt);
                    }
                    if ui.add(kylig_ui::Button::new("Загрузить сохранённое")).clicked()
                    {
                        next.clone_from(&task.prompt);
                    }
                });
            }
            ui.add_space(6.0);
            self.composer(ui, task, available, next, false, send);
            return;
        }
        if composer_height > 0.0 {
            egui::Panel::bottom(ui.scope_id().with("composer"))
                .exact_size(composer_height)
                .frame(egui::Frame::NONE)
                .show_separator_line(false)
                .show(ui, |ui| {
                    ui.add_space(6.0);
                    if management {
                        if ui.add(kylig_ui::Button::new("Управление задачей")).clicked()
                        {
                            self.result_view.select(result::Tab::Parameters, Some(task.id));
                        }
                    } else if needs_answer {
                        egui::ScrollArea::vertical()
                            .id_salt("chat_decision")
                            .show(ui, |ui| self.local_panels(ui, snapshot, send));
                    } else {
                        self.composer(ui, task, available, next, inline_result, send);
                    }
                });
        }
        scroll_body(ui, "task_chat", true, |ui| {
            if task.runs.is_empty() {
                ui.add(egui::Label::new(&task.prompt).selectable(true).wrap());
            } else {
                result::View::transcript(ui, task, &mut self.result_view);
                if task.active() && !needs_answer {
                    ui.horizontal_wrapped(|ui| {
                        ui.add(kylig_ui::ProgressRing::indeterminate().size(18.0));
                        ui.label(
                            self.selected_live(snapshot)
                                .filter(|live| live.active)
                                .map_or("Агент выполняет поручение…", |live| live.status.as_str()),
                        );
                    });
                }
            }
        });
    }

    fn chat_header(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        available: &Availability,
        next: &str,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        let Some(scope) = self.scope.clone() else { return };
        let compact = ui.available_height() < COMPACT_CHAT_HEIGHT;
        if compact {
            ui.spacing_mut().item_spacing.y = 3.0;
        }
        self.breadcrumb(ui, task);
        // The pending-delete row renders in exactly one place per tab: here while the Chat tab
        // is active; on Parameters it belongs to «Управление задачей» (workflow_controls).
        let confirming = self.confirm_delete == Some((task.id, task.revision))
            && matches!(self.result_view.tab(task), result::Tab::Chat);
        let tab = self.result_view.tab(task);
        let mut actions = |ui: &mut egui::Ui| {
            if !confirming && tab == result::Tab::Chat {
                self.header_menu(ui, task, available, next, send);
                self.header_actions(ui, task, available, snapshot, send);
            }
        };
        if ui.available_width() < 520.0 {
            task_heading(ui, task);
            if !confirming {
                ui.horizontal_wrapped(actions);
            }
        } else {
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    actions(ui);
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width());
                        task_heading(ui, task);
                    });
                });
            });
        }
        if confirming {
            self.delete_confirm(ui, task, send);
        }
        let agent = match BackendKind::parse(&task.backend) {
            Some(agent) => agent_title(agent),
            None => &task.backend,
        };
        let target = scope.0.map_or_else(
            || "Локально".to_owned(),
            |key| {
                let name =
                    snapshot.topics.iter().find(|t| t.key == key).and_then(|t| t.title.as_deref());
                name.map_or_else(
                    || format!("Telegram · #{}", key.thread.0),
                    |s| format!("Telegram · {s}"),
                )
            },
        );
        let (state, severity) = task_badge(task, self.selected_live(snapshot));
        ui.horizontal_wrapped(|ui| {
            ui.add(kylig_ui::Badge::dot().severity(severity));
            ui.label(state);
            ui.weak(agent);
            ui.weak(target);
            ui.small(memory_description(task, &scope, snapshot));
        });
        if !compact || tab != result::Tab::Chat {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if ui.selectable_label(tab == result::Tab::Chat, "Чат").clicked() {
                    self.result_view.select(result::Tab::Chat, Some(task.id));
                }
                if result::View::review_run(task).is_some()
                    && ui
                        .selectable_label(
                            !matches!(tab, result::Tab::Chat | result::Tab::Parameters),
                            "Результат",
                        )
                        .clicked()
                {
                    self.result_view.select(result::Tab::Summary, Some(task.id));
                }
                let narrow = ui.available_width() < 310.0;
                if ui
                    .selectable_label(
                        tab == result::Tab::Parameters,
                        if narrow {
                            "Параметры"
                        } else {
                            "Параметры и контекст"
                        },
                    )
                    .clicked()
                {
                    self.result_view.select(result::Tab::Parameters, Some(task.id));
                }
                if ui.link(if narrow { "Правила" } else { "Правила проекта" }).clicked()
                {
                    self.open_rules();
                }
            });
        }
        ui.separator();
    }

    // The primary action by task state, mirroring `task_state` precedence
    // (spec «Шапка задачи и меню действий»): running → stop or topic, queue,
    // review, failed run; drafts and finished states keep the composer button.
    fn header_actions(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        available: &Availability,
        snapshot: &Snapshot,
        send: &mut impl FnMut(Command),
    ) {
        if task.archived {
            return;
        }
        if task.active() {
            let Some(scope) = self.scope.as_ref() else { return };
            if scope.0.is_none() {
                if let Some(token) =
                    self.selected_live(snapshot).filter(|live| live.active).map(|live| live.token)
                {
                    let stop = egui::Button::new(
                        RichText::new("Остановить").color(tone_colors(ui, Tone::Error).1),
                    );
                    if ui.add(stop).clicked() {
                        send(Command::LocalStop(token));
                    }
                }
            } else {
                let url = scope.0.and_then(topic_url);
                let open = ui
                    .add_enabled(url.is_some(), egui::Button::new("Открыть тему"))
                    .on_hover_text("Открыть тему в Telegram");
                if open.clicked()
                    && let Some(url) = url
                {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(url));
                }
            }
            return;
        }
        if task.queue.is_some() {
            if ui.button("Снять с очереди").clicked() {
                self.request(Request::Unqueue { id: task.id, revision: task.revision }, send);
            }
            return;
        }
        if matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred) {
            return;
        }
        if task
            .runs
            .last()
            .is_some_and(|run| matches!(run.state, RunState::Failed | RunState::Interrupted))
        {
            if ui.button("Разобрать запуск").clicked() {
                self.result_view.select(result::Tab::Checks, Some(task.id));
            }
            return;
        }
        if task.status == TaskStatus::NeedsReview
            && ui.add_enabled(available.edit, kylig_ui::Button::new("Принять")).clicked()
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
    }

    fn header_menu(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        available: &Availability,
        next: &str,
        send: &mut impl FnMut(Command),
    ) {
        let queued = task.queue.is_some();
        let first = task.runs.is_empty();
        let offers = if queued { Vec::new() } else { self.queue_offer(task, next) };
        let _ = ui.menu_button("…", |ui| {
            if ui.button("Параметры задачи").clicked() {
                self.result_view.select(result::Tab::Parameters, Some(task.id));
            }
            if ui.button("Правила проекта").clicked() {
                self.open_rules();
            }
            if ui.button("Контекст следующего запуска").clicked() {
                self.result_view.select(result::Tab::Parameters, Some(task.id));
                // `select` resets the view; the request is recorded after it.
                self.result_view.request_context_open();
            }
            if queued || !offers.is_empty() {
                ui.separator();
            }
            if queued {
                if ui.button("Снять с очереди").clicked() {
                    self.request(Request::Unqueue { id: task.id, revision: task.revision }, send);
                }
            } else {
                for (isolated, label, block) in offers {
                    let button = ui
                        .add_enabled(block.is_none(), egui::Button::new(label))
                        .on_disabled_hover_text(block.map_or("", QueueBlock::label));
                    if button.clicked() {
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
            }
            let archive = egui::Button::new(if task.archived {
                "Восстановить"
            } else {
                "В архив"
            });
            if ui.add_enabled(available.edit, archive).clicked() {
                self.request(
                    Request::Archive {
                        id: task.id,
                        revision: task.revision,
                        archived: !task.archived,
                    },
                    send,
                );
            }
            ui.separator();
            let delete =
                egui::Button::new(RichText::new("Удалить…").color(tone_colors(ui, Tone::Error).1));
            let delete = if queued {
                ui.add_enabled(false, delete)
                    .on_disabled_hover_text("Сначала снимите задачу с очереди")
            } else {
                ui.add_enabled(available.edit, delete)
            };
            if delete.clicked() {
                self.confirm_delete = Some((task.id, task.revision));
            }
        });
    }

    fn composer_guards(
        &self,
        task: &Task,
        available: &Availability,
        backend: BackendKind,
        next: &str,
    ) -> ComposerGuards {
        let dirty = task.runs.is_empty() && next != task.prompt;
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
        ComposerGuards { dirty, conflict, blocked }
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
        let button = if first { "Запустить" } else { "Отправить" };
        let before = self.composer_guards(task, available, backend, next);
        let editor = ui.make_persistent_id("task_message");
        let focused = ui.memory(|m| m.has_focus(editor));
        let frame = egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .stroke(if focused {
                ui.visuals().selection.stroke
            } else {
                ui.visuals().window_stroke
            })
            .corner_radius(12.0)
            .inner_margin(COMPOSER_FRAME_MARGIN);
        let (send_clicked, save_clicked, submit) = frame
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let submit = self.message_field(ui, editor, task, next, !first || can_edit_prompt);
                ui.separator();
                let (launch, saved) = ui
                    .horizontal_wrapped(|ui| {
                        let launch = ui
                            .add_enabled(
                                before.blocked.is_none(),
                                kylig_ui::Button::new(button)
                                    .icon(kylig_icons::PAPER_PLANE_TILT)
                                    .primary(),
                            )
                            .on_disabled_hover_text(before.blocked.unwrap_or_default())
                            .clicked();
                        let saved = first
                            && ui
                                .add_enabled(
                                    before.dirty
                                        && can_edit_prompt
                                        && !before.conflict
                                        && !next.trim().is_empty()
                                        && next.len() <= 16384,
                                    kylig_ui::Button::new("Сохранить").ghost(),
                                )
                                .clicked();
                        if inline_result && ui.button("Результат").clicked() {
                            self.result_view.select(result::Tab::Summary, Some(task.id));
                        }
                        ui.weak(ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(
                            egui::Modifiers::COMMAND,
                            egui::Key::Enter,
                        )));
                        (launch, saved)
                    })
                    .inner;
                (launch, saved, submit)
            })
            .inner;
        let after = self.composer_guards(task, available, backend, next);
        // A click can land in the same frame as the field changed the draft,
        // so the fresh guards decide whether the save and the send actually fire.
        if save_clicked
            && after.dirty
            && can_edit_prompt
            && !after.conflict
            && !next.trim().is_empty()
            && next.len() <= 16384
        {
            self.request(
                Request::EditPrompt { id: task.id, revision: task.revision, prompt: next.clone() },
                send,
            );
        }
        if let Some(note) = after.blocked.filter(|reason| *reason != "Введите сообщение агенту.")
        {
            ui.add_sized(
                [ui.available_width(), ui.spacing().interact_size.y],
                egui::Label::new(RichText::new(note).small().weak()).halign(Align::Min).wrap(),
            )
            .on_hover_text(note);
        }
        if after.blocked.is_none()
            && (send_clicked || submit)
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
        editor: egui::Id,
        task: &Task,
        next: &mut String,
        editable: bool,
    ) -> bool {
        let submit = shortcut(ui, editor);
        let max_height = ui.text_style_height(&egui::TextStyle::Body) * 8.0 + 8.0;
        egui::ScrollArea::vertical()
            .id_salt("message_field")
            .min_scrolled_height(0.0)
            .max_height(max_height)
            .show(ui, |ui| {
                let hint = if self.reworking.as_ref().is_some_and(|r| r.task == task.id) {
                    "Что нужно доработать?"
                } else if task.runs.is_empty() {
                    "Что поручить агенту?"
                } else {
                    "Сообщение агенту…"
                };
                let field = ui.add_enabled(
                    editable && !self.mutation_pending(),
                    egui::TextEdit::multiline(next)
                        .id(editor)
                        .desired_width(f32::INFINITY)
                        .desired_rows(1)
                        .char_limit(16384)
                        .hint_text(hint)
                        // the composer frame owns the border, the field stays transparent
                        .frame(
                            egui::Frame::new()
                                .fill(egui::Color32::TRANSPARENT)
                                .inner_margin(COMPOSER_FIELD_MARGIN),
                        ),
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
        crate::gui::theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("Параметры задачи");
            ui.weak(format!(
                "Назначен {}",
                BackendKind::parse(&task.backend)
                    .map_or(task.backend.as_str(), |kind| agent_title(kind))
            ));
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
        });
        if self.resolved_scope(snapshot) != self.scope {
            return;
        }
        if self.rules.saved.git.is_some() || task.git.is_some() || task.isolated {
            crate::gui::theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                self.git_controls(ui, task, send);
            });
        }
        if self.selected_live(snapshot).is_some_and(|live| live.active) {
            self.local_panels(ui, snapshot, send);
        }
        crate::gui::theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("Управление задачей");
            self.show_queue_controls(ui, task, next, send);
            self.workflow_controls(ui, task, !available.edit || task.queue.is_some(), send);
        });
        self.next_run_context(ui, task, next, snapshot);
    }

    fn next_run_context(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        next: &str,
        snapshot: &Snapshot,
    ) {
        let open_context = self.result_view.take_context_open();
        let card = crate::gui::theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.strong("Контекст следующего запуска");
            ui.weak(format!(
                "Проект {}   /   Исполнитель {}",
                project_name(&task.project),
                task.backend
            ));
            ui.add(
                egui::Label::new(if task.runs.is_empty() { next } else { &task.prompt })
                    .wrap()
                    .selectable(true),
            );
            let step = task.queue.as_ref().and_then(|q| q.next.as_deref()).unwrap_or(next);
            if !task.runs.is_empty() && !step.trim().is_empty() {
                ui.add(egui::Label::new(step).wrap().selectable(true));
            }
            ui.separator();
            ui.strong("Правила проекта");
            if let Some(topic) = snapshot
                .topics
                .iter()
                .find(|topic| self.scope.as_ref().and_then(|scope| scope.0) == Some(topic.key))
            {
                ui.weak(format!(
                    "Тема Telegram — {}",
                    topic.title.clone().unwrap_or_else(|| format!("#{}", topic.key.thread.0))
                ));
            }
            if self.rules.saved.enabled && !self.rules.saved.text.trim().is_empty() {
                ui.add(egui::Label::new(&self.rules.saved.text).wrap().selectable(true));
            } else {
                ui.weak("Правила выключены или пусты.");
            }
            if let Some(plan) = self.rules.saved.effective_workflow(&super::shared_roles(snapshot))
            {
                ui.weak(format!(
                    "{} › {} › ревью {}",
                    plan.implementer.name(),
                    plan.command,
                    plan.reviewer.name()
                ));
            } else {
                ui.weak("Автоматическая проверка выключена.");
            }
            egui::CollapsingHeader::new("Полный текст запроса")
                .open(open_context.then_some(true))
                .show(ui, |ui| {
                    let mut preview;
                    let task = if task.runs.is_empty() && next != task.prompt {
                        preview = task.clone();
                        next.clone_into(&mut preview.prompt);
                        &preview
                    } else {
                        task
                    };
                    match task.preview_with_rules(
                        (!task.runs.is_empty()).then_some(step),
                        &self.rules.saved,
                    ) {
                        Ok(text) => {
                            ui.add(egui::Label::new(text).wrap().selectable(true));
                        }
                        Err(error) => {
                            ui.weak(error);
                        }
                    }
                });
        });
        if open_context {
            card.response.scroll_to_me(Some(Align::TOP));
        }
    }

    fn review(
        &mut self,
        ui: &mut egui::Ui,
        task: &Task,
        next: &mut String,
        available: &Availability,
        send: &mut impl FnMut(Command),
    ) {
        let failed = task
            .runs
            .last()
            .is_some_and(|run| matches!(run.state, RunState::Failed | RunState::Interrupted));
        if failed
            && !task.archived
            && task.queue.is_none()
            && !matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)
        {
            ui.weak("Перед повторным запуском проверьте результат и опишите следующий шаг.");
            if primary_button(ui, available.edit, "Подготовить повторный запуск").clicked()
            {
                self.result_view.select(result::Tab::Chat, Some(task.id));
                self.reworking = Some(Rework { task: task.id, focus: true });
            }
        }
        if task.status == TaskStatus::NeedsReview && !task.archived && task.queue.is_none() {
            egui::Panel::bottom(ui.scope_id().with("review_actions"))
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
                            .add_enabled(available.edit, kylig_ui::Button::new("На доработку"))
                            .clicked()
                        {
                            self.result_view.select(result::Tab::Chat, Some(task.id));
                            self.reworking = Some(Rework { task: task.id, focus: true });
                        }
                    });
                });
        }
        if self.result_view.tab(task) == result::Tab::Chat {
            return;
        }
        scroll_body(ui, "review_body", false, |ui| {
            if result::View::review_run(task).is_none() {
                ui.weak("Сохранённых попыток пока нет.");
            }
            if let Some(annotated) = self.result_view.show(ui, task, next, available.edit) {
                *next = annotated;
                self.result_view.select(result::Tab::Chat, Some(task.id));
            }
        });
    }
}
