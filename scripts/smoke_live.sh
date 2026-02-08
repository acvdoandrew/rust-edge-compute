#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:-tmux}"
BIND_ADDR="${BIND_ADDR:-[::1]:50051}"
SERVER_URL="http://${BIND_ADDR}"

print_manual_steps() {
    cat <<EOF
Manual live demo (3 terminals):
1) Terminal A (server TUI):
   cd "${ROOT_DIR}"
   cargo run --bin server -- --bind "${BIND_ADDR}"

2) Terminal B (client 1):
   cd "${ROOT_DIR}"
   cargo run --bin rust-edge-compute -- --server "${SERVER_URL}" --id Node-A

3) Terminal C (client 2):
   cd "${ROOT_DIR}"
   cargo run --bin rust-edge-compute -- --server "${SERVER_URL}" --id Node-B

What to verify live:
- Server table shows Node-A and Node-B with heartbeat counts increasing.
- Press q in Terminal B; Node-A should disappear from the server table quickly (Disconnect RPC).
- Press q in Terminal C; Node-B should disappear too.
- Press q in Terminal A to stop orchestrator cleanly.
EOF
}

run_tmux_demo() {
    if ! command -v tmux >/dev/null 2>&1; then
        echo "[smoke] tmux is not installed."
        echo
        print_manual_steps
        exit 1
    fi

    local session_name
    session_name="edge-live-$(date +%s)"

    local server_cmd client_a_cmd client_b_cmd
    server_cmd="cd \"${ROOT_DIR}\" && cargo run --bin server -- --bind \"${BIND_ADDR}\""
    client_a_cmd="cd \"${ROOT_DIR}\" && cargo run --bin rust-edge-compute -- --server \"${SERVER_URL}\" --id Node-A"
    client_b_cmd="cd \"${ROOT_DIR}\" && cargo run --bin rust-edge-compute -- --server \"${SERVER_URL}\" --id Node-B"

    tmux new-session -d -s "${session_name}" -n live "${server_cmd}"
    tmux split-window -h -t "${session_name}:0" "${client_a_cmd}"
    tmux split-window -v -t "${session_name}:0.1" "${client_b_cmd}"
    tmux select-layout -t "${session_name}:0" tiled
    tmux display-message -t "${session_name}:0" "Press q in client panes to test graceful disconnect; q in server pane to exit."

    echo "[smoke] attached tmux session: ${session_name}"
    tmux attach -t "${session_name}"
}

run_headless_smoke() {
    if ! command -v script >/dev/null 2>&1; then
        echo "[smoke] 'script' command is required for headless mode."
        exit 1
    fi

    local tmp_dir server_log client_a_log client_b_log server_pid
    tmp_dir="$(mktemp -d /tmp/rust-edge-smoke.XXXXXX)"
    server_log="${tmp_dir}/server.log"
    client_a_log="${tmp_dir}/client_a.log"
    client_b_log="${tmp_dir}/client_b.log"
    server_pid=""

    cleanup() {
        if [[ -n "${server_pid}" ]] && kill -0 "${server_pid}" >/dev/null 2>&1; then
            kill -TERM "${server_pid}" >/dev/null 2>&1 || true
            wait "${server_pid}" >/dev/null 2>&1 || true
        fi
    }
    trap cleanup EXIT INT TERM

    local pty_check
    pty_check="$(mktemp /tmp/rust-edge-smoke-pty-check.XXXXXX)"
    if ! script -q -c "true" "${pty_check}" >/dev/null 2>&1; then
        rm -f "${pty_check}"
        echo "[smoke] cannot allocate a pseudo-terminal with 'script' in this environment."
        echo "[smoke] run './scripts/smoke_live.sh tmux' or './scripts/smoke_live.sh manual' on your local terminal."
        exit 1
    fi
    rm -f "${pty_check}"

    echo "[smoke] building project..."
    (cd "${ROOT_DIR}" && cargo build --quiet)

    echo "[smoke] starting server..."
    script -q -c "cd \"${ROOT_DIR}\" && cargo run --quiet --bin server -- --bind \"${BIND_ADDR}\"" "${server_log}" &
    server_pid=$!
    sleep 3

    echo "[smoke] running Node-SMOKE-A (INT after 8s)..."
    if timeout --preserve-status -s INT 8s \
        script -q -c "cd \"${ROOT_DIR}\" && cargo run --quiet --bin rust-edge-compute -- --server \"${SERVER_URL}\" --id Node-SMOKE-A" \
        "${client_a_log}"; then
        :
    else
        local status=$?
        if [[ "${status}" -ne 124 && "${status}" -ne 130 ]]; then
            echo "[smoke] client A failed with status ${status}"
            echo "[smoke] logs: ${tmp_dir}"
            exit 1
        fi
    fi

    sleep 2
    echo "[smoke] running Node-SMOKE-B (INT after 8s)..."
    if timeout --preserve-status -s INT 8s \
        script -q -c "cd \"${ROOT_DIR}\" && cargo run --quiet --bin rust-edge-compute -- --server \"${SERVER_URL}\" --id Node-SMOKE-B" \
        "${client_b_log}"; then
        :
    else
        local status=$?
        if [[ "${status}" -ne 124 && "${status}" -ne 130 ]]; then
            echo "[smoke] client B failed with status ${status}"
            echo "[smoke] logs: ${tmp_dir}"
            exit 1
        fi
    fi

    echo "[smoke] stopping server..."
    kill -INT "${server_pid}" >/dev/null 2>&1 || true
    wait "${server_pid}" || true
    server_pid=""

    echo "[smoke] validating logs..."
    grep -q "Orchestrator listening on" "${server_log}"
    grep -q "Orchestrator stopped." "${server_log}"
    grep -q "Edge Compute Node Initializing ID: Node-SMOKE-A" "${client_a_log}"
    grep -q "Telemetry stream ended." "${client_a_log}"
    grep -q "Edge Compute Node Initializing ID: Node-SMOKE-B" "${client_b_log}"
    grep -q "Telemetry stream ended." "${client_b_log}"

    trap - EXIT INT TERM
    echo "[smoke] PASS"
    echo "[smoke] logs preserved at: ${tmp_dir}"
}

usage() {
    cat <<EOF
Usage:
  scripts/smoke_live.sh tmux      # Live visual demo in 3 tmux panes
  scripts/smoke_live.sh headless  # Automated smoke run (no visual panes)
  scripts/smoke_live.sh manual    # Print step-by-step manual commands
EOF
}

case "${MODE}" in
    tmux)
        run_tmux_demo
        ;;
    headless)
        run_headless_smoke
        ;;
    manual)
        print_manual_steps
        ;;
    -h|--help|help)
        usage
        ;;
    *)
        echo "Unknown mode: ${MODE}" >&2
        echo
        usage
        exit 1
        ;;
esac
