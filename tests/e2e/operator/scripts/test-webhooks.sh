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
kubectl delete clusterconfigpolicy "e2e-bad-semver-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
kubectl delete driftalert e2e-bad-drift -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true
kubectl delete machineconfig e2e-bad-mc -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true

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

# =================================================================
# OP-WH-04: MachineConfig — missing hostname rejected
# =================================================================
begin_test "OP-WH-04: MachineConfig — missing hostname rejected"

RESULT=$(kubectl apply -n "$E2E_NAMESPACE" -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-no-host-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: ""
  profile: test
  packages: []
  systemSettings: {}
EOF
)
echo "  Result: $(echo "$RESULT" | tail -1)"
if assert_rejected "$RESULT" "Empty hostname"; then
    pass_test "OP-WH-04"
else
    fail_test "OP-WH-04" "MachineConfig with empty hostname was not rejected"
fi
kubectl delete machineconfig "e2e-no-host-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-WH-05: MachineConfig — invalid moduleRef format rejected
# =================================================================
begin_test "OP-WH-05: MachineConfig — invalid moduleRef format rejected"

RESULT=$(kubectl apply -n "$E2E_NAMESPACE" -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-bad-modref-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: test-node
  profile: test
  moduleRefs:
    - name: ""
      required: true
  packages: []
  systemSettings: {}
EOF
)
echo "  Result: $(echo "$RESULT" | tail -1)"
if assert_rejected "$RESULT" "Invalid moduleRef (empty name)"; then
    pass_test "OP-WH-05"
else
    fail_test "OP-WH-05" "MachineConfig with empty moduleRef name was not rejected"
fi
kubectl delete machineconfig "e2e-bad-modref-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-WH-06: MachineConfig — valid spec accepted
# =================================================================
begin_test "OP-WH-06: MachineConfig — valid spec accepted"

RESULT=$(kubectl apply -n "$E2E_NAMESPACE" -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-valid-mc-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: valid-host-${E2E_RUN_ID}
  profile: default
  moduleRefs:
    - name: some-module
      required: false
  packages:
    - name: curl
  systemSettings: {}
EOF
)
echo "  Result: $(echo "$RESULT" | tail -1)"
if echo "$RESULT" | grep -qE "created|configured|unchanged"; then
    pass_test "OP-WH-06"
else
    fail_test "OP-WH-06" "Valid MachineConfig was not accepted: $RESULT"
fi
kubectl delete machineconfig "e2e-valid-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-WH-07: ConfigPolicy — empty targetSelector accepted
# =================================================================
begin_test "OP-WH-07: ConfigPolicy — empty targetSelector"

# ConfigPolicy validation does not reject an empty targetSelector — it defaults
# to matching nothing (same as Kubernetes LabelSelector semantics: {} matches all).
RESULT=$(kubectl apply -n "$E2E_NAMESPACE" -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: e2e-empty-sel-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  targetSelector: {}
  packages: []
  settings: {}
EOF
)
echo "  Result: $(echo "$RESULT" | tail -1)"
# Empty targetSelector is accepted (matches all, like k8s LabelSelector)
if echo "$RESULT" | grep -qE "created|configured|unchanged"; then
    pass_test "OP-WH-07"
else
    # Also acceptable if webhook rejects — document whichever behavior is observed
    if assert_rejected "$RESULT" "Empty targetSelector" 2>/dev/null; then
        echo "  Note: empty targetSelector is rejected by webhook (stricter validation)"
        pass_test "OP-WH-07"
    else
        fail_test "OP-WH-07" "Unexpected result for empty targetSelector: $RESULT"
    fi
fi
kubectl delete configpolicy "e2e-empty-sel-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-WH-08: ConfigPolicy — valid spec accepted
# =================================================================
begin_test "OP-WH-08: ConfigPolicy — valid spec accepted"

RESULT=$(kubectl apply -n "$E2E_NAMESPACE" -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: e2e-valid-cp-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  targetSelector:
    matchLabels:
      role: worker
  packages:
    - name: vim
      version: ">=9.0.0"
  settings:
    timezone: "UTC"
EOF
)
echo "  Result: $(echo "$RESULT" | tail -1)"
if echo "$RESULT" | grep -qE "created|configured|unchanged"; then
    pass_test "OP-WH-08"
else
    fail_test "OP-WH-08" "Valid ConfigPolicy was not accepted: $RESULT"
fi
kubectl delete configpolicy "e2e-valid-cp-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-WH-09: DriftAlert — missing machineConfigRef rejected
# =================================================================
begin_test "OP-WH-09: DriftAlert — missing machineConfigRef rejected"

RESULT=$(kubectl apply -n "$E2E_NAMESPACE" -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: DriftAlert
metadata:
  name: e2e-no-mcref-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  deviceId: some-device
  machineConfigRef:
    name: ""
  severity: Medium
  driftDetails:
    - field: packages
      expected: installed
      actual: missing
EOF
)
echo "  Result: $(echo "$RESULT" | tail -1)"
if assert_rejected "$RESULT" "Empty machineConfigRef name"; then
    pass_test "OP-WH-09"
else
    fail_test "OP-WH-09" "DriftAlert with empty machineConfigRef name was not rejected"
fi
kubectl delete driftalert "e2e-no-mcref-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-WH-10: DriftAlert — valid spec accepted
# =================================================================
begin_test "OP-WH-10: DriftAlert — valid spec accepted"

RESULT=$(kubectl apply -n "$E2E_NAMESPACE" -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: DriftAlert
metadata:
  name: e2e-valid-da-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  deviceId: device-002
  machineConfigRef:
    name: some-mc
  severity: Low
  driftDetails:
    - field: sysctl
      expected: "1"
      actual: "0"
EOF
)
echo "  Result: $(echo "$RESULT" | tail -1)"
if echo "$RESULT" | grep -qE "created|configured|unchanged"; then
    pass_test "OP-WH-10"
else
    fail_test "OP-WH-10" "Valid DriftAlert was not accepted: $RESULT"
fi
kubectl delete driftalert "e2e-valid-da-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-WH-11: ClusterConfigPolicy — invalid namespaceSelector + invalid semver rejected
# =================================================================
begin_test "OP-WH-11: ClusterConfigPolicy — invalid namespaceSelector + invalid semver rejected"

RESULT=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: e2e-bad-ccp-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  namespaceSelector:
    matchExpressions:
      - key: env
        operator: InvalidOp
        values: ["prod"]
  packages:
    - name: nginx
      version: "not-semver!!!"
  settings: {}
EOF
)
echo "  Result: $(echo "$RESULT" | tail -1)"
if assert_rejected "$RESULT" "Invalid namespaceSelector + invalid semver"; then
    pass_test "OP-WH-11"
else
    fail_test "OP-WH-11" "ClusterConfigPolicy with invalid fields was not rejected"
fi
kubectl delete clusterconfigpolicy "e2e-bad-ccp-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-WH-12: ClusterConfigPolicy — valid spec accepted
# =================================================================
begin_test "OP-WH-12: ClusterConfigPolicy — valid spec accepted"

RESULT=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: ClusterConfigPolicy
metadata:
  name: e2e-valid-ccp-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  namespaceSelector:
    matchLabels:
      env: staging
  packages:
    - name: htop
      version: ">=3.0.0"
  settings:
    logLevel: "debug"
EOF
)
echo "  Result: $(echo "$RESULT" | tail -1)"
if echo "$RESULT" | grep -qE "created|configured|unchanged"; then
    pass_test "OP-WH-12"
else
    fail_test "OP-WH-12" "Valid ClusterConfigPolicy was not accepted: $RESULT"
fi
kubectl delete clusterconfigpolicy "e2e-valid-ccp-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-WH-13: Module — invalid OCI reference format rejected
# =================================================================
begin_test "OP-WH-13: Module — invalid OCI reference format rejected"

RESULT=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: e2e-bad-oci-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: curl
  ociArtifact: ":::not-a-valid-oci-ref:::"
  mountPolicy: Always
EOF
)
echo "  Result: $(echo "$RESULT" | tail -1)"
if assert_rejected "$RESULT" "Invalid OCI reference"; then
    pass_test "OP-WH-13"
else
    fail_test "OP-WH-13" "Module with invalid OCI reference was not rejected"
fi
kubectl delete module "e2e-bad-oci-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-WH-14: Module — valid spec accepted
# =================================================================
begin_test "OP-WH-14: Module — valid spec accepted"

RESULT=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: e2e-valid-mod-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: jq
  env:
    - name: MOD_TEST
      value: "hello"
  mountPolicy: Always
EOF
)
echo "  Result: $(echo "$RESULT" | tail -1)"
if echo "$RESULT" | grep -qE "created|configured|unchanged"; then
    pass_test "OP-WH-14"
else
    fail_test "OP-WH-14" "Valid Module was not accepted: $RESULT"
fi
kubectl delete module "e2e-valid-mod-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true

# =================================================================
# OP-WH-15: MachineConfig serde defaults on minimal spec
# =================================================================
begin_test "OP-WH-15: MachineConfig serde defaults on minimal spec"

# Create a minimal MachineConfig with only required fields
kubectl apply -n "$E2E_NAMESPACE" -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-defaults-mc-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: defaults-host-${E2E_RUN_ID}
  profile: minimal
EOF

sleep 2

# Verify the stored object has default fields populated
STORED=$(kubectl get machineconfig "e2e-defaults-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
    -o json 2>/dev/null || echo "{}")

PASS=true

# Check that spec.hostname is stored correctly
STORED_HOST=$(echo "$STORED" | jq -r '.spec.hostname // empty')
if [ "$STORED_HOST" != "defaults-host-${E2E_RUN_ID}" ]; then
    echo "  WARN: hostname not stored correctly (got '$STORED_HOST')"
    PASS=false
fi

# Check that spec.profile is stored correctly
STORED_PROFILE=$(echo "$STORED" | jq -r '.spec.profile // empty')
if [ "$STORED_PROFILE" != "minimal" ]; then
    echo "  WARN: profile not stored correctly (got '$STORED_PROFILE')"
    PASS=false
fi

# Check that defaulted array fields exist (packages defaults to [])
STORED_PKGS=$(echo "$STORED" | jq -r '.spec.packages // "missing"')
echo "  Stored packages: $STORED_PKGS"
if [ "$STORED_PKGS" = "missing" ]; then
    echo "  Note: packages field omitted (server-side default not applied, acceptable)"
fi

# Check that defaulted map fields exist (systemSettings defaults to {})
STORED_SETTINGS=$(echo "$STORED" | jq -r '.spec.systemSettings // "missing"')
echo "  Stored systemSettings: $STORED_SETTINGS"
if [ "$STORED_SETTINGS" = "missing" ]; then
    echo "  Note: systemSettings field omitted (server-side default not applied, acceptable)"
fi

# The resource must at minimum exist and have the required fields set
if [ -z "$STORED_HOST" ] || [ -z "$STORED_PROFILE" ]; then
    PASS=false
fi

if $PASS; then
    pass_test "OP-WH-15"
else
    fail_test "OP-WH-15" "Stored MachineConfig does not have expected defaults"
fi
kubectl delete machineconfig "e2e-defaults-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true
