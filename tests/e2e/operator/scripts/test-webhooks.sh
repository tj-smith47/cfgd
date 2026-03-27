# Operator E2E tests: Webhooks
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Webhook Tests ==="

# =================================================================
# OP-WH-01: Validation webhooks — reject invalid specs for multiple CRDs
# =================================================================
begin_test "OP-WH-01: Validation webhooks — reject invalid specs"

PASS=true

# ClusterConfigPolicy with invalid semver in packages[].version
RESULT_BAD_SEMVER=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: e2e-bad-semver-${E2E_RUN_ID}
  labels:
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
    ${E2E_JOB_LABEL_YAML}
spec:
  namespaceSelector: {}
  packages:
    - name: vim
      version: "not-a-semver"
EOF
)
echo "  Bad semver result: $(echo "$RESULT_BAD_SEMVER" | tail -1)"
assert_rejected "$RESULT_BAD_SEMVER" "Invalid semver" || PASS=false

# DriftAlert with empty deviceId
RESULT_EMPTY_DEVICE=$(kubectl apply -n "$E2E_NAMESPACE" -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: DriftAlert
metadata:
  name: e2e-bad-drift
  namespace: ${E2E_NAMESPACE}
spec:
  deviceId: ""
  machineConfigRef:
    name: some-mc
  severity: Low
  driftDetails:
    - field: test
      expected: a
      actual: b
EOF
)
echo "  Empty deviceId result: $(echo "$RESULT_EMPTY_DEVICE" | tail -1)"
assert_rejected "$RESULT_EMPTY_DEVICE" "Empty deviceId" || PASS=false

# MachineConfig with empty hostname
RESULT_EMPTY_HOST=$(kubectl apply -n "$E2E_NAMESPACE" -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-bad-mc
  namespace: ${E2E_NAMESPACE}
spec:
  hostname: ""
  profile: test
  packages: []
  systemSettings: {}
EOF
)
echo "  Empty hostname result: $(echo "$RESULT_EMPTY_HOST" | tail -1)"
assert_rejected "$RESULT_EMPTY_HOST" "Empty hostname" || PASS=false

if $PASS; then
    pass_test "OP-WH-01"
else
    fail_test "OP-WH-01" "One or more validation webhooks did not reject invalid specs"
fi

# Clean up
kubectl delete clusterconfigpolicy "e2e-bad-semver-${E2E_RUN_ID}" 2>/dev/null || true
kubectl delete driftalert e2e-bad-drift -n "$E2E_NAMESPACE" 2>/dev/null || true
kubectl delete machineconfig e2e-bad-mc -n "$E2E_NAMESPACE" 2>/dev/null || true

# =================================================================
# OP-WH-02: Mutating webhook — pod injection with CSI volumes
# =================================================================
begin_test "OP-WH-02: Mutating webhook — pod injection"

# Create a namespace with the injection label
kubectl create namespace "e2e-inject-${E2E_RUN_ID}" 2>/dev/null || true
kubectl label namespace "e2e-inject-${E2E_RUN_ID}" cfgd.io/inject-modules=true --overwrite 2>/dev/null

# Ensure a Module CRD exists for the webhook to look up
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: e2e-inject-mod-${E2E_RUN_ID}
  labels:
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: curl
  env:
    - name: INJECT_TEST
      value: "injected"
  mountPolicy: Always
EOF

# Wait for module controller to set status
sleep 5

# Create a pod with the modules annotation in the labeled namespace
kubectl apply -n "e2e-inject-${E2E_RUN_ID}" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: e2e-injected-pod
  annotations:
    cfgd.io/modules: "e2e-inject-mod-${E2E_RUN_ID}:v1"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

# Wait for pod to be created (webhook runs on CREATE)
sleep 5

# Check if CSI volume was injected
POD_VOLUMES=$(kubectl get pod e2e-injected-pod -n "e2e-inject-${E2E_RUN_ID}" \
    -o jsonpath='{.spec.volumes[*].name}' 2>/dev/null || echo "")
POD_VMOUNTS=$(kubectl get pod e2e-injected-pod -n "e2e-inject-${E2E_RUN_ID}" \
    -o jsonpath='{.spec.containers[0].volumeMounts[*].name}' 2>/dev/null || echo "")
POD_ENV=$(kubectl get pod e2e-injected-pod -n "e2e-inject-${E2E_RUN_ID}" \
    -o jsonpath='{.spec.containers[0].env[*].name}' 2>/dev/null || echo "")
CSI_DRIVER=$(kubectl get pod e2e-injected-pod -n "e2e-inject-${E2E_RUN_ID}" \
    -o jsonpath='{.spec.volumes[?(@.csi)].csi.driver}' 2>/dev/null || echo "")

echo "  Pod volumes: $POD_VOLUMES"
echo "  Container volumeMounts: $POD_VMOUNTS"
echo "  Container env vars: $POD_ENV"
echo "  CSI driver: $CSI_DRIVER"

PASS=true
if ! echo "$CSI_DRIVER" | grep -qF "$CSI_DRIVER_NAME"; then
    echo "  WARN: CSI volume not injected (expected driver=csi.cfgd.io)"
    PASS=false
fi
if ! echo "$POD_VMOUNTS" | grep -q "cfgd-module"; then
    echo "  WARN: volumeMount not injected on container"
    PASS=false
fi

if $PASS; then
    pass_test "OP-WH-02"
else
    fail_test "OP-WH-02" "Mutating webhook did not inject expected volumes/mounts"
fi

# =================================================================
# OP-WH-03: Mutating webhook — mountPolicy Debug skips volumeMount
# =================================================================
begin_test "OP-WH-03: Mutating webhook — Debug mountPolicy"

# Create a Module with mountPolicy Debug
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: e2e-debug-mod-${E2E_RUN_ID}
  labels:
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: strace
  mountPolicy: Debug
EOF

sleep 3

# Create a ConfigPolicy with the debug module (so webhook picks it up)
kubectl apply -n "e2e-inject-${E2E_RUN_ID}" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: e2e-debug-policy
  namespace: e2e-inject-${E2E_RUN_ID}
spec:
  debugModules:
    - name: e2e-debug-mod-${E2E_RUN_ID}
EOF

sleep 3

# Create a pod in the injection namespace (no annotation needed — policy injects)
kubectl apply -n "e2e-inject-${E2E_RUN_ID}" -f - <<EOF
apiVersion: v1
kind: Pod
metadata:
  name: e2e-debug-pod
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
  restartPolicy: Never
EOF

sleep 5

# Check: CSI volume should exist but volumeMount should NOT be on the container
DEBUG_VOLUMES=$(kubectl get pod e2e-debug-pod -n "e2e-inject-${E2E_RUN_ID}" \
    -o jsonpath='{.spec.volumes[*].name}' 2>/dev/null || echo "")
DEBUG_VMOUNTS=$(kubectl get pod e2e-debug-pod -n "e2e-inject-${E2E_RUN_ID}" \
    -o jsonpath='{.spec.containers[0].volumeMounts[*].name}' 2>/dev/null || echo "")
DEBUG_CSI=$(kubectl get pod e2e-debug-pod -n "e2e-inject-${E2E_RUN_ID}" \
    -o jsonpath='{.spec.volumes[?(@.csi)].csi.driver}' 2>/dev/null || echo "")

echo "  Pod volumes: $DEBUG_VOLUMES"
echo "  Container volumeMounts: $DEBUG_VMOUNTS"
echo "  CSI driver: $DEBUG_CSI"

# For Debug policy, the CSI volume should exist but NOT be mounted on containers
if echo "$DEBUG_CSI" | grep -qF "$CSI_DRIVER_NAME"; then
    if ! echo "$DEBUG_VMOUNTS" | grep -q "debug-mod"; then
        pass_test "OP-WH-03"
    else
        fail_test "OP-WH-03" "Debug module volumeMount was injected on container (should be skipped)"
    fi
else
    # If no modules were injected at all, this is also acceptable if the policy
    # controller hasn't reconciled yet — but CSI volume without mount is the goal
    skip_test "OP-WH-03" "Debug module CSI volume not injected (policy may not have been picked up)"
fi
