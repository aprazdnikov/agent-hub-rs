# Glyph-fix report — missing-glyph squares in the egui GUI

Branch: `feat/gui-refresh`. Crate touched: `hub-app` only.
Reviewed revision: working tree over `9dad1ff` (12 files changed; see *Files changed*).

## Root cause

egui renders any character with no glyph in the installed font as the replacement
box `◻` (U+25FB WHITE MEDIUM SQUARE). The bundled proportional family (Ubuntu-Light,
installed via eframe `default_fonts`) lacks several symbols that the GUI put into
labels. Confirmed empirically with `FontsView::has_glyph(&FontId::proportional(14.0), ch)`:

```
U+26A0 ⚠ has_glyph=false     U+2713 ✓ has_glyph=false     U+2192 → has_glyph=false
U+22EF ⋯ has_glyph=false     U+2190 ← has_glyph=false     U+25B6 ▶ has_glyph=false
```

The task named four (`⚠ ✓ → ⋯`). Grepping the whole `hub-app` GUI + autostart code and
probing coverage surfaced **two additional uncovered glyphs in rendered labels** the four
did not cover: `←` (U+2190, memory pagination "previous") and `▶` (U+25B6, status "Start"
button). `←` is the direct sibling of the in-scope `→` on the same row; `▶` sits next to
`⏹` (U+23F9 Stop) which is *covered*, so the Start/Stop pair was visibly inconsistent
(Start showed a square, Stop showed the stop glyph). These fall under the "grep to find
ALL, incl. any missed" instruction, so they were fixed too.

Other special characters that appear in GUI labels were probed and are **covered** (left
unchanged): `№` (U+2116, queue counter), `⌘` (U+2318, kbd hint), `⏹` (U+23F9, Stop),
`«»`, `·`, `–`, `—`, `…`, `›`, `√`, `⏵`, `‹`. Only `⚠️`(U+26A0+VS16) and `→` in
`hub-core`/`hub-telegram` also remain uncovered, but those are **Telegram HTML/Markdown
output, not egui labels** — a different render path (see scope notes below).

## Chosen replacements (all verified covered by the probe)

| Uncovered | Covered replacement | Why |
|---|---|---|
| `⚠` U+26A0 | `!`  | sanctioned mapping; matches the existing Error-tone `!` plate |
| `✓` U+2713 | `√` U+221A | sanctioned mapping; √ is a check-like glyph and covered |
| `→` U+2192 | `›` U+203A | sanctioned mapping; single right guillemet, covered |
| `⋯` U+22EF | `…` U+2026 | sanctioned mapping; horizontal ellipsis, covered |
| `←` U+2190 | `‹` U+2039 | **missed sibling**; single left guillemet, covered, symmetric with `›` |
| `▶` U+25B6 | `⏵` U+23F5 | **missed**; black-medium-right-triangle, covered, media-sibling of `⏹` |

Wording is unchanged everywhere — only the glyph codepoints swap.

## Every site changed

Production, rendered labels (11 code sites):
1. `gui/tasks/attention.rs:178` — Permission icon plate `"⚠"` → `"!"`
2. `gui/tasks/attention.rs:230` — Review icon plate `"✓"` → `"√"`
3. `gui/tasks/attention.rs:234` — Git/Queue icon plate `"⚠"` → `"!"`
4. `gui/tasks/workspace.rs:628` — `menu_button("⋯")` → `menu_button("…")`
5. `gui/tasks/workspace.rs:941` — workflow label `"{} → {} → ревью {}"` → `"{} › {} › ревью {}"`
6. `gui/tasks/result.rs:435` — `ui.link("Результат →")` → `ui.link("Результат ›")`
7. `gui/settings.rs:856` — Qwen help `` (`qwen` → /auth `` → `` (`qwen` › /auth ``
8. `gui/tasks/rules.rs:86` — Git help text three `→` → `›`
9. `gui/memory/lifecycle.rs:267` — pagination prev `"←"` → `"‹"`
10. `gui/memory/lifecycle.rs:269` — pagination next `"→"` → `"›"`
11. `gui/status.rs:48` — Start button `"▶ Запустить"` → `"⏵ Запустить"`

Production, OS-gated labels (`hub-app`, rendered by `gui/autostart.rs:93`
`ui.label(format!("Нужна проверка: {reason}"))`; compiled + clippy-checked under `test`,
but not executed on Linux):
12. `autostart/macos.rs:335` — approval help three `→` → `›`
13. `autostart/windows.rs:74,77` — menu-path `«Параметры → Приложения → Автозагрузка»` (two
    occurrences, four arrows) → `›`

Doc/inline comments that name a changed literal (updated for consistency, not rendered):
14. `gui/tasks/result.rs:28`  — `«⋯»` → `«…»`
15. `gui/tasks/workspace.rs:950` — `«⋯»` → `«…»`
16. `gui/tasks.rs:640` — `«⋯»` → `«…»`

Tests (assertions preserved, only expected glyph strings updated):
17. `gui/tasks/attention/tests.rs:450` — `contains("✓")` → `contains("√")`
18. `gui/tasks.rs:1067,1552` — `"Результат →"` → `"Результат ›"` (2 sites)
19. `gui/tasks.rs` — 18 × `click_text(...,"⋯",...)`/`all_text_rects(...,"⋯")` → `"…"`
20. `gui/theme.rs` — new regression test `gui_labels_use_only_covered_glyphs`

Intentionally NOT changed: `№ ⌘ ⏹ «» · – —` (covered); `×` (only in comments/test panic
messages, and covered anyway); three prose code comments containing `→` that describe
behaviour, not a label (`workspace.rs:541`, `result.rs:275`, `sidebar.rs:300`) — non-rendered;
`hub-telegram`/`hub-core` `⚠️`/`→` strings — Telegram/HTML output, not the egui font path.

## Regression test — RED/GREEN evidence

The new test `gui_labels_use_only_covered_glyphs` (in `gui/theme.rs`) is a static
font-coverage guard: it `configure`s a ctx, runs `ctx.run_ui` drawing the covered set, then
asserts `has_glyph(&FontId::proportional(14.0), ch)` is **true** for every glyph now used in
labels (`… › ‹ √ ⏵ ⏹ ! ? · № ⌘ « » – — • ○`) and **false** for the removed ones
(`⚠ ✓ → ← ⋯ ▶`), documenting why each was replaced. It is a characterization guard on the
font, so on its own it passes regardless of label content; the **behavioural** RED→GREEN for
the swap is demonstrated by the string-matching tests, which literally match on the swapped
galley text.

RED — production reverted to old glyphs, tests kept at new expectations
(`git checkout HEAD -- attention.rs workspace.rs result.rs`, then `cargo test -p hub-app
--locked --lib`):
```
test gui::tasks::attention::tests::narrow_inbox_stacks_action_below_content ... FAILED
test gui::tasks::tests::menu_archives_and_opens_parameters ... FAILED
test gui::tasks::tests::result_link_only_on_latest_reviewable_run ... FAILED
... (18 FAILED total)
thread '...narrow_inbox_stacks_action_below_content' panicked at attention/tests.rs:450:5:
  assertion failed: screen.contains("√")
thread '...menu_archives_and_opens_parameters' panicked at tasks.rs:858:32:
  missing text: …
thread '...result_link_only_on_latest_reviewable_run' panicked:  (Результат › not found)
```
This proves the tests exercise the glyphs: with old production they look for `…`/`›`/`√` and
do not find them (the menu galley is `⋯`, link is `Результат →`, plate is `✓`).

GREEN — full change applied (`git apply` the production patch; `cargo test -p hub-app --locked`):
```
test result: ok. 427 passed; 0 failed; 3 ignored;  ...  (lib)
test result: ok. 1 passed;   (main)
test result: ok. 13 passed;  (tests/actions.rs)
test result: ok. 1 passed;   (tests/autostart.rs)
test result: ok. 5 passed;   (tests/desktop_tasks.rs)
test result: ok. 7 passed;   (tests/git.rs)
test result: ok. 6 passed;   (tests/memory.rs)
test result: ok. 24 passed;  (tests/speech.rs)
test gui::theme::tests::gui_labels_use_only_covered_glyphs ... ok   (isolated run)
```

Clippy (exact command from the brief):
```
cargo clippy -p hub-app --all-targets --locked -- -D warnings
  Finished `dev` profile ...
  clippy_exit=0
```

Coverage probe (execution, `ctx.fonts_mut(|f| f.has_glyph(&FontId::proportional(14.0), ch))`,
temporary test, removed before commit):
```
U+26A0 ⚠ false  U+2713 ✓ false  U+2192 → false  U+22EF ⋯ false  U+2190 ← false  U+25B6 ▶ false
U+23F5 ⏵ true   U+2039 ‹ true  U+221A √ true   U+2026 … true  U+203A › true   U+0021 ! true
U+2116 № true  U+2318 ⌘ true   U+23F9 ⏹ true   U+00AB « true  U+00BB » true  U+2013 – true  U+2014 — true
U+00B7 · true   U+003F ? true   U+2022 • true   U+25CB ○ true   U+00D7 × true
```

## Self-review

- `grep` across all `hub-app` for the six uncovered codepoints shows the only remaining hits
  are: (a) the new test's comment + `removed[]` array (intentional), and (b) three prose code
  comments not naming a label. No rendered label string literal contains an uncovered glyph.
- Repo-wide, the surviving uncovered `→`/`⚠️` are confined to `hub-core`/`hub-telegram`
  message text (Telegram/HTML) and a handful of doc comments in other crates — none reach the
  egui font path; verified by tracing the two render paths, not by runtime execution on Linux.
- `№`, `⌘`, `⏹` (queue counter, kbd hint, Stop) are covered → left as-is; the test asserts
  them covered, so reintroducing an uncovered glyph there would be caught for the documented set.
- Wording, layout and every test's assertion structure preserved; only glyph codepoints changed.
- Interaction contracts hold: `click_text`/`all_text_rects` match galley text, so both the
  menu button source (`"…"`) and its test targets updated together.

## Concerns / residual items

1. **`has_glyph` probed at proportional 14 only.** Labels render at sizes 12/26/14 (proportional)
   and 13 (monospace). Glyph coverage is a property of the font *face*, not the pixel size, so a
   face covering `⏵` at 14 also covers it at 12/26 — but this is reasoning, not executed at every
   size. Monospace labels are not the affected set (all swaps are in proportional-family labels).
2. **macOS/Windows autostart labels not executed on Linux.** `windows.rs`/`macos.rs` compile and
   pass clippy under `test` cfg, but the `NeedsAttention(...)` text is not exercised on Linux;
   the macos test only checks `State::NeedsAttention(_)`, not the message glyphs. Visual fix is
   verified by inspection + coverage probe, not by rendering on those OSes.
3. **Regression test is a static characterization guard, not an all-labels scan.** It pins the
   documented set. A *new* uncovered glyph added to a different label would not be caught by this
   test (only by the per-site behavioural tests if they assert on it). A full runtime
   "every drawn galley char is covered" scan was judged too brittle/over-scoped for this fix
   (it would also flag dynamic agent-authored text) and is intentionally not done.
4. **Out-of-scope by decision, flagged for the owner:** `⌘`(workspace.rs:798, covered → fine);
   `№`(tasks.rs:681, covered → fine); `hub-core`/`hub-telegram` `⚠️`/`→` (Telegram path, not
   egui). Left untouched per existing-code "do not transplant across bounded contexts"; if the
   Telegram/chat rendering is also expected to be font-safe, that is a separate follow-up.

## Gate records

### Gate: security-review
- **Applicability:** applicable — invoked for code work (the brief's binding gate). Reviewed the actual diff.
- **Scope:** 12-file diff (10 `gui` + 2 `autostart`): glyph codepoint substitutions in label string
  literals, updated test expectations, one new inline `#[cfg(test)]` test in `theme.rs`.
  Revision: working tree over `9dad1ff`.
- **Status:** completed.
- **Findings:** none. The diff adds no injection sink (the swapped literals are display-only;
  `format!("Нужна проверка: {reason}")` structure unchanged), no secret/credential, no network/IPC,
  no auth/authorization change, no file/path handling change, no serialization, no new untrusted
  input. egui immediate-mode labels are not an HTML/JS sink (no XSS surface). No DoS/amplification:
  the new test draws ≤17 chars. Resource/supply-chain: no dependency added (per constraint, no fonts).
- **Fixes:** n/a (clean).
- **Limitations:** macOS/Windows string paths not executed here (cfg); judged non-sensitive by inspection.
- **Evidence:** read every changed line (`git diff`); confirmed no uncovered glyph in rendered literals
  via grep; executed `cargo test`/`cargo clippy` (exit 0). Inspection vs execution distinguished above.

### Gate: reliability-review
- **Applicability:** applicable — invoked for code work; the change determines user-visible system behaviour.
- **Scope:** same 12-file diff, assessed for the failure mode the fix targets (replacement-box rendering)
  plus data-integrity/contract/concurrency dimensions. Revision: working tree over `9dad1ff`.
- **Status:** completed.
- **Findings:** none unresolved. No shared-state, concurrency, persistence, retry, idempotency,
  queue, or external-call surface is touched — these are display literals and test-match strings.
  No schema/API/event contract crosses a boundary: the label strings are consumed only by egui
  rendering and by in-crate tests (updated in the same change). The core integrity risk (a
  replacement glyph that is itself uncovered) is eliminated — every replacement is `has_glyph`=true
  (executed probe), every removed glyph is `has_glyph`=false.
- **Fixes:** six glyph substitutions each verified by an executed `has_glyph` probe; regression test
  added; 21 test expectations updated and proven RED (18 FAILED, `missing text: …`, `assertion failed:
  contains("√")`) → GREEN (427 lib + all integration suites pass; clippy exit 0).
- **Limitations:** (a) coverage checked at proportional-14; other sizes rely on face-level coverage
  reasoning, not per-size execution. (b) macOS/Windows autostart labels not executed on Linux. (c) the
  named test is a static set guard, not a runtime scan of arbitrary labels — a new uncovered glyph in
  an unswept label is caught only if a behavioural test asserts on it. (d) hub-core/hub-telegram
  `⚠️`/`→` judged out-of-scope by tracing the render path (inspection), not by executing the GUI with
  that data.
- **Evidence:** executed — coverage probe output (has_glyph table above); RED run (revert + `cargo test`
  failing 18 tests); GREEN run (`cargo test -p hub-app --locked`, 427 + suites pass;
  `cargo clippy -p hub-app --all-targets --locked -- -D warnings`, exit 0); isolated
  `gui_labels_use_only_covered_glyphs` pass. Inspected — full `git diff`, grep sweeps,
  `gui/autostart.rs:92` render-path read, `clippy.toml` (`allow-*-in-tests`) + `[workspace.lints]` read.
