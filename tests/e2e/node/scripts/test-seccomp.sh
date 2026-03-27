# Node E2E tests: Seccomp
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Seccomp Tests ==="

# =================================================================
# SECCOMP-01: Seccomp profile write
# =================================================================
begin_test "SECCOMP-01: Seccomp profile management"
# Use seccomp-only config
exec_in_pod bash -c 'cat > /etc/cfgd/e2e-seccomp-cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: seccomp-test
spec:
  profile: k8s-worker-seccomp
INNEREOF'

RC=0
OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/e2e-seccomp-cfgd.yaml apply --yes --no-color 2>&1) || RC=$?

if [ "$RC" -eq 0 ] && \
   exec_in_pod test -f /tmp/cfgd-e2e-seccomp/audit.json; then
    # Verify content
    CONTENT=$(exec_in_pod cat /tmp/cfgd-e2e-seccomp/audit.json)
    if assert_contains "$CONTENT" "SCMP_ACT_LOG"; then
        pass_test "SECCOMP-01"
    else
        fail_test "SECCOMP-01" "Seccomp profile content incorrect"
    fi
else
    fail_test "SECCOMP-01" "Seccomp profile not created (exit code: $RC)"
    echo "  Output: $OUTPUT"
fi

# =================================================================
# SECCOMP-02: Seccomp profile write and verify
# =================================================================
begin_test "SECCOMP-02: Seccomp profile write"
exec_in_pod bash -c 'cat > /etc/cfgd/e2e-seccomp-cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: seccomp-test
spec:
  profile: k8s-worker-seccomp
INNEREOF'

exec_in_pod cfgd --config /etc/cfgd/e2e-seccomp-cfgd.yaml apply --yes --no-color > /dev/null 2>&1 || true

if exec_in_pod test -f /tmp/cfgd-e2e-seccomp/audit.json; then
    CONTENT=$(exec_in_pod cat /tmp/cfgd-e2e-seccomp/audit.json)
    if assert_contains "$CONTENT" "SCMP_ACT_LOG" && \
       assert_contains "$CONTENT" "SCMP_ARCH_X86_64"; then
        pass_test "SECCOMP-02"
    else
        fail_test "SECCOMP-02" "Seccomp profile content incorrect"
    fi
else
    fail_test "SECCOMP-02" "Seccomp profile file not created"
fi

# =================================================================
# SECCOMP-03: Seccomp profile drift detection
# =================================================================
begin_test "SECCOMP-03: Seccomp profile drift"
# Modify the seccomp file
exec_in_pod bash -c 'echo "corrupted" > /tmp/cfgd-e2e-seccomp/audit.json'

PLAN=$(exec_in_pod cfgd --config /etc/cfgd/e2e-seccomp-cfgd.yaml apply --dry-run --no-color 2>&1) || true
echo "  Plan after seccomp corruption:"
echo "$PLAN" | head -10 | sed 's/^/    /'

if assert_contains "$PLAN" "seccomp"; then
    pass_test "SECCOMP-03"
else
    fail_test "SECCOMP-03" "Seccomp drift not detected"
fi
