# Operator E2E tests: Module
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Module Tests ==="

# =================================================================
# OP-MOD-01: Module CRD — create and verify controller sets status
# =================================================================
begin_test "OP-MOD-01: Module CRD — controller sets status"

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: e2e-nettools-${E2E_RUN_ID}
  labels:
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: netcat
      platforms:
        apt: netcat-openbsd
        brew: netcat
    - name: curl
  files:
    - source: bin/probe.sh
      target: bin/probe.sh
  env:
    - name: NETTOOLS_VERSION
      value: "1.0.0"
  ociArtifact: "${REGISTRY}/cfgd-e2e/nettools:v1.0"
  signature:
    cosign:
      publicKey: |
        -----BEGIN PUBLIC KEY-----
        MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEY1k7MJOEHLPJSKpCmwbL+VZvjnL
        BSoSjO1RxUNFU0RFNEM5T3lOamF4WGd3c3BPaEY0RGxPZmNqSGtjYQpGZz09Cg==
        -----END PUBLIC KEY-----
  mountPolicy: Always
EOF

# Wait for Module controller to reconcile
echo "  Waiting for Module status..."
MOD_VERIFIED=$(wait_for_k8s_field module "e2e-nettools-${E2E_RUN_ID}" "" \
    '{.status.verified}' "" 60) || true

RESOLVED=$(kubectl get module "e2e-nettools-${E2E_RUN_ID}" \
    -o jsonpath='{.status.resolvedArtifact}' 2>/dev/null || echo "")
AVAIL_COND=$(kubectl get module "e2e-nettools-${E2E_RUN_ID}" \
    -o jsonpath='{.status.conditions[?(@.type=="Available")].status}' 2>/dev/null || echo "")
VERIFIED_COND=$(kubectl get module "e2e-nettools-${E2E_RUN_ID}" \
    -o jsonpath='{.status.conditions[?(@.type=="Verified")].status}' 2>/dev/null || echo "")

echo "  verified: ${MOD_VERIFIED:-not set}"
echo "  resolvedArtifact: ${RESOLVED:-not set}"
echo "  Available condition: ${AVAIL_COND:-not set}"
echo "  Verified condition: ${VERIFIED_COND:-not set}"

if [ -n "$MOD_VERIFIED" ] && [ -n "$RESOLVED" ]; then
    pass_test "OP-MOD-01"
else
    fail_test "OP-MOD-01" "Module controller did not set status fields"
fi

# =================================================================
# OP-MOD-02: Module webhook — rejects invalid OCI refs and malformed PEM
# =================================================================
begin_test "OP-MOD-02: Module webhook — rejects invalid specs"

# Test 1: Invalid OCI reference (missing tag/digest)
RESULT_INVALID_OCI=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: e2e-bad-oci-${E2E_RUN_ID}
  labels:
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: test
  ociArtifact: "not a valid oci reference!@#"
EOF
)
echo "  Invalid OCI ref result: $(echo "$RESULT_INVALID_OCI" | tail -1)"

# Test 2: Malformed PEM public key
RESULT_BAD_PEM=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: e2e-bad-pem-${E2E_RUN_ID}
  labels:
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: test
  ociArtifact: "ghcr.io/test/module:v1"
  signature:
    cosign:
      publicKey: "this is not a valid PEM key"
EOF
)
echo "  Bad PEM result: $(echo "$RESULT_BAD_PEM" | tail -1)"

# Test 3: Empty package name
RESULT_EMPTY_PKG=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: e2e-empty-pkg-${E2E_RUN_ID}
  labels:
    cfgd.io/e2e-run: "${E2E_RUN_ID}"
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: ""
EOF
)
echo "  Empty pkg result: $(echo "$RESULT_EMPTY_PKG" | tail -1)"

PASS=true
assert_rejected "$RESULT_INVALID_OCI" "Invalid OCI ref" || PASS=false
assert_rejected "$RESULT_BAD_PEM" "Bad PEM key" || PASS=false

if $PASS; then
    pass_test "OP-MOD-02"
else
    fail_test "OP-MOD-02" "Webhook did not reject invalid Module specs"
fi

# Clean up any resources that might have been created
kubectl delete module "e2e-bad-oci-${E2E_RUN_ID}" "e2e-bad-pem-${E2E_RUN_ID}" "e2e-empty-pkg-${E2E_RUN_ID}" 2>/dev/null || true
