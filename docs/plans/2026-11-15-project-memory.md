# Переносимая память проекта — план реализации

Этот документ хранит завершённую реализацию и результаты проверок. Новая работа
ведётся по [плану продолжения](../roadmap/2026-11-15-company-brain-roadmap.md).

> **For agentic workers:** Use superpowers:executing-plans or subagent-driven-development; independent storage, protocol and adapter tasks may run concurrently against the interfaces below.

**Goal:** Общая локальная память для всех пяти агентов через MCP без LLM-вызовов в сервисе.

**Architecture:** Новый `hub-memory` содержит хранилище и stdio MCP. Существующий бинарник получает отдельный CLI-режим; адаптеры подключают его к обычным сессиям.

**Tech Stack:** Rust, serde/serde_json, файловое хранилище, OS locks, существующие зависимости workspace.

**Spec:** [Дизайн](../specs/2026-11-15-project-memory-design.md).

## Global Constraints

- Сохранять существующие настройки, инструменты и формат сессий.
- Лимиты, формат и ограничения из дизайна обязательны для всех этапов.
- Без LLM-вызовов, изменения пользовательских конфигураций CLI, коммитов или push.
- Исследовательская заметка предыдущего этапа сохраняется.

## Review Focus

- Одновременные записи разных процессов: без потери изменений, явный конфликт.
- Повтор запроса после потери ответа: без новой ревизии.
- Повреждение/переполнение/симлинки: отказ без перезаписи данных.
- Восстановленная сессия и /handoff: правильный набор MCP, без записи из служебного хода.
- Запуск MCP без дисплея и с уже работающим GUI: только JSON-RPC в stdout.

## Task 1: Хранилище

**Files:** `crates/hub-memory/{Cargo.toml,src/lib.rs,src/model.rs,src/store.rs,tests/storage.rs,tests/storage/}`; корневой `Cargo.toml`.

**Interfaces:** `Store::open(base: &Path, project: &Path, actor: &str) -> Result<Store, Error>`; `write(WriteRequest) -> Result<Entry, Error>`; `get(&str, Option<u32>) -> Result<Entry, Error>`; `search(&str, usize) -> Result<Vec<SearchHit>, Error>`.

- [x] Написать и выполнить падающие тесты сохранения/повторного открытия, конфликтов, повторов, истории и изоляции.
- [x] Реализовать валидацию, ограниченный формат, межпроцессную блокировку и атомарную запись.
- [x] Проверить повреждённые/слишком большие файлы, неизвестную версию, симлинки и права.
- [x] `cargo test -p hub-memory --test storage` — PASS.

## Task 2: MCP и CLI

**Files:** `crates/hub-memory/src/{protocol,stdio,cli}.rs`, `tests/protocol.rs`, `tests/protocol/`; `crates/hub-app/{Cargo.toml,src/main.rs}`.

**Interfaces:** `hub_memory::run_cli(Vec<OsString>) -> Result<(), String>` принимает аргументы после `--memory-mcp`; `serve` работает с BufRead/Write и Store.

- [x] Проверить RED для initialize/list/search/write/get и некорректного JSON-RPC.
- [x] Реализовать три инструмента, инструкции памяти, ограниченное чтение строк, ошибки и CLI.
- [x] Подключить CLI-режим до GUI/lock/logging; проверить subprocess без DISPLAY.
- [x] `cargo test -p hub-memory --test protocol` — PASS; самостоятельный MCP smoke — PASS.

## Task 3: Три адаптера

**Files:** `hub-agent/src/memory.rs`; `hub-codex`, `hub-qwen`, `hub-hermes` protocol/backend/tests; `hub-telegram` agents/hub/handoff_summary; `hub-app/src/connector.rs`.

**Interfaces:** `MemoryMcp` описывает executable, project-dir, actor; app задаёт executable, адаптеры сериализуют descriptor без shell.

- [x] Проверить RED для new/resume payloads каждого адаптера и исключения памяти из summary.
- [x] Добавить descriptor и подключение без замены пользовательских серверов.
- [x] Проверить start/resume Codex configuration по установленной схеме, ACP stdio по текущей реализации.
- [x] Целевые тесты трёх backend crates, hub-agent, hub-telegram — PASS.

## Task 4: Документация и итоговая проверка

- [x] Руководство: хранение, инструменты, самостоятельное подключение, ограничения и пилот с доступными агентами.
- [x] Независимое ревью целостности, границ доступа и интеграции; устранить существенные замечания.
- [x] `cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo fmt --all -- --check`, `cargo deny check`, `cargo build -p hub-app --locked`, `git diff --check`.
- [x] Отметить проверенное поведение и открытый живой сценарий без объявления его успешным.

## Журнал

- Начата реализация согласованного направления. План выполняется в текущем workspace; работа разделена по независимым файлам. Коммиты не создаются.
- Решение реализации: расположение хранилища не запрещается внутри каталога проекта — проектом может быть домашний каталог пользователя. По умолчанию память находится в `~/.agent-hub/memory`; при расположении внутри репозитория её нужно исключить из публикации.
- Реальный startup-only probe установленного Codex выявил, что `enabled=false` без транспорта отвергается. Отключение передаёт полное валидное описание stdio с `enabled=false`; модельные ходы не выполнялись.

## Результат проверки, 2026-11-15

- `cargo test --workspace --locked` — **1860 passed, 0 failed, 15 ignored**. Из пропущенных два новых startup-only теста Codex выполнены отдельно; остальные живые интеграции и GUI fixture не запускались.
- `cargo test -p hub-codex --test startup --locked -- --ignored` — **2 passed** на установленном Codex 0.166.0, временный CODEX_HOME, без модельных ходов.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — PASS.
- `cargo fmt --all -- --check`, `git diff --check` — PASS.
- `cargo deny check` — PASS после точечного обновления rustix 1.1.5 → 1.1.6 (RUSTSEC-2026-0486). Минимальная версия прямой зависимости также 1.1.6.
- `cargo build -p hub-app --locked` — PASS на Linux.
- `cargo check -p hub-memory --all-targets --locked --target x86_64-pc-windows-msvc` и `--target aarch64-apple-darwin` — PASS; это проверки компиляции, не запуска.
- Три subprocess-проверки приложения: MCP без GUI/настроек; чтение записи из другого процесса; конкурирующие процессы и идемпотентный повтор — PASS.
- 22 проверки storage и 14 protocol покрывают историю, источники, конфликты, лимиты, повреждение, симлинки, занятость и ограниченный ввод.
- Независимое ревью: существенных нерешённых замечаний нет. Исправлена гонка чтения старого атомарно заменённого snapshot: открытый файл может иметь nlink=0 и остаётся корректным для чтения. Проверены исходники установленных Qwen/Hermes: session/load использует заново переданный список MCP.

Решения по итогам проверки: запуск чтения не захватывает блокировку писателя;
существующий базовый каталог не меняет права; после сбоя остаётся не более одного
staging-файла; канонизация допускает системные symlink-предки, но не ссылки на
управляемые файлы/каталоги памяти.

Открытые проверки: реальный пользовательский пилот между моделями Codex/Qwen/Hermes
через Telegram и самостоятельные CLI; запуск бинарника на Windows/macOS. Качество
выбора записей моделью не следует из transport-тестов. Windows использует стандартные
проверки путей, без Unix fd-relative механизма; изоляция от программ того же пользователя
не заявляется. Глобальные MCP и нативные инструменты CLI не блокируются хабом при /handoff.

На этом этапе функция была реализована локально; коммиты, push и публикация
ещё не выполнялись. Дальнейшие действия записаны ниже.

## Расширение на все существующие агенты

По уточнению пользователя наличие подписок не ограничивает поддержку: память должна
работать также в Claude Code и OpenCode. Формат хранилища и инструменты сохраняются.

- [x] Claude Code: добавить stdio server рядом с SDK server в --mcp-config; проверить новые и восстановленные сессии, отсутствие памяти в служебном ходе.
- [x] OpenCode: добавить stdio server рядом с HTTP tools в ACP new/load; проверить сохранение текущих инструментов и параметров модели.
- [x] Общий dispatcher: подключать память для всех BackendKind, оставить /handoff без встроенной памяти. Регрессия до исправления: новые случаи Claude/OpenCode падают, остальные три проходят; после исправления 5/5 проходят.
- [x] Обновить руководство и выполнить общий прогон, Clippy, форматирование, сборку и независимое ревью расширения.

Проверки расширения:

- Claude: два теста состава argv и четыре subprocess-теста фактического запуска — new/resume × память включена/отсутствует. CLI help и stdio-схема сверены с установленным Claude Code 2.1.296; авторизация и модели не вызывались.
- OpenCode: четыре subprocess-теста new/load × память включена/отсутствует. На реальном OpenCode 1.19.5 с временными конфигурациями и fake MCP проверено: load в новом процессе без descriptor не восстанавливает старый сервер; повторный load с descriptor подключает его снова. Без модельного хода.
- Независимое ревью расширения не выявило существенных нерешённых замечаний. Статический анализ установленного Claude подтверждает загрузку MCP из текущих параметров перед resume; реальный resumed-диалог Claude с моделью не запускался.
- Финальные `cargo test --workspace --locked`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo build -p hub-app --locked`, `cargo fmt --all -- --check` и `git diff --check` — PASS на Linux.
- Итог расширенного workspace: **1872 passed, 0 failed, 15 ignored**.
- Руководство описывает встроенную и самостоятельную настройку для всех пяти агентов. Пилот с настоящими моделями остаётся отдельной пользовательской проверкой.

## Подготовка ветки 1.0.0, 2026-11-15

Пользователь выбрал `release/1.0.0` для будущего выпуска и поручил поднять версию,
закоммитить и отправить все изменения. Слияние в `main`, тег и публикация релиза
не входят в это поручение.

- `Cargo.toml` и записи всех 10 пакетов workspace в `Cargo.lock` обновлены до `1.0.0`.
  Версии сверены с `cargo metadata --locked --offline --format-version 1 --no-deps`.
  Обновление версии не изменило сторонние зависимости относительно состояния до bump.
- `cargo test --workspace --locked` — **1872 passed, 0 failed, 15 ignored** на Linux.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — PASS.
- `cargo build -p hub-app --bin agent-hub --locked` — PASS.
- `cargo fmt --all --check` и `cargo deny check` — PASS.
- `node --test scripts/test-macos-authorization.mjs` — **29 passed, 0 failed**.
- Push этой ветки не запускает Release или Pages; текущий CI запускается для
  `main` и pull request, но не от push `release/1.0.0`.

Живой пилот MEM-01 остаётся следующим шагом
[плана продолжения](../roadmap/2026-11-15-company-brain-roadmap.md).
