# Node E2E tests: Init
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== Init Tests ==="

# =================================================================
# BIN-08: cfgd init from local path
# =================================================================
begin_test "BIN-08: cfgd init"
# Create a source directory on the node
exec_in_pod mkdir -p /tmp/e2e-source/profiles
exec_in_pod bash -c 'cat > /tmp/e2e-source/cfgd.yaml << "INNEREOF"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: init-test
spec:
  profile: k8s-worker-minimal
INNEREOF'
exec_in_pod cp /etc/cfgd/profiles/k8s-worker-minimal.yaml /tmp/e2e-source/profiles/
if ! exec_in_pod which git > /dev/null 2>&1; then
    skip_test "BIN-08" "git not available in test pod"
else
exec_in_pod bash -c 'cd /tmp/e2e-source && git init -q && git config user.email "e2e@test" && git config user.name "E2E" && git add -A && git commit -qm "init"'

RC=0
OUTPUT=$(exec_in_pod cfgd init /tmp/e2e-init-test --from /tmp/e2e-source --no-color 2>&1) || RC=$?

if [ "$RC" -eq 0 ] && \
   exec_in_pod test -f /tmp/e2e-init-test/cfgd.yaml && \
   exec_in_pod test -f /tmp/e2e-init-test/profiles/k8s-worker-minimal.yaml; then
    pass_test "BIN-08"
else
    fail_test "BIN-08" "Init failed or files missing (exit code: $RC)"
fi
fi  # end git availability check
