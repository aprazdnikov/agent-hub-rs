# Синхронизация тем: надёжная очистка исчезнувших тем

> **For agentic workers:** REQUIRED SUB-SKILL: Use `executing-plans` to implement this
> plan task-by-task; rules `rust-rules` + `existing-code`. Ревью — Qwen Code по
> «Review Focus» и глобальному шаблону из
> [обзора](2026-11-25-improvements-overview.md).

**Goal:** Тема, удалённая в Telegram, гарантированно уходит из списка после «Синхронизировать»,
а отчёт синхронизации честно объясняет, что пропущено и почему.

**Architecture:** Классификатор ответа Bot API `topic_gone()` заменяет точечный подбор
подстрок `thread_not_found()` в `crates/hub-telegram/src/telegram.rs`; обход синхронизации
в `crates/hub-telegram/src/hub/deletion.rs` собирает категоризованный итог (удалено / живые /
занятые / не удалось проверить) вместо «Все темы актуальны» по умолчанию; подтверждённо
отсутствующая тема чистится даже с задачами в очереди — очередь отвязывается через
новый метод `TaskBook`.

**Tech stack:** Rust 1.96, teloxide 0.17 / teloxide-core 0.13, tokio, rstest. Без новых
зависимостей.

## Корень проблемы (проверено по коду)

1. `crates/hub-telegram/src/telegram.rs:389-394` — `thread_not_found()` ищет подстроки
   `"thread not found"`, `"topic_deleted"`, `"topic_not_found"`, `"message_thread_not_found"`
   **только** в `ApiError::Unknown(reason)` (`telegram.rs:402,429`). Реальные описания
   Telegram для удалённой темы приходят в форме `THREAD_NOT_FOUND` / `TOPIC_ID_INVALID`
   (подчёркивания, другой регистр) — матчер их не ловит, проба даёт
   `Err(Rejected)`, и `sync_topics` (`hub/deletion.rs:194`) **молча** пропускает тему.
   Тот же ложный отказ ломает и кнопку «Удалить» для уже отсутствующей темы
   (`delete_topic`, `telegram.rs:402`).
2. Синхронизация нигде не сообщает пропущенные темы: занятые исключены из кандидатов
   (`hub/deletion.rs:168`), ошибки пробы проглатываются (`:194`), итог при этом —
   «Все темы актуальны» (`:255`). Пользователь видит «синхронизация не помогает».
3. Тема с задачей в очереди не чистится никогда: `check_deletion()` возвращает Err на
   признаке очереди (`hub/deletion.rs:72-84`), а `finish_sync` повторяет проверку
   (`:220-224`). Для уже удалённой в Telegram темы это вечный блок.

## Global Constraints

- Линты: warnings deny, clippy pedantic; `unwrap/expect/panic/indexing_slicing` вне тестов запрещены.
- Строки ответа Telegram — недоверенные данные: никакого вывода токена/URL в логи и сообщения;
  в `tracing` писать только нормализованное описание ошибки.
- Сообщения пользователю на русском; символы только из покрытого шрифтом набора
  (`… › · « » —`), регрессия GUI-меток не должна сломаться.
- Изменение только в `hub-telegram`, `hub-core::tasks` и сообщениях GUI; поведение
  «живая тема остаётся» не меняется.
- Проверки: `cargo test -p hub-telegram --locked`, `cargo test --workspace --locked`,
  `cargo clippy --workspace --all-targets --locked -- -D warnings`.

## Review Focus (для Qwen Code)

- Классификатор матчит нормализованное описание (нижний регистр, `_`/`-`/повторные пробелы →
  одиночные пробелы) и применяется ко **всем** `ApiError`, а не только `Unknown`; ложных
  срабатываний на права/сеть нет (`CHAT_ADMIN_REQUIRED`, `not enough rights` → не «gone»). → Task 1.
- `probe_topic`: `Ok(false)` только по доказанному «gone»; таймаут/сеть → неопределённость,
  тема остаётся. → Task 1.
- Отчёт синхронизации: числа удалено/живые/занятые/не удалось сверяются с поведением FakeMessenger
  в тестах; «Все темы актуальны» только когда нет пропусков и ошибок. → Task 2.
- Удалённая в Telegram тема с задачами в очереди: тема уходит, задачи остаются с отвязанным
  `origin`, повтор синхронизации идемпотентен. Активный запуск — тема не удаляется. → Task 3.
- Диск: сбой сохранения по-прежнему повторяем (сообщение «повторите синхронизацию»). → Task 2.

## Открытые решения владельца

- **D-3 (подтвердить):** доказанно отсутствующую тему удаляем из хаба даже с задачами в
  очереди (отвязка origin), пропуская только темы с активным запуском/резервированной сессией.
- Наборы «gone»-строк фиксируются живой проверкой Task 4; в код попадают только
  подтверждённые описания (стандарт репозитория: «контракт сверялся с реальной сборкой»).

---

### Task 1: Классификатор `topic_gone` вместо точечного матчера (hub-telegram)

**Files:**
- Modify: `crates/hub-telegram/src/telegram.rs` (`thread_not_found:389-394`, `delete_topic:397-416`, `probe_topic:418-443`, tests `:740-760`, `topic_api:830-880`)
- Test: там же, `mod tests`

**Interfaces:**
- Produces:
  ```rust
  /// Нормализует описание ошибки Bot API и проверяет принадлежность к «тема отсутствует».
  fn topic_gone(reason: &str) -> bool
  ```
  Срабатывает на нормализованные формы: `thread not found`, `message thread not found`,
  `topic not found`, `topic deleted`, `topic id invalid`, `thread id invalid`
  (финальный список — после Task 4; до подтверждения живьём элементы помечать в комментарии).
- `delete_topic`/`probe_topic` матчат `Err(RequestError::Api(err))` через
  `topic_gone(&err.to_string())` (покрывает и типизированные варианты, и `Unknown`),
  ветки `RetryAfter`/иной `Api`/`_` сохраняются.

- [ ] **Step 1: Write failing tests** — unit-таблица `#[rstest] topic_gone_detects_missing_topic`
  со строками `"Bad Request: THREAD_NOT_FOUND"`, `"Bad Request: MESSAGE_THREAD_NOT_FOUND"`,
  `"Bad Request: TOPIC_ID_INVALID"`, `"Bad Request: topic not found"`,
  `"Bad Request: TOPIC_DELETED"` и негативы `"Forbidden: CHAT_ADMIN_REQUIRED"`,
  `"Bad Request: not enough rights"`; плюс через `topic_api`: ответ
  `{"ok":false,"error_code":400,"description":"Bad Request: THREAD_NOT_FOUND"}` →
  `delete_topic == Ok(())`, `probe_topic == Ok(false)`.
- [ ] **Step 2: Verify RED** — `cargo test -p hub-telegram --lib topic_gone` → FAIL (нет функции/кейсов).
- [ ] **Step 3: Implement** — нормализация (`to_lowercase`, разделители → пробелы, сжатие),
  константный список игл, замена матчеров в `delete_topic`/`probe_topic`.
- [ ] **Step 4: Verify GREEN** — `cargo test -p hub-telegram --locked` → PASS (в т.ч. старые
  тесты `topic_deletion_uses_bot_api_once`).
- [ ] **Step 5: Gates + commit** — `fix(telegram): надёжное распознавание удалённой темы в probe и delete`

---

### Task 2: Честный отчёт синхронизации (hub)

**Files:**
- Modify: `crates/hub-telegram/src/hub/deletion.rs` (`sync_topics:149-212`, `SyncDone:15-21`, `finish_sync:214-259`)
- Modify: `crates/hub-telegram/src/hub.rs` (тесты `delayed_topic_created_after_sync_stays_dropped:2837`, `sync_reports_cleanup_disk_failure:2861`)
- Test: `crates/hub-telegram/src/hub.rs` `mod tests`

**Interfaces:**
- Produces: в `SyncDone` поля `alive: usize`, `uncertain: usize` (сбой/неопределённый ответ
  пробы), счётчик занятых считается на стороне актора при сборе кандидатов. Сообщения:
  - только удалено: `Удалено устаревших тем: {n}.`
  - всё живо и пропусков нет: `Все темы актуальны.`
  - смешанный итог: `Удалено: {n}. Живых: {a}. Пропущено занятых: {k}. Не удалось проверить: {u} — повторите синхронизацию.`
  - к этому добавляются существующие хвосты про RetryAfter и сбой диска (не удалять).
- Каждый пропуск/ошибка пробы пишется `tracing::debug!` с ключом темы и категорией.

- [ ] **Step 1: Write failing tests** — в `hub.rs mod tests`: тема с `Ok(true)`, тема с ошибкой
  пробы (`Err(Rejected)` из скрипта FakeMessenger) и занятая тема → ответ содержит «Не удалось
  проверить: 1» и не содержит «Все темы актуальны»; после второй успешной пробы тот же ключ
  удалён и счётчики пересчитаны.
- [ ] **Step 2: Verify RED** — `cargo test -p hub-telegram --lib sync_reports` → FAIL.
- [ ] **Step 3: Implement** — категории в обходе; `finish_sync` собирает строку из частей.
- [ ] **Step 4: Verify GREEN** — `cargo test -p hub-telegram --locked` → PASS.
- [ ] **Step 5: Gates + commit** — `fix(telegram): синхронизация сообщает про пропущенные и непроверенные темы`

---

### Task 3: Удаление доказанно отсутствующей темы с отвязкой очереди

**Decision:** зависит от D-3.

**Files:**
- Modify: `crates/hub-core/src/tasks/queue.rs` (снимать `origin` у записи очереди) и
  `crates/hub-core/src/tasks.rs` (метод `TaskBook`)
- Modify: `crates/hub-telegram/src/hub/deletion.rs` (`finish_sync:214`, обход `check_deletion`)
- Modify: `crates/hub-telegram/src/hub.rs` (`Hub<A>` уже держит `tasks: Arc<TaskService>`)
- Test: `crates/hub-core/src/tasks.rs` `mod tests`; `crates/hub-telegram/src/hub.rs` `mod tests`

**Interfaces:**
- Produces:
  ```rust
  // hub-core
  impl TaskBook {
      /// Снимает origin очереди у всех задач, привязанных к теме. Возвращает число отвязанных.
      pub fn detach_topic_queue(&mut self, origin: &Origin) -> usize;
  }
  ```
  В `hub/deletion.rs` — `fn check_force_cleanup(&self, key) -> Result<(), ()>`: блокируют
  только активные состояния (`running`, `task_runs`, `snapshotting`, `deleting`,
  `handoff_busy`, `git_job`, активный execution, `is_reserved`); очередь и недоставленные
  ответы — нет.
- `finish_sync` для ключей из `stale` (Telegram доказал отсутствие): если
  `check_force_cleanup` ок → `deleted.insert` + `clean_deleted_topic` +
  `tasks.access(Access::Edit, detach_topic_queue)` (через существующий `self.tasks`),
  число отвязок попадает в отчёт («Задач откреплено: {d}»); иначе — категория «занятые».

- [ ] **Step 1: Write failing tests** — hub: тема в `stale`, у задачи `queue.origin == Some(..)` →
  тема удалена, `book.tasks()[0].queue.as_ref().unwrap().origin == None`; активный
  `running[&key]` → тема осталась и попала в «Пропущено занятых». Unit в hub-core:
  `detach_topic_queue` трогает только совпадающий origin и не меняет остальные очереди.
- [ ] **Step 2: Verify RED** — `cargo test -p hub-core --lib detach && cargo test -p hub-telegram --lib sync_gone` → FAIL.
- [ ] **Step 3: Implement** — оба метода; сообщение sync дополняется счётчиком откреплённых задач.
- [ ] **Step 4: Verify GREEN** — `cargo test -p hub-core -p hub-telegram --locked` → PASS.
- [ ] **Step 5: Gates + commit** — `fix(telegram): удалённая в telegram тема чистится с отвязкой очереди`

---

### Task 4: Живое подтверждение контракта + документация

**Files:**
- Modify: `crates/hub-telegram/src/testing.rs` — не требуется; живой тест в
  `crates/hub-app/tests/live_telegram.rs` (там уже есть шаблон `AGENT_HUB_LIVE_CHAT`)
- Modify: `docs/guide/telegram.md` (раздел синхронизации — новые сообщения),
  `docs/plans/2026-11-25-topic-sync-gone.md` (отметка выполненных шагов)

- [ ] **Step 1: Реализовать ignored-тест** `live_topic_gone_probe`: токен из keyring, в группе
  `AGENT_HUB_LIVE_CHAT` создаётся тема `probe-<uuid>`, сразу `delete_forum_topic`, затем
  `probe_topic` и `delete_topic` —assert` `Ok(false)`/`Ok(())`; `tracing::info!` печатает
  точное описание ошибки Telegram. Запуск:
  `cargo test -p hub-app --test live_telegram -- --ignored live_topic_gone`.
- [ ] **Step 2: Зафиксировать evidence** — вывод теста (или ручной прогон GUI-кнопки на трёх
  реальных темах) вписать в этот план коммитом `docs: живой контракт удалённой темы`. Если
  фактическая строка не покрыта матчером — дополнить список Task 1 отдельным коммитом с фиксом.
- [ ] **Step 3: Docs** — обновить описание кнопки «Синхронизировать» в руководстве;
  регрессия меток GUI не затронута.
- [ ] **Step 4: Gates + commit** — полный gate-набор, `git status -s` чист.

## Security Review этого плана

- Описание ошибки Telegram пишется в логи только нормализованным и без тела запроса —
  утечки токена нет (паттерн уже использован в `telegram.rs:438-440`).
- `detach_topic_queue` меняет только поле `origin`, статус задач сохраняется — нет тихой
  потери задач; модалка/отчёт сообщают число затронутых задач.

## Reliability Review этого плана

- Классификатор расширяет множество «доказанное отсутствие»; ложное удаление живою темы
  невозможно: `Ok(true)` и сетевые ошибки не удаляют. Решающее подтверждение — живая проба.
- Повторная синхронизация идемпотентна (маркер `deleted` + повторная очистка диска —
  существующий паттерн `finish_deletion`).
