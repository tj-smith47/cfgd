# shellcheck shell=bash
# Full-stack E2E tests: Debug
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Debug Tests ==="

# =================================================================
# FS-DEBUG-01: Debug flow — pod with Debug mountPolicy module
# =================================================================
begin_test "FS-DEBUG-01: Debug flow — mountPolicy Debug module"

# Create a Module with Debug mountPolicy
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: debug-tools-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: strace
  env:
    - name: DEBUG_TOOLS_LOADED
      value: "true"
  mountPolicy: Debug
EOF

# Create namespace with injection and a ConfigPolicy with debugModules
ensure_namespace "e2e-debug-flow-${E2E_RUN_ID}"
ensure_label namespace "e2e-debug-flow-${E2E_RUN_ID}" "$E2E_RUN_LABEL" cfgd.io/inject-modules=true --overwrite

kubectl apply -n "e2e-debug-flow-${E2E_RUN_ID}" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: debug-tools-policy
  namespace: e2e-debug-flow-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
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

# Check pod spec: CSI volume should exist, volumeMount should NOT be on app container
DEBUG_CSI=$(wait_for_k8s_field pod debug-target "e2e-debug-flow-${E2E_RUN_ID}" \
    '{.spec.volumes[?(@.csi.driver=="'"$CSI_DRIVER_NAME"'")].csi.driver}' "$CSI_DRIVER_NAME" 30) || true
APP_VMOUNTS=$(kubectl get pod debug-target -n "e2e-debug-flow-${E2E_RUN_ID}" \
    -o jsonpath='{.spec.containers[0].volumeMounts[*].name}' 2>/dev/null || echo "")
APP_ENV=$(kubectl get pod debug-target -n "e2e-debug-flow-${E2E_RUN_ID}" \
    -o jsonpath='{.spec.containers[0].env[*].name}' 2>/dev/null || echo "")

echo "  CSI driver: ${DEBUG_CSI:-none}"
echo "  App container volumeMounts: ${APP_VMOUNTS:-none}"
echo "  App container env: ${APP_ENV:-none}"

if [ "$DEBUG_CSI" = "$CSI_DRIVER_NAME" ]; then
    # Volume exists — check that it's NOT mounted on the app container
    if ! echo "$APP_VMOUNTS" | grep -q "debug-tools-${E2E_RUN_ID}"; then
        pass_test "FS-DEBUG-01"
    else
        fail_test "FS-DEBUG-01" "Debug module volumeMount present on app container (should be omitted)"
    fi
else
    DEBUG_CSI=$(kubectl get pod debug-target -n "e2e-debug-flow-${E2E_RUN_ID}" \
        -o jsonpath='{.spec.volumes[*].csi.driver}' 2>/dev/null || echo "")
    fail_test "FS-DEBUG-01" "no $CSI_DRIVER_NAME volume injected for the Debug module: volumes=[$DEBUG_CSI]"
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
