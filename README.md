# Rust Edge Compute Node 🦀 ⚡

<div>
  <video src="https://github.com/user-attachments/assets/605a6683-45a5-4121-ba8e-5dd271bf3ce1" width="100%"></video>  
</div>

> A fault-tolerant, distributed compute agent built with Rust, Tokio, and gRPC.
> Simulates a decentralized GPU cluster where worker nodes report telemetry to a central control plane.


## 🏗 Architecture

This system uses an asynchronous **Actor Model** architecture to decouple data generation, rendering, and networking.

*   **Telemetry Actor:** Streams hardware stats (Temperature, VRAM, Usage) from a pluggable source (`sim`, `nvml`, `amd-sysfs`, or `auto`).
*   **TUI Actor (Main Thread):** Renders a 60 FPS terminal dashboard using `ratatui`.
*   **Network Client:** A background gRPC worker that streams Heartbeats to the Orchestrator.
*   **State Management:** Uses `Arc<Mutex<State>>` to share live telemetry between the rendering loop and the network client without blocking.

## 🚀 Features

*   **Distributed Orchestration:** A central Server (`bin/server`) handling concurrent connections from multiple Nodes.
*   **High-Performance TUI:** Real-time client and orchestrator dashboards powered by `ratatui` v0.29.
*   **Pluggable Telemetry HAL:** Runtime-selectable telemetry backend (`--telemetry-backend sim|nvml|amd-sysfs|auto`) with deterministic `auto` fallback.
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

# Force AMD Linux sysfs telemetry (fails fast if amdgpu sysfs is unavailable)
cargo run --bin rust-edge-compute -- --telemetry-backend amd-sysfs

# Select a specific GPU index (cardN for amd-sysfs, device index for nvml)
cargo run --bin rust-edge-compute -- --telemetry-backend auto --gpu-index 1
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

Telemetry backend notes:
- `auto` fallback order is `nvml -> amd-sysfs -> sim`.
- `nvml` requires NVIDIA driver libraries to be present on the node.
- `amd-sysfs` requires Linux with the `amdgpu` kernel driver and `/sys/class/drm/cardN/device` metrics.
- `--gpu-index` defaults to `0`; increase it to target another GPU.

### 4. Job Orchestration Usage (MVP)

With the server and at least one worker running, submit jobs through `JobService`:

```bash
grpcurl -plaintext -import-path proto -proto node.proto \
  -d '{"kind":"simulated","payload":"{\"task\":\"demo\"}"}' \
  localhost:50051 node.JobService/SubmitJob
```

Use the returned `job_id` to query state:

```bash
grpcurl -plaintext -import-path proto -proto node.proto \
  -d '{"job_id":"job-000001"}' \
  localhost:50051 node.JobService/GetJobStatus
```

Submit a constrained high-priority job (example):

```bash
grpcurl -plaintext -import-path proto -proto node.proto \
  -d '{"kind":"simulated","payload":"{\"task\":\"priority-demo\"}","requiredCapabilities":["telemetry:nvml"],"priority":"JOB_PRIORITY_HIGH"}' \
  localhost:50051 node.JobService/SubmitJob
```

What to expect:
- Workers poll `LeaseJob`, execute `kind=simulated`, then send `ReportJobResult`.
- Workers advertise capabilities (for example `telemetry:simulated` or `telemetry:nvml`) during lease polling.
- Workers renew active leases with `ExtendJobLease` while a job is running.
- Payloads containing `fail` intentionally produce a failed simulated job result.
- Failed jobs retry automatically (max 3 attempts) with exponential backoff (`2s`, `4s`, `8s`) before moving to `FAILED`.
- Scheduler filters by `requiredCapabilities` and picks the highest-priority eligible jobs first (`HIGH > NORMAL > LOW`, FIFO within each priority).
- If a worker disappears after leasing, the server requeues the job after the lease timeout (15s) so another worker can pick it up.
- `CancelJob` is cooperative for leased/running work: queued jobs cancel immediately, active jobs move to `CANCEL_REQUESTED` and finalize to `CANCELLED` when the worker reports or when its lease expires.
- The orchestrator TUI shows queue/run counters in `Jobs Q/L/R/S/F`, queued priority mix in `Queued H/N/L`, and a recent jobs panel.

Optional directives for simulated jobs:
- Include `sleep_ms=<N>` in payload to emulate longer-running work and exercise lease renewal/cancellation behavior.

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
- [x] **Hardware HAL:** Implement `nvml-wrapper` + `amd-sysfs` telemetry sources with runtime backend selection and `auto` fallback.
- [x] **Reconnect Backoff:** Replace fixed retry delay with exponential backoff + jitter + reset-on-success.
- [x] **Protocol Expansion:** Extend heartbeat payload to include usage, VRAM, uptime, and client version.
- [x] **Stale Eviction:** Add TTL-based stale node pruning with eviction counters on the orchestrator dashboard.
- [x] **CI Pipeline:** Add formatting, lint, tests, and headless smoke checks in GitHub Actions.
