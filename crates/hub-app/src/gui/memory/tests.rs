use super::*;
#[test]
fn empty_project_does_not_start_a_disk_read() {
    let mut view = MemoryView::default();
    view.load(Path::new("/"), Request::Get { key: String::new(), revision: None });
    assert!(view.pending.is_none());
    assert_eq!(view.error.as_deref(), Some("Сначала выберите каталог проекта."));
}

#[test]
fn empty_key_does_not_start_a_disk_read() {
    let mut view = MemoryView { project: "/project".into(), ..Default::default() };
    view.load(Path::new("/"), Request::Get { key: String::new(), revision: None });
    assert!(view.pending.is_none());
    assert_eq!(view.error.as_deref(), Some("Введите ключ записи или выберите запись из списка."));
}

#[test]
fn direct_memory_navigation_rechecks_the_saved_project_root() {
    let home = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let mut view = MemoryView::default();
    view.open_project(outside.path().into());
    view.set_workspace_root(Some(root.path().into()));
    assert!(view.project.as_os_str().is_empty());
    view.open_project(outside.path().into());
    view.refresh(home.path());
    let (_, result) =
        view.pending.take().unwrap().recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert_eq!(result.err().as_deref(), Some("Каталог находится вне настроенного корня проектов."));
    assert!(!hub_memory::base_in(home.path()).exists());
}

#[test]
fn switching_projects_during_a_read_loads_only_the_selected_project() {
    let home = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let (tx, rx) = mpsc::channel();
    let mut view =
        MemoryView { project: home.path().into(), pending: Some(rx), ..Default::default() };
    view.open_project(target.path().to_owned());
    view.refresh(home.path());
    assert!(view.refresh_on_open);
    tx.send((0, Ok(View::Notice("old data".into())))).unwrap();
    view.poll(&egui::Context::default());
    view.refresh(home.path());
    let (_, result) =
        view.pending.as_ref().unwrap().recv_timeout(std::time::Duration::from_secs(5)).unwrap();
    assert!(
        matches!(result.unwrap(), View::Admin(panel) if matches!(*panel, Panel::Overview(ref overview) if overview.page.project == std::fs::canonicalize(target.path()).unwrap()))
    );
}

struct Browser {
    ctx: egui::Context,
    view: MemoryView,
    home: tempfile::TempDir,
    project: tempfile::TempDir,
    size: egui::Vec2,
    time: f64,
}

impl Browser {
    fn new(width: f32) -> Self {
        let ctx = egui::Context::default();
        kylig_icons::install_weights(&ctx, &[kylig_icons::Weight::Regular]);
        ctx.all_styles_mut(|s| s.animation_time = 0.0);
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let store =
            hub_memory::Store::open(&hub_memory::base_in(home.path()), project.path(), "test")
                .unwrap();
        for (n, body) in [(0, "First text"), (1, "Updated text")] {
            let revision = hub_memory::WriteRequest {
                key: "pilot".into(),
                expected_revision: n,
                kind: hub_memory::EntryKind::Progress,
                title: "Pilot".into(),
                body: body.into(),
                sources: vec!["test".into()],
            };
            // A concurrently forked process may briefly retain the previous write's file lock.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            loop {
                match store.write(revision.clone()) {
                    Ok(_) => break,
                    Err(hub_memory::Error::Busy) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(error) => panic!("could not seed memory revision: {error}"),
                }
            }
        }
        let mut view = MemoryView::default();
        view.open_project(project.path().to_owned());
        Self { ctx, view, home, project, size: egui::vec2(width, 1500.0), time: 0.0 }
    }
    fn draw(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        self.time += 0.05;
        let mut output = self.ctx.run_ui(
            egui::RawInput {
                time: Some(self.time),
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.size)),
                events,
                ..Default::default()
            },
            |ui| {
                egui::CentralPanel::default().show(ui, |ui| self.view.show(ui, self.home.path()));
            },
        );
        output.textures_delta.clear();
        output
    }
    fn settle(&mut self) -> egui::FullOutput {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let frame = self.draw(Vec::new());
            if self.view.pending.is_none() {
                return frame;
            }
            assert!(std::time::Instant::now() < end);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }
    fn click(&mut self, label: &str) {
        self.settle();
        let frame = self.draw(Vec::new());
        let rect = frame
            .shapes
            .iter()
            .rev()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.text() == label => {
                    Some(t.galley.rect.translate(t.pos.to_vec2()))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing {label}: {:?}", labels(&frame)));
        let pos = rect.center();
        for pressed in [true, false] {
            self.draw(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
    }
}

fn labels(frame: &egui::FullOutput) -> Vec<&str> {
    frame
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some(t.galley.text()),
            _ => None,
        })
        .collect()
}

#[test]
fn cancelling_record_change_preserves_revision_and_context_survives_navigation() {
    let mut browser = Browser::new(440.0);
    assert!(labels(&browser.settle()).contains(&"pilot  ·  r2  ·  Активная"));
    browser.click("Pilot");
    browser.click("r1");
    browser.click("Управление записью");
    browser.click("В архив");
    browser.click("Отмена");
    browser.click("Содержание");
    assert!(labels(&browser.settle()).contains(&"First text"));
    browser.click("Добавить в пакет контекста");
    assert!(labels(&browser.settle()).contains(&"pilot · r1 добавлена в пакет."));
    browser.click("‹ К списку записей");
    browser.click("Обновить");
    browser.click("Пакет контекста");
    assert!(labels(&browser.settle()).contains(&"pilot · r1"));
    let other = tempfile::tempdir().unwrap();
    browser.view.open_project(other.path().to_owned());
    assert!(!labels(&browser.settle()).contains(&"pilot · r1"));
}

#[test]
fn cleanup_dialog_fits_and_only_confirm_changes_store_policy() {
    for width in [320.0, 430.0, 1600.0] {
        let mut browser = Browser::new(width);
        browser.click("Управление");
        assert!(labels(&browser.settle()).contains(&"Записей — 1 / 500   ·   ревизий — 2"));
        browser.click("Включить автоочистку");
        browser.settle();
        browser.draw(Vec::new());
        let rect = browser.ctx.memory(|m| m.area_rect(m.top_modal_layer().unwrap().id)).unwrap();
        assert!(
            rect.left() >= 12.0 && (rect.width() - (width - 32.0).min(580.0)).abs() < 5.0,
            "{width} {rect:?}"
        );
        browser.click("Отмена");
        let reader = hub_memory::MemoryReader::new(
            &hub_memory::base_in(browser.home.path()),
            browser.project.path(),
        )
        .unwrap();
        assert_eq!(
            reader.overview("", 0, 20, StatusFilter::All).unwrap().policy,
            hub_memory::CleanupPolicy::Manual
        );
        browser.click("Включить автоочистку");
        browser.click("Включить");
        browser.settle();
        assert_eq!(
            reader.overview("", 0, 20, StatusFilter::All).unwrap().policy,
            hub_memory::CleanupPolicy::Automatic
        );
    }
}

#[test]
fn archiving_a_record_keeps_management_open_on_the_selected_revision() {
    let mut browser = Browser::new(800.0);
    browser.click("Pilot");
    browser.click("r1");
    browser.click("Управление записью");
    browser.click("В архив");
    browser.click("Подтвердить");
    assert!(labels(&browser.settle()).contains(&"Текущий статус · В архиве"));
    browser.click("Содержание");
    assert!(labels(&browser.settle()).contains(&"First text"));
    browser.click("‹ К списку записей");
    assert!(labels(&browser.settle()).contains(&"pilot  ·  r2  ·  В архиве"));
}

#[test]
fn pending_navigation_does_not_advance_visible_page() {
    let mut browser = Browser::new(800.0);
    browser.settle();
    let overview = browser.view.overview.as_mut().unwrap();
    overview.page.matched_count = 60;
    let (_tx, rx) = mpsc::channel();
    browser.view.pending = Some(rx);
    let frame = browser.draw(Vec::new());
    let pos = frame
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text() == "Далее" => {
                Some(t.galley.rect.translate(t.pos.to_vec2()).center())
            }
            _ => None,
        })
        .unwrap();
    for pressed in [true, false] {
        browser.draw(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
    }
    assert_eq!(browser.view.overview.as_ref().unwrap().page.offset, 0);
}
#[test]
fn a_verified_generation_snapshot_can_be_restored_in_another_project() {
    let mut browser = Browser::new(600.0);
    browser.click("Управление");
    browser.click("Новое поколение");
    let export = browser.home.path().join("backup.json");
    browser.view.load(browser.home.path(), request(Action::PrepareReset(export.clone())));
    browser.click("pilot");
    browser.click("Проверить новое поколение");
    browser.click("Создать поколение");
    browser.settle();
    let base = hub_memory::base_in(browser.home.path());
    let source = hub_memory::MemoryReader::new(&base, browser.project.path()).unwrap();
    assert_eq!(source.overview("", 0, 20, StatusFilter::All).unwrap().page.key_count, 0);
    let target = tempfile::tempdir().unwrap();
    browser.view.open_project(target.path().to_owned());
    browser.click("Перейти к импорту");
    browser.click("Последний снимок");
    browser.click("Импортировать в проект…");
    browser.click("Импортировать");
    browser.settle();
    let destination = hub_memory::MemoryReader::new(&base, target.path()).unwrap();
    assert_eq!(destination.details("pilot", Some(1)).unwrap().entry.body, "First text");
    assert_eq!(source.overview("", 0, 20, StatusFilter::All).unwrap().page.key_count, 0);
}

#[test]
fn import_conflicts_show_both_histories_side_by_side_or_stacked() {
    let mut browser = Browser::new(1080.0);
    let incoming = tempfile::tempdir().unwrap();
    let base = hub_memory::base_in(browser.home.path());
    let store = hub_memory::Store::open(&base, incoming.path(), "test").unwrap();
    store
        .write(hub_memory::WriteRequest {
            key: "pilot".into(),
            expected_revision: 0,
            kind: hub_memory::EntryKind::Decision,
            title: "Incoming decision".into(),
            body: "Incoming text".into(),
            sources: vec!["test".into()],
        })
        .unwrap();
    let snapshot = hub_memory::MemoryReader::new(&base, incoming.path()).unwrap().export().unwrap();
    browser.click("Управление");
    browser.click("Импорт");
    browser.view.load(
        browser.home.path(),
        request(Action::Import { export: snapshot, choices: std::collections::BTreeMap::new() }),
    );
    browser.settle();
    for (width, stacked) in [(1080.0, false), (420.0, true)] {
        browser.size.x = width;
        browser.draw(Vec::new());
        let frame = browser.draw(Vec::new());
        for value in
            ["Incoming decision", "First text", "Updated text", "Incoming text", "Ключ копии"]
        {
            assert!(labels(&frame).contains(&value), "missing {value} at width {width}");
        }
        let position = |label| {
            frame
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.text() == label => Some(t.pos),
                    _ => None,
                })
                .unwrap()
        };
        let local = position("В памяти проекта");
        let incoming = position("В импортируемом файле");
        if stacked {
            assert!(incoming.y > local.y && (incoming.x - local.x).abs() < 2.0);
        } else {
            assert!(incoming.x > local.x && (incoming.y - local.y).abs() < 2.0);
        }
    }
}
