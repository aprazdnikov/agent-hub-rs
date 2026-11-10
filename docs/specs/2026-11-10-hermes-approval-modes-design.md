# Режимы одобрений Hermes

> Спецификация доработки бэкенда [Hermes](2026-10-09-hermes-backend-design.md).
> [Каталог документов](../README.md) · [Порядок этапов](../history.md).

## Цель и границы

Hermes был единственным бэкендом без настраиваемого режима одобрений: хаб всегда
запускал `hermes acp` без обхода подтверждений и после открытия сессии принудительно
выставлял `session/set_mode` с `modeId: default`. Требование — вынести это в настройки
как у Claude (`permission_mode`), Codex (`sandbox`/`approval`) и Qwen (`approval`),
включая режим «yolo».

Изменение затрагивает `hub-core` (домен и разбор настроек), `hub-hermes` (argv процесса и
`session/set_mode`) и `hub-app` (форма настроек). Поведение Telegram-команд `/mode` и
`/model` не меняется как код: Hermes подключается к уже существующему реестру режимов.
Claude, Codex, Qwen и OpenCode сохраняют прежнее поведение. Значение по умолчанию —
`default`, то есть ровно то, что делал хаб до изменения.

## Контракт Hermes

Контракт сверялся с установленной сборкой `Hermes Agent v0.22.5` (2026.9.24) и её
исходниками в `~/.hermes/hermes-agent`, а также живой ACP-пробой `initialize` →
`session/new` → `session/set_mode`.

1. `--yolo` — глобальная опция до подкоманды. `hermes --yolo acp --version` завершается
   `rc=0`; `hermes acp --yolo --version` даёт `hermes: error: unrecognized arguments:
   --yolo` (`rc=2`). Опция стоит в одной группе с `--profile`, который хаб передавал и
   раньше.
2. `--yolo` выставляет `HERMES_YOLO_MODE=1` в `main()` до discovery
   (`hermes_cli/main.py:3533`), `tools/approval.py:46` замораживает это в
   `_YOLO_MODE_FROZEN` при импорте, и обход проверяет `_bypass_active`
   (`tools/approval.py:507-510`: `--yolo`, session-`/yolo`, `approvals.mode: off`).
   Обход относится только к подтверждениям опасных команд.
3. Подтверждения правок файлов — отдельный ACP-гейт (`acp_adapter/edit_approval.py`),
   который управляется режимом сессии, а не yolo: `hermes --yolo acp` в пробе вернул
   `currentModeId: "default"`. Таблица режимов — `acp_adapter/modes.py:11-16`:
   `default` (политика `ask`), `accept_edits` (`workspace_session`), `dont_ask`
   (`session`).
4. `yolo` не является режимом ACP: `session/set_mode` с `modeId: "yolo"` в пробе вернул
   `-32603` с `data.details = "invalid_params"`. Режимы объявляются в ответе
   `session/new`/`session/load` как `modes.availableModes[].id` (форма —
   `acp_adapter/gateway_session_meta.py:65-68`, `acp_adapter/server.py:279-289`).
5. Часть подтверждений не обходится ни при каком режиме: чувствительные пути (`.git`,
   `.ssh`, имена из списка секретов) спрашиваются всегда
   (`acp_adapter/edit_approval.py:144-157, 149-171`), правила оператора
   `approvals.deny` документированы как необходимые (`tools/approval.py:1059-1073`),
   и до yolo есть безусловные блоки (`tools/approval.py:1070`).

## Решение

Один лестничный enum из четырёх значений; `yolo` — верх ступени и означает «самый
мягкий режим правок плюс обход подтверждений команд». Две ортогональные настройки
(`mode` и `yolo: bool`) отвергнуты: они ломают единый `/mode` темы (там одно строковое
поле) и расходятся с формой настроек остальных агентов.

| Значение настройки | argv до `acp` | `session/set_mode` | Смысл |
|---|---|---|---|
| `default` | — | `default` | Спрашивает всё; поведение хаба до изменения |
| `accept_edits` | — | `accept_edits` | Правки в workspace и temp без вопросов |
| `dont_ask` | — | `dont_ask` | Правки без вопросов всю сессию |
| `yolo` | `--yolo` | `dont_ask` | Плюс команды без вопросов |

Проекций у enum две, и они намеренно не совпадают: `wire()` — имя настройки (`yolo`),
`mode_id()` — идентификатор ACP (`dont_ask`). `launch_options()` возвращает глобальные
опции процесса. Строка из настройки в argv не попадает: в процесс уходит только
фиксированный `--yolo`, поэтому инъекция аргументов через эту настройку невозможна.

Разделение ответственности:

- `hub-core::settings::HermesApproval` — значение, `ALL`, `wire()`, `parse()`, `mode_id()`,
  `launch_options()`; total-enum, поэтому `HermesDraft` хранит его напрямую и проверка
  формы не нужна (текст не пройдёт `choice()` при чтении `settings.toml`).
- `hub-core::options::Mode::Hermes` — тот же режим как override темы: `Mode::all`,
  `wire`, `bypasses_approval` (только `yolo`), `resolve`, `apply`. Это включает Hermes в
  `/mode` без отдельного кода в `hub-telegram`.
- `hub-hermes::protocol::advertised_modes` — разбор `modes.availableModes[].id`.
- `hub-hermes::backend` — `--yolo` при запуске и `session/set_mode` с `mode_id()`;
  необъявленный режим отклоняется до `set_mode` с явной ошибкой вместо `-32603`.
- `hub-app::gui::settings` — выпадающий список и предупреждение для `yolo`.

## Поведение и ограничения

- Режим применяется при каждом запуске сессии, включая восстановленную по `session/load`:
  старый инвариант «перевыставить режим явно» сохранён, изменилось только значение.
  Прерванный `set_mode` валит разговор целиком — частично применённого режима нет.
- Смена режима не требует новой сессии (в отличие от Qwen): хаб поднимает новый процесс
  `hermes acp` на каждый разговор и сам перевыставляет `set_mode`.
- Если Hermes не объявляет режимы вообще (пустой или отсутствующий список), хаб не
  чинит запрет вслепую и продолжает вызывать `set_mode` — решение остаётся за Hermes.
- Подтверждения, которые всё равно приходят при любом режиме, обрабатывает прежний
  `Gate`: хаб выбирает только разовое разрешение, `allow_always` не выбирается,
  ожидание ограничено 5 минутами и заканчивается разовым отказом.
- `MIN_VERSION` остаётся `0.21.5`; общая таблица режимов `.py` появилась в 0.22.3 (первый
  содержащий её тег — `rc.1-v0.22.3`). Вместо raise пина — проверка объявленных режимов,
  которая честно объясняет оператору, что его Hermes режима не поддерживает.
- Совместимость файла настроек: новое поле `[hermes] approval` читается с дефолтом, если
  его нет; старый бинарник хаба при наличии поля откажется загружать файл
  (`deny_unknown_fields`) — то же ограничение уже есть у `[codex]` и `[qwen]`.
- `--accept-hooks` (автоприёмление незнакомых shell-хуков без TTY) вне объёма:
  это отдельная политика Hermes, не ступень одобрений.

## Проверка и критерии приёмки

- `hub-core::settings`: секция `[hermes]` разбирается с `approval`, дефолт — `default`,
  round trip черновик ↔ файл, неизвестное значение в файле указывает на
  `Field::Hermes(HermesField::Approval)`, `deny_unknown_fields` по-прежнему отклоняет
  чужие поля, имена `wire()` ровно `default`/`accept_edits`/`dont_ask`/`yolo`.
- `hub-core::options`: перебор `Mode::all` по всем бэкендам (Hermes — 4 значения),
  `parse(Hermes, "plan")` ошибается, `bypasses_approval` истинна только для `yolo`,
  режим одного бэкенда не принимается другим даже при совпадении имени (`default` у
  Claude и Hermes).
- `hub-hermes` (детерминированный фикстурный ACP): argv `["--yolo", "--profile",
  "test-profile", "acp"]` только при `yolo`; `modeId` соответствует `mode_id()`;
  необъявленный режим прекращает разговор до `set_mode` и до `session/prompt`; Hermes
  без объявленных режимов получает `set_mode` как раньше.
- `hub-app`: round trip `settings.toml` с `yolo` не добавляет секретов; форма агента
  Hermes показывает поле «Одобрения».
- Полные гейты: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  --locked -- -D warnings`, `cargo test --workspace --locked`,
  `cargo build -p hub-app --locked`, `cargo deny check`, `git diff --check`.
