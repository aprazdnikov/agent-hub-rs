# MEM-05 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans.

**Goal:** Подготовить полный проверяемый пакет выбранных записей для следующего агента.
**Architecture:** Read-only сборка одного snapshot, ограниченный immutable пакет, повторная проверка перед выдачей.
**Tech Stack:** Rust, serde_json, egui; без новых зависимостей.
**Spec:** `docs/specs/2026-11-16-memory-context-package-design.md`.

## Global Constraints

Предыдущие локальные изменения MEM-04 сохраняются. Реальные данные не менять.
/handoff и MCP tool surface не менять. Без model calls и автоматического запуска агентов.
Без commit/push/публикации. Бюджет 1024–65536 байт, default 16384; поля поручения
до 4096 байт, источники 1–8 по 2048 байт; выбранных ключей 1–20, ревизии точные.

## Review Focus

- Поля поручения не теряются при переполнении бюджета.
- Пакет старого проекта/привязки не выдаётся в новом контексте.
- Историческая/stale запись не выглядит как текущий факт.
- Источники из памяти не запускают файловые/сетевые операции.
- Длинный Telegram пакет передаётся целиком нумерованными частями; ошибка останавливает выдачу без ложного успеха.

### Task 1: Пакет и проверка свежести

**Files:** `hub-memory/src/store/context.rs`, `lib.rs`, `model.rs`, `presentation.rs`,
`tests/storage/context.rs`.
**Interfaces:** `ContextRequest`, `ContextBrief`, `ContextSelection`, immutable `ContextPacket`;
`MemoryReader::context(&ContextRequest) -> Result<ContextPacket, Error>`;
`MemoryReader::validate_context(&ContextPacket) -> Result<(), Error>`.

- [x] Написать тесты полного состава и границы бюджета; запустить до реализации (RED).
- [x] Реализовать чтение одного snapshot, полный JSON и предупреждения, byte budget.
- [x] Проверить stale token, другую область, отсутствующие версии, источники и bindings.
- [x] `cargo test -p hub-memory --locked`: PASS.

### Task 2: GUI и Telegram

**Files:** `hub-app/src/gui/memory/{context.rs,lifecycle.rs}`, `gui/memory.rs`,
`hub-telegram/src/memory/{context.rs,lifecycle.rs,admin.rs}`, tests.
**Interfaces:** потребляет Task 1; Action::Context/ValidateContext,
Panel::Context/ContextReady. Clipboard копирует только проверенный ответ текущей generation.

- [x] Написать проверки формы, target scope и очистки GUI draft при смене проекта.
- [x] Добавить выбор текущей ревизии, поля, бюджет, preview и кнопку выдачи.
- [x] Добавить /memory context с русскими заголовками строк, без парсинга содержимого записей.
- [x] `cargo test -p hub-telegram -p hub-app --locked`: PASS.

### Task 3: Проверка и передача

**Files:** руководство памяти, roadmap, отдельный отчёт.

- [x] Выполнить workspace tests/clippy/fmt, сборку приложения, diff check.
- [x] Независимое итоговое ревью; существенные замечания исправить с regression RED→GREEN.
- [x] Зафиксировать реальные результаты и короткую проверку сценария MEM-01 пользователем.


## Результат

[Отчёт](../research/2026-11-16-memory-context-package-check.md): 1949 passed / 0 failed /
15 ignored. Clippy/fmt/build/diff check — PASS. Один Important независимого ревью
исправлен с runtime RED → GREEN. Живая проверка MEM-01 остаётся пользователю.
