# Settings UI Implementation Plan

**Status:** Implemented locally; build and automated checks passed. The user reviewed
the local UI and accepted the result. Included with this plan in the local UI and
documentation commit; not yet released.

[Documentation](../README.md) · [User guide](../guide/settings.md) · [History](../history.md#обновление-интерфейса)

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace the long settings form with the approved section navigation and modern, system-aware light/dark styling.

**Architecture:** Keep one existing SettingsForm draft and validation/save pipeline. Add presentation-only section state, route field errors to the relevant page, and configure both egui styles once with ThemePreference::System.

**Tech Stack:** Rust, eframe/egui 0.36.2; Linux declares the already locked zbus 5.19 dependency for the desktop portal.

**Spec:** Approved in chat on 2026-11-05: sidebar with Telegram/access, workspace, agents, projects, actions, speech, application; one agent editor at a time; fixed save/cancel; draft preserved across navigation; errors marked by section. Additional request: light and dark themes follow the OS. Reference: settings-redesign-v1.html in chat scratch.

## Global Constraints

- Preserve configuration schema, secrets, validation, restart confirmation and immediate OS autostart behavior.
- Use the existing Russian labels, platform behavior and native Rust UI.
- Default to Telegram for first setup and when applying IDs from the log.
- Configure System theme once; do not override the selected style each frame.
- Compact section selector on narrow windows; scroll content independently of save controls.

## Review Focus

- Hidden invalid fields must be reachable and prevent saving.
- Navigation and theme changes must not discard unsaved values.
- Save/cancel must fit at 640×360 as well as 960×640.
- Every agent's existing settings and safety notices must remain accessible.
- OS autostart must work independently of an invalid Telegram draft.

### Task 1: Section navigation

Files: `crates/hub-app/src/gui/settings.rs`, new `settings/navigation.rs`.

- [x] Add failing rendered-UI test: initial Telegram page omits unrelated field editors; navigate away/back without losing edits.
- [x] Run focused GUI test and confirm failure on existing long-form layout.
- [x] Introduce section/agent selection, sidebar/compact navigation, per-page scroll and footer error jump. Move updates to Application and timeouts to Agents / General. Preserve field editors and save semantics.
- [x] Adapt existing render tests to visit their page; add tests for hidden errors, agent switching, cancel and log actions.
- [x] Run `cargo test -p hub-app gui::`.

### Task 2: System themes and layout

Files: new `gui/theme.rs`, `gui/mod.rs`, `gui/app.rs`, `gui/status.rs`, `gui/topics.rs`, `gui/log.rs`, `src/main.rs`.

- [x] Configure slate/blue light and dark palettes, text scale, spacing, focus and selection; retain bundled Cyrillic fonts.
- [x] Add theme integration test with light→dark→light RawInput.system_theme changes; assert styles change while System preference remains active.
- [x] Update app navigation, page headings and status presentation using shared theme tokens. Add a system-theme explanation in Application.
- [x] Run GUI tests and inspect rendered UI in both themes and at compact size where tools permit.

### Task 3: Verify and document

- [x] Update the relevant README settings/window guidance.
- [x] Run formatting, `cargo test --workspace` and `cargo clippy --workspace --all-targets`.
- [x] Review the final diff for lost settings, secret exposure, draft lifecycle, clipping and unwanted behavior changes.

## Execution notes

The user has approved implementation. Continue directly in the shared workspace; leave changes uncommitted for review. No further design approval is needed.


## Review and evidence

- Initial long-form isolation test failed before the navigation change, then passed.
- Reviewer found shared TextEdit undo history between agent pages. The keyboard regression test reproduced Claude's value appearing in Codex after Ctrl+Z, then passed with page-specific widget IDs.
- Reviewer found winit does not report Linux system theme. Added bounded asynchronous XDG portal startup/read, SettingChanged subscription, periodic reconciliation and retry; Windows/macOS keep native events.
- Linux read-only live portal probe confirmed subscription remains active and current dark preference is applied. It also caught a D-Bus array signature mismatch, corrected to a string slice. Temporary probe and rendering harness were removed.
- Both palettes and 640×360 layout inspected by rasterizing the actual egui meshes/font atlas; no production bot/config was started.
- Final checks: cargo fmt --all --check passed; cargo test --workspace --locked: 1717 passed, 13 existing environment/live-agent tests ignored, 0 failures; cargo clippy --workspace --all-targets --locked -- -D warnings passed.
