#!/usr/bin/env bash
# E2E tests for: cfgd daemon
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

# Per-instance IPC path so the test never collides with /tmp/cfgd.sock used
# by a real running daemon on the developer's machine. The CFGD_DAEMON_IPC_PATH
# env var is honored by daemon::run_daemon_with after the test-only
# DaemonRunOverrides field and before the DEFAULT_IPC_PATH const.
export CFGD_DAEMON_IPC_PATH="$SCRATCH/cfgd.sock"

# Kill any daemons we spawn, on every exit path. Bash trap is per-process and
# REPLACES inherited handlers, so this re-installs the parent's CLI_SCRATCH
# cleanup explicitly to keep the contract.
DAEMON_PIDS=()
daemon_cleanup() {
    local pid
    for pid in "${DAEMON_PIDS[@]:-}"; do
        if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
            kill -KILL "$pid" 2>/dev/null || true
        fi
    done
    rm -f "$CFGD_DAEMON_IPC_PATH" 2>/dev/null || true
}
trap 'daemon_cleanup; rm -rf "$CLI_SCRATCH"' EXIT

# Spawn a daemon in the background, capture PID, wait for IPC socket readiness.
# Writes the PID into the global SPAWNED_PID (do NOT use `pid=$(spawn_daemon ...)`
# — command substitution runs in a subshell, so the parent shell loses the
# backgrounded child from its job table and a later `wait $pid` returns 127).
# Returns 0 on ready, 1 on timeout/exit; caller decides whether to fail or skip.
SPAWNED_PID=""
spawn_daemon() {
    local log="$1"
    local retries=25
    rm -f "$CFGD_DAEMON_IPC_PATH"
    "$CFGD" $C daemon > "$log" 2>&1 &
    SPAWNED_PID=$!
    DAEMON_PIDS+=("$SPAWNED_PID")
    until [ -S "$CFGD_DAEMON_IPC_PATH" ] || [ "$retries" -le 0 ]; do
        if ! kill -0 "$SPAWNED_PID" 2>/dev/null; then
            return 1
        fi
        sleep 0.2
        retries=$((retries - 1))
    done
    [ -S "$CFGD_DAEMON_IPC_PATH" ]
}

# Stop a daemon cleanly via SIGTERM, wait for exit, return its exit code via
# $REAPED_RC. Uses bash `wait` directly so we reap before any other code can —
# polling kill -0 ahead of `wait` lets bash drop the job from its table and
# turns `wait $pid` into "no such job" (rc=127), losing the real exit status.
# A 3s SIGKILL backstop fires from a sidecar shell so a hung daemon can't
# block `wait` forever.
stop_daemon() {
    local pid="$1"
    kill -TERM "$pid" 2>/dev/null || true
    ( sleep 3; kill -KILL "$pid" 2>/dev/null || true ) &
    local watchdog=$!
    REAPED_RC=0
    wait "$pid" 2>/dev/null || REAPED_RC=$?
    kill -KILL "$watchdog" 2>/dev/null || true
    wait "$watchdog" 2>/dev/null || true
    if kill -0 "$pid" 2>/dev/null; then
        return 1
    fi
    return 0
}

echo "=== cfgd daemon tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "DM01: daemon --help"
run $C daemon --help
if assert_ok && assert_contains "$OUTPUT" "install"; then
    pass_test "DM01"
else fail_test "DM01"; fi

begin_test "DM02: daemon status"
run $C daemon status
# Daemon not running, status should still succeed (reports not-running)
if assert_ok; then
    pass_test "DM02"
else fail_test "DM02"; fi

begin_test "DM03: daemon install"
run $C daemon install
# Requires systemd/launchd — skip if unavailable
if assert_ok; then
    pass_test "DM03"
else
    skip_test "DM03" "daemon install not available (no init system)"
fi

begin_test "DM04: daemon uninstall"
run $C daemon uninstall
# Requires systemd/launchd — skip if unavailable
if assert_ok; then
    pass_test "DM04"
else
    skip_test "DM04" "daemon uninstall not available (no init system)"
fi

# --- Live-daemon lifecycle (DM05-DM08) -----------------------------------
# Probe host unix-socket binding via python3. If the host can't bind unix
# sockets (rare CI shapes), chain-skip DM05-DM08. If python3 itself is
# missing, attempt anyway — the environment is too minimal to introspect.
LIVE_SKIP=""
if command -v python3 > /dev/null 2>&1; then
    if ! python3 -c "import socket,os;p='$SCRATCH/probe.sock';s=socket.socket(socket.AF_UNIX);s.bind(p);s.close();os.unlink(p)" > /dev/null 2>&1; then
        LIVE_SKIP="host cannot bind unix sockets"
    fi
fi

DAEMON_LOG="$SCRATCH/daemon.log"
DAEMON_PID=""

begin_test "DM05: daemon spawn + ready"
if [ -n "$LIVE_SKIP" ]; then
    skip_test "DM05" "$LIVE_SKIP"
else
    spawn_daemon "$DAEMON_LOG" || true
    DAEMON_PID="$SPAWNED_PID"
    if [ -S "$CFGD_DAEMON_IPC_PATH" ] && kill -0 "$DAEMON_PID" 2>/dev/null \
        && grep -qF "Health:" "$DAEMON_LOG"; then
        pass_test "DM05"
    else
        fail_test "DM05" "spawn or readiness failed (pid=$DAEMON_PID); log tail:"
        tail -20 "$DAEMON_LOG" 2>/dev/null | sed 's/^/    /' || true
        DAEMON_PID=""
    fi
fi

begin_test "DM06: daemon status against live daemon"
if [ -n "$LIVE_SKIP" ]; then
    skip_test "DM06" "$LIVE_SKIP"
elif [ -z "$DAEMON_PID" ]; then
    skip_test "DM06" "DM05 did not produce a live daemon"
else
    run $C daemon status
    if assert_ok && assert_contains "$OUTPUT" "Daemon is running"; then
        pass_test "DM06"
    else
        fail_test "DM06"
    fi
fi

begin_test "DM07: SIGHUP reload"
if [ -n "$LIVE_SKIP" ]; then
    skip_test "DM07" "$LIVE_SKIP"
elif [ -z "$DAEMON_PID" ]; then
    skip_test "DM07" "DM05 did not produce a live daemon"
else
    kill -HUP "$DAEMON_PID" 2>/dev/null || true
    sleep 0.5
    SIGHUP_OK=""
    for _ in $(seq 1 25); do
        if grep -qF "Reloading configuration (SIGHUP)" "$DAEMON_LOG"; then
            SIGHUP_OK=1
            break
        fi
        sleep 0.2
    done
    if [ -n "$SIGHUP_OK" ] && kill -0 "$DAEMON_PID" 2>/dev/null; then
        pass_test "DM07"
    else
        fail_test "DM07" "SIGHUP log line missing or daemon died; log tail:"
        tail -20 "$DAEMON_LOG" 2>/dev/null | sed 's/^/    /' || true
    fi
fi

begin_test "DM08: SIGTERM clean shutdown + restart cycle"
if [ -n "$LIVE_SKIP" ]; then
    skip_test "DM08" "$LIVE_SKIP"
elif [ -z "$DAEMON_PID" ]; then
    skip_test "DM08" "DM05 did not produce a live daemon"
else
    DM08_OK=1
    # Cycle 1: shut down the daemon spawned in DM05.
    if ! stop_daemon "$DAEMON_PID"; then
        fail_test "DM08" "first daemon did not exit within 3s"
        DM08_OK=0
    elif [ "$REAPED_RC" -ne 0 ]; then
        fail_test "DM08" "first daemon exited with rc=$REAPED_RC (expected 0)"
        DM08_OK=0
    elif ! grep -qF "Received SIGTERM" "$DAEMON_LOG"; then
        fail_test "DM08" "first daemon log missing SIGTERM line"
        DM08_OK=0
    fi
    # Cycle 2: respawn and shut down again — the restart-not-reload invariant.
    if [ "$DM08_OK" -eq 1 ]; then
        DAEMON_LOG2="$SCRATCH/daemon-cycle2.log"
        spawn_daemon "$DAEMON_LOG2" || true
        DAEMON_PID2="$SPAWNED_PID"
        if [ ! -S "$CFGD_DAEMON_IPC_PATH" ] || ! kill -0 "$DAEMON_PID2" 2>/dev/null \
            || ! grep -qF "Health:" "$DAEMON_LOG2"; then
            fail_test "DM08" "respawn failed; cycle2 log tail:"
            tail -20 "$DAEMON_LOG2" 2>/dev/null | sed 's/^/    /' || true
            DM08_OK=0
        elif ! stop_daemon "$DAEMON_PID2"; then
            fail_test "DM08" "respawned daemon did not exit within 3s"
            DM08_OK=0
        elif [ "$REAPED_RC" -ne 0 ]; then
            fail_test "DM08" "respawned daemon exited with rc=$REAPED_RC (expected 0)"
            DM08_OK=0
        fi
    fi
    if [ "$DM08_OK" -eq 1 ]; then
        pass_test "DM08"
    fi
    DAEMON_PID=""
fi

print_summary "Daemon"
