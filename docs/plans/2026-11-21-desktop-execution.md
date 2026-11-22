# Настольный запуск задач — этап 3.2

> Execute inline with superpowers:executing-plans; final fresh-context review.

**Goal:** Запуск, остановка, вопросы и разрешения для задачи в GUI без Telegram.
**Architecture:** Локальный исполнитель использует те же CLI-backends и TaskService.
Отдельная типизированная конфигурация исполнения не содержит Telegram credentials.
Supervisor владеет одной локальной операцией, публикует её состояние и останавливает
при выходе. Бот и локальное выполнение взаимоисключающие; управление задачами доступно.
Каждый запуск использует свежую CLI-сессию и полный сохранённый контекст задачи.
Git подготовка/проверка использует существующий LocalGitRunner.
**Spec:** docs/specs/2026-11-21-task-workflow-design.md
**Tech Stack:** Rust, Tokio, egui; существующие CLI backends.

## Review Focus

- Старая ревизия задачи/правил или чужой каталог: отказ до внешнего действия.
- Сбой записи: нет запуска; сбой сохранения результата останавливает исполнение.
- Позднее разрешение, timeout, закрытие приложения: нет разрешения другой попытке.
- Бот/автоподключение/смена настроек во время local job не создают второй исполнитель.
- Git отмена и ошибка оставляют диагностику, но не создают попытку агента.

## Tasks

- [x] ExecutionSettings + Draft parsing без Telegram; shared backend execution entry.
  Tests: missing Telegram allowed, malformed selected configuration rejected.
- [x] Desktop UserChannel: bounded pending request, unique ID, explicit allow/deny,
  questions/free text, timeout/cancel denial. No automatic approval; file transfer
  returns honest unsupported result. Tests stale reply, cancellation, answers.
- [x] Local runtime: revision/project/rules guards, durable intent, result/terminal state,
  Git preparation and checked launch, stop/drop cleanup, no automatic retry.
  Tests fake executor/temporary storage: run/result/stop/errors, Git no launch on failure.
- [x] Supervisor and GUI: LocalTasks effects, local responses, live state, stop,
  mutually exclusive bot lifecycle, inputs stable across refresh. Tests no-credentials
  start, explicit response, shutdown, headless local controls.
- [x] Full tests/fmt/clippy/build, fresh independent review, fixes and manual checklist.

No commit/push. Earlier uncommitted changes preserved. Manual live CLI acceptance remains
with user; automated tests use fake backends, no paid model calls.
