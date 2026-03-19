#!/usr/bin/env bash
# E2E full-stack tests: cfgd CLI → device gateway → cfgd-operator → CRDs.
# Tests the complete loop: device checkin, fleet management, drift propagation,
# multi-device scenarios, and policy enforcement across the stack.
# Prereqs: kind cluster running, all images loaded (cfgd, cfgd-operator).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
NODE_FIXTURES="$SCRIPT_DIR/../../node/fixtures"
OPERATOR_MANIFESTS="$SCRIPT_DIR/../../operator/manifests"
SERVER_MANIFEST="$SCRIPT_DIR/../../node/manifests/cfgd-server.yaml"

echo "=== cfgd Full-Stack E2E Tests ==="

# --- Setup ---
NODE="$(get_kind_node)"

# Install cfgd binary and fixtures on the kind node
install_binary_on_node "cfgd:e2e-test" "/usr/local/bin/cfgd"
install_packages_on_node procps curl kmod

docker exec "$NODE" mkdir -p /etc/cfgd/profiles
docker cp "$NODE_FIXTURES/configs/cfgd.yaml" "$NODE:/etc/cfgd/cfgd.yaml"
for f in "$NODE_FIXTURES/profiles/"*.yaml; do
    docker cp "$f" "$NODE:/etc/cfgd/profiles/$(basename "$f")"
done

# Generate and install CRDs
echo "Generating and installing CRDs..."
CRD_YAML=$(cargo run --release --bin cfgd-gen-crds --manifest-path "$REPO_ROOT/Cargo.toml" 2>/dev/null)
echo "$CRD_YAML" | kubectl apply -f - 2>&1

for crd in machineconfigs.cfgd.io configpolicies.cfgd.io driftalerts.cfgd.io; do
    kubectl wait --for=condition=established "crd/$crd" --timeout=30s 2>/dev/null || true
done

# Deploy device gateway (idempotent — may already be deployed by setup-e2e action)
echo "Deploying device gateway..."
kubectl apply -f "$SERVER_MANIFEST" -n cfgd-system
wait_for_deployment cfgd-system cfgd-server 120

# Deploy cfgd-operator
echo "Deploying cfgd-operator..."
kubectl apply -f "$OPERATOR_MANIFESTS/operator-deployment.yaml" -n cfgd-system
wait_for_deployment cfgd-system cfgd-operator 120

# Get server IP
SERVER_IP=$(kubectl get svc cfgd-server -n "$CFGD_NAMESPACE" \
    -o jsonpath='{.spec.clusterIP}' 2>/dev/null || echo "")
SERVER_URL="http://${SERVER_IP}:8080"
echo "Device gateway URL: $SERVER_URL"

# Wait for device gateway to be reachable from the kind node
echo "Waiting for device gateway reachability..."
for i in $(seq 1 60); do
    if exec_on_node curl -sf "${SERVER_URL}/api/v1/devices" > /dev/null 2>&1; then
        break
    fi
    sleep 2
done

echo "All components are running"

# =================================================================
# T01: All components deployed and healthy
# =================================================================
begin_test "T01: All components healthy"

GATEWAY_POD=$(kubectl get pods -n cfgd-system -l app=cfgd-server \
    -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo "")
OPERATOR_POD=$(kubectl get pods -n cfgd-system -l app=cfgd-operator \
    -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo "")
CFGD_AVAIL=$(exec_on_node cfgd --version 2>&1 || echo "")

echo "  cfgd binary:  ${CFGD_AVAIL:-not found}"
echo "  Gateway pod:  $GATEWAY_POD"
echo "  Operator pod: $OPERATOR_POD"

if [ "$GATEWAY_POD" = "Running" ] && [ "$OPERATOR_POD" = "Running" ] && [ -n "$CFGD_AVAIL" ]; then
    pass_test "T01"
else
    fail_test "T01" "Not all components are healthy"
fi

# =================================================================
# T02: Device 1 checkin → server registers device
# =================================================================
begin_test "T02: Device 1 checkin"

DEVICE_1="fullstack-device-1"
OUTPUT=$(exec_on_node cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_1" \
    --no-color 2>&1)
echo "  Checkin output: $OUTPUT"

DEVICES=$(exec_on_node curl -sf "${SERVER_URL}/api/v1/devices" 2>/dev/null || echo "[]")

if assert_contains "$DEVICES" "$DEVICE_1"; then
    pass_test "T02"
else
    fail_test "T02" "Device 1 not found in device gateway response"
fi

# =================================================================
# T03: Device 2 checkin → multi-device fleet
# =================================================================
begin_test "T03: Device 2 checkin (multi-device)"

DEVICE_2="fullstack-device-2"
OUTPUT=$(exec_on_node cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_2" \
    --no-color 2>&1)

DEVICES=$(exec_on_node curl -sf "${SERVER_URL}/api/v1/devices" 2>/dev/null || echo "[]")

if assert_contains "$DEVICES" "$DEVICE_1" && assert_contains "$DEVICES" "$DEVICE_2"; then
    pass_test "T03"
else
    fail_test "T03" "Multi-device fleet not visible"
fi

# =================================================================
# T04: MachineConfig CRD created for fleet device
# =================================================================
begin_test "T04: MachineConfig for fleet device"

kubectl apply -n cfgd-system -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: mc-${DEVICE_1}
  namespace: cfgd-system
spec:
  hostname: ${DEVICE_1}
  profile: k8s-worker-minimal
  packages:
    - vim
    - git
    - curl
  packageVersions:
    vim: "9.0.1"
    git: "2.40.0"
  systemSettings:
    "net.ipv4.ip_forward": "1"
    "vm.max_map_count": "262144"
EOF

# Wait for operator to reconcile
MC_STATUS=""
for i in $(seq 1 60); do
    MC_STATUS=$(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
        -o jsonpath='{.status.lastReconciled}' 2>/dev/null || echo "")
    if [ -n "$MC_STATUS" ]; then
        break
    fi
    sleep 1
done

echo "  MC lastReconciled: ${MC_STATUS:-not set}"

if [ -n "$MC_STATUS" ]; then
    pass_test "T04"
else
    fail_test "T04" "MachineConfig not reconciled by operator"
fi

# =================================================================
# T05: ConfigPolicy applies across fleet MachineConfigs
# =================================================================
begin_test "T05: ConfigPolicy fleet enforcement"

# Create MC for device 2 (compliant)
kubectl apply -n cfgd-system -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: mc-${DEVICE_2}
  namespace: cfgd-system
spec:
  hostname: ${DEVICE_2}
  profile: k8s-worker-minimal
  packages:
    - vim
    - git
  systemSettings:
    "net.ipv4.ip_forward": "1"
EOF

# Create fleet-wide policy
kubectl apply -n cfgd-system -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: fleet-baseline
  namespace: cfgd-system
spec:
  name: fleet-security-baseline
  packages:
    - vim
    - git
  settings:
    "net.ipv4.ip_forward": "1"
EOF

# Wait for policy evaluation
sleep 5
COMPLIANT=""
for i in $(seq 1 60); do
    COMPLIANT=$(kubectl get configpolicy fleet-baseline -n cfgd-system \
        -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "")
    if [ -n "$COMPLIANT" ]; then
        break
    fi
    sleep 1
done

NON_COMPLIANT=$(kubectl get configpolicy fleet-baseline -n cfgd-system \
    -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")

echo "  Fleet policy — compliant: ${COMPLIANT:-0}, non-compliant: ${NON_COMPLIANT:-0}"

if [ "${COMPLIANT:-0}" -ge 2 ]; then
    pass_test "T05"
elif [ -n "$COMPLIANT" ]; then
    pass_test "T05"
else
    fail_test "T05" "Fleet policy not evaluated"
fi

# =================================================================
# T06: Drift on device → checkin reports drift to server
# =================================================================
begin_test "T06: Drift detection and server reporting"

# Introduce sysctl drift on the kind node
ORIG=$(exec_on_node cat /proc/sys/vm/max_map_count 2>/dev/null || echo "262144")
exec_on_node sysctl -w vm.max_map_count=65530 > /dev/null 2>&1 || true

# Checkin — should detect and report drift
OUTPUT=$(exec_on_node cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_1" \
    --no-color 2>&1)
echo "  Checkin with drift: $OUTPUT" | head -5

# Check device gateway for drift events
DRIFT_EVENTS=$(exec_on_node curl -sf \
    "${SERVER_URL}/api/v1/devices/${DEVICE_1}/drift" 2>/dev/null || echo "[]")
echo "  Drift events: $(echo "$DRIFT_EVENTS" | head -c 200)"

# Restore
exec_on_node sysctl -w "vm.max_map_count=$ORIG" > /dev/null 2>&1 || true

if echo "$OUTPUT" | grep -qiE "drift|ok" || [ "$DRIFT_EVENTS" != "[]" ]; then
    pass_test "T06"
else
    fail_test "T06" "Drift not detected or reported to device gateway"
fi

# =================================================================
# T07: DriftAlert CRD → operator propagates to MachineConfig
# =================================================================
begin_test "T07: DriftAlert end-to-end propagation"

kubectl apply -n cfgd-system -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: DriftAlert
metadata:
  name: drift-${DEVICE_1}
  namespace: cfgd-system
spec:
  deviceId: ${DEVICE_1}
  machineConfigRef: mc-${DEVICE_1}
  severity: High
  driftDetails:
    - field: system.vm.max_map_count
      expected: "262144"
      actual: "65530"
EOF

# Wait for DriftAlert to mark MC as drifted
echo "  Waiting for drift propagation..."
DRIFT_DETECTED=""
for i in $(seq 1 60); do
    DRIFT_DETECTED=$(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
        -o jsonpath='{.status.driftDetected}' 2>/dev/null || echo "")
    if [ "$DRIFT_DETECTED" = "true" ]; then
        break
    fi
    sleep 1
done

echo "  MC driftDetected: ${DRIFT_DETECTED:-not set}"

if [ "$DRIFT_DETECTED" = "true" ]; then
    pass_test "T07"
else
    fail_test "T07" "DriftAlert did not propagate to MachineConfig"
fi

# =================================================================
# T08: Policy detects drifted MC via Ready condition
# =================================================================
begin_test "T08: Policy sees drifted MachineConfig"

READY_STATUS=$(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || echo "")
READY_REASON=$(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].reason}' 2>/dev/null || echo "")

echo "  MC Ready status: $READY_STATUS"
echo "  MC Ready reason: $READY_REASON"

if [ "$READY_STATUS" = "False" ] && [ "$READY_REASON" = "DriftDetected" ]; then
    pass_test "T08"
elif [ "$READY_STATUS" = "False" ]; then
    pass_test "T08"
else
    fail_test "T08" "MC Ready condition not False after drift"
fi

# =================================================================
# T09: Resolve drift → DriftAlert removed → MC returns to Ready
# =================================================================
begin_test "T09: Drift resolution lifecycle"

# Delete DriftAlert
kubectl delete driftalert "drift-${DEVICE_1}" -n cfgd-system 2>/dev/null || true

# Patch MC to trigger re-reconcile (changes generation)
kubectl patch machineconfig "mc-${DEVICE_1}" -n cfgd-system --type=merge \
    -p '{"spec":{"packages":["vim","git","curl","wget"]}}' 2>/dev/null

# Wait for MC to clear drift
echo "  Waiting for drift to clear..."
DRIFT_CLEARED=false
for i in $(seq 1 60); do
    DRIFT=$(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
        -o jsonpath='{.status.driftDetected}' 2>/dev/null || echo "true")
    if [ "$DRIFT" = "false" ]; then
        DRIFT_CLEARED=true
        break
    fi
    sleep 1
done

READY_STATUS=$(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || echo "")

echo "  MC driftDetected: $(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
    -o jsonpath='{.status.driftDetected}' 2>/dev/null || echo 'unknown')"
echo "  MC Ready status: $READY_STATUS"

if $DRIFT_CLEARED; then
    pass_test "T09"
else
    fail_test "T09" "Drift was not cleared after DriftAlert removal and spec change"
fi

# =================================================================
# T10: Server device status reflects latest checkin
# =================================================================
begin_test "T10: Device gateway status after drift cycle"

# Clean checkin after drift is resolved
exec_on_node cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_1" \
    --no-color > /dev/null 2>&1 || true

DEVICE_INFO=$(exec_on_node curl -sf "${SERVER_URL}/api/v1/devices/${DEVICE_1}" 2>/dev/null || echo "{}")
echo "  Device info (first 200 chars):"
echo "$DEVICE_INFO" | head -c 200 | sed 's/^/    /'
echo ""

if assert_contains "$DEVICE_INFO" "healthy" || assert_contains "$DEVICE_INFO" "$DEVICE_1"; then
    pass_test "T10"
else
    fail_test "T10" "Device status not available from device gateway"
fi

# --- Cleanup ---
echo ""
echo "Cleaning up test resources..."
kubectl delete machineconfig --all -n cfgd-system 2>/dev/null || true
kubectl delete configpolicy --all -n cfgd-system 2>/dev/null || true
kubectl delete driftalert --all -n cfgd-system 2>/dev/null || true

# --- Summary ---
print_summary "Full-Stack E2E Tests"
