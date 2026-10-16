# Лимиты подписок — план реализации

> **For the implementer:** Use `executing-plans` (или superpowers:subagent-driven-development) to execute this plan task-by-task. Шаги размечены чекбоксами (`- [ ]`).

**Goal:** команда `/limits` (в теме и в общем чате группы) показывает карточку с израсходованными лимитами подписок Claude и Codex — процент по окну, полоса из 10 делений, время сброса, тариф; кнопка «🔄 Обновить» перерисовывает то же сообщение; при сбое пробы — последние данные из живых сессий с пометкой «(на ЧЧ:ММ)»; релиз 0.6.0.

**Architecture:** `hub-core::quota` — чистые типы (`WindowKind`, `UsageWindow`, `QuotaSource`, `QuotaReport`, `QuotaUpdate`, `clean_label`, `percent`), `AgentEvent::Quota(QuotaUpdate)`; `hub-core::render::render_limits` — чистая отрисовка карточки (HTML, всё из ответов агентов экранируется). `hub-claude::usage` — разбор ответа `get_usage` и `rate_limit_event`, проба: короткий `claude` в stream-json без пользовательского сообщения, один управляющий запрос `get_usage`, ограниченное чтение stdout, закрытие stdin, ожидание ≤ 5 с, `kill`; сессия Claude превращает `rate_limit_event` в `AgentEvent::Quota`. `hub-codex::usage` — разбор `account/rateLimits/read`/`updated` и проба: короткий `codex app-server`, `initialize` → `initialized` → `account/read` → `account/rateLimits/read` (без входа по ключу и без `thread/start`); `TurnTracker` превращает `account/rateLimits/updated` в `AgentEvent::Quota`. `hub-telegram::limits` — кэш квот по бэкендам (последнее состояние и время, слияние разреженных обновлений), выбор «свежее или кэш», порядок «бэкенд по умолчанию первым», кнопка «Обновить»; `Agents::quotas` у `HubAgents` параллельно опрашивает найденные CLI с тайм-аутом 20 с каждая; хаб владеет кэшем, держит не больше одного опроса за раз и редактирует сообщение-заглушку «⏳ Собираю…» в карточку.

**Tech stack:** Rust 2024 (1.96), tokio (`process`, `time`, `io-util`), serde/serde_json, chrono 0.4.45 (`clock`, `std` — уже в `Cargo.lock` с `iana-time-zone`/`windows-link`; часовой пояс компьютера — `chrono::Local`, `chrono-tz` не нужен и в lock отсутствует), futures (`join_all`), thiserror; тесты — rstest, `tokio::io::duplex` вместо поддельного процесса (одинаково на Linux, macOS и Windows), `#[ignore]` живые тесты.

**Spec:** `docs/superpowers/specs/2026-10-16-subscription-limits-design.md` (решения не меняются; расхождения с исходниками агентов и кодом — ниже).

**Constraints:**
- Линты рабочего пространства (`Cargo.toml:56-67`): `warnings = "deny"`, `unsafe_code = "forbid"`, `clippy::pedantic = deny`, `unwrap_used`/`expect_used`/`panic`/`indexing_slicing = deny` вне тестов (`clippy.toml` разрешает их в тестовых функциях и `#[cfg(test)]`-модулях). Никаких `#[allow]` в продакшен-коде, никаких `_ =>` на собственных enum (`WindowKind`, `QuotaSource`, `BackendKind`, `Incoming`, `HubMessage`, `ProbeError`…), никакой индексации срезов, никаких `as`-приведений чисел (`cast_*` входят в pedantic): `f64 → u8` — через `percent`, `i32 → u8` — `u8::try_from(clamp)`.
- Гейты после каждой задачи (перед ними — `cargo fmt --all`: блоки кода плана отформатированы не везде): `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`. CI гоняет clippy и тесты на Linux, macOS и Windows (уроки 0.5.0): тесты не опираются на `/`-пути и права файлов; поддельные агенты — пары `tokio::io::duplex`, а не shell-скрипты; импорт, нужный только под `cfg(unix)`/`cfg(windows)`, ставится внутрь функции под тем же `cfg`, а не наверх файла.
- Задачи, меняющие `Cargo.toml` (2, 5, 8), сначала выполняют `cargo check --workspace --all-targets` (без `--locked`) — он дописывает рёбра в `Cargo.lock`; `git diff Cargo.lock` должен показывать только списки `dependencies` наших крейтов, **без новых пакетов** (`chrono`, `serde`, `futures` уже в lock). Затем `cargo deny check`, если `cargo-deny` установлен.
- Безопасность (`security-review`): хаб не читает, не хранит и не передаёт токены агентов (`.credentials.json`, `auth.json`, связка ключей) и не вызывает `api.anthropic.com/api/oauth/usage` — только сами CLI. Ответы проб — недоверенные данные: строка stdout пробы Claude ≤ 1 МиБ и ≤ 64 строк; строки из ответов (тариф, имена окон, текст отказа) проходят `clean_label` (без управляющих символов, ≤ 32 символов) и экранируются в `render_limits`; окон на бэкенд ≤ 8. В лог — только бэкенд, вид ошибки (`ProbeError::kind`) и проценты; без путей, текста ответов и stderr (stderr пробы Claude — `Stdio::null()`).
- Надёжность (`reliability-review`): у пробы Claude — тайм-аут ответа 15 с, затем закрытие stdin, ожидание выхода ≤ 5 с, `kill`; `kill_on_drop(true)` у обоих проб; внешний тайм-аут 20 с на пробу в `HubAgents`; пробы параллельны, ошибка или тайм-аут одного бэкенда не влияет на другие; пробы не создают ходов и сессий (Claude — без пользовательского сообщения, Codex — без `thread/start`, без `account/login/start`); у хаба не больше одного опроса за раз, повторное «Обновить» во время опроса игнорируется; кэш — не больше 4 записей × 8 окон; паника опроса возвращает хабу пустой результат (счётчик опросов не залипает).
- Тексты для пользователя — по-русски; дословные из спецификации (карточка, «⏳ Собираю…», «🔄 Обновить», «квоты подписки агент не сообщает», «вход по API-ключу — лимитов подписки нет»), новые — перечислены в задаче 4.
- Код в плане написан против текущих API, но не компилировался. Если `clippy::pedantic` просит `#[must_use]`, ссылку вместо значения, `# Errors`, бэктики (`doc_markdown`) и т. п. — исправить код по линту, не подавляя его, сохраняя имена и сигнатуры из блоков **Interfaces**.
- Коммит после каждой задачи; сообщения по-русски в стиле истории репозитория (`hub-claude: …`, `Хаб …`, `Версия 0.6.0: …`); **никаких строк атрибуции ИИ** (`Co-Authored-By`, «Generated with …», упоминаний Claude как автора). Ветка — текущая `docs/subscription-limits`; план её не меняет. `git push` и push тега — только в задаче 15 после отдельного подтверждения пользователя.

**Non-goals:** Telegram Mini App; Rich Messages Bot API 10.x; квоты Qwen и Hermes; уведомления о приближении к лимиту; фоновый опрос; показ в GUI; `rateLimitsByLimitId` Codex (карточка показывает только `rateLimits`, «совместимое одноведёрное» представление); кредиты Codex (`credits`) и `individualLimit`; раздел `behaviors` и `session` ответа `get_usage` (`skip_behaviors: true`); проверка версии CLI перед пробой (старый CLI просто ответит ошибкой → кэш); сохранение кэша между запусками.

---

## Расхождения со спецификацией

Решения спецификации сохранены; ниже — места, где её текст не совпадает с исходниками агентов или кодом репозитория, и как план это учитывает. Источники — раздел «Протокол: проверено по исходникам».

1. **`get_usage` — ответ вложен дважды и почти всё допускает `null`.** Ответ CLI — `{"type":"control_response","response":{"subtype":"success","request_id":…,"response":{…}}}` (`SDKControlResponse`, `ControlResponse.response?: Record<string, unknown>`); полезная нагрузка — `response.response`. Окна `five_hour`/`seven_day`/… сами `| null`, у каждого `utilization: number | null`, `resets_at: string | null`. У `extra_usage` **нет** `resets_at` (`is_enabled`, `monthly_limit`, `used_credits`, `utilization`, `currency?`) — окно `Extra` без времени сброса. Элемент `model_scoped[]` — `{display_name, utilization, resets_at}`. Окно с `utilization: null` пропускается.
2. **`rate_limit_event`.** `resetsAt?: number` — единица не документирована (у Codex — Unix-секунды); план принимает целое и считает его миллисекундами, если оно больше `10^11` (секунды 2026 года ≈ 1,8·10⁹, миллисекунды ≈ 1,8·10¹²); дробное значение — без времени сброса. `rateLimitType` кроме перечисленных в спецификации имеет `seven_day_overage_included`. Записанные транскрипты репозитория (`crates/hub-claude/tests/fixtures/*.jsonl`, CLI 2.1.28x) содержат только `{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"}}` — без `utilization` и `rateLimitType`; такое событие обновлением не считается. Значит, кэш Claude из живых сессий может оставаться пустым — тогда при сбое пробы карточка показывает «⚠️ <причина>». Единица `utilization` — переключатель `EVENT_SCALE` (задача 1).
3. **Codex `account/rateLimits/read` шире спецификации.** Ответ: `ordinaryUsageAllowed`, `rateLimits`, `rateLimitsByLimitId`, `rateLimitResetCredits`, `accountId`, `rateLimitUpsell`. `RateLimitSnapshot`: `limitId`, `limitName`, `normalModelSlug`, `primary`, `secondary`, `credits`, `individualLimit`, `spendControlReached`, `planType`, `rateLimitReachedType` — все `Option`. `RateLimitWindow`: `usedPercent: i32`, `windowDurationMins: i64 | null`, `resetsAt: i64 | null`. `credits.balance` — **строка** (`Option<String>`), не число. `planType` — строки в нижнем регистре (`free`, `go`, `plus`, `pro`, `prolite`, `team`, `business`, `enterprise`, `edu`, `self_serve_business_usage_based`, …). Разбор берёт только `rateLimits.{primary, secondary, planType}`.
4. **Подписи окон Codex — как в карточке спецификации:** `primary` → «основное», `secondary` → «недельное», независимо от `windowDurationMins`. Окна Claude: `five_hour` → «5 часов», `seven_day` → «7 дней», `seven_day_opus`/`seven_day_sonnet` → «7 дней Opus»/«7 дней Sonnet», `model_scoped[i]` → «7 дней <display_name>», `seven_day_oauth_apps` → «7 дней, приложения», `seven_day_overage_included` → «7 дней, включённые модели», `extra_usage`/`overage` → «сверх подписки». Тариф: Claude `subscription_type` → с заглавной буквы (`max` → `Max`); Codex `planType` → `ChatGPT <Имя>` (`plus` → `ChatGPT Plus`, `prolite` → `ChatGPT Pro Lite`, неизвестное — как есть после `clean_label`).
5. **Проба Claude изолирована от настроек пользователя.** SDK в режиме изоляции (`settingSources: []`) передаёт `--setting-sources=` (`sdk.mjs`, функция `Be` → `--flag=value`), `strictMcpConfig` — `--strict-mcp-config`. Проба запускается с ними и без `--mcp-config`, в `std::env::temp_dir()`: иначе на каждый `/limits` стартовали бы MCP-серверы и хуки `SessionStart` пользователя. Вход claude.ai хранится не в настройках, поэтому ответ не меняется — подтверждает задача 1. Флаги пробы — константа `PROBE_ARGS`.
6. **Порядок рукопожатия Claude — переключатель.** `Handshake::{Direct, InitializeFirst, CacheOnly}`, константа `HANDSHAKE` (задача 6) выставляется по итогу задачи 1. `CacheOnly` — проба не запускается, `/limits` для Claude сразу берёт кэш. SDK сам всегда сначала шлёт `initialize`, поэтому значение по умолчанию до живой проверки — `InitializeFirst` (самое безопасное звено цепочки спецификации); если задача 1 покажет, что `Direct` работает, константа меняется на `Direct` в задаче 6. Других мест, зависящих от итога задачи 1, нет (кроме `EVENT_SCALE` и, возможно, `-p` в `PROBE_ARGS`).
7. **Проба Codex не входит по ключу.** `probe_auth` (`crates/hub-codex/src/backend.rs:215-236`) вызывает `account/login/start` с ключом из настроек и переписывает `auth.json`; проба лимитов — только `initialize`, `initialized`, `account/read`, затем `account/rateLimits/read`. Вход не через ChatGPT (`apiKey`, иной, «не требуется») → окончательный ответ «вход по API-ключу — лимитов подписки нет», вход отсутствует → «Codex не авторизован — лимитов подписки нет». Claude с `rate_limits_available: false` → «вход не через подписку claude.ai — лимитов подписки нет». Эти **окончательные** ответы (`QuotaSource::Unavailable` в `Ok`) не заменяются кэшем; кэш — только при ошибке пробы или тайм-ауте (`Err`), как в спецификации.
8. **`Messenger::edit` снимает клавиатуру** (`crates/hub-telegram/src/messenger.rs:67`). Карточка — два вызова: `edit(…, Format::Html)` и `edit_keyboard(…, [[🔄 Обновить]])`; «Обновить» сначала редактирует сообщение в «⏳ Собираю…» (клавиатура исчезает — второе нажатие физически невозможно, а пришедшее раньше игнорируется хабом).
9. **Один опрос за раз на весь хаб** — сильнее «повторное нажатие игнорируется» из спецификации: каждый опрос запускает до двух процессов. `/limits` во время опроса отвечает новым текстом `LIMITS_BUSY`; «Обновить» во время опроса — ответ на callback без текста, без изменений.
10. **Размещение.** Типы — новый модуль `hub_core::quota`, `render_limits` — `hub_core::render`; `hub-core` получает зависимость `chrono` (уже в lock). Сигнатура `render_limits<Tz: TimeZone>(&[QuotaReport], DateTime<Utc>, &Tz)`: продакшен передаёт `&chrono::Local` (часовой пояс компьютера), тесты — `FixedOffset`. Новый вариант `AgentEvent::Quota(QuotaUpdate)`; новый метод `Agents::quotas`; `HubMessage::{Quota, Limits}` (а значит, новые ветки в `crates/hub-app/src/supervisor.rs:483-490`). `HubAgents` не вызывает `version::check` перед пробой.
11. **Мелочи карточки.** Кэшированные данные — «(на ЧЧ:ММ)», если не сегодня — «(на ДД.ММ ЧЧ:ММ)»; окно без данных у бэкенда — строка «нет данных»; ни одного найденного CLI — «Ни один CLI агента не найден». `/limits` в реестре команд — после `/status` (меню и `/help`).
12. **«get_usage (CLI ≥ 2.1.169)»** по `sdk.d.ts` не проверяется (версии там нет); не важно — `MIN_VERSION` Claude = 2.1.280 (`crates/hub-claude/src/version.rs:10`).

## Протокол: проверено по исходникам

**Claude Agent SDK** `@anthropic-ai/claude-agent-sdk@0.3.301` (`package.json`: `claudeCodeVersion: "2.1.301"`), `https://unpkg.com/@anthropic-ai/claude-agent-sdk@0.3.301/sdk.d.ts` (sha256 `26f9a0c91ad64e20…`), загружено 2026-10-16:
- `SDKControlGetUsageRequest` (`sdk.d.ts:4315-4321`): `{subtype: 'get_usage'; skip_behaviors?: boolean}`. Метод SDK `usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET` (`:3086-3104`): «EXPERIMENTAL: this API is unstable and may change or be removed in any release without notice»; в `sdk.mjs` — `this.request({subtype:"get_usage", ...skipBehaviors && {skip_behaviors:!0}})`, результат — `.response`.
- Обёртка (`:5086-5091`, `:5137-5140`, `:420-429`, `:396-406`): запрос `{"type":"control_request","request_id":<id>,"request":{…}}`; ответ `{"type":"control_response","response":{"subtype":"success","request_id":<id>,"response":{…}}}` или `{"type":"control_response","response":{"subtype":"error","request_id":<id>,"error":"<текст>"}}`.
- `SDKControlGetUsageResponse` (`:4326-4534`): `session {…}`; `subscription_type: string | null` («'pro', 'max', 'team', 'enterprise' or null for API key / 3P»); `rate_limits_available: boolean` («False when plan rate limits do not apply (API key, Bedrock, Vertex, or missing profile scope) — rate_limits will be null»); `rate_limits: {five_hour?, seven_day?, seven_day_oauth_apps?, seven_day_opus?, seven_day_sonnet?: {utilization: number | null /* 0-100 */; resets_at: string | null /* ISO 8601 */} | null; model_scoped?: {display_name: string; utilization: number | null; resets_at: string | null}[]; extra_usage?: {is_enabled: boolean; monthly_limit: number | null; used_credits: number | null; utilization: number | null; currency?: string | null} | null} | null`; `behaviors` (null при `skip_behaviors`); `session_id: string`.
- `SDKRateLimitEvent` (`:5756-5763`): `{type: 'rate_limit_event'; rate_limit_info: SDKRateLimitInfo; uuid; session_id}`. `SDKRateLimitInfo` (`:5769-5792`): `status: 'allowed' | 'allowed_warning' | 'rejected'`; `resetsAt?: number`; `rateLimitType?: 'five_hour' | 'seven_day' | 'seven_day_opus' | 'seven_day_sonnet' | 'seven_day_overage_included' | 'overage'`; `utilization?: number` (единица не указана); плюс `overage*`, `isUsingOverage`, `surpassedThreshold`, `limitScope`, … — не используются.
- Изоляция: `settingSources` (`:2290-2297`, «Pass `[]` to disable filesystem settings (SDK isolation mode)»), `strictMcpConfig` (`:2344-2346`, «Maps to the CLI `--strict-mcp-config` flag»); в `sdk.mjs` — `G.push(Be("--setting-sources", L.join(",")))`, `function Be(e,t){return`${e}=${t}`}`, базовые флаги `["--output-format","stream-json","--verbose","--input-format","stream-json"]`.

**Codex app-server protocol v2**, `openai/codex@4eb4a65407d98cebc4c83bf13be440b9fbc78a7c` (main, 2026-10-16):
- `codex-rs/app-server-protocol/src/protocol/common.rs:1341-1344`: `GetAccountRateLimits => "account/rateLimits/read"`, параметры `Option<GetAccountRateLimitsParams>`; `:2020`: `AccountRateLimitsUpdated => "account/rateLimits/updated"`; `:1475`: `"account/read"`.
- `codex-rs/app-server-protocol/src/protocol/v2/account.rs` (sha256 `f52c03c9f98f8fd4…`), все структуры `#[serde(rename_all = "camelCase")]`:
  - `GetAccountRateLimitsParams` (`:315-324`): `supportsLunaReserve: bool`, `excludeResetCreditDetails: bool` (оба по умолчанию `false`, «Skip the separate reset-credit detail lookup for background usage polls»).
  - `GetAccountRateLimitsResponse` (`:331-345`): `ordinaryUsageAllowed: Option<bool>`, `rateLimits: RateLimitSnapshot`, `rateLimitsByLimitId: Option<HashMap<String, RateLimitSnapshot>>`, `rateLimitResetCredits`, `accountId: Option<String>`, `rateLimitUpsell: Option<Value>`.
  - `AccountRateLimitsUpdatedNotification` (`:650-659`): `{rateLimits: RateLimitSnapshot}` — «Sparse rolling rate-limit update. Clients should merge available values into the most recent `account/rateLimits/read` response… Nullable account metadata may be unavailable in a rolling update and does not clear a previously observed value».
  - `RateLimitSnapshot` (`:664-677`): `limitId`, `limitName`, `normalModelSlug: Option<String>`, `primary`, `secondary: Option<RateLimitWindow>`, `credits: Option<CreditsSnapshot>`, `individualLimit`, `spendControlReached: Option<bool>`, `planType: Option<PlanType>`, `rateLimitReachedType`.
  - `RateLimitWindow` (`:752-758`): `usedPercent: i32` (из `f64` через `round()`), `windowDurationMins: Option<i64>`, `resetsAt: Option<i64>`.
  - `CreditsSnapshot` (`:773-777`): `hasCredits: bool`, `unlimited: bool`, `balance: Option<String>`.
- `codex-rs/protocol/src/account.rs:9-42`: `PlanType` — `#[serde(rename_all = "lowercase")]` (`free`, `go`, `plus`, `pro`, `prolite`, `promax`, `team`, `business`, `enterprise`, `edu`, …) с явными `self_serve_business_prolite`, `enterprise_cbp_usage_based`, `edu_plus` и т. п.

**Не проверено (живой проверкой задачи 1):** отвечает ли CLI на `get_usage` без пользовательского сообщения и без/после `initialize`; единица `utilization` и `resetsAt` в `rate_limit_event`; не пишет ли проба транскрипт в `~/.claude/projects/`; работает ли вход при `--setting-sources=`. Записанных ответов `get_usage`/`account/rateLimits/read` в репозитории нет — фикстуры задач 5 и 8 построены по типам выше; задача 1 сохраняет живые ответы, и задачи 5/8 заменяют ими фикстуры, если они есть.

## Review Focus

1. **Изоляция пробы Claude** (задача 6): нет пользовательского сообщения, нет `--resume`, `--setting-sources=`, `--strict-mcp-config`, stderr — `null`, чтение ≤ 64 строк по ≤ 1 МиБ, тайм-аут 15 с, закрытие stdin, ≤ 5 с ожидания, `kill`.
2. **Проба Codex без побочных эффектов** (задача 8): ни `account/login/start`, ни `thread/start`; тест сверяет список методов.
3. **Недоверенные строки** (задачи 2, 3, 5, 8): `clean_label` на входе, `escape` в `render_limits`, лимиты длины и числа окон.
4. **Кэш и окончательные ответы** (задачи 10, 12): кэш подменяет только `Err`; `Unavailable` в `Ok` показывается как есть; разреженные обновления Codex не стирают тариф и окна.
5. **Хаб** (задача 12): один опрос за раз, паника опроса не оставляет хаб «занятым», «Обновить» редактирует то же сообщение.

## Порядок задач

| # | Задача | Крейты | Зависит от |
|---|---|---|---|
| 1 | **Живая проверка `get_usage` и `rate_limit_event`** (человек; агентов здесь нет) | — | — |
| 2 | Типы квот, `AgentEvent::Quota` | hub-core, hub-telegram | — |
| 3 | `render_limits` — карточка | hub-core | 2 |
| 4 | `Command::Limits` и тексты | hub-core, hub-telegram | 2 |
| 5 | Разбор `get_usage` и `rate_limit_event` | hub-claude | 2 |
| 6 | Проба Claude (`usage::probe`) | hub-claude | 1, 5 |
| 7 | Сессия Claude отдаёт `rate_limit_event` хабу | hub-claude | 5 |
| 8 | Разбор `account/rateLimits/*` и проба Codex | hub-codex | 2 |
| 9 | Сессия Codex отдаёт `account/rateLimits/updated` хабу | hub-codex | 8 |
| 10 | Кэш квот и выбор «свежее или кэш» | hub-telegram | 2 |
| 11 | `Agents::quotas` и `HubAgents`: пробы найденных CLI | hub-telegram | 6, 8 |
| 12 | Хаб: `/limits`, «Обновить», кэш из сессий | hub-telegram, hub-app | 3, 4, 10, 11 |
| 13 | Живые тесты, README, версия 0.6.0 | hub-claude, hub-codex, README, Cargo | 12 |
| 14 | Ручная проверка (человек) | — | 13 |
| 15 | Релиз `v0.6.0` (push — только после подтверждения) | — | 14 |

---
### Task 1: Живая проверка `get_usage` и `rate_limit_event`

> **Выполняет человек** (или агент на машине, где установлен и вошёл в подписку claude.ai `claude` ≥ 2.1.280). Здесь агентов нет (`which claude codex` пусто). Код не меняется, коммита нет. Результат — три значения для задачи 6 и задачи 5 плюс, если есть, живые ответы для фикстур. **Задачи 2–5 и 7–13 от результата не зависят**; задачу 6 можно выполнить и до проверки с `HANDSHAKE = InitializeFirst` — тогда после проверки меняются только константы.

**Files:** нет (выход — во временный каталог `$OUT`).

- [ ] **Step 1: Подготовка**

Команды шагов 1–6 — для `bash` (в `zsh` `$FLAGS` не делится на слова): сначала `bash`.

```bash
claude --version            # ожидается ≥ 2.1.280
OUT=$(mktemp -d) && echo "$OUT"
touch "$OUT/marker"
FLAGS='--input-format stream-json --output-format stream-json --verbose --setting-sources= --strict-mcp-config'
USAGE='{"type":"control_request","request_id":"u1","request":{"subtype":"get_usage","skip_behaviors":true}}'
INIT='{"type":"control_request","request_id":"i1","request":{"subtype":"initialize","hooks":null}}'
cd "$(mktemp -d)"            # как проба: каталог без проекта
```

- [ ] **Step 2: `get_usage` без `initialize` и без сообщения (`Direct`)**

stdin держится открытым 10 с, иначе CLI может выйти по EOF раньше ответа.

```bash
{ printf '%s\n' "$USAGE"; sleep 10; } | timeout 20 claude -p $FLAGS > "$OUT/direct.jsonl" 2> "$OUT/direct.err"; echo "exit=$?"
grep -c '"request_id":"u1"' "$OUT/direct.jsonl"
```

Expected: одна строка `{"type":"control_response","response":{"subtype":"success","request_id":"u1","response":{…"rate_limits":{…}}}}`. Записать: есть ли ответ, его `subtype`, время до ответа.

- [ ] **Step 3: `initialize`, затем `get_usage` (`InitializeFirst`), с `-p` и без**

```bash
{ printf '%s\n%s\n' "$INIT" "$USAGE"; sleep 10; } | timeout 20 claude -p $FLAGS > "$OUT/init-p.jsonl" 2> "$OUT/init-p.err"; echo "exit=$?"
{ printf '%s\n%s\n' "$INIT" "$USAGE"; sleep 10; } | timeout 20 claude $FLAGS > "$OUT/init.jsonl" 2> "$OUT/init.err"; echo "exit=$?"
grep -c '"request_id":"u1"' "$OUT/init-p.jsonl" "$OUT/init.jsonl"
```

Expected: ответ `request_id: "u1"` хотя бы в одном варианте. Записать, в каких.

- [ ] **Step 4: Проба не оставляет транскриптов и не запускает хуки**

```bash
find ~/.claude/projects -newer "$OUT/marker" -name '*.jsonl' | wc -l    # ожидается 0
grep -ci 'hook' "$OUT"/*.jsonl                                          # ожидается 0 сообщений hook_*
```

Если файл появился — записать его путь (не содержимое) и проверить, не создаёт ли его только вариант с `initialize`.

- [ ] **Step 5: Единица `utilization` и `resetsAt` в `rate_limit_event`**

Короткий настоящий ход (тратит немного токенов) и сразу `get_usage` для сравнения:

```bash
MSG='{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Ответь одним словом: да"}]},"parent_tool_use_id":null}'
{ printf '%s\n' "$MSG"; sleep 40; printf '%s\n' "$USAGE"; sleep 5; } | timeout 60 claude -p $FLAGS > "$OUT/turn.jsonl" 2> "$OUT/turn.err"
grep '"rate_limit_event"' "$OUT/turn.jsonl"
grep '"request_id":"u1"' "$OUT/turn.jsonl" | python3 -c 'import json,sys; r=json.loads(sys.stdin.read())["response"]["response"]["rate_limits"]; print({k: (v or {}).get("utilization") for k, v in r.items() if isinstance(v, dict)})'
```

Expected: у `rate_limit_event` с `rateLimitType` и `utilization` сравнить число с `utilization` того же окна из `get_usage` (0–100): совпадает — `Scale::Percent`; меньше в ~100 раз — `Scale::Fraction`. `resetsAt` ≈ 1,8·10⁹ — секунды, ≈ 1,8·10¹² — миллисекунды (план принимает оба). Если событие пришло только со `status` — записать «единица не наблюдалась», оставить `Scale::Fraction` (расхождение 2).

- [ ] **Step 6 (необязательно): живой ответ Codex**

На машине с `codex` ≥ 0.160 и входом через ChatGPT:

```bash
{ printf '%s\n' '{"id":1,"method":"initialize","params":{"clientInfo":{"name":"agent-hub","title":"agent-hub","version":"0.6.0"}}}' '{"method":"initialized"}' '{"id":2,"method":"account/rateLimits/read","params":{"excludeResetCreditDetails":true}}'; sleep 10; } | timeout 20 codex app-server > "$OUT/codex.jsonl" 2> /dev/null
grep '"id":2' "$OUT/codex.jsonl"
```

Expected: `{"id":2,"result":{"rateLimits":{"primary":{"usedPercent":…,"windowDurationMins":…,"resetsAt":…},…,"planType":"plus"},…}}`.

- [ ] **Step 7: Записать решение**

| Наблюдение | Значение в коде (задача 6/5) |
|---|---|
| Шаг 2 ответил | `HANDSHAKE = Handshake::Direct` |
| Шаг 2 молчит, шаг 3 ответил | `HANDSHAKE = Handshake::InitializeFirst` |
| Ни один не ответил | `HANDSHAKE = Handshake::CacheOnly` (только кэш из живых сессий) |
| Ответил только вариант с `-p` | добавить `"-p"` первым элементом `PROBE_ARGS` |
| Ответил только вариант без `-p` | `PROBE_ARGS` без `-p` (как в плане) |
| `utilization` события — 0–100 | `EVENT_SCALE = Scale::Percent` |
| `utilization` события — 0–1 или не наблюдалась | `EVENT_SCALE = Scale::Fraction` |
| Шаг 4 нашёл транскрипт | вариант, создающий файл, не выбирать; если создают оба — `CacheOnly` и сообщить пользователю |

Живые ответы для фикстур: из строки `u1` взять `response.response`, оставить только `subscription_type`, `rate_limits_available`, `rate_limits` (без `session`, `behaviors`, `session_id`), сохранить как `crates/hub-claude/tests/fixtures/get_usage.json` в задаче 5; из `codex.jsonl` — `result` как `crates/hub-codex/tests/fixtures/rate_limits_read.json` в задаче 8 (заменить `accountId` на `"a"`). Временный каталог `$OUT` затем удалить: в нём сведения об аккаунте.

---
### Task 2: Типы квот и `AgentEvent::Quota` (`hub-core`)

**Files:**
- Create: `crates/hub-core/src/quota.rs`
- Modify: `crates/hub-core/Cargo.toml:9-14` (зависимость `chrono`)
- Modify: `crates/hub-core/src/lib.rs:8` (`pub mod quota;` после `pub mod questions;`)
- Modify: `crates/hub-core/src/domain.rs:1-3` (импорт), `:280-288` (`AgentEvent`)
- Modify: `crates/hub-telegram/src/session.rs:136-161` (временная ветка в `relay`)
- Test: `crates/hub-core/src/quota.rs` (модуль `tests`)

**Interfaces:**
- Produces (`hub_core::quota`): `LABEL_LIMIT: usize = 32`, `WINDOW_LIMIT: usize = 8`; `enum WindowKind { FiveHour, Week, WeekModel(String), Extra, Other(String) }` (`Debug, Clone, PartialEq, Eq, Hash`); `struct UsageWindow { kind: WindowKind, percent: u8, resets_at: Option<DateTime<Utc>> }`; `enum QuotaSource { Fresh, Cached { at: DateTime<Utc> }, NotReported, Unavailable(String) }`; `struct QuotaReport { backend: BackendKind, plan: Option<String>, windows: Vec<UsageWindow>, source: QuotaSource }` с `fresh(BackendKind, Option<String>, Vec<UsageWindow>)`, `not_reported(BackendKind)`, `unavailable(BackendKind, impl Into<String>)`; `struct QuotaUpdate { backend: BackendKind, plan: Option<String>, windows: Vec<UsageWindow> }`; `fn clean_label(&str) -> Option<String>`; `fn percent(f64) -> Option<u8>`.
- Produces (`hub_core::domain`): `AgentEvent::Quota(QuotaUpdate)`.

- [ ] **Step 1: Тесты (RED)**

Создать `crates/hub-core/src/quota.rs` только с модулем тестов:

```rust
#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain("Max", Some("Max"))]
    #[case::trimmed("  Fable  ", Some("Fable"))]
    #[case::controls("Op\u{1b}[31mus\n", Some("Op[31mus"))]
    #[case::empty(" \t ", None)]
    #[case::long("абвгдежзийклмнопрстуфхцчшщъыьэюя-ещё", Some("абвгдежзийклмнопрстуфхцчшщъыьэю…"))]
    fn labels_are_cleaned_and_bounded(#[case] raw: &str, #[case] expected: Option<&str>) {
        assert_eq!(clean_label(raw).as_deref(), expected);
    }

    #[rstest]
    #[case(0.0, Some(0))]
    #[case(62.4, Some(62))]
    #[case(99.5, Some(100))]
    #[case(-3.0, Some(0))]
    #[case(250.0, Some(100))]
    #[case(f64::NAN, None)]
    #[case(f64::INFINITY, None)]
    fn percent_is_clamped_and_rounded(#[case] value: f64, #[case] expected: Option<u8>) {
        assert_eq!(percent(value), expected);
    }

    #[test]
    fn a_fresh_report_keeps_at_most_eight_windows() {
        let windows = (0..12)
            .map(|i| UsageWindow {
                kind: WindowKind::Other(format!("w{i}")),
                percent: 1,
                resets_at: None,
            })
            .collect();
        let report = QuotaReport::fresh(BackendKind::Claude, None, windows);
        assert_eq!(report.windows.len(), WINDOW_LIMIT);
        assert_eq!(report.source, QuotaSource::Fresh);
    }
}
```

В `crates/hub-core/src/lib.rs` после `pub mod questions;` (`:8`) — `pub mod quota;`.

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-core --lib -- quota`
Expected: FAIL — ошибки компиляции: нет `clean_label`, `percent`, `UsageWindow`, `QuotaReport`, нет крейта `chrono`.

- [ ] **Step 3: Реализация**

`crates/hub-core/Cargo.toml`, `[dependencies]` (`:9`), первой строкой:

```toml
chrono.workspace = true
```

Начало `crates/hub-core/src/quota.rs` (над модулем тестов):

```rust
//! Subscription limits the agents report, as plain data for the `/limits` card.
//!
//! Strings come from the agents' answers: parsers pass them through `clean_label`, and
//! `render::render_limits` escapes them.

use chrono::{DateTime, Utc};

use crate::domain::BackendKind;
use crate::render::truncate;

/// Longest plan, window name or refusal taken from an agent's answer.
pub const LABEL_LIMIT: usize = 32;
/// Most windows kept per backend, so the card always fits one Telegram message.
pub const WINDOW_LIMIT: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WindowKind {
    FiveHour,
    Week,
    /// A weekly window of one model, e.g. `Opus`.
    WeekModel(String),
    /// Usage beyond the plan, paid separately.
    Extra,
    /// A window shown under the given name, e.g. Codex `основное`.
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageWindow {
    pub kind: WindowKind,
    /// 0–100.
    pub percent: u8,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaSource {
    /// Just asked the agent.
    Fresh,
    /// The last values live sessions reported, as of `at`.
    Cached { at: DateTime<Utc> },
    /// The agent's protocol has no subscription limits.
    NotReported,
    /// Nothing to show; the reason is shown instead.
    Unavailable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaReport {
    pub backend: BackendKind,
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
    pub source: QuotaSource,
}

impl QuotaReport {
    #[must_use]
    pub fn fresh(backend: BackendKind, plan: Option<String>, windows: Vec<UsageWindow>) -> Self {
        let windows = windows.into_iter().take(WINDOW_LIMIT).collect();
        Self { backend, plan, windows, source: QuotaSource::Fresh }
    }

    #[must_use]
    pub fn not_reported(backend: BackendKind) -> Self {
        Self { backend, plan: None, windows: Vec::new(), source: QuotaSource::NotReported }
    }

    #[must_use]
    pub fn unavailable(backend: BackendKind, reason: impl Into<String>) -> Self {
        let source = QuotaSource::Unavailable(reason.into());
        Self { backend, plan: None, windows: Vec::new(), source }
    }
}

/// A rolling update from a live session: the windows it names replace the cached ones, a
/// missing plan keeps the cached plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaUpdate {
    pub backend: BackendKind,
    pub plan: Option<String>,
    pub windows: Vec<UsageWindow>,
}

/// Agent text fit for a card: no control characters, trimmed, at most `LABEL_LIMIT` chars.
#[must_use]
pub fn clean_label(raw: &str) -> Option<String> {
    let printable: String = raw.chars().filter(|c| !c.is_control()).collect();
    let trimmed = printable.trim();
    (!trimmed.is_empty()).then(|| truncate(trimmed, LABEL_LIMIT))
}

/// A percentage clamped to 0–100 and rounded; `None` for NaN and infinities.
#[must_use]
pub fn percent(value: f64) -> Option<u8> {
    if !value.is_finite() {
        return None;
    }
    let rounded = value.clamp(0.0, 100.0).round();
    // No float-to-int cast: the lints deny them, and 101 steps are nothing.
    (0..=100_u8).find(|step| f64::from(*step) >= rounded)
}
```

`crates/hub-core/src/domain.rs`: после `use rust_decimal::Decimal;` (`:3`) — `use crate::quota::QuotaUpdate;` (через пустую строку, как остальные `crate::`-импорты); в `AgentEvent` (`:280-288`) последним вариантом:

```rust
    /// Subscription limits a live session reported.
    Quota(QuotaUpdate),
```

`crates/hub-telegram/src/session.rs`, `relay` (`:136-161`), после ветки `AgentEvent::Failed` — временная ветка (задача 12 заменяет её пересылкой в хаб):

```rust
            AgentEvent::Quota(update) => {
                tracing::debug!(
                    backend = update.backend.name(),
                    windows = update.windows.len(),
                    "quota update not cached yet"
                );
            }
```

- [ ] **Step 4: Проверить GREEN**

Run: `cargo check --workspace --all-targets && git diff --stat Cargo.lock && cargo test -p hub-core --lib -- quota`
Expected: в `Cargo.lock` добавлено только ребро `hub-core → chrono`, новых пакетов нет; тесты PASS (13 случаев).

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS; других `match` по `AgentEvent` без ветки `Quota` нет (единственный исчерпывающий — `relay`).

- [ ] **Step 6: Commit**

```bash
git add crates/hub-core/Cargo.toml crates/hub-core/src/lib.rs crates/hub-core/src/quota.rs crates/hub-core/src/domain.rs crates/hub-telegram/src/session.rs Cargo.lock
git commit -m "hub-core: типы лимитов подписок и событие квот от сессии"
```

---

### Task 3: Карточка `render_limits` (`hub-core::render`)

**Files:**
- Modify: `crates/hub-core/src/render.rs:1-6` (импорты), после `:102` (`format_abandoned`) — новые функции, тесты `:104-240`

**Interfaces:**
- Produces (`hub_core::render`): `LIMITS_TITLE: &str = "📊 <b>Лимиты подписок</b>"`; `fn render_limits<Tz>(reports: &[QuotaReport], now: DateTime<Utc>, tz: &Tz) -> String where Tz: TimeZone, Tz::Offset: Display`.
- Consumes: `hub_core::quota::*` (задача 2), `hub_core::escape::escape`.

Правила (спецификация + расхождение 11): заголовок `📊 <b>Лимиты подписок</b> · ДД.ММ ЧЧ:ММ`; блок бэкенда — `<b>Имя</b> (тариф)` и строки окон `<code>  <имя><выравнивание> <полоса> <проценты>%</code> · сброс …`; полоса — `(p + 5) / 10` из 10 делений `▓`, остальное `░`; проценты `{:>2}` после ограничения 100; сброс сегодня — `в ЧЧ:ММ`, через 1–6 дней — `пн ЧЧ:ММ`, иначе — `ДД.ММ ЧЧ:ММ`; кэш — `(на ЧЧ:ММ)` или `(на ДД.ММ ЧЧ:ММ)`; `Unavailable` — `<b>Имя</b> ⚠️ причина`; пустые окна — `  нет данных`; все `NotReported` — одной последней строкой `Qwen, Hermes — квоты подписки агент не сообщает`; пустой список — `Ни один CLI агента не найден`. Порядок бэкендов — как во входе (задача 10 ставит бэкенд по умолчанию первым).

- [ ] **Step 1: Тесты (RED)**

В модуль `tests` `crates/hub-core/src/render.rs` (импорты — к существующим `:106-113`):

```rust
    use chrono::{DateTime, FixedOffset, Utc};

    use crate::domain::BackendKind;
    use crate::quota::{QuotaReport, QuotaSource, UsageWindow, WindowKind};

    fn at(raw: &str) -> DateTime<Utc> {
        raw.parse().unwrap()
    }

    fn msk() -> FixedOffset {
        FixedOffset::east_opt(3 * 3600).unwrap()
    }

    fn window(kind: WindowKind, percent: u8, resets_at: Option<&str>) -> UsageWindow {
        UsageWindow { kind, percent, resets_at: resets_at.map(at) }
    }

    fn card(reports: &[QuotaReport]) -> String {
        render_limits(reports, at("2026-10-16T11:05:00Z"), &msk())
    }

    fn other(name: &str) -> WindowKind {
        WindowKind::Other(name.to_owned())
    }

    #[test]
    fn the_spec_card_is_rendered() {
        let claude = QuotaReport::fresh(
            BackendKind::Claude,
            Some("Max".to_owned()),
            vec![
                window(WindowKind::FiveHour, 62, Some("2026-10-16T14:30:00Z")),
                window(WindowKind::Week, 18, Some("2026-10-19T06:00:00Z")),
                window(WindowKind::WeekModel("Opus".to_owned()), 9, None),
            ],
        );
        let codex = QuotaReport::fresh(
            BackendKind::Codex,
            Some("ChatGPT Plus".to_owned()),
            vec![
                window(other("основное"), 41, Some("2026-10-16T13:10:00Z")),
                window(other("недельное"), 12, Some("2026-10-22T08:00:00Z")),
            ],
        );
        let reports = [
            claude,
            codex,
            QuotaReport::not_reported(BackendKind::Qwen),
            QuotaReport::not_reported(BackendKind::Hermes),
        ];
        assert_eq!(
            card(&reports),
            [
                "📊 <b>Лимиты подписок</b> · 16.10 14:05",
                "<b>Claude</b> (Max)",
                "<code>  5 часов     ▓▓▓▓▓▓░░░░ 62%</code> · сброс в 17:30",
                "<code>  7 дней      ▓▓░░░░░░░░ 18%</code> · сброс пн 09:00",
                "<code>  7 дней Opus ▓░░░░░░░░░  9%</code>",
                "<b>Codex</b> (ChatGPT Plus)",
                "<code>  основное  ▓▓▓▓░░░░░░ 41%</code> · сброс в 16:10",
                "<code>  недельное ▓░░░░░░░░░ 12%</code> · сброс чт 11:00",
                "Qwen, Hermes — квоты подписки агент не сообщает",
            ]
            .join("\n")
        );
    }

    #[rstest]
    #[case::empty(0, "<code>  7 дней ░░░░░░░░░░  0%</code>")]
    #[case::rounds_down(4, "<code>  7 дней ░░░░░░░░░░  4%</code>")]
    #[case::rounds_up(5, "<code>  7 дней ▓░░░░░░░░░  5%</code>")]
    #[case::almost(95, "<code>  7 дней ▓▓▓▓▓▓▓▓▓▓ 95%</code>")]
    #[case::full(100, "<code>  7 дней ▓▓▓▓▓▓▓▓▓▓ 100%</code>")]
    #[case::clamped(250, "<code>  7 дней ▓▓▓▓▓▓▓▓▓▓ 100%</code>")]
    fn the_bar_has_ten_cells_and_percent_is_clamped(#[case] percent: u8, #[case] line: &str) {
        let report = QuotaReport::fresh(
            BackendKind::Claude,
            None,
            vec![window(WindowKind::Week, percent, None)],
        );
        assert_eq!(card(&[report]).lines().nth(2), Some(line));
    }

    #[rstest]
    #[case::today("2026-10-16T20:59:00Z", " · сброс в 23:59")]
    #[case::tomorrow_local("2026-10-16T21:00:00Z", " · сброс сб 00:00")]
    #[case::within_a_week("2026-10-22T08:00:00Z", " · сброс чт 11:00")]
    #[case::a_week_on("2026-10-23T08:00:00Z", " · сброс 23.10 11:00")]
    #[case::past("2026-10-15T08:00:00Z", " · сброс 15.10 11:00")]
    fn resets_are_shown_in_local_time(#[case] resets_at: &str, #[case] suffix: &str) {
        let report = QuotaReport::fresh(
            BackendKind::Claude,
            None,
            vec![window(WindowKind::FiveHour, 50, Some(resets_at))],
        );
        let text = card(&[report]);
        let line = text.lines().nth(2).unwrap();
        assert!(line.ends_with(&format!("</code>{suffix}")), "{line}");
    }

    #[rstest]
    #[case::same_day("2026-10-16T11:02:00Z", "<b>Claude</b> (Max) (на 14:02)")]
    #[case::yesterday("2026-10-15T20:00:00Z", "<b>Claude</b> (Max) (на 15.10 23:00)")]
    fn cached_data_is_dated(#[case] cached: &str, #[case] head: &str) {
        let report = QuotaReport {
            backend: BackendKind::Claude,
            plan: Some("Max".to_owned()),
            windows: vec![window(WindowKind::FiveHour, 10, None)],
            source: QuotaSource::Cached { at: at(cached) },
        };
        assert_eq!(card(&[report]).lines().nth(1), Some(head));
    }

    #[test]
    fn unavailable_and_empty_reports_say_why() {
        let reports = [
            QuotaReport::unavailable(BackendKind::Codex, "вход по API-ключу — лимитов подписки нет"),
            QuotaReport::fresh(BackendKind::Claude, Some("Pro".to_owned()), Vec::new()),
        ];
        assert_eq!(
            card(&reports),
            "📊 <b>Лимиты подписок</b> · 16.10 14:05\n\
             <b>Codex</b> ⚠️ вход по API-ключу — лимитов подписки нет\n\
             <b>Claude</b> (Pro)\n  нет данных"
        );
        assert_eq!(
            card(&[]),
            "📊 <b>Лимиты подписок</b> · 16.10 14:05\nНи один CLI агента не найден"
        );
    }

    #[test]
    fn agent_strings_are_escaped() {
        let reports = [
            QuotaReport::fresh(
                BackendKind::Claude,
                Some("<b>&".to_owned()),
                vec![window(other("<i>x</i>"), 1, None)],
            ),
            QuotaReport::unavailable(BackendKind::Codex, "a<script>"),
        ];
        let text = card(&reports);
        assert!(text.contains("<b>Claude</b> (&lt;b&gt;&amp;)"), "{text}");
        assert!(text.contains("<code>  &lt;i&gt;x&lt;/i&gt; ▓░"), "{text}");
        assert!(text.contains("⚠️ a&lt;script&gt;"), "{text}");
        assert!(!text.contains("<i>") && !text.contains("<script>"));
    }

    #[test]
    fn the_header_uses_the_local_date() {
        let text = render_limits(&[], at("2026-10-16T22:30:00Z"), &msk());
        assert!(text.starts_with("📊 <b>Лимиты подписок</b> · 17.10 01:30"), "{text}");
    }
```

Ожидание `the_spec_card_is_rendered` сверено с карточкой спецификации; выравнивание — по самому длинному имени окна в блоке (`7 дней Opus` — 11 символов, `недельное` — 9); единственное отличие от спецификации — `чт 11:00` вместо `пт 11:00`, потому что 16.10.2026 — пятница, и пятница через неделю показывается датой (`a_week_on`).

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-core --lib -- render`
Expected: FAIL — ошибка компиляции: нет `render_limits`.

- [ ] **Step 3: Реализация**

`crates/hub-core/src/render.rs`, импорты (`:1-6`) заменить на:

```rust
use std::fmt::Display;
use std::num::NonZeroUsize;
use std::time::Duration;

use chrono::{DateTime, Datelike, TimeZone, Utc, Weekday};
use rust_decimal::Decimal;

use crate::domain::{BackendKind, Finished, Usage};
use crate::escape::escape;
use crate::quota::{QuotaReport, QuotaSource, UsageWindow, WindowKind};
```

После `format_abandoned` (`:102`):

```rust
pub const LIMITS_TITLE: &str = "📊 <b>Лимиты подписок</b>";
const NOT_REPORTED: &str = "квоты подписки агент не сообщает";
const NO_AGENTS: &str = "Ни один CLI агента не найден";
const NO_DATA: &str = "  нет данных";
const BAR_CELLS: usize = 10;

/// The `/limits` card as Telegram HTML, times in `tz`; every string from the agents escaped.
#[must_use]
pub fn render_limits<Tz>(reports: &[QuotaReport], now: DateTime<Utc>, tz: &Tz) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let now = now.with_timezone(tz);
    let header = format!("{LIMITS_TITLE} · {}", now.format("%d.%m %H:%M"));
    let silent: Vec<&str> = reports
        .iter()
        .filter(|report| report.source == QuotaSource::NotReported)
        .map(|report| backend_title(report.backend))
        .collect();
    let blocks = reports
        .iter()
        .filter(|report| report.source != QuotaSource::NotReported)
        .flat_map(|report| report_lines(report, &now));
    let tail = match (reports.is_empty(), silent.is_empty()) {
        (true, _) => Some(NO_AGENTS.to_owned()),
        (false, true) => None,
        (false, false) => Some(format!("{} — {NOT_REPORTED}", silent.join(", "))),
    };
    std::iter::once(header).chain(blocks).chain(tail).collect::<Vec<_>>().join("\n")
}

fn report_lines<Tz>(report: &QuotaReport, now: &DateTime<Tz>) -> Vec<String>
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let plan =
        report.plan.as_deref().map_or_else(String::new, |plan| format!(" ({})", escape(plan)));
    let head = format!("<b>{}</b>{plan}", backend_title(report.backend));
    let head = match &report.source {
        QuotaSource::Unavailable(reason) => return vec![format!("{head} ⚠️ {}", escape(reason))],
        QuotaSource::Cached { at } => {
            format!("{head} (на {})", moment(&at.with_timezone(&now.timezone()), now))
        }
        QuotaSource::Fresh | QuotaSource::NotReported => head,
    };
    if report.windows.is_empty() {
        return vec![head, NO_DATA.to_owned()];
    }
    let width = report
        .windows
        .iter()
        .map(|window| label(&window.kind).chars().count())
        .max()
        .unwrap_or_default();
    std::iter::once(head)
        .chain(report.windows.iter().map(|window| window_line(window, width, now)))
        .collect()
}

fn window_line<Tz>(window: &UsageWindow, width: usize, now: &DateTime<Tz>) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let percent = window.percent.min(100);
    let filled = usize::from(percent.saturating_add(5) / 10).min(BAR_CELLS);
    let bar = format!("{}{}", "▓".repeat(filled), "░".repeat(BAR_CELLS - filled));
    let name = label(&window.kind);
    let pad = " ".repeat(width.saturating_sub(name.chars().count()));
    let reset = window.resets_at.map_or_else(String::new, |at| {
        format!(" · сброс {}", reset_text(&at.with_timezone(&now.timezone()), now))
    });
    format!("<code>  {}{pad} {bar} {percent:>2}%</code>{reset}", escape(&name))
}

fn reset_text<Tz>(at: &DateTime<Tz>, now: &DateTime<Tz>) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    match at.date_naive().signed_duration_since(now.date_naive()).num_days() {
        0 => format!("в {}", at.format("%H:%M")),
        1..=6 => format!("{} {}", weekday(at.weekday()), at.format("%H:%M")),
        _ => at.format("%d.%m %H:%M").to_string(),
    }
}

fn moment<Tz>(at: &DateTime<Tz>, now: &DateTime<Tz>) -> String
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let pattern = if at.date_naive() == now.date_naive() { "%H:%M" } else { "%d.%m %H:%M" };
    at.format(pattern).to_string()
}

const fn weekday(day: Weekday) -> &'static str {
    match day {
        Weekday::Mon => "пн",
        Weekday::Tue => "вт",
        Weekday::Wed => "ср",
        Weekday::Thu => "чт",
        Weekday::Fri => "пт",
        Weekday::Sat => "сб",
        Weekday::Sun => "вс",
    }
}

const fn backend_title(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Claude => "Claude",
        BackendKind::Codex => "Codex",
        BackendKind::Qwen => "Qwen",
        BackendKind::Hermes => "Hermes",
    }
}

fn label(kind: &WindowKind) -> String {
    match kind {
        WindowKind::FiveHour => "5 часов".to_owned(),
        WindowKind::Week => "7 дней".to_owned(),
        WindowKind::WeekModel(model) => format!("7 дней {model}"),
        WindowKind::Extra => "сверх подписки".to_owned(),
        WindowKind::Other(name) => name.clone(),
    }
}
```

`match` по `i64` (`num_days`) с `_` — не собственный enum. `BAR_CELLS - filled` не переполняется: `filled` ограничен `min(BAR_CELLS)`.

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-core --lib -- render`
Expected: PASS (новые 17 случаев и прежние тесты `render`).

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-core/src/render.rs
git commit -m "hub-core: карточка лимитов подписок с полосами и временем сброса"
```

---
### Task 4: Команда `/limits` в реестре и её тексты

Команда появляется в реестре (меню бота и `/help` строятся из `Command::ALL`). Хаб пока отвечает временным текстом; настоящая обработка — задача 12.

**Files:**
- Modify: `crates/hub-core/src/commands.rs:7-18` (enum), `:21-32` (`ALL`), `:35-76` (`name`, `description`, `arguments`), тесты `:221`, `:225-234`, `:366-379`
- Modify: `crates/hub-telegram/src/texts.rs:50-52` (константы), тест `:261-283`
- Modify: `crates/hub-telegram/src/hub.rs:410-414` (временная ветка в `command`)

**Interfaces:**
- Produces (`hub_core::commands`): `Command::Limits`; `Command::ALL: [Self; 11]` = `[New, Backend, Cwd, Reset, Stop, Status, Limits, Save, Saves, Restore, Help]`; `name() == "limits"`, `description() == "лимиты подписок агентов"`, `arguments() == ""`.
- Produces (`hub_telegram::texts`): `LIMITS_COLLECTING = "⏳ Собираю…"` (спецификация), `LIMITS_REFRESH = "🔄 Обновить"` (спецификация), `LIMITS_BUSY = "⏳ Уже собираю лимиты — подождите."` (новый, расхождение 9), `LIMITS_PENDING` (временный, удаляется в задаче 12).

- [ ] **Step 1: Тесты (RED)**

`crates/hub-core/src/commands.rs`, модуль `tests`: в `menu_registry_is_unique_valid_and_parsed` (`:221`) — `assert_eq!(names.len(), 11);`; тест `snapshot_commands_sit_before_help` (`:225-234`) переименовать и расширить:

```rust
    #[test]
    fn limits_sit_after_status_and_snapshots_before_help() {
        let names: Vec<_> = Command::ALL.into_iter().map(Command::name).collect();
        assert_eq!(
            names,
            [
                "new", "backend", "cwd", "reset", "stop", "status", "limits", "save", "saves",
                "restore", "help"
            ]
        );
    }
```

в таблицу `input_is_classified` (после `:370`):

```rust
    #[case("/limits", Input::Command { command: Command::Limits, args: vec![] })]
    #[case("/limits@agent_hub_bot", Input::Command { command: Command::Limits, args: vec![] })]
```

`crates/hub-telegram/src/texts.rs`, тест `help_names_root_uploads_and_backends` (`:261-283`), после проверки `/restore`:

```rust
        assert!(text.contains("/limits — лимиты подписок агентов"));
```

и новый тест рядом:

```rust
    #[test]
    fn limits_texts_match_the_spec() {
        assert_eq!(LIMITS_COLLECTING, "⏳ Собираю…");
        assert_eq!(LIMITS_REFRESH, "🔄 Обновить");
        assert!(LIMITS_BUSY.starts_with("⏳ "));
    }
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-core --lib -- commands && cargo test -p hub-telegram --lib -- texts`
Expected: FAIL — ошибки компиляции: нет `Command::Limits`, `LIMITS_COLLECTING`, `LIMITS_REFRESH`, `LIMITS_BUSY`.

- [ ] **Step 3: Реализация**

`crates/hub-core/src/commands.rs`: вариант `Limits` в `enum Command` (после `Backend`, `:14`); `ALL`:

```rust
    pub const ALL: [Self; 11] = [
        Self::New,
        Self::Backend,
        Self::Cwd,
        Self::Reset,
        Self::Stop,
        Self::Status,
        Self::Limits,
        Self::Save,
        Self::Saves,
        Self::Restore,
        Self::Help,
    ];
```

В `name`: `Self::Limits => "limits"`; в `description`: `Self::Limits => "лимиты подписок агентов"`; в `arguments` — в общую ветку: `Self::Help | Self::Reset | Self::Stop | Self::Status | Self::Saves | Self::Limits => ""`.

`crates/hub-telegram/src/texts.rs`, после `RESTORE_NOT_PERSISTED` (`:50-51`):

```rust
pub const LIMITS_COLLECTING: &str = "⏳ Собираю…";
pub const LIMITS_REFRESH: &str = "🔄 Обновить";
pub const LIMITS_BUSY: &str = "⏳ Уже собираю лимиты — подождите.";
// Until the hub answers /limits (task 12 of the plan).
pub const LIMITS_PENDING: &str = "Лимиты подписок появятся в следующей версии";
```

`crates/hub-telegram/src/hub.rs`, `command`, после `Command::Restore => self.restore(key, args),` (`:413`):

```rust
            Command::Limits => self.say(Target::Topic(key), texts::LIMITS_PENDING.to_owned()),
```

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-core --lib -- commands && cargo test -p hub-telegram --lib -- texts && cargo test -p hub-telegram --test menu`
Expected: PASS; тест меню (`tests/menu/registration.rs:169-176`) строит ожидание из `Command::ALL` и проходит с 11 командами.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-core/src/commands.rs crates/hub-telegram/src/texts.rs crates/hub-telegram/src/hub.rs
git commit -m "Команда /limits в реестре и её тексты"
```

---
### Task 5: Разбор `get_usage` и `rate_limit_event` (`hub-claude`)

**Files:**
- Create: `crates/hub-claude/src/usage.rs`
- Create: `crates/hub-claude/tests/fixtures/get_usage.json`
- Modify: `crates/hub-claude/Cargo.toml:9-22` (зависимость `chrono`)
- Modify: `crates/hub-claude/src/lib.rs:9` (`pub mod usage;` после `pub mod tracker;`)
- Modify: `crates/hub-claude/src/wire.rs:6-26` (вариант `RateLimitEvent`), после `:149` (`RateLimitInfo`), тесты `:268-293`
- Modify: `crates/hub-claude/src/tracker.rs:35-42`, `:51-60`, `crates/hub-claude/src/activity.rs:92-98`, `crates/hub-claude/src/session.rs:255-259` (новый вариант в исчерпывающих `match`)
- Test: `crates/hub-claude/src/usage.rs`, `crates/hub-claude/src/wire.rs`

**Interfaces:**
- Produces (`hub_claude::wire`): `Incoming::RateLimitEvent { rate_limit_info: RateLimitInfo }`; `struct RateLimitInfo { rate_limit_type: Option<Value>, utilization: Option<Value>, resets_at: Option<Value> }` (`camelCase`, `Default`; поля — `Value`, чтобы неожиданный тип не делал строку нечитаемой).
- Produces (`hub_claude::usage`): `NO_SUBSCRIPTION: &str`; `enum Scale { Fraction, Percent }`, `EVENT_SCALE: Scale = Scale::Fraction` (переключатель задачи 1); `enum ProbeError { Rejected(String), Shape }` (`Debug, Clone, PartialEq, Eq, thiserror::Error`; задача 6 добавляет варианты) с `const fn kind(&self) -> &'static str`; `fn parse_reply(reply: &Value) -> Result<QuotaReport, ProbeError>` — `reply` — объект `response` строки `control_response`; `fn event_update(info: &RateLimitInfo, scale: Scale) -> Option<QuotaUpdate>`.
- Consumes: `hub_core::quota::{QuotaReport, QuotaUpdate, UsageWindow, WindowKind, clean_label, percent}` (задача 2).

- [ ] **Step 1: Фикстура и тесты (RED)**

`crates/hub-claude/tests/fixtures/get_usage.json` — полезная нагрузка ответа по `SDKControlGetUsageResponse` (`sdk.d.ts:4326-4534`) с лишними полями (`future_window`, `future_field`) и `null`-окнами. Если задача 1 сохранила живой ответ — положить его вместо этого файла и переписать ожидание `the_recorded_reply_becomes_a_report` по его значениям.

```json
{
  "session": {"total_cost_usd": 0, "total_api_duration_ms": 0, "total_duration_ms": 0, "total_lines_added": 0, "total_lines_removed": 0, "model_usage": {}},
  "subscription_type": "max",
  "rate_limits_available": true,
  "rate_limits": {
    "five_hour": {"utilization": 62.4, "resets_at": "2026-10-16T14:30:00+00:00"},
    "seven_day": {"utilization": 18, "resets_at": "2026-10-19T06:00:00.000Z"},
    "seven_day_oauth_apps": null,
    "seven_day_opus": {"utilization": 9, "resets_at": null},
    "seven_day_sonnet": {"utilization": null, "resets_at": null},
    "model_scoped": [{"display_name": "Fable", "utilization": 33, "resets_at": "2026-10-19T06:00:00Z"}],
    "extra_usage": {"is_enabled": false, "monthly_limit": null, "used_credits": null, "utilization": null, "currency": null},
    "future_window": {"utilization": 1}
  },
  "behaviors": null,
  "session_id": "s",
  "future_field": true
}
```

`crates/hub-claude/src/usage.rs` — пока только тесты:

```rust
#[cfg(test)]
mod tests {
    use rstest::rstest;
    use serde_json::json;

    use super::*;

    const RECORDED: &str = include_str!("../tests/fixtures/get_usage.json");

    fn at(raw: &str) -> DateTime<Utc> {
        raw.parse().unwrap()
    }

    fn success(body: Value) -> Value {
        json!({"subtype": "success", "request_id": "hub-usage", "response": body})
    }

    fn window(kind: WindowKind, percent: u8, resets_at: Option<&str>) -> UsageWindow {
        UsageWindow { kind, percent, resets_at: resets_at.map(at) }
    }

    #[test]
    fn the_recorded_reply_becomes_a_report() {
        let body: Value = serde_json::from_str(RECORDED).unwrap();
        assert_eq!(
            parse_reply(&success(body)),
            Ok(QuotaReport::fresh(
                BackendKind::Claude,
                Some("Max".to_owned()),
                vec![
                    window(WindowKind::FiveHour, 62, Some("2026-10-16T14:30:00Z")),
                    window(WindowKind::Week, 18, Some("2026-10-19T06:00:00Z")),
                    window(WindowKind::WeekModel("Opus".to_owned()), 9, None),
                    window(WindowKind::WeekModel("Fable".to_owned()), 33, Some("2026-10-19T06:00:00Z")),
                ],
            ))
        );
    }

    #[rstest]
    #[case::api_key(
        json!({"subscription_type": null, "rate_limits_available": false, "rate_limits": null}),
        Ok(QuotaReport::unavailable(BackendKind::Claude, NO_SUBSCRIPTION))
    )]
    #[case::fields_missing(
        json!({"rate_limits": {"five_hour": {"utilization": 7.5}}}),
        Ok(QuotaReport::fresh(BackendKind::Claude, None, vec![UsageWindow { kind: WindowKind::FiveHour, percent: 8, resets_at: None }]))
    )]
    #[case::all_windows_null(
        json!({"subscription_type": "pro", "rate_limits_available": true, "rate_limits": {"five_hour": null, "seven_day": {"utilization": null, "resets_at": null}}}),
        Ok(QuotaReport::fresh(BackendKind::Claude, Some("Pro".to_owned()), Vec::new()))
    )]
    #[case::bad_time_is_dropped(
        json!({"rate_limits": {"seven_day": {"utilization": 120, "resets_at": "tomorrow"}}}),
        Ok(QuotaReport::fresh(BackendKind::Claude, None, vec![UsageWindow { kind: WindowKind::Week, percent: 100, resets_at: None }]))
    )]
    #[case::extra_usage_when_enabled(
        json!({"rate_limits": {"extra_usage": {"is_enabled": true, "utilization": 40.0}}}),
        Ok(QuotaReport::fresh(BackendKind::Claude, None, vec![UsageWindow { kind: WindowKind::Extra, percent: 40, resets_at: None }]))
    )]
    #[case::wrong_type(json!({"rate_limits": {"five_hour": {"utilization": "a lot"}}}), Err(ProbeError::Shape))]
    #[case::no_limits(json!({"rate_limits_available": true, "rate_limits": null}), Err(ProbeError::Shape))]
    #[case::not_an_object(json!("usage"), Err(ProbeError::Shape))]
    fn replies_are_read_tolerantly(
        #[case] body: Value,
        #[case] expected: Result<QuotaReport, ProbeError>,
    ) {
        assert_eq!(parse_reply(&success(body)), expected);
    }

    #[rstest]
    #[case::error(
        json!({"subtype": "error", "request_id": "hub-usage", "error": "Unsupported control request subtype: get_usage\n"}),
        Err(ProbeError::Rejected("Unsupported control request sub…".to_owned()))
    )]
    #[case::unknown_subtype(json!({"subtype": "pending", "request_id": "hub-usage"}), Err(ProbeError::Shape))]
    fn refusals_are_errors(#[case] reply: Value, #[case] expected: Result<QuotaReport, ProbeError>) {
        assert_eq!(parse_reply(&reply), expected);
    }

    #[test]
    fn many_model_windows_are_capped() {
        let models: Vec<Value> = (0..20)
            .map(|i| json!({"display_name": format!("m{i}"), "utilization": 1, "resets_at": null}))
            .collect();
        let report = parse_reply(&success(json!({"rate_limits": {"model_scoped": models}}))).unwrap();
        assert_eq!(report.windows.len(), hub_core::quota::WINDOW_LIMIT);
    }

    fn info(value: Value) -> RateLimitInfo {
        serde_json::from_value(value).unwrap()
    }

    fn update(kind: WindowKind, percent: u8, resets_at: Option<&str>) -> Option<QuotaUpdate> {
        Some(QuotaUpdate {
            backend: BackendKind::Claude,
            plan: None,
            windows: vec![window(kind, percent, resets_at)],
        })
    }

    #[rstest]
    #[case::fraction(json!({"status": "allowed", "rateLimitType": "five_hour", "utilization": 0.62, "resetsAt": 1_792_160_000}), Scale::Fraction, update(WindowKind::FiveHour, 62, Some("2026-10-16T14:13:20Z")))]
    #[case::percent(json!({"status": "allowed_warning", "rateLimitType": "seven_day", "utilization": 81}), Scale::Percent, update(WindowKind::Week, 81, None))]
    #[case::millis(json!({"rateLimitType": "seven_day_opus", "utilization": 0.5, "resetsAt": 1_792_160_000_000_i64}), Scale::Fraction, update(WindowKind::WeekModel("Opus".to_owned()), 50, Some("2026-10-16T14:13:20Z")))]
    #[case::fractional_reset(json!({"rateLimitType": "overage", "utilization": 0.1, "resetsAt": 1.5}), Scale::Fraction, update(WindowKind::Extra, 10, None))]
    #[case::status_only(json!({"status": "allowed"}), Scale::Fraction, None)]
    #[case::unknown_window(json!({"rateLimitType": "one_day", "utilization": 0.1}), Scale::Fraction, None)]
    #[case::bad_utilization(json!({"rateLimitType": "five_hour", "utilization": "high"}), Scale::Fraction, None)]
    fn rate_limit_events_update_one_window(
        #[case] raw: Value,
        #[case] scale: Scale,
        #[case] expected: Option<QuotaUpdate>,
    ) {
        assert_eq!(event_update(&info(raw), scale), expected);
    }
}
```

Проверка констант теста: `1_792_160_000` с — `2026-10-16T14:13:20Z` (`date -u -d @1792160000`); текст ошибки после `clean_label` — без `\n`, 31 символ и `…`.

`crates/hub-claude/src/wire.rs`, тесты: случай `rate_limit_event` в `unknown_kinds_are_tolerated` (`:269`) удалить, вместо него — тест:

```rust
    #[rstest]
    #[case(json!({"type": "rate_limit_event", "rate_limit_info": {"status": "allowed"}}), RateLimitInfo::default())]
    #[case(json!({"type": "rate_limit_event"}), RateLimitInfo::default())]
    #[case(
        json!({"type": "rate_limit_event", "rate_limit_info": {"rateLimitType": "five_hour", "utilization": 0.5, "resetsAt": 1, "isUsingOverage": false}}),
        RateLimitInfo { rate_limit_type: Some(json!("five_hour")), utilization: Some(json!(0.5)), resets_at: Some(json!(1)) }
    )]
    fn rate_limit_events_are_parsed(#[case] value: serde_json::Value, #[case] info: RateLimitInfo) {
        assert_eq!(parse(&value), Incoming::RateLimitEvent { rate_limit_info: info });
    }
```

и в `every_recorded_line_parses_and_known_kinds_are_recognised` (`:286-289`) добавить `"rate_limit_event"` в список известных видов.

`crates/hub-claude/src/lib.rs` после `pub mod tracker;` (`:8`) — `pub mod usage;`.

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-claude --lib -- usage wire`
Expected: FAIL — ошибки компиляции: нет `RateLimitInfo`, `Incoming::RateLimitEvent`, `parse_reply`, `event_update`, `Scale`, крейта `chrono`.

- [ ] **Step 3: Реализация**

`crates/hub-claude/Cargo.toml`, `[dependencies]` после `base64.workspace = true` (`:10`) — `chrono.workspace = true`.

`crates/hub-claude/src/wire.rs`: в `Incoming` перед `ControlRequest` (`:13`):

```rust
    /// Plan usage of one rate-limit window, sent when it changes.
    RateLimitEvent {
        #[serde(default)]
        rate_limit_info: RateLimitInfo,
    },
```

комментарий над `Other` (`:23`) — `/// Kinds this hub does not act on: command lifecycle, stream events, …`; после `ControlReply` (`:149`):

```rust
/// `SDKRateLimitInfo`, only what the `/limits` card uses. Kept as raw values: an unexpected
/// type must not make the whole line unreadable.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitInfo {
    #[serde(default)]
    pub rate_limit_type: Option<Value>,
    #[serde(default)]
    pub utilization: Option<Value>,
    #[serde(default)]
    pub resets_at: Option<Value>,
}
```

Исчерпывающие `match` по `Incoming` — `Incoming::RateLimitEvent { .. }` рядом с `Incoming::Other`: `tracker.rs:35-42` (`translate`, без событий), `tracker.rs:51-60` (`session_of`, `None`), `activity.rs:92-98` (без изменения фазы), `session.rs:255-259` (ветка `observe`/`translate` — поведение как у `Other`; задача 7 выносит вариант в свою ветку).

`crates/hub-claude/src/usage.rs` (над тестами):

```rust
//! Subscription limits from the Claude CLI: the `get_usage` control reply and
//! `rate_limit_event`. Both are untrusted: strings pass `clean_label`, numbers `percent`.

use chrono::{DateTime, Utc};
use hub_core::domain::BackendKind;
use hub_core::quota::{QuotaReport, QuotaUpdate, UsageWindow, WindowKind, clean_label, percent};
use serde::Deserialize;
use serde_json::Value;

use crate::wire::RateLimitInfo;

pub const NO_SUBSCRIPTION: &str = "вход не через подписку claude.ai — лимитов подписки нет";
const OAUTH_APPS: &str = "7 дней, приложения";
const OVERAGE_INCLUDED: &str = "7 дней, включённые модели";
/// `resetsAt` above this is in milliseconds: in seconds it stays below until the year 5138.
const MILLIS_FROM: i64 = 100_000_000_000;

/// The unit of `utilization` in `rate_limit_event`, which the SDK leaves undocumented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    Fraction,
    Percent,
}

/// What the live check found (task 1 of the plan); `get_usage` is 0–100 by the SDK types.
pub const EVENT_SCALE: Scale = Scale::Fraction;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    #[error("Claude отклонил запрос лимитов: {0}")]
    Rejected(String),
    #[error("непонятный ответ Claude о лимитах")]
    Shape,
}

impl ProbeError {
    /// What goes to the log: never the agent's text.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Rejected(_) => "rejected",
            Self::Shape => "shape",
        }
    }
}

#[derive(Deserialize)]
struct UsageBody {
    #[serde(default)]
    subscription_type: Option<String>,
    #[serde(default)]
    rate_limits_available: Option<bool>,
    #[serde(default)]
    rate_limits: Option<RateLimits>,
}

#[derive(Deserialize)]
struct RateLimits {
    #[serde(default)]
    five_hour: Option<Window>,
    #[serde(default)]
    seven_day: Option<Window>,
    #[serde(default)]
    seven_day_opus: Option<Window>,
    #[serde(default)]
    seven_day_sonnet: Option<Window>,
    #[serde(default)]
    seven_day_oauth_apps: Option<Window>,
    #[serde(default)]
    model_scoped: Option<Vec<ModelWindow>>,
    #[serde(default)]
    extra_usage: Option<Extra>,
}

#[derive(Deserialize)]
struct Window {
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct ModelWindow {
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    utilization: Option<f64>,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Deserialize)]
struct Extra {
    #[serde(default)]
    is_enabled: bool,
    #[serde(default)]
    utilization: Option<f64>,
}

/// The reply to `get_usage`: the `response` object of its `control_response` line.
pub fn parse_reply(reply: &Value) -> Result<QuotaReport, ProbeError> {
    match reply.get("subtype").and_then(Value::as_str) {
        Some("success") => {}
        Some("error") => {
            let error = reply.get("error").and_then(Value::as_str).and_then(clean_label);
            return Err(ProbeError::Rejected(error.unwrap_or_default()));
        }
        Some(_) | None => return Err(ProbeError::Shape),
    }
    let body = reply.get("response").cloned().unwrap_or(Value::Null);
    let UsageBody { subscription_type, rate_limits_available, rate_limits } =
        serde_json::from_value(body).map_err(|_| ProbeError::Shape)?;
    match (rate_limits_available, rate_limits) {
        (Some(false), _) => Ok(QuotaReport::unavailable(BackendKind::Claude, NO_SUBSCRIPTION)),
        (Some(true) | None, None) => Err(ProbeError::Shape),
        (Some(true) | None, Some(limits)) => {
            let plan = subscription_type.as_deref().and_then(plan_name);
            Ok(QuotaReport::fresh(BackendKind::Claude, plan, windows(limits)))
        }
    }
}

fn windows(limits: RateLimits) -> Vec<UsageWindow> {
    let RateLimits {
        five_hour,
        seven_day,
        seven_day_opus,
        seven_day_sonnet,
        seven_day_oauth_apps,
        model_scoped,
        extra_usage,
    } = limits;
    let named = [
        (WindowKind::FiveHour, five_hour),
        (WindowKind::Week, seven_day),
        (WindowKind::WeekModel("Opus".to_owned()), seven_day_opus),
        (WindowKind::WeekModel("Sonnet".to_owned()), seven_day_sonnet),
        (WindowKind::Other(OAUTH_APPS.to_owned()), seven_day_oauth_apps),
    ]
    .into_iter()
    .filter_map(|(kind, window)| {
        let Window { utilization, resets_at } = window?;
        usage_window(kind, utilization, resets_at.as_deref())
    });
    let models = model_scoped.unwrap_or_default().into_iter().filter_map(|model| {
        let ModelWindow { display_name, utilization, resets_at } = model;
        let kind = WindowKind::WeekModel(clean_label(&display_name)?);
        usage_window(kind, utilization, resets_at.as_deref())
    });
    let extra = extra_usage
        .filter(|extra| extra.is_enabled)
        .and_then(|extra| usage_window(WindowKind::Extra, extra.utilization, None));
    named.chain(models).chain(extra).collect()
}

fn usage_window(
    kind: WindowKind,
    utilization: Option<f64>,
    resets_at: Option<&str>,
) -> Option<UsageWindow> {
    let percent = percent(utilization?)?;
    Some(UsageWindow { kind, percent, resets_at: resets_at.and_then(iso_time) })
}

fn iso_time(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw).ok().map(|time| time.with_timezone(&Utc))
}

/// `max` → `Max`.
fn plan_name(raw: &str) -> Option<String> {
    let clean = clean_label(raw)?;
    let mut chars = clean.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

/// The one window a `rate_limit_event` reports; `None` when it names no known window or no
/// usage (CLI 2.1.28x sends only `status`, see the fixtures).
#[must_use]
pub fn event_update(info: &RateLimitInfo, scale: Scale) -> Option<QuotaUpdate> {
    let kind = match info.rate_limit_type.as_ref()?.as_str()? {
        "five_hour" => WindowKind::FiveHour,
        "seven_day" => WindowKind::Week,
        "seven_day_opus" => WindowKind::WeekModel("Opus".to_owned()),
        "seven_day_sonnet" => WindowKind::WeekModel("Sonnet".to_owned()),
        "seven_day_overage_included" => WindowKind::Other(OVERAGE_INCLUDED.to_owned()),
        "overage" => WindowKind::Extra,
        _ => return None,
    };
    let raw = info.utilization.as_ref()?.as_f64()?;
    let value = match scale {
        Scale::Fraction => raw * 100.0,
        Scale::Percent => raw,
    };
    let resets_at = info.resets_at.as_ref().and_then(Value::as_i64).and_then(unix_time);
    let window = UsageWindow { kind, percent: percent(value)?, resets_at };
    Some(QuotaUpdate { backend: BackendKind::Claude, plan: None, windows: vec![window] })
}

fn unix_time(raw: i64) -> Option<DateTime<Utc>> {
    if raw > MILLIS_FROM {
        DateTime::from_timestamp_millis(raw)
    } else {
        DateTime::from_timestamp(raw, 0)
    }
}
```

`model_scoped.unwrap_or_default()` — `Vec` по умолчанию, не подмена сбоя: отсутствие массива по SDK значит «ничего не известно».

- [ ] **Step 4: Проверить GREEN**

Run: `cargo check --workspace --all-targets && git diff --stat Cargo.lock && cargo test -p hub-claude --lib -- usage wire && cargo test -p hub-claude --test replay`
Expected: в `Cargo.lock` только ребро `hub-claude → chrono`; тесты PASS; снимки `replay` не изменились (событие `status: allowed` по-прежнему ничего не порождает).

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-claude/Cargo.toml Cargo.lock crates/hub-claude/src/lib.rs crates/hub-claude/src/usage.rs crates/hub-claude/src/wire.rs crates/hub-claude/src/tracker.rs crates/hub-claude/src/activity.rs crates/hub-claude/src/session.rs crates/hub-claude/tests/fixtures/get_usage.json
git commit -m "hub-claude: разбор ответа get_usage и rate_limit_event в окна лимитов"
```

---
### Task 6: Проба Claude — `usage::probe`

Короткий `claude` без пользовательского сообщения: управляющие запросы → ограниченное чтение stdout до ответа на `get_usage` → закрытие stdin → ожидание выхода ≤ 5 с → `kill`. Обмен вынесен в `exchange` над `AsyncRead`/`AsyncWrite`: тесты гоняют его на `tokio::io::duplex` — одинаково на всех ОС, без shell-скриптов.

**Files:**
- Modify: `crates/hub-claude/src/usage.rs` (импорты, константы, `ProbeError`, новые функции, тесты)

**Interfaces:**
- Produces (`hub_claude::usage`): `enum Handshake { Direct, InitializeFirst, CacheOnly }`, `HANDSHAKE: Handshake = Handshake::InitializeFirst` (переключатель задачи 1); `PROBE_ARGS: [&str; 7]`; `ANSWER_TIMEOUT: Duration = 15 с`; `ProbeError` += `Disabled`, `Spawn(io::ErrorKind)`, `Timeout`, `NoAnswer`, `TooLarge`, `Io(io::ErrorKind)`; `fn requests(Handshake) -> Option<Vec<Value>>`; `async fn exchange<R, W>(reader: R, writer: W, requests: &[Value], wait: Duration) -> Result<QuotaReport, ProbeError>`; `async fn probe(cli: &Path) -> Result<QuotaReport, ProbeError>`.
- Consumes: `crate::outgoing::{control_request, initialize, ENTRYPOINT}`, `hub_agent::cli::hide_window`, `parse_reply` (задача 5).

- [ ] **Step 1: Тесты (RED)**

В модуль `tests` `crates/hub-claude/src/usage.rs`:

```rust
    use std::time::Duration;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

    const WAIT: Duration = Duration::from_secs(5);

    fn usage_request() -> Value {
        json!({"type": "control_request", "request_id": "hub-usage",
               "request": {"subtype": "get_usage", "skip_behaviors": true}})
    }

    fn answer(body: &Value) -> String {
        json!({"type": "control_response",
               "response": {"subtype": "success", "request_id": "hub-usage", "response": body}})
        .to_string()
    }

    enum Then {
        /// Keep stdout open: the probe must stop on its own.
        Wait,
        /// Close stdout, as an exiting CLI does.
        Exit,
    }

    /// The CLI side: reads `expect` request lines, writes `lines`, then checks that the probe
    /// closes stdin.
    async fn fake_cli(theirs: DuplexStream, expect: usize, lines: Vec<String>, then: Then) -> Vec<Value> {
        let (read, mut write) = tokio::io::split(theirs);
        let mut input = BufReader::new(read).lines();
        let mut seen = Vec::new();
        for _ in 0..expect {
            let line = input.next_line().await.unwrap().unwrap();
            seen.push(serde_json::from_str(&line).unwrap());
        }
        for line in lines {
            write.write_all(format!("{line}\n").as_bytes()).await.unwrap();
        }
        match then {
            Then::Wait => {}
            Then::Exit => write.shutdown().await.unwrap(),
        }
        assert_eq!(input.next_line().await.unwrap(), None, "the probe must close stdin");
        seen
    }

    async fn run(
        handshake: Handshake,
        lines: Vec<String>,
        then: Then,
        wait: Duration,
    ) -> (Result<QuotaReport, ProbeError>, Vec<Value>) {
        let requests = requests(handshake).unwrap();
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        let cli = tokio::spawn(fake_cli(theirs, requests.len(), lines, then));
        let (read, write) = tokio::io::split(ours);
        let outcome = exchange(read, write, &requests, wait).await;
        (outcome, cli.await.unwrap())
    }

    fn recorded() -> Value {
        serde_json::from_str(RECORDED).unwrap()
    }

    #[tokio::test]
    async fn initialize_first_then_the_answer_among_other_lines() {
        let lines = vec![
            json!({"type": "control_response", "response": {"subtype": "success", "request_id": "hub-usage-initialize", "response": {}}}).to_string(),
            "not json".to_owned(),
            json!({"type": "system", "subtype": "status"}).to_string(),
            answer(&recorded()),
        ];
        let (outcome, seen) = run(Handshake::InitializeFirst, lines, Then::Wait, WAIT).await;
        assert_eq!(outcome.map(|report| report.plan), Ok(Some("Max".to_owned())));
        assert_eq!(
            seen,
            [
                json!({"type": "control_request", "request_id": "hub-usage-initialize",
                       "request": {"subtype": "initialize", "hooks": null}}),
                usage_request(),
            ]
        );
    }

    #[tokio::test]
    async fn direct_sends_only_get_usage_and_never_a_prompt() {
        let (outcome, seen) =
            run(Handshake::Direct, vec![answer(&recorded())], Then::Wait, WAIT).await;
        assert!(outcome.is_ok());
        assert_eq!(seen, [usage_request()]);
    }

    #[test]
    fn cache_only_runs_no_cli() {
        assert_eq!(requests(Handshake::CacheOnly), None);
    }

    #[tokio::test]
    async fn an_unknown_shape_is_an_error() {
        let lines = vec![answer(&json!({"rate_limits": 5}))];
        let (outcome, _seen) = run(Handshake::Direct, lines, Then::Wait, WAIT).await;
        assert_eq!(outcome, Err(ProbeError::Shape));
    }

    #[tokio::test]
    async fn silence_times_out_and_closes_stdin() {
        let (outcome, _seen) =
            run(Handshake::Direct, Vec::new(), Then::Wait, Duration::from_millis(100)).await;
        assert_eq!(outcome, Err(ProbeError::Timeout));
    }

    #[tokio::test]
    async fn exit_without_an_answer() {
        let lines = vec![json!({"type": "system", "subtype": "init", "session_id": "s"}).to_string()];
        let (outcome, _seen) = run(Handshake::Direct, lines, Then::Exit, WAIT).await;
        assert_eq!(outcome, Err(ProbeError::NoAnswer));
    }

    #[tokio::test]
    async fn too_many_lines_without_an_answer() {
        let lines = vec![json!({"type": "system", "subtype": "status"}).to_string(); 65];
        let (outcome, _seen) = run(Handshake::Direct, lines, Then::Wait, WAIT).await;
        assert_eq!(outcome, Err(ProbeError::NoAnswer));
    }

    #[tokio::test]
    async fn a_huge_line_is_refused() {
        let lines = vec!["x".repeat(1024 * 1024 + 1)];
        let (outcome, _seen) = run(Handshake::Direct, lines, Then::Exit, WAIT).await;
        assert_eq!(outcome, Err(ProbeError::TooLarge));
    }

    #[test]
    fn the_probe_loads_no_settings_and_no_mcp_servers() {
        assert!(PROBE_ARGS.contains(&"--setting-sources="));
        assert!(PROBE_ARGS.contains(&"--strict-mcp-config"));
        assert!(!PROBE_ARGS.iter().any(|arg| arg.starts_with("--resume") || *arg == "--mcp-config"));
    }

    #[tokio::test]
    async fn a_missing_cli_is_a_spawn_error() {
        let outcome = probe(&std::env::temp_dir().join("definitely-not-claude")).await;
        // `Disabled` if the live check turned the probe off.
        assert!(matches!(outcome, Err(ProbeError::Spawn(_) | ProbeError::Disabled)), "{outcome:?}");
    }
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-claude --lib -- usage`
Expected: FAIL — ошибки компиляции: нет `Handshake`, `requests`, `exchange`, `probe`, `PROBE_ARGS`, вариантов `ProbeError::Timeout` и др.

- [ ] **Step 3: Реализация**

Импорты `crates/hub-claude/src/usage.rs` дополнить:

```rust
use std::io;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use futures::StreamExt;
use hub_agent::cli::hide_window;
use serde_json::json;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::process::Command;
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};

use crate::outgoing::{self, ENTRYPOINT};
```

Константы и рукопожатие (после `EVENT_SCALE`):

```rust
/// How the probe asks; the live check (task 1 of the plan) picks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handshake {
    /// `get_usage` right away.
    Direct,
    /// A minimal `initialize` first, as SDK hosts do.
    InitializeFirst,
    /// The CLI does not answer without a prompt: `/limits` uses the cache only.
    CacheOnly,
}

pub const HANDSHAKE: Handshake = Handshake::InitializeFirst;

/// No prompt, no settings, no MCP servers, no hooks: the run only answers control requests.
pub const PROBE_ARGS: [&str; 7] = [
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--setting-sources=",
    "--strict-mcp-config",
];
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(15);
// After stdin closes the CLI exits on its own; this bounds the wait before killing it.
const EXIT_TIMEOUT: Duration = Duration::from_secs(5);
const USAGE_REQUEST: &str = "hub-usage";
const INITIALIZE_REQUEST: &str = "hub-usage-initialize";
const LINE_LIMIT: usize = 1024 * 1024;
const LINES_LIMIT: usize = 64;
```

`ProbeError` — новые варианты и ветки `kind`:

```rust
    #[error("проба лимитов Claude отключена: CLI не отвечает без сообщения")]
    Disabled,
    #[error("не удалось запустить Claude: {0}")]
    Spawn(io::ErrorKind),
    #[error("Claude не ответил о лимитах за {} с", ANSWER_TIMEOUT.as_secs())]
    Timeout,
    #[error("Claude завершился, не ответив о лимитах")]
    NoAnswer,
    #[error("ответ Claude о лимитах длиннее {} КиБ", LINE_LIMIT / 1024)]
    TooLarge,
    #[error("ошибка обмена с Claude: {0}")]
    Io(io::ErrorKind),
```

```rust
            Self::Disabled => "disabled",
            Self::Spawn(_) => "spawn",
            Self::Timeout => "timeout",
            Self::NoAnswer => "no_answer",
            Self::TooLarge => "too_large",
            Self::Io(_) => "io",
```

Функции:

```rust
/// The control requests of one probe; `None` when the probe is off.
#[must_use]
pub fn requests(handshake: Handshake) -> Option<Vec<Value>> {
    let usage = outgoing::control_request(
        USAGE_REQUEST,
        json!({"subtype": "get_usage", "skip_behaviors": true}),
    );
    match handshake {
        Handshake::Direct => Some(vec![usage]),
        Handshake::InitializeFirst => Some(vec![
            outgoing::control_request(INITIALIZE_REQUEST, outgoing::initialize()),
            usage,
        ]),
        Handshake::CacheOnly => None,
    }
}

/// Asks a short-lived CLI for the plan limits. It never gets a prompt, so it starts no turn
/// and writes no session; its stderr is dropped, so nothing of it reaches the log.
pub async fn probe(cli: &Path) -> Result<QuotaReport, ProbeError> {
    let requests = requests(HANDSHAKE).ok_or(ProbeError::Disabled)?;
    let mut command = Command::new(cli);
    command
        .args(PROBE_ARGS)
        .current_dir(std::env::temp_dir())
        .env(ENTRYPOINT.0, ENTRYPOINT.1)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    hide_window(&mut command);
    let mut child = command.spawn().map_err(|error| ProbeError::Spawn(error.kind()))?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(ProbeError::Io(io::ErrorKind::BrokenPipe));
    };
    let answer = exchange(stdout, stdin, &requests, ANSWER_TIMEOUT).await;
    if tokio::time::timeout(EXIT_TIMEOUT, child.wait()).await.is_err()
        && let Err(error) = child.kill().await
    {
        tracing::warn!(error = ?error.kind(), "claude usage probe could not be killed");
    }
    answer
}

/// Writes `requests`, reads at most `LINES_LIMIT` lines of at most `LINE_LIMIT` bytes for the
/// usage answer within `wait`, then closes `writer` whatever happened, so the CLI sees EOF.
pub async fn exchange<R, W>(
    reader: R,
    mut writer: W,
    requests: &[Value],
    wait: Duration,
) -> Result<QuotaReport, ProbeError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let asked = async {
        for request in requests {
            let line = format!("{request}\n");
            writer.write_all(line.as_bytes()).await.map_err(|error| ProbeError::Io(error.kind()))?;
        }
        writer.flush().await.map_err(|error| ProbeError::Io(error.kind()))?;
        let mut lines = FramedRead::new(reader, LinesCodec::new_with_max_length(LINE_LIMIT))
            .take(LINES_LIMIT);
        while let Some(line) = lines.next().await {
            let line = line.map_err(|error| match error {
                LinesCodecError::MaxLineLengthExceeded => ProbeError::TooLarge,
                LinesCodecError::Io(error) => ProbeError::Io(error.kind()),
            })?;
            if let Some(reply) = usage_reply(&line) {
                return parse_reply(&reply);
            }
        }
        Err(ProbeError::NoAnswer)
    };
    let answer = tokio::time::timeout(wait, asked).await.unwrap_or(Err(ProbeError::Timeout));
    if let Err(error) = writer.shutdown().await {
        tracing::debug!(error = ?error.kind(), "claude usage probe stdin did not close");
    }
    answer
}

/// The `response` of the `control_response` that answers the usage request; other lines and
/// lines that are not JSON are skipped.
fn usage_reply(line: &str) -> Option<Value> {
    let mut message: Value = serde_json::from_str(line).ok()?;
    let answers = message.get("type").and_then(Value::as_str) == Some("control_response")
        && message.pointer("/response/request_id").and_then(Value::as_str) == Some(USAGE_REQUEST);
    if !answers {
        return None;
    }
    message.get_mut("response").map(Value::take)
}
```

`LinesCodecError` — внешний enum с двумя вариантами; оба перечислены. Ответ на `initialize` пропускается (его `request_id` другой) — отказ инициализации проявится как отсутствие ответа на `get_usage` (`Timeout`/`NoAnswer`) и кэш.

После задачи 1 — только правка констант: `HANDSHAKE` по таблице шага 7, при необходимости `"-p"` первым элементом `PROBE_ARGS` (размер массива — 8), `EVENT_SCALE` в задаче 5.

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-claude --lib -- usage`
Expected: PASS (10 новых тестов и тесты задачи 5).

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-claude/src/usage.rs
git commit -m "hub-claude: проба лимитов коротким запуском CLI без сообщения"
```

---
### Task 7: Сессия Claude отдаёт `rate_limit_event` хабу

**Files:**
- Modify: `crates/hub-claude/src/session.rs:20-25` (импорт), `:228-270` (`receive`)
- Test: `crates/hub-claude/tests/session.rs` (импорты `:11-17`, новый тест после `plain_turn_relays_text_and_finishes`, `:230-246`)

**Interfaces:**
- Consumes: `usage::{event_update, EVENT_SCALE}` (задачи 5, 6), `AgentEvent::Quota` (задача 2).
- Produces: в потоке событий сессии Claude — `AgentEvent::Quota(QuotaUpdate)` на каждое `rate_limit_event` с окном и `utilization`; событие только со `status` ничего не порождает.

- [ ] **Step 1: Тест (RED)**

`crates/hub-claude/tests/session.rs`, импорты:

```rust
    use hub_claude::usage::{EVENT_SCALE, Scale};
    use hub_core::domain::BackendKind;
    use hub_core::quota::{QuotaUpdate, UsageWindow, WindowKind};
```

тест:

```rust
    fn rate_limit(info: &Value) -> Value {
        json!({"type": "rate_limit_event", "rate_limit_info": info, "uuid": "u", "session_id": "s"})
    }

    #[tokio::test]
    async fn rate_limit_events_become_quota_updates() {
        let utilization = match EVENT_SCALE {
            Scale::Fraction => 0.62,
            Scale::Percent => 62.0,
        };
        let mut harness = Harness::start(FakeChannel::allowing(), limits());
        harness.cli.handshake().await;
        harness.cli.send(&rate_limit(&json!({"status": "allowed"}))).await;
        harness
            .cli
            .send(&rate_limit(&json!({"status": "allowed_warning", "rateLimitType": "five_hour",
                                      "utilization": utilization, "resetsAt": 1_792_160_000})))
            .await;
        harness.cli.send(&text("Привет")).await;
        harness.cli.send(&result(None)).await;

        let (events, _leftover, _cli) = harness.finish().await;
        let window = UsageWindow {
            kind: WindowKind::FiveHour,
            percent: 62,
            resets_at: Some("2026-10-16T14:13:20Z".parse().unwrap()),
        };
        assert_eq!(
            events,
            [
                AgentEvent::Quota(QuotaUpdate {
                    backend: BackendKind::Claude,
                    plan: None,
                    windows: vec![window],
                }),
                AgentEvent::SessionStarted(session()),
                AgentEvent::AssistantText("Привет".to_owned()),
                finished(0)
            ]
        );
    }
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-claude --test session -- rate_limit_events_become_quota_updates`
Expected: FAIL — в событиях нет `AgentEvent::Quota` (событие обрабатывается как раньше `Other`).

- [ ] **Step 3: Реализация**

`crates/hub-claude/src/session.rs`: импорт `use crate::usage;` рядом с `use crate::tracker::SessionTracker;` (`:24`). В `receive` (`:228-270`) убрать `| Incoming::RateLimitEvent { .. }` из ветки `observe`/`translate` (добавлен в задаче 5) и перед ней добавить:

```rust
            Incoming::RateLimitEvent { rate_limit_info } => {
                let Some(update) = usage::event_update(&rate_limit_info, usage::EVENT_SCALE)
                else {
                    return Flow::Continue;
                };
                if self.emit(AgentEvent::Quota(update)).await { Flow::Continue } else { Flow::Stop }
            }
```

`activity` событие не наблюдает (на фазу хода оно и раньше не влияло: `activity.rs:92-98`), `tracker` — тоже (`session_of` для него `None`). Остановка при недоставленном событии — как у остальных событий (`emit` → `false` только после `/stop` или если тема ушла).

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-claude --test session && cargo test -p hub-claude --test replay`
Expected: PASS; снимки `replay` не изменились (в записанных транскриптах событие только со `status`).

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS. До задачи 12 хаб принимает `AgentEvent::Quota` временной веткой задачи 2 (только `debug`-лог).

- [ ] **Step 6: Commit**

```bash
git add crates/hub-claude/src/session.rs crates/hub-claude/tests/session.rs
git commit -m "hub-claude: сессия передаёт окна rate_limit_event хабу"
```

---
### Task 8: Разбор `account/rateLimits/*` и проба Codex (`hub-codex`)

Проба повторяет `probe_auth` (`crates/hub-codex/src/backend.rs:229-236`: короткий `codex app-server`, отказывающий обработчик, `stop`), но без входа по ключу: только рукопожатие (`initialize`, `initialized`, `account/read`) и `account/rateLimits/read`. Разбор и обмен тестируются на паре `crate::testing::pair` (in-memory, все ОС).

**Files:**
- Create: `crates/hub-codex/src/usage.rs`
- Create: `crates/hub-codex/tests/fixtures/rate_limits_read.json`
- Modify: `crates/hub-codex/Cargo.toml:9-18` (зависимость `chrono`)
- Modify: `crates/hub-codex/src/lib.rs:7` (`pub mod usage;` после `mod testing;`)
- Modify: `crates/hub-codex/src/protocol.rs:41-66` (`Call::RateLimitsRead`)
- Modify: `crates/hub-codex/src/backend.rs:184` (`handshake` → `pub(crate)`), `:238-281` (`AppServer`, `spawn`, `stop`, поле `client` → `pub(crate)`)
- Test: `crates/hub-codex/src/usage.rs`

**Interfaces:**
- Produces (`hub_codex::protocol`): `Call::RateLimitsRead` → `"account/rateLimits/read"`.
- Produces (`hub_codex::usage`): `API_KEY_LOGIN = "вход по API-ключу — лимитов подписки нет"` (спецификация), `NOT_LOGGED_IN = "Codex не авторизован — лимитов подписки нет"`; `enum ProbeError { Spawn(io::ErrorKind), Rpc(RpcError), Shape }` (`Debug, Clone, PartialEq, Eq, thiserror::Error`) с `const fn kind(&self) -> &'static str`; `fn parse_snapshot(&Value) -> Result<(Option<String>, Vec<UsageWindow>), ProbeError>`; `fn update_from(params: &Value) -> Option<QuotaUpdate>`; `async fn read(&RpcClient) -> Result<QuotaReport, ProbeError>`; `async fn probe(cli: &Path) -> Result<QuotaReport, ProbeError>`.
- `pub(crate)` в `backend` (модуль `pub`, поэтому сужение явное): `handshake`, `AppServer { client, .. }`, `AppServer::spawn`, `AppServer::stop`.

- [ ] **Step 1: Фикстура и тесты (RED)**

`crates/hub-codex/tests/fixtures/rate_limits_read.json` — `GetAccountRateLimitsResponse` по `account.rs:331-345` (если задача 1 записала живой ответ — положить его и переписать ожидание `the_recorded_read_becomes_a_report`):

```json
{
  "ordinaryUsageAllowed": true,
  "rateLimits": {
    "limitId": "codex",
    "limitName": null,
    "normalModelSlug": null,
    "primary": {"usedPercent": 41, "windowDurationMins": 300, "resetsAt": 1792156200},
    "secondary": {"usedPercent": 12, "windowDurationMins": 10080, "resetsAt": 1792656000},
    "credits": {"hasCredits": false, "unlimited": false, "balance": "0"},
    "individualLimit": null,
    "spendControlReached": null,
    "planType": "plus",
    "rateLimitReachedType": null
  },
  "rateLimitsByLimitId": {"codex": {"limitId": "codex", "primary": null, "secondary": null, "credits": null, "planType": "plus"}},
  "rateLimitResetCredits": null,
  "accountId": "a",
  "rateLimitUpsell": null
}
```

`1792156200` — `2026-10-16T13:10:00Z`, `1792656000` — `2026-10-22T08:00:00Z` (`date -u -d @…`).

`crates/hub-codex/src/usage.rs` — пока только тесты:

```rust
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use rstest::rstest;

    use super::*;
    use crate::testing::{Peer, pair, refusing};

    const RECORDED: &str = include_str!("../tests/fixtures/rate_limits_read.json");

    fn window(label: &str, percent: u8, resets_at: Option<&str>) -> UsageWindow {
        UsageWindow {
            kind: WindowKind::Other(label.to_owned()),
            percent,
            resets_at: resets_at.map(|raw| raw.parse().unwrap()),
        }
    }

    fn recorded() -> Value {
        serde_json::from_str(RECORDED).unwrap()
    }

    #[test]
    fn the_recorded_read_becomes_a_report() {
        let snapshot = recorded().get("rateLimits").cloned().unwrap();
        assert_eq!(
            parse_snapshot(&snapshot),
            Ok((
                Some("ChatGPT Plus".to_owned()),
                vec![
                    window("основное", 41, Some("2026-10-16T13:10:00Z")),
                    window("недельное", 12, Some("2026-10-22T08:00:00Z")),
                ]
            ))
        );
    }

    #[rstest]
    #[case::sparse(json!({"primary": null, "secondary": {"usedPercent": 13}, "planType": null}), Ok((None, vec![window("недельное", 13, None)])))]
    #[case::float_percent(json!({"primary": {"usedPercent": 41.6, "resetsAt": null}}), Ok((None, vec![window("основное", 42, None)])))]
    #[case::clamped(json!({"primary": {"usedPercent": 150}, "secondary": {"usedPercent": -5}}), Ok((None, vec![window("основное", 100, None), window("недельное", 0, None)])))]
    #[case::pro_lite(json!({"planType": "prolite"}), Ok((Some("ChatGPT Pro Lite".to_owned()), Vec::new())))]
    #[case::unknown_plan(json!({"planType": "self_serve_business_usage_based"}), Ok((Some("ChatGPT self_serve_business_usage_based".to_owned()), Vec::new())))]
    #[case::bad_percent(json!({"primary": {"usedPercent": "x"}}), Err(ProbeError::Shape))]
    #[case::not_an_object(json!([1]), Err(ProbeError::Shape))]
    fn snapshots_are_read_tolerantly(
        #[case] snapshot: Value,
        #[case] expected: Result<(Option<String>, Vec<UsageWindow>), ProbeError>,
    ) {
        assert_eq!(parse_snapshot(&snapshot), expected);
    }

    #[rstest]
    #[case::one_window(json!({"rateLimits": {"secondary": {"usedPercent": 20}}}), Some(QuotaUpdate { backend: BackendKind::Codex, plan: None, windows: vec![window("недельное", 20, None)] }))]
    #[case::nothing(json!({"rateLimits": {"primary": null, "planType": null}}), None)]
    #[case::malformed(json!({"rateLimits": 7}), None)]
    #[case::missing(json!({}), None)]
    fn rolling_updates_carry_what_they_name(#[case] params: Value, #[case] expected: Option<QuotaUpdate>) {
        assert_eq!(update_from(&params), expected);
    }

    /// Answers each request with the next reply (`Err` is a JSON-RPC error); once they run
    /// out it stays silent. Returns every method the client sent and the params it sent them with.
    async fn server(mut peer: Peer, replies: Vec<Result<Value, &'static str>>) -> Vec<(String, Value)> {
        let mut seen = Vec::new();
        let mut replies = replies.into_iter();
        while let Some(message) = peer.read().await {
            let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
            seen.push((method.to_owned(), message.get("params").cloned().unwrap_or(Value::Null)));
            let Some(id) = message.get("id").cloned() else { continue };
            match replies.next() {
                Some(Ok(result)) => peer.write(json!({"id": id, "result": result})).await,
                Some(Err(message)) => {
                    peer.write(json!({"id": id, "error": {"code": -32000, "message": message}})).await;
                }
                None => {}
            }
        }
        seen
    }

    async fn probe_over_pair(
        replies: Vec<Result<Value, &'static str>>,
        timeout: Duration,
    ) -> (Result<QuotaReport, ProbeError>, Vec<(String, Value)>) {
        let (connection, peer) = pair(refusing(), timeout);
        let server = tokio::spawn(server(peer, replies));
        let report = read(&connection.client).await;
        connection.client.close();
        (report, server.await.unwrap())
    }

    fn methods(seen: &[(String, Value)]) -> Vec<&str> {
        seen.iter().map(|(method, _)| method.as_str()).collect()
    }

    #[tokio::test]
    async fn a_chatgpt_login_reads_the_limits_without_side_effects() {
        let replies = vec![
            Ok(json!({})),
            Ok(json!({"account": {"type": "chatgpt"}, "requiresOpenaiAuth": true})),
            Ok(recorded()),
        ];
        let (report, seen) = probe_over_pair(replies, Duration::from_secs(5)).await;
        assert_eq!(report.map(|report| report.plan), Ok(Some("ChatGPT Plus".to_owned())));
        assert_eq!(
            methods(&seen),
            ["initialize", "initialized", "account/read", "account/rateLimits/read"]
        );
        assert_eq!(seen.last().map(|(_, params)| params), Some(&json!({"excludeResetCreditDetails": true})));
    }

    #[rstest]
    #[case::api_key(json!({"account": {"type": "apiKey"}}), API_KEY_LOGIN)]
    #[case::not_required(json!({"account": null, "requiresOpenaiAuth": false}), API_KEY_LOGIN)]
    #[case::logged_out(json!({"account": null, "requiresOpenaiAuth": true}), NOT_LOGGED_IN)]
    #[tokio::test]
    async fn without_chatgpt_there_are_no_limits_and_no_login(#[case] account: Value, #[case] reason: &str) {
        let (report, seen) = probe_over_pair(vec![Ok(json!({})), Ok(account)], Duration::from_secs(5)).await;
        assert_eq!(report, Ok(QuotaReport::unavailable(BackendKind::Codex, reason)));
        assert_eq!(methods(&seen), ["initialize", "initialized", "account/read"]);
    }

    #[tokio::test]
    async fn a_refused_read_is_an_error() {
        let replies = vec![Ok(json!({})), Ok(json!({"account": {"type": "chatgpt"}})), Err("usage unavailable")];
        let (report, _seen) = probe_over_pair(replies, Duration::from_secs(5)).await;
        assert_eq!(
            report,
            Err(ProbeError::Rpc(RpcError::Remote { code: -32000, message: "usage unavailable".to_owned() }))
        );
    }

    #[tokio::test]
    async fn a_silent_read_times_out() {
        let replies = vec![Ok(json!({})), Ok(json!({"account": {"type": "chatgpt"}}))];
        let (report, _seen) = probe_over_pair(replies, Duration::from_millis(100)).await;
        assert!(matches!(report, Err(ProbeError::Rpc(RpcError::Timeout { .. }))), "{report:?}");
    }

    #[tokio::test]
    async fn a_read_without_rate_limits_is_a_shape_error() {
        let replies = vec![Ok(json!({})), Ok(json!({"account": {"type": "chatgpt"}})), Ok(json!({}))];
        let (report, _seen) = probe_over_pair(replies, Duration::from_secs(5)).await;
        assert_eq!(report, Err(ProbeError::Shape));
    }

    #[tokio::test]
    async fn a_missing_binary_is_a_spawn_error() {
        let outcome = probe(Path::new("definitely-not-codex-binary")).await;
        assert!(matches!(outcome, Err(ProbeError::Spawn(_))), "{outcome:?}");
    }
}
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-codex --lib -- usage`
Expected: FAIL — ошибки компиляции: нет `parse_snapshot`, `update_from`, `read`, `probe`, `ProbeError`, `API_KEY_LOGIN`.

- [ ] **Step 3: Реализация**

`crates/hub-codex/Cargo.toml`, `[dependencies]` после `base64.workspace = true` (`:10`) — `chrono.workspace = true`. `crates/hub-codex/src/lib.rs` после `mod testing;` (`:7`) — `pub mod usage;`.

`crates/hub-codex/src/protocol.rs`: вариант `RateLimitsRead` в `Call` (после `Login`, `:46`), в `method` — `Self::RateLimitsRead => "account/rateLimits/read"`.

`crates/hub-codex/src/backend.rs`: `async fn handshake` (`:184`) → `pub(crate) async fn handshake`; `struct AppServer` (`:238`) → `pub(crate) struct AppServer`, поле `client: RpcClient` → `pub(crate) client: RpcClient`; `fn spawn` (`:246`) → `pub(crate) fn spawn`; `async fn stop` (`:270`) → `pub(crate) async fn stop`. Поведение не меняется.

`crates/hub-codex/src/usage.rs` (над тестами):

```rust
//! Subscription limits from Codex: `account/rateLimits/read`, its sparse
//! `account/rateLimits/updated`, and a short app-server run that asks for them. The probe
//! never logs in with the configured key and never opens a thread.

use std::io;
use std::path::Path;
use std::sync::Arc;

use chrono::DateTime;
use hub_agent::rpc::{Handler, RequestError, RpcClient, RpcError};
use hub_core::domain::BackendKind;
use hub_core::quota::{QuotaReport, QuotaUpdate, UsageWindow, WindowKind, clean_label, percent};
use serde_json::{Value, json};

use crate::backend::{AppServer, handshake};
use crate::protocol::{Call, CodexAuth};

pub const API_KEY_LOGIN: &str = "вход по API-ключу — лимитов подписки нет";
pub const NOT_LOGGED_IN: &str = "Codex не авторизован — лимитов подписки нет";
// The spec's card names the windows by position, not by duration.
const PRIMARY: &str = "основное";
const SECONDARY: &str = "недельное";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProbeError {
    #[error("не удалось запустить Codex app-server: {0}")]
    Spawn(io::ErrorKind),
    #[error("Codex app-server: {0}")]
    Rpc(#[from] RpcError),
    #[error("непонятный ответ Codex о лимитах")]
    Shape,
}

impl ProbeError {
    /// What goes to the log: never the server's text.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Spawn(_) => "spawn",
            Self::Rpc(RpcError::Remote { .. }) => "remote",
            Self::Rpc(RpcError::Closed) => "closed",
            Self::Rpc(RpcError::Protocol(_)) => "protocol",
            Self::Rpc(RpcError::Timeout { .. }) => "timeout",
            Self::Shape => "shape",
        }
    }
}

/// Asks a short-lived app-server for the plan limits.
pub async fn probe(cli: &Path) -> Result<QuotaReport, ProbeError> {
    let refuse: Handler =
        Arc::new(|method, _params| Box::pin(async move { Err(RequestError::Unsupported(method)) }));
    let server = AppServer::spawn(cli, refuse).map_err(|error| ProbeError::Spawn(error.kind()))?;
    let report = read(&server.client).await;
    server.stop().await;
    report
}

/// The handshake, then the limits; a login other than `ChatGPT` has none, which is an
/// answer, not an error.
pub async fn read(client: &RpcClient) -> Result<QuotaReport, ProbeError> {
    match handshake(client).await? {
        CodexAuth::ChatGpt => {}
        CodexAuth::Missing => {
            return Ok(QuotaReport::unavailable(BackendKind::Codex, NOT_LOGGED_IN));
        }
        CodexAuth::ApiKey | CodexAuth::Other | CodexAuth::NotRequired => {
            return Ok(QuotaReport::unavailable(BackendKind::Codex, API_KEY_LOGIN));
        }
    }
    let params = json!({"excludeResetCreditDetails": true});
    let result = client.request(Call::RateLimitsRead.method(), params).await?;
    let (plan, windows) = parse_snapshot(result.get("rateLimits").ok_or(ProbeError::Shape)?)?;
    Ok(QuotaReport::fresh(BackendKind::Codex, plan, windows))
}

/// A sparse `account/rateLimits/updated` as a session update; `None` when it names nothing.
#[must_use]
pub fn update_from(params: &Value) -> Option<QuotaUpdate> {
    // A malformed rolling update is skipped: the next one, or a probe, brings the values.
    let (plan, windows) = parse_snapshot(params.get("rateLimits")?).ok()?;
    (plan.is_some() || !windows.is_empty()).then_some(QuotaUpdate {
        backend: BackendKind::Codex,
        plan,
        windows,
    })
}

/// A `RateLimitSnapshot`: the plan and the windows it names; `null` or absent ones are left out.
pub fn parse_snapshot(snapshot: &Value) -> Result<(Option<String>, Vec<UsageWindow>), ProbeError> {
    if !snapshot.is_object() {
        return Err(ProbeError::Shape);
    }
    let windows = [(PRIMARY, "primary"), (SECONDARY, "secondary")]
        .into_iter()
        .filter_map(|(label, key)| match snapshot.get(key) {
            None | Some(Value::Null) => None,
            Some(window) => Some(parse_window(label, window)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let plan = snapshot.get("planType").and_then(Value::as_str).and_then(plan_name);
    Ok((plan, windows))
}

fn parse_window(label: &str, window: &Value) -> Result<UsageWindow, ProbeError> {
    let used = window.get("usedPercent").ok_or(ProbeError::Shape)?;
    let percent = match used.as_i64() {
        Some(whole) => u8::try_from(whole.clamp(0, 100)).ok(),
        None => used.as_f64().and_then(percent),
    }
    .ok_or(ProbeError::Shape)?;
    let resets_at = window
        .get("resetsAt")
        .and_then(Value::as_i64)
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0));
    Ok(UsageWindow { kind: WindowKind::Other(label.to_owned()), percent, resets_at })
}

/// `plus` → `ChatGPT Plus`; an unknown plan keeps its name.
fn plan_name(raw: &str) -> Option<String> {
    let known = match raw {
        "free" => "Free",
        "go" => "Go",
        "plus" => "Plus",
        "pro" => "Pro",
        "prolite" => "Pro Lite",
        "promax" => "Pro Max",
        "team" => "Team",
        "business" => "Business",
        "enterprise" => "Enterprise",
        "edu" => "Edu",
        other => return clean_label(other).map(|name| format!("ChatGPT {name}")),
    };
    Some(format!("ChatGPT {known}"))
}
```

`RpcError` — enum `hub-agent`, а не этого крейта, но все его варианты перечислены явно. `let percent = match … { … None => used.as_f64().and_then(percent) }` — внутри выражения `percent` ещё функция (привязка появляется после `;`).

- [ ] **Step 4: Проверить GREEN**

Run: `cargo check --workspace --all-targets && git diff --stat Cargo.lock && cargo test -p hub-codex --lib -- usage backend`
Expected: в `Cargo.lock` только ребро `hub-codex → chrono`; новые тесты и прежние тесты `backend` PASS.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-codex/Cargo.toml Cargo.lock crates/hub-codex/src/lib.rs crates/hub-codex/src/usage.rs crates/hub-codex/src/protocol.rs crates/hub-codex/src/backend.rs crates/hub-codex/tests/fixtures/rate_limits_read.json
git commit -m "hub-codex: разбор account/rateLimits и проба лимитов без входа по ключу"
```

---
### Task 9: Сессия Codex отдаёт `account/rateLimits/updated` хабу

**Files:**
- Modify: `crates/hub-codex/src/protocol.rs:16` (импорт), `:243-263` (`TurnTracker::translate`), тесты `:498-510`
- Test: `crates/hub-codex/src/session.rs` (импорты `:161-162`, новый тест после `a_turn_relays_text_and_finishes`, `:284-302`)

**Interfaces:**
- Consumes: `usage::update_from` (задача 8), `AgentEvent::Quota` (задача 2).
- Produces: `TurnTracker::translate` на `account/rateLimits/updated` → `Translation { events: [AgentEvent::Quota(update)], completed: None }` или пустой перевод, если уведомление ничего не называет.

- [ ] **Step 1: Тесты (RED)**

`crates/hub-codex/src/protocol.rs`, модуль `tests`: импорт `use hub_core::quota::{QuotaUpdate, UsageWindow, WindowKind};` и случаи в таблицу `notifications_become_events` (`:498-502`):

```rust
    #[case(notification("account/rateLimits/updated", json!({"rateLimits": {"primary": {"usedPercent": 30, "resetsAt": null}, "planType": null}})), vec![AgentEvent::Quota(QuotaUpdate { backend: BackendKind::Codex, plan: None, windows: vec![UsageWindow { kind: WindowKind::Other("основное".to_owned()), percent: 30, resets_at: None }] })])]
    #[case(notification("account/rateLimits/updated", json!({"rateLimits": {"primary": null}})), vec![])]
```

`crates/hub-codex/src/session.rs`, модуль `tests`: импорты `use hub_core::domain::BackendKind;` и `use hub_core::quota::{QuotaUpdate, UsageWindow, WindowKind};`, тест:

```rust
    #[tokio::test]
    async fn rate_limit_updates_reach_the_hub_mid_turn() {
        let mut harness = start(FakeThread::default());
        harness
            .notify
            .send(Notification {
                method: "account/rateLimits/updated".to_owned(),
                params: json!({"rateLimits": {"secondary": {"usedPercent": 12}, "planType": "plus"}}),
            })
            .unwrap();
        harness.notify.send(completed("u-1")).unwrap();
        let (_inbox, outcome) = harness.done.await.unwrap();
        assert_eq!(outcome, Ok(()));
        let update = QuotaUpdate {
            backend: BackendKind::Codex,
            plan: Some("ChatGPT Plus".to_owned()),
            windows: vec![UsageWindow {
                kind: WindowKind::Other("недельное".to_owned()),
                percent: 12,
                resets_at: None,
            }],
        };
        assert_eq!(drain(&mut harness.events).first(), Some(&AgentEvent::Quota(update)));
    }
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-codex --lib -- protocol session`
Expected: FAIL — `account/rateLimits/updated` уходит в ветку `_` и событий не даёт.

- [ ] **Step 3: Реализация**

`crates/hub-codex/src/protocol.rs`: после `use hub_agent::rpc::{Notification, RpcError};` (`:16`) — `use crate::usage;`; в `translate` перед `_ => Ok(Translation::default())` (`:261`):

```rust
            "account/rateLimits/updated" => {
                Ok(Translation::of(usage::update_from(params).map(AgentEvent::Quota)))
            }
```

Уведомление глобальное для аккаунта, а не для потока: оно приходит в любую сессию app-server, и хаб сливает его в кэш (задачи 10, 12). Ход оно не завершает (`completed: None`).

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-codex --lib -- protocol session usage`
Expected: PASS.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-codex/src/protocol.rs crates/hub-codex/src/session.rs
git commit -m "hub-codex: сессия передаёт обновления лимитов хабу"
```

---
### Task 10: Кэш квот и выбор «свежее или кэш» (`hub-telegram::limits`)

Чистое ядро без ввода-вывода: хаб (задача 12) владеет `QuotaCache`, передаёт время явно.

**Files:**
- Create: `crates/hub-telegram/src/limits.rs`
- Modify: `crates/hub-telegram/src/lib.rs:5` (`pub mod limits;` после `pub mod inbound;`; задача 12 сужает до `mod limits;`)
- Test: `crates/hub-telegram/src/limits.rs`

**Interfaces:**
- Produces (`hub_telegram::limits`; до задачи 12 модуль `pub`, чтобы ещё не используемые хабом элементы не давали `dead_code`, затем `mod limits;`): `REFRESH: &str = "limits:refresh"` (данные кнопки, ≤ 64 байт); `type Outcome = (BackendKind, Result<QuotaReport, String>)`; `struct QuotaCache` (`Default`) с `apply(&mut self, QuotaUpdate, DateTime<Utc>)` и `settle(&mut self, Outcome, DateTime<Utc>) -> QuotaReport`; `fn ordered(Vec<QuotaReport>, default: BackendKind) -> Vec<QuotaReport>`; `fn refresh_keyboard() -> Vec<Vec<Button>>`.
- Consumes: `hub_core::quota` (задача 2), `texts::LIMITS_REFRESH` (задача 4).

Правила: `settle(Ok(Fresh))` заменяет запись кэша целиком и возвращает отчёт; `settle(Ok(Unavailable | NotReported | Cached))` — окончательный ответ, возвращается как есть, кэш не трогает; `settle(Err(reason))` — запись кэша как `QuotaSource::Cached { at }`, а без записи — `Unavailable(reason)` (без управляющих символов, ≤ 200 символов). `apply` — слияние разреженного обновления: окно того же вида заменяется, новое добавляется, пока окон меньше 8; `plan: None` не стирает тариф; `at` — время последнего обновления.

- [ ] **Step 1: Тесты (RED)**

`crates/hub-telegram/src/limits.rs` — пока только тесты; в `lib.rs` после `pub mod inbound;` — `pub mod limits;`.

```rust
#[cfg(test)]
mod tests {
    use hub_core::quota::WindowKind;

    use super::*;

    fn at(raw: &str) -> DateTime<Utc> {
        raw.parse().unwrap()
    }

    fn window(kind: WindowKind, percent: u8) -> UsageWindow {
        UsageWindow { kind, percent, resets_at: None }
    }

    fn update(backend: BackendKind, plan: Option<&str>, windows: Vec<UsageWindow>) -> QuotaUpdate {
        QuotaUpdate { backend, plan: plan.map(str::to_owned), windows }
    }

    fn cached(backend: BackendKind, plan: Option<&str>, windows: Vec<UsageWindow>, when: &str) -> QuotaReport {
        QuotaReport {
            backend,
            plan: plan.map(str::to_owned),
            windows,
            source: QuotaSource::Cached { at: at(when) },
        }
    }

    const T1: &str = "2026-10-16T10:00:00Z";
    const T2: &str = "2026-10-16T11:00:00Z";
    const T3: &str = "2026-10-16T12:00:00Z";

    #[test]
    fn rolling_updates_merge_by_window_and_keep_the_plan() {
        let mut cache = QuotaCache::default();
        let main = || WindowKind::Other("основное".to_owned());
        let weekly = || WindowKind::Other("недельное".to_owned());
        cache.apply(update(BackendKind::Codex, Some("ChatGPT Plus"), vec![window(main(), 10)]), at(T1));
        cache.apply(update(BackendKind::Codex, None, vec![window(weekly(), 20)]), at(T2));
        cache.apply(update(BackendKind::Codex, None, vec![window(main(), 30)]), at(T2));
        assert_eq!(
            cache.settle((BackendKind::Codex, Err("тайм-аут".to_owned())), at(T3)),
            cached(BackendKind::Codex, Some("ChatGPT Plus"), vec![window(main(), 30), window(weekly(), 20)], T2)
        );
    }

    #[test]
    fn a_fresh_report_replaces_the_cache_and_is_shown_as_is() {
        let mut cache = QuotaCache::default();
        cache.apply(update(BackendKind::Claude, None, vec![window(WindowKind::FiveHour, 10)]), at(T1));
        let fresh = QuotaReport::fresh(BackendKind::Claude, Some("Max".to_owned()), vec![window(WindowKind::Week, 5)]);
        assert_eq!(cache.settle((BackendKind::Claude, Ok(fresh.clone())), at(T2)), fresh);
        assert_eq!(
            cache.settle((BackendKind::Claude, Err("сбой".to_owned())), at(T3)),
            cached(BackendKind::Claude, Some("Max"), vec![window(WindowKind::Week, 5)], T2)
        );
    }

    #[test]
    fn a_definitive_answer_is_not_replaced_by_the_cache() {
        let mut cache = QuotaCache::default();
        cache.apply(update(BackendKind::Codex, None, vec![window(WindowKind::Week, 10)]), at(T1));
        let answer = QuotaReport::unavailable(BackendKind::Codex, "вход по API-ключу — лимитов подписки нет");
        assert_eq!(cache.settle((BackendKind::Codex, Ok(answer.clone())), at(T2)), answer);
        let silent = QuotaReport::not_reported(BackendKind::Qwen);
        assert_eq!(cache.settle((BackendKind::Qwen, Ok(silent.clone())), at(T2)), silent);
    }

    #[test]
    fn a_failure_without_a_cache_shows_its_reason() {
        let mut cache = QuotaCache::default();
        let reason = format!("сбой\n{}", "x".repeat(300));
        let report = cache.settle((BackendKind::Claude, Err(reason)), at(T1));
        let QuotaSource::Unavailable(shown) = &report.source else { panic!("{report:?}") };
        assert!(shown.starts_with("сбойxxx"), "{shown}");
        assert_eq!(shown.chars().count(), 200);
    }

    #[test]
    fn the_cache_is_bounded_per_backend() {
        let mut cache = QuotaCache::default();
        let many = (0..12).map(|i| window(WindowKind::Other(format!("w{i}")), 1)).collect();
        cache.apply(update(BackendKind::Claude, None, many), at(T1));
        let report = cache.settle((BackendKind::Claude, Err("сбой".to_owned())), at(T2));
        assert_eq!(report.windows.len(), hub_core::quota::WINDOW_LIMIT);
    }

    #[test]
    fn the_default_backend_comes_first() {
        let reports = [BackendKind::Claude, BackendKind::Codex, BackendKind::Qwen]
            .into_iter()
            .map(QuotaReport::not_reported)
            .collect();
        let order: Vec<_> = ordered(reports, BackendKind::Codex).into_iter().map(|report| report.backend).collect();
        assert_eq!(order, [BackendKind::Codex, BackendKind::Claude, BackendKind::Qwen]);
    }

    #[test]
    fn the_refresh_button_fits_telegram() {
        let keyboard = refresh_keyboard();
        assert_eq!(
            keyboard,
            [[Button { text: "🔄 Обновить".to_owned(), data: REFRESH.to_owned() }]]
        );
        assert!(REFRESH.len() <= 64);
    }
}
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-telegram --lib -- limits`
Expected: FAIL — ошибки компиляции: нет `QuotaCache`, `ordered`, `refresh_keyboard`, `REFRESH`.

- [ ] **Step 3: Реализация**

`crates/hub-telegram/src/limits.rs` (над тестами):

```rust
//! Data of the `/limits` card: the last quotas live sessions reported, and the choice between
//! a probe's answer and that cache. Pure: the hub owns the cache and passes the time.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use hub_core::domain::BackendKind;
use hub_core::questions::Button;
use hub_core::quota::{QuotaReport, QuotaSource, QuotaUpdate, UsageWindow, WINDOW_LIMIT};
use hub_core::render::truncate;

use crate::texts;

/// Callback data of the refresh button.
pub const REFRESH: &str = "limits:refresh";
/// Longest probe failure shown on the card.
const REASON_LIMIT: usize = 200;

/// One backend's probe: its report, or a failure the cache may stand in for.
pub type Outcome = (BackendKind, Result<QuotaReport, String>);

#[derive(Debug, Clone, PartialEq, Eq)]
struct Cached {
    plan: Option<String>,
    windows: Vec<UsageWindow>,
    at: DateTime<Utc>,
}

/// One entry per backend with at most `WINDOW_LIMIT` windows: at most four entries in all.
#[derive(Debug, Default)]
pub struct QuotaCache {
    entries: HashMap<BackendKind, Cached>,
}

impl QuotaCache {
    /// A live session's rolling update: a window replaces the cached one of the same kind, a
    /// missing plan keeps the cached plan.
    pub fn apply(&mut self, update: QuotaUpdate, at: DateTime<Utc>) {
        let QuotaUpdate { backend, plan, windows } = update;
        let entry = self
            .entries
            .entry(backend)
            .or_insert_with(|| Cached { plan: None, windows: Vec::new(), at });
        for window in windows {
            match entry.windows.iter_mut().find(|cached| cached.kind == window.kind) {
                Some(cached) => *cached = window,
                None if entry.windows.len() < WINDOW_LIMIT => entry.windows.push(window),
                None => {}
            }
        }
        if plan.is_some() {
            entry.plan = plan;
        }
        entry.at = at;
    }

    /// What the card shows for one probe: a fresh report, which also replaces the cache; a
    /// definitive answer as is; for a failure, the cache dated by its last update.
    pub fn settle(&mut self, outcome: Outcome, now: DateTime<Utc>) -> QuotaReport {
        let (backend, result) = outcome;
        match result {
            Ok(report) => {
                match &report.source {
                    QuotaSource::Fresh => {
                        let fresh = Cached {
                            plan: report.plan.clone(),
                            windows: report.windows.clone(),
                            at: now,
                        };
                        self.entries.insert(backend, fresh);
                    }
                    QuotaSource::Cached { .. }
                    | QuotaSource::NotReported
                    | QuotaSource::Unavailable(_) => {}
                }
                report
            }
            Err(reason) => match self.entries.get(&backend) {
                Some(Cached { plan, windows, at }) => QuotaReport {
                    backend,
                    plan: plan.clone(),
                    windows: windows.clone(),
                    source: QuotaSource::Cached { at: *at },
                },
                None => QuotaReport::unavailable(backend, reason_text(&reason)),
            },
        }
    }
}

fn reason_text(raw: &str) -> String {
    let printable: String = raw.chars().filter(|c| !c.is_control()).collect();
    truncate(printable.trim(), REASON_LIMIT)
}

/// The default backend first, the others in the order they came.
#[must_use]
pub fn ordered(reports: Vec<QuotaReport>, default: BackendKind) -> Vec<QuotaReport> {
    let (first, rest): (Vec<_>, Vec<_>) =
        reports.into_iter().partition(|report| report.backend == default);
    first.into_iter().chain(rest).collect()
}

#[must_use]
pub fn refresh_keyboard() -> Vec<Vec<Button>> {
    vec![vec![Button { text: texts::LIMITS_REFRESH.to_owned(), data: REFRESH.to_owned() }]]
}
```

Тест `a_failure_without_a_cache_shows_its_reason`: `\n` удаляется, 304 символа обрезаются до 199 и `…` — всего 200.

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-telegram --lib -- limits`
Expected: PASS (7 тестов). Модуль пока `pub`: хаб начнёт им пользоваться в задаче 12, до этого приватный модуль дал бы `dead_code` (а `warnings = "deny"`).

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-telegram/src/lib.rs crates/hub-telegram/src/limits.rs
git commit -m "hub-telegram: кэш лимитов подписок из живых сессий"
```

---
### Task 11: `Agents::quotas` и `HubAgents` — пробы найденных CLI

**Files:**
- Modify: `crates/hub-telegram/src/hub.rs:16-18` (импорт `QuotaReport`), `:47-63` (трейт `Agents`), тесты `:962-1004` (`impl Agents for FakeAgents` — заглушка)
- Modify: `crates/hub-telegram/src/agents.rs:3-22` (импорты), `:91-98` (метод в `impl Agents for HubAgents`), после `:156` (функции `quotas`, `failed`), тесты `:168-325`

**Interfaces:**
- Produces (`hub_telegram::hub::Agents`): `fn quotas(&self) -> BoxFuture<'_, Vec<(BackendKind, Result<QuotaReport, String>)>>` — по одному элементу на бэкенд, CLI которого найден (`version::locate` → `Ok`), в порядке `BackendKind::ALL`; `Err` — сбой или тайм-аут пробы (текст для карточки).
- Produces (`agents.rs`): `QUOTA_TIMEOUT: Duration = 20 с`; `async fn quotas(&Settings) -> Vec<Outcome>`.
- Consumes: `hub_claude::usage::probe`, `hub_codex::usage::probe` (задачи 6, 8), `limits::Outcome` (задача 10).

- [ ] **Step 1: Тест (RED)**

`crates/hub-telegram/src/agents.rs`, модуль `tests`:

```rust
    #[tokio::test]
    async fn quotas_probe_every_configured_cli_and_isolate_failures() {
        let root = std::env::temp_dir();
        let missing = root.join("definitely-not-an-agent-binary").display().to_string();
        let mut form = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            ..Draft::default()
        };
        form.cli.clone_from(&missing);
        form.codex.cli.clone_from(&missing);
        form.qwen.cli.clone_from(&missing);
        form.hermes.cli = missing;
        let agents =
            HubAgents::new(watch::Sender::new(Arc::new(form.parse(&root).unwrap())).subscribe());

        let outcomes = agents.quotas().await;

        let backends: Vec<_> = outcomes.iter().map(|(backend, _)| *backend).collect();
        assert_eq!(backends, BackendKind::ALL);
        let [(_, claude), (_, codex), (_, qwen), (_, hermes)] = outcomes.as_slice() else {
            panic!("{outcomes:?}");
        };
        assert!(
            matches!(claude, Err(reason) if reason.starts_with("не удалось запустить Claude")
                || reason.starts_with("проба лимитов Claude отключена")),
            "{claude:?}"
        );
        assert!(
            matches!(codex, Err(reason) if reason.starts_with("не удалось запустить Codex app-server")),
            "{codex:?}"
        );
        assert_eq!(qwen, &Ok(QuotaReport::not_reported(BackendKind::Qwen)));
        assert_eq!(hermes, &Ok(QuotaReport::not_reported(BackendKind::Hermes)));
    }
```

Пропуск ненайденного CLI (`which` не нашёл, путь не задан) юнит-тестом не проверяется: для этого нужно менять `PATH`, а `std::env::set_var` в edition 2024 — `unsafe` (запрещён). Проверяется вручную в задаче 14.

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-telegram --lib -- agents`
Expected: FAIL — ошибка компиляции: нет метода `quotas` у `HubAgents`.

- [ ] **Step 3: Реализация**

`crates/hub-telegram/src/hub.rs`: импорт `use hub_core::quota::QuotaReport;` (после `use hub_core::domain::{…};`, `:16-18`); в трейт `Agents` (`:47-63`) после `archive`:

```rust
    /// Subscription limits of every backend whose CLI is found, in `BackendKind::ALL` order;
    /// `Err` is a probe failure or timeout, which the hub may answer from its cache.
    fn quotas(&self) -> BoxFuture<'_, Vec<(BackendKind, Result<QuotaReport, String>)>>;
```

`impl Agents for FakeAgents` (тесты, `:962`) — временная заглушка (задача 12 делает её сценарной):

```rust
        fn quotas(&self) -> BoxFuture<'_, Vec<(BackendKind, Result<QuotaReport, String>)>> {
            Box::pin(async { Vec::new() })
        }
```

`crates/hub-telegram/src/agents.rs`, импорты:

```rust
use std::time::Duration;

use futures::future::join_all;
use hub_core::quota::QuotaReport;

use crate::limits::Outcome;
```

в `impl Agents for HubAgents` после `archive` (`:91-98`):

```rust
    fn quotas(&self) -> BoxFuture<'_, Vec<Outcome>> {
        let settings = Arc::clone(&self.settings.borrow());
        Box::pin(async move { quotas(&settings).await })
    }
```

после `archive` (функции, `:103-156`):

```rust
/// Per backend, on top of the probes' own deadlines; dropping a probe kills its process.
const QUOTA_TIMEOUT: Duration = Duration::from_secs(20);

/// Probes of every found CLI, side by side: one backend's failure or timeout leaves the
/// others alone. A CLI that is not found is skipped, as the card shows found agents only.
async fn quotas(settings: &Settings) -> Vec<Outcome> {
    let probes = BackendKind::ALL.into_iter().filter_map(|backend| {
        let probe: BoxFuture<'static, Result<QuotaReport, String>> = match backend {
            BackendKind::Claude => {
                let cli = hub_claude::version::locate(settings.claude.cli.as_deref()).ok()?;
                Box::pin(async move {
                    hub_claude::usage::probe(&cli)
                        .await
                        .map_err(|error| failed(backend, error.kind(), error.to_string()))
                })
            }
            BackendKind::Codex => {
                let cli = hub_codex::version::locate(settings.codex.cli.as_deref()).ok()?;
                Box::pin(async move {
                    hub_codex::usage::probe(&cli)
                        .await
                        .map_err(|error| failed(backend, error.kind(), error.to_string()))
                })
            }
            BackendKind::Qwen => {
                hub_qwen::version::locate(settings.qwen.cli.as_deref()).ok()?;
                Box::pin(async { Ok(QuotaReport::not_reported(BackendKind::Qwen)) })
            }
            BackendKind::Hermes => {
                hub_hermes::version::locate(settings.hermes.cli.as_deref()).ok()?;
                Box::pin(async { Ok(QuotaReport::not_reported(BackendKind::Hermes)) })
            }
        };
        Some(async move {
            let outcome = tokio::time::timeout(QUOTA_TIMEOUT, probe).await.unwrap_or_else(|_| {
                let text =
                    format!("агент не ответил о лимитах за {} с", QUOTA_TIMEOUT.as_secs());
                Err(failed(backend, "timeout", text))
            });
            (backend, outcome)
        })
    });
    join_all(probes).await
}

/// Logs the backend and the kind of failure only: the text can carry the agent's words.
fn failed(backend: BackendKind, kind: &'static str, text: String) -> String {
    tracing::warn!(backend = backend.name(), error = kind, "quota probe failed");
    text
}
```

`locate(...).ok()?` отбрасывает `CliError::Missing` намеренно: «CLI не найден» — значит «бэкенда нет в карточке» (спецификация), а не ошибка. Пробы не вызывают `version::check` (расхождение 10).

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-telegram --lib -- agents`
Expected: PASS: Claude и Codex — `Err` от запуска, Qwen и Hermes — `NotReported`, порядок — `BackendKind::ALL`.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-telegram/src/hub.rs crates/hub-telegram/src/agents.rs
git commit -m "Хаб опрашивает лимиты подписок у найденных CLI параллельно"
```

---
### Task 12: Хаб — `/limits`, «Обновить», кэш из живых сессий

Номера строк `hub.rs` — по состоянию `19d1760`; задачи 4 и 11 сдвигают их на несколько строк, поэтому у каждой правки указан якорь.

**Files:**
- Modify: `crates/hub-telegram/src/hub.rs` — импорты (`:9-38`), `HubMessage` (`:83-106`, якорь `Shutdown(oneshot::Sender<()>),`), после `SnapshotDone` (`:110`), поля `Hub` (`:136-161`, якорь `reserved: HashMap<Reservation, TopicKey>,`) и `spawn` (`:185`), `handle` (`:219-225`, якорь `HubMessage::Press(pressed) =>`), `outside_topic` (`:297-316`), `command` (якорь `Command::Limits =>` из задачи 4), новые методы перед `say` (`:892`); тесты — `FakeAgents` (`:944-1004`), новые тесты после `general_topic_asks_for_a_topic` (`:1250-1258`)
- Modify: `crates/hub-telegram/src/session.rs` (ветка `AgentEvent::Quota` из задачи 2)
- Modify: `crates/hub-telegram/src/texts.rs` (удалить `LIMITS_PENDING`)
- Modify: `crates/hub-telegram/src/lib.rs` (`pub mod limits;` → `mod limits;`)
- Modify: `crates/hub-app/src/supervisor.rs:483-490` (новые варианты `HubMessage` в исчерпывающем `match`)

**Interfaces:**
- Produces (`hub_telegram::hub`): `HubMessage::Quota(QuotaUpdate)`, `HubMessage::Limits(LimitsDone)`; `pub struct LimitsDone(Polled)` (поле приватное, создаёт только хаб); приватная `struct Polled { chat: ChatId, message: Option<MessageId>, outcomes: Option<Vec<Outcome>> }` — `message: None` — заглушку не удалось отправить, `outcomes: None` — опрос упал.
- Поведение: `/limits` в теме (`Target::Topic`) и в общем чате (`Target::Reply`): если опрос уже идёт — `LIMITS_BUSY`; иначе `polling = true`, заглушка `⏳ Собираю…` (Plain), `Agents::quotas`, затем `HubMessage::Limits` → `settle` каждого исхода через кэш → `ordered` (бэкенд по умолчанию первым) → `render_limits(…, Utc::now(), &chrono::Local)` → `edit(Html)` + `edit_keyboard([[🔄 Обновить]])`. Нажатие с данными `limits::REFRESH`: ответ на callback без текста; если опрос не идёт и у нажатия есть чат и сообщение — `edit(⏳ Собираю…, Plain)` (клавиатура снимается) и тот же опрос с этим сообщением; иначе — ничего. Остальные нажатия — `press::handle`, как раньше. `HubMessage::Quota` → `quotas.apply(update, Utc::now())`.

- [ ] **Step 1: Тесты (RED)**

`crates/hub-telegram/src/hub.rs`, модуль `tests`. Импорты:

```rust
    use std::sync::atomic::AtomicUsize;

    use hub_core::quota::{QuotaReport, QuotaUpdate, UsageWindow, WindowKind};

    use crate::limits::{self, Outcome};
```

`FakeAgents` — новые поля и сценарный `quotas` вместо заглушки задачи 11:

```rust
        /// Outcomes of the next polls, in order; afterwards a poll finds no CLI.
        quotas: StdMutex<VecDeque<Vec<Outcome>>>,
        /// The next poll waits until this is released.
        hold_quotas: StdMutex<Option<oneshot::Receiver<()>>>,
        /// The next poll panics, as a bug in a probe would.
        crash_quotas: AtomicBool,
        polls: AtomicUsize,
```

```rust
        fn quotas(&self) -> BoxFuture<'_, Vec<(BackendKind, Result<QuotaReport, String>)>> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            let outcomes = self.quotas.lock().unwrap().pop_front().unwrap_or_default();
            let hold = self.hold_quotas.lock().unwrap().take();
            let crash = self.crash_quotas.swap(false, Ordering::SeqCst);
            Box::pin(async move {
                if let Some(release) = hold {
                    let _ = release.await;
                }
                assert!(!crash, "опрос упал");
                outcomes
            })
        }
```

Помощники и тесты:

```rust
    impl FakeAgents {
        fn polling(polls: Vec<Vec<Outcome>>) -> Self {
            Self { quotas: StdMutex::new(polls.into()), ..Self::default() }
        }
    }

    fn five_hour(percent: u8) -> Vec<UsageWindow> {
        vec![UsageWindow { kind: WindowKind::FiveHour, percent, resets_at: None }]
    }

    fn claude_max(percent: u8) -> Outcome {
        (
            BackendKind::Claude,
            Ok(QuotaReport::fresh(BackendKind::Claude, Some("Max".to_owned()), five_hour(percent))),
        )
    }

    fn press(callback: &str, message: i32, data: &str) -> HubMessage {
        HubMessage::Press(Press {
            callback: callback.to_owned(),
            user: UserId(1),
            chat: Some(ChatId(-100)),
            message: Some(MessageId(message)),
            html: None,
            data: data.to_owned(),
        })
    }

    /// Texts the message `id` was edited to, in order.
    fn edits(world: &World, id: i32) -> Vec<(String, Format)> {
        world
            .messenger
            .calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::Edit { message, text, format, .. } if message == MessageId(id) => {
                    Some((text, format))
                }
                _ => None,
            })
            .collect()
    }

    fn has_card(world: &World, id: i32, needle: &str) -> bool {
        edits(world, id)
            .iter()
            .any(|(text, format)| *format == Format::Html && text.contains(needle))
    }

    #[tokio::test]
    async fn limits_turn_a_placeholder_into_the_card() {
        let world = world(FakeAgents::polling(vec![vec![
            claude_max(62),
            (BackendKind::Qwen, Ok(QuotaReport::not_reported(BackendKind::Qwen))),
        ]]));
        world.send(text(Some(7), 1, "/limits")).await;

        eventually("card", || has_card(&world, 1, "<b>Claude</b> (Max)")).await;
        assert!(world.messenger.calls().contains(&Call::Send {
            target: Target::Topic(KEY),
            message: Outgoing::plain(texts::LIMITS_COLLECTING.to_owned()),
            id: MessageId(1),
        }));
        assert!(has_card(&world, 1, "62%"));
        assert!(has_card(&world, 1, "Qwen — квоты подписки агент не сообщает"));
        eventually("refresh button", || {
            world.messenger.calls().contains(&Call::EditKeyboard {
                chat: ChatId(-100),
                message: MessageId(1),
                keyboard: limits::refresh_keyboard(),
            })
        })
        .await;
    }

    #[tokio::test]
    async fn limits_work_in_the_general_chat() {
        let world = world(FakeAgents::polling(vec![vec![claude_max(5)]]));
        world.send(text(None, 3, "/limits")).await;
        eventually("card", || has_card(&world, 1, "5%")).await;
        assert!(world.messenger.calls().iter().any(|call| matches!(
            call,
            Call::Send { target: Target::Reply { message: MessageId(3), .. }, message, .. }
                if message.text == texts::LIMITS_COLLECTING
        )));
    }

    #[tokio::test]
    async fn the_default_backend_is_listed_first() {
        let codex = (
            BackendKind::Codex,
            Ok(QuotaReport::unavailable(BackendKind::Codex, "вход по API-ключу — лимитов подписки нет")),
        );
        let agents = FakeAgents::polling(vec![vec![claude_max(1), codex]]);
        let world = world_from(agents, FakeMessenger::default(), BackendKind::Codex);
        world.send(text(Some(7), 1, "/limits")).await;
        eventually("card", || has_card(&world, 1, "<b>Claude</b>")).await;
        let (card, _) = edits(&world, 1).into_iter().find(|(_, format)| *format == Format::Html).unwrap();
        assert!(card.find("<b>Codex</b>") < card.find("<b>Claude</b>"), "{card}");
    }

    #[tokio::test]
    async fn a_failed_probe_falls_back_to_what_a_session_reported() {
        let update = QuotaUpdate { backend: BackendKind::Claude, plan: None, windows: five_hour(40) };
        let world = world(FakeAgents {
            scripts: StdMutex::new(vec![Script::Say(vec![AgentEvent::Quota(update), finished()])].into()),
            ..FakeAgents::polling(vec![vec![(BackendKind::Claude, Err("тайм-аут".to_owned()))]])
        });
        world.send(text(Some(7), 1, "сделай")).await;
        eventually("finished", || world.texts().iter().any(|t| t == "✅ Готово · ходов: 1")).await;
        world.send(text(Some(7), 2, "/limits")).await;
        let id = || world.texts().iter().position(|t| t == texts::LIMITS_COLLECTING).map(|i| i32::try_from(i).unwrap() + 1);
        eventually("cached card", || id().is_some_and(|id| has_card(&world, id, "40%"))).await;
        assert!(id().is_some_and(|id| has_card(&world, id, "(на ")));
    }

    #[tokio::test]
    async fn a_failure_without_a_cache_shows_its_reason() {
        let failed = (BackendKind::Claude, Err("Claude не ответил о лимитах за 15 с".to_owned()));
        let world = world(FakeAgents::polling(vec![vec![failed]]));
        world.send(text(Some(7), 1, "/limits")).await;
        eventually("card", || has_card(&world, 1, "⚠️ Claude не ответил о лимитах за 15 с")).await;
    }

    #[tokio::test]
    async fn refresh_redraws_the_same_message() {
        let world = world(FakeAgents::polling(vec![vec![claude_max(62)], vec![claude_max(70)]]));
        world.send(text(Some(7), 1, "/limits")).await;
        eventually("card", || has_card(&world, 1, "62%")).await;

        world.send(press("c1", 1, limits::REFRESH)).await;

        eventually("redrawn", || has_card(&world, 1, "70%")).await;
        assert!(edits(&world, 1).contains(&(texts::LIMITS_COLLECTING.to_owned(), Format::Plain)));
        eventually("answered", || {
            world.messenger.calls().contains(&Call::Answer { callback: "c1".to_owned(), text: None })
        })
        .await;
        assert_eq!(world.texts(), [texts::LIMITS_COLLECTING], "no new message");
    }

    #[tokio::test]
    async fn presses_and_commands_wait_while_a_poll_runs() {
        let (release, held) = oneshot::channel();
        let agents = FakeAgents::polling(vec![vec![claude_max(10)]]);
        *agents.hold_quotas.lock().unwrap() = Some(held);
        let world = world(agents);
        world.send(text(Some(7), 1, "/limits")).await;
        eventually("placeholder", || world.texts() == [texts::LIMITS_COLLECTING]).await;

        world.send(press("c2", 1, limits::REFRESH)).await;
        world.send(text(Some(7), 2, "/limits")).await;
        eventually("busy", || world.texts().contains(&texts::LIMITS_BUSY.to_owned())).await;
        eventually("answered", || {
            world.messenger.calls().contains(&Call::Answer { callback: "c2".to_owned(), text: None })
        })
        .await;
        assert!(edits(&world, 1).is_empty(), "a press during a poll changes nothing");

        release.send(()).unwrap();
        eventually("card", || has_card(&world, 1, "10%")).await;
        assert_eq!(world.agents.polls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_crashed_poll_frees_the_next_one() {
        let agents = FakeAgents::polling(vec![Vec::new(), vec![claude_max(33)]]);
        agents.crash_quotas.store(true, Ordering::SeqCst);
        let world = world(agents);
        world.send(text(Some(7), 1, "/limits")).await;
        eventually("internal error", || {
            edits(&world, 1).contains(&(texts::INTERNAL_ERROR.to_owned(), Format::Plain))
        })
        .await;
        world.send(text(Some(7), 2, "/limits")).await;
        eventually("second card", || has_card(&world, 2, "33%")).await;
    }

    #[tokio::test]
    async fn other_presses_still_reach_approvals_and_questions() {
        let world = world(FakeAgents::default());
        world.send(press("c3", 1, "ap:allow:unknown")).await;
        eventually("stale answer", || {
            world.messenger.calls().contains(&Call::Answer {
                callback: "c3".to_owned(),
                text: Some(texts::APPROVAL_STALE.to_owned()),
            })
        })
        .await;
    }
```

Идентификаторы сообщений `FakeMessenger` — по порядку отправки с 1 (`testing.rs:77`): в `a_failed_probe_falls_back…` до заглушки уходят ответы хода, поэтому её id ищется по позиции в `texts()`. Данные `ap:allow:unknown` разбираются `approvals::parse_callback_data` (`crates/hub-core/src/approvals.rs:64-71`, любой непустой id), но такого запроса нет — `press::handle` отвечает `APPROVAL_STALE` (`press.rs:27-29`).

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-telegram --lib -- hub::tests::limits hub::tests::refresh hub::tests::presses hub::tests::a_crashed_poll hub::tests::the_default_backend hub::tests::a_failed_probe hub::tests::a_failure_without hub::tests::other_presses`
Expected: FAIL — ошибки компиляции (`polls`, `hold_quotas`, `crash_quotas`, нет `HubMessage::Limits`) или, после их добавления, таймаут `eventually`: хаб отвечает `LIMITS_PENDING`.

- [ ] **Step 3: Реализация**

`crates/hub-telegram/src/hub.rs`, импорты:

```rust
use std::future::Future;

use chrono::Utc;
use hub_core::quota::{QuotaReport, QuotaUpdate};
use hub_core::render::render_limits;

use crate::limits::{self, Outcome, QuotaCache};
use crate::messenger::{Format, Messenger, Outgoing, Target};
```

(`use crate::messenger::{Messenger, Target};` заменяется последней строкой; `QuotaReport` уже импортирован задачей 11.)

`HubMessage`, перед `Shutdown`:

```rust
    /// A live session reported subscription limits.
    Quota(QuotaUpdate),
    /// A `/limits` poll finished.
    Limits(LimitsDone),
```

После `pub struct SnapshotDone(pub(crate) Done);`:

```rust
/// A finished `/limits` poll; only the hub makes and reads it.
pub struct LimitsDone(Polled);

struct Polled {
    chat: ChatId,
    /// The message to turn into the card; `None` when the placeholder was not delivered.
    message: Option<MessageId>,
    /// `None` when the poll crashed.
    outcomes: Option<Vec<Outcome>>,
}
```

Поля `Hub` после `reserved` (и в `spawn` — `quotas: QuotaCache::default(), polling: false,`):

```rust
    /// The last subscription limits live sessions reported, the card's fallback.
    quotas: QuotaCache,
    /// A `/limits` poll runs: each one starts up to two agent processes, so one at a time.
    polling: bool,
```

`handle`, ветка нажатия и новые ветки:

```rust
            HubMessage::Press(pressed) => {
                if pressed.data == limits::REFRESH {
                    self.refresh(pressed);
                } else {
                    tokio::spawn(press::handle(
                        Arc::clone(&self.registries),
                        self.sender.clone(),
                        pressed,
                    ));
                }
            }
            HubMessage::Quota(update) => self.quotas.apply(update, Utc::now()),
            HubMessage::Limits(LimitsDone(polled)) => self.limits_done(polled),
```

`outside_topic`: сигнатура `fn outside_topic(&mut self, inbound: Inbound)`; во внутреннем `match parse_input(...)` перед `Input::Command { .. } | Input::Text =>`:

```rust
                Input::Command { command: Command::Limits, .. } => self.limits(target, inbound.chat),
```

`command`: временную ветку задачи 4 заменить на `Command::Limits => self.limits(Target::Topic(key), chat),`.

Новые методы перед `say`:

```rust
    /// `/limits`: a placeholder now, the card once every found CLI answered or timed out.
    fn limits(&mut self, target: Target, chat: ChatId) {
        if self.polling {
            self.say(target, texts::LIMITS_BUSY.to_owned());
            return;
        }
        self.polling = true;
        let sender = self.sender.clone();
        self.poll(chat, async move {
            sender.one(target, Outgoing::plain(texts::LIMITS_COLLECTING.to_owned())).await
        });
    }

    /// «Обновить» redraws its own message; a press while a poll runs only stops the spinner.
    fn refresh(&mut self, pressed: Press) {
        let Press { callback, user: _, chat, message, html: _, data: _ } = pressed;
        let sender = self.sender.clone();
        tokio::spawn(async move { sender.answer(callback, None).await });
        let (Some(chat), Some(message), false) = (chat, message, self.polling) else {
            return;
        };
        self.polling = true;
        let sender = self.sender.clone();
        self.poll(chat, async move {
            // Dropping the keyboard here also stops further presses on this card.
            let text = texts::LIMITS_COLLECTING.to_owned();
            sender.edit(chat, message, text, Format::Plain).await;
            Some(message)
        });
    }

    /// Runs the poll outside the hub; whatever happens, the hub hears back exactly once, so
    /// `polling` never stays set.
    fn poll(&self, chat: ChatId, placeholder: impl Future<Output = Option<MessageId>> + Send + 'static) {
        let agents = Arc::clone(&self.agents);
        let mailbox = self.mailbox.clone();
        tokio::spawn(async move {
            let message = placeholder.await;
            let outcomes = match message {
                None => None,
                Some(_) => match tokio::spawn(async move { agents.quotas().await }).await {
                    Ok(outcomes) => Some(outcomes),
                    Err(error) => {
                        tracing::error!(panic = error.is_panic(), "limits poll crashed");
                        None
                    }
                },
            };
            // The hub is gone only during shutdown, when the card no longer matters.
            let _ = mailbox.send(HubMessage::Limits(LimitsDone(Polled { chat, message, outcomes }))).await;
        });
    }

    fn limits_done(&mut self, polled: Polled) {
        self.polling = false;
        let Polled { chat, message, outcomes } = polled;
        let Some(message) = message else { return };
        let sender = self.sender.clone();
        let Some(outcomes) = outcomes else {
            let text = texts::INTERNAL_ERROR.to_owned();
            tokio::spawn(async move { sender.edit(chat, message, text, Format::Plain).await });
            return;
        };
        let now = Utc::now();
        let reports: Vec<QuotaReport> =
            outcomes.into_iter().map(|outcome| self.quotas.settle(outcome, now)).collect();
        tracing::info!(backends = reports.len(), "limits card");
        let default = self.settings.borrow().default_backend;
        let card = render_limits(&limits::ordered(reports, default), now, &chrono::Local);
        tokio::spawn(async move {
            sender.edit(chat, message, card, Format::Html).await;
            sender.edit_keyboard(chat, message, limits::refresh_keyboard()).await;
        });
    }
```

`let (Some(chat), Some(message), false) = (…, self.polling)` — шаблон по кортежу внешних типов (`Option`, `bool`), не по собственному enum. Деструктуризация `Press` перечисляет все поля (`defensive.md`): новое поле `Press` не пройдёт мимо незамеченным.

`crates/hub-telegram/src/session.rs`, ветку `AgentEvent::Quota` задачи 2 заменить:

```rust
            AgentEvent::Quota(update) => {
                tracing::debug!(
                    backend = update.backend.name(),
                    windows = update.windows.len(),
                    "quota update"
                );
                // The hub is gone only during shutdown, when the cache no longer matters.
                let _ = mailbox.send(HubMessage::Quota(update)).await;
            }
```

`crates/hub-telegram/src/texts.rs`: удалить `LIMITS_PENDING` и комментарий над ним. `crates/hub-telegram/src/lib.rs`: `pub mod limits;` → `mod limits;` (тип `Outcome` в сигнатуре `HubAgents::quotas` — псевдоним, при проверке приватности он раскрывается в публичные типы).

`crates/hub-app/src/supervisor.rs:483-490`, в список вариантов ветки `"other"`:

```rust
                            | HubMessage::Quota(_)
                            | HubMessage::Limits(_)
```

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-telegram --lib -- hub && cargo test -p hub-app`
Expected: PASS — новые 9 тестов и прежние тесты хаба (нажатия подтверждений и вопросов по-прежнему идут в `press::handle`).

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS; `rg -n "LIMITS_PENDING|quota update not cached" crates` — пусто.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-telegram/src/hub.rs crates/hub-telegram/src/session.rs crates/hub-telegram/src/texts.rs crates/hub-telegram/src/lib.rs crates/hub-app/src/supervisor.rs
git commit -m "Хаб: /limits и кнопка «Обновить» с кэшем лимитов из живых сессий"
```

---
### Task 13: Живые тесты, README, версия 0.6.0

Два коммита: сначала живые тесты и документация, последним — версия.

**Files:**
- Modify: `crates/hub-claude/tests/live.rs` (новый тест после `live_cli_round_trip`, импорты `:11-23`)
- Modify: `crates/hub-codex/tests/live.rs` (новый тест после `codex_answers_a_prompt`, импорты `:6-22`)
- Modify: `README.md:147-160` (таблица команд), после `:297` (раздел «Лимиты подписок»), `:399-425` («Безопасность»), `:437-439` («Разработка»), `:475-482` («Ограничения»)
- Modify: `Cargo.toml:6`, `Cargo.lock` (версии крейтов рабочего пространства)

- [ ] **Step 1: Живые тесты (`#[ignore]`)**

`crates/hub-claude/tests/live.rs`, импорты `use hub_claude::usage;` и `use hub_core::quota::QuotaSource;`, тест:

```rust
    #[tokio::test]
    #[ignore = "talks to the real agent CLI"]
    async fn live_usage_probe_answers_without_a_turn() {
        if std::env::var_os("AGENT_HUB_LIVE_CLI").is_none() {
            return;
        }
        let cli = locate(None).unwrap();
        check(&cli).await.unwrap();
        let report = usage::probe(&cli).await.unwrap();
        assert_eq!(report.backend, BackendKind::Claude);
        assert!(
            matches!(report.source, QuotaSource::Fresh | QuotaSource::Unavailable(_)),
            "{report:?}"
        );
        assert!(report.windows.iter().all(|window| window.percent <= 100));
    }
```

`crates/hub-codex/tests/live.rs`, импорты `use hub_codex::usage;` и `use hub_core::quota::QuotaSource;`, тест:

```rust
    #[tokio::test]
    #[ignore = "needs a logged-in codex"]
    async fn live_usage_probe_reads_the_limits() {
        if std::env::var_os("AGENT_HUB_LIVE_CODEX").is_none() {
            return;
        }
        let cli = locate(None).unwrap();
        check(&cli).await.unwrap();
        let report = usage::probe(&cli).await.unwrap();
        assert_eq!(report.backend, BackendKind::Codex);
        assert!(
            matches!(report.source, QuotaSource::Fresh | QuotaSource::Unavailable(_)),
            "{report:?}"
        );
    }
```

Run: `cargo test -p hub-claude --test live && cargo test -p hub-codex --test live`
Expected: PASS (тесты помечены `ignored`, без переменных окружения не запускаются).

- [ ] **Step 2: README**

Таблица команд (`README.md:147-160`), после `/status`:

```markdown
| `/limits` | Лимиты подписок Claude и Codex: процент по окнам, время сброса, тариф; кнопка «🔄 Обновить». Работает и в общем чате группы |
```

После раздела «Что приходит в тему» (`:289-297`) — новый раздел:

~~~markdown
### Лимиты подписок

`/limits` в теме или в общем чате показывает, сколько израсходовано лимитов подписок:

```
📊 Лимиты подписок · 16.10 14:05
Claude (Max)
  5 часов     ▓▓▓▓▓▓░░░░ 62% · сброс в 17:30
  7 дней      ▓▓░░░░░░░░ 18% · сброс пн 09:00
  7 дней Opus ▓░░░░░░░░░  9%
Codex (ChatGPT Plus)
  основное  ▓▓▓▓░░░░░░ 41% · сброс в 16:10
  недельное ▓░░░░░░░░░ 12% · сброс чт 11:00
Qwen, Hermes — квоты подписки агент не сообщает
```

Данные спрашиваются у самих CLI при каждой команде и нажатии «🔄 Обновить»; фонового опроса нет. Claude — короткий запуск `claude` без сообщения (управляющий запрос `get_usage`, без ваших настроек, MCP-серверов и хуков), только для входа через подписку claude.ai; Codex — короткий `codex app-server` (`account/rateLimits/read`), только для входа через ChatGPT. Каждый агент отвечает не дольше 20 с; если он не ответил, показываются последние значения из живых сессий с пометкой «(на ЧЧ:ММ)». В карточке — только агенты, CLI которых найден; агент по умолчанию — первым; время — часовой пояс компьютера.
~~~

«Безопасность» (`:399-425`), пункт после «Снимки»:

```markdown
- **Лимиты подписок** хаб узнаёт только у самих CLI: токены агентов не читаются и не хранятся, внешние сервисы напрямую не вызываются. Проба Claude не отправляет сообщений и не загружает ваши настройки, MCP-серверы и хуки; проба Codex не входит по ключу из настроек и не открывает сессий. Ответы ограничены по размеру и экранируются; в лог — только бэкенд, вид ошибки и проценты.
```

«Разработка» (`:437-439`), после строки о живом тесте Claude:

```markdown
Пробы лимитов: `AGENT_HUB_LIVE_CLI=1 cargo test -p hub-claude --test live -- --ignored live_usage` и `AGENT_HUB_LIVE_CODEX=1 cargo test -p hub-codex --test live -- --ignored live_usage`.
```

«Ограничения» (`:475-482`):

```markdown
- `/limits`: запрос лимитов Claude (`get_usage`) помечен в Claude Agent SDK как экспериментальный; при непонятном ответе карточка показывает последние значения из живых сессий. Лимиты Qwen и Hermes их протоколы не сообщают.
```

Если задача 1 выбрала `Handshake::CacheOnly`, абзац о Claude в «Лимиты подписок» заменить: «Claude — последние значения из живых сессий (`rate_limit_event`): CLI не отвечает о лимитах без сообщения».

- [ ] **Step 3: Гейты и коммит документации**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

```bash
git add crates/hub-claude/tests/live.rs crates/hub-codex/tests/live.rs README.md
git commit -m "Лимиты подписок: живые тесты и документация"
```

- [ ] **Step 4: Версия 0.6.0 (последний коммит)**

`Cargo.toml:6`: `version = "0.6.0"`.

Run: `cargo check --workspace --all-targets && git diff --stat Cargo.lock && cargo test --workspace --locked`
Expected: в `Cargo.lock` меняются только строки `version = "0.5.0"` → `"0.6.0"` восьми крейтов `hub-*` (как в `cd57056`); тесты PASS.

```bash
git add Cargo.toml Cargo.lock
git commit -m "Версия 0.6.0: лимиты подписок"
```

---

### Task 14: Ручная проверка (человек)

> **Выполняет человек** с ботом, группой и установленными `claude` (вход через claude.ai) и `codex` (вход через ChatGPT). Кода нет. Нашлась ошибка — исправление отдельным коммитом с тестом, воспроизводящим её, поверх коммита версии (история не переписывается), затем повтор шага; тег задачи 15 ставится на последний коммит.

- [ ] **Step 1: Живые тесты**

Run (Linux/macOS): `AGENT_HUB_LIVE_CLI=1 cargo test -p hub-claude --test live -- --ignored live_usage && AGENT_HUB_LIVE_CODEX=1 cargo test -p hub-codex --test live -- --ignored live_usage`
(Windows PowerShell — `$env:AGENT_HUB_LIVE_CLI=1; …`.)
Expected: 2 PASS. После прогона `find ~/.claude/projects -mmin -5 -name '*.jsonl'` — пусто (проба не пишет сессий).

- [ ] **Step 2: Через бота**

1. Тема → `/limits` → «⏳ Собираю…», затем то же сообщение — карточка: Claude (тариф) и Codex (тариф) с полосами, «Qwen, Hermes — квоты подписки агент не сообщает» (если их CLI найдены); бэкенд по умолчанию — первым.
2. Общий чат группы (не тема) → `/limits` → ответ на сообщение, та же карточка.
3. «🔄 Обновить» → сообщение превращается в «⏳ Собираю…» и обратно в карточку с новым временем в заголовке; новых сообщений нет.
4. Дважды быстро «🔄 Обновить» и `/limits` во время опроса → второе нажатие ничего не меняет, `/limits` отвечает «⏳ Уже собираю лимиты — подождите.».
5. В «Настройках» указать для Claude несуществующий путь → `/limits` → «Claude ⚠️ не удалось запустить Claude: …» (кэша ещё нет); вернуть путь.
6. Тема с Claude: задача → ответ; затем в «Настройках» сломать путь Claude → `/limits` → блок Claude с «(на ЧЧ:ММ)», если CLI в ходе прислал `rate_limit_event` с `utilization` (иначе «⚠️ …» — ожидаемо, расхождение 2); то же для Codex (`account/rateLimits/updated` приходит в ходе).
7. Codex со входом по API-ключу (`codex logout`, ключ в «Настройках») → «Codex ⚠️ вход по API-ключу — лимитов подписки нет»; `~/.codex/auth.json` не изменился (`stat` до и после).
8. Убрать `qwen` из `PATH` (или переименовать) → строки Qwen в карточке нет.
9. Логи: `rg -n "quota probe failed|limits card" ~/.local/share/agent-hub/logs` — только `backend=…`, `error=<вид>`, число бэкендов; `rg -n "rate_limits|utilization|five_hour" ~/.local/share/agent-hub/logs` — пусто.
10. В пробе Claude не стартовали MCP-серверы и хуки: при настроенном в `~/.claude/settings.json` хуке `SessionStart`, пишущем в файл, файл после `/limits` не изменился.

- [ ] **Step 3: Windows**

Шаги 1–3, 5, 7 на Windows: окно консоли при пробе не мигает (`hide_window`), время — местное.

- [ ] **Step 4: Регрессия**

Обычные задачи, `/new`, `/cwd`, `/backend`, `/reset`, `/stop`, `/save`, `/restore` во всех четырёх бэкендах работают как в 0.5.0; кнопки подтверждений и вопросов работают; меню бота показывает `limits` после `status`.

---

### Task 15: Релиз `v0.6.0`

> **Push тега и веток — только после отдельного явного подтверждения пользователя.** Агент, выполняющий план, останавливается перед шагом 3 и спрашивает.

- [ ] **Step 1: Проверка перед тегом**

Run: `git status --short && cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: чистое дерево, PASS; последний коммит — «Версия 0.6.0: лимиты подписок» или исправление из задачи 14 после него; ветка влита в `main` способом, который выбрал пользователь (план его не предписывает).

- [ ] **Step 2: Аннотированный тег (локально)**

```bash
git tag -a v0.6.0 -m "agent-hub 0.6.0: лимиты подписок"
git show --stat v0.6.0 | head -5
```

Expected: тег на коммите версии, сообщение в стиле `v0.5.0`.

- [ ] **Step 3: Push (только после подтверждения пользователя)**

```bash
git push origin main
git push origin v0.6.0
```

Expected: `release.yml` собирает Windows, macOS и Linux. Без подтверждения — остановиться после шага 2 и сообщить, что тег создан локально.

---

## Покрытие security-review и reliability-review

| Риск | Где закрыт |
|---|---|
| Чтение или утечка токенов агентов | Задачи 6, 8: только запуск самих CLI; ни одного чтения `~/.claude`, `~/.codex/auth.json`, связки ключей; недокументированный `api.anthropic.com/api/oauth/usage` не вызывается; README (задача 13) |
| Побочные эффекты пробы Claude (ход, транскрипт, MCP-серверы, хуки) | Задача 6: без пользовательского сообщения и `--resume`, `--setting-sources=`, `--strict-mcp-config`, `cwd` — временный каталог; тесты `direct_sends_only_get_usage_and_never_a_prompt`, `the_probe_loads_no_settings_and_no_mcp_servers`; задача 1, шаг 4; задача 14, шаги 1 и 2.10 |
| Побочные эффекты пробы Codex (вход по ключу, переписанный `auth.json`, поток) | Задача 8: `read` — только `initialize`, `initialized`, `account/read`, `account/rateLimits/read`; тесты сверяют список методов; задача 14, шаг 2.7 |
| Недоверенный вывод агента: размер | Задача 6: строка ≤ 1 МиБ (`LinesCodec::new_with_max_length`), ≤ 64 строк, тайм-аут 15 с; задача 8: `hub_agent::rpc` — 64 МиБ на строку и тайм-аут запроса (как у живых сессий) плюс 20 с внешнего тайм-аута; окна ≤ 8 (задача 2), кэш ≤ 4 × 8 (задача 10) |
| Недоверенный вывод агента: разметка и управляющие символы | Задача 2: `clean_label` (≤ 32 символов, без управляющих); задача 10: `reason_text` (≤ 200); задача 3: `escape` всех строк, тест `agent_strings_are_escaped` |
| Секреты и пути в логах | Задачи 6, 8, 11, 12: в лог — `backend`, `ProbeError::kind`, число бэкендов и окон; stderr пробы Claude — `Stdio::null()`; stderr Codex — прежний `log_stderr` app-server (как в `probe_auth`); задача 14, шаг 2.9 |
| Зависшая проба | Задача 6: 15 с на ответ, закрытие stdin, ≤ 5 с ожидания, `kill`, `kill_on_drop`; задача 8: `AppServer::stop` (≤ 5 с, `kill`), `kill_on_drop`; задача 11: внешний тайм-аут 20 с, по истечении будущее пробы сбрасывается — процесс убивается |
| Сбой одного бэкенда ломает карточку | Задача 11: пробы параллельны (`join_all`), у каждой свой тайм-аут и свой `Result`; тест `quotas_probe_every_configured_cli_and_isolate_failures` |
| Неизвестная форма экспериментального `get_usage` | Задача 5: разбор терпим к лишним/отсутствующим полям и `null`, иной тип → `ProbeError::Shape` → кэш (задача 10), а не ошибка карточки |
| Шторм проб от повторных команд и нажатий | Задача 12: один опрос за раз (`polling`), `LIMITS_BUSY`, повторное «Обновить» игнорируется; тест `presses_and_commands_wait_while_a_poll_runs` |
| Залипший «опрос идёт» после паники | Задача 12: внутренний `tokio::spawn` для `quotas`, внешняя задача всегда шлёт `HubMessage::Limits`; тест `a_crashed_poll_frees_the_next_one` |
| Устаревшие данные выдаются за свежие | Задачи 3, 10: кэш показывается только как `QuotaSource::Cached { at }` с «(на ЧЧ:ММ)»; окончательные ответы («вход по API-ключу…») кэшем не подменяются |
| Разреженные обновления Codex стирают данные | Задача 10: `apply` заменяет только названные окна, `plan: None` не стирает тариф; тест `rolling_updates_merge_by_window_and_keep_the_plan` |
| Совместимость | Новые варианты `AgentEvent`, `HubMessage`, `Incoming`, метод `Agents::quotas` — внутри рабочего пространства, все исчерпывающие `match` обновлены (задачи 2, 5, 12); формат файлов на диске не меняется |
| Стоимость | Пробы только по команде/кнопке; Claude — без хода (токены не тратятся), `skip_behaviors: true` (без сканирования транскриптов), Codex — `excludeResetCreditDetails: true` |

Остаточные риски (приняты): `get_usage` экспериментальный и может исчезнуть — тогда Claude в карточке только из кэша (README, «Ограничения»); кэш Claude может оставаться пустым, если CLI шлёт `rate_limit_event` только со `status` (расхождение 2); 64 МиБ на строку у пробы Codex — общий предел `hub_agent::rpc` для того же доверенного локального процесса, что и живые сессии; если Telegram отклонит HTML карточки, `Sender::edit` только пишет предупреждение и заглушка остаётся («⏳ Собираю…») — экранирование и тесты задачи 3 делают это маловероятным; при одинаковом тексте карточки Telegram отвечает «message is not modified» — в логе предупреждение, клавиатура остаётся.

## Самопроверка плана

- Имена сквозь задачи: `WindowKind`, `UsageWindow`, `QuotaSource`, `QuotaReport::{fresh, not_reported, unavailable}`, `QuotaUpdate`, `clean_label`, `percent`, `LABEL_LIMIT`, `WINDOW_LIMIT`, `AgentEvent::Quota` (2) → `render_limits`, `LIMITS_TITLE` (3) → `Command::Limits`, `LIMITS_COLLECTING`, `LIMITS_REFRESH`, `LIMITS_BUSY`, временный `LIMITS_PENDING` (4, удалён в 12) → `wire::{Incoming::RateLimitEvent, RateLimitInfo}`, `usage::{parse_reply, event_update, Scale, EVENT_SCALE, ProbeError, NO_SUBSCRIPTION}` (5) → `usage::{Handshake, HANDSHAKE, PROBE_ARGS, ANSWER_TIMEOUT, requests, exchange, probe}` (6) → сессия Claude (7) → `hub_codex::usage::{parse_snapshot, update_from, read, probe, ProbeError, API_KEY_LOGIN, NOT_LOGGED_IN}`, `Call::RateLimitsRead`, `pub(crate) AppServer`/`handshake` (8) → `TurnTracker` (9) → `limits::{REFRESH, Outcome, QuotaCache::{apply, settle}, ordered, refresh_keyboard}` (10) → `Agents::quotas`, `QUOTA_TIMEOUT`, `quotas`, `failed` (11) → `HubMessage::{Quota, Limits}`, `LimitsDone`, `Polled`, `Hub::{limits, refresh, poll, limits_done}` (12).
- Формы данных: `get_usage` — запрос `{"type":"control_request","request_id":"hub-usage","request":{"subtype":"get_usage","skip_behaviors":true}}`, ответ разбирается из `control_response.response` (`subtype`, `request_id`, `response`/`error`) — одинаково в `parse_reply` (5), `exchange`/`usage_reply` (6), фикстуре и тестах. `rate_limit_event` — `rate_limit_info.{rateLimitType, utilization, resetsAt}` в `RateLimitInfo` (5), тестах задач 5 и 7. Codex — `rateLimits.{primary, secondary}.{usedPercent, resetsAt}`, `rateLimits.planType` в `parse_snapshot` (8), фикстуре, тестах задач 8–9.
- Подписи окон одинаковы в разборе (5, 8), отрисовке (3), тестах хаба (12) и README (13): «5 часов», «7 дней», «7 дней <модель>», «сверх подписки», «основное», «недельное».
- Переключатели живой проверки — только `HANDSHAKE`, `PROBE_ARGS` (задача 6) и `EVENT_SCALE` (задача 5); тесты задач 6, 7, 11 проходят при любом их значении (`a_missing_cli_is_a_spawn_error` и тест задачи 11 принимают `Disabled`, тест задачи 7 берёт масштаб из `EVENT_SCALE`).
- Временные ветки: `relay` для `AgentEvent::Quota` (2) и заглушка `FakeAgents::quotas` (11) заменяются в задаче 12; `pub mod limits` (10) сужается в задаче 12.
- `Cargo.lock`: новые рёбра только к `chrono` (задачи 2, 5, 8), новых пакетов нет; версия — отдельным последним коммитом (13, шаг 4).
- Windows: тесты не создают исполняемых файлов и не используют `/`-пути (поддельные CLI — `duplex`/`pair`, отсутствующий CLI — `temp_dir().join(…)`); `cfg`-зависимых импортов план не добавляет.
- Коммиты — по-русски, без строк атрибуции; push — только в задаче 15 после подтверждения пользователя.
