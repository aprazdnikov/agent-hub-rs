# Лимиты подписок

## Цель

Одной командой посмотреть в Telegram, сколько израсходовано лимитов подписок агентов: процент по каждому окну (5 часов, неделя, неделя по модели), время сброса и тариф. Работает для Claude (подписка claude.ai) и Codex (вход через ChatGPT); Qwen и Hermes квот подписки по протоколу не сообщают.

## Решения

| Вопрос | Решение |
|---|---|
| Где показывать | Сообщение в теме или общем чате группы по команде `/limits`; кнопка «🔄 Обновить» перерисовывает то же сообщение. Mini App — не сейчас |
| Источник Claude | Короткий процесс `claude` в stream-json без хода: управляющий запрос `get_usage` (`skip_behaviors: true`). Запасной вариант — последние `rate_limit_event` из обычных ходов с пометкой «на ЧЧ:ММ» |
| Источник Codex | Короткий `codex app-server`: `initialize` → `account/rateLimits/read` (`excludeResetCreditDetails: true`). Запасной вариант — последние `account/rateLimits/updated` из живых сессий |
| Qwen, Hermes | Строка «квоты подписки агент не сообщает» |
| Токены агентов | Хаб их не читает и не хранит; недокументированный `api.anthropic.com/api/oauth/usage` не вызывается — только через сами CLI |
| Опрос | Только по команде и кнопке; фонового опроса нет |
| Время | Часовой пояс компьютера |
| Изоляция пробы Claude | Запуск с `--setting-sources=` и `--strict-mcp-config` во временном каталоге: MCP-серверы и хуки пользователя не стартуют |
| Проба Codex | Только `initialize` → `account/read` → `account/rateLimits/read`; вход по ключу из настроек не выполняется (`auth.json` не переписывается) |
| Одновременность | Один опрос на весь хаб: `/limits` во время опроса — «опрос уже идёт», «Обновить» — без изменений |
| Окончательные ответы | «вход не через подписку» (Claude `rate_limits_available: false`, Codex не ChatGPT) кэшем не заменяются; кэш — только при ошибке или тайм-ауте пробы |

## Протокол

### Claude Code

- `rate_limit_event` (`SDKRateLimitEvent`, есть с CLI 2.1.63): одно окно на событие — `status`, необязательные `resetsAt`, `rateLimitType` (`five_hour`, `seven_day`, `seven_day_opus`, `seven_day_sonnet`, `overage`, …), `utilization`. Единица `utilization` (0–1 или 0–100) не документирована — уточняется живой проверкой.
- `get_usage` (CLI ≥ 2.1.169): `control_request {subtype: "get_usage", skip_behaviors: true}` → `control_response` с `rate_limits`: окна `five_hour`, `seven_day`, `seven_day_opus`, `seven_day_sonnet`, `seven_day_oauth_apps`, `model_scoped[]`, `extra_usage` — каждое с `utilization` 0–100 и ISO `resets_at`; `subscription_type`; `rate_limits_available: false` при входе по API-ключу, Bedrock/Vertex или без нужной области токена.
- Записанные транскрипты (CLI 2.1.28x) содержат `rate_limit_event` только со `status` — без `utilization`: кэш Claude из живых сессий может оставаться пустым; тогда при сбое пробы карточка показывает «⚠️ <причина>». У `extra_usage` нет `resets_at`.
- Запрос помечен в SDK как экспериментальный («may change or be removed»): разбор терпим к лишним и отсутствующим полям; неизвестная форма — переход на кэш, а не ошибка всей карточки.
- Не проверено: отвечает ли `claude -p --input-format stream-json --output-format stream-json` на `get_usage`, не получив ни одного пользовательского сообщения. Проверяется первой задачей плана (живой прогон); если нет — проба отправляет минимальный `initialize` и только потом `get_usage`; если и так нет — только кэш.

### Codex

- `account/rateLimits/read {excludeResetCreditDetails: true}` (есть с rust-v0.48.0; хаб требует ≥ 0.160.0): `rateLimits {primary, secondary: {usedPercent: i32, windowDurationMins, resetsAt (Unix с)}, planType, credits {hasCredits, unlimited, balance}}`, `rateLimitsByLimitId`.
- `account/rateLimits/updated` — разреженное обновление: значения сливаются с последним известным состоянием.
- Без входа через ChatGPT лимитов подписки нет — строка «вход по API-ключу — лимитов подписки нет».

## Карточка

```
📊 Лимиты подписок · 16.10 14:05
Claude (Max)
  5 часов      ▓▓▓▓▓▓░░░░ 62% · сброс в 17:30
  7 дней       ▓▓░░░░░░░░ 18% · сброс пн 09:00
  7 дней Opus  ▓░░░░░░░░░  9%
Codex (ChatGPT Plus)
  основное     ▓▓▓▓░░░░░░ 41% · сброс в 16:10
  недельное    ▓░░░░░░░░░ 12% · сброс пт 11:00
Qwen, Hermes — квоты подписки агент не сообщает
```

- HTML, все значения из ответов агентов экранируются.
- Кэшированные данные — «(на 14:02)» у бэкенда; недоступные — «⚠️ <причина>».
- Сброс сегодня — «в ЧЧ:ММ», в пределах недели — «<день> ЧЧ:ММ», дальше — «ДД.ММ ЧЧ:ММ».
- Пока идёт опрос — «⏳ Собираю…», затем сообщение редактируется в карточку.
- Показываются только бэкенды, CLI которых найден; бэкенд по умолчанию — первым.

## Архитектура

### `hub-core`

- `WindowKind { FiveHour, Week, WeekModel(String), Extra, Other(String) }`, `UsageWindow { kind, percent: u8, resets_at: Option<DateTime<Utc>> }`.
- `QuotaReport { backend: BackendKind, plan: Option<String>, windows: Vec<UsageWindow>, source: QuotaSource }`, `QuotaSource { Fresh, Cached { at: DateTime<Utc> }, NotReported, Unavailable(String) }`.
- `render_limits(&[QuotaReport], now: DateTime<Utc>, tz) -> String` — чистая функция; полоса из 10 делений; проценты ограничиваются 0–100.
- `Command::Limits`, тексты.

### `hub-claude`

- `usage::probe(cli) -> Result<QuotaReport, ProbeError>`: запуск, `get_usage`, разбор, закрытие stdin, ожидание ≤ 5 с, `kill`.
- В сессии `rate_limit_event` разбирается в `UsageWindow` и отдаётся хабу событием (сейчас — `Incoming::Other`).

### `hub-codex`

- `usage::probe(cli) -> Result<QuotaReport, ProbeError>`.
- В сессии `account/rateLimits/updated` отдаётся хабу событием.

### `hub-telegram`

- Кэш квот по бэкендам: последнее состояние и время; пополняется событиями сессий.
- `/limits`: «⏳ Собираю…» → параллельные пробы найденных CLI с тайм-аутом 20 с каждая → при ошибке или тайм-ауте — кэш → редактирование сообщения в карточку + кнопка «🔄 Обновить» (callback перерисовывает то же сообщение).
- Команда доступна в теме и в общем чате группы; повторное нажатие «Обновить», пока опрос идёт, игнорируется.

## Безопасность

- Токены агентов не читаются, не хранятся и не передаются; только вызовы самих CLI.
- В лог — только бэкенд, вид ошибки и проценты; без путей и тел ответов.
- Ответы агентов — недоверенные данные: размер ответа пробы ограничен, строки (тариф, имена окон) экранируются и обрезаются.

## Надёжность

- У проб тайм-аут, `kill_on_drop`, штатное закрытие stdin; ошибка одного бэкенда не мешает остальным.
- Пробы не трогают сессии тем и не создают ходов (Claude — без пользовательского сообщения; Codex — без `thread/start`).
- Устаревшие данные подписаны временем.

## Тесты

- `hub-core`: `render_limits` таблицей (окна, кэш, недоступно, нет данных, границы 0/100, форматы сброса), экранирование.
- `hub-claude`: разбор `get_usage` и `rate_limit_event` на записанных фикстурах (в т. ч. лишние/отсутствующие поля, `rate_limits_available: false`); проба на поддельном CLI — успех, неизвестная форма, тайм-аут.
- `hub-codex`: разбор `account/rateLimits/read` и слияние `updated`; проба на поддельном app-server.
- `hub-telegram`: `/limits` — свежие данные, переход на кэш при ошибке и тайм-ауте, «Обновить» редактирует то же сообщение, повторное нажатие во время опроса.
- Живые тесты Claude и Codex (`#[ignore]`).

## Вне объёма

Telegram Mini App (позже — через личный чат с ботом и статическую страницу); Rich Messages Bot API 10.x (нет в teloxide 0.17); квоты Qwen и Hermes; уведомления о приближении к лимиту; фоновый опрос; показ в GUI.
