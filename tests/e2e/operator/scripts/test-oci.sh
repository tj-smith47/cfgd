# Operator E2E tests: OCI
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== OCI Tests ==="

# =================================================================
# OP-OCI-01: OCI supply chain — push, pull, verify content integrity
# =================================================================
begin_test "OP-OCI-01: OCI supply chain — push, pull, verify"

# Create a test module directory
TEST_MODULE_DIR=$(mktemp -d)
create_test_module_dir "$TEST_MODULE_DIR" "e2e-oci-test" "1.0.0"

OCI_REF="${REGISTRY}/cfgd-e2e/oci-test:v1.0"

ensure_cfgd_binary

# Push module to local registry
echo "  Pushing module to ${OCI_REF}..."
PUSH_OUTPUT=$("$CFGD_BIN" module push "$TEST_MODULE_DIR" --artifact "$OCI_REF" --no-color 2>&1) || true
echo "  Push output: $(echo "$PUSH_OUTPUT" | head -3)"

# Pull module back
PULL_DIR=$(mktemp -d)
echo "  Pulling module from ${OCI_REF}..."
PULL_OUTPUT=$("$CFGD_BIN" module pull "$OCI_REF" --dir "$PULL_DIR" --no-color 2>&1) || true
echo "  Pull output: $(echo "$PULL_OUTPUT" | head -3)"

# Verify content integrity
PASS=true
if [ -f "$PULL_DIR/module.yaml" ]; then
    echo "  module.yaml present: yes"
    if grep -q "e2e-oci-test" "$PULL_DIR/module.yaml"; then
        echo "  module.yaml content: correct"
    else
        echo "  module.yaml content: incorrect"
        PASS=false
    fi
else
    echo "  module.yaml present: no"
    PASS=false
fi

if [ -f "$PULL_DIR/bin/hello.sh" ]; then
    echo "  bin/hello.sh present: yes"
    if [ -x "$PULL_DIR/bin/hello.sh" ]; then
        echo "  bin/hello.sh executable: yes"
    fi
else
    echo "  bin/hello.sh present: no"
    PASS=false
fi

if $PASS; then
    pass_test "OP-OCI-01"
else
    fail_test "OP-OCI-01" "OCI push/pull content integrity check failed"
fi

# Clean up temp dirs
rm -rf "$TEST_MODULE_DIR" "$PULL_DIR"

# Bonus: create Module CRD referencing the pushed artifact to verify controller resolves it
# This may be rejected if ClusterConfigPolicy disallows unsigned modules — that's fine
if kubectl apply -f - 2>/dev/null <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: e2e-oci-module-${E2E_RUN_ID}
  labels:
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
    ${E2E_JOB_LABEL_YAML}
spec:
  packages: []
  ociArtifact: "${OCI_REF}"
EOF
then
    sleep 5
    OCI_RESOLVED=$(kubectl get module "e2e-oci-module-${E2E_RUN_ID}" \
        -o jsonpath='{.status.resolvedArtifact}' 2>/dev/null || echo "")
    OCI_AVAIL=$(kubectl get module "e2e-oci-module-${E2E_RUN_ID}" \
        -o jsonpath='{.status.conditions[?(@.type=="Available")].status}' 2>/dev/null || echo "")
    echo "  Module resolvedArtifact: ${OCI_RESOLVED:-not set}"
    echo "  Module Available: ${OCI_AVAIL:-not set}"
else
    echo "  (Module rejected by policy — unsigned module not allowed, which is correct behavior)"
fi
