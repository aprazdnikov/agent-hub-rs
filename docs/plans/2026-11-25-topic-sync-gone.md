# Синхронизация тем: надёжная очистка исчезнувших тем

> **For agentic workers:** REQUIRED SUB-SKILL: Use `executing-plans` to implement this
> plan task-by-task; rules `rust-rules` + `existing-code`. Ревью — Qwen Code по
> «Review Focus» и глобальному шаблону из
> [обзора](2026-11-25-improvements-overview.md).

**Goal:** Тема, удалённая в Telegram, гарантированно уходит из списка после «Синхронизировать»,
а отчёт синхронизации честно объясняет, что пропущено и почему.

**Architecture:** Классификатор ответа Bot API `topic_gone_evidence()` заменяет точечный подбор
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
   (подчёркивания, другой регистр; вторая — гипотеза, её подтверждает только Task 4) —
   матчер их не ловит, проба даёт
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
- **D-4 (новое, после security-гейта):** чем для отзывов доступа должна быть отвязка — сейчас
  `origin = None` выводит задачу из-под перепроверки allowlist на запуске и из-под настроек темы.
  Варианты: пауза с ошибкой вместо отвязки для недоступных пользователей (`pause_queue`) или
  отметка «тема утрачена», читаемая насосом. Вне объёма этого плана; требует решения владельца.
- Наборы «gone»-строк фиксируются живой проверкой Task 4; в код попадают только
  подтверждённые описания (стандарт репозитория: «контракт сверялся с реальной сборкой»).
  Подтверждённое множество — четыре формы релидного матчера v1.1.1; правдоподобные, но
  невиданные живьём формы живут в `UNCONFIRMED_TOPIC_GONE_MARKERS` и на удаление не влияют.

---

### Task 1: Классификатор `topic_gone_evidence` вместо точечного матчера (hub-telegram)

**Files:**
- Modify: `crates/hub-telegram/src/telegram.rs` (`thread_not_found:389-394`, `delete_topic:397-416`, `probe_topic:418-443`, tests `:740-760`, `topic_api:830-880`)
- Test: там же, `mod tests`

**Interfaces:**
- Produces:
  ```rust
  /// Классифицирует описание ошибки Bot API: доказанное отсутствие, кандидат или ничего.
  fn topic_gone_evidence(reason: &str) -> GoneEvidence
  ```
  Два множества нормализованных игл (финальный раунд ревью, Б1):
  - `CONFIRMED_TOPIC_GONE_MARKERS` — формы, уже принятые релидным матчером v1.1.1:
    `thread not found`, `message thread not found`, `topic not found`, `topic deleted`.
    Только они дают `GoneEvidence::Proven` и разрешают локальную очистку.
  - `UNCONFIRMED_TOPIC_GONE_MARKERS` — `topic id invalid`, `thread id invalid`: правдоподобные,
    но невиданные живьём формы. Дают `GoneEvidence::Candidate` — только
    `tracing::warn!(category = "unconfirmed_topic_gone")` с нормализованным описанием.
  Переезд кандидата в confirmed — решение Task 4 (живой контракт владельца), не кода.
- `delete_topic`/`probe_topic` классифицируют `Err(RequestError::Api(err))` через
  `topic_gone_evidence(&err.to_string())` (покрывает и типизированные варианты, и `Unknown`):
  `Proven` → `Ok(())`/`Ok(false)`, `Candidate` и `Unrelated` → тот же `Err(Rejected)`;
  ветки `RetryAfter`/иной `Api`/`_` сохраняются.

- [x] **Step 1: Write failing tests** — unit-таблица
  `#[rstest] topic_gone_evidence_splits_confirmed_from_candidates` со строками
  `"Bad Request: THREAD_NOT_FOUND"`, `"Bad Request: MESSAGE_THREAD_NOT_FOUND"`,
  `"Bad Request: topic not found"`, `"Bad Request: TOPIC_DELETED"` → `Proven`,
  `"Bad Request: TOPIC_ID_INVALID"`/`"Bad Request: THREAD_ID_INVALID"` → `Candidate`
  (финальный раунд ревью, Б1) и негативы `"Forbidden: CHAT_ADMIN_REQUIRED"`,
  `"Bad Request: not enough rights"` → `Unrelated`; плюс через `topic_api`: ответ
  `{"ok":false,"error_code":400,"description":"Bad Request: THREAD_NOT_FOUND"}` →
  `delete_topic == Ok(())`, `probe_topic == Ok(false)`, а `TOPIC_ID_INVALID`/`THREAD_ID_INVALID`
  → `Err(Rejected)` в обоих вызовах и ровно два `warn` категории `unconfirmed_topic_gone`
  (`unconfirmed_needle_is_reported_as_a_candidate_and_never_removes`).
- [x] **Step 2: Verify RED** — `cargo test -p hub-telegram --lib topic_gone` → FAIL (нет функции/кейсов);
  в финальном раунде — те же 5 кейсов FAIL на поведении (неподтверждённая игла давала `Ok`).
- [x] **Step 3: Implement** — нормализация (`to_lowercase`, разделители → пробелы, сжатие),
  два константных списка игл (`CONFIRMED_`/`UNCONFIRMED_TOPIC_GONE_MARKERS`) и warn кандидата,
  замена матчеров в `delete_topic`/`probe_topic`.
- [x] **Step 4: Verify GREEN** — `cargo test -p hub-telegram --locked` → PASS (в т.ч. старые
  тесты `topic_deletion_uses_bot_api_once`).
- [x] **Step 5: Gates + commit** — `fix(telegram): надёжное распознавание удалённой темы в probe и delete`

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
- Тема, пропущенная на этапе `finish_sync` (активная работа или удаление появились между пробой
  и обработкой `SyncDone`), прибавляется к счётчику занятых (`busy + skipped`) и пишется
  `tracing::debug!` с категорией `skipped` — «Все темы актуальны» невозможен при оставшейся
  в списке доказанно удалённой теме (раунд исправлений ревью, блокер 1).
- Строки `topics.json`, помеченные `deleted` после сбоя диска, `sync_topics` собирает отдельно
  (`pending`) и дозаписывает в `finish_sync` **без новой пробы**, попадая в счётчики
  `removed`/`failed`; повтор «Синхронизировать» — действительная инструкция восстановления
  (плановое «Диск: сбой сохранения повторяем», раунд исправлений ревью, блокер 2).

- [x] **Step 1: Write failing tests** — в `hub.rs mod tests`: тема с `Ok(true)`, тема с ошибкой
  пробы (`Err(Rejected)` из скрипта FakeMessenger) и занятая тема → ответ содержит «Не удалось
  проверить: 1» и не содержит «Все темы актуальны»; после второй успешной пробы тот же ключ
  удалён и счётчики пересчитаны.
- [x] **Step 2: Verify RED** — `cargo test -p hub-telegram --lib sync_reports` → FAIL.
- [x] **Step 3: Implement** — категории в обходе; `finish_sync` собирает строку из частей.
- [x] **Step 4: Verify GREEN** — `cargo test -p hub-telegram --locked` → PASS.
- [x] **Step 5: Gates + commit** — `fix(telegram): синхронизация сообщает про пропущенные и непроверенные темы`

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
  `check_force_cleanup` ок → `deleted.insert` + `clean_proven_gone` (отвязка очереди через
  существующий `self.tasks`, затем очистка строк `topics.json`); число отвязок попадает в отчёт
  («Задач откреплено: {d}»); иначе — категория «занятые».
- Порядок записи в `clean_proven_gone` — сначала отвязка очереди, потом строки `topics.json`:
  крах между двумя атомарными записями оставляет помеченную строку уже отвязанной, повтор
  синхронизации дозавершает очистку (`detach_topic_queue` вернёт 0). Ошибка отвязки прерывает
  очистку до записи на диск и попадает в отчёт отдельным хвостом «Не удалось открепить очередь
  задач … — повторите синхронизацию» на манер сбоя диска; проглатывать её в `Ok(0)` нельзя —
  это тот самый сиротский привязанный `origin` (раунд исправлений ревью, блокер 2).
- Ветка `delete_topic` по маркеру `deleted` зовёт тот же идемпотентный `clean_proven_gone`, а
  не только `clean_deleted_topic`: очистка одних строк оставила бы привязанный `origin` вне
  досягаемости следующих обходов (раунд исправлений ревью, блокер 1).

- [x] **Step 1: Write failing tests** — hub: тема в `stale`, у задачи `queue.origin == Some(..)` →
  тема удалена, `book.tasks()[0].queue.as_ref().unwrap().origin == None`; активный
  `running[&key]` → тема осталась и попала в «Пропущено занятых». Unit в hub-core:
  `detach_topic_queue` трогает только совпадающий origin и не меняет остальные очереди.
- [x] **Step 2: Verify RED** — `cargo test -p hub-core --lib detach && cargo test -p hub-telegram --lib sync_gone` → FAIL.
- [x] **Step 3: Implement** — оба метода; сообщение sync дополняется счётчиком откреплённых задач.
- [x] **Step 4: Verify GREEN** — `cargo test -p hub-core -p hub-telegram --locked` → PASS.
- [x] **Step 5: Gates + commit** — `fix(telegram): удалённая в telegram тема чистится с отвязкой очереди`

---

### Task 4: Живое подтверждение контракта + документация

**Files:**
- Modify: `crates/hub-telegram/src/testing.rs` — не требуется; живой тест в
  `crates/hub-app/tests/live_telegram.rs` (там уже есть шаблон `AGENT_HUB_LIVE_CHAT`)
- Modify: `docs/guide/telegram.md` (раздел синхронизации — новые сообщения),
  `docs/plans/2026-11-25-topic-sync-gone.md` (отметка выполненных шагов)

- [x] **Step 1: Реализовать ignored-тест** `live_topic_gone_probe`: токен из keyring, в группе
  `AGENT_HUB_LIVE_CHAT` создаётся тема `probe-<uuid>`, сразу `delete_forum_topic`, затем
  `probe_topic` и `delete_topic` — assert` `Ok(false)`/`Ok(())`. `delete_topic`/`probe_topic`
  пишут `tracing::debug!("topic api error")` с нормализованным `description` (раунд исправлений
  ревью, блокер 3), а `init_evidence_logging()` в тесте включает подписчик с этим debug-таргетом —
  прогон сам печатает доказательство, в том числе при падении assert. Запуск:
  `cargo test -p hub-app --test live_telegram -- --ignored live_topic_gone`.
  Тест подготовлен и компилируется, но НЕ запускался (живые проверки — на владельце).
- [x] **Step 2: Зафиксировать evidence** — прогон владельца через GUI (2026-11-25, тема 554
  в группе `-1004314310694`, уже удалённой в Telegram). Подтверждённые строки
  (`unconfirmed_topic_gone … description=…`, нормализованная форма):
  - `unknown error: "bad request: topic id invalid"` — получена дважды: на пробе
    «Синхронизировать» и на кнопке «Удалить».
  Итог по чек-листу:
  1. Форма `topic id invalid` подтверждена живым прогоном — переведена в
     `CONFIRMED_TOPIC_GONE_MARKERS` коммитом `fix(telegram): форма topic id invalid
     подтверждена живым прогоном`.
  2. `thread id invalid` в живом прогоне не наблюдалась — остаётся кандидатом.
  3. Ложных «gone»-строк (прав/транспорта) в прогоне не было.
  4. ~~Пустое имя в пробе `edit_forum_topic` не вызвало `TOPIC_NAME_NOT_EMPTY`~~ — вывод был
     ошибочным: лог владельца идёт на уровне INFO, а неклассифицированная ошибка пробы писалась
     только в DEBUG, поэтому отсутствие строк ничего не доказывало.
- [x] **Step 2bis: Второй живой прогон (2026-11-25, GUI, сборка с промоутенным маркером)** —
  кнопка «Удалить» чистит доказанно удалённую тему (`topic id invalid` из
  `deleteForumTopic`), но «Синхронизировать» отвечает «Не удалось проверить: 85» и пишет
  `WARN category=unclassified_topic_error description=invalid bot token`. Разбор:
  промежуточная версия пробы ходила в `getForumTopic` — **такого метода в Bot API нет**
  (это MTProto; в документации Bot API есть только `getForumTopicIconStickers`), сервер
  отвечает 404, а teloxide маппит `Unauthorized`/`Not Found` в
  `ApiError::InvalidToken("Invalid bot token")` — текст про токен misleading, про классификатор
  это не сбивает (Unrelated → тема остаётся, WARN показывает точную форму). Урок: ручной
  payload возможен только после сверки имени метода с официальной документацией.
- [x] **Step 2ter: Контракт пробы — `editForumTopic` с сохранённым заголовком.**
  Проба принимает `name` (снимок `titles` на момент обхода): имя отправляется только когда
  заголовок известен — пустой `name` (старая форма пробы) живым прогоном не подтверждена и
  может отсекается валидацией до проверки существования темы. Если заголовок в хабе совпадает
  с Telegram (ренаймы отслеживаются сервисными сообщениями), запись — no-op; расхождение
  переименует тему в сохранённое имя — обратимо и восстанавливается следующим ренеймом.
  Неклассифицированный ответ пробы пишется `tracing::warn!` с категорией
  `unclassified_topic_error` — форма ошибки видна оператору на дефолтном уровне логов.
  Проверки за владельцем (живой прогон GUI на этой сборке):
  1. ghost-темы уходят после «Синхронизировать», ИЛИ в логе появляется WARN с реальной формой
     ответа `editForumTopic` на удалённой теме (она станет кандидатом/подтверждением).
  2. Живые темы остаются с прежними названиями (`editForumTopic` с тем же именем — no-op).
  3. Закрытая (не удалённая) тема: записать форму ответа — ожидание «uncertain», не «gone».
- [x] **Step 2quater: Третий живой прогон (2026-11-25, сборка Step 2ter) — edit отвергнут.**
  Отчёт: «Живых: 66. Не удалось проверить: 7». Все семь WARN — `topic not modified`, и это
  самые **свежие** треды (22366–24131), т.е. живые темы, у которых имя совпало; старые ghost-треды
  (15736–16546) в предупреждениях не появились — `editForumTopic` на удалённой теме отвечает
  успехом и проба считала их живыми. Вывод: правка темы принципиально не годится как проба на
  существование (Ok не означает существование). Проба переведена на `sendChatAction(Typing)` —
  тот же путь валидации треда, что у обычной отправки (отправка в удалённый топик исторически
  даёт `message thread not found` — уже подтверждённый маркер). Видимый эффект: короткий
  «печатает…» в живых темах во время синхронизации. Ожидания следующего прогона: ghost-темы
  уходят; если закрытая тема даст свою форму ошибки — она попадёт в WARN как `uncertain`,
  живые подтвердятся через Ok. Если `sendChatAction` окажется слепым к удалённым топикам
  (Ok на ghost), проба переезжает на detection-at-write: поведению `sendMessage` с теми же
  маркерами, а синхронизация остаётся кнопкой «Удалить».
  Разделение множеств (финальный раунд ревью, Б1) сохранено как механизм: удаление строки
  темы и маркер `deleted` возможны только по `CONFIRMED_TOPIC_GONE_MARKERS`;
  `UNCONFIRMED_TOPIC_GONE_MARKERS` (`thread id invalid`) лишь пишут `tracing::warn!` с
  категорией `unconfirmed_topic_gone`. Ложное «gone» при этом невозможно: неполное покрытие
  сознательно принесено в жертву восстановимости.
- [x] **Step 2quint: Четвёртый живой прогон (2026-11-25) — flood control.**
  Сплошной обход ~73 тем со скоростью ~2 запроса/с упёрся в групповой лимит Telegram
  («Telegram просит подождать 18 с») — до призраков обход не дошёл. Добавлено:
  темп обхода `DEFAULT_SYNC_PROBE_PACING` = 1.5 с на пинг; TTL-кэш подтверждённо-живых
  `DEFAULT_SYNC_ALIVE_TTL` = 10 мин (повторное нажатие не перепингует только что живые
  темы; кэш пополняется только честными `Ok(true)`, чистится при удалении строки);
  частичный прогресс при 429 уже был — остановка, чистка найденного, продолжение со
  следующего неповторённого. Полный обход теперь ~2 мин вместо 429 на середине.
  Регрессия: `sync_reuses_recent_alive_answers`.
- [x] **Step 3: Docs** — обновить описание кнопки «Синхронизировать» в руководстве;
  регрессия меток GUI не затронута.
- [x] **Step 4: Gates + commit** — полный gate-набор, `git status -s` чист.

## Security Review этого плана

- Описание ошибки Telegram пишется в логи только нормализованным и без тела запроса —
  утечки токена нет (паттерн уже использован в `telegram.rs:438-440`); то же к
  `warn` неподтверждённого кандидата.
- `detach_topic_queue` меняет только поле `origin`, статус задач сохраняется — нет тихой
  потери задач; число откреплённых видят только отчёт синхронизации (`Задач откреплено: D`),
  модального подтверждения у кнопки «Синхронизировать» нет.
- Не закрыто этим планом (вне Б1, на решении владельца): у отвязанной задачи
  `origin == None`, поэтому перепроверка прав на запуске (`chat` + allowlist в
  `hub/tasks/shared.rs`) и надстройки темы к ней уже не применяются — отзыв пользователя
  из `telegram.users` не останавливает такую задачу, и она может уйти в насос рабочего
  стола при остановленном боте. См. «Открытые решения владельца».

## Reliability Review этого плана

- Классификатор расширяет множество «доказанное отсутствие» только формами релидного матчера;
  ложное удаление живою темы невозможно: `Ok(true)`, сетевые ошибки и невиданные живьём формы
  (`GoneEvidence::Candidate`) не удаляют и не ставят маркер — последние пишутся в `warn` как
  кандидаты для Task 4. Решающее подтверждение — живая проба.
- Повторная синхронизация идемпотентна (маркер `deleted` + повторная очистка диска —
  существующий паттерн `finish_deletion`).
