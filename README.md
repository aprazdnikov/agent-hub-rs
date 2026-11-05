# agent-hub

[![Architecture diagram](https://gitdiagram.com/diagram-badge.svg)](https://gitdiagram.com/aprazdnikov/agent-hub-rs?utm_source=readme&utm_medium=badge)
[![XERJ Corpus Hub contributor](https://hub.xerj.org/contributors/aprazdnikov/badge.svg)](https://hub.xerj.org/contributors/aprazdnikov)

**Каждая тема Telegram-группы — отдельная сессия AI-агента на вашем компьютере.**

Пишете задачу с телефона — Claude Code, Codex, Qwen Code, Hermes или OpenCode работает
в выбранном проекте, присылает результаты и спрашивает разрешения кнопками в Telegram.
Темы работают параллельно; контекст сохраняется между сообщениями и перезапусками.

agent-hub — приложение для **Windows, macOS и Linux** с окном настроек и значком в трее.
Светлая и тёмная темы следуют системной теме. Настройки разделены по задачам и агентам.

```text
Telegram (форум-группа)
  ├─ backend: тесты → Claude, ~/Projects/shop/backend
  ├─ agent-hub      → Codex,  ~/Projects/agent-hub
  └─ идеи           → Qwen,   ~/Projects
```

[Сайт проекта](https://aprazdnikov.github.io/agent-hub-rs/) ·
[Скачать](https://github.com/aprazdnikov/agent-hub-rs/releases) ·
[Документация](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/README.md)

> Бот выполняет команды от вашего пользователя. До запуска прочитайте
> [о безопасности и границах доступа](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/guide/security.md).

## Быстрый старт

1. Установите и настройте CLI нужного агента. Поддерживаемые версии и ОС перечислены
   в [требованиях](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/guide/getting-started.md#требования).
2. Скачайте сборку из [Releases](https://github.com/aprazdnikov/agent-hub-rs/releases),
   проверьте SHA-256 и запустите приложение. Для macOS используйте `.app.zip`.
3. Создайте бота через [@BotFather](https://t.me/BotFather), добавьте его администратором
   в группу с включёнными темами.
4. В настройках приложения заполните токен, ID группы, разрешённые user_id и корень
   рабочих каталогов. В разделе «Агенты → Общие» выберите настроенного агента по умолчанию.
5. Сохраните настройки, создайте тему в группе и отправьте первую задачу.

[Пошаговая инструкция](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/guide/getting-started.md)
объясняет установку на каждой ОС, права бота и получение ID группы и пользователей.

## Повседневная работа

Создайте тему для задачи и пишите в неё обычным текстом. Для выбора проекта, проверки
состояния и остановки используйте команды:

```text
/projects
/status
/stop
```

Профили проектов объединяют каталог, агента и параметры. `/save` сохраняет снимок
истории, `/handoff` передаёт задачу другому агенту со сводкой. `/last` повторяет последний
сохранённый ответ, `/outbox` управляет доставкой после сбоев Telegram.
Можно отправлять вложения и голосовые сообщения, получать файлы от агента и запускать
заранее заданные команды на компьютере через `/actions`.

## Руководство

| Задача | Где читать |
|---|---|
| Установить приложение и подключить бота | [Установка и первый запуск](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/guide/getting-started.md) |
| Работать с темами, командами и прогрессом | [Работа в Telegram](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/guide/telegram.md) |
| Подключить агента и проверить лимиты | [Агенты и лимиты](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/guide/agents.md) |
| Настроить проекты, модели, проверки и команды | [Проекты и действия](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/guide/projects.md) |
| Подтверждать действия, отправлять голос и файлы | [Сообщения, разрешения и файлы](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/guide/messages.md) |
| Сохранить историю, передать задачу, вернуть ответ | [Сессии, ответы и доставка](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/guide/sessions.md) |
| Настроить окно, автозапуск и обновления; найти файлы данных | [Приложение и настройки](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/guide/settings.md) |
| Проверить границы доступа и ограничения | [Безопасность и ограничения](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/guide/security.md) |

## Разработка

Для локальной сборки из корня репозитория:

```bash
cargo build -p hub-app --bin agent-hub --locked
```

Подробности — в [руководстве разработчика](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/development.md):
проверки, структура крейтов, упаковка macOS и выпуск релизов.
[Каталог документации](https://github.com/aprazdnikov/agent-hub-rs/blob/main/docs/README.md)
содержит планы, архитектурные решения и историю разработки.

## Лицензия

[MIT](LICENSE)
