//! The «Темы» tab.

use eframe::egui;
use hub_core::domain::{BackendKind, SessionId, TopicKey};
use hub_core::options::memory_label;
use hub_core::progress::Phase;
use hub_telegram::hub::{TopicState, TopicView};
use std::path::Path;
use tokio::sync::oneshot::error::TryRecvError;

use crate::gui::folder::{Destination, Opener};
use crate::gui::look::{agent_title, topic_url};
use crate::gui::theme;
use crate::supervisor::{Command, Snapshot};
use kylig_ui::{Badge, Button, Chip, InfoBar, ListItem, ProgressRing, Severity, TextField};

#[derive(Default)]
pub struct TopicsView {
    selected: Option<TopicKey>,
    filters_open: bool,
    resetting: Option<TopicKey>,
    deleting: Option<(TopicKey, String)>,
    pending: Option<Pending>,
    notice: Option<(Severity, String)>,
    reveal_progress: bool,
    requested_reset: Option<(TopicKey, SessionId)>,
    requested_stop: Option<TopicKey>,
    filter: TopicFilter,
    opener: Opener,
}

type Reply<T> = tokio::sync::oneshot::Receiver<Result<T, String>>;

enum Pending {
    Sync(Reply<String>),
    Delete(TopicKey, Reply<()>),
    Memory(TopicKey, Reply<()>),
}

impl Pending {
    fn label(&self) -> &'static str {
        match self {
            Self::Sync(_) => "Синхронизируем темы…",
            Self::Delete(_, _) => "Удаляем тему…",
            Self::Memory(_, _) => "Обновляем параметры памяти…",
        }
    }

    fn poll(&mut self) -> Option<(Severity, String)> {
        let reply = match self {
            Self::Sync(rx) => rx.try_recv(),
            Self::Delete(key, rx) => rx
                .try_recv()
                .map(|r| r.map(|()| format!("Тема #{} удалена из Telegram.", key.thread.0))),
            Self::Memory(key, rx) => rx.try_recv().map(|r| {
                r.map(|()| {
                    format!(
                        "Параметр памяти темы #{} сохранён для следующего запуска.",
                        key.thread.0
                    )
                })
            }),
        };
        match reply {
            Ok(Ok(message)) => Some((Severity::Success, message)),
            Ok(Err(error)) => Some((Severity::Error, error)),
            Err(TryRecvError::Closed) => Some(match self {
                Self::Sync(_) => (Severity::Info, "Синхронизация прервана".into()),
                Self::Delete(_, _) => {
                    (Severity::Error, "Связь с ботом прервана. Проверьте тему в Telegram.".into())
                }
                Self::Memory(_, _) => (Severity::Error, "Связь с ботом прервана.".into()),
            }),
            Err(TryRecvError::Empty) => None,
        }
    }
}

#[derive(Default, PartialEq)]
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
        TopicState::Running => "Работает",
        TopicState::Waiting => "Свободна",
    }
}

impl TopicsView {
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.opener.poll(ctx);
        if let Some(error) = self.opener.take_error() {
            self.notice = Some((Severity::Error, error));
        }
        if let Some(pending) = &mut self.pending {
            if let Some(notice) = pending.poll() {
                self.notice = Some(notice);
                self.pending = None;
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
    }

    fn start(&mut self, ui: &egui::Ui, pending: Pending) {
        self.notice = None;
        self.reveal_progress = true;
        self.pending = Some(pending);
        ui.ctx().request_repaint();
    }

    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        self.poll(ui.ctx());
        if !snapshot.topics.is_empty() {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs(1));
        }
        if let Some((key, previous)) = &self.requested_reset {
            let topic = snapshot.topics.iter().find(|topic| topic.key == *key);
            if topic.is_none_or(|topic| {
                topic.state == TopicState::Running
                    || topic.session.session.as_ref() != Some(previous)
            }) {
                if topic.is_some_and(|topic| topic.session.session.is_none()) {
                    self.notice = Some((
                        Severity::Success,
                        format!("Сессия темы #{} сброшена.", key.thread.0),
                    ));
                }
                self.requested_reset = None;
            }
        }
        if let Some(key) = self.requested_stop {
            let topic = snapshot.topics.iter().find(|topic| topic.key == key);
            if topic.is_none_or(|topic| topic.state != TopicState::Running) {
                if topic.is_some_and(|topic| topic.progress.phase() == Phase::Stopped) {
                    self.notice = Some((
                        Severity::Success,
                        format!("Запуск в теме #{} остановлен.", key.thread.0),
                    ));
                }
                self.requested_stop = None;
            }
        }
        egui::ScrollArea::vertical()
            .id_salt(("telegram-topics", self.selected))
            .show(ui, |ui| self.page(ui, snapshot, send));
        // A snapshot can change while a confirmation is open.
        if !snapshot.topics.iter().any(|t| {
            Some(t.key) == self.resetting
                && t.state != TopicState::Running
                && t.session.session.is_some()
        }) {
            self.resetting = None;
        }
        if !snapshot.topics.iter().any(|t| {
            Some(t.key) == self.deleting.as_ref().map(|(key, _)| *key)
                && t.state != TopicState::Running
        }) {
            self.deleting = None;
        }
        self.confirm(ui, snapshot, send);
        self.confirm_delete(ui, send);
    }

    fn page(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        let stopping = self.requested_stop.is_some_and(|key| {
            snapshot.topics.iter().any(|t| t.key == key && t.progress.phase() == Phase::Stopping)
        });
        let progress = self
            .pending
            .as_ref()
            .map(Pending::label)
            .or_else(|| stopping.then_some("Останавливаем запуск…"))
            .or_else(|| self.opener.is_pending().then_some("Ожидание системы…"));
        if let Some(label) = progress {
            let row = ui.horizontal_wrapped(|ui| {
                ui.add(ProgressRing::indeterminate().size(20.0));
                ui.strong(label);
            });
            if std::mem::take(&mut self.reveal_progress)
                && !ui.clip_rect().contains_rect(row.response.rect)
            {
                row.response.scroll_to_me(None);
            }
        }
        let selected = snapshot.topics.iter().find(|topic| Some(topic.key) == self.selected);
        if let Some(topic) = selected {
            if ui.link("‹ Все темы").clicked() {
                self.selected = None;
            }
            ui.heading(name(topic));
            ui.weak(subtitle(topic));
        } else {
            self.selected = None;
            ui.heading("Темы Telegram");
            ui.weak("Групповые обсуждения, привязанные проекты и агентские сессии.");
        }
        ui.add_space(12.0);
        if let Some((severity, message)) = &self.notice {
            let mut open = true;
            InfoBar::new(*severity, message.as_str()).open(&mut open).show(ui);
            if !open {
                self.notice = None;
            }
        }
        ui.add_enabled_ui(progress.is_none(), |ui| {
            if let Some(topic) = selected {
                theme::card(ui).show(ui, |ui| {
                    self.status(ui, topic, send);
                    ui.separator();
                    self.details(ui, topic, send);
                });
            } else {
                self.list(ui, snapshot, send);
            }
        });
    }

    fn list(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        if snapshot.local_only && snapshot.topics.is_empty() {
            theme::card(ui).show(ui, |ui| {
                ui.strong("Telegram отключён");
                ui.label("Вы работаете локально. Чтобы получать задачи из тем группы, подключите бота в разделе «Настройки › Telegram и доступ».");
            });
            return;
        }
        self.filters(ui, send);
        ui.add_space(6.0);
        if snapshot.topics.is_empty() {
            theme::card(ui).show(ui, |ui| {
                ui.strong("В группе пока нет тем");
                ui.label("Темы появятся после первого сообщения боту в группе.");
            });
            return;
        }
        let topics: Vec<_> =
            snapshot.topics.iter().filter(|topic| self.filter.matches(topic)).collect();
        ui.weak(format!("Показано тем — {} из {}", topics.len(), snapshot.topics.len()));
        if topics.is_empty() {
            ui.label("По этим условиям тем нет. Измените поиск или сбросьте фильтры.");
            return;
        }
        for topic in topics {
            ui.push_id(topic.key, |ui| {
                theme::card(ui).show(ui, |ui| {
                    let severity = if topic.state == TopicState::Running {
                        Severity::Info
                    } else {
                        Severity::Success
                    };
                    let heading = ListItem::new(name(topic))
                        .id_salt(topic.key)
                        .subtitle(subtitle(topic))
                        .leading(move |ui| {
                            ui.add(Badge::dot().severity(severity));
                        })
                        .chevron(true)
                        .show(ui);
                    if heading.response.clicked() {
                        self.selected = Some(topic.key);
                    }
                    self.status(ui, topic, send);
                });
            });
        }
    }

    fn filters(&mut self, ui: &mut egui::Ui, send: &mut impl FnMut(Command)) {
        ui.add(
            TextField::new(&mut self.filter.query)
                .id_salt("topic-search")
                .placeholder("Найти тему по названию или каталогу")
                .clearable(true)
                .width(ui.available_width().min(360.0)),
        );
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::new("Синхронизировать").primary())
                .on_hover_text("Проверить темы в Telegram и убрать удалённые там")
                .clicked()
            {
                let (reply, received) = tokio::sync::oneshot::channel();
                self.start(ui, Pending::Sync(received));
                send(Command::SyncTopics { reply });
            }
            if ui.add(Button::new("Фильтры")).clicked() {
                self.filters_open = !self.filters_open;
            }
            if self.filter != TopicFilter::default()
                && ui.add(Button::new("Сбросить фильтры")).clicked()
            {
                self.filter = TopicFilter::default();
            }
        });
        if self.filters_open {
            ui.label("Агент");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
                for value in std::iter::once(None).chain(BackendKind::ALL.map(Some)) {
                    if Chip::new(value.map_or("Все агенты", agent_title))
                        .selected(self.filter.backend == value)
                        .clickable(true)
                        .show(ui)
                        .response
                        .clicked()
                    {
                        self.filter.backend = value;
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                egui::ComboBox::from_label("Состояние")
                    .selected_text(self.filter.state.map_or("Все состояния", state_text))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.filter.state, None, "Все состояния");
                        for state in [TopicState::Running, TopicState::Waiting] {
                            ui.selectable_value(
                                &mut self.filter.state,
                                Some(state),
                                state_text(state),
                            );
                        }
                    });
                egui::ComboBox::from_label("Этап")
                    .selected_text(self.filter.phase.map_or("Все этапы", Phase::label))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.filter.phase, None, "Все этапы");
                        for phase in Phase::ALL {
                            ui.selectable_value(&mut self.filter.phase, Some(phase), phase.label());
                        }
                    });
            });
        }
    }

    fn details(&mut self, ui: &mut egui::Ui, topic: &TopicView, send: &mut impl FnMut(Command)) {
        let running = topic.state == TopicState::Running;
        let path = topic.session.cwd.as_path();
        ui.strong("Параметры и прогресс");
        details(ui, topic, running, path);
        let id = topic.session.session.as_ref().map(hub_core::domain::SessionId::as_str);
        ui.label(format!("Сессия   {}", id.unwrap_or("Ещё не создана")));
        let current = topic.session.preferences.memory;
        ui.label("Память темы");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
            for mode in [None, Some(true), Some(false)] {
                if Chip::new(memory_label(mode))
                    .selected(mode == current)
                    .clickable(true)
                    .show(ui)
                    .response
                    .clicked()
                    && current != mode
                {
                    let (reply, received) = tokio::sync::oneshot::channel();
                    self.start(ui, Pending::Memory(topic.key, received));
                    send(Command::TopicMemory { key: topic.key, value: mode, reply });
                }
            }
        });
        let link = topic_url(topic.key);
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(link.is_some(), Button::new("Открыть в Telegram")).clicked() {
                self.notice = None;
                self.reveal_progress = true;
                self.opener.start(Destination::Topic(topic.key), ui.ctx());
            }
            if ui.add(Button::new("Открыть папку")).clicked() {
                self.notice = None;
                self.reveal_progress = true;
                self.opener.start(Destination::Folder(path.to_owned()), ui.ctx());
            }
            if ui.add_enabled(link.is_some(), Button::new("Копировать ссылку")).clicked()
                && let Some(link) = link
            {
                ui.ctx().copy_text(link);
                self.notice = Some((Severity::Success, "Ссылка на тему скопирована.".into()));
            }
            if let Some(id) = id
                && ui.add(Button::new("Копировать ID сессии")).clicked()
            {
                ui.ctx().copy_text(id.to_owned());
                self.notice = Some((Severity::Success, "Идентификатор сессии скопирован.".into()));
            }
        });
        if !running {
            ui.horizontal_wrapped(|ui| {
                if id.is_some() && ui.add(Button::new("Сбросить сессию…")).clicked()
                {
                    self.resetting = Some(topic.key);
                }
                if topic.key.thread.0 > 1 && ui.add(Button::new("Удалить тему…").danger()).clicked()
                {
                    self.deleting = Some((topic.key, name(topic)));
                }
            });
        }
    }

    fn confirm_delete(&mut self, ui: &egui::Ui, send: &mut impl FnMut(Command)) {
        let Some((key, name)) = self.deleting.clone() else { return };
        let mut open = true;
        theme::dialog(ui, "delete-topic", &mut open, "Удалить тему из Telegram?").show(ui.ctx(), |ui| {
            ui.strong(format!("{name} · #{}", key.thread.0));
            ui.label("Тема и все её сообщения будут удалены для всей группы. Отменить это действие невозможно.");
            ui.horizontal_wrapped(|ui| {
                if ui.add(Button::new("Отмена")).clicked() { self.deleting = None; }
                if ui.add(Button::new("Удалить тему").danger()).clicked() {
                    let (reply, receipt) = tokio::sync::oneshot::channel();
                    self.start(ui, Pending::Delete(key, receipt));
                    self.deleting = None;
                    send(Command::DeleteTopic { key, reply });
                }
            });
        });
        if !open {
            self.deleting = None;
        }
    }

    fn confirm(&mut self, ui: &egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        let Some(key) = self.resetting else { return };
        let Some(topic) = snapshot.topics.iter().find(|t| t.key == key) else { return };
        let mut open = true;
        theme::dialog(ui, "reset-topic", &mut open, "Сбросить сессию темы?").show(ui.ctx(), |ui| {
            ui.strong(format!("{} · #{}", name(topic), key.thread.0));
            ui.label("Следующее поручение начнётся с новой агентской сессии.");
            ui.horizontal_wrapped(|ui| {
                if ui.add(Button::new("Отмена")).clicked() {
                    self.resetting = None;
                }
                if ui.add(Button::new("Сбросить сессию").danger()).clicked() {
                    self.notice = None;
                    self.requested_reset = topic.session.session.clone().map(|id| (key, id));
                    send(Command::ResetTopic(key));
                    self.resetting = None;
                }
            });
        });
        if !open {
            self.resetting = None;
        }
    }
    fn status(&mut self, ui: &mut egui::Ui, topic: &TopicView, send: &mut impl FnMut(Command)) {
        ui.horizontal_wrapped(|ui| {
            ui.strong(state_text(topic.state));
            ui.weak(topic.progress.phase().label());
            if topic.state == TopicState::Running
                && topic.progress.phase() != Phase::Stopping
                && ui.add(Button::new("Остановить")).clicked()
            {
                self.requested_stop = Some(topic.key);
                self.notice = None;
                self.reveal_progress = true;
                send(Command::StopTopic(topic.key));
            }
        });
    }
}

fn subtitle(topic: &TopicView) -> String {
    let path = topic.session.cwd.as_path();
    let project = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
    format!("#{}  ·  {project}  ·  {}", topic.key.thread.0, agent_title(topic.session.backend))
}

fn details(ui: &mut egui::Ui, topic: &TopicView, running: bool, path: &Path) {
    let now = std::time::Instant::now();
    if topic.progress.elapsed(now).is_some() {
        // The status row above already shows the phase.
        for line in topic.progress.describe(now).lines().skip(1) {
            ui.label(line);
        }
    } else {
        ui.label("Сведения о последнем запуске отсутствуют.");
    }
    if running {
        ui.weak(match topic.memory_enabled {
            Some(true) => "Память текущего запуска включена",
            Some(false) => "Память текущего запуска выключена",
            None => "Состояние памяти текущего запуска неизвестно",
        });
    }
    ui.label(format!("Каталог проекта   {}", path.display()));
    for line in topic.parameters.lines() {
        ui.label(line);
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
            )
            .with_session(hub_core::domain::SessionId::parse("session-42")),
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
        frame_size(ctx, view, snapshot, egui::vec2(960.0, 1000.0), events, commands)
    }

    fn frame_size(
        ctx: &egui::Context,
        view: &mut TopicsView,
        snapshot: &Snapshot,
        screen: egui::Vec2,
        events: Vec<egui::Event>,
        commands: &mut Vec<Command>,
    ) -> egui::FullOutput {
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
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

    fn galley_rect(output: &egui::FullOutput, label: &str) -> egui::Rect {
        output
            .shapes
            .iter()
            .rev()
            .find_map(|shape| {
                if let egui::Shape::Text(text) = &shape.shape
                    && text.galley.text() == label
                {
                    Some(text.galley.rect.translate(text.pos.to_vec2()))
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("missing {label}: {:?}", labels(output)))
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
        for chip_text in ["Работает", "Подготовка", "Свободна"] {
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
    fn topic_details_offer_actions_and_confirms_destructive_ones() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let snapshot = Snapshot {
            topics: vec![topic(42, "Проект", BackendKind::Claude, TopicState::Waiting)],
            ..Snapshot::default()
        };
        let mut commands = Vec::new();
        click(&ctx, &mut view, &snapshot, "Проект", &mut commands);
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        for item in [
            "Память темы",
            "Параметры и прогресс",
            "Открыть папку",
            "Копировать ссылку",
            "Копировать ID сессии",
            "Сбросить сессию…",
            "Удалить тему…",
        ] {
            assert!(shown.iter().any(|text| text == item), "missing menu item {item}: {shown:?}");
        }
        click(&ctx, &mut view, &snapshot, "Сбросить сессию…", &mut commands);
        assert!(commands.is_empty(), "reset requires confirmation");
        assert_eq!(view.resetting, Some(snapshot.topics[0].key));
        click(&ctx, &mut view, &snapshot, "Сбросить сессию", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::ResetTopic(key)] if key.thread.0 == 42));
        commands.clear();
        click(&ctx, &mut view, &snapshot, "Удалить тему…", &mut commands);
        assert!(commands.is_empty(), "delete requires confirmation");
        assert!(view.deleting.is_some());
        click(&ctx, &mut view, &snapshot, "Отмена", &mut commands);
        assert!(view.deleting.is_none());
    }

    #[test]
    fn narrow_card_moves_actions_below_the_title() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let mut running = topic(
            7,
            "Очень длинное название темы для узкого окна",
            BackendKind::Claude,
            TopicState::Running,
        );
        running.progress = hub_core::progress::TaskProgress::new(std::time::Instant::now());
        let snapshot = Snapshot { topics: vec![running], ..Snapshot::default() };
        frame_size(
            &ctx,
            &mut view,
            &snapshot,
            egui::vec2(400.0, 640.0),
            Vec::new(),
            &mut Vec::new(),
        );
        let output = frame_size(
            &ctx,
            &mut view,
            &snapshot,
            egui::vec2(400.0, 640.0),
            Vec::new(),
            &mut Vec::new(),
        );
        let title = galley_rect(&output, "Очень длинное название темы для узкого окна");
        let stop = galley_rect(&output, "Остановить");
        assert!(stop.min.y >= title.max.y, "stop must sit below the title: {stop:?} vs {title:?}");
        assert!(title.width() > 150.0, "title must stay readable at 400px: {title:?}");
    }

    #[test]
    fn sync_button_is_inert_while_a_reply_is_pending() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let snapshot = Snapshot {
            topics: vec![topic(42, "Проект", BackendKind::Claude, TopicState::Waiting)],
            ..Snapshot::default()
        };
        let mut commands = Vec::new();
        click(&ctx, &mut view, &snapshot, "Синхронизировать", &mut commands);
        click(&ctx, &mut view, &snapshot, "Синхронизировать", &mut commands);
        let syncs =
            commands.iter().filter(|command| matches!(command, Command::SyncTopics { .. })).count();
        assert_eq!(syncs, 1, "the button must stay inert until the reply arrives");
    }

    #[test]
    fn memory_chips_send_topic_memory_only_on_change() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let snapshot = Snapshot {
            topics: vec![topic(42, "Проект", BackendKind::Claude, TopicState::Waiting)],
            ..Snapshot::default()
        };
        let mut commands = Vec::new();
        click(&ctx, &mut view, &snapshot, "Проект", &mut commands);
        click(&ctx, &mut view, &snapshot, "Наследовать", &mut commands);
        assert!(commands.is_empty(), "re-picking the current mode must not send anything");
        click(&ctx, &mut view, &snapshot, "Включена", &mut commands);
        assert!(
            matches!(commands.as_slice(), [Command::TopicMemory { key, value: Some(true), .. }]
                if key.thread.0 == 42),
            "switching memory must send exactly one TopicMemory",
        );
        assert!(
            labels(&draw(&ctx, &mut view, &snapshot))
                .iter()
                .any(|text| text == "Обновляем параметры памяти…")
        );
        let Some(Command::TopicMemory { reply, .. }) = commands.pop() else {
            panic!("missing memory reply")
        };
        reply.send(Ok(())).unwrap();
        assert!(
            labels(&draw(&ctx, &mut view, &snapshot))
                .iter()
                .any(|text| text == "Параметр памяти темы #42 сохранён для следующего запуска.")
        );
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
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        assert!(shown.iter().any(|text| text == "Показано тем — 1 из 2"));
        assert!(!shown.iter().any(|text| text == "Stopped"));
        click(&ctx, &mut view, &snapshot, "Permission", &mut commands);
        let detail = labels(&draw(&ctx, &mut view, &snapshot));
        assert!(detail.iter().any(|text| text.starts_with("Длительность запуска:")));
        assert!(
            !detail.iter().any(|label| label == "Сбросить сессию…"),
            "a waiting task is still busy"
        );
        click(&ctx, &mut view, &snapshot, "Остановить", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::StopTopic(key)] if key.thread.0 == 7));
    }

    #[test]
    fn topics_offer_search_and_navigation() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let snapshot = Snapshot {
            topics: vec![topic(42, "Бэкенд проекта", BackendKind::Claude, TopicState::Waiting)],
            ..Snapshot::default()
        };
        click(&ctx, &mut view, &snapshot, "Фильтры", &mut Vec::new());
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        for label in ["Найти тему по названию или каталогу", "Все агенты", "Все состояния"]
        {
            assert!(shown.iter().any(|text| text == label), "missing {label}: {shown:?}");
        }
        click(&ctx, &mut view, &snapshot, "Бэкенд проекта", &mut Vec::new());
        let details = labels(&draw(&ctx, &mut view, &snapshot));
        for label in ["Открыть в Telegram", "Копировать ссылку", "Открыть папку", "‹ Все темы"]
        {
            assert!(details.iter().any(|s| s == label));
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
        click(&ctx, &mut view, &snapshot, "Найти тему по названию или каталогу", &mut commands);
        frame(&ctx, &mut view, &snapshot, vec![egui::Event::Text("бэк".to_owned())], &mut commands);
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        assert!(shown.iter().any(|text| text == "Показано тем — 1 из 2"));
        assert!(!shown.iter().any(|text| text == "Фронтенд"));
        click(&ctx, &mut view, &snapshot, "Бэкенд", &mut commands);
        let copied = click(&ctx, &mut view, &snapshot, "Копировать ссылку", &mut commands);
        assert!(copied.platform_output.commands.iter().any(|command| matches!(command,
            egui::OutputCommand::CopyText(text) if text == "https://t.me/c/1234567890/42")));
        click(&ctx, &mut view, &snapshot, "Остановить", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::StopTopic(key)] if key.thread.0 == 42));
        commands.clear();
        snapshot.topics.last_mut().unwrap().state = TopicState::Waiting;
        click(&ctx, &mut view, &snapshot, "Сбросить сессию…", &mut commands);
        assert!(commands.is_empty(), "reset requires confirmation");
        click(&ctx, &mut view, &snapshot, "Сбросить сессию", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::ResetTopic(key)] if key.thread.0 == 42));
        click(&ctx, &mut view, &snapshot, "‹ Все темы", &mut commands);
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
        click(&ctx, &mut view, &snapshot, "Проект", &mut commands);
        click(&ctx, &mut view, &snapshot, "Удалить тему…", &mut commands);
        click(&ctx, &mut view, &snapshot, "Отмена", &mut commands);
        assert!(commands.is_empty());
        click(&ctx, &mut view, &snapshot, "Удалить тему…", &mut commands);
        click(&ctx, &mut view, &snapshot, "Удалить тему", &mut commands);
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
        click(&ctx, &mut view, &snapshot, "Все агенты", &mut commands);
        click(&ctx, &mut view, &snapshot, "Claude Code", &mut commands);
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        assert!(shown.iter().any(|text| text == "Показано тем — 0 из 1"));
        assert!(shown.iter().any(|text| text.starts_with("По этим условиям тем нет")));
        click(&ctx, &mut view, &snapshot, "Сбросить фильтры", &mut commands);
        click(&ctx, &mut view, &snapshot, "Все состояния", &mut commands);
        click(&ctx, &mut view, &snapshot, "Свободна", &mut commands);
        assert!(
            labels(&draw(&ctx, &mut view, &snapshot))
                .iter()
                .any(|text| text == "Показано тем — 0 из 1")
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
        click(&ctx, &mut view, &snapshot, "Проект", &mut Vec::new());
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
        click(&ctx, &mut view, &snapshot, "Alpha", &mut Vec::new());
        let open = labels(&draw(&ctx, &mut view, &snapshot));
        assert!(open.iter().any(|line| line == "Модель: model · тема"));
        assert!(open.iter().any(|line| line == "Режим: plan · проект"));
    }

    #[test]
    fn cleared_session_removes_session_actions_and_confirms_the_observed_reset() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let mut snapshot = Snapshot {
            topics: vec![topic(1, "Главная", BackendKind::Codex, TopicState::Waiting)],
            ..Snapshot::default()
        };
        let mut commands = Vec::new();
        click(&ctx, &mut view, &snapshot, "Главная", &mut commands);
        click(&ctx, &mut view, &snapshot, "Сбросить сессию…", &mut commands);
        click(&ctx, &mut view, &snapshot, "Сбросить сессию", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::ResetTopic(key)] if key.thread.0 == 1));
        assert!(view.notice.is_none());
        snapshot.topics.first_mut().unwrap().session.session = None;
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        assert!(shown.iter().any(|text| text == "Сессия темы #1 сброшена."));
        for action in ["Сбросить сессию…", "Копировать ID сессии", "Удалить тему…"]
        {
            assert!(!shown.iter().any(|text| text == action));
        }
    }
}
