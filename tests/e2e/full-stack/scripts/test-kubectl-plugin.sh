# Full-stack E2E tests: kubectl Plugin
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== kubectl Plugin Tests ==="

# =================================================================
# FS-PLUGIN-01: kubectl cfgd inject — patches annotation on deployment
# =================================================================
begin_test "FS-PLUGIN-01: kubectl cfgd inject"

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
    pass_test "FS-PLUGIN-01"
else
    fail_test "FS-PLUGIN-01" "kubectl cfgd inject did not set annotation"
fi

# =================================================================
# FS-PLUGIN-02: kubectl cfgd status — lists modules
# =================================================================
begin_test "FS-PLUGIN-02: kubectl cfgd status"

STATUS_OUTPUT=$(NO_COLOR=1 "$KUBECTL_CFGD" status 2>&1) || true
echo "  Status output:"
echo "$STATUS_OUTPUT" | head -10 | sed 's/^/    /'

# Should list the modules we created (csi-test-mod-*, e2e-nettools if still around)
if echo "$STATUS_OUTPUT" | grep -qi "module\|csi-test-mod\|name"; then
    pass_test "FS-PLUGIN-02"
else
    fail_test "FS-PLUGIN-02" "kubectl cfgd status did not list modules"
fi

# =================================================================
# FS-PLUGIN-03: kubectl cfgd version — returns version info
# =================================================================
begin_test "FS-PLUGIN-03: kubectl cfgd version"

VERSION_OUTPUT=$(NO_COLOR=1 "$KUBECTL_CFGD" version 2>&1) || true
echo "  Version output: $VERSION_OUTPUT"

if echo "$VERSION_OUTPUT" | grep -qi "version\|client\|server\|v[0-9]"; then
    pass_test "FS-PLUGIN-03"
else
    fail_test "FS-PLUGIN-03" "kubectl cfgd version did not return version info"
fi
