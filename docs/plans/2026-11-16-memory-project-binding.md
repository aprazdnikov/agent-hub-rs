# MEM-04 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans. Steps use checkbox syntax.

**Goal:** Сохранить память при явном переносе checkout и подключении worktree.
**Architecture:** Общий resolver и атомарный локальный реестр; данные остаются на месте.
**Tech Stack:** Rust, существующие serde/sha2/filesystem, egui.
**Spec:** `docs/specs/2026-11-16-memory-project-binding-design.md`.

## Global Constraints

Без новых зависимостей, без изменений реальных пользовательских данных. Все пять
адаптеров используют существующий CLI. Ручной режим очистки остаётся по умолчанию.
Текущая release-ветка; работа без автоматического commit/push и публикации.

## Review Focus

- Устаревший процесс после смены привязки не пишет в прежнюю память.
- Сбой публикации не скрывает исходные данные.
- macOS/Windows canonical paths совпадают с legacy идентичностью.
- Подмена реестра symlink и некорректный JSON дают отказ.
- Занятое назначение не теряет даже пустой snapshot с политикой/tombstones.

### Task 1: Реестр и общий resolver

**Files:** `hub-memory/src/store/{bindings.rs,../store.rs}`, `lib.rs`, `model.rs`,
`tests/storage/{bindings.rs,../storage.rs}`.
**Interfaces:** `BindingMode::{Move,Share}`, `BindingPreview::prepare(base,source,target,mode)`,
`BindingPreview::apply()`, read-only getters и `confirmation()`.

- [x] Написать тест переноса после rename: прежняя запись читается новым Store/Reader.
- [x] Запустить `cargo test -p hub-memory --test storage bindings`; ожидать отказ до реализации.
- [x] Реализовать preview/apply, resolver и проверку привязки существующих handles.
- [x] Проверить share/изоляцию, конфликты, старые handles, stale preview, v1,
      повреждение реестра, symlink, прерванную публикацию и повтор.
- [x] `cargo test -p hub-memory --locked`: все тесты проходят.

### Task 2: Управление в GUI и CLI

**Files:** `hub-memory/src/project_cli.rs`, `hub-app/src/{main.rs,gui/memory/lifecycle.rs}`,
`hub-telegram/src/memory/{lifecycle.rs,admin.rs}`.
**Interfaces:** потребляет BindingPreview; `run_project_cli(args)`; общие Action/Panel
для preview и подтверждения привязки с проверкой целевого cwd.

- [x] Тест CLI: preview не изменяет привязку, неверный токен не применяет её,
      правильный токен делает память доступной целевому MCP клиенту.
- [x] Реализовать `--memory-project move|share --from PATH --to PATH
      [--store-dir PATH] [--confirm TOKEN]`.
- [x] GUI: исходный путь, режим, preview, явное подтверждение; выбранный проект — назначение.
- [x] Проверить устаревшее подтверждение/смену cwd и общую отрисовку.
- [x] `cargo test -p hub-memory -p hub-telegram -p hub-app --locked`: PASS.

### Task 3: Документация и итоговая проверка

**Files:** руководство памяти, roadmap, отчёт MEM-04.
**Interfaces:** документирует поведение Task 1–2 и ограничения восстановления.

- [x] Обновить руководство и статус MEM-03 по сообщению пользователя.
- [x] `cargo fmt --all -- --check`, workspace clippy, workspace tests, cargo deny,
      `cargo build -p hub-app --locked`, `git diff --check`: PASS.
- [x] Независимое итоговое ревью; существенные замечания исправить с regression tests.
- [x] Записать фактические результаты и короткий сценарий ручной проверки.


## Фактический результат

Выполнено в текущем checkout без commit/push. [Отчёт](../research/2026-11-16-memory-project-binding-check.md)
содержит точные результаты, замечания ревью, платформенные ограничения и ручной сценарий.
Начальные RED новых API были ошибками отсутствующего интерфейса при компиляции;
три regression-проверки исправлений прошли runtime RED → GREEN.
Итог: workspace 1938 passed / 0 failed / 15 ignored; fmt/clippy/deny/build — PASS.
Независимое ревью выполнено, два Important исправлены, один Minor отображения отложен.
