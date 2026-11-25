# Inbox card_header layout fix — report

Repo `/home/aprazdnikov/Projects/aprazdnikov/agent-hub-rs`, branch `feat/gui-refresh`, base `8a889c6`, fix commit `20cfc65`.

## Root cause

`crates/hub-app/src/gui/tasks/attention.rs::icon_plate` built the plate as
`egui::Frame::new().fill(bg).corner_radius(8).show(ui, |ui| { ui.set_min_size(vec2(26,26)); ui.centered_and_justified(|ui| label); })`.
Verified against egui 0.36.2 source (`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/egui-0.36.2/src/containers/frame.rs`):

- `Frame::begin` gives the content `Ui` a `max_rect` equal to the parent's full `available_rect_before_wrap()` — in a `ui.horizontal` row that is the whole remaining card width;
- `Layout::centered_and_justified` stretches its single child to that width, inflating `content_ui.min_rect()`;
- `Frame::paint` fills exactly that inflated `min_rect`, and `allocate_space` advances the horizontal cursor past it, leaving the title a ~1-character column that wraps vertically.

`set_min_size` only sets a floor, so it constrained the width to ≥26 but not ≤26.

RED run confirmed the mechanism numerically: plate fill rect `[[15.0 99.0] - [985.0 131.0]]` — 970×32 px at a 1000 px window (32 px tall because the two-row details block sets the row height the justified rect fills).

## Fix

`icon_plate` now allocates a fixed box inside the frame:

```rust
egui::Frame::new().fill(bg).corner_radius(8).show(ui, |ui| {
    // A fixed box, not just a minimum: `centered_and_justified` inside a
    // horizontal row otherwise stretches the frame across the whole card.
    ui.allocate_ui_with_layout(
        egui::vec2(26.0, 26.0),
        egui::Layout::centered_and_justified(egui::Direction::TopDown),
        |ui| {
            ui.add(
                egui::Label::new(egui::RichText::new(glyph).strong().color(fg))
                    .selectable(false),
            );
        },
    );
});
```

Note: in egui 0.36 `Layout::centered_and_justified` takes a `Direction` (`Direction::TopDown`, matching what `Ui::centered_and_justified` used internally); the brief's zero-arg form does not compile. Frame fill, `corner_radius(8)`, glyph color/`strong`, `selectable(false)` unchanged. Blast radius: `icon_plate` is a private helper with exactly three call sites, all inside `card_header` in the same module (Local scope per existing-code); signatures and observable contracts of `card_header`/`Item::show` unchanged.

## Regression test

Added to `crates/hub-app/src/gui/tasks/attention/tests.rs` (inline test module beside the code, per rust-rules/testing):

- `Screen::rects()` — filled (`Shape::Rect` with non-transparent fill, clip-intersected, positive) rects of one frame, using the same `ctx.run_ui` harness as `frame()`;
- `Screen::plate(glyph)` — asserts exactly one plate-height (20..40 px) filled rect contains the glyph's center and returns it;
- `icon_plate_does_not_stretch_the_card_header` — Review task, `for width in [1000.0, 400.0]` (covers the wide `HeaderEnd::Action` branch with the right-to-left button and the narrow stacked-button branch): asserts `24.0 <= plate.width() <= 40.0` and `title.width() > 40.0` (title not a vertical 1-char column).

### RED (against `8a889c6`, fix not yet applied)

```
$ cargo test -p hub-app --locked -- gui::tasks::attention::tests::icon_plate
test gui::tasks::attention::tests::icon_plate_does_not_stretch_the_card_header ... FAILED
thread ... panicked at crates/hub-app/src/gui/tasks/attention/tests.rs:518:9:
icon plate stretched to [[15.0 99.0] - [985.0 131.0]] across the card_header at 1000px
test result: FAILED. 0 passed; 1 failed; ... 430 filtered out
```

### GREEN (after fix, probe lines removed)

```
$ cargo test -p hub-app --locked
test gui::tasks::attention::tests::icon_plate_does_not_stretch_the_card_header ... ok
test result: ok. 428 passed; 0 failed; 3 ignored; 0 measured   (lib)
+ 8 further integration test binaries: all "test result: ok", 0 failed

$ cargo clippy -p hub-app --all-targets --locked -- -D warnings
Finished `dev` profile ... (no warnings; exit 0)
```

Measured values after fix (temporary `eprintln` probe, removed before commit): at BOTH 1000 px and 400 px, plate `[[15.0 102.0] - [41.0 128.0]]` = exactly 26.0×26.0 (frame `inner_margin`/`outer_margin` are 0), title `[[50.0 102.0] - [167.0 115.0]]` = one line, 117×13 px. All pre-existing inbox tests (narrow stacking, commands, glyph √/!, collapse, keyboard/padding) pass.

## Files changed

- `/home/aprazdnikov/Projects/aprazdnikov/agent-hub-rs/crates/hub-app/src/gui/tasks/attention.rs` — `icon_plate` fixed-size allocation.
- `/home/aprazdnikov/Projects/aprazdnikov/agent-hub-rs/crates/hub-app/src/gui/tasks/attention/tests.rs` — `rects()`/`plate()` harness helpers + regression test.

Commit: `20cfc65 fix(gui): плашка иконки входящих фиксирована 26×26, не растягивает строку` (2 files, +85/−6). Working tree clean apart from untracked `.superpowers/`.

## Self-review

- `git show` re-read before commit: only the intended two files; wording, glyphs, colors, corner radius untouched; Russian UI text unchanged; no new dependencies; no GUI event loop launched (headless `Context::run_ui`).
- Grep `centered_and_justified|set_min_size` over `crates/hub-app/src` (excluding tests): the stretch-prone pattern existed only in `icon_plate`; no sibling call paths share the flaw.
- egui API facts verified against the locked 0.36.2 source (`allocate_ui_with_layout` hint→`next_space`→fixed `child_rect`; `Layout::centered_and_justified(Direction)`), not from memory. `theme.rs` was inspected only via grep for `card`/`chip`/`tone_colors` signatures (their exact fills are not used by the test predicate).
- rust-rules routing rows fired and read: main.md, testing.md, developer.md, defensive.md, errors.md. Diff complies: panics/`unwrap`/slicing only inside `#[cfg(test)]` (repo brief allows), functional chains, slice pattern `[rect] => *rect` instead of indexing, no new `pub`, no lints silenced. existing-code Local discipline followed (helper rewritten together with its 3 call sites, which were untouched).
- Unverified-by-execution items are marked in Concerns; everything else above is backed by the shown command output.

## Concerns

- Test couples to egui painting internals (`Frame` fill emitted as `Shape::Rect`) and to a height-window heuristic (20..40 px). Robust across the 26↔32 height change between broken/fixed states, but an egui upgrade that changes frame painting could make `plate()` fail loudly (candidate-count panic), not silently.
- The `HeaderEnd::Chip` branch (question/permission cards) shares the same `icon_plate` call site and is therefore fixed by construction, but it is not rect-measured by the new test (code-verified by inspection + existing render tests, not executed against the plate predicate) — accepted residual per the brief, whose required coverage was the wide+narrow `Action` branches.
- Lower bound `24.0` on plate width means a future shrink below 24 px would fail; intentional — the plate must keep its 26 px design size.
- No real-screenshot re-verification (headless constraint); the measured galley/frame rects above are the substitute evidence.

## Gate records

### Gate: security-review
- Applicability: applicable — code change plus test code; reviewed for adversarial causes.
- Scope: commit `20cfc65` (diff vs `8a889c6`): `crates/hub-app/src/gui/tasks/attention.rs` `icon_plate`; `crates/hub-app/src/gui/tasks/attention/tests.rs` harness helpers + new test.
- Status: completed (checklist `security-review/references/security.md` read and applied to the stated scope).
- Findings: none — no external input parsed (test strings are in-file literals; production change is layout-only), no injection/auth/secret/serialization surface, no unsafe, no arithmetic on untrusted values (float comparisons on constants), no dependency or Cargo.toml change (`--locked` throughout), no new resource-growth path (test renders one bounded frame; `rects()` bounded by shape count).
- Fixes: none required.
- Limitations: none material.
- Evidence: diff re-read (`git show --stat`/`git diff`), grep of changed files for input-handling APIs; execution: `cargo test -p hub-app --locked` and `cargo clippy ... -D warnings` (outputs above). Inspection-only items marked above.

### Gate: reliability-review
- Applicability: applicable — behavior-determining code change; reviewed for non-adversarial failure.
- Scope: same revision/diff as the security gate; production path = `icon_plate` → `card_header` → `show_attention` rendering only.
- Status: completed (checklist `reliability-review/references/reliability.md` read and applied).
- Findings: none — the change touches no state writes, no concurrency, no external calls/retries, no schema/API/events (private fn, unchanged signature), no unbounded structures; the test's assertions fail loudly with the measured rects in the message (observable, no hidden degradation). Bounded-operation/idempotency dimensions are not applicable to a pure-layout function.
- Fixes: none required; regression protection itself added (RED→GREEN evidence above).
- Limitations: Chip-branch plate rect not execution-measured (see Concerns, code-verified); egui-internal coupling of the test noted as future brittleness, not a production reliability risk.
- Evidence: executed commands `cargo test -p hub-app --locked` (428+passed/0 failed), `cargo clippy -p hub-app --all-targets --locked -- -D warnings` (clean), probe measurement of fixed rects; sibling-pattern grep (`centered_and_justified|set_min_size`) showing single call path.
