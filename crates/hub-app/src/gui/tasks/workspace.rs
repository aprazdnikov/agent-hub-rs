//! Chat, result review and execution parameters for the selected durable task.
use super::{
    Availability, BackendKind, Command, Effect, LocalBlock, Page, QueueBlock, Request, Rework,
    Scope, Snapshot, Task, TaskStatus, TasksView, result,
};
use crate::gui::{
    look::{agent_title, topic_url},
    theme::{Tone, chip, chip_width, primary_button, tone_colors},
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

// Chip tone for the header state, mirroring `task_state` branch order so the color
// describes exactly the text it shows (spec «Шапка задачи», макет chat.html).
fn task_state_tone(task: &Task) -> Tone {
    if task.archived {
        Tone::Neutral
    } else if task.active() {
        Tone::Accent
    } else if task.queue.is_some()
        || matches!(task.status, TaskStatus::Accepted | TaskStatus::Deferred)
    {
        Tone::Neutral
    } else if task
        .runs
        .last()
        .is_some_and(|run| matches!(run.state, RunState::Failed | RunState::Interrupted))
    {
        Tone::Error
    } else if task.status == TaskStatus::NeedsReview {
        Tone::Warning
    } else {
        Tone::Neutral
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

// Composer frame from chat.html: 1px border, radius 12, surface fill, mockup padding.
const COMPOSER_FRAME_MARGIN: egui::Margin = egui::Margin { left: 12, right: 6, top: 6, bottom: 6 };
// Spec «Чат задачи»: below this chat height bubbles, the header and composer switch to compact.
const COMPACT_CHAT_HEIGHT: f32 = 360.0;
const COMPOSER_FRAME_EXTRA: f32 = 14.0; // margins 6+6 plus the 1px border top and bottom
const COMPOSER_FIELD_MARGIN: egui::Margin = egui::Margin::symmetric(4, 2);

fn ui_font(ui: &egui::Ui, style: &egui::TextStyle) -> egui::FontId {
    ui.style()
        .text_styles
        .get(style)
        .cloned()
        .unwrap_or_else(|| egui::FontId::proportional(ui.text_style_height(style)))
}

// Natural galley size of the heading title, so the header can truncate the title
// to the width left by the state chip instead of pushing the chip out (spec line 65).
fn heading_size(ui: &egui::Ui, text: &str) -> egui::Vec2 {
    let job = egui::WidgetText::from(RichText::new(text).heading().strong()).into_layout_job(
        ui.style(),
        egui::FontSelection::Default,
        Align::Min,
    );
    ui.fonts_mut(|fonts| fonts.layout_job(std::sync::Arc::unwrap_or_clone(job))).size()
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

fn button_label_width(ui: &egui::Ui, label: &str) -> f32 {
    let job = egui::text::LayoutJob::simple(
        label.to_owned(),
        ui_font(ui, &egui::TextStyle::Button),
        egui::Color32::PLACEHOLDER,
        f32::INFINITY,
    );
    let text_width = ui.fonts_mut(|fonts| fonts.layout_job(job)).size().x;
    text_width + 2.0 * ui.spacing().button_padding.x
}

// The bottom panel reserves the composer before it lays out, and the composer must
// split its width before the buttons exist; both derive the same numbers here: the
// field width after the visible action buttons and the height of the bottom row.
fn composer_row(
    ui: &egui::Ui,
    available_width: f32,
    draft: &str,
    send_label: &str,
    show_save: bool,
    show_open: bool,
) -> f32 {
    let spacing_x = ui.spacing().item_spacing.x;
    let button_height =
        ui.spacing().interact_size.y.max(
            ui.text_style_height(&egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.y,
        );
    let mut zone = spacing_x + button_label_width(ui, send_label);
    if show_save {
        zone += spacing_x + button_label_width(ui, "Сохранить");
    }
    if show_open {
        zone += spacing_x + button_label_width(ui, "Открыть результат");
    }
    let field_width = (available_width - zone).max(48.0);
    let field_height = composer_field_height(ui, draft, field_width);
    field_height.max(button_height)
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
            result::Tab::Summary
            | result::Tab::Changes
            | result::Tab::Checks
            | result::Tab::Files
            | result::Tab::History => self.review(ui, task, &mut next, &available, send),
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
        self.chat_header(ui, task, available, next.as_str(), snapshot, send);
        let needs_answer = self
            .selected_live(snapshot)
            .is_some_and(|live| live.active && live.interaction.is_some());
        let has_result =
            result::View::review_run(task).is_some() || task.status == TaskStatus::NeedsReview;
        let button_height = ui.spacing().interact_size.y.max(
            ui.text_style_height(&egui::TextStyle::Button) + 2.0 * ui.spacing().button_padding.y,
        );
        let inline_result = has_result && compact && !needs_answer;
        // design.md:171 — «Открыть результат» duplicates inline only in compact mode; outside
        // it the header primary owns the review state, so the pinned bar keeps the button
        // only for the result-bearing states whose header primary is something else.
        let pinned_result = has_result && !inline_result && task.status != TaskStatus::NeedsReview;
        let result_space =
            if pinned_result { button_height + ui.spacing().item_spacing.y } else { 0.0 };
        let composer_height = if needs_answer {
            (ui.available_height() * 0.60).clamp(130.0, 380.0)
        } else {
            let row_height = composer_row(
                ui,
                // frame margins 12+6 plus the 1px border on both sides
                ui.available_width() - 20.0,
                next,
                if task.runs.is_empty() { "Запустить" } else { "Отправить" },
                task.runs.is_empty() && next != &task.prompt,
                inline_result,
            );
            row_height + COMPOSER_FRAME_EXTRA + ui.spacing().interact_size.y + 12.0
        };
        egui::Panel::bottom(ui.id().with("composer"))
            .exact_size(composer_height + result_space)
            .frame(egui::Frame::NONE)
            .show_separator_line(false)
            .show(ui, |ui| {
                if pinned_result && ui.button("Открыть результат").clicked() {
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
                result::View::transcript(ui, task, &mut self.result_view);
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
        ui.horizontal(|ui| {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // A pending delete replaces the actions and the menu: the user must answer it first.
                if !confirming {
                    self.header_menu(ui, task, available, next, send);
                    self.header_actions(ui, task, available, snapshot, send);
                }
                ui.vertical(|ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let state = task_state(task);
                        let size = heading_size(ui, &task.title);
                        let reserve = chip_width(ui, state) + ui.spacing().item_spacing.x;
                        let title_width = (ui.available_width() - reserve).max(48.0).min(size.x);
                        ui.allocate_ui_with_layout(
                            egui::vec2(title_width, size.y),
                            Layout::left_to_right(Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Label::new(RichText::new(&task.title).heading().strong())
                                        .truncate(),
                                )
                                .on_hover_text(&task.title);
                            },
                        );
                        chip(ui, task_state_tone(task), state);
                    });
                });
            });
        });
        if confirming {
            self.delete_confirm(ui, task, send);
        }
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
        let memory = memory_description(task, &scope, snapshot);
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
                chip(ui, Tone::Accent, "Выполняется");
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
        if task.status == TaskStatus::NeedsReview {
            if primary_button(ui, true, "Открыть результат").clicked() {
                self.result_view.select(result::Tab::Summary, Some(task.id));
            }
            if ui.add_enabled(available.edit, egui::Button::new("Принять")).clicked() {
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
                self.page = Page::Rules;
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
        let frame = egui::Frame::new()
            .fill(ui.visuals().panel_fill)
            .stroke(egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color))
            .corner_radius(12.0)
            .inner_margin(COMPOSER_FRAME_MARGIN);
        let row_height =
            composer_row(ui, ui.available_width(), next, button, before.dirty, inline_result);
        let (send_clicked, save_clicked, submit) = frame
            .show(ui, |ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), row_height),
                    Layout::right_to_left(Align::Max),
                    |ui| {
                        let send_clicked = primary_button(ui, before.blocked.is_none(), button)
                            .on_disabled_hover_text(before.blocked.unwrap_or_default())
                            .clicked();
                        let save_clicked = before.dirty
                            && ui
                                .add_enabled(
                                    can_edit_prompt
                                        && !before.conflict
                                        && !next.trim().is_empty()
                                        && next.len() <= 16384,
                                    egui::Button::new("Сохранить"),
                                )
                                .clicked();
                        if inline_result && ui.button("Открыть результат").clicked()
                        {
                            self.result_view.select(result::Tab::Summary, Some(task.id));
                        }
                        let field = ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), row_height),
                            egui::Layout {
                                main_align: Align::Max,
                                ..egui::Layout::top_down(Align::Min)
                            },
                            |ui| self.message_field(ui, task, next, !first || can_edit_prompt),
                        );
                        (send_clicked, save_clicked, field.inner)
                    },
                )
                .inner
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
        let note = match after.blocked {
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
        task: &Task,
        next: &mut String,
        editable: bool,
    ) -> bool {
        let editor = ui.make_persistent_id("task_message");
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
                    "Что поручить агенту"
                } else {
                    "Сообщение агенту"
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
                "{} › {} › ревью {}",
                plan.implementer.name(),
                plan.command,
                plan.reviewer.name()
            ));
        }
        if ui.button("Правила проекта").clicked() {
            self.page = Page::Rules;
        }
        // The «…» menu can ask to land here with the context already open.
        let open_context = self.result_view.take_context_open();
        egui::CollapsingHeader::new("Контекст следующего запуска")
            .open(open_context.then_some(true))
            .show(ui, |ui| {
                ui.label(format!("Поручение хаба · r{} · {}", task.revision, task.backend));
                ui.small("Каждый запуск создаёт новую CLI-сессию с сохранёнными правилами и контекстом задачи.");
                let mut preview;
                let task = if task.runs.is_empty() && next != task.prompt {
                    preview = task.clone();
                    next.clone_into(&mut preview.prompt);
                    &preview
                } else { task };
                match task
                    .preview_with_rules((!task.runs.is_empty()).then_some(next), &self.rules.saved)
                {
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
