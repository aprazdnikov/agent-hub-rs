# Бэкенд Codex

## Цель

OpenAI Codex — второй агент рядом с Claude, выбираемый на уровне темы. Поведение — как в Python-версии agent-hub (коммит `733bf58`, спецификация `agent-hub/docs/superpowers/specs/2026-10-04-codex-backend-design.md`): текст, вызовы инструментов, одобрения кнопками, `send_file`, `ask_user`, фото на вход, досылка сообщений посреди хода (`turn/steer`), вопросы `requestUserInput`, токены в итоге хода.

## Решения

| Вопрос | Решение |
|---|---|
| Паритет | Полный, кроме фоновых задач (у Codex их нет) |
| Интеграция | Собственный JSON-RPC клиент к `codex app-server` (stdio, построчный JSON) |
| Бинарник | Как у Claude: путь из настроек или `codex` в `PATH`; `codex --version` не ниже `0.160.0` (на ней проверен протокол) |
| Авторизация | Пользователь входит сам (`codex login`). Необязательный API-ключ OpenAI в настройках GUI хранится в системном хранилище ключей и передаётся через `account/login/start`, только если входа нет или это уже вход по ключу; вход по подписке ChatGPT не трогается |
| Права | Песочница (`read-only`/`workspace-write`/`danger-full-access`, по умолчанию `workspace-write`) и одобрения (`untrusted`/`on-request`/`never`, по умолчанию `on-request`) выбираются в GUI; `approvalsReviewer: user` всегда |
| Выбор агента | `/new codex <path>`, бэкенд по умолчанию в настройках, `/backend claude\|codex` в существующей теме (сбрасывает сессию) |
| Крейты | Новый `hub-agent` (общее для бэкендов) и новый `hub-codex`; `hub-claude` и `hub-codex` не зависят друг от друга |

## Протокол (codex-cli 0.160.0)

- Рукопожатие: `initialize {clientInfo, capabilities: {experimentalApi: true}}`, затем уведомление `initialized`.
- `account/read` → `{account: null | {type: "apiKey"|"chatgpt"|...}, requiresOpenaiAuth}`.
- `account/login/start {type: "apiKey", apiKey}` — без сети, пишет `auth.json` в `$CODEX_HOME`.
- `thread/start {cwd, sandbox, approvalPolicy, approvalsReviewer, model?, dynamicTools}` → `{thread: {id}}`; `thread/resume {threadId, cwd, sandbox, approvalPolicy, approvalsReviewer, model?}` — dynamic tools восстанавливаются из сохранённого треда.
- `turn/start {threadId, input}` → `{turn: {id}}`; `turn/steer {threadId, expectedTurnId, input}`.
- Ввод: `{type: "image", url: "data:<mime>;base64,..."}` для каждого фото, затем `{type: "text", text}`, если текст не пустой.
- Уведомления: `item/started`, `item/completed`, `thread/tokenUsage/updated` (`tokenUsage.total.totalTokens`), `turn/completed` (`turn.status`: `completed`/`interrupted`/`failed`, `turn.error.message`).
- Запросы сервера: `item/commandExecution/requestApproval` (`command`, `reason`), `item/fileChange/requestApproval` (`reason`, `grantRoot`) → `{decision: "accept"|"decline"}`; `item/tool/call` (`tool`, `arguments`) → `{success, contentItems: [{type: "inputText", text}]}`; `item/tool/requestUserInput` (`questions[{id, header, question, options?}]`) → `{answers: {id: {answers: [..]}}}`. Прочие → ошибка `-32601`.

## Архитектура

### `hub-agent` (новый крейт)

Общее для бэкендов, переносится из `hub-claude` без изменения поведения:

- `channel::UserChannel` — одобрения, вопросы, файлы.
- `conversation::{Conversation, Limits}` — канал событий, отмена, лимиты.
- `tools` — имена `send_file`/`ask_user`, описания и JSON-схемы, `ToolResult { Success(String), Error(String) }`, `parse_send_file`, `deliver_file(args, channel)`, `parse_questions`, `parse_option`, `TOOL_SUMMARY_LIMIT`.
- `cli` — `Version`, `locate(name, configured)`, `version_output(cli)` (`--version` с тайм-аутом 10 с), `hide_window`; `hub-claude::version` и `hub-codex::version` — тонкие обёртки со своими сообщениями.

`hub-claude` и `hub-telegram` импортируют эти типы из `hub-agent`.

### `hub-codex` (новый крейт)

| Модуль | Роль |
|---|---|
| `rpc` | `RpcConnection`: JSON-RPC 2.0 построчно. `request(method, params)` ждёт ответ по id (`oneshot`); уведомления — в `mpsc`; каждый запрос сервера — в своей задаче `JoinSet`, ожидание кнопки не блокирует чтение. После EOF ожидающие и новые вызовы получают `RpcError::Closed`. Ответ на запрос сервера: `Unsupported` → `-32601`, `Malformed` → `-32602`. |
| `protocol` | Чистые функции: параметры всех вызовов, `parse_thread_id`, `parse_turn_id`, `auth_state`, `user_input`, `tool_call`, `agent_text`, `TurnTracker::translate(&Notification) -> Translation { events, completed: Option<TurnId> }`. |
| `requests` | `answer(channel, method, params) -> Result<Value, RequestError>` для четырёх запросов сервера. |
| `session` | `converse(thread, tracker, prompt, inbox, conversation) -> inbox`. |
| `backend` | `CodexBackend::run(session, prompt, inbox, conversation) -> inbox` (сигнатура как у `ClaudeBackend::run`), `probe_auth(cli, settings) -> Result<CodexAuth, ProbeError>`. |
| `version` | `locate(configured) -> Result<PathBuf, CliError>`, `check(cli) -> Result<Version, CliError>`, `MIN_VERSION = 0.160.0`. |

Типы: id треда — `SessionId` (он же хранится в теме); `TurnId` — newtype над `String`; `CodexAuth { ApiKey, ChatGpt, Other, NotRequired, Missing }`; `RpcError { Remote { code, message }, Closed, Protocol(String), Timeout }` (`thiserror`).

### Сессия

1. `CodexBackend::run` запускает `codex app-server` (`hide_window`, `kill_on_drop`, лимит строки stdout 64 МиБ, stderr построчно в лог `info`, из окружения убран `OPENAI_API_KEY`).
2. Рукопожатие, `account/read`. Нет входа и есть ключ → `account/login/start`, повторный `account/read`. Входа нет → `Failed("Codex не авторизован: выполните `codex login` или задайте API-ключ OpenAI в настройках")`.
3. `thread/start` (нет сессии) или `thread/resume` → `SessionStarted(thread_id)`. Ошибка `thread/resume` → `Failed` с подсказкой `/reset`; новый тред сам не создаётся.
4. `converse`: первый промпт → `turn/start`. Промпт из inbox во время хода → `turn/steer`; отклонённый steer оставляет его и все следующие в очереди до `turn/completed` активного хода, затем — новый ход. Ход завершён только по `turn/completed` с id активного хода. Сессия закрывается, когда нет активного хода и очередь пуста; промпт, уже взятый из inbox, отправляется всегда. `cancel` → выход из цикла, inbox возвращается.
5. Ошибки: `RpcError::Remote` → `Failed("Codex: <message>")`; `Closed`/`Protocol`/`Timeout`/ошибка запуска → `Failed("Codex app-server завершился, подробности в логе")` и запись в лог. Тайм-аут 60 с — только на управляющие вызовы; длительность хода не ограничена.
6. Остановка: закрыть stdin, ждать выхода до 5 с, затем `kill`.

### События

| Codex | Хаб |
|---|---|
| ответ `thread/start`/`thread/resume` | `SessionStarted(thread_id)` |
| `item/completed` `agentMessage` с непустым текстом | `AssistantText` |
| `item/started` `commandExecution` | `ToolCall("shell", command)` |
| `item/started` `fileChange` | `ToolCall("patch", пути через запятую ∨ «изменение файлов»)` |
| `item/started` `mcpToolCall` | `ToolCall("<server>/<tool>", аргументы JSON ∨ `{}`)` |
| `item/started` `webSearch` | `ToolCall("web_search", query)` |
| `thread/tokenUsage/updated` | запоминается `total.totalTokens` |
| `turn/completed` `completed` | `Finished { usage: Codex { tokens } }` |
| `turn/completed` `interrupted` | `Failed("Ход Codex прерван")` |
| `turn/completed` с `error.message` | `Failed(message)` |
| `turn/completed` иначе | `Failed("Ход Codex завершился ошибкой")` |
| прочие уведомления | ничего |

Сводки инструментов обрезаются до `TOOL_SUMMARY_LIMIT`.

### Запросы сервера

| Запрос | Обработка |
|---|---|
| одобрение команды | `ToolRequest("shell", command ∨ reason ∨ JSON params)` → `accept`/`decline` |
| одобрение правки | `ToolRequest("patch", reason ∨ «запись в <grantRoot>» ∨ «изменение файлов»)` |
| `item/tool/call` `send_file` | `deliver_file` |
| `item/tool/call` `ask_user` | `parse_questions` → `channel.ask` → строки `вопрос: ответ`; неверная форма → `success: false` с описанием формы |
| `item/tool/call` другой инструмент | `success: false` |
| `item/tool/requestUserInput` | вопросы → `channel.ask`; отказ → `{answers: {}}`; неверная форма → `-32602` |
| другое | `-32601` |

### Общий слой (`hub-core`)

- `BackendKind::Codex`, `ALL = [Claude, Codex]`.
- `Finished { session, usage: Usage, background: usize }`, `enum Usage { Claude { turns: u32, cost: Option<Decimal> }, Codex { tokens: Option<u64> } }`.
- `format_finished(&Finished)`: Claude — «✅ Готово · ходов: N · $X»; Codex — «✅ Готово» и «· токенов в сессии: 12 345», если токены известны; ожидающие фоновые задачи — как сейчас.
- `parse_new_args(args, default: BackendKind)`; константа `DEFAULT_BACKEND` удаляется.
- `Command::Backend` и `decide_backend(args, &TopicSession) -> BackendDecision { Show, Unknown(String), AlreadySelected, Switch(TopicSession) }`; `Switch` сохраняет каталог и сбрасывает сессию.
- Справка: Codex, `/new [claude|codex] [path]`, `/backend`.
- Настройки:
  - `Settings.default_backend: BackendKind`.
  - `Settings.codex: CodexSettings { cli: Option<PathBuf>, model: Option<String>, sandbox: Sandbox, approval: Approval, api_key: Option<ApiKey> }`; `ApiKey` — newtype, `Debug` без значения.
  - `SettingsFile`: `[agents] default`, `[codex] cli, model, sandbox, approval` — все необязательные, старый `settings.toml` читается без изменений; ключ в файл не пишется.
  - `Draft`/`Field`: новые поля и проверка значений.

### `hub-telegram`

- `ClaudeAgents` → `HubAgents`; ветка `BackendKind::Codex` находит `codex` (`locate`) и запускает `CodexBackend`; ошибка поиска → `Failed`.
- Команда `/backend`; `/new` с бэкендом по умолчанию из настроек.
- Итог хода — `format_finished(&finished)`.

### `hub-app`

- `Secrets` параметризуется `enum Secret { TelegramToken, OpenAiKey }` (аккаунты `telegram-token`, `openai-api-key`).
- `connector`: обязателен CLI бэкенда по умолчанию (как сейчас Claude); второй бэкенд проверяется мягко — ошибка в лог и в статус. Для Codex после проверки версии — `probe_auth`. `StartError::Claude` → `StartError::Agent { kind, source }`.
- `Started.claude: String` → `agents: Vec<AgentStatus { kind, state: AgentState }>`, `AgentState { Ready { version, auth: Option<CodexAuth> }, Unavailable(String) }`.
- GUI «Настройки»: бэкенд по умолчанию; секция «Codex» — путь, модель, песочница и одобрения списками, API-ключ полем-паролем.
- GUI «Статус»: строка на каждый агент («Claude Code 2.1.287», «Codex 0.160.0 · вход: ChatGPT» или причина недоступности).

## Тесты

- `hub-agent`: `deliver_file`, `parse_questions` (rstest-таблицы). Тесты `hub-claude` проходят без изменений поведения.
- `hub-codex`:
  - `rpc` на `tokio::io::duplex`: ответ по id, уведомления, долгий запрос сервера не блокирует поток, EOF → `Closed`, неизвестный метод → `-32601`.
  - `protocol`: параметры вызовов — insta-снимки; `auth_state`, `tool_call`, `TurnTracker` — таблицы.
  - `requests`: четыре запроса сервера с поддельным `UserChannel`.
  - `session`: поддельный тред — steer во время хода, отклонённый steer после `turn/completed`, промпт без активного хода, отмена.
  - `backend`: поддельный app-server на `duplex` — новая сессия, продолжение, нет входа, вход по ключу, ошибка `thread/resume`, ошибки посреди хода.
  - `tests/live.rs`: живой прогон, `#[ignore]`, `AGENT_HUB_LIVE_CODEX=1`.
- `hub-core`: `Usage`/`format_finished`, `decide_backend`, `parse_new_args` с бэкендом по умолчанию, `SettingsFile`/`Draft` с `[codex]` и `[agents]`, чтение старого `settings.toml`.
- `hub-app`: `connector` — обязательный бэкенд по умолчанию и мягкая проверка второго; ключ OpenAI через `MemorySecrets`.
- Последняя задача — ручная проверка на Windows с реальным входом в Codex.

## Вне объёма

Фоновые задачи Codex; перенос контекста при `/backend`; вход в Codex из GUI или Telegram; автоустановка `codex`; исправление трея на Windows.
