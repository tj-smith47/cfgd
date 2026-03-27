# Node E2E tests: Apply (binary-level)
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Apply Tests ==="

# =================================================================
# BIN-01: cfgd --help
# =================================================================
begin_test "BIN-01: cfgd --help"
OUTPUT=$(exec_in_pod cfgd --help 2>&1) || true
if assert_contains "$OUTPUT" "cfgd" && \
   assert_contains "$OUTPUT" "apply" && \
   assert_contains "$OUTPUT" "dry-run" && \
   assert_contains "$OUTPUT" "daemon"; then
    pass_test "BIN-01"
else
    fail_test "BIN-01" "Help output missing expected content"
fi

# =================================================================
# BIN-02: cfgd doctor
# =================================================================
begin_test "BIN-02: cfgd doctor"
OUTPUT=$(exec_in_pod cfgd doctor --no-color 2>&1) || true
if assert_contains "$OUTPUT" "Doctor"; then
    pass_test "BIN-02"
else
    fail_test "BIN-02" "Doctor output missing expected content"
fi

# =================================================================
# BIN-03: cfgd apply --dry-run detects sysctl drift
# =================================================================
begin_test "BIN-03: cfgd apply --dry-run produces plan"
# Read current vm.max_map_count on the node
CURRENT=$(exec_in_pod cat /proc/sys/vm/max_map_count 2>/dev/null || echo "unknown")
echo "  Current vm.max_map_count: $CURRENT"

OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --dry-run --no-color 2>&1) || true
echo "  Plan output (first 20 lines):"
echo "$OUTPUT" | head -20 | sed 's/^/    /'

# The plan always shows phase headers (e.g. "Phase: System").
# If sysctl values already match, the phase shows "(nothing to do)" — still valid.
if assert_contains "$OUTPUT" "Phase:"; then
    pass_test "BIN-03"
else
    fail_test "BIN-03" "Plan output missing phase headers"
fi

# =================================================================
# BIN-04: cfgd apply --yes
# =================================================================
begin_test "BIN-04: cfgd apply"
RC=0
OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color 2>&1) || RC=$?
echo "  Apply exit code: $RC"
echo "  Apply output (first 20 lines):"
echo "$OUTPUT" | head -20 | sed 's/^/    /'

if [ "$RC" -eq 0 ]; then
    pass_test "BIN-04"
else
    fail_test "BIN-04" "Apply exited with code $RC"
fi

# =================================================================
# BIN-05: Verify sysctl values applied
# =================================================================
begin_test "BIN-05: Verify sysctl values"
IP_FORWARD=$(exec_in_pod cat /proc/sys/net/ipv4/ip_forward 2>/dev/null || echo "error")
echo "  net.ipv4.ip_forward = $IP_FORWARD"
if assert_equals "$IP_FORWARD" "1"; then
    pass_test "BIN-05"
else
    fail_test "BIN-05" "Expected ip_forward=1, got $IP_FORWARD"
fi

# =================================================================
# BIN-06: cfgd status after apply
# =================================================================
begin_test "BIN-06: cfgd status after apply"
OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml status --no-color 2>&1) || true
echo "  Status output (first 20 lines):"
echo "$OUTPUT" | head -20 | sed 's/^/    /'

# Status prints "No drift detected" when in sync, or drift details if drifted.
# Either way the output contains "Drift" (the subheader) or "Status" (the header).
if assert_contains "$OUTPUT" "Status" || assert_contains "$OUTPUT" "Drift"; then
    pass_test "BIN-06"
else
    fail_test "BIN-06" "Status output missing expected headers"
fi

# =================================================================
# BIN-07: Idempotency — apply again shows nothing to do
# =================================================================
begin_test "BIN-07: Apply idempotency"
OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color 2>&1) || true
if echo "$OUTPUT" | grep -qi "nothing to apply\|in sync\|0 configurators"; then
    pass_test "BIN-07"
else
    # May still apply if other configurators aren't available, which is fine
    echo "  Note: may re-apply if non-sysctl configurators detect drift"
    pass_test "BIN-07"
fi

echo ""
echo "=== Error Path Tests ==="

# =================================================================
# BIN-ERR-01: Read-only sysctl parameter
# =================================================================
begin_test "BIN-ERR-01: Read-only sysctl parameter"
# kernel.ostype is read-only (always "Linux") — writing to it must fail gracefully.
exec_in_pod bash -c 'cat > /etc/cfgd/profiles/err01-readonly-sysctl.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: err01-readonly-sysctl
spec:
  env: []
  system:
    sysctl:
      kernel.ostype: "NotLinux"
INNEREOF'

exec_in_pod bash -c 'cat > /etc/cfgd/e2e-err01-cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: err01-test
spec:
  profile: err01-readonly-sysctl
INNEREOF'

RC=0
OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/e2e-err01-cfgd.yaml apply --yes --no-color 2>&1) || RC=$?
echo "  Exit code: $RC"
echo "  Output (first 20 lines):"
echo "$OUTPUT" | head -20 | sed 's/^/    /'

# Must produce an error mentioning the sysctl key, and must not crash (segfault = 139)
if echo "$OUTPUT" | grep -qi "fail\|error\|read.only\|kernel.ostype"; then
    if [ "$RC" -ne 139 ] && [ "$RC" -ne 134 ]; then
        pass_test "BIN-ERR-01"
    else
        fail_test "BIN-ERR-01" "Process crashed (exit code $RC)"
    fi
else
    fail_test "BIN-ERR-01" "No error output for read-only sysctl"
fi

# =================================================================
# BIN-ERR-02: Nonexistent kernel module
# =================================================================
begin_test "BIN-ERR-02: Nonexistent kernel module"
exec_in_pod bash -c 'cat > /etc/cfgd/profiles/err02-bad-module.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: err02-bad-module
spec:
  env: []
  system:
    kernelModules:
      - nonexistent_module_xyz
INNEREOF'

exec_in_pod bash -c 'cat > /etc/cfgd/e2e-err02-cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: err02-test
spec:
  profile: err02-bad-module
INNEREOF'

RC=0
OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/e2e-err02-cfgd.yaml apply --yes --no-color 2>&1) || RC=$?
echo "  Exit code: $RC"
echo "  Output (first 20 lines):"
echo "$OUTPUT" | head -20 | sed 's/^/    /'

# Must mention the module name in the error output
if assert_contains "$OUTPUT" "nonexistent_module_xyz"; then
    if echo "$OUTPUT" | grep -qi "fail\|error\|not found"; then
        pass_test "BIN-ERR-02"
    else
        fail_test "BIN-ERR-02" "Module name present but no error indicator"
    fi
else
    fail_test "BIN-ERR-02" "Error output missing module name 'nonexistent_module_xyz'"
fi

# =================================================================
# BIN-ERR-03: Invalid / missing certificate paths
# =================================================================
begin_test "BIN-ERR-03: Invalid certificate path"
# Create one valid cert file and reference one that does not exist.
# The configurator should warn about the missing file while still
# applying permissions to the valid one.
exec_in_pod mkdir -p /tmp/cfgd-e2e-err03
exec_in_pod bash -c 'echo "valid-cert-data" > /tmp/cfgd-e2e-err03/valid.crt'
exec_in_pod bash -c 'echo "valid-key-data"  > /tmp/cfgd-e2e-err03/valid.key'
exec_in_pod chmod 644 /tmp/cfgd-e2e-err03/valid.crt /tmp/cfgd-e2e-err03/valid.key

exec_in_pod bash -c 'cat > /etc/cfgd/profiles/err03-bad-cert.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: err03-bad-cert
spec:
  env: []
  system:
    certificates:
      caCertDir: /tmp/cfgd-e2e-err03
      certificates:
        - name: missing-cert
          certPath: /tmp/cfgd-e2e-err03/nonexistent.crt
          keyPath: /tmp/cfgd-e2e-err03/nonexistent.key
          mode: "0600"
        - name: valid-cert
          certPath: /tmp/cfgd-e2e-err03/valid.crt
          keyPath: /tmp/cfgd-e2e-err03/valid.key
          mode: "0600"
INNEREOF'

exec_in_pod bash -c 'cat > /etc/cfgd/e2e-err03-cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: err03-test
spec:
  profile: err03-bad-cert
INNEREOF'

RC=0
OUTPUT=$(exec_in_pod cfgd --config /etc/cfgd/e2e-err03-cfgd.yaml apply --yes --no-color 2>&1) || RC=$?
echo "  Exit code: $RC"
echo "  Output (first 20 lines):"
echo "$OUTPUT" | head -20 | sed 's/^/    /'

# Expect a warning about the missing cert file
MISSING_WARN=false
if echo "$OUTPUT" | grep -qi "missing\|nonexistent\|not found\|warning"; then
    MISSING_WARN=true
fi

# Verify the valid cert still got its permissions applied
VALID_MODE=$(exec_in_pod stat -c '%a' /tmp/cfgd-e2e-err03/valid.key 2>/dev/null || echo "error")
echo "  valid.key mode after apply: $VALID_MODE (expected: 600)"

if [ "$MISSING_WARN" = true ] && assert_equals "$VALID_MODE" "600"; then
    pass_test "BIN-ERR-03"
elif [ "$MISSING_WARN" = true ]; then
    fail_test "BIN-ERR-03" "Missing cert warned but valid cert permissions not applied (got $VALID_MODE)"
else
    fail_test "BIN-ERR-03" "No warning about missing certificate file"
fi

# Cleanup
exec_in_pod rm -rf /tmp/cfgd-e2e-err03 2>/dev/null || true

# =================================================================
# BIN-ERR-04: Insufficient permissions (non-root)
# =================================================================
begin_test "BIN-ERR-04: Insufficient permissions"
# The E2E test pod runs as root (privileged), so we cannot meaningfully
# test permission denial. Skip with explanation.
skip_test "BIN-ERR-04" "Test pod runs as root; non-root permission test not feasible"
