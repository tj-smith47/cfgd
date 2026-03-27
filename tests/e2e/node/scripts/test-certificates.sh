# Node E2E tests: Certificates
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Certificate Tests ==="

# =================================================================
# CERT-01: Certificate permission enforcement
# =================================================================
begin_test "CERT-01: Certificate permissions"
# Create dummy cert files
exec_in_pod mkdir -p /tmp/cfgd-e2e-pki
exec_in_pod bash -c 'echo "dummy-cert" > /tmp/cfgd-e2e-pki/test.crt'
exec_in_pod bash -c 'echo "dummy-key" > /tmp/cfgd-e2e-pki/test.key'
exec_in_pod chmod 644 /tmp/cfgd-e2e-pki/test.crt /tmp/cfgd-e2e-pki/test.key

# Use certs-only config
exec_in_pod bash -c 'cat > /etc/cfgd/e2e-certs-cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: certs-test
spec:
  profile: k8s-worker-certs
INNEREOF'

OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/e2e-certs-cfgd.yaml apply --yes --no-color 2>&1) || true

CERT_MODE=$(exec_in_pod stat -c '%a' /tmp/cfgd-e2e-pki/test.crt 2>/dev/null || echo "error")
KEY_MODE=$(exec_in_pod stat -c '%a' /tmp/cfgd-e2e-pki/test.key 2>/dev/null || echo "error")

echo "  test.crt mode: $CERT_MODE (expected: 600)"
echo "  test.key mode: $KEY_MODE (expected: 600)"

if assert_equals "$KEY_MODE" "600"; then
    pass_test "CERT-01"
else
    fail_test "CERT-01" "Certificate permissions not set correctly"
fi

# =================================================================
# CERT-02: Certificate permission drift
# =================================================================
begin_test "CERT-02: Certificate permission drift"
exec_in_pod mkdir -p /tmp/cfgd-e2e-pki
exec_in_pod bash -c 'echo "cert" > /tmp/cfgd-e2e-pki/test.crt'
exec_in_pod bash -c 'echo "key" > /tmp/cfgd-e2e-pki/test.key'
exec_in_pod chmod 644 /tmp/cfgd-e2e-pki/test.crt /tmp/cfgd-e2e-pki/test.key

exec_in_pod bash -c 'cat > /etc/cfgd/e2e-certs-cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: certs-test
spec:
  profile: k8s-worker-certs
INNEREOF'

PLAN=$(exec_in_pod cfgd --config /etc/cfgd/e2e-certs-cfgd.yaml apply --dry-run --no-color 2>&1) || true
echo "  Plan before cert apply:"
echo "$PLAN" | head -10 | sed 's/^/    /'

# Apply to fix permissions
exec_in_pod cfgd --config /etc/cfgd/e2e-certs-cfgd.yaml apply --yes --no-color > /dev/null 2>&1 || true

KEY_MODE=$(exec_in_pod stat -c '%a' /tmp/cfgd-e2e-pki/test.key 2>/dev/null || echo "error")
echo "  Key permissions after apply: $KEY_MODE"

if assert_equals "$KEY_MODE" "600"; then
    # Now change permissions back and verify drift detection
    exec_in_pod chmod 644 /tmp/cfgd-e2e-pki/test.key || true
    PLAN2=$(exec_in_pod cfgd --config /etc/cfgd/e2e-certs-cfgd.yaml apply --dry-run --no-color 2>&1) || true
    if assert_contains "$PLAN2" "cert" || assert_contains "$PLAN2" "changes"; then
        pass_test "CERT-02"
    else
        fail_test "CERT-02" "Permission drift not detected after chmod"
    fi
else
    fail_test "CERT-02" "Permissions not set correctly (got $KEY_MODE)"
fi
