# Rust Edge Compute Node 🦀 ⚡

> **Status:** 🚧 Active Development
> **Role:** Distributed Infrastructure / Systems Engineering

A fault-tolerant distributed worker node designed to aggregate heterogeneous hardware (Consumer & Data Center GPUs) into a unified compute substrate.

## 🏗 Architecture
This system mimics the worker-node architecture found in decentralized compute networks like Prime Intellect or Gensyn.

*   **Heartbeat Protocol:** gRPC-based health checks using `tonic`.
*   **Hardware Telemetry:** Real-time VRAM and Thermal monitoring via `nvml-wrapper` (NVIDIA Management Library).
*   **Concurrency:** Fully asynchronous event loop using `tokio` to handle job execution without blocking telemetry.
*   **Fault Tolerance:** Automatic connection retries and "Straggler Detection" for stalled compute jobs.

## 🛠 Tech Stack
*   **Language:** Rust (2021 Edition)
*   **Networking:** Tonic (gRPC), Prost (Protocol Buffers)
*   **Async Runtime:** Tokio
*   **Hardware:** NVML
*   **Interface:** Ratatui (TUI Dashboard)

## 🚀 Roadmap
- [ ] Phase 1: Establish gRPC Heartbeat with Mock Orchestrator
- [ ] Phase 2: Implement NVML Telemetry Stream
- [ ] Phase 3: Build Terminal UI (TUI) Dashboard
- [ ] Phase 4: Job Execution Sandbox (Docker)
