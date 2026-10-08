# Бэкенд Qwen

## Цель

Qwen Code (`qwen`, `@qwen-code/qwen-code`) — третий агент рядом с Claude и Codex, выбираемый на уровне темы. Поведение — как у Codex: текст, вызовы инструментов, одобрения кнопками, `send_file`, вопросы пользователю, фото на вход, досылка сообщений посреди хода, токены в итоге хода. Выходит в релизе 0.3.0.

## Решения

| Вопрос | Решение |
|---|---|
| Интеграция | ACP: `qwen --acp`, JSON-RPC 2.0 построчно по stdio. Режим stream-json отвергнут: в документации Qwen он «under construction» и не умеет досылку посреди хода |
| Бинарник | Путь из настроек или `qwen` в `PATH`; `qwen --version` не ниже `0.25.0` (на ней протокол проверен по исходникам) |
| Авторизация | Основной путь — собственная настройка qwen (`/auth`, `~/.qwen/settings.json`, переменные окружения). Необязательно в GUI: OpenAI-совместимый `base_url` + API-ключ (ключ в системном хранилище ключей) |
| Права | Режим одобрений `plan`/`default`/`auto-edit`/`yolo`, по умолчанию `default`; `auto` не предлагается |
| `send_file` | MCP-сервер хаба по HTTP на `127.0.0.1`, на сессию |
| Вопросы | Встроенный в Qwen `ask_user_question`; свой `ask_user` не передаётся |
| Выбор агента | `/new qwen <path>`, бэкенд по умолчанию в настройках, `/backend qwen` |
| Крейты | Новый `hub-qwen`; JSON-RPC клиент переезжает из `hub-codex` в `hub-agent`; `hub-qwen` не зависит от `hub-claude` и `hub-codex` |

## Протокол (qwen-code 0.25.0, ACP SDK `^0.14.1`)

- Рукопожатие: `initialize {protocolVersion, clientCapabilities}` → `{agentInfo, authMethods, agentCapabilities}`.
- `session/new {cwd, mcpServers}` → `{sessionId, ...}`; нет входа → ошибка `authRequired` (-32000).
- `session/load {sessionId, cwd, mcpServers}` — история повторяется уведомлениями `session/update` до ответа.
- `session/prompt {sessionId, prompt: ContentBlock[]}` → `{stopReason}` по окончании хода (`end_turn`, `cancelled`, `max_tokens`, ...). Второй `session/prompt` во время хода прерывает текущий — хаб его не шлёт.
- `session/cancel {sessionId}` — уведомление.
- Ввод: `{type: "image", mimeType, data: <base64>}` на каждое фото, затем `{type: "text", text}`, если текст не пустой.
- Уведомления `session/update` по `sessionUpdate`: `agent_message_chunk`, `agent_thought_chunk`, `tool_call`, `tool_call_update`, `plan`, `usage_update`, `current_mode_update`, `available_commands_update`, `session_info_update`; токены — `_meta.usage.totalTokens` пустого `agent_message_chunk`.
- Запросы агента к клиенту: `session/request_permission {sessionId, toolCall, options}` → `{outcome: {outcome: "selected", optionId} | {outcome: "cancelled"}}`; для `ask_user_question` — `toolCall._meta.qwenInteractionKind = "user_question"`, вопросы в `toolCall._meta.qwenQuestions`, ответы — поле `answers` в корне ответа, `{"0": "метка", ...}` (ключ — индекс вопроса). `craft/drainMidTurnQueue {sessionId, promptId?}` → `{items: [{content: ContentBlock[]}]}`; ответ нужен за 2 с, иначе Qwen ждёт и после трёх тайм-аутов отключает досылку.

## Архитектура

### `hub-agent`

- `rpc` — `RpcClient`, `connect`, `Notification`, `Handler`, `RpcError`, `RequestError`, `METHOD_NOT_FOUND`, `INVALID_PARAMS` переносятся из `hub-codex::rpc` без изменения поведения; имя агента в текстах ошибок — параметр `connect`. `hub-codex` импортирует их отсюда.
- `mcp_http` — `McpHttpServer::start(channel) -> McpHttpServer { url, token }`, завершается при drop:
  - слушает `127.0.0.1:0` (hyper 1, `hyper-util`);
  - токен — 128 бит из `uuid::Uuid::new_v4`, на сессию; запрос без `Authorization: Bearer <token>` (сравнение за постоянное время) → 401;
  - только `POST /mcp`, тело — JSON-RPC MCP, ответ `application/json`; иное → 404/405;
  - `initialize`, `notifications/*` (202 без тела), `tools/list` → `send_file`, `tools/call send_file` → `deliver_file`; прочее → `-32601`;
  - тело запроса не больше 1 МиБ.

### `hub-qwen`

| Модуль | Роль |
|---|---|
| `version` | `locate(configured) -> Result<PathBuf, CliError>`, `check(cli) -> Result<Version, CliError>`, `MIN_VERSION = 0.25.0` |
| `protocol` | Чистые функции: параметры `initialize`/`session/new`/`session/load`/`session/prompt`/`session/cancel`, `parse_session_id`, `parse_stop_reason`, `prompt_blocks`, `TurnTracker::translate(&Notification) -> Vec<AgentEvent>` и `finish(StopReason) -> AgentEvent` |
| `requests` | `answer(channel, inbox, method, params) -> Result<Value, RequestError>`: одобрение, вопрос, drain |
| `session` | `converse(session, tracker, prompt, inbox, conversation) -> inbox` |
| `backend` | `QwenBackend::run(session, prompt, inbox, conversation) -> inbox` (сигнатура как у `CodexBackend::run`) |

Типы: id сессии — `SessionId`; `StopReason { EndTurn, Cancelled, Other(String) }`; `QwenApproval { Plan, Default, AutoEdit, Yolo }`.

### Процесс

- `qwen --acp --approval-mode <режим>` и `-m <модель>`, если задана; `--auth-type openai`, если в хабе задан `endpoint`.
- Окружение: `QWEN_CODE_NO_RELAUNCH=true` (иначе qwen перезапускает себя дочерним Node-процессом, который переживает родителя до 120 с). Если задан `endpoint` — `OPENAI_API_KEY`, `OPENAI_BASE_URL` и, если задана модель, `OPENAI_MODEL` только дочернему процессу; без `endpoint` окружение не меняется.
- `hide_window`, `kill_on_drop`, длина строки stdout не ограничивается сверх 64 МиБ, stderr построчно в лог `info`.
- Остановка: закрыть stdin, ждать выхода до 10 с, затем `kill`.

### Сессия

1. Поднять `McpHttpServer`, запустить `qwen`, `initialize`.
2. Нет сессии → `session/new {cwd, mcpServers: [{type: "http", name: "agent-hub", url, headers: [{name: "Authorization", value: "Bearer <token>"}]}]}`; есть → `session/load` с теми же `mcpServers`, уведомления до ответа отбрасываются. → `SessionStarted(sessionId)`.
3. `authRequired` → `Failed("Qwen не авторизован: настройте qwen (/auth) или задайте API-ключ в настройках")`. Ошибка `session/load` → `Failed` с подсказкой `/reset`; новая сессия сама не создаётся.
4. Ход — `session/prompt`. `craft/drainMidTurnQueue` → немедленно всё, что есть в inbox (`try_recv`), пусто → `{items: []}`. Промпты, пришедшие после последнего drain, — следующий ход после окончания текущего.
5. Сессия закрывается, когда нет активного хода и очередь пуста; промпт, уже взятый из inbox, отправляется всегда. `cancel` → `session/cancel`, выход из цикла, inbox возвращается.
6. Ошибки: `RpcError::Remote` → `Failed("Qwen: <message>")`; `Closed`/`Protocol`/`Timeout`/ошибка запуска → `Failed("Qwen Code завершился, подробности в логе")` и запись в лог. Тайм-аут 60 с — только на управляющие вызовы; длительность хода не ограничена.

### События

| Qwen | Хаб |
|---|---|
| ответ `session/new`/`session/load` (`load` id не возвращает — берётся сохранённый) | `SessionStarted(sessionId)` |
| `agent_message_chunk` с текстом | копится; `AssistantText` перед следующим `tool_call` и в конце хода, если не пусто |
| `tool_call` | `ToolCall(kind ∨ "tool", title)`; с `_meta.phase = "preparing"` откладывается до первого `tool_call_update` того же id с непустым `title` |
| `_meta.usage.totalTokens` (итог раунда модели, `0` — нет данных) | запоминается последнее ненулевое |
| чанки с `_meta.parentToolCallId` (субагенты) | ничего |
| `stopReason: end_turn` | `Finished { usage: Qwen { tokens } }` |
| `stopReason: cancelled` | `Failed("Ход Qwen прерван")` |
| другой `stopReason` | `Failed("Ход Qwen остановлен: <reason>")` |
| прочие уведомления (`agent_thought_chunk`, `plan`, `tool_call_update`, ...) | ничего |

Сводки инструментов обрезаются до `TOOL_SUMMARY_LIMIT`.

### Запросы агента

| Запрос | Обработка |
|---|---|
| `session/request_permission` | `ToolRequest(kind ∨ "tool", title)` → разрешено: `proceed_once`, отклонено: `cancel`; «разрешить всегда» не предлагается |
| то же с `toolCall._meta.qwenInteractionKind = user_question` | `qwenQuestions` → `channel.ask` → `{outcome: selected proceed_once, answers}`; отказ → `cancel`; неверная форма → `-32602` |
| `craft/drainMidTurnQueue` | см. «Сессия», п. 4 |
| другое | `-32601` |

Не проверено по исходникам до конца и сверяется в первой задаче плана (снимки протокола), окончательно — живым тестом: что поле `answers` проходит через ACP SDK, ключи ответов (индексы вопросов), просит ли Qwen одобрение на вызов `agent-hub/send_file` (если да — обычная кнопка; автоодобрения по совпадению строк нет).

### Общий слой (`hub-core`)

- `BackendKind::Qwen` (`qwen`), `ALL = [Claude, Codex, Qwen]`.
- `Usage::Qwen { tokens: Option<u64> }`; `format_finished` — «✅ Готово · токенов в сессии: 12 345», если токены известны.
- Справка: Qwen, `/new [claude|codex|qwen] [path]`, `/backend claude|codex|qwen`.
- Настройки:
  - `Settings.qwen: QwenSettings { cli: Option<PathBuf>, model: Option<String>, approval: QwenApproval, endpoint: Option<ApiEndpoint> }`, `ApiEndpoint { base_url, key: ApiKey }` — ключ и URL задаются только вместе.
  - `SettingsFile`: `[qwen] cli, model, approval, base_url` — все необязательные, старый `settings.toml` читается без изменений; ключ в файл не пишется.
  - `Draft`/`Field`: новые поля; ключ без URL или URL без ключа — ошибка поля.

### `hub-telegram`

- `HubAgents`: ветка `BackendKind::Qwen` находит `qwen` (`locate`) и запускает `QwenBackend`; ошибка поиска → `Failed`.
- Тексты справки.

### `hub-app`

- `Secret::QwenKey` (аккаунт `qwen-api-key`).
- `connector`: обязателен CLI бэкенда по умолчанию; Qwen не по умолчанию проверяется мягко (поиск и версия). Вход заранее не проверяется: `authRequired` приходит только на `session/new`, а пробная сессия оставила бы файл в `~/.qwen`.
- GUI «Настройки»: бэкенд по умолчанию включает Qwen; секция «Qwen» — путь, модель, режим одобрений списком, base URL, API-ключ полем-паролем.
- GUI «Статус»: «Qwen Code 0.25.0 · ключ из настроек хаба» или «· собственная настройка qwen», либо причина недоступности.

## Тесты

- `hub-agent::rpc`: тесты переезжают из `hub-codex`; тесты `hub-codex` проходят без изменений поведения.
- `hub-agent::mcp_http` на реальном `127.0.0.1`: без токена и с чужим токеном → 401, `initialize`, `tools/list`, `tools/call send_file` через поддельный `UserChannel`, не-POST и неизвестный путь, слишком большое тело.
- `hub-qwen`:
  - `protocol`: параметры вызовов — insta-снимки; `translate`, разбор `request_permission` и вопросов — rstest-таблицы.
  - `requests`: одобрение, отказ, вопрос, отказ от вопроса, drain с промптами и пустой.
  - `session`: досылка через drain, промпт после `end_turn`, отмена, повтор истории при `load` отбрасывается.
  - `backend`: поддельный ACP-агент на `duplex` — новая сессия, `load`, `authRequired`, ошибка `load`, ошибки посреди хода.
  - `tests/live.rs`: живой прогон, `#[ignore]`, `AGENT_HUB_LIVE_QWEN=1`.
- `hub-core`: `[qwen]` в `SettingsFile`/`Draft`, пара ключ+URL, `Usage::Qwen`/`format_finished`, `parse_new_args`/`decide_backend` с `qwen`, чтение старого `settings.toml`.
- `hub-app`: мягкая проверка Qwen в `connector`; ключ Qwen через `MemorySecrets`.
- Последняя задача — ручная проверка на Windows и Linux с реальным ключом.

## Релиз 0.3.0

- Версия workspace `0.3.0`.
- README: требования (Qwen Code ≥ 0.25, Node ≥ 22, собственный ключ — бесплатного Qwen OAuth больше нет), раздел о Qwen и его настройках.
- Коммит версии и тег `v0.3.0`; push тега запускает `release.yml` — только после отдельного подтверждения.

## Вне объёма

Режим `auto`; переключение режима одобрений из Telegram; показ `plan`; «разрешить всегда»; перенос контекста при `/backend`; вход в Qwen из GUI или Telegram, кроме ключа; автоустановка `qwen` и Node; общий ACP-крейт для других агентов.
