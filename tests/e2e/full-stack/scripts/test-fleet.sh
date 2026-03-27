# Full-stack E2E tests: Fleet
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Fleet Tests ==="

# =================================================================
# FS-FLEET-01: Device 1 checkin → server registers device
# =================================================================
begin_test "FS-FLEET-01: Device 1 checkin"

DEVICE_1="fullstack-device-1-${E2E_RUN_ID}"
OUTPUT=$(exec_in_pod cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_1" \
    --no-color 2>&1) || true
echo "  Checkin output: $OUTPUT"

DEVICES=$(exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices" 2>/dev/null || echo "[]")

if assert_contains "$DEVICES" "$DEVICE_1"; then
    pass_test "FS-FLEET-01"
else
    fail_test "FS-FLEET-01" "Device 1 not found in device gateway response"
fi

# =================================================================
# FS-FLEET-02: Device 2 checkin → multi-device fleet
# =================================================================
begin_test "FS-FLEET-02: Device 2 checkin (multi-device)"

DEVICE_2="fullstack-device-2-${E2E_RUN_ID}"
OUTPUT=$(exec_in_pod cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_2" \
    --no-color 2>&1) || true

DEVICES=$(exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices" 2>/dev/null || echo "[]")

if assert_contains "$DEVICES" "$DEVICE_1" && assert_contains "$DEVICES" "$DEVICE_2"; then
    pass_test "FS-FLEET-02"
else
    fail_test "FS-FLEET-02" "Multi-device fleet not visible"
fi

# =================================================================
# FS-FLEET-03: MachineConfig CRD created for fleet device
# =================================================================
begin_test "FS-FLEET-03: MachineConfig for fleet device"

kubectl apply -n cfgd-system -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: mc-${DEVICE_1}
  namespace: cfgd-system
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  hostname: ${DEVICE_1}
  profile: k8s-worker-minimal
  packages:
    - name: vim
    - name: git
    - name: curl
  systemSettings:
    "net.ipv4.ip_forward": "1"
    "vm.max_map_count": "262144"
EOF

# Wait for operator to reconcile
MC_STATUS=$(wait_for_k8s_field machineconfig "mc-${DEVICE_1}" cfgd-system \
    '{.status.lastReconciled}' "" 60) || true

echo "  MC lastReconciled: ${MC_STATUS:-not set}"

if [ -n "$MC_STATUS" ]; then
    pass_test "FS-FLEET-03"
else
    fail_test "FS-FLEET-03" "MachineConfig not reconciled by operator"
fi

# =================================================================
# FS-FLEET-04: ConfigPolicy applies across fleet MachineConfigs
# =================================================================
begin_test "FS-FLEET-04: ConfigPolicy fleet enforcement"

# Create MC for device 2 (compliant)
kubectl apply -n cfgd-system -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: mc-${DEVICE_2}
  namespace: cfgd-system
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  hostname: ${DEVICE_2}
  profile: k8s-worker-minimal
  packages:
    - name: vim
    - name: git
  systemSettings:
    "net.ipv4.ip_forward": "1"
EOF

# Create fleet-wide policy
kubectl apply -n cfgd-system -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: fleet-baseline-${E2E_RUN_ID}
  namespace: cfgd-system
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  packages:
    - name: vim
    - name: git
  settings:
    "net.ipv4.ip_forward": "1"
EOF

# Wait for policy evaluation
sleep 5
COMPLIANT=$(wait_for_k8s_field configpolicy "fleet-baseline-${E2E_RUN_ID}" cfgd-system \
    '{.status.compliantCount}' "" 60) || true

NON_COMPLIANT=$(kubectl get configpolicy "fleet-baseline-${E2E_RUN_ID}" -n cfgd-system \
    -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")

echo "  Fleet policy — compliant: ${COMPLIANT:-0}, non-compliant: ${NON_COMPLIANT:-0}"

if [ "${COMPLIANT:-0}" -ge 1 ]; then
    pass_test "FS-FLEET-04"
else
    fail_test "FS-FLEET-04" "Fleet policy not evaluated (compliantCount=${COMPLIANT:-0})"
fi
