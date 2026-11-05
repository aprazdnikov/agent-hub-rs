# Разработка

[Документация](README.md) · [О проекте](../README.md)

Сборка, проверки, устройство workspace и подготовка релизов.

## Сборка и проверки

Команды выполняются из корня репозитория. Toolchain Rust 1.96 и компоненты
`rustfmt`/`clippy` заданы в [`rust-toolchain.toml`](../rust-toolchain.toml).

```bash
cargo build -p hub-app --bin agent-hub --locked
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
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

| Крейт | Что внутри |
|---|---|
| `hub-core` | Ядро без ввода-вывода: доменные типы, разбор настроек и команд, Markdown → Telegram HTML, реестры подтверждений и вопросов, формат `topics.json` |
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
bash scripts/test-macos-package.sh target/release/agent-hub 0.9.0
bash scripts/package-macos.sh target/release/agent-hub 0.9.0 dist/agent-hub.app
open dist/agent-hub.app
```

Скрипт не перезаписывает существующий `.app`. Smoke-тест проверяет пакет и ZIP без запуска
приложения и без обращения к пользовательским настройкам. PNG для macOS создаются из того же
рисунка командой `python3 scripts/make-icon.py --iconset OUTPUT.iconset`; `iconutil` собирает
из них `.icns`. Бинарник копируется без изменений, без добавления shell-обёртки, подписи
пакета или нотарификации. Подпись Mach-O от линкера, если она есть, сохраняется, но это не
подпись всего `.app`: полноценная проверка пакета через `codesign --verify` не проходит.

Выпуск релиза: поднять `version` в корневом `Cargo.toml`, закоммитить и поставить тег
`vX.Y.Z` — workflow `release.yml` соберёт четыре платформы, архивы, бинарники и `.sha256` и
опубликует релиз. Для macOS дополнительно создаются `.app.zip` и их `.sha256`; прежние
артефакты сохраняются. Тег должен совпадать с версией.

Планы и история: [каталог документации](README.md). Порядок публикации и журнал
выпуска: [релизы](releases/README.md).
