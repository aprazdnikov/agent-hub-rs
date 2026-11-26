# Бэкенд Pi — спека

Дата: 2026-11-26. Статус: принята. Ветка: `aprazdnikov/Pi-and-CLI`.

## Зачем

`Pi` (`@earendil-works/pi-coding-agent`) — лёгкий агент с собственной подписочной
моделью доступа: OAuth-подписка OpenAI Codex (`openai-codex`) и подписка Qwen
Token Plan (`qwen-token-plan`). Хабу он интересен как пятый исполняемый агент:
Node-скрипт без собственного UI-слоя, который умеет полностью headless-сессию.

Задача — добавить `Pi` как `BackendKind::Pi` с выбором режима так, чтобы Telegram
и GUI вели себя как с остальными агентами: стриминг инструментов, ответы на
вопросы, `send_file`, продолжение сессии, снимки.

## Контракт: чем проверено

Контракт сверялся с **установленной сборкой Pi 1.6.0**
(`~/.pi/agent/install/releases/1.6.0`), а не только с документацией.
`MIN_VERSION = 1.6.0`. Живые доказательства, снятые 2026-11-26:

| Факт | Как получено |
| --- | --- |
| `pi --version` печатает `1.6.0`, rc=0 | запуск CLI |
| `pi --mode rpc` — JSON-строки в обе стороны, 0 записей в stderr при чистом прогоне | прогон `probe.py`, дамп `wire.jsonl` |
| Формы ответов/событий | `dist/modes/rpc/rpc-types.d.ts` (477 строк), сверено с живым дампом |
| `prompt` → `{"disposition":"started"}`, финал — `agent_settled` | живой прогон с инструментным вызовом |
| MCP-инструменты доходят до модели | фикстура-MCP сервер на 127.0.0.1: `initialize` → `tools/list` с bearer-токеном, вызов `mcp__agent_hub__agent_hub_ask_user` |
| Подтверждения из расширения всплывают в поток | `pi.on("tool_call")` + `ctx.ui.confirm` → `extension_ui_request`, ответ `extension_ui_response{confirmed:true}` разблокировал `bash` |
| Уровни thinking объявляются моделью | `get_available_thinking_levels` → `["off","minimal","low","medium","high"]` для `qwen3.7-max`; у части моделей `thinkingLevelMap` с `null` |
| `pi auth check --provider <p> --json` → `{"status":"ready"|"not_ready"}` | живой запуск, rc=0/rc=1 |
| Подписка Qwen живёт на региональном эндпоинте | `token-plan.ap-southeast-1.maas.aliyuncs.com` → 200; `token-plan.maas.qwencloudapi.com` → 401 тем же ключом |

## Транспорт

Pi не говорит по ACP: собственный JSONL-RPC поверх stdio. Команды — плоские
объекты `{"id"?, "type", ...}`, ответы — `{"type":"response", "command",
"success", "data"|"error"}`, события — плоские же объекты без `type:"response"`.
`hub-agent::rpc` (JSON-RPC 2.0, `method`/`params`/`result`) не переиспользуется:
форма другая. Свой тонкий слой в `hub-pi::protocol` поверх тех же примитивов
(`LinesCodec`, `Envelope::Bare` не подходит).

Идентификаторы команд — строки (`cmd-1`), корреляция ответа по `id`; события
коррелируются по порядку и по `sessionId`.

### Команды, которые хаб реально шлёт

| Команда | Когда |
| --- | --- |
| `get_state` | сразу после старта: readiness, `sessionId`, текущая модель |
| `get_available_models` | только в тестах/живой проверке выбора модели |
| `set_model` | когда тема выбрала модель (`provider` + `modelId` отдельно) |
| `get_available_thinking_levels` | перед `set_thinking_level`, чтобы не гадать |
| `set_thinking_level` | применяет режим темы |
| `prompt` | ход; `streamingBehavior` не используется, очереди нет |
| `steer` | сообщение, пришедшее во время хода |
| `abort` | `/stop` |
| `get_session_stats` | токены и стоимость для `Finished` |
| `export_html` | снимок сессии |

Остальные 24 команды контракта (`fork`, `clone`, `bash`, `compact`, `get_tree` и
прочее) в v1 не используются.

## Процесс

Запуск: `pi --mode rpc --session-id <uuid> [--model <pattern>] [--thinking <level>]
[--extension <path>]`, `cwd` = каталог проекта, stdin/stdout/stderr пайпятся,
`kill_on_drop`. Флаги — только константные строки, перечисленные разбором
значений; пользовательские строки (модель, путь) приходят из настроек хаба и
проходят проверку `Options::parse`.

`--no-session` **не** используется: сессии Pi живут в
`~/.pi/agent/sessions/<каталог-проекта>/<время>_<uuid>.jsonl` и нужны для
продолжения и снимков. `sessionId` берётся из `get_state` и сохраняется в
`TopicSession`; повторный запуск того же диалога идёт с тем же `--session-id`.

Сессия Pi привязана к каталогу (`--session-id` ищет сессию «текущего
каталога»), поэтому продолжение работает только в том же `cwd`. Перенос между
каталогами — через снимок (`export_html` + `--session <путь>`), не через id.

Готовность: `get_state` ответил. Таймаут инициализации — общий `Limits::initialize`.
stderr читается и логируется построчно без текста (только длина строки), как в
остальных бэкендах: в выводе агента могут быть секреты.

## Карта событий → `AgentEvent`

| Поток Pi | Событие хаба |
| --- | --- |
| `message_update` с `assistantMessageEvent.type = text_delta` | копить текст |
| `text_end` | (копить не нужно: дельты уже собраны) |
| `thinking_*` | игнор: рассуждения в ленту не попадают |
| `tool_execution_start` | `AssistantText` сбросить, затем `ToolCall{tool, summary}` |
| `tool_execution_update` | игнор |
| `tool_execution_end` | игнор: итог виден в следующем тексте |
| `turn_end` | игнор (их несколько на ход) |
| `agent_settled` | `Finished{session, usage}` после `get_session_stats` |
| `response.success=false` | `Failed` с текстом ошибки |
| EOF / ноль событий | `Failed` |

`ToolUse.summary` — заголовок инструмента и аргументы, усечённые
`TOOL_SUMMARY_LIMIT`. Токены берутся из `get_session_stats.tokens.total`,
стоимость — из `cost` (`Usage::Pi { tokens }`).

## Режим (`/mode`)

Pi не спрашивает разрешения сам: инструменты выполняются молча. Зато у него есть
нативная ось, которую объявляет сама модель — **уровень мышления**. `Mode::Pi`
принимает ladder `off | minimal | low | medium | high | xhigh | max` (7 значений
контракта `ThinkingLevel`), проецируется в `--thinking` при запуске и в
`set_thinking_level` для живой сессии.

Значение сверяется с `get_available_thinking_levels` **до** отправки хода: у
части моделей уровень не объявлен (например `thinkingLevelMap: {"medium": null}`),
и Pi на неизвестный уровень отвечает молча. Незаявленный режим — ошибка с
перечислением доступных, а не тихий fallback.

`Mode::bypasses_approval` для Pi — `false` у всех значений: режим мышления не
снимает подтверждений, они у Pi появляются только из хаба (см. ниже), и их
нельзя отключить выбором режима.

## Инструменты хаба и подтверждения

Раздаются расширением Pi, которое хаб пишет в свой каталог данных и подкладывает
через `--extension <path>`:

- `pi.registerMcpServer("agent-hub", { url, headers: {Authorization: Bearer …},
  exposure: "direct" })` — сервер `send_file`/`ask_user` живёт в текущей папке
  проекта и доступен модели под именами `mcp__agent_hub__*`. `url` и токен
  читаются из переменных окружения дочернего процесса, **не** из argv: путь к
  файлу расширения стабилен, а токен меняется каждую сессию.
- `pi.on("tool_call")` + `ctx.ui.confirm` — шлюз подтверждений: опасные команды
  всплывают как `extension_ui_request{method:"confirm"}`, хаб отвечает
  `extension_ui_response{confirmed}` по решению пользователя из Telegram.

`exposure: "direct"` обязателен: по умолчанию (`codemode`) инструменты сервера
не видны модели как отдельные вызовы, и агент их не находит.

Подписки (OAuth Codex, ключ Token Plan) остаются в `~/.pi/agent/auth.json` —
хаб их не читает, не копирует и не пишет.

## Ограничения v1

- Снимки не поддерживаются, как у OpenCode: `archive()` проверяет CLI и версию,
  затем сообщает, что история остаётся у агента. Сессии Pi живёт в её
  собственных JSONL-файлах и открывается по `id`, который хаб запоминает из
  `get_state`; перенос между машинами не проверялся (`export_html` есть,
  импорта нет).
- Квоты подписок Pi не сообщает ни в RPC, ни в `auth check` —
  `QuotaReport::not_reported`, как у Qwen/Hermes/OpenCode.
- Изображения передаются (`prompt.images` — base64 и `mimeType`), форма взята
  из `rpc-types.d.ts`; живой прогон картинки — отдельная проверка, без неё
  модель отвечает на текст.
- Steering работает (`steer`), но очередь Pi (`one-at-a-time`) не совпадает с
  хабовой: сообщение, пришедшее во время хода, уходит как `steer`, остальное
  возвращается в `inbox`.

## Консольный режим (`--cli`)

Тот же разговор без окна: `crates/hub-app/src/cli.rs`, ключ `--cli` (или
`--tui`) до остальных аргументов. Режим нужен машине без дисплея — Raspberry Pi,
сервер, SSH.

- Настройки, ключи и бэкенды берутся из тех же `settings.toml` и связки ключей;
  `--backend` и `--dir` переопределяют их на один запуск. Окно не поднимается:
  `eframe`, tray и бот Telegram в этом пути не участвуют.
- Берётся та же блокировка экземпляра: второй запуск сообщает, что хаб уже
  работает, вместо борьбы за одни настройки.
- `Input` — единственный источник строк: одна очередь кормит и команды цикла, и
  канал `UserChannel`, поэтому согласование агент получает в том же терминале.
  Очередь проверяется раньше ожидания, так что отменённое ожидание не теряет
  строку.
- Ошибка исполнителя приходит не текстом, а полем `errorMessage` в `message_end`
  (проверено живьём на недоступной модели): `Wire::Error` поднимает её как
  `AgentEvent::Failed`, иначе ход выглядит как «Готово · 0 токенов».
- Открытый диалог владеет вводом: счётчик диалогов в очереди stdin не даёт циклу
  REPL перехватить ответ согласования (регрессия `an_open_dialog_takes_the_line_ahead_of_the_repl`).
- Строка во время хода — уточнение (`steer`), `/stop` — `abort`; команды режима и
  модели применяются между ходами через тот же `Effective`, что и тема в
  Telegram, поэтому `/mode high` для Pi означает уровень размышления.
- `send_file` печатает путь файла: в консоли нет чата, куда его приложить.
- Не делает: не просит недостающие настройки и токен бота (это делает окно), не
  принимает вложения. Руководство — `docs/guide/console.md`.

## Гейтры

`cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked --
-D warnings`, `cargo test --workspace --locked`, `cargo build -p hub-app
--locked`, `cargo deny check`, `git diff --check`. Живой прогон Pi — опциональные
тесты `crates/hub-pi/tests/live.rs` (`AGENT_HUB_LIVE_PI=1`, модель
`AGENT_HUB_LIVE_PI_MODEL`, по умолчанию бесплатная подписка Qwen) — тратят
реальные токены и потому под `--ignored`.
