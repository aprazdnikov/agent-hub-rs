# Удаление проекта вместе с данными хаба

> **For agentic workers:** REQUIRED SUB-SKILL: Use `executing-plans` to implement this
> plan task-by-task; rules `rust-rules`, `existing-code`, `reliability-review`. Ревью —
> Qwen Code по «Review Focus».

**Goal:** Из GUI удаляется проект вместе со всеми его данными внутри хаба (профиль,
задачи и правила, память, ссылки тем, последние проекты); каталог самого проекта,
worktree внутри него и темы Telegram не затрагиваются.

**Architecture:** Новая команда supervisor `Command::DeleteProject` исполняет
идемпотентный каскад по хранилищам: hub-актор отвязывает проект от тем (`topics.json`) и
из `topics.projects.json`; `TaskService` вычищает задачи/правила из `topics.tasks.json`
(файлы задач удаляет уже существующий `purge_deleted_files`); `hub_memory::purge` убирает
запись из реестра `.projects` и каталог данных памяти; в последнюю очередь через штатный
путь `save()` из `settings.toml` удаляется профиль `[[projects]]`. Любая остановка на
среднем шаге повторяема: профиль ещё жив — проект виден, кнопка работает снова.

**Tech stack:** Rust 1.96, serde/serde_json, eframe/egui 0.36, jiff. Без новых зависимостей.

## Карта данных проекта (проверено по коду)

| Хранилище | Что привязано | Код |
|---|---|---|
| `config/settings.toml` | `[[projects]]` (id, name, cwd, backend, actions, checks, options) | `hub-core/src/settings.rs:385,581,1093`; GUI `hub-app/src/gui/settings.rs:504-585` |
| `data/topics.projects.json` | id в списке последних | `hub-app/src/topics.rs:37-46,138-141`; `hub-core/src/projects.rs:146-180` |
| `data/topics.json` | `TopicSession.preferences.project {id,name,options}` | `hub-core/src/options.rs:112-133`; `hub-core/src/topics.rs` |
| `data/topics.tasks.json` | `Task.project: PathBuf`, `ProjectRuleEntry.project: PathBuf` | `hub-core/src/tasks.rs:19-27,68-71,414-427,588,699` |
| `data/task-files/…` | файлы удалённых попыток | `hub-app/src/topics.rs:98-107` (авто-чистка при укорочении books) |
| `~/.agent-hub/memory/.projects` + `…/memory/<digest>/` | реестр биндингов и снимки памяти | `hub-memory/src/store/bindings.rs:14-95`, `hub-memory/src/lib.rs:12-14` |

Физический каталог проекта и `<project>/.agent-hub-worktrees` — вне каскада
(`hub-app/src/git/isolated.rs:11-40` их создаёт внутри проекта; удаление папки проекта —
не цель).

## Global Constraints

- Никогда не выполнять файловые операции вне каталогов хаба (`AppDirs`, memory base):
  канонизация и префикс-проверка путей до удаления; symlink — отказ.
- Каскад идемпотентен и упорядочен: темы → задачи → память → settings (профиль гасим
  последним). Частичный сбой возвращает сообщение «что уже удалено, что можно повторить».
- Сохранение настроек идёт штатным путём supervisor `save()` (`supervisor.rs:808`) со
  всей существующей семантикой перезапуска бота; в обход него писать `settings.toml` нельзя.
- Подтверждение удаления — модалка с вводом названия проекта (**D-2**), паттерн модалки
  удаления темы: `hub-app/src/gui/topics.rs:383-400`, `hub-app/src/gui/settings.rs:962-982`.
- Линты/gate: как в `docs/development.md`; старые файлы `topics.json`/`tasks.json`/
  `settings.toml` обязана читать новая сборка (round-trip тесты).

## Review Focus (для Qwen Code)

- Нет пути, при котором удаление проекта трогает каталог `cwd` или worktree: все удаленные
  пути выводятся из `AppDirs`/memory base и проверяются на префикс. → Task 2, 4.
- Задачи с активным запуском (`RunState::Running`, pending lease, git-подготовка) блокируют
  удаление понятным сообщением; очередь без активных запусков удаляется. → Task 5.
- `topics.json`: отвязка проекта не меняет session_id/backend/cwd темы — сессия продолжается;
  байт-в-байт round-trip формата сохранён. → Task 3.
- `hub_memory::purge` не оставляет записей в `.projects` и не задевает чужие digest-каталоги;
  при занятости (живой MCP-стор) — явная ошибка и повтор. → Task 2.
- Профиль проекта исчезает только после успешных шагов данных; при отказе шаг settings
  не выполнен. → Task 5.
- GUI: кнопка «Удалить проект» не удаляет молча из черновика; карточка, отсутствующая в
  сохранённых настройках (новый несохранённый профиль), снимается без каскада. → Task 6.

---

### Task 1: `hub-core` — чистые примитивы каскада

**Files:**
- Modify: `crates/hub-core/src/options.rs` (поле `project: Option<ProjectOptions>` — `:115`)
- Modify: `crates/hub-core/src/tasks.rs` (`TaskBook`, рядом с `rules:414`, `delete:699`)
- Modify: `crates/hub-core/src/projects.rs` (`RecentProjects`, `:156-180`)
- Test: те же файлы, `mod tests`

**Interfaces:**
- Produces:
  ```rust
  // options.rs — снимает привязку к проекту, возвращает true если что-то было
  pub fn Preferences::detach_project(&mut self, id: &str) -> bool;
  // tasks.rs
  impl TaskBook {
      /// Удаляет задачи проекта (все, где task.project == path) и его правила.
      /// Возвращает (удалённых задач, были ли правила).
      pub fn forget_project(&mut self, path: &Path) -> (usize, bool);
  }
  // projects.rs
  impl RecentProjects { pub fn forget(&mut self, id: &str); }
  ```
  `forget_project` сравнивает `fs-нормализованные` пути так же, как это делает
  `rules(&Path)` (тот же канонический вид); удаляет и `tasks`, и запись `rules`.

- [x] **Step 1: RED** — тесты: `detach_project` чистит `preferences.project`, не трогает
  `overrides`/`memory` и корректно сериализуется обратно (`dump_state/parse_state`);
  `forget_project` удаляет две задачи+правила одного пути и оставляет другой проект;
  повторный вызов → `(0,false)`; `RecentProjects::forget` идемпотентен.
- [x] **Step 2: Verify RED** — `cargo test -p hub-core --lib detach_project forget_project` → FAIL.
- [x] **Step 3: Implement** — три метода, минимально, без IO.
- [x] **Step 4: GREEN** — `cargo test -p hub-core --locked` → PASS.
- [x] **Step 5: Commit** — `feat(core): примитивы отвязки и забывания проекта`

---

### Task 2: `hub-memory` — `purge(base, project)`

**Files:**
- Modify: `crates/hub-memory/src/store/bindings.rs` (Location/Registry, `:95-137`)
- Modify: `crates/hub-memory/src/store.rs` / `lib.rs` (экспорт публичной функции)
- Test: `crates/hub-memory/src/store.rs` `mod tests` (fixture-паттерны соседних тестов)

**Interfaces:**
- Produces:
  ```rust
  /// Полностью убирает проект из памяти хаба: записи реестра `.projects`, ведущие к его
  /// identity, и каталог данных `base/<digest(identity)>`. Ничего вне base не трогает.
  pub fn purge(base: &Path, project: &Path) -> Result<PurgeReport, Error>;
  pub struct PurgeReport { pub removed_paths: usize, pub had_data: bool }
  ```
  Шаги: canonicalize(project) (NotFound → `Error::InvalidInput`) → `Location::resolve` →
  взять блокировку каталога данных (как `lock()` в bindings; живой MCP-стор → `Busy`) →
  удалить реестровые записи `path → Some(identity)` c совпадающим identity и саму запись
  `identity` → записать реестр атомарно → удалить каталог `directory()` рекурсивно.
  Несовпадение префикса `base` после canonicalize → `Error::InvalidInput` (защита от symlink).

- [x] **Step 1: RED** — тесты: проект с записями + move-биндинг (два пути к одному
  identity) → после purge реестр чист, каталог удалён, второй проект цел; повторный
  purge → `had_data:false, removed_paths:0`; нет каталога данных — только чистка реестра;
  путь вне base (symlink-ловушка) → ошибка.
- [x] **Step 2: Verify RED** — `cargo test -p hub-memory --lib purge` → FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: GREEN** — `cargo test -p hub-memory --locked` → PASS.
- [x] **Step 5: Commit** — `feat(memory): purge проекта из реестра и хранилища памяти`

---

### Task 3: `hub-telegram` — сообщение `ForgetProject` (темы + последние)

**Files:**
- Modify: `crates/hub-telegram/src/hub.rs` (`HubMessage:124+`, диспетчер `:601`, `projects: Projects`, `topics`, `store`)
- Modify: `crates/hub-telegram/src/projects.rs` (очистка recent через `forget` + `save_recent_projects`)
- Test: `crates/hub-telegram/src/hub.rs` `mod tests` (world/FakeAgents/FakeStore)

**Interfaces:**
- Produces: `HubMessage::ForgetProject { id: String, reply: oneshot::Sender<Result<usize, String>> }`
  — число отвязанных тем. Обработчик: для каждой темы с `preferences.project.id == id`
  применить `detach_project`; изменения записать `store.save(&topics)` **до** мутации
  состояния (паттерн `clean_deleted_topic`: сбой диска — Err, состояние не изменено); затем
  `self.projects.forget(id)` + `store.save_recent_projects`. `replace_topics` обновляет views.

- [x] **Step 1: RED** — тест: две темы одного проекта и одна другого → Ok(2), в `store.last()`
  у двух тем нет `project`, session_id/backend сохранены; сбой `store.fail` → Err и
  темы в прежнем виде; идемпотентный повтор → Ok(0).
- [x] **Step 2: Verify RED** — `cargo test -p hub-telegram --lib forget_project` → FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: GREEN** — `cargo test -p hub-telegram --locked` → PASS.
- [x] **Step 5: Commit** — `feat(telegram): отвязка проекта от тем по команде хаба`

---

### Task 4: `hub-app` — команда supervisor и каскад

**Files:**
- Modify: `crates/hub-app/src/supervisor.rs` (`Command:30-72`, dispatch `:687`, `save:808`, `finish_save`)
- Modify: `crates/hub-app/src/config.rs` (если нужен доступ к текущему Draft/Settings — уточнить по месту)
- Test: `crates/hub-app/src/supervisor.rs` `mod tests` (harness `:1372+`)

**Interfaces:**
- Produces:
  ```rust
  Command::DeleteProject { id: String, reply: oneshot::Sender<Result<String, String>> }
  ```
  Обработка (только когда бот запущен или настройки загружены):
  1. найти проект в текущих `Settings` (по `id`); нет — Ok c пометкой «профиль уже удалён».
  2. абсолютный путь: `hub_core::workspace::candidate(&settings.workspace_root, &home, Some(&cwd))`
     — так же, как резолвится при выборе проекта (проверить `projects.rs::confirm`).
  3. guard активности: через `TaskService::inspect` — есть `RunState::Running`/pending lease/
     git-задача у задач этого пути → Err «Есть активные задачи проекта…».
  4. каскад: `HubMessage::ForgetProject` (Task 3) → `tasks.access(Edit, forget_project)` →
     `spawn_blocking(hub_memory::purge)`; ошибки каждого шага аккумулируются в сообщение,
     порядок шагов позволяет повторить каскад кнопкой.
  5. успех шагов данных → черновик из текущих настроек без профиля
     (`Draft::from_settings(&settings)`, удалить `projects[i]`) → существующий `self.save(draft)`;
     reply Ok(сводка: «Проект удалён: N задач, M тем отвязано, память удалена»).
  6. результат `save` остаётся штатным (SaveStatus/notice), reply не ждёт записи settings —
     в сводке указать «профиль снимается сохранением настроек».

- [x] **Step 1: RED** — тесты supervisor-harness: каскад зовёт все четыре хранилища (spy),
  активная задача блокирует без побочных эффектов, отсутствующий memory-каталог не ошибка.
- [x] **Step 2: Verify RED** — `cargo test -p hub-app --lib delete_project` → FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: GREEN** — `cargo test -p hub-app --locked` → PASS.
- [x] **Step 5: Commit** — `feat(app): каскадное удаление проекта с данными хаба`

---

### Task 5: GUI — модалка подтверждения и предпросмотр объёма

**Files:**
- Modify: `crates/hub-app/src/gui/settings.rs` (`projects:504-585`, `confirm:962-982`, новые поля формы)
- Test: `crates/hub-app/src/gui/settings.rs` `mod tests` (render-паттерн `:1424`)

**Interfaces:**
- Производит: кнопка карточки `Удалить проект…` (вместо `Удалить проект:515`) открывает
  модальное окно (egui::Modal, тон Danger — паттерн модалки удаления темы) с:
  - названием и каталогом проекта; списком что удалится: профиль, задачи/правила, память,
    отвязка N тем, запись в «последних»; явной строкой «Каталог проекта <cwd> не удаляется»;
  - полем ввода названия проекта; кнопка «Удалить безвозвратно» активна только при точном
    совпадении;
  - отправки `Command::DeleteProject` и приёма `Command::Save`-не-нужен: форма после Ok-reply
    убирает карточку из `draft.projects` (для несохранённой карточки — просто снимает без
    команды).
  Счётчики для текста модалки: задачи — из уже загружаемого списка задач GUI
  (`tasks` snapshot), темы — из `handle.views` (фильтр `preferences.project.id`).

- [ ] **Step 1: RED** — render-тест: клик «Удалить проект…» открывает модалку; кнопка
  удаления неактивна без ввода имени; ввод имени → клик шлёт `Command::DeleteProject`.
- [ ] **Step 2: Verify RED** — `cargo test -p hub-app --lib delete_project_modal` → FAIL.
- [ ] **Step 3: Implement** (поверх существующего паттерна `confirming`).
- [ ] **Step 4: GREEN** — `cargo test -p hub-app --locked` → PASS.
- [ ] **Step 5: Commit** — `feat(gui): подтверждение удаления проекта с описанием каскада`

---

### Task 6: Документация и regression

**Files:**
- Modify: `docs/guide/projects.md`, `docs/guide/settings.md` (раздел проектов), `docs/guide/memory.md`
  (purge памяти), `docs/history.md`-строка не нужна до релиза.
- [ ] **Step 1:** обновить тексты: как удалить проект, что удаляется, что остаётся;
  сверить якоря ссылок по правилу doc-link (русские заголовки, slug без знаков препинания).
- [ ] **Step 2:** `cargo test --workspace --locked && cargo clippy --workspace --all-targets
  --locked -- -D warnings && cargo fmt --all --check && cargo deny check && git diff --check`.
- [ ] **Step 3: Commit** — `docs: удаление проекта вместе с данными хаба`

## Security Review этого плана

- Удаление файлов ограничено каталогами хаба: canonicalize + проверка префикса memory base
  перед удалением; symlink-и отклоняются (в проекте уже есть этот паттерн в
  `filesystem::destination` и `git/isolated.rs:35-40`).
- Модалка с вводом имени против случайного удаления; каскад не запускается для «чужого»
  проекта из устаревшего черновика (поиск id в сохранённых Settings).

## Reliability Review этого плана

- Порядок каскада выбран так, что повтор после сбоя безопасен (все шаги идемпотентны,
  профиль жив до конца); частичный результат различим по сообщению.
- Конкурентные записи: memory purge берёт тот же lock, что и MCP-стор; задачи пишутся через
  `TaskService` с `BusyLease`; `topics.json` пишет только hub-актор.
