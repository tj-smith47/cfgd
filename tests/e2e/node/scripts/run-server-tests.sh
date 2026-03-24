#!/usr/bin/env bash
# E2E tests for cfgd <-> device gateway integration.
# Prereqs: k3s cluster running, cfgd image available, device gateway deployed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
FIXTURES="$SCRIPT_DIR/../fixtures"

echo "=== cfgd Device Gateway Integration Tests ==="

trap 'cleanup_e2e' EXIT

echo "Setting up test pod..."
ensure_test_pod

echo "Copying test fixtures to test pod..."
exec_in_pod mkdir -p /etc/cfgd/profiles
cp_to_pod "$FIXTURES/configs/cfgd.yaml" /etc/cfgd/cfgd.yaml
for f in "$FIXTURES/profiles/"*.yaml; do
    cp_to_pod "$f" "/etc/cfgd/profiles/$(basename "$f")"
done

SERVER_URL="http://cfgd-server.cfgd-system.svc.cluster.local:8080"
echo "Device gateway URL: $SERVER_URL"

# Verify device gateway is reachable from the test pod
echo "Verifying device gateway reachability from test pod..."
exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices" > /dev/null 2>&1 || {
    echo "WARNING: device gateway not reachable from test pod. Waiting..."
    sleep 10
    exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices" > /dev/null 2>&1 || {
        echo "ERROR: device gateway not reachable"
        exit 1
    }
}

DEVICE_ID="e2e-test-$(date +%s)"

# =================================================================
# T30: cfgd checkin succeeds
# =================================================================
begin_test "T30: cfgd checkin"
RC=0
OUTPUT=$(exec_in_pod cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_ID" \
    --no-color 2>&1) || RC=$?

echo "  Checkin output:"
echo "$OUTPUT" | sed 's/^/    /'

if [ "$RC" -eq 0 ] && assert_contains "$OUTPUT" "ok"; then
    pass_test "T30"
else
    fail_test "T30" "Checkin failed (exit code: $RC)"
fi

# =================================================================
# T31: Device registered on server
# =================================================================
begin_test "T31: Device registered on device gateway"
DEVICES=$(exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices" 2>/dev/null || echo "[]")
echo "  Device gateway response (first 200 chars):"
echo "$DEVICES" | head -c 200 | sed 's/^/    /'
echo ""

if assert_contains "$DEVICES" "$DEVICE_ID"; then
    pass_test "T31"
else
    fail_test "T31" "Device not found in device gateway response"
fi

# =================================================================
# T32: Device status is healthy after apply + re-checkin
# =================================================================
begin_test "T32: Device status is healthy"
# Apply to bring the node into compliance, then checkin again
exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color > /dev/null 2>&1 || true
exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml checkin \
    --server-url "$SERVER_URL" --device-id "$DEVICE_ID" --no-color > /dev/null 2>&1 || true

DEVICE=$(exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices/${DEVICE_ID}" 2>/dev/null || echo "{}")
echo "  Device details (first 300 chars):"
echo "$DEVICE" | head -c 300 | sed 's/^/    /'
echo ""

if assert_contains "$DEVICE" "healthy"; then
    pass_test "T32"
else
    fail_test "T32" "Device status is not healthy"
fi

# =================================================================
# T33: Drift reporting
# =================================================================
begin_test "T33: Drift reporting to device gateway"
# Introduce drift on a sysctl value
ORIG_MAX=$(exec_in_pod cat /proc/sys/vm/max_map_count 2>/dev/null || echo "262144")
exec_in_pod sysctl -w vm.max_map_count=65530 > /dev/null 2>&1 || true

# Checkin again — should detect and report drift
OUTPUT=$(exec_in_pod cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_ID" \
    --no-color 2>&1) || true
echo "  Checkin with drift output:"
echo "$OUTPUT" | head -10 | sed 's/^/    /'

# Restore sysctl
exec_in_pod sysctl -w "vm.max_map_count=$ORIG_MAX" > /dev/null 2>&1 || true

if assert_contains "$OUTPUT" "drift"; then
    pass_test "T33"
else
    # Drift may have been auto-fixed by a previous apply; check server anyway
    DRIFT_EVENTS=$(exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices/${DEVICE_ID}/drift" 2>/dev/null || echo "[]")
    echo "  Drift events from server:"
    echo "$DRIFT_EVENTS" | head -c 200 | sed 's/^/    /'
    echo ""

    if [ "$DRIFT_EVENTS" != "[]" ] && [ -n "$DRIFT_EVENTS" ]; then
        pass_test "T33"
    else
        fail_test "T33" "No drift reported"
    fi
fi

# =================================================================
# T34: Server drift events list
# =================================================================
begin_test "T34: Device gateway has drift events"
DRIFT_EVENTS=$(exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices/${DEVICE_ID}/drift" 2>/dev/null || echo "[]")
echo "  Drift events:"
echo "$DRIFT_EVENTS" | head -c 300 | sed 's/^/    /'
echo ""

if [ "$DRIFT_EVENTS" != "[]" ] && assert_contains "$DRIFT_EVENTS" "timestamp"; then
    pass_test "T34"
else
    # It's possible no drift was detected if sysctl was already at desired value
    skip_test "T34" "No drift events (sysctl may have been at desired value)"
fi

# =================================================================
# T35: Second checkin updates last_checkin timestamp
# =================================================================
begin_test "T35: Checkin updates timestamp"
BEFORE=$(exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices/${DEVICE_ID}" 2>/dev/null \
    | grep -o '"lastCheckin":"[^"]*"' || echo "")

sleep 2

exec_in_pod cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_ID" \
    --no-color > /dev/null 2>&1 || true

AFTER=$(exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices/${DEVICE_ID}" 2>/dev/null \
    | grep -o '"lastCheckin":"[^"]*"' || echo "")

echo "  Before: $BEFORE"
echo "  After:  $AFTER"

if [ "$BEFORE" != "$AFTER" ] && [ -n "$AFTER" ]; then
    pass_test "T35"
else
    fail_test "T35" "Timestamp did not change between checkins"
fi

# --- Summary ---
print_summary "Device Gateway Integration Tests"
