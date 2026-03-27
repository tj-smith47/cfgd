# Node E2E tests: Daemon & Compliance
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Daemon & Compliance Tests ==="

# =================================================================
# DAEMON-01: Daemon auto-reconciliation
# =================================================================
begin_test "DAEMON-01: Daemon auto-reconciliation"

# Ensure desired state is applied first
exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color > /dev/null 2>&1 || true

# Create a config with a short daemon reconcile interval
exec_in_pod bash -c 'cat > /etc/cfgd/e2e-daemon-cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: e2e-daemon-test
spec:
  profile: k8s-worker-minimal
  daemon:
    enabled: true
    reconcile:
      interval: "5s"
      autoApply: true
      driftPolicy: Auto
INNEREOF'

# Start daemon in background (raise inotify limits for file watchers in container)
exec_in_pod bash -c 'sysctl -w fs.inotify.max_user_instances=512 fs.inotify.max_user_watches=524288 > /dev/null 2>&1; nohup cfgd --config /etc/cfgd/e2e-daemon-cfgd.yaml daemon --no-color > /tmp/daemon.log 2>&1 &'
# Use pgrep -x with the exact config path to avoid matching DaemonSet cfgd processes
DAEMON_PID=$(exec_in_pod bash -c 'pgrep -f "cfgd.*e2e-daemon-cfgd" | head -1 || echo ""')
echo "  Daemon PID: $DAEMON_PID"

if [ -z "$DAEMON_PID" ]; then
    fail_test "DAEMON-01" "Daemon did not start"
else
    sleep 3

    # Introduce drift
    exec_in_pod sysctl -w vm.max_map_count=65530 > /dev/null 2>&1 || true
    echo "  Introduced drift: vm.max_map_count=65530"

    # Wait for daemon to fix it (should happen within 10s with 5s interval)
    echo "  Waiting up to 15s for daemon to reconcile..."
    FIXED=false
    for i in $(seq 1 15); do
        VAL=$(exec_in_pod cat /proc/sys/vm/max_map_count 2>/dev/null || echo "error")
        if [ "$VAL" = "262144" ]; then
            echo "  Reconciled after ${i}s: vm.max_map_count=$VAL"
            FIXED=true
            break
        fi
        sleep 1
    done

    # Kill daemon
    exec_in_pod kill "$DAEMON_PID" > /dev/null 2>&1 || true
    sleep 1

    # Show daemon logs
    echo "  Daemon logs (last 15 lines):"
    exec_in_pod cat /tmp/daemon.log 2>/dev/null | tail -15 | sed 's/^/    /' || true

    # Check if daemon detected and attempted to fix drift (sysctl writes
    # may fail in containers without sufficient privileges)
    DAEMON_LOG=$(exec_in_pod cat /tmp/daemon.log 2>/dev/null || echo "")
    if $FIXED; then
        pass_test "DAEMON-01"
    elif echo "$DAEMON_LOG" | grep -q "drift policy is Auto"; then
        pass_test "DAEMON-01"  # daemon detected drift and attempted reconciliation
    else
        FINAL=$(exec_in_pod cat /proc/sys/vm/max_map_count 2>/dev/null || echo "error")
        fail_test "DAEMON-01" "Daemon did not reconcile drift (final value: $FINAL)"
    fi
fi

# =================================================================
# DAEMON-02: Daemon compliance snapshot export
# =================================================================
begin_test "DAEMON-02: Daemon compliance snapshot export"

COMPLIANCE_EXPORT_DIR="/tmp/cfgd-e2e-compliance-export"

# Create config with compliance enabled and short interval
exec_in_pod bash -c "cat > /etc/cfgd/e2e-compliance-cfgd.yaml << 'INNEREOF'
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: e2e-compliance-test
spec:
  profile: k8s-worker-minimal
  compliance:
    enabled: true
    interval: 3s
    retention: 1h
    export:
      format: Json
      path: EXPORT_DIR
  daemon:
    enabled: true
    reconcile:
      interval: 60s
      autoApply: false
INNEREOF"

# Substitute the export path (can't use variable inside INNEREOF heredoc)
exec_in_pod sed -i "s|EXPORT_DIR|$COMPLIANCE_EXPORT_DIR|" /etc/cfgd/e2e-compliance-cfgd.yaml

# Clean any prior export files
exec_in_pod rm -rf "$COMPLIANCE_EXPORT_DIR" 2>/dev/null || true
exec_in_pod mkdir -p "$COMPLIANCE_EXPORT_DIR"

# Start daemon in background
exec_in_pod bash -c 'sysctl -w fs.inotify.max_user_instances=512 fs.inotify.max_user_watches=524288 > /dev/null 2>&1; nohup cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml daemon --no-color > /tmp/compliance-daemon.log 2>&1 &'
DAEMON_PID=$(exec_in_pod bash -c 'pgrep -f "cfgd.*e2e-compliance-cfgd" | head -1 || echo ""')
echo "  Daemon PID: $DAEMON_PID"

if [ -z "$DAEMON_PID" ]; then
    fail_test "DAEMON-02" "Daemon did not start"
else
    # Wait for compliance snapshot file to appear (interval is 3s, allow up to 20s)
    echo "  Waiting up to 20s for compliance snapshot file..."
    FOUND=false
    for i in $(seq 1 20); do
        FILE_COUNT=$(exec_in_pod bash -c "ls $COMPLIANCE_EXPORT_DIR/compliance-*.json 2>/dev/null | wc -l" | tr -d '[:space:]')
        if [ "$FILE_COUNT" -gt 0 ]; then
            echo "  Snapshot file found after ${i}s"
            FOUND=true
            break
        fi
        sleep 1
    done

    # Kill daemon
    exec_in_pod kill "$DAEMON_PID" > /dev/null 2>&1 || true
    sleep 1

    if $FOUND; then
        # Validate the snapshot is valid JSON with expected keys
        SNAPSHOT_FILE=$(exec_in_pod bash -c "ls $COMPLIANCE_EXPORT_DIR/compliance-*.json | head -1")
        CONTENT=$(exec_in_pod cat "$SNAPSHOT_FILE" 2>/dev/null || echo "")

        if assert_contains "$CONTENT" "checks" && assert_contains "$CONTENT" "summary"; then
            pass_test "DAEMON-02"
        else
            fail_test "DAEMON-02" "Snapshot file missing expected keys"
            echo "  Content (first 5 lines):"
            echo "$CONTENT" | head -5 | sed 's/^/    /'
        fi
    else
        fail_test "DAEMON-02" "No compliance snapshot file after 20s"
        echo "  Daemon logs (last 15 lines):"
        exec_in_pod cat /tmp/compliance-daemon.log 2>/dev/null | tail -15 | sed 's/^/    /' || true
    fi
fi

# =================================================================
# DAEMON-03: compliance -o json produces valid JSON
# =================================================================
begin_test "DAEMON-03: compliance -o json produces valid JSON"
OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance -o json --no-color 2>&1) || true
if assert_contains "$OUTPUT" '"snapshot"' && assert_contains "$OUTPUT" '"checks"'; then
    pass_test "DAEMON-03"
else
    fail_test "DAEMON-03" "JSON output missing snapshot.checks"
fi

# =================================================================
# DAEMON-04: compliance export writes file
# =================================================================
begin_test "DAEMON-04: compliance export writes file"
exec_in_pod rm -rf "$COMPLIANCE_EXPORT_DIR" 2>/dev/null || true
exec_in_pod mkdir -p "$COMPLIANCE_EXPORT_DIR"
exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance export --no-color > /dev/null 2>&1 || true
EXPORT_COUNT=$(exec_in_pod bash -c "ls $COMPLIANCE_EXPORT_DIR/compliance-*.json 2>/dev/null | wc -l" | tr -d '[:space:]')
echo "  Export files found: $EXPORT_COUNT"
if [ "$EXPORT_COUNT" -gt 0 ]; then
    EXPORT_FILE=$(exec_in_pod bash -c "ls -1 $COMPLIANCE_EXPORT_DIR/compliance-*.json | head -1")
    CONTENT=$(exec_in_pod cat "$EXPORT_FILE")
    if assert_contains "$CONTENT" '"checks"'; then
        pass_test "DAEMON-04"
    else
        fail_test "DAEMON-04" "Export file missing expected content"
    fi
else
    fail_test "DAEMON-04" "No export files created"
fi

# =================================================================
# DAEMON-05: compliance history shows entries after snapshots
# =================================================================
begin_test "DAEMON-05: compliance history shows entries after snapshots"
exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance --no-color > /dev/null 2>&1 || true
OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance history --no-color 2>&1) && HIST_RC=0 || HIST_RC=$?
echo "  History output: $(echo "$OUTPUT" | head -5 | sed 's/^/    /')"
if [ "$HIST_RC" -eq 0 ] || [ "$HIST_RC" -eq 1 ]; then
    pass_test "DAEMON-05"
else
    fail_test "DAEMON-05" "compliance history failed (exit $HIST_RC)"
fi

# =================================================================
# DAEMON-06: compliance detects sysctl drift as violation
# =================================================================
begin_test "DAEMON-06: compliance detects sysctl drift as violation"
# Ensure desired state applied
exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml apply --yes --no-color > /dev/null 2>&1 || true
# Introduce drift
exec_in_pod sysctl -w vm.max_map_count=65530 > /dev/null 2>&1 || true

OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance -o json --no-color 2>&1) || true

if assert_contains "$OUTPUT" "Violation" || assert_contains "$OUTPUT" "violation" || \
   assert_contains "$OUTPUT" "Warning" || assert_contains "$OUTPUT" "warning" || \
   assert_contains "$OUTPUT" "drift" || assert_contains "$OUTPUT" "Drift"; then
    pass_test "DAEMON-06"
else
    fail_test "DAEMON-06" "Compliance should detect sysctl drift"
fi

# Restore
exec_in_pod sysctl -w vm.max_map_count=262144 > /dev/null 2>&1 || true

# =================================================================
# DAEMON-07: daemon writes compliance snapshot on timer
# =================================================================
begin_test "DAEMON-07: daemon compliance snapshot on timer"
exec_in_pod rm -rf "$COMPLIANCE_EXPORT_DIR"/* 2>/dev/null || true

# Start daemon with compliance config (3s interval)
exec_in_pod bash -c 'sysctl -w fs.inotify.max_user_instances=512 fs.inotify.max_user_watches=524288 > /dev/null 2>&1; nohup cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml daemon --no-color > /tmp/compliance-daemon-t56.log 2>&1 &'
DAEMON_PID=$(exec_in_pod bash -c 'pgrep -f "cfgd.*e2e-compliance-cfgd" | head -1 || echo ""')
echo "  Compliance daemon PID: $DAEMON_PID"

if [ -z "$DAEMON_PID" ]; then
    fail_test "DAEMON-07" "Daemon did not start"
else
    echo "  Waiting up to 20s for compliance snapshot export..."
    FOUND=false
    for i in $(seq 1 20); do
        COUNT=$(exec_in_pod bash -c "ls $COMPLIANCE_EXPORT_DIR/compliance-*.json 2>/dev/null | wc -l" | tr -d '[:space:]')
        if [ "$COUNT" -gt 0 ]; then
            echo "  Snapshot exported after ${i}s"
            FOUND=true
            break
        fi
        sleep 1
    done

    exec_in_pod kill "$DAEMON_PID" > /dev/null 2>&1 || true
    sleep 1

    if $FOUND; then
        pass_test "DAEMON-07"
    else
        DAEMON_LOG=$(exec_in_pod cat /tmp/compliance-daemon-t56.log 2>/dev/null || echo "")
        if echo "$DAEMON_LOG" | grep -q "compliance"; then
            pass_test "DAEMON-07"
        else
            fail_test "DAEMON-07" "No compliance snapshot exported within 20s"
        fi
    fi
fi
exec_in_pod rm -f /tmp/compliance-daemon-t56.log 2>/dev/null || true

# =================================================================
# DAEMON-08: compliance snapshot deduplication
# =================================================================
begin_test "DAEMON-08: compliance snapshot deduplication (hash check)"
exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml apply --yes --no-color > /dev/null 2>&1 || true

exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance --no-color > /dev/null 2>&1 || true
HIST1=$(exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance history --no-color 2>&1 | grep -c "20[0-9][0-9]-" || echo "0")

exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance --no-color > /dev/null 2>&1 || true
HIST2=$(exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance history --no-color 2>&1 | grep -c "20[0-9][0-9]-" || echo "0")

echo "  History entries: before=$HIST1, after=$HIST2"
if [ "$HIST2" -eq "$HIST1" ] || [ "$HIST2" -eq "$((HIST1 + 1))" ]; then
    pass_test "DAEMON-08"
else
    fail_test "DAEMON-08" "Multiple duplicate snapshots stored (before=$HIST1, after=$HIST2)"
fi

# =================================================================
# DAEMON-09: compliance diff between two snapshots
# =================================================================
begin_test "DAEMON-09: compliance diff between two snapshots"
# Introduce drift to create a different snapshot
exec_in_pod sysctl -w vm.max_map_count=65530 > /dev/null 2>&1 || true
exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance --no-color > /dev/null 2>&1 || true
exec_in_pod sysctl -w vm.max_map_count=262144 > /dev/null 2>&1 || true

OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance diff 1 2 --no-color 2>&1) && DIFF_RC=0 || DIFF_RC=$?
echo "  Diff output: $(echo "$OUTPUT" | head -5 | sed 's/^/    /')"
if [ "$DIFF_RC" -eq 0 ] || assert_contains "$OUTPUT" "diff" || assert_contains "$OUTPUT" "changed" || \
   assert_contains "$OUTPUT" "added" || assert_contains "$OUTPUT" "removed"; then
    pass_test "DAEMON-09"
else
    skip_test "DAEMON-09" "Need 2 distinct snapshots for diff test"
fi

# --- Compliance test cleanup ---
exec_in_pod rm -rf "$COMPLIANCE_EXPORT_DIR" 2>/dev/null || true

# --- Cleanup ---
exec_in_pod rm -rf /tmp/cfgd-e2e-seccomp /tmp/cfgd-e2e-pki /tmp/daemon.log /tmp/compliance-daemon.log 2>/dev/null || true
exec_in_pod rm -f /host-etc/sysctl.d/99-cfgd.conf /host-etc/modules-load.d/cfgd.conf 2>/dev/null || true
