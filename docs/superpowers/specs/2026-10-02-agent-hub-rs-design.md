# agent-hub-rs — дизайн

Дата: 2026-10-02. Статус: согласован, ожидает плана реализации.

## Цель

Переписать [agent-hub](https://github.com/aprazdnikov/agent-hub) (Python, ~2.6k строк) на Rust:
Telegram-мост «тема форум-группы = сессия Claude Code» с полным функциональным паритетом,
распространяемый готовыми бинарниками под Windows, macOS и Linux, с десктопным GUI (трей +
окно), в котором настройки меняются без перезапуска приложения.

## Решения

| Вопрос | Решение |
|---|---|
| Форма приложения | Десктоп: иконка в трее + окно. Headless/Docker/systemd не переносятся |
| GUI-стек | `eframe`/`egui` + `tray-icon`/`muda` |
| Редактируемые на лету настройки | Telegram (токен, chat_id, allowlist), корень workspace, параметры Claude, таймауты |
| Содержимое окна | Статус, Темы, Лог, Настройки |
| Хранение токена | Системное хранилище (`keyring`); остальное — `settings.toml` |
| Объём v1 | Полный паритет с Python-версией |
| Применение изменений | Со следующего запуска сессии; смена токена/чата/allowlist перезапускает бота и останавливает сессии |
| Бэкенды | Как в Python: тема хранит бэкенд + cwd + session_id; трейт `AgentBackend`, одна реализация Claude; параметры Claude глобальные |
| Интеграция с Claude | Собственная реализация stream-json протокола `claude` CLI; CLI — установленный у пользователя |
| Дистрибуция | GitHub Releases по тегу для 4 таргетов; автообновление по кнопке с проверкой SHA-256 |
| Язык UI | Русский (бот и окно) |

Не входит в v1: инсталляторы (.msi/.dmg), подпись и нотаризация, автозапуск при входе,
переопределения модели/режима на уровне темы, подпись релизов (minisign), миграция
существующего `topics.json` из Python-версии (формат совместим, файл можно скопировать вручную).

## Функциональный паритет

Поведение и тексты сообщений бота повторяют Python-версию (README agent-hub — эталон):

- Новая тема → новая сессия в корне workspace; сессия создаётся и при первом сообщении в теме,
  созданной, пока бот был выключен. Тема General не используется — бот просит создать тему.
- Сообщения во время работы агента уходят в ту же сессию (inbox); разные темы параллельны.
- Фоновые задачи агента: сессия держится открытой до их результата или до
  `background_timeout`; затем сообщение со списком брошенных задач.
- Команды: `/new [backend] [путь]`, `/cwd <путь>`, `/reset`, `/stop`, `/status`, `/help`.
  Пути абсолютные, с `~` или относительно корня; выход за корень (в т. ч. симлинками) запрещён.
- Подтверждения `🔐 Tool` с кнопками ✅/❌; таймаут → запрет.
- `AskUserQuestion`: кнопки вариантов, мультивыбор `☐/☑` + `✅ Готово`, `❌ Не отвечать`,
  ответ текстом следующим сообщением; таймаут → отказ.
- Вложения: фото и альбомы → изображения в prompt (подпись — текст задачи); файлы, аудио, видео
  → `.agent-hub/uploads/` в cwd (со своим `.gitignore`), путь добавляется к задаче, лимит 20 МБ;
  голосовые и видеосообщения → подсказка.
- Инструмент `send_file` (in-process MCP `agent-hub`): только файлы внутри cwd после разрешения
  симлинков, лимит 50 МБ, ошибка доставки возвращается агенту.
- Вывод: Markdown агента → Telegram HTML (жирный, курсив, код, блоки кода с языком, ссылки,
  цитаты, заголовки → жирный, списки → строки с маркерами, таблицы из 2 колонок → «**ключ**:
  значение», шире → карточки); разбиение длинных ответов по границам блоков; при отказе
  Telegram от разметки — plain text. `🔧 Tool: …`, `✅ Готово · ходов: N · $X`, `❌ …`,
  `⏹ Остановлено`.
- `session_id` сохраняется сразу при старте хода.
- Остановка приложения прерывает сессии с сообщением «⏹ Остановлено».

## Архитектура

### Процессы и потоки

- Главный поток: eframe (окно) и `tray-icon` — на macOS оба обязаны жить в главном потоке.
- Фоновый поток: tokio-runtime — бот, сессии, хранилища.
- GUI ↔ ядро только через каналы:
  - `Command` (GUI → ядро, `mpsc`): `SaveSettings`, `StartBot`, `StopBot`, `StopTopic`,
    `ResetTopic`, `InstallUpdate`, `Quit`.
  - `Snapshot` (ядро → GUI, `watch`): статус бота, темы, хвост лога, доступное обновление; после
    публикации ядро вызывает `egui::Context::request_repaint()`.
- Закрытие окна сворачивает в трей; выход — «Выход» в трее.

### Cargo workspace

| Крейт | Роль | Ключевые зависимости |
|---|---|---|
| `hub-core` | Функциональное ядро без I/O: доменные типы, разбор настроек и формы, раскрытие путей workspace (без обращения к ФС), имена и пути вложений, разбор команд, Markdown → Telegram HTML, разбиение сообщений, callback_data, реестры подтверждений и вопросов (обобщённые по ответчику), формат `topics.json` | serde, serde_json, pulldown-cmark, rust_decimal, thiserror |
| `hub-claude` | Протокол `claude` CLI: чистые модули `wire`, `activity` (`SessionActivity`), `tracker` (`SessionTracker`), `permissions` (решения `can_use_tool`, разбор вопросов, сводки инструментов) + оболочка: процесс, транспорт, control-запросы, MCP `send_file`, трейты `AgentBackend` и `UserChannel` | tokio, serde_json, tokio-util |
| `hub-telegram` | Обработчики, актор `Hub`, `TelegramSender`, `UserChannel`, альбомы, загрузки | teloxide, tokio |
| `hub-app` | Бинарник: супервизор, хранилища (`settings.toml`, keyring, `topics.json`), tracing, GUI, трей, автообновление, single-instance | eframe, tray-icon, muda, keyring, directories, rfd, fs4, self_update, self_replace, tracing, tracing-appender |

`hub-core` не зависит от tokio и не делает I/O — граница проверяется компилятором.

Проверки путей, требующие файловой системы (канонизация, симлинки, существование каталога, размер файла), живут в оболочках: `hub-telegram::paths` (`/new`, `/cwd`, вложения, `send_file`) и `hub-app` (корень workspace из настроек). Канонизация — `dunce::canonicalize`, чтобы на Windows не появлялся префикс `\\?\`.

### Хранение

| Что | Где |
|---|---|
| Настройки (кроме токена) | `config_dir/agent-hub/settings.toml` |
| Токен бота | keyring, сервис `agent-hub`, учётная запись `telegram-token` |
| Привязки тем | `data_dir/agent-hub/topics.json`, формат v1 Python-версии |
| Логи | `data_dir/agent-hub/logs/` (ежедневная ротация) |
| Lock single-instance | `data_dir/agent-hub/agent-hub.lock` |

Каталоги — через `directories::ProjectDirs`. Запись `settings.toml` и `topics.json` атомарна:
временный файл в том же каталоге → fsync → rename.

### Настройки

```text
Settings
├─ telegram: token (keyring), chat_id: ChatId, allowed_users: NonEmpty<UserId>
├─ workspace_root: WorkspaceRoot (абсолютный, существующий, канонизированный)
├─ claude: cli_path: Option<PathBuf>, model: Option<Model>, permission_mode: PermissionMode,
│          max_budget_usd: Option<Budget>
├─ timeouts: approval: Duration, background: Duration
└─ app: check_updates: UpdateCheck (Enabled | Disabled)
```

- Одна функция разбора `Settings::parse(raw) -> Result<Settings, Vec<FieldError>>` для файла и
  для черновика формы.
- Живое значение — `watch::Sender<Arc<Settings>>`. Супервизор сравнивает старую и новую версию:
  изменились `telegram.*` → остановка сессий и пересоздание бота; иначе новые значения читаются
  при следующем запуске сессии.

## hub-claude: протокол CLI

### Запуск

Одна сессия = один процесс:

```text
claude --output-format stream-json --verbose --input-format stream-json
       --permission-prompt-tool stdio --permission-mode <mode>
       [--model <m>] [--max-budget-usd <b>] [--resume=<session_id>]
       --setting-sources=user,project,local --replay-user-messages
       --mcp-config '{"mcpServers":{"agent-hub":{"type":"sdk","name":"agent-hub"}}}'
```

- `cwd` — директория темы; окружение `CLAUDE_CODE_ENTRYPOINT=sdk-rs`.
- Системный промпт — стандартный промпт Claude Code (флаг `--system-prompt` не передаётся). Python
  SDK передавал `--system-prompt ""`, то есть Python-бот работал без него; отличие согласовано.
- `tokio::process::Command`, `kill_on_drop(true)`; на Windows `CREATE_NO_WINDOW`.
- Путь к CLI — из настроек, иначе поиск `claude` в `PATH`.
- После старта — control `initialize` (таймаут 60 с).

### Слои

1. `wire` — serde-типы.
   - Входящие: `#[serde(tag = "type")]` — `system` (`init`, `task_started`, `task_updated`,
     `task_notification`), `assistant`, `user` (replay с `uuid`), `result` (с `origin`),
     `control_request`, `control_response`, `control_cancel_request`. Неизвестные типы/подтипы
     → `Unknown`, debug-лог, сессия продолжается.
   - Исходящие: `user` (base64-изображения первыми, затем текст; `uuid`), `control_request`
     (`initialize`, `interrupt`), `control_response` (`success`/`error`).
2. `transport` — построчное чтение stdout (лимит строки 16 МБ), запись в stdin через `mpsc`,
   stderr → лог.
3. `control` — обработка входящих control-запросов, каждый в отдельной задаче (ожидание
   человека до `approval_timeout`), `control_cancel_request` отменяет задачу:
   - `can_use_tool`:
     - `mcp__agent-hub__send_file` → allow без вопроса;
     - `AskUserQuestion` с разбираемым input → `channel.ask`; `Answered` →
       `{"behavior":"allow","updatedInput":{...input,"answers":{...}}}`; `Denied` → deny;
     - иначе → `channel.request(ToolRequest{tool, summary})`.
     - Ответ allow: `{"behavior":"allow","updatedInput":<input>}`; deny:
       `{"behavior":"deny","message":<reason>}`.
   - `mcp_message` для сервера `agent-hub` — JSON-RPC: `initialize`, `notifications/initialized`
     (ack `{"jsonrpc":"2.0","result":{}}`), `tools/list`, `tools/call send_file`
     (`{path, caption?}` → `channel.send_file`; ошибки — `isError: true`).
   - Прочие подтипы → `control_response` с `error`.
4. `session` — цикл `converse`: `tokio::select!` по сообщениям CLI, inbox и дедлайну фазы.
   Решения принимают чистые `SessionActivity` (фазы `Busy`/`Background`/`Settling`/`Idle`,
   окно settling 30 с) и `SessionTracker` (сообщения → `AgentEvent`) из модулей `hub-claude` — перенос
   логики `backends/claude.py`.

### Контракт

```rust
impl ClaudeBackend {
    pub async fn run(
        &self,
        session: &TopicSession,
        prompt: Prompt,
        inbox: mpsc::Receiver<Prompt>,
        conversation: Conversation, // channel, events, cancel, limits
    ) -> mpsc::Receiver<Prompt>;
}
```

- Inbox возвращается вызывающему: сообщения, пришедшие, пока разговор закрывался, не теряются —
  актор `Hub` проверяет его и при необходимости запускает разговор снова.
- Выбор бэкенда — enum-диспетчеризация в `hub-telegram`: `enum Backend { Claude(ClaudeBackend) }`,
  `match` по `BackendKind` без wildcard.
- `AgentEvent = SessionStarted | AssistantText | ToolCall | Finished | Failed | BackgroundAbandoned`.
- Ошибки агента — событие `Failed`, не `Err`.
- Отмена: `interrupt` → закрытие stdin → kill через 5 с.

### Совместимость CLI

При старте бота `claude --version` (таймаут 10 с); минимальная версия 2.1.280. Нет CLI или
версия ниже → `BotStartError::ClaudeMissing` / `ClaudeTooOld`.

## hub-telegram

### Актор `Hub`

Одна tokio-задача владеет всем изменяемым состоянием: `running: HashMap<TopicKey, LiveSession>`,
`TopicStore`, реестры подтверждений и вопросов, буфер альбомов. Сообщения обрабатываются
последовательно — гонка «сообщение пришло, пока сессия закрывается» исключена построением.

### Входящие апдейты

1. Long polling (teloxide).
2. Guard: чужой чат или пользователь вне allowlist → отбросить, лог
   `rejected update chat_id=… user_id=…`.
3. Чистая классификация в `hub-core`: `Command`, `Text`, `Media`, `AlbumPart`, `Unsupported`,
   `TopicCreated`, `Callback`, `General`.
4. Сообщение в `Hub`.

### Сообщение в теме

- Открыт вопрос в теме → текст — ответ на вопрос.
- Сессии нет → `Hub` запускает задачу сессии: загрузка вложений → `backend.run` → события →
  `TelegramSender`.
- Сессия есть → `Prompt` в её inbox; вложения сначала скачиваются вспомогательной задачей.
- Задача сессии завершилась → `Hub::SessionEnded`; если в inbox остался prompt — `Hub`
  перезапускает сессию с ним.
- Альбомы: части копятся по `media_group_id`, через 1 с таймер шлёт `Hub::FlushAlbum`.

### `UserChannel`

```rust
pub trait UserChannel: Send + Sync {
    fn request(&self, tool: ToolRequest) -> BoxFuture<'_, Decision>;            // Allowed | Denied
    fn ask(&self, questions: Vec<Question>) -> BoxFuture<'_, QuestionsOutcome>; // Answered | Denied
    fn send_file(&self, file: OutgoingFile) -> BoxFuture<'_, FileDelivery>;     // Delivered | Denied
}
```

Трейт объявлен в `hub-claude` рядом с `AgentBackend` (контракт бэкенда), реализуется в
`hub-telegram`. `BoxFuture` (`futures::future::BoxFuture`) делает его dyn-совместимым:
`hub-claude` получает `Arc<dyn UserChannel>` и не зависит от `hub-telegram`.

Telegram-реализация: сообщения с инлайн-кнопками, `oneshot` в реестре, `timeout(approval)`,
после ответа кнопки снимаются. `callback_data` — формат Python-версии, разбор в `hub-core`.

### `TelegramSender`

- `RetryAfter` → одна повторная попытка после паузы.
- Отказ HTML-разметки → повтор plain-текстом.
- Текст — best-effort (ошибка логируется, агент продолжает); документ — ошибка возвращается.
- `typing` при старте сессии.
- Лимиты из Python: подтверждение 3500, вызов инструмента 900, ошибка 3500, ответ 500,
  подпись 1024 символа.

### Snapshot и лог

- После каждого изменения `Hub` публикует состояние тем в супервизор → `Snapshot`.
- `tracing`: слой кольцевого буфера (500 строк) для GUI + `tracing-appender` в файл.
- Остановка (Quit / смена Telegram-настроек): отмена всех сессий, «⏹ Остановлено» в темы,
  общий таймаут 10 с, затем остановка dispatcher.

### Уточнения реализации (фаза 3)

- Реестры подтверждений и вопросов — `Arc<Mutex<Registries>>`, а не состояние актора: канал ждёт
  ответа в задаче сессии, нажатие кнопки обрабатывается отдельной задачей; блокировка держится
  только на время вставки/удаления.
- Сообщения, пришедшие после `/stop` и не прочитанные сессией, запускают новую сессию (Python их
  терял вместе с задачей).
- `/help` и подсказка «создайте тему» вне темы отправляются ответом на сообщение, как `reply_text`
  в Python.

## GUI и трей

### Трей

- Иконка: зелёная — подключён; серая — остановлен/не настроен; красная — ошибка.
- Tooltip: `agent-hub — подключён · активных сессий: N`.
- Меню: «Открыть», «Запустить/Остановить бота», «Доступно обновление vX.Y.Z» (когда есть),
  «Выход».
- Linux без трея → окно не прячется при закрытии, закрытие = выход.

### Окно (вкладки)

1. **Статус** — состояние бота и текст ошибки, Старт/Стоп, версия `claude`, username бота,
   число активных сессий, баннер обновления.
2. **Темы** — тема (thread_id и название из `forum_topic_created`, если известно), бэкенд,
   cwd, session_id (сокращённый, копируется по клику), статус (свободна / работает / фон N /
   ждёт ответа); Stop и Reset (Reset — с подтверждением).
3. **Лог** — хвост 500 строк, фильтр уровня, автопрокрутка, «Открыть папку логов». Строки
   `rejected update` подсвечены, с кнопками «Использовать chat_id» / «Добавить user_id».
4. **Настройки** — форма-черновик всех полей `Settings`; токен — поле-пароль с «показать»;
   выбор папки через `rfd`; `bypassPermissions` с предупреждением; ошибки разбора под полями,
   «Сохранить» неактивна при ошибках; «Отменить изменения». Пометка у Telegram-полей о
   прерывании сессий, при активных сессиях — подтверждение.

### Запуск

- Нет настроек → окно на вкладке «Настройки», бот не стартует; после первого сохранения —
  стартует.
- Второй экземпляр: lock-файл занят → диалог «agent-hub уже запущен», выход.

### Уточнения реализации (фаза 4)

- Single-instance — `std::fs::File::try_lock` (стабилен с Rust 1.89) вместо `fs4`.
- Трей на Linux — бэкенд `ksni` (StatusNotifierItem по D-Bus, свой поток), GTK в процессе не нужен;
  если трей не создался, закрытие окна = выход (на любой ОС). На Linux закрытие окна при наличии
  трея сворачивает его, а не прячет: хоста трея может не быть (GNOME без расширения), а Wayland не
  умеет скрывать окна.
- Временный сбой запуска (Telegram недоступен по сети) повторяется автоматически: 5 с, удвоение
  до 5 мин; любая команда пользователя отменяет ожидание.
- Записи крейта `log` (teloxide) попадают в лог через мост `tracing-log`.
- События трея обрабатываются в `App::logic`: eframe 0.36 вызывает его и при скрытом окне после
  `request_repaint`.
- Статус темы во вкладке «Темы» — «свободна» / «работает»: актор `Hub` не отслеживает фон и
  ожидание ответа.
- Время в строках вкладки «Лог» — UTC.
- Баннер и пункт меню обновления, команда `InstallUpdate` — фаза 5.

### Уточнения реализации (фаза 5)

- Для автообновления релиз публикует ещё и голый бинарник `agent-hub-<target>[.exe]` + `.sha256`:
  приложению не нужно распаковывать zip/tar.gz. Архивы остаются для ручной установки.
- Подтверждение при активных сессиях спрашивается на «Перезапустить», а не на «Установить»: сессии
  прерывает перезапуск, установка их не трогает.
- Ошибка проверки обновлений только пишется в лог; баннер показывает доступную версию, ход установки
  и ошибку установки.
- Иконка — `assets/agent-hub.ico` в репозитории (генерирует `scripts/make-icon.py`); окно и трей рисуют
  свою программно.

## Ошибки и надёжность

### Типы ошибок

| Тип | Варианты | Потребитель |
|---|---|---|
| `SettingsError` / `FieldError` | по полям | форма настроек |
| `StoreError` | `Read`, `Corrupt`, `Write` | супервизор → статус |
| `AttachmentError` | `TooLarge`, `OutsideWorkspace`, `Symlink`, `Io` | тема (`⚠️ …`) |
| `WorkspaceError` | `NotFound`, `NotDirectory`, `OutsideRoot` | ответы `/new`, `/cwd` |
| `BotStartError` | `InvalidToken`, `ChatNotFound`, `ClaudeMissing`, `ClaudeTooOld`, `Network` | статус + иконка |
| `UpdateError` | `Check`, `Download`, `Checksum`, `Replace` | баннер обновления |

- Внутренняя склейка — `anyhow`.
- Повреждённый `topics.json` не перезаписывается: бот не стартует, в статусе — путь к файлу.
- Паника в задаче сессии → `JoinError` → «💥 Внутренняя ошибка agent-hub», подробности в лог;
  `Hub` продолжает работу.

### Таймауты и лимиты

| Операция | Значение |
|---|---|
| HTTP-клиент Telegram | 30 с (long poll 25 с) |
| Отправка документа | 120 с |
| `initialize` CLI | 60 с |
| `claude --version` | 10 с |
| Kill после interrupt | 5 с |
| Остановка приложения | 10 с |
| Проверка обновлений | 15 с |
| Строка stdout CLI | 16 МБ |
| Канал событий сессии | `mpsc(256)` |
| Загрузка вложения / отправка файла | 20 МБ / 50 МБ |

### Секреты

- Токен только в keyring; не логируется (уровень логов `reqwest`/`hyper` ограничен; тест на
  отсутствие токена в отформатированных ошибках).
- Источник обновлений зашит в бинарник.

## Тестирование

- `hub-core` — `rstest` + `insta`; перенос Python-тестов как спецификации: `test_markdown_html`,
  `test_render`, `test_approvals`, `test_questions`, `test_attachments`, `test_commands`,
  `test_store` (включая чтение файла Python-версии), `test_workspace`, `test_config`,
  `test_activity`, `test_claude_backend` (трекер и разбор вопросов).
- `hub-claude` — snapshot-тесты разбора на записанных JSONL реального CLI 2.1.287; интеграционные
  тесты `session` с поддельным CLI внутри теста поверх `tokio::io::duplex`, живой CLI — тест
  `#[ignore]` `live_cli_round_trip`; сценарии:
  `can_use_tool`, `AskUserQuestion`, `mcp_message`/`send_file`, interrupt, фоновая задача.
- `hub-telegram` — тесты актора `Hub` с фейковыми `Sender`/`AgentBackend`: сообщение во время
  закрытия сессии, альбомы, `/stop`, fallback на plain-текст.
- GUI — без автотестов; логика формы в `hub-core`.
- Сквозной сценарий Telegram + Claude — вручную в тестовой группе.

### Строгость

- `rust-toolchain.toml`: 1.96.
- `[workspace.lints]`: `rust.warnings = "deny"`, `unsafe_code = "forbid"`, clippy `pedantic`,
  `unwrap_used`, `expect_used`, `panic`, `indexing_slicing` = deny.
- `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`,
  `cargo test --locked`, `cargo deny check` — в CI и pre-commit.

## CI, релизы, автообновление

### CI (`.github/workflows/ci.yml`)

Push в main и PR; матрица ubuntu/macos/windows: fmt, clippy, test. На Ubuntu —
`libgtk-3-dev libxdo-dev libayatana-appindicator3-dev`. Кэш `Swatinem/rust-cache`.
`cargo deny check` — отдельной задачей.

### Релиз (`.github/workflows/release.yml`, тег `v*`)

| Таргет | Раннер | Архив |
|---|---|---|
| `x86_64-pc-windows-msvc` | windows-latest | `.zip` |
| `aarch64-apple-darwin` | macos-latest | `.tar.gz` |
| `x86_64-apple-darwin` | macos-latest (кросс) | `.tar.gz` |
| `x86_64-unknown-linux-gnu` | ubuntu-22.04 | `.tar.gz` |

Имя: `agent-hub-<target>.<ext>` + `.sha256`. Публикация — `softprops/action-gh-release`.
Windows: `#![windows_subsystem = "windows"]`, иконка через `winresource`. Без подписи — README
описывает обход Gatekeeper (`xattr -d com.apple.quarantine`) и SmartScreen.

### Автообновление

- Проверка при старте и раз в 24 ч (если включено), GitHub Releases `aprazdnikov/agent-hub-rs`.
- Найдено → баннер в окне и пункт в трее; установка только по кнопке.
- Установка: скачивание архива своего таргета → проверка SHA-256 → `self_replace` → предложение
  перезапуска; при активных сессиях — подтверждение их остановки.
- Остаточный риск: контрольная сумма из того же релиза защищает от повреждения, но не от
  компрометации GitHub-аккаунта; подпись релизов — отдельная задача.

## README

README agent-hub-rs переписывается под десктоп: установка из Releases, первый запуск и
вкладка «Настройки», получение chat_id/user_id через вкладку «Лог», команды бота, безопасность
(перенос раздела Python-версии + keyring + обновления), обход Gatekeeper/SmartScreen,
требование установленного Claude Code ≥ 2.1.280, разработка.
