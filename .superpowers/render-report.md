# Offscreen render harness — agent-hub-rs GUI screenshots

## Status

Done. Two faithful 960×640 light-theme PNGs produced from the REAL GUI code
(`hub_app::gui::app::HubApp`, branch `feat/gui-refresh`) with fictional demo data,
no display, no real user data. No crate under `crates/` and no workspace
`Cargo.toml`/`Cargo.lock` was modified (the root manifest diffs visible in
`git status` are the pre-existing 1.1.0→1.1.1 branch bump, present before this work).

## Output files

- `/home/aprazdnikov/Projects/aprazdnikov/agent-hub-rs/site/screenshots/inbox.png` (960×640, RGB)
- `/home/aprazdnikov/Projects/aprazdnikov/agent-hub-rs/site/screenshots/task-chat.png` (960×640, RGB)

Harness (throwaway, detached from the workspace via an empty `[workspace]` table):
`/home/aprazdnikov/Projects/aprazdnikov/agent-hub-rs/.superpowers/render/`
(`Cargo.toml`, `src/main.rs`, `src/demo.rs`, `src/raster.rs`, own `Cargo.lock`).
Built with `CARGO_TARGET_DIR=<repo>/target` to reuse workspace artifacts.

## Approach

- `main.rs` constructs `HubApp` through the public `gui::app::AppSetup` (tokio
  `mpsc`/`watch` channels, `LogBuffer::default()`, `tray: None`, temp `home`/`logs_dir`,
  `UpdateState::Idle`, `Exit::Staying`). `gui::tasks`/`gui::theme` are private modules,
  so the full `HubApp` window is rendered (tabs + sidebar + content) rather than
  individual views; theme via the public `gui::configure_theme`.
- Each frame: `ctx.run_ui(RawInput { screen_rect: 960×640, system_theme: Some(Light),
  events }, |ui| app.ui(ui, &mut frame))` with `eframe::Frame::_new_kittest()` (the
  public test constructor; `HubApp::ui` ignores the frame). `pixels_per_point = 1.0`,
  `animation_time = 0.0` for determinism. `output.textures_delta.clear()` (the atlas is
  read straight from `FontsView`; no backend textures exist).
- The harness stands in for the core: it drains the `Command` channel between frames
  and answers `Command::LocalTasks`/`Command::Tasks` reads with a demo
  `hub_telegram::hub::tasks::Snapshot` (task book + default `ProjectRules`).
- Interaction is driven by real egui pointer events, locating widgets by their
  `Shape::Text` galley (the same technique the app's own GUI tests use): expand the
  collapsed «Витрина» sidebar branch (toggle left of the project name), click the
  «В очереди» option chip (inbox); then «Задачи» tab → «Витрина» → «Поиск по каталогу»
  (chat). The sidebar catalog resolves on a background thread; the harness warm-ups
  until «Atlas» appears.

## Rasterization (`raster.rs`, pure CPU core)

Color model verified against `egui_glow`/`epaint` 0.36.2 sources in `~/.cargo/registry`:
- `epaint::mesh::Vertex.color` is sRGBA with **premultiplied** alpha.
- Light mode uses `FontColorTransferFunction::Off`, so the font atlas
  (`ctx.fonts(|f| f.image())`) keeps the rasterizer's premultiplied white glyph coverage
  (rgb == alpha == coverage).
- The glow fragment shader multiplies vertex × texel **in gamma space** (no
  linearization); blend state is `ONE, ONE_MINUS_SRC_ALPHA` (premultiplied src-over).

The rasterizer: `ctx.tessellate(output.shapes, 1.0)` → `Vec<ClippedPrimitive>`; for each
`Primitive::Mesh`, each triangle is barycentrically rasterized at pixel centers with a
top-left fill rule (exact-zero edge test assigns shared-edge pixels to one triangle, so
feathered seams are not double-blended), both windings normalized. UV/color interpolated
affinely; atlas sampled bilinearly with clamp-to-edge (matches `GL_LINEAR`); fragment =
vertex × texel in gamma space; blended premultiplied src-over into an f32 framebuffer
initialized to the light `extreme_bg_color`. Clip rects respected per primitive. Output
unpremultiplied to RGB8 and encoded with the `image` crate. Only `TextureId::Managed(0)`
(font atlas) is supported; any other texture or `Primitive::Callback` is a hard error
(the demo GUI emits none).

## Demo data (`demo.rs`, fictional)

Projects «Atlas» (Claude Code) and «Витрина» (Codex), real temp dirs under
`.superpowers/render/demo-root/{Atlas,Vitrina}` so `workspace_root` canonicalization
succeeds; names seeded via `Snapshot.saved` (`Draft.workspace_root` + `ProjectDraft`s).
- `task_index`: 4 `TaskRow`s — Atlas #1 «Отчёт по очереди», #2 «Инструкция для команды»;
  Витрина #3 «Поиск по каталогу» (`Attention::Review`, summary, files 2, checks 2/2),
  #4 «Перевод формы заказа».
- `local_jobs`: active Atlas `Live` with `Interaction Kind::Questions` — question
  «Какие задачи включить в отчёт?», header «Отчёт», options «Все»/«В очереди»
  (`Selection::Single`).
- Task books: Витрина #3 has 2 completed `Run`s (user step + agent result, timestamps,
  last run with 2 `ResultFile`s + 2 passed `CheckResult`s) → chat bubbles + footer chips.
- `Snapshot.bot = Stopped`, `local_only = true`, `saved = Some(draft)` (so no
  «Настроить приложение» prompt).

## Fidelity verification (visual, by Reading each PNG)

- inbox.png: tabs «Входящие (2)»/«Задачи»/«Темы Telegram»/«Настройки»; sidebar Atlas +
  Витрина expanded with status dots (question dot, review ring, idle dots) and attention
  badges «1»; group «Ждут вашего ответа · 1» question card (? icon plate, «Вопрос» chip,
  «Отчёт» header, question text, «Все»/«В очереди» chips with «В очереди» selected,
  answer field, «Отправить ответ»/«Отказаться отвечать»); group «Результаты на проверку · 1»
  review card (√ plate, «Проверить», «Проверки · 2/2» + «Файлов · 2» chips, 2-line summary).
- task-chat.png: «Задачи» tab; sidebar Витрина selected, «Поиск по каталогу» selected;
  header «Витрина / #3», title + «Нужна проверка» chip, «Принять»/«Открыть результат»
  (primary)/«…» menu, subtitle «Codex · локально · Нужна проверка»; chat bubbles — agent
  «codex» left white card with «Запуск завершён» chip + right-flush time, user «Вы» right
  selected-bg bubble; footer chips «Файлов · 2»/«Проверки · 2/2»/«Результат ›»; composer
  «Сообщение агенту» + disabled «Отправить» + «Ctrl+Enter для отправки».
- Text crisp, no tofu squares (GUI enforces covered glyphs), no stretched icon plates,
  chips/bubbles/corner radii correct, light palette matches `gui::theme`.
- `cargo clippy` clean under the harness lint table; PNG dimensions confirmed 960×640 RGB.

## Limitations

- The chat `ScrollArea` is `stick_to_bottom(true)` in the real app; with the new taller
  bubbles, four full turns do not fit 640px, so the earliest user turn scrolls off and
  three full turns are visible (clean, no partial clip). This is faithful app behavior.
- Agent bubble headers show the raw backend string «codex» (what `result.rs` renders);
  the titled «Codex» appears in the subtitle via `look::agent_title`.
- CPU rasterizer reproduces glow's gamma-space premultiplied blending but not its optional
  dithering; at 8-bit output the difference is negligible. No GPU/sRGB-framebuffer path.
- Times render in the system timezone (UTC here), so HH:MM differs from the +03:00 source
  timestamps; dates are fictional anyway.
- Verification is visual inspection of the PNGs plus clippy/compile, not a pixel diff
  against a live window (no display available).

## Rust rules routing (skill-issue-rust:rust-rules)

Read every fired reference: `main.md`, `architect.md`, `developer.md`, `types.md`,
`defensive.md`, `errors.md`, `clippy.md` (no SQLx, no new test files). Applied: bin-only
crate with `anyhow::Result` main; pure rasterizer core separated from the I/O shell;
`.get()`/slice patterns instead of indexing; explicit destructuring of `Draft`/`Snapshot`
defaults (no lazy `..Default::default()`); no `unwrap`/`expect`/`panic`; parameter counts
kept ≤7 by inlining `TaskRow` literals and splitting `demo::build`. Lint table narrowed
with recorded reasons in `Cargo.toml`: the four `cast_*` lints (bounded f32↔u8↔usize pixel
math), `float_cmp` (exact-zero top-left fill-rule tie-break is the algorithm),
`redundant_closure`/`redundant_closure_for_method_calls` (the suggested method-pointer
replacement fails `ctx.fonts`'s HRTB bound), and `must_use_candidate`/`missing_errors_doc`/
`module_name_repetitions` (bin-only harness, no downstream contract). `cargo clippy` passes
with zero warnings under this table.

## Gate records

Gate: security-review
Applicability: applicable (code work: a Rust harness that reads/writes files and drives GUI code).
Scope: `.superpowers/render/{Cargo.toml,src/main.rs,src/demo.rs,src/raster.rs}`; the two PNG outputs; read-only reference of `crates/hub-app/src/gui/**` and egui/epaint/egui_glow 0.36.2 registry sources.
Status: completed
Findings: none unresolved. No secrets/credentials touched (autostart `Err`, tray `None`, temp dirs, fictional data). No network, no shell/command injection, no deserialization of untrusted input (demo data is compile-time literals; `Task`/`Run` constructed directly, not parsed from external JSON). File writes limited to the two intended PNGs and throwaway dirs under `.superpowers/render`. Path inputs are harness-controlled constants, not user input → no traversal. Integer/float casts bounded by construction (framebuffer sized to the screen; atlas indices clamped). Residual risk: none material for a local throwaway renderer.
Fixes: none required.
Limitations: no live-window pixel comparison (no display); rasterizer color math verified by source inspection of egui_glow shaders/blend state and epaint atlas/vertex docs, plus visual PNG inspection — not by execution against a GPU reference.
Evidence: read `egui_glow-0.36.2/src/shader/{fragment,vertex}.glsl` and `painter.rs` blend func; `epaint` `mesh.rs` (premultiplied vertex), `image.rs` (`FontColorTransferFunction::Off`/`to_atlas_color`), `texture_atlas.rs`, `text/fonts.rs`; visual Read of both PNGs; `cargo clippy` clean. Inspection + execution (harness ran, PNGs written).

Gate: reliability-review
Applicability: applicable (code work with concurrency: background catalog thread, command channel, frame loop).
Scope: same harness files and the frame/command loop; read-only reference of `gui/tasks/{sidebar,context,attention,local,result,workspace}.rs`.
Status: completed
Findings: none unresolved. Blocking operations bounded: catalog warm-up capped at 60 frames × 25ms then `bail!`; command drain is non-blocking `try_recv`; no retry loops, no unbounded queues (mpsc capacity 64, demo never fills it). No persistent state written beyond the two PNGs (idempotent overwrite each run). No shared mutable state across threads except egui's own `Context` (thread-safe) and the catalog's std `mpsc` (handled by the app). Failures observable: missing label / unexpanded branch / unresolved catalog / unsupported texture all return `Err` with context (fail-fast, no silent degradation). The `reply.send` results are intentionally ignored (`let _`) because a dropped oneshot receiver only means the GUI cancelled a read — safe, commented.
Fixes: none required.
Limitations: single-process deterministic render; no multi-instance or redelivery scenario exists to test. Concurrency correctness of the app's own catalog thread is taken as given (read-only reference, not modified).
Evidence: read sidebar `Catalog::update` (std thread + mpsc), `context.rs` scope resolution, `workspace.rs` `scroll_body`/`stick_to_bottom`; harness ran to completion writing both PNGs; `cargo clippy` clean. Inspection + execution.
