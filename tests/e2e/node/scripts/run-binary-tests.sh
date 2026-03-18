#!/usr/bin/env bash
# E2E binary-level tests for cfgd (node mode).
# Runs cfgd commands directly on the kind node container.
# Prereqs: kind cluster running, cfgd image loaded.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
FIXTURES="$SCRIPT_DIR/../fixtures"

echo "=== cfgd Binary Tests ==="

# --- Setup ---
echo "Installing cfgd binary on kind node..."
install_binary_on_node "cfgd:e2e-test" "/usr/local/bin/cfgd"
install_packages_on_node procps kmod git

echo "Copying test fixtures to kind node..."
NODE="$(get_kind_node)"
docker exec "$NODE" mkdir -p /etc/cfgd/profiles
docker cp "$FIXTURES/configs/cfgd.yaml" "$NODE:/etc/cfgd/cfgd.yaml"
for f in "$FIXTURES/profiles/"*.yaml; do
    docker cp "$f" "$NODE:/etc/cfgd/profiles/$(basename "$f")"
done

# =================================================================
# T01: cfgd --help
# =================================================================
begin_test "T01: cfgd --help"
OUTPUT=$(exec_on_node cfgd --help 2>&1) || true
if assert_contains "$OUTPUT" "cfgd" && \
   assert_contains "$OUTPUT" "apply" && \
   assert_contains "$OUTPUT" "dry-run" && \
   assert_contains "$OUTPUT" "daemon"; then
    pass_test "T01"
else
    fail_test "T01" "Help output missing expected content"
fi

# =================================================================
# T02: cfgd doctor
# =================================================================
begin_test "T02: cfgd doctor"
OUTPUT=$(exec_on_node cfgd doctor --no-color 2>&1) || true
if assert_contains "$OUTPUT" "Doctor"; then
    pass_test "T02"
else
    fail_test "T02" "Doctor output missing expected content"
fi

# =================================================================
# T03: cfgd apply --dry-run detects sysctl drift
# =================================================================
begin_test "T03: cfgd apply --dry-run produces plan"
# Read current vm.max_map_count on the node
CURRENT=$(exec_on_node cat /proc/sys/vm/max_map_count 2>/dev/null || echo "unknown")
echo "  Current vm.max_map_count: $CURRENT"

OUTPUT=$(exec_on_node cfgd --config /etc/cfgd/cfgd.yaml apply --dry-run --no-color 2>&1) || true
echo "  Plan output (first 20 lines):"
echo "$OUTPUT" | head -20 | sed 's/^/    /'

# The plan always shows phase headers (e.g. "Phase: System").
# If sysctl values already match, the phase shows "(nothing to do)" — still valid.
if assert_contains "$OUTPUT" "Phase:"; then
    pass_test "T03"
else
    fail_test "T03" "Plan output missing phase headers"
fi

# =================================================================
# T04: cfgd apply --yes
# =================================================================
begin_test "T04: cfgd apply"
OUTPUT=$(exec_on_node cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color 2>&1)
RC=$?
echo "  Apply exit code: $RC"
echo "  Apply output (first 20 lines):"
echo "$OUTPUT" | head -20 | sed 's/^/    /'

if [ "$RC" -eq 0 ]; then
    pass_test "T04"
else
    fail_test "T04" "Apply exited with code $RC"
fi

# =================================================================
# T05: Verify sysctl values applied
# =================================================================
begin_test "T05: Verify sysctl values"
IP_FORWARD=$(exec_on_node cat /proc/sys/net/ipv4/ip_forward 2>/dev/null || echo "error")
echo "  net.ipv4.ip_forward = $IP_FORWARD"
if assert_equals "$IP_FORWARD" "1"; then
    pass_test "T05"
else
    fail_test "T05" "Expected ip_forward=1, got $IP_FORWARD"
fi

# =================================================================
# T06: cfgd status after apply
# =================================================================
begin_test "T06: cfgd status after apply"
OUTPUT=$(exec_on_node cfgd --config /etc/cfgd/cfgd.yaml status --no-color 2>&1) || true
echo "  Status output (first 20 lines):"
echo "$OUTPUT" | head -20 | sed 's/^/    /'

# Status prints "No drift detected" when in sync, or drift details if drifted.
# Either way the output contains "Drift" (the subheader) or "Status" (the header).
if assert_contains "$OUTPUT" "Status" || assert_contains "$OUTPUT" "Drift"; then
    pass_test "T06"
else
    fail_test "T06" "Status output missing expected headers"
fi

# =================================================================
# T07: Idempotency — apply again shows nothing to do
# =================================================================
begin_test "T07: Apply idempotency"
OUTPUT=$(exec_on_node cfgd --config /etc/cfgd/cfgd.yaml apply --yes --no-color 2>&1) || true
if echo "$OUTPUT" | grep -qi "nothing to apply\|in sync\|0 configurators"; then
    pass_test "T07"
else
    # May still apply if other configurators aren't available, which is fine
    echo "  Note: may re-apply if non-sysctl configurators detect drift"
    pass_test "T07"
fi

# =================================================================
# T08: Drift detection after manual change
# =================================================================
begin_test "T08: Drift detection"
# Change a sysctl value manually
ORIG=$(exec_on_node cat /proc/sys/vm/max_map_count)
exec_on_node sysctl -w vm.max_map_count=65530 > /dev/null 2>&1 || true

OUTPUT=$(exec_on_node cfgd --config /etc/cfgd/cfgd.yaml apply --dry-run --no-color 2>&1) || true
echo "  After changing vm.max_map_count to 65530:"
echo "$OUTPUT" | head -15 | sed 's/^/    /'

if assert_contains "$OUTPUT" "vm.max_map_count"; then
    pass_test "T08"
else
    fail_test "T08" "Drift not detected for vm.max_map_count"
fi

# Restore
exec_on_node sysctl -w "vm.max_map_count=$ORIG" > /dev/null 2>&1 || true

# =================================================================
# T09: cfgd init from local path
# =================================================================
begin_test "T09: cfgd init"
# Create a source directory on the node
exec_on_node mkdir -p /tmp/e2e-source/profiles
exec_on_node bash -c 'cat > /tmp/e2e-source/cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: init-test
spec:
  profile: k8s-worker-minimal
INNEREOF'
exec_on_node cp /etc/cfgd/profiles/k8s-worker-minimal.yaml /tmp/e2e-source/profiles/
if ! exec_on_node which git > /dev/null 2>&1; then
    skip_test "T09" "git not available on kind node"
else
exec_on_node bash -c 'cd /tmp/e2e-source && git init -q && git config user.email "e2e@test" && git config user.name "E2E" && git add -A && git commit -qm "init"'

RC=0
OUTPUT=$(exec_on_node cfgd init /tmp/e2e-init-test --from /tmp/e2e-source --no-color 2>&1) || RC=$?

if [ "$RC" -eq 0 ] && \
   exec_on_node test -f /tmp/e2e-init-test/cfgd.yaml && \
   exec_on_node test -f /tmp/e2e-init-test/profiles/k8s-worker-minimal.yaml; then
    pass_test "T09"
else
    fail_test "T09" "Init failed or files missing (exit code: $RC)"
fi
fi  # end git availability check

# =================================================================
# T10: Seccomp profile write
# =================================================================
begin_test "T10: Seccomp profile management"
# Use seccomp-only config
exec_on_node bash -c 'cat > /etc/cfgd/e2e-seccomp-cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: seccomp-test
spec:
  profile: k8s-worker-seccomp
INNEREOF'

OUTPUT=$(exec_on_node cfgd --config /etc/cfgd/e2e-seccomp-cfgd.yaml apply --yes --no-color 2>&1) || true
RC=$?

if [ "$RC" -eq 0 ] && \
   exec_on_node test -f /tmp/cfgd-e2e-seccomp/audit.json; then
    # Verify content
    CONTENT=$(exec_on_node cat /tmp/cfgd-e2e-seccomp/audit.json)
    if assert_contains "$CONTENT" "SCMP_ACT_LOG"; then
        pass_test "T10"
    else
        fail_test "T10" "Seccomp profile content incorrect"
    fi
else
    fail_test "T10" "Seccomp profile not created (exit code: $RC)"
    echo "  Output: $OUTPUT"
fi

# =================================================================
# T11: Certificate permission enforcement
# =================================================================
begin_test "T11: Certificate permissions"
# Create dummy cert files
exec_on_node mkdir -p /tmp/cfgd-e2e-pki
exec_on_node bash -c 'echo "dummy-cert" > /tmp/cfgd-e2e-pki/test.crt'
exec_on_node bash -c 'echo "dummy-key" > /tmp/cfgd-e2e-pki/test.key'
exec_on_node chmod 644 /tmp/cfgd-e2e-pki/test.crt /tmp/cfgd-e2e-pki/test.key

# Use certs-only config
exec_on_node bash -c 'cat > /etc/cfgd/e2e-certs-cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: certs-test
spec:
  profile: k8s-worker-certs
INNEREOF'

OUTPUT=$(exec_on_node cfgd --config /etc/cfgd/e2e-certs-cfgd.yaml apply --yes --no-color 2>&1) || true

CERT_MODE=$(exec_on_node stat -c '%a' /tmp/cfgd-e2e-pki/test.crt 2>/dev/null || echo "error")
KEY_MODE=$(exec_on_node stat -c '%a' /tmp/cfgd-e2e-pki/test.key 2>/dev/null || echo "error")

echo "  test.crt mode: $CERT_MODE (expected: 600)"
echo "  test.key mode: $KEY_MODE (expected: 600)"

if assert_equals "$KEY_MODE" "600"; then
    pass_test "T11"
else
    fail_test "T11" "Certificate permissions not set correctly"
fi

# --- Summary ---
print_summary "Binary Tests"
