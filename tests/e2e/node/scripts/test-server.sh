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
HEALTH_URL="http://cfgd-server.cfgd-system.svc.cluster.local:8081"
# Admin API key must match CFGD_API_KEY on the server deployment
GW_API_KEY="${CFGD_E2E_API_KEY:-cfgd-e2e-admin-key}"
echo "Device gateway URL: $SERVER_URL"

# Verify device gateway is reachable from the test pod (use the health endpoint, as the API requires auth)
echo "Verifying device gateway reachability from test pod..."
if ! wait_for_pod_url "${HEALTH_URL}/readyz" 60; then
    echo "ERROR: device gateway not reachable after 60s" >&2
    exit 1
fi

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
    --api-key "$GW_API_KEY" \
    --device-id "$DEVICE_ID" \
    --no-color 2>&1) || RC=$?

echo "  Checkin output:"
# shellcheck disable=SC2001  # sed indents each line; an expansion cannot
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
DEVICES=$(exec_in_pod curl -sf -H "Authorization: Bearer $GW_API_KEY" "${SERVER_URL}/api/v1/devices" 2>/dev/null || echo "[]")
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
    --server-url "$SERVER_URL" --api-key "$GW_API_KEY" --device-id "$DEVICE_ID" --no-color > /dev/null 2>&1 || true

DEVICE=$(exec_in_pod curl -sf -H "Authorization: Bearer $GW_API_KEY" "${SERVER_URL}/api/v1/devices/${DEVICE_ID}" 2>/dev/null || echo "{}")
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
T33_BEFORE=$(gateway_drift_count "$DEVICE_ID" net.ipv4.ip_forward)
# Introduce drift on a sysctl value
ORIG_FWD=$(exec_in_pod cat /proc/sys/net/ipv4/ip_forward 2>/dev/null || echo "1")
exec_in_pod sysctl -w net.ipv4.ip_forward=0 > /dev/null 2>&1 || true

# Checkin again: should detect and report drift
OUTPUT=$(exec_in_pod cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --api-key "$GW_API_KEY" \
    --device-id "$DEVICE_ID" \
    --no-color 2>&1) || true
echo "  Checkin with drift output:"
echo "$OUTPUT" | head -10 | sed 's/^/    /'

# Restore sysctl
exec_in_pod sysctl -w "net.ipv4.ip_forward=$ORIG_FWD" > /dev/null 2>&1 || true

T33_AFTER=$(gateway_drift_count "$DEVICE_ID" net.ipv4.ip_forward)
echo "  Gateway drift events naming net.ipv4.ip_forward: ${T33_BEFORE:-unreadable} before, ${T33_AFTER:-unreadable} after"

if [[ $T33_BEFORE =~ ^[0-9]+$ ]] && [[ $T33_AFTER =~ ^[0-9]+$ ]] && [ "$T33_AFTER" -gt "$T33_BEFORE" ]; then
    pass_test "T33"
else
    fail_test "T33" "The checkin reported no net.ipv4.ip_forward drift to the device gateway (events before ${T33_BEFORE:-unreadable}, after ${T33_AFTER:-unreadable})"
fi

# =================================================================
# T34: Server drift events list
# =================================================================
begin_test "T34: Device gateway has drift events"
DRIFT_EVENTS=$(exec_in_pod curl -sf -H "Authorization: Bearer $GW_API_KEY" "${SERVER_URL}/api/v1/devices/${DEVICE_ID}/drift" 2>/dev/null || echo "[]")
echo "  Drift events:"
echo "$DRIFT_EVENTS" | head -c 300 | sed 's/^/    /'
echo ""

# T33 left an event naming net.ipv4.ip_forward, and every event in the list is
# this device's.
if echo "$DRIFT_EVENTS" | jq -e --arg d "$DEVICE_ID" 'length >= 1 and all(.[]; .deviceId == $d and (.id // "") != "" and (.timestamp // "") != "") and any(.[]; .details | contains("net.ipv4.ip_forward"))' >/dev/null 2>&1; then
    pass_test "T34"
else
    fail_test "T34" "Expected this device's drift events, one naming net.ipv4.ip_forward: $(echo "$DRIFT_EVENTS" | head -c 300)"
fi

# =================================================================
# T35: Second checkin updates last_checkin timestamp
# =================================================================
begin_test "T35: Checkin updates timestamp"
BEFORE_RC=0
BEFORE_BODY=$(exec_in_pod curl -sf -H "Authorization: Bearer $GW_API_KEY" "${SERVER_URL}/api/v1/devices/${DEVICE_ID}" 2>&1) || BEFORE_RC=$?
BEFORE=$(sed -n 's/.*\("lastCheckin":"[^"]*"\).*/\1/p' <<<"$BEFORE_BODY")

sleep 1 # sleep-ok: lastCheckin has one-second resolution, so the second checkin has to land in a later second

exec_in_pod cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --api-key "$GW_API_KEY" \
    --device-id "$DEVICE_ID" \
    --no-color > /dev/null 2>&1 || true

AFTER_RC=0
AFTER_BODY=$(exec_in_pod curl -sf -H "Authorization: Bearer $GW_API_KEY" "${SERVER_URL}/api/v1/devices/${DEVICE_ID}" 2>&1) || AFTER_RC=$?
AFTER=$(sed -n 's/.*\("lastCheckin":"[^"]*"\).*/\1/p' <<<"$AFTER_BODY")

echo "  Before: $BEFORE"
echo "  After:  $AFTER"

if [ "$BEFORE_RC" -ne 0 ] || [ "$AFTER_RC" -ne 0 ]; then
    fail_test "T35" "Could not read the device record (curl exit before=$BEFORE_RC after=$AFTER_RC)"
elif [ "$BEFORE" != "$AFTER" ] && [ -n "$AFTER" ]; then
    pass_test "T35"
else
    fail_test "T35" "Timestamp did not change between checkins"
fi

# =================================================================
# T36: Compliance data in checkin
# =================================================================
begin_test "T36: Compliance data included in checkin"

# Create a config with compliance enabled
exec_in_pod bash -c 'cat > /etc/cfgd/e2e-compliance-checkin.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: e2e-compliance-checkin
spec:
  profile: k8s-worker-minimal
  compliance:
    enabled: true
    scope:
      files: true
      packages: false
      system: true
      secrets: false
INNEREOF'

# Copy profiles for this config (reuse existing profiles)
exec_in_pod bash -c 'ls /etc/cfgd/profiles/*.yaml > /dev/null 2>&1' || \
    cp_to_pod "$FIXTURES/profiles/k8s-worker-minimal.yaml" /etc/cfgd/profiles/k8s-worker-minimal.yaml 2>/dev/null || true

COMPLIANCE_DEVICE_ID="e2e-compliance-$(date +%s)"

RC=0
OUTPUT=$(exec_in_pod cfgd \
    --config /etc/cfgd/e2e-compliance-checkin.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --api-key "$GW_API_KEY" \
    --device-id "$COMPLIANCE_DEVICE_ID" \
    --no-color 2>&1) || RC=$?

echo "  Compliance checkin output:"
echo "$OUTPUT" | head -15 | sed 's/^/    /'

# Verify the device now has complianceSummary in its API response
DEVICE_RESP=$(exec_in_pod curl -sf -H "Authorization: Bearer $GW_API_KEY" "${SERVER_URL}/api/v1/devices/${COMPLIANCE_DEVICE_ID}" 2>/dev/null || echo "{}")
echo "  Device API response (first 400 chars):"
echo "$DEVICE_RESP" | head -c 400 | sed 's/^/    /'
echo ""

if [ "$RC" -eq 0 ] \
    && assert_contains "$DEVICE_RESP" "complianceSummary" \
    && assert_contains "$DEVICE_RESP" "compliant" \
    && assert_contains "$DEVICE_RESP" "violation"; then
    pass_test "T36"
else
    fail_test "T36" "Compliance summary not found in device API response (exit: $RC)"
fi

# Cleanup the compliance config
exec_in_pod rm -f /etc/cfgd/e2e-compliance-checkin.yaml 2>/dev/null || true

# --- Summary ---
print_summary "Device Gateway Integration Tests"
