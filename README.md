# Rust Edge Compute Node 🦀 ⚡

> A fault-tolerant, distributed compute agent built with Rust, Tokio, and gRPC.
> Simulates a decentralized GPU cluster where worker nodes report telemetry to a central control plane.

## 🏗 Architecture

This system uses an asynchronous **Actor Model** architecture to decouple data generation, rendering, and networking.

*   **Telemetry Actor:** Generates hardware stats (Temperature, VRAM, Usage) on an independent Tokio task.
*   **TUI Actor (Main Thread):** Renders a 60 FPS terminal dashboard using `ratatui`.
*   **Network Client:** A background gRPC worker that streams Heartbeats to the Orchestrator.
*   **State Management:** Uses `Arc<Mutex<State>>` to share live telemetry between the rendering loop and the network client without blocking.

## 🚀 Features

*   **Distributed Orchestration:** A central Server (`bin/server`) handling concurrent connections from multiple Nodes.
*   **High-Performance TUI:** Split-screen dashboard with real-time gauges and logs, powered by `ratatui` v0.29.
*   **Fault Tolerance:** Client automatically attempts reconnection with exponential backoff if the Orchestrator goes down.
*   **Dynamic Identity:** Nodes generate unique IDs at runtime (`Node-8821`), simulating a heterogeneous cluster.
*   **Simulated HAL:** Currently uses a **Simulated Hardware Abstraction Layer** to generate realistic thermal patterns for demonstration purposes (Cross-platform compatibility).

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

## 2. Launch Worker Nodes (Client)
Open a **new terminal tab** (or multiple) to spin up worker nodes.
```bash
cargo run --bin rust-edge-compute
```
* **Observe**: The Client TUI will launch, displaying live stats.
* **Verify**: Check the Server terminal. You will see interleaved logs confirming connection from multiple unique Node IDs.

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

### Phase 3: Polish & Systems Engineering (🚧 In Progress)
- [ ] **CLI Configuration:** Add `clap` to parse arguments (`--server <IP>`, `--id <NAME>`).
- [ ] **Orchestrator Dashboard:** Upgrade Server from stdout logs to a real-time TUI table of connected nodes.
- [ ] **Graceful Shutdown:** Handle `Ctrl+C` signals to disconnect cleanly from the mesh.
- [ ] **Hardware HAL:** Implement `nvml-wrapper` trait for optional real-GPU monitoring on supported Linux/NVIDIA machines.
```
```
```
