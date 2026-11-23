# Обновление GUI — план фазы 2 (Темы, Новая задача) + удаление/синхронизация тем

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Обновить разделы «Темы Telegram» и «Новая задача» в утверждённом языке дизайна; сделать удаление несуществующей темы тихим (убрать из списка без ошибки API); добавить кнопку синхронизации тем, убирающую темы, которых уже нет в Telegram.

**Architecture:** GUI-карточки/меню поверх существующих команд. Бэкенд: устойчивое распознавание «thread not found» при удалении + новый метод мессенджера `probe_topic` (проверка существования через `edit_forum_topic` с пустым именем — имя/иконка не меняются) + команда `SyncTopics`, последовательно проверяющая темы и убирающая устаревшие локально.

**Tech Stack:** Rust 1.96, edition 2024, eframe/egui 0.36, teloxide 0.17 / teloxide-core 0.13, jiff 0.2. Без новых зависимостей.

**Spec:** `docs/specs/2026-11-22-gui-refresh-design.md` (фаза 2). Макеты: `.superpowers/brainstorm/3616026-1795399083/content/phase2-topics-newtask.html` (утверждены владельцем 2026-11-23).

## Global Constraints

- Линты: `warnings = deny`, clippy pedantic deny; `unwrap/expect/panic/indexing_slicing` вне тестов запрещены (в тестах разрешены).
- Без новых зависимостей; шрифты не добавлять; символы в GUI-метках только покрытые шрифтом (`… › ‹ √ ⏵ ⏹ ! ? · № ⌘ « » – — • ○`), регрессия `gui_labels_use_only_covered_glyphs`.
- Светлая/тёмная темы следуют ОС; токены `gui/theme.rs`, без хардкода цветов.
- UI-тексты русские; подтверждения опасных действий (сброс/удаление темы) остаются модалами.
- Команды/эффекты GUI не меняют логику ядра; новые команды следуют паттерну `DeleteTopic`.
- Проверки: `cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`.
- Не запускать GUI event loop (headless); визуальная проверка — владелец.

## Review Focus

- Удаление темы, которой нет в Telegram: список очищается, ошибки API нет (включая вариации описания ошибки). → Task 1.
- Синхронизация не удаляет живые темы и не трогает занятые; при RetryAfter не спамит API. → Task 2.
- `probe_topic` не меняет имя/иконку темы (пустое имя сохраняет текущие). → Task 1.
- Карточки тем и «Новая задача» читаются в узком/низком окне; плашки иконок фиксированы (не растягивают строку). → Task 3, 4.
- Тёмная тема: новые чипы/карточки читаются. → Task 3, 4.

---

### Task 1: Устойчивое удаление + probe_topic (hub-telegram)

**Files:**
- Modify: `crates/hub-telegram/src/messenger.rs` (trait Messenger)
- Modify: `crates/hub-telegram/src/telegram.rs` (delete_topic ~386, новый probe_topic)
- Modify: `crates/hub-telegram/src/testing.rs` (FakeMessenger, Call)
- Test: `crates/hub-telegram/src/telegram.rs` / `hub.rs` (mod tests)

**Interfaces:**
- Produces:
  - `Messenger::probe_topic(&self, key: TopicKey) -> BoxFuture<'_, Result<bool, SendError>>` — `Ok(true)` тема существует, `Ok(false)` устарела (thread not found), `Err` — неопределённость/RetryAfter.
  - внутренняя `fn thread_not_found(reason: &str) -> bool` — case-insensitive подстроки: `"thread not found"`, `"topic_deleted"`, `"topic_not_found"`, `"message_thread_not_found"`.
  - `Call::ProbeTopic(TopicKey)` в testing.rs; скриптование probe в FakeMessenger.

- [ ] **Step 1: Write failing tests**

```rust
// telegram.rs tests (or hub.rs): delete_topic maps not-found variants to Ok
#[test] fn delete_treats_missing_thread_as_success() {
    for reason in ["Bad Request: message thread not found", "Bad Request: TOPIC_DELETED",
                   "Bad Request: TOPIC_NOT_FOUND", "Bad Request: MESSAGE_THREAD_NOT_FOUND",
                   "Bad Request: some prefix message thread not found"] {
        assert!(thread_not_found(reason));
    }
    assert!(!thread_not_found("Bad Request: not enough rights"));
}
// probe: stale -> false, exists -> true, retry -> Err (FakeMessenger scripted)
```

- [ ] **Step 2: Run to verify fail** — `cargo test -p hub-telegram --lib thread_not_found probe` → FAIL (нет функций).

- [ ] **Step 3: Implement**
- `thread_not_found` helper; `delete_topic` использует её вместо точных сравнений (сохранить RetryAfter/Rejected/Failed ветки).
- `probe_topic`: `self.topics.edit_forum_topic(tg::ChatId(key.chat.0), thread(key), "").await` (пустое имя сохраняет текущее имя и иконку по API). `Ok(_)`→`Ok(true)`; `Api(Unknown(reason))` с `thread_not_found`→`Ok(false)`; `RetryAfter`→`Err`; иной `Api`→`Err(Rejected)`; иной→`Err(Failed)`.
- FakeMessenger: `probe_topic` со скриптом (по умолчанию `Ok(true)`), `Call::ProbeTopic`.

- [ ] **Step 4: Run to pass** — `cargo test -p hub-telegram --locked` → PASS.

- [ ] **Step 5: Commit** — `feat(telegram): тихое удаление несуществующей темы и probe существования`

---

### Task 2: Команда синхронизации тем (hub + supervisor)

**Files:**
- Modify: `crates/hub-telegram/src/hub.rs` (HubMessage, handler)
- Modify: `crates/hub-telegram/src/hub/deletion.rs` (переиспользовать clean_deleted_topic)
- Modify: `crates/hub-app/src/supervisor.rs` (Command, dispatch, busy-guard)
- Test: `crates/hub-telegram/src/hub.rs` (mod tests), `crates/hub-app/src/supervisor.rs`

**Interfaces:**
- Consumes: `Messenger::probe_topic` (Task 1), `clean_deleted_topic` (deletion.rs).
- Produces:
  - `HubMessage::SyncTopics { reply: oneshot::Sender<Result<String, String>> }`.
  - `Command::SyncTopics { reply: oneshot::Sender<Result<String, String>> }` в supervisor; диспетчеризация по паттерну DeleteTopic; включить в busy-guard match (~1168).
  - Отчёт: «Удалено устаревших тем: N» либо «Все темы актуальны»; при RetryAfter — «Telegram просит подождать N с, повторите позже».

- [ ] **Step 1: Write failing tests**

```rust
#[tokio::test] async fn sync_removes_stale_topics_and_keeps_live() {
    // две темы; probe скрипт: первая Ok(false), вторая Ok(true)
    // sync → reply Ok("...1..."), store содержит только вторую
}
#[tokio::test] async fn sync_skips_busy_and_deleted_topics() { /*.running/deleting не трогаем*/ }
#[tokio::test] async fn sync_stops_on_retry_after() { /*probe Err(RetryAfter) → notice, без спама*/ }
```

- [ ] **Step 2: Run to verify fail** — `cargo test -p hub-telegram --lib sync_` → FAIL.

- [ ] **Step 3: Implement**
- Handler `sync_topics`: при shutdown/несовпадении identity → Err. Собрать ключи (`thread>1`, не `topic_unavailable`, не занятые — переиспользовать проверки `check_deletion` без очереди доставки). Последовательно `probe_topic`; `Ok(false)` → `clean_deleted_topic(key)` (без удалённого delete); `Ok(true)` → оставить; `Err(RetryAfter)` → прервать с notice; иной `Err` → пропустить тему (не удалять при неопределённости). Reply с числом удалённых.
- supervisor: `Command::SyncTopics` → `mailbox.try_send(HubMessage::SyncTopics…)` (при закрытом ящике — Err в reply, как DeleteTopic).

- [ ] **Step 4: Run to pass** — `cargo test -p hub-telegram -p hub-app --locked` → PASS.

- [ ] **Step 5: Commit** — `feat(telegram): синхронизация тем убирает устаревшие`

---

### Task 3: Темы Telegram — карточки, меню, синхронизация (GUI)

**Files:**
- Modify: `crates/hub-app/src/gui/topics.rs`
- Test: `crates/hub-app/src/gui/topics.rs` (mod tests)

**Interfaces:**
- Consumes: `theme::{card, chip, Tone}`, `Command::SyncTopics` (Task 2), существующие StopTopic/ResetTopic/DeleteTopic/TopicMemory, `topic_url`, opener.
- Produces: карточки тем; кнопка «Синхронизировать» в заголовке; меню «…» на карточке.

- [ ] **Step 1: Write failing tests**

```rust
#[test] fn topic_card_shows_state_chip_and_sync_button_sends_command() {
    // карточка: имя + чип «работает»/«свободна»; клик «Синхронизировать» → Command::SyncTopics
}
#[test] fn topic_menu_offers_reset_and_delete_with_confirm() { /*меню «…» → Сбросить/Удалить → модал*/ }
```

- [ ] **Step 2: Run to verify fail** — `cargo test -p hub-app --lib gui::topics` → FAIL.

- [ ] **Step 3: Implement**
- Заголовок: «Темы Telegram» + кнопка «Синхронизировать» (шлёт SyncTopics, показывает notice из reply). Фильтры (поиск/Фильтры/комбо) без изменений.
- Строки → `theme::card`: плашка иконки фикс 26×26 (● работает / ○ свободна, покрытые символы), имя strong + чип состояния (работает→Accent, свободна→Neutral) + чип этапа при работе; meta `проект · бэкенд`; справа «Telegram» (opener) + меню «…».
- Меню «…»: Память темы (переключатель TopicMemory), Параметры и прогресс (раскрытие деталей), Открыть папку, Скопировать ссылку, Скопировать session_id, Сбросить сессию… (confirm), Удалить тему… (confirm, danger). Inline-раскрытие «Параметры» заменяется меню/деталями.
- При работе: «Остановить» на карточке (StopTopic). Подтверждения — существующие модалы.
- Плашка иконки через фикс-alloc (как attention::icon_plate), не растягивать строку.

- [ ] **Step 4: Run to pass** — `cargo test -p hub-app --locked` → PASS.

- [ ] **Step 5: Commit** — `feat(gui): темы Telegram карточками с меню и синхронизацией`

---

### Task 4: Новая задача — компактная форма (GUI)

**Files:**
- Modify: `crates/hub-app/src/gui/tasks/workspace.rs` (new_task ~103)
- Modify: `crates/hub-app/src/gui/tasks/context.rs` (context_controls)
- Test: `crates/hub-app/src/gui/tasks.rs` (mod tests)

**Interfaces:**
- Consumes: существующие project_choice/context_controls/local_project, черновики, rules.
- Produces: компактная строка селекторов; растущее поле поручения; видимое опциональное название; подсказка блокировки у кнопки.

- [ ] **Step 1: Write failing test**

```rust
#[test] fn new_task_title_field_is_visible_without_expanding() {
    // фрейм «Новая задача»: поле названия видно сразу (нет collapsing «Название задачи»)
}
```

- [ ] **Step 2: Run to verify fail** — `cargo test -p hub-app --lib gui::tasks::tests::new_task_title` → FAIL.

- [ ] **Step 3: Implement**
- Селекторы «Проект / Где запустить / Агент» компактной строкой (существующие комбо).
- Поле поручения — авторост как композер чата (1→8 строк), char_limit 16384, Ctrl+Enter.
- Название — видимое опциональное singleline с подсказкой «по умолчанию — первая строка поручения» (убрать `collapsing`).
- Причина блокировки — подсказкой у кнопки «Начать задачу»; «Сохранить черновик» secondary; ссылка «Правила проекта ›».

- [ ] **Step 4: Run to pass** — `cargo test -p hub-app --locked` → PASS.

- [ ] **Step 5: Commit** — `feat(gui): новая задача компактной формой с авторостом`

---

### Task 5: Полный гейт

- [ ] `cargo test --workspace --locked` + `cargo clippy --workspace --all-targets --locked -- -D warnings` → PASS.
- [ ] Владелец: визуальная проверка светлой/тёмной тем, узкого окна; синхронизация и удаление несуществующей темы на реальном боте.
- [ ] Commit при правках.

## Вызовы исполнителя дизайна

- Проверка существования темы — `edit_forum_topic` с пустым именем (API сохраняет имя/иконку): у Bot API нет метода перечисления тем; это наименее заметный probe. Цена ошибки: лишнее право can_manage_topics у бота (уже нужно для удаления).
- Синхронизация последовательная, при неопределённой ошибке тема НЕ удаляется (безопасно); при RetryAfter — пауза/повтор пользователем.