# Режимы одобрений Hermes: план и результаты

> Спецификация: [2026-11-10-hermes-approval-modes-design.md](../specs/2026-11-10-hermes-approval-modes-design.md).
> [Каталог документов](../README.md) · [Хронология](../history.md).

Этап добавляет настройку одобрений агенту Hermes: раньше хаб всегда принудительно
выставлял `default` и не запускал `--yolo`.

## Задачи

- [x] Ядро: `HermesApproval` (`default`, `accept_edits`, `dont_ask`, `yolo`) с `wire()`,
      `parse()`, `mode_id()`, `launch_options()`; поле `approval` в `HermesSettings`,
      `HermesField`, `HermesDraft`, `HermesFile`; round trip `settings.toml` и дефолт для
      файла без ключа.
- [x] Режим темы: `Mode::Hermes`, `Mode::all`/`wire`/`parse`, `resolve`, `apply`,
      `bypasses_approval` только для `yolo`. Отдельного кода в `hub-telegram` не требуется.
- [x] Бэкенд: `--yolo` перед `acp` (глобальная опция), `session/set_mode` с `mode_id()`,
      сверка с `modes.availableModes[].id` и явная ошибка для необъявленного режима,
      перевыставление режима после `session/load`.
- [x] GUI: поле «Одобрения» на странице Hermes и предупреждение при `yolo`.
- [x] Тесты: разбор и round trip настроек, контракт enum, реестр режимов и обход
      одобрений, фикстурный ACP (argv, `modeId`, необъявленный режим, отсутствие режимов),
      видимость поля в GUI.
- [x] Руководство: `agents.md`, `settings.md`, `projects.md`, `security.md`.
- [x] Гейты: форматирование, Clippy, тесты, сборка, `cargo deny`, `git diff --check`.

## Результаты проверки

- Контракт подтверждён на установленной сборке `Hermes 0.22.5`:
  `hermes --yolo acp --version` → `rc=0`; `hermes acp --yolo --version` →
  `rc=2`, `hermes: error: unrecognized arguments: --yolo`.
- Живая ACP-проба (`initialize` → `session/new` → `session/set_mode`): объявлены режимы
  `default`, `accept_edits`, `dont_ask`; `modeId: "yolo"` отклонён с `-32603`
  (`data.details = "invalid_params"`) — поэтому `yolo` в хабе это `--yolo` плюс режим
  `dont_ask`, а не отдельный ACP-режим.
- `cargo fmt --all`, `cargo clippy --workspace --all-targets --locked -- -D warnings`,
  `cargo test --workspace --locked`, `cargo build --workspace --locked` прошли.
- Изменения не закоммичены; выпуск и запись в хронологии — на этапе подготовки релиза.

## Что осталось за рамками

- `--accept-hooks` и `approvals.mode: off` в конфигурации Hermes — его собственные
  настройки, хаб их не пишет.
- Подтверждения чувствительных путей и правил `deny` приходят и при `yolo`; их обрабатывает
  существующий `Gate` хаба.
- Sticky-режим самого Hermes (переключение `/yolo` в его CLI) не трогается: при каждом
  запуске сессии хаб перевыставляет настроенный режим.
