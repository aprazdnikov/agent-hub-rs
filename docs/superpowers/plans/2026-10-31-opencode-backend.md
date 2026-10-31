# План: бэкенд OpenCode

Спецификация: [2026-10-31-opencode-backend-design.md](../specs/2026-10-31-opencode-backend-design.md).

1. **Контракт** — живые пробы `opencode acp` (1.19.5): `initialize`, `session/new` (configOptions, modes build/plan), `session/load` (replay до ответа), `session/set_model` (`провайдер/модель`), `session/prompt` (`stopReason`, `usage.totalTokens`), форма `session/request_permission` (once/always/reject) по встроенному коду SDK.
2. **hub-core** — `BackendKind::Opencode`, `Usage::Opencode`, `OpencodeSettings/Draft/File/Field`, секция `[opencode]`, рендер и тема; тесты round-trip и парсинга.
3. **hub-opencode** — крейт по образцу `hub-hermes`: `backend`, `protocol`, `requests`, `version`; без `set_mode` и без archive; тесты: in-memory (`testing.rs`), процессные фикстуры (`tests/acp*`), живой opt-in (`tests/live.rs`).
4. **hub-app** — `AgentError::Opencode`, проверка CLI в `connector`, `AgentAuth::Opencode`, секция настроек GUI, заголовки статуса; тесты.
5. **hub-telegram** — ветка запуска в `HubAgents`, отказ снимков/переноса с ясным сообщением, `not_reported` в квотах, списки бэкендов в текстах; тесты.
6. **Документация** — README (требования, команда, раздел OpenCode), сайт, эта спецификация.
7. **Версия и релиз** — workspace 0.7.0; gates (fmt/clippy/test/build/diff-check); ветка `feat/opencode`, тег `v0.7.0` — отдельный релиз, `main` не меняется.
