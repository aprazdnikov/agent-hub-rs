# Hermes backend: implementation plan

**Goal:** add Hermes as a fourth per-topic agent without introducing another Telegram gateway.

**Architecture:** `hub-hermes` owns Hermes ACP protocol and subprocess lifecycle. It depends only on `hub-core` and `hub-agent`, using the existing JSON-RPC client, `Conversation`, `UserChannel`, cancellation and authenticated loopback MCP server. Telegram and the desktop application select it through `BackendKind::Hermes`.

**Stack:** Rust workspace, Tokio, serde_json, Hermes ACP over newline-delimited JSON-RPC stdio.

**Constraints:** keep Claude/Codex/Qwen behavior and old settings/topics compatible; no changes to Hermes installation, credentials or profiles; no model calls without separate budget approval. No release tag, push or automatic credential setup. Branch `feat/hermes-backend` is based on `61d06a0`, including macOS packaging/settings fixes.

**Non-goals:** a generic ACP framework, Qwen-specific drain protocol, background-task parity, live steering, provider/key management in agent-hub, installation of Hermes.

## 1. Establish protocol contract

Read the installed `acp_adapter/{server,session,events,approval,entry}.py` and official ACP documentation. Run `hermes acp --version` and `hermes acp --check` (no inference). Establish initialize capabilities, MCP HTTP descriptors, session/new/load, model selection, permission option kinds, updates, prompt result usage and cancellation. Use `session/load` for restoration; never silently recreate a missing session. Distinguish context occupancy from billable/model tokens. Messages received during a turn remain in the hub inbox for the next sequential turn.

Verification: dependency check succeeds, version output parses, and deterministic fixture messages reflect observed schemas. No user configuration is edited.

## 2. Extend the domain and settings

Modify `crates/hub-core/src/{domain,settings,commands,render,topics}.rs` as necessary and `crates/hub-telegram/src/texts.rs`.

- `BackendKind::Hermes`, wire name `hermes`, included in `ALL`.
- `Usage::Hermes { tokens: Option<u64> }`, no invented usage when unavailable.
- `HermesSettings { cli: Option<PathBuf>, profile: Option<String>, model: Option<String> }`.
- `Draft.hermes: HermesDraft { cli: String, profile: String, model: String }` and `[hermes]` in `SettingsFile`.
- Field errors use `Field::Hermes(HermesField::{Cli,Profile,Model})`.
- Old files default to an unconfigured Hermes section; topic sessions serialize backend `hermes`.

RED: tests using raw `hermes` backend/settings input fail because the backend is not recognized. GREEN: parse, command switching, settings round-trip, validation, finished text and topic serialization pass.

Commands: `cargo test -p hub-core --locked`; `cargo clippy -p hub-core --all-targets --locked -- -D warnings`.

## 3. Implement the Hermes boundary

Create `crates/hub-hermes/Cargo.toml` and source modules for CLI version checks, protocol translation, requests, turns and backend; expose `backend::HermesBackend` and `version::{locate,check,CliError}` consistently with existing backends. Add workspace membership/dependencies in `Cargo.toml`.

`HermesBackend::run(&TopicSession, Prompt, mpsc::Receiver<Prompt>, Conversation) -> mpsc::Receiver<Prompt>` must preserve the unread inbox on failures and cancellation. Spawn `hermes [--profile NAME] acp` without a shell, set cwd to the topic, use piped streams and bounded initialization/shutdown. Do not enable YOLO or automatically accept hooks. Apply a configured model only through the verified ACP model method. Fail explicitly for unsupported required capabilities and missing saved sessions. Validate permission options and only select an offered one-time allowance; rejection/cancellation never grants permission.

Add `McpHttpServer::start_with_questions` in `crates/hub-agent/src/mcp_http.rs`: opt-in `ask_user` using existing schemas/parser and `UserChannel::ask`; existing `start` remains send_file-only. Preserve bearer-token/Host/Origin guards. Bound question count/options and return tool errors for malformed/refused input.

RED: opt-in MCP test fails because ask_user is unknown. GREEN: loopback tests prove question delivery, answers/refusal, malformed/over-limit rejection, opt-in listing and unauthenticated rejection. Backend deterministic fixtures cover new/load, replay suppression, text/tools, usage, queued prompts, /stop including initialization, EOF, offered permission options and unknown requests. No paid live prompt.

Commands: `cargo test -p hub-agent --locked`; `cargo test -p hub-hermes --locked`; `cargo clippy -p hub-hermes --all-targets --locked -- -D warnings`.

## 4. Wire the application and Telegram

Modify `crates/hub-telegram/{Cargo.toml,src/agents.rs}` and `crates/hub-app/{Cargo.toml,src/connector.rs,src/config.rs,src/supervisor.rs,src/gui/settings.rs,src/gui/look.rs}`. Selection routes only Hermes topics to `HermesBackend`. Required default CLI errors block bot startup; optional agent errors become status rows. ACP dependency readiness does not prove provider login: status explicitly says sign-in is unverified. Settings expose CLI/profile/model only. Existing fixed settings action panel stays visible.

RED: headless egui test fails with `Hermes section missing`. GREEN: section/profile labels render; existing save/cancel visibility tests still pass. Add configured-missing-CLI routing/inbox preservation, required/optional readiness, status label and `FileSettings` round-trip tests.

Commands: `cargo test -p hub-app hermes --locked`; `cargo test -p hub-telegram hermes --locked`.

## 5. Documentation and gates

Update `README.md` with setup, commands, settings, architecture and honest limitations. Record verification scope: deterministic tests and ACP check only; end-to-end inference/Telegram delivery remains unverified until a separately approved live run. No Windows/Linux execution claim from a macOS run.

Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `cargo build --locked -p hub-app`, and `git diff --check`. Inspect the final diff for credentials, arbitrary shell execution, automatic approval, token leakage and accidental changes to existing backends. Keep commits/push/release separate unless explicitly authorized.
