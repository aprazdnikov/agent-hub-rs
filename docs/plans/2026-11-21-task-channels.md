# Единая задача в GUI и Telegram

> Execute inline using superpowers:executing-plans. Final fresh-context review.

**Goal:** Запуск из GUI, просмотр/ответ/остановка/следующий шаг из Telegram без повтора агента.
**Spec:** docs/specs/2026-11-21-task-workflow-design.md; user approved continuation across channels.
**Architecture:** Один Runtime за синхронным Control port, общий TaskService. Supervisor и
TelegramConnector используют один Arc. При работающем боте новые GUI-запуски проходят
через Hub actor: он сериализует их с обычными сессиями, Git и действиями. Управляемые
/task run/resume/prepare используют тот же Runtime; старый путь остаётся для тестовых
хостов без Control. /task follow ID явно включает доставку состояния в текущую тему.
Вопросы и разрешения связаны с уникальным ID; ответ принимается единожды. При отключении
бота Runtime продолжает работу. Автоматического возобновления после выхода приложения нет.

## Review Focus

- Start bot during running task must not recover/interfere with active attempt.
- Concurrent GUI/TG start and ordinary chat/Git activity must not launch twice.
- Old callbacks, other topic/project/user, changed permissions: no approval or disclosure.
- Answer in either interface removes pending interaction and invalidates other controls.
- Bot disconnect/reconnect and delayed delivery cannot replay effects or leak a late card.

## Tasks

- [x] Shared Control and types, thread-safe runtime host; tests single winner/recovery.
- [x] Hub shared execution routing and follow/answer/stop controls; project/user checks,
  bounded delivery, stale request checks, completion/result delivery. Tests fake transport.
- [x] Supervisor/connector/GUI: allow running bot, serialize starts through Hub,
  no cancellation on transport restart, current live state in GUI. Tests lifecycle.
- [x] Full tests/fmt/clippy/build, independent review and one fix pass, manual guide.

No commit/push or live Telegram/model messages during implementation. Preserve earlier work.
