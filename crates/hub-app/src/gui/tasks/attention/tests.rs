use super::*;
use crate::local_execution::Answer;
use hub_core::domain::{Question, QuestionOption, Selection, ToolRequest};

struct Screen {
    ctx: egui::Context,
    size: egui::Vec2,
    view: TasksView,
    snapshot: Snapshot,
    commands: Vec<Command>,
    opened: bool,
}

impl Screen {
    fn new(snapshot: Snapshot) -> Self {
        let ctx = egui::Context::default();
        crate::gui::configure_theme(&ctx);
        ctx.all_styles_mut(|style| style.animation_time = 0.0);
        Self {
            ctx,
            size: egui::vec2(790.0, 800.0),
            view: TasksView::default(),
            snapshot,
            commands: Vec::new(),
            opened: false,
        }
    }
    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Rect)> {
        let mut output = self.ctx.run_ui(
            egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
                ..Default::default()
            },
            |ui| {
                self.opened |=
                    self.view.show_attention(ui, &self.snapshot, &mut |c| self.commands.push(c));
            },
        );
        output.textures_delta.clear();
        output
            .shapes
            .into_iter()
            .filter_map(|shape| {
                if let egui::Shape::Text(text) = shape.shape {
                    let rect = text.visual_bounding_rect().intersect(shape.clip_rect);
                    if rect.is_positive() {
                        return Some((text.galley.text().to_owned(), rect));
                    }
                }
                None
            })
            .collect()
    }
    fn rect(&mut self, text: &str, index: usize) -> egui::Rect {
        self.frame(vec![]);
        let labels = self.frame(vec![]);
        labels
            .iter()
            .filter(|(label, _)| label == text)
            .nth(index)
            .map_or_else(|| panic!("missing {text:?} at {index} in {labels:?}"), |(_, p)| *p)
    }
    fn contains(&mut self, text: &str) -> bool {
        self.frame(vec![]).iter().any(|(label, _)| label == text)
    }
    fn pointer(&mut self, pos: egui::Pos2, pressed: bool) {
        self.frame(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
    }
    fn click(&mut self, text: &str, index: usize) {
        let position = self.rect(text, index).center();
        self.pointer(position, true);
        self.pointer(position, false);
    }
}

fn job(id: u64, project: &str, kind: Kind) -> Live {
    Live {
        token: Uuid::new_v4(),
        project: std::env::temp_dir().join(project),
        id,
        run: Some(1),
        active: true,
        status: "Ожидает решения".into(),
        interaction: Some(Interaction { id: Uuid::new_v4(), kind }),
    }
}

fn permission(id: u64, command: &str) -> Live {
    job(id, "Hub", Kind::Permission(ToolRequest { tool: "Bash".into(), summary: command.into() }))
}

fn question(project: &str, id: u64) -> Live {
    let options = ["Основная", "Архивная"]
        .map(|label| QuestionOption { label: label.into(), description: String::new() });
    job(
        id,
        project,
        Kind::Questions(vec![
            Question::new(
                "Какие версии проверить?".into(),
                String::new(),
                options.into(),
                Selection::Multiple,
            )
            .unwrap(),
        ]),
    )
}

#[test]
fn parallel_permission_shows_full_request_and_recovers_from_failed_delivery_in_place() {
    let full = "cargo test --locked\n--workspace\n--exclude hub-core\n--features wayland x11 glow\n--package hub-app";
    let first = permission(1, "cargo check");
    let second = permission(2, full);
    let mut screen = Screen::new(Snapshot {
        local: Some(first.clone()),
        local_jobs: vec![first, second.clone()],
        ..Default::default()
    });
    assert_eq!(attention_count(&screen.snapshot), 2);
    assert!(screen.contains("Разрешение"));
    assert!(!screen.contains(full));
    screen.click("Подробнее", 0);
    assert!(screen.contains(full));
    screen.click("Разрешить один раз", 1);
    let Some(Command::LocalAnswer { token, id, answer: Answer::Allow, reply }) =
        screen.commands.pop()
    else {
        panic!("missing permission answer")
    };
    assert_eq!((token, Some(id)), (second.token, second.interaction.as_ref().map(|r| r.id)));
    reply.send(Err("Не удалось доставить ответ".into())).unwrap();
    assert!(screen.contains("Не удалось доставить ответ"));
    screen.click("Отклонить", 1);
    assert!(
        matches!(screen.commands.pop(), Some(Command::LocalAnswer { token: actual, id: req, answer: Answer::Deny, .. }) if actual == token && req == id)
    );
    assert!(!screen.opened);
}

#[test]
fn switching_project_and_request_restores_multiselect_and_free_text() {
    let first = question("Hub", 1);
    let second = question("Atlas", 2);
    let rows = [&first, &second]
        .into_iter()
        .map(|job| TaskRow {
            id: job.id,
            project: job.project.clone(),
            title: format!("Задача {}", job.id),
            prompt: "Проверь".into(),
            backend: "codex".into(),
            status: hub_core::tasks::TaskStatus::Planned,
            archived: false,
            summary: String::new(),
            reason: None,
            files: 0,
            checks: None,
        })
        .collect();
    let mut screen = Screen::new(Snapshot {
        local_jobs: vec![first.clone(), second],
        task_index: Ok(rows),
        ..Default::default()
    });
    screen.click("Основная", 0);
    screen.click("Архивная", 0);
    screen.click("Основная", 0);
    screen.click("Ответ или свой вариант", 0);
    screen.frame(vec![egui::Event::Text("остальные позже".into())]);
    screen.click("Все проекты", 0);
    screen.click("Atlas", 0);
    screen.click("Основная", 0);
    screen.click("Atlas", 0);
    screen.click("Hub", 0);
    screen.click("Отправить ответ", 0);
    assert!(
        matches!(screen.commands.pop(), Some(Command::LocalAnswer { token, id, answer: Answer::Text(values), .. }) if token == first.token && Some(id) == first.interaction.as_ref().map(|r|r.id) && values == ["Архивная, остальные позже"])
    );
    assert!(!screen.opened);
}

#[test]
fn permission_changed_between_press_and_release_cannot_authorize_the_new_job() {
    let mut screen =
        Screen::new(Snapshot { local: Some(permission(1, "cargo check")), ..Default::default() });
    let pos = screen.rect("Разрешить один раз", 0).center();
    screen.pointer(pos, true);
    screen.snapshot.local = Some(permission(1, "cargo clean"));
    screen.pointer(pos, false);
    assert!(screen.commands.is_empty());
    assert!(screen.contains("Разрешить один раз"));
    assert!(screen.contains("cargo clean"));
}

#[test]
fn oversized_unicode_answer_leaves_denial_available() {
    let mut screen =
        Screen::new(Snapshot { local: Some(question("Hub", 1)), ..Default::default() });
    let labels = screen.frame(vec![]);
    assert_eq!(labels.iter().filter(|(text, _)| text == "Какие версии проверить?").count(), 1);
    screen.click("Ответ или свой вариант", 0);
    screen.frame(vec![egui::Event::Text("Я".repeat(9000))]);
    screen.click("Отправить ответ", 0);
    assert!(screen.commands.is_empty());
    screen.click("Отказаться отвечать", 0);
    assert!(matches!(
        screen.commands.pop(),
        Some(Command::LocalAnswer { answer: Answer::Deny, .. })
    ));
}

#[test]
fn index_error_keeps_live_decisions_and_scoped_empty_state_can_be_cleared() {
    let mut screen = Screen::new(Snapshot {
        local: Some(question("Hub", 1)),
        task_index: Err("Не удалось прочитать файл.".into()),
        ..Default::default()
    });
    assert!(screen.contains("Не удалось загрузить задачи. Не удалось прочитать файл."));
    assert!(screen.contains("Вопрос"));
    assert!(!screen.contains("Всё разобрано."));
    screen.view.inbox.project = Some(std::env::temp_dir().join("Atlas"));
    screen.snapshot.task_index = Ok(Vec::new());
    assert!(screen.contains("В этом проекте всё разобрано."));
    screen.click("Показать все проекты", 0);
    assert!(screen.contains("Вопрос"));
}

#[test]
fn inactive_or_unrelated_jobs_cannot_replace_the_selected_chat_decision() {
    let target = question("Hub", 1);
    let unrelated = question("Atlas", 1);
    let mut inactive = target.clone();
    inactive.active = false;
    let snapshot = Snapshot {
        local: Some(unrelated.clone()),
        local_jobs: vec![inactive, unrelated, target.clone()],
        ..Default::default()
    };
    let mut view = TasksView {
        focused: Some(1),
        scope: Some((None, target.project.clone(), BackendKind::Codex)),
        ..Default::default()
    };
    assert_eq!(view.selected_live(&snapshot), Some(&target));
    view.focused = Some(2);
    assert!(view.selected_live(&snapshot).is_none());
}

#[test]
fn single_job_snapshot_cannot_reintroduce_a_previous_attempt() {
    let previous = permission(1, "cargo check");
    let mut newer = permission(1, "cargo test");
    newer.interaction = None;
    let snapshot =
        Snapshot { local: Some(previous), local_jobs: vec![newer], ..Default::default() };
    assert_eq!(attention_count(&snapshot), 0);
}

#[test]
fn stored_attention_opens_the_corresponding_task_section() {
    let mut book = hub_core::tasks::TaskBook::default();
    let id = book.create(std::env::temp_dir(), "codex", "Исправление", "Проверь").unwrap();
    let task = book.get(id).unwrap();
    for (reason, button, section) in [
        (Attention::Review, "Проверить", Tab::Summary),
        (Attention::CheckFailed, "Открыть проверку", Tab::Checks),
        (Attention::Interrupted, "Разобрать запуск", Tab::Checks),
        (Attention::AgentFailed, "Разобрать запуск", Tab::Checks),
        (Attention::Git, "Проверить Git", Tab::Parameters),
        (Attention::Queue, "Открыть очередь", Tab::Parameters),
    ] {
        let row = TaskRow {
            reason: Some(reason),
            summary: "Подробности задачи".into(),
            ..TaskRow::from(task)
        };
        let mut screen = Screen::new(Snapshot { task_index: Ok(vec![row]), ..Default::default() });
        assert!(screen.contains("Подробности задачи"));
        screen.click(button, 0);
        assert!(screen.opened && screen.view.review_from_inbox);
        assert_eq!(screen.view.focused, Some(id));
        assert!(screen.view.result_view.tab(task) == section);
    }
}

#[test]
fn failed_reply_remains_visible_if_the_request_disappears_before_the_next_frame() {
    let mut screen =
        Screen::new(Snapshot { local: Some(permission(1, "cargo test")), ..Default::default() });
    screen.click("Разрешить один раз", 0);
    let Some(Command::LocalAnswer { reply, .. }) = screen.commands.pop() else {
        panic!("missing permission answer")
    };
    reply.send(Err("Запрос уже завершён.".into())).unwrap();
    screen.snapshot.local = None;
    assert!(screen.contains("Задача #1 · Запрос уже завершён."));
    assert!(screen.contains("Всё разобрано."));
}

#[test]
fn each_question_requires_its_own_answer_and_preserves_order() {
    let first = Question::new(
        "Что сделать с черновиками?".into(),
        String::new(),
        vec![QuestionOption {
            label: "Сохранить".into(), description: "Для каждой задачи".into()
        }],
        Selection::Single,
    )
    .unwrap();
    let second =
        Question::new("Как назвать отчёт?".into(), String::new(), Vec::new(), Selection::Single)
            .unwrap();
    let mut screen = Screen::new(Snapshot {
        local: Some(job(3, "Hub", Kind::Questions(vec![first, second]))),
        ..Default::default()
    });
    screen.click("Сохранить", 0);
    screen.click("Отправить ответы", 0);
    assert!(screen.commands.is_empty());
    screen.click("Ответ или свой вариант", 1);
    screen.frame(vec![egui::Event::Text("Отчёт по задачам".into())]);
    screen.click("Отправить ответы", 0);
    assert!(
        matches!(screen.commands.pop(), Some(Command::LocalAnswer { answer: Answer::Text(values), .. }) if values == ["Сохранить", "Отчёт по задачам"])
    );
}

#[test]
fn question_card_answers_without_expand_step() {
    let live = question("Hub", 1);
    let mut screen = Screen::new(Snapshot { local: Some(live.clone()), ..Default::default() });
    assert!(screen.contains("Ждут вашего ответа · 1"));
    assert!(screen.contains("Вопрос"));
    assert!(screen.contains("Какие версии проверить?"));
    assert!(screen.contains("Отправить ответ"));
    assert!(!screen.contains("Ответить"));
    screen.click("Основная", 0);
    screen.click("Отправить ответ", 0);
    assert!(
        matches!(screen.commands.pop(), Some(Command::LocalAnswer { token, id, answer: Answer::Text(values), .. }) if token == live.token && Some(id) == live.interaction.as_ref().map(|r| r.id) && values == ["Основная"])
    );
}

#[test]
fn permission_card_shows_actions_and_details_toggle() {
    let long = "cargo test --locked\n--workspace\n--exclude hub-core\n--features wayland x11 glow\n--package hub-app";
    let live = permission(1, long);
    let mut screen = Screen::new(Snapshot { local: Some(live.clone()), ..Default::default() });
    assert!(screen.contains("Разрешить один раз"));
    assert!(screen.contains("Отклонить"));
    assert!(screen.contains("Подробнее"));
    assert!(!screen.contains(long));
    screen.click("Подробнее", 0);
    assert!(screen.contains(long));
    assert!(screen.contains("Свернуть"));
    screen.click("Свернуть", 0);
    assert!(!screen.contains(long));
    screen.click("Разрешить один раз", 0);
    assert!(
        matches!(screen.commands.pop(), Some(Command::LocalAnswer { token, id, answer: Answer::Allow, .. }) if token == live.token && Some(id) == live.interaction.as_ref().map(|r| r.id))
    );
}

#[test]
fn permission_summary_collapses_by_lines_not_characters() {
    let summary = "строка 1\nстрока 2\nстрока 3\nстрока 4\nстрока 5";
    let mut screen =
        Screen::new(Snapshot { local: Some(permission(1, summary)), ..Default::default() });
    assert!(
        screen.contains("строка 1\nстрока 2\nстрока 3…"),
        "collapsed summary keeps its first three lines with breaks"
    );
    assert!(!screen.contains(summary));
    assert!(screen.contains("Подробнее"));
    screen.click("Подробнее", 0);
    assert!(screen.contains(summary));
    let three = "одна\nдве\nтри";
    let mut short =
        Screen::new(Snapshot { local: Some(permission(2, three)), ..Default::default() });
    assert!(short.contains(three));
    assert!(!short.contains("Подробнее"), "summaries up to 3 lines stay uncollapsed");
}

#[test]
fn single_line_permission_summary_collapses_by_visual_rows() {
    let long = "npm publish --access public --tag latest ".repeat(12);
    let mut screen =
        Screen::new(Snapshot { local: Some(permission(1, &long)), ..Default::default() });
    assert!(
        screen.contains("Подробнее"),
        "a single-line command of 504 chars has one logical line but must still be collapsible"
    );
    let monospace = screen
        .ctx
        .style_of(screen.ctx.theme())
        .text_styles
        .get(&egui::TextStyle::Monospace)
        .cloned()
        .expect("monospace style");
    let row = screen.ctx.fonts_mut(|fonts| fonts.row_height(&monospace));
    let collapsed = screen.rect(&long, 0).height();
    assert!(
        collapsed <= 3.0 * row + 1.0,
        "collapsed summary must not exceed three visual rows: {collapsed} > {}",
        3.0 * row + 1.0
    );
    screen.click("Подробнее", 0);
    let expanded = screen.rect(&long, 0).height();
    assert!(expanded > 4.0 * row, "reveal shows the whole command: {expanded}");
}

#[test]
fn answered_live_request_shows_status_only_inline() {
    let live = question("Hub", 1);
    let mut screen = Screen::new(Snapshot { local: Some(live.clone()), ..Default::default() });
    screen.click("Отказаться отвечать", 0);
    let Some(Command::LocalAnswer { reply, .. }) = screen.commands.pop() else {
        panic!("missing answer")
    };
    reply.send(Ok(())).unwrap();
    assert!(screen.contains("Ответ получен."));
    assert!(!screen.contains("Задача #1 · Ответ получен."), "no duplicate global row while inline");
    screen.snapshot.local = None;
    assert!(screen.contains("Задача #1 · Ответ получен."));
}

#[test]
fn narrow_inbox_stacks_action_below_content() {
    let mut book = hub_core::tasks::TaskBook::default();
    let id = book.create(std::env::temp_dir(), "codex", "Поиск по каталогу", "Проверь").unwrap();
    let row = TaskRow {
        reason: Some(Attention::Review),
        summary: "Готово".into(),
        ..TaskRow::from(book.get(id).unwrap())
    };
    let mut screen = Screen::new(Snapshot { task_index: Ok(vec![row]), ..Default::default() });
    screen.size = egui::vec2(400.0, 600.0);
    assert!(screen.contains("√"));
    assert!(screen.contains("Результаты на проверку · 1"));
    let title = screen.rect("Поиск по каталогу", 0);
    let action = screen.rect("Проверить", 0);
    assert!(action.top() >= title.bottom(), "action must stack below the content at 400px");
}

#[test]
fn narrow_inbox_supports_keyboard_and_clicks_in_button_padding() {
    let long = "cargo test --locked\n--workspace\n--exclude hub-core\n--features wayland x11 glow\n--package hub-app";
    let mut screen =
        Screen::new(Snapshot { local: Some(permission(1, long)), ..Default::default() });
    screen.size = egui::vec2(330.0, 560.0);
    let target = screen.rect("Подробнее", 0).center();
    let key = |screen: &mut Screen, key| {
        for pressed in [true, false] {
            screen.frame(vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]);
        }
    };
    let focused = (0..12).any(|_| {
        key(&mut screen, egui::Key::Tab);
        screen
            .ctx
            .memory(egui::Memory::focused)
            .and_then(|id| screen.ctx.read_response(id))
            .is_some_and(|r| r.rect.height() < 50.0 && r.rect.contains(target))
    });
    assert!(focused, "the permission details link must be reachable with Tab");
    key(&mut screen, egui::Key::Enter);
    assert!(screen.contains(long));
    let beside_text = screen.rect("Свернуть", 0).right_center() + egui::vec2(4.0, 0.0);
    screen.pointer(beside_text, true);
    screen.pointer(beside_text, false);
    assert!(!screen.contains(long), "a click in the link's padding collapses the summary");
    assert!(screen.contains("Подробнее"));
    assert!(screen.commands.is_empty());
}

#[test]
fn review_card_shows_check_and_file_chips() {
    let row = TaskRow {
        id: 1,
        project: std::env::temp_dir().join("Hub"),
        title: "Поиск по каталогу".into(),
        prompt: "Проверь".into(),
        backend: "codex".into(),
        status: hub_core::tasks::TaskStatus::NeedsReview,
        archived: false,
        reason: Some(Attention::Review),
        summary: "Готово".into(),
        files: 2,
        checks: Some((2, 3)),
    };
    let mut screen = Screen::new(Snapshot { task_index: Ok(vec![row]), ..Default::default() });
    assert!(screen.contains("Проверки · 2/3"));
    assert!(screen.contains("Файлов · 2"));
    assert!(screen.contains("Результаты на проверку · 1"));
}

#[test]
fn project_attention_counts_inbox_items_per_project() {
    let first = permission(1, "cargo check");
    let second = question("Atlas", 2);
    let snapshot =
        Snapshot { local_jobs: vec![first.clone(), second.clone()], ..Default::default() };
    assert_eq!(project_attention(&snapshot, &first.project), 1);
    assert_eq!(project_attention(&snapshot, &second.project), 1);
    assert_eq!(project_attention(&snapshot, std::path::Path::new("/absent")), 0);
}
