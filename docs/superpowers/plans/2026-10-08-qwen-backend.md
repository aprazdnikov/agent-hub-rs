# Бэкенд Qwen — план реализации

> **For the implementer:** Use `executing-plans` (или superpowers:subagent-driven-development) to execute this plan task-by-task. Шаги размечены чекбоксами (`- [ ]`).

**Goal:** Qwen Code (`qwen --acp`) как третий агент рядом с Claude и Codex, выбираемый на уровне темы, с тем же поведением, что у Codex; релиз 0.3.0.

**Architecture:** JSON-RPC клиент переезжает из `hub-codex` в `hub-agent` (без изменения поведения Codex) и получает вызов без тайм-аута, уведомление с параметрами и конверт `"jsonrpc": "2.0"`. В `hub-agent` появляется `mcp_http` — MCP-сервер хаба по HTTP на `127.0.0.1` с bearer-токеном на сессию. Новый крейт `hub-qwen`: `version`, `protocol` (чистый перевод ACP), `session` (цикл ходов и выдача досылки через `Drain`), `requests` (одобрения, вопросы, drain), `backend` (процесс `qwen`, `session/new`/`session/load`). `hub-core` получает `BackendKind::Qwen`, `Usage::Qwen`, `QwenSettings`; `hub-telegram` и `hub-app` подключают их.

**Tech stack:** Rust 2024 (1.96), tokio, tokio-util (`LinesCodec`, `CancellationToken`, `DropGuard`), hyper 1 (`http1`, `server`), hyper-util (`tokio`), http-body-util, subtle, uuid v4, serde_json, thiserror, base64, eframe, keyring; тесты — rstest, insta, `tokio::io::duplex`, сырой TCP к `127.0.0.1`.

**Spec:** `docs/superpowers/specs/2026-10-08-qwen-backend-design.md` (решения не меняются; расхождения с исходниками Qwen — ниже).

**Constraints:**
- Линты рабочего пространства: `warnings = "deny"`, `unsafe_code = "forbid"`, `clippy::pedantic = deny`, `unwrap_used`/`expect_used`/`panic`/`indexing_slicing = deny` вне тестов (`clippy.toml` разрешает их в тестах). Никаких `#[allow]` в продакшен-коде; никаких `_ =>` на собственных enum.
- Гейты после каждой задачи: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked` (CI гоняет их на Linux, macOS и Windows — снимки и тесты не должны зависеть от платформы). После задач, добавляющих зависимости, — `cargo deny check` (если `cargo-deny` установлен; `deny.toml` в корне).
- Протокол проверен по исходникам qwen-code `v0.25.0` и `@agentclientprotocol/sdk@0.14.1`; `MIN_VERSION = 0.25.0`.
- Тайм-аут управляющих вызовов (`initialize`, `session/new`, `session/load`) — 60 с; `session/prompt` без тайм-аута (ход ограничивает `/stop` и выход процесса); ответ на `craft/drainMidTurnQueue` — не позже 1,5 с (Qwen ждёт 2 с).
- Остановка `qwen`: закрыть stdin, ждать до 10 с, затем `kill`. `QWEN_CODE_NO_RELAUNCH=true` всегда.
- Ключ Qwen — только в системном хранилище ключей (аккаунт `qwen-api-key`), не в `settings.toml`, не в `Debug`, не в логах. Токен MCP-сервера — не в логах.
- Тексты для пользователя — по-русски, дословно как в спецификации.
- Код в плане написан против текущих API, но не компилировался. Если `clippy::pedantic` просит `#[must_use]`, ссылку вместо значения, `# Errors` и т. п. — исправить код по линту, не подавляя его, сохраняя имена и сигнатуры из блоков **Interfaces**.
- Коммит после каждой задачи; сообщения по-русски в стиле истории репозитория; **никаких строк атрибуции ИИ** (`Co-Authored-By`, «Generated with …») — пользователь их запрещает. Ветку план не предписывает. `git push` и push тега — только после отдельного подтверждения пользователя (задача 15).

**Non-goals:** режим `auto`; переключение режима одобрений из Telegram; показ `plan`; «разрешить всегда»; перенос контекста при `/backend`; вход в Qwen из GUI/Telegram, кроме ключа; автоустановка `qwen`/Node; общий ACP-крейт; изменение `hub-claude` (его `mcp.rs` остаётся как есть, хотя `mcp_http` повторяет часть его логики — перенос был бы вне объёма).

---

## Расхождения со спецификацией

Решения спецификации сохранены; ниже — места, где её текст не совпадает с исходниками Qwen/SDK или с кодом репозитория, и как план это учитывает.

1. **Где лежат вопросы.** `qwenInteractionKind` и `qwenQuestions` — в `params.toolCall._meta`, а не в корневом `params._meta` (`packages/cli/src/acp-integration/session/Session.ts:14805-14822`, `session/permissionUtils.ts:124-133`). Корневой `_meta` может нести только `mcpAppCallId`/`backgroundTurn`.
2. **Форма `answers`.** `Record<string, string>`: ключ — индекс вопроса десятичной строкой (`"0"`, `"1"`, …), значение — строка (`packages/core/src/tools/askUserQuestion.ts:274-285`, `packages/core/src/permissions/trusted-user-answers.ts:34-48`). Мультивыбор — одна строка; хаб уже склеивает метки через `", "` (`QuestionAnswer`).
3. **Подготовленные вызовы инструментов.** При потоковых ответах OpenAI-совместимых и Anthropic-провайдеров Qwen сначала шлёт `tool_call` с `_meta.phase: "preparing"`, пустым `rawInput` и заголовком из одного имени инструмента, а полный заголовок приходит позже в `tool_call_update` того же `toolCallId` (`session/tool-call-preparation-tracker.ts:24-46`, `session/emitters/tool-call-emitter.ts:111-160`, `packages/acp-bridge/src/transcript-replay.ts`, `createTranscriptToolCallStartUpdate` — `sessionUpdate: asUpdate ? 'tool_call_update' : 'tool_call'`; источник подготовок — `packages/core/src/core/openaiContentGenerator/converter.ts:2063`). Спецификация велит игнорировать `tool_call_update`; тогда строки инструментов были бы пустыми. План: `tool_call` с `phase: "preparing"` откладывается, `ToolCall` выдаётся по первому `tool_call_update` этого id с непустым `title`; отменённая подготовка (`_meta.preparationDiscarded: true`) просто забывается. Прочие `tool_call_update` — по-прежнему ничего.
4. **Токены.** `_meta.usage.totalTokens` — итог одного обращения к модели (prompt + ответ этого раунда), а не накопленный счёт сессии; при отсутствии данных у провайдера Qwen пишет `0` (`transcript-replay.ts`, `createTranscriptUsageUpdate`: `totalTokens: finiteNumber(...) ?? 0`). План: запоминается последнее ненулевое значение (≈ текущий объём контекста сессии); подпись «токенов в сессии» не меняется.
5. **Субагенты.** Текст и `usage` субагентов приходят как `agent_message_chunk` с `_meta.parentToolCallId` (`session/SubAgentTracker.ts:87, 356, 376`; `MessageEmitter.ts` — `extra: { ...subagentMeta }`). План: такие чанки игнорируются целиком (иначе внутренний текст субагента склеится с ответом); их `tool_call` показываются как обычно.
6. **`session/load` не возвращает `sessionId`** (`zLoadSessionResponse` в SDK: только `_meta`, `configOptions`, `models`, `modes`). `SessionStarted` получает сохранённый id. Несуществующая сессия → `-32002 "Resource not found: session:<id>"` (`acpAgent.ts:6082-6085`); `authRequired` возможен и на `load` (`acpAgent.ts:6156, 6178`).
7. **Drain.** Ответ — `{items: [{content: ContentBlock[]}]}`; Qwen берёт не больше 10 элементов и отбрасывает остальные (`Session.ts:1162, 1393-1398`). Запрос может нести `todoStopGuardWatchQueuedPrompt: true`; тогда без булева `hasQueuedPrompt` ответ считается ненадёжным (`Session.ts:1531-1540`). План: за один drain отдаётся не больше 10 промптов, остальные ждут следующего drain или хода; в ответе всегда `"hasQueuedPrompt": false`.
8. **Обработчик drain не владеет inbox.** Inbox принадлежит циклу сессии, а запросы агента отвечаются в отдельных задачах. Вместо `answer(channel, inbox, …)` — `answer(channel, drain, …)`, где `Drain` просит цикл отдать промпты через `oneshot`; цикл отвечает мгновенно, обработчик ждёт не дольше 1,5 с.
9. **`session/prompt` длится весь ход**, а `RpcClient::request` ставит 60 с на любой вызов. В `hub-agent::rpc` добавляется `request_untimed`. `RpcClient::notify` не умеет параметры, а `session/cancel` их требует — добавляется `notify_with`.
10. **`"jsonrpc"`.** `qwen --acp` читает stdin без проверки поля `jsonrpc` (`acpAgent.ts:3141` передаёт `ndJsonStream` без `limits` → `createLegacyReadable`, `packages/acp-bridge/src/ndJsonStream.ts:520-562`), но ограниченный режим того же модуля требует `"jsonrpc": "2.0"` и рвёт соединение (`ndJsonStream.ts:647-650`). План: соединение с Qwen шлёт `"jsonrpc": "2.0"` (`Envelope::JsonRpc2`), Codex — как раньше (`Envelope::Bare`).
11. **Имя агента в ошибках.** `RpcError` сравнивается по вариантам (`RpcError::Closed` в тестах Codex), поэтому имя агента (`Wire::peer`) попадает в строки лога `rpc`, а тексты `RpcError` становятся нейтральными («the agent closed the connection»). Для Codex меняется только текст этой ошибки в логе и в статусе «Codex app-server: …»; тесты Codex на него не опираются.
12. **Токен MCP.** `Uuid::new_v4` даёт 122 случайных бита, не 128. План: токен — два v4 подряд (244 бита).
13. **MCP `ping`** отвечается `{}` (обязателен по MCP), неизвестный инструмент в `tools/call` → `-32602` (как в `hub-claude/src/mcp.rs`); прочие методы → `-32601`, как в спецификации.
14. **`TurnTracker::finish`** возвращает `Vec<AgentEvent>`: накопленный текст должен уйти перед `Finished`.
15. **Справка** живёт в `hub-telegram/src/texts.rs`, а её тесты (и тест в `hub.rs:897`) ломаются, как только появляется `BackendKind::Qwen`; поэтому тексты меняются в задаче 3 вместе с `hub-core`.
16. **Информационно.** В недоверенной папке Qwen сам понижает любой режим, кроме `plan`/`default`, до `default` (`packages/cli/src/config/config.ts:1914-1925`); `session/load` может восстановить режим одобрений, сохранённый в сессии (`acpAgent.ts`, `applyRestoredSessionApprovalMode`). Поведение хаба от этого не меняется.

## Протокол: проверено по исходникам

Источники: qwen-code тег `v0.25.0` (коммит `6788c035`), `packages/cli/package.json:4` — `"version": "0.25.0"`, зависимость `@agentclientprotocol/sdk ^0.14.1` (в `pnpm-lock.yaml` — `0.14.1`); пакет SDK `0.14.1` распакован из npm. Пути Qwen ниже — относительно `packages/cli/src/acp-integration/`, если не указано иное.

**Транспорт.** Построчный JSON, по одному сообщению на строку. Запросы агента к клиенту получают числовые id с нуля (SDK `dist/acp.js:858-864`, `sendRequest`). Ответ клиента SDK не проверяет схемой: `requestPermission` и `extMethod` — это `sendRequest` без zod (`dist/acp.js:152-153, 198-199`), `#handleResponse` отдаёт `response.result` как есть (`dist/acp.js:843-856`). Ошибка обработчика — `{error: {code, message, data}}`.

**1. `answers` доходит до Qwen.** Ответ на `session/request_permission` не валидируется (см. выше), Qwen читает `output.answers` из корня ответа (`session/Session.ts:14855-14868, 14938`) и передаёт в `onConfirm` (`packages/core/src/tools/askUserQuestion.ts:229-247`). Запрос вопроса:

```json
{"jsonrpc":"2.0","id":0,"method":"session/request_permission","params":{
  "sessionId":"s-1",
  "options":[{"optionId":"proceed_once","name":"Submit","kind":"allow_once"},
             {"optionId":"cancel","name":"Cancel","kind":"reject_once"}],
  "toolCall":{"toolCallId":"c-1","status":"pending","title":"Ask user 1 question","content":[],"locations":[],
    "kind":"think","rawInput":{"questions":[...]},
    "_meta":{"toolName":"ask_user_question","qwenInteractionKind":"user_question",
             "qwenQuestions":[{"question":"Какую БД?","header":"БД","multiSelect":false,
               "options":[{"label":"Postgres","description":"надёжно"},{"label":"SQLite","description":"просто"}]}]}}}}
```

(`session/Session.ts:14803-14822`, `session/permissionUtils.ts:124-133, 360-372`; `kind` — `Kind.Think` → `think`, `askUserQuestion.ts:330`). Ответ хаба:

```json
{"outcome":{"outcome":"selected","optionId":"proceed_once"},"answers":{"0":"Postgres"}}
```

Отказ — `{"outcome":{"outcome":"selected","optionId":"cancel"}}` → Qwen отвечает модели «User declined to answer the questions.» (`askUserQuestion.ts:265-270`). Выбор неотмеченного варианта Qwen считает ошибкой (`permissionUtils.ts:230-252`), поэтому хаб проверяет, что `proceed_once` и `cancel` есть в `options`.

**2. MCP-сервер из `session/new`.** Запись: `{"type":"http","name":"agent-hub","url":"http://127.0.0.1:<port>/mcp","headers":[{"name":"Authorization","value":"Bearer <token>"}]}` — схема `zMcpServerHttp.and({type:"http"})`, `headers: [{name, value}]` обязательны (SDK `dist/schema/zod.gen.js:168-172, 217-222, 250-257`); Qwen превращает её в `MCPServerConfig(httpUrl=url, headers)` (`acpAgent.ts:3543-3550, 15281-15295`), заголовки идут в `requestInit` транспорта Streamable HTTP (`packages/core/src/tools/mcp-client.ts:2610-2622`). `trust` не задаётся (`packages/core/src/config/mcp-server-config.ts:106-127`), а инструмент MCP без `trust` получает `'ask'` (`packages/core/src/tools/mcp-tool.ts:385-395`) → **вызов `send_file` вызывает `session/request_permission`** с `toolCall.kind` по `Kind` инструмента и вариантами `proceed_always_project`/`proceed_always_user`/`proceed_once`/`cancel` (`permissionUtils.ts:296-315`); хаб отвечает только `proceed_once` или `cancel`. В режиме `yolo` запроса нет. GET с `Accept: text/event-stream` на 405 клиент MCP SDK понимает как «SSE не поддерживается» (`mcp-client.ts:257-290`). Авто-обнаружение удалённого MCP — 5 с (`mcp-server-config.ts`, комментарий к `discoveryTimeout`): сервер хаба поднимается до `session/new`.

**3. Остальное.**
- `initialize` → `{protocolVersion: 1, agentInfo: {name: "qwen-code", title: "Qwen Code", version}, authMethods, agentCapabilities: {loadSession: true, promptCapabilities: {image: true, audio: true, embeddedContext: true}, mcpCapabilities: {http: true, sse: true}, ...}}` (`acpAgent.ts:5377-5445`); `protocolVersion` — целое 0..65535 (`zod.gen.js:393`), SDK `PROTOCOL_VERSION = 1` (`dist/schema/index.js:27`).
- `session/new {cwd, mcpServers}` → `{sessionId, models, modes, configOptions}` (`acpAgent.ts:5662-5810`). Нет входа → `RequestError.authRequired` = код **`-32000`**, сообщение `"Authentication required: Use Qwen Code CLI to authenticate first."` или `"Authentication required: Authentication failed: <причина>"` (`acpAgent.ts:15722-15750`, SDK `dist/acp.js:939-941`, `errorCodes.ts` — `AUTH_REQUIRED: -32000`).
- `session/load {sessionId, cwd, mcpServers}` — в обычном (не bulk) режиме история уходит уведомлениями `session/update` **до** ответа (`acpAgent.ts:6002-6006`); ответ без `sessionId`.
- `session/prompt {sessionId, prompt}` → `{stopReason}` по окончании хода; значения схемы — `end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled` (`zod.gen.js:1029-1035`); Qwen возвращает `end_turn`, `cancelled`, `max_tokens` (`session/Session.ts`, `stopReason: '…'`). Блоки: `{type: "image", data, mimeType}` (`zod.gen.js:475-481`), `{type: "text", text}`.
- `session/cancel {sessionId}` — уведомление, схема требует `sessionId` (`zod.gen.js:627-630`, `dist/acp.js:110-114`).
- `session/update {sessionId, update}`: текст — `{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"…"}}`; токены — `{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":""},"_meta":{"usage":{"inputTokens":…,"outputTokens":…,"totalTokens":…},"durationMs":…}}` (`MessageEmitter.ts:245-310`, `createTranscriptUsageUpdate`); вызов инструмента — `{"sessionUpdate":"tool_call","toolCallId":"c-1","status":"pending","title":"Shell: ls -la","content":[],"locations":[],"kind":"execute","rawInput":{…},"_meta":{"toolName":"run_shell_command",…}}` (`createTranscriptToolCallStartUpdate`; `kind` из `KIND_MAP`, `tool-call-emitter.ts:463-473`).
- `craft/drainMidTurnQueue` — запрос агента к клиенту, имя на проводе ровно такое (`packages/acp-bridge/src/bridgeTypes.ts:1474`, вызов `Session.ts:9580-9587`), параметры `{sessionId, promptId?, todoStopGuardWatchQueuedPrompt?}`; ответ `{items: [{content: ContentBlock[]}], hasQueuedPrompt?}` (`Session.ts:1467-1545`); ожидание 2 с, после 3 тайм-аутов подряд или `-32601` drain отключается до конца сессии (`Session.ts:1147, 1170, 9640-9662`).
- `--approval-mode` принимает `plan`, `default`, `auto-edit` (также `auto_edit`/`autoedit`), `auto`, `yolo` (`packages/core/src/config/approval-mode.ts:7-17`, `packages/cli/src/config/approval-mode-value.ts`); без флага и настройки режим — `auto` (`config.ts:1890-1911`), поэтому хаб всегда передаёт флаг. `--auth-type` принимает `openai` (`top-level-options.ts:47-54, 444-448`), `-m` — модель (`top-level-options.ts:185-189`). Переменные ключа: `OPENAI_API_KEY`, `OPENAI_BASE_URL`, `OPENAI_MODEL` (`packages/core/src/models/constants.ts:76-82`). `QWEN_CODE_NO_RELAUNCH` отключает перезапуск дочерним процессом (`packages/cli/src/utils/relaunch.ts:114-131`).
- Закрытие stdin: `connection.closed` → завершение сессий и `process.exit` (`acpAgent.ts:3489` и далее).
- `qwen --version` печатает строку версии yargs (`packages/cli/src/config/config.ts:906-907`, `utils/version.ts` — `CLI_VERSION` или `package.json`), т. е. `0.25.0`. **Не проверено живым запуском** — разбор (`split_whitespace().find_map(Version::parse)`) принимает и `0.25.0`, и `qwen 0.25.0`.

**Не проверено и подтверждается только живым тестом/ручной проверкой (задачи 13–14):** фактический текст `qwen --version` у npm-сборки; что запрос одобрения `send_file` действительно приходит (по исходникам — да); поведение `answers` на реальном процессе.

## Review Focus

1. **Drain за 2 с.** Ответ должен уйти до 2 с даже при занятом цикле; промпты, которые цикл отдал после тайм-аута обработчика, не теряются (задача 6).
2. **`/stop` посреди хода.** `session/cancel`, выход из цикла, остановка `qwen` (stdin → 10 с → kill), сервер MCP гасится после процесса; inbox возвращается (задачи 6, 8).
3. **MCP-сервер.** Только `127.0.0.1`, токен на сессию, сравнение за постоянное время, лимиты тела/заголовков/соединений, остановка при drop (задача 2).
4. **Ключ.** Хранилище ключей, `Debug`, окружение только дочернего процесса; пара URL+ключ (задачи 3, 8, 10).
5. **Старый `settings.toml`** без `[qwen]` читается без изменений (задача 3).

## Порядок задач

| # | Задача | Крейты |
|---|---|---|
| 1 | JSON-RPC клиент переезжает в `hub-agent` | hub-agent, hub-codex |
| 2 | `hub-agent::mcp_http` | hub-agent |
| 3 | `BackendKind::Qwen`, `Usage::Qwen`, настройки Qwen, тексты | hub-core, hub-telegram, hub-app |
| 4 | Крейт `hub-qwen` и поиск `qwen` | hub-qwen |
| 5 | Протокол (`protocol`) | hub-qwen |
| 6 | Цикл ходов и drain (`session`) | hub-qwen |
| 7 | Запросы агента (`requests`) | hub-qwen |
| 8 | Бэкенд: процесс и сессия (`backend`) | hub-qwen |
| 9 | Хаб запускает Qwen (`HubAgents`) | hub-telegram |
| 10 | Ключ Qwen в хранилище ключей | hub-app |
| 11 | Проверка Qwen при запуске и статус | hub-app |
| 12 | GUI: настройки Qwen | hub-app |
| 13 | Живой тест, README, версия 0.3.0 | hub-qwen, README, Cargo |
| 14 | Ручная проверка на Linux и Windows | — |
| 15 | Релиз: тег `v0.3.0` (push — только после подтверждения) | — |

---
### Task 1: JSON-RPC клиент переезжает в `hub-agent`

Перенос без изменения поведения Codex плюс три добавления, нужные Qwen: `request_untimed`, `notify_with`, конверт `"jsonrpc"`.

**Files:**
- Create: `crates/hub-agent/src/rpc.rs` (из `crates/hub-codex/src/rpc.rs:1-448` с изменениями ниже)
- Create: `crates/hub-agent/src/testing.rs`
- Modify: `crates/hub-agent/src/lib.rs:1-4`, `crates/hub-agent/Cargo.toml`
- Delete: `crates/hub-codex/src/rpc.rs`
- Modify: `crates/hub-codex/src/lib.rs:1-8`, `crates/hub-codex/src/protocol.rs:16`, `crates/hub-codex/src/requests.rs:13`, `crates/hub-codex/src/session.rs:17`, `crates/hub-codex/src/backend.rs:27, 244-264`, `crates/hub-codex/src/testing.rs:11, 30-35`
- Test: `crates/hub-agent/src/rpc.rs` (`#[cfg(test)] mod tests` — тесты из `hub-codex` плюс новые)

**Interfaces:**
- Produces (`hub_agent::rpc`):
  - `RpcError { Remote { code: i64, message: String }, Closed, Protocol(String), Timeout { method: String } }`, `RequestError { Unsupported(String), Malformed(String) }`, `Notification { method, params }`, `type Handler`, `Connection { client, notifications, reader }`, `METHOD_NOT_FOUND = -32601`, `INVALID_PARAMS = -32602` — как в `hub-codex`.
  - `pub enum Envelope { Bare, JsonRpc2 }`; `pub struct Wire { pub peer: &'static str, pub envelope: Envelope, pub timeout: Duration }`.
  - `pub fn connect<R, W>(reader: R, writer: W, handler: Handler, wire: Wire) -> Connection`.
  - `RpcClient`: `request(&self, method: &str, params: Value) -> Result<Value, RpcError>` (тайм-аут `wire.timeout`), `request_untimed(&self, method: &str, params: Value) -> Result<Value, RpcError>`, `notify(&self, method: &str) -> Result<(), RpcError>`, `notify_with(&self, method: &str, params: Value) -> Result<(), RpcError>`, `close(&self)`.
- Produces (`#[cfg(test)] hub_agent::testing`): `Peer { read, write }`, `pair(handler, wire) -> (Connection, Peer)`, `refusing() -> Handler`, `bare(timeout) -> Wire`.

- [ ] **Step 1: Перенести файл и тесты (RED)**

`git mv crates/hub-codex/src/rpc.rs crates/hub-agent/src/rpc.rs`.

`crates/hub-agent/src/lib.rs`:

```rust
pub mod channel;
pub mod cli;
pub mod conversation;
pub mod rpc;
#[cfg(test)]
mod testing;
pub mod tools;
```

`crates/hub-agent/src/testing.rs` — файл `crates/hub-codex/src/testing.rs:1-36` (верхняя часть до `use std::sync::Mutex;`), с правками:

```rust
//! The agent side of a duplex pipe, for tests.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};

use crate::rpc::{Connection, Envelope, Handler, RequestError, Wire, connect};

pub struct Peer {
    lines: Lines<BufReader<ReadHalf<DuplexStream>>>,
    writer: WriteHalf<DuplexStream>,
}

impl Peer {
    /// The next message from the client; `None` once the client closed its output.
    pub async fn read(&mut self) -> Option<Value> {
        let line = self.lines.next_line().await.unwrap()?;
        Some(serde_json::from_str(&line).unwrap())
    }

    pub async fn write(&mut self, message: Value) {
        self.writer.write_all(format!("{message}\n").as_bytes()).await.unwrap();
    }
}

pub fn pair(handler: Handler, wire: Wire) -> (Connection, Peer) {
    let (ours, theirs) = tokio::io::duplex(1 << 20);
    let (our_read, our_write) = tokio::io::split(ours);
    let (their_read, their_write) = tokio::io::split(theirs);
    let connection = connect(our_read, our_write, handler, wire);
    (connection, Peer { lines: BufReader::new(their_read).lines(), writer: their_write })
}

pub fn bare(timeout: Duration) -> Wire {
    Wire { peer: "test agent", envelope: Envelope::Bare, timeout }
}

pub fn refusing() -> Handler {
    Arc::new(|method, _params| Box::pin(async move { Err(RequestError::Unsupported(method)) }))
}
```

В тестах `crates/hub-agent/src/rpc.rs` (`mod tests`, бывшие строки 277-456): `use crate::testing::{pair, refusing};` → `use crate::testing::{bare, pair, refusing};`; каждое `pair(x, LONG)` → `pair(x, bare(LONG))`, `pair(refusing(), Duration::from_millis(50))` → `pair(refusing(), bare(Duration::from_millis(50)))`. Добавить новые тесты в конец модуля:

```rust
    #[tokio::test]
    async fn untimed_request_outlives_the_timeout() {
        let (connection, mut peer) = pair(refusing(), bare(Duration::from_millis(50)));
        let request = tokio::spawn(async move {
            connection.client.request_untimed("session/prompt", json!({})).await
        });
        let sent = peer.read().await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        peer.write(json!({"id": id(&sent), "result": {"stopReason": "end_turn"}})).await;
        assert_eq!(request.await.unwrap(), Ok(json!({"stopReason": "end_turn"})));
    }

    #[tokio::test]
    async fn notification_carries_its_params() {
        let (connection, mut peer) = pair(refusing(), bare(LONG));
        connection.client.notify_with("session/cancel", json!({"sessionId": "s-1"})).await.unwrap();
        assert_eq!(
            peer.read().await,
            Some(json!({"method": "session/cancel", "params": {"sessionId": "s-1"}}))
        );
    }

    #[tokio::test]
    async fn json_rpc_envelope_marks_every_outgoing_message() {
        let wire = Wire { peer: "qwen", envelope: Envelope::JsonRpc2, timeout: LONG };
        let handler: Handler = Arc::new(|_method, _params| Box::pin(async { Ok(json!({})) }));
        let (connection, mut peer) = pair(handler, wire);
        connection.client.notify_with("session/cancel", json!({"sessionId": "s-1"})).await.unwrap();
        let request = tokio::spawn({
            let client = connection.client.clone();
            async move { client.request("initialize", json!({})).await }
        });
        let notified = peer.read().await.unwrap();
        let requested = peer.read().await.unwrap();
        peer.write(json!({"jsonrpc": "2.0", "id": id(&requested), "result": {}})).await;
        request.await.unwrap().unwrap();
        peer.write(json!({"jsonrpc": "2.0", "id": 0, "method": "craft/drainMidTurnQueue", "params": {}}))
            .await;
        let answered = peer.read().await.unwrap();
        assert_eq!(
            [&notified, &requested, &answered].map(|message| message.get("jsonrpc").cloned()),
            [Some(json!("2.0")), Some(json!("2.0")), Some(json!("2.0"))]
        );
    }
```

Run: `cargo test -p hub-agent rpc`
Expected: FAIL — не компилируется: нет `Wire`, `Envelope`, `request_untimed`, `notify_with`; в `hub-agent` нет зависимостей `tracing`, `tokio-util/codec`.

- [ ] **Step 2: Реализация (GREEN)**

`crates/hub-agent/Cargo.toml`:

```toml
[dependencies]
futures.workspace = true
hub-core.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["io-util", "macros", "process", "rt", "sync", "time"] }
tokio-util = { workspace = true, features = ["codec"] }
tracing.workspace = true
which.workspace = true

[dev-dependencies]
rstest.workspace = true
tokio = { workspace = true, features = ["io-util", "macros", "process", "rt", "sync", "time"] }
```

`crates/hub-agent/src/rpc.rs` — изменения относительно перенесённого файла:

1. Заголовок модуля: «Newline-delimited JSON-RPC 2.0 over a child's stdio, in both directions.» — без упоминания app-server.
2. Тексты ошибок нейтральные:

```rust
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    #[error("{message}")]
    Remote { code: i64, message: String },
    #[error("the agent closed the connection")]
    Closed,
    #[error("unexpected message from the agent: {0}")]
    Protocol(String),
    #[error("the agent did not answer {method} in time")]
    Timeout { method: String },
}
```

3. Конверт и параметры соединения (после `pub type Handler`):

```rust
/// Codex app-server takes bare messages; ACP agents expect the JSON-RPC 2.0 member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Envelope {
    Bare,
    JsonRpc2,
}

impl Envelope {
    fn seal(self, message: Value) -> String {
        match (self, message) {
            (Self::JsonRpc2, Value::Object(mut fields)) => {
                fields.insert("jsonrpc".to_owned(), Value::from("2.0"));
                Value::Object(fields).to_string()
            }
            (Self::Bare | Self::JsonRpc2, message) => message.to_string(),
        }
    }
}

/// Who is on the other end: named in the log, and how long a control call may wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wire {
    pub peer: &'static str,
    pub envelope: Envelope,
    pub timeout: Duration,
}
```

4. `RpcClient` получает поле `wire: Wire` вместо `timeout: Duration`; `Link` — поля `peer: &'static str` и `envelope: Envelope`. `connect`:

```rust
#[must_use]
pub fn connect<R, W>(reader: R, writer: W, handler: Handler, wire: Wire) -> Connection
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (outbox, lines) = mpsc::channel(OUTBOX);
    // Unbounded: a session waiting on a reply must not block the reader behind it.
    let (notify, notifications) = mpsc::unbounded_channel();
    let pending: Pending = Arc::new(Mutex::new(Some(HashMap::new())));
    let shutdown = CancellationToken::new();
    tokio::spawn(write(writer, lines, shutdown.clone(), wire.peer));
    let router = Link {
        outbox: outbox.clone(),
        pending: Arc::clone(&pending),
        notify,
        handler,
        peer: wire.peer,
        envelope: wire.envelope,
    };
    let reader = tokio::spawn(read(reader, router));
    let client = RpcClient { outbox, pending, next: Arc::new(AtomicU64::new(1)), wire, shutdown };
    Connection { client, notifications, reader }
}
```

5. Вызовы:

```rust
impl RpcClient {
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.call(method, params, Some(self.wire.timeout)).await
    }

    /// For a call that lasts as long as the agent works, such as a whole ACP turn; it ends
    /// with the reply, with `/stop` (the caller drops it) or when the agent's output closes.
    pub async fn request_untimed(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.call(method, params, None).await
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        deadline: Option<Duration>,
    ) -> Result<Value, RpcError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (waiter, reply) = oneshot::channel();
        match lock(&self.pending).as_mut() {
            Some(waiters) => waiters.insert(id, waiter),
            None => return Err(RpcError::Closed),
        };
        let line = self.wire.envelope.seal(json!({"id": id, "method": method, "params": params}));
        if self.outbox.send(line).await.is_err() {
            self.forget(id);
            return Err(RpcError::Closed);
        }
        let outcome = match deadline {
            Some(deadline) => tokio::time::timeout(deadline, reply).await,
            None => Ok(reply.await),
        };
        self.forget(id);
        match outcome {
            Err(_) => Err(RpcError::Timeout { method: method.to_owned() }),
            Ok(Err(_)) => Err(RpcError::Closed),
            Ok(Ok(reply)) => reply,
        }
    }

    pub async fn notify(&self, method: &str) -> Result<(), RpcError> {
        self.send_notification(json!({"method": method})).await
    }

    pub async fn notify_with(&self, method: &str, params: Value) -> Result<(), RpcError> {
        self.send_notification(json!({"method": method, "params": params})).await
    }

    async fn send_notification(&self, message: Value) -> Result<(), RpcError> {
        let line = self.wire.envelope.seal(message);
        self.outbox.send(line).await.map_err(|_| RpcError::Closed)
    }

    /// Closes the peer's input, which is how a stdio agent is told to exit.
    pub fn close(&self) {
        self.shutdown.cancel();
    }
    // `forget` — без изменений.
}
```

`Ok(reply.await)` в ветке `None` даёт тот же тип `Result<Result<_, RecvError>, Elapsed>`, что и `timeout`; если компилятор не выводит тип `Elapsed`, обернуть явно: `Ok::<_, tokio::time::error::Elapsed>(reply.await)`.

6. Логи с именем агента: в `write` добавить параметр `peer: &'static str` и писать `tracing::warn!(peer, %error, "agent input closed")`, `tracing::debug!(peer, %error, "agent input already closed")`; в `read` — `tracing::warn!(peer = router.peer, %error, "agent output unreadable")`; в `Link::dispatch` — `"unparsable agent line"`, `"unexpected agent message"`, `"reply to a request this client never sent"` с полем `peer = self.peer`; `"server request handler crashed"` → `"agent request handler crashed"` с `peer`.
7. Ответы на запросы агента запечатываются тем же конвертом: в `Link::dispatch` передать `self.envelope` в `answer`, а в `answer` заменить `outbox.send(message.to_string())` на `outbox.send(envelope.seal(message))`; сигнатура `async fn answer(id: Value, method: String, reply: BoxFuture<'static, Result<Value, RequestError>>, outbox: mpsc::Sender<String>, envelope: Envelope)`.

- [ ] **Step 3: Перевести `hub-codex`**

- `crates/hub-codex/src/lib.rs`: убрать строку `pub mod rpc;`.
- `protocol.rs:16`: `use crate::rpc::{Notification, RpcError};` → `use hub_agent::rpc::{Notification, RpcError};`
- `requests.rs:13`: `use crate::rpc::RequestError;` → `use hub_agent::rpc::RequestError;`
- `session.rs:17`: `use crate::rpc::{Notification, RpcClient, RpcError};` → `use hub_agent::rpc::{Notification, RpcClient, RpcError};`
- `backend.rs:27`: `use crate::rpc::{self, Connection, Handler, Notification, RequestError, RpcClient, RpcError};` → `use hub_agent::rpc::{self, Connection, Envelope, Handler, Notification, RequestError, RpcClient, RpcError, Wire};`; в `AppServer::spawn` (строки 261-262):

```rust
        let wire = Wire { peer: "codex app-server", envelope: Envelope::Bare, timeout: REQUEST_TIMEOUT };
        let Connection { client, notifications, reader } = rpc::connect(stdout, stdin, handler, wire);
```

- `testing.rs:11`: `use crate::rpc::{Connection, Handler, RequestError, connect};` → `use hub_agent::rpc::{Connection, Envelope, Handler, RequestError, Wire, connect};`; в `pair` (строка 34): `connect(our_read, our_write, handler, Wire { peer: "codex app-server", envelope: Envelope::Bare, timeout })`.

Проверить, что старых путей не осталось: `rg "crate::rpc" crates/hub-codex` — пусто.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p hub-agent rpc && cargo test -p hub-codex`
Expected: PASS; в `hub-agent` — 9 перенесённых тестов `rpc` и 3 новых; тесты `hub-codex` проходят без изменения ожиданий.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS, без предупреждений.

- [ ] **Step 6: Commit**

```bash
git add -A crates
git commit -m "hub-agent: JSON-RPC клиент переезжает из hub-codex"
```

---

### Task 2: MCP-сервер хаба по HTTP (`hub-agent::mcp_http`)

**Files:**
- Modify: `Cargo.toml` (`[workspace.dependencies]`)
- Modify: `crates/hub-agent/Cargo.toml`, `crates/hub-agent/src/lib.rs`
- Create: `crates/hub-agent/src/mcp_http.rs`
- Test: `crates/hub-agent/src/mcp_http.rs` (чистые `admit`/`dispatch`), `crates/hub-agent/tests/mcp_http.rs` (реальный `127.0.0.1`)

**Interfaces:**
- Produces (`hub_agent::mcp_http`):
  - `pub struct McpHttpServer` (без `Clone`; `Debug` скрывает токен), `pub async fn start(channel: Arc<dyn UserChannel>) -> io::Result<McpHttpServer>`, `pub fn url(&self) -> &str` (`http://127.0.0.1:<port>/mcp`), `pub fn token(&self) -> &str`. Drop останавливает приём и обрывает открытые соединения.
  - `pub const SERVER_NAME: &str = "agent-hub"`, `pub const MCP_PATH: &str = "/mcp"`.
- Consumes: `hub_agent::tools::{SEND_FILE, SEND_FILE_DESCRIPTION, send_file_schema, deliver_file, ToolResult}`, `hub_agent::rpc::{METHOD_NOT_FOUND, INVALID_PARAMS}`.

Поведение (`admit` → `dispatch`):
- проверка `Authorization` идёт первой: точное `Bearer <token>`, сравнение `subtle::ConstantTimeEq`; нет/чужой → `401` без тела;
- путь не `/mcp` → `404`; метод не `POST` → `405` с `Allow: POST`;
- `Content-Length` больше 1 МиБ → `413` без чтения тела; тело читается через `Limited` (1 МиБ) за 10 с: превышение → `413`, тайм-аут → `408`, иная ошибка → `400`;
- не JSON → `400` и `{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}`; не объект → `400`, `-32600`;
- сообщение без `id` (уведомления `notifications/*`) или без `method` (ответ клиента) → `202` без тела;
- `initialize` → `protocolVersion` клиента (или `2025-06-18`), `capabilities: {tools: {}}`, `serverInfo: {name: "agent-hub", version}`; `ping` → `{}`; `tools/list` → один `send_file`; `tools/call` `send_file` → `deliver_file` → `{"content":[{"type":"text","text":…}],"isError":…}`; другой инструмент → `-32602 "Unknown tool: <name>"`; прочее → `-32601 "Method not found: <method>"`;
- ответы `200`, `Content-Type: application/json`;
- заголовки читаются не дольше 10 с (`header_read_timeout`), одновременно не больше 16 соединений; ошибка `accept` → предупреждение в лог и пауза 100 мс.

- [ ] **Step 1: Зависимости**

`Cargo.toml`, `[workspace.dependencies]` (по алфавиту рядом с соседями):

```toml
http-body-util = "0.1"
hyper = { version = "1.11", default-features = false, features = ["http1", "server"] }
hyper-util = { version = "0.1.21", default-features = false, features = ["tokio"] }
subtle = "2.6"
```

В `Cargo.lock` уже есть `hyper 1.11.1`, `hyper-util 0.1.21`, `http-body-util 0.1.5`, `subtle 2.6.1`; фича `hyper/server` добавит новый крейт `httpdate` (MIT/Apache-2.0).

`crates/hub-agent/Cargo.toml`, `[dependencies]` — добавить:

```toml
http-body-util.workspace = true
hyper.workspace = true
hyper-util.workspace = true
subtle.workspace = true
uuid = { workspace = true, features = ["v4"] }
```

и в `tokio` обоих разделов — фичу `"net"`.

`crates/hub-agent/src/lib.rs` — `pub mod mcp_http;` между `conversation` и `rpc`.

Run: `cargo check -p hub-agent && cargo deny check`
Expected: PASS (если `cargo-deny` не установлен — пропустить и отметить в отчёте задачи).

- [ ] **Step 2: Написать тесты (RED)**

Модульные тесты в конце `crates/hub-agent/src/mcp_http.rs`:

```rust
#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    const TOKEN: &str = "t0k3n";

    #[rstest]
    #[case::no_token(Method::POST, "/mcp", None, Admission::Unauthorized)]
    #[case::wrong_token(Method::POST, "/mcp", Some("Bearer nope"), Admission::Unauthorized)]
    #[case::token_without_scheme(Method::POST, "/mcp", Some("t0k3n"), Admission::Unauthorized)]
    #[case::unknown_path(Method::POST, "/other", Some("Bearer t0k3n"), Admission::NotFound)]
    #[case::get(Method::GET, "/mcp", Some("Bearer t0k3n"), Admission::MethodNotAllowed)]
    #[case::post(Method::POST, "/mcp", Some("Bearer t0k3n"), Admission::Mcp)]
    fn requests_are_admitted(
        #[case] method: Method,
        #[case] path: &str,
        #[case] authorization: Option<&'static str>,
        #[case] expected: Admission,
    ) {
        let header = authorization.map(HeaderValue::from_static);
        assert_eq!(admit(&method, path, header.as_ref(), TOKEN), expected);
    }

    #[rstest]
    #[case::notification(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}), Step::Accepted)]
    #[case::client_reply(json!({"jsonrpc": "2.0", "id": 3, "result": {}}), Step::Accepted)]
    #[case::ping(json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}), Step::Reply(json!({"jsonrpc": "2.0", "id": 1, "result": {}})))]
    #[case::unknown_method(json!({"jsonrpc": "2.0", "id": 2, "method": "resources/list"}), Step::Reply(json!({"jsonrpc": "2.0", "id": 2, "error": {"code": -32601, "message": "Method not found: resources/list"}})))]
    #[case::unknown_tool(json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "rm"}}), Step::Reply(json!({"jsonrpc": "2.0", "id": 4, "error": {"code": -32602, "message": "Unknown tool: rm"}})))]
    #[case::send_file(json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "send_file", "arguments": {"path": "a.txt"}}}), Step::SendFile { id: json!(5), arguments: json!({"path": "a.txt"}) })]
    #[case::not_an_object(json!([1]), Step::Invalid(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "Invalid request"}})))]
    fn messages_are_dispatched(#[case] message: Value, #[case] expected: Step) {
        assert_eq!(dispatch(&message), expected);
    }

    #[test]
    fn initialize_echoes_the_protocol_and_names_the_server() {
        let message = json!({"jsonrpc": "2.0", "id": 0, "method": "initialize",
                             "params": {"protocolVersion": "2025-03-26", "capabilities": {}}});
        let Step::Reply(reply) = dispatch(&message) else { panic!("expected a reply") };
        assert_eq!(
            (reply.pointer("/result/protocolVersion"), reply.pointer("/result/serverInfo/name"), reply.pointer("/result/capabilities")),
            (Some(&json!("2025-03-26")), Some(&json!("agent-hub")), Some(&json!({"tools": {}})))
        );
    }

    #[test]
    fn tools_list_offers_only_send_file() {
        let Step::Reply(reply) = dispatch(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"})) else {
            panic!("expected a reply")
        };
        assert_eq!(reply.pointer("/result/tools/0/name"), Some(&json!("send_file")));
        assert_eq!(reply.pointer("/result/tools/1"), None);
    }

    #[test]
    fn tokens_are_long_and_fresh() {
        let (first, second) = (new_token(), new_token());
        assert!(first.len() >= 64 && first != second);
    }
}
```

Интеграционный тест `crates/hub-agent/tests/mcp_http.rs`:

```rust
//! The hub's MCP server on a real loopback socket, driven with raw HTTP/1.1.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use hub_agent::channel::UserChannel;
use hub_agent::mcp_http::McpHttpServer;
use hub_core::domain::{
    Decision, FileDelivery, OutgoingFile, Question, QuestionsOutcome, ToolRequest,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

#[derive(Default)]
struct Recording {
    sent: Mutex<Vec<OutgoingFile>>,
}

impl UserChannel for Recording {
    fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
        Box::pin(async { Decision::Allowed })
    }
    fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        Box::pin(async { QuestionsOutcome::Answered(Vec::new()) })
    }
    fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        self.sent.lock().unwrap().push(file);
        Box::pin(async { FileDelivery::Delivered })
    }
}

struct Reply {
    status: u16,
    head: String,
    body: String,
}

fn address(server: &McpHttpServer) -> String {
    let rest = server.url().strip_prefix("http://").unwrap();
    rest.split_once('/').unwrap().0.to_owned()
}

async fn send(address: &str, request: String) -> Reply {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw)).await.unwrap().unwrap();
    let text = String::from_utf8(raw).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
    Reply { status, head: head.to_owned(), body: body.to_owned() }
}

fn request(method: &str, path: &str, token: Option<&str>, body: &str) -> String {
    let auth = token.map(|token| format!("Authorization: Bearer {token}\r\n")).unwrap_or_default();
    format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

async fn start() -> (Arc<Recording>, McpHttpServer) {
    let channel = Arc::new(Recording::default());
    let server = McpHttpServer::start(Arc::clone(&channel) as Arc<dyn UserChannel>).await.unwrap();
    (channel, server)
}

#[tokio::test]
async fn listens_on_loopback_only() {
    let (_channel, server) = start().await;
    assert!(server.url().starts_with("http://127.0.0.1:") && server.url().ends_with("/mcp"));
}

#[tokio::test]
async fn requests_without_the_session_token_are_refused() {
    let (channel, server) = start().await;
    let call = r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"send_file","arguments":{"path":"a.txt"}}}"#;
    let missing = send(&address(&server), request("POST", "/mcp", None, call)).await;
    let wrong = send(&address(&server), request("POST", "/mcp", Some("guess"), call)).await;
    assert_eq!((missing.status, wrong.status), (401, 401));
    assert!(channel.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn handshake_and_listing_work() {
    let (_channel, server) = start().await;
    let token = Some(server.token());
    let init = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{}}}"#;
    let initialized = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    let init = send(&address(&server), request("POST", "/mcp", token, init)).await;
    let notified = send(&address(&server), request("POST", "/mcp", token, initialized)).await;
    let listed = send(&address(&server), request("POST", "/mcp", token, list)).await;
    let init_body: Value = serde_json::from_str(&init.body).unwrap();
    let listed_body: Value = serde_json::from_str(&listed.body).unwrap();
    assert_eq!(
        (init.status, init_body.pointer("/result/serverInfo/name").cloned(), notified.status, notified.body.as_str(), listed_body.pointer("/result/tools/0/name").cloned()),
        (200, Some(json!("agent-hub")), 202, "", Some(json!("send_file")))
    );
    assert!(init.head.to_ascii_lowercase().contains("content-type: application/json"));
}

#[tokio::test]
async fn send_file_reaches_the_user() {
    let (channel, server) = start().await;
    let call = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"send_file","arguments":{"path":"out/a.txt","caption":"отчёт"}}}"#;
    let reply = send(&address(&server), request("POST", "/mcp", Some(server.token()), call)).await;
    let body: Value = serde_json::from_str(&reply.body).unwrap();
    assert_eq!(
        body,
        json!({"jsonrpc": "2.0", "id": 7, "result": {
            "content": [{"type": "text", "text": "Файл out/a.txt отправлен пользователю"}],
            "isError": false
        }})
    );
    assert_eq!(
        *channel.sent.lock().unwrap(),
        [OutgoingFile { path: "out/a.txt".to_owned(), caption: "отчёт".to_owned() }]
    );
}

#[tokio::test]
async fn wrong_method_path_and_size_are_rejected() {
    let (_channel, server) = start().await;
    let token = Some(server.token());
    let get = send(&address(&server), request("GET", "/mcp", token, "")).await;
    let other = send(&address(&server), request("POST", "/other", token, "{}")).await;
    let huge = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\n\
         Content-Type: application/json\r\nContent-Length: 2097152\r\nConnection: close\r\n\r\n",
        server.token()
    );
    let huge = send(&address(&server), huge).await;
    let garbage = send(&address(&server), request("POST", "/mcp", token, "не json")).await;
    assert_eq!((get.status, other.status, huge.status, garbage.status), (405, 404, 413, 400));
    assert!(get.head.to_ascii_lowercase().contains("allow: post"));
    assert_eq!(serde_json::from_str::<Value>(&garbage.body).unwrap().pointer("/error/code"), Some(&json!(-32700)));
}

#[tokio::test]
async fn dropping_the_server_stops_listening() {
    let (_channel, server) = start().await;
    let address = address(&server);
    drop(server);
    for _ in 0..200 {
        if TcpStream::connect(&address).await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the server still accepts connections after drop");
}
```

`crates/hub-agent/Cargo.toml`, `[dev-dependencies]`: `futures.workspace = true` (если интеграционный тест не видит его через `[dependencies]` — видит, добавлять не нужно).

Run: `cargo test -p hub-agent mcp`
Expected: FAIL — модуля `mcp_http` нет (ошибка компиляции).

- [ ] **Step 3: Реализация (GREEN)**

`crates/hub-agent/src/mcp_http.rs`:

```rust
//! The hub's own MCP server for one agent session, over HTTP on the loopback interface.
//!
//! It offers only `send_file`. Every request must carry the session's bearer token, so other
//! local processes and web pages cannot reach the user's chat; dropping the server stops it.

use std::convert::Infallible;
use std::fmt;
use std::io;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{ALLOW, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinSet;
use tokio_util::sync::{CancellationToken, DropGuard};
use uuid::Uuid;

use crate::channel::UserChannel;
use crate::rpc::{INVALID_PARAMS, METHOD_NOT_FOUND};
use crate::tools::{SEND_FILE, SEND_FILE_DESCRIPTION, ToolResult, deliver_file, send_file_schema};

pub const SERVER_NAME: &str = "agent-hub";
pub const MCP_PATH: &str = "/mcp";
const MAX_BODY: usize = 1024 * 1024;
const MAX_CONNECTIONS: usize = 16;
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);
// Used only if the client does not say which protocol version it speaks.
const FALLBACK_PROTOCOL: &str = "2025-06-18";
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;

pub struct McpHttpServer {
    url: String,
    token: String,
    _stop: DropGuard,
}

impl fmt::Debug for McpHttpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpHttpServer").field("url", &self.url).field("token", &"***").finish()
    }
}

impl McpHttpServer {
    pub async fn start(channel: Arc<dyn UserChannel>) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let port = listener.local_addr()?.port();
        let token = new_token();
        let stop = CancellationToken::new();
        let state = Arc::new(State { token: token.clone(), channel });
        tokio::spawn(accept(listener, state, stop.clone()));
        Ok(Self { url: format!("http://127.0.0.1:{port}{MCP_PATH}"), token, _stop: stop.drop_guard() })
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }
}

/// Two v4 UUIDs from the OS generator: 244 random bits, above the 128 the design asks for.
fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

struct State {
    token: String,
    channel: Arc<dyn UserChannel>,
}

async fn accept(listener: TcpListener, state: Arc<State>, stop: CancellationToken) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            accepted = listener.accept(), if connections.len() < MAX_CONNECTIONS => match accepted {
                Ok((stream, _peer)) => {
                    connections.spawn(serve_connection(stream, Arc::clone(&state)));
                }
                Err(error) => {
                    tracing::warn!(%error, "hub MCP server could not accept a connection");
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
            Some(done) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = done {
                    tracing::error!(%error, "hub MCP connection crashed");
                }
            }
        }
    }
    // Dropping the set aborts the connections still open, a delivery in flight included.
}

async fn serve_connection(stream: TcpStream, state: Arc<State>) {
    let service = service_fn(move |request| {
        let state = Arc::clone(&state);
        async move { Ok::<_, Infallible>(respond(request, &state).await) }
    });
    let mut builder = http1::Builder::new();
    builder.timer(TokioTimer::new()).header_read_timeout(READ_TIMEOUT);
    if let Err(error) = builder.serve_connection(TokioIo::new(stream), service).await {
        tracing::debug!(%error, "hub MCP connection ended with an error");
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admission {
    Mcp,
    Unauthorized,
    NotFound,
    MethodNotAllowed,
}

/// The token is checked before anything else, so an unauthenticated caller learns nothing.
fn admit(method: &Method, path: &str, authorization: Option<&HeaderValue>, token: &str) -> Admission {
    let expected = format!("Bearer {token}");
    let presented = authorization.map_or(&b""[..], HeaderValue::as_bytes);
    if !bool::from(presented.ct_eq(expected.as_bytes())) {
        return Admission::Unauthorized;
    }
    if path != MCP_PATH {
        return Admission::NotFound;
    }
    if *method != Method::POST {
        return Admission::MethodNotAllowed;
    }
    Admission::Mcp
}

#[derive(Debug, PartialEq)]
enum Step {
    Reply(Value),
    Accepted,
    Invalid(Value),
    SendFile { id: Value, arguments: Value },
}

fn dispatch(message: &Value) -> Step {
    let Some(object) = message.as_object() else {
        return Step::Invalid(rpc_error(Value::Null, INVALID_REQUEST, "Invalid request"));
    };
    // Notifications and the client's replies get no JSON-RPC answer.
    let (Some(id), Some(method)) =
        (object.get("id").cloned(), object.get("method").and_then(Value::as_str))
    else {
        return Step::Accepted;
    };
    match method {
        "initialize" => {
            let version = message
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(FALLBACK_PROTOCOL);
            Step::Reply(rpc_result(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
                }),
            ))
        }
        "ping" => Step::Reply(rpc_result(id, json!({}))),
        "tools/list" => Step::Reply(rpc_result(
            id,
            json!({"tools": [{
                "name": SEND_FILE,
                "description": SEND_FILE_DESCRIPTION,
                "inputSchema": send_file_schema(),
            }]}),
        )),
        "tools/call" => match message.pointer("/params/name").and_then(Value::as_str) {
            Some(SEND_FILE) => Step::SendFile {
                id,
                arguments: message.pointer("/params/arguments").cloned().unwrap_or(Value::Null),
            },
            Some(other) => {
                Step::Reply(rpc_error(id, INVALID_PARAMS, &format!("Unknown tool: {other}")))
            }
            None => Step::Reply(rpc_error(id, INVALID_PARAMS, "Unknown tool: ")),
        },
        other => Step::Reply(rpc_error(id, METHOD_NOT_FOUND, &format!("Method not found: {other}"))),
    }
}

async fn respond(request: Request<Incoming>, state: &State) -> Response<Full<Bytes>> {
    let admission = admit(
        request.method(),
        request.uri().path(),
        request.headers().get(AUTHORIZATION),
        &state.token,
    );
    match admission {
        Admission::Unauthorized => return empty(StatusCode::UNAUTHORIZED),
        Admission::NotFound => return empty(StatusCode::NOT_FOUND),
        Admission::MethodNotAllowed => {
            let mut response = empty(StatusCode::METHOD_NOT_ALLOWED);
            response.headers_mut().insert(ALLOW, HeaderValue::from_static("POST"));
            return response;
        }
        Admission::Mcp => {}
    }
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|length| length.to_str().ok())
        .and_then(|length| length.parse::<usize>().ok());
    if declared.is_some_and(|length| length > MAX_BODY) {
        return empty(StatusCode::PAYLOAD_TOO_LARGE);
    }
    let body = Limited::new(request.into_body(), MAX_BODY).collect();
    let body = match tokio::time::timeout(READ_TIMEOUT, body).await {
        Err(_) => return empty(StatusCode::REQUEST_TIMEOUT),
        Ok(Err(error)) if error.downcast_ref::<LengthLimitError>().is_some() => {
            return empty(StatusCode::PAYLOAD_TOO_LARGE);
        }
        Ok(Err(error)) => {
            tracing::debug!(%error, "hub MCP request body unreadable");
            return empty(StatusCode::BAD_REQUEST);
        }
        Ok(Ok(collected)) => collected.to_bytes(),
    };
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return json_response(StatusCode::BAD_REQUEST, &rpc_error(Value::Null, PARSE_ERROR, "Parse error"));
    };
    match dispatch(&message) {
        Step::Reply(reply) => json_response(StatusCode::OK, &reply),
        Step::Accepted => empty(StatusCode::ACCEPTED),
        Step::Invalid(reply) => json_response(StatusCode::BAD_REQUEST, &reply),
        Step::SendFile { id, arguments } => {
            let result = deliver_file(&arguments, state.channel.as_ref()).await;
            json_response(StatusCode::OK, &rpc_result(id, tool_result(result)))
        }
    }
}

fn tool_result(result: ToolResult) -> Value {
    let (text, failed) = match result {
        ToolResult::Success(text) => (text, false),
        ToolResult::Error(text) => (text, true),
    };
    json!({"content": [{"type": "text", "text": text}], "isError": failed})
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn empty(status: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::new()));
    *response.status_mut() = status;
    response
}

fn json_response(status: StatusCode, body: &Value) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body.to_string())));
    *response.status_mut() = status;
    response.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}
```


- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p hub-agent mcp`
Expected: PASS — модульные (`requests_are_admitted` ×6, `messages_are_dispatched` ×7, ещё 3) и 6 интеграционных.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked && cargo deny check`
Expected: PASS, без предупреждений; `cargo deny` не находит проблем с `httpdate`.

- [ ] **Step 6: Security / reliability checklist задачи**

Проверить по коду:
- сокет только `127.0.0.1` (тест `listens_on_loopback_only`);
- токен не пишется в лог: `rg "token" crates/hub-agent/src/mcp_http.rs` — только поле, `Debug` со `***`, сравнение;
- все ожидания ограничены: заголовки 10 с, тело 10 с и 1 МиБ, 16 соединений; `send_file` ограничен таймаутами отправки в Telegram на стороне `TelegramChannel`;
- drop обрывает приём и соединения (тест `dropping_the_server_stops_listening`).

- [ ] **Step 7: Commit**

```bash
git add -A Cargo.toml Cargo.lock crates/hub-agent
git commit -m "hub-agent: MCP-сервер хаба по HTTP на 127.0.0.1"
```

---
### Task 3: `BackendKind::Qwen`, `Usage::Qwen`, настройки Qwen, тексты

**Files:**
- Modify: `crates/hub-core/src/domain.rs:40-61` (`BackendKind`), `:259-264` (`Usage`), тесты `:304-315`
- Modify: `crates/hub-core/src/render.rs:53-67` (`format_finished`), тесты `:166-199`
- Modify: `crates/hub-core/src/settings.rs` (новые типы после `ApiKey` — `:131-153`; `Settings` `:204-213`; `Field` `:215-238`; `Draft` `:280-473`; `parse_*` `:479-567`; `SettingsFile` `:569-742`; тесты `:759-1023`)
- Modify: `crates/hub-core/src/commands.rs` (тесты `:117-164`)
- Modify: `crates/hub-telegram/src/texts.rs:39-59` (справка), тесты `:138-155`; `crates/hub-telegram/src/hub.rs:897`
- Modify: `crates/hub-telegram/src/agents.rs:37-58` (временная ветка Qwen)
- Modify: `crates/hub-app/src/connector.rs:46-65` (временная ветка Qwen), `crates/hub-app/src/gui/look.rs:58-64` и тест `:283-287`, `crates/hub-app/src/config.rs:48-51` (`Keys`)

**Interfaces:**
- Produces (`hub_core::domain`): `BackendKind::Qwen` (`name()` → `"qwen"`), `BackendKind::ALL: [BackendKind; 3] = [Claude, Codex, Qwen]`; `Usage::Qwen { tokens: Option<u64> }`.
- Produces (`hub_core::settings`):
  - `QwenApproval { Plan, Default, AutoEdit, Yolo }`, `ALL`, `wire()` → `"plan"`, `"default"`, `"auto-edit"`, `"yolo"`, `parse(&str) -> Option<Self>`.
  - `ApiEndpoint` (поля приватные): `base_url(&self) -> &str`, `key(&self) -> &ApiKey`, `is_cleartext_remote(&self) -> bool`; `Debug` ключ не показывает.
  - `QwenSettings { cli: Option<PathBuf>, model: Option<String>, approval: QwenApproval, endpoint: Option<ApiEndpoint> }`; `Settings { .., qwen: QwenSettings, .. }`.
  - `Field::Qwen(QwenField)`, `QwenField { Cli, Model, Approval, BaseUrl, ApiKey }`.
  - `QwenDraft { cli: String, model: String, approval: QwenApproval, base_url: String, api_key: String }` (`Default`: всё пусто, `approval: Default`; `Debug` прячет ключ); `Draft { .., qwen: QwenDraft, .. }`.
  - `QwenFile { cli, model, approval, base_url: Option<String> }`; `SettingsFile { .., qwen: QwenFile, .. }` (`#[serde(default)]`).
  - `Keys { token: String, api_key: String, qwen_key: String }`.

- [ ] **Step 1: Написать тесты (RED)**

`domain.rs`, в `backend_kind_parses_case_insensitively`:

```rust
    #[case("qwen", Some(BackendKind::Qwen))]
    #[case("Qwen", Some(BackendKind::Qwen))]
```

`render.rs`, в тесты:

```rust
    #[rstest]
    #[case(None, "✅ Готово")]
    #[case(Some(12_345), "✅ Готово · токенов в сессии: 12 345")]
    fn qwen_finished_line(#[case] tokens: Option<u64>, #[case] expected: &str) {
        let finished = Finished {
            session: SessionId::parse("q").unwrap(),
            usage: Usage::Qwen { tokens },
            background: 0,
        };
        assert_eq!(format_finished(&finished), expected);
    }
```

`commands.rs`, в `new_args_fall_back_to_the_default_backend`:

```rust
    #[case(&["qwen"], BackendKind::Qwen, None)]
    #[case(&["QWEN", "shop"], BackendKind::Qwen, Some("shop"))]
```

и новый тест:

```rust
    #[test]
    fn qwen_is_a_backend_to_switch_to() {
        let switched = TopicSession::fresh(BackendKind::Qwen, current().cwd);
        assert_eq!(decide_backend(&["qwen"], &current()), BackendDecision::Switch(switched));
    }
```

`settings.rs`, тесты. В `minimal_draft_uses_defaults` добавить:

```rust
        assert_eq!(settings.qwen.cli, None);
        assert_eq!(settings.qwen.model, None);
        assert_eq!(settings.qwen.approval, QwenApproval::Default);
        assert_eq!(settings.qwen.endpoint, None);
```

Новые тесты:

```rust
    fn qwen_draft() -> Draft {
        Draft {
            default_backend: BackendKind::Qwen,
            qwen: QwenDraft {
                cli: "~/bin/qwen".to_owned(),
                model: " qwen3-coder-plus ".to_owned(),
                approval: QwenApproval::AutoEdit,
                base_url: " https://dashscope-intl.aliyuncs.com/compatible-mode/v1 ".to_owned(),
                api_key: " sk-qwen ".to_owned(),
            },
            ..draft()
        }
    }

    #[test]
    fn qwen_values_are_parsed() {
        let settings = qwen_draft().parse(&home()).unwrap();
        assert_eq!(
            (
                settings.default_backend,
                settings.qwen.cli,
                settings.qwen.model.as_deref(),
                settings.qwen.approval,
                settings.qwen.endpoint.as_ref().map(|e| (e.base_url().to_owned(), e.key().expose().to_owned())),
            ),
            (
                BackendKind::Qwen,
                Some(home().join("bin/qwen")),
                Some("qwen3-coder-plus"),
                QwenApproval::AutoEdit,
                Some((
                    "https://dashscope-intl.aliyuncs.com/compatible-mode/v1".to_owned(),
                    "sk-qwen".to_owned()
                )),
            )
        );
    }

    #[rstest]
    #[case::relative_cli(QwenDraft { cli: "qwen".to_owned(), ..QwenDraft::default() }, QwenField::Cli)]
    #[case::spaced_model(QwenDraft { model: "a b".to_owned(), ..QwenDraft::default() }, QwenField::Model)]
    #[case::not_http(QwenDraft { base_url: "ftp://x".to_owned(), api_key: "k".to_owned(), ..QwenDraft::default() }, QwenField::BaseUrl)]
    #[case::bare_scheme(QwenDraft { base_url: "https://".to_owned(), api_key: "k".to_owned(), ..QwenDraft::default() }, QwenField::BaseUrl)]
    #[case::spaced_key(QwenDraft { base_url: "https://x/v1".to_owned(), api_key: "sk a".to_owned(), ..QwenDraft::default() }, QwenField::ApiKey)]
    #[case::url_without_key(QwenDraft { base_url: "https://x/v1".to_owned(), ..QwenDraft::default() }, QwenField::ApiKey)]
    #[case::key_without_url(QwenDraft { api_key: "sk-q".to_owned(), ..QwenDraft::default() }, QwenField::BaseUrl)]
    fn invalid_qwen_values_are_reported(#[case] qwen: QwenDraft, #[case] field: QwenField) {
        let errors = Draft { qwen, ..draft() }.parse(&home()).unwrap_err();
        assert_eq!(errors.iter().map(|error| error.field).collect::<Vec<_>>(), [Field::Qwen(field)]);
    }

    #[test]
    fn qwen_file_round_trips_through_draft_without_the_key() {
        let settings = qwen_draft().parse(&home()).unwrap();
        let file = SettingsFile::from_settings(&settings);
        let keys = Keys {
            token: settings.telegram.token.expose().to_owned(),
            api_key: String::new(),
            qwen_key: "sk-qwen".to_owned(),
        };
        assert_eq!(file.to_draft(keys).unwrap().parse(&home()).unwrap(), settings);
        let text = toml::to_string(&file).unwrap();
        assert!(!text.contains("sk-qwen") && text.contains("[qwen]"));
    }

    #[test]
    fn debug_output_hides_qwen_key() {
        let settings = qwen_draft().parse(&home()).unwrap();
        assert!(!format!("{settings:?}").contains("sk-qwen"));
        assert!(!format!("{:?}", qwen_draft()).contains("sk-qwen"));
    }

    #[test]
    fn qwen_approvals_have_wire_names() {
        let names: Vec<_> = QwenApproval::ALL.into_iter().map(QwenApproval::wire).collect();
        assert_eq!(names, ["plan", "default", "auto-edit", "yolo"]);
        assert_eq!(QwenApproval::parse("auto"), None);
    }

    #[rstest]
    #[case("https://api.example.com/v1", false)]
    #[case("http://localhost:11434/v1", false)]
    #[case("http://127.0.0.1/v1", false)]
    #[case("http://[::1]:8000", false)]
    #[case("http://192.168.1.5:8000/v1", true)]
    #[case("http://localhost.example.com/v1", true)]
    fn cleartext_to_another_machine_is_detected(#[case] base_url: &str, #[case] expected: bool) {
        let endpoint = ApiEndpoint { base_url: base_url.to_owned(), key: ApiKey::parse("k").unwrap() };
        assert_eq!(endpoint.is_cleartext_remote(), expected);
    }
```

В `old_file_without_agents_and_codex_reads_with_defaults` добавить `assert_eq!(draft.qwen, QwenDraft::default());`. В `file_with_unknown_choice_is_rejected` добавить случай:

```rust
    #[case(
        "[qwen]
approval = \"auto\"
",
        Field::Qwen(QwenField::Approval)
    )]
```

Все литералы `Keys { token, api_key }` в тестах (`settings.rs:876, 885, 946, 966, 1003`) дополнить `qwen_key: String::new()`.

`crates/hub-telegram/src/texts.rs`, тесты:

```rust
    #[test]
    fn unknown_backend_lists_the_known_ones() {
        assert_eq!(
            unknown_backend("gpt"),
            "⚠️ Неизвестный бэкенд gpt. Доступны: claude, codex, qwen"
        );
    }
```

в `help_names_root_uploads_and_backends`: `text.ends_with("Бэкенды: claude, codex, qwen")` и `text.contains("/backend [claude|codex|qwen] — сменить агента в этой теме (сброс контекста)")`.

`crates/hub-telegram/src/hub.rs:897`: `"⚠️ Неизвестный бэкенд gpt. Доступны: claude, codex, qwen"`.

`crates/hub-app/src/gui/look.rs`, тест `agents_have_titles`: `assert_eq!(agent_title(BackendKind::Qwen), "Qwen Code");`.

Run: `cargo test -p hub-core`
Expected: FAIL — нет `BackendKind::Qwen`, `Usage::Qwen`, `QwenDraft`, `QwenApproval`, `ApiEndpoint`, `QwenField`, поля `qwen_key`.

- [ ] **Step 2: `domain.rs` и `render.rs` (GREEN часть 1)**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendKind {
    Claude,
    Codex,
    Qwen,
}

impl BackendKind {
    pub const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::Qwen];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Qwen => "qwen",
        }
    }
    // parse — без изменений
}
```

```rust
/// What the agent reports about a finished turn: Claude counts turns and cost, Codex and Qwen tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Usage {
    Claude { turns: u32, cost: Option<Decimal> },
    Codex { tokens: Option<u64> },
    Qwen { tokens: Option<u64> },
}
```

`render.rs`, `format_finished`:

```rust
        Usage::Codex { tokens } | Usage::Qwen { tokens } => tokens.map_or_else(String::new, |tokens| {
            format!(" · токенов в сессии: {}", group_thousands(tokens))
        }),
```

- [ ] **Step 3: `settings.rs` (GREEN часть 2)**

После `impl fmt::Debug for ApiKey`:

```rust
/// When Qwen asks the human before acting; `auto` (Qwen's own classifier) is not offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QwenApproval {
    Plan,
    Default,
    AutoEdit,
    Yolo,
}

impl QwenApproval {
    pub const ALL: [Self; 4] = [Self::Plan, Self::Default, Self::AutoEdit, Self::Yolo];

    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Default => "default",
            Self::AutoEdit => "auto-edit",
            Self::Yolo => "yolo",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|approval| approval.wire() == raw)
    }
}

/// An OpenAI-compatible endpoint for Qwen; the address and the key exist only together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiEndpoint {
    base_url: String,
    key: ApiKey,
}

impl ApiEndpoint {
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub fn key(&self) -> &ApiKey {
        &self.key
    }

    /// Plain `http://` to another machine sends the key in clear text; the form warns about it.
    #[must_use]
    pub fn is_cleartext_remote(&self) -> bool {
        let Some(rest) = self.base_url.strip_prefix("http://") else {
            return false;
        };
        let authority = rest.split_once('/').map_or(rest, |(authority, _)| authority);
        !["localhost", "127.0.0.1", "[::1]"].into_iter().any(|host| {
            authority == host
                || authority.strip_prefix(host).is_some_and(|port| port.starts_with(':'))
        })
    }
}
```

После `CodexSettings`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QwenSettings {
    pub cli: Option<PathBuf>,
    pub model: Option<String>,
    pub approval: QwenApproval,
    /// None: Qwen uses its own setup (`/auth`, `~/.qwen/settings.json`, environment).
    pub endpoint: Option<ApiEndpoint>,
}
```

`Settings`: поле `pub qwen: QwenSettings,` после `codex`. `Field`: вариант `Qwen(QwenField),` после `Codex(CodexField)`, и:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QwenField {
    Cli,
    Model,
    Approval,
    BaseUrl,
    ApiKey,
}
```

После `impl fmt::Debug for CodexDraft`:

```rust
#[derive(Clone, PartialEq, Eq)]
pub struct QwenDraft {
    pub cli: String,
    pub model: String,
    pub approval: QwenApproval,
    pub base_url: String,
    pub api_key: String,
}

impl Default for QwenDraft {
    fn default() -> Self {
        Self {
            cli: String::new(),
            model: String::new(),
            approval: QwenApproval::Default,
            base_url: String::new(),
            api_key: String::new(),
        }
    }
}

impl fmt::Debug for QwenDraft {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { cli, model, approval, base_url, api_key: _api_key } = self;
        f.debug_struct("QwenDraft")
            .field("cli", cli)
            .field("model", model)
            .field("approval", approval)
            .field("base_url", base_url)
            .field("api_key", &"***")
            .finish()
    }
}
```

`Draft`: поле `pub qwen: QwenDraft,` после `codex`; в `Default` — `qwen: QwenDraft::default(),`; в `Debug` — `qwen` в деструктуризации и `.field("qwen", qwen)` после `codex`.

`Draft::parse` — после `api_key` (Codex):

```rust
        let qwen_cli =
            check(&mut errors, Field::Qwen(QwenField::Cli), parse_cli(&self.qwen.cli, home));
        let qwen_model =
            check(&mut errors, Field::Qwen(QwenField::Model), parse_model(&self.qwen.model));
        let endpoint = parse_endpoint(&self.qwen.base_url, &self.qwen.api_key)
            .map_err(|error| errors.push(error))
            .ok();
```

в кортеж `let (...) = (...) else` добавить `Some(qwen_cli), Some(qwen_model), Some(endpoint)` (после `Some(api_key)`) и соответствующие значения; в `Ok(Settings { .. })` после `codex`:

```rust
            qwen: QwenSettings {
                cli: qwen_cli,
                model: qwen_model,
                approval: self.qwen.approval,
                endpoint,
            },
```

`Draft::from_settings`, после `codex`:

```rust
            qwen: QwenDraft {
                cli: settings
                    .qwen
                    .cli
                    .as_ref()
                    .map(|cli| cli.display().to_string())
                    .unwrap_or_default(),
                model: settings.qwen.model.clone().unwrap_or_default(),
                approval: settings.qwen.approval,
                base_url: settings
                    .qwen
                    .endpoint
                    .as_ref()
                    .map(|endpoint| endpoint.base_url().to_owned())
                    .unwrap_or_default(),
                api_key: settings
                    .qwen
                    .endpoint
                    .as_ref()
                    .map(|endpoint| endpoint.key().expose().to_owned())
                    .unwrap_or_default(),
            },
```

Разборщики, после `parse_api_key`:

```rust
const BASE_URL_SHAPE: &str =
    "Ожидается адрес http:// или https://, например https://dashscope-intl.aliyuncs.com/compatible-mode/v1";

/// Empty means Qwen's own setup; anything else is an `http(s)://` address without spaces.
fn parse_base_url(raw: &str) -> Result<Option<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let rest = trimmed.strip_prefix("https://").or_else(|| trimmed.strip_prefix("http://"));
    match rest {
        Some(rest) if !rest.is_empty() && !trimmed.chars().any(char::is_whitespace) => {
            Ok(Some(trimmed.to_owned()))
        }
        Some(_) | None => Err(BASE_URL_SHAPE.to_owned()),
    }
}

fn parse_endpoint(base_url: &str, key: &str) -> Result<Option<ApiEndpoint>, FieldError> {
    let error = |field, message: String| FieldError { field: Field::Qwen(field), message };
    let base_url = parse_base_url(base_url).map_err(|message| error(QwenField::BaseUrl, message))?;
    let key = parse_api_key(key).map_err(|message| error(QwenField::ApiKey, message))?;
    match (base_url, key) {
        (Some(base_url), Some(key)) => Ok(Some(ApiEndpoint { base_url, key })),
        (None, None) => Ok(None),
        (Some(_), None) => {
            Err(error(QwenField::ApiKey, "Укажите API-ключ для этого адреса".to_owned()))
        }
        (None, Some(_)) => Err(error(QwenField::BaseUrl, "Укажите base URL для этого ключа".to_owned())),
    }
}
```

`SettingsFile`: после `codex`

```rust
    #[serde(default)]
    pub qwen: QwenFile,
```

и тип:

```rust
/// The API key is kept in the OS keyring, not here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QwenFile {
    pub cli: Option<String>,
    pub model: Option<String>,
    pub approval: Option<String>,
    pub base_url: Option<String>,
}
```

`Keys`: поле `pub qwen_key: String,`. `SettingsFile::from_settings`, после `codex`:

```rust
            qwen: QwenFile {
                cli: settings.qwen.cli.as_ref().map(|cli| cli.display().to_string()),
                model: settings.qwen.model.clone(),
                approval: Some(settings.qwen.approval.wire().to_owned()),
                base_url: settings.qwen.endpoint.as_ref().map(|endpoint| endpoint.base_url().to_owned()),
            },
```

`to_draft`: `let Keys { token, api_key, qwen_key } = keys;`, после `approval` (Codex):

```rust
        let qwen_approval = choice(
            self.qwen.approval.as_deref(),
            QwenApproval::Default,
            QwenApproval::parse,
            Field::Qwen(QwenField::Approval),
            "Неизвестный режим одобрений",
        )?;
```

и в `Draft { .. }` после `codex`:

```rust
            qwen: QwenDraft {
                cli: self.qwen.cli.clone().unwrap_or_default(),
                model: self.qwen.model.clone().unwrap_or_default(),
                approval: qwen_approval,
                base_url: self.qwen.base_url.clone().unwrap_or_default(),
                api_key: qwen_key,
            },
```

Ключ из хранилища при пустом `base_url` даёт ошибку поля «Укажите base URL для этого ключа» → `Loaded::Incomplete`: это и есть правило «ключ и URL только вместе».

- [ ] **Step 4: Тексты и временные ветки в зависимых крейтах**

`crates/hub-telegram/src/texts.rs:47`: `/backend [claude|codex|qwen] — сменить агента в этой теме (сброс контекста)\n\`.

`crates/hub-telegram/src/agents.rs`, в `match session.backend` после ветки Codex:

```rust
                BackendKind::Qwen => {
                    // Replaced by the Qwen backend once `hub-qwen` exists.
                    refuse(&conversation, "Qwen пока не подключён".to_owned(), inbox).await
                }
```

`crates/hub-app/src/connector.rs`, в `check_agent` после ветки Codex:

```rust
        // Replaced by the real check once `hub-qwen` exists.
        BackendKind::Qwen => Ok(AgentState::Unavailable("Qwen пока не подключён".to_owned())),
```

`crates/hub-app/src/gui/look.rs`, `agent_title`: `BackendKind::Qwen => "Qwen Code",`.

`crates/hub-app/src/config.rs:48-51`:

```rust
        let keys = Keys {
            token: self.secrets.read(Secret::TelegramToken)?.unwrap_or_default(),
            api_key: self.secrets.read(Secret::OpenAiKey)?.unwrap_or_default(),
            // The Qwen key joins in once it has a keyring entry.
            qwen_key: String::new(),
        };
```

`rg "Settings \{|Draft \{" crates --glob '!**/settings.rs'` — полные литералы (без `..Draft::default()`) дополнить полем `qwen`; ожидается, что таких нет.

- [ ] **Step 5: Verify GREEN**

Run: `cargo test -p hub-core && cargo test -p hub-telegram texts hub && cargo test -p hub-app look`
Expected: PASS.

- [ ] **Step 6: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A crates
git commit -m "Настройки Qwen и бэкенд qwen в командах"
```

---

### Task 4: Крейт `hub-qwen` и поиск `qwen`

**Files:**
- Modify: `Cargo.toml` (members, `[workspace.dependencies] hub-qwen`)
- Create: `crates/hub-qwen/Cargo.toml`, `crates/hub-qwen/src/lib.rs`, `crates/hub-qwen/src/version.rs`
- Test: `crates/hub-qwen/src/version.rs`

**Interfaces:**
- Produces (`hub_qwen::version`): `MIN_VERSION: Version = 0.25.0`; `CliError { Missing, Version(VersionError), Unrecognized(String), TooOld { found: Version } }`; `parse_version(&str) -> Option<Version>`; `locate(configured: Option<&Path>) -> Result<PathBuf, CliError>`; `async fn check(path: &Path) -> Result<Version, CliError>`; `pub use hub_agent::cli::Version`.

- [ ] **Step 1: Крейт**

`Cargo.toml`: в `members` после `"crates/hub-codex"` — `"crates/hub-qwen"`; в `[workspace.dependencies]` после `hub-codex` — `hub-qwen = { path = "crates/hub-qwen" }`.

`crates/hub-qwen/Cargo.toml`:

```toml
[package]
name = "hub-qwen"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
base64.workspace = true
futures.workspace = true
hub-agent.workspace = true
hub-core.workspace = true
serde_json.workspace = true
thiserror.workspace = true
tokio = { workspace = true, features = ["io-util", "macros", "process", "rt", "sync", "time"] }
tokio-util = { workspace = true, features = ["codec"] }
tracing.workspace = true

[dev-dependencies]
insta.workspace = true
rstest.workspace = true
tokio = { workspace = true, features = ["io-util", "macros", "process", "rt", "rt-multi-thread", "sync", "time"] }

[lints]
workspace = true
```

`crates/hub-qwen/src/lib.rs`:

```rust
pub mod version;
```

- [ ] **Step 2: Тесты (RED)**

`crates/hub-qwen/src/version.rs`, модуль тестов:

```rust
#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("0.25.0\n", Some(Version { major: 0, minor: 25, patch: 0 }))]
    #[case("qwen 0.26.1", Some(Version { major: 0, minor: 26, patch: 1 }))]
    #[case("", None)]
    #[case("unknown", None)]
    #[case("0.25", None)]
    fn versions_are_parsed(#[case] output: &str, #[case] expected: Option<Version>) {
        assert_eq!(parse_version(output), expected);
    }

    #[test]
    fn minimum_is_the_verified_protocol() {
        assert_eq!(MIN_VERSION.to_string(), "0.25.0");
        assert!(Version { major: 0, minor: 24, patch: 99 } < MIN_VERSION);
    }

    #[test]
    fn configured_path_is_used_as_is() {
        // npm installs `qwen.cmd` on Windows; it must be run as found.
        let path = std::env::temp_dir().join("qwen.cmd");
        assert_eq!(locate(Some(&path)).unwrap(), path);
    }

    #[tokio::test]
    async fn missing_binary_is_a_spawn_error() {
        let path = std::env::temp_dir().join("definitely-not-qwen-binary");
        assert!(matches!(check(&path).await, Err(CliError::Version(VersionError::Spawn { .. }))));
    }
}
```

Run: `cargo test -p hub-qwen version`
Expected: FAIL — `parse_version`, `MIN_VERSION`, `locate`, `check` не определены.

- [ ] **Step 3: Реализация (GREEN)**

Над тестами:

```rust
//! Which Qwen Code CLI to run and whether it speaks the ACP this hub was verified against.

use std::path::{Path, PathBuf};

pub use hub_agent::cli::Version;
use hub_agent::cli::{self, VersionError};

/// The ACP wire shapes this hub speaks were checked against qwen-code at this version.
pub const MIN_VERSION: Version = Version { major: 0, minor: 25, patch: 0 };
const CLI_NAME: &str = "qwen";

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("Qwen Code не найден: укажите путь в настройках или установите `qwen` в PATH")]
    Missing,
    #[error(transparent)]
    Version(#[from] VersionError),
    #[error("непонятный ответ `qwen --version`: {0}")]
    Unrecognized(String),
    #[error("Qwen Code {found} слишком старый, нужен {} или новее", MIN_VERSION)]
    TooOld { found: Version },
}

/// `0.25.0` (yargs prints the bare version) → 0.25.0.
#[must_use]
pub fn parse_version(output: &str) -> Option<Version> {
    output.split_whitespace().find_map(Version::parse)
}

pub fn locate(configured: Option<&Path>) -> Result<PathBuf, CliError> {
    cli::locate(CLI_NAME, configured).ok_or(CliError::Missing)
}

pub async fn check(path: &Path) -> Result<Version, CliError> {
    let text = cli::version_output(path).await?;
    let found =
        parse_version(&text).ok_or_else(|| CliError::Unrecognized(text.trim().to_owned()))?;
    if found < MIN_VERSION { Err(CliError::TooOld { found }) } else { Ok(found) }
}
```

- [ ] **Step 4: Verify GREEN и гейты**

Run: `cargo test -p hub-qwen version && cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS (если `--locked` падает из-за нового члена workspace — один раз выполнить `cargo check --workspace`, чтобы обновить `Cargo.lock`, и повторить).

- [ ] **Step 5: Commit**

```bash
git add -A Cargo.toml Cargo.lock crates/hub-qwen
git commit -m "Крейт hub-qwen: поиск и проверка версии qwen"
```

---
### Task 5: Протокол Qwen (`protocol`)

Чистые функции: параметры вызовов, разбор ответов, перевод `session/update` в события хаба, разбор `session/request_permission`, ответы на вопросы и drain. Формы — из раздела «Протокол: проверено по исходникам».

**Files:**
- Modify: `crates/hub-qwen/src/lib.rs`
- Create: `crates/hub-qwen/src/protocol.rs`
- Create: `crates/hub-qwen/src/snapshots/hub_qwen__protocol__tests__{initialize,session_new,session_load,session_prompt,session_cancel}.snap`
- Test: `crates/hub-qwen/src/protocol.rs`

**Interfaces:**
- Produces (`hub_qwen::protocol`):
  - `PROTOCOL_VERSION: u64 = 1`, `AUTH_REQUIRED: i64 = -32000`, `ALLOW_ONCE = "proceed_once"`, `REJECT = "cancel"`, `MAX_DRAIN_ITEMS: usize = 10`, `DRAIN_METHOD = "craft/drainMidTurnQueue"`, `PERMISSION_METHOD = "session/request_permission"`.
  - `Call { Initialize, SessionNew, SessionLoad, SessionPrompt, SessionCancel }`, `Call::method(self) -> &'static str`.
  - `HubTools<'a> { url: &'a str, token: &'a str }`.
  - `initialize_params(version: &str) -> Value`; `open_session(&TopicSession, &HubTools) -> (Call, Value)`; `parse_session_id(&Value) -> Result<SessionId, RpcError>`; `prompt_params(&SessionId, &Prompt) -> Value`; `cancel_params(&SessionId) -> Value`; `prompt_blocks(&Prompt) -> Value`.
  - `StopReason { EndTurn, Cancelled, Other(String) }`; `parse_stop_reason(&Value) -> Result<StopReason, RpcError>`.
  - `PermissionAsk { Tool(ToolRequest), Questions(Vec<Question>) }`; `parse_permission(&Value) -> Result<PermissionAsk, RequestError>`; `selected(option: &str) -> Value`; `answered(&[Question], &[QuestionAnswer]) -> Value`; `drain_reply(&[Prompt]) -> Value`.
  - `TurnTracker::new(SessionId)`, `translate(&mut self, &Notification) -> Vec<AgentEvent>`, `finish(&mut self, StopReason) -> Vec<AgentEvent>`.

- [ ] **Step 1: Снимки и тесты (RED)**

`crates/hub-qwen/src/lib.rs`:

```rust
pub mod protocol;
pub mod version;
```

Снимки (содержимое после заголовка `---`; insta сравнивает только содержимое, метаданные `expression` могут отличаться):

`crates/hub-qwen/src/snapshots/hub_qwen__protocol__tests__initialize.snap`:

```
---
source: crates/hub-qwen/src/protocol.rs
expression: rendered(call(name))
---
initialize
{
  "clientCapabilities": {
    "fs": {
      "readTextFile": false,
      "writeTextFile": false
    },
    "terminal": false
  },
  "clientInfo": {
    "name": "agent-hub",
    "title": "agent-hub",
    "version": "test"
  },
  "protocolVersion": 1
}
```

`…__session_new.snap`:

```
---
source: crates/hub-qwen/src/protocol.rs
expression: rendered(call(name))
---
session/new
{
  "cwd": "[cwd]",
  "mcpServers": [
    {
      "headers": [
        {
          "name": "Authorization",
          "value": "Bearer t0k3n"
        }
      ],
      "name": "agent-hub",
      "type": "http",
      "url": "http://127.0.0.1:4321/mcp"
    }
  ]
}
```

`…__session_load.snap`:

```
---
source: crates/hub-qwen/src/protocol.rs
expression: rendered(call(name))
---
session/load
{
  "cwd": "[cwd]",
  "mcpServers": [
    {
      "headers": [
        {
          "name": "Authorization",
          "value": "Bearer t0k3n"
        }
      ],
      "name": "agent-hub",
      "type": "http",
      "url": "http://127.0.0.1:4321/mcp"
    }
  ],
  "sessionId": "s-1"
}
```

`…__session_prompt.snap`:

```
---
source: crates/hub-qwen/src/protocol.rs
expression: rendered(call(name))
---
session/prompt
{
  "prompt": [
    {
      "data": "AQID",
      "mimeType": "image/png",
      "type": "image"
    },
    {
      "text": "что на фото?",
      "type": "text"
    }
  ],
  "sessionId": "s-1"
}
```

`…__session_cancel.snap`:

```
---
source: crates/hub-qwen/src/protocol.rs
expression: rendered(call(name))
---
session/cancel
{
  "sessionId": "s-1"
}
```

Тесты в конце `crates/hub-qwen/src/protocol.rs`:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use hub_core::domain::{AbsolutePath, BackendKind, Image, ImageMediaType, QuestionOption, Selection};
    use rstest::rstest;

    use super::*;

    fn cwd() -> AbsolutePath {
        AbsolutePath::new(PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" })).unwrap()
    }

    fn session() -> SessionId {
        SessionId::parse("s-1").unwrap()
    }

    fn tools() -> HubTools<'static> {
        HubTools { url: "http://127.0.0.1:4321/mcp", token: "t0k3n" }
    }

    fn text(text: &str) -> Prompt {
        Prompt::new(text.to_owned(), Vec::new()).unwrap()
    }

    fn call(name: &str) -> (Call, Value) {
        let fresh = TopicSession::fresh(BackendKind::Qwen, cwd());
        match name {
            "initialize" => (Call::Initialize, initialize_params("test")),
            "session_new" => open_session(&fresh, &tools()),
            "session_load" => open_session(&fresh.with_session(Some(session())), &tools()),
            "session_prompt" => {
                let image = Image { media: ImageMediaType::Png, data: vec![1, 2, 3] };
                let prompt = Prompt::new("что на фото?".to_owned(), vec![image]).unwrap();
                (Call::SessionPrompt, prompt_params(&session(), &prompt))
            }
            "session_cancel" => (Call::SessionCancel, cancel_params(&session())),
            other => panic!("no call named {other}"),
        }
    }

    /// The working directory differs per platform; the snapshot keeps the rest exact.
    fn rendered((call, mut params): (Call, Value)) -> String {
        if let Some(cwd) = params.get_mut("cwd") {
            *cwd = json!("[cwd]");
        }
        format!("{}\n{}", call.method(), serde_json::to_string_pretty(&params).unwrap())
    }

    #[rstest]
    #[case("initialize")]
    #[case("session_new")]
    #[case("session_load")]
    #[case("session_prompt")]
    #[case("session_cancel")]
    fn calls_match_the_verified_wire_shapes(#[case] name: &str) {
        insta::assert_snapshot!(name, rendered(call(name)));
    }

    #[test]
    fn blank_text_is_not_sent() {
        let image = Image { media: ImageMediaType::Jpeg, data: vec![0] };
        let prompt = Prompt::new("  ".to_owned(), vec![image]).unwrap();
        assert_eq!(prompt_blocks(&prompt), json!([{"type": "image", "mimeType": "image/jpeg", "data": "AA=="}]));
    }

    #[rstest]
    #[case(json!({"sessionId": "s-1", "models": {}}), Some(session()))]
    #[case(json!({"sessionId": " "}), None)]
    #[case(json!({}), None)]
    fn session_id_is_parsed(#[case] result: Value, #[case] expected: Option<SessionId>) {
        assert_eq!(parse_session_id(&result).ok(), expected);
    }

    #[rstest]
    #[case(json!({"stopReason": "end_turn"}), Some(StopReason::EndTurn))]
    #[case(json!({"stopReason": "cancelled"}), Some(StopReason::Cancelled))]
    #[case(json!({"stopReason": "max_tokens"}), Some(StopReason::Other("max_tokens".to_owned())))]
    #[case(json!({}), None)]
    fn stop_reason_is_parsed(#[case] result: Value, #[case] expected: Option<StopReason>) {
        assert_eq!(parse_stop_reason(&result).ok(), expected);
    }

    fn update(update: Value) -> Notification {
        Notification {
            method: "session/update".to_owned(),
            params: json!({"sessionId": "s-1", "update": update}),
        }
    }

    fn chunk(text: &str) -> Notification {
        update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}))
    }

    fn tool(kind: &str, title: &str) -> AgentEvent {
        AgentEvent::ToolCall(ToolUse { tool: kind.to_owned(), summary: title.to_owned() })
    }

    #[rstest]
    #[case::text_waits_for_the_turn_end(vec![chunk("Прив"), chunk("ет")], vec![])]
    #[case::tool_call_flushes_text(
        vec![chunk("Смотрю"), update(json!({"sessionUpdate": "tool_call", "toolCallId": "c-1", "status": "pending", "title": "Shell: ls -la", "kind": "execute", "rawInput": {"command": "ls -la"}}))],
        vec![AgentEvent::AssistantText("Смотрю".to_owned()), tool("execute", "Shell: ls -la")]
    )]
    #[case::kind_is_optional(
        vec![update(json!({"sessionUpdate": "tool_call", "toolCallId": "c-1", "title": "x"}))],
        vec![tool("tool", "x")]
    )]
    #[case::untitled_call_shows_its_input(
        vec![update(json!({"sessionUpdate": "tool_call", "toolCallId": "c-1", "title": " ", "kind": "read", "rawInput": {"path": "a"}}))],
        vec![tool("read", r#"{"path":"a"}"#)]
    )]
    #[case::prepared_call_waits_for_its_title(
        vec![
            update(json!({"sessionUpdate": "tool_call", "toolCallId": "c-1", "title": "Shell", "kind": "execute", "rawInput": {}, "_meta": {"phase": "preparing"}})),
            update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c-1", "status": "pending", "title": "Shell: npm test", "kind": "execute"})),
            update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c-1", "status": "completed"})),
        ],
        vec![tool("execute", "Shell: npm test")]
    )]
    #[case::discarded_preparation_is_forgotten(
        vec![
            update(json!({"sessionUpdate": "tool_call", "toolCallId": "c-1", "title": "Shell", "_meta": {"phase": "preparing"}})),
            update(json!({"sessionUpdate": "tool_call_update", "toolCallId": "c-1", "status": "failed", "_meta": {"phase": "preparing", "preparationDiscarded": true}})),
        ],
        vec![]
    )]
    #[case::subagent_text_is_not_the_answer(
        vec![update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "внутреннее"}, "_meta": {"parentToolCallId": "c-9"}})), update(json!({"sessionUpdate": "tool_call", "toolCallId": "c-1", "title": "t", "kind": "read"}))],
        vec![tool("read", "t")]
    )]
    #[case::other_updates_are_ignored(
        vec![update(json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "думаю"}})), update(json!({"sessionUpdate": "plan", "entries": []})), update(json!({"sessionUpdate": "usage_update", "used": 1, "size": 2}))],
        vec![]
    )]
    #[case::other_sessions_are_ignored(
        vec![Notification { method: "session/update".to_owned(), params: json!({"sessionId": "s-2", "update": {"sessionUpdate": "tool_call", "toolCallId": "c", "title": "t"}}) }],
        vec![]
    )]
    fn updates_become_events(#[case] incoming: Vec<Notification>, #[case] expected: Vec<AgentEvent>) {
        let mut tracker = TurnTracker::new(session());
        let events: Vec<_> = incoming.iter().flat_map(|n| tracker.translate(n)).collect();
        assert_eq!(events, expected);
    }

    fn usage(total: u64, parent: Option<&str>) -> Notification {
        let mut meta = json!({"usage": {"inputTokens": 1, "outputTokens": 1, "totalTokens": total}});
        if let Some(parent) = parent {
            meta["parentToolCallId"] = json!(parent);
        }
        update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": ""}, "_meta": meta}))
    }

    #[test]
    fn finished_turn_reports_text_then_the_last_main_token_total() {
        let mut tracker = TurnTracker::new(session());
        for notification in [chunk("Готово"), usage(41, None), usage(12_345, None), usage(7, Some("c-9")), usage(0, None)] {
            tracker.translate(&notification);
        }
        assert_eq!(
            tracker.finish(StopReason::EndTurn),
            [
                AgentEvent::AssistantText("Готово".to_owned()),
                AgentEvent::Finished(Finished {
                    session: session(),
                    usage: Usage::Qwen { tokens: Some(12_345) },
                    background: 0,
                }),
            ]
        );
    }

    #[rstest]
    #[case(StopReason::Cancelled, "Ход Qwen прерван")]
    #[case(StopReason::Other("max_tokens".to_owned()), "Ход Qwen остановлен: max_tokens")]
    fn unsuccessful_turns_fail(#[case] stop: StopReason, #[case] reason: &str) {
        assert_eq!(TurnTracker::new(session()).finish(stop), [AgentEvent::Failed(reason.to_owned())]);
    }

    fn options() -> Value {
        json!([
            {"optionId": "proceed_always_project", "name": "Always", "kind": "allow_always"},
            {"optionId": "proceed_once", "name": "Allow", "kind": "allow_once"},
            {"optionId": "cancel", "name": "Reject", "kind": "reject_once"},
        ])
    }

    fn question(text: &str, header: &str) -> Question {
        Question::new(
            text.to_owned(),
            header.to_owned(),
            vec![
                QuestionOption { label: "Postgres".to_owned(), description: "надёжно".to_owned() },
                QuestionOption { label: "SQLite".to_owned(), description: "просто".to_owned() },
            ],
            Selection::Single,
        )
        .unwrap()
    }

    fn qwen_questions() -> Value {
        json!([{"question": "Какую БД?", "header": "БД", "multiSelect": false, "options": [
            {"label": "Postgres", "description": "надёжно"}, {"label": "SQLite", "description": "просто"}
        ]}])
    }

    #[rstest]
    #[case::shell(
        json!({"sessionId": "s-1", "options": options(), "toolCall": {"toolCallId": "c-1", "status": "pending", "title": "npm test", "kind": "execute", "rawInput": {"command": "npm test"}, "_meta": {"toolName": "run_shell_command"}}}),
        Ok(PermissionAsk::Tool(ToolRequest { tool: "execute".to_owned(), summary: "npm test".to_owned() }))
    )]
    #[case::hub_send_file(
        json!({"sessionId": "s-1", "options": options(), "toolCall": {"toolCallId": "c-2", "title": "", "kind": "other", "rawInput": {"path": "a.txt"}}}),
        Ok(PermissionAsk::Tool(ToolRequest { tool: "other".to_owned(), summary: r#"{"path":"a.txt"}"#.to_owned() }))
    )]
    #[case::question(
        json!({"sessionId": "s-1", "options": [{"optionId": "proceed_once", "name": "Submit", "kind": "allow_once"}, {"optionId": "cancel", "name": "Cancel", "kind": "reject_once"}], "toolCall": {"toolCallId": "c-3", "title": "Ask user 1 question", "kind": "think", "_meta": {"toolName": "ask_user_question", "qwenInteractionKind": "user_question", "qwenQuestions": qwen_questions()}}}),
        Ok(PermissionAsk::Questions(vec![question("Какую БД?", "БД")]))
    )]
    fn permission_requests_are_parsed(#[case] params: Value, #[case] expected: Result<PermissionAsk, RequestError>) {
        assert_eq!(parse_permission(&params), expected);
    }

    #[rstest]
    #[case::no_allow_once(json!({"options": [{"optionId": "cancel"}], "toolCall": {"title": "x"}}))]
    #[case::no_tool_call(json!({"options": options()}))]
    #[case::question_without_questions(json!({"options": options(), "toolCall": {"_meta": {"qwenInteractionKind": "user_question", "qwenQuestions": []}}}))]
    #[case::question_without_text(json!({"options": options(), "toolCall": {"_meta": {"qwenInteractionKind": "user_question", "qwenQuestions": [{"header": "h", "options": []}]}}}))]
    fn malformed_permission_requests_are_rejected(#[case] params: Value) {
        assert!(matches!(parse_permission(&params), Err(RequestError::Malformed(_))));
    }

    #[test]
    fn answers_are_keyed_by_question_index() {
        let asked = [question("Какую БД?", "БД"), question("Где хостить?", "Хост")];
        let answers = [
            QuestionAnswer { question: "Где хостить?".to_owned(), answer: "дома".to_owned() },
            QuestionAnswer { question: "Какую БД?".to_owned(), answer: "Postgres, SQLite".to_owned() },
        ];
        assert_eq!(
            answered(&asked, &answers),
            json!({"outcome": {"outcome": "selected", "optionId": "proceed_once"},
                   "answers": {"0": "Postgres, SQLite", "1": "дома"}})
        );
    }

    #[test]
    fn drain_reply_carries_each_prompt_as_an_item() {
        assert_eq!(
            drain_reply(&[text("ещё"), text("и это")]),
            json!({"items": [
                {"content": [{"type": "text", "text": "ещё"}]},
                {"content": [{"type": "text", "text": "и это"}]},
            ], "hasQueuedPrompt": false})
        );
    }
}
```

Run: `cargo test -p hub-qwen protocol`
Expected: FAIL — модуль пуст (ошибки компиляции: нет `Call`, `TurnTracker` и т. д.).

- [ ] **Step 2: Реализация (GREEN)**

Над тестами в `crates/hub-qwen/src/protocol.rs`:

```rust
//! Pure mapping between Qwen Code's ACP messages and agent-hub types.

use std::collections::HashSet;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use hub_agent::mcp_http::SERVER_NAME;
use hub_agent::rpc::{Notification, RequestError, RpcError};
use hub_agent::tools::{TOOL_SUMMARY_LIMIT, parse_questions};
use hub_core::domain::{
    AgentEvent, Finished, Prompt, Question, QuestionAnswer, SessionId, ToolRequest, ToolUse,
    TopicSession, Usage,
};
use hub_core::render::truncate;
use serde_json::{Map, Value, json};

pub const PROTOCOL_VERSION: u64 = 1;
/// `RequestError.authRequired` in the ACP SDK.
pub const AUTH_REQUIRED: i64 = -32000;
pub const ALLOW_ONCE: &str = "proceed_once";
pub const REJECT: &str = "cancel";
pub const PERMISSION_METHOD: &str = "session/request_permission";
pub const DRAIN_METHOD: &str = "craft/drainMidTurnQueue";
/// Qwen takes at most this many drained items and drops the rest.
pub const MAX_DRAIN_ITEMS: usize = 10;
const GENERIC_TOOL: &str = "tool";

/// Client-to-agent methods agent-hub uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    Initialize,
    SessionNew,
    SessionLoad,
    SessionPrompt,
    SessionCancel,
}

impl Call {
    #[must_use]
    pub const fn method(self) -> &'static str {
        match self {
            Self::Initialize => "initialize",
            Self::SessionNew => "session/new",
            Self::SessionLoad => "session/load",
            Self::SessionPrompt => "session/prompt",
            Self::SessionCancel => "session/cancel",
        }
    }
}

/// Where this session's hub MCP server listens.
#[derive(Debug, Clone, Copy)]
pub struct HubTools<'a> {
    pub url: &'a str,
    pub token: &'a str,
}

#[must_use]
pub fn initialize_params(version: &str) -> Value {
    // The hub answers no file-system or terminal requests; Qwen uses its own tools.
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
        "clientInfo": {"name": "agent-hub", "title": "agent-hub", "version": version},
    })
}

fn mcp_servers(tools: &HubTools<'_>) -> Value {
    json!([{
        "type": "http",
        "name": SERVER_NAME,
        "url": tools.url,
        "headers": [{"name": "Authorization", "value": format!("Bearer {}", tools.token)}],
    }])
}

/// `session/new` for a fresh topic session, `session/load` for a saved one.
#[must_use]
pub fn open_session(session: &TopicSession, tools: &HubTools<'_>) -> (Call, Value) {
    let cwd = session.cwd.as_path().display().to_string();
    match &session.session {
        None => (Call::SessionNew, json!({"cwd": cwd, "mcpServers": mcp_servers(tools)})),
        Some(saved) => (
            Call::SessionLoad,
            json!({"sessionId": saved.as_str(), "cwd": cwd, "mcpServers": mcp_servers(tools)}),
        ),
    }
}

pub fn parse_session_id(result: &Value) -> Result<SessionId, RpcError> {
    result
        .get("sessionId")
        .and_then(Value::as_str)
        .and_then(SessionId::parse)
        .ok_or_else(|| RpcError::Protocol(format!("session/new without a session id: {result}")))
}

#[must_use]
pub fn prompt_params(session: &SessionId, prompt: &Prompt) -> Value {
    json!({"sessionId": session.as_str(), "prompt": prompt_blocks(prompt)})
}

#[must_use]
pub fn cancel_params(session: &SessionId) -> Value {
    json!({"sessionId": session.as_str()})
}

/// Each photo as an image block, then the text if there is any.
#[must_use]
pub fn prompt_blocks(prompt: &Prompt) -> Value {
    let images = prompt.images().iter().map(|image| {
        json!({"type": "image", "mimeType": image.media.mime(), "data": STANDARD.encode(&image.data)})
    });
    let text =
        (!prompt.text().trim().is_empty()).then(|| json!({"type": "text", "text": prompt.text()}));
    Value::Array(images.chain(text).collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    Cancelled,
    Other(String),
}

pub fn parse_stop_reason(result: &Value) -> Result<StopReason, RpcError> {
    match result.get("stopReason").and_then(Value::as_str) {
        Some("end_turn") => Ok(StopReason::EndTurn),
        Some("cancelled") => Ok(StopReason::Cancelled),
        Some(other) => Ok(StopReason::Other(other.to_owned())),
        None => Err(RpcError::Protocol(format!("session/prompt without a stop reason: {result}"))),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionAsk {
    Tool(ToolRequest),
    Questions(Vec<Question>),
}

/// Qwen rejects an option it did not offer, so both answers the hub gives must be on the list.
pub fn parse_permission(params: &Value) -> Result<PermissionAsk, RequestError> {
    let malformed = |what: &str| RequestError::Malformed(format!("{what}: {params}"));
    let offered: Vec<&str> = params
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options.iter().filter_map(|option| option.get("optionId").and_then(Value::as_str)).collect()
        })
        .unwrap_or_default();
    if !offered.contains(&ALLOW_ONCE) || !offered.contains(&REJECT) {
        return Err(malformed("permission request without proceed_once and cancel"));
    }
    let call = params
        .get("toolCall")
        .filter(|call| call.is_object())
        .ok_or_else(|| malformed("permission request without a tool call"))?;
    if call.pointer("/_meta/qwenInteractionKind").and_then(Value::as_str) == Some("user_question") {
        let raw = call.pointer("/_meta/qwenQuestions").cloned().unwrap_or(Value::Null);
        return parse_questions(&json!({"questions": raw}))
            .map(PermissionAsk::Questions)
            .ok_or_else(|| malformed("question without valid qwenQuestions"));
    }
    let ToolUse { tool, summary } = tool_use(call);
    Ok(PermissionAsk::Tool(ToolRequest { tool, summary }))
}

#[must_use]
pub fn selected(option: &str) -> Value {
    json!({"outcome": {"outcome": "selected", "optionId": option}})
}

/// Qwen reads answers keyed by the question's index, as a decimal string.
#[must_use]
pub fn answered(questions: &[Question], answers: &[QuestionAnswer]) -> Value {
    let by_index: Map<String, Value> = questions
        .iter()
        .enumerate()
        .filter_map(|(index, question)| {
            answers
                .iter()
                .find(|answer| answer.question == question.text())
                .map(|answer| (index.to_string(), Value::from(answer.answer.as_str())))
        })
        .collect();
    json!({"outcome": {"outcome": "selected", "optionId": ALLOW_ONCE}, "answers": by_index})
}

/// Prompts left in the inbox start the next turn, so Qwen never holds a queued one.
#[must_use]
pub fn drain_reply(prompts: &[Prompt]) -> Value {
    let items: Vec<Value> =
        prompts.iter().map(|prompt| json!({"content": prompt_blocks(prompt)})).collect();
    json!({"items": items, "hasQueuedPrompt": false})
}

fn tool_use(call: &Value) -> ToolUse {
    let tool = call
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.trim().is_empty())
        .unwrap_or(GENERIC_TOOL);
    let summary = call
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
        .map_or_else(
            || call.get("rawInput").map_or_else(|| "{}".to_owned(), Value::to_string),
            str::to_owned,
        );
    ToolUse { tool: tool.to_owned(), summary: truncate(&summary, TOOL_SUMMARY_LIMIT) }
}

fn meta_text<'a>(update: &'a Value, key: &str) -> Option<&'a str> {
    update.get("_meta").and_then(|meta| meta.get(key)).and_then(Value::as_str)
}

/// `session/update` of one session → hub events; remembers the latest token total.
pub struct TurnTracker {
    session: SessionId,
    text: String,
    tokens: Option<u64>,
    /// Calls announced before their arguments arrived; shown once their title does.
    preparing: HashSet<String>,
}

impl TurnTracker {
    #[must_use]
    pub fn new(session: SessionId) -> Self {
        Self { session, text: String::new(), tokens: None, preparing: HashSet::new() }
    }

    pub fn translate(&mut self, notification: &Notification) -> Vec<AgentEvent> {
        let params = &notification.params;
        let ours = params.get("sessionId").and_then(Value::as_str) == Some(self.session.as_str());
        let Some(update) = params.get("update").filter(|_| ours && notification.method == "session/update")
        else {
            return Vec::new();
        };
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                self.chunk(update);
                Vec::new()
            }
            Some("tool_call") => self.tool_call(update),
            Some("tool_call_update") => self.tool_call_update(update),
            Some(_) | None => Vec::new(),
        }
    }

    fn chunk(&mut self, update: &Value) {
        // A subagent's text and usage describe its own context, not the answer.
        if update.pointer("/_meta/parentToolCallId").is_some() {
            return;
        }
        // Qwen writes 0 when the provider reported nothing.
        if let Some(total) =
            update.pointer("/_meta/usage/totalTokens").and_then(Value::as_u64).filter(|total| *total > 0)
        {
            self.tokens = Some(total);
        }
        if update.pointer("/content/type").and_then(Value::as_str) == Some("text")
            && let Some(text) = update.pointer("/content/text").and_then(Value::as_str)
        {
            self.text.push_str(text);
        }
    }

    fn tool_call(&mut self, update: &Value) -> Vec<AgentEvent> {
        let Some(id) = update.get("toolCallId").and_then(Value::as_str) else {
            return Vec::new();
        };
        if meta_text(update, "phase") == Some("preparing") {
            self.preparing.insert(id.to_owned());
            return Vec::new();
        }
        self.preparing.remove(id);
        self.announce(update)
    }

    fn tool_call_update(&mut self, update: &Value) -> Vec<AgentEvent> {
        let Some(id) = update.get("toolCallId").and_then(Value::as_str) else {
            return Vec::new();
        };
        if !self.preparing.contains(id) {
            return Vec::new();
        }
        if update.pointer("/_meta/preparationDiscarded") == Some(&Value::Bool(true)) {
            self.preparing.remove(id);
            return Vec::new();
        }
        let titled =
            update.get("title").and_then(Value::as_str).is_some_and(|title| !title.trim().is_empty());
        if !titled || meta_text(update, "phase") == Some("preparing") {
            return Vec::new();
        }
        self.preparing.remove(id);
        self.announce(update)
    }

    fn announce(&mut self, call: &Value) -> Vec<AgentEvent> {
        let mut events = self.flush();
        events.push(AgentEvent::ToolCall(tool_use(call)));
        events
    }

    fn flush(&mut self) -> Vec<AgentEvent> {
        let text = std::mem::take(&mut self.text);
        if text.trim().is_empty() { Vec::new() } else { vec![AgentEvent::AssistantText(text)] }
    }

    /// The turn's remaining text, then how it ended.
    pub fn finish(&mut self, stop: StopReason) -> Vec<AgentEvent> {
        self.preparing.clear();
        let mut events = self.flush();
        events.push(match stop {
            StopReason::EndTurn => AgentEvent::Finished(Finished {
                session: self.session.clone(),
                usage: Usage::Qwen { tokens: self.tokens },
                background: 0,
            }),
            StopReason::Cancelled => AgentEvent::Failed("Ход Qwen прерван".to_owned()),
            StopReason::Other(reason) => AgentEvent::Failed(format!("Ход Qwen остановлен: {reason}")),
        });
        events
    }
}
```

Если `clippy` отметит `unwrap_or_default()` в `parse_permission` как сокрытие ошибки — замены не нужно: отсутствующий список вариантов сразу превращается в `Malformed` строкой ниже.

- [ ] **Step 3: Verify GREEN**

Run: `cargo test -p hub-qwen protocol`
Expected: PASS; `ls crates/hub-qwen/src/snapshots` — ровно 5 файлов `.snap`, ни одного `.snap.new`. Если insta сообщит о расхождении содержимого — сверить `.snap.new` со снимками выше; правится код, а не снимок (снимки записаны по исходникам Qwen).

- [ ] **Step 4: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A crates/hub-qwen
git commit -m "hub-qwen: перевод сообщений ACP"
```

---
### Task 6: Цикл ходов и досылка (`session`)

**Files:**
- Modify: `crates/hub-qwen/src/lib.rs`, `crates/hub-qwen/Cargo.toml` (`[dev-dependencies] tokio` + `"test-util"`)
- Create: `crates/hub-qwen/src/session.rs`, `crates/hub-qwen/src/testing.rs`
- Test: `crates/hub-qwen/src/session.rs`

**Interfaces:**
- Produces (`hub_qwen::session`):
  - `trait Agent: Send + Sync { fn prompt<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<StopReason, RpcError>>; fn cancel(&self) -> BoxFuture<'_, Result<(), RpcError>>; }`.
  - `RpcAgent::new(client: RpcClient, session: SessionId)` — `session/prompt` через `request_untimed`, `session/cancel` через `notify_with`.
  - `Drain` (`Clone`), `DrainRequests`, `drain_channel() -> (Drain, DrainRequests)`, `Drain::take(&self) -> Vec<Prompt>` (не дольше `DRAIN_DEADLINE = 1500 мс`).
  - `Turns<'a, A> { agent: &'a A, notifications: &'a mut UnboundedReceiver<Notification>, drains: &'a mut DrainRequests }`.
  - `async fn converse<A: Agent>(turns: Turns<'_, A>, tracker: TurnTracker, prompt: Prompt, inbox: mpsc::Receiver<Prompt>, conversation: &Conversation) -> (mpsc::Receiver<Prompt>, Result<(), RpcError>)`.
- Produces (`#[cfg(test)] crate::testing`): `Peer`, `pair(handler, timeout) -> (Connection, Peer)` (конверт `JsonRpc2`), `refusing()`, `Scripted` — как в `hub-codex/src/testing.rs`.

Правила цикла (спецификация, «Сессия» п. 4–5):
- ход — `session/prompt`; пока он идёт, цикл с приоритетом `/stop` → запрос drain → уведомление → ответ хода;
- drain: сначала промпты, удержанные после опоздавшего drain, затем `try_recv` из inbox, всего не больше 10; если обработчик уже не ждёт (`oneshot` закрыт), промпты возвращаются в начало удержанной очереди;
- после ответа хода: события `finish`; если `/stop` уже нажат — выход, inbox не трогается; иначе следующий ход берёт удержанный промпт или `try_recv` из inbox; пусто — разговор окончен;
- `/stop` → `session/cancel` (не дольше 2 с), выход, inbox возвращается; промпт, взятый из inbox, всегда отправляется ходом или drain'ом;
- закрытый поток уведомлений не обрывает ход: ход сам закончится ответом или `Closed`.

- [ ] **Step 1: Тестовые помощники**

`crates/hub-qwen/Cargo.toml`, `[dev-dependencies]`: в фичи `tokio` добавить `"test-util"`.

`crates/hub-qwen/src/lib.rs`:

```rust
pub mod protocol;
pub mod session;
#[cfg(test)]
mod testing;
pub mod version;
```

`crates/hub-qwen/src/testing.rs` — копия `crates/hub-codex/src/testing.rs` (после задачи 1) с правками: заголовок `//! The Qwen side of a duplex pipe, and a scripted human, for tests.`; `pair`:

```rust
pub fn pair(handler: Handler, timeout: Duration) -> (Connection, Peer) {
    let (ours, theirs) = tokio::io::duplex(1 << 20);
    let (our_read, our_write) = tokio::io::split(ours);
    let (their_read, their_write) = tokio::io::split(theirs);
    let wire = Wire { peer: "qwen", envelope: Envelope::JsonRpc2, timeout };
    let connection = connect(our_read, our_write, handler, wire);
    (connection, Peer { lines: BufReader::new(their_read).lines(), writer: their_write })
}
```

`Peer`, `refusing`, `Scripted` — без изменений.

- [ ] **Step 2: Тесты (RED)**

`crates/hub-qwen/src/session.rs`, модуль тестов:

```rust
#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use hub_agent::conversation::Limits;
    use hub_core::domain::{Finished, Usage};
    use serde_json::json;
    use tokio::task::JoinHandle;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::testing::Scripted;

    type Ends = mpsc::UnboundedSender<Result<StopReason, RpcError>>;

    struct FakeAgent {
        calls: Mutex<Vec<String>>,
        ends: tokio::sync::Mutex<mpsc::UnboundedReceiver<Result<StopReason, RpcError>>>,
    }

    impl FakeAgent {
        fn new() -> (Self, Ends) {
            let (ends, ended) = mpsc::unbounded_channel();
            (Self { calls: Mutex::new(Vec::new()), ends: tokio::sync::Mutex::new(ended) }, ends)
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl Agent for FakeAgent {
        fn prompt<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<StopReason, RpcError>> {
            self.calls.lock().unwrap().push(format!("prompt:{}", prompt.text()));
            Box::pin(async move { self.ends.lock().await.recv().await.unwrap_or(Err(RpcError::Closed)) })
        }

        fn cancel(&self) -> BoxFuture<'_, Result<(), RpcError>> {
            self.calls.lock().unwrap().push("cancel".to_owned());
            Box::pin(async { Ok(()) })
        }
    }

    struct Harness {
        agent: Arc<FakeAgent>,
        ends: Ends,
        notify: mpsc::UnboundedSender<Notification>,
        inbox: mpsc::Sender<Prompt>,
        drain: Drain,
        cancel: CancellationToken,
        events: mpsc::Receiver<AgentEvent>,
        done: JoinHandle<(mpsc::Receiver<Prompt>, Result<(), RpcError>)>,
    }

    fn prompt(text: &str) -> Prompt {
        Prompt::new(text.to_owned(), Vec::new()).unwrap()
    }

    fn session_id() -> SessionId {
        SessionId::parse("s-1").unwrap()
    }

    fn start() -> Harness {
        let (agent, ends) = FakeAgent::new();
        let agent = Arc::new(agent);
        let (notify, mut notifications) = mpsc::unbounded_channel();
        let (inbox, inbox_out) = mpsc::channel(32);
        let (events_in, events) = mpsc::channel(64);
        let (drain, mut drains) = drain_channel();
        let cancel = CancellationToken::new();
        let conversation = Conversation {
            channel: Arc::new(Scripted::default()),
            events: events_in,
            cancel: cancel.clone(),
            limits: Limits::new(Duration::from_secs(1)),
        };
        let done = tokio::spawn({
            let agent = Arc::clone(&agent);
            async move {
                let turns =
                    Turns { agent: agent.as_ref(), notifications: &mut notifications, drains: &mut drains };
                converse(turns, TurnTracker::new(session_id()), prompt("hi"), inbox_out, &conversation).await
            }
        });
        Harness { agent, ends, notify, inbox, drain, cancel, events, done }
    }

    fn said(text: &str) -> Notification {
        Notification {
            method: "session/update".to_owned(),
            params: json!({"sessionId": "s-1", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}}),
        }
    }

    async fn eventually(what: &str, check: impl Fn() -> bool) {
        for _ in 0..400 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for: {what}");
    }

    fn drained(events: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        std::iter::from_fn(|| events.try_recv().ok()).collect()
    }

    fn texts(prompts: &[Prompt]) -> Vec<String> {
        prompts.iter().map(|prompt| prompt.text().to_owned()).collect()
    }

    #[tokio::test]
    async fn a_turn_relays_text_and_finishes() {
        let mut harness = start();
        harness.notify.send(said("При")).unwrap();
        harness.notify.send(said("вет")).unwrap();
        eventually("started", || harness.agent.calls().len() == 1).await;
        harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        let finished = AgentEvent::Finished(Finished {
            session: session_id(),
            usage: Usage::Qwen { tokens: None },
            background: 0,
        });
        assert_eq!(drained(&mut harness.events), [AgentEvent::AssistantText("Привет".to_owned()), finished]);
    }

    #[tokio::test]
    async fn a_prompt_during_a_turn_goes_through_the_drain() {
        let harness = start();
        eventually("started", || harness.agent.calls().len() == 1).await;
        harness.inbox.send(prompt("ещё")).await.unwrap();
        assert_eq!(texts(&harness.drain.take().await), ["ещё"]);
        harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!((outcome, harness.agent.calls()), (Ok(()), vec!["prompt:hi".to_owned()]));
    }

    #[tokio::test]
    async fn a_prompt_after_the_last_drain_starts_the_next_turn() {
        let harness = start();
        eventually("started", || harness.agent.calls().len() == 1).await;
        harness.inbox.send(prompt("потом")).await.unwrap();
        harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
        eventually("next turn", || harness.agent.calls().len() == 2).await;
        harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(harness.agent.calls(), ["prompt:hi", "prompt:потом"]);
    }

    #[tokio::test]
    async fn a_drain_hands_over_at_most_ten_prompts() {
        let harness = start();
        eventually("started", || harness.agent.calls().len() == 1).await;
        for n in 1..=12 {
            harness.inbox.send(prompt(&n.to_string())).await.unwrap();
        }
        assert_eq!(texts(&harness.drain.take().await), ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"]);
        for turns in 2..=3 {
            harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
            eventually("next turn", || harness.agent.calls().len() == turns).await;
        }
        harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(harness.agent.calls(), ["prompt:hi", "prompt:11", "prompt:12"]);
    }

    #[test]
    fn prompts_for_a_drain_that_gave_up_are_kept() {
        let (reply, handed) = oneshot::channel();
        drop(handed);
        let (sender, mut inbox) = mpsc::channel(4);
        sender.try_send(prompt("a")).unwrap();
        let mut held = VecDeque::from([prompt("раньше")]);
        hand_over(reply, &mut inbox, &mut held);
        assert_eq!(held.iter().map(Prompt::text).collect::<Vec<_>>(), ["раньше", "a"]);
    }

    #[tokio::test]
    async fn a_drain_without_a_conversation_is_empty() {
        let (drain, requests) = drain_channel();
        drop(requests);
        assert!(drain.take().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn an_unanswered_drain_gives_up_before_qwen_does() {
        let (drain, _requests) = drain_channel();
        let started = tokio::time::Instant::now();
        assert!(drain.take().await.is_empty());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn stop_cancels_the_turn_and_keeps_the_inbox() {
        let harness = start();
        eventually("started", || harness.agent.calls().len() == 1).await;
        harness.cancel.cancel();
        let (mut inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        assert_eq!(harness.agent.calls(), ["prompt:hi", "cancel"]);
        harness.inbox.send(prompt("после стопа")).await.unwrap();
        assert_eq!(inbox.recv().await.map(|p| p.text().to_owned()), Some("после стопа".to_owned()));
    }

    #[tokio::test]
    async fn stop_wins_over_a_prompt_that_arrived_at_the_same_moment() {
        for _ in 0..20 {
            let harness = start();
            eventually("started", || harness.agent.calls().len() == 1).await;
            harness.inbox.send(prompt("одновременно")).await.unwrap();
            harness.cancel.cancel();
            let (mut inbox, outcome) = harness.done.await.unwrap();
            assert_eq!(outcome, Ok(()));
            assert!(!harness.agent.calls().contains(&"prompt:одновременно".to_owned()));
            assert_eq!(inbox.recv().await.map(|p| p.text().to_owned()), Some("одновременно".to_owned()));
        }
    }

    #[tokio::test]
    async fn closed_updates_do_not_end_a_turn_that_still_answers() {
        let harness = start();
        eventually("started", || harness.agent.calls().len() == 1).await;
        drop(harness.notify);
        harness.ends.send(Ok(StopReason::EndTurn)).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
    }

    #[tokio::test]
    async fn a_failed_prompt_ends_the_conversation_with_its_error() {
        let harness = start();
        let error = RpcError::Remote { code: -32603, message: "Internal error: quota".to_owned() };
        harness.ends.send(Err(error.clone())).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Err(error));
    }
}
```

Run: `cargo test -p hub-qwen session`
Expected: FAIL — нет `Agent`, `Drain`, `converse`, `hand_over`.

- [ ] **Step 3: Реализация (GREEN)**

Над тестами:

```rust
//! One Qwen conversation: turns, prompts handed over mid-turn, and the end of the conversation.
//!
//! During a turn Qwen asks for queued prompts with `craft/drainMidTurnQueue`. That request is
//! answered in another task, so it asks this loop through `Drain`, and the loop answers at once
//! from the inbox. Prompts that arrive after the last drain start the next turn.

use std::collections::VecDeque;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_agent::conversation::Conversation;
use hub_agent::rpc::{Notification, RpcClient, RpcError};
use hub_core::domain::{AgentEvent, Prompt, SessionId};
use tokio::sync::{mpsc, oneshot};

use crate::protocol::{
    Call, MAX_DRAIN_ITEMS, StopReason, TurnTracker, cancel_params, parse_stop_reason,
    prompt_params,
};

/// Qwen waits 2 s for a drain answer and stops asking after three misses in a row.
pub const DRAIN_DEADLINE: Duration = Duration::from_millis(1500);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_REQUESTS: usize = 4;

/// The part of an ACP session a conversation needs.
pub trait Agent: Send + Sync {
    fn prompt<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<StopReason, RpcError>>;
    fn cancel(&self) -> BoxFuture<'_, Result<(), RpcError>>;
}

pub struct RpcAgent {
    client: RpcClient,
    session: SessionId,
}

impl RpcAgent {
    #[must_use]
    pub fn new(client: RpcClient, session: SessionId) -> Self {
        Self { client, session }
    }
}

impl Agent for RpcAgent {
    fn prompt<'a>(&'a self, prompt: &'a Prompt) -> BoxFuture<'a, Result<StopReason, RpcError>> {
        Box::pin(async move {
            let params = prompt_params(&self.session, prompt);
            // A turn lasts as long as Qwen works; `/stop` and the process exit end it.
            let result = self.client.request_untimed(Call::SessionPrompt.method(), params).await?;
            parse_stop_reason(&result)
        })
    }

    fn cancel(&self) -> BoxFuture<'_, Result<(), RpcError>> {
        Box::pin(async move {
            self.client.notify_with(Call::SessionCancel.method(), cancel_params(&self.session)).await
        })
    }
}

type Handover = oneshot::Sender<Vec<Prompt>>;

/// The request handler's way to ask the conversation loop for the prompts waiting in the inbox.
#[derive(Clone)]
pub struct Drain {
    requests: mpsc::Sender<Handover>,
}

pub struct DrainRequests(mpsc::Receiver<Handover>);

#[must_use]
pub fn drain_channel() -> (Drain, DrainRequests) {
    let (requests, received) = mpsc::channel(DRAIN_REQUESTS);
    (Drain { requests }, DrainRequests(received))
}

impl Drain {
    /// The prompts the conversation hands over; none if it is gone or did not answer in time.
    pub async fn take(&self) -> Vec<Prompt> {
        let (reply, handed) = oneshot::channel();
        let asked = async {
            self.requests.send(reply).await.ok()?;
            handed.await.ok()
        };
        match tokio::time::timeout(DRAIN_DEADLINE, asked).await {
            Ok(Some(prompts)) => prompts,
            // The conversation ended; nothing is waiting for this turn any more.
            Ok(None) => Vec::new(),
            Err(_) => {
                tracing::warn!("qwen drain not answered in time; prompts wait for the next turn");
                Vec::new()
            }
        }
    }
}

pub struct Turns<'a, A> {
    pub agent: &'a A,
    pub notifications: &'a mut mpsc::UnboundedReceiver<Notification>,
    pub drains: &'a mut DrainRequests,
}

/// Runs turns until none is running and nothing waits, or until /stop.
///
/// Returns the inbox, so prompts that arrive while the conversation closes are not lost.
pub async fn converse<A: Agent>(
    turns: Turns<'_, A>,
    mut tracker: TurnTracker,
    prompt: Prompt,
    mut inbox: mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> (mpsc::Receiver<Prompt>, Result<(), RpcError>) {
    let outcome = relay(turns, &mut tracker, prompt, &mut inbox, conversation).await;
    (inbox, outcome)
}

async fn relay<A: Agent>(
    turns: Turns<'_, A>,
    tracker: &mut TurnTracker,
    prompt: Prompt,
    inbox: &mut mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> Result<(), RpcError> {
    let Turns { agent, notifications, drains } = turns;
    // Prompts taken for a drain whose handler had already given up; they go first.
    let mut held = VecDeque::new();
    let mut next = Some(prompt);
    while let Some(prompt) = next.take() {
        let turn = agent.prompt(&prompt);
        tokio::pin!(turn);
        let mut updates_open = true;
        let stop = loop {
            tokio::select! {
                // A stop wins over anything that arrived at the same moment, which then stays
                // in the inbox; every update Qwen sent before its reply is read before the reply.
                biased;
                () = conversation.cancel.cancelled() => {
                    cancel(agent).await;
                    return Ok(());
                }
                Some(handover) = drains.0.recv() => hand_over(handover, inbox, &mut held),
                update = notifications.recv(), if updates_open => match update {
                    Some(update) => {
                        for event in tracker.translate(&update) {
                            emit(conversation, event).await;
                        }
                    }
                    // The reader is gone; the turn itself ends with `Closed`.
                    None => updates_open = false,
                },
                stop = &mut turn => break stop?,
            }
        };
        for event in tracker.finish(stop) {
            emit(conversation, event).await;
        }
        if conversation.cancel.is_cancelled() {
            return Ok(());
        }
        next = held.pop_front().or_else(|| inbox.try_recv().ok());
    }
    Ok(())
}

/// Answers a drain from the held prompts, then the inbox, up to what Qwen accepts at once.
fn hand_over(
    handover: Handover,
    inbox: &mut mpsc::Receiver<Prompt>,
    held: &mut VecDeque<Prompt>,
) {
    let from_held = held.len().min(MAX_DRAIN_ITEMS);
    let room = MAX_DRAIN_ITEMS.saturating_sub(from_held);
    let taken: Vec<Prompt> = held
        .drain(..from_held)
        .chain(std::iter::from_fn(|| inbox.try_recv().ok()).take(room))
        .collect();
    if let Err(late) = handover.send(taken) {
        tracing::warn!(prompts = late.len(), "qwen drain handler gave up; prompts start the next turn");
        for prompt in late.into_iter().rev() {
            held.push_front(prompt);
        }
    }
}

async fn cancel<A: Agent>(agent: &A) {
    match tokio::time::timeout(CANCEL_TIMEOUT, agent.cancel()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::warn!(%error, "qwen did not take session/cancel"),
        Err(_) => tracing::warn!("qwen input stalled on session/cancel"),
    }
}

async fn emit(conversation: &Conversation, event: AgentEvent) {
    // Nobody listening means the topic is gone; there is no one left to tell.
    let _ = conversation.events.send(event).await;
}
```

Если borrow checker не даёт взять `inbox` и `held` внутри `select!` вместе с закреплённым `turn` (он держит `&prompt` и `agent`), — поля не пересекаются; при ошибке вынести тело ветки в функцию, принимающую `&mut` явно.

- [ ] **Step 4: Verify GREEN**

Run: `cargo test -p hub-qwen session`
Expected: PASS (11 тестов).

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A crates/hub-qwen
git commit -m "hub-qwen: цикл ходов и досылка сообщений через drain"
```

---
### Task 7: Запросы агента (`requests`)

**Files:**
- Modify: `crates/hub-qwen/src/lib.rs` (`pub mod requests;`)
- Create: `crates/hub-qwen/src/requests.rs`
- Test: `crates/hub-qwen/src/requests.rs` (путь drain с промптами — в задаче 8, через настоящий цикл)

**Interfaces:**
- Produces: `hub_qwen::requests::answer(channel: &dyn UserChannel, drain: &Drain, method: &str, params: Value) -> Result<Value, RequestError>`.
- Consumes: `protocol::{parse_permission, PermissionAsk, selected, answered, drain_reply, ALLOW_ONCE, REJECT, PERMISSION_METHOD, DRAIN_METHOD}`, `session::Drain`.

| Запрос | Ответ |
|---|---|
| `session/request_permission`, инструмент | `ToolRequest(kind ∨ "tool", title)` → разрешено: `{"outcome":{"outcome":"selected","optionId":"proceed_once"}}`, отклонено: `optionId: "cancel"` |
| то же, `qwenInteractionKind = user_question` | `channel.ask` → `answered(...)` с `answers` по индексам; отказ → `optionId: "cancel"` |
| неверная форма | `RequestError::Malformed` → `-32602` |
| `craft/drainMidTurnQueue` | `drain_reply(drain.take())` |
| другое (`fs/*`, `terminal/*`, …) | `RequestError::Unsupported` → `-32601` |

- [ ] **Step 1: Тесты (RED)**

`crates/hub-qwen/src/lib.rs`: добавить `pub mod requests;` (по алфавиту после `protocol`).

`crates/hub-qwen/src/requests.rs`, тесты:

```rust
#[cfg(test)]
mod tests {
    use hub_core::domain::{Decision, Denied, QuestionAnswer, QuestionsOutcome, ToolRequest};
    use rstest::rstest;
    use serde_json::json;

    use super::*;
    use crate::session::drain_channel;
    use crate::testing::Scripted;

    fn offered() -> Value {
        json!([
            {"optionId": "proceed_always_project", "name": "Always", "kind": "allow_always"},
            {"optionId": "proceed_once", "name": "Allow", "kind": "allow_once"},
            {"optionId": "cancel", "name": "Reject", "kind": "reject_once"},
        ])
    }

    fn shell() -> Value {
        json!({"sessionId": "s-1", "options": offered(), "toolCall": {
            "toolCallId": "c-1", "status": "pending", "title": "rm -rf build", "kind": "execute",
            "rawInput": {"command": "rm -rf build"}}})
    }

    fn question() -> Value {
        json!({"sessionId": "s-1", "options": offered(), "toolCall": {
            "toolCallId": "c-2", "title": "Ask user 1 question", "kind": "think",
            "_meta": {"qwenInteractionKind": "user_question", "qwenQuestions": [
                {"question": "Цвет?", "header": "Цвет", "options": [
                    {"label": "синий", "description": "спокойный"}, {"label": "красный", "description": "яркий"}]}]}}})
    }

    async fn answer_with(channel: &Scripted, method: &str, params: Value) -> Result<Value, RequestError> {
        let (drain, _requests) = drain_channel();
        answer(channel, &drain, method, params).await
    }

    #[rstest]
    #[case::allowed(Decision::Allowed, "proceed_once")]
    #[case::denied(Decision::Denied(Denied::new("нет")), "cancel")]
    #[tokio::test]
    async fn tool_approvals_ask_the_human(#[case] decision: Decision, #[case] option: &str) {
        let channel = Scripted { decision, ..Scripted::default() };
        let reply = answer_with(&channel, "session/request_permission", shell()).await;
        assert_eq!(reply, Ok(json!({"outcome": {"outcome": "selected", "optionId": option}})));
        assert_eq!(
            *channel.requests.lock().unwrap(),
            [ToolRequest { tool: "execute".to_owned(), summary: "rm -rf build".to_owned() }]
        );
    }

    #[tokio::test]
    async fn questions_are_answered_by_index() {
        let channel = Scripted {
            outcome: QuestionsOutcome::Answered(vec![QuestionAnswer {
                question: "Цвет?".to_owned(),
                answer: "синий".to_owned(),
            }]),
            ..Scripted::default()
        };
        let reply = answer_with(&channel, "session/request_permission", question()).await;
        assert_eq!(
            reply,
            Ok(json!({"outcome": {"outcome": "selected", "optionId": "proceed_once"}, "answers": {"0": "синий"}}))
        );
        assert_eq!(channel.questions.lock().unwrap().first().map(|q| q.options().len()), Some(2));
    }

    #[tokio::test]
    async fn declined_questions_cancel() {
        let channel = Scripted {
            outcome: QuestionsOutcome::Denied(Denied::new("нет ответа")),
            ..Scripted::default()
        };
        let reply = answer_with(&channel, "session/request_permission", question()).await;
        assert_eq!(reply, Ok(json!({"outcome": {"outcome": "selected", "optionId": "cancel"}})));
    }

    #[tokio::test]
    async fn malformed_permission_is_rejected_without_asking() {
        let channel = Scripted::default();
        let reply = answer_with(&channel, "session/request_permission", json!({"options": []})).await;
        assert!(matches!(reply, Err(RequestError::Malformed(_))));
        assert!(channel.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn drain_without_a_running_turn_hands_over_nothing() {
        let (drain, requests) = drain_channel();
        drop(requests);
        let reply = answer(&Scripted::default(), &drain, "craft/drainMidTurnQueue", json!({"sessionId": "s-1"})).await;
        assert_eq!(reply, Ok(json!({"items": [], "hasQueuedPrompt": false})));
    }

    #[tokio::test]
    async fn unknown_request_is_unsupported() {
        let reply = answer_with(&Scripted::default(), "fs/read_text_file", json!({})).await;
        assert_eq!(reply, Err(RequestError::Unsupported("fs/read_text_file".to_owned())));
    }
}
```

Run: `cargo test -p hub-qwen requests`
Expected: FAIL — нет `answer`.

- [ ] **Step 2: Реализация (GREEN)**

```rust
//! Requests Qwen sends the client: approvals, its own clarifying questions, and the drain.

use hub_agent::channel::UserChannel;
use hub_agent::rpc::RequestError;
use hub_core::domain::{Decision, Question, QuestionsOutcome, ToolRequest};
use serde_json::Value;

use crate::protocol::{
    ALLOW_ONCE, DRAIN_METHOD, PERMISSION_METHOD, PermissionAsk, REJECT, answered, drain_reply,
    parse_permission, selected,
};
use crate::session::Drain;

/// Replies to one Qwen request on behalf of the human behind `channel`.
pub async fn answer(
    channel: &dyn UserChannel,
    drain: &Drain,
    method: &str,
    params: Value,
) -> Result<Value, RequestError> {
    match method {
        PERMISSION_METHOD => Ok(match parse_permission(&params)? {
            PermissionAsk::Tool(tool) => approve(channel, tool).await,
            PermissionAsk::Questions(questions) => ask(channel, questions).await,
        }),
        DRAIN_METHOD => Ok(drain_reply(&drain.take().await)),
        other => Err(RequestError::Unsupported(other.to_owned())),
    }
}

/// «Allow always» is never chosen: every call goes to the human again.
async fn approve(channel: &dyn UserChannel, tool: ToolRequest) -> Value {
    match channel.request(tool).await {
        Decision::Allowed => selected(ALLOW_ONCE),
        Decision::Denied(_) => selected(REJECT),
    }
}

async fn ask(channel: &dyn UserChannel, questions: Vec<Question>) -> Value {
    match channel.ask(questions.clone()).await {
        QuestionsOutcome::Answered(answers) => answered(&questions, &answers),
        // Qwen tells the model the user declined to answer.
        QuestionsOutcome::Denied(_) => selected(REJECT),
    }
}
```

- [ ] **Step 3: Verify GREEN и гейты**

Run: `cargo test -p hub-qwen requests && cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add -A crates/hub-qwen
git commit -m "hub-qwen: одобрения, вопросы и drain от Qwen"
```

---
### Task 8: Бэкенд — процесс `qwen`, сервер инструментов, сессия (`backend`)

**Files:**
- Modify: `crates/hub-qwen/src/lib.rs` (`pub mod backend;`)
- Create: `crates/hub-qwen/src/backend.rs`
- Test: `crates/hub-qwen/src/backend.rs` (поддельный ACP-агент на `duplex`)

**Interfaces:**
- Produces (`hub_qwen::backend`):
  - `QwenBackend::new(cli: PathBuf, settings: QwenSettings)`, `async fn run(&self, session: &TopicSession, prompt: Prompt, inbox: mpsc::Receiver<Prompt>, conversation: Conversation) -> mpsc::Receiver<Prompt>` (как `CodexBackend::run`).
  - `pub const NOT_AUTHENTICATED: &str = "Qwen не авторизован: настройте qwen (/auth) или задайте API-ключ в настройках"`, `pub const QWEN_FAILED: &str = "Qwen Code завершился, подробности в логе"`.
  - `QwenAuth { HubKey, OwnSetup }`, `QwenAuth::of(&QwenSettings) -> QwenAuth`.
  - `Link<'a> { client: &'a RpcClient, notifications: &'a mut UnboundedReceiver<Notification>, drains: &'a mut DrainRequests }`; `async fn serve(link, tools: &HubTools<'_>, session, prompt, inbox, conversation: &Conversation) -> mpsc::Receiver<Prompt>`.

Порядок жизни (спецификация, «Процесс» и «Сессия»):
1. `McpHttpServer::start(channel)`; ошибка → лог `error` + `Failed(QWEN_FAILED)`.
2. `qwen --acp --approval-mode <режим> [-m <модель>] [--auth-type openai]`; окружение: `QWEN_CODE_NO_RELAUNCH=true`, при `endpoint` — `OPENAI_API_KEY`, `OPENAI_BASE_URL`, `OPENAI_MODEL` (если модель задана); без `endpoint` окружение не трогается. `stdin/stdout/stderr` — трубы, `kill_on_drop(true)`, `hide_window`; stderr построчно (до 64 КиБ строка) в лог `info`. Ошибка запуска → лог `error` + `Failed(QWEN_FAILED)`.
3. `initialize`, затем `session/new` или `session/load`, гонка с `/stop`; уведомления, пришедшие до ответа, отбрасываются; `SessionStarted(id)`.
4. `converse`; `RpcError::Remote` → `Failed("Qwen: <message>")`, иначе `Failed(QWEN_FAILED)` и лог.
5. Остановка: `client.close()` (EOF на stdin), ожидание до 10 с, `kill`; затем `reader.abort()`; MCP-сервер гасится **после** выхода процесса.

- [ ] **Step 1: Тесты (RED)**

`crates/hub-qwen/src/lib.rs`:

```rust
pub mod backend;
pub mod protocol;
pub mod requests;
pub mod session;
#[cfg(test)]
mod testing;
pub mod version;
```

`crates/hub-qwen/src/backend.rs`, тесты:

```rust
#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::Duration;

    use hub_agent::conversation::Limits;
    use hub_core::domain::{AbsolutePath, BackendKind, Finished, Usage};
    use hub_core::settings::{Draft, QwenApproval, QwenDraft};
    use rstest::rstest;
    use serde_json::{Value, json};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::testing::{Peer, Scripted, pair};

    enum Reply {
        Result(Value),
        Error(i64, &'static str),
    }

    struct Step {
        method: &'static str,
        before: Vec<Value>,
        reply: Reply,
    }

    fn ok(method: &'static str, result: Value) -> Step {
        Step { method, before: Vec::new(), reply: Reply::Result(result) }
    }

    fn err(method: &'static str, code: i64, message: &'static str) -> Step {
        Step { method, before: Vec::new(), reply: Reply::Error(code, message) }
    }

    impl Step {
        /// Updates Qwen sends before it replies, as it does for a turn and for a load replay.
        fn preceded_by(self, before: Vec<Value>) -> Self {
            Self { before, ..self }
        }
    }

    async fn next_request(peer: &mut Peer, seen: &mut Vec<Value>) -> Value {
        loop {
            let message = peer.read().await.unwrap();
            seen.push(message.clone());
            if message.get("id").is_some() && message.get("method").is_some() {
                return message;
            }
        }
    }

    /// Answers the client's requests in order and records every message it got.
    async fn fake_qwen(mut peer: Peer, steps: Vec<Step>) -> (Peer, Vec<Value>) {
        let mut seen = Vec::new();
        for step in steps {
            let request = next_request(&mut peer, &mut seen).await;
            assert_eq!(request.get("method").and_then(Value::as_str), Some(step.method));
            for update in step.before {
                peer.write(update).await;
            }
            let id = request.get("id").cloned().unwrap();
            peer.write(match step.reply {
                Reply::Result(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
                Reply::Error(code, message) => {
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
                }
            })
            .await;
        }
        (peer, seen)
    }

    fn update(update: Value) -> Value {
        json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": "s-1", "update": update}})
    }

    fn chunk(text: &str) -> Value {
        update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}))
    }

    fn usage(total: u64) -> Value {
        update(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": ""},
                      "_meta": {"usage": {"inputTokens": 1, "outputTokens": 1, "totalTokens": total}}}))
    }

    fn tools() -> HubTools<'static> {
        HubTools { url: "http://127.0.0.1:4321/mcp", token: "t0k3n" }
    }

    fn topic(saved: Option<&str>) -> TopicSession {
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        TopicSession::fresh(BackendKind::Qwen, cwd).with_session(saved.and_then(SessionId::parse))
    }

    fn session_id() -> SessionId {
        SessionId::parse("s-1").unwrap()
    }

    fn prompt(text: &str) -> Prompt {
        Prompt::new(text.to_owned(), Vec::new()).unwrap()
    }

    fn conversation() -> (Conversation, mpsc::Receiver<AgentEvent>) {
        let (events, received) = mpsc::channel(64);
        let conversation = Conversation {
            channel: Arc::new(Scripted::default()),
            events,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_secs(1)),
        };
        (conversation, received)
    }

    async fn collect(mut received: mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Some(event) = received.recv().await {
            events.push(event);
        }
        events
    }

    fn methods(seen: &[Value]) -> Vec<String> {
        seen.iter().filter_map(|m| m.get("method").and_then(Value::as_str)).map(str::to_owned).collect()
    }

    fn sent<'a>(seen: &'a [Value], method: &str) -> Option<&'a Value> {
        seen.iter().find(|m| m.get("method") == Some(&json!(method))).and_then(|m| m.get("params"))
    }

    /// Runs `serve` against a fake Qwen; returns the events and what Qwen saw.
    async fn run(topic: TopicSession, steps: Vec<Step>) -> (Vec<AgentEvent>, Vec<Value>) {
        let (drain, mut drains) = drain_channel();
        let (mut connection, peer) =
            pair(handler(Arc::new(Scripted::default()), drain), Duration::from_secs(5));
        let server = tokio::spawn(fake_qwen(peer, steps));
        let (conversation, received) = conversation();
        let (_inbox_in, inbox) = mpsc::channel(4);
        let link = Link {
            client: &connection.client,
            notifications: &mut connection.notifications,
            drains: &mut drains,
        };
        let _inbox = serve(link, &tools(), &topic, prompt("hi"), inbox, &conversation).await;
        drop(conversation);
        let (_peer, seen) = server.await.unwrap();
        (collect(received).await, seen)
    }

    fn initialized() -> Step {
        ok("initialize", json!({"protocolVersion": 1, "agentInfo": {"name": "qwen-code", "version": "0.25.0"}}))
    }

    fn end_turn() -> Value {
        json!({"stopReason": "end_turn"})
    }

    #[tokio::test]
    async fn new_session_brings_the_hub_tools_and_runs_a_turn() {
        let (events, seen) = run(
            topic(None),
            vec![
                initialized(),
                ok("session/new", json!({"sessionId": "s-1", "models": {}, "modes": {}})),
                ok("session/prompt", end_turn()).preceded_by(vec![chunk("Привет"), usage(42)]),
            ],
        )
        .await;
        assert_eq!(
            events,
            [
                AgentEvent::SessionStarted(session_id()),
                AgentEvent::AssistantText("Привет".to_owned()),
                AgentEvent::Finished(Finished {
                    session: session_id(),
                    usage: Usage::Qwen { tokens: Some(42) },
                    background: 0,
                }),
            ]
        );
        assert_eq!(methods(&seen), ["initialize", "session/new", "session/prompt"]);
        assert_eq!(
            sent(&seen, "session/new").and_then(|p| p.pointer("/mcpServers/0/headers/0/value")),
            Some(&json!("Bearer t0k3n"))
        );
        assert!(seen.iter().all(|message| message.get("jsonrpc") == Some(&json!("2.0"))));
    }

    #[tokio::test]
    async fn saved_session_is_loaded_and_its_replay_is_dropped() {
        let replay = vec![chunk("старый ответ"), update(json!({"sessionUpdate": "tool_call", "toolCallId": "old", "title": "Shell: ls", "kind": "execute"}))];
        let (events, seen) = run(
            topic(Some("s-1")),
            vec![
                initialized(),
                ok("session/load", json!({"models": {}})).preceded_by(replay),
                ok("session/prompt", end_turn()),
            ],
        )
        .await;
        assert_eq!(
            events,
            [
                AgentEvent::SessionStarted(session_id()),
                AgentEvent::Finished(Finished {
                    session: session_id(),
                    usage: Usage::Qwen { tokens: None },
                    background: 0,
                }),
            ]
        );
        assert_eq!(sent(&seen, "session/load").and_then(|p| p.get("sessionId")), Some(&json!("s-1")));
    }

    #[rstest]
    #[case::new(None, "session/new")]
    #[case::load(Some("s-1"), "session/load")]
    #[tokio::test]
    async fn missing_auth_fails_with_a_hint(#[case] saved: Option<&str>, #[case] open: &'static str) {
        let refused = err(open, -32000, "Authentication required: Use Qwen Code CLI to authenticate first.");
        let (events, seen) = run(topic(saved), vec![initialized(), refused]).await;
        assert_eq!(events, [AgentEvent::Failed(NOT_AUTHENTICATED.to_owned())]);
        assert!(!methods(&seen).contains(&"session/prompt".to_owned()));
    }

    #[rstest]
    #[case::not_started(None, err("session/new", -32603, "Internal error: bad model"), "Qwen не начал сессию: Internal error: bad model")]
    #[case::not_loaded(Some("s-1"), err("session/load", -32002, "Resource not found: session:s-1"), "Qwen не смог продолжить сессию s-1: Resource not found: session:s-1. /reset — начать заново")]
    #[tokio::test]
    async fn a_session_that_does_not_open_fails_with_its_reason(
        #[case] saved: Option<&str>,
        #[case] open: Step,
        #[case] reason: &str,
    ) {
        let (events, _seen) = run(topic(saved), vec![initialized(), open]).await;
        assert_eq!(events, [AgentEvent::Failed(reason.to_owned())]);
    }

    #[tokio::test]
    async fn an_error_mid_turn_fails_with_its_message() {
        let (events, _seen) = run(
            topic(None),
            vec![
                initialized(),
                ok("session/new", json!({"sessionId": "s-1"})),
                err("session/prompt", -32603, "Internal error: quota exceeded"),
            ],
        )
        .await;
        assert_eq!(
            events,
            [
                AgentEvent::SessionStarted(session_id()),
                AgentEvent::Failed("Qwen: Internal error: quota exceeded".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn qwen_exiting_mid_turn_fails_the_turn() {
        let (drain, mut drains) = drain_channel();
        let (mut connection, peer) =
            pair(handler(Arc::new(Scripted::default()), drain), Duration::from_secs(5));
        let server = tokio::spawn(async move {
            let steps = vec![initialized(), ok("session/new", json!({"sessionId": "s-1"}))];
            let (mut peer, mut seen) = fake_qwen(peer, steps).await;
            next_request(&mut peer, &mut seen).await;
            drop(peer);
        });
        let (conversation, received) = conversation();
        let (_inbox_in, inbox) = mpsc::channel(4);
        let link = Link {
            client: &connection.client,
            notifications: &mut connection.notifications,
            drains: &mut drains,
        };
        let _inbox = serve(link, &tools(), &topic(None), prompt("hi"), inbox, &conversation).await;
        drop(conversation);
        server.await.unwrap();
        assert_eq!(
            collect(received).await,
            [AgentEvent::SessionStarted(session_id()), AgentEvent::Failed(QWEN_FAILED.to_owned())]
        );
    }

    #[tokio::test]
    async fn a_drain_mid_turn_hands_over_the_waiting_prompt() {
        let (drain, mut drains) = drain_channel();
        let (mut connection, peer) =
            pair(handler(Arc::new(Scripted::default()), drain), Duration::from_secs(5));
        let server = tokio::spawn(async move {
            let steps = vec![initialized(), ok("session/new", json!({"sessionId": "s-1"}))];
            let (mut peer, mut seen) = fake_qwen(peer, steps).await;
            let turn = next_request(&mut peer, &mut seen).await;
            peer.write(json!({"jsonrpc": "2.0", "id": 0, "method": "craft/drainMidTurnQueue",
                              "params": {"sessionId": "s-1", "promptId": "p-1"}}))
                .await;
            let drained = peer.read().await.unwrap();
            let id = turn.get("id").cloned().unwrap();
            peer.write(json!({"jsonrpc": "2.0", "id": id, "result": {"stopReason": "end_turn"}})).await;
            drained
        });
        let (conversation, received) = conversation();
        let (inbox_in, inbox) = mpsc::channel(4);
        inbox_in.send(prompt("ещё")).await.unwrap();
        let link = Link {
            client: &connection.client,
            notifications: &mut connection.notifications,
            drains: &mut drains,
        };
        let _inbox = serve(link, &tools(), &topic(None), prompt("hi"), inbox, &conversation).await;
        drop(conversation);
        assert_eq!(
            server.await.unwrap(),
            json!({"jsonrpc": "2.0", "id": 0, "result": {
                "items": [{"content": [{"type": "text", "text": "ещё"}]}],
                "hasQueuedPrompt": false
            }})
        );
        assert_eq!(collect(received).await.len(), 2);
    }

    #[tokio::test]
    async fn stop_while_the_session_opens_ends_without_a_turn() {
        let (drain, mut drains) = drain_channel();
        let (mut connection, peer) =
            pair(handler(Arc::new(Scripted::default()), drain), Duration::from_secs(5));
        let server = tokio::spawn(fake_qwen(peer, vec![initialized()]));
        let (conversation, received) = conversation();
        let (inbox_in, inbox) = mpsc::channel(4);
        inbox_in.send(prompt("потом")).await.unwrap();
        let cancel = conversation.cancel.clone();
        let stopper = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });
        let link = Link {
            client: &connection.client,
            notifications: &mut connection.notifications,
            drains: &mut drains,
        };
        let outcome = tokio::time::timeout(
            Duration::from_secs(2),
            serve(link, &tools(), &topic(None), prompt("hi"), inbox, &conversation),
        )
        .await;
        stopper.await.unwrap();
        let mut leftover = outcome.expect("serve must return on /stop while session/new waits");
        drop(conversation);
        let (_peer, seen) = server.await.unwrap();
        assert!(!methods(&seen).contains(&"session/prompt".to_owned()));
        assert!(collect(received).await.is_empty());
        assert_eq!(leftover.recv().await.map(|p| p.text().to_owned()), Some("потом".to_owned()));
    }

    fn settings(qwen: QwenDraft) -> QwenSettings {
        let root = std::env::temp_dir();
        Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            qwen,
            ..Draft::default()
        }
        .parse(&root)
        .unwrap()
        .qwen
    }

    #[rstest]
    #[case::own_setup(QwenDraft::default(), &["--acp", "--approval-mode", "default"], &[])]
    #[case::model_and_mode(
        QwenDraft { model: "qwen3-coder-plus".to_owned(), approval: QwenApproval::Yolo, ..QwenDraft::default() },
        &["--acp", "--approval-mode", "yolo", "-m", "qwen3-coder-plus"],
        &[]
    )]
    #[case::hub_key(
        QwenDraft { model: "m".to_owned(), base_url: "https://x/v1".to_owned(), api_key: "sk-q".to_owned(), ..QwenDraft::default() },
        &["--acp", "--approval-mode", "default", "-m", "m", "--auth-type", "openai"],
        &[("OPENAI_API_KEY", "sk-q"), ("OPENAI_BASE_URL", "https://x/v1"), ("OPENAI_MODEL", "m")]
    )]
    fn process_gets_its_mode_model_and_key(
        #[case] qwen: QwenDraft,
        #[case] args: &[&str],
        #[case] env: &[(&str, &str)],
    ) {
        let settings = settings(qwen);
        assert_eq!((launch_args(&settings), child_env(&settings)), (args.to_vec(), env.to_vec()));
    }

    #[test]
    fn auth_follows_the_endpoint() {
        let own = settings(QwenDraft::default());
        let hub = settings(QwenDraft { base_url: "https://x".to_owned(), api_key: "k".to_owned(), ..QwenDraft::default() });
        assert_eq!((QwenAuth::of(&own), QwenAuth::of(&hub)), (QwenAuth::OwnSetup, QwenAuth::HubKey));
    }

    #[tokio::test]
    async fn missing_binary_fails_the_turn_and_keeps_the_inbox() {
        let backend = QwenBackend::new(PathBuf::from("definitely-not-qwen-binary"), settings(QwenDraft::default()));
        let (conversation, mut received) = conversation();
        let (inbox_in, inbox) = mpsc::channel(4);
        inbox_in.send(prompt("потом")).await.unwrap();
        let mut leftover = backend.run(&topic(None), prompt("hi"), inbox, conversation).await;
        assert_eq!(received.recv().await, Some(AgentEvent::Failed(QWEN_FAILED.to_owned())));
        assert!(leftover.try_recv().is_ok());
    }
}
```

(Последний тест поднимает настоящий `McpHttpServer` на `127.0.0.1` — это единственный тест модуля, касающийся сети; он короткий и не зависит от внешних сервисов.)

Run: `cargo test -p hub-qwen backend`
Expected: FAIL — нет `serve`, `Link`, `handler`, `launch_args`, `child_env`, `QwenBackend`, `QwenAuth`.

- [ ] **Step 2: Реализация (GREEN)**

Над тестами:

```rust
//! The `qwen --acp` child for one conversation: the hub's MCP server, the process, the session.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use hub_agent::channel::UserChannel;
use hub_agent::cli::hide_window;
use hub_agent::conversation::Conversation;
use hub_agent::mcp_http::McpHttpServer;
use hub_agent::rpc::{self, Connection, Envelope, Handler, Notification, RpcClient, RpcError, Wire};
use hub_core::domain::{AgentEvent, Prompt, SessionId, TopicSession};
use hub_core::settings::QwenSettings;
use tokio::io::AsyncRead;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, LinesCodec};

use crate::protocol::{
    AUTH_REQUIRED, Call, HubTools, TurnTracker, initialize_params, open_session, parse_session_id,
};
use crate::requests;
use crate::session::{Drain, DrainRequests, RpcAgent, Turns, converse, drain_channel};

// Handshake and session control only; a turn runs until it ends, /stop, or the process exits.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(1);
// qwen exits on stdin EOF after closing its sessions; the grace lets it do that before a kill.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_STDERR_LINE: usize = 64 * 1024;
// Without it qwen restarts itself as a child Node process that outlives the parent for up to 120 s.
const NO_RELAUNCH: &str = "QWEN_CODE_NO_RELAUNCH";
pub const NOT_AUTHENTICATED: &str =
    "Qwen не авторизован: настройте qwen (/auth) или задайте API-ключ в настройках";
pub const QWEN_FAILED: &str = "Qwen Code завершился, подробности в логе";

/// How Qwen gets its model credentials; shown on the status tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QwenAuth {
    HubKey,
    OwnSetup,
}

impl QwenAuth {
    #[must_use]
    pub fn of(settings: &QwenSettings) -> Self {
        match settings.endpoint {
            Some(_) => Self::HubKey,
            None => Self::OwnSetup,
        }
    }
}

pub struct QwenBackend {
    cli: PathBuf,
    settings: QwenSettings,
}

impl QwenBackend {
    #[must_use]
    pub fn new(cli: PathBuf, settings: QwenSettings) -> Self {
        Self { cli, settings }
    }

    /// Runs one conversation and returns the inbox with the prompts it did not take.
    pub async fn run(
        &self,
        session: &TopicSession,
        prompt: Prompt,
        inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation,
    ) -> mpsc::Receiver<Prompt> {
        let tools = match McpHttpServer::start(Arc::clone(&conversation.channel)).await {
            Ok(tools) => tools,
            Err(error) => {
                tracing::error!(%error, "hub MCP server for qwen did not start");
                fail(&conversation, QWEN_FAILED.to_owned()).await;
                return inbox;
            }
        };
        let (drain, mut drains) = drain_channel();
        let handler = handler(Arc::clone(&conversation.channel), drain);
        let mut agent = match QwenProcess::spawn(&self.cli, &self.settings, handler) {
            Ok(agent) => agent,
            Err(error) => {
                tracing::error!(%error, cli = %self.cli.display(), "qwen did not start");
                fail(&conversation, QWEN_FAILED.to_owned()).await;
                return inbox;
            }
        };
        let link = Link {
            client: &agent.client,
            notifications: &mut agent.notifications,
            drains: &mut drains,
        };
        let hub = HubTools { url: tools.url(), token: tools.token() };
        let inbox = serve(link, &hub, session, prompt, inbox, &conversation).await;
        agent.stop().await;
        // Only now: qwen's MCP client must not see the server vanish in the middle of a call.
        drop(tools);
        inbox
    }
}

fn handler(channel: Arc<dyn UserChannel>, drain: Drain) -> Handler {
    Arc::new(move |method, params| {
        let channel = Arc::clone(&channel);
        let drain = drain.clone();
        Box::pin(async move { requests::answer(channel.as_ref(), &drain, &method, params).await })
    })
}

pub struct Link<'a> {
    pub client: &'a RpcClient,
    pub notifications: &'a mut mpsc::UnboundedReceiver<Notification>,
    pub drains: &'a mut DrainRequests,
}

/// One conversation over an established connection: handshake, session, turns.
pub async fn serve(
    link: Link<'_>,
    tools: &HubTools<'_>,
    session: &TopicSession,
    prompt: Prompt,
    inbox: mpsc::Receiver<Prompt>,
    conversation: &Conversation,
) -> mpsc::Receiver<Prompt> {
    let Link { client, notifications, drains } = link;
    let opened = tokio::select! {
        biased;
        () = conversation.cancel.cancelled() => return inbox,
        opened = open(client, notifications, tools, session) => opened,
    };
    let id = match opened {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(%error, "qwen session did not open");
            fail(conversation, error.to_string()).await;
            return inbox;
        }
    };
    emit(conversation, AgentEvent::SessionStarted(id.clone())).await;
    let agent = RpcAgent::new(client.clone(), id.clone());
    let turns = Turns { agent: &agent, notifications, drains };
    let (inbox, outcome) = converse(turns, TurnTracker::new(id), prompt, inbox, conversation).await;
    if let Err(error) = outcome {
        tracing::warn!(%error, "qwen session broke");
        fail(conversation, session_failure(&error)).await;
    }
    inbox
}

#[derive(Debug)]
enum OpenError {
    NotAuthenticated,
    NotStarted(String),
    NotLoaded { session: SessionId, message: String },
    Rpc(RpcError),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAuthenticated => f.write_str(NOT_AUTHENTICATED),
            Self::NotStarted(message) => write!(f, "Qwen не начал сессию: {message}"),
            Self::NotLoaded { session, message } => write!(
                f,
                "Qwen не смог продолжить сессию {}: {message}. /reset — начать заново",
                session.as_str()
            ),
            Self::Rpc(error) => f.write_str(&session_failure(error)),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<RpcError> for OpenError {
    fn from(error: RpcError) -> Self {
        Self::Rpc(error)
    }
}

fn session_failure(error: &RpcError) -> String {
    match error {
        RpcError::Remote { message, .. } => format!("Qwen: {message}"),
        RpcError::Closed | RpcError::Protocol(_) | RpcError::Timeout { .. } => QWEN_FAILED.to_owned(),
    }
}

async fn open(
    client: &RpcClient,
    notifications: &mut mpsc::UnboundedReceiver<Notification>,
    tools: &HubTools<'_>,
    session: &TopicSession,
) -> Result<SessionId, OpenError> {
    client.request(Call::Initialize.method(), initialize_params(env!("CARGO_PKG_VERSION"))).await?;
    let (call, params) = open_session(session, tools);
    let opened = client.request(call.method(), params).await;
    // A loaded session replays its history as updates before the reply; none of it is news.
    while notifications.try_recv().is_ok() {}
    match (opened, &session.session) {
        (Ok(result), None) => Ok(parse_session_id(&result)?),
        // `session/load` does not repeat the id it was given.
        (Ok(_), Some(saved)) => Ok(saved.clone()),
        (Err(RpcError::Remote { code: AUTH_REQUIRED, .. }), _) => Err(OpenError::NotAuthenticated),
        (Err(RpcError::Remote { message, .. }), None) => Err(OpenError::NotStarted(message)),
        (Err(RpcError::Remote { message, .. }), Some(saved)) => {
            Err(OpenError::NotLoaded { session: saved.clone(), message })
        }
        (Err(other @ (RpcError::Closed | RpcError::Protocol(_) | RpcError::Timeout { .. })), _) => {
            Err(OpenError::Rpc(other))
        }
    }
}

fn launch_args(settings: &QwenSettings) -> Vec<&str> {
    ["--acp", "--approval-mode", settings.approval.wire()]
        .into_iter()
        .chain(settings.model.iter().flat_map(|model| ["-m", model.as_str()]))
        .chain(settings.endpoint.iter().flat_map(|_| ["--auth-type", "openai"]))
        .collect()
}

/// The key goes only to this child; without an endpoint qwen keeps its own setup.
fn child_env(settings: &QwenSettings) -> Vec<(&'static str, &str)> {
    settings
        .endpoint
        .iter()
        .flat_map(|endpoint| {
            [("OPENAI_API_KEY", endpoint.key().expose()), ("OPENAI_BASE_URL", endpoint.base_url())]
                .into_iter()
                .chain(settings.model.iter().map(|model| ("OPENAI_MODEL", model.as_str())))
        })
        .collect()
}

struct QwenProcess {
    client: RpcClient,
    notifications: mpsc::UnboundedReceiver<Notification>,
    reader: JoinHandle<()>,
    child: Child,
}

impl QwenProcess {
    fn spawn(cli: &Path, settings: &QwenSettings, handler: Handler) -> io::Result<Self> {
        let mut command = Command::new(cli);
        command
            .args(launch_args(settings))
            .env(NO_RELAUNCH, "true")
            .envs(child_env(settings))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        hide_window(&mut command);
        let mut child = command.spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(io::Error::other("qwen started without standard streams"));
        };
        tokio::spawn(log_stderr(stderr));
        let wire = Wire { peer: "qwen", envelope: Envelope::JsonRpc2, timeout: REQUEST_TIMEOUT };
        let Connection { client, notifications, reader } = rpc::connect(stdout, stdin, handler, wire);
        Ok(Self { client, notifications, reader, child })
    }

    async fn stop(mut self) {
        self.client.close();
        match tokio::time::timeout(EXIT_TIMEOUT, self.child.wait()).await {
            Ok(Ok(status)) => tracing::debug!(%status, "qwen exited"),
            Ok(Err(error)) => tracing::warn!(%error, "qwen exit status unavailable"),
            Err(_) => {
                tracing::warn!("qwen did not exit on stdin close, killing it");
                if let Err(error) = self.child.kill().await {
                    tracing::warn!(%error, "qwen could not be killed");
                }
            }
        }
        // Answers still in flight have nobody left to reach.
        self.reader.abort();
    }
}

async fn log_stderr<R: AsyncRead + Unpin>(stderr: R) {
    let mut lines = FramedRead::new(stderr, LinesCodec::new_with_max_length(MAX_STDERR_LINE));
    while let Some(line) = lines.next().await {
        match line {
            Ok(line) => tracing::info!(%line, "qwen stderr"),
            Err(error) => {
                tracing::debug!(%error, "qwen stderr unreadable");
                return;
            }
        }
    }
}

async fn emit(conversation: &Conversation, event: AgentEvent) {
    // Nobody listening means the topic is gone; there is no one left to tell.
    let _ = conversation.events.send(event).await;
}

async fn fail(conversation: &Conversation, reason: String) {
    emit(conversation, AgentEvent::Failed(reason)).await;
}
```

Если константа в образце `code: AUTH_REQUIRED` не принимается компилятором как шаблон (принимается: это `const i64`), заменить на охрану `Err(RpcError::Remote { code, .. }) if code == AUTH_REQUIRED`.

- [ ] **Step 3: Verify GREEN**

Run: `cargo test -p hub-qwen backend`
Expected: PASS (13 тестов с учётом случаев `rstest`).

- [ ] **Step 4: Security / reliability checklist задачи**

- `rg "tracing::.*(key|token|OPENAI)" crates/hub-qwen/src` — пусто: ключ и токен не логируются; параметры `session/new` (с токеном) не логируются нигде (`rpc` пишет в лог только ошибки и метод).
- Каждое ожидание ограничено: управляющие вызовы 60 с, `session/cancel` 2 с, выход процесса 10 с + `kill`, drain 1,5 с; ход ограничен `/stop` и выходом процесса (решение спецификации).
- Порядок остановки: процесс → `reader.abort()` (обрывает обработчики, ждущие человека) → MCP-сервер.
- Остаточный риск (в README, задача 13): при ключе из настроек хаба `OPENAI_API_KEY` есть в окружении `qwen` и видно командам, которые запускает агент.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add -A crates/hub-qwen
git commit -m "hub-qwen: бэкенд — процесс qwen, сервер инструментов и сессия"
```

---
### Task 9: Хаб запускает Qwen (`HubAgents`)

**Files:**
- Modify: `crates/hub-telegram/Cargo.toml` (`[dependencies] hub-qwen.workspace = true`)
- Modify: `crates/hub-telegram/src/agents.rs:1-13` (импорты), временная ветка из задачи 3, тесты `:73-135`

**Interfaces:**
- Consumes: `hub_qwen::version::locate`, `hub_qwen::backend::{QwenBackend, QWEN_FAILED}`.

- [ ] **Step 1: Тест (RED)**

В `mod tests` файла `agents.rs` (импорт `hub_core::settings::QwenDraft` рядом с `CodexDraft`):

```rust
    #[tokio::test]
    async fn qwen_topics_run_the_configured_qwen() {
        let missing = std::env::temp_dir().join("definitely-not-qwen-binary");
        let settings = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: std::env::temp_dir().display().to_string(),
            qwen: QwenDraft { cli: missing.display().to_string(), ..QwenDraft::default() },
            ..Draft::default()
        }
        .parse(&std::env::temp_dir())
        .unwrap();
        let agents = HubAgents::new(watch::Sender::new(Arc::new(settings)).subscribe());
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        let session = TopicSession::fresh(BackendKind::Qwen, cwd);
        let (events, mut received) = mpsc::channel(4);
        let conversation = Conversation {
            channel: Arc::new(Allowing),
            events,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_secs(1)),
        };
        let (_inbox_in, inbox) = mpsc::channel(1);
        let prompt = Prompt::new("hi".to_owned(), Vec::new()).unwrap();

        let _inbox = agents.run(&session, prompt, inbox, conversation).await;

        assert_eq!(
            received.recv().await,
            Some(AgentEvent::Failed(hub_qwen::backend::QWEN_FAILED.to_owned()))
        );
    }
```

Run: `cargo test -p hub-telegram qwen_topics`
Expected: FAIL — приходит `Failed("Qwen пока не подключён")` (временная ветка задачи 3); после добавления зависимости без изменения ветки — то же.

- [ ] **Step 2: Реализация (GREEN)**

`crates/hub-telegram/Cargo.toml`: `hub-qwen.workspace = true` после `hub-codex`.

`agents.rs`: импорт `use hub_qwen::backend::QwenBackend;`; временную ветку заменить на:

```rust
                BackendKind::Qwen => match hub_qwen::version::locate(settings.qwen.cli.as_deref()) {
                    Ok(cli) => {
                        QwenBackend::new(cli, settings.qwen.clone())
                            .run(session, prompt, inbox, conversation)
                            .await
                    }
                    Err(error) => refuse(&conversation, error.to_string(), inbox).await,
                },
```

- [ ] **Step 3: Verify GREEN и гейты**

Run: `cargo test -p hub-telegram agents && cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add -A crates/hub-telegram Cargo.lock
git commit -m "Хаб запускает Qwen в темах с бэкендом qwen"
```

---

### Task 10: Ключ Qwen в хранилище ключей

**Files:**
- Modify: `crates/hub-app/src/secrets.rs:5-20` (`Secret`)
- Modify: `crates/hub-app/src/config.rs:48-53` (`load`), `:61-71` (`save`), тесты `:73-181`

**Interfaces:**
- Produces: `hub_app::secrets::Secret::QwenKey` (аккаунт `qwen-api-key`).
- Consumes: `hub_core::settings::{Keys { qwen_key }, QwenSettings::endpoint, ApiEndpoint::key}` (задача 3).

- [ ] **Step 1: Тесты (RED)**

`crates/hub-app/src/config.rs`, тесты (импорты: `hub_core::settings::{ApiKey, Field, QwenField}`, `crate::secrets::{MemorySecrets, Secret, Secrets}`):

```rust
    fn with_qwen_endpoint(root: &Path) -> Draft {
        let mut form = draft(root);
        form.qwen.base_url = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1".to_owned();
        form.qwen.api_key = "sk-qwen-secret".to_owned();
        form
    }

    #[test]
    fn qwen_key_goes_to_the_keyring_not_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let settings = with_qwen_endpoint(dir.path()).parse(dir.path()).unwrap();
        let store = store(dir.path());
        store.save(&settings).unwrap();
        let text = std::fs::read_to_string(dir.path().join("settings.toml")).unwrap();
        assert!(!text.contains("sk-qwen-secret") && text.contains("base_url"));
        match store.load(dir.path()).unwrap() {
            Loaded::Ready(loaded) => assert_eq!(loaded.qwen, settings.qwen),
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn cleared_qwen_endpoint_removes_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        store.save(&with_qwen_endpoint(dir.path()).parse(dir.path()).unwrap()).unwrap();
        store.save(&draft(dir.path()).parse(dir.path()).unwrap()).unwrap();
        match store.load(dir.path()).unwrap() {
            Loaded::Ready(loaded) => assert_eq!(loaded.qwen.endpoint, None),
            other => panic!("expected Ready, got {other:?}"),
        }
    }

    #[test]
    fn a_stored_key_without_an_address_leaves_the_form_incomplete() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path()).save(&draft(dir.path()).parse(dir.path()).unwrap()).unwrap();
        let secrets = MemorySecrets::default();
        secrets.write(Secret::TelegramToken, Some("123:secret-token")).unwrap();
        secrets.write(Secret::QwenKey, Some("sk-orphan")).unwrap();
        let store = FileSettings::new(dir.path().join("settings.toml"), Box::new(secrets));
        match store.load(dir.path()).unwrap() {
            Loaded::Incomplete { errors, .. } => assert_eq!(
                errors.iter().map(|e| e.field).collect::<Vec<_>>(),
                [Field::Qwen(QwenField::BaseUrl)]
            ),
            other => panic!("expected Incomplete, got {other:?}"),
        }
    }
```

Run: `cargo test -p hub-app config`
Expected: FAIL — нет `Secret::QwenKey`; после его добавления без правок `load`/`save` — ключ не переживает `load` (`qwen_key_goes_to_the_keyring_not_the_file` падает на `Incomplete`).

- [ ] **Step 2: Реализация (GREEN)**

`secrets.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Secret {
    TelegramToken,
    OpenAiKey,
    QwenKey,
}

impl Secret {
    const fn account(self) -> &'static str {
        match self {
            Self::TelegramToken => "telegram-token",
            Self::OpenAiKey => "openai-api-key",
            Self::QwenKey => "qwen-api-key",
        }
    }
}
```

`config.rs`, `load`:

```rust
        let keys = Keys {
            token: self.secrets.read(Secret::TelegramToken)?.unwrap_or_default(),
            api_key: self.secrets.read(Secret::OpenAiKey)?.unwrap_or_default(),
            qwen_key: self.secrets.read(Secret::QwenKey)?.unwrap_or_default(),
        };
```

(`unwrap_or_default` здесь — «записи нет», а не сокрытие ошибки: ошибка хранилища уходит через `?`.)

`save`, после записи `OpenAiKey`:

```rust
        let qwen_key = settings.qwen.endpoint.as_ref().map(|endpoint| endpoint.key().expose());
        self.secrets.write(Secret::QwenKey, qwen_key)?;
```

- [ ] **Step 3: Verify GREEN и гейты**

Run: `cargo test -p hub-app config && cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add -A crates/hub-app
git commit -m "Ключ Qwen хранится в системном хранилище ключей"
```

---

### Task 11: Проверка Qwen при запуске бота и строка статуса

**Files:**
- Modify: `crates/hub-app/Cargo.toml` (`hub-qwen.workspace = true`)
- Modify: `crates/hub-app/src/supervisor.rs:4, 45-49` (`AgentAuth`, `AgentState`)
- Modify: `crates/hub-app/src/connector.rs:5-6, 24-32, 46-65`, тесты `:215-270`
- Modify: `crates/hub-app/src/gui/look.rs:3, 66-86`, тесты `:166-287`

**Interfaces:**
- Produces: `hub_app::supervisor::AgentAuth { Codex(CodexAuth), Qwen(QwenAuth) }`; `AgentState::Ready { version: String, auth: Option<AgentAuth> }`; `AgentError::Qwen(hub_qwen::version::CliError)`.
- Consumes: `hub_qwen::version::{locate, check}`, `hub_qwen::backend::QwenAuth`.

Правило (спецификация, `hub-app`): обязателен CLI бэкенда по умолчанию; Qwen не по умолчанию проверяется мягко (поиск и версия) — уже обеспечено `statuses`. Вход заранее не проверяется: `authRequired` приходит только на `session/new`, а пробная сессия оставила бы файл в `~/.qwen`.

- [ ] **Step 1: Тесты (RED)**

`connector.rs`, тесты:

```rust
    #[test]
    fn qwen_that_is_not_the_default_may_be_missing() {
        let checked = vec![
            (BackendKind::Claude, Ok(ready("2.1.287"))),
            (BackendKind::Qwen, Err(AgentError::Qwen(hub_qwen::version::CliError::Missing))),
        ];
        assert_eq!(
            statuses(BackendKind::Claude, checked).unwrap(),
            [
                AgentStatus { kind: BackendKind::Claude, state: ready("2.1.287") },
                AgentStatus {
                    kind: BackendKind::Qwen,
                    state: AgentState::Unavailable(
                        "Qwen Code не найден: укажите путь в настройках или установите `qwen` в PATH".to_owned()
                    ),
                },
            ]
        );
    }

    #[tokio::test]
    async fn a_missing_default_qwen_stops_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let connector = TelegramConnector {
            home: dir.path().to_path_buf(),
            topics: dir.path().join("topics.json"),
        };
        let settings = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: dir.path().display().to_string(),
            default_backend: BackendKind::Qwen,
            qwen: QwenDraft { cli: dir.path().join("нет-qwen").display().to_string(), ..QwenDraft::default() },
            ..Draft::default()
        }
        .parse(dir.path())
        .unwrap();
        let receiver = watch::Sender::new(Arc::new(settings)).subscribe();
        let error = connector.connect(receiver).await.err().unwrap();
        assert!(matches!(error, StartError::Agent(AgentError::Qwen(_))));
    }
```

(импорт `hub_core::settings::QwenDraft` в тестах).

`gui/look.rs`, `agent_lines` — Codex-случаи переписать на `Some(AgentAuth::Codex(CodexAuth::…))` с теми же ожиданиями и добавить:

```rust
    #[case(Some(AgentState::Ready { version: "0.25.0".to_owned(), auth: Some(AgentAuth::Qwen(QwenAuth::HubKey)) }), "0.25.0 · ключ из настроек хаба")]
    #[case(Some(AgentState::Ready { version: "0.25.0".to_owned(), auth: Some(AgentAuth::Qwen(QwenAuth::OwnSetup)) }), "0.25.0 · собственная настройка qwen")]
```

(импорты `crate::supervisor::AgentAuth`, `hub_qwen::backend::QwenAuth`).

Run: `cargo test -p hub-app connector look`
Expected: FAIL — нет `AgentError::Qwen`, `AgentAuth`, `hub_qwen` в зависимостях.

- [ ] **Step 2: Реализация (GREEN)**

`crates/hub-app/Cargo.toml`: `hub-qwen.workspace = true` после `hub-codex`.

`supervisor.rs`:

```rust
use hub_codex::protocol::CodexAuth;
use hub_qwen::backend::QwenAuth;
```

```rust
/// How an agent signs in to its model, where the hub knows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAuth {
    Codex(CodexAuth),
    Qwen(QwenAuth),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentState {
    Ready { version: String, auth: Option<AgentAuth> },
    Unavailable(String),
}
```

`connector.rs`: импорт `use hub_qwen::backend::QwenAuth;`, `use crate::supervisor::{AgentAuth, AgentState, AgentStatus, Connector, Started};`; в `AgentError`:

```rust
    #[error(transparent)]
    Qwen(#[from] hub_qwen::version::CliError),
```

в `check_agent` ветка Codex: `Ok(AgentState::Ready { version: version.to_string(), auth: Some(AgentAuth::Codex(auth)) })`; временную ветку Qwen заменить:

```rust
        BackendKind::Qwen => {
            let cli = hub_qwen::version::locate(settings.qwen.cli.as_deref())?;
            let version = hub_qwen::version::check(&cli).await?;
            // Sign-in is not probed: qwen reports it only on session/new, and a trial session
            // would leave a file in ~/.qwen.
            let auth = AgentAuth::Qwen(QwenAuth::of(&settings.qwen));
            Ok(AgentState::Ready { version: version.to_string(), auth: Some(auth) })
        }
```

`gui/look.rs`:

```rust
use hub_codex::protocol::CodexAuth;
use hub_qwen::backend::QwenAuth;
```

```rust
#[must_use]
pub fn agent_line(state: Option<&AgentState>) -> String {
    match state {
        None => "—".to_owned(),
        Some(AgentState::Ready { version, auth: None }) => version.clone(),
        Some(AgentState::Ready { version, auth: Some(auth) }) => {
            format!("{version} · {}", auth_text(*auth))
        }
        Some(AgentState::Unavailable(reason)) => format!("недоступен: {reason}"),
    }
}

const fn auth_text(auth: AgentAuth) -> &'static str {
    match auth {
        AgentAuth::Codex(CodexAuth::ApiKey) => "вход: API-ключ",
        AgentAuth::Codex(CodexAuth::ChatGpt) => "вход: ChatGPT",
        AgentAuth::Codex(CodexAuth::Other) => "вход: другой способ",
        AgentAuth::Codex(CodexAuth::NotRequired) => "вход: не требуется",
        AgentAuth::Codex(CodexAuth::Missing) => "вход: не выполнен — `codex login` или API-ключ",
        AgentAuth::Qwen(QwenAuth::HubKey) => "ключ из настроек хаба",
        AgentAuth::Qwen(QwenAuth::OwnSetup) => "собственная настройка qwen",
    }
}
```

Импорт `AgentAuth` из `crate::supervisor`. `rg "auth: Some\(" crates/hub-app` — других мест нет (тесты `supervisor.rs:499, 656` используют `auth: None` и не меняются).

- [ ] **Step 3: Verify GREEN и гейты**

Run: `cargo test -p hub-app && cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS; строки статуса Codex не изменились.

- [ ] **Step 4: Commit**

```bash
git add -A crates/hub-app Cargo.lock
git commit -m "Запуск бота проверяет Qwen; статус показывает способ входа Qwen"
```

---

### Task 12: GUI — настройки Qwen

**Files:**
- Modify: `crates/hub-app/src/gui/settings.rs:7-9` (импорты), `:15-22` (`SettingsForm`), `:88-99` (`show`), новая `fn qwen` после `fn codex` (`:210-280`)

**Interfaces:**
- Consumes: `hub_core::settings::{ApiEndpoint, QwenApproval, QwenField}`; `Draft::qwen` (задача 3). Выбор «Агент по умолчанию» уже перебирает `BackendKind::ALL` (`:161`), статус — тоже (`gui/status.rs:31`): Qwen появляется там без правок.

У egui-кода нет модульных тестов (как и у секции Codex); проверка — компиляция, clippy и ручная проверка (задача 14). Логика формы (разбор, пара URL+ключ, предупреждение о `http://`) протестирована в `hub-core` (задача 3).

- [ ] **Step 1: Реализация**

Импорты:

```rust
use hub_core::settings::{
    ApiEndpoint, Approval, CodexField, Draft, Field, FieldError, PermissionMode, QwenApproval,
    QwenField, Sandbox, UpdateCheck,
};
```

`SettingsForm`: поле `reveal_qwen_key: bool,` после `reveal_key`.

`show`, внутри `ScrollArea` после `self.codex(ui, &errors);`:

```rust
            ui.separator();
            let parsed = self.draft.parse(home).ok();
            let endpoint = parsed.as_ref().and_then(|settings| settings.qwen.endpoint.as_ref());
            self.qwen(ui, &errors, endpoint);
```

Новая функция:

```rust
    fn qwen(&mut self, ui: &mut egui::Ui, errors: &[FieldError], endpoint: Option<&ApiEndpoint>) {
        ui.heading("Qwen");
        ui.horizontal(|ui| {
            ui.label("Путь к qwen");
            ui.add(egui::TextEdit::singleline(&mut self.draft.qwen.cli).hint_text("из PATH"));
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_file(&self.draft.qwen.cli)
            {
                self.draft.qwen.cli = path;
            }
        });
        messages(ui, errors, Field::Qwen(QwenField::Cli));
        ui.horizontal(|ui| {
            ui.label("Модель");
            ui.add(egui::TextEdit::singleline(&mut self.draft.qwen.model).hint_text("из настроек Qwen"));
        });
        messages(ui, errors, Field::Qwen(QwenField::Model));
        ui.horizontal(|ui| {
            ui.label("Одобрения");
            egui::ComboBox::from_id_salt("qwen_approval")
                .selected_text(self.draft.qwen.approval.wire())
                .show_ui(ui, |ui| {
                    for approval in QwenApproval::ALL {
                        ui.selectable_value(&mut self.draft.qwen.approval, approval, approval.wire());
                    }
                });
        });
        if self.draft.qwen.approval == QwenApproval::Yolo {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "yolo: Qwen выполняет любые инструменты без подтверждения.",
            );
        }
        ui.horizontal(|ui| {
            ui.label("Base URL");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.qwen.base_url)
                    .hint_text("собственная настройка qwen"),
            );
        });
        messages(ui, errors, Field::Qwen(QwenField::BaseUrl));
        ui.horizontal(|ui| {
            ui.label("API-ключ");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.qwen.api_key)
                    .password(!self.reveal_qwen_key)
                    .hint_text("собственная настройка qwen"),
            );
            ui.checkbox(&mut self.reveal_qwen_key, "показать");
        });
        messages(ui, errors, Field::Qwen(QwenField::ApiKey));
        if endpoint.is_some_and(ApiEndpoint::is_cleartext_remote) {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "http:// к другому компьютеру: ключ уходит в сеть открытым текстом.",
            );
        }
        ui.label(
            "Без адреса и ключа Qwen использует собственную настройку (`qwen` → /auth, \
             ~/.qwen/settings.json). Адрес OpenAI-совместимый; ключ хранится в системном \
             хранилище ключей и передаётся только процессу qwen.",
        );
    }
```

- [ ] **Step 2: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 3: Быстрая визуальная проверка**

Run: `cargo run -p hub-app`
Expected: на вкладке «Настройки» после «Codex» — секция «Qwen»; ввод только base URL даёт под полем ключа «Укажите API-ключ для этого адреса», «Сохранить» неактивна; `http://192.168.1.5:8000/v1` + ключ — жёлтое предупреждение; «Агент по умолчанию» предлагает `qwen`; вкладка «Статус» — строка «Qwen Code».

- [ ] **Step 4: Commit**

```bash
git add -A crates/hub-app
git commit -m "GUI: настройки Qwen"
```

---
### Task 13: Живые тесты, README, версия 0.3.0

**Files:**
- Create: `crates/hub-qwen/tests/live.rs`
- Modify: `README.md` (разделы «Требования» `:39-47`, «Команды» `:137-155`, новый «Qwen» после «Codex» `:158-166`, «Окно и трей» `:236-243`, «Настройки и файлы» `:253-284`, «Безопасность» `:305-328`)
- Modify: `Cargo.toml:6` (`version = "0.3.0"`), `Cargo.lock`

**Interfaces:**
- Consumes: `hub_qwen::backend::QwenBackend`, `hub_qwen::version::{check, locate}`.

Живые тесты закрывают то, что проверено только по исходникам: доходит ли `answers`, просит ли Qwen одобрение на `agent-hub/send_file`, токены в итоге хода.

- [ ] **Step 1: Живые тесты**

`crates/hub-qwen/tests/live.rs`:

```rust
//! Round trips through the real `qwen --acp`. They spend tokens and need a configured qwen
//! (its own setup), so they are opt-in:
//! `AGENT_HUB_LIVE_QWEN=1 cargo test -p hub-qwen --test live -- --ignored --test-threads=1`.

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use futures::future::BoxFuture;
    use hub_agent::channel::UserChannel;
    use hub_agent::conversation::{Conversation, Limits};
    use hub_core::domain::{
        AbsolutePath, AgentEvent, BackendKind, Decision, Denied, FileDelivery, OutgoingFile,
        Prompt, Question, QuestionAnswer, QuestionsOutcome, ToolRequest, TopicSession, Usage,
    };
    use hub_core::settings::{Draft, QwenApproval, QwenDraft, QwenSettings};
    use hub_qwen::backend::QwenBackend;
    use hub_qwen::version::{check, locate};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    /// Denies every tool, answers every question with its first option, records what it saw.
    #[derive(Default)]
    struct Recording {
        requests: Mutex<Vec<ToolRequest>>,
        questions: Mutex<Vec<Question>>,
    }

    impl UserChannel for Recording {
        fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision> {
            self.requests.lock().unwrap().push(tool);
            Box::pin(async { Decision::Denied(Denied::new("live test")) })
        }
        fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
            let answers = questions
                .iter()
                .map(|q| QuestionAnswer {
                    question: q.text().to_owned(),
                    answer: q.options().first().map(|o| o.label.clone()).unwrap_or_default(),
                })
                .collect();
            self.questions.lock().unwrap().extend(questions);
            Box::pin(async move { QuestionsOutcome::Answered(answers) })
        }
        fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
            Box::pin(async { FileDelivery::Delivered })
        }
    }

    fn settings(approval: QwenApproval) -> QwenSettings {
        let root = std::env::temp_dir();
        Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            qwen: QwenDraft { approval, ..QwenDraft::default() },
            ..Draft::default()
        }
        .parse(&root)
        .unwrap()
        .qwen
    }

    async fn live(text: &str, approval: QwenApproval) -> Option<(Vec<AgentEvent>, Arc<Recording>)> {
        std::env::var_os("AGENT_HUB_LIVE_QWEN")?;
        let cli = locate(None).unwrap();
        check(&cli).await.unwrap();
        let channel = Arc::new(Recording::default());
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        let session = TopicSession::fresh(BackendKind::Qwen, cwd);
        let (events, mut received) = mpsc::channel(256);
        let conversation = Conversation {
            channel: Arc::clone(&channel) as Arc<dyn UserChannel>,
            events,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_mins(1)),
        };
        let (_inbox_in, inbox) = mpsc::channel(1);
        let prompt = Prompt::new(text.to_owned(), Vec::new()).unwrap();
        let backend = QwenBackend::new(cli, settings(approval));
        let run = backend.run(&session, prompt, inbox, conversation);
        let _inbox = tokio::time::timeout(Duration::from_mins(5), run).await.unwrap();
        Some((std::iter::from_fn(|| received.try_recv().ok()).collect(), channel))
    }

    #[tokio::test]
    #[ignore = "needs a configured qwen and spends tokens"]
    async fn qwen_answers_a_prompt() {
        let Some((seen, _channel)) = live("Ответь одним словом: да", QwenApproval::Plan).await else {
            return;
        };
        assert!(matches!(seen.first(), Some(AgentEvent::SessionStarted(_))), "{seen:?}");
        assert!(
            matches!(seen.last(), Some(AgentEvent::Finished(f)) if matches!(f.usage, Usage::Qwen { .. })),
            "{seen:?}"
        );
    }

    #[tokio::test]
    #[ignore = "needs a configured qwen and spends tokens"]
    async fn qwen_asks_through_the_hub_and_gets_the_answer() {
        let text = "Вызови инструмент ask_user_question с одним вопросом «Какой цвет?» и \
                    вариантами «синий» и «красный», затем ответь одним словом — выбранным цветом.";
        let Some((seen, channel)) = live(text, QwenApproval::Default).await else { return };
        assert!(!channel.questions.lock().unwrap().is_empty(), "{seen:?}");
        let said: String = seen
            .iter()
            .filter_map(|e| if let AgentEvent::AssistantText(text) = e { Some(text.as_str()) } else { None })
            .collect();
        assert!(said.to_lowercase().contains("син"), "answers did not reach qwen: {seen:?}");
    }

    #[tokio::test]
    #[ignore = "needs a configured qwen and spends tokens"]
    async fn qwen_asks_before_using_send_file() {
        let text = "Отправь мне файл README.md инструментом send_file сервера agent-hub.";
        let Some((seen, channel)) = live(text, QwenApproval::Default).await else { return };
        let requests = channel.requests.lock().unwrap();
        assert!(
            requests.iter().any(|r| r.summary.contains("README") || r.summary.contains("send_file")),
            "no approval was asked for send_file: {requests:?} {seen:?}"
        );
    }
}
```

`crates/hub-qwen/Cargo.toml`, `[dev-dependencies]`: `tokio-util.workspace = true`, `hub-agent`/`hub-core` видны через `[dependencies]`.

Run: `cargo test -p hub-qwen --test live`
Expected: PASS (3 теста `ignored`).

- [ ] **Step 2: README**

- «Требования», после пункта о Codex: «Для агента Qwen (необязательно): [Qwen Code](https://github.com/QwenLM/qwen-code) **0.25 или новее** (`npm i -g @qwen-code/qwen-code`, Node.js 22+). Бесплатного входа Qwen OAuth больше нет — нужен собственный ключ: настройка `qwen` (`/auth`, `~/.qwen/settings.json`, переменные окружения) или OpenAI-совместимый адрес и API-ключ в настройках agent-hub.»
- Первый абзац и схема: «сессия Claude Code, Codex или Qwen Code».
- «Команды»: `/backend [claude\|codex\|qwen]`.
- Новый подраздел после «Codex»:

```markdown
### Qwen

`/new qwen <путь>` создаёт сессию Qwen Code, `/backend qwen` переключает существующую тему (контекст сбрасывается). Qwen работает как Codex: одобрения кнопками, `send_file`, свои вопросы с вариантами ответа, фото, досылка сообщений посреди хода (Qwen забирает их между вызовами инструментов), число токенов в итоге хода.

Доступ к модели — один из двух способов:
- собственная настройка `qwen`: выполните `qwen` в терминале и `/auth`, или заполните `~/.qwen/settings.json`, или задайте переменные окружения;
- «Настройки → Qwen»: OpenAI-совместимый base URL и API-ключ (например, DashScope). Ключ хранится в системном хранилище ключей и передаётся только процессу `qwen` (как `OPENAI_API_KEY`).

Режим одобрений — `plan`, `default` (по умолчанию), `auto-edit`, `yolo`. Отправка файла инструментом хаба тоже спрашивает разрешение (кроме `yolo`). «Разрешить всегда» не предлагается.
```

- «Окно и трей», «Статус»: «версии agent-hub, Claude Code, Codex и Qwen Code; для Qwen — откуда ключ (настройки хаба или собственная настройка qwen)».
- «Настройки и файлы»: строки таблицы «Агент по умолчанию (`claude`, `codex`, `qwen`)», «Путь к `qwen` — из `PATH`», «Модель Qwen — из настроек Qwen», «Одобрения Qwen (`plan`, `default`, `auto-edit`, `yolo`) — `default`», «Base URL и API-ключ Qwen — нет (собственная настройка qwen)»; текст: «таблица `[qwen]` (`cli`, `model`, `approval`, `base_url`); ключ Qwen — в системном хранилище ключей (`agent-hub` / `qwen-api-key`)»; в таблице путей — «Токен бота, API-ключи OpenAI и Qwen».
- «Безопасность», новые пункты:
  - «**Инструменты хаба для Qwen** — HTTP-сервер только на `127.0.0.1`, отдельный случайный токен на каждую сессию, живёт столько же, сколько сессия.»
  - Рекомендация: «Ключ Qwen из настроек хаба передаётся процессу `qwen` переменной окружения — команды, которые запускает агент, могут его прочитать. Не используйте `yolo`, если это недопустимо; `http://` к другому компьютеру передаёт ключ открытым текстом.»

- [ ] **Step 3: Полная проверка**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add -A crates/hub-qwen README.md
git commit -m "Qwen: живые тесты и документация"
```

- [ ] **Step 5: Версия 0.3.0**

`Cargo.toml:6`: `version = "0.3.0"`. Затем `cargo check --workspace` (обновит версии членов workspace в `Cargo.lock`).

Run: `cargo test --workspace --locked && git diff --stat`
Expected: PASS; изменены только `Cargo.toml` и `Cargo.lock` (как в коммите `b62e81b`). Снимки `protocol` от версии не зависят (`initialize_params("test")`).

```bash
git add Cargo.toml Cargo.lock
git commit -m "Версия 0.3.0: бэкенд Qwen"
```

---

### Task 14: Ручная проверка на Linux и Windows с реальным ключом

Без изменений кода. Если что-то не так — исправление отдельным коммитом с тестом, воспроизводящим проблему, затем повтор шага.

- [ ] **Step 1: Живые тесты на каждой ОС**

Run: `AGENT_HUB_LIVE_QWEN=1 cargo test -p hub-qwen --test live -- --ignored --test-threads=1` (Windows PowerShell: `$env:AGENT_HUB_LIVE_QWEN=1; cargo test -p hub-qwen --test live -- --ignored --test-threads=1`)
Expected: 3 PASS. Падение `qwen_asks_through_the_hub_and_gets_the_answer` при пустом `questions` — модель не вызвала инструмент (повторить); при непустом `questions`, но без «син» в ответе — `answers` не доходит: остановиться и разобрать (раздел «Протокол», п. 1). Падение `qwen_asks_before_using_send_file` с пустым `requests` и ответом-файлом — Qwen не спрашивает разрешения: зафиксировать в README и спецификации, кнопка не появится.

- [ ] **Step 2: Linux**

1. `npm i -g @qwen-code/qwen-code`; `qwen --version` ≥ 0.25.0 (записать точный вывод — проверка разбора версии).
2. «Настройки → Qwen»: base URL и ключ (например DashScope), модель; «Сохранить». В `settings.toml` — `[qwen]` с `base_url`, без ключа; `secret-tool search service agent-hub` показывает `qwen-api-key`.
3. «Статус»: «Qwen Code 0.25.x · ключ из настроек хаба».
4. В теме: `/backend qwen` → «🔀 Бэкенд изменён … backend: qwen». Задача с командой («выведи список файлов»): строка инструмента с осмысленным заголовком (`execute · Shell: ls …`, не просто `Shell`), кнопки одобрения; «Разрешить» → ответ и «✅ Готово · токенов в сессии: N».
5. Во время хода, пока агент выполняет инструменты, отправить второе сообщение — Qwen учитывает его в том же ходе; в логе нет `qwen drain not answered in time`.
6. Попросить «задай мне уточняющий вопрос с вариантами» — вопрос с кнопками; ответ учтён Qwen.
7. «Пришли файл README.md» — кнопка одобрения `send_file`, после «Разрешить» файл в теме.
8. Фото с вопросом — Qwen его описывает (если модель поддерживает изображения).
9. `/stop` во время ожидания кнопки → «⏹ Остановлено»; `pgrep -af qwen` пусто через ≤ 10 с; `ss -ltnp | grep agent-hub` — порт MCP закрыт.
10. Перезапуск приложения и новое сообщение в той же теме — `session/load` продолжает разговор, старые ответы не дублируются в тему.
11. Очистить base URL и ключ, «Сохранить» → «Статус»: «· собственная настройка qwen»; без своей настройки qwen сообщение → «Qwen не авторизован: настройте qwen (/auth) или задайте API-ключ в настройках».
12. В логе (`~/.local/share/agent-hub/logs/`) нет ни ключа, ни строки `Bearer `: `rg -n "sk-|Bearer " ~/.local/share/agent-hub/logs` — пусто.

- [ ] **Step 3: Windows**

Те же шаги 1–12 с поправками: `qwen` из npm — это `qwen.cmd` (проверить, что найден в PATH и запускается без окна консоли); хранилище — «Диспетчер учётных данных» (`agent-hub` / `qwen-api-key`); после `/stop` в «Диспетчере задач» нет `node.exe` от qwen через ≤ 10 с (stdin закрыт → Qwen завершается сам; `kill` убивает только `cmd.exe`-обёртку — если `node.exe` остаётся, записать и разобрать отдельно).

- [ ] **Step 4: Регрессия Claude и Codex**

`/backend claude` и `/backend codex` в той же теме, по одной задаче — работают как в 0.2.0.

---

### Task 15: Релиз `v0.3.0`

> **Push тега и веток — только после отдельного явного подтверждения пользователя.** Агент, выполняющий план, останавливается перед шагом 3 и спрашивает.

- [ ] **Step 1: Проверка перед тегом**

Run: `git status --short && cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: чистое дерево, PASS; последний коммит — «Версия 0.3.0: бэкенд Qwen».

- [ ] **Step 2: Аннотированный тег (локально)**

```bash
git tag -a v0.3.0 -m "agent-hub 0.3.0: бэкенд Qwen"
git show --stat v0.3.0 | head -5
```

Expected: тег на коммите версии, сообщение в стиле `v0.2.0` («agent-hub 0.2.0: бэкенд Codex»).

- [ ] **Step 3: Push (только после подтверждения пользователя)**

```bash
git push origin main
git push origin v0.3.0
```

Expected: `release.yml` (`on: push: tags: ["v*"]`) собирает Windows, macOS (arm64, x86_64) и Linux; релиз появляется на GitHub. Если пользователь не подтвердил — остановиться после шага 2 и сообщить, что тег создан локально.

---

## Покрытие security-review и reliability-review

| Риск | Где закрыт |
|---|---|
| Чужой локальный процесс или веб-страница вызывает `send_file` | Задача 2: только `127.0.0.1`, токен ≥128 бит на сессию, `ConstantTimeEq`, проверка токена до маршрутизации; тесты 401 |
| DoS MCP-сервера (тело, медленные заголовки, много соединений) | Задача 2: 1 МиБ, 10 с на заголовки и тело, 16 соединений, пауза после ошибки `accept` |
| Утечка ключа/токена | Задачи 3 (`Debug`), 8 (не логируются; ключ только в окружении дочернего процесса), 10 (хранилище ключей), 13 (README: видимость ключа командам агента, `http://`), 14 (поиск в логах) |
| Ключ открытым текстом по сети | Задачи 3 (`is_cleartext_remote`), 12 (предупреждение в форме) |
| Зависший `qwen` или его потомки | Задача 8: `QWEN_CODE_NO_RELAUNCH`, `kill_on_drop`, stdin → 10 с → `kill`; задача 14: проверка процессов |
| Drain дольше 2 с → Qwen отключает досылку | Задача 6: ответ ≤1,5 с, цикл отвечает сразу; опоздавшие промпты удерживаются |
| Потеря промптов | Задача 6: не больше 10 за drain (Qwen отбрасывает лишнее), остальное — следующим ходом; `/stop` не забирает из inbox; inbox возвращается |
| Бесконечные ожидания | 60 с на управляющие вызовы, 2 с на `session/cancel`, 1,5 с на drain; ход ограничен `/stop`/выходом процесса (решение спецификации) |
| Устаревший `settings.toml` | Задача 3: `#[serde(default)] qwen`, тест чтения старого файла |
| Новые зависимости | Задача 2: `cargo deny check` (`httpdate` — MIT/Apache-2.0) |

Остаточные риски, принятые спецификацией: ключ в окружении `qwen` виден командам агента; ход без тайм-аута; при `kill` на Windows может остаться `node.exe` за обёрткой `qwen.cmd`, если Qwen не завершился по закрытию stdin (проверяется в задаче 14).

## Самопроверка плана

- Имена сквозь задачи: `Wire`/`Envelope`/`request_untimed`/`notify_with` (1) → `testing::pair`, `RpcAgent`, `QwenProcess::spawn` (6, 8); `McpHttpServer::{start, url, token}`, `SERVER_NAME` (2) → `protocol::mcp_servers`, `QwenBackend::run` (5, 8); `QwenSettings`/`ApiEndpoint`/`QwenApproval`/`QwenDraft`/`QwenField`/`Keys::qwen_key` (3) → 8, 10, 11, 12; `Drain`/`DrainRequests`/`drain_channel` (6) → `requests::answer` (7), `backend::handler` (8); `QwenAuth` (8) → `AgentAuth::Qwen` (11); `QWEN_FAILED`/`NOT_AUTHENTICATED` (8) → 9, 14.
- Формы на проводе в тестах совпадают с разделом «Протокол»: `mcpServers` http-запись, `answers` с ключами-индексами, `toolCall._meta.qwenQuestions`, `{"items": [...], "hasQueuedPrompt": false}`, `-32000`, `stopReason`, `_meta.usage.totalTokens`, `_meta.phase: "preparing"`.
- Временные ветки задачи 3 (`agents.rs`, `connector.rs`) заменяются в задачах 9 и 11; `Keys.qwen_key` из хранилища — в задаче 10.
- Коммиты — по-русски, без строк атрибуции; push — только в задаче 15 после подтверждения.
