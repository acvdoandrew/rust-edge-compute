# Rust Edge Compute Node 🦀 ⚡

<div>
  <video src="https://github.com/user-attachments/assets/605a6683-45a5-4121-ba8e-5dd271bf3ce1" width="100%"></video>  
</div>

> A fault-tolerant, distributed compute agent built with Rust, Tokio, and gRPC.
> Simulates a decentralized GPU cluster where worker nodes report telemetry to a central control plane.


## 🏗 Architecture

This system uses an asynchronous **Actor Model** architecture to decouple data generation, rendering, and networking.

*   **Telemetry Actor:** Streams hardware stats (Temperature, VRAM, Usage) from a pluggable source (`sim`, `nvml`, or `auto`).
*   **TUI Actor (Main Thread):** Renders a 60 FPS terminal dashboard using `ratatui`.
*   **Network Client:** A background gRPC worker that streams Heartbeats to the Orchestrator.
*   **State Management:** Uses `Arc<Mutex<State>>` to share live telemetry between the rendering loop and the network client without blocking.

## 🚀 Features

*   **Distributed Orchestration:** A central Server (`bin/server`) handling concurrent connections from multiple Nodes.
*   **High-Performance TUI:** Real-time client and orchestrator dashboards powered by `ratatui` v0.29.
*   **Pluggable Telemetry HAL:** Runtime-selectable telemetry backend (`--telemetry-backend sim|nvml|auto`) with deterministic `auto` fallback.
*   **Fault Tolerance:** Client reconnects with jittered exponential backoff (capped) and resets the backoff after a successful reconnect.
*   **Richer Heartbeats:** Nodes send temperature, usage, VRAM bytes, uptime, and client version on every heartbeat.
*   **Node Lifecycle Hygiene:** Orchestrator marks stale nodes, evicts long-stale entries via TTL, and tracks total evictions.
*   **Dynamic Identity:** Nodes generate unique IDs at runtime (`Node-8821`) to simulate a heterogeneous cluster.

## 🛠 Tech Stack

*   **Runtime:** `tokio` (Async I/O, Green Threads)
*   **Networking:** `tonic` (gRPC), `prost` (Protobuf)
*   **Interface:** `ratatui`, `crossterm` (Raw Mode TUI)
*   **State:** `dashmap` (Server-side concurrent HashMap), `std::sync::Mutex` (Client-side)
*   **Build:** Hermetic Protobuf compilation via `protoc-bin-vendored`.

## 📦 How to Run the Demo

**Prerequisite:** Cargo (Rust Toolchain)

### 1. Start the Orchestrator (Server)
Open a terminal and launch the control plane. It will listen on `[::1]:50051`.

```bash
cargo run --bin server
```

### 2. Launch Worker Nodes (Client)
Open a **new terminal tab** (or multiple) to spin up worker nodes.
```bash
# Standard run (Random ID, connects to localhost)
cargo run --bin rust-edge-compute

# Custom configuration (Optional)
cargo run --bin rust-edge-compute -- --id "Worker-01" --server "http://127.0.0.1:50051" --telemetry-backend auto

# Force simulated telemetry
cargo run --bin rust-edge-compute -- --telemetry-backend sim

# Force NVML telemetry (fails fast if NVML/GPU is unavailable)
cargo run --bin rust-edge-compute -- --telemetry-backend nvml
```
* **Observe**: The Client TUI will launch, displaying live stats.
* **Verify**: Check the Server TUI table. Connected node rows, heartbeat counters, usage, VRAM, uptime, and version should update in real time.

### 3. Scripted Smoke Demo
Run the helper script for either a live visual demo or an automated smoke run.

```bash
./scripts/smoke_live.sh tmux
./scripts/smoke_live.sh headless
./scripts/smoke_live.sh manual
```

Notes:
- `tmux` mode opens a 3-pane live session (server + 2 clients).
- `headless` mode runs a non-visual smoke test and validates startup/shutdown logs.
- `manual` mode prints explicit multi-terminal commands.

## ✅ Quality Gates

This repository includes a GitHub Actions workflow that runs on pushes and PRs to `master`:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test --all-targets --all-features`
- `./scripts/smoke_live.sh headless`

## 🗺 Roadmap

### Phase 1: Foundation (✅ Completed)
- [x] Scaffold Async Runtime & Project Structure
- [x] Define gRPC Schema (`node.proto`) with Hermetic Build
- [x] Implement Telemetry Actor & Channel-based Communication

### Phase 2: Interface & Networking (✅ Completed)
- [x] Build TUI Dashboard with `ratatui` (Gauges, Logs)
- [x] Implement gRPC Heartbeat Client
- [x] Thread-safe State Synchronization (`Arc<Mutex>`)
- [x] Dynamic Node ID Generation

### Phase 3: Polish & Systems Engineering (✅ Completed)
- [x] **CLI Configuration:** Add `clap` to parse arguments (`--server <IP>`, `--id <NAME>`).
- [x] **Orchestrator Dashboard:** Upgrade Server from stdout logs to a real-time TUI table of connected nodes.
- [x] **Graceful Shutdown:** Handle `Ctrl+C` signals to disconnect cleanly from the mesh.
- [x] **Hardware HAL:** Implement `nvml-wrapper` telemetry source with runtime backend selection and `auto` fallback.
- [x] **Reconnect Backoff:** Replace fixed retry delay with exponential backoff + jitter + reset-on-success.
- [x] **Protocol Expansion:** Extend heartbeat payload to include usage, VRAM, uptime, and client version.
- [x] **Stale Eviction:** Add TTL-based stale node pruning with eviction counters on the orchestrator dashboard.
- [x] **CI Pipeline:** Add formatting, lint, tests, and headless smoke checks in GitHub Actions.
