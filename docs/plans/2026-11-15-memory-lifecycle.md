# MEM-03 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Управление жизненным циклом локальной памяти из настольного приложения и Telegram: ручная очистка по умолчанию, явно включаемая автоочистка, состояния, экспорт и импорт.

**Architecture:** Единый Rust API в hub-memory выполняет операции под блокировкой с проверкой поколения snapshot. GUI и Telegram показывают предварительный просмотр и передают подтверждение; агентский MCP получает только чтение, поиск и запись. Формат v2 поддерживает разреженную историю, состояния, политику, отметки удаления и идентификатор поколения хранилища.

**Tech Stack:** Rust, serde_json, SHA-256, egui, tokio; существующие файловые операции с атомарной заменой. Для случайного incarnation использовать уже имеющуюся workspace-зависимость uuid с v4 после проверки её конфигурации.

**Spec:** [Утверждённый дизайн](../specs/2026-11-15-memory-lifecycle-design.md).

## Global Constraints

- Ручной режим по умолчанию. Включение автоматического режима само ничего не удаляет.
- Автоочистка при записи: последние 32 ревизии; при превышении 8 МиБ удалять старейшие исторические ревизии, сохраняя последнюю каждого ключа. Порядок: время, ключ, ревизия.
- Максимум 500 неудалённых ключей, 4096 отметок удаления, 8 МиБ итогового JSON; входящий экспорт до 16 МиБ. Существующие лимиты полей сохраняются.
- Номера ревизий u32 монотонны; никакого переиспользования номеров и скрытого удаления целых ключей.
- Импорт атомарный, без автоочистки и без включения входящей политики. Расходящаяся история не заменяет локальную.
- Любое подтверждение привязано к snapshot и проекту; callbacks дополнительно к пользователю, чату и теме.
- Миграция v1 только при изменении, после проверенной резервной копии. Чтение v1 ничего не создаёт.
- Разработка и тесты работают с временными хранилищами. Реальную память пользователя не мигрировать и не очищать.
- Исполнение самостоятельно, в текущем checkout с сохранением существующих изменений MEM-01/MEM-02. Без commit, push, публикации и перезапуска пользовательского процесса.

## Review Focus

1. Сбой между backup и записью v2: исходный файл читается, повтор миграции безопасен — задача 1.
2. Агент держит старый Store во время очистки/пересоздания: запись не воскрешает данные — задачи 2 и 3.
3. Два разных входящих ключа переименованы в один целевой: импорт отклоняется целиком — задача 4.
4. Отмена/смена проекта во время фоновой операции: результат не применяется к новому экрану и подтверждение недействительно — задача 5.
5. Telegram-вложение имеет неверный или отсутствующий размер: поток ограничивается фактически прочитанными байтами, без передачи импорта агенту — задача 6.

## Раскладка и общие типы

`hub-memory/src/store.rs` остаётся владельцем блокировки, открытия и транзакций.
Выделить существующую файловую реализацию без изменения поведения в
`hub-memory/src/store/filesystem.rs`; модель snapshot и её валидацию —
`store/snapshot.rs`. Новые `store/lifecycle.rs` и `store/portable.rs` реализуют
административные операции и перенос. Публичные типы — `lifecycle.rs` и
`portable.rs`, экспорт через `lib.rs`. MCP остаётся в `protocol.rs`.

Общие типы API:

- `MemoryStatus { Active, Stale, Archived }`, `StatusFilter { Active, Stale, Archived, All }`.
- `CleanupPolicy { Manual, Automatic }`; обе enum сериализуются snake_case, default Manual/Active.
- `SnapshotToken { incarnation: String, generation: u64 }`, проверяется целиком.
- `EntryDetails { entry: Entry, status: MemoryStatus, revisions: Vec<u32>, history_pruned: bool }`.
- `MemoryOverview { token: SnapshotToken, policy: CleanupPolicy, page: MemoryPage, tombstone_count: usize, last_cleanup: Option<CleanupSummary> }`.
- `CleanupSummary { removed_revisions: usize, performed_at: i64 }`.
- `AdminAction`: `SetPolicy(CleanupPolicy)`, `SetStatus { key, status }`, `Prune { revisions: BTreeMap<String, Vec<u32>> }`, `Delete { keys: Vec<String> }`.
- `AdminPreview { token, action, removed_keys: Vec<String>, removed_revisions: usize, before_bytes: u64, after_bytes: u64 }`. Все пользовательские поля перепроверяются при apply; preview не является доверенным разрешением.
- Новые ошибки: устаревший preview, пересозданное хранилище, архивный/удалённый ключ, недоступная сокращённая ревизия. Тексты ошибок не содержат тела памяти.

### Task 1: Формат v2, чтение и безопасная миграция

**Files:** `crates/hub-memory/src/{store.rs,model.rs,lib.rs}`, новые `store/{snapshot.rs,filesystem.rs}`, `crates/hub-memory/Cargo.toml`, `Cargo.lock`; тесты `crates/hub-memory/tests/storage/migration.rs`, подключение в `tests/storage.rs`.

**Interfaces:** Внутренний `Snapshot` хранит project, incarnation, generation, policy, записи с history/status/pruned/provenance, tombstones и last_cleanup. `Snapshot::decode(bytes: &[u8], project: &Path) -> Result<Self, Error>` читает v1/v2; версия исходного формата сохраняется отдельно от сериализуемого v2. `Store::persist_snapshot(&self, snapshot: &Snapshot) -> Result<(), Error>` сохраняет проверенный backup v1 перед атомарной записью v2. Существующие Store/Reader API сохраняются.

- [ ] Написать тесты: `read_v1_does_not_migrate` (байты/состав каталога прежние), `first_mutation_preserves_exact_v1_backup`, `migration_retry_reuses_matching_backup`, `different_backup_blocks_migration`, `unknown_schema_is_rejected`, `v2_rejects_duplicate_keys_and_revisions`.
- [ ] Запустить `cargo test -p hub-memory --test storage migration`; подтвердить отсутствие реализации v2.
- [ ] Выделить файловый модуль без изменения гарантий NOFOLLOW, прав, sync и атомарной замены; реализовать decode/validate v1/v2 и запись backup через create_new, sync, повторное чтение и сравнение. Несовпадающую существующую копию не заменять.
- [ ] Обеспечить стабильный token v1 до миграции: incarnation из SHA-256 исходного snapshot, generation 0; Store, открытые на v1, допускают только миграцию с этим исходным incarnation. Новый пустой Store получает общий incarnation под блокировкой при первой записи; конкурирующие открытия пустого проекта не должны конфликтовать из-за независимых случайных ID.
- [ ] Добавить тесты сбоя сохранения backup/основного файла и повторного открытия, включая два Store, открытых до первой записи; успешная миграция не блокирует обычное продолжение тем же Store.
- [ ] Запустить `cargo test -p hub-memory`; поправить фикстуры, не ослабляя проверок v1 и старых писателей, отклоняющих schema_version 2.

### Task 2: Монотонная история и автоочистка

**Files:** `crates/hub-memory/src/store.rs`, `store/snapshot.rs`, новый `store/lifecycle.rs`, `lifecycle.rs`; тесты `tests/storage/retention.rs`.

**Interfaces:** `Store::search_filtered(&self, query: &str, limit: usize, filter: StatusFilter) -> Result<Vec<SearchHit>, Error>`; прежний search делегирует Active. `MemoryReader::details(&self, key: &str, revision: Option<u32>) -> Result<EntryDetails, Error>`; `MemoryReader::overview(&self, query: &str, offset: usize, limit: usize, filter: StatusFilter) -> Result<MemoryOverview, Error>`.

- [ ] Написать `manual_33rd_revision_preserves_file`, `automatic_33rd_keeps_2_through_33`, `pruned_retry_cannot_append`, `retry_uses_revision_number`, `revision_overflow_preserves_file`, `automatic_size_pruning_keeps_every_latest`, `impossible_fit_rolls_back_pruning`, `equal_timestamps_have_stable_order`.
- [ ] Запустить `cargo test -p hub-memory --test storage retention`, увидеть ожидаемые отказы.
- [ ] Реализовать append с поиском retry по expected_revision+1, checked_add, чтением policy под lock, фильтрацией поиска и фактическим списком ревизий. После успешной транзакции увеличить generation; no-op его не меняет.
- [ ] Реализовать чистую функцию отбора старой истории в snapshot: сначала 32 на ключ, затем итоговый сериализованный размер, детерминированно. На ошибке ничего не сохранять. Проверить верхнюю границу работы при 500×32 ревизиях; не сериализовать весь snapshot на каждый удаляемый элемент без необходимости.
- [ ] Добавить `policy_change_seen_by_already_open_store` и `retention_never_removes_latest_archived_entry`; запустить все тесты hub-memory.

### Task 3: Ручные операции, отметки удаления и новое поколение

**Files:** `crates/hub-memory/src/{lifecycle.rs,model.rs,lib.rs}`, `store/lifecycle.rs`, `store/snapshot.rs`; тесты `tests/storage/lifecycle.rs`.

**Interfaces:** `Store::preview(&self, action: AdminAction) -> Result<AdminPreview, Error>`; `Store::apply(&self, preview: AdminPreview) -> Result<SnapshotToken, Error>`; `Store::reset(&self, token: SnapshotToken, keep: Vec<String>, export: VerifiedExport) -> Result<SnapshotToken, Error>`. `VerifiedExport` — закрытый от конструирования вызывающим кодом тип задачи 4, привязанный к точному token и SHA-256 экспорта.

- [ ] Написать `stale_preview_changes_nothing`, `archive_blocks_agent_write`, `stale_and_archive_hidden_by_default`, `status_change_preserves_revision`, `delete_blocks_delayed_writer`, `tombstone_limit_preserves_file`, `prune_cannot_remove_latest`, `enabling_auto_does_not_prune`.
- [ ] Запустить `cargo test -p hub-memory --test storage lifecycle` и зафиксировать ожидаемые отказы.
- [ ] Реализовать preview и повторный расчёт операции внутри apply под lock; проверять token/лимиты/дубликаты выбранных ключей и ревизий. Удалённые ключи сохраняют latest revision и provenance. Ограничения данных валидировать до backup/сохранения.
- [ ] После задачи 4 написать `reset_requires_current_verified_export`, `reset_keeps_selected_history`, `old_store_cannot_write_after_reset`, `failed_reset_preserves_incarnation`; реализовать reset с новым UUID и проверкой incarnation на всех изменениях Store.
- [ ] Запустить `cargo test -p hub-memory`; не выполнять административные операции с реальным каталогом пользователя.

### Task 4: Экспорт и атомарный импорт

**Files:** новые `crates/hub-memory/src/{portable.rs,store/portable.rs}`; `store/snapshot.rs`, `lib.rs`; тесты `tests/storage/portable.rs`.

**Interfaces:** `MemoryReader::export(&self) -> Result<MemoryExport, Error>`; `MemoryExport::write_new(&self, path: &Path) -> Result<VerifiedExport, Error>` записывает с create_new, sync и проверкой чтением. `MemoryExport::decode(reader: impl Read) -> Result<Self, Error>` ограничивает чтение 16 МиБ+1. `Store::preview_import(&self, export: MemoryExport, choices: BTreeMap<String, ImportChoice>) -> Result<ImportPreview, Error>`; `Store::apply_import(&self, preview: ImportPreview) -> Result<SnapshotToken, Error>`.

`ImportChoice { KeepLocal, CopyAs(String) }`; совместимые новые/продолженные истории применяются автоматически после общего подтверждения, конфликты по умолчанию KeepLocal. `ImportPreview` содержит token целевого проекта, исходный export, choices, классификацию каждого ключа, размер результата и межпроектный признак. Источники/авторы сохраняются как данные, без приписывания доверия.

- [ ] Написать тесты `roundtrip_preserves_history_and_sources`, `import_does_not_enable_auto`, `repeat_import_is_noop`, `matching_overlap_appends`, `disjoint_or_divergent_history_conflicts`, `incoming_tombstone_cannot_delete_live_key`, `renamed_copy_retry_after_local_write_is_noop`, `duplicate_targets_reject_whole_import`, `oversized_stream_stops_at_limit`, `import_never_auto_prunes`, `stale_import_preview_is_rejected`.
- [ ] Запустить `cargo test -p hub-memory --test storage portable`, подтвердить отказы.
- [ ] Реализовать versioned envelope и строгую валидацию, канонический fingerprint исходной записи с областью/ключом, хранение происхождения с записью/отметкой. Для совместимости проверять все общие ревизии и наличие хотя бы одной общей; конфликт статуса/метаданных не затирать молча.
- [ ] Реализовать preview/apply на копии snapshot с одной атомарной записью, ограничением размера и generation CAS. При копировании переписать ключ каждой Entry на новый ключ, сохранив исходный ключ в provenance; повторное применение не дублирует копию.
- [ ] Проверить экспорт в существующий файл/симлинк, ошибки записи/проверки backup, неизвестную версию/повторяющиеся ключи; завершить тесты reset из задачи 3 и запустить весь hub-memory.

### Task 5: Настольное приложение и расширение MCP

**Files:** `crates/hub-app/src/gui/memory.rs`, новый `gui/memory/lifecycle.rs`; `crates/hub-telegram/src/memory.rs` (общие Request/View/read и тексты); `crates/hub-memory/src/protocol.rs`; тесты `crates/hub-memory/tests/protocol/transport.rs` и unit-тесты GUI.

**Interfaces:** Расширить общий Request фильтром и чтением EntryDetails/MemoryOverview, View — preview административных операций/импорта. Команды администратора GUI используют API задач 3–4 в ограниченной фоновой задаче; глобальный memory.enabled остаётся независимым от проектного CleanupPolicy. MCP search получает необязательный `status`, default active; других инструментов не добавлять.

- [ ] Написать тесты: `mcp_default_search_excludes_archived`, `mcp_explicit_status_search`, `mcp_has_no_admin_tools`, `gui_project_switch_invalidates_preview`, `gui_auto_policy_requires_confirmation`, `gui_revision_navigation_uses_available_numbers`, `gui_cancel_import_preserves_file`.
- [ ] Запустить `cargo test -p hub-memory --test protocol` и соответствующие `cargo test -p hub-app memory`; подтвердить ожидаемые отказы новых тестов.
- [ ] Добавить фильтр, состояние, доступные номера ревизий, режим/статистику очистки, действия archive/stale/activate/delete/prune. Подтверждение удаления показывает ключи/число ревизий/байты, предлагает экспорт и предупреждает о работе без копии.
- [ ] Добавить export/save picker, import/open picker с выбором новых ключей для конфликтов, межпроектным подтверждением и apply только свежего preview. Добавить новое поколение с выбором сохраняемых ключей после успешного проверенного экспорта; предупредить о необходимости новой агентской сессии.
- [ ] Для auto показать точный текст из дизайна и отдельную кнопку «Включить автоочистку»; не связывать её с глобальным подключением MCP. Ошибки/отмена сохраняют действующую политику.
- [ ] Запустить GUI unit-тесты и `cargo test -p hub-memory`; проверить на тестовых данных маленькое окно, пустой проект, историю с пропусками и смену проекта при занятом worker.

### Task 6: Telegram без модели и безопасная передача файлов

**Files:** `crates/hub-telegram/src/memory.rs`, новые `memory/lifecycle.rs`, `memory/portable.rs`; `hub.rs`, `messenger.rs`, `telegram.rs`, `testing.rs`, `sender.rs` по необходимости для ограниченной загрузки; тесты `crates/hub-telegram/tests/memory/lifecycle.rs` и `portable.rs`.

**Interfaces:** `Browser` хранит ограниченный сценарий импорта по (user, TopicKey, cwd), отменяемый при смене контекста, /memory cancel и через 10 минут. Добавить обработку вложения перед обычной постановкой агентской задачи только для совпавшего ожидающего сценария. На уровне транспорта обеспечить bounded streaming download с пределом 16 МиБ+1; проверка metadata сама по себе недостаточна.

- [ ] Написать `memory_admin_never_launches_agent`, `auto_requires_bound_confirmation`, `stale_or_cross_thread_callback_is_rejected`, `ordinary_attachment_is_not_imported`, `pending_import_requires_initiator_and_project`, `oversized_unknown_length_download_is_bounded`, `export_needs_group_visibility_confirmation`, `failed_send_does_not_reset_store`.
- [ ] Запустить `cargo test -p hub-telegram --test memory`, подтвердить ожидаемые отказы.
- [ ] Добавить /memory status/filter/policy/prune/delete/export/import/cancel/reset и кнопки с привязкой к пользователю/чату/теме/cwd/token. Никаких путей из содержимого JSON и скачивания источников. Импорт после preview требует отдельного подтверждения целевого проекта и решений по конфликтам.
- [ ] Экспортировать во временный закрытый файл через write_new, отправлять существующим механизмом документа только после подтверждения. Удалять временный файл по завершении/ошибке; для reset успешная отправка и свежий VerifiedExport обязательны. Не утверждать, что Telegram-получатель сохранил файл на своё устройство.
- [ ] Ограничить pending state и фоновые операции, просроченные callbacks и импорты отклонять. Сохранить рабочие обычные вложения, /save и /restore.
- [ ] Запустить `cargo test -p hub-telegram`; проверить fake transport и отказы без живой отправки сообщений пользователю.

### Task 7: Документация, общая проверка и сборка

**Files:** `docs/guide/{memory.md,telegram.md,settings.md}`, `docs/roadmap/2026-11-15-company-brain-roadmap.md`, этот план; результаты в `docs/research/2026-11-15-memory-lifecycle-check.md`.

- [ ] Описать оба режима, команды/кнопки, ограничения, перенос, backup v1 и ручной rollback с остановленными агентами. Указать, что GUI-чтение не мигрирует, импорт не включает auto, архив не освобождает место.
- [ ] Запустить `cargo fmt --all -- --check`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo deny check`, `cargo build -p hub-app --bin agent-hub --locked`, `git diff --check`.
- [ ] Провести независимый финальный review по executing-plans/requesting-code-review; исправить подтверждённые ошибки и повторить затронутые проверки.
- [ ] Записать фактические результаты, не выдавая автоматические тесты за пользовательскую проверку GUI/Telegram; обновить roadmap точным статусом. Предоставить путь к бинарнику и короткую памятку проверки в отдельном тестовом проекте.

## Самопроверка плана

Все разделы дизайна покрыты задачами 1–7. Зависимость reset от VerifiedExport
явно завершена в задаче 4; до неё reset не экспортируется как работающий API.
Ручное подтверждение изменения данных выполняется интерфейсами, а блокировка,
token, валидация и атомарность — hub-memory. Предварительные просмотры не
делегируются модели. Пять рисков Review Focus включены в тесты своих задач.

Статус: реализация выполнена; замечания ревью исправлены, автоматические проверки пройдены. Фактическая
матрица проверок и отличия от плана — в [отчёте](../research/2026-11-15-memory-lifecycle-check.md).
Чекбоксы исходного плана сохранены как исходные требования, а не доказательство
того, что каждое перечисленное имя теста было использовано буквально.
