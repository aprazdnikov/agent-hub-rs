# Общие роли агентов + настройка для проекта

> **For agentic workers:** REQUIRED SUB-SKILL: Use `executing-plans` + `tui-design`
> (проектирование модалки/формы) и `rust-rules`. Ревью — Qwen Code по «Review Focus».
> Дизайн-блок ниже — согласованное с владельцем направление «сразу удобно и продуманно»;
> отклонения — только через правку этого файла.

**Goal:** В настройках появляется общая секция «Роли в задачах» (исполнитель и ревьюер по
умолчанию), а проект выбирает режим: выключено / наследовать общие роли / свой выбор —
без потери обратной совместимости `tasks.json` и `settings.toml`.

**Architecture:** Единая чистая функция резолвинга `ProjectRules::effective_workflow(&AgentRoles)`
в `hub-core` — единственный источник правды; все точки использования
(`local_execution.rs:450`, `gui/tasks/workspace.rs:988`, `ProjectRules::describe:54-63`,
редактор правил `gui/tasks/rules.rs:28-51`) переходят на неё. Модель выбора — три состояния:
`workflow: Option<WorkflowChoice>`, где отсутствие поля = выключено (сегодняшний `None`),
`Inherit { command }` = общие роли со своей командой, `Custom(WorkflowPreset)` = прежний
объект (serde untagged, обратная совместимость `tasks.json` без миграции). Роли живут в
`settings.toml [agents] implementer/reviewer` рядом с `default` и доезжают до исполнителя
через `ExecutionSettings` (`hub-core/src/settings.rs:2316`).

**Tech stack:** Rust 1.96, eframe/egui 0.36, serde untagged-enum. Без новых зависимостей.

## Текущее состояние (проверено)

- `WorkflowPreset { implementer, command, reviewer }` — только внутри правил проекта:
  `hub-core/src/tasks/workflow.rs:7-11`; применение — `crates/hub-app/src/local_execution.rs:450`
  (`rules.workflow.filter(|p| p.implementer == session.backend)`), проверка→ревью —
  `local_execution/workflow.rs:16-110` (ревьюер получает `TopicSession::fresh(preset.reviewer,…)`).
- GUI: переключатель и комбобоксы — `gui/tasks/rules.rs:28-51` (дефолты при включении:
  Codex/Claude); фильтр чипа в воркспейсе — `gui/tasks/workspace.rs:988-992`.
- Общая настройка есть только для «агента новых тем» — `Settings::default_backend`
  (`settings.rs:389`), секция `[agents]` (`settings.rs:1144-1146`), страница GUI
  `gui/settings.rs:688-700`.

## Дизайн UX (реализовать точно так)

1. **Настройки → Агенты → Общие.** После блока «Новые темы» — блок «Роли в задачах»:
   - строка `Исполнитель по умолчанию` + комбобокс из `BackendKind::ALL`;
   - строка `Ревьюер по умолчанию` + комбобокс;
   - подпись: «Используется проектами в режиме „Наследовать общие роли“. Команда проверки
     остаётся своей у проекта.»
   Дефолты при отсутствии секции в файле: исполнитель Codex, ревьюер Claude (совпадают с
   текущими дефолтами GUI).
2. **Задача → проект → Правила.** Блок «Исправить и проверить автоматически» становится
   сегмент-выбором из трёх состояний (radio-строки, как существующие поля формы):
   - `Выключено` — ничего не применяется (сегодняшний `None`/снятый чекбокс);
   - `Наследовать общие роли` — один поле-инпут «Доверенная команда проверки»; над ним
     строка факта: «Исполнитель: codex · Ревьюер: claude (общие роли)», значения берутся
     из текущих настроек и меняются «на лету» при их правке;
   - `Свой выбор` — прежние два комбобокса + инпут команды.
   Валидация: в режимах «Наследовать» и «Свой выбор» команда обязательна (пустая → ошибка
   сохранения, текст как сейчас в `workflow.validate`); в «Выключено» — нет.
   Подсказка режима «Наследовать»: «Ревьюер меняется для новых запусков сразу после правки
   общих ролей; запущенная попытка доигрывает старую конфигурацию.»
3. **Карточка проекта в настройках** — без изменений (роли не дублируем в профиль проекта:
   один источник — Правила проекта на странице задач), в описании профиля добавить строку
   «Режим проверок настраивается на вкладке „Правила“ страницы „Задачи“».
4. Отчёт запуска (`WorkflowReport::describe`, `gui/tasks/result.rs:494`) показывает
   фактических агентов попытки — данные уже лежат в `run.rules`/preset, менять формат не нужно.

## Global Constraints

- Wire-совместимость: старый `tasks.json` (workflow: объект или отсутствие) читается без
  миграции; новый файл, прочитанный старой версией приложения, не обязан работать — но
  round-trip тест «записали новой версией → прочитали той же структурой» обязателен.
- `ProjectRules::validate` сохраняет смысл: `Custom` валидирует пресет, `Inherit` требует
  команду; `Off` всегда валиден.
- Никакого «строкового» режима: только enum; запрет на Option<bool>-флаги.
- GUI-символы — только покрытые шрифтом; русские подписи; регрессия меток не ломается.
- Gate: `cargo test --workspace --locked`, clippy `-D warnings`, fmt, deny.

## Review Focus (для Qwen Code)

- Единственная точка резолвинга ролей: `grep -n "effective_workflow" crates/` покрывает
  `local_execution.rs:450`, `workspace.rs:988`, `rules.rs`, `tasks.rs::describe`; прямого
  `.workflow` больше нет. → Task 2, 3, 4.
- `Off` для проектов без workflow — поведение байт-в-байту как до изменения (ни проверок,
  ни ревью). → Task 1.
- Родительский фильтр `p.implementer == session.backend` применяется к **разрешённому**
  пресету, к глобальному сам по себе — нет (иначе глобальные роли включали бы workflow
  всем проектам). → Task 3.
- Смена общих ролей в настройках не требует пересохранения правил проекта; в
  `run.rules.revision`-гард (`local_execution.rs:437-440`) не попадает глобальная правка. → Task 3.
- Сериализация: `"inherit"`/`"off"` строки, объект — пресет; денормализованные значения →
  ошибка разбора, не тихий `Off`. → Task 1.

---

### Task 1: `hub-core` — `WorkflowChoice` + роли в настройках

**Files:**
- Modify: `crates/hub-core/src/tasks/workflow.rs` (новый enum, сериализация, validate)
- Modify: `crates/hub-core/src/tasks.rs` (`ProjectRules.workflow:26`, `describe:54-63`,
  `validate:29-40`, тесты `:205,278,870+`)
- Modify: `crates/hub-core/src/settings.rs` (`AgentsFile:1144`, `Settings:380-397`,
  `Field:400-420`, `Draft::from_settings:1230`, parse `:1297`, `SettingsFile:1085-1112`,
  `ExecutionSettings:2316`)
- Test: те же файлы

**Interfaces:**
- Produces:
  ```rust
  // tasks/workflow.rs — три состояния выбора; «выключено» остаётся отсутствием поля
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(untagged)]
  pub enum WorkflowChoice {
      Inherit { command: String },          // wire: {"command": "cargo test"}
      Custom(WorkflowPreset),               // wire: прежний объект {implementer, command, reviewer}
  }
  // tasks.rs: ProjectRules.workflow: Option<WorkflowChoice>
  // (serde default + skip_serializing_if = Option::is_none — None == выключено, как сейчас)
  impl WorkflowChoice { pub fn command(&self) -> &str; }
  // tasks/workflow.rs
  impl ProjectRules {
      /// Разрешённый пресет проекта: Inherit собирает агентов из roles, команду берёт
      /// из правил; Custom возвращается как есть; None -> None.
      pub fn effective_workflow<'a>(&'a self, roles: &'a AgentRoles)
          -> Option<std::borrow::Cow<'a, WorkflowPreset>>;
      pub fn validate(&self) -> Result<(), String>;  // Inherit: command non-empty (2048/\\0)
  }
  // settings.rs
  pub struct AgentRoles { pub implementer: BackendKind, pub reviewer: BackendKind }
  // + Settings.roles; AgentsFile.implementer/reviewer: Option<String>;
  // + Field::Role(&'static str) для ошибок разбора; ExecutionSettings.roles (From<&Settings>).
  ```
  Порядок untagged важен: объект с `implementer` не должен сворачиваться в `Inherit` —
  проверить в
  тестах round-trip оба направления. Дефолты `AgentRoles::default() = { Codex, Claude }`;
  парсинг строк — как `default_backend` через `choice()` (`settings.rs:1297`).

- [x] **Step 1: RED** — тесты round-trip `ProjectRules`: отсутствие поля → None (выключено);
  `workflow = { command = "cargo test" }` → Inherit; старый трёхполевой объект → Custom;
  `{}` и `42` → ошибка разбора книги. `effective_workflow`: None → `None`;
  `Custom(p)` → `Some(Cow::Borrowed(p))`; `Inherit{command}` →
  `Some(Cow::Owned(WorkflowPreset { implementer: roles.implementer,
  reviewer: roles.reviewer, command }))`. `validate`: Inherit с пустой командой → ошибка,
  с командой > 2048 байт или с `\0` → ошибка. Настройки: секция `[agents]` с
  `implementer`/`reviewer` парсится, неизвестное имя агента → `Field::Role`,
  `Draft::from_settings` round-trip, старый TOML без новых ключей читается.
- [x] **Step 2: Verify RED** — `cargo test -p hub-core --lib workflow_choice agents_roles` → FAIL.
- [x] **Step 3: Implement.**
- [x] **Step 4: GREEN** — `cargo test -p hub-core --locked` → PASS.
- [x] **Step 5: Commit** — `feat(core): трёхрежимный workflow проекта и общие роли агентов`

---

### Task 2: GUI-настройки — секция «Роли в задачах»

**Files:**
- Modify: `crates/hub-app/src/gui/settings.rs` (`agent():688-700`, Draft-поля, `page()` `:343`)
- Test: там же `mod tests`

- [x] **Step 1: RED** — render-тест: на странице «Агенты → Общие» есть два комбобокса ролей;
  смена значения помечает черновик грязным и попадает в `Command::Save`.
- [x] **Step 2: Verify RED** — `cargo test -p hub-app --lib roles_section` → FAIL.
- [x] **Step 3: Implement** — по образцу блока «Новые темы» (`field_row` + `ComboBox`).
- [x] **Step 4: GREEN** — `cargo test -p hub-app --locked` → PASS.
- [x] **Step 5: Commit** — `feat(gui): общие роли исполнителя и ревьюера`

---

### Task 3: Резолвинг в исполнении и отчётах

**Files:**
- Modify: `crates/hub-app/src/local_execution.rs:437-460,508-517`
- Modify: `crates/hub-telegram/src/hub/tasks.rs:551,598-607,661-667` (показ правил и
  превью — через `describe` c ролями)
- Modify: `crates/hub-core/src/tasks.rs:54-63` (`describe` принимает `&AgentRoles` или
  отдельный `describe_with_roles`)
- Test: `local_execution/tests.rs` (паттерн `ScriptedWorkflow:408+`), `hub.rs` tests

- [x] **Step 1: RED** — тест: проект `Inherit{command}` + `ExecutionSettings.roles = {Qwen,
  Hermes}` → запускается verify с исполнителем Qwen (backend сессии Qwen) и ревьюером Hermes;
  тот же проект с `Off` → verify нет; `Custom{implementer=Codex,...}` при сессии Codex →
  прежний фильтр сработал; глобальные роли не включают workflow у проекта `Off`.
- [x] **Step 2: Verify RED** — `cargo test -p hub-app --lib inherit` → FAIL.
- [x] **Step 3: Implement** — во всех точках `rules.workflow` → `effective_workflow(&settings.roles)`;
  в telegram-хьюке роли берутся из watch-канала настроек (`self.settings`).
- [x] **Step 4: GREEN** — `cargo test -p hub-app -p hub-telegram --locked` → PASS.
- [x] **Step 5: Commit** — `feat(app,telegram): исполнение учитывает общие роли агентов`

---

### Task 4: GUI правил проекта — трёхрежимный блок

**Files:**
- Modify: `crates/hub-app/src/gui/tasks/rules.rs:28-51` (workflow_settings)
- Modify: `crates/hub-app/src/gui/tasks/workspace.rs:988-992` (чип использует эффективный пресет)
- Modify: проброс `AgentRoles` в редактор (через существующий snapshot настроек GUI —
  уточнить по вызовам `Editor::observe`; если не проброшено — добавить параметром)
- Test: `rules.rs` render-тесты

- [ ] **Step 1: RED** — тесты: выбор `Inherit` показывает строку факта с глобальными агентами
  и инпут команды; переключение `Custom` возвращает два комбобокса; `Off` скрывает всё;
  сохранение шлёт `Request::…` с корректным `WorkflowChoice`; валидация пустой команды
  блокирует кнопку.
- [ ] **Step 2: Verify RED** — `cargo test -p hub-app --lib workflow_modes` → FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: GREEN** — `cargo test -p hub-app --locked` → PASS.
- [ ] **Step 5: Commit** — `feat(gui): режим workflow проекта — выкл / общие роли / свой выбор`

---

### Task 5: Документация

**Files:**
- Modify: `docs/guide/tasks.md` (раздел правил проекта), `docs/guide/settings.md`
  (страница «Агенты»), `docs/guide/projects.md` (строка про вкладку «Правила»)
- [ ] **Step 1:** тексты по дизайн-блоку; сверка якорей рус. заголовков (правило doc-link).
- [ ] **Step 2:** полный gate + `git diff --check`; `git status -s` чист.
- [ ] **Step 3: Commit** — `docs: общие роли агентов и режимы проверок проекта`

## Security Review этого плана

- Команда проверки остаётся обязательным полем проекта и выполняется тем же механизмом
  `actions::run` с теми же ограничениями — новых путей исполнения нет; глобальная настройка
  не вводит «глобальной команды».
- Разбор `WorkflowChoice` на границе (serde untagged + явный default Off) без stringly-логики;
  мусор в `tasks.json` даёт ошибку разбора книги, а не тихое включение проверок.

## Reliability Review этого плана

- Обратная совместимость: старые `tasks.json`/`settings.toml` читаются без миграции
  (тесты Task 1); отсутствие поля == текущий `None`.
- Разделение эффектов: глобальные роли влияют только на новые запуски (резолвинг в момент
  начала попытки), mid-run правила не меняются — существующий revision-guard сохранён.
