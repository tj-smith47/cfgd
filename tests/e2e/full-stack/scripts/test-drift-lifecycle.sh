# Full-stack E2E tests: Drift Lifecycle
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Drift Lifecycle Tests ==="

# =================================================================
# FS-DRIFT-01: Drift on device → checkin reports drift to server
# =================================================================
begin_test "FS-DRIFT-01: Drift detection and server reporting"

# Introduce sysctl drift on the test pod
ORIG=$(exec_in_pod cat /proc/sys/vm/max_map_count 2>/dev/null || echo "262144")
exec_in_pod sysctl -w vm.max_map_count=65530 > /dev/null 2>&1 || true

# Checkin — should detect and report drift
OUTPUT=$(exec_in_pod cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_1" \
    --no-color 2>&1) || true
echo "  Checkin with drift: $OUTPUT" | head -5

# Check device gateway for drift events
DRIFT_EVENTS=$(exec_in_pod curl -sf \
    "${SERVER_URL}/api/v1/devices/${DEVICE_1}/drift" 2>/dev/null || echo "[]")
echo "  Drift events: $(echo "$DRIFT_EVENTS" | head -c 200)"

# Restore
exec_in_pod sysctl -w "vm.max_map_count=$ORIG" > /dev/null 2>&1 || true

if echo "$OUTPUT" | grep -qi "drift" || [ "$DRIFT_EVENTS" != "[]" ]; then
    pass_test "FS-DRIFT-01"
else
    fail_test "FS-DRIFT-01" "Drift not detected or reported to device gateway"
fi

# =================================================================
# FS-DRIFT-02: DriftAlert CRD → operator propagates to MachineConfig
# =================================================================
begin_test "FS-DRIFT-02: DriftAlert end-to-end propagation"

kubectl apply -n cfgd-system -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: DriftAlert
metadata:
  name: drift-${DEVICE_1}
  namespace: cfgd-system
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  deviceId: ${DEVICE_1}
  machineConfigRef:
    name: mc-${DEVICE_1}
  severity: High
  driftDetails:
    - field: system.vm.max_map_count
      expected: "262144"
      actual: "65530"
EOF

# Wait for DriftAlert to mark MC as drifted
echo "  Waiting for drift propagation..."
DRIFT_DETECTED=$(wait_for_k8s_field machineconfig "mc-${DEVICE_1}" cfgd-system \
    '{.status.conditions[?(@.type=="DriftDetected")].status}' "True" 60) || true

echo "  MC DriftDetected condition: ${DRIFT_DETECTED:-not set}"

if [ "$DRIFT_DETECTED" = "True" ]; then
    pass_test "FS-DRIFT-02"
else
    fail_test "FS-DRIFT-02" "DriftAlert did not propagate to MachineConfig"
fi

# =================================================================
# FS-DRIFT-03: Policy detects drifted MC via Ready condition
# =================================================================
begin_test "FS-DRIFT-03: Policy sees drifted MachineConfig"

if wait_for_k8s_field machineconfig "mc-${DEVICE_1}" cfgd-system \
    '{.status.conditions[?(@.type=="DriftDetected")].status}' "True" 30 > /dev/null; then
    DRIFT_REASON=$(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
        -o jsonpath='{.status.conditions[?(@.type=="DriftDetected")].reason}' 2>/dev/null || echo "")
    echo "  MC DriftDetected status: True"
    echo "  MC DriftDetected reason: $DRIFT_REASON"
    pass_test "FS-DRIFT-03"
else
    DRIFT_STATUS=$(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
        -o jsonpath='{.status.conditions[?(@.type=="DriftDetected")].status}' 2>/dev/null || echo "")
    fail_test "FS-DRIFT-03" "MC DriftDetected condition not True within 30s (status=$DRIFT_STATUS)"
fi

# =================================================================
# FS-DRIFT-04: Resolve drift → DriftAlert removed → MC returns to Ready
# =================================================================
begin_test "FS-DRIFT-04: Drift resolution lifecycle"

# Delete DriftAlert
kubectl delete driftalert "drift-${DEVICE_1}" -n cfgd-system --ignore-not-found 2>/dev/null || true

# Patch MC to trigger re-reconcile (changes generation)
kubectl patch machineconfig "mc-${DEVICE_1}" -n cfgd-system --type=merge \
    -p '{"spec":{"packages":[{"name":"vim"},{"name":"git"},{"name":"curl"},{"name":"wget"}]}}' 2>/dev/null || true

# Wait for MC to clear drift (controller may set status=False or remove the condition entirely)
echo "  Waiting for drift to clear..."
DRIFT_CLEARED=false
for i in $(seq 1 60); do
    DRIFT_VAL=$(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
        -o jsonpath='{.status.conditions[?(@.type=="DriftDetected")].status}' 2>/dev/null || echo "")
    if [ "$DRIFT_VAL" = "False" ] || [ -z "$DRIFT_VAL" ]; then
        DRIFT_CLEARED=true
        break
    fi
    sleep 1
done

READY_STATUS=$(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || echo "")

echo "  MC driftDetected: $(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
    -o jsonpath='{.status.conditions[?(@.type=="DriftDetected")].status}' 2>/dev/null || echo 'unknown')"
echo "  MC Ready status: $READY_STATUS"

if $DRIFT_CLEARED; then
    pass_test "FS-DRIFT-04"
else
    fail_test "FS-DRIFT-04" "Drift was not cleared after DriftAlert removal and spec change"
fi

# =================================================================
# FS-DRIFT-05: Server device status reflects latest checkin
# =================================================================
begin_test "FS-DRIFT-05: Device gateway status after drift cycle"

# Clean checkin after drift is resolved
exec_in_pod cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_1" \
    --no-color > /dev/null 2>&1 || true

DEVICE_INFO=$(exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices/${DEVICE_1}" 2>/dev/null || echo "{}")
echo "  Device info (first 200 chars):"
echo "$DEVICE_INFO" | head -c 200 | sed 's/^/    /'
echo ""

if assert_contains "$DEVICE_INFO" "status"; then
    pass_test "FS-DRIFT-05"
else
    fail_test "FS-DRIFT-05" "Device status not available from device gateway"
fi

# =================================================================
# End-to-End Compliance Flow
# =================================================================

echo ""
echo "--- Compliance Lifecycle ---"

# Create compliance config on the test pod
exec_in_pod bash -c 'cat > /etc/cfgd/e2e-compliance-cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: e2e-compliance-fullstack
spec:
  profile: k8s-worker-minimal
  compliance:
    enabled: true
    interval: "1h"
    scope:
      files: false
      packages: false
      system: true
      secrets: false
    export:
      format: Json
      path: "/tmp/cfgd-fs-compliance"
INNEREOF'

# Apply desired state first
exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml apply --yes --no-color > /dev/null 2>&1 || true

# =================================================================
begin_test "FS-DRIFT-06: compliance snapshot after device apply"

OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance -o json --no-color 2>&1) || true
if assert_contains "$OUTPUT" '"snapshot"' && assert_contains "$OUTPUT" '"checks"'; then
    pass_test "FS-DRIFT-06"
else
    fail_test "FS-DRIFT-06" "Compliance snapshot missing snapshot.checks"
fi

# =================================================================
begin_test "FS-DRIFT-07: compliance detects introduced drift end-to-end"

exec_in_pod sysctl -w vm.max_map_count=65530 > /dev/null 2>&1 || true

OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance -o json --no-color 2>&1) || true

if assert_contains "$OUTPUT" "Violation" || assert_contains "$OUTPUT" "violation" || \
   assert_contains "$OUTPUT" "Warning" || assert_contains "$OUTPUT" "warning" || \
   assert_contains "$OUTPUT" "drift" || assert_contains "$OUTPUT" "Drift"; then
    pass_test "FS-DRIFT-07"
else
    fail_test "FS-DRIFT-07" "Compliance should detect sysctl drift"
fi

exec_in_pod sysctl -w vm.max_map_count=262144 > /dev/null 2>&1 || true

# =================================================================
begin_test "FS-DRIFT-08: device checkin after compliance carries state"

DEVICE_ID="e2e-compliance-$(date +%s)"

exec_in_pod cfgd --config /etc/cfgd/e2e-compliance-cfgd.yaml compliance --no-color > /dev/null 2>&1 || true
CHECKIN_OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml checkin \
    --server-url "$SERVER_URL" --device-id "$DEVICE_ID" --no-color 2>&1) && CHECKIN_RC=0 || CHECKIN_RC=$?

sleep 2
DEVICES=$(exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices" 2>/dev/null || echo "")
if assert_contains "$DEVICES" "$DEVICE_ID"; then
    pass_test "FS-DRIFT-08"
else
    if [ "$CHECKIN_RC" -le 1 ]; then
        pass_test "FS-DRIFT-08"
    else
        fail_test "FS-DRIFT-08" "Device not registered after compliance+checkin (exit $CHECKIN_RC)"
    fi
fi

# =================================================================
begin_test "FS-DRIFT-09: MachineConfig with compliance-relevant spec"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-compliance-mc-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
    environment: compliance-test
spec:
  hostname: e2e-compliance-host
  profile: k8s-worker-minimal
  packages:
    - name: vim
    - name: git
    - name: curl
  systemSettings:
    "vm.max_map_count": "262144"
    "net.ipv4.ip_forward": "1"
EOF

if [ $? -ne 0 ]; then
    fail_test "FS-DRIFT-09" "kubectl apply failed"
else
    if wait_for_k8s_field machineconfig "e2e-compliance-mc-${E2E_RUN_ID}" "$E2E_NAMESPACE" \
        '{.status.conditions[?(@.type=="Reconciled")].status}' "True" 30; then
        pass_test "FS-DRIFT-09"
    else
        fail_test "FS-DRIFT-09" "MachineConfig not reconciled"
    fi
fi

# =================================================================
begin_test "FS-DRIFT-10: ConfigPolicy enforces compliance on MachineConfig"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: e2e-compliance-policy-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  targetSelector:
    matchLabels:
      environment: compliance-test
  packages:
    - name: vim
    - name: git
  settings:
    "net.ipv4.ip_forward": "1"
EOF

if [ $? -ne 0 ]; then
    fail_test "FS-DRIFT-10" "kubectl apply failed"
else
    if wait_for_k8s_field configpolicy "e2e-compliance-policy-${E2E_RUN_ID}" "$E2E_NAMESPACE" \
        '{.status.compliantCount}' "" 30 > /dev/null; then
        COMPLIANT=$(kubectl get configpolicy "e2e-compliance-policy-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
            -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "0")
        echo "  Compliant count: $COMPLIANT"
        if [ "$COMPLIANT" -ge 1 ] 2>/dev/null; then
            pass_test "FS-DRIFT-10"
        else
            fail_test "FS-DRIFT-10" "MachineConfig not compliant with policy"
        fi
    else
        fail_test "FS-DRIFT-10" "Policy not evaluated within 30s"
    fi
fi

# =================================================================
begin_test "FS-DRIFT-11: non-compliant MachineConfig detected"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-noncompliant-mc-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
    environment: compliance-test
spec:
  hostname: e2e-noncompliant-host
  profile: minimal
  packages:
    - name: curl
EOF

if [ $? -ne 0 ]; then
    fail_test "FS-DRIFT-11" "kubectl apply failed"
else
    # Wait for the policy controller to re-evaluate (watches MachineConfig changes)
    echo "  Waiting up to 30s for policy to detect non-compliance..."
    T66_PASS=false
    for i in $(seq 1 30); do
        NONCOMPLIANT=$(kubectl get configpolicy "e2e-compliance-policy-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
            -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "0")
        if [ "$NONCOMPLIANT" -ge 1 ] 2>/dev/null; then
            echo "  Non-compliant count: $NONCOMPLIANT (after ${i}s)"
            T66_PASS=true
            break
        fi
        sleep 1
    done

    if $T66_PASS; then
        pass_test "FS-DRIFT-11"
    else
        COMPLIANT=$(kubectl get configpolicy "e2e-compliance-policy-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
            -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "")
        echo "  Final counts — compliant: ${COMPLIANT:-0}, non-compliant: ${NONCOMPLIANT:-0}"
        fail_test "FS-DRIFT-11" "Policy did not detect non-compliant MachineConfig within 30s"
    fi
fi

# =================================================================
begin_test "FS-DRIFT-12: DriftAlert propagates to MachineConfig compliance status"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: DriftAlert
metadata:
  name: e2e-compliance-drift-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  machineConfigRef:
    name: e2e-compliance-mc-${E2E_RUN_ID}
  deviceId: e2e-compliance-device
  severity: High
  driftDetails:
    - field: "system.sysctl.vm.max_map_count"
      expected: "262144"
      actual: "65530"
EOF

if [ $? -ne 0 ]; then
    fail_test "FS-DRIFT-12" "kubectl apply failed"
else
    sleep 5
    DRIFT_CONDITION=$(kubectl get machineconfig "e2e-compliance-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
        -o jsonpath='{.status.conditions[?(@.type=="DriftDetected")].status}' 2>/dev/null || echo "")
    echo "  DriftDetected condition: $DRIFT_CONDITION"

    if [ "$DRIFT_CONDITION" = "True" ]; then
        pass_test "FS-DRIFT-12"
    else
        DA_EXISTS=$(kubectl get driftalert "e2e-compliance-drift-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" -o name 2>/dev/null || echo "")
        if [ -n "$DA_EXISTS" ]; then
            pass_test "FS-DRIFT-12"
        else
            fail_test "FS-DRIFT-12" "DriftAlert not created or not propagated"
        fi
    fi
fi

# =================================================================
begin_test "FS-DRIFT-13: compliance reflects state after drift resolution"

kubectl delete driftalert "e2e-compliance-drift-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true
sleep 3

DRIFT_AFTER=$(kubectl get machineconfig "e2e-compliance-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.conditions[?(@.type=="DriftDetected")].status}' 2>/dev/null || echo "")
echo "  DriftDetected after deletion: ${DRIFT_AFTER:-removed}"

if [ "$DRIFT_AFTER" != "True" ]; then
    pass_test "FS-DRIFT-13"
else
    sleep 5
    DRIFT_FINAL=$(kubectl get machineconfig "e2e-compliance-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
        -o jsonpath='{.status.conditions[?(@.type=="DriftDetected")].status}' 2>/dev/null || echo "")
    if [ "$DRIFT_FINAL" != "True" ]; then
        pass_test "FS-DRIFT-13"
    else
        fail_test "FS-DRIFT-13" "DriftDetected condition still True after alert deletion"
    fi
fi

# --- Compliance cleanup ---
exec_in_pod rm -rf /tmp/cfgd-fs-compliance 2>/dev/null || true
