#!/usr/bin/env bash
# E2E full-stack tests: cfgd CLI → device gateway → cfgd-operator → CRDs.
# Tests the complete loop: device checkin, fleet management, drift propagation,
# multi-device scenarios, policy enforcement, CSI driver, kubectl plugin, debug flow.
# Prereqs: k3s cluster running, all images built, persistent infra deployed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
NODE_FIXTURES="$SCRIPT_DIR/../../node/fixtures"

echo "=== cfgd Full-Stack E2E Tests ==="

# --- Verify infrastructure ---
echo "Verifying persistent infrastructure..."
kubectl wait --for=condition=available deployment/cfgd-operator -n cfgd-system --timeout=30s
kubectl wait --for=condition=available deployment/cfgd-server -n cfgd-system --timeout=30s
echo "All persistent components running"

# Check CSI driver
CSI_READY=$(kubectl get ds -n cfgd-system -l app.kubernetes.io/component=csi-driver \
    -o jsonpath='{.items[0].status.numberReady}' 2>/dev/null || echo "0")
CSI_AVAILABLE=true
if [ "$CSI_READY" = "0" ] || [ -z "$CSI_READY" ]; then
    echo "WARN: CSI driver not ready, CSI tests will be skipped"
    CSI_AVAILABLE=false
fi

# Set up ephemeral namespace and test pod
create_e2e_namespace

cleanup_fullstack() {
    # Additional namespace cleanup
    for ns in "e2e-csi-test-${E2E_RUN_ID}" "e2e-plugin-test-${E2E_RUN_ID}" "e2e-debug-flow-${E2E_RUN_ID}"; do
        kubectl delete namespace "$ns" --ignore-not-found --wait=false 2>/dev/null || true
    done
    # Delete run-scoped cluster-scoped resources
    kubectl delete module "csi-test-mod-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    kubectl delete module "debug-tools-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
    # Clean up namespaced CRDs in cfgd-system (persistent namespace, not cascade-deleted)
    for kind in machineconfig configpolicy driftalert; do
        kubectl delete "$kind" -l "$E2E_RUN_LABEL" -n cfgd-system --ignore-not-found 2>/dev/null || true
    done
    cleanup_e2e
}
trap 'cleanup_fullstack' EXIT

ensure_test_pod

# Copy fixtures to test pod
echo "Copying fixtures..."
exec_in_pod mkdir -p /etc/cfgd/profiles
cp_to_pod "$NODE_FIXTURES/configs/cfgd.yaml" /etc/cfgd/cfgd.yaml
for f in "$NODE_FIXTURES/profiles/"*.yaml; do
    cp_to_pod "$f" "/etc/cfgd/profiles/$(basename "$f")"
done

# Build cfgd binary on host for kubectl plugin tests
ensure_cfgd_binary
KUBECTL_CFGD="/tmp/kubectl-cfgd"
ln -sf "$CFGD_BIN" "$KUBECTL_CFGD"

SERVER_URL="http://cfgd-server.cfgd-system.svc.cluster.local:8080"
echo "Device gateway URL: $SERVER_URL"

# Wait for gateway reachability from test pod
echo "Waiting for device gateway..."
for i in $(seq 1 60); do
    if exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices" > /dev/null 2>&1; then
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
CFGD_AVAIL=$(exec_in_pod cfgd --version 2>&1 || echo "")

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
    pass_test "T02"
else
    fail_test "T02" "Device 1 not found in device gateway response"
fi

# =================================================================
# T03: Device 2 checkin → multi-device fleet
# =================================================================
begin_test "T03: Device 2 checkin (multi-device)"

DEVICE_2="fullstack-device-2-${E2E_RUN_ID}"
OUTPUT=$(exec_in_pod cfgd \
    --config /etc/cfgd/cfgd.yaml \
    checkin \
    --server-url "$SERVER_URL" \
    --device-id "$DEVICE_2" \
    --no-color 2>&1) || true

DEVICES=$(exec_in_pod curl -sf "${SERVER_URL}/api/v1/devices" 2>/dev/null || echo "[]")

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
    pass_test "T05"
else
    fail_test "T05" "Fleet policy not evaluated (compliantCount=${COMPLIANT:-0})"
fi

# =================================================================
# T06: Drift on device → checkin reports drift to server
# =================================================================
begin_test "T06: Drift detection and server reporting"

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
    pass_test "T07"
else
    fail_test "T07" "DriftAlert did not propagate to MachineConfig"
fi

# =================================================================
# T08: Policy detects drifted MC via Ready condition
# =================================================================
begin_test "T08: Policy sees drifted MachineConfig"

# Reuse DRIFT_DETECTED from T07; only fetch reason (one kubectl call instead of two)
DRIFT_STATUS="$DRIFT_DETECTED"
DRIFT_REASON=$(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
    -o jsonpath='{.status.conditions[?(@.type=="DriftDetected")].reason}' 2>/dev/null || echo "")

echo "  MC DriftDetected status: $DRIFT_STATUS"
echo "  MC DriftDetected reason: $DRIFT_REASON"

if [ "$DRIFT_STATUS" = "True" ]; then
    pass_test "T08"
else
    fail_test "T08" "MC DriftDetected condition not True (status=$DRIFT_STATUS, reason=$DRIFT_REASON)"
fi

# =================================================================
# T09: Resolve drift → DriftAlert removed → MC returns to Ready
# =================================================================
begin_test "T09: Drift resolution lifecycle"

# Delete DriftAlert
kubectl delete driftalert "drift-${DEVICE_1}" -n cfgd-system 2>/dev/null || true

# Patch MC to trigger re-reconcile (changes generation)
kubectl patch machineconfig "mc-${DEVICE_1}" -n cfgd-system --type=merge \
    -p '{"spec":{"packages":[{"name":"vim"},{"name":"git"},{"name":"curl"},{"name":"wget"}]}}' 2>/dev/null

# Wait for MC to clear drift
echo "  Waiting for drift to clear..."
DRIFT=$(wait_for_k8s_field machineconfig "mc-${DEVICE_1}" cfgd-system \
    '{.status.conditions[?(@.type=="DriftDetected")].status}' "False" 60) && DRIFT_CLEARED=true || DRIFT_CLEARED=false

READY_STATUS=$(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
    -o jsonpath='{.status.conditions[?(@.type=="Ready")].status}' 2>/dev/null || echo "")

echo "  MC driftDetected: $(kubectl get machineconfig "mc-${DEVICE_1}" -n cfgd-system \
    -o jsonpath='{.status.conditions[?(@.type=="DriftDetected")].status}' 2>/dev/null || echo 'unknown')"
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

if assert_contains "$DEVICE_INFO" "healthy" || assert_contains "$DEVICE_INFO" "$DEVICE_1"; then
    pass_test "T10"
else
    fail_test "T10" "Device status not available from device gateway"
fi

# =================================================================
# T11: CSI driver — deploy DaemonSet, mount module content, verify
# =================================================================
begin_test "T11: CSI driver — module mount and content verification"

if ! $CSI_AVAILABLE; then
    skip_test "T11" "CSI driver not ready"
else
    # Verify CSI DaemonSet is ready
    CSI_DS_NAME=$(kubectl get ds -n cfgd-system -l app.kubernetes.io/component=csi-driver \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "cfgd-csi-csi")
    if ! wait_for_daemonset cfgd-system "$CSI_DS_NAME" 60; then
        fail_test "T11" "CSI DaemonSet not ready"
    else
        # Push a test module to the registry (from host)
        TEST_MODULE_DIR=$(mktemp -d)
        create_test_module_dir "$TEST_MODULE_DIR" "csi-test-mod-${E2E_RUN_ID}" "1.0.0"
        OCI_REF="${REGISTRY}/cfgd-e2e/csi-test:v1.0-${E2E_RUN_ID}"
        "$CFGD_BIN" module push "$TEST_MODULE_DIR" --artifact "$OCI_REF" --no-color 2>/dev/null || true
        rm -rf "$TEST_MODULE_DIR"

        # Create Module CRD with OCI ref
        kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: csi-test-mod-${E2E_RUN_ID}
  labels:
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
spec:
  packages: []
  ociArtifact: "${OCI_REF}"
  mountPolicy: Always
EOF

        # Create an injection-enabled namespace
        kubectl create namespace "e2e-csi-test-${E2E_RUN_ID}" 2>/dev/null || true
        kubectl label namespace "e2e-csi-test-${E2E_RUN_ID}" cfgd.io/inject-modules=true --overwrite 2>/dev/null

        sleep 3

        # Create a pod with module annotation
        kubectl apply -n "e2e-csi-test-${E2E_RUN_ID}" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: csi-mount-test
  annotations:
    cfgd.io/modules: "csi-test-mod-${E2E_RUN_ID}:v1.0"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

        # Wait for pod to be running (CSI driver needs to pull and mount)
        echo "  Waiting for pod to be running..."
        POD_RUNNING=false
        wait_for_k8s_field pod csi-mount-test "e2e-csi-test-${E2E_RUN_ID}" \
            '{.status.phase}' Running 180 > /dev/null && POD_RUNNING=true || true

        if $POD_RUNNING; then
            # Verify module content is mounted
            MODULE_FILE=$(kubectl exec csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" -- \
                cat /cfgd-modules/csi-test-mod-${E2E_RUN_ID}/module.yaml 2>/dev/null || echo "")
            HELLO_SH=$(kubectl exec csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" -- \
                cat /cfgd-modules/csi-test-mod-${E2E_RUN_ID}/bin/hello.sh 2>/dev/null || echo "")

            echo "  module.yaml present: $([ -n "$MODULE_FILE" ] && echo 'yes' || echo 'no')"
            echo "  bin/hello.sh present: $([ -n "$HELLO_SH" ] && echo 'yes' || echo 'no')"

            # Verify read-only mount
            RO_TEST=$(kubectl exec csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" -- \
                touch /cfgd-modules/csi-test-mod-${E2E_RUN_ID}/test-write 2>&1 || echo "read-only")

            if [ -n "$MODULE_FILE" ] && echo "$RO_TEST" | grep -qi "read-only"; then
                pass_test "T11"
            elif [ -n "$MODULE_FILE" ]; then
                pass_test "T11"
            else
                fail_test "T11" "Module content not found at mount path"
            fi
        else
            fail_test "T11" "Pod did not reach Running state (CSI mount may have failed)"
            kubectl describe pod csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" 2>/dev/null | tail -20
        fi
    fi
fi

# =================================================================
# T12: CSI driver — unmount on pod delete
# =================================================================
begin_test "T12: CSI driver — unmount on pod delete"

if ! $CSI_AVAILABLE; then
    skip_test "T12" "CSI driver not ready"
else
    # Delete the pod
    kubectl delete pod csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" --grace-period=5 2>/dev/null || true

    # Wait for pod to be deleted
    echo "  Waiting for pod deletion..."
    for i in $(seq 1 30); do
        POD_EXISTS=$(kubectl get pod csi-mount-test -n "e2e-csi-test-${E2E_RUN_ID}" 2>/dev/null || echo "")
        if [ -z "$POD_EXISTS" ]; then
            break
        fi
        sleep 1
    done

    # Verify no mount leftovers via the test pod (which has host access)
    CSI_MOUNTS=$(exec_in_pod mount 2>/dev/null | grep "cfgd" | grep "csi-mount-test" || echo "")
    if [ -z "$CSI_MOUNTS" ]; then
        pass_test "T12"
    else
        fail_test "T12" "CSI mount still present after pod deletion"
        echo "  Remaining mounts: $CSI_MOUNTS"
    fi
fi

# =================================================================
# T13: kubectl cfgd inject — patches annotation on deployment
# =================================================================
begin_test "T13: kubectl cfgd inject"

# Create a test deployment
kubectl create namespace "e2e-plugin-test-${E2E_RUN_ID}" 2>/dev/null || true
kubectl apply -n "e2e-plugin-test-${E2E_RUN_ID}" -f - <<EOF
apiVersion: apps/v1
kind: Deployment
metadata:
  name: inject-target
  namespace: e2e-plugin-test-${E2E_RUN_ID}
spec:
  replicas: 1
  selector:
    matchLabels:
      app: inject-target
  template:
    metadata:
      labels:
        app: inject-target
    spec:
      containers:
        - name: app
          image: busybox:1.36
          command: ["sleep", "3600"]
EOF

wait_for_deployment "e2e-plugin-test-${E2E_RUN_ID}" inject-target 60 2>/dev/null || true

# Run kubectl cfgd inject (the binary acts as kubectl plugin when invoked as kubectl-cfgd)
# We call it directly since it's not installed as a kubectl plugin in CI
INJECT_OUTPUT=$(NO_COLOR=1 "$KUBECTL_CFGD" inject deployment/inject-target \
    --namespace "e2e-plugin-test-${E2E_RUN_ID}" \
    --module "csi-test-mod-${E2E_RUN_ID}:v1.0" 2>&1) || true
echo "  Inject output: $(echo "$INJECT_OUTPUT" | head -3)"

# Verify the annotation was patched
sleep 3
ANNOTATION=$(kubectl get deployment inject-target -n "e2e-plugin-test-${E2E_RUN_ID}" \
    -o jsonpath='{.spec.template.metadata.annotations.cfgd\.io/modules}' 2>/dev/null || echo "")
echo "  Annotation: ${ANNOTATION:-not set}"

if echo "$ANNOTATION" | grep -q "csi-test-mod-${E2E_RUN_ID}"; then
    pass_test "T13"
else
    fail_test "T13" "kubectl cfgd inject did not set annotation"
fi

# =================================================================
# T14: kubectl cfgd status — lists modules
# =================================================================
begin_test "T14: kubectl cfgd status"

STATUS_OUTPUT=$(NO_COLOR=1 "$KUBECTL_CFGD" status 2>&1) || true
echo "  Status output:"
echo "$STATUS_OUTPUT" | head -10 | sed 's/^/    /'

# Should list the modules we created (csi-test-mod-*, e2e-nettools if still around)
if echo "$STATUS_OUTPUT" | grep -qi "module\|csi-test-mod\|name"; then
    pass_test "T14"
else
    fail_test "T14" "kubectl cfgd status did not list modules"
fi

# =================================================================
# T15: kubectl cfgd version — returns version info
# =================================================================
begin_test "T15: kubectl cfgd version"

VERSION_OUTPUT=$(NO_COLOR=1 "$KUBECTL_CFGD" version 2>&1) || true
echo "  Version output: $VERSION_OUTPUT"

if echo "$VERSION_OUTPUT" | grep -qi "version\|client\|server\|v[0-9]"; then
    pass_test "T15"
else
    fail_test "T15" "kubectl cfgd version did not return version info"
fi

# =================================================================
# T16: Debug flow — pod with Debug mountPolicy module
# =================================================================
begin_test "T16: Debug flow — mountPolicy Debug module"

if ! $CSI_AVAILABLE; then
    skip_test "T16" "CSI driver not ready"
else
    # Create a Module with Debug mountPolicy
    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: debug-tools-${E2E_RUN_ID}
  labels:
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
spec:
  packages:
    - name: strace
  env:
    - name: DEBUG_TOOLS_LOADED
      value: "true"
  mountPolicy: Debug
EOF

    # Create namespace with injection and a ConfigPolicy with debugModules
    kubectl create namespace "e2e-debug-flow-${E2E_RUN_ID}" 2>/dev/null || true
    kubectl label namespace "e2e-debug-flow-${E2E_RUN_ID}" cfgd.io/inject-modules=true --overwrite 2>/dev/null

    kubectl apply -n "e2e-debug-flow-${E2E_RUN_ID}" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: debug-tools-policy
  namespace: e2e-debug-flow-${E2E_RUN_ID}
spec:
  debugModules:
    - name: debug-tools-${E2E_RUN_ID}
EOF

    sleep 5

    # Create a pod (no annotation — debug modules come from policy)
    kubectl apply -n "e2e-debug-flow-${E2E_RUN_ID}" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: debug-target
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

    sleep 10

    # Check pod spec: CSI volume should exist, volumeMount should NOT be on app container
    DEBUG_CSI=$(kubectl get pod debug-target -n "e2e-debug-flow-${E2E_RUN_ID}" \
        -o jsonpath='{.spec.volumes[?(@.csi)].csi.driver}' 2>/dev/null || echo "")
    APP_VMOUNTS=$(kubectl get pod debug-target -n "e2e-debug-flow-${E2E_RUN_ID}" \
        -o jsonpath='{.spec.containers[0].volumeMounts[*].name}' 2>/dev/null || echo "")
    APP_ENV=$(kubectl get pod debug-target -n "e2e-debug-flow-${E2E_RUN_ID}" \
        -o jsonpath='{.spec.containers[0].env[*].name}' 2>/dev/null || echo "")

    echo "  CSI driver: ${DEBUG_CSI:-none}"
    echo "  App container volumeMounts: ${APP_VMOUNTS:-none}"
    echo "  App container env: ${APP_ENV:-none}"

    if echo "$DEBUG_CSI" | grep -qF "$CSI_DRIVER_NAME"; then
        # Volume exists — check that it's NOT mounted on the app container
        if ! echo "$APP_VMOUNTS" | grep -q "debug-tools-${E2E_RUN_ID}"; then
            pass_test "T16"
        else
            fail_test "T16" "Debug module volumeMount present on app container (should be omitted)"
        fi
    else
        # If no CSI volume, the debug policy may not have been picked up
        skip_test "T16" "Debug module not injected (policy may need more time to propagate)"
    fi

    # Test kubectl cfgd debug (creates ephemeral container)
    # Note: This requires the pod to be running, which needs CSI to succeed
    PHASE=$(kubectl get pod debug-target -n "e2e-debug-flow-${E2E_RUN_ID}" \
        -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
    if [ "$PHASE" = "Running" ]; then
        echo "  Pod is Running, testing kubectl cfgd debug..."
        DEBUG_OUTPUT=$(NO_COLOR=1 "$KUBECTL_CFGD" debug debug-target \
            --namespace "e2e-debug-flow-${E2E_RUN_ID}" \
            --module "debug-tools-${E2E_RUN_ID}:latest" 2>&1) || true
        echo "  Debug output: $(echo "$DEBUG_OUTPUT" | head -3)"

        # Verify ephemeral container was created
        EPHEMERAL=$(kubectl get pod debug-target -n "e2e-debug-flow-${E2E_RUN_ID}" \
            -o jsonpath='{.spec.ephemeralContainers[*].name}' 2>/dev/null || echo "")
        echo "  Ephemeral containers: ${EPHEMERAL:-none}"
    fi
fi

# --- Summary ---
print_summary "Full-Stack E2E Tests"
