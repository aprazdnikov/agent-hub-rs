# MEM-06 Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans for inline execution.

**Goal:** Явно доступная процедура continue-work@1 в GUI и Telegram.
**Architecture:** Один встроенный Markdown; существующие поверхности чтения.
**Tech Stack:** Rust, egui, текущий Telegram dispatcher.
**Spec:** ../specs/2026-11-21-memory-procedures-design.md

## Global Constraints

Без изменения MCP, схемы данных, CLI settings и запуска модели. Без commit/push/релиза.
Сохранить изменения подготовки релиза. Продолжать в текущей release-ветке.

## Review Focus

Не выдавать инструкции за согласие пользователя. Не открывать память для справки.
Не обрезать Telegram текст. Не принимать несуществующие версии или лишние аргументы.
Пользователь должен увидеть текст до его явного копирования.

### Task 1: Процедура в обоих интерфейсах

**Files:** docs/guide/procedures/continue-work-v1.md;
crates/hub-memory/src/lib.rs; crates/hub-telegram/src/memory.rs;
crates/hub-telegram/src/memory/lifecycle.rs;
crates/hub-app/src/gui/memory/context.rs;
crates/hub-telegram/tests/memory/context.rs.
**Interfaces:** hub_memory::CONTINUE_WORK_V1: &str;
Action::Procedure → Panel::Notice; `/memory procedure` без аргументов.

- [x] Добавить regression parser/read: команда возвращает текст без доступа
  к отсутствующему проекту/хранилищу; лишние аргументы отвергаются. Запустить RED.
- [x] Написать Markdown с идентификатором, условиями, шагами и критериями результата;
  экспортировать include_str, подключить к dispatcher и GUI preview/copy.
- [x] Проверить GREEN, лимит одного Telegram сообщения и GUI вывод.
- [x] Выполнить workspace tests, Clippy, fmt, build; независимое финальное ревью.
- [x] Записать результаты и подготовить сценарий сравнения на двух harness.

Коммит отложен до поручения пользователя. MEM-07/08 имеют отдельные планы после
уточнения рабочего сценария; их реализация не входит в этот маленький этап.

Результат: 1952 теста workspace + отдельный GUI-тест PASS; Clippy/fmt/build PASS.
Два Important финального ревью исправлены с runtime RED→GREEN. Живой пилот
подготовлен, но не выполнен; это не входит в отметку завершения реализации.
