# AGENTS.md

Guidance for autonomous coding agents working in this repository.

## 1) Project Snapshot
- Language: Rust (edition 2021), built with Cargo.
- Runtime/network/UI: tokio + tonic/prost + ratatui/crossterm.
- Proto schema: `proto/node.proto` (compiled by `build.rs` with vendored `protoc`).
- Primary binaries:
  - `edge`: unified CLI router (preferred entrypoint)
  - `server`: orchestrator + dashboard + scheduler
  - `rust-edge-compute`: worker node
  - `jobctl`: job management CLI

## 2) Repository Map
- `src/bin/edge.rs`: command forwarding to server/node/job binaries.
- `src/bin/server.rs`: orchestrator services, scheduling, dashboard, and many tests.
- `src/main.rs`: worker node runtime, telemetry backend selection, worker-side TUI.
- `src/client/mod.rs`: heartbeat loop, lease/execute/report flow, reconnect logic.
- `src/client/backoff.rs`: exponential backoff with jitter.
- `src/telemetry/*.rs`: `simulated`, `nvml`, and `amd_sysfs` telemetry providers.
- `tests/node_lifecycle.rs`: integration test for heartbeat/disconnect lifecycle.
- `proto/node.proto`: gRPC contracts for node and job services.
- `build.rs`: protobuf generation wiring.
- `scripts/smoke_live.sh`: tmux/manual/headless smoke workflows.
- `.github/workflows/ci.yml`: canonical quality gates.

## 3) Build, Lint, and Test Commands
Run all commands from repository root.

### Build and run
- `cargo build`
- `cargo build --all-targets --all-features`
- `cargo build --bin edge`
- `cargo build --bin server`
- `cargo build --bin rust-edge-compute`
- `cargo build --bin jobctl`
- `cargo run --bin edge -- --help`
- `cargo run --bin edge -- server`
- `cargo run --bin edge -- node`
- `cargo run --bin edge -- job status job-000001`

### Format and lint
- `cargo fmt --all`
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`

### Tests
- `cargo test --all-targets --all-features`
- `cargo test --lib`
- `cargo test --tests`
- `cargo test --bin server`
- `cargo test --bin rust-edge-compute`

### Single-test cookbook (important)
- Integration test file: `cargo test --test node_lifecycle`
- Integration test function (exact):
  - `cargo test --test node_lifecycle node_client_lifecycle_emits_heartbeat_then_disconnect -- --exact --nocapture`
- Single server binary test (exact):
  - `cargo test --bin server tests::lease_job_prioritizes_high_then_normal_then_low -- --exact`
- Single worker binary test (exact):
  - `cargo test --bin rust-edge-compute tests::init_source_auto_prefers_nvml_when_available -- --exact`
- Single lib test (exact):
  - `cargo test --lib client::backoff::tests::backoff_reset_restores_initial_delay -- --exact`
- Name-filter run (substring): `cargo test retry_delay_scales_and_caps`

### Smoke checks
- CI smoke path: `./scripts/smoke_live.sh headless`
- Live demo path: `./scripts/smoke_live.sh tmux`
- Manual printed steps: `./scripts/smoke_live.sh manual`

## 4) CI Contract (Do Not Drift)
CI in `.github/workflows/ci.yml` runs:
- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`
- `./scripts/smoke_live.sh headless`

Keep these green locally before proposing behavior changes.

## 5) Code Style and Conventions
Follow existing style in `src/` and tests.

### Imports
- Order imports as: `std`, third-party crates, then local crate/module imports.
- Keep blank lines between import groups.
- Prefer grouped/nested imports when they improve clarity.
- Remove unused imports; do not silence warnings.

### Formatting
- Use default rustfmt behavior (no repo-specific rustfmt config).
- Keep expressions readable and let rustfmt handle wrapping.
- Avoid manual alignment that rustfmt will undo.

### Types and data modeling
- Prefer explicit structs/enums for domain state and protocol transitions.
- Use `Duration`, `Instant`, and saturating arithmetic for timing/retry logic.
- Keep enum conversion helpers (`as_str`, `from_proto`, `as_proto`) near enum types.
- Use thread-safe shared state patterns already present (`Arc`, `DashMap`, `Mutex`, atomics).

### Naming
- Types/traits: `PascalCase`.
- Functions/modules/variables: `snake_case`.
- Constants: `UPPER_SNAKE_CASE`.
- Tests: descriptive behavior names (what_happens_when_condition style).

### Error handling
- Use `anyhow::Result` for internal fallible paths and attach context with `.context(...)` / `.with_context(...)`.
- gRPC handlers should return `Result<Response<_>, tonic::Status>`.
- Validate request fields before mutating state.
- Map gRPC status codes precisely:
  - `invalid_argument` for malformed input
  - `not_found` for missing entities
  - `permission_denied` for lease ownership violations
  - `failed_precondition` for invalid state transitions
- Avoid panics for recoverable runtime errors.
- In tests, use `expect(...)` with concrete failure messages.

### Async and concurrency
- Use `tokio::select!` for shutdown-aware waiting.
- Respect `watch::Receiver<bool>` shutdown signals and exit loops cleanly.
- Keep lock scopes short and avoid holding locks across `.await` points.
- Reset reconnect backoff after successful reconnect.
- In polling logic, use bounded sleeps/timeouts to avoid hangs.

### gRPC and protobuf rules
- Edit API schema in `proto/node.proto`; do not hand-edit generated prost code.
- Preserve wire compatibility when evolving enums/messages/services.
- Keep lease/state-machine invariants intact in scheduler logic.
- Keep CLI defaults consistent with existing constants (`[::1]:50051`, `http://[::1]:50051`).

### CLI and UX conventions
- Define arguments/subcommands via `clap` derive macros.
- Keep compatibility for users invoking legacy binaries via `edge` forwarding.
- Preserve existing output expectations used by smoke tests.

### Testing conventions
- Add tests near changed logic (unit first, integration as needed).
- Cover happy-path and failure-path transitions for orchestration changes.
- Use `#[tokio::test]` for async behavior with explicit timeout boundaries.
- For integration tests, prefer ephemeral ports and graceful shutdown assertions.

## 6) Agent Workflow Checklist
Before finishing substantial changes:
- Run `cargo fmt --all`.
- Run `cargo clippy --all-targets --all-features -- -D warnings`.
- Run targeted tests first, then `cargo test --all-targets --all-features`.
- If startup/shutdown/orchestration changed, run `./scripts/smoke_live.sh headless`.
- Update `README.md` when user-visible commands, flags, or behavior change.

## 7) Cursor/Copilot Rules Audit
Checked for additional instruction files:
- `.cursorrules`: not present
- `.cursor/rules/`: not present
- `.github/copilot-instructions.md`: not present

No extra Cursor/Copilot-specific rules are currently defined in this repository.
