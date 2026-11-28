# Разработка

[Документация](README.md) · [О проекте](../README.md)

Сборка, проверки, устройство workspace и подготовка релизов.

Настольный интерфейс использует Kylig UI поверх egui. Команда `cargo run -p hub-app --bin agent-hub` открывает рабочее приложение. [Интерактивный макет](prototypes/kylig/README.md) использует ту же палитру и открывается в браузере с демонстрационными данными.

## Сборка и проверки

Команды выполняются из корня репозитория. Toolchain Rust 1.98 и компоненты
`rustfmt`/`clippy` заданы в [`rust-toolchain.toml`](../rust-toolchain.toml).

```bash
cargo build -p hub-app --bin agent-hub --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all --check
cargo deny check
node --test scripts/test-macos-authorization.mjs
cargo run -p hub-app --bin agent-hub
```

Проверка скрипта авторизации macOS требует Node.js 18+ и работает на любой ОС.
Она дополняет нативный тест Service Management, который запускается только на macOS.

Живой тест с настоящим `claude` (тратит токены): `cargo test -p hub-claude -- --ignored`.
Пробы лимитов (не тратят токены): `AGENT_HUB_LIVE_CLI=1 cargo test -p hub-claude --test live -- --ignored live_usage` и `AGENT_HUB_LIVE_CODEX=1 cargo test -p hub-codex --test live -- --ignored live_usage`.
Живые тесты с `qwen` (тратят токены, нужен настроенный `qwen`):
`AGENT_HUB_LIVE_QWEN=1 cargo test -p hub-qwen --test live -- --ignored --test-threads=1`.

Проверить полный цикл задачи с настоящим Codex можно командой `AGENT_HUB_LIVE_CODEX=1 cargo test -p hub-app --lib live_codex -- --ignored`. Два сценария расходуют токены и используют временные Git-репозитории. Второй проверяет правку файла, одну попытку исправления, сохранённый diff и ревью.

Для проверки доставки в Telegram задайте `AGENT_HUB_LIVE_CHAT` и `AGENT_HUB_LIVE_TOPIC` и выполните `cargo test -p hub-app --test live_telegram -- --ignored --nocapture`. Тест берёт токен из системного хранилища, отправляет одну карточку в указанную тему настроенной группы и снимает кнопки редактированием сообщения. Для проверки обработки нажатий локально выполните `cargo test -p hub-telegram --lib result_card`.

| Крейт | Что внутри |
|---|---|
| `hub-core` | Ядро без ввода-вывода: доменные типы, разбор настроек и команд, Markdown → Telegram HTML, реестры подтверждений и вопросов, формат `topics.json` |
| `hub-memory` | Общая версионируемая память проекта, межпроцессная запись и самостоятельный stdio MCP без вызовов моделей |
| `hub-agent` | Общие контракты разговора и пользователя, JSON-RPC клиент, CLI-проверки, схемы инструментов и защищённый MCP HTTP-сервер |
| `hub-claude` | Протокол `claude` CLI (stream-json): процесс, control-запросы, разрешения, вопросы, MCP-инструмент `send_file` |
| `hub-codex` | Протокол `codex app-server`: процесс, одобрения, вопросы, `send_file` |
| `hub-qwen` | Протокол `qwen --acp` (ACP): процесс, одобрения, вопросы, MCP-сервер инструментов хаба, `send_file` |
| `hub-hermes` | Протокол `hermes acp`: отдельный адаптер, восстановление, последовательные ходы, разрешения и инструменты общего слоя; не зависит от других бэкендов |
| `hub-opencode` | Протокол `opencode acp`: процесс, сессии, разрешения и инструменты хаба |
| `hub-telegram` | Бот: актор тем, команды, вложения, отправка сообщений с повтором и откатом на обычный текст |
| `hub-app` | Приложение: супервизор, хранилища, keyring, лог, окно (egui), трей, обновления |

Иконка рисуется скриптом: `python scripts/make-icon.py` → `assets/agent-hub.ico`.

Локальная упаковка macOS (нужны macOS и Python 3; версия должна совпадать с `Cargo.toml`):

```bash
cargo build --release --locked -p hub-app
version=$(cargo pkgid --locked -p hub-app | sed 's/.*[#@]//')
bash scripts/test-macos-package.sh target/release/agent-hub "$version"
bash scripts/package-macos.sh target/release/agent-hub "$version" dist/agent-hub.app
open dist/agent-hub.app
```

Скрипт не перезаписывает существующий `.app`. Smoke-тест проверяет пакет и ZIP без запуска
приложения и без обращения к пользовательским настройкам. PNG для macOS создаются из того же
рисунка командой `python3 scripts/make-icon.py --iconset OUTPUT.iconset`; `iconutil` собирает
из них `.icns`. Бинарник копируется без изменений, без добавления shell-обёртки, подписи
пакета или нотарификации. Подпись Mach-O от линкера, если она есть, сохраняется, но это не
подпись всего `.app`: полноценная проверка пакета через `codesign --verify` не проходит.

Проверка Debian-пакета на Linux x86-64 (нужны `dpkg-deb`, `dpkg-shlibdeps`,
`readelf` и Python 3; тест не устанавливает пакет):

```bash
cargo build --release --locked -p hub-app
version=$(cargo pkgid --locked -p hub-app | sed 's/.*[#@]//')
bash scripts/test-deb-package.sh target/release/agent-hub "$version"
bash scripts/package-deb.sh target/release/agent-hub "$version" dist/agent-hub.deb
```

Windows-установщик собирается из x64 PE через NSIS 3.10. На Windows скрипт
`scripts/test-windows-installer.ps1` проверяет установку, обновление и удаление
во временном каталоге и тестовом разделе HKCU.

## Выпуск релиза

1. Обновите версию workspace в `Cargo.toml` и `Cargo.lock`. Подготовьте непустой
   `docs/releases/vX.Y.Z.md`. Этот Markdown станет описанием на GitHub.
2. После слияния дождитесь CI на итоговом коммите `main`. Поставьте на него
   аннотированный тег `vX.Y.Z` с версией из `Cargo.toml` и отправьте тег в `origin`.
3. Дождитесь workflow Release. Он собирает Windows x64, Linux x64 и обе архитектуры macOS,
   проверяет упаковку и публикует отдельные бинарники, архивы, установщик Windows,
   приложения macOS и пакет Debian. К каждому файлу прилагается `.sha256`.
4. Проверьте цель тега, описание на GitHub и SHA-256 скачанных файлов.
   Сайт публикуется отдельным workflow Pages при изменении `site/` в `main`.

Подробности и журнал выпусков см. в [порядке выпуска](releases/README.md#порядок-выпуска).
