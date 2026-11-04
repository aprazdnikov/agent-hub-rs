# Хронология разработки

[Каталог документов](README.md) · [Текущий статус](roadmap/README.md)

Состояние сверено с историей Git на 2026-11-05, до коммита `33f7693` включительно.
Таблицы различают порядок планирования, реализацию и включение в релиз. Исходные
чекбоксы старых планов не позволяют восстановить выполнение каждого отдельного шага.

## Первый релиз: пять фаз

Основание — [дизайн от 2026-10-02](specs/2026-10-02-agent-hub-rs-design.md).
Планы задавали следующую последовательность:

| Порядок | План | Что подготовил для следующего этапа |
|---|---|---|
| 1 | [hub-core](plans/2026-10-02-phase-1-hub-core.md) | Доменные типы, команды, настройки и форматирование без I/O |
| 2 | [hub-claude](plans/2026-10-03-phase-2-hub-claude.md) | Протокол Claude CLI, процесс и разговор поверх ядра |
| 3 | [hub-telegram](plans/2026-10-03-phase-3-hub-telegram.md) | Темы, сообщения, разрешения и запуск сессий через Telegram |
| 4 | [hub-app](plans/2026-10-03-phase-4-hub-app.md) | GUI, трей, хранилища и управление ботом |
| 5 | [Обновления и выпуск](plans/2026-10-03-phase-5-release.md) | Автообновление, CI, сборки, README и релиз |

Это порядок планов, а не пять отдельных коммитов: исходный набор вошёл коммитом
[`2599aff`](https://github.com/aprazdnikov/agent-hub-rs/commit/2599aff), затем через PR #1
в первый релиз [v0.1.0](https://github.com/aprazdnikov/agent-hub-rs/tree/v0.1.0).

## Развитие до v0.7.1

| Реализация / релиз | Основание → план | Результат и связь с предыдущими этапами |
|---|---|---|
| 2026-10-04 · [v0.2.0](https://github.com/aprazdnikov/agent-hub-rs/tree/v0.2.0) | [Codex: решение](specs/2026-10-04-codex-backend-design.md) → [план](plans/2026-10-04-codex-backend.md) | Второй бэкенд; общие контракты вынесены в `hub-agent` |
| 2026-10-08 · [v0.3.0](https://github.com/aprazdnikov/agent-hub-rs/tree/v0.3.0) | [Qwen: решение](specs/2026-10-08-qwen-backend-design.md) → [план](plans/2026-10-08-qwen-backend.md) | Третий бэкенд; общий JSON-RPC и локальный HTTP MCP |
| 2026-10-09 · [v0.3.1](https://github.com/aprazdnikov/agent-hub-rs/tree/v0.3.1) | Отдельного документа в `docs` нет; [PR #2](https://github.com/aprazdnikov/agent-hub-rs/pull/2) | Исправления настроек и упаковки macOS |
| 2026-10-09 · [v0.4.0](https://github.com/aprazdnikov/agent-hub-rs/tree/v0.4.0) | [Hermes: решение](specs/2026-10-09-hermes-backend-design.md) → [план](plans/2026-10-09-hermes-backend.md); [меню: решение](specs/2026-10-09-telegram-command-menu-design.md) → [план](plans/2026-10-09-telegram-command-menu.md) | Четвёртый бэкенд и синхронизация меню Telegram из общего реестра команд |
| 2026-10-15 · [v0.5.0](https://github.com/aprazdnikov/agent-hub-rs/tree/v0.5.0) | [Снимки: решение](specs/2026-10-09-session-snapshots-design.md) → [план от 9 октября](plans/2026-10-09-session-snapshots.md) | Сохранение и восстановление полной истории поддержанных бэкендов; реализация и ревью завершались позже даты плана |
| 2026-10-17 · [v0.6.0](https://github.com/aprazdnikov/agent-hub-rs/tree/v0.6.0) | [Лимиты: решение](specs/2026-10-16-subscription-limits-design.md) → [план от 16 октября](plans/2026-10-16-subscription-limits.md) | `/limits` для Claude и Codex; короткие пробы CLI и данные живых сессий |
| 2026-10-22 · [v0.6.1](https://github.com/aprazdnikov/agent-hub-rs/tree/v0.6.1) | Отдельного плана в `docs` нет; [PR #6](https://github.com/aprazdnikov/agent-hub-rs/pull/6), [`ab64708`](https://github.com/aprazdnikov/agent-hub-rs/commit/ab64708) | Сайт проекта и обновления TLS-зависимостей |
| 2026-10-30 · [v0.6.2](https://github.com/aprazdnikov/agent-hub-rs/tree/v0.6.2) | [Коммит зависимостей](https://github.com/aprazdnikov/agent-hub-rs/commit/11305a6) | Исправления зависимостей без отдельного плана |
| 2026-10-31 · [v0.7.0](https://github.com/aprazdnikov/agent-hub-rs/tree/v0.7.0) | [OpenCode: решение](specs/2026-10-31-opencode-backend-design.md) → [план](plans/2026-10-31-opencode-backend.md) | Пятый бэкенд; собственные ограничения снимков и лимитов |
| 2026-11-04 · [v0.7.1](https://github.com/aprazdnikov/agent-hub-rs/tree/v0.7.1) | [Исправление](https://github.com/aprazdnikov/agent-hub-rs/commit/7ec6337), [релизный коммит](https://github.com/aprazdnikov/agent-hub-rs/commit/fd7aa46) | Устранена гонка подтверждения разрешения и тайм-аута |

## Улучшения после v0.7.1

Основание — [backlog удобства использования](roadmap/2026-11-04-usability-backlog.md).
Запрос начался с необходимости включать VPN с телефона. Вместо отдельных интеграций
OpenVPN/snx были выбраны произвольные именованные команды, включая постоянные процессы.
AH-01–AH-05 отложены; ACT-01 реализовал универсальный механизм.

Ниже **фактический порядок коммитов**. Номера AH — идентификаторы задач исходного списка,
поэтому AH-13 и AH-14 выполнены раньше AH-06, а AH-11 и AH-12 — раньше AH-08.

| № | Дата | Этап и план / журнал | Основание или зависимость | Коммит |
|---|---|---|---|---|
| 1 | 2026-11-04 | [ACT-01: пользовательские действия](plans/2026-11-04-custom-actions.md), DEP-01: зависимость async-executor | Выбор универсальных команд вместо VPN-интеграций | [`ec5037d`](https://github.com/aprazdnikov/agent-hub-rs/commit/ec5037d) |
| 2 | 2026-11-04 | [AH-13: названия тем](plans/2026-11-04-topic-titles.md) | Метаданные для навигации | [`1641c6e`](https://github.com/aprazdnikov/agent-hub-rs/commit/1641c6e) |
| 3 | 2026-11-04 | [AH-14: поиск и переходы](plans/2026-11-04-topic-navigation.md) | Использует сохранённые названия AH-13 | [`c334342`](https://github.com/aprazdnikov/agent-hub-rs/commit/c334342) |
| 4 | 2026-11-04 | [AH-06: профили проектов](plans/2026-11-04-project-profiles.md) | Связывает каталог, бэкенд и действия ACT-01 | [`6a79309`](https://github.com/aprazdnikov/agent-hub-rs/commit/6a79309) |
| 5 | 2026-11-04 | [AH-07: `/doctor`](plans/2026-11-04-project-doctor.md) | Проверяет ресурсы профиля AH-06 и связанные действия | [`a4d9fa4`](https://github.com/aprazdnikov/agent-hub-rs/commit/a4d9fa4) |
| 6 | 2026-11-04 | [AH-11: `/last`](plans/2026-11-04-last-result.md) | Сохранение финального ответа перед доставкой | [`67fcf4c`](https://github.com/aprazdnikov/agent-hub-rs/commit/67fcf4c) |
| 7 | 2026-11-04 | [AH-12: `/outbox`](plans/2026-11-04-outbox.md) | Расширяет AH-11 очередью и повторной доставкой | [`6411709`](https://github.com/aprazdnikov/agent-hub-rs/commit/6411709) |
| 8 | 2026-11-04 | [AH-08: модель и режим темы](plans/2026-11-04-topic-options.md) | Добавляет параметры темы, профиля AH-06 и снимков v0.5.0 | [`7c0f794`](https://github.com/aprazdnikov/agent-hub-rs/commit/7c0f794) |
| 9 | 2026-11-04 | [AH-09: состояния задач](plans/2026-11-04-task-states.md) | Общая модель прогресса для GUI и Telegram | [`9a666b0`](https://github.com/aprazdnikov/agent-hub-rs/commit/9a666b0) |
| 10 | 2026-11-04 | [AH-10: карточка прогресса](plans/2026-11-04-progress-card.md) | Показывает состояния AH-09 в одном сообщении | [`a57f600`](https://github.com/aprazdnikov/agent-hub-rs/commit/a57f600) |
| 11 | 2026-11-04 | [AH-15: автозапуск](plans/2026-11-04-autostart.md) | Linux, Windows и macOS по выбору пользователя | [`fd23479`](https://github.com/aprazdnikov/agent-hub-rs/commit/fd23479) |
| 12 | 2026-11-05 | [AH-16: голосовые задачи](plans/2026-11-04-voice-tasks.md) | Два способа распознавания, исправление и подтверждение | [`5f20485`](https://github.com/aprazdnikov/agent-hub-rs/commit/5f20485) |
| 13 | 2026-11-05 | [AH-17: передача со сводкой](plans/2026-11-05-task-handoff.md) | Служебный ход источника, проверка текста, новая тема получателя | [`33f7693`](https://github.com/aprazdnikov/agent-hub-rs/commit/33f7693) |

Все 13 коммитов отправлены в `origin/main` 2026-11-05. Версия в Cargo.toml остаётся
`0.7.1`: включение в main ещё не является выпуском следующей версии.
[Текст v0.8.0](releases/v0.8.0.md) подготовлен как черновик; обновление версии и тег
ещё не выполнены. Открытые ручные проверки собраны в [текущем статусе](roadmap/README.md).

## Что изменилось в самой документации

2026-11-05 документы распределены по назначению: спецификации, планы, roadmap и релизы.
Все 34 исходных документа сохранены. Добавлены ссылки между ними, статус исторических
планов и эта хронология; подготовленный текст релиза перенесён из временного файла.
Проверка документации не меняет результаты проверок продукта в журналах этапов.

Проверка структуры: 38 документов доступны из общего каталога; 208 локальных ссылок
и изображений, якоря заголовков и 29 ссылок на Git-коммиты/теги проверены. У всех
34 исходных документов сохранены количества открытых и выполненных чекбоксов.
Изменены только Markdown-файлы; тесты приложения для этой перестановки не запускались.
