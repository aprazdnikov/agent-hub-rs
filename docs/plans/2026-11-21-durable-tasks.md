# MEM-07 Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans, inline implementation and one fresh final reviewer.

**Goal:** Долговечные задачи с ручным запуском, результатом и восстановлением.
**Architecture:** Чистая TaskBook в hub-core; отдельный sidecar через TopicStore;
единственный writer — актор Hub. GUI отправляет те же операции через supervisor.
**Tech Stack:** Rust, serde JSON, существующий atomic writer, egui, Telegram.
**Spec:** ../specs/2026-11-21-durable-tasks-design.md

## Global Constraints

Без очереди, сети и автоматического повтора. Одна управляемая задача одновременно.
Сохранить незакоммиченный MEM-06 и подготовку релиза. Без commit/push/публикации.
Текущие CLI permissions, MCP, native skills, обычные чаты сохраняют контракты.

## Review Focus

Сохранение до побочного эффекта; stale revision; сбой между сохранением и запуском;
неподходящий проект/агент; запуск из GUI при остановленном боте; неизвестный формат;
результаты прежнего поколения; остановка и рестарт; разграничение результата и приёмки.

### Task 1: Модель и файловый порт

Files: hub-core/src/tasks.rs, hub-core/tests/tasks.rs; hub-app/src/topics.rs;
hub-telegram/src/hub.rs (TopicStore defaults).
Interfaces: TaskBook create/begin/record/finish/recover, serde validation;
TopicStore load_tasks/save_tasks. 100 задач, 32 запуска на задачу, файл 8 МиБ + 1 КиБ служебного резерва,
поручение/следующий шаг 16 КиБ, результат 64 КиБ. Никакой скрытой обрезки.
- [x] Проверки (начальный RED — отсутствие API; runtime RED регрессий указан в отчёте): повтор begin с прежней ревизией; другой одновременный run; restart recover;
  повреждённый формат; завершение старой попытки не меняет новую.
- [x] Implement domain and bounded load/atomic persist; failure before launch propagates.
- [x] GREEN focused tests.

### Task 2: Актор и Telegram

Files: hub-telegram/src/hub/tasks.rs (child module), commands.rs, hub.rs.
Interfaces: task Request/Reply; HubMessage::Tasks for GUI; /task list/create/get/run/resume.
Create берёт проект и агента текущей темы; run проверяет совпадение и свободную тему.
Before launch persist Running; failed launch → Interrupted. Result and end events matched
by topic+generation+run ID. Restart: persist recovered book; no auto launch.
- [x] Integration tests: creation no launch, one explicit launch, stale replay no effect,
  result persists; restart never starts model; foreign scope rejected; write failure blocks.
- [x] Implement hooks, block unsolicited additional prompts in managed run.
- [x] Run hub tests.

### Task 3: GUI и итоговая проверка

Files: gui/tasks.rs, gui/app.rs, gui/mod.rs, supervisor.rs, docs/guide/tasks.md.
View: choose topic; list/create; exact request revision; explicit start/resume with next step;
result/history; pending/error. Bot must run for the actor port; stopped bot explains why.
- [x] Add GUI routing and clear stale state when changing selected topic.
- [x] Verify tests, clippy, fmt, build, review; fix Important with regression tests.
- [x] Update roadmap/readiness, document remaining live acceptance.
