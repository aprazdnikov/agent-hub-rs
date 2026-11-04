//! The «Темы» tab.

use eframe::egui;
use hub_core::domain::{BackendKind, TopicKey};
use hub_telegram::hub::{TopicState, TopicView};

use crate::gui::folder::{Destination, Opener};
use crate::gui::look::{short_session, topic_url};
use crate::supervisor::{Command, Snapshot};

#[derive(Default)]
pub struct TopicsView {
    resetting: Option<TopicKey>,
    filter: TopicFilter,
    opener: Opener,
}

#[derive(Default)]
struct TopicFilter {
    query: String,
    backend: Option<BackendKind>,
    state: Option<TopicState>,
}

impl TopicFilter {
    fn matches(&self, topic: &TopicView) -> bool {
        let query = self.query.trim().to_lowercase();
        self.backend.is_none_or(|kind| topic.session.backend == kind)
            && self.state.is_none_or(|state| topic.state == state)
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
    }

    pub fn show(&mut self, ui: &mut egui::Ui, snapshot: &Snapshot, send: &mut impl FnMut(Command)) {
        self.opener.poll(ui.ctx());
        self.filters(ui);
        if let Some(error) = self.opener.error() {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if self.opener.is_pending() {
            ui.label("Ожидание системы…");
        }
        let topics: Vec<_> =
            snapshot.topics.iter().filter(|topic| self.filter.matches(topic)).collect();
        ui.label(format!("Темы: {} из {}", topics.len(), snapshot.topics.len()));
        if snapshot.topics.is_empty() {
            ui.label("Тем пока нет: они появляются, когда бот запущен и в группе есть сообщения.");
        } else if topics.is_empty() {
            ui.label("Темы не найдены. Измените поиск или сбросьте фильтры.");
        } else {
            egui::ScrollArea::both().show(ui, |ui| {
                egui::Grid::new("topics").num_columns(6).striped(true).show(ui, |ui| {
                    for header in ["Тема", "Бэкенд", "Каталог", "Сессия", "Статус", ""]
                    {
                        ui.strong(header);
                    }
                    ui.end_row();
                    for topic in topics {
                        self.row(ui, topic, send);
                        ui.end_row();
                    }
                });
            });
        }
        self.confirm(ui.ctx(), send);
    }

    fn filters(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.filter.query)
                    .hint_text("Поиск по названию или каталогу")
                    .desired_width(250.0),
            );
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
            if ui.button("Сбросить фильтры").clicked() {
                self.filter = TopicFilter::default();
            }
        });
        ui.separator();
    }

    fn row(&mut self, ui: &mut egui::Ui, topic: &TopicView, send: &mut impl FnMut(Command)) {
        ui.push_id((topic.key, "name"), |ui| {
            ui.vertical(|ui| {
                ui.set_width(155.0);
                let title = name(topic);
                ui.add(egui::Label::new(&title).truncate()).on_hover_text(&title);
                ui.horizontal(|ui| {
                    let url = topic_url(topic.key);
                    let telegram = ui.add_enabled(url.is_some(), egui::Button::new("Telegram"));
                    let copy = ui.add_enabled(url.is_some(), egui::Button::new("Ссылка"));
                    if let Some(url) = url {
                        if telegram.on_hover_text("Открыть тему в Telegram").clicked() {
                            self.opener.start(Destination::Topic(topic.key), ui.ctx());
                        }
                        if copy.on_hover_text("Скопировать ссылку на тему").clicked()
                        {
                            ui.ctx().copy_text(url);
                        }
                    } else {
                        telegram.on_disabled_hover_text("Для этой темы нельзя построить ссылку");
                        copy.on_disabled_hover_text("Для этой темы нельзя построить ссылку");
                    }
                });
            })
        });
        ui.label(topic.session.backend.name());
        ui.push_id((topic.key, "path"), |ui| {
            ui.vertical(|ui| {
                ui.set_width(190.0);
                let path = topic.session.cwd.as_path();
                let text = path.display().to_string();
                ui.add(egui::Label::new(&text).truncate()).on_hover_text(&text);
                if ui.button("Открыть").on_hover_text("Открыть каталог проекта").clicked()
                {
                    self.opener.start(Destination::Folder(path.to_owned()), ui.ctx());
                }
            })
        });
        match &topic.session.session {
            Some(id) => {
                if ui
                    .add(egui::Label::new(short_session(id.as_str())).sense(egui::Sense::click()))
                    .on_hover_text("Скопировать полный session_id")
                    .clicked()
                {
                    ui.ctx().copy_text(id.as_str().to_owned());
                }
            }
            None => {
                ui.label("—");
            }
        }
        let running = topic.state == TopicState::Running;
        ui.label(state_text(topic.state));
        ui.push_id((topic.key, "controls"), |ui| {
            ui.horizontal(|ui| {
                if ui.add_enabled(running, egui::Button::new("Stop")).clicked() {
                    send(Command::StopTopic(topic.key));
                }
                if ui.add_enabled(!running, egui::Button::new("Reset")).clicked() {
                    self.resetting = Some(topic.key);
                }
            })
        });
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
#[cfg(test)]
mod tests {
    use super::*;
    use hub_core::domain::{AbsolutePath, BackendKind, ChatId, ThreadId, TopicSession};
    use hub_telegram::hub::TopicView;

    fn topic(thread: i32, name: &str, backend: BackendKind, state: TopicState) -> TopicView {
        TopicView {
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
    fn topics_offer_search_and_navigation() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let snapshot = Snapshot {
            topics: vec![topic(42, "Бэкенд", BackendKind::Claude, TopicState::Waiting)],
            ..Snapshot::default()
        };
        draw(&ctx, &mut view, &snapshot);
        let shown = labels(&draw(&ctx, &mut view, &snapshot));
        for label in [
            "Поиск по названию или каталогу",
            "Все бэкенды",
            "Все состояния",
            "Telegram",
            "Ссылка",
            "Открыть",
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
        let filter = TopicFilter { query: query.to_owned(), backend, state };
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
        let copied = click(&ctx, &mut view, &snapshot, "Ссылка", &mut commands);
        assert!(copied.platform_output.commands.iter().any(|command| matches!(command,
            egui::OutputCommand::CopyText(text) if text == "https://t.me/c/1234567890/42")));
        click(&ctx, &mut view, &snapshot, "Stop", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::StopTopic(key)] if key.thread.0 == 42));
        commands.clear();
        snapshot.topics.last_mut().unwrap().state = TopicState::Waiting;
        click(&ctx, &mut view, &snapshot, "Reset", &mut commands);
        assert!(commands.is_empty(), "reset requires confirmation");
        click(&ctx, &mut view, &snapshot, "Сбросить", &mut commands);
        assert!(matches!(commands.as_slice(), [Command::ResetTopic(key)] if key.thread.0 == 42));
        click(&ctx, &mut view, &snapshot, "Сбросить фильтры", &mut commands);
        assert!(
            labels(&draw(&ctx, &mut view, &snapshot)).iter().any(|text| text == "Темы: 2 из 2")
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
        click(&ctx, &mut view, &snapshot, "Открыть", &mut Vec::new());
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
    fn topic_cells_stay_under_their_grid_headers() {
        let ctx = egui::Context::default();
        let mut view = TopicsView::default();
        let snapshot = Snapshot {
            topics: vec![topic(42, "Alpha", BackendKind::Claude, TopicState::Waiting)],
            ..Snapshot::default()
        };
        draw(&ctx, &mut view, &snapshot);
        let output = draw(&ctx, &mut view, &snapshot);
        let x = |label| {
            output
                .shapes
                .iter()
                .find_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape
                        && text.galley.text() == label
                    {
                        Some(text.pos.x)
                    } else {
                        None
                    }
                })
                .unwrap()
        };
        assert!(x("claude") >= x("Бэкенд"), "backend cell is left of its header");
        assert!(x("claude") < x("Каталог"), "backend cell exceeds its column");
    }
}
