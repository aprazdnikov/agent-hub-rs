use super::*;

fn draw(
    tasks: &mut Tasks,
    ctx: &egui::Context,
    width: f32,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut output = ctx.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 900.0),
            )),
            time: Some(ctx.input(|input| input.time) + 0.15),
            events,
            ..Default::default()
        },
        |ui| {
            egui::CentralPanel::default().show(ui, |ui| tasks.show(ui));
        },
    );
    output.textures_delta.clear();
    output
}

fn position(output: &egui::FullOutput, label: &str) -> egui::Rect {
    output
        .shapes
        .iter()
        .find_map(|shape| {
            if let egui::Shape::Text(text) = &shape.shape
                && text.galley.text() == label
            {
                Some(text.galley.rect.translate(text.pos.to_vec2()))
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("Missing text {label}"))
}

#[test]
fn speakers_occupy_opposite_sides_at_both_widths() {
    for width in [400.0, 1100.0] {
        let mut tasks = Tasks::default();
        let ctx = egui::Context::default();
        draw(&mut tasks, &ctx, width, vec![]);
        let output = draw(&mut tasks, &ctx, width, vec![]);
        let task = tasks.entry(37).unwrap();
        let user = position(&output, &task.prompt);
        let agent = position(&output, task.summary());
        assert!(user.left() > agent.left() + 36.0);
        assert!(user.right() < width);
    }
}

fn click(tasks: &mut Tasks, ctx: &egui::Context, label: &str) -> egui::FullOutput {
    draw(tasks, ctx, 1000.0, vec![]);
    let output = draw(tasks, ctx, 1000.0, vec![]);
    let pos = position(&output, label).center();
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    draw(tasks, ctx, 1000.0, vec![egui::Event::PointerMoved(pos), button(true)]);
    draw(tasks, ctx, 1000.0, vec![button(false)])
}

fn enter(modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
fn enter_inserts_a_line_and_command_enter_submits_only_from_the_composer() {
    let ctx = egui::Context::default();
    let mut tasks = Tasks::default();
    click(&mut tasks, &ctx, "Сообщение агенту…");
    draw(
        &mut tasks,
        &ctx,
        1000.0,
        vec![
            egui::Event::Text("Строка 1".into()),
            enter(egui::Modifiers::NONE),
            egui::Event::Text("Строка 2".into()),
        ],
    );
    assert_eq!(tasks.entry(37).unwrap().composer, "Строка 1\nСтрока 2");
    ctx.memory_mut(egui::Memory::stop_text_input);
    draw(&mut tasks, &ctx, 1000.0, vec![enter(egui::Modifiers::COMMAND)]);
    assert!(tasks.pending.is_none());
    click(&mut tasks, &ctx, "Строка 1\nСтрока 2");
    draw(&mut tasks, &ctx, 1000.0, vec![enter(egui::Modifiers::COMMAND)]);
    assert!(tasks.wait.busy());
    assert_eq!(tasks.entry(37).unwrap().latest_step(), "Строка 1\nСтрока 2");
    assert!(tasks.entry(37).unwrap().composer.is_empty());
}

#[test]
fn review_attachment_can_be_read_and_copied() {
    let ctx = egui::Context::default();
    kylig_icons::install_weights(&ctx, &[kylig_icons::Weight::Regular]);
    let mut tasks = Tasks::default();
    click(&mut tasks, &ctx, "Открыть результат");
    click(&mut tasks, &ctx, "Файлы");
    let output = click(&mut tasks, &ctx, kylig_icons::COPY);
    assert!(output.platform_output.commands.iter().any(|cmd|
        matches!(cmd, egui::OutputCommand::CopyText(text) if text.contains("test cyrillic_case ... ok"))
    ));
}

#[test]
fn selected_attempt_opens_its_original_step() {
    let ctx = egui::Context::default();
    let mut tasks = Tasks::default();
    tasks.entry_mut(37).unwrap().message(Speaker::User, "Запусти проверки повторно.");
    tasks.finish(Effect::Run(37));
    click(&mut tasks, &ctx, "Открыть результат");
    click(&mut tasks, &ctx, "История");
    click(&mut tasks, &ctx, "Попытка 2 · Codex");
    click(&mut tasks, &ctx, "Попытка 1 · Codex");
    let output = draw(&mut tasks, &ctx, 1000.0, vec![]);
    assert!(position(&output, &tasks.entry(37).unwrap().prompt).is_positive());
}

#[test]
fn compact_project_browser_stays_open_while_filtering() {
    let ctx = egui::Context::default();
    let mut tasks = Tasks { compact_projects: true, ..Tasks::default() };
    tasks.switch_archive(true);
    let output = draw(&mut tasks, &ctx, 400.0, vec![]);
    assert!(position(&output, "Проекты").is_positive());
}

#[test]
fn project_tree_uses_room_below_the_first_twelve_rows() {
    for width in [400.0, 1100.0] {
        let ctx = egui::Context::default();
        let mut tasks = Tasks { compact_projects: true, ..Tasks::default() };
        for id in 100..114 {
            tasks.entries.push(Task::seed(
                id,
                Project::Hub,
                "Codex",
                Stage::Draft,
                &format!("Задача {id}"),
                "Поручение",
            ));
        }
        draw(&mut tasks, &ctx, width, vec![]);
        let output = draw(&mut tasks, &ctx, width, vec![]);
        let label = "Задача 113";
        assert!(
            output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == label
                && shape.clip_rect.contains_rect(text.galley.rect.translate(text.pos.to_vec2())))
            }),
            "Последняя задача должна быть видна без прокрутки при ширине {width}"
        );
    }
}

#[test]
fn dragging_the_divider_gives_the_project_tree_more_width() {
    let ctx = egui::Context::default();
    let mut tasks = Tasks::default();
    let label = "agent-hub   /   #37";
    draw(&mut tasks, &ctx, 1200.0, vec![]);
    let before = position(&draw(&mut tasks, &ctx, 1200.0, vec![]), label);
    let start = egui::pos2(before.left() - 16.5, 360.0);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    draw(&mut tasks, &ctx, 1200.0, vec![egui::Event::PointerMoved(start), button(start, true)]);
    let end = start + egui::vec2(124.0, 0.0);
    draw(&mut tasks, &ctx, 1200.0, vec![egui::Event::PointerMoved(end)]);
    draw(&mut tasks, &ctx, 1200.0, vec![button(end, false)]);
    let after = position(&draw(&mut tasks, &ctx, 1200.0, vec![]), label);
    assert!(after.left() - before.left() > 100.0);
}

#[test]
fn a_multiline_composer_keeps_send_visible_at_the_bottom() {
    let ctx = egui::Context::default();
    let mut tasks = Tasks::default();
    tasks.entry_mut(37).unwrap().composer =
        (1..=7).map(|n| format!("Строка {n}")).collect::<Vec<_>>().join("\n");
    draw(&mut tasks, &ctx, 1100.0, vec![]);
    let output = draw(&mut tasks, &ctx, 1100.0, vec![]);
    assert!(output.shapes.iter().any(|shape| {
        matches!(&shape.shape, egui::Shape::Text(text) if text.galley.text() == "Отправить"
            && text.pos.y > 815.0
            && shape.clip_rect.contains_rect(text.galley.rect.translate(text.pos.to_vec2())))
    }));
}
