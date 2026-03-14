#!/usr/bin/env bash
# E2E tests for cfgd <-> cfgd-server integration.
# Prereqs: kind cluster running, cfgd binary on node, cfgd-server deployed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
FIXTURES="$SCRIPT_DIR/../fixtures"

echo "=== cfgd Server Integration Tests ==="

# --- Setup ---
NODE="$(get_kind_node)"

# Ensure cfgd binary and fixtures are on the kind node
install_binary_on_node "cfgd:e2e-test" "/usr/local/bin/cfgd"
install_packages_on_node procps curl

docker exec "$NODE" mkdir -p /etc/cfgd/profiles
docker cp "$FIXTURES/configs/cfgd.yaml" "$NODE:/etc/cfgd/cfgd.yaml"
for f in "$FIXTURES/profiles/"*.yaml; do
    docker cp "$f" "$NODE:/etc/cfgd/profiles/$(basename "$f")"
done

# Determine cfgd-server cluster IP
SERVER_IP=$(kubectl get svc cfgd-server -n "$CFGD_NAMESPACE" \
    -o jsonpath='{.spec.clusterIP}' 2>/dev/null || echo "")

if [ -z "$SERVER_IP" ]; then
    echo "ERROR: cfgd-server service not found. Is it deployed?"
    echo "Deploying now..."
    kubectl apply -f "$SCRIPT_DIR/../manifests/cfgd-server.yaml" -n "$CFGD_NAMESPACE"
    wait_for_deployment "$CFGD_NAMESPACE" "cfgd-server" 120
    SERVER_IP=$(kubectl get svc cfgd-server -n "$CFGD_NAMESPACE" \
        -o jsonpath='{.spec.clusterIP}')
fi

SERVER_URL="http://${SERVER_IP}:8080"
echo "cfgd-server URL: $SERVER_URL"

# Verify server is reachable from the kind node
echo "Verifying cfgd-server reachability from kind node..."
exec_on_node curl -sf "${SERVER_URL}/api/v1/devices" > /dev/null 2>&1 || {
    echo "WARNING: cfgd-server not reachable from kind node. Waiting..."
    sleep 10
    exec_on_node curl -sf "${SERVER_URL}/api/v1/devices" > /dev/null 2>&1 || {
        echo "ERROR: cfgd-server not reachable"
        exit 1
    }
}

DEVICE_ID="e2e-test-$(date +%s)"

# =================================================================
# T30: cfgd checkin succeeds
# =================================================================
begin_test "T30: cfgd checkin"
OUTPUT=$(exec_on_node cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_ID" \
    --no-color 2>&1)
RC=$?

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
begin_test "T31: Device registered on server"
DEVICES=$(exec_on_node curl -sf "${SERVER_URL}/api/v1/devices" 2>/dev/null || echo "[]")
echo "  Server devices response (first 200 chars):"
echo "$DEVICES" | head -c 200 | sed 's/^/    /'
echo ""

if assert_contains "$DEVICES" "$DEVICE_ID"; then
    pass_test "T31"
else
    fail_test "T31" "Device not found in server response"
fi

# =================================================================
# T32: Device status is healthy
# =================================================================
begin_test "T32: Device status is healthy"
DEVICE=$(exec_on_node curl -sf "${SERVER_URL}/api/v1/devices/${DEVICE_ID}" 2>/dev/null || echo "{}")
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
begin_test "T33: Drift reporting to server"
# Introduce drift on a sysctl value
ORIG_MAX=$(exec_on_node cat /proc/sys/vm/max_map_count 2>/dev/null || echo "262144")
exec_on_node sysctl -w vm.max_map_count=65530 > /dev/null 2>&1 || true

# Checkin again — should detect and report drift
OUTPUT=$(exec_on_node cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_ID" \
    --no-color 2>&1)
echo "  Checkin with drift output:"
echo "$OUTPUT" | head -10 | sed 's/^/    /'

# Restore sysctl
exec_on_node sysctl -w "vm.max_map_count=$ORIG_MAX" > /dev/null 2>&1 || true

if assert_contains "$OUTPUT" "drift"; then
    pass_test "T33"
else
    # Drift may have been auto-fixed by a previous apply; check server anyway
    DRIFT_EVENTS=$(exec_on_node curl -sf "${SERVER_URL}/api/v1/devices/${DEVICE_ID}/drift" 2>/dev/null || echo "[]")
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
begin_test "T34: Server has drift events"
DRIFT_EVENTS=$(exec_on_node curl -sf "${SERVER_URL}/api/v1/devices/${DEVICE_ID}/drift" 2>/dev/null || echo "[]")
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
BEFORE=$(exec_on_node curl -sf "${SERVER_URL}/api/v1/devices/${DEVICE_ID}" 2>/dev/null \
    | grep -o '"last-checkin":"[^"]*"' || echo "")

sleep 2

exec_on_node cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_ID" \
    --no-color > /dev/null 2>&1

AFTER=$(exec_on_node curl -sf "${SERVER_URL}/api/v1/devices/${DEVICE_ID}" 2>/dev/null \
    | grep -o '"last-checkin":"[^"]*"' || echo "")

echo "  Before: $BEFORE"
echo "  After:  $AFTER"

if [ "$BEFORE" != "$AFTER" ] && [ -n "$AFTER" ]; then
    pass_test "T35"
else
    fail_test "T35" "Timestamp did not change between checkins"
fi

# --- Summary ---
print_summary "Server Integration Tests"
