# Operator E2E tests: BackupPolicy
# Sourced by run-all.sh — do NOT set traps or pipefail here.

echo ""
echo "=== BackupPolicy Tests ==="

BP_NS="e2e-backup-${E2E_RUN_ID}"

kubectl create namespace "$BP_NS" 2>/dev/null || true
kubectl label namespace "$BP_NS" "$E2E_RUN_LABEL" --overwrite 2>/dev/null

# =================================================================
# OP-BP-01: A policy over two machines, one of which pins its unit
# =================================================================
begin_test "OP-BP-01: BackupPolicy matches two machines, one pinning locally"

# nuc-01 and nuc-02 carry the profile label the policy selects; nuc-03 is the
# negative — it must not appear in the projection or in machinesMatched.
for BP_MC in nuc-01 nuc-02; do
    kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: ${BP_MC}
  namespace: ${BP_NS}
  labels:
    cfgd.io/profile: workstation
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: ${BP_MC}
  profile: workstation
EOF
done

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: nuc-03
  namespace: ${BP_NS}
  labels:
    cfgd.io/profile: server
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: nuc-03
  profile: server
EOF

# The device gateway writes this map at check-in; this suite runs no device, so
# the patch stands in for one that reported the unit as its own.
kubectl patch machineconfig nuc-02 -n "$BP_NS" --subresource=status --type=merge \
    -p '{"status":{"backupScheduleOwners":{"dotfiles":"local"}}}' 2>/dev/null

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: BackupPolicy
metadata:
  name: nightly-dotfiles
  namespace: ${BP_NS}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  selector:
    matchLabels:
      cfgd.io/profile: workstation
  units:
    - name: dotfiles
      schedule: "0 3 * * *"
      retention: 14
EOF

echo "  Waiting for BackupPolicy projection..."
BP01_ROWS=0
BP01_DEADLINE=$((SECONDS + 120))
while [ $SECONDS -lt $BP01_DEADLINE ]; do
    BP01_ROWS=$(kubectl get backuppolicy nightly-dotfiles -n "$BP_NS" \
        -o jsonpath='{.status.units[*].hostname}' 2>/dev/null | wc -w)
    if [ "$BP01_ROWS" -eq 2 ]; then
        break
    fi
    sleep 2
done

BP01_MATCHED=$(kubectl get backuppolicy nightly-dotfiles -n "$BP_NS" \
    -o jsonpath='{.status.machinesMatched}' 2>/dev/null || echo "")
BP01_HOSTS=$(kubectl get backuppolicy nightly-dotfiles -n "$BP_NS" \
    -o jsonpath='{.status.units[*].hostname}' 2>/dev/null || echo "")
BP01_SUMMARY=$(kubectl get backuppolicy nightly-dotfiles -n "$BP_NS" \
    -o jsonpath='{.status.unitsSummary}' 2>/dev/null || echo "")

echo "  status.units rows: $BP01_ROWS (hostnames: ${BP01_HOSTS:-none})"
echo "  machinesMatched:   ${BP01_MATCHED:-not set}"
echo "  unitsSummary:      ${BP01_SUMMARY:-not set}"

# WHICH two hostnames is the point of the third MachineConfig: a selector that
# admitted nuc-03 and dropped nuc-01 still reports two rows. status.units is
# sorted by (hostname, name), so the order is stable.
if [ "$BP01_ROWS" -eq 2 ] && assert_equals "$BP01_MATCHED" "2" &&
    assert_equals "$BP01_HOSTS" "nuc-01 nuc-02"; then
    pass_test "OP-BP-01"
else
    fail_test "OP-BP-01" "Expected nuc-01 and nuc-02 with machinesMatched=2, got rows=${BP01_ROWS} (${BP01_HOSTS:-none}), matched=${BP01_MATCHED:-unset}"
fi

# =================================================================
# OP-BP-02: The cluster-owned machine takes the policy's schedule
# =================================================================
begin_test "OP-BP-02: cluster-owned machine takes the policy schedule"

BP02_OWNER=$(kubectl get backuppolicy nightly-dotfiles -n "$BP_NS" \
    -o jsonpath='{.status.units[?(@.hostname=="nuc-01")].owner}' 2>/dev/null || echo "")
BP02_SCHEDULE=$(kubectl get backuppolicy nightly-dotfiles -n "$BP_NS" \
    -o jsonpath='{.status.units[?(@.hostname=="nuc-01")].schedule}' 2>/dev/null || echo "")

echo "  nuc-01 owner:    ${BP02_OWNER:-not set}"
echo "  nuc-01 schedule: ${BP02_SCHEDULE:-not set}"

BP02_PASS=true
assert_equals "$BP02_OWNER" "cluster" || BP02_PASS=false
assert_equals "$BP02_SCHEDULE" "0 3 * * *" || BP02_PASS=false

if [ "$BP02_PASS" = true ]; then
    pass_test "OP-BP-02"
else
    fail_test "OP-BP-02" "nuc-01 row is not the policy's own schedule"
fi

# =================================================================
# OP-BP-03: The locally-pinned machine is reported, not overridden
# =================================================================
begin_test "OP-BP-03: locally-pinned machine is reported, not overridden"

BP03_OWNER=$(kubectl get backuppolicy nightly-dotfiles -n "$BP_NS" \
    -o jsonpath='{.status.units[?(@.hostname=="nuc-02")].owner}' 2>/dev/null || echo "")
BP03_SCHEDULE=$(kubectl get backuppolicy nightly-dotfiles -n "$BP_NS" \
    -o jsonpath='{.status.units[?(@.hostname=="nuc-02")].schedule}' 2>/dev/null || echo "")
BP03_MESSAGE=$(kubectl get backuppolicy nightly-dotfiles -n "$BP_NS" \
    -o jsonpath='{.status.units[?(@.hostname=="nuc-02")].message}' 2>/dev/null || echo "")

echo "  nuc-02 owner:    ${BP03_OWNER:-not set}"
echo "  nuc-02 schedule: ${BP03_SCHEDULE:-empty}"
echo "  nuc-02 message:  ${BP03_MESSAGE:-not set}"

BP03_PASS=true
assert_equals "$BP03_OWNER" "local" || BP03_PASS=false
assert_equals "$BP03_SCHEDULE" "" || BP03_PASS=false
assert_contains "$BP03_MESSAGE" "pins this unit's schedule" || BP03_PASS=false

if [ "$BP03_PASS" = true ]; then
    pass_test "OP-BP-03"
else
    fail_test "OP-BP-03" "nuc-02 row does not report the machine's own pin"
fi

# =================================================================
# OP-BP-04: Admission rejects a unit with no schedule
# =================================================================
begin_test "OP-BP-04: admission rejects a unit with no schedule"

BP04_RESULT=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: BackupPolicy
metadata:
  name: e2e-no-schedule-${E2E_RUN_ID}
  namespace: ${BP_NS}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  units:
    - name: dotfiles
      schedule: ""
EOF
)
echo "  Empty schedule result: $(echo "$BP04_RESULT" | tail -1)"

# assert_rejected matches any error text, so a terminating namespace or a
# connectivity failure reads as a refusal; the webhook has to name itself.
BP04_PASS=true
assert_rejected "$BP04_RESULT" "BackupPolicy unit with an empty schedule" || BP04_PASS=false
assert_contains "$BP04_RESULT" "validate-backuppolicy.cfgd.io" || BP04_PASS=false

if [ "$BP04_PASS" = true ]; then
    pass_test "OP-BP-04"
else
    fail_test "OP-BP-04" "A unit with no schedule was admitted, or the refusal did not come from the webhook"
fi

# =================================================================
# OP-BP-05: The schema refuses a policy that schedules nothing
# =================================================================
begin_test "OP-BP-05: a policy that schedules nothing is refused"

BP05_EMPTY=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: BackupPolicy
metadata:
  name: e2e-empty-units-${E2E_RUN_ID}
  namespace: ${BP_NS}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  units: []
EOF
)
echo "  Empty units result: $(echo "$BP05_EMPTY" | tail -1)"

BP05_ABSENT=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: BackupPolicy
metadata:
  name: e2e-absent-units-${E2E_RUN_ID}
  namespace: ${BP_NS}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  selector:
    matchLabels:
      cfgd.io/profile: workstation
EOF
)
echo "  Absent units result: $(echo "$BP05_ABSENT" | tail -1)"

# Each arm is refused by a different layer, and only the wording says which:
# the webhook for a list that is present and empty, the schema's own `required`
# for a spec that names no units at all.
BP05_PASS=true
assert_rejected "$BP05_EMPTY" "BackupPolicy with an empty units list" || BP05_PASS=false
assert_contains "$BP05_EMPTY" "must declare at least one unit" || BP05_PASS=false
assert_rejected "$BP05_ABSENT" "BackupPolicy with no units field" || BP05_PASS=false
assert_contains "$BP05_ABSENT" "spec.units: Required value" || BP05_PASS=false

if [ "$BP05_PASS" = true ]; then
    pass_test "OP-BP-05"
else
    fail_test "OP-BP-05" "A policy that schedules nothing was admitted"
fi

# --- Clean up the BackupPolicy namespace ---
kubectl delete namespace "$BP_NS" --wait=false --ignore-not-found 2>/dev/null || true
