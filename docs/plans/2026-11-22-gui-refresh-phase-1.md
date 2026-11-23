# Обновление GUI — план фазы 1

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Сделать чат, входящие, действия задачи, сайдбар и экран результата удобнее по утверждённому дизайну, не меняя навигацию, палитру и логику команд.

**Architecture:** Общие презентационные компоненты (чипы, карточки, пузыри, токены статусов) добавляются в `gui/theme.rs`; экраны переставляют контролы поверх существующих `Request`/`Effect`/`Command`. Время запусков хранится в `Run` и инжектируется вызывающей стороной в `TaskBook::begin/finish`.

**Tech Stack:** Rust 1.96, edition 2024, eframe/egui 0.36, jiff 0.2, serde. Без новых зависимостей.

**Spec:** `docs/specs/2026-11-22-gui-refresh-design.md` (утверждён 2026-11-22; макеты в `docs/specs/assets/gui-refresh/`).

Фаза 2 (Темы → Новая задача/Правила → Настройки → Память) планируется отдельным планом после фазы 1: её задачи опираются на интерфейсы компонентов, созданных здесь.

## Global Constraints

- Линты: `warnings = deny`, clippy pedantic deny; запрещены `unwrap/expect/panic/indexing_slicing` вне тестов (в тестах разрешены `clippy.toml`).
- Без новых зависимостей; `jiff` уже в воркспейсе.
- `Run`: `deny_unknown_fields`; новые поля только `Option` + `serde(default, skip_serializing_if = "Option::is_none")`.
- Лимиты: поручение 16 КиБ, название 256 байт, замечание 2048, 32 запуска, 32 файла на попытку.
- Светлая и тёмная темы следуют ОС; каждый новый цвет имеет тёмный вариант.
- UI-тексты русские; существующие формулировки кнопок сохраняются, где кнопка остаётся на месте.
- GUI-тесты кликают по текстам (`click_text` в `gui/tasks.rs`): перенос кнопки в меню требует клика по меню, а не удаления проверки.
- Команды/эффекты не меняются; единственное изменение сигнатур — параметр времени в `begin`/`finish`.
- Проверки: `cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`.

## Review Focus

- Низкое окно (высота чата < 360px): пузыри + шапка с действиями не ломают резерв нижних панелей (`scroll_body`), композер и кнопка результата видимы. → Task 3, 6.
- Узкое окно входящих (< 440px): кнопка действия уходит под содержимое карточки, текст читаем. → Task 5.
- Старые файлы задач без дат: загружаются, время скрыто, без паник. → Task 2, 3.
- Тёмная тема: все новые компоненты читаются (паритет токенов). → Task 1, визуальная проверка в каждом task.
- Клавиатура: Ctrl+Enter отправляет после переделки композера; фокус-кольцо навигации остаётся внутри hover/focus ring. → Task 4 (существующий тест `theme` обязан проходить).

---

### Task 1: Токены статусов и общие компоненты

**Files:**
- Modify: `crates/hub-app/src/gui/theme.rs`
- Test: `crates/hub-app/src/gui/theme.rs` (mod tests)

**Interfaces:**
- Produces:
  - `pub(super) enum Tone { Accent, Warning, Error, Neutral }` (Copy, Eq)
  - `pub(super) fn tone_colors(ui: &egui::Ui, tone: Tone) -> (Color32, Color32)` — (bg, fg); Accent = (selected, accent), Warning = (warning_bg, warning_fg), Error = (canvas, error_fg), Neutral = (canvas, muted)
  - `pub(super) fn chip(ui: &mut egui::Ui, tone: Tone, text: &str) -> egui::Response` — капсула (corner_radius 999), small-текст, padding (9, 2)
  - `pub(super) fn card(ui: &egui::Ui) -> egui::Frame` — surface fill, border 1px, radius 10, inner_margin (14, 12)
  - `pub(super) fn bubble(ui: &egui::Ui, user: bool) -> egui::Frame` — user: selected fill, без border; agent: surface fill + border; radius 12, inner_margin (12, 9)
  - в `configure`: warning_bg/warning_fg light `251,240,220`/`138,90,0`, dark `58,49,32`/`229,192,123`; error light `148,49,47`, dark `253,166,160`; review light `183,121,31`, dark `229,192,123`. Хранение — в `style.visuals` нельзя (нет полей): модульные функции `pub(super) fn warning(ui) -> (Color32, Color32)` и `pub(super) fn review_color(ui) -> Color32`, выбирающие по `ui.visuals().dark_mode`; `tone_colors` использует их.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn status_tokens_have_dark_variants_and_chips_render() {
    let ctx = egui::Context::default();
    configure(&ctx);
    for theme in [Theme::Light, Theme::Dark] {
        let output = ctx.run_ui(egui::RawInput { system_theme: Some(theme), ..Default::default() }, |ui| {
            let (bg, fg) = tone_colors(ui, Tone::Warning);
            assert_ne!(bg, fg);
            chip(ui, Tone::Warning, "Нужна проверка");
            card(ui).show(ui, |ui| { ui.label("карточка"); });
            bubble(ui, true).show(ui, |ui| { ui.label("вы"); });
            bubble(ui, false).show(ui, |ui| { ui.label("агент"); });
        });
        output.textures_delta.clear();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p hub-app --lib gui::theme::tests::status_tokens_have_dark_variants_and_chips_render`
Expected: FAIL — `tone_colors`/`Tone` не определены.

- [ ] **Step 3: Implement tokens and components in `theme.rs`**

Сигнатуры выше. `chip` — `egui::Frame::new().fill(bg).corner_radius(999).inner_margin(...)` + `Label` small fg; возвращает response label. Цвета error брать из существующих diff-констант `result.rs` не нужно — продублировать значения в theme (единый источник теперь theme; `result.rs` переедет на `theme::tone_colors` в Task 8).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p hub-app --lib gui::theme`
Expected: PASS, включая существующие тесты фокус-кольца и системной темы.

- [ ] **Step 5: Commit**

```bash
git add crates/hub-app/src/gui/theme.rs
git commit -m "feat(gui): токены статусов и компоненты чип/карточка/пузырь"
```

---

### Task 2: Время запусков в данных

**Files:**
- Modify: `crates/hub-core/src/tasks.rs` (Run ~151, begin ~729, finish ~845)
- Modify: `crates/hub-app/src/local_execution.rs` (239, 353, 445, 530)
- Modify: `crates/hub-telegram/src/hub/tasks.rs` (668, 677, 714), `crates/hub-telegram/src/hub/tasks/git.rs`
- Modify (вызовы в тестах): `crates/hub-core/tests/tasks.rs`, `crates/hub-core/tests/task_service.rs`, `crates/hub-app/src/gui/tasks.rs`, `crates/hub-app/src/local_execution/tests.rs`, `crates/hub-app/tests/desktop_tasks.rs`
- Test: `crates/hub-core/tests/tasks.rs`

**Interfaces:**
- Consumes: `jiff::Timestamp` (уже зависимость hub-core).
- Produces:
  - `Run { started_at: Option<Timestamp>, finished_at: Option<Timestamp>, .. }` с serde-атрибутами выше
  - `TaskBook::begin(&mut self, id: u64, revision: u64, next: Option<&str>, now: Timestamp) -> Result<u32, String>`
  - `TaskBook::finish(&mut self, id: u64, run: u32, state: RunState, now: Timestamp) -> Result<(), String>`
  - `recover()` без изменений сигнатуры и без `finished_at`.
  - Продакшен-вызовы передают `Timestamp::now()`; тесты — фиксированный `Timestamp::from_second(1_700_000_000).unwrap()`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn run_timestamps_are_injected_and_recovery_leaves_finish_empty() {
    let mut book = TaskBook::default();
    let project = /* существующий helper каталога в этом файле */;
    let id = book.create(project, "qwen", "T", "Read").unwrap();
    let now = Timestamp::from_second(1_700_000_000).unwrap();
    let run = book.begin(id, 1, None, now).unwrap();
    assert_eq!(book.tasks()[0].runs[0].started_at, Some(now));
    assert_eq!(book.tasks()[0].runs[0].finished_at, None);
    book.finish(id, run, RunState::Completed, now).unwrap();
    assert_eq!(book.tasks()[0].runs[0].finished_at, Some(now));
}

#[test]
fn recover_does_not_stamp_finish_time() {
    // begin без finish, затем recover()
    assert_eq!(book.tasks()[0].runs[0].state, RunState::Interrupted);
    assert_eq!(book.tasks()[0].runs[0].finished_at, None);
}

#[test]
fn old_run_without_dates_loads() {
    let json = r#"{"id":1,"step":"s","state":"completed","result":"r"}"#;
    let run: Run = serde_json::from_str(json).unwrap();
    assert_eq!(run.started_at, None);
    assert_eq!(run.finished_at, None);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p hub-core --test tasks run_timestamps recover_does_not old_run_without`
Expected: FAIL — нет полей/параметра.

- [ ] **Step 3: Add fields and parameters; stamp in begin/finish**

`begin`: `started_at: Some(now)` в конструкторе Run. `finish`: `last.finished_at = Some(now)` рядом с `last.state = state`. `recover`: не трогать даты. Обновить все вызовы по списку файлов (механически: добавить аргумент).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p hub-core -p hub-app -p hub-telegram --locked`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hub-core crates/hub-app crates/hub-telegram
git commit -m "feat(tasks): время начала и завершения запуска с инъекцией часов"
```

---

### Task 3: Чат — пузыри, время, сворачивание

**Files:**
- Modify: `crates/hub-app/src/gui/tasks/result.rs` (`View`, `transcript` ~130)
- Modify: `crates/hub-app/src/gui/tasks/workspace.rs` (`chat` ~256, вызов transcript ~327)
- Test: `crates/hub-app/src/gui/tasks.rs` (mod tests, паттерн `input_frame`/`click_text`)

**Interfaces:**
- Consumes: `theme::{Tone, tone_colors, chip, bubble}`, `Run::{started_at, finished_at}` (Task 1, 2).
- Produces:
  - `result::View` поле `expanded: std::collections::BTreeSet<u32>` (id запусков, развёрнутых пользователем); `select()` сбрасывает.
  - `pub(super) fn transcript(ui: &mut egui::Ui, task: &Task, view: &mut View)` — вызов в `workspace::chat` обновляется.
  - `pub(super) fn format_time(ts: Option<Timestamp>) -> Option<String>` — `HH:MM` локально (`TimeZone::system()`), дата в тултипе при отличии от сегодня; `None` → не показывать.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn long_agent_answer_collapses_and_expands_by_click() {
    // задача с одним завершённым запуском, result = 12 строк
    // input_frame → текст «Развернуть» присутствует; клик по нему →
    // следующий фрейм содержит «Свернуть» и последнюю строку ответа
}

#[test]
fn bubbles_show_role_and_time_only_when_known() {
    // запуск без дат: нет текста времени; с датами: «Вы» и имя агента в шапках пузырей
}

#[test]
fn result_link_only_on_latest_reviewable_run() {
    // два завершённых запуска: «Результат →» ровно один раз (у последнего)
}
```

Тесты строятся на существующих helper'ах `test_project()`, `topic()`, `input_frame()`; ответы агента — многострочные строки.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p hub-app --lib gui::tasks::tests::long_agent_answer gui::tasks::tests::bubbles_show gui::tasks::tests::result_link_only`
Expected: FAIL.

- [ ] **Step 3: Implement transcript as bubbles**

Для каждого `turn` в `task.runs`: пузырь пользователя (`bubble(ui, true)`, выравнивание вправо через `allocate_ui_with_layout` right_to_left, макс. ширина 78% — `ui.available_width() * 0.78`), шапка «Вы» + время `started_at`; тело `turn.step` wrap selectable, без сворачивания. Пузырь агента слева (86%): шапка — точка accent 8px + имя (`turn.backend`/`task.backend`) + `chip(RunState → Tone: Completed=Accent, Running=Neutral, Failed/Interrupted=Error)` + время `finished_at`; тело `turn.result`; если строк > 8 и `!view.expanded.contains(&turn.id)` — показать первые 8 строк + «Развернуть» (клик добавляет id); иначе всё + «Свернуть». Diagnostic — `tone_colors(Error).1`. Футер: чипы файлов/проверок; «Результат →» только если `Some(turn) == View::review_run(task)` — клик `view.select(Tab::Summary, Some(task.id))`.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p hub-app --lib gui::tasks`
Expected: PASS.

- [ ] **Step 5: Compact-проверка (Review Focus)**

Тест: окно высотой 300px, задача с результатом — `input_frame` не паникует, текст «Открыть результат» присутствует (инлайн-режим сохраняется). Добавить в тот же mod tests, прогнать.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-app/src/gui/tasks/result.rs crates/hub-app/src/gui/tasks/workspace.rs crates/hub-app/src/gui/tasks.rs
git commit -m "feat(gui): чат задачи пузырями с временем и сворачиванием ответов"
```

---

### Task 4: Композер одной строкой с авторостом

**Files:**
- Modify: `crates/hub-app/src/gui/tasks/workspace.rs` (`composer` ~382, `message_field` ~507)
- Test: `crates/hub-app/src/gui/tasks.rs` (mod tests)

**Interfaces:**
- Consumes: существующие `shortcut`, `primary_button`, черновики/конфликт-баннер.
- Produces: сигнатуры `composer`/`message_field` не меняются; `message_field` рисует поле внутри рамки композера, высота 1→8 строк (`body_line * 8.0 + 8.0` максимум), дальше внутренняя прокрутка.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn composer_field_grows_with_text_up_to_eight_lines() {
    // ввод 3 строк через Event::Text → высота TextEdit rect больше, чем у 1 строки;
    // ввод 20 строк → высота не превышает 8 строк + padding
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p hub-app --lib gui::tasks::tests::composer_field_grows`
Expected: FAIL (текущий максимум — 2 строки).

- [ ] **Step 3: Implement**

`composer`: рамка `egui::Frame` radius 12 border 1px surface; внутри `horizontal` (align bottom): `message_field` (grow) + `primary_button` справа. Подсказка Ctrl+Enter — под рамкой (существующий текст). Кнопки «Сохранить»/«Открыть результат» (compact) остаются в той же строке. `message_field`: `ScrollArea` max_height = 8 строк, `desired_rows(1)`, авторост по содержимому (egui multiline растёт сам до max_height). `char_limit`, `shortcut`, focus rework — без изменений.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p hub-app --lib gui::tasks && cargo test -p hub-app --lib gui::theme`
Expected: PASS (включая фокус-кольцо и существующие сценарии отправки).

- [ ] **Step 5: Commit**

```bash
git add crates/hub-app/src/gui/tasks/workspace.rs crates/hub-app/src/gui/tasks.rs
git commit -m "feat(gui): композер одной рамкой с авторостом до 8 строк"
```

---

### Task 5: Входящие — карточки с ответом без раскрытия

**Files:**
- Modify: `crates/hub-app/src/gui/tasks/attention.rs` (`Inbox`, `Item::show`, `row_header`, `show_attention`)
- Modify: `crates/hub-app/src/gui/tasks/local.rs` (обёртка карточки вокруг `decision`)
- Test: `crates/hub-app/src/gui/tasks/attention/tests.rs`

**Interfaces:**
- Consumes: `theme::{card, chip, Tone}`, `local::Panel::decision` (сигнатура не меняется).
- Produces:
  - `Inbox` поле `expanded` переименовывается в `details: Option<(Uuid, Uuid)>` — только раскрытие summary разрешения («Подробнее»).
  - `pub(super) fn project_attention(snapshot: &Snapshot, project: &Path) -> usize` — число элементов `items(snapshot)` этого проекта (для Task 7).
  - Заголовки групп со счётчиком: «Ждут вашего ответа · N».

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn question_card_answers_without_expand_step() {
    // снапшот с живым вопросом: фрейм сразу содержит текст вопроса и «Отправить ответ»;
    // клик по варианту + «Отправить ответ» → Command::LocalAnswer (без клика «Ответить»)
}

#[test]
fn permission_card_shows_actions_and_details_toggle() {
    // «Разрешить один раз» виден сразу; «Подробнее» раскрывает полный summary
}

#[test]
fn narrow_inbox_stacks_action_below_content() {
    // ширина 400px: кнопка «Проверить» ниже заголовка, фрейм без паник
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p hub-app --lib gui::tasks::attention::tests`
Expected: FAIL (новые тесты), существующие — могут упасть после шага 3, обновить в шаге 4.

- [ ] **Step 3: Implement cards**

`Item::Request`: карточка `theme::card`; шапка — иконка-плашка 26px (?/⚠), заголовок strong + meta small weak, чип типа справа. Вопросы: `panel.decision` рисуется сразу внутри карточки (без `expanded`); «Свернуть» убрать. Разрешение: summary monospace свёрнут до 3 строк, «Подробнее» переключает `details`; кнопки сразу. `Item::Task`: карточка + чипы проверок/файлов для Review + кнопка действия (существующий маппинг `Attention → Tab`); ширина < 440px — кнопка под содержимым. Группы: счётчик в заголовке. Пустое состояние и feedback — без изменений.

- [ ] **Step 4: Update existing tests and run**

Существующие тесты, кликавшие «Ответить»/«Свернуть», переводятся на прямой ответ. Run: `cargo test -p hub-app --lib gui::tasks`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hub-app/src/gui/tasks/attention.rs crates/hub-app/src/gui/tasks/attention/tests.rs crates/hub-app/src/gui/tasks/local.rs
git commit -m "feat(gui): входящие карточками, ответ и разрешение без раскрытия"
```

---

### Task 6: Шапка задачи — действия по состоянию и меню «⋯»

**Files:**
- Modify: `crates/hub-app/src/gui/tasks/workspace.rs` (`chat_header` ~332)
- Modify: `crates/hub-app/src/gui/tasks.rs` (тесты, кликавшие «Параметры»)
- Test: `crates/hub-app/src/gui/tasks.rs` (mod tests)

**Interfaces:**
- Consumes: `Request::{Status, Archive, Unqueue, Queue, Delete}`, `Command::LocalStop`, `topic_url`/opener тем, `result::Tab`.
- Produces: `fn header_actions(&mut self, ui: &mut egui::Ui, task: &Task, available: &Availability, snapshot: &Snapshot, send: &mut impl FnMut(Command))` — вызывается из `chat_header` справа от заголовка; меню «⋯» через `egui::menu::menu_button`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn running_local_task_stops_from_header() {
    // локальный выполняющийся запуск: клик «Остановить» → Command::LocalStop
}

#[test]
fn running_telegram_task_offers_topic_not_stop() {
    // scope Telegram + running: текст «Остановить» отсутствует, «Открыть тему» присутствует
}

#[test]
fn review_task_accepts_from_header() {
    // NeedsReview: «Принять» → Request::Status { status: Accepted }
}

#[test]
fn menu_archives_and_opens_parameters() {
    // клик «⋯» → «В архив» → Request::Archive; «⋯» → «Параметры задачи» → страница параметров
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p hub-app --lib gui::tasks::tests::running_local gui::tasks::tests::running_telegram gui::tasks::tests::review_task_accepts gui::tasks::tests::menu_archives`
Expected: FAIL.

- [ ] **Step 3: Implement header actions and menu**

Таблица состояний из спеки: локальный running → «Остановить» (danger-outline: `Button` с fg `tone_colors(Error).1`, шлёт `LocalStop` живого токена); Telegram running → чип «Выполняется» + «Открыть тему» (существующий opener тем); NeedsReview → «Открыть результат» (primary) + «Принять» (secondary, `Request::Status` Accepted, `available.edit`); очередь → «Снять с очереди» (`Unqueue`); Failed/Interrupted → «Разобрать запуск» (`Tab::Checks`). Меню «⋯»: «Параметры задачи», «Правила проекта», «Контекст следующего запуска» (открывает параметры с раскрытым контекстом), очередь — оба варианта «В очередь» / «В очередь в отдельном worktree» с существующими условиями видимости и валидации (при неготовности — disabled + hover-причина), «В архив»/«Восстановить», «Удалить…» (существующий confirm). Кнопка «Параметры» из шапки убирается.

- [ ] **Step 4: Update existing tests and run**

Тесты, кликавшие «Параметры» напрямую, получают предварительный клик «⋯» → «Параметры задачи». Run: `cargo test -p hub-app --lib gui::tasks`
Expected: PASS.

- [ ] **Step 5: Compact-проверка (Review Focus)**

Высота 300px: шапка с действиями не перекрывает композер; «Открыть результат» инлайн присутствует. Тест в тот же mod, прогнать.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-app/src/gui/tasks/workspace.rs crates/hub-app/src/gui/tasks.rs
git commit -m "feat(gui): действия задачи в шапке по состоянию и меню «⋯»"
```

---

### Task 7: Сайдбар — точки состояний и счётчик внимания

**Files:**
- Modify: `crates/hub-app/src/gui/tasks/sidebar.rs` (`Mark::paint` ~300, `ProjectRow::header` ~410)
- Test: `crates/hub-app/src/gui/tasks/sidebar.rs` или `gui/tasks.rs` (mod tests)

**Interfaces:**
- Consumes: `attention::project_attention` (Task 5), `theme::{tone_colors, review_color, Tone}`.
- Produces: `Mark::paint` — точки 8px: Running accent (пульсация), Review кольцо `review_color` 2px, Question accent, Warning/Error error-цвет, Idle border-цвет. Бейдж проекта — `chip(Tone::Accent, count)` только при count > 0.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn project_badge_counts_attention_items() {
    // снапшот: вопрос + review в одном проекте, другой проект чист →
    // бейдж «2» у первого, отсутствует у второго
}

#[test]
fn marks_use_status_colors() {
    // painter shapes: круг 8px нужного цвета для Running/Review/Warning
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p hub-app --lib gui::tasks::tests::project_badge gui::tasks::tests::marks_use`
Expected: FAIL.

- [ ] **Step 3: Implement**

`Mark::paint` по интерфейсу; пульсация — модуляция alpha через `ui.ctx().request_repaint_after(~500ms)` только при наличии Running-запусков в сайдбаре (иначе без repaint). Бейдж в `ProjectRow::header` после имени, до меню «⋯». Тултипы состояний и выделение строки — без изменений.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p hub-app --lib gui::tasks`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hub-app/src/gui/tasks/sidebar.rs crates/hub-app/src/gui/tasks.rs
git commit -m "feat(gui): читаемые состояния задач в сайдбаре и счётчик внимания"
```

---

### Task 8: Экран результата — вкладки вместо раскрытий

**Files:**
- Modify: `crates/hub-app/src/gui/tasks/result.rs` (`Tab`, `View::show` ~43)
- Test: `crates/hub-app/src/gui/tasks/result.rs` (mod tests), `gui/tasks.rs` (mod tests)

**Interfaces:**
- Consumes: `theme::{chip, Tone, tone_colors}`.
- Produces: `Tab::Files` вариант; `View::show` рисует строку вкладок (Сводка / Изменения · N / Проверки · X/Y / Файлы · N / История · N) и одну секцию; `Tab::Checks|Changes|History` deep-link из входящих сохраняется; `Tab::Files` — новый переход. Diff-цвета переезжают на `theme::tone_colors`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn review_tabs_switch_sections_one_at_a_time() {
    // запуск с diff, checks, files: по умолчанию виден result (Сводка);
    // клик «Файлы» → имена файлов видны, diff-строки скрыты
}

#[test]
fn inbox_check_link_lands_on_checks_tab() {
    // Attention::CheckFailed → «Открыть проверку» → вкладка Проверки активна
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p hub-app --lib gui::tasks::result::tests::review_tabs gui::tasks::tests::inbox_check_link`
Expected: FAIL.

- [ ] **Step 3: Implement tabs**

Строка вкладок — `theme::tab` (существующий) в `horizontal`; активная секция по `self.tab`. Сводка: result label + diagnostic + чипы файлов/проверок. Изменения: существующий `changes()` без изменений. Проверки: `report()`. Файлы: `files()`. История: существующий список попыток с одной ступенью раскрытия. `CollapsingHeader`-обёртки секций убрать. Нижняя панель принятия — без изменений (в `workspace::review`).

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p hub-app --lib gui::tasks`
Expected: PASS, включая тест diff-аннотаций.

- [ ] **Step 5: Full gate**

Run: `cargo test --workspace --locked && cargo clippy --workspace --all-targets --locked -- -D warnings`
Expected: PASS, без предупреждений.

- [ ] **Step 6: Визуальная проверка**

Запустить приложение (`cargo run -p hub-app`), проверить светлую и тёмную темы: чат, входящие, шапка, сайдбар, результат; низкое и узкое окно. Скриншоты приложить к PR.

- [ ] **Step 7: Commit**

```bash
git add crates/hub-app/src/gui/tasks/result.rs crates/hub-app/src/gui/tasks.rs
git commit -m "feat(gui): экран результата вкладками вместо вложенных раскрытий"
```

---

После фазы 1: релизные заметки — новые файлы задач не читаются старыми версиями (откат не поддерживается). План фазы 2 пишется отдельно.