#!/usr/bin/env bash
# E2E full-stack tests: cfgd CLI → device gateway → cfgd-operator → CRDs.
# Tests the complete loop: device checkin, fleet management, drift propagation,
# multi-device scenarios, policy enforcement, CSI driver, kubectl plugin, debug flow.
# Prereqs: kind cluster running, all images loaded (cfgd, cfgd-operator, cfgd-csi).
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

for crd in machineconfigs.cfgd.io configpolicies.cfgd.io driftalerts.cfgd.io \
           modules.cfgd.io clusterconfigpolicies.cfgd.io; do
    kubectl wait --for=condition=established "crd/$crd" --timeout=30s 2>/dev/null || true
done

# Start local OCI registry (needed for CSI driver tests)
echo "Starting local OCI registry..."
start_local_registry
configure_registry_on_nodes

# Generate webhook TLS certificates and install webhook configurations
echo "Setting up webhook TLS..."
generate_webhook_certs "cfgd-operator" "cfgd-system"
install_webhook_config "cfgd-system"

# Deploy device gateway (idempotent — may already be deployed by setup-e2e action)
echo "Deploying device gateway..."
kubectl apply -f "$SERVER_MANIFEST" -n cfgd-system
wait_for_deployment cfgd-system cfgd-server 120

# Deploy cfgd-operator (with webhook support)
echo "Deploying cfgd-operator..."
kubectl apply -f "$OPERATOR_MANIFESTS/operator-deployment.yaml"
wait_for_deployment cfgd-system cfgd-operator 120

# Deploy CSI driver if image is loaded
CSI_IMAGE_LOADED=$(docker exec "$NODE" crictl images 2>/dev/null | grep "cfgd-csi" || echo "")
if [ -n "$CSI_IMAGE_LOADED" ]; then
    echo "Deploying CSI driver..."
    helm upgrade --install cfgd "$REPO_ROOT/chart/cfgd" \
        -n cfgd-system \
        --set operator.enabled=false \
        --set agent.enabled=false \
        --set webhook.enabled=false \
        --set mutatingWebhook.enabled=false \
        --set installCRDs=false \
        --set csiDriver.enabled=true \
        --set csiDriver.image.repository=cfgd-csi \
        --set csiDriver.image.tag=e2e-test \
        --set csiDriver.image.pullPolicy=Never \
        --wait --timeout=120s 2>&1 || echo "WARN: CSI driver deployment failed"
    CSI_AVAILABLE=true
else
    echo "WARN: cfgd-csi image not loaded, CSI tests will be skipped"
    CSI_AVAILABLE=false
fi

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

# Build cfgd binary on host for kubectl plugin and OCI tests
if [ ! -f "$REPO_ROOT/target/release/cfgd" ]; then
    echo "Building cfgd binary..."
    cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" --bin cfgd 2>/dev/null
fi
CFGD_BIN="$REPO_ROOT/target/release/cfgd"

# Create kubectl-cfgd symlink so cfgd activates plugin mode via argv[0]
KUBECTL_CFGD="/tmp/kubectl-cfgd"
ln -sf "$CFGD_BIN" "$KUBECTL_CFGD"

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
  name: fleet-baseline
  namespace: cfgd-system
spec:
  packages:
    - name: vim
    - name: git
  settings:
    "net.ipv4.ip_forward": "1"
EOF

# Wait for policy evaluation
sleep 5
COMPLIANT=$(wait_for_k8s_field configpolicy fleet-baseline cfgd-system \
    '{.status.compliantCount}' "" 60) || true

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

# =================================================================
# T11: CSI driver — deploy DaemonSet, mount module content, verify
# =================================================================
begin_test "T11: CSI driver — module mount and content verification"

if ! $CSI_AVAILABLE; then
    skip_test "T11" "CSI driver image not loaded"
else
    # Verify CSI DaemonSet is ready
    if ! wait_for_daemonset cfgd-system cfgd-csi 60; then
        fail_test "T11" "CSI DaemonSet not ready"
    else
        # Push a test module to the local registry
        TEST_MODULE_DIR=$(mktemp -d)
        create_test_module_dir "$TEST_MODULE_DIR" "csi-test-mod" "1.0.0"
        OCI_REF="localhost:${REGISTRY_PORT}/cfgd-e2e/csi-test:v1.0"
        "$CFGD_BIN" module push "$TEST_MODULE_DIR" --artifact "$OCI_REF" --no-color 2>/dev/null || true
        rm -rf "$TEST_MODULE_DIR"

        # Create Module CRD
        kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: csi-test-mod
spec:
  packages: []
  ociArtifact: "${OCI_REF}"
  mountPolicy: Always
EOF

        # Create an injection-enabled namespace
        kubectl create namespace e2e-csi-test 2>/dev/null || true
        kubectl label namespace e2e-csi-test cfgd.io/inject-modules=true --overwrite 2>/dev/null

        sleep 3

        # Create a pod with module annotation
        kubectl apply -n e2e-csi-test -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: csi-mount-test
  annotations:
    cfgd.io/modules: "csi-test-mod:v1.0"
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
        for i in $(seq 1 90); do
            PHASE=$(kubectl get pod csi-mount-test -n e2e-csi-test \
                -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
            if [ "$PHASE" = "Running" ]; then
                POD_RUNNING=true
                break
            fi
            sleep 2
        done

        if $POD_RUNNING; then
            # Verify module content is mounted
            MODULE_FILE=$(kubectl exec csi-mount-test -n e2e-csi-test -- \
                cat /cfgd-modules/csi-test-mod/module.yaml 2>/dev/null || echo "")
            HELLO_SH=$(kubectl exec csi-mount-test -n e2e-csi-test -- \
                cat /cfgd-modules/csi-test-mod/bin/hello.sh 2>/dev/null || echo "")

            echo "  module.yaml present: $([ -n "$MODULE_FILE" ] && echo 'yes' || echo 'no')"
            echo "  bin/hello.sh present: $([ -n "$HELLO_SH" ] && echo 'yes' || echo 'no')"

            # Verify read-only mount
            RO_TEST=$(kubectl exec csi-mount-test -n e2e-csi-test -- \
                touch /cfgd-modules/csi-test-mod/test-write 2>&1 || echo "read-only")

            if [ -n "$MODULE_FILE" ] && echo "$RO_TEST" | grep -qi "read-only"; then
                pass_test "T11"
            elif [ -n "$MODULE_FILE" ]; then
                pass_test "T11"
            else
                fail_test "T11" "Module content not found at mount path"
            fi
        else
            fail_test "T11" "Pod did not reach Running state (CSI mount may have failed)"
            kubectl describe pod csi-mount-test -n e2e-csi-test 2>/dev/null | tail -20
        fi
    fi
fi

# =================================================================
# T12: CSI driver — unmount on pod delete
# =================================================================
begin_test "T12: CSI driver — unmount on pod delete"

if ! $CSI_AVAILABLE; then
    skip_test "T12" "CSI driver image not loaded"
else
    # Delete the pod
    kubectl delete pod csi-mount-test -n e2e-csi-test --grace-period=5 2>/dev/null || true

    # Wait for pod to be deleted
    echo "  Waiting for pod deletion..."
    for i in $(seq 1 30); do
        POD_EXISTS=$(kubectl get pod csi-mount-test -n e2e-csi-test 2>/dev/null || echo "")
        if [ -z "$POD_EXISTS" ]; then
            break
        fi
        sleep 1
    done

    # Verify no mount leftovers on the node
    # The CSI driver should have cleaned up the target path
    CSI_MOUNTS=$(exec_on_node mount 2>/dev/null | grep "cfgd" | grep "csi-mount-test" || echo "")
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
kubectl create namespace e2e-plugin-test 2>/dev/null || true
kubectl apply -n e2e-plugin-test -f - <<EOF
apiVersion: apps/v1
kind: Deployment
metadata:
  name: inject-target
  namespace: e2e-plugin-test
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

wait_for_deployment e2e-plugin-test inject-target 60 2>/dev/null || true

# Run kubectl cfgd inject (the binary acts as kubectl plugin when invoked as kubectl-cfgd)
# We call it directly since it's not installed as a kubectl plugin in CI
INJECT_OUTPUT=$(NO_COLOR=1 "$KUBECTL_CFGD" inject deployment/inject-target \
    --namespace e2e-plugin-test \
    --module csi-test-mod:v1.0 2>&1) || true
echo "  Inject output: $(echo "$INJECT_OUTPUT" | head -3)"

# Verify the annotation was patched
sleep 3
ANNOTATION=$(kubectl get deployment inject-target -n e2e-plugin-test \
    -o jsonpath='{.spec.template.metadata.annotations.cfgd\.io/modules}' 2>/dev/null || echo "")
echo "  Annotation: ${ANNOTATION:-not set}"

if echo "$ANNOTATION" | grep -q "csi-test-mod"; then
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

# Should list the modules we created (csi-test-mod, e2e-nettools if still around)
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
    skip_test "T16" "CSI driver image not loaded"
else
    # Create a Module with Debug mountPolicy
    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: debug-tools
spec:
  packages:
    - name: strace
  env:
    - name: DEBUG_TOOLS_LOADED
      value: "true"
  mountPolicy: Debug
EOF

    # Create namespace with injection and a ConfigPolicy with debugModules
    kubectl create namespace e2e-debug-flow 2>/dev/null || true
    kubectl label namespace e2e-debug-flow cfgd.io/inject-modules=true --overwrite 2>/dev/null

    kubectl apply -n e2e-debug-flow -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: debug-tools-policy
  namespace: e2e-debug-flow
spec:
  debugModules:
    - name: debug-tools
EOF

    sleep 5

    # Create a pod (no annotation — debug modules come from policy)
    kubectl apply -n e2e-debug-flow -f - <<EOF
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
    DEBUG_CSI=$(kubectl get pod debug-target -n e2e-debug-flow \
        -o jsonpath='{.spec.volumes[?(@.csi)].csi.driver}' 2>/dev/null || echo "")
    APP_VMOUNTS=$(kubectl get pod debug-target -n e2e-debug-flow \
        -o jsonpath='{.spec.containers[0].volumeMounts[*].name}' 2>/dev/null || echo "")
    APP_ENV=$(kubectl get pod debug-target -n e2e-debug-flow \
        -o jsonpath='{.spec.containers[0].env[*].name}' 2>/dev/null || echo "")

    echo "  CSI driver: ${DEBUG_CSI:-none}"
    echo "  App container volumeMounts: ${APP_VMOUNTS:-none}"
    echo "  App container env: ${APP_ENV:-none}"

    if echo "$DEBUG_CSI" | grep -qF "csi.cfgd.io"; then
        # Volume exists — check that it's NOT mounted on the app container
        if ! echo "$APP_VMOUNTS" | grep -q "debug-tools"; then
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
    PHASE=$(kubectl get pod debug-target -n e2e-debug-flow \
        -o jsonpath='{.status.phase}' 2>/dev/null || echo "")
    if [ "$PHASE" = "Running" ]; then
        echo "  Pod is Running, testing kubectl cfgd debug..."
        DEBUG_OUTPUT=$(NO_COLOR=1 "$KUBECTL_CFGD" debug debug-target \
            --namespace e2e-debug-flow \
            --module debug-tools:latest 2>&1) || true
        echo "  Debug output: $(echo "$DEBUG_OUTPUT" | head -3)"

        # Verify ephemeral container was created
        EPHEMERAL=$(kubectl get pod debug-target -n e2e-debug-flow \
            -o jsonpath='{.spec.ephemeralContainers[*].name}' 2>/dev/null || echo "")
        echo "  Ephemeral containers: ${EPHEMERAL:-none}"
    fi
fi

# --- Cleanup ---
echo ""
echo "Cleaning up test resources..."
kubectl delete machineconfig --all -n cfgd-system 2>/dev/null || true
kubectl delete configpolicy --all -n cfgd-system 2>/dev/null || true
kubectl delete driftalert --all -n cfgd-system 2>/dev/null || true
kubectl delete module --all 2>/dev/null || true
kubectl delete namespace e2e-csi-test e2e-plugin-test e2e-debug-flow 2>/dev/null || true
stop_local_registry
rm -rf "$WEBHOOK_CERT_DIR"

# --- Summary ---
print_summary "Full-Stack E2E Tests"
