# Снимки сессий — план реализации

> **For the implementer:** Use `executing-plans` (или superpowers:subagent-driven-development) to execute this plan task-by-task. Шаги размечены чекбоксами (`- [ ]`).

**Goal:** команды `/save [имя]`, `/saves`, `/restore <имя> [каталог]` сохраняют данные хаба и историю агента темы в каталог снимков и разворачивают снимок в другой теме для Claude, Codex, Qwen и Hermes; релиз 0.5.0.

**Architecture:** `hub-core::snapshot` — чистые типы (`SnapshotName`, `RelPath`, `BackendData`, `Manifest`, `default_name`) и разбор команд. `hub-agent::archive` — трейт `SessionArchive` (блокирующий, вызывается из `spawn_blocking`), `ArchiveError` и помощники копирования по белому списку (только обычные файлы, без символьных ссылок, `0700`/`0600`, без перезаписи, откат при сбое). Каждый бэкенд-крейт получает модуль `archive` со своей функцией корня хранилища; Hermes переносит строки `state.db` встроенным Python-скриптом через собственный запуск Hermes. `hub-telegram::snapshots::SnapshotStore` пишет снимок через приватный staging-каталог и одно `rename`; `hub-telegram::transfer` выполняет файловую работу `/save`/`/restore` вне задачи хаба и возвращает результат сообщением `HubMessage::Snapshot`; хаб только затем меняет привязки тем (одна операция над `Topics` и один `put`). `hub-core::settings` получает `[snapshots] dir`, `hub-app` — поле в GUI.

**Tech stack:** Rust 2024 (1.96), tokio (`spawn_blocking`), serde/serde_json, thiserror, uuid v4, chrono 0.4.45 (`clock`, уже в `Cargo.lock`), tempfile (уже в `Cargo.lock`); тесты — rstest, tempfile, `#[ignore]` живые тесты; Python ≥ 3.11 внутри Hermes.

**Spec:** `docs/superpowers/specs/2026-10-09-session-snapshots-design.md` (решения не меняются; расхождения с исходниками агентов и кодом — ниже).

**Constraints:**
- Линты рабочего пространства (`Cargo.toml:56-67`): `warnings = "deny"`, `unsafe_code = "forbid"`, `clippy::pedantic = deny`, `unwrap_used`/`expect_used`/`panic`/`indexing_slicing = deny` вне тестов (`clippy.toml` разрешает их в тестовых функциях и `#[cfg(test)]`-модулях; файлы в `tests/` с помощниками вне `#[test]` ставят `#![allow(clippy::unwrap_used)]` в точке входа, как `crates/hub-hermes/tests/acp.rs:2`). Никаких `#[allow]` в продакшен-коде, никаких `_ =>` на собственных enum, никакой индексации срезов (`get`, `split_first`, шаблоны срезов).
- Гейты после каждой задачи: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`. CI гоняет их на Linux, macOS и Windows: тесты прав доступа и символьных ссылок — `#[cfg(unix)]`, пути в ожиданиях строятся через `Path::join`.
- Задачи, меняющие `Cargo.toml`, сначала выполняют `cargo check --workspace --all-targets` (без `--locked`) — он дописывает рёбра зависимостей в `Cargo.lock`; затем `git diff Cargo.lock` должен показывать только изменения списков `dependencies` наших крейтов, **без новых пакетов** (`chrono`, `tempfile`, `uuid` уже в lock). Затем `cargo deny check`, если `cargo-deny` установлен.
- Секреты: в снимок попадает только перечисленное бэкендом (белый список); `.credentials.json`, `~/.claude.json`, `auth.json`, `oauth_creds.json`, `mcp-oauth-tokens.json`, `.env`, `settings.json`, базы SQLite целиком никогда не перечисляются. В логи — только имена снимков, бэкенд, число файлов; не содержимое историй и не текст ошибок Hermes с данными.
- Hermes и прочие внешние процессы запускаются `std::process::Command` со списком аргументов, без оболочки; данные — через файлы во временном каталоге `0700`; тайм-аут 120 с, затем `kill`.
- Тексты для пользователя — по-русски, дословно как в спецификации, новые — как в задаче 2.
- Код в плане написан против текущих API, но не компилировался. Если `clippy::pedantic` просит `#[must_use]`, ссылку вместо значения, `# Errors`, бэктики в документации (`doc_markdown`) и т. п. — исправить код по линту, не подавляя его, сохраняя имена и сигнатуры из блоков **Interfaces**.
- Коммит после каждой задачи; сообщения по-русски в стиле истории репозитория; **никаких строк атрибуции ИИ** (`Co-Authored-By`, «Generated with …», упоминаний Claude как автора) — пользователь их запрещает. Ветку план не предписывает. `git push` и push тега — только в задаче 14 после отдельного подтверждения пользователя.

**Non-goals:** удаление снимков командой; архив одним файлом и передача через Telegram; создание темы ботом; ответвление сессии; перенос файлов проекта и `file-history` Claude; субагенты Codex и цепочки `history_base`; `advanced.runtimeOutputDir` Qwen; относительные `QWEN_RUNTIME_DIR`/`QWEN_HOME`; ожидание файловой работы снимков при остановке бота (результат, пришедший после `Shutdown`, отбрасывается — повторный `/restore` идемпотентен).

---

## Расхождения со спецификацией

Решения спецификации сохранены; ниже — места, где её первоначальный текст не совпадал с исходниками агентов или с кодом репозитория, и как план это учитывает. Спецификация обновлена по этим пунктам. Пути Hermes — относительно клона `github.com/NousResearch/hermes-agent` (коммит `7085fbf7`, 2026-10-09, `/tmp/claude-1000/-home-aprazdnikov-Projects-aprazdnikov/630dabad-3910-4be9-a25f-331433ae3e58/scratchpad/src/hermes`); Qwen — клона qwen-code `0.25.0` (коммит `4360b00`, `…/scratchpad/src/qwen`).

1. **Выгрузка Hermes — линия сжатия, а не одна строка.** `export_session_lineage(session_id, include_compacted=False, include_inactive=False)` возвращает один словарь `{**последний_сегмент, "segments": [...], "lineage_session_ids": [...], "messages": [...]}` (`hermes_state_portability.py:302-316`). Возврат в Hermes делается так же, как в `adopt_session_lineage_from`: `import_sessions([dict(seg) for seg in payload["segments"]])` (`hermes_state_portability.py:346-386`), а не `import_sessions([d])`. Выгрузка — с `include_inactive=True` (иначе пропадут строки, архивированные сжатием: комментарий `:362-364`). `cwd` и `model_config.cwd` подставляются **в каждый сегмент**.
2. **Интерпретатор Hermes — не из shebang.** Текущие установщики публикуют POSIX-обёртку `#!/bin/sh` + `exec <python> -I -c '<bootstrap>' "$@"` (`hermes_cli/_launchers.py:485-502`, `_write_shell`/`_mint_shell_launcher`), а `~/.local/bin/hermes` — пересылку `exec <root>/.hermes/bin/hermes "$@"` (`_launchers.py:531-548`, `_publish_conveniences`); на Windows — distlib `hermes.exe` или `hermes.cmd` с тем же bootstrap (`_launchers.py:214-243`). Голый интерпретатор не видит зависимостей Hermes — их подключает `hermes_bootstrap` (`_launchers.py:38-75`, `runtime_command`). Зато сам запуск понимает `--run-module <модуль>` (`_launchers.py:472-477`). План: обёртка/`.exe`/`.cmd` → `hermes --run-module cProfile -o <work>/profile.out <work>/agent_hub_snapshot.py <op> …` (`cProfile` — модуль stdlib, который исполняет файл-скрипт как `__main__`; он глотает `SystemExit`, поэтому результат передаётся только файлом `--result`); классический консольный скрипт pip/uv (shebang с абсолютным путём к `python*`, distlib-шим `'''exec' "<python>"` или на Windows `python.exe` рядом с `hermes.exe`) → `<python> -I <script> <op> …`. Shebang из спецификации покрывает только второй случай.
3. **Корень Hermes вычисляет сам Hermes.** `hermes [--profile P] acp` выбирает каталог в `_apply_profile_override` (`hermes_cli/main.py:594-656`): `HERMES_HOME`, указывающий на `<root>/profiles/<x>`, принимается как есть; иначе явный `--profile` или «липкий» `<root>/active_profile`; профиль → `resolve_profile_env` (`hermes_cli/profiles.py:2551-2567`); корень по умолчанию — `~/.hermes` или `%LOCALAPPDATA%\hermes` (`hermes_constants.py:51-58, 216-233`). Хаб эту логику не дублирует: скрипт повторяет `_apply_profile_override` функциями самого Hermes и открывает `<home>/state.db`. Поэтому для Hermes «функция корня» — это `resolve_home` в скрипте, а Rust передаёт только `--profile` из настроек.
4. **`present` и `_restore`.** `_restore` требует `sessions.source == "acp"` (`acp_adapter/session.py:456`) и берёт `cwd` из `model_config` (`:468-469`). Импорт сохраняет `source` из выгрузки (`hermes_state_portability.py:663`, `str(raw.get("source") or "import")`), так что достаточно проверить `get_session(id) is not None`.
5. **Лимиты импорта Hermes:** ≤ 500 сессий, ≤ 10 000 сообщений на сессию, ≤ 5 МиБ на сессию, ≤ 25 МиБ всего (`hermes_state.py:506-507`). Больше — `import_sessions` возвращает `ok: false`; хаб показывает `ArchiveError::Malformed`, README называет ограничение.
6. **Версия схемы Hermes** — две: константа кода `hermes_state_common.SCHEMA_VERSION = 31` (`hermes_state_common.py:283`) и строка `schema_version.version` в `state.db` (`hermes_state_schema.py:988`). Скрипт сверяет обе с `31` (вторую — если файл есть) и иначе отвечает `schema` → «Hermes <версия> не поддерживается для переноса».
7. **Субагенты Qwen лежат в каталоге проекта:** `<projectDir>/subagents/<id>/` (`packages/core/src/agents/agent-transcript.ts:64-87`), т. е. `projects/<san(cwd)>/subagents/<id>/`, а не `subagents/<id>/` в корне. Они переносятся под `san(нового cwd)`, `cwd` в их JSONL переписывается вместе с чатом.
8. **`san` Qwen** работает по кодовым единицам UTF-16 (`packages/core/src/utils/paths.ts:388-392`, `replace(/[^a-zA-Z0-9]/g, '-')`): символ вне BMP даёт `--`. План воспроизводит это через `encode_utf16`.
9. **Относительные `QWEN_RUNTIME_DIR`/`QWEN_HOME`** Qwen разрешает от своего рабочего каталога (`packages/core/src/config/storage.ts:71-90`), а `qwen` хаб запускает без `current_dir` (`crates/hub-qwen/src/backend.rs:274`). План отказывает (`Unsupported`), а не угадывает; `~` разворачивается как у Qwen.
10. **`SnapshotStore` — в `hub-telegram`, не в `hub-app`.** `hub-app` зависит от `hub-telegram` (`crates/hub-app/Cargo.toml`), а хранилище вызывает `Hub`; каталог снимков читается из `Settings` на каждую операцию. `hub-app` получает только поле настроек и GUI. Вместо `save(manifest, staged)` — `save(name, fill)`: хранилище само создаёт staging-каталог, отдаёт `agent/` замыканию, пишет `manifest.json`, переименовывает и убирает мусор при любой ошибке.
11. **`SnapshotName` строже:** кроме `[a-z0-9._-]{1,64}` и запрета ведущей точки — запрет завершающей точки и имён устройств Windows по основе до первой точки (`con`, `prn`, `aux`, `nul`, `com1`…`com9`, `lpt1`…`lpt9`): такие каталоги на Windows не создаются или совпадают с другими.
12. **Название темы.** Хаб знает название только из `forum_topic_created`, увиденного в этом запуске (`crates/hub-telegram/src/hub.rs:238-239`, `titles` не сохраняются). Нет названия — имя по умолчанию строится из имени бэкенда (`claude-20261009-1530`), `manifest.title = null`, `/saves` показывает «—». Пример спецификации `fix-tests-…` — перевод; транслитерация «Починить тесты» даёт `pochinit-testy-…`.
13. **Время.** Суффикс имени — местное время (`chrono::Local`), `created_at` — UTC `YYYY-MM-DDTHH:MM:SSZ`. `chrono 0.4.45` уже в `Cargo.lock` с `iana-time-zone`/`windows-link` (возможность `clock`); новых пакетов нет.
14. **Новые тексты и защита на время файловой работы.** Пока `/save` или `/restore` темы не завершились, в этой теме не стартует ход и отказывают `/new`, `/cwd`, `/reset`, `/backend` (новый текст `SNAPSHOT_BUSY`): иначе ход начался бы со старой сессией или агент дописывал бы копируемую историю. Новые тексты: занятое и неверное имя, подсказка `/restore`, `SNAPSHOT_BUSY`, `SESSION_BUSY_ELSEWHERE`, «снимок не найден» (задача 2).
15. **Сессия занята в другой теме.** Если к концу `/restore` тема-владелец сессии выполняет ход, привязки не меняются (`SESSION_BUSY_ELSEWHERE`); уже возвращённые агенту файлы остаются — повтор `/restore` увидит их через `present` и только перепривяжет.
16. **Профиль Hermes при возврате** — текущий из настроек (в нём тема и будет работать), а не записанный в манифесте; `backend_data.profile` информационный, расхождение пишется в лог.
17. **Манифест:** `backend` выводится из `backend_data.kind`, разбор отвергает несовпадение; `title` может быть `null`; `files` — не больше 10 000. `ArchiveError::Archived(String)` несёт id для текста `codex unarchive <id>`.
18. **Проверенная версия Hermes.** `MIN_VERSION = 0.21.5` (`crates/hub-hermes/src/version.rs:91`) проверен для ACP; путь снимков (`--run-module`, `SessionDB`, схема 31) — только по исходникам `7085fbf7`. Подтверждается живым тестом задачи 12; при несовпадении — `Unsupported`, а не порча данных.

## Хранилища агентов: проверено

**Claude Code ≥ 2.1.280** (исследование к спецификации; локального клона исходников нет — подтверждает живой тест задачи 12). Корень — `$CLAUDE_CONFIG_DIR` или `~/.claude`. Транскрипт — `projects/<P>/<id>.jsonl` и каталог `projects/<P>/<id>/` (`subagents/`, `tool-results/`). `<P>` — `cwd`, где всё, кроме букв и цифр, заменено на `-`; длиннее 200 символов — обрезка и хэш, поэтому хаб `<P>` не вычисляет, а ищет `projects/*/<id>.jsonl`. С 2.1.223 возобновление по id ищет по всем проектам; две копии — «No conversation found» (отсюда `Ambiguous`). `.credentials.json`, `~/.claude.json`, `file-history/` не берутся.

**Codex ≥ 0.160** (исследование; клон `…/scratchpad/src/codex`, `codex-rs/rollout/src/lib.rs:86-87` — `SESSIONS_SUBDIR = "sessions"`, `ARCHIVED_SESSIONS_SUBDIR = "archived_sessions"`; имя — `codex-rs/tui/src/lib.rs:2563`, `rollout-{ts}-{uuid}.jsonl`). Корень — `$CODEX_HOME` (должен существовать) или `~/.codex`. Файлы — `sessions/YYYY/MM/DD/rollout-YYYY-MM-DDTHH-MM-SS-<id>[_<rollout>].jsonl[.zst]`; архивные — в `archived_sessions/`, возобновление отказывает до `codex unarchive`. `thread/resume` находит файл по id даже без строки в SQLite и восстанавливает её; `state_5.sqlite` не копируется; `cwd` при `thread/resume` не проверяется.

**Qwen Code ≥ 0.25** (клон `0.25.0`):
- Корень: `Storage.getRuntimeBaseDir()` — `QWEN_RUNTIME_DIR` → (`advanced.runtimeOutputDir`, не поддерживается) → `getGlobalQwenDir()` = `QWEN_HOME` или `~/.qwen` (`packages/core/src/config/storage.ts:169-202`); `~` разворачивается, относительный путь — от `cwd` процесса (`:71-90`).
- Каталог проекта — `projects/<sanitizeCwd(projectRoot)>` (`storage.ts:619-623`); `sanitizeCwd` — на Windows сначала нижний регистр, затем `[^a-zA-Z0-9]` → `-` (`packages/core/src/utils/paths.ts:388-392`); хэш проекта — `sha256(cwd)`, на Windows от нижнего регистра (`paths.ts:368-373`).
- Запись чата: поле **`cwd`** верхнего уровня каждой записи JSONL (`packages/core/src/services/chatRecordingService.ts:447-448`, заполняется в `createBaseRecord`, `:1515-1548`), рядом `sessionId`, `uuid`, `parentUuid`, `version`.
- Загрузка проверяет первую запись: `sessionId` совпадает с запрошенным без учёта регистра и `getProjectHash(firstRecord.cwd) === projectHash` (`packages/core/src/services/sessionService.ts:1186-1197, 986-1023`); иначе — сессия чужого проекта. Отсюда переписывание `cwd` при новом каталоге.
- Спутники `<id>.*` в `chats/` — `.runtime.json`/`.worktree.json` хранят старые пути (`chatRecordingService.ts`, `getRuntimeStatusPath`, `storage.ts:730-737`) и не берутся; субагенты — `projects/<P>/subagents/<id>/` (`agent-transcript.ts:64-87`); задачи — `<root>/todos/<id>.json` (`packages/core/src/tools/todoWrite.ts:95-103`).

**Hermes** (клон `7085fbf7`; `MIN_VERSION` — `crates/hub-hermes/src/version.rs:91`):
- `SessionDB(db_path: Path | None = None, read_only: bool = False)` (`hermes_state.py:580`), путь по умолчанию — `get_hermes_home() / "state.db"` на момент вызова (`hermes_state.py:198-201`); `close()` (`:1508`), контекстный менеджер (`:1491-1506`); WAL, один писатель, ожидание блокировки до 20–60 с (`:484`).
- `get_session(session_id) -> dict | None` (`hermes_state_sessions.py:866`).
- `export_session_lineage(session_id, include_compacted=False, include_inactive=False) -> dict | None` и `export_session(...)` (`hermes_state_portability.py:292-316`).
- `import_sessions(sessions: list[dict]) -> {"ok", "imported", "skipped", "detached", "imported_ids", "skipped_ids", "errors"}`: существующие id пропускаются, родитель привязывается, если есть; `model_config` принимается строкой JSON или объектом; `source` сохраняется; FTS — триггерами при вставке (`hermes_state_portability.py:733-776, 580-604, 659-700`); лимиты — `hermes_state.py:506-507`.
- Версия схемы — `hermes_state_common.SCHEMA_VERSION = 31` (`hermes_state_common.py:283`), в базе — `SELECT version FROM schema_version LIMIT 1` (`hermes_state_schema.py:988`).
- Корень и профиль: `hermes_constants.get_default_hermes_root()` (`hermes_constants.py:216-233`), `hermes_cli.profiles.resolve_profile_env(name) -> str` (бросает `FileNotFoundError`/`ValueError`, `hermes_cli/profiles.py:2551-2567`), логика выбора — `hermes_cli/main.py:594-656`.
- Запуск: `[project.scripts] hermes = "hermes_cli.main:main"` (`pyproject.toml:557-560`); опубликованная обёртка и `--run-module` — `hermes_cli/_launchers.py:411-479`; каталог команд — `~/.local/bin` или `%LOCALAPPDATA%\hermes\bin` (`setup-hermes.sh:216-223`).
- `_restore` (`acp_adapter/session.py:446-486`): `source == "acp"`, `cwd` из `model_config`, закрытая сессия переоткрывается.
- Удаление для живого теста: `hermes [--profile P] sessions delete <id> --yes` (`hermes_cli/subcommands/sessions.py:102-104`, `_shared.py:19-21`).

## Review Focus

1. **Белый список и символьные ссылки** (задачи 3–7): ни одна функция экспорта не перечисляет каталоги целиком, кроме каталогов сессии (`projects/<P>/<id>/`, `subagents/<id>/`); ссылки пропускаются, не разыменовываются.
2. **Пути из манифеста** (задачи 1, 4–7, 9): `RelPath` без `..`, абсолютных, `\`, `:`, NUL; имя снимка — без разделителей; импорт проверяет форму каждого пути для своего бэкенда.
3. **Атомарность** (задача 9): снимок виден целиком или не виден; ошибка на любом шаге не оставляет `.tmp-*`.
4. **Порядок восстановления** (задача 11): `load` → проверка каталога → `present` → `import` (с откатом созданных файлов) → только затем одна операция над `Topics` и один `put`.
5. **Ход и снимок не пересекаются** (задача 11): отказ во время хода; во время файловой работы ход и команды смены сессии в этой теме не стартуют.
6. **Hermes** (задача 7): без оболочки, тайм-аут, результат только файлом, схема 31, без перезаписи существующих id.

## Порядок задач

| # | Задача | Крейты |
|---|---|---|
| 1 | Типы снимков: `SnapshotName`, `RelPath`, `BackendData`, `Manifest`, `default_name` | hub-core |
| 2 | Команды `/save`, `/saves`, `/restore`, разбор аргументов, тексты | hub-core, hub-telegram |
| 3 | `SessionArchive`, `ArchiveError`, копирование по белому списку | hub-agent |
| 4 | Архив Claude | hub-claude |
| 5 | Архив Codex | hub-codex |
| 6 | Архив Qwen (переписывание `cwd`) | hub-qwen |
| 7 | Архив Hermes (встроенный Python-скрипт) | hub-hermes, hub-agent |
| 8 | Настройка `[snapshots] dir` и поле GUI | hub-core, hub-app |
| 9 | `SnapshotStore`: атомарная запись, список, загрузка | hub-telegram |
| 10 | `Agents::archive` и `HubAgents` выдают архивы бэкендов | hub-telegram |
| 11 | `Hub`: `/save`, `/saves`, `/restore`, перенос из прежней темы | hub-telegram, hub-app |
| 12 | Живые тесты, README, версия 0.5.0 | все бэкенды, README, Cargo |
| 13 | Ручная проверка на Linux и Windows | — |
| 14 | Релиз: тег `v0.5.0` (push — только после подтверждения) | — |

---
### Task 1: Типы снимков в `hub-core`

Чистый модуль без ввода-вывода: имя снимка, относительный путь, данные бэкенда, манифест версии 1 и имя по умолчанию.

**Files:**
- Create: `crates/hub-core/src/snapshot.rs`
- Modify: `crates/hub-core/src/lib.rs:10-11` (после `pub mod settings;` — `pub mod snapshot;`)
- Test: `crates/hub-core/src/snapshot.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Produces (`hub_core::snapshot`):
  - `MANIFEST_VERSION: u32 = 1`, `NAME_LIMIT: usize = 64`, `FILES_LIMIT: usize = 10_000`.
  - `SnapshotName` (`parse(&str) -> Result<Self, NameError>`, `as_str`, `Display`), `NameError { Empty, TooLong, Dot, Reserved, Character(char) }`.
  - `Minute { year: i32, month: u32, day: u32, hour: u32, minute: u32 }`; `default_name(title: Option<&str>, fallback: &str, at: Minute) -> SnapshotName`.
  - `RelPath` (`parse`, `from_parts(&[&str])`, `as_str`, `parts() -> Split<'_, char>`, `under(&Path) -> PathBuf`, `Display`), `PathError { Shape(String), Character(String) }`.
  - `BackendData { Claude { project: RelPath }, Codex, Qwen, Hermes { profile: Option<String> } }`, `BackendData::backend() -> BackendKind`.
  - `Manifest { name, created_at: String, title: Option<String>, cwd: AbsolutePath, session: SessionId, agent_version: String, data: BackendData, files: Vec<RelPath> }`, `Manifest::parse(&str) -> Result<Self, ManifestError>`, `Manifest::dump(&self) -> Result<String, ManifestError>`.
  - `ManifestError { Json(serde_json::Error), Version(u32), Field { field: &'static str, reason: String }, NonUtf8Path(PathBuf) }`.

- [ ] **Step 1: Тесты (RED)**

`crates/hub-core/src/lib.rs` — добавить строку `pub mod snapshot;` после `pub mod settings;`.

`crates/hub-core/src/snapshot.rs` — пока только тесты (реализация — шаг 3):

```rust
#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rstest::rstest;
    use serde_json::json;

    use super::*;

    fn cwd() -> String {
        if cfg!(windows) { r"C:\work\shop".to_owned() } else { "/work/shop".to_owned() }
    }

    const AT: Minute = Minute { year: 2026, month: 10, day: 9, hour: 15, minute: 30 };

    #[rstest]
    #[case::typical("fix-tests-20261009-1530")]
    #[case::single("a")]
    #[case::punctuation("a.b_c-1")]
    #[case::longest(&"a".repeat(64))]
    #[case::device_prefix("console")]
    fn valid_names_parse(#[case] raw: &str) {
        assert_eq!(SnapshotName::parse(raw).map(|name| name.as_str().to_owned()), Ok(raw.to_owned()));
    }

    #[rstest]
    #[case::empty("", NameError::Empty)]
    #[case::upper("Fix", NameError::Character('F'))]
    #[case::space("a b", NameError::Character(' '))]
    #[case::slash("a/b", NameError::Character('/'))]
    #[case::backslash("a\\b", NameError::Character('\\'))]
    #[case::cyrillic("имя", NameError::Character('и'))]
    #[case::too_long(&"a".repeat(65), NameError::TooLong)]
    #[case::dots("..", NameError::Dot)]
    #[case::hidden(".hidden", NameError::Dot)]
    #[case::trailing("name.", NameError::Dot)]
    #[case::device("con", NameError::Reserved)]
    #[case::device_with_extension("nul.txt", NameError::Reserved)]
    #[case::port("com1", NameError::Reserved)]
    fn invalid_names_are_refused(#[case] raw: &str, #[case] expected: NameError) {
        assert_eq!(SnapshotName::parse(raw), Err(expected));
    }

    #[rstest]
    #[case::cyrillic(Some("Починить тесты"), "claude", "pochinit-testy-20261009-1530")]
    #[case::no_title(None, "claude", "claude-20261009-1530")]
    #[case::symbols_only(Some("  *** "), "codex", "codex-20261009-1530")]
    #[case::mixed(Some("Ёлка: v2.0!"), "qwen", "elka-v2-0-20261009-1530")]
    #[case::digraphs(Some("Щука и Хек"), "hermes", "shchuka-i-khek-20261009-1530")]
    #[case::soft_signs(Some("Объём"), "claude", "obem-20261009-1530")]
    fn default_names_are_transliterated(
        #[case] title: Option<&str>,
        #[case] fallback: &str,
        #[case] expected: &str,
    ) {
        assert_eq!(default_name(title, fallback, AT).as_str(), expected);
    }

    #[test]
    fn long_titles_are_cut_to_the_limit() {
        let name = default_name(Some(&"я".repeat(100)), "claude", AT);
        assert_eq!(SnapshotName::parse(name.as_str()), Ok(name.clone()));
        assert!(name.as_str().len() <= NAME_LIMIT && name.as_str().ends_with("-20261009-1530"));
    }

    #[rstest]
    #[case::nested("projects/-work-shop/abc.jsonl")]
    #[case::single("hermes-session.json")]
    #[case::dotted_name("chats/abc.runtime.json")]
    fn relative_paths_parse(#[case] raw: &str) {
        assert_eq!(RelPath::parse(raw).map(|path| path.as_str().to_owned()), Ok(raw.to_owned()));
    }

    #[rstest]
    #[case::empty("")]
    #[case::absolute("/etc/passwd")]
    #[case::double_slash("a//b")]
    #[case::trailing_slash("a/")]
    #[case::current("a/./b")]
    #[case::parent("a/../b")]
    #[case::only_parent("..")]
    fn malformed_paths_are_refused(#[case] raw: &str) {
        assert_eq!(RelPath::parse(raw), Err(PathError::Shape(raw.to_owned())));
    }

    #[rstest]
    #[case::backslash("a\\..\\b")]
    #[case::drive("C:/Windows")]
    #[case::stream("file.txt:stream")]
    #[case::nul("a\u{0}b")]
    #[case::newline("a\nb")]
    fn paths_with_platform_syntax_are_refused(#[case] raw: &str) {
        assert_eq!(RelPath::parse(raw), Err(PathError::Character(raw.to_owned())));
    }

    #[test]
    fn relative_paths_join_under_a_base() {
        let path = RelPath::from_parts(&["projects", "p", "id.jsonl"]).unwrap();
        assert_eq!(
            path.under(Path::new("root")),
            Path::new("root").join("projects").join("p").join("id.jsonl")
        );
    }

    fn manifest(data: BackendData) -> Manifest {
        Manifest {
            name: SnapshotName::parse("fix-tests-20261009-1530").unwrap(),
            created_at: "2026-10-09T15:30:12Z".to_owned(),
            title: Some("Починить тесты".to_owned()),
            cwd: AbsolutePath::new(PathBuf::from(cwd())).unwrap(),
            session: SessionId::parse("0b1c-id").unwrap(),
            agent_version: "2.1.292".to_owned(),
            data,
            files: vec![RelPath::parse("projects/-work-shop/0b1c-id.jsonl").unwrap()],
        }
    }

    #[rstest]
    #[case::claude(BackendData::Claude { project: RelPath::parse("-work-shop").unwrap() })]
    #[case::codex(BackendData::Codex)]
    #[case::qwen(BackendData::Qwen)]
    #[case::hermes(BackendData::Hermes { profile: Some("work".to_owned()) })]
    #[case::hermes_default(BackendData::Hermes { profile: None })]
    fn manifests_round_trip(#[case] data: BackendData) {
        let original = manifest(data);
        assert_eq!(Manifest::parse(&original.dump().unwrap()).unwrap(), original);
    }

    #[test]
    fn manifest_shape_matches_the_spec() {
        let text = manifest(BackendData::Claude { project: RelPath::parse("-work-shop").unwrap() })
            .dump()
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value,
            json!({
                "version": 1,
                "name": "fix-tests-20261009-1530",
                "created_at": "2026-10-09T15:30:12Z",
                "title": "Починить тесты",
                "backend": "claude",
                "cwd": cwd(),
                "session_id": "0b1c-id",
                "agent_version": "2.1.292",
                "backend_data": {"kind": "claude", "project": "-work-shop"},
                "files": ["projects/-work-shop/0b1c-id.jsonl"],
            })
        );
    }

    fn raw(overrides: serde_json::Value) -> String {
        let mut base = json!({
            "version": 1, "name": "snap", "created_at": "2026-10-09T15:30:12Z", "title": null,
            "backend": "codex", "cwd": cwd(), "session_id": "t-1", "agent_version": "0.160.0",
            "backend_data": {"kind": "codex"}, "files": ["sessions/2026/10/09/rollout-x-t-1.jsonl"],
        });
        if let (Some(base), Some(overrides)) = (base.as_object_mut(), overrides.as_object()) {
            base.extend(overrides.clone());
        }
        base.to_string()
    }

    #[test]
    fn minimal_manifest_parses_without_a_title() {
        let parsed = Manifest::parse(&raw(json!({}))).unwrap();
        assert_eq!((parsed.title, parsed.data), (None, BackendData::Codex));
    }

    #[test]
    fn unknown_manifest_version_is_reported_before_the_shape() {
        assert!(matches!(Manifest::parse(r#"{"version": 2}"#), Err(ManifestError::Version(2))));
    }

    #[rstest]
    #[case::backend_mismatch(json!({"backend": "claude"}), "backend")]
    #[case::unknown_backend(json!({"backend": "gpt"}), "backend")]
    #[case::bad_name(json!({"name": "../x"}), "name")]
    #[case::relative_cwd(json!({"cwd": "shop"}), "cwd")]
    #[case::empty_session(json!({"session_id": " "}), "session_id")]
    #[case::traversal(json!({"files": ["../../.ssh/id_ed25519"]}), "files")]
    #[case::absolute_file(json!({"files": ["/etc/passwd"]}), "files")]
    #[case::nested_project(
        json!({"backend": "claude", "backend_data": {"kind": "claude", "project": "a/b"}}),
        "backend_data.project"
    )]
    fn malformed_manifests_name_the_field(
        #[case] overrides: serde_json::Value,
        #[case] expected: &str,
    ) {
        assert!(matches!(
            Manifest::parse(&raw(overrides)),
            Err(ManifestError::Field { field, .. }) if field == expected
        ));
    }

    #[test]
    fn too_many_files_are_refused() {
        let files: Vec<String> = (0..=FILES_LIMIT).map(|i| format!("f{i}")).collect();
        assert!(matches!(
            Manifest::parse(&raw(json!({ "files": files }))),
            Err(ManifestError::Field { field: "files", .. })
        ));
    }
}
```

`crates/hub-core/Cargo.toml` — без изменений (`serde`, `serde_json`, `thiserror`, `rstest` уже есть; `serde_json::json!` доступен).

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-core --lib -- snapshot`
Expected: FAIL — ошибки компиляции `cannot find type SnapshotName`/`Manifest`/`RelPath` и функции `default_name` в `snapshot`.

- [ ] **Step 3: Реализация**

Начало `crates/hub-core/src/snapshot.rs` (над тестами):

```rust
//! Session snapshots: names, agent-relative paths and `manifest.json` format version 1.
//! Pure: the store and the archives do the I/O.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::{AbsolutePath, BackendKind, SessionId};

pub const MANIFEST_VERSION: u32 = 1;
pub const NAME_LIMIT: usize = 64;
pub const FILES_LIMIT: usize = 10_000;
/// Device names Windows reserves for any extension: such directories cannot be created there.
const RESERVED: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("Имя снимка пустое")]
    Empty,
    #[error("Имя снимка длиннее {NAME_LIMIT} символов")]
    TooLong,
    #[error("Имя снимка не может начинаться или заканчиваться точкой")]
    Dot,
    #[error("Имя снимка зарезервировано в Windows")]
    Reserved,
    #[error("В имени снимка допустимы a-z, 0-9, «.», «_» и «-»; встретился «{0}»")]
    Character(char),
}

/// A snapshot directory name: `[a-z0-9._-]{1,64}`, no leading or trailing dot, no device name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SnapshotName(String);

impl SnapshotName {
    pub fn parse(raw: &str) -> Result<Self, NameError> {
        if raw.is_empty() {
            return Err(NameError::Empty);
        }
        if let Some(bad) =
            raw.chars().find(|c| !matches!(c, 'a'..='z' | '0'..='9' | '.' | '_' | '-'))
        {
            return Err(NameError::Character(bad));
        }
        // Only ASCII is left, so bytes are characters.
        if raw.len() > NAME_LIMIT {
            return Err(NameError::TooLong);
        }
        if raw.starts_with('.') || raw.ends_with('.') {
            return Err(NameError::Dot);
        }
        if RESERVED.contains(&raw.split('.').next().unwrap_or(raw)) {
            return Err(NameError::Reserved);
        }
        Ok(Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SnapshotName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The local wall-clock minute a default name is stamped with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Minute {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
}

/// `<title>-YYYYMMDD-HHMM`: Cyrillic transliterated, any other run outside `[a-z0-9]` becomes
/// one `-`; a title that leaves nothing falls back to `fallback` (the backend name).
#[must_use]
pub fn default_name(title: Option<&str>, fallback: &str, at: Minute) -> SnapshotName {
    let Minute { year, month, day, hour, minute } = at;
    let suffix = format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}");
    let room = NAME_LIMIT.saturating_sub(suffix.len() + 1);
    let slug = [title.unwrap_or_default(), fallback]
        .into_iter()
        .map(|text| slug(text, room))
        .find(|slug| !slug.is_empty())
        .unwrap_or_default();
    // Only [a-z0-9-], ends with a digit, no dot and at most NAME_LIMIT long: a valid name.
    SnapshotName(if slug.is_empty() { suffix } else { format!("{slug}-{suffix}") })
}

fn slug(text: &str, room: usize) -> String {
    let raw = text.chars().flat_map(char::to_lowercase).fold(String::new(), |mut out, c| {
        match latin(c) {
            Some(latin) => out.push_str(latin),
            None if c.is_ascii_lowercase() || c.is_ascii_digit() => out.push(c),
            None if out.ends_with('-') => {}
            None => out.push('-'),
        }
        out
    });
    let cut: String = raw.trim_start_matches('-').chars().take(room).collect();
    cut.trim_end_matches('-').to_owned()
}

/// Russian letters in the common passport-style transliteration.
const fn latin(c: char) -> Option<&'static str> {
    Some(match c {
        'а' => "a",
        'б' => "b",
        'в' => "v",
        'г' => "g",
        'д' => "d",
        'е' | 'ё' | 'э' => "e",
        'ж' => "zh",
        'з' => "z",
        'и' => "i",
        'й' | 'ы' => "y",
        'к' => "k",
        'л' => "l",
        'м' => "m",
        'н' => "n",
        'о' => "o",
        'п' => "p",
        'р' => "r",
        'с' => "s",
        'т' => "t",
        'у' => "u",
        'ф' => "f",
        'х' => "kh",
        'ц' => "ts",
        'ч' => "ch",
        'ш' => "sh",
        'щ' => "shch",
        'ъ' | 'ь' => "",
        'ю' => "yu",
        'я' => "ya",
        _ => return None,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("путь «{0}» должен быть относительным, без пустых частей, «.» и «..»")]
    Shape(String),
    #[error("путь «{0}» содержит «\\», «:» или управляющий символ")]
    Character(String),
}

/// A `/`-separated path inside a snapshot's `agent/` or an agent's storage root.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

impl RelPath {
    pub fn parse(raw: &str) -> Result<Self, PathError> {
        if raw.chars().any(|c| c == '\\' || c == ':' || c.is_control()) {
            return Err(PathError::Character(raw.to_owned()));
        }
        if raw.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
            return Err(PathError::Shape(raw.to_owned()));
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn from_parts(parts: &[&str]) -> Result<Self, PathError> {
        Self::parse(&parts.join("/"))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn parts(&self) -> std::str::Split<'_, char> {
        self.0.split('/')
    }

    #[must_use]
    pub fn under(&self, base: &Path) -> PathBuf {
        self.parts().fold(base.to_path_buf(), |path, part| path.join(part))
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a backend needs, besides the files, to put a session back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendData {
    /// The `projects/<P>` directory Claude kept the transcript in (one path component).
    Claude { project: RelPath },
    Codex,
    Qwen,
    /// The profile the session was exported from; `None` means Hermes's own choice.
    Hermes { profile: Option<String> },
}

impl BackendData {
    #[must_use]
    pub const fn backend(&self) -> BackendKind {
        match self {
            Self::Claude { .. } => BackendKind::Claude,
            Self::Codex => BackendKind::Codex,
            Self::Qwen => BackendKind::Qwen,
            Self::Hermes { .. } => BackendKind::Hermes,
        }
    }
}

/// `manifest.json` of one snapshot; the backend is `data.backend()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub name: SnapshotName,
    /// UTC `YYYY-MM-DDTHH:MM:SSZ`: sorts chronologically as text.
    pub created_at: String,
    pub title: Option<String>,
    pub cwd: AbsolutePath,
    pub session: SessionId,
    pub agent_version: String,
    pub data: BackendData,
    /// Paths inside the snapshot's `agent/` directory.
    pub files: Vec<RelPath>,
}

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest.json не является JSON нужной формы: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ожидалась версия манифеста {MANIFEST_VERSION}, найдена {0}")]
    Version(u32),
    #[error("поле {field}: {reason}")]
    Field { field: &'static str, reason: String },
    #[error("путь {} не в UTF-8", .0.display())]
    NonUtf8Path(PathBuf),
}

#[derive(Deserialize)]
struct Versioned {
    version: u32,
}

#[derive(Serialize, Deserialize)]
struct ManifestFile {
    version: u32,
    name: String,
    created_at: String,
    title: Option<String>,
    backend: String,
    cwd: String,
    session_id: String,
    agent_version: String,
    backend_data: DataFile,
    files: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum DataFile {
    Claude { project: String },
    Codex {},
    Qwen {},
    Hermes { profile: Option<String> },
}

fn bad(field: &'static str, reason: impl Into<String>) -> ManifestError {
    ManifestError::Field { field, reason: reason.into() }
}

impl Manifest {
    /// The version is checked first, so a newer format is reported as such, not as garbage.
    pub fn parse(raw: &str) -> Result<Self, ManifestError> {
        let Versioned { version } = serde_json::from_str(raw)?;
        if version != MANIFEST_VERSION {
            return Err(ManifestError::Version(version));
        }
        let ManifestFile {
            version: _version,
            name,
            created_at,
            title,
            backend,
            cwd,
            session_id,
            agent_version,
            backend_data,
            files,
        } = serde_json::from_str(raw)?;
        let name = SnapshotName::parse(&name).map_err(|error| bad("name", error.to_string()))?;
        let cwd = AbsolutePath::new(PathBuf::from(cwd))
            .ok_or_else(|| bad("cwd", "нужен абсолютный путь"))?;
        let session = SessionId::parse(&session_id).ok_or_else(|| bad("session_id", "пусто"))?;
        let data = match backend_data {
            DataFile::Claude { project } => {
                let project = RelPath::parse(&project)
                    .map_err(|error| bad("backend_data.project", error.to_string()))?;
                if project.parts().count() != 1 {
                    return Err(bad("backend_data.project", "ожидалось одно имя каталога"));
                }
                BackendData::Claude { project }
            }
            DataFile::Codex {} => BackendData::Codex,
            DataFile::Qwen {} => BackendData::Qwen,
            DataFile::Hermes { profile } => BackendData::Hermes { profile },
        };
        if BackendKind::parse(&backend) != Some(data.backend()) {
            return Err(bad("backend", format!("«{backend}» не совпадает с backend_data")));
        }
        if files.len() > FILES_LIMIT {
            return Err(bad("files", format!("больше {FILES_LIMIT} файлов")));
        }
        let files = files
            .iter()
            .map(|file| RelPath::parse(file))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| bad("files", error.to_string()))?;
        Ok(Self { name, created_at, title, cwd, session, agent_version, data, files })
    }

    pub fn dump(&self) -> Result<String, ManifestError> {
        let Self { name, created_at, title, cwd, session, agent_version, data, files } = self;
        let path = cwd.as_path();
        let file = ManifestFile {
            version: MANIFEST_VERSION,
            name: name.as_str().to_owned(),
            created_at: created_at.clone(),
            title: title.clone(),
            backend: data.backend().name().to_owned(),
            cwd: path
                .to_str()
                .ok_or_else(|| ManifestError::NonUtf8Path(path.to_path_buf()))?
                .to_owned(),
            session_id: session.as_str().to_owned(),
            agent_version: agent_version.clone(),
            backend_data: match data {
                BackendData::Claude { project } => {
                    DataFile::Claude { project: project.as_str().to_owned() }
                }
                BackendData::Codex => DataFile::Codex {},
                BackendData::Qwen => DataFile::Qwen {},
                BackendData::Hermes { profile } => DataFile::Hermes { profile: profile.clone() },
            },
            files: files.iter().map(|file| file.as_str().to_owned()).collect(),
        };
        Ok(serde_json::to_string_pretty(&file)?)
    }
}
```

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-core --lib -- snapshot`
Expected: PASS (все тесты модуля `snapshot`).

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS без предупреждений. Если `clippy::pedantic` просит иное (например, `needless_pass_by_value` для `bad`), — поправить по линту.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-core/src/snapshot.rs crates/hub-core/src/lib.rs
git commit -m "hub-core: имена снимков, относительные пути и манифест"
```

---

### Task 2: Команды `/save`, `/saves`, `/restore` и тексты

Команды появляются в реестре (`/help` и меню бота строятся из `Command::ALL`), разбор аргументов — чистый. Хаб пока отвечает временным текстом; настоящая обработка — задача 11.

**Files:**
- Modify: `crates/hub-core/src/commands.rs:3` (импорт), `:5-14` (enum), `:17-18` (`ALL`), `:20-55` (`name`, `description`, `arguments`), после `:141` (`join_path_args`) — новые функции, тесты `:150-280`
- Modify: `crates/hub-telegram/src/texts.rs:230-261` (импорты и константы), после `:350` (`outside_root`) — новые функции, тесты `:352-443`
- Modify: `crates/hub-telegram/src/hub.rs:363-379` (временная ветка в `command`)

**Interfaces:**
- Produces (`hub_core::commands`): `Command::{Save, Saves, Restore}`; `Command::ALL: [Self; 10]` = `[New, Backend, Cwd, Reset, Stop, Status, Save, Saves, Restore, Help]`; `SaveArgs { Default, Named(SnapshotName) }`, `parse_save_args(&[&str]) -> Result<SaveArgs, NameError>`; `RestoreArgs { name: SnapshotName, cwd: Option<String> }`, `parse_restore_args(&[&str]) -> Result<Option<RestoreArgs>, NameError>`.
- Produces (`hub_telegram::texts`): `NOTHING_TO_SAVE`, `NO_SNAPSHOTS`, `SESSION_MOVED`, `FINISH_TURN_FIRST`, `RESTORE_USAGE`, `SNAPSHOT_BUSY`, `SESSION_BUSY_ELSEWHERE`, `SNAPSHOTS_PENDING` (временная, удаляется в задаче 11); `saved(&Manifest) -> String`, `restored(&SnapshotName, &TopicSession) -> String`, `snapshot_list(&[Manifest]) -> String`.
- Consumes: `hub_core::snapshot::{SnapshotName, NameError, Manifest}` (задача 1).

- [ ] **Step 1: Тесты (RED)**

`crates/hub-core/src/commands.rs`, модуль `tests`: в `menu_registry_is_unique_valid_and_parsed` (`:150-161`) заменить `assert_eq!(names.len(), 7);` на `assert_eq!(names.len(), 10);` и добавить:

```rust
    #[test]
    fn snapshot_commands_sit_before_help() {
        let names: Vec<_> = Command::ALL.into_iter().map(Command::name).collect();
        assert_eq!(
            names,
            ["new", "backend", "cwd", "reset", "stop", "status", "save", "saves", "restore", "help"]
        );
    }

    #[rstest]
    #[case::default(&[], Ok(SaveArgs::Default))]
    #[case::named(&["snap-1"], Ok(SaveArgs::Named(SnapshotName::parse("snap-1").unwrap())))]
    #[case::spaced(&["my", "snap"], Err(NameError::Character(' ')))]
    #[case::upper(&["Snap"], Err(NameError::Character('S')))]
    #[case::traversal(&["../x"], Err(NameError::Character('/')))]
    fn save_args_are_one_valid_name(
        #[case] args: &[&str],
        #[case] expected: Result<SaveArgs, NameError>,
    ) {
        assert_eq!(parse_save_args(args), expected);
    }

    fn restore(name: &str, cwd: Option<&str>) -> RestoreArgs {
        RestoreArgs { name: SnapshotName::parse(name).unwrap(), cwd: cwd.map(str::to_owned) }
    }

    #[rstest]
    #[case::usage(&[], Ok(None))]
    #[case::name_only(&["snap"], Ok(Some(restore("snap", None))))]
    #[case::with_dir(&["snap", "my", "dir"], Ok(Some(restore("snap", Some("my dir")))))]
    #[case::bad_name(&["Snap", "dir"], Err(NameError::Character('S')))]
    fn restore_args_are_a_name_and_an_optional_path(
        #[case] args: &[&str],
        #[case] expected: Result<Option<RestoreArgs>, NameError>,
    ) {
        assert_eq!(parse_restore_args(args), expected);
    }
```

и в таблицу `input_is_classified` (`:262-273`) три случая:

```rust
    #[case("/save snap", Input::Command { command: Command::Save, args: vec!["snap"] })]
    #[case("/saves", Input::Command { command: Command::Saves, args: vec![] })]
    #[case("/restore snap shop", Input::Command { command: Command::Restore, args: vec!["snap", "shop"] })]
```

`use` в тестах: `use crate::snapshot::{NameError, SnapshotName};`.

`crates/hub-telegram/src/texts.rs`, модуль `tests` — в `help_names_root_uploads_and_backends` (`:375-392`) после проверки `/backend`:

```rust
        assert!(text.contains("/save [имя] — сохранить снимок сессии этой темы"));
        assert!(text.contains("/saves — список снимков"));
        assert!(text.contains("/restore <имя> [каталог] — продолжить сессию из снимка в этой теме"));
```

и новые тесты:

```rust
    fn manifest(name: &str, created_at: &str, title: Option<&str>) -> Manifest {
        Manifest {
            name: SnapshotName::parse(name).unwrap(),
            created_at: created_at.to_owned(),
            title: title.map(str::to_owned),
            cwd: absolute("shop"),
            session: SessionId::parse("s-1").unwrap(),
            agent_version: "2.1.292".to_owned(),
            data: BackendData::Codex,
            files: Vec::new(),
        }
    }

    #[test]
    fn saved_and_restored_name_backend_and_directory() {
        let shop = absolute("shop").as_path().display().to_string();
        let snapshot = manifest("snap", "2026-10-09T15:30:12Z", None);
        assert_eq!(saved(&snapshot), format!("💾 Сохранено: snap · codex · {shop}"));
        let session = TopicSession::fresh(BackendKind::Codex, absolute("shop"));
        assert_eq!(
            restored(&snapshot.name, &session),
            format!("♻️ Восстановлено: snap · codex · {shop}")
        );
    }

    #[test]
    fn snapshot_list_shows_up_to_thirty_with_titles() {
        assert_eq!(snapshot_list(&[]), NO_SNAPSHOTS);
        let shop = absolute("shop").as_path().display().to_string();
        let titled = [manifest("a", "2026-10-09T15:30:12Z", Some("Починить тесты"))];
        assert_eq!(
            snapshot_list(&titled),
            format!(
                "💾 Снимки, новые сверху:\na · codex · {shop} · 2026-10-09T15:30:12Z · Починить тесты"
            )
        );
        let many: Vec<_> =
            (0..31).map(|i| manifest(&format!("s{i}"), "2026-10-09T15:30:12Z", None)).collect();
        let text = snapshot_list(&many);
        assert_eq!(text.lines().count(), 31);
        assert!(text.lines().last().is_some_and(|line| line.starts_with("s29 · ") && line.ends_with(" · —")));
    }
```

`use` в тестах `texts.rs`: `use hub_core::snapshot::{BackendData, Manifest, SnapshotName};` (`SessionId`, `BackendKind` уже импортированы, `:357`).

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-core --lib -- commands && cargo test -p hub-telegram --lib -- texts`
Expected: FAIL — ошибки компиляции: нет `Command::Save`, `parse_save_args`, `RestoreArgs`, `texts::saved`, `snapshot_list`.

- [ ] **Step 3: Реализация**

`crates/hub-core/src/commands.rs`:

```rust
use crate::domain::{BackendKind, TopicSession};
use crate::snapshot::{NameError, SnapshotName};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Help,
    New,
    Cwd,
    Reset,
    Stop,
    Status,
    Backend,
    Save,
    Saves,
    Restore,
}

impl Command {
    pub const ALL: [Self; 10] = [
        Self::New,
        Self::Backend,
        Self::Cwd,
        Self::Reset,
        Self::Stop,
        Self::Status,
        Self::Save,
        Self::Saves,
        Self::Restore,
        Self::Help,
    ];
```

В `name`: `Self::Save => "save"`, `Self::Saves => "saves"`, `Self::Restore => "restore"`. В `description`: `Self::Save => "сохранить снимок сессии этой темы"`, `Self::Saves => "список снимков"`, `Self::Restore => "продолжить сессию из снимка в этой теме"`. В `arguments`: `Self::Save => " [имя]"`, `Self::Restore => " <имя> [каталог]"`, а `Self::Saves` — в общую ветку с `""`: `Self::Help | Self::Reset | Self::Stop | Self::Status | Self::Saves => ""`.

После `join_path_args`:

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveArgs {
    Default,
    Named(SnapshotName),
}

/// `/save [name]`; several words are one name with spaces, which the name check refuses.
pub fn parse_save_args(args: &[&str]) -> Result<SaveArgs, NameError> {
    match args {
        [] => Ok(SaveArgs::Default),
        [_, ..] => SnapshotName::parse(&args.join(" ")).map(SaveArgs::Named),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreArgs {
    pub name: SnapshotName,
    pub cwd: Option<String>,
}

/// `/restore <name> [path]`; `Ok(None)` when the name is missing.
pub fn parse_restore_args(args: &[&str]) -> Result<Option<RestoreArgs>, NameError> {
    match args.split_first() {
        None => Ok(None),
        Some((name, rest)) => Ok(Some(RestoreArgs {
            name: SnapshotName::parse(name)?,
            cwd: join_path_args(rest),
        })),
    }
}
```

`crates/hub-telegram/src/texts.rs` — импорты:

```rust
use hub_core::snapshot::{Manifest, SnapshotName};
```

константы после `BACKEND_SWITCHED` (`:258`):

```rust
pub const NOTHING_TO_SAVE: &str = "Нечего сохранять: в теме ещё нет сессии";
pub const NO_SNAPSHOTS: &str = "Снимков пока нет";
pub const SESSION_MOVED: &str = "Сессия перенесена в другую тему";
pub const FINISH_TURN_FIRST: &str = "Дождитесь конца хода или /stop";
pub const RESTORE_USAGE: &str = "Укажите снимок: /restore <имя> [каталог]";
pub const SNAPSHOT_BUSY: &str =
    "⏳ Снимок этой темы ещё сохраняется или восстанавливается — подождите.";
pub const SESSION_BUSY_ELSEWHERE: &str = "⚠️ Сессия сейчас выполняет ход в другой теме — \
                                          дождитесь его конца или /stop там и повторите /restore";
/// Until the hub handles snapshots (removed in the task that wires them).
pub const SNAPSHOTS_PENDING: &str = "Снимки сессий ещё не подключены";
const SNAPSHOT_LIST_LIMIT: usize = 30;
const SNAPSHOT_LIST_TEXT_LIMIT: usize = 3500;
```

функции после `outside_root`:

```rust
#[must_use]
pub fn saved(manifest: &Manifest) -> String {
    format!(
        "💾 Сохранено: {} · {} · {}",
        manifest.name,
        manifest.data.backend().name(),
        manifest.cwd.as_path().display()
    )
}

#[must_use]
pub fn restored(name: &SnapshotName, session: &TopicSession) -> String {
    format!(
        "♻️ Восстановлено: {name} · {} · {}",
        session.backend.name(),
        session.cwd.as_path().display()
    )
}

/// Newest first is the caller's order; at most 30 lines.
#[must_use]
pub fn snapshot_list(manifests: &[Manifest]) -> String {
    if manifests.is_empty() {
        return NO_SNAPSHOTS.to_owned();
    }
    let lines = manifests
        .iter()
        .take(SNAPSHOT_LIST_LIMIT)
        .map(|manifest| {
            format!(
                "{} · {} · {} · {} · {}",
                manifest.name,
                manifest.data.backend().name(),
                manifest.cwd.as_path().display(),
                manifest.created_at,
                manifest.title.as_deref().unwrap_or("—")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    truncate(&format!("💾 Снимки, новые сверху:\n{lines}"), SNAPSHOT_LIST_TEXT_LIMIT)
}
```

`crates/hub-telegram/src/hub.rs`, в `command` после ветки `Command::Status` (`:368-378`):

```rust
            Command::Save | Command::Saves | Command::Restore => {
                self.say(Target::Topic(key), texts::SNAPSHOTS_PENDING.to_owned());
            }
```

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-core --lib -- commands && cargo test -p hub-telegram --lib -- texts && cargo test -p hub-telegram --test menu`
Expected: PASS; тест меню (`tests/menu/registration.rs:171`) строит ожидание из `Command::ALL` и проходит с 10 командами.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-core/src/commands.rs crates/hub-telegram/src/texts.rs crates/hub-telegram/src/hub.rs
git commit -m "Команды /save, /saves, /restore в реестре и их тексты"
```

---

### Task 3: `SessionArchive` и копирование по белому списку (`hub-agent`)

Общий контракт бэкендов и помощники, на которых держатся гарантии безопасности: только перечисленные обычные файлы, ссылки не разыменовываются, файлы снимка приватные, файлы агента не перезаписываются, неудачный возврат откатывается.

**Files:**
- Create: `crates/hub-agent/src/archive.rs`
- Modify: `crates/hub-agent/src/lib.rs:1` (`pub mod archive;` первой строкой)
- Modify: `crates/hub-agent/Cargo.toml:24-26` (`[dev-dependencies]`: `tempfile.workspace = true`), `Cargo.lock`
- Create: `crates/hub-agent/tests/archive.rs`, `crates/hub-agent/tests/archive/copy.rs`
- Test: `crates/hub-agent/src/archive.rs` (`#[cfg(test)]`, `file_stem`), `crates/hub-agent/tests/archive/copy.rs`

**Interfaces:**
- Produces (`hub_agent::archive`):
  - `pub struct Exported { pub files: Vec<RelPath>, pub data: BackendData, pub agent_version: String }`.
  - `pub enum ArchiveError { NotFound, Ambiguous, Archived(String), Unsupported(String), Io(io::Error), Malformed(String) }` (`thiserror`, `Io` — `#[from]`), `impl From<PathError> for ArchiveError` (→ `Malformed`).
  - `pub trait SessionArchive: Send + Sync` с `export(&self, session: &SessionId, cwd: &AbsolutePath, dest: &Path) -> Result<Exported, ArchiveError>`, `present(&self, session: &SessionId, cwd: &AbsolutePath, data: &BackendData) -> Result<bool, ArchiveError>`, `import(&self, session: &SessionId, data: &BackendData, files: &[RelPath], src: &Path, cwd: &AbsolutePath) -> Result<(), ArchiveError>`. Все три — блокирующие.
  - `file_stem(&SessionId) -> Result<&str, ArchiveError>`; `private_dirs(&Path) -> io::Result<()>`; `private_file(&Path) -> io::Result<File>`; `restrict_dir(&Path) -> io::Result<()>`; `is_regular_file(&Path) -> io::Result<bool>`; `is_real_dir(&Path) -> io::Result<bool>`; `collect_tree(root: &Path, dir: &RelPath) -> Result<Vec<RelPath>, ArchiveError>`; `export_files(root: &Path, files: &[RelPath], dest: &Path) -> Result<(), ArchiveError>`; `place_all(pairs: &[(PathBuf, PathBuf)], write: impl FnMut(&Path, &mut File) -> io::Result<()>) -> Result<(), ArchiveError>`; `copy_plain(from: &Path, out: &mut File) -> io::Result<()>`.
- Consumes: `hub_core::snapshot::{BackendData, RelPath}` (задача 1).

- [ ] **Step 1: Тесты (RED)**

`crates/hub-agent/Cargo.toml`, `[dev-dependencies]`: добавить `tempfile.workspace = true`. Затем `cargo check -p hub-agent --all-targets` (без `--locked`) и `git diff --stat Cargo.lock` — изменён только список зависимостей `hub-agent`.

`crates/hub-agent/src/lib.rs`: первой строкой `pub mod archive;`.

`crates/hub-agent/tests/archive.rs`:

```rust
#![allow(clippy::unwrap_used)]
#[path = "archive/copy.rs"]
mod copy;
```

`crates/hub-agent/tests/archive/copy.rs`:

```rust
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use hub_agent::archive::{ArchiveError, collect_tree, copy_plain, export_files, place_all};
use hub_core::snapshot::RelPath;

fn rel(raw: &str) -> RelPath {
    RelPath::parse(raw).unwrap()
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn listed_files_are_copied_under_the_same_paths() {
    let (root, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&rel("projects/p/id.jsonl").under(root.path()), "{}\n");
    write(&root.path().join(".credentials.json"), "secret");

    export_files(root.path(), &[rel("projects/p/id.jsonl")], dest.path()).unwrap();

    assert_eq!(fs::read_to_string(rel("projects/p/id.jsonl").under(dest.path())).unwrap(), "{}\n");
    assert!(!dest.path().join(".credentials.json").exists());
}

#[cfg(unix)]
#[test]
fn exported_files_and_directories_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let (root, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&rel("projects/p/id.jsonl").under(root.path()), "{}\n");
    export_files(root.path(), &[rel("projects/p/id.jsonl")], dest.path()).unwrap();
    let mode = |raw: &str| fs::metadata(rel(raw).under(dest.path())).unwrap().permissions().mode() & 0o777;
    assert_eq!((mode("projects"), mode("projects/p"), mode("projects/p/id.jsonl")), (0o700, 0o700, 0o600));
}

#[cfg(unix)]
#[test]
fn a_linked_source_is_refused_not_followed() {
    let (root, dest, outside) =
        (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&outside.path().join("key"), "secret");
    fs::create_dir_all(root.path().join("projects").join("p")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("key"), rel("projects/p/id.jsonl").under(root.path()))
        .unwrap();

    let result = export_files(root.path(), &[rel("projects/p/id.jsonl")], dest.path());

    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    assert!(!rel("projects/p/id.jsonl").under(dest.path()).exists());
}

#[test]
fn trees_list_regular_files_sorted_and_missing_trees_are_empty() {
    let root = tempfile::tempdir().unwrap();
    write(&rel("s/id/b.txt").under(root.path()), "b");
    write(&rel("s/id/a/c.txt").under(root.path()), "c");
    assert_eq!(collect_tree(root.path(), &rel("s/id")).unwrap(), [rel("s/id/a/c.txt"), rel("s/id/b.txt")]);
    assert_eq!(collect_tree(root.path(), &rel("s/missing")).unwrap(), Vec::<RelPath>::new());
}

#[cfg(unix)]
#[test]
fn trees_skip_links_to_files_and_directories() {
    let (root, outside) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&outside.path().join("secret"), "x");
    write(&rel("s/id/a.txt").under(root.path()), "a");
    std::os::unix::fs::symlink(outside.path().join("secret"), rel("s/id/file-link").under(root.path()))
        .unwrap();
    std::os::unix::fs::symlink(outside.path(), rel("s/id/dir-link").under(root.path())).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).unwrap();

    assert_eq!(collect_tree(root.path(), &rel("s/id")).unwrap(), [rel("s/id/a.txt")]);
    assert_eq!(collect_tree(root.path(), &rel("linked")).unwrap(), Vec::<RelPath>::new());
}

#[test]
fn placing_never_overwrites_and_rolls_back() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "new a");
    write(&src.path().join("b"), "new b");
    write(&rel("x/b").under(dest.path()), "old b");
    let pairs = [
        (src.path().join("a"), rel("x/a").under(dest.path())),
        (src.path().join("b"), rel("x/b").under(dest.path())),
    ];

    let result = place_all(&pairs, copy_plain);

    assert!(matches!(result, Err(ArchiveError::Io(ref error)) if error.kind() == io::ErrorKind::AlreadyExists));
    assert!(!rel("x/a").under(dest.path()).exists());
    assert_eq!(fs::read_to_string(rel("x/b").under(dest.path())).unwrap(), "old b");
}

#[test]
fn a_failed_write_leaves_nothing_behind() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "a");
    write(&src.path().join("b"), "b");
    let pairs = [
        (src.path().join("a"), rel("x/a").under(dest.path())),
        (src.path().join("b"), rel("x/b").under(dest.path())),
    ];

    let result = place_all(&pairs, |from, out| {
        if from.ends_with("b") { Err(io::Error::other("boom")) } else { copy_plain(from, out) }
    });

    assert!(matches!(result, Err(ArchiveError::Io(_))));
    assert!(!rel("x/a").under(dest.path()).exists() && !rel("x/b").under(dest.path()).exists());
}

#[test]
fn placed_files_carry_what_the_writer_wrote() {
    let (src, dest) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(&src.path().join("a"), "abc");
    let pairs = [(src.path().join("a"), rel("y/a").under(dest.path()))];
    place_all(&pairs, |from, out| out.write_all(fs::read_to_string(from)?.to_uppercase().as_bytes()))
        .unwrap();
    assert_eq!(fs::read_to_string(rel("y/a").under(dest.path())).unwrap(), "ABC");
}
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-agent --test archive`
Expected: FAIL — `unresolved import hub_agent::archive` (модуль пуст/нет символов).

- [ ] **Step 3: Реализация**

`crates/hub-agent/src/archive.rs`:

```rust
//! Moving one agent session's own history into a snapshot and back.
//!
//! Archives block (plain `std::fs`, child processes): call them from `spawn_blocking`.
//! Only what a backend lists is copied, links are never followed, snapshot files are private,
//! and nothing already in the agent's storage is overwritten.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, PathError, RelPath};

const ID_LIMIT: usize = 128;
const TREE_DEPTH: usize = 16;
const TREE_ENTRIES: usize = 100_000;

/// What `export` put into the snapshot's `agent/` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exported {
    pub files: Vec<RelPath>,
    pub data: BackendData,
    pub agent_version: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("История сессии не найдена у агента")]
    NotFound,
    #[error("У агента несколько копий этой сессии — оставьте одну")]
    Ambiguous,
    #[error("Сессия в архиве Codex: выполните `codex unarchive {0}`")]
    Archived(String),
    #[error("{0}")]
    Unsupported(String),
    #[error("Файлы агента недоступны: {0}")]
    Io(#[from] io::Error),
    #[error("Данные снимка повреждены: {0}")]
    Malformed(String),
}

/// A path that cannot be a `RelPath` came from the snapshot or the agent's storage.
impl From<PathError> for ArchiveError {
    fn from(error: PathError) -> Self {
        Self::Malformed(error.to_string())
    }
}

/// One backend's way to copy a session's history out of its storage and back.
pub trait SessionArchive: Send + Sync {
    /// Copies the session's files into `dest` (the snapshot's empty `agent/`).
    fn export(
        &self,
        session: &SessionId,
        cwd: &AbsolutePath,
        dest: &Path,
    ) -> Result<Exported, ArchiveError>;

    /// Whether the agent can already resume `session` in `cwd`; then nothing is imported.
    fn present(
        &self,
        session: &SessionId,
        cwd: &AbsolutePath,
        data: &BackendData,
    ) -> Result<bool, ArchiveError>;

    /// Puts `files` from `src` (the snapshot's `agent/`) back for `cwd`; all or nothing.
    fn import(
        &self,
        session: &SessionId,
        data: &BackendData,
        files: &[RelPath],
        src: &Path,
        cwd: &AbsolutePath,
    ) -> Result<(), ArchiveError>;
}

/// The id as a file-name stem; agents mint UUID-like ids, anything else could leave a directory.
pub fn file_stem(session: &SessionId) -> Result<&str, ArchiveError> {
    let id = session.as_str();
    let safe = id.len() <= ID_LIMIT
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_');
    if safe {
        Ok(id)
    } else {
        Err(ArchiveError::Malformed("id сессии нельзя использовать как имя файла".to_owned()))
    }
}

/// Creates `path` and its missing parents; new directories are `0700` on Unix.
pub fn private_dirs(path: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// A new file — never an existing one or a link — readable only by the owner on Unix.
pub fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// `0700` on an existing directory.
#[cfg(unix)]
pub fn restrict_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

/// Windows ACLs already follow the user profile; nothing to narrow.
#[cfg(not(unix))]
pub fn restrict_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Whether `path` itself is a regular file (a link to one is not).
pub fn is_regular_file(path: &Path) -> io::Result<bool> {
    kind(path).map(|kind| kind.is_some_and(|kind| kind.is_file()))
}

/// Whether `path` itself is a directory (a link or junction to one is not).
pub fn is_real_dir(path: &Path) -> io::Result<bool> {
    kind(path).map(|kind| kind.is_some_and(|kind| kind.is_dir()))
}

fn kind(path: &Path) -> io::Result<Option<fs::FileType>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata.file_type())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Every regular file under `root/dir` as paths relative to `root`, sorted. Links, special
/// files and names that are not valid `RelPath`s are skipped; a missing directory is empty.
pub fn collect_tree(root: &Path, dir: &RelPath) -> Result<Vec<RelPath>, ArchiveError> {
    let mut found = Vec::new();
    walk(root, dir, 0, &mut found)?;
    found.sort();
    Ok(found)
}

fn walk(
    root: &Path,
    dir: &RelPath,
    depth: usize,
    found: &mut Vec<RelPath>,
) -> Result<(), ArchiveError> {
    let path = dir.under(root);
    if !is_real_dir(&path)? {
        return Ok(());
    }
    if depth > TREE_DEPTH {
        return Err(ArchiveError::Malformed(format!("{dir}: вложенность больше {TREE_DEPTH}")));
    }
    for entry in fs::read_dir(&path)? {
        let entry = entry?;
        let child = entry
            .file_name()
            .to_str()
            .and_then(|name| RelPath::parse(&format!("{dir}/{name}")).ok());
        let Some(child) = child else {
            tracing::warn!(%dir, "agent file with an unusable name skipped");
            continue;
        };
        // `DirEntry::file_type` does not follow links.
        let kind = entry.file_type()?;
        if kind.is_dir() {
            walk(root, &child, depth + 1, found)?;
        } else if kind.is_file() {
            found.push(child);
            if found.len() > TREE_ENTRIES {
                return Err(ArchiveError::Malformed(format!("{dir}: больше {TREE_ENTRIES} файлов")));
            }
        }
    }
    Ok(())
}

/// Copies `files` from `root` into `dest` under the same relative paths, into private
/// directories and files; a source that is not a regular file is refused, never followed.
pub fn export_files(root: &Path, files: &[RelPath], dest: &Path) -> Result<(), ArchiveError> {
    files.iter().try_for_each(|file| {
        let from = file.under(root);
        if !is_regular_file(&from)? {
            return Err(ArchiveError::Malformed(format!("{file}: не обычный файл")));
        }
        let to = file.under(dest);
        if let Some(parent) = to.parent() {
            private_dirs(parent)?;
        }
        let mut out = private_file(&to)?;
        io::copy(&mut File::open(&from)?, &mut out)?;
        out.sync_all()?;
        Ok(())
    })
}

/// Writes every `(from, to)` pair through `write`, creating each `to` anew (an existing file is
/// an error, not overwritten). On the first failure every file created so far is removed, so a
/// failed import leaves the agent's storage as it was. Missing parent directories are created
/// and may stay behind empty.
pub fn place_all(
    pairs: &[(PathBuf, PathBuf)],
    mut write: impl FnMut(&Path, &mut File) -> io::Result<()>,
) -> Result<(), ArchiveError> {
    let mut created: Vec<&Path> = Vec::new();
    for (from, to) in pairs {
        if let Err(error) = place(from, to, &mut write) {
            for path in created.iter().rev() {
                if let Err(cleanup) = fs::remove_file(path) {
                    tracing::warn!(path = %path.display(), error = %cleanup, "restored file left behind");
                }
            }
            return Err(error);
        }
        created.push(to);
    }
    Ok(())
}

fn place(
    from: &Path,
    to: &Path,
    write: &mut impl FnMut(&Path, &mut File) -> io::Result<()>,
) -> Result<(), ArchiveError> {
    if !is_regular_file(from)? {
        return Err(ArchiveError::Malformed(format!("{}: не обычный файл", from.display())));
    }
    if let Some(parent) = to.parent() {
        private_dirs(parent)?;
    }
    let mut out = private_file(to)?;
    let written = write(from, &mut out).and_then(|()| out.sync_all());
    drop(out);
    if let Err(error) = written {
        if let Err(cleanup) = fs::remove_file(to) {
            tracing::warn!(path = %to.display(), error = %cleanup, "half-written file left behind");
        }
        return Err(error.into());
    }
    Ok(())
}

/// The plain writer for [`place_all`]: the bytes as they are.
pub fn copy_plain(from: &Path, out: &mut File) -> io::Result<()> {
    io::copy(&mut File::open(from)?, out).map(drop)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::uuid("0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0", true)]
    #[case::hermes("20261009_153012_ab12cd", true)]
    #[case::traversal("../../etc/passwd", false)]
    #[case::separator("a/b", false)]
    #[case::dot("a.b", false)]
    #[case::too_long(&"a".repeat(129), false)]
    fn session_ids_must_be_safe_file_stems(#[case] raw: &str, #[case] safe: bool) {
        let id = SessionId::parse(raw).unwrap();
        assert_eq!(file_stem(&id).is_ok(), safe);
    }
}
```

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-agent --test archive && cargo test -p hub-agent --lib -- archive`
Expected: PASS (на Windows тесты `#[cfg(unix)]` не собираются).

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked && (command -v cargo-deny >/dev/null && cargo deny check || true)`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-agent Cargo.lock
git commit -m "hub-agent: архив сессии агента и копирование по белому списку"
```

---

### Task 4: Архив Claude (`hub-claude::archive`)

**Files:**
- Create: `crates/hub-claude/src/archive.rs`
- Modify: `crates/hub-claude/src/lib.rs:1` (`pub mod archive;` первой строкой)
- Modify: `crates/hub-claude/Cargo.toml:24-27` (`[dev-dependencies]`: `tempfile.workspace = true`), `Cargo.lock`
- Create: `crates/hub-claude/tests/archive.rs`, `crates/hub-claude/tests/archive/claude.rs`
- Test: `crates/hub-claude/src/archive.rs` (`storage_root`), `crates/hub-claude/tests/archive/claude.rs`

**Interfaces:**
- Produces (`hub_claude::archive`): `storage_root(config_dir: Option<&OsStr>, home: &Path) -> PathBuf`; `ClaudeArchive::new(root: PathBuf, version: String) -> Self`; `impl SessionArchive for ClaudeArchive`.
- Consumes: `hub_agent::archive::*` (задача 3), `hub_core::snapshot::{BackendData, RelPath}` (задача 1).

Поведение: `export` ищет ровно один `projects/*/<id>.jsonl` (обычный файл в реальном каталоге), копирует его и дерево `projects/<P>/<id>/`; `present` — 0 → `false`, 1 → `true`, больше — `Ambiguous`; `import` принимает только `projects/<P из манифеста>/<id>.jsonl` и `projects/<P>/<id>/…`, кладёт их без перезаписи с откатом. `cwd` не используется: Claude ≥ 2.1.223 возобновляет по id из любого проекта.

- [ ] **Step 1: Тесты (RED)**

`crates/hub-claude/Cargo.toml`, `[dev-dependencies]`: `tempfile.workspace = true`; `cargo check -p hub-claude --all-targets`; `git diff --stat Cargo.lock`.

`crates/hub-claude/tests/archive.rs`:

```rust
#![allow(clippy::unwrap_used)]
#[path = "archive/claude.rs"]
mod claude;
```

`crates/hub-claude/tests/archive/claude.rs`:

```rust
use std::fs;
use std::io;
use std::path::Path;

use hub_agent::archive::{ArchiveError, SessionArchive};
use hub_claude::archive::ClaudeArchive;
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};

const ID: &str = "0b1c2d3e-4f50-6172-8394-a5b6c7d8e9f0";

fn rel(raw: &str) -> RelPath {
    RelPath::parse(raw).unwrap()
}

fn write(root: &Path, raw: &str, text: &str) {
    let path = rel(raw).under(root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn id() -> SessionId {
    SessionId::parse(ID).unwrap()
}

fn cwd() -> AbsolutePath {
    AbsolutePath::new(std::env::temp_dir()).unwrap()
}

fn archive(root: &Path) -> ClaudeArchive {
    ClaudeArchive::new(root.to_path_buf(), "2.1.292".to_owned())
}

/// A `~/.claude` with the session, its side directory, another session and credentials.
fn claude_home() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), &format!("projects/-work-shop/{ID}.jsonl"), "{\"type\":\"user\"}\n");
    write(root.path(), &format!("projects/-work-shop/{ID}/subagents/agent-1.jsonl"), "{}\n");
    write(root.path(), &format!("projects/-work-shop/{ID}/tool-results/t.txt"), "out");
    write(root.path(), "projects/-work-shop/other.jsonl", "{}\n");
    write(root.path(), ".credentials.json", "secret");
    root
}

#[test]
fn export_copies_the_transcript_and_its_directory_only() {
    let root = claude_home();
    let dest = tempfile::tempdir().unwrap();

    let exported = archive(root.path()).export(&id(), &cwd(), dest.path()).unwrap();

    assert_eq!(
        exported.files,
        [
            rel(&format!("projects/-work-shop/{ID}.jsonl")),
            rel(&format!("projects/-work-shop/{ID}/subagents/agent-1.jsonl")),
            rel(&format!("projects/-work-shop/{ID}/tool-results/t.txt")),
        ]
    );
    assert_eq!(exported.data, BackendData::Claude { project: rel("-work-shop") });
    assert_eq!(exported.agent_version, "2.1.292");
    assert!(rel(&format!("projects/-work-shop/{ID}/tool-results/t.txt")).under(dest.path()).is_file());
    assert!(!dest.path().join(".credentials.json").exists());
    assert!(!rel("projects/-work-shop/other.jsonl").under(dest.path()).exists());
}

#[test]
fn a_missing_session_is_not_found() {
    let root = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(
        archive(root.path()).export(&id(), &cwd(), dest.path()),
        Err(ArchiveError::NotFound)
    ));
    let data = BackendData::Claude { project: rel("-work-shop") };
    assert!(!archive(root.path()).present(&id(), &cwd(), &data).unwrap());
}

#[test]
fn two_copies_are_ambiguous() {
    let root = claude_home();
    write(root.path(), &format!("projects/-elsewhere/{ID}.jsonl"), "{}\n");
    let dest = tempfile::tempdir().unwrap();
    let data = BackendData::Claude { project: rel("-work-shop") };
    assert!(matches!(archive(root.path()).present(&id(), &cwd(), &data), Err(ArchiveError::Ambiguous)));
    assert!(matches!(
        archive(root.path()).export(&id(), &cwd(), dest.path()),
        Err(ArchiveError::Ambiguous)
    ));
}

#[test]
fn import_puts_the_session_back_and_never_overwrites() {
    let root = claude_home();
    let snapshot = tempfile::tempdir().unwrap();
    let exported = archive(root.path()).export(&id(), &cwd(), snapshot.path()).unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let target = archive(fresh.path());

    assert!(!target.present(&id(), &cwd(), &exported.data).unwrap());
    target.import(&id(), &exported.data, &exported.files, snapshot.path(), &cwd()).unwrap();
    assert!(target.present(&id(), &cwd(), &exported.data).unwrap());
    assert_eq!(
        fs::read_to_string(rel(&format!("projects/-work-shop/{ID}.jsonl")).under(fresh.path())).unwrap(),
        "{\"type\":\"user\"}\n"
    );

    let again = target.import(&id(), &exported.data, &exported.files, snapshot.path(), &cwd());
    assert!(matches!(again, Err(ArchiveError::Io(ref error)) if error.kind() == io::ErrorKind::AlreadyExists));
    assert!(rel(&format!("projects/-work-shop/{ID}.jsonl")).under(fresh.path()).is_file());
}

#[test]
fn import_refuses_paths_outside_the_session() {
    let snapshot = tempfile::tempdir().unwrap();
    write(snapshot.path(), &format!("projects/-work-shop/{ID}.jsonl"), "{}\n");
    write(snapshot.path(), ".credentials.json", "planted");
    let fresh = tempfile::tempdir().unwrap();
    let data = BackendData::Claude { project: rel("-work-shop") };
    for files in [
        vec![rel(&format!("projects/-work-shop/{ID}.jsonl")), rel(".credentials.json")],
        vec![rel(&format!("projects/-other/{ID}.jsonl"))],
        vec![rel(&format!("projects/-work-shop/{ID}/x"))],
    ] {
        let result = archive(fresh.path()).import(&id(), &data, &files, snapshot.path(), &cwd());
        assert!(matches!(result, Err(ArchiveError::Malformed(_))), "{files:?}");
    }
    assert_eq!(fs::read_dir(fresh.path()).unwrap().count(), 0);
}

#[test]
fn import_refuses_another_backends_snapshot() {
    let snapshot = tempfile::tempdir().unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let result = archive(fresh.path()).import(&id(), &BackendData::Codex, &[], snapshot.path(), &cwd());
    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
}

#[cfg(unix)]
#[test]
fn a_linked_transcript_is_skipped() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "t.jsonl", "{}\n");
    fs::create_dir_all(root.path().join("projects").join("-work-shop")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("t.jsonl"),
        rel(&format!("projects/-work-shop/{ID}.jsonl")).under(root.path()),
    )
    .unwrap();
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(
        archive(root.path()).export(&id(), &cwd(), dest.path()),
        Err(ArchiveError::NotFound)
    ));
}
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-claude --test archive`
Expected: FAIL — `unresolved import hub_claude::archive`.

- [ ] **Step 3: Реализация**

`crates/hub-claude/src/lib.rs`: первой строкой `pub mod archive;`.

`crates/hub-claude/src/archive.rs`:

```rust
//! Claude Code keeps a session as `projects/<P>/<id>.jsonl` plus `projects/<P>/<id>/`
//! (subagents, tool results). `<P>` encodes the cwd with a hash for long paths, so it is
//! found, not computed; Claude resumes an id from any project.

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use hub_agent::archive::{
    ArchiveError, Exported, SessionArchive, collect_tree, copy_plain, export_files, file_stem,
    is_real_dir, is_regular_file, place_all,
};
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};

const PROJECTS: &str = "projects";

/// `$CLAUDE_CONFIG_DIR`, or `~/.claude` when it is unset or empty.
#[must_use]
pub fn storage_root(config_dir: Option<&OsStr>, home: &Path) -> PathBuf {
    match config_dir.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home.join(".claude"),
    }
}

pub struct ClaudeArchive {
    root: PathBuf,
    version: String,
}

impl ClaudeArchive {
    #[must_use]
    pub fn new(root: PathBuf, version: String) -> Self {
        Self { root, version }
    }

    /// Project directories holding `<stem>.jsonl` as a regular file, sorted.
    fn projects_with(&self, stem: &str) -> Result<Vec<String>, ArchiveError> {
        let projects = self.root.join(PROJECTS);
        if !is_real_dir(&projects)? {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        for entry in fs::read_dir(&projects)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            if entry.file_type()?.is_dir()
                && is_regular_file(&entry.path().join(format!("{stem}.jsonl")))?
            {
                found.push(name);
            }
        }
        found.sort();
        Ok(found)
    }
}

impl SessionArchive for ClaudeArchive {
    fn export(
        &self,
        session: &SessionId,
        _cwd: &AbsolutePath,
        dest: &Path,
    ) -> Result<Exported, ArchiveError> {
        let stem = file_stem(session)?;
        let project = match self.projects_with(stem)?.as_slice() {
            [] => return Err(ArchiveError::NotFound),
            [project] => RelPath::parse(project)?,
            [_, _, ..] => return Err(ArchiveError::Ambiguous),
        };
        let transcript = RelPath::from_parts(&[PROJECTS, project.as_str(), &format!("{stem}.jsonl")])?;
        let side = RelPath::from_parts(&[PROJECTS, project.as_str(), stem])?;
        let files: Vec<RelPath> =
            std::iter::once(transcript).chain(collect_tree(&self.root, &side)?).collect();
        export_files(&self.root, &files, dest)?;
        Ok(Exported {
            files,
            data: BackendData::Claude { project },
            agent_version: self.version.clone(),
        })
    }

    fn present(
        &self,
        session: &SessionId,
        _cwd: &AbsolutePath,
        _data: &BackendData,
    ) -> Result<bool, ArchiveError> {
        match self.projects_with(file_stem(session)?)?.as_slice() {
            [] => Ok(false),
            [_] => Ok(true),
            [_, _, ..] => Err(ArchiveError::Ambiguous),
        }
    }

    fn import(
        &self,
        session: &SessionId,
        data: &BackendData,
        files: &[RelPath],
        src: &Path,
        _cwd: &AbsolutePath,
    ) -> Result<(), ArchiveError> {
        let stem = file_stem(session)?;
        let project = match data {
            BackendData::Claude { project } => project.as_str(),
            BackendData::Codex | BackendData::Qwen | BackendData::Hermes { .. } => {
                return Err(ArchiveError::Malformed("снимок другого бэкенда".to_owned()));
            }
        };
        let transcript = format!("{stem}.jsonl");
        let pairs = files
            .iter()
            .map(|file| {
                let parts: Vec<&str> = file.parts().collect();
                let ours = match parts.as_slice() {
                    [PROJECTS, dir, name] => *dir == project && *name == transcript,
                    [PROJECTS, dir, id, _, ..] => *dir == project && *id == stem,
                    _ => false,
                };
                if ours {
                    Ok((file.under(src), file.under(&self.root)))
                } else {
                    Err(ArchiveError::Malformed(format!("{file}: не файл этой сессии Claude")))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !files.iter().any(|file| file.parts().last() == Some(transcript.as_str())) {
            return Err(ArchiveError::Malformed("в снимке нет транскрипта Claude".to_owned()));
        }
        place_all(&pairs, copy_plain)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    #[test]
    fn config_dir_overrides_the_home_default() {
        let home = Path::new("home");
        assert_eq!(storage_root(Some(&OsString::from("custom")), home), PathBuf::from("custom"));
        assert_eq!(storage_root(Some(&OsString::new()), home), home.join(".claude"));
        assert_eq!(storage_root(None, home), home.join(".claude"));
    }
}
```

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-claude --test archive && cargo test -p hub-claude --lib -- archive`
Expected: PASS.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-claude Cargo.lock
git commit -m "hub-claude: выгрузка и возврат истории сессии для снимков"
```

---

### Task 5: Архив Codex (`hub-codex::archive`)

**Files:**
- Create: `crates/hub-codex/src/archive.rs`
- Modify: `crates/hub-codex/src/lib.rs:1` (`pub mod archive;`)
- Modify: `crates/hub-codex/Cargo.toml` (`[dev-dependencies]`: `tempfile.workspace = true`), `Cargo.lock`
- Create: `crates/hub-codex/tests/archive.rs`, `crates/hub-codex/tests/archive/codex.rs`
- Test: `crates/hub-codex/src/archive.rs` (чистые `is_rollout_of`, `day_of`, `storage_root`), `crates/hub-codex/tests/archive/codex.rs`

**Interfaces:**
- Produces (`hub_codex::archive`): `storage_root(codex_home: Option<&OsStr>, home: &Path) -> PathBuf`; `CodexArchive::new(root: PathBuf, version: String) -> Self`; `impl SessionArchive for CodexArchive`.

Поведение: все `sessions/**/rollout-*-<id>[_*].jsonl[.zst]` (обычные файлы, ссылки пропущены); если в `sessions/` нет, а в `archived_sessions/` есть — `Archived(id)`; SQLite не трогается. Возврат — в `sessions/YYYY/MM/DD/` по дате из имени файла, имя прежнее; любой другой путь — `Malformed`.

- [ ] **Step 1: Тесты (RED)**

`crates/hub-codex/tests/archive.rs`:

```rust
#![allow(clippy::unwrap_used)]
#[path = "archive/codex.rs"]
mod codex;
```

`crates/hub-codex/tests/archive/codex.rs`:

```rust
use std::fs;
use std::path::Path;

use hub_agent::archive::{ArchiveError, SessionArchive};
use hub_codex::archive::CodexArchive;
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};

const ID: &str = "019a0b1c-2d3e-7f50-8172-a5b6c7d8e9f0";

fn rel(raw: &str) -> RelPath {
    RelPath::parse(raw).unwrap()
}

fn write(root: &Path, raw: &str) {
    let path = rel(raw).under(root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, "{\"type\":\"session_meta\"}\n").unwrap();
}

fn id() -> SessionId {
    SessionId::parse(ID).unwrap()
}

fn cwd() -> AbsolutePath {
    AbsolutePath::new(std::env::temp_dir()).unwrap()
}

fn archive(root: &Path) -> CodexArchive {
    CodexArchive::new(root.to_path_buf(), "0.160.0".to_owned())
}

#[test]
fn export_copies_every_rollout_of_the_session_and_nothing_else() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), &format!("sessions/2026/10/09/rollout-2026-10-09T15-30-12-{ID}.jsonl"));
    write(root.path(), &format!("sessions/2026/10/10/rollout-2026-10-10T09-00-00-{ID}_2.jsonl.zst"));
    write(root.path(), "sessions/2026/10/09/rollout-2026-10-09T15-30-12-other.jsonl");
    write(root.path(), "auth.json");
    write(root.path(), "state_5.sqlite");
    let dest = tempfile::tempdir().unwrap();

    let exported = archive(root.path()).export(&id(), &cwd(), dest.path()).unwrap();

    assert_eq!(
        exported.files,
        [
            rel(&format!("sessions/2026/10/09/rollout-2026-10-09T15-30-12-{ID}.jsonl")),
            rel(&format!("sessions/2026/10/10/rollout-2026-10-10T09-00-00-{ID}_2.jsonl.zst")),
        ]
    );
    assert_eq!(exported.data, BackendData::Codex);
    assert!(!dest.path().join("auth.json").exists() && !dest.path().join("state_5.sqlite").exists());
}

#[test]
fn an_archived_session_names_the_unarchive_command() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), &format!("archived_sessions/rollout-2026-10-09T15-30-12-{ID}.jsonl"));
    let dest = tempfile::tempdir().unwrap();
    let exported = archive(root.path()).export(&id(), &cwd(), dest.path());
    assert!(matches!(exported, Err(ArchiveError::Archived(ref archived)) if archived == ID));
    assert!(matches!(
        archive(root.path()).present(&id(), &cwd(), &BackendData::Codex),
        Err(ArchiveError::Archived(_))
    ));
    assert_eq!(
        ArchiveError::Archived(ID.to_owned()).to_string(),
        format!("Сессия в архиве Codex: выполните `codex unarchive {ID}`")
    );
}

#[test]
fn a_missing_session_is_not_found_and_not_present() {
    let root = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(archive(root.path()).export(&id(), &cwd(), dest.path()), Err(ArchiveError::NotFound)));
    assert!(!archive(root.path()).present(&id(), &cwd(), &BackendData::Codex).unwrap());
}

#[test]
fn import_files_rollouts_by_their_date() {
    let snapshot = tempfile::tempdir().unwrap();
    let name = format!("rollout-2026-10-09T15-30-12-{ID}.jsonl");
    write(snapshot.path(), &format!("sessions/2026/10/09/{name}"));
    let fresh = tempfile::tempdir().unwrap();
    let files = [rel(&format!("sessions/2026/10/09/{name}"))];

    archive(fresh.path()).import(&id(), &BackendData::Codex, &files, snapshot.path(), &cwd()).unwrap();

    assert!(rel(&format!("sessions/2026/10/09/{name}")).under(fresh.path()).is_file());
    assert!(archive(fresh.path()).present(&id(), &cwd(), &BackendData::Codex).unwrap());
}

#[test]
fn import_refuses_foreign_or_misdated_paths() {
    let snapshot = tempfile::tempdir().unwrap();
    write(snapshot.path(), "auth.json");
    write(snapshot.path(), &format!("sessions/2026/01/01/rollout-2026-10-09T15-30-12-{ID}.jsonl"));
    write(snapshot.path(), "sessions/2026/10/09/rollout-2026-10-09T15-30-12-other.jsonl");
    let fresh = tempfile::tempdir().unwrap();
    for file in [
        "auth.json",
        &format!("sessions/2026/01/01/rollout-2026-10-09T15-30-12-{ID}.jsonl"),
        "sessions/2026/10/09/rollout-2026-10-09T15-30-12-other.jsonl",
    ] {
        let result =
            archive(fresh.path()).import(&id(), &BackendData::Codex, &[rel(file)], snapshot.path(), &cwd());
        assert!(matches!(result, Err(ArchiveError::Malformed(_))), "{file}");
    }
    assert_eq!(fs::read_dir(fresh.path()).unwrap().count(), 0);
}
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-codex --test archive`
Expected: FAIL — `unresolved import hub_codex::archive`.

- [ ] **Step 3: Реализация**

`crates/hub-codex/src/archive.rs`:

```rust
//! Codex keeps a thread as `sessions/YYYY/MM/DD/rollout-<timestamp>-<id>[_<n>].jsonl[.zst]`;
//! `thread/resume` finds the file by id and re-creates its SQLite row, so only files move.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use hub_agent::archive::{
    ArchiveError, Exported, SessionArchive, collect_tree, copy_plain, export_files, file_stem,
    place_all,
};
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};

const SESSIONS: &str = "sessions";
const ARCHIVED: &str = "archived_sessions";

/// `$CODEX_HOME`, or `~/.codex` when it is unset or empty.
#[must_use]
pub fn storage_root(codex_home: Option<&OsStr>, home: &Path) -> PathBuf {
    match codex_home.filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => home.join(".codex"),
    }
}

/// `rollout-<timestamp>-<id>.jsonl`, with an optional `_<n>` and `.zst`.
fn is_rollout_of(name: &str, id: &str) -> bool {
    let stem = name.strip_suffix(".jsonl.zst").or_else(|| name.strip_suffix(".jsonl"));
    stem.and_then(|stem| stem.strip_prefix("rollout-")).is_some_and(|rest| {
        rest.ends_with(&format!("-{id}")) || rest.contains(&format!("-{id}_"))
    })
}

/// `["YYYY", "MM", "DD"]` from `rollout-YYYY-MM-DDT…`.
fn day_of(name: &str) -> Option<[&str; 3]> {
    let date = name.strip_prefix("rollout-")?.get(..10)?;
    let mut parts = date.split('-');
    let day = [parts.next()?, parts.next()?, parts.next()?];
    let digits = |part: &str, len: usize| part.len() == len && part.bytes().all(|b| b.is_ascii_digit());
    let [year, month, date] = day;
    (digits(year, 4) && digits(month, 2) && digits(date, 2)).then_some(day)
}

pub struct CodexArchive {
    root: PathBuf,
    version: String,
}

impl CodexArchive {
    #[must_use]
    pub fn new(root: PathBuf, version: String) -> Self {
        Self { root, version }
    }

    fn rollouts(&self, top: &str, id: &str) -> Result<Vec<RelPath>, ArchiveError> {
        Ok(collect_tree(&self.root, &RelPath::parse(top)?)?
            .into_iter()
            .filter(|file| file.parts().last().is_some_and(|name| is_rollout_of(name, id)))
            .collect())
    }

    /// Live rollouts; none live but some archived is `Archived`.
    fn live(&self, id: &str) -> Result<Vec<RelPath>, ArchiveError> {
        let live = self.rollouts(SESSIONS, id)?;
        if live.is_empty() && !self.rollouts(ARCHIVED, id)?.is_empty() {
            return Err(ArchiveError::Archived(id.to_owned()));
        }
        Ok(live)
    }
}

impl SessionArchive for CodexArchive {
    fn export(
        &self,
        session: &SessionId,
        _cwd: &AbsolutePath,
        dest: &Path,
    ) -> Result<Exported, ArchiveError> {
        let files = self.live(file_stem(session)?)?;
        if files.is_empty() {
            return Err(ArchiveError::NotFound);
        }
        export_files(&self.root, &files, dest)?;
        Ok(Exported { files, data: BackendData::Codex, agent_version: self.version.clone() })
    }

    fn present(
        &self,
        session: &SessionId,
        _cwd: &AbsolutePath,
        _data: &BackendData,
    ) -> Result<bool, ArchiveError> {
        Ok(!self.live(file_stem(session)?)?.is_empty())
    }

    fn import(
        &self,
        session: &SessionId,
        data: &BackendData,
        files: &[RelPath],
        src: &Path,
        _cwd: &AbsolutePath,
    ) -> Result<(), ArchiveError> {
        let id = file_stem(session)?;
        match data {
            BackendData::Codex => {}
            BackendData::Claude { .. } | BackendData::Qwen | BackendData::Hermes { .. } => {
                return Err(ArchiveError::Malformed("снимок другого бэкенда".to_owned()));
            }
        }
        let pairs = files
            .iter()
            .map(|file| {
                let parts: Vec<&str> = file.parts().collect();
                let target = match parts.as_slice() {
                    [SESSIONS, year, month, date, name] if is_rollout_of(name, id) => day_of(name)
                        .filter(|day| *day == [*year, *month, *date])
                        .map(|[y, m, d]| RelPath::from_parts(&[SESSIONS, y, m, d, *name])),
                    _ => None,
                };
                match target {
                    Some(target) => Ok((file.under(src), target?.under(&self.root))),
                    None => Err(ArchiveError::Malformed(format!("{file}: не файл этой сессии Codex"))),
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        place_all(&pairs, copy_plain)
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::plain("rollout-2026-10-09T15-30-12-abc.jsonl", true)]
    #[case::compressed("rollout-2026-10-09T15-30-12-abc.jsonl.zst", true)]
    #[case::numbered("rollout-2026-10-09T15-30-12-abc_2.jsonl", true)]
    #[case::other_id("rollout-2026-10-09T15-30-12-abcd.jsonl", false)]
    #[case::suffix_id("rollout-2026-10-09T15-30-12-xabc.jsonl", false)]
    #[case::not_rollout("abc.jsonl", false)]
    #[case::wrong_extension("rollout-2026-10-09T15-30-12-abc.json", false)]
    fn rollouts_match_by_id(#[case] name: &str, #[case] expected: bool) {
        assert_eq!(is_rollout_of(name, "abc"), expected);
    }

    #[rstest]
    #[case("rollout-2026-10-09T15-30-12-abc.jsonl", Some(["2026", "10", "09"]))]
    #[case("rollout-26-10-09T15-30-12-abc.jsonl", None)]
    #[case("rollout-x", None)]
    fn rollout_dates_come_from_the_name(#[case] name: &str, #[case] expected: Option<[&str; 3]>) {
        assert_eq!(day_of(name), expected);
    }

    #[test]
    fn codex_home_overrides_the_home_default() {
        let home = Path::new("home");
        assert_eq!(storage_root(Some(OsStr::new("c")), home), PathBuf::from("c"));
        assert_eq!(storage_root(Some(OsStr::new("")), home), home.join(".codex"));
        assert_eq!(storage_root(None, home), home.join(".codex"));
    }
}
```

Замечание к `import`: `.map(|[y, m, d]| …)` даёт `Option<Result<RelPath, PathError>>`; `target?` преобразует `PathError` в `ArchiveError` через `From` (задача 3). Если `clippy` потребует упростить форму — переписать через `and_then`, сохранив проверку «дата каталога = дата из имени».

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-codex --test archive && cargo test -p hub-codex --lib -- archive`
Expected: PASS.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-codex Cargo.lock
git commit -m "hub-codex: выгрузка и возврат rollout-файлов для снимков"
```

---

### Task 6: Архив Qwen (`hub-qwen::archive`)

**Files:**
- Create: `crates/hub-qwen/src/archive.rs`
- Modify: `crates/hub-qwen/src/lib.rs:1` (`pub mod archive;`)
- Modify: `crates/hub-qwen/Cargo.toml` (`[dev-dependencies]`: `tempfile.workspace = true`), `Cargo.lock`
- Create: `crates/hub-qwen/tests/archive.rs`, `crates/hub-qwen/tests/archive/qwen.rs`
- Test: `crates/hub-qwen/src/archive.rs` (чистые `storage_root`, `sanitize`, `rewrite_cwd`), `crates/hub-qwen/tests/archive/qwen.rs`

**Interfaces:**
- Produces (`hub_qwen::archive`): `storage_root(runtime: Option<&OsStr>, qwen_home: Option<&OsStr>, home: &Path) -> Result<PathBuf, ArchiveError>`; `sanitize(cwd: &str, windows: bool) -> String`; `QwenArchive::new(root: PathBuf, version: String) -> Self`; `impl SessionArchive for QwenArchive`.

Поведение (раздел «Хранилища агентов», Qwen): выгружаются `projects/<san(cwd)>/chats/<id>.jsonl` и спутники `<id>.*` кроме `.runtime.json`/`.worktree.json`, `projects/<san(cwd)>/subagents/<id>/**`, `todos/<id>.json`. `present` — есть ли `projects/<san(cwd назначения)>/chats/<id>.jsonl`. `import` переносит файлы под `san(cwd назначения)`; если каталог проекта другой — во всех `*.jsonl` строковое поле `cwd` верхнего уровня записи заменяется новым `cwd` (прочие поля и нераспознанные строки — как есть).

- [ ] **Step 1: Тесты (RED)**

`crates/hub-qwen/tests/archive.rs`:

```rust
#![allow(clippy::unwrap_used)]
#[path = "archive/qwen.rs"]
mod qwen;
```

`crates/hub-qwen/tests/archive/qwen.rs`:

```rust
use std::fs;
use std::path::{Path, PathBuf};

use hub_agent::archive::{ArchiveError, SessionArchive};
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};
use hub_qwen::archive::{QwenArchive, sanitize};
use serde_json::Value;

const ID: &str = "4f1c2d3e-0b50-4172-8394-a5b6c7d8e9f0";

fn rel(raw: &str) -> RelPath {
    RelPath::parse(raw).unwrap()
}

fn write(root: &Path, raw: &str, text: &str) {
    let path = rel(raw).under(root);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn id() -> SessionId {
    SessionId::parse(ID).unwrap()
}

fn cwd(name: &str) -> AbsolutePath {
    AbsolutePath::new(std::env::temp_dir().join(name)).unwrap()
}

fn san(cwd: &AbsolutePath) -> String {
    sanitize(cwd.as_path().to_str().unwrap(), cfg!(windows))
}

fn archive(root: &Path) -> QwenArchive {
    QwenArchive::new(root.to_path_buf(), "0.25.0".to_owned())
}

fn record(cwd: &AbsolutePath) -> String {
    serde_json::json!({"uuid": "u-1", "sessionId": ID, "type": "user", "cwd": cwd.as_path(), "version": "0.25.0"})
        .to_string()
}

/// A `~/.qwen` with the session in project `shop`, its extras, another session and secrets.
fn qwen_home() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let project = format!("projects/{}", san(&cwd("shop")));
    write(root.path(), &format!("{project}/chats/{ID}.jsonl"), &format!("{}\nnot json\n", record(&cwd("shop"))));
    write(root.path(), &format!("{project}/chats/{ID}.pr.json"), "{}");
    write(root.path(), &format!("{project}/chats/{ID}.runtime.json"), "{\"workDir\":\"old\"}");
    write(root.path(), &format!("{project}/chats/{ID}.worktree.json"), "{}");
    write(root.path(), &format!("{project}/chats/other.jsonl"), "{}\n");
    write(root.path(), &format!("{project}/subagents/{ID}/agent-1.jsonl"), &format!("{}\n", record(&cwd("shop"))));
    write(root.path(), &format!("todos/{ID}.json"), "{\"todos\":[]}");
    write(root.path(), "settings.json", "{\"apiKey\":\"secret\"}");
    write(root.path(), "oauth_creds.json", "secret");
    root
}

fn exported_files(project: &str) -> Vec<RelPath> {
    vec![
        rel(&format!("projects/{project}/chats/{ID}.jsonl")),
        rel(&format!("projects/{project}/chats/{ID}.pr.json")),
        rel(&format!("projects/{project}/subagents/{ID}/agent-1.jsonl")),
        rel(&format!("todos/{ID}.json")),
    ]
}

#[test]
fn export_takes_the_chat_its_extras_and_todos_but_no_secrets() {
    let root = qwen_home();
    let dest = tempfile::tempdir().unwrap();

    let exported = archive(root.path()).export(&id(), &cwd("shop"), dest.path()).unwrap();

    assert_eq!(exported.files, exported_files(&san(&cwd("shop"))));
    assert_eq!(exported.data, BackendData::Qwen);
    for secret in ["settings.json", "oauth_creds.json"] {
        assert!(!dest.path().join(secret).exists(), "{secret}");
    }
}

#[test]
fn a_session_of_another_project_is_not_found() {
    let root = qwen_home();
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(archive(root.path()).export(&id(), &cwd("other"), dest.path()), Err(ArchiveError::NotFound)));
    assert!(archive(root.path()).present(&id(), &cwd("shop"), &BackendData::Qwen).unwrap());
    assert!(!archive(root.path()).present(&id(), &cwd("other"), &BackendData::Qwen).unwrap());
}

fn exported() -> (tempfile::TempDir, Vec<RelPath>) {
    let root = qwen_home();
    let snapshot = tempfile::tempdir().unwrap();
    let files = archive(root.path()).export(&id(), &cwd("shop"), snapshot.path()).unwrap().files;
    (snapshot, files)
}

#[test]
fn the_same_directory_gets_the_files_unchanged() {
    let (snapshot, files) = exported();
    let fresh = tempfile::tempdir().unwrap();

    archive(fresh.path()).import(&id(), &BackendData::Qwen, &files, snapshot.path(), &cwd("shop")).unwrap();

    for file in &files {
        assert_eq!(fs::read(file.under(fresh.path())).unwrap(), fs::read(file.under(snapshot.path())).unwrap());
    }
}

#[test]
fn a_new_directory_gets_the_files_under_its_project_with_cwd_rewritten() {
    let (snapshot, files) = exported();
    let fresh = tempfile::tempdir().unwrap();
    let target = cwd("moved");

    archive(fresh.path()).import(&id(), &BackendData::Qwen, &files, snapshot.path(), &target).unwrap();

    assert!(archive(fresh.path()).present(&id(), &target, &BackendData::Qwen).unwrap());
    let chat: PathBuf = rel(&format!("projects/{}/chats/{ID}.jsonl", san(&target))).under(fresh.path());
    let text = fs::read_to_string(chat).unwrap();
    let mut lines = text.lines();
    let first: Value = serde_json::from_str(lines.next().unwrap()).unwrap();
    assert_eq!(first["cwd"], Value::from(target.as_path().to_str().unwrap()));
    assert_eq!((first["sessionId"].as_str(), first["uuid"].as_str()), (Some(ID), Some("u-1")));
    assert_eq!(lines.next(), Some("not json"));
    let agent = rel(&format!("projects/{}/subagents/{ID}/agent-1.jsonl", san(&target))).under(fresh.path());
    assert!(fs::read_to_string(agent).unwrap().contains("moved"));
    assert!(!rel(&format!("projects/{}", san(&cwd("shop")))).under(fresh.path()).exists());
}

#[test]
fn import_refuses_files_that_are_not_this_session() {
    let (snapshot, mut files) = exported();
    write(snapshot.path(), "settings.json", "planted");
    files.push(rel("settings.json"));
    let fresh = tempfile::tempdir().unwrap();
    let result = archive(fresh.path()).import(&id(), &BackendData::Qwen, &files, snapshot.path(), &cwd("shop"));
    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    let mixed = vec![
        rel(&format!("projects/a/chats/{ID}.jsonl")),
        rel(&format!("projects/b/chats/{ID}.pr.json")),
    ];
    let result = archive(fresh.path()).import(&id(), &BackendData::Qwen, &mixed, snapshot.path(), &cwd("shop"));
    assert!(matches!(result, Err(ArchiveError::Malformed(_))));
    assert_eq!(fs::read_dir(fresh.path()).unwrap().count(), 0);
}
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-qwen --test archive`
Expected: FAIL — `unresolved import hub_qwen::archive`.

- [ ] **Step 3: Реализация**

`crates/hub-qwen/src/archive.rs`:

```rust
//! Qwen keeps a session under `projects/<sanitized cwd>/` and loads it only when the first
//! record's `cwd` hashes to the project, so a session moved to another directory has its
//! records' `cwd` rewritten.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use hub_agent::archive::{
    ArchiveError, Exported, SessionArchive, collect_tree, copy_plain, export_files, file_stem,
    is_real_dir, is_regular_file, place_all,
};
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};
use serde_json::Value;

const PROJECTS: &str = "projects";
const CHATS: &str = "chats";
const SUBAGENTS: &str = "subagents";
const TODOS: &str = "todos";
/// Sidecars that hold the old directory and would point the session back at it.
const STALE: [&str; 2] = [".runtime.json", ".worktree.json"];

/// `$QWEN_RUNTIME_DIR`, else `$QWEN_HOME`, else `~/.qwen`; `~` expands as in Qwen, a relative
/// value (Qwen resolves it against its own cwd) is refused.
pub fn storage_root(
    runtime: Option<&OsStr>,
    qwen_home: Option<&OsStr>,
    home: &Path,
) -> Result<PathBuf, ArchiveError> {
    let Some(raw) = [runtime, qwen_home].into_iter().flatten().find(|raw| !raw.is_empty()) else {
        return Ok(home.join(".qwen"));
    };
    let unsupported =
        || ArchiveError::Unsupported("QWEN_RUNTIME_DIR/QWEN_HOME: нужен абсолютный путь или ~".to_owned());
    let raw = raw.to_str().ok_or_else(unsupported)?;
    let path = match raw.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => rest
            .split(['/', '\\'])
            .filter(|part| !part.is_empty())
            .fold(home.to_path_buf(), |path, part| path.join(part)),
        Some(_) | None => PathBuf::from(raw),
    };
    if path.is_absolute() { Ok(path) } else { Err(unsupported()) }
}

/// Qwen's `sanitizeCwd`: lowercase on Windows, then every UTF-16 unit outside `[A-Za-z0-9]` → `-`.
#[must_use]
pub fn sanitize(cwd: &str, windows: bool) -> String {
    let cwd = if windows { cwd.to_lowercase() } else { cwd.to_owned() };
    cwd.encode_utf16()
        .map(|unit| match char::from_u32(u32::from(unit)) {
            Some(c) if c.is_ascii_alphanumeric() => c,
            Some(_) | None => '-',
        })
        .collect()
}

/// The record with its top-level string `cwd` replaced; anything else is left as written.
fn rewrite_cwd(line: &str, cwd: &str) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(Value::Object(mut record)) if record.get("cwd").is_some_and(Value::is_string) => {
            record.insert("cwd".to_owned(), Value::String(cwd.to_owned()));
            Value::Object(record).to_string()
        }
        Ok(_) | Err(_) => line.to_owned(),
    }
}

fn rewrite_lines(from: &Path, out: &mut File, cwd: &str) -> io::Result<()> {
    for line in BufReader::new(File::open(from)?).lines() {
        writeln!(out, "{}", rewrite_cwd(&line?, cwd))?;
    }
    Ok(())
}

fn cwd_text(cwd: &AbsolutePath) -> Result<&str, ArchiveError> {
    cwd.as_path()
        .to_str()
        .ok_or_else(|| ArchiveError::Unsupported("рабочий каталог не в UTF-8".to_owned()))
}

fn project_of(cwd: &AbsolutePath) -> Result<String, ArchiveError> {
    Ok(sanitize(cwd_text(cwd)?, cfg!(windows)))
}

pub struct QwenArchive {
    root: PathBuf,
    version: String,
}

impl QwenArchive {
    #[must_use]
    pub fn new(root: PathBuf, version: String) -> Self {
        Self { root, version }
    }

    /// The chat and its sidecars `<stem>.*`, minus the stale ones; regular files only.
    fn chat_files(&self, project: &str, stem: &str) -> Result<Vec<RelPath>, ArchiveError> {
        let chats = RelPath::from_parts(&[PROJECTS, project, CHATS])?;
        let dir = chats.under(&self.root);
        if !is_real_dir(&dir)? {
            return Ok(Vec::new());
        }
        let prefix = format!("{stem}.");
        let mut found = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else { continue };
            let wanted = name.starts_with(&prefix) && !STALE.iter().any(|stale| name.ends_with(stale));
            if wanted && entry.file_type()?.is_file() {
                found.push(RelPath::from_parts(&[PROJECTS, project, CHATS, &name])?);
            }
        }
        found.sort();
        Ok(found)
    }
}

impl SessionArchive for QwenArchive {
    fn export(
        &self,
        session: &SessionId,
        cwd: &AbsolutePath,
        dest: &Path,
    ) -> Result<Exported, ArchiveError> {
        let stem = file_stem(session)?;
        let project = project_of(cwd)?;
        let main = RelPath::from_parts(&[PROJECTS, &project, CHATS, &format!("{stem}.jsonl")])?;
        if !is_regular_file(&main.under(&self.root))? {
            return Err(ArchiveError::NotFound);
        }
        let todo = RelPath::from_parts(&[TODOS, &format!("{stem}.json")])?;
        let todos = if is_regular_file(&todo.under(&self.root))? { vec![todo] } else { Vec::new() };
        let subagents = RelPath::from_parts(&[PROJECTS, &project, SUBAGENTS, stem])?;
        let files: Vec<RelPath> = self
            .chat_files(&project, stem)?
            .into_iter()
            .chain(collect_tree(&self.root, &subagents)?)
            .chain(todos)
            .collect();
        export_files(&self.root, &files, dest)?;
        Ok(Exported { files, data: BackendData::Qwen, agent_version: self.version.clone() })
    }

    fn present(
        &self,
        session: &SessionId,
        cwd: &AbsolutePath,
        _data: &BackendData,
    ) -> Result<bool, ArchiveError> {
        let stem = file_stem(session)?;
        let chat = RelPath::from_parts(&[PROJECTS, &project_of(cwd)?, CHATS, &format!("{stem}.jsonl")])?;
        Ok(is_regular_file(&chat.under(&self.root))?)
    }

    fn import(
        &self,
        session: &SessionId,
        data: &BackendData,
        files: &[RelPath],
        src: &Path,
        cwd: &AbsolutePath,
    ) -> Result<(), ArchiveError> {
        match data {
            BackendData::Qwen => {}
            BackendData::Claude { .. } | BackendData::Codex | BackendData::Hermes { .. } => {
                return Err(ArchiveError::Malformed("снимок другого бэкенда".to_owned()));
            }
        }
        let stem = file_stem(session)?;
        let new = project_of(cwd)?;
        let (chat, todo) = (format!("{stem}.jsonl"), format!("{stem}.json"));
        let prefix = format!("{stem}.");
        let foreign = |file: &RelPath| ArchiveError::Malformed(format!("{file}: не файл этой сессии Qwen"));
        let mut old: Option<String> = None;
        let mut pairs = Vec::new();
        for file in files {
            let parts: Vec<&str> = file.parts().collect();
            let (project, target) = match parts.as_slice() {
                [TODOS, name] if *name == todo => (None, file.clone()),
                [PROJECTS, project, CHATS, name]
                    if name.starts_with(&prefix) && !STALE.iter().any(|stale| name.ends_with(stale)) =>
                {
                    (Some(*project), RelPath::from_parts(&[PROJECTS, &new, CHATS, name])?)
                }
                [PROJECTS, project, SUBAGENTS, id, rest @ ..] if *id == stem && !rest.is_empty() => {
                    let tail = [PROJECTS, new.as_str(), SUBAGENTS, stem].into_iter().chain(rest.iter().copied());
                    (Some(*project), RelPath::from_parts(&tail.collect::<Vec<_>>())?)
                }
                _ => return Err(foreign(file)),
            };
            if let Some(project) = project {
                match &old {
                    None => old = Some(project.to_owned()),
                    Some(seen) if seen == project => {}
                    Some(_) => return Err(foreign(file)),
                }
            }
            pairs.push((file.under(src), target.under(&self.root)));
        }
        if !files.iter().any(|file| file.parts().nth(3) == Some(chat.as_str())) {
            return Err(ArchiveError::Malformed("в снимке нет чата Qwen".to_owned()));
        }
        let rewrite = old.as_deref() != Some(new.as_str());
        let cwd = cwd_text(cwd)?;
        place_all(&pairs, |from, out| {
            if rewrite && from.extension().is_some_and(|ext| ext == "jsonl") {
                rewrite_lines(from, out, cwd)
            } else {
                copy_plain(from, out)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::unix("/home/me/shop", false, "-home-me-shop")]
    #[case::windows(r"C:\Work\Shop", true, "c--work-shop")]
    #[case::cyrillic("/tmp/проект", false, "-tmp-------")]
    #[case::astral("/a😀", false, "-a--")]
    fn cwd_is_sanitized_like_qwen(#[case] cwd: &str, #[case] windows: bool, #[case] expected: &str) {
        assert_eq!(sanitize(cwd, windows), expected);
    }

    #[rstest]
    #[case::rewritten(r#"{"cwd":"/old","uuid":"u"}"#, r#"{"cwd":"/new","uuid":"u"}"#)]
    #[case::no_cwd(r#"{"uuid":"u"}"#, r#"{"uuid":"u"}"#)]
    #[case::not_a_string(r#"{"cwd":5}"#, r#"{"cwd":5}"#)]
    #[case::garbage("not json", "not json")]
    fn only_a_string_cwd_is_rewritten(#[case] line: &str, #[case] expected: &str) {
        assert_eq!(rewrite_cwd(line, "/new"), expected);
    }

    #[test]
    fn runtime_dir_wins_then_qwen_home_then_the_default() {
        let home = std::env::temp_dir();
        let absolute = home.join("runtime");
        let raw = absolute.as_os_str();
        assert_eq!(storage_root(Some(raw), Some(OsStr::new("~/q")), &home).unwrap(), absolute);
        assert_eq!(storage_root(Some(OsStr::new("")), Some(OsStr::new("~/q")), &home).unwrap(), home.join("q"));
        assert_eq!(storage_root(None, None, &home).unwrap(), home.join(".qwen"));
        assert_eq!(storage_root(None, Some(OsStr::new("~")), &home).unwrap(), home);
        assert!(matches!(storage_root(Some(OsStr::new("rel")), None, &home), Err(ArchiveError::Unsupported(_))));
    }
}
```

Проверка в `rewritten`: ключи `serde_json::Map` упорядочены (`BTreeMap`, без `preserve_order`), поэтому `{"cwd","uuid"}` выводится в том же порядке; если в `Cargo.lock` у `serde_json` включён `preserve_order` (через `indexmap`), порядок тоже сохранится. Qwen порядок ключей не важен.

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-qwen --test archive && cargo test -p hub-qwen --lib -- archive`
Expected: PASS.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS. Цикл `for file in files` в `import` — накопление с ранним выходом и проверкой общего каталога; если `clippy` не возражает, оставить (свёртка `try_fold` с двумя аккумуляторами читается хуже).

- [ ] **Step 6: Commit**

```bash
git add crates/hub-qwen Cargo.lock
git commit -m "hub-qwen: выгрузка и возврат чата с переписыванием cwd для снимков"
```

---

### Task 7: Архив Hermes (`hub-hermes::archive` и встроенный скрипт)

Строки `state.db` переносятся API самого Hermes (раздел «Хранилища агентов», Hermes; расхождения 1–6). Хаб пишет скрипт во временный каталог `0700`, запускает его через Hermes без оболочки, ждёт до 120 с и читает ответ только из файла `--result`.

**Files:**
- Modify: `crates/hub-agent/src/cli.rs:70-78` (добавить `hide_std_window` рядом с `hide_window`)
- Create: `crates/hub-hermes/src/archive.py`, `crates/hub-hermes/src/archive.rs`
- Modify: `crates/hub-hermes/src/lib.rs:6-7` (`pub mod archive;` перед `pub mod backend;`)
- Modify: `crates/hub-hermes/Cargo.toml` (`[dependencies]`: `tempfile.workspace = true`), `Cargo.lock`
- Create: `crates/hub-hermes/tests/fixtures/fake-python` (исполняемый), `crates/hub-hermes/tests/fixtures/stub_hermes/{hermes_constants.py, hermes_state.py, hermes_state_common.py, hermes_cli/__init__.py, hermes_cli/profiles.py}`
- Create: `crates/hub-hermes/tests/archive.rs`, `crates/hub-hermes/tests/archive/launch.rs`, `crates/hub-hermes/tests/archive/script.rs`
- Test: `crates/hub-hermes/src/archive.rs` (`classify`), `crates/hub-hermes/tests/archive/*.rs` (только Unix)

**Interfaces:**
- Produces (`hub_agent::cli`): `hide_std_window(&mut std::process::Command)`.
- Produces (`hub_hermes::archive`): `SCHEMA_VERSION: u32 = 31`, `SESSION_FILE = "hermes-session.json"`; `enum Launch { Launcher(PathBuf), Python(PathBuf) }`; `launch_for(cli: &Path) -> Result<Launch, ArchiveError>`; `classify(cli: &Path, head: &str) -> Result<Launch, ArchiveError>`; `HermesArchive::new(cli: &Path, profile: Option<String>, version: String) -> Result<Self, ArchiveError>`; `impl SessionArchive for HermesArchive`.
- Протокол скрипта: `<op> --result R --schema 31 --session ID [--profile P] [--out F] [--in F --cwd C]`, `op ∈ {present, export, import}`; `R` — JSON `{"ok": true, …}` или `{"ok": false, "error": "schema"|"profile"|"payload"|"import"|"internal", "detail": "…"}`.

- [ ] **Step 1: Скрипт и фикстуры**

`crates/hub-hermes/src/archive.py`:

```python
"""agent-hub snapshot bridge: present / export / import one Hermes ACP session.

Runs inside Hermes's own Python: through the published launcher
(``hermes --run-module cProfile -o <stats> <this file> ...``) or a console script's interpreter
(``python -I <this file> ...``). The hub reads only the ``--result`` file: cProfile swallows
``SystemExit``, so the exit status means nothing.
"""

import argparse
import json
import os
import pathlib
import sqlite3
import traceback


class Refused(Exception):
    def __init__(self, kind, detail):
        super().__init__(detail)
        self.kind = kind
        self.detail = detail


def write_new(path, text):
    """A new private file; never follows or replaces an existing one."""
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as out:
        out.write(text)


def resolve_home(profile):
    """The home ``hermes [--profile P] acp`` would use (hermes_cli/main.py _apply_profile_override)."""
    from hermes_constants import get_default_hermes_root

    env = os.environ.get("HERMES_HOME", "").strip()
    if profile is None and env and pathlib.Path(env).parent.name == "profiles":
        return pathlib.Path(env)
    if profile is None:
        active = get_default_hermes_root() / "active_profile"
        try:
            name = active.read_text(encoding="utf-8-sig").strip() if active.exists() else ""
        except (UnicodeDecodeError, OSError):
            name = ""
        if name and name != "default":
            profile = name
    if profile is None:
        return pathlib.Path(env) if env else get_default_hermes_root()
    from hermes_cli.profiles import resolve_profile_env

    try:
        return pathlib.Path(resolve_profile_env(profile))
    except (FileNotFoundError, ValueError) as error:
        raise Refused("profile", str(error)) from error


def check_schema(expected, db_path):
    from hermes_state_common import SCHEMA_VERSION

    if SCHEMA_VERSION != expected:
        raise Refused("schema", f"code schema {SCHEMA_VERSION}")
    if not db_path.exists():
        return
    conn = sqlite3.connect(db_path.resolve().as_uri() + "?mode=ro", uri=True)
    try:
        row = conn.execute("SELECT version FROM schema_version LIMIT 1").fetchone()
    finally:
        conn.close()
    if row is None or row[0] != expected:
        raise Refused("schema", f"state.db schema {row[0] if row else None}")


def open_db(home, read_only):
    os.environ["HERMES_HOME"] = str(home)
    from hermes_state import SessionDB

    return SessionDB(db_path=home / "state.db", read_only=read_only)


def present(args, home):
    if not (home / "state.db").exists():
        return {"ok": True, "present": False}
    db = open_db(home, read_only=True)
    try:
        return {"ok": True, "present": db.get_session(args.session) is not None}
    finally:
        db.close()


def export(args, home):
    if not (home / "state.db").exists():
        return {"ok": True, "found": False}
    db = open_db(home, read_only=True)
    try:
        payload = db.export_session_lineage(args.session, include_inactive=True)
    finally:
        db.close()
    if not payload:
        return {"ok": True, "found": False}
    write_new(args.out, json.dumps(payload, ensure_ascii=False, default=str))
    return {"ok": True, "found": True, "segments": len(payload.get("segments") or [payload])}


def with_cwd(segment, cwd):
    seg = {k: v for k, v in segment.items() if k not in ("segments", "lineage_session_ids", "timings")}
    seg["cwd"] = cwd
    config = seg.get("model_config")
    if isinstance(config, str) and config:
        try:
            config = json.loads(config)
        except ValueError:
            config = None
    if not isinstance(config, dict):
        config = {}
    config["cwd"] = cwd
    seg["model_config"] = config
    return seg


def import_(args, home):
    with open(args.input, encoding="utf-8") as source:
        payload = json.load(source)
    if not isinstance(payload, dict):
        raise Refused("payload", "payload is not an object")
    segments = payload.get("segments") or [payload]
    if not isinstance(segments, list) or not all(isinstance(s, dict) for s in segments):
        raise Refused("payload", "segments are not objects")
    if args.session not in [str(s.get("id") or "") for s in segments]:
        raise Refused("payload", "the session is not in the payload")
    db = open_db(home, read_only=False)
    try:
        result = db.import_sessions([with_cwd(s, args.cwd) for s in segments])
    finally:
        db.close()
    if not result.get("ok"):
        raise Refused("import", json.dumps(result.get("errors"), ensure_ascii=False)[:2000])
    done = set(result.get("imported_ids") or []) | set(result.get("skipped_ids") or [])
    if args.session not in done:
        raise Refused("import", "the session was not imported")
    return {"ok": True, "imported": result.get("imported", 0), "skipped": result.get("skipped", 0)}


OPERATIONS = {"present": present, "export": export, "import": import_}


def main():
    parser = argparse.ArgumentParser(prog="agent-hub-hermes-snapshot")
    parser.add_argument("op", choices=sorted(OPERATIONS))
    parser.add_argument("--result", required=True)
    parser.add_argument("--schema", type=int, required=True)
    parser.add_argument("--session", required=True)
    parser.add_argument("--profile")
    parser.add_argument("--out")
    parser.add_argument("--in", dest="input")
    parser.add_argument("--cwd")
    args = parser.parse_args()
    if args.op == "export" and not args.out:
        parser.error("export needs --out")
    if args.op == "import" and not (args.input and args.cwd):
        parser.error("import needs --in and --cwd")
    try:
        home = resolve_home(args.profile)
        check_schema(args.schema, home / "state.db")
        result = OPERATIONS[args.op](args, home)
    except Refused as refused:
        result = {"ok": False, "error": refused.kind, "detail": refused.detail}
    except Exception as error:  # reported through the result file; the traceback goes to stderr
        traceback.print_exc()
        result = {"ok": False, "error": "internal", "detail": f"{type(error).__name__}: {error}"[:2000]}
    write_new(args.result, json.dumps(result, ensure_ascii=False))


if __name__ == "__main__":
    main()
```

`crates/hub-hermes/tests/fixtures/fake-python` (затем `chmod +x`, git сохраняет бит):

```sh
#!/bin/sh
# Stands in for Hermes's Python: records its arguments and answers as the `mode` file beside
# the symlink says. Checked in once (per-test executables race with spawns: ETXTBSY).
dir=$(dirname "$0")
mode=$(cat "$dir/mode")
printf '%s\n' "$@" > "$dir/argv.txt"
result=
out=
while [ $# -gt 0 ]; do
  case "$1" in
    --result) result=$2; shift ;;
    --out) out=$2; shift ;;
  esac
  shift
done
case "$mode" in
  present) printf '{"ok":true,"present":true}' > "$result" ;;
  absent) printf '{"ok":true,"present":false}' > "$result" ;;
  export) printf '{"id":"s-1","segments":[]}' > "$out"; printf '{"ok":true,"found":true}' > "$result" ;;
  missing) printf '{"ok":true,"found":false}' > "$result" ;;
  imported) printf '{"ok":true,"imported":1,"skipped":0}' > "$result" ;;
  schema) printf '{"ok":false,"error":"schema","detail":"code schema 32"}' > "$result" ;;
  silent) echo "Traceback: boom" >&2 ;;
esac
```

Заглушки Hermes для проверки логики скрипта (`crates/hub-hermes/tests/fixtures/stub_hermes/`):

`hermes_constants.py`:

```python
import os
import pathlib


def get_default_hermes_root():
    return pathlib.Path(os.environ["STUB_HERMES_ROOT"])
```

`hermes_state_common.py`:

```python
SCHEMA_VERSION = 31
```

`hermes_cli/__init__.py` — пустой; `hermes_cli/profiles.py`:

```python
import os
import pathlib


def resolve_profile_env(name):
    path = pathlib.Path(os.environ["STUB_HERMES_ROOT"]) / "profiles" / name
    if not path.is_dir():
        raise FileNotFoundError(f"profile {name!r} does not exist")
    return str(path)
```

`hermes_state.py` (хранит сессии в `sessions.json` рядом с `state.db`; повторяет контракт `export_session_lineage`/`import_sessions`):

```python
import json
import pathlib


class SessionDB:
    def __init__(self, db_path=None, read_only=False):
        self.store = pathlib.Path(db_path).with_name("sessions.json")
        self.read_only = read_only
        self.sessions = json.loads(self.store.read_text()) if self.store.exists() else {}

    def close(self):
        pass

    def get_session(self, session_id):
        return self.sessions.get(session_id)

    def export_session_lineage(self, session_id, include_compacted=False, include_inactive=False):
        if session_id not in self.sessions or not include_inactive:
            return None
        lineage = self.sessions[session_id].get("lineage") or [session_id]
        segments = [self.sessions[i] for i in lineage]
        return {**segments[-1], "segments": segments, "lineage_session_ids": lineage}

    def import_sessions(self, sessions):
        assert not self.read_only
        imported = [s["id"] for s in sessions if s["id"] not in self.sessions]
        skipped = [s["id"] for s in sessions if s["id"] in self.sessions]
        for s in sessions:
            self.sessions.setdefault(s["id"], s)
        self.store.write_text(json.dumps(self.sessions))
        return {"ok": True, "imported": len(imported), "skipped": len(skipped),
                "imported_ids": imported, "skipped_ids": skipped, "errors": []}
```

- [ ] **Step 2: Тесты (RED)**

`crates/hub-hermes/tests/archive.rs`:

```rust
#![cfg(unix)]
#![allow(clippy::unwrap_used)]
#[path = "archive/launch.rs"]
mod launch;
#[path = "archive/script.rs"]
mod script;
```

`crates/hub-hermes/tests/archive/launch.rs` — Rust-сторона протокола через поддельный Python:

```rust
use std::fs;
use std::path::{Path, PathBuf};

use hub_agent::archive::{ArchiveError, SessionArchive};
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, RelPath};
use hub_hermes::archive::{HermesArchive, SESSION_FILE};

struct Fake {
    dir: tempfile::TempDir,
    cli: PathBuf,
}

/// A `hermes` console script whose shebang names the checked-in fake Python.
fn fake(mode: &str) -> Fake {
    let dir = tempfile::tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-python");
    let python = dir.path().join("python3");
    std::os::unix::fs::symlink(fixture, &python).unwrap();
    fs::write(dir.path().join("mode"), mode).unwrap();
    let cli = dir.path().join("hermes");
    fs::write(&cli, format!("#!{}\nfrom hermes_cli.main import main\n", python.display())).unwrap();
    Fake { dir, cli }
}

impl Fake {
    fn archive(&self, profile: Option<&str>) -> HermesArchive {
        HermesArchive::new(&self.cli, profile.map(str::to_owned), "0.21.5".to_owned()).unwrap()
    }

    fn argv(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join("argv.txt")).unwrap().lines().map(str::to_owned).collect()
    }
}

fn id() -> SessionId {
    SessionId::parse("s-1").unwrap()
}

fn cwd() -> AbsolutePath {
    AbsolutePath::new(std::env::temp_dir()).unwrap()
}

#[test]
fn present_runs_the_script_with_a_list_of_arguments() {
    let fake = fake("present");
    let data = BackendData::Hermes { profile: None };
    assert!(fake.archive(Some("work")).present(&id(), &cwd(), &data).unwrap());
    let argv = fake.argv();
    assert_eq!(argv.first().map(String::as_str), Some("-I"));
    assert!(argv.get(1).is_some_and(|script| script.ends_with("agent_hub_snapshot.py")));
    assert_eq!(argv.get(2).map(String::as_str), Some("present"));
    for pair in [["--schema", "31"], ["--session", "s-1"], ["--profile", "work"]] {
        assert!(argv.windows(2).any(|window| window == pair), "{pair:?} in {argv:?}");
    }
}

#[test]
fn absent_sessions_are_not_present() {
    let fake = fake("absent");
    let data = BackendData::Hermes { profile: None };
    assert!(!fake.archive(None).present(&id(), &cwd(), &data).unwrap());
    assert!(!fake.argv().iter().any(|arg| arg == "--profile"));
}

#[test]
fn export_lists_the_one_session_file() {
    let fake = fake("export");
    let dest = tempfile::tempdir().unwrap();
    let exported = fake.archive(Some("work")).export(&id(), &cwd(), dest.path()).unwrap();
    assert_eq!(exported.files, [RelPath::parse(SESSION_FILE).unwrap()]);
    assert_eq!(exported.data, BackendData::Hermes { profile: Some("work".to_owned()) });
    assert!(dest.path().join(SESSION_FILE).is_file());
}

#[test]
fn a_missing_session_is_not_found() {
    let dest = tempfile::tempdir().unwrap();
    assert!(matches!(fake("missing").archive(None).export(&id(), &cwd(), dest.path()), Err(ArchiveError::NotFound)));
}

#[test]
fn another_schema_names_the_hermes_version() {
    let data = BackendData::Hermes { profile: None };
    let error = fake("schema").archive(None).present(&id(), &cwd(), &data).unwrap_err();
    assert_eq!(error.to_string(), "Hermes 0.21.5 не поддерживается для переноса");
}

#[test]
fn no_result_file_is_a_failure_not_a_guess() {
    let data = BackendData::Hermes { profile: None };
    assert!(matches!(
        fake("silent").archive(None).present(&id(), &cwd(), &data),
        Err(ArchiveError::Unsupported(_))
    ));
}

#[test]
fn import_passes_the_new_directory_and_accepts_only_the_session_file() {
    let fake = fake("imported");
    let snapshot = tempfile::tempdir().unwrap();
    fs::write(snapshot.path().join(SESSION_FILE), "{}").unwrap();
    let data = BackendData::Hermes { profile: None };
    let files = [RelPath::parse(SESSION_FILE).unwrap()];
    fake.archive(None).import(&id(), &data, &files, snapshot.path(), &cwd()).unwrap();
    let argv = fake.argv();
    assert_eq!(argv.get(2).map(String::as_str), Some("import"));
    let dir = cwd().as_path().display().to_string();
    assert!(argv.windows(2).any(|window| window == ["--cwd", dir.as_str()]), "{argv:?}");
    let wrong = [RelPath::parse("other.json").unwrap()];
    assert!(matches!(
        fake.archive(None).import(&id(), &data, &wrong, snapshot.path(), &cwd()),
        Err(ArchiveError::Malformed(_))
    ));
}
```

`crates/hub-hermes/tests/archive/script.rs` — логика `archive.py` на заглушках Hermes (нужен `python3`; без него тест пропускается с сообщением):

```rust
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

fn crate_path(raw: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(raw)
}

const HARNESS: &str = "import runpy, sys\n\
stubs, script = sys.argv[1], sys.argv[2]\n\
sys.path.insert(0, stubs)\n\
sys.argv = [script] + sys.argv[3:]\n\
runpy.run_path(script, run_name='__main__')\n";

fn python_available() -> bool {
    Command::new("python3").arg("--version").output().is_ok_and(|output| output.status.success())
}

fn schema_db(home: &Path, version: u32) {
    fs::create_dir_all(home).unwrap();
    let status = Command::new("python3")
        .args(["-c", "import sqlite3, sys\nc = sqlite3.connect(sys.argv[1])\nc.execute('create table schema_version (version int)')\nc.execute('insert into schema_version values (?)', (int(sys.argv[2]),))\nc.commit()\n"])
        .arg(home.join("state.db"))
        .arg(version.to_string())
        .status()
        .unwrap();
    assert!(status.success());
}

/// Runs the embedded script against the stubs and returns its result file.
fn run(root: &Path, args: &[&str]) -> Value {
    let result = root.join(format!("result-{}.json", uuid_like(args)));
    let status = Command::new("python3")
        .arg("-I")
        .arg("-c")
        .arg(HARNESS)
        .arg(crate_path("tests/fixtures/stub_hermes"))
        .arg(crate_path("src/archive.py"))
        .args(args)
        .arg("--result")
        .arg(&result)
        .arg("--schema")
        .arg("31")
        .env("STUB_HERMES_ROOT", root)
        .env_remove("HERMES_HOME")
        .status()
        .unwrap();
    assert!(status.success());
    serde_json::from_str(&fs::read_to_string(result).unwrap()).unwrap()
}

/// Distinct result names per call inside one test.
fn uuid_like(args: &[&str]) -> String {
    args.join("-").replace(['/', '\\', ':', '.'], "_")
}

fn seeded_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    schema_db(root.path(), 31);
    let sessions = json!({
        "s-0": {"id": "s-0", "source": "acp", "cwd": "/old", "model_config": "{\"cwd\": \"/old\", \"provider\": \"p\"}", "messages": [{"role": "user", "content": "hi"}]},
        "s-1": {"id": "s-1", "source": "acp", "parent_session_id": "s-0", "lineage": ["s-0", "s-1"], "cwd": "/old", "model_config": {"cwd": "/old"}, "messages": []},
    });
    fs::write(root.path().join("sessions.json"), sessions.to_string()).unwrap();
    root
}

#[test]
fn a_lineage_moves_to_another_profile_with_the_new_directory() {
    if !python_available() {
        eprintln!("python3 not found: script test skipped");
        return;
    }
    let root = seeded_root();
    let out = root.path().join("out.json");
    let exported = run(root.path(), &["export", "--session", "s-1", "--out", out.to_str().unwrap()]);
    assert_eq!(exported, json!({"ok": true, "found": true, "segments": 2}));

    let work = root.path().join("profiles").join("work");
    schema_db(&work, 31);
    let imported = run(
        root.path(),
        &["import", "--session", "s-1", "--profile", "work", "--in", out.to_str().unwrap(), "--cwd", "/new"],
    );
    assert_eq!(imported, json!({"ok": true, "imported": 2, "skipped": 0}));

    let moved: Value = serde_json::from_str(&fs::read_to_string(work.join("sessions.json")).unwrap()).unwrap();
    assert_eq!(moved["s-0"]["cwd"], "/new");
    assert_eq!(moved["s-0"]["model_config"], json!({"cwd": "/new", "provider": "p"}));
    assert_eq!(moved["s-1"]["model_config"], json!({"cwd": "/new"}));
    assert_eq!(moved["s-0"]["source"], "acp");
    assert!(moved["s-1"].get("segments").is_none());

    let present = run(root.path(), &["present", "--session", "s-1", "--profile", "work"]);
    assert_eq!(present, json!({"ok": true, "present": true}));
}

#[test]
fn the_sticky_active_profile_is_honoured() {
    if !python_available() {
        return;
    }
    let root = seeded_root();
    let work = root.path().join("profiles").join("work");
    schema_db(&work, 31);
    fs::write(root.path().join("active_profile"), "work\n").unwrap();
    assert_eq!(run(root.path(), &["present", "--session", "s-1"]), json!({"ok": true, "present": false}));
}

#[test]
fn another_schema_or_a_missing_profile_is_refused() {
    if !python_available() {
        return;
    }
    let old = tempfile::tempdir().unwrap();
    schema_db(old.path(), 30);
    assert_eq!(run(old.path(), &["present", "--session", "s-1"])["error"], "schema");
    let root = seeded_root();
    assert_eq!(run(root.path(), &["present", "--session", "s-1", "--profile", "ghost"])["error"], "profile");
}
```

В `crates/hub-hermes/src/archive.rs` — сначала только модуль тестов `classify` (реализация — шаг 4):

```rust
#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::pip("#!/home/me/.venv/bin/python3\nimport sys\n", Some(Launch::Python(PathBuf::from("/home/me/.venv/bin/python3"))))]
    #[case::pip_flags("#!/opt/py/bin/python3.12 -E\n", Some(Launch::Python(PathBuf::from("/opt/py/bin/python3.12"))))]
    #[case::long_path_shim("#!/bin/sh\n'''exec' \"/very long/bin/python\" \"$0\" \"$@\"\n' '''\n", Some(Launch::Python(PathBuf::from("/very long/bin/python"))))]
    #[case::bare_shim("#!/bin/sh\n'''exec' /venv/bin/python \"$0\" \"$@\"\n", Some(Launch::Python(PathBuf::from("/venv/bin/python"))))]
    #[case::launcher("#!/bin/sh\nexec /store/bin/python3 -I -c 'import os' \"$@\"\n", Some(Launch::Launcher(PathBuf::from("/bin/hermes"))))]
    #[case::forwarder("#!/bin/sh\nexec /root/.hermes/bin/hermes \"$@\"\n", Some(Launch::Launcher(PathBuf::from("/bin/hermes"))))]
    #[case::env_python("#!/usr/bin/env python3\n", None)]
    #[case::binary("\u{7f}ELF\u{2}\u{1}", None)]
    #[case::other_shell_script("#!/bin/sh\necho hi\n", None)]
    fn launch_comes_from_the_hermes_file(#[case] head: &str, #[case] expected: Option<Launch>) {
        assert_eq!(classify(Path::new("/bin/hermes"), head).ok(), expected);
    }
}
```

- [ ] **Step 3: Проверить RED**

Run: `cargo test -p hub-hermes --test archive && cargo test -p hub-hermes --lib -- archive`
Expected: FAIL — `unresolved import hub_hermes::archive` / нет `classify`, `Launch`. (Тесты скрипта на заглушках к этому моменту уже могут проходить — они проверяют `archive.py`, а не Rust.)

- [ ] **Step 4: Реализация**

`crates/hub-agent/src/cli.rs`, после `hide_window`:

```rust
/// The same for a blocking child started from `spawn_blocking`.
#[cfg(windows)]
pub fn hide_std_window(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_std_window(_command: &mut std::process::Command) {}
```

`crates/hub-hermes/Cargo.toml`, `[dependencies]`: `tempfile.workspace = true`; `cargo check -p hub-hermes --all-targets`; `git diff --stat Cargo.lock`.

`crates/hub-hermes/src/lib.rs`: `pub mod archive;` перед `pub mod backend;`.

`crates/hub-hermes/src/archive.rs` (над тестами):

```rust
//! Hermes keeps sessions as rows of `state.db`; they move through Hermes's own `SessionDB`
//! export and import, run by an embedded script inside Hermes's Python. No shell: arguments
//! are a list, data travel as files in a private temporary directory, the answer only through
//! the result file.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use hub_agent::archive::{
    ArchiveError, Exported, SessionArchive, file_stem, is_regular_file, private_file,
};
use hub_agent::cli::hide_std_window;
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::render::truncate;
use hub_core::snapshot::{BackendData, RelPath};
use serde_json::Value;

/// The `state.db` schema the script was checked against (`hermes_state_common.SCHEMA_VERSION`).
pub const SCHEMA_VERSION: u32 = 31;
pub const SESSION_FILE: &str = "hermes-session.json";
const SCRIPT: &str = include_str!("archive.py");
const SCRIPT_NAME: &str = "agent_hub_snapshot.py";
const TIMEOUT: Duration = Duration::from_mins(2);
const POLL: Duration = Duration::from_millis(50);
const RESULT_LIMIT: u64 = 1024 * 1024;
const HEAD_LIMIT: u64 = 4096;
const STDERR_TAIL: usize = 2048;
const DETAIL_LIMIT: usize = 300;

/// How to run a Python file inside Hermes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// The published launcher: `hermes --run-module cProfile -o <stats> <script> …`.
    Launcher(PathBuf),
    /// A classic console script's interpreter: `python -I <script> …`.
    Python(PathBuf),
}

/// Windows: a venv's `python.exe` next to `hermes.exe`, else the launcher; elsewhere the file's
/// first lines decide.
pub fn launch_for(cli: &Path) -> Result<Launch, ArchiveError> {
    let real = fs::canonicalize(cli)?;
    if cfg!(windows) {
        let python = real.with_file_name("python.exe");
        return Ok(if python.is_file() { Launch::Python(python) } else { Launch::Launcher(real) });
    }
    let mut head = Vec::new();
    File::open(&real)?.take(HEAD_LIMIT).read_to_end(&mut head)?;
    classify(&real, &String::from_utf8_lossy(&head))
}

/// Pure: the first lines of a POSIX `hermes` file → how to run Python there.
pub fn classify(cli: &Path, head: &str) -> Result<Launch, ArchiveError> {
    let unknown = || {
        ArchiveError::Unsupported(format!("Не удалось определить Python Hermes по {}", cli.display()))
    };
    let mut lines = head.lines();
    let interpreter =
        lines.next().and_then(|line| line.strip_prefix("#!")).map(str::trim).ok_or_else(unknown)?;
    let program = Path::new(interpreter.split_whitespace().next().ok_or_else(unknown)?);
    let rest: Vec<&str> = lines.collect();
    if program.file_name().is_some_and(|name| name == "sh") {
        if let Some(python) = rest.iter().find_map(|line| shim_python(line)) {
            return Ok(Launch::Python(PathBuf::from(python)));
        }
        return if rest.iter().any(|line| line.starts_with("exec ")) {
            Ok(Launch::Launcher(cli.to_path_buf()))
        } else {
            Err(unknown())
        };
    }
    let python = program
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.starts_with("python"));
    if python && program.is_absolute() { Ok(Launch::Python(program.to_path_buf())) } else { Err(unknown()) }
}

/// pip/distlib's long-path shim: `'''exec' "<python>" "$0" "$@"`.
fn shim_python(line: &str) -> Option<&str> {
    let tail = line.strip_prefix("'''exec' ")?;
    match tail.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"').map(|(python, _)| python),
        None => tail.split_whitespace().next(),
    }
}

pub struct HermesArchive {
    launch: Launch,
    profile: Option<String>,
    version: String,
}

impl HermesArchive {
    pub fn new(cli: &Path, profile: Option<String>, version: String) -> Result<Self, ArchiveError> {
        Ok(Self { launch: launch_for(cli)?, profile, version })
    }

    fn run(&self, op: &str, session: &str, extra: &[(&str, &OsStr)]) -> Result<Value, ArchiveError> {
        let work = tempfile::Builder::new().prefix("agent-hub-hermes-").tempdir()?;
        let script = work.path().join(SCRIPT_NAME);
        private_file(&script)?.write_all(SCRIPT.as_bytes())?;
        let result = work.path().join("result.json");
        let stderr = work.path().join("stderr.txt");
        let mut command = match &self.launch {
            Launch::Launcher(cli) => {
                let mut command = Command::new(cli);
                command.args(["--run-module", "cProfile", "-o"]).arg(work.path().join("profile.out"));
                command
            }
            Launch::Python(python) => {
                let mut command = Command::new(python);
                command.arg("-I");
                command
            }
        };
        command
            .arg(&script)
            .arg(op)
            .arg("--result")
            .arg(&result)
            .arg("--schema")
            .arg(SCHEMA_VERSION.to_string())
            .arg("--session")
            .arg(session);
        if let Some(profile) = &self.profile {
            command.arg("--profile").arg(profile);
        }
        for (flag, value) in extra {
            command.arg(flag).arg(value);
        }
        command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(private_file(&stderr)?);
        hide_std_window(&mut command);
        let status = wait(command.spawn()?)?;
        match read_result(&result)? {
            Some(value) => self.judge(value),
            None => {
                tracing::warn!(%status, op, "Hermes snapshot script left no result");
                tracing::debug!(stderr = %tail_of(&stderr), "Hermes snapshot script stderr");
                Err(ArchiveError::Unsupported("Hermes не выполнил перенос: подробности в логе".to_owned()))
            }
        }
    }

    fn judge(&self, value: Value) -> Result<Value, ArchiveError> {
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            return Ok(value);
        }
        let kind = value.get("error").and_then(Value::as_str).unwrap_or("internal");
        let detail = value.get("detail").and_then(Value::as_str).unwrap_or_default();
        tracing::warn!(kind, "Hermes refused the snapshot operation");
        Err(match kind {
            "schema" => {
                ArchiveError::Unsupported(format!("Hermes {} не поддерживается для переноса", self.version))
            }
            "profile" => ArchiveError::Unsupported("Профиль Hermes из настроек не найден".to_owned()),
            "payload" | "import" => ArchiveError::Malformed(format!(
                "Hermes не принял выгрузку сессии: {}",
                truncate(detail, DETAIL_LIMIT)
            )),
            _ => ArchiveError::Unsupported("Hermes не выполнил перенос: подробности в логе".to_owned()),
        })
    }
}

/// Waits up to `TIMEOUT`, then kills: a wedged Hermes must not hold the topic forever.
fn wait(mut child: Child) -> Result<ExitStatus, ArchiveError> {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            if let Err(error) = child.kill() {
                tracing::warn!(%error, "Hermes snapshot script could not be killed");
            }
            child.wait()?;
            return Err(ArchiveError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "Hermes не ответил за 120 с",
            )));
        }
        std::thread::sleep(POLL);
    }
}

fn read_result(path: &Path) -> Result<Option<Value>, ArchiveError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.len() > RESULT_LIMIT {
        return Err(ArchiveError::Malformed("ответ Hermes не похож на результат".to_owned()));
    }
    serde_json::from_str(&fs::read_to_string(path)?)
        .map(Some)
        .map_err(|_| ArchiveError::Malformed("ответ Hermes — не JSON".to_owned()))
}

fn tail_of(path: &Path) -> String {
    let text = fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default();
    let tail: Vec<char> = text.chars().rev().take(STDERR_TAIL).collect();
    tail.into_iter().rev().collect()
}

impl SessionArchive for HermesArchive {
    fn export(
        &self,
        session: &SessionId,
        _cwd: &AbsolutePath,
        dest: &Path,
    ) -> Result<Exported, ArchiveError> {
        let out = dest.join(SESSION_FILE);
        let value = self.run("export", file_stem(session)?, &[("--out", out.as_os_str())])?;
        if value.get("found").and_then(Value::as_bool) != Some(true) {
            return Err(ArchiveError::NotFound);
        }
        if !is_regular_file(&out)? {
            return Err(ArchiveError::Malformed("Hermes не записал выгрузку".to_owned()));
        }
        Ok(Exported {
            files: vec![RelPath::parse(SESSION_FILE)?],
            data: BackendData::Hermes { profile: self.profile.clone() },
            agent_version: self.version.clone(),
        })
    }

    fn present(
        &self,
        session: &SessionId,
        _cwd: &AbsolutePath,
        _data: &BackendData,
    ) -> Result<bool, ArchiveError> {
        self.run("present", file_stem(session)?, &[])?
            .get("present")
            .and_then(Value::as_bool)
            .ok_or_else(|| ArchiveError::Malformed("Hermes не ответил, есть ли сессия".to_owned()))
    }

    fn import(
        &self,
        session: &SessionId,
        data: &BackendData,
        files: &[RelPath],
        src: &Path,
        cwd: &AbsolutePath,
    ) -> Result<(), ArchiveError> {
        match data {
            BackendData::Hermes { profile } => {
                if *profile != self.profile {
                    tracing::info!(from = ?profile, to = ?self.profile, "Hermes session moves to the configured profile");
                }
            }
            BackendData::Claude { .. } | BackendData::Codex | BackendData::Qwen => {
                return Err(ArchiveError::Malformed("снимок другого бэкенда".to_owned()));
            }
        }
        match files {
            [file] if file.as_str() == SESSION_FILE => {}
            _ => return Err(ArchiveError::Malformed(format!("ожидался один файл {SESSION_FILE}"))),
        }
        let cwd = cwd
            .as_path()
            .to_str()
            .ok_or_else(|| ArchiveError::Unsupported("рабочий каталог не в UTF-8".to_owned()))?;
        let input = src.join(SESSION_FILE);
        self.run(
            "import",
            file_stem(session)?,
            &[("--in", input.as_os_str()), ("--cwd", OsStr::new(cwd))],
        )
        .map(drop)
    }
}
```

Замечания: `judge` сопоставляет внешние строки `&str`, поэтому `_ =>` допустим; `kind` и `detail` об ошибке не содержат истории (поля проверки Hermes), трассировка `stderr` идёт только в `debug`. `tempfile::TempDir` удаляет рабочий каталог при выходе из `run`.

- [ ] **Step 5: Проверить GREEN**

Run: `chmod +x crates/hub-hermes/tests/fixtures/fake-python && cargo test -p hub-hermes --test archive && cargo test -p hub-hermes --lib -- archive && cargo test -p hub-agent --lib`
Expected: PASS (на Windows `tests/archive.rs` не собирается, `classify` проверяется везде).

- [ ] **Step 6: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked && git ls-files -s crates/hub-hermes/tests/fixtures/fake-python`
Expected: PASS; режим файла `100755`.

- [ ] **Step 7: Commit**

```bash
git add crates/hub-agent/src/cli.rs crates/hub-hermes Cargo.lock
git commit -m "hub-hermes: перенос сессии через SessionDB самого Hermes для снимков"
```

---

### Task 8: Настройка `[snapshots] dir` и поле GUI

**Files:**
- Modify: `crates/hub-core/src/settings.rs` — `:15-18` (константа), `:280-293` (`Settings`), `:295-311` (`Field`), `:428-446` (`Draft`), `:448-468` (`Default`), `:470-509` (`Debug`), `:511-604` (`parse`), `:606-685` (`from_settings`), после `:765` (`parse_snapshots_dir`), `:825-846` (`SettingsFile`), после `:903` (`SnapshotsFile`), `:923-985` (`SettingsFile::from_settings`), `:987-1056` (`to_draft`), тесты `:1072-…`
- Modify: `crates/hub-app/src/gui/settings.rs:161-173` (`workspace`), тесты `:455-…`
- Test: `crates/hub-core/src/settings.rs` (`#[cfg(test)]`), `crates/hub-app/src/gui/settings.rs` (`#[cfg(test)]`), `crates/hub-app/src/config.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces (`hub_core::settings`): `DEFAULT_SNAPSHOTS_DIR: &str = ".agent-hub"` (относительно домашнего каталога); `Settings.snapshots_dir: PathBuf` (абсолютный); `Draft.snapshots_dir: String` (пусто — по умолчанию); `Field::SnapshotsDir`; `SettingsFile.snapshots: SnapshotsFile { dir: Option<String> }` с `#[serde(default)]`.
- Consumes: ничего нового. Используется задачами 9–11 (`settings.snapshots_dir`).

Каталог читается из `Settings` на каждую операцию, перезапуск бота не нужен; существование не проверяется (создаётся при первом `/save`).

- [ ] **Step 1: Тесты (RED)**

`crates/hub-core/src/settings.rs`, модуль `tests`:

```rust
    #[test]
    fn snapshots_live_in_agent_hub_under_home_by_default() {
        assert_eq!(draft().parse(&home()).unwrap().snapshots_dir, home().join(".agent-hub"));
    }

    #[rstest]
    #[case::tilde("~/snaps", Ok(()))]
    #[case::relative("snaps", Err(Field::SnapshotsDir))]
    fn snapshots_dir_is_absolute_or_from_home(#[case] raw: &str, #[case] expected: Result<(), Field>) {
        let parsed = Draft { snapshots_dir: raw.to_owned(), ..draft() }.parse(&home());
        match expected {
            Ok(()) => assert_eq!(parsed.unwrap().snapshots_dir, home().join("snaps")),
            Err(field) => assert_eq!(parsed.unwrap_err().iter().map(|e| e.field).collect::<Vec<_>>(), [field]),
        }
    }

    #[test]
    fn a_file_without_snapshots_section_still_loads() {
        let file: SettingsFile = toml::from_str(
            "[telegram]\nchat = -100\nusers = [1]\n[workspace]\nroot = \"~/Projects\"\n",
        )
        .unwrap();
        let keys = Keys { token: "1:a".to_owned(), api_key: String::new(), qwen_key: String::new() };
        let settings = file.to_draft(keys).unwrap().parse(&home()).unwrap();
        assert_eq!(settings.snapshots_dir, home().join(".agent-hub"));
    }

    #[test]
    fn snapshots_dir_round_trips_through_the_file() {
        let settings = Draft { snapshots_dir: "~/snaps".to_owned(), ..draft() }.parse(&home()).unwrap();
        let file = SettingsFile::from_settings(&settings);
        let keys = Keys { token: "123:abc".to_owned(), api_key: String::new(), qwen_key: String::new() };
        assert_eq!(file.to_draft(keys).unwrap().parse(&home()).unwrap(), settings);
    }
```

`crates/hub-app/src/gui/settings.rs`, модуль `tests` — по образцу `hermes_settings_are_visible` (`:457-481`):

```rust
    #[test]
    fn snapshots_directory_is_visible() {
        let ctx = egui::Context::default();
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(960.0, 4000.0));
        let mut form = SettingsForm::default();
        let mut output =
            ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| {
                egui::CentralPanel::default().show(ui, |ui| {
                    form.show(ui, Path::new("/"), 0, &mut |_| {});
                });
            });
        output.textures_delta.clear();
        assert!(output.shapes.iter().any(|clipped| matches!(
            &clipped.shape,
            egui::Shape::Text(text) if text.galley.text() == "Каталог снимков"
        )));
    }
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-core --lib -- settings && cargo test -p hub-app --lib -- gui::settings`
Expected: FAIL — нет поля `snapshots_dir`, варианта `Field::SnapshotsDir`; в GUI нет «Каталог снимков».

- [ ] **Step 3: Реализация**

`crates/hub-core/src/settings.rs`:

- после `MAX_TIMEOUT`: `pub const DEFAULT_SNAPSHOTS_DIR: &str = ".agent-hub";`
- `Settings`: после `workspace_root` — `pub snapshots_dir: PathBuf,`
- `Field`: после `WorkspaceRoot` — `SnapshotsDir,`
- `Draft`: после `workspace_root` — `pub snapshots_dir: String,`; в `Default` — `snapshots_dir: String::new(),`; в `Debug` — поле в деструктуризации и `.field("snapshots_dir", snapshots_dir)`.
- `parse`: после `root` — `let snapshots = check(&mut errors, Field::SnapshotsDir, parse_snapshots_dir(&self.snapshots_dir, home));`, добавить `Some(snapshots)` в кортеж `let (…) = (…) else` и `snapshots_dir: snapshots,` в литерал `Settings`.
- `from_settings` (черновик): `snapshots_dir: settings.snapshots_dir.display().to_string(),`
- новая функция рядом с `parse_root`:

```rust
/// Empty means `~/.agent-hub`; otherwise absolute or from `~`, like the workspace root.
fn parse_snapshots_dir(raw: &str, home: &Path) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() { Ok(home.join(DEFAULT_SNAPSHOTS_DIR)) } else { absolute(trimmed, home) }
}
```

- файл:

```rust
    #[serde(default)]
    pub snapshots: SnapshotsFile,
```

в `SettingsFile` после `hermes`, и

```rust
/// Where `/save` writes snapshots; absent means `~/.agent-hub`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotsFile {
    pub dir: Option<String>,
}
```

- `SettingsFile::from_settings`: `snapshots: SnapshotsFile { dir: Some(settings.snapshots_dir.display().to_string()) },`
- `to_draft`: `snapshots_dir: self.snapshots.dir.clone().unwrap_or_default(),`

`crates/hub-app/src/gui/settings.rs`, в `workspace` после `messages(ui, errors, Field::WorkspaceRoot);`:

```rust
        ui.horizontal(|ui| {
            ui.label("Каталог снимков");
            ui.add(
                egui::TextEdit::singleline(&mut self.draft.snapshots_dir).hint_text("~/.agent-hub"),
            );
            if ui.button("Выбрать…").clicked()
                && let Some(path) = pick_folder(&self.draft.snapshots_dir)
            {
                self.draft.snapshots_dir = path;
            }
        });
        messages(ui, errors, Field::SnapshotsDir);
```

Остальные места, собирающие `Draft`/`Settings` (`rg -n "Draft \{" crates`), используют `..Draft::default()` и правок не требуют; `Settings` строится только в `Draft::parse`.

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-core --lib -- settings && cargo test -p hub-app --lib -- gui::settings config`
Expected: PASS; существующие `saved_settings_load_back` и `*_round_trip*` проходят с новым полем.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-core/src/settings.rs crates/hub-app/src/gui/settings.rs
git commit -m "Настройка каталога снимков и поле в окне настроек"
```

---

### Task 9: `SnapshotStore` — атомарная запись, список, загрузка (`hub-telegram::snapshots`)

**Files:**
- Create: `crates/hub-telegram/src/snapshots.rs`
- Modify: `crates/hub-telegram/src/lib.rs:12-13` (`pub mod snapshots;` после `mod session;`)
- Create: `crates/hub-telegram/tests/snapshots.rs`, `crates/hub-telegram/tests/snapshots/store.rs`
- Test: `crates/hub-telegram/tests/snapshots/store.rs`

**Interfaces:**
- Produces (`hub_telegram::snapshots`):
  - `SnapshotStore::new(dir: &Path) -> Self` (корень — `<dir>/snapshots`), `#[derive(Debug, Clone)]`.
  - `save(&self, name: &SnapshotName, fill: impl FnOnce(&Path) -> Result<Manifest, ArchiveError>) -> Result<Manifest, SnapshotError>` — создаёт `snapshots/.tmp-<uuid>/agent/` (`0700`), вызывает `fill(agent)`, пишет `manifest.json` (`0600`, `sync_all`), переименовывает в `snapshots/<name>/`; любая ошибка удаляет staging; staging старше суток (после сбоя питания) убирается при следующем `save`.
  - `list(&self) -> Result<Vec<Manifest>, SnapshotError>` — новые сверху (по `created_at`, затем по имени); повреждённые и служебные каталоги пропускаются с `warn`; нет каталога — пустой список.
  - `load(&self, name: &SnapshotName) -> Result<Loaded, SnapshotError>`; `pub struct Loaded { pub manifest: Manifest, pub agent: PathBuf }` — проверены: каталог не ссылка, имя в манифесте совпадает, каждый файл из `files` — обычный файл в `agent/`.
  - `pub enum SnapshotError { Exists(SnapshotName), Missing(SnapshotName), Corrupt { name: SnapshotName, reason: String }, Io(io::Error), Archive(ArchiveError) }`; тексты: «Снимок <имя> уже существует», «Снимок <имя> не найден», «Снимок <имя> повреждён: …», «Каталог снимков недоступен: …», `Archive` — прозрачно.
- Consumes: `hub_agent::archive::{ArchiveError, private_dirs, private_file, restrict_dir, is_real_dir, is_regular_file}`, `hub_core::snapshot::{Manifest, SnapshotName}`.

- [ ] **Step 1: Тесты (RED)**

`crates/hub-telegram/tests/snapshots.rs`:

```rust
#![allow(clippy::unwrap_used)]
#[path = "snapshots/store.rs"]
mod store;
```

`crates/hub-telegram/tests/snapshots/store.rs`:

```rust
use std::fs;
use std::io::Write;
use std::path::Path;

use hub_agent::archive::{ArchiveError, private_dirs, private_file};
use hub_core::domain::{AbsolutePath, SessionId};
use hub_core::snapshot::{BackendData, Manifest, RelPath, SnapshotName};
use hub_telegram::snapshots::{SnapshotError, SnapshotStore};

fn name(raw: &str) -> SnapshotName {
    SnapshotName::parse(raw).unwrap()
}

fn manifest(raw: &str, created_at: &str) -> Manifest {
    Manifest {
        name: name(raw),
        created_at: created_at.to_owned(),
        title: None,
        cwd: AbsolutePath::new(std::env::temp_dir()).unwrap(),
        session: SessionId::parse("s-1").unwrap(),
        agent_version: "0.160.0".to_owned(),
        data: BackendData::Codex,
        files: vec![RelPath::parse("fake/s-1.txt").unwrap()],
    }
}

/// What an archive does: writes its file into `agent/` and describes it.
fn fill(raw: &str, created_at: &str) -> impl FnOnce(&Path) -> Result<Manifest, ArchiveError> {
    let manifest = manifest(raw, created_at);
    move |agent: &Path| {
        private_dirs(&agent.join("fake"))?;
        private_file(&agent.join("fake").join("s-1.txt"))?.write_all(b"history")?;
        Ok(manifest)
    }
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn a_saved_snapshot_loads_back_with_its_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());

    let saved = store.save(&name("snap"), fill("snap", "2026-10-09T15:30:12Z")).unwrap();
    let loaded = store.load(&name("snap")).unwrap();

    assert_eq!(loaded.manifest, saved);
    assert_eq!(fs::read_to_string(loaded.agent.join("fake").join("s-1.txt")).unwrap(), "history");
    assert_eq!(entries(&dir.path().join("snapshots")), ["snap"]);
    assert_eq!(entries(&dir.path().join("snapshots").join("snap")), ["agent", "manifest.json"]);
}

#[test]
fn a_failed_export_leaves_no_trace() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    let result = store.save(&name("snap"), |agent: &Path| {
        fs::write(agent.join("partial"), "x")?;
        Err(ArchiveError::NotFound)
    });
    assert!(matches!(result, Err(SnapshotError::Archive(ArchiveError::NotFound))));
    assert!(entries(&dir.path().join("snapshots")).is_empty());
}

#[test]
fn a_taken_name_is_refused_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    store.save(&name("snap"), fill("snap", "2026-10-09T15:30:12Z")).unwrap();
    let again = store.save(&name("snap"), fill("snap", "2026-10-10T00:00:00Z"));
    assert!(matches!(again, Err(SnapshotError::Exists(_))));
    assert_eq!(store.load(&name("snap")).unwrap().manifest.created_at, "2026-10-09T15:30:12Z");
    assert_eq!(SnapshotError::Exists(name("snap")).to_string(), "Снимок snap уже существует");
}

#[test]
fn the_list_is_newest_first_and_skips_what_is_not_a_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    assert!(store.list().unwrap().is_empty());
    store.save(&name("old"), fill("old", "2026-10-01T10:00:00Z")).unwrap();
    store.save(&name("new"), fill("new", "2026-10-09T10:00:00Z")).unwrap();
    let root = dir.path().join("snapshots");
    fs::create_dir(root.join("broken")).unwrap();
    fs::write(root.join("broken").join("manifest.json"), "{garbage").unwrap();
    fs::create_dir(root.join(".tmp-leftover")).unwrap();
    fs::create_dir(root.join("Bad Name")).unwrap();
    fs::write(root.join("stray.txt"), "x").unwrap();

    let names: Vec<String> = store.list().unwrap().into_iter().map(|m| m.name.as_str().to_owned()).collect();

    assert_eq!(names, ["new", "old"]);
}

#[test]
fn loading_checks_name_presence_and_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    assert!(matches!(store.load(&name("nope")), Err(SnapshotError::Missing(_))));
    assert_eq!(SnapshotError::Missing(name("nope")).to_string(), "Снимок nope не найден");

    store.save(&name("snap"), fill("snap", "2026-10-09T15:30:12Z")).unwrap();
    let root = dir.path().join("snapshots");
    fs::rename(root.join("snap"), root.join("renamed")).unwrap();
    assert!(matches!(store.load(&name("renamed")), Err(SnapshotError::Corrupt { .. })));

    fs::remove_file(root.join("renamed").join("agent").join("fake").join("s-1.txt")).unwrap();
    fs::rename(root.join("renamed"), root.join("snap")).unwrap();
    assert!(matches!(store.load(&name("snap")), Err(SnapshotError::Corrupt { .. })));
}

#[cfg(unix)]
#[test]
fn snapshots_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    SnapshotStore::new(dir.path()).save(&name("snap"), fill("snap", "2026-10-09T15:30:12Z")).unwrap();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let snap = dir.path().join("snapshots").join("snap");
    assert_eq!(
        [
            mode(&dir.path().join("snapshots")),
            mode(&snap),
            mode(&snap.join("agent")),
            mode(&snap.join("manifest.json")),
            mode(&snap.join("agent").join("fake").join("s-1.txt")),
        ],
        [0o700, 0o700, 0o700, 0o600, 0o600]
    );
}

#[cfg(unix)]
#[test]
fn a_linked_snapshot_directory_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path());
    SnapshotStore::new(elsewhere.path()).save(&name("snap"), fill("snap", "2026-10-09T15:30:12Z")).unwrap();
    fs::create_dir_all(dir.path().join("snapshots")).unwrap();
    std::os::unix::fs::symlink(
        elsewhere.path().join("snapshots").join("snap"),
        dir.path().join("snapshots").join("snap"),
    )
    .unwrap();
    assert!(matches!(store.load(&name("snap")), Err(SnapshotError::Corrupt { .. })));
    assert!(store.list().unwrap().is_empty());
}
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-telegram --test snapshots`
Expected: FAIL — `unresolved import hub_telegram::snapshots`.

- [ ] **Step 3: Реализация**

`crates/hub-telegram/src/lib.rs`: `pub mod snapshots;` после `mod session;`.

`crates/hub-telegram/src/snapshots.rs`:

```rust
//! Snapshots on disk: `<dir>/snapshots/<name>/{manifest.json, agent/…}`. A snapshot is
//! written in a private staging directory and renamed into place, so it is whole or absent.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use hub_agent::archive::{
    ArchiveError, is_real_dir, is_regular_file, private_dirs, private_file, restrict_dir,
};
use hub_core::snapshot::{Manifest, ManifestError, SnapshotName};

const SNAPSHOTS: &str = "snapshots";
const MANIFEST: &str = "manifest.json";
const AGENT: &str = "agent";
const STAGING_PREFIX: &str = ".tmp-";
const MANIFEST_LIMIT: u64 = 1024 * 1024;
/// Staging older than this was left by a crash; a live save finishes in minutes.
const STALE_STAGING: Duration = Duration::from_hours(24);

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("Снимок {0} уже существует")]
    Exists(SnapshotName),
    #[error("Снимок {0} не найден")]
    Missing(SnapshotName),
    #[error("Снимок {name} повреждён: {reason}")]
    Corrupt { name: SnapshotName, reason: String },
    #[error("Каталог снимков недоступен: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Archive(#[from] ArchiveError),
}

pub struct Loaded {
    pub manifest: Manifest,
    /// The snapshot's `agent/` directory, which `manifest.files` are relative to.
    pub agent: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SnapshotStore {
    root: PathBuf,
}

/// Removes the staging directory unless it was renamed into place.
struct Staging {
    path: PathBuf,
    committed: bool,
}

impl Staging {
    fn create(root: &Path) -> io::Result<Self> {
        let path = root.join(format!("{STAGING_PREFIX}{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir(&path)?;
        restrict_dir(&path)?;
        Ok(Self { path, committed: false })
    }

    fn commit(mut self, target: &Path) -> io::Result<()> {
        fs::rename(&self.path, target)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.committed
            && let Err(error) = fs::remove_dir_all(&self.path)
        {
            tracing::warn!(path = %self.path.display(), %error, "snapshot staging left behind");
        }
    }
}

impl SnapshotStore {
    #[must_use]
    pub fn new(dir: &Path) -> Self {
        Self { root: dir.join(SNAPSHOTS) }
    }

    pub fn save(
        &self,
        name: &SnapshotName,
        fill: impl FnOnce(&Path) -> Result<Manifest, ArchiveError>,
    ) -> Result<Manifest, SnapshotError> {
        private_dirs(&self.root)?;
        restrict_dir(&self.root)?;
        self.sweep();
        let target = self.root.join(name.as_str());
        if fs::symlink_metadata(&target).is_ok() {
            return Err(SnapshotError::Exists(name.clone()));
        }
        let staging = Staging::create(&self.root)?;
        let agent = staging.path.join(AGENT);
        private_dirs(&agent)?;
        let manifest = fill(&agent)?;
        if manifest.name != *name {
            return Err(SnapshotError::Corrupt { name: name.clone(), reason: "имя в манифесте другое".to_owned() });
        }
        let text = manifest.dump().map_err(|error| corrupt(name, &error))?;
        let mut file = private_file(&staging.path.join(MANIFEST))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        // A concurrent save of the same name is refused by the hub; this narrows what is left.
        if fs::symlink_metadata(&target).is_ok() {
            return Err(SnapshotError::Exists(name.clone()));
        }
        staging.commit(&target)?;
        sync_dir(&self.root);
        tracing::info!(snapshot = name.as_str(), files = manifest.files.len(), "snapshot saved");
        Ok(manifest)
    }

    pub fn list(&self) -> Result<Vec<Manifest>, SnapshotError> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut manifests: Vec<Manifest> = entries
            .filter_map(|entry| {
                let raw = entry.ok()?.file_name().to_str()?.to_owned();
                if raw.starts_with('.') {
                    return None;
                }
                let name = SnapshotName::parse(&raw).ok()?;
                self.read(&name)
                    .map_err(|error| tracing::warn!(snapshot = %raw, %error, "snapshot skipped"))
                    .ok()
            })
            .collect();
        manifests.sort_by(|a, b| b.created_at.cmp(&a.created_at).then_with(|| a.name.cmp(&b.name)));
        Ok(manifests)
    }

    pub fn load(&self, name: &SnapshotName) -> Result<Loaded, SnapshotError> {
        let manifest = self.read(name)?;
        let agent = self.root.join(name.as_str()).join(AGENT);
        if let Some(missing) = manifest
            .files
            .iter()
            .find(|file| !is_regular_file(&file.under(&agent)).unwrap_or(false))
        {
            return Err(SnapshotError::Corrupt { name: name.clone(), reason: format!("нет файла {missing}") });
        }
        Ok(Loaded { manifest, agent })
    }

    fn read(&self, name: &SnapshotName) -> Result<Manifest, SnapshotError> {
        let dir = self.root.join(name.as_str());
        match fs::symlink_metadata(&dir) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(SnapshotError::Missing(name.clone()));
            }
            Err(error) => return Err(error.into()),
            Ok(_) if !is_real_dir(&dir)? => {
                return Err(SnapshotError::Corrupt { name: name.clone(), reason: "не каталог".to_owned() });
            }
            Ok(_) => {}
        }
        let path = dir.join(MANIFEST);
        if !is_regular_file(&path)? {
            return Err(SnapshotError::Corrupt { name: name.clone(), reason: "нет manifest.json".to_owned() });
        }
        let mut raw = String::new();
        File::open(&path)?.take(MANIFEST_LIMIT).read_to_string(&mut raw)?;
        let manifest = Manifest::parse(&raw).map_err(|error| corrupt(name, &error))?;
        if manifest.name != *name {
            return Err(SnapshotError::Corrupt {
                name: name.clone(),
                reason: format!("в манифесте имя {}", manifest.name),
            });
        }
        Ok(manifest)
    }

    /// Staging directories a crash left behind; failures only cost disk space.
    fn sweep(&self) {
        let Ok(entries) = fs::read_dir(&self.root) else { return };
        for entry in entries.flatten() {
            let stale = entry.file_name().to_str().is_some_and(|name| name.starts_with(STAGING_PREFIX))
                && entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .ok()
                    .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                    .is_some_and(|age| age > STALE_STAGING);
            if stale && let Err(error) = fs::remove_dir_all(entry.path()) {
                tracing::warn!(path = %entry.path().display(), %error, "stale snapshot staging left behind");
            }
        }
    }
}

fn corrupt(name: &SnapshotName, error: &ManifestError) -> SnapshotError {
    SnapshotError::Corrupt { name: name.clone(), reason: error.to_string() }
}

/// Makes the rename durable on Unix; elsewhere the filesystem gives no handle for it.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Err(error) = File::open(dir).and_then(|dir| dir.sync_all()) {
        tracing::warn!(%error, "snapshot directory not synced");
    }
    #[cfg(not(unix))]
    let _unused = dir;
}
```

Замечания: `is_regular_file(..).unwrap_or(false)` в `load` — ошибка доступа к файлу считается «файла нет» и даёт `Corrupt` (снимок не используется), а не тихую подстановку; `sweep` — лучшее усилие (сбой — только место на диске, пишется `warn`). Если `clippy` отвергает `let _unused = dir;`, разделить `sync_dir` на две `#[cfg]`-функции, как `restrict_dir`.

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-telegram --test snapshots`
Expected: PASS.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-telegram/src/snapshots.rs crates/hub-telegram/src/lib.rs crates/hub-telegram/tests/snapshots.rs crates/hub-telegram/tests/snapshots
git commit -m "hub-telegram: хранилище снимков с атомарной записью"
```

---

### Task 10: `Agents::archive` — архивы бэкендов для хаба

`Hub` получает архив через тот же трейт `Agents`, что и сессии: так тесты хаба подменяют его, а `HubAgents` проверяет CLI и вычисляет корень хранилища из того же окружения, с которым хаб запускает агентов. `HubSetup`/`TelegramConnector` (`crates/hub-app/src/connector.rs:148-157`) не меняются: `home` уже передаётся хабу, настройки — через `watch`.

**Files:**
- Modify: `crates/hub-telegram/src/hub.rs:41-49` (трейт `Agents`), `:633-662` (`FakeAgents` в тестах — временная заглушка)
- Modify: `crates/hub-telegram/src/agents.rs:1-93` (реализация `archive`), тесты `:95-223`
- Test: `crates/hub-telegram/src/agents.rs` (`#[cfg(test)]`)

**Interfaces:**
- Produces (`hub_telegram::hub::Agents`): `fn archive<'a>(&'a self, backend: BackendKind, home: &'a Path) -> BoxFuture<'a, Result<Arc<dyn SessionArchive>, String>>` — ошибка — готовый текст для пользователя (`CliError`/`ArchiveError` через `Display`).
- Consumes: `hub_claude::archive::{ClaudeArchive, storage_root}`, `hub_codex::archive::{CodexArchive, storage_root}`, `hub_qwen::archive::{QwenArchive, storage_root}`, `hub_hermes::archive::HermesArchive`, `*::version::{locate, check}` (задачи 4–7).

- [ ] **Step 1: Тест (RED)**

`crates/hub-telegram/src/agents.rs`, модуль `tests`:

```rust
    #[tokio::test]
    async fn archives_check_the_configured_cli_first() {
        let root = std::env::temp_dir();
        let missing = root.join("definitely-not-an-agent-binary");
        let mut form = Draft {
            token: "1:a".to_owned(),
            chat: "-100".to_owned(),
            users: "1".to_owned(),
            workspace_root: root.display().to_string(),
            ..Draft::default()
        };
        form.cli = missing.display().to_string();
        form.codex.cli = missing.display().to_string();
        form.qwen.cli = missing.display().to_string();
        form.hermes.cli = missing.display().to_string();
        let agents = HubAgents::new(watch::Sender::new(Arc::new(form.parse(&root).unwrap())).subscribe());
        for backend in BackendKind::ALL {
            let Err(reason) = agents.archive(backend, &root).await else {
                panic!("{} archive without its CLI", backend.name());
            };
            assert!(reason.contains("definitely-not-an-agent-binary"), "{}: {reason}", backend.name());
        }
    }
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-telegram --lib -- agents::tests::archives_check_the_configured_cli_first`
Expected: FAIL — нет метода `archive` у `HubAgents`/`Agents`.

- [ ] **Step 3: Реализация**

`crates/hub-telegram/src/hub.rs` — импорты `use std::path::{Path, PathBuf};`, `use hub_agent::archive::SessionArchive;`, `use hub_core::domain::BackendKind;` (в общий `use hub_core::domain::{…}`), и трейт:

```rust
pub trait Agents: Send + Sync + 'static {
    fn run<'a>(
        &'a self,
        session: &'a TopicSession,
        prompt: Prompt,
        inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation,
    ) -> BoxFuture<'a, mpsc::Receiver<Prompt>>;

    /// The backend's session archive once its CLI is found and new enough; the error is the
    /// text shown to the user.
    fn archive<'a>(
        &'a self,
        backend: BackendKind,
        home: &'a Path,
    ) -> BoxFuture<'a, Result<Arc<dyn SessionArchive>, String>>;
}
```

В тестах `hub.rs`, `impl Agents for FakeAgents` — временно (заменяется в задаче 11):

```rust
        fn archive<'a>(
            &'a self,
            _backend: BackendKind,
            _home: &'a Path,
        ) -> BoxFuture<'a, Result<Arc<dyn SessionArchive>, String>> {
            Box::pin(async { Err("архивы в этих тестах не нужны".to_owned()) })
        }
```

`crates/hub-telegram/src/agents.rs`:

```rust
use std::env;
use std::path::Path;

use hub_agent::archive::SessionArchive;
use hub_claude::archive::ClaudeArchive;
use hub_codex::archive::CodexArchive;
use hub_hermes::archive::HermesArchive;
use hub_qwen::archive::QwenArchive;
```

в `impl Agents for HubAgents`:

```rust
    fn archive<'a>(
        &'a self,
        backend: BackendKind,
        home: &'a Path,
    ) -> BoxFuture<'a, Result<Arc<dyn SessionArchive>, String>> {
        let settings = Arc::clone(&self.settings.borrow());
        Box::pin(async move { archive(backend, &settings, home).await })
    }
```

и функция рядом с `refuse`:

```rust
/// The same CLI and environment the backend runs with: the storage root follows the variables
/// the agent itself reads, and the checked version goes into the manifest.
async fn archive(
    backend: BackendKind,
    settings: &Settings,
    home: &Path,
) -> Result<Arc<dyn SessionArchive>, String> {
    let archive: Arc<dyn SessionArchive> = match backend {
        BackendKind::Claude => {
            let cli = hub_claude::version::locate(settings.claude.cli.as_deref())
                .map_err(|error| error.to_string())?;
            let version = hub_claude::version::check(&cli).await.map_err(|error| error.to_string())?;
            let root = hub_claude::archive::storage_root(env::var_os("CLAUDE_CONFIG_DIR").as_deref(), home);
            Arc::new(ClaudeArchive::new(root, version.to_string()))
        }
        BackendKind::Codex => {
            let cli = hub_codex::version::locate(settings.codex.cli.as_deref())
                .map_err(|error| error.to_string())?;
            let version = hub_codex::version::check(&cli).await.map_err(|error| error.to_string())?;
            let root = hub_codex::archive::storage_root(env::var_os("CODEX_HOME").as_deref(), home);
            Arc::new(CodexArchive::new(root, version.to_string()))
        }
        BackendKind::Qwen => {
            let cli = hub_qwen::version::locate(settings.qwen.cli.as_deref())
                .map_err(|error| error.to_string())?;
            let version = hub_qwen::version::check(&cli).await.map_err(|error| error.to_string())?;
            let root = hub_qwen::archive::storage_root(
                env::var_os("QWEN_RUNTIME_DIR").as_deref(),
                env::var_os("QWEN_HOME").as_deref(),
                home,
            )
            .map_err(|error| error.to_string())?;
            Arc::new(QwenArchive::new(root, version.to_string()))
        }
        BackendKind::Hermes => {
            let cli = hub_hermes::version::locate(settings.hermes.cli.as_deref())
                .map_err(|error| error.to_string())?;
            let version = hub_hermes::version::check(&cli).await.map_err(|error| error.to_string())?;
            let archive = HermesArchive::new(&cli, settings.hermes.profile.clone(), version.to_string())
                .map_err(|error| error.to_string())?;
            Arc::new(archive)
        }
    };
    Ok(archive)
}
```

`HermesArchive::new` читает первые 4 КиБ файла `hermes` — короткая блокирующая операция после асинхронной проверки версии, как и `resolve_cwd` в хабе.

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-telegram --lib -- agents`
Expected: PASS (текст ошибки `VersionError::Spawn` содержит путь — `crates/hub-agent/src/cli.rs:41`).

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hub-telegram/src/hub.rs crates/hub-telegram/src/agents.rs
git commit -m "Хаб получает архив сессий бэкенда после проверки его CLI"
```

---

### Task 11: `Hub` — `/save`, `/saves`, `/restore` и перенос из прежней темы

Хаб остаётся актором без ожиданий: проверки и выбор имени — в задаче хаба, файловая работа — в `transfer` (`spawn_blocking`), результат возвращается сообщением `HubMessage::Snapshot`, и только тогда меняются привязки. Порядок `/restore`: `load` → каталог (явный — до запуска задачи, из манифеста — в задаче, оба через `resolve_cwd` внутри `workspace_root`) → `present` → `import` (с откатом) → проверка, что ни тема, ни владелец сессии не выполняют ход → одна правка `Topics` (сброс владельцев и привязка темы) и один `put` → тексты.

**Files:**
- Modify: `Cargo.toml:16-54` (`[workspace.dependencies]`: `chrono = { version = "0.4.45", default-features = false, features = ["clock", "std"] }`), `crates/hub-telegram/Cargo.toml:9-24` (`chrono.workspace = true`), `Cargo.lock`
- Create: `crates/hub-telegram/src/transfer.rs`
- Modify: `crates/hub-telegram/src/lib.rs` (`mod transfer;` после `mod session;`)
- Modify: `crates/hub-telegram/src/hub.rs` — импорты `:4-33`, `HubMessage` `:69-91`, `Hub` `:117-133`, `spawn` `:136-160`, `handle` `:183-226`, `command` `:299-380`, `launch` `:465-509`, `refuse_if_running` `:561-567`, новые методы после `bind` `:527-532`; тесты `:593-1044`
- Modify: `crates/hub-telegram/src/texts.rs` (удалить `SNAPSHOTS_PENDING` из задачи 2)
- Modify: `crates/hub-app/src/supervisor.rs:483-489` (`| HubMessage::Snapshot(_)` в исчерпывающем `match`)
- Test: `crates/hub-telegram/src/hub.rs` (`#[cfg(test)]`), `crates/hub-telegram/src/transfer.rs` (`holders`)

**Interfaces:**
- Produces (`hub_telegram::hub`): `HubMessage::Snapshot(SnapshotDone)`; `pub struct SnapshotDone(pub(crate) Done)` (содержимое закрыто).
- Produces (`crate::transfer`, приватный модуль): `enum Done { Saved { key, name, outcome: Result<Manifest, String> }, Restored { key, outcome: Result<Restoration, String> } }`; `struct Restoration { name: SnapshotName, session: TopicSession, imported: bool }`; `SaveJob<A>`, `RestoreJob<A>` с `async fn run(self)`; `holders(&Topics, TopicKey, &TopicSession) -> Vec<TopicKey>`; `local_minute() -> Minute`; `utc_now() -> String`.
- Consumes: задачи 1, 2, 9, 10.

- [ ] **Step 1: Тесты (RED)**

`crates/hub-telegram/src/hub.rs`, модуль `tests` — импорты:

```rust
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, Ordering};

    use hub_agent::archive::{ArchiveError, Exported};
    use hub_core::snapshot::{BackendData, RelPath};
```

подделка архива (заменяет временную `archive` из задачи 10):

```rust
    /// Remembers which sessions the "agent" has; `hold` blocks the next export until released.
    #[derive(Default)]
    struct FakeArchive {
        known: StdMutex<HashSet<String>>,
        imports: StdMutex<Vec<String>>,
        fail_import: AtomicBool,
        hold: StdMutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl FakeArchive {
        fn forget(&self) {
            self.known.lock().unwrap().clear();
        }

        fn imports(&self) -> Vec<String> {
            self.imports.lock().unwrap().clone()
        }
    }

    impl SessionArchive for FakeArchive {
        fn export(
            &self,
            session: &SessionId,
            _cwd: &AbsolutePath,
            dest: &Path,
        ) -> Result<Exported, ArchiveError> {
            let held = self.hold.lock().unwrap().take();
            if let Some(release) = held {
                release.recv().unwrap();
            }
            let file = RelPath::parse(&format!("fake/{}.txt", session.as_str())).unwrap();
            std::fs::create_dir_all(dest.join("fake"))?;
            std::fs::write(file.under(dest), "history")?;
            self.known.lock().unwrap().insert(session.as_str().to_owned());
            Ok(Exported {
                files: vec![file],
                data: BackendData::Claude { project: RelPath::parse("p").unwrap() },
                agent_version: "1.0.0".to_owned(),
            })
        }

        fn present(
            &self,
            session: &SessionId,
            _cwd: &AbsolutePath,
            _data: &BackendData,
        ) -> Result<bool, ArchiveError> {
            Ok(self.known.lock().unwrap().contains(session.as_str()))
        }

        fn import(
            &self,
            session: &SessionId,
            _data: &BackendData,
            _files: &[RelPath],
            _src: &Path,
            _cwd: &AbsolutePath,
        ) -> Result<(), ArchiveError> {
            if self.fail_import.load(Ordering::SeqCst) {
                return Err(ArchiveError::Malformed("сбой".to_owned()));
            }
            self.imports.lock().unwrap().push(session.as_str().to_owned());
            self.known.lock().unwrap().insert(session.as_str().to_owned());
            Ok(())
        }
    }
```

`FakeAgents` получает поле `archive: Arc<FakeArchive>` (`#[derive(Default)]` сохраняется), а `impl Agents for FakeAgents`:

```rust
        fn archive<'a>(
            &'a self,
            _backend: BackendKind,
            _home: &'a Path,
        ) -> BoxFuture<'a, Result<Arc<dyn SessionArchive>, String>> {
            let archive: Arc<dyn SessionArchive> = Arc::clone(&self.archive) as Arc<dyn SessionArchive>;
            Box::pin(async move { Ok(archive) })
        }
```

`world_from` (`:716-728`): в `Draft` добавить `snapshots_dir: root.path().join("store").display().to_string(),`.

Новые тесты:

```rust
    const OTHER: TopicKey = TopicKey { chat: ChatId(-100), thread: ThreadId(8) };

    async fn bound(world: &World, key: TopicKey, id: &str) {
        world.send(HubMessage::Bind { key, session: SessionId::parse(id).unwrap() }).await;
        eventually("bound", || {
            world
                .store
                .last()
                .and_then(|topics| topics.get(&key).cloned())
                .is_some_and(|session| session.session == SessionId::parse(id))
        })
        .await;
    }

    fn said(world: &World, prefix: &str) -> bool {
        world.texts().iter().any(|text| text.starts_with(prefix))
    }

    async fn saved_snap(world: &World) {
        bound(world, KEY, "s-1").await;
        world.send(text(Some(7), 1, "/save snap")).await;
        eventually("saved", || said(world, "💾 Сохранено: snap · claude · ")).await;
    }

    #[tokio::test]
    async fn restore_moves_the_session_from_its_old_topic() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;

        world.send(text(Some(8), 2, "/restore snap")).await;

        eventually("restored", || said(&world, "♻️ Восстановлено: snap · claude · ")).await;
        eventually("told", || world.texts().contains(&texts::SESSION_MOVED.to_owned())).await;
        let topics = world.store.last().unwrap();
        assert_eq!(topics.get(&OTHER).and_then(|s| s.session.clone()), SessionId::parse("s-1"));
        assert_eq!(topics.get(&KEY).map(|s| s.session.is_none()), Some(true));
        assert!(world.agents.archive.imports().is_empty(), "the agent still had it");
    }

    #[tokio::test]
    async fn restore_imports_what_the_agent_lost_into_the_named_directory() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;
        world.agents.archive.forget();

        world.send(text(Some(8), 2, "/restore snap project")).await;

        eventually("restored", || {
            world.texts().iter().any(|t| t.starts_with("♻️ Восстановлено: snap") && t.ends_with("project"))
        })
        .await;
        assert_eq!(world.agents.archive.imports(), ["s-1"]);
    }

    #[tokio::test]
    async fn a_failed_import_leaves_the_topics_unchanged() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;
        world.agents.archive.forget();
        world.agents.archive.fail_import.store(true, Ordering::SeqCst);

        world.send(text(Some(8), 2, "/restore snap")).await;

        eventually("failed", || world.texts().contains(&"⚠️ Данные снимка повреждены: сбой".to_owned())).await;
        let topics = world.store.last().unwrap();
        assert_eq!(topics.get(&KEY).and_then(|s| s.session.clone()), SessionId::parse("s-1"));
        assert!(topics.get(&OTHER).is_none());
    }

    #[tokio::test]
    async fn save_and_restore_wait_for_the_turn() {
        let world = world(FakeAgents::scripted(vec![Script::UntilStopped]));
        world.send(text(Some(7), 1, "долго")).await;
        eventually("running", || world.running()).await;

        world.send(text(Some(7), 2, "/save snap")).await;
        world.send(text(Some(7), 3, "/restore snap")).await;

        eventually("refused twice", || {
            world.texts().iter().filter(|t| *t == texts::FINISH_TURN_FIRST).count() == 2
        })
        .await;
        world.send(HubMessage::Stop(KEY)).await;
    }

    #[tokio::test]
    async fn turns_wait_while_a_snapshot_is_being_written() {
        let world = world(FakeAgents::default());
        bound(&world, KEY, "s-1").await;
        let (release, held) = std::sync::mpsc::channel();
        *world.agents.archive.hold.lock().unwrap() = Some(held);

        world.send(text(Some(7), 1, "/save snap")).await;
        world.send(text(Some(7), 2, "привет")).await;
        world.send(text(Some(7), 3, "/reset")).await;

        eventually("busy twice", || {
            world.texts().iter().filter(|t| *t == texts::SNAPSHOT_BUSY).count() == 2
        })
        .await;
        assert!(world.agents.prompts().is_empty());
        release.send(()).unwrap();
        eventually("saved", || said(&world, "💾 Сохранено: snap")).await;
    }

    #[tokio::test]
    async fn snapshot_commands_report_what_is_wrong() {
        let world = world(FakeAgents::default());
        world.send(text(Some(7), 1, "/save")).await;
        eventually("nothing", || world.texts().contains(&texts::NOTHING_TO_SAVE.to_owned())).await;
        world.send(text(Some(7), 2, "/save Bad")).await;
        eventually("bad name", || said(&world, "⚠️ В имени снимка допустимы")).await;
        world.send(text(Some(7), 3, "/restore")).await;
        eventually("usage", || world.texts().contains(&texts::RESTORE_USAGE.to_owned())).await;
        world.send(text(Some(7), 4, "/restore nope")).await;
        eventually("unknown", || world.texts().contains(&"⚠️ Снимок nope не найден".to_owned())).await;
        world.send(text(Some(7), 5, "/restore nope ../..")).await;
        eventually("outside", || world.texts().iter().any(|t| t.starts_with("⚠️") && t.contains("вне корня"))).await;
    }

    #[tokio::test]
    async fn a_taken_name_is_refused() {
        let world = world(FakeAgents::default());
        saved_snap(&world).await;
        world.send(text(Some(7), 2, "/save snap")).await;
        eventually("taken", || world.texts().contains(&"⚠️ Снимок snap уже существует".to_owned())).await;
    }

    #[tokio::test]
    async fn saves_list_snapshots_and_default_names_follow_the_title() {
        let world = world(FakeAgents::default());
        world.send(text(Some(7), 1, "/saves")).await;
        eventually("empty", || world.texts().contains(&texts::NO_SNAPSHOTS.to_owned())).await;

        world.send(inbound(Some(7), 2, Content::TopicCreated { name: "Починить тесты".to_owned() })).await;
        bound(&world, KEY, "s-1").await;
        world.send(text(Some(7), 3, "/save")).await;
        eventually("saved", || said(&world, "💾 Сохранено: pochinit-testy-")).await;

        world.send(text(Some(7), 4, "/saves")).await;
        eventually("listed", || said(&world, "💾 Снимки, новые сверху:\npochinit-testy-")).await;
        assert!(world.texts().iter().any(|t| t.ends_with(" · Починить тесты")));
    }
```

`crates/hub-telegram/src/transfer.rs`, тест `holders` (вместе с реализацией на шаге 3):

```rust
#[cfg(test)]
mod tests {
    use hub_core::domain::{AbsolutePath, BackendKind, ChatId, SessionId, ThreadId};

    use super::*;

    fn key(thread: i32) -> TopicKey {
        TopicKey { chat: ChatId(-100), thread: ThreadId(thread) }
    }

    fn session(backend: BackendKind, id: Option<&str>) -> TopicSession {
        TopicSession::fresh(backend, AbsolutePath::new(std::env::temp_dir()).unwrap())
            .with_session(id.and_then(SessionId::parse))
    }

    #[test]
    fn holders_are_other_topics_with_the_same_agent_session() {
        let moved = session(BackendKind::Claude, Some("s-1"));
        let topics = Topics::from([
            (key(1), moved.clone()),
            (key(2), session(BackendKind::Claude, Some("s-1"))),
            (key(3), session(BackendKind::Codex, Some("s-1"))),
            (key(4), session(BackendKind::Claude, Some("s-2"))),
            (key(5), session(BackendKind::Claude, None)),
        ]);
        assert_eq!(holders(&topics, key(1), &moved), [key(2)]);
    }
}
```

- [ ] **Step 2: Проверить RED**

Run: `cargo test -p hub-telegram --lib -- hub::tests transfer`
Expected: FAIL — нет `HubMessage::Snapshot`, модуля `transfer`; тесты снимков получают `SNAPSHOTS_PENDING`.

- [ ] **Step 3: Реализация**

`Cargo.toml` (workspace) и `crates/hub-telegram/Cargo.toml` — `chrono` как в **Files**; `cargo check --workspace --all-targets`; `git diff Cargo.lock` — только `"chrono"` в зависимостях `hub-telegram`, новых пакетов нет.

`crates/hub-telegram/src/lib.rs`: `mod transfer;` после `mod session;`.

`crates/hub-telegram/src/transfer.rs`:

```rust
//! The file work of `/save` and `/restore`, off the hub task: the agent CLI is checked, files
//! copied and Hermes run in blocking threads; the hub then applies the result in one step.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{Datelike, Timelike};
use hub_agent::archive::{ArchiveError, Exported};
use hub_core::domain::{AbsolutePath, SessionId, TopicKey, TopicSession};
use hub_core::snapshot::{Manifest, Minute, SnapshotName};
use hub_core::topics::Topics;
use tokio::sync::mpsc;
use tokio::task::JoinError;

use crate::hub::{Agents, HubMessage, SnapshotDone};
use crate::paths::resolve_cwd;
use crate::snapshots::{Loaded, SnapshotStore};
use crate::texts;

pub enum Done {
    Saved { key: TopicKey, name: SnapshotName, outcome: Result<Manifest, String> },
    Restored { key: TopicKey, outcome: Result<Restoration, String> },
}

pub struct Restoration {
    pub name: SnapshotName,
    pub session: TopicSession,
    pub imported: bool,
}

#[must_use]
pub fn local_minute() -> Minute {
    let now = chrono::Local::now();
    Minute { year: now.year(), month: now.month(), day: now.day(), hour: now.hour(), minute: now.minute() }
}

#[must_use]
pub fn utc_now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// Topics other than `key` bound to the same agent session.
#[must_use]
pub fn holders(topics: &Topics, key: TopicKey, session: &TopicSession) -> Vec<TopicKey> {
    topics
        .iter()
        .filter(|(other, bound)| {
            **other != key
                && bound.backend == session.backend
                && bound.session.is_some()
                && bound.session == session.session
        })
        .map(|(other, _)| *other)
        .collect()
}

fn internal(_: JoinError) -> String {
    texts::INTERNAL_ERROR.to_owned()
}

async fn report(mailbox: &mpsc::Sender<HubMessage>, done: Done) {
    // The hub is gone only during shutdown; a restore is then simply repeated later.
    let _ = mailbox.send(HubMessage::Snapshot(SnapshotDone(done))).await;
}

pub struct SaveJob<A> {
    pub key: TopicKey,
    pub name: SnapshotName,
    pub title: Option<String>,
    pub session: TopicSession,
    pub id: SessionId,
    pub created_at: String,
    pub store: SnapshotStore,
    pub home: PathBuf,
    pub agents: Arc<A>,
    pub mailbox: mpsc::Sender<HubMessage>,
}

impl<A: Agents> SaveJob<A> {
    pub async fn run(self) {
        let (key, name, mailbox) = (self.key, self.name.clone(), self.mailbox.clone());
        let outcome = self.save().await;
        if let Err(reason) = &outcome {
            tracing::warn!(chat = key.chat.0, thread = key.thread.0, snapshot = name.as_str(), %reason, "snapshot not saved");
        }
        report(&mailbox, Done::Saved { key, name, outcome }).await;
    }

    async fn save(self) -> Result<Manifest, String> {
        let Self { key: _key, name, title, session, id, created_at, store, home, agents, mailbox: _mailbox } =
            self;
        let archive = agents.archive(session.backend, &home).await?;
        tokio::task::spawn_blocking(move || {
            store.save(&name, |agent| {
                let Exported { files, data, agent_version } = archive.export(&id, &session.cwd, agent)?;
                if data.backend() != session.backend {
                    return Err(ArchiveError::Malformed("архив вернул данные другого бэкенда".to_owned()));
                }
                Ok(Manifest {
                    name: name.clone(),
                    created_at,
                    title,
                    cwd: session.cwd.clone(),
                    session: id.clone(),
                    agent_version,
                    data,
                    files,
                })
            })
        })
        .await
        .map_err(internal)?
        .map_err(|error| error.to_string())
    }
}

pub struct RestoreJob<A> {
    pub key: TopicKey,
    pub name: SnapshotName,
    /// An explicit directory already resolved inside the root; `None` — the snapshot's own.
    pub cwd: Option<AbsolutePath>,
    pub root: AbsolutePath,
    pub store: SnapshotStore,
    pub home: PathBuf,
    pub agents: Arc<A>,
    pub mailbox: mpsc::Sender<HubMessage>,
}

impl<A: Agents> RestoreJob<A> {
    pub async fn run(self) {
        let (key, name, mailbox) = (self.key, self.name.clone(), self.mailbox.clone());
        let outcome = self.restore().await;
        match &outcome {
            Ok(restoration) => tracing::info!(snapshot = name.as_str(), imported = restoration.imported, "snapshot restored"),
            Err(reason) => tracing::warn!(chat = key.chat.0, thread = key.thread.0, snapshot = name.as_str(), %reason, "snapshot not restored"),
        }
        report(&mailbox, Done::Restored { key, outcome }).await;
    }

    async fn restore(self) -> Result<Restoration, String> {
        let Self { key: _key, name, cwd, root, store, home, agents, mailbox: _mailbox } = self;
        let wanted = name.clone();
        let Loaded { manifest, agent } = tokio::task::spawn_blocking(move || store.load(&wanted))
            .await
            .map_err(internal)?
            .map_err(|error| error.to_string())?;
        let Manifest {
            name: _name,
            created_at: _created_at,
            title: _title,
            cwd: saved,
            session,
            agent_version: _agent_version,
            data,
            files,
        } = manifest;
        let cwd = match cwd {
            Some(cwd) => cwd,
            None => resolve_cwd(&root, &home, Some(&saved.as_path().display().to_string()))
                .map_err(|error| error.to_string())?,
        };
        let backend = data.backend();
        let archive = agents.archive(backend, &home).await?;
        let (id, target) = (session.clone(), cwd.clone());
        let imported = tokio::task::spawn_blocking(move || -> Result<bool, ArchiveError> {
            if archive.present(&id, &target, &data)? {
                return Ok(false);
            }
            archive.import(&id, &data, &files, &agent, &target)?;
            Ok(true)
        })
        .await
        .map_err(internal)?
        .map_err(|error| error.to_string())?;
        Ok(Restoration { name, session: TopicSession { backend, cwd, session: Some(session) }, imported })
    }
}
```

`crates/hub-telegram/src/hub.rs`:

- импорты: `use std::collections::{HashMap, HashSet};`, `use hub_core::commands::{…, RestoreArgs, SaveArgs, parse_restore_args, parse_save_args};`, `use hub_core::snapshot::{SnapshotName, default_name};`, `use crate::snapshots::{SnapshotError, SnapshotStore};`, `use crate::transfer::{Done, Restoration, RestoreJob, SaveJob, holders, local_minute, utc_now};`.
- `HubMessage` — после `Reset(TopicKey),`:

```rust
    /// A `/save` or `/restore` finished its file work.
    Snapshot(SnapshotDone),
```

и рядом с `HubMessage`:

```rust
/// The outcome of a snapshot job; only the hub reads it.
pub struct SnapshotDone(pub(crate) Done);
```

- `Hub` — поля:

```rust
    /// Topics with a `/save` or `/restore` in flight: no turn or session change starts there.
    snapshotting: HashSet<TopicKey>,
    /// Names being written now, so two topics cannot race for one name.
    naming: HashSet<SnapshotName>,
```

и в `spawn` — `snapshotting: HashSet::new(), naming: HashSet::new(),`.
- `handle`: `HubMessage::Snapshot(SnapshotDone(done)) => self.snapshot_done(done),`.
- `command`: ветку из задачи 2 заменить на

```rust
            Command::Save => self.save(key, args),
            Command::Saves => self.saves(key),
            Command::Restore => self.restore(key, args),
```

- `launch`, сразу после проверки `shutdown`:

```rust
        if self.snapshotting.contains(&key) {
            self.say(Target::Topic(key), texts::SNAPSHOT_BUSY.to_owned());
            return;
        }
```

- `refuse_if_running`:

```rust
    fn refuse_if_running(&self, key: TopicKey) -> bool {
        if self.snapshotting.contains(&key) {
            self.say(Target::Topic(key), texts::SNAPSHOT_BUSY.to_owned());
            return true;
        }
        let running = self.running.contains_key(&key);
        if running {
            self.say(Target::Topic(key), texts::ALREADY_RUNNING.to_owned());
        }
        running
    }
```

- новые методы после `bind`:

```rust
    /// `/save` and `/restore` refuse during a turn and while another snapshot job runs here.
    fn refuse_snapshot(&self, key: TopicKey) -> bool {
        let text = if self.running.contains_key(&key) {
            texts::FINISH_TURN_FIRST
        } else if self.snapshotting.contains(&key) {
            texts::SNAPSHOT_BUSY
        } else {
            return false;
        };
        self.say(Target::Topic(key), text.to_owned());
        true
    }

    fn save(&mut self, key: TopicKey, args: &[&str]) {
        if self.refuse_snapshot(key) {
            return;
        }
        let requested = match parse_save_args(args) {
            Ok(requested) => requested,
            Err(error) => {
                self.say(Target::Topic(key), texts::warning(&error));
                return;
            }
        };
        let Some((session, id)) =
            self.topics.get(&key).and_then(|session| Some((session.clone(), session.session.clone()?)))
        else {
            self.say(Target::Topic(key), texts::NOTHING_TO_SAVE.to_owned());
            return;
        };
        let title = self.titles.get(&key).cloned();
        let name = match requested {
            SaveArgs::Named(name) => name,
            SaveArgs::Default => default_name(title.as_deref(), session.backend.name(), local_minute()),
        };
        if !self.naming.insert(name.clone()) {
            self.say(Target::Topic(key), texts::warning(&SnapshotError::Exists(name)));
            return;
        }
        self.snapshotting.insert(key);
        let job = SaveJob {
            key,
            name,
            title,
            session,
            id,
            created_at: utc_now(),
            store: SnapshotStore::new(&self.settings.borrow().snapshots_dir),
            home: self.home.clone(),
            agents: Arc::clone(&self.agents),
            mailbox: self.mailbox.clone(),
        };
        tokio::spawn(job.run());
    }

    fn saves(&self, key: TopicKey) {
        let store = SnapshotStore::new(&self.settings.borrow().snapshots_dir);
        let sender = self.sender.clone();
        tokio::spawn(async move {
            let text = match tokio::task::spawn_blocking(move || store.list()).await {
                Ok(Ok(manifests)) => texts::snapshot_list(&manifests),
                Ok(Err(error)) => texts::warning(&error),
                Err(_) => texts::INTERNAL_ERROR.to_owned(),
            };
            sender.text(Target::Topic(key), &text).await;
        });
    }

    fn restore(&mut self, key: TopicKey, args: &[&str]) {
        if self.refuse_snapshot(key) {
            return;
        }
        let RestoreArgs { name, cwd } = match parse_restore_args(args) {
            Ok(Some(args)) => args,
            Ok(None) => {
                self.say(Target::Topic(key), texts::RESTORE_USAGE.to_owned());
                return;
            }
            Err(error) => {
                self.say(Target::Topic(key), texts::warning(&error));
                return;
            }
        };
        let resolved = self.root().and_then(|root| {
            let cwd = cwd.as_deref().map(|raw| resolve_cwd(&root, &self.home, Some(raw))).transpose()?;
            Ok((root, cwd))
        });
        let (root, cwd) = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                self.say(Target::Topic(key), texts::warning(&error));
                return;
            }
        };
        self.snapshotting.insert(key);
        let job = RestoreJob {
            key,
            name,
            cwd,
            root,
            store: SnapshotStore::new(&self.settings.borrow().snapshots_dir),
            home: self.home.clone(),
            agents: Arc::clone(&self.agents),
            mailbox: self.mailbox.clone(),
        };
        tokio::spawn(job.run());
    }

    fn snapshot_done(&mut self, done: Done) {
        match done {
            Done::Saved { key, name, outcome } => {
                self.snapshotting.remove(&key);
                self.naming.remove(&name);
                let text = match outcome {
                    Ok(manifest) => texts::saved(&manifest),
                    Err(reason) => texts::warning(&reason),
                };
                self.say(Target::Topic(key), text);
            }
            Done::Restored { key, outcome } => {
                self.snapshotting.remove(&key);
                match outcome {
                    Ok(restoration) => self.rebind(key, restoration),
                    Err(reason) => self.say(Target::Topic(key), texts::warning(&reason)),
                }
            }
        }
    }

    /// One session, one topic: every other holder is reset, then `key` is bound, in one save.
    fn rebind(&mut self, key: TopicKey, restoration: Restoration) {
        let Restoration { name, session, imported: _imported } = restoration;
        let holders = holders(&self.topics, key, &session);
        if self.running.contains_key(&key) || holders.iter().any(|holder| self.running.contains_key(holder)) {
            self.say(Target::Topic(key), texts::SESSION_BUSY_ELSEWHERE.to_owned());
            return;
        }
        for holder in &holders {
            if let Some(old) = self.topics.get(holder).cloned() {
                self.topics.insert(*holder, old.with_session(None));
            }
        }
        self.put(key, session.clone());
        self.say(Target::Topic(key), texts::restored(&name, &session));
        for holder in holders {
            self.say(Target::Topic(holder), texts::SESSION_MOVED.to_owned());
        }
    }
```

`put` вставляет и сохраняет все `Topics` одним вызовом `store.save` — сброс владельцев и новая привязка уходят на диск вместе (`hub.rs:546-551`). Уведомление прежней теме — через `say`, без гарантии доставки (как в спецификации). Результат, пришедший во время остановки, обрабатывается, если хаб ещё жив; иначе отбрасывается (non-goal).

`crates/hub-telegram/src/texts.rs`: удалить `SNAPSHOTS_PENDING` и её комментарий.

`crates/hub-app/src/supervisor.rs:483-489`: добавить `| HubMessage::Snapshot(_)` в ветку `"other"`.

- [ ] **Step 4: Проверить GREEN**

Run: `cargo test -p hub-telegram --lib -- hub::tests transfer && cargo test -p hub-app --lib -- supervisor`
Expected: PASS, включая прежние тесты хаба.

- [ ] **Step 5: Гейты**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked && (command -v cargo-deny >/dev/null && cargo deny check || true)`
Expected: PASS. Если `clippy::too_many_lines` сработает на `command` или `Hub::handle`, — вынести ветки в методы (они уже вынесены) и не подавлять линт.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/hub-telegram crates/hub-app/src/supervisor.rs
git commit -m "Хаб сохраняет и восстанавливает снимки сессий и переносит сессию между темами"
```

---

### Task 12: Живые тесты, README, версия 0.5.0

Живой тест на каждый бэкенд закрывает то, что проверено только по исходникам: ход 1 запоминает слово → `export` → история удаляется у агента → `present == false` → `import` (Qwen и Hermes — в другой каталог) → `present == true` → ход 2 по тому же id вспоминает слово.

**Files:**
- Modify: `crates/hub-claude/tests/live.rs` (новый тест в `mod tests`), `crates/hub-codex/tests/live.rs`, `crates/hub-qwen/tests/live.rs`
- Create: `crates/hub-hermes/tests/live.rs`
- Modify: `README.md` — «Команды» `:147-158`, новый раздел «Снимки сессий» после «Что приходит в тему» (`:286-295`), «Настройки и файлы» `:313-361`, «Безопасность» `:380-406`, «Ограничения» `:455-460`
- Modify: `Cargo.toml:6` (`version = "0.5.0"`), `Cargo.lock`

**Interfaces:**
- Consumes: `*::archive::{…Archive, storage_root}` (задачи 4–7), `*::backend::*Backend::{new, run}`, `*::version::{locate, check}`.

- [ ] **Step 1: Живой тест Claude**

`crates/hub-claude/tests/live.rs`, в `mod tests` (рядом с `live_cli_round_trip`; импорты дополнить `use std::path::PathBuf;`, `use hub_agent::archive::SessionArchive;`, `use hub_claude::archive::{ClaudeArchive, storage_root};`, `SessionId` в `hub_core::domain`):

```rust
    fn home() -> PathBuf {
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap()
    }

    async fn turn(backend: &ClaudeBackend, session: &TopicSession, text: &str) -> Vec<AgentEvent> {
        let (events_out, mut events) = mpsc::channel(256);
        let (_inbox, inbox_in) = mpsc::channel(1);
        let conversation = Conversation {
            channel: Arc::new(Allowing),
            events: events_out,
            cancel: CancellationToken::new(),
            limits: Limits::new(Duration::from_mins(1)),
        };
        let prompt = Prompt::new(text.to_owned(), Vec::new()).unwrap();
        tokio::time::timeout(Duration::from_mins(5), backend.run(session, prompt, inbox_in, conversation))
            .await
            .unwrap();
        std::iter::from_fn(|| events.try_recv().ok()).collect()
    }

    fn finished_session(seen: &[AgentEvent]) -> SessionId {
        seen.iter()
            .find_map(|event| if let AgentEvent::Finished(finished) = event { Some(finished.session.clone()) } else { None })
            .unwrap()
    }

    fn said(seen: &[AgentEvent]) -> String {
        seen.iter()
            .filter_map(|event| if let AgentEvent::AssistantText(text) = event { Some(text.as_str()) } else { None })
            .collect()
    }

    #[tokio::test]
    #[ignore = "talks to the real agent CLI and spends tokens"]
    async fn live_snapshot_round_trip_keeps_the_history() {
        if std::env::var_os("AGENT_HUB_LIVE_CLI").is_none() {
            return;
        }
        let cli = locate(None).unwrap();
        let version = check(&cli).await.unwrap();
        let settings = ClaudeSettings {
            cli: None,
            model: Some("haiku".to_owned()),
            permission_mode: PermissionMode::Default,
            budget: None,
        };
        let backend = ClaudeBackend::new(cli, settings);
        let workdir = std::env::temp_dir().join(format!("agent-hub-live-snap-{}", std::process::id()));
        std::fs::create_dir_all(&workdir).unwrap();
        let cwd = AbsolutePath::new(workdir).unwrap();
        let fresh = TopicSession::fresh(BackendKind::Claude, cwd.clone());
        let first = turn(&backend, &fresh, "Запомни кодовое слово «бирюза». Ответь одним словом: ок").await;
        let id = finished_session(&first);

        let root = storage_root(std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(), &home());
        let archive = ClaudeArchive::new(root.clone(), version.to_string());
        let snapshot = tempfile::tempdir().unwrap();
        let exported = archive.export(&id, &cwd, snapshot.path()).unwrap();
        for file in &exported.files {
            std::fs::remove_file(file.under(&root)).unwrap();
        }
        assert!(!archive.present(&id, &cwd, &exported.data).unwrap());
        archive.import(&id, &exported.data, &exported.files, snapshot.path(), &cwd).unwrap();
        assert!(archive.present(&id, &cwd, &exported.data).unwrap());

        let resumed = fresh.with_session(Some(id));
        let second = turn(&backend, &resumed, "Какое кодовое слово я просил запомнить? Ответь одним словом.").await;
        assert!(said(&second).to_lowercase().contains("бирюз"), "{second:?}");
    }
```

- [ ] **Step 2: Живой тест Codex**

`crates/hub-codex/tests/live.rs`, в `mod tests` — те же помощники `home`, `finished_session`, `said` и `turn` с `CodexBackend` и каналом `Denying` (`Duration::from_mins(3)` как в `codex_answers_a_prompt`). Тест:

```rust
    #[tokio::test]
    #[ignore = "needs a logged-in codex and spends tokens"]
    async fn codex_snapshot_round_trip_keeps_the_history() {
        if std::env::var_os("AGENT_HUB_LIVE_CODEX").is_none() {
            return;
        }
        let cli = locate(None).unwrap();
        let version = check(&cli).await.unwrap();
        let settings = CodexSettings {
            cli: None,
            model: None,
            sandbox: Sandbox::ReadOnly,
            approval: Approval::Untrusted,
            api_key: None,
        };
        let backend = CodexBackend::new(cli, settings);
        let cwd = AbsolutePath::new(std::env::temp_dir()).unwrap();
        let fresh = TopicSession::fresh(BackendKind::Codex, cwd.clone());
        let first = turn(&backend, &fresh, "Запомни кодовое слово «бирюза». Ответь одним словом: ок").await;
        let id = finished_session(&first);

        let root = hub_codex::archive::storage_root(std::env::var_os("CODEX_HOME").as_deref(), &home());
        let archive = hub_codex::archive::CodexArchive::new(root.clone(), version.to_string());
        let snapshot = tempfile::tempdir().unwrap();
        let exported = archive.export(&id, &cwd, snapshot.path()).unwrap();
        for file in &exported.files {
            std::fs::remove_file(file.under(&root)).unwrap();
        }
        assert!(!archive.present(&id, &cwd, &exported.data).unwrap());
        archive.import(&id, &exported.data, &exported.files, snapshot.path(), &cwd).unwrap();

        let second = turn(&backend, &fresh.with_session(Some(id)), "Какое кодовое слово я просил запомнить? Ответь одним словом.").await;
        assert!(said(&second).to_lowercase().contains("бирюз"), "{second:?}");
    }
```

(`use hub_agent::archive::SessionArchive;`, `SessionId`, `std::path::PathBuf`; `[dev-dependencies]` `tempfile` уже добавлен в задаче 5.) Строка SQLite остаётся — `thread/resume` находит файл по id (раздел «Хранилища агентов», Codex).

- [ ] **Step 3: Живой тест Qwen (другой каталог — переписывание `cwd`)**

`crates/hub-qwen/tests/live.rs`, в `mod tests` — помощники `home`, `finished_session`, `said`, `turn` с `QwenBackend` и каналом `Recording` (`Arc::new(Recording::default())`):

```rust
    #[tokio::test]
    #[ignore = "needs a configured qwen and spends tokens"]
    async fn qwen_snapshot_moves_to_another_directory_with_its_history() {
        if std::env::var_os("AGENT_HUB_LIVE_QWEN").is_none() {
            return;
        }
        let cli = locate(None).unwrap();
        let version = check(&cli).await.unwrap();
        let backend = QwenBackend::new(cli, settings(QwenApproval::Plan));
        let base = std::env::temp_dir().join(format!("agent-hub-live-qwen-{}", std::process::id()));
        let (old, new) = (base.join("old"), base.join("new"));
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        let (old, new) = (AbsolutePath::new(old).unwrap(), AbsolutePath::new(new).unwrap());
        let first = turn(&backend, &TopicSession::fresh(BackendKind::Qwen, old.clone()), "Запомни кодовое слово «бирюза». Ответь одним словом: ок").await;
        let id = finished_session(&first);

        let root = hub_qwen::archive::storage_root(
            std::env::var_os("QWEN_RUNTIME_DIR").as_deref(),
            std::env::var_os("QWEN_HOME").as_deref(),
            &home(),
        )
        .unwrap();
        let archive = hub_qwen::archive::QwenArchive::new(root.clone(), version.to_string());
        let snapshot = tempfile::tempdir().unwrap();
        let exported = archive.export(&id, &old, snapshot.path()).unwrap();
        for file in &exported.files {
            std::fs::remove_file(file.under(&root)).unwrap();
        }
        assert!(!archive.present(&id, &new, &exported.data).unwrap());
        archive.import(&id, &exported.data, &exported.files, snapshot.path(), &new).unwrap();

        let moved = TopicSession::fresh(BackendKind::Qwen, new).with_session(Some(id));
        let second = turn(&backend, &moved, "Какое кодовое слово я просил запомнить? Ответь одним словом.").await;
        assert!(said(&second).to_lowercase().contains("бирюз"), "{second:?}");
    }
```

- [ ] **Step 4: Живой тест Hermes**

`crates/hub-hermes/tests/live.rs`:

```rust
//! Round trip through the real Hermes, opt-in:
//! `AGENT_HUB_LIVE_HERMES=1 [AGENT_HUB_LIVE_HERMES_PROFILE=<p>] cargo test -p hub-hermes --test live -- --ignored`.
#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use hub_agent::archive::SessionArchive;
use hub_agent::channel::UserChannel;
use hub_agent::conversation::{Conversation, Limits};
use hub_core::domain::{
    AbsolutePath, AgentEvent, BackendKind, Decision, Denied, FileDelivery, OutgoingFile, Prompt,
    Question, QuestionsOutcome, SessionId, ToolRequest, TopicSession,
};
use hub_core::settings::HermesSettings;
use hub_hermes::archive::HermesArchive;
use hub_hermes::backend::HermesBackend;
use hub_hermes::version::{check, locate};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Denying;

impl UserChannel for Denying {
    fn request(&self, _tool: ToolRequest) -> BoxFuture<'_, Decision> {
        Box::pin(async { Decision::Denied(Denied::new("live test")) })
    }
    fn ask(&self, _questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome> {
        Box::pin(async { QuestionsOutcome::Answered(Vec::new()) })
    }
    fn send_file(&self, _file: OutgoingFile) -> BoxFuture<'_, FileDelivery> {
        Box::pin(async { FileDelivery::Delivered })
    }
}

async fn turn(backend: &HermesBackend, session: &TopicSession, text: &str) -> Vec<AgentEvent> {
    let (events_out, mut events) = mpsc::channel(256);
    let (_inbox, inbox_in) = mpsc::channel(1);
    let conversation = Conversation {
        channel: Arc::new(Denying),
        events: events_out,
        cancel: CancellationToken::new(),
        limits: Limits::new(Duration::from_mins(1)),
    };
    let prompt = Prompt::new(text.to_owned(), Vec::new()).unwrap();
    tokio::time::timeout(Duration::from_mins(5), backend.run(session, prompt, inbox_in, conversation))
        .await
        .unwrap();
    std::iter::from_fn(|| events.try_recv().ok()).collect()
}

fn finished_session(seen: &[AgentEvent]) -> SessionId {
    seen.iter()
        .find_map(|event| if let AgentEvent::Finished(finished) = event { Some(finished.session.clone()) } else { None })
        .unwrap()
}

fn said(seen: &[AgentEvent]) -> String {
    seen.iter()
        .filter_map(|event| if let AgentEvent::AssistantText(text) = event { Some(text.as_str()) } else { None })
        .collect()
}

#[tokio::test]
#[ignore = "needs a configured hermes and spends tokens"]
async fn hermes_snapshot_moves_to_another_directory_with_its_history() {
    if std::env::var_os("AGENT_HUB_LIVE_HERMES").is_none() {
        return;
    }
    let cli = locate(None).unwrap();
    let version = check(&cli).await.unwrap();
    let profile = std::env::var("AGENT_HUB_LIVE_HERMES_PROFILE").ok();
    let settings = HermesSettings { cli: None, profile: profile.clone(), model: None };
    let backend = HermesBackend::new(cli.clone(), settings);
    let base: PathBuf = std::env::temp_dir().join(format!("agent-hub-live-hermes-{}", std::process::id()));
    let (old, new) = (base.join("old"), base.join("new"));
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&new).unwrap();
    let (old, new) = (AbsolutePath::new(old).unwrap(), AbsolutePath::new(new).unwrap());
    let first = turn(&backend, &TopicSession::fresh(BackendKind::Hermes, old.clone()), "Запомни кодовое слово «бирюза». Ответь одним словом: ок").await;
    let id = finished_session(&first);

    let archive = HermesArchive::new(&cli, profile.clone(), version.to_string()).unwrap();
    let snapshot = tempfile::tempdir().unwrap();
    let exported = archive.export(&id, &old, snapshot.path()).unwrap();
    let deleted = std::process::Command::new(&cli)
        .args(profile.iter().flat_map(|profile| ["--profile", profile.as_str()]))
        .args(["sessions", "delete", id.as_str(), "--yes"])
        .status()
        .unwrap();
    assert!(deleted.success());
    assert!(!archive.present(&id, &new, &exported.data).unwrap());
    archive.import(&id, &exported.data, &exported.files, snapshot.path(), &new).unwrap();
    assert!(archive.present(&id, &new, &exported.data).unwrap());

    let moved = TopicSession::fresh(BackendKind::Hermes, new).with_session(Some(id));
    let second = turn(&backend, &moved, "Какое кодовое слово я просил запомнить? Ответь одним словом.").await;
    assert!(said(&second).to_lowercase().contains("бирюз"), "{second:?}");
}
```

Run: `cargo test -p hub-claude --test live && cargo test -p hub-codex --test live && cargo test -p hub-qwen --test live && cargo test -p hub-hermes --test live`
Expected: PASS — новые тесты `ignored`.

- [ ] **Step 5: README**

- «Команды» — три строки таблицы:
  - `| /save [имя] | Снимок сессии темы в каталог снимков; без имени — <название темы>-ГГГГММДД-ЧЧММ |`
  - `| /saves | Последние 30 снимков: имя, бэкенд, каталог, дата, название темы |`
  - `| /restore <имя> [каталог] | Продолжить сессию снимка в этой теме; прежняя тема сессии сбрасывается |`
- Новый раздел «### Снимки сессий» после «Что приходит в тему»:

```markdown
### Снимки сессий

`/save [имя]` сохраняет сессию темы: бэкенд, рабочий каталог, id сессии, название темы, версию агента и копию истории самого агента. Файлы проекта и `file-history` Claude не сохраняются. Имя — латиница в нижнем регистре, цифры, `.`, `_`, `-`, до 64 символов; занятое имя не перезаписывается.

`/restore <имя> [каталог]` в новой теме продолжает ту же сессию: одна сессия — одна тема, прежняя тема сбрасывается и получает уведомление. Каталог — из снимка или указанный (внутри корня рабочих каталогов, должен существовать). Если у агента сессия уже есть, файлы не трогаются — тема только перепривязывается; иначе история возвращается агенту без перезаписи его файлов. Во время хода `/save` и `/restore` отказывают.

Снимки лежат в `<каталог снимков>/snapshots/<имя>/` (`manifest.json` и `agent/`); каталог по умолчанию — `~/.agent-hub`, меняется в «Настройках». Удалить снимок — удалить его каталог.

Что переносится по бэкендам: Claude — `projects/<проект>/<id>.jsonl` и каталог сессии; Codex — файлы `rollout-…-<id>` из `sessions/` (архивную сессию сначала верните: `codex unarchive <id>`); Qwen — чат, его спутники, субагенты и задачи, при новом каталоге поле `cwd` в истории переписывается; Hermes — строки сессии (с цепочкой сжатия) из `state.db` профиля из настроек, через собственный Python Hermes.
```

- «Настройки и файлы»: строка таблицы `| Каталог снимков | ~/.agent-hub | сразу |`; текст: «таблица `[snapshots]` (`dir`)»; в таблице путей — `| Снимки сессий | ~/.agent-hub\snapshots\ (или из настроек) | ~/.agent-hub/snapshots/ | ~/.agent-hub/snapshots/ |`.
- «Безопасность», новый пункт: «**Снимки** содержат историю агента открытым текстом — вывод команд и фрагменты файлов. Каталоги снимков доступны только владельцу (`0700`/`0600` на Linux и macOS). Файлы входа (`.credentials.json`, `~/.claude.json`, `auth.json`, `oauth_creds.json`, `mcp-oauth-tokens.json`, `.env`, `settings.json`) и базы целиком не копируются; символьные ссылки пропускаются; имена и пути из `manifest.json` проверяются.»
- «Ограничения»: «Снимки Qwen не поддерживают `advanced.runtimeOutputDir` и относительные `QWEN_RUNTIME_DIR`/`QWEN_HOME`. Перенос Hermes проверен со схемой `state.db` 31; другая версия получает отказ. Hermes не принимает сессии больше 5 МиБ или 10 000 сообщений.»

- [ ] **Step 6: Полная проверка и commit**

Run: `cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: PASS.

```bash
git add crates/hub-claude/tests/live.rs crates/hub-codex/tests/live.rs crates/hub-qwen/tests/live.rs crates/hub-hermes/tests/live.rs README.md
git commit -m "Снимки сессий: живые тесты и документация"
```

- [ ] **Step 7: Версия 0.5.0**

`Cargo.toml:6`: `version = "0.5.0"`; `cargo check --workspace` (обновит версии членов в `Cargo.lock`).

Run: `cargo test --workspace --locked && git diff --stat`
Expected: PASS; изменены только `Cargo.toml` и `Cargo.lock`.

```bash
git add Cargo.toml Cargo.lock
git commit -m "Версия 0.5.0: снимки сессий"
```

---

### Task 13: Ручная проверка на Linux и Windows

Без изменений кода. Нашлась ошибка — исправление отдельным коммитом с тестом, воспроизводящим её, затем повтор шага.

- [ ] **Step 1: Живые тесты на каждой ОС**

Run (Linux/macOS): `AGENT_HUB_LIVE_CLI=1 cargo test -p hub-claude --test live -- --ignored live_snapshot && AGENT_HUB_LIVE_CODEX=1 cargo test -p hub-codex --test live -- --ignored snapshot && AGENT_HUB_LIVE_QWEN=1 cargo test -p hub-qwen --test live -- --ignored --test-threads=1 snapshot && AGENT_HUB_LIVE_HERMES=1 cargo test -p hub-hermes --test live -- --ignored`
(Windows PowerShell — те же команды с `$env:AGENT_HUB_LIVE_…=1;`.)
Expected: 4 PASS. Падение Hermes на `sessions delete` — сверить флаг (`hermes sessions delete --help`) и поправить тест; `Unsupported` «Hermes … не поддерживается» — сверить `SCHEMA_VERSION` и `--run-module` с установленной версией (расхождение 18) и не расширять поддержку без проверки. Падение Qwen на ходе 2 при успешном `import` — проверить, что первая запись чата содержит новый `cwd` (`head -c 400 ~/.qwen/projects/<san(new)>/chats/<id>.jsonl`).

- [ ] **Step 2: Linux, через бота**

1. «Настройки»: «Каталог снимков» пуст → после «Сохранить» в `settings.toml` есть `[snapshots] dir = ".../.agent-hub"`.
2. Тема «Починить тесты», бэкенд claude: задача → ответ. `/save` → «💾 Сохранено: pochinit-testy-<дата>-<время> · claude · <каталог>»; `ls -la ~/.agent-hub/snapshots/<имя>` — `drwx------`, `manifest.json` `-rw-------`; в `agent/` нет `.credentials.json`.
3. Новая тема → `/restore <имя>` → «♻️ Восстановлено …»; старая тема — «Сессия перенесена в другую тему»; `/status` в старой — «session: —». Вопрос «что мы делали?» в новой — агент помнит.
4. `/saves` — снимок сверху, с названием темы.
5. Во время хода `/save` → «Дождитесь конца хода или /stop».
6. `/restore <имя> ../..` → «⚠️ … вне корня …»; `/restore nope` → «⚠️ Снимок nope не найден»; `/save <то же имя>` → «⚠️ Снимок … уже существует».
7. Повторить 2–3 для codex, qwen (с `/restore <имя> <другой каталог>`), hermes (с профилем из настроек).
8. Codex: `codex` в терминале → архивировать сессию → `/save` → «Сессия в архиве Codex: выполните `codex unarchive <id>`».
9. Сбой посреди сохранения: `chmod 000 ~/.claude/projects/<P>/<id>` → `/save x` → предупреждение; в `~/.agent-hub/snapshots/` нет `.tmp-*`; вернуть права.
10. Логи: `rg -n "бирюза|\"content\"" ~/.local/share/agent-hub/logs` — пусто (в лог не попадает история).

- [ ] **Step 3: Windows**

Те же шаги 1–8 с поправками: каталог `%USERPROFILE%\.agent-hub\snapshots`; Hermes — `hermes.exe`/`hermes.cmd` из `%LOCALAPPDATA%\hermes\bin` (путь «Launcher», без окна консоли); Qwen — проверить `san` с нижним регистром (`projects\c--users-…`).

- [ ] **Step 4: Регрессия**

Обычные задачи, `/new`, `/cwd`, `/backend`, `/reset`, `/stop` во всех четырёх бэкендах работают как в 0.4.0; меню бота показывает `save`, `saves`, `restore` перед `help`.

---

### Task 14: Релиз `v0.5.0`

> **Push тега и веток — только после отдельного явного подтверждения пользователя.** Агент, выполняющий план, останавливается перед шагом 3 и спрашивает.

- [ ] **Step 1: Проверка перед тегом**

Run: `git status --short && cargo fmt --all --check && cargo clippy --workspace --all-targets --locked -- -D warnings && cargo test --workspace --locked`
Expected: чистое дерево, PASS; последний коммит — «Версия 0.5.0: снимки сессий».

- [ ] **Step 2: Аннотированный тег (локально)**

```bash
git tag -a v0.5.0 -m "agent-hub 0.5.0: снимки сессий"
git show --stat v0.5.0 | head -5
```

Expected: тег на коммите версии, сообщение в стиле `v0.4.0`.

- [ ] **Step 3: Push (только после подтверждения пользователя)**

```bash
git push origin main
git push origin v0.5.0
```

Expected: `release.yml` собирает Windows, macOS и Linux. Без подтверждения — остановиться после шага 2 и сообщить, что тег создан локально.

---

## Покрытие security-review и reliability-review

| Риск | Где закрыт |
|---|---|
| Учётные данные агента в снимке | Белый список в задачах 4–7: перечисляются только файлы сессии; тесты кладут `.credentials.json`, `auth.json`, `settings.json`, `oauth_creds.json`, `state_5.sqlite` рядом и проверяют, что их нет в снимке |
| Выход за каталог через имя снимка | Задача 1: `SnapshotName` — `[a-z0-9._-]`, без ведущей/завершающей точки и имён устройств Windows; тесты `../x`, `a/b`, `con` |
| Выход за каталог через `manifest.json` | Задача 1: `RelPath` без `..`, `.`, пустых частей, `\`, `:`, управляющих символов; `project` Claude — одна часть; задачи 4–7: импорт принимает только форму путей своей сессии (`Malformed` иначе), тесты с `.credentials.json`, `../`, чужим проектом/датой |
| Id сессии как часть имени файла | Задача 3: `file_stem` — `[A-Za-z0-9_-]{1,128}` |
| Символьные ссылки | Задача 3: `symlink_metadata`/`DirEntry::file_type` — ссылки пропускаются в обходе, источник-ссылка отвергается; задача 9: каталог снимка-ссылка — `Corrupt`; тесты `#[cfg(unix)]` |
| Права файлов снимка | Задачи 3, 9: каталоги `0700` (`DirBuilder::mode`, `restrict_dir` для существующего корня), файлы `0600` (`create_new` + `mode`); тесты прав |
| Перезапись данных агента | Задача 3: `place_all` создаёт файлы `create_new`, при ошибке удаляет созданные; задачи 4–7: `present` до `import`; Hermes `import_sessions` пропускает существующие id |
| Неполный снимок | Задача 9: staging `.tmp-<uuid>` → `manifest.json` с `sync_all` → `rename` → `fsync` каталога; любая ошибка — `Drop` удаляет staging; брошенные после сбоя — уборка старше суток |
| Гонка двух `/save` с одним именем | Задача 11: множество `naming` в акторе хаба + проверка перед `rename` (остаточное окно — только при внешней записи в каталог снимков) |
| Ход во время снимка, перепривязка при сбое импорта | Задача 11: `/save`/`/restore` отказывают во время хода; `snapshotting` блокирует `launch` и `/new`/`/cwd`/`/reset`/`/backend`; привязки меняются только после успешных `present`/`import` и только если ни тема, ни владелец не выполняют ход; одна правка `Topics` и один `put`; тест `a_failed_import_leaves_the_topics_unchanged` |
| Запуск Hermes | Задача 7: `std::process::Command` со списком аргументов, без оболочки; скрипт и данные — в `tempfile` `0700`; результат — только файлом, ограничен 1 МиБ; тайм-аут 120 с с `kill`; схема 31 сверяется в коде и в базе; `stdin`/`stdout` закрыты, `stderr` в файл, в лог — только `debug` |
| Подмена интерпретатора Hermes | Задача 7: интерпретатор — из файла `hermes`, который пользователь указал или нашёл `which` (тот же, что уже запускается для ACP); `#!/usr/bin/env` и неизвестные формы — отказ |
| Утечка истории в логи | Задачи 9–11: в `info`/`warn` — имя снимка, бэкенд, число файлов, вид отказа Hermes; задача 13, шаг 2.10 |
| Блокировка актора хаба | Задача 11: файловая работа и процессы — в `spawn_blocking`, хаб получает `HubMessage::Snapshot`; список снимков — тоже в отдельной задаче |
| Рост хранилища | Снимки удаляет пользователь (решение спецификации, README); staging убирается; список ограничен 30 строками, манифест — 1 МиБ, `files` — 10 000, обход — 100 000 файлов и глубина 16 |
| Совместимость `settings.toml` | Задача 8: `#[serde(default)] snapshots`, тест старого файла без секции |
| Формат снимка во времени | Задача 1: `manifest.version = 1`, неизвестная версия сообщается до разбора формы |

Остаточные риски: TOCTOU между `symlink_metadata` и `open` в каталогах агента (возможен только процессу того же пользователя, у которого и так есть доступ к истории); при остановке бота результат незавершённого `/restore` теряется (повтор идемпотентен); Hermes и Codex проверены по исходникам конкретных коммитов — несовпадение версий ловят `MIN_VERSION`, схема 31 и живые тесты.

## Самопроверка плана

- Имена сквозь задачи: `SnapshotName`, `NameError`, `Minute`, `default_name`, `RelPath`, `PathError`, `BackendData`, `Manifest`, `ManifestError` (1) → `SaveArgs`/`RestoreArgs`, `texts::{saved, restored, snapshot_list}` (2) → `Exported`, `ArchiveError` (+`From<PathError>`), `SessionArchive`, помощники (3) → `ClaudeArchive`/`CodexArchive`/`QwenArchive`/`HermesArchive` и `storage_root` с сигнатурами `(Option<&OsStr>, &Path)`, `(Option<&OsStr>, Option<&OsStr>, &Path) -> Result`, `HermesArchive::new(&Path, Option<String>, String) -> Result` (4–7) → `Agents::archive` в `HubAgents` (10) → `SaveJob`/`RestoreJob` (11) и живые тесты (12).
- `Settings.snapshots_dir`/`Draft.snapshots_dir` (8) используются в `Hub::save/saves/restore` и в `world_from` тестов (11); `SnapshotStore::{new, save, list, load}`, `Loaded`, `SnapshotError` (9) — в `transfer` и `Hub` (11).
- Тексты: дословные из спецификации — `NOTHING_TO_SAVE`, `NO_SNAPSHOTS`, `SESSION_MOVED`, `FINISH_TURN_FIRST`, «💾 Сохранено: …», «♻️ Восстановлено: …», «Сессия в архиве Codex: выполните `codex unarchive <id>`», «Hermes <версия> не поддерживается для переноса»; новые — перечислены в расхождении 14. Ожидания тестов хаба (`⚠️ Данные снимка повреждены: сбой`, `⚠️ Снимок nope не найден`, `⚠️ Снимок snap уже существует`) совпадают с `Display` `ArchiveError::Malformed`/`SnapshotError` и `texts::warning`.
- Временные ветки: `SNAPSHOTS_PENDING` (2) и заглушка `FakeAgents::archive` (10) заменяются в задаче 11.
- Форма `manifest.json` в тесте задачи 1 совпадает с примером спецификации (плюс `title: null` и `backend_data.kind`); протокол скрипта Hermes в задаче 7 одинаков в `archive.py`, `HermesArchive::run`, поддельном Python и заглушках.
- `Cargo.lock`: новые рёбра только к пакетам, уже бывшим в lock (`tempfile`, `chrono`); после задач 3–7 и 11 — `git diff --stat Cargo.lock`.
- Коммиты — по-русски, без строк атрибуции; push — только в задаче 14 после подтверждения пользователя.
