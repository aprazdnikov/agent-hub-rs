//! The «Темы» tab.

use eframe::egui;
use hub_core::domain::{BackendKind, TopicKey};
use hub_core::options::memory_label;
use hub_core::progress::Phase;
use hub_telegram::hub::{TopicState, TopicView};
use std::path::Path;
use tokio::sync::oneshot::error::TryRecvError;

use crate::gui::folder::{Destination, Opener};
use crate::gui::look::{short_session, topic_url};
use crate::gui::theme::{self, Tone};
use crate::supervisor::{Command, Snapshot};

#[derive(Default)]
pub struct TopicsView {
    expanded: Option<TopicKey>,
    filters_open: bool,
    resetting: Option<TopicKey>,
    deleting: Option<(TopicKey, String)>,
    deletion_result: Option<(TopicKey, tokio::sync::oneshot::Receiver<Result<(), String>>)>,
    deletion_notice: Option<Result<String, String>>,
    sync_reply: Option<tokio::sync::oneshot::Receiver<Result<String, String>>>,
    sync_notice: Option<SyncNotice>,
    memory_reply: Option<tokio::sync::oneshot::Receiver<Result<(), String>>>,
    memory_error: Option<String>,
    filter: TopicFilter,
    opener: Opener,
}

/// One-line notice for a «Синхронизировать» request. The hub going away mid-sync
/// is not a failed sync, so `Interrupted` renders as a neutral notice, not an error.
enum SyncNotice {
    Done(String),
    Rejected(String),
    Interrupted,
}

#[derive(Default)]
struct TopicFilter {
    query: String,
    backend: Option<BackendKind>,
    state: Option<TopicState>,
    phase: Option<Phase>,
}

impl TopicFilter {
    fn matches(&self, topic: &TopicView) -> bool {
        let query = self.query.trim().to_lowercase();
        self.backend.is_none_or(|kind| topic.session.backend == kind)
            && self.state.is_none_or(|state| topic.state == state)
            && self.phase.is_none_or(|phase| topic.progress.phase() == phase)
            && (name(topic).to_lowercase().contains(&query)
                || topic.session.cwd.as_path().to_string_lossy().to_lowercase().contains(&query))
    }
}

fn name(topic: &TopicView) -> String {
    topic.title.clone().unwrap_or_else(|| format!("#{}", topic.key.thread.0))
}

fn state_text(state: TopicState) -> &'static str {
    match state {
        TopicState::Running => "работает",
        TopicState::Waiting => "свободна",
    }
}

impl TopicsView {
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.opener.poll(ctx);
        let mut waiting = false;
        if let Some(receipt) = &mut self.sync_reply {
            match receipt.try_recv() {
                Ok(Ok(message)) => self.sync_notice = Some(SyncNotice::Done(message)),
                Ok(Err(message)) => self.sync_notice = Some(SyncNotice::Rejected(message)),
                Err(TryRecvError::Closed) => self.sync_notice = Some(SyncNotice::Interrupted),
                Err(TryRecvError::Empty) => waiting = true,
            }
            if !waiting {
                self.sync_reply = None;
            }
        }
        if let Some((key, receipt)) = self.deletion_result.as_mut() {
            match receipt.try_recv() {
                Ok(result) => {
                    self.deletion_notice = Some(
                        result.map(|()| format!("Тема #{} удалена из Telegram.", key.thread.0)),
                    );
                    self.deletion_result = None;
                }
                Err(TryRecvError::Empty) => waiting = true,
                Err(TryRecvError::Closed) => {
                    self.deletion_notice =
                        Some(Err("Связь с ботом прервана. Проверьте тему в Telegram.".into()));
                    self.deletion_result = None;
                }
            }
        }
        if waiting {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        ui.horizontal(|ui| {
            ui.heading("Темы Telegram");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(self.sync_reply.is_none(), egui::Button::new("Синхронизировать"))
                    .on_hover_text("Проверить темы в Telegram и убрать удалённые там")
                    .clicked()
                {
                    let (reply, received) = tokio::sync::oneshot::channel();
                    self.sync_reply = Some(received);
                    self.sync_notice = None;
                    send(Command::SyncTopics { reply });
                }
            });
        });
        ui.add_space(8.0);
        if !snapshot.topics.is_empty() {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        }
        self.poll(ui.ctx());
        if self.deletion_result.is_some() {
            ui.label("Удаление темы…");
        }
        match &self.deletion_notice {
            Some(Ok(message)) => {
                ui.label(message);
            }
            Some(Err(message)) => {
                ui.colored_label(ui.visuals().error_fg_color, message);
            }
            None => {}
        }
        match &self.sync_notice {
            Some(SyncNotice::Done(message)) => {
                ui.label(message);
            }
            Some(SyncNotice::Rejected(message)) => {
                ui.colored_label(ui.visuals().error_fg_color, message);
            }
            Some(SyncNotice::Interrupted) => {
                ui.label("Синхронизация прервана");
            }
            None => {}
        }
        if let Some(reply) = &mut self.memory_reply {
            match reply.try_recv() {
                Ok(result) => {
                    self.memory_error = result.err();
                    self.memory_reply = None;
                }
                Err(TryRecvError::Closed) => {
                    self.memory_error = Some("Связь с ботом прервана.".into());
                    self.memory_reply = None;
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        if let Some(error) = &self.memory_error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if !snapshot.topics.is_empty() {
            self.filters(ui);
        }
        if let Some(error) = self.opener.error() {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if self.opener.is_pending() {
            ui.label("Ожидание системы…");
        }
        let topics: Vec<_> =
            snapshot.topics.iter().filter(|topic| self.filter.matches(topic)).collect();
        if snapshot.topics.is_empty() {
            ui.label("Темы появятся, когда бот получит сообщение в группе с темами.");
        } else {
            if topics.len() != snapshot.topics.len() {
                ui.weak(format!("Темы: {} из {}", topics.len(), snapshot.topics.len()));
            }
            if topics.is_empty() {
                ui.label("Темы не найдены. Измените поиск или сбросьте фильтры.");
            }
            egui::ScrollArea::vertical().id_salt("telegram_topic_rows").show(ui, |ui| {
                for topic in topics {
                    ui.push_id(topic.key, |ui| self.card(ui, topic, send));
                    ui.add_space(4.0);
                }
            });
        }
        self.confirm(ui.ctx(), send);
        self.confirm_delete(ui.ctx(), send);
    }

    fn filters(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter.query)
                    .hint_text("Поиск по названию или каталогу")
                    .desired_width(250.0),
            );
            if ui.selectable_label(self.filters_open, "Фильтры").clicked() {
                self.filters_open = !self.filters_open;
            }
            if (self.filter.backend.is_some()
                || self.filter.phase.is_some()
                || self.filter.state.is_some()
                || !self.filter.query.is_empty())
                && ui.button("Сбросить фильтры").clicked()
            {
                self.filter = TopicFilter::default();
            }
        });
        if !self.filters_open {
            return;
        }
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("topic-backend")
                .selected_text(self.filter.backend.map_or("Все бэкенды", BackendKind::name))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filter.backend, None, "Все бэкенды");
                    for backend in BackendKind::ALL {
                        ui.selectable_value(
                            &mut self.filter.backend,
                            Some(backend),
                            backend.name(),
                        );
                    }
                });
            egui::ComboBox::from_id_salt("topic-state")
                .selected_text(self.filter.state.map_or("Все состояния", state_text))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filter.state, None, "Все состояния");
                    ui.selectable_value(
                        &mut self.filter.state,
                        Some(TopicState::Running),
                        "работает",
                    );
                    ui.selectable_value(
                        &mut self.filter.state,
                        Some(TopicState::Waiting),
                        "свободна",
                    );
                });
            egui::ComboBox::from_id_salt("topic-phase")
                .selected_text(self.filter.phase.map_or("Все этапы", Phase::label))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.filter.phase, None, "Все этапы");
                    for phase in Phase::ALL {
                        ui.selectable_value(&mut self.filter.phase, Some(phase), phase.label());
                    }
                });
        });
    }

    fn card(&mut self, ui: &mut egui::Ui, topic: &TopicView, send: &mut impl FnMut(Command)) {
        let path = topic.session.cwd.as_path();
        let project = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
        let running = topic.state == TopicState::Running;
        let phase = topic.progress.phase();
        let tone = if running { Tone::Accent } else { Tone::Neutral };
        let url = topic_url(topic.key);
        let open = self.expanded == Some(topic.key);
        theme::card(ui).show(ui, |ui| {
            ui.horizontal(|ui| {
                icon_plate(ui, if running { "•" } else { "○" }, tone);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.menu(ui, topic, send);
                    if ui
                        .add_enabled(url.is_some(), egui::Button::new("Telegram"))
                        .on_hover_text("Открыть тему в Telegram")
                        .clicked()
                    {
                        self.opener.start(Destination::Topic(topic.key), ui.ctx());
                    }
                    if running && ui.button("Остановить").clicked() {
                        send(Command::StopTopic(topic.key));
                    }
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            let title = name(topic);
                            ui.strong(&title);
                            theme::chip(ui, tone, state_text(topic.state));
                            if running && phase != Phase::Unknown {
                                theme::chip(ui, Tone::Neutral, phase.label());
                            }
                        });
                        ui.weak(format!("{project} · {}", topic.session.backend.name()))
                            .on_hover_text(path.display().to_string());
                    });
                });
            });
            if open {
                ui.separator();
                details(ui, topic, running, phase, path);
            }
        });
    }

    fn menu(&mut self, ui: &mut egui::Ui, topic: &TopicView, send: &mut impl FnMut(Command)) {
        let running = topic.state == TopicState::Running;
        let path = topic.session.cwd.as_path();
        let url = topic_url(topic.key);
        let memory_busy = self.memory_reply.is_some();
        let current = topic.session.preferences.memory;
        let session_id = topic.session.session.as_ref().map(|id| id.as_str().to_owned());
        let _ = ui.menu_button("…", |ui| {
            ui.spacing_mut().button_padding = egui::vec2(10.0, 5.0);
            egui::containers::menu::SubMenuButton::from_button(
                egui::Button::new("Память темы").right_text(memory_label(current)),
            )
            .ui(ui, |ui| {
                for mode in [None, Some(true), Some(false)] {
                    let label = egui::Button::selectable(current == mode, memory_label(mode));
                    let picked = ui.add_enabled(!memory_busy, label).clicked();
                    if picked && mode != current {
                        let (reply, received) = tokio::sync::oneshot::channel();
                        self.memory_reply = Some(received);
                        self.memory_error = None;
                        send(Command::TopicMemory { key: topic.key, value: mode, reply });
                        ui.close();
                    }
                }
            });
            if ui.button("Параметры и прогресс").clicked() {
                self.expanded = (self.expanded != Some(topic.key)).then_some(topic.key);
                ui.close();
            }
            if ui.button("Открыть папку").clicked() {
                self.opener.start(Destination::Folder(path.to_owned()), ui.ctx());
                ui.close();
            }
            if ui.add_enabled(url.is_some(), egui::Button::new("Скопировать ссылку")).clicked()
                && let Some(link) = &url
            {
                ui.ctx().copy_text(link.clone());
            }
            let mut copy_session = egui::Button::new("Скопировать session_id");
            if let Some(id) = &session_id {
                copy_session = copy_session.right_text(short_session(id));
            }
            if ui.add_enabled(session_id.is_some(), copy_session).clicked()
                && let Some(id) = &session_id
            {
                ui.ctx().copy_text(id.clone());
            }
            ui.separator();
            if ui.add_enabled(!running, egui::Button::new("Сбросить сессию…")).clicked()
            {
                self.resetting = Some(topic.key);
                ui.close();
            }
            let danger =
                egui::RichText::new("Удалить тему…").color(theme::tone_colors(ui, Tone::Error).1);
            if ui
                .add_enabled(
                    !running && topic.key.thread.0 > 1 && self.deletion_result.is_none(),
                    egui::Button::new(danger),
                )
                .clicked()
            {
                self.deleting = Some((topic.key, name(topic)));
                ui.close();
            }
        });
    }

    fn confirm_delete(&mut self, ctx: &egui::Context, send: &mut impl FnMut(Command)) {
        let Some((key, name)) = self.deleting.clone() else { return };
        let modal = egui::Modal::new(egui::Id::new("delete-topic")).show(ctx, |ui| {
            ui.heading("Удалить тему из Telegram?");
            ui.label(format!("Тема «{name}» (#{}) и все её сообщения будут удалены для всей группы. Отменить это действие невозможно.", key.thread.0));
            ui.horizontal(|ui| {
                if ui.button("Отмена").clicked() {
                    self.deleting = None;
                }
                if ui.button("Удалить из Telegram").clicked() {
                    let (reply, receipt) = tokio::sync::oneshot::channel();
                    self.deletion_result = Some((key, receipt));
                    self.deletion_notice = None;
                    self.deleting = None;
                    send(Command::DeleteTopic { key, reply });
                }
            });
        });
        if modal.should_close() {
            self.deleting = None;
        }
    }

    fn confirm(&mut self, ctx: &egui::Context, send: &mut impl FnMut(Command)) {
        let Some(key) = self.resetting else { return };
        let modal = egui::Modal::new(egui::Id::new("reset")).show(ctx, |ui| {
            ui.label("Сбросить контекст темы? Агент начнёт следующую задачу с чистого листа.");
            ui.horizontal(|ui| {
                if ui.button("Сбросить").clicked() {
                    send(Command::ResetTopic(key));
                    self.resetting = None;
                }
                if ui.button("Отмена").clicked() {
                    self.resetting = None;
                }
            });
        });
        if modal.should_close() {
            self.resetting = None;
        }
    }
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

fn details(ui: &mut egui::Ui, topic: &TopicView, running: bool, phase: Phase, path: &Path) {
    if running {
        ui.weak(match topic.memory_enabled {
            Some(true) => "Память текущего запуска включена",
            Some(false) => "Память текущего запуска выключена",
            None => "Состояние памяти текущего запуска неизвестно",
        });
    }
    ui.label(path.display().to_string());
    for line in topic.parameters.lines() {
        ui.label(line);
    }
    if phase != Phase::Unknown {
        for line in topic.progress.describe(std::time::Instant::now()).lines() {
            ui.small(line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::domain::{AbsolutePath, BackendKind, ChatId, ThreadId, TopicSession};
    use hub_telegram::hub::TopicView;

    fn topic(thread: i32, name: &str, backend: BackendKind, state: TopicState) -> TopicView {
        TopicView {
            memory_enabled: None,
            progress: hub_core::progress::TaskProgress::default(),
            parameters: String::new(),
            key: TopicKey { chat: ChatId(-1_001_234_567_890), thread: ThreadId(thread) },
            title: Some(name.to_owned()),
            session: TopicSession::fresh(
                backend,
                AbsolutePath::new(std::env::temp_dir().join("Проект с пробелами")).unwrap(),
            ),
            state,
        }
    }

    fn draw(ctx: &egui::Context, view: &mut TopicsView, snapshot: &Snapshot) -> egui::FullOutput {
        frame(ctx, view, snapshot, Vec::new(), &mut Vec::new())
    }

    fn frame(
        ctx: &egui::Context,
        view: &mut TopicsView,
        snapshot: &Snapshot,
        events: Vec<egui::Event>,
        commands: &mut Vec<Command>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 640.0),
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

    fn click(
        ctx: &egui::Context,
        view: &mut TopicsView,
        snapshot: &Snapshot,
        label: &str,
        commands: &mut Vec<Command>,
    ) -> egui::FullOutput {
        draw(ctx, view, snapshot);
        let output = draw(ctx, view, snapshot);
        let position = output
            .shapes
            .iter()
            .rev()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.text() == label
                {
                    Some(text.galley.rect.translate(text.pos.to_vec2()).center())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("missing {label}: {:?}", labels(&output)));
        let mut output = output;
        for pressed in [true, false] {
            output = frame(
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
        output
    }

    fn labels(output: &egui::FullOutput) -> Vec<String> {
        output
            .shapes
            .iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape {
                    Some(text.galley.text().to_owned())
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn topic_card_shows_state_and_phase_chips() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let mut running = topic(7, "Рабочая", BackendKind::Claude, TopicState::Running);
        running.progress = hub_core::progress::TaskProgress::new(std::time::Instant::now());
        let waiting = topic(8, "Свободная", BackendKind::Codex, TopicState::Waiting);
        let snapshot = Snapshot { topics: vec![running, waiting], ..Snapshot::default() };
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        for chip_text in ["работает", "Подготовка", "свободна"] {
            assert!(
                shown.iter().any(|text| text == chip_text),
                "missing chip {chip_text}: {shown:?}"
            );
        }
    }

    #[test]
    fn sync_button_sends_command_and_notifies_result() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let snapshot = Snapshot {
            topics: vec![topic(42, "Проект", BackendKind::Claude, TopicState::Waiting)],
            ..Snapshot::default()
        };
        let mut commands = Vec::new();
        for (outcome, expected) in [
            (Ok("Все темы актуальны".to_owned()), "Все темы актуальны"),
            (
                Err("Бот занят или остановлен. Повторите синхронизацию.".to_owned()),
                "Бот занят или остановлен. Повторите синхронизацию.",
            ),
        ] {
            click(&ctx, &mut view, &snapshot, "Синхронизировать", &mut commands);
            let Some(Command::SyncTopics { reply }) = commands.pop() else {
                panic!("missing sync command")
            };
            reply.send(outcome).unwrap();
            let shown = labels(&draw(&ctx, &mut view, &snapshot));
            assert!(
                shown.iter().any(|text| text == expected),
                "missing notice {expected}: {shown:?}"
            );
        }
        click(&ctx, &mut view, &snapshot, "Синхронизировать", &mut commands);
        let Some(Command::SyncTopics { reply }) = commands.pop() else {
            panic!("missing sync command after hub shutdown")
        };
        drop(reply);
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        assert!(
            shown.iter().any(|text| text == "Синхронизация прервана"),
            "a dropped reply must render as a neutral notice: {shown:?}"
        );
    }

    #[test]
    fn topic_menu_offers_actions_and_confirms_destructive_ones() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let snapshot = Snapshot {
            topics: vec![topic(42, "Проект", BackendKind::Claude, TopicState::Waiting)],
            ..Snapshot::default()
        };
        let mut commands = Vec::new();
        click(&ctx, &mut view, &snapshot, "…", &mut commands);
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        for item in [
            "Память темы",
            "Параметры и прогресс",
            "Открыть папку",
            "Скопировать ссылку",
            "Скопировать session_id",
            "Сбросить сессию…",
            "Удалить тему…",
        ] {
            assert!(shown.iter().any(|text| text == item), "missing menu item {item}: {shown:?}");
        }
        click(&ctx, &mut view, &snapshot, "Сбросить сессию…", &mut commands);
        assert!(commands.is_empty(), "reset requires confirmation");
        assert_eq!(view.resetting, Some(snapshot.topics[0].key));
        click(&ctx, &mut view, &snapshot, "Сбросить", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::ResetTopic(key)] if key.thread.0 == 42));
        commands.clear();
        click(&ctx, &mut view, &snapshot, "…", &mut commands);
        click(&ctx, &mut view, &snapshot, "Удалить тему…", &mut commands);
        assert!(commands.is_empty(), "delete requires confirmation");
        assert!(view.deleting.is_some());
        click(&ctx, &mut view, &snapshot, "Отмена", &mut commands);
        assert!(view.deleting.is_none());
    }

    #[test]
    fn detailed_phase_filter_keeps_waiting_tasks_busy_and_stop_targets_the_topic() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let mut waiting = topic(7, "Permission", BackendKind::Claude, TopicState::Running);
        let now = std::time::Instant::now();
        waiting.progress = hub_core::progress::TaskProgress::new(now);
        waiting.progress.wait_started(hub_core::progress::WaitKind::Approval, now);
        let mut stopped = topic(8, "Stopped", BackendKind::Codex, TopicState::Waiting);
        stopped.progress = hub_core::progress::TaskProgress::new(now);
        stopped.progress.finish(true, now);
        let snapshot = Snapshot { topics: vec![waiting, stopped], ..Snapshot::default() };
        let mut commands = Vec::new();
        click(&ctx, &mut view, &snapshot, "Фильтры", &mut commands);
        click(&ctx, &mut view, &snapshot, "Все этапы", &mut commands);
        click(&ctx, &mut view, &snapshot, "Ожидает разрешения", &mut commands);
        click(&ctx, &mut view, &snapshot, "…", &mut commands);
        click(&ctx, &mut view, &snapshot, "Параметры и прогресс", &mut commands);
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        assert!(shown.iter().any(|text| text == "Темы: 1 из 2"));
        assert!(!shown.iter().any(|text| text == "Stopped"));
        assert!(shown.iter().any(|text| text.starts_with("Длительность запуска:")));
        click(&ctx, &mut view, &snapshot, "…", &mut commands);
        click(&ctx, &mut view, &snapshot, "Сбросить сессию…", &mut commands);
        assert!(view.resetting.is_none(), "a waiting task is still busy");
        click(&ctx, &mut view, &snapshot, "…", &mut commands);
        click(&ctx, &mut view, &snapshot, "Остановить", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::StopTopic(key)] if key.thread.0 == 7));
    }

    #[test]
    fn topics_offer_search_and_navigation() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let snapshot = Snapshot {
            topics: vec![topic(42, "Бэкенд", BackendKind::Claude, TopicState::Waiting)],
            ..Snapshot::default()
        };
        click(&ctx, &mut view, &snapshot, "Фильтры", &mut Vec::new());
        click(&ctx, &mut view, &snapshot, "…", &mut Vec::new());
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        for label in [
            "Поиск по названию или каталогу",
            "Все бэкенды",
            "Все состояния",
            "Telegram",
            "Скопировать ссылку",
            "Открыть папку",
        ] {
            assert!(shown.iter().any(|text| text == label), "missing {label}: {shown:?}");
        }
    }

    #[rstest::rstest]
    #[case("бЭКЕНД", None, None, true)]
    #[case("  ПРОЕКТ С  ", None, None, true)]
    #[case("нет совпадения", None, None, false)]
    #[case("бэкенд", Some(BackendKind::Claude), Some(TopicState::Running), true)]
    #[case("бэкенд", Some(BackendKind::Codex), Some(TopicState::Running), false)]
    #[case("бэкенд", Some(BackendKind::Claude), Some(TopicState::Waiting), false)]
    fn search_combines_text_backend_and_state(
        #[case] query: &str,
        #[case] backend: Option<BackendKind>,
        #[case] state: Option<TopicState>,
        #[case] expected: bool,
    ) {
        let filter = TopicFilter { query: query.to_owned(), backend, state, phase: None };
        assert_eq!(
            filter.matches(&topic(42, "Бэкенд", BackendKind::Claude, TopicState::Running)),
            expected
        );
    }

    #[test]
    fn an_unnamed_topic_can_be_found_by_its_number() {
        let mut unnamed = topic(42, "", BackendKind::Claude, TopicState::Waiting);
        unnamed.title = None;
        assert!(
            TopicFilter { query: "#42".to_owned(), ..TopicFilter::default() }.matches(&unnamed)
        );
        assert!(
            !TopicFilter { query: "#43".to_owned(), ..TopicFilter::default() }.matches(&unnamed)
        );
    }

    #[test]
    fn search_copy_stop_and_reset_target_the_filtered_topic() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let mut snapshot = Snapshot {
            topics: vec![
                topic(7, "Фронтенд", BackendKind::Claude, TopicState::Waiting),
                topic(42, "Бэкенд", BackendKind::Codex, TopicState::Running),
            ],
            ..Snapshot::default()
        };
        let mut commands = Vec::new();
        click(&ctx, &mut view, &snapshot, "Поиск по названию или каталогу", &mut commands);
        frame(&ctx, &mut view, &snapshot, vec![egui::Event::Text("бэк".to_owned())], &mut commands);
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        assert!(shown.iter().any(|text| text == "Темы: 1 из 2"));
        assert!(!shown.iter().any(|text| text == "Фронтенд"));
        click(&ctx, &mut view, &snapshot, "…", &mut commands);
        let copied = click(&ctx, &mut view, &snapshot, "Скопировать ссылку", &mut commands);
        assert!(copied.platform_output.commands.iter().any(|command| matches!(command,
            egui::OutputCommand::CopyText(text) if text == "https://t.me/c/1234567890/42")));
        click(&ctx, &mut view, &snapshot, "Остановить", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::StopTopic(key)] if key.thread.0 == 42));
        commands.clear();
        snapshot.topics.last_mut().unwrap().state = TopicState::Waiting;
        click(&ctx, &mut view, &snapshot, "…", &mut commands);
        click(&ctx, &mut view, &snapshot, "Сбросить сессию…", &mut commands);
        assert!(commands.is_empty(), "reset requires confirmation");
        click(&ctx, &mut view, &snapshot, "Сбросить", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::ResetTopic(key)] if key.thread.0 == 42));
        click(&ctx, &mut view, &snapshot, "Сбросить фильтры", &mut commands);
        assert!(labels(&draw(&ctx, &mut view, &snapshot)).iter().any(|text| text == "Фронтенд"));
    }

    #[test]
    fn topic_deletion_requires_confirmation_and_reports_result() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let mut snapshot = Snapshot {
            topics: vec![topic(42, "Проект", BackendKind::Claude, TopicState::Waiting)],
            ..Snapshot::default()
        };
        let mut commands = Vec::new();
        click(&ctx, &mut view, &snapshot, "…", &mut commands);
        click(&ctx, &mut view, &snapshot, "Удалить тему…", &mut commands);
        click(&ctx, &mut view, &snapshot, "Отмена", &mut commands);
        assert!(commands.is_empty());
        click(&ctx, &mut view, &snapshot, "…", &mut commands);
        click(&ctx, &mut view, &snapshot, "Удалить тему…", &mut commands);
        click(&ctx, &mut view, &snapshot, "Удалить из Telegram", &mut commands);
        let Some(Command::DeleteTopic { key, reply }) = commands.pop() else {
            panic!("missing confirmed deletion")
        };
        assert_eq!(key, snapshot.topics[0].key);
        snapshot.topics.clear();
        reply.send(Ok(())).unwrap();
        assert!(
            labels(&draw(&ctx, &mut view, &snapshot))
                .iter()
                .any(|text| text.contains("Тема #42 удалена"))
        );
    }

    #[test]
    fn filters_survive_refresh_and_empty_results_can_be_reset() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let snapshot = Snapshot {
            topics: vec![topic(42, "Бэкенд", BackendKind::Codex, TopicState::Running)],
            ..Snapshot::default()
        };
        let mut commands = Vec::new();
        click(&ctx, &mut view, &snapshot, "Фильтры", &mut commands);
        click(&ctx, &mut view, &snapshot, "Все бэкенды", &mut commands);
        click(&ctx, &mut view, &snapshot, "claude", &mut commands);
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        assert!(shown.iter().any(|text| text == "Темы: 0 из 1"));
        assert!(shown.iter().any(|text| text.starts_with("Темы не найдены")));
        click(&ctx, &mut view, &snapshot, "Сбросить фильтры", &mut commands);
        click(&ctx, &mut view, &snapshot, "Все состояния", &mut commands);
        click(&ctx, &mut view, &snapshot, "свободна", &mut commands);
        assert!(
            labels(&draw(&ctx, &mut view, &snapshot)).iter().any(|text| text == "Темы: 0 из 1")
        );
        assert!(commands.is_empty());
    }

    #[test]
    fn opening_a_deleted_directory_shows_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let mut deleted = topic(42, "Проект", BackendKind::Claude, TopicState::Waiting);
        deleted.session.cwd = AbsolutePath::new(temp.path().join("Удалён")).unwrap();
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let snapshot = Snapshot { topics: vec![deleted], ..Snapshot::default() };
        click(&ctx, &mut view, &snapshot, "…", &mut Vec::new());
        click(&ctx, &mut view, &snapshot, "Открыть папку", &mut Vec::new());
        let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let shown = labels(&draw(&ctx, &mut view, &snapshot));
            if shown
                .iter()
                .any(|text| text.contains("Не удалось открыть") && text.contains("Удалён"))
            {
                break;
            }
            assert!(std::time::Instant::now() < end, "no open error: {shown:?}");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn topic_details_show_launch_parameters_only_for_the_expanded_topic() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let mut configured = topic(42, "Alpha", BackendKind::Claude, TopicState::Waiting);
        configured.parameters = "Модель: model · тема\nРежим: plan · проект".to_owned();
        let snapshot = Snapshot { topics: vec![configured], ..Snapshot::default() };
        draw(&ctx, &mut view, &snapshot);
        let closed = labels(&draw(&ctx, &mut view, &snapshot));
        assert!(!closed.iter().any(|line| line.contains("Модель: model")));
        click(&ctx, &mut view, &snapshot, "…", &mut Vec::new());
        click(&ctx, &mut view, &snapshot, "Параметры и прогресс", &mut Vec::new());
        let open = labels(&draw(&ctx, &mut view, &snapshot));
        assert!(open.iter().any(|line| line == "Модель: model · тема"));
        assert!(open.iter().any(|line| line == "Режим: plan · проект"));
    }
}
