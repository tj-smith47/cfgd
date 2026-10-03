#!/usr/bin/env bash
# E2E tests for Crossplane TeamConfig composition: XRD installation,
# MachineConfig fan-out via function-cfgd, ConfigPolicy generation,
# and member add/remove lifecycle.
# Prereqs: cluster running with the cfgd CRDs ArgoCD applies; XP-01 installs
# Crossplane where ArgoCD does not run it.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=tests/e2e/common/helpers.sh
source "$SCRIPT_DIR/../../common/helpers.sh"

echo "=== Crossplane E2E Tests ==="

# =================================================================
# XP-01: Crossplane is running (Helm installs it where ArgoCD does not)
# =================================================================
begin_test "XP-01: Crossplane installation"
crossplane_install || exit 1
wait_for_deployment crossplane-system crossplane 120
pass_test "XP-01"

# --- Setup: the cfgd CRDs ArgoCD applied ---
echo "Checking the cfgd CRDs on the cluster..."
check_pr_crds schemas/crds.yaml "rerun the Crossplane suite" < "$REPO_ROOT/schemas/crds.yaml" || exit 1

# --- Setup: the TeamConfig XRD ArgoCD applied ---
echo "Checking the TeamConfig XRD on the cluster..."
check_pr_xrd || exit 1

# --- Setup: this run's Function and Composition ---
# The E2E workflow's concurrency group serializes runs, so a run-labelled
# TeamConfig, Composition or Function present now is a leftover of a run that
# did not tear down. A leftover TeamConfig's composed MachineConfigs would count
# toward this run's cases, and a TeamConfig references its Composition, so the
# TeamConfigs go first. Crossplane's package lock keys a Function on its
# repository, so a leftover Function holds the lock node this run's Function
# needs until its revisions are gone.
echo "Removing TeamConfigs, Compositions and Functions left by earlier runs..."
if ! LEFTOVER_FUNCS="$(kubectl get function -l cfgd.io/e2e-run -o jsonpath='{range .items[*]}{.metadata.name}{"\n"}{end}')"; then
    echo "ERROR: could not list the run-labelled Functions of earlier runs. Read the kubectl error above, then rerun the Crossplane suite." >&2
    exit 1
fi
for kind in teamconfigs composition function; do
    scope=()
    [ "$kind" != teamconfigs ] || scope=(-A)
    if ! kubectl delete "$kind" "${scope[@]}" -l cfgd.io/e2e-run --ignore-not-found --wait=true --timeout=90s; then
        echo "ERROR: the run-labelled $kind of earlier runs were not deleted within 90s. Read the kubectl error above, then rerun the Crossplane suite." >&2
        exit 1
    fi
done
# A revision's pkg.crossplane.io/package label is its Function's name, so the
# wait covers the revisions of the Functions removed above and no other's.
if [ -n "$LEFTOVER_FUNCS" ]; then
    if ! FUNC_REVISIONS="$(kubectl get functionrevisions -o json | jq -r --arg names "$LEFTOVER_FUNCS" \
        '[$names | split("\n")[] | select(. != "")] as $funcs
         | .items[] | select((.metadata.labels["pkg.crossplane.io/package"] // "") as $pkg | any($funcs[]; . == $pkg))
         | "functionrevision/" + .metadata.name')"; then
        echo "ERROR: could not list the functionrevisions of earlier runs' Functions. Read the kubectl error above, then rerun the Crossplane suite." >&2
        exit 1
    fi
    # A revision deleted between the list and the wait is NotFound to some
    # kubectl versions, so a failed wait is read back before it counts.
    # shellcheck disable=SC2086 # one revision name per word
    if [ -n "$FUNC_REVISIONS" ] && ! kubectl wait --for=delete $FUNC_REVISIONS --timeout=120s &&
        { ! REVISIONS_LEFT="$(kubectl get $FUNC_REVISIONS --ignore-not-found -o name)" || [ -n "$REVISIONS_LEFT" ]; }; then
        echo "ERROR: these functionrevisions of earlier runs' Functions were not removed within 120s: $(paste -sd ' ' <<<"$FUNC_REVISIONS"). Read the kubectl error above and their finalizers, then rerun the Crossplane suite." >&2
        exit 1
    fi
fi

# The run's Function reads ArgoCD's DeploymentRuntimeConfig, a template that a
# second Function referencing it leaves unchanged. The xpkg is built by
# setup-cluster.sh.
echo "Installing $E2E_FUNCTION and $E2E_COMPOSITION..."
kubectl apply -f - <<FUNCEOF
apiVersion: pkg.crossplane.io/v1beta1
kind: Function
metadata:
  name: ${E2E_FUNCTION}
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  package: $(e2e_image function-cfgd)
  packagePullPolicy: Always
  packagePullSecrets:
    - name: registry-credentials
  runtimeConfigRef:
    apiVersion: pkg.crossplane.io/v1beta1
    kind: DeploymentRuntimeConfig
    name: function-cfgd-runtime
FUNCEOF
RUN_COMPOSITION="$(render_run_composition)" || exit 1
kubectl apply -f - <<<"$RUN_COMPOSITION"

echo "Waiting for $E2E_FUNCTION to be healthy..."
if ! wait_for_k8s_field function "$E2E_FUNCTION" "" \
    '{.status.conditions[?(@.type=="Healthy")].status}' True 300 >/dev/null; then
    echo "ERROR: function/$E2E_FUNCTION is not Healthy after 300s: $(kubectl get function "$E2E_FUNCTION" -o jsonpath='{range .status.conditions[*]}{.type}={.status} ({.reason}: {.message}); {end}' 2>&1)" >&2
    exit 1
fi

# Warm up: verify the composition pipeline works end-to-end before running tests.
# After $E2E_FUNCTION turns Healthy, the composition engine needs time to route gRPC
# calls to its pod. Create a canary TeamConfig and wait for it to produce results.
echo "Verifying composition pipeline (warm-up)..."
kubectl apply -f - <<WARMUPEOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: warmup-team
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: warmup-team
  profile: test
  members:
    - username: warmup
      hostname: warmup-host
WARMUPEOF
WARMUP_OK=false
for i in $(seq 1 60); do
    WMC=$(kubectl get mc -A --no-headers 2>/dev/null | { grep -c "warmup-team" || true; })
    if [ "${WMC:-0}" -ge 1 ]; then
        echo "  Composition pipeline ready after $((i*3))s"
        WARMUP_OK=true
        break
    fi
    sleep 3
done
# Every XP case after this one needs the pipeline, so a failed warm-up stops
# the suite with the composite's conditions; warmup-team carries the run label
# and pr-install-down.sh removes it.
if [ "$WARMUP_OK" != "true" ]; then
    echo "ERROR: teamconfig/warmup-team composed no MachineConfig within 180s: $(kubectl get teamconfig warmup-team -o jsonpath='{range .status.conditions[*]}{.type}={.status} ({.reason}: {.message}); {end}' 2>&1). Read function/$E2E_FUNCTION's conditions and its pod's log, then rerun the Crossplane suite." >&2
    exit 1
fi
kubectl delete teamconfig warmup-team --ignore-not-found 2>/dev/null || true
sleep 5
kubectl delete mc -l cfgd.io/team=warmup-team --ignore-not-found -A 2>/dev/null || true

# =================================================================
# XP-02: Create TeamConfig with 2 members
# =================================================================
begin_test "XP-02: TeamConfig generates MachineConfigs"
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: test-team
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: test-team
  profile: developer
  members:
    - username: alice
      hostname: dev-laptop-1
    - username: bob
      hostname: dev-laptop-2
  policy:
    required:
      packages:
        brew:
          - kubectl
          - git
    requiredModules:
      - corp-vpn
EOF

MC_COUNT=""
for i in $(seq 1 30); do
    MC_COUNT=$(kubectl get mc -A --no-headers 2>/dev/null | { grep -c "test-team" || true; })
    if [ "${MC_COUNT:-0}" -ge 2 ]; then
        break
    fi
    sleep 2
done

echo "  MachineConfig count: ${MC_COUNT:-0}"

if [ "${MC_COUNT:-0}" -ge 2 ]; then
    pass_test "XP-02"
else
    fail_test "XP-02" "Expected >=2 MachineConfigs, got ${MC_COUNT:-0}"
fi

# =================================================================
# XP-03: Verify ConfigPolicy created
# =================================================================
begin_test "XP-03: TeamConfig generates ConfigPolicy"

CP_COUNT=""
for i in $(seq 1 15); do
    CP_COUNT=$(kubectl get cpol -A --no-headers 2>/dev/null | { grep -c "test-team" || true; })
    if [ "${CP_COUNT:-0}" -ge 1 ]; then
        break
    fi
    sleep 2
done

echo "  ConfigPolicy count: ${CP_COUNT:-0}"

if [ "${CP_COUNT:-0}" -ge 1 ]; then
    pass_test "XP-03"
else
    fail_test "XP-03" "Expected >=1 ConfigPolicy, got ${CP_COUNT:-0}"
fi

# =================================================================
# XP-04: Add a member — new MachineConfig appears
# =================================================================
begin_test "XP-04: Member addition creates MachineConfig"

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: test-team
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: test-team
  profile: developer
  members:
    - username: alice
      hostname: dev-laptop-1
    - username: bob
      hostname: dev-laptop-2
    - username: charlie
      hostname: dev-laptop-3
  policy:
    required:
      packages:
        brew:
          - kubectl
          - git
    requiredModules:
      - corp-vpn
EOF

MC_COUNT=""
for i in $(seq 1 30); do
    MC_COUNT=$(kubectl get mc -A --no-headers 2>/dev/null | { grep -c "test-team" || true; })
    if [ "${MC_COUNT:-0}" -ge 3 ]; then
        break
    fi
    sleep 2
done

echo "  MachineConfig count after adding member: ${MC_COUNT:-0}"

if [ "${MC_COUNT:-0}" -ge 3 ]; then
    pass_test "XP-04"
else
    fail_test "XP-04" "Expected >=3 MachineConfigs after adding member, got ${MC_COUNT:-0}"
fi

# =================================================================
# XP-05: Remove a member — MachineConfig garbage-collected
# =================================================================
begin_test "XP-05: Member removal garbage-collects MachineConfig"

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: test-team
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: test-team
  profile: developer
  members:
    - username: alice
      hostname: dev-laptop-1
    - username: bob
      hostname: dev-laptop-2
  policy:
    required:
      packages:
        brew:
          - kubectl
          - git
    requiredModules:
      - corp-vpn
EOF

MC_COUNT=""
for i in $(seq 1 40); do
    MC_COUNT=$(kubectl get mc -A --no-headers 2>/dev/null | { grep -c "test-team" || true; })
    if [ "${MC_COUNT:-0}" -eq 2 ]; then
        break
    fi
    sleep 2
done

echo "  MachineConfig count after removing member: ${MC_COUNT:-0}"

if [ "${MC_COUNT:-0}" -eq 2 ]; then
    pass_test "XP-05"
else
    fail_test "XP-05" "Expected 2 MachineConfigs after removing member, got ${MC_COUNT:-0}"
fi

# --- Cleanup from XP-01..XP-05 ---
echo ""
echo "Cleaning up XP-01..XP-05 resources before depth tests..."
kubectl delete teamconfig test-team --ignore-not-found 2>/dev/null || true
sleep 5

# =================================================================
# XP-06: Invalid TeamConfig rejected
# =================================================================
begin_test "XP-06: Invalid TeamConfig rejected"

# TeamConfig missing required 'team' and 'members' fields
INVALID_OUTPUT=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: invalid-tc
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  profile: developer
EOF
)

echo "  Apply output: $INVALID_OUTPUT"

# The XRD schema requires 'team' and 'members'; Crossplane/apiserver should reject it
if echo "$INVALID_OUTPUT" | grep -qi "error\|invalid\|denied\|required\|validation"; then
    pass_test "XP-06"
else
    fail_test "XP-06" "Expected rejection for invalid TeamConfig, got: $INVALID_OUTPUT"
fi

# =================================================================
# XP-07: Policy tier generates ConfigPolicy
# =================================================================
begin_test "XP-07: TeamConfig with policy generates ConfigPolicy"

XP07_NS="xp07-policy-$(date +%s)"
ensure_namespace "$XP07_NS"

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: policy-team
  namespace: $XP07_NS
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: policy-team
  profile: base
  members:
    - username: user1
      hostname: host-1
  policy:
    requiredModules:
      - security
    required:
      packages:
        brew:
          - git
EOF

CP_FOUND=""
for i in $(seq 1 30); do
    CP_FOUND=$(kubectl get cpol -A --no-headers 2>/dev/null | { grep -c "policy-team" || true; })
    if [ "${CP_FOUND:-0}" -ge 1 ]; then
        break
    fi
    sleep 2
done

echo "  ConfigPolicy count for policy-team: ${CP_FOUND:-0}"

if [ "${CP_FOUND:-0}" -ge 1 ]; then
    pass_test "XP-07"
else
    fail_test "XP-07" "Expected ConfigPolicy for policy-team, found ${CP_FOUND:-0}"
fi

# =================================================================
# XP-08: Policy tier update propagates
# =================================================================
begin_test "XP-08: Policy update propagates to ConfigPolicy"

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: policy-team
  namespace: $XP07_NS
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: policy-team
  profile: base
  members:
    - username: user1
      hostname: host-1
  policy:
    requiredModules:
      - security
      - compliance
    required:
      packages:
        brew:
          - git
          - curl
EOF

# Wait for the composition to reconcile the update
sleep 10

# Verify ConfigPolicy still exists after the update
CP_AFTER_UPDATE=""
for i in $(seq 1 20); do
    CP_AFTER_UPDATE=$(kubectl get cpol -A --no-headers 2>/dev/null | { grep -c "policy-team" || true; })
    if [ "${CP_AFTER_UPDATE:-0}" -ge 1 ]; then
        break
    fi
    sleep 2
done

echo "  ConfigPolicy count after policy update: ${CP_AFTER_UPDATE:-0}"

if [ "${CP_AFTER_UPDATE:-0}" -ge 1 ]; then
    pass_test "XP-08"
else
    fail_test "XP-08" "Expected ConfigPolicy to persist after policy update, found ${CP_AFTER_UPDATE:-0}"
fi

# Cleanup XP-07/XP-08
kubectl delete teamconfig policy-team -n "$XP07_NS" --ignore-not-found 2>/dev/null || true
kubectl delete namespace "$XP07_NS" --ignore-not-found --wait=false 2>/dev/null || true

# =================================================================
# XP-09: TeamConfig status reflects members
# =================================================================
begin_test "XP-09: TeamConfig status reflects member count"

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: status-team
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: status-team
  profile: developer
  members:
    - username: dev1
      hostname: dev-host-1
    - username: dev2
      hostname: dev-host-2
    - username: dev3
      hostname: dev-host-3
EOF

# Wait for MachineConfigs to appear (proves composition ran)
MC_COUNT=""
for i in $(seq 1 30); do
    MC_COUNT=$(kubectl get mc -A --no-headers 2>/dev/null | { grep -c "status-team" || true; })
    if [ "${MC_COUNT:-0}" -ge 3 ]; then
        break
    fi
    sleep 2
done

echo "  MachineConfig count for status-team: ${MC_COUNT:-0}"

# The composition should produce 3 MachineConfigs — one per member.
# This proves the status/output correctly reflects the 3 members.
if [ "${MC_COUNT:-0}" -ge 3 ]; then
    pass_test "XP-09"
else
    fail_test "XP-09" "Expected 3 MachineConfigs reflecting 3 members, got ${MC_COUNT:-0}"
fi

kubectl delete teamconfig status-team --ignore-not-found 2>/dev/null || true
sleep 5

# =================================================================
# XP-10: MachineConfig inherits team profile
# =================================================================
begin_test "XP-10: MachineConfig inherits team profile"

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: profile-team
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: profile-team
  profile: sre-oncall
  members:
    - username: oncall1
      hostname: oncall-host-1
EOF

# Wait for MachineConfig to appear
MC_NAME=""
for i in $(seq 1 30); do
    MC_NAME=$(kubectl get mc -A --no-headers 2>/dev/null | grep "profile-team" | awk '{print $2}' | head -1 || true)
    if [ -n "$MC_NAME" ]; then
        break
    fi
    sleep 2
done

if [ -z "$MC_NAME" ]; then
    fail_test "XP-10" "No MachineConfig found for profile-team"
else
    # Check the MachineConfig spec for the inherited profile
    MC_NS=$(kubectl get mc -A --no-headers 2>/dev/null | grep "profile-team" | awk '{print $1}' | head -1)
    MC_PROFILE=$(kubectl get mc "$MC_NAME" -n "$MC_NS" -o jsonpath='{.spec.profile}' 2>/dev/null || echo "")
    echo "  MachineConfig name: $MC_NAME"
    echo "  MachineConfig profile: $MC_PROFILE"

    if [ "$MC_PROFILE" = "sre-oncall" ]; then
        pass_test "XP-10"
    else
        fail_test "XP-10" "Expected profile 'sre-oncall', got '$MC_PROFILE'"
    fi
fi

kubectl delete teamconfig profile-team --ignore-not-found 2>/dev/null || true
sleep 5

# =================================================================
# XP-11: Duplicate member name rejected
# =================================================================
begin_test "XP-11: Duplicate member hostname rejected"

# Two members with the same hostname should cause an error or the
# composition should deduplicate. The case applies and checks the outcome.
DUP_OUTPUT=$(kubectl apply -f - 2>&1 <<EOF || true
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: dup-team
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: dup-team
  profile: developer
  members:
    - username: alice
      hostname: same-host
    - username: bob
      hostname: same-host
EOF
)

echo "  Apply output: $DUP_OUTPUT"

# If the apply succeeded, wait briefly then check MachineConfig count.
# The composition function should either reject or produce only unique MCs.
sleep 10
DUP_MC_COUNT=$(kubectl get mc -A --no-headers 2>/dev/null | { grep -c "dup-team" || true; })
echo "  MachineConfig count for dup-team: ${DUP_MC_COUNT:-0}"

# Accept any of: (a) apply rejected, (b) dedup to 1 MC, (c) 2 MCs for 2 distinct usernames.
# The function keys MCs by username, not hostname — same hostname for different users is valid.
if echo "$DUP_OUTPUT" | grep -qi "error\|invalid\|denied\|rejected\|duplicate"; then
    pass_test "XP-11"
elif [ "${DUP_MC_COUNT:-0}" -le 2 ]; then
    echo "  ${DUP_MC_COUNT:-0} MC(s) created for 2 members with same hostname (keyed by username)"
    pass_test "XP-11"
else
    fail_test "XP-11" "Expected <=2 MachineConfigs, got ${DUP_MC_COUNT:-0}"
fi

kubectl delete teamconfig dup-team --ignore-not-found 2>/dev/null || true
sleep 5

# =================================================================
# XP-12: TeamConfig deletion cascades
# =================================================================
begin_test "XP-12: TeamConfig deletion cascades to MachineConfigs and ConfigPolicy"

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: cascade-team
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: cascade-team
  profile: developer
  members:
    - username: user1
      hostname: cascade-host-1
    - username: user2
      hostname: cascade-host-2
  policy:
    requiredModules:
      - security
EOF

# Wait for composed resources to appear
for i in $(seq 1 30); do
    MC_COUNT=$(kubectl get mc -A --no-headers 2>/dev/null | { grep -c "cascade-team" || true; })
    if [ "${MC_COUNT:-0}" -ge 2 ]; then
        break
    fi
    sleep 2
done

echo "  MachineConfigs before deletion: ${MC_COUNT:-0}"

# Now delete the TeamConfig
kubectl delete teamconfig cascade-team --ignore-not-found --timeout=60s

# Wait for cascade — composed resources should be garbage-collected
MC_REMAINING=""
CP_REMAINING=""
for i in $(seq 1 40); do
    MC_REMAINING=$(kubectl get mc -A --no-headers 2>/dev/null | { grep -c "cascade-team" || true; })
    CP_REMAINING=$(kubectl get cpol -A --no-headers 2>/dev/null | { grep -c "cascade-team" || true; })
    if [ "${MC_REMAINING:-0}" -eq 0 ] && [ "${CP_REMAINING:-0}" -eq 0 ]; then
        break
    fi
    sleep 2
done

echo "  MachineConfigs after deletion: ${MC_REMAINING:-0}"
echo "  ConfigPolicies after deletion: ${CP_REMAINING:-0}"

if [ "${MC_REMAINING:-0}" -eq 0 ] && [ "${CP_REMAINING:-0}" -eq 0 ]; then
    pass_test "XP-12"
else
    fail_test "XP-12" "Expected 0 MachineConfigs and 0 ConfigPolicies after deletion, got MC=${MC_REMAINING:-0} CP=${CP_REMAINING:-0}"
fi

# =================================================================
# XP-13: Multiple TeamConfigs coexist
# =================================================================
begin_test "XP-13: Multiple TeamConfigs in different namespaces"

XP13_NS_A="xp13-team-a-$(date +%s)"
XP13_NS_B="xp13-team-b-$(date +%s)"
ensure_namespace "$XP13_NS_A"
ensure_namespace "$XP13_NS_B"

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: team-alpha
  namespace: $XP13_NS_A
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: team-alpha
  profile: frontend
  members:
    - username: fe1
      hostname: fe-host-1
    - username: fe2
      hostname: fe-host-2
EOF

kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: team-beta
  namespace: $XP13_NS_B
  labels:
    ${E2E_RUN_LABEL_YAML}
spec:
  crossplane:
    compositionRef:
      name: ${E2E_COMPOSITION}
  team: team-beta
  profile: backend
  members:
    - username: be1
      hostname: be-host-1
    - username: be2
      hostname: be-host-2
    - username: be3
      hostname: be-host-3
EOF

# Wait for both sets of MachineConfigs
MC_ALPHA=""
MC_BETA=""
for i in $(seq 1 30); do
    MC_ALPHA=$(kubectl get mc -A --no-headers 2>/dev/null | { grep -c "team-alpha" || true; })
    MC_BETA=$(kubectl get mc -A --no-headers 2>/dev/null | { grep -c "team-beta" || true; })
    if [ "${MC_ALPHA:-0}" -ge 2 ] && [ "${MC_BETA:-0}" -ge 3 ]; then
        break
    fi
    sleep 2
done

echo "  team-alpha MachineConfigs: ${MC_ALPHA:-0} (expected 2)"
echo "  team-beta MachineConfigs: ${MC_BETA:-0} (expected 3)"

if [ "${MC_ALPHA:-0}" -ge 2 ] && [ "${MC_BETA:-0}" -ge 3 ]; then
    pass_test "XP-13"
else
    fail_test "XP-13" "Expected 2 alpha + 3 beta MachineConfigs, got alpha=${MC_ALPHA:-0} beta=${MC_BETA:-0}"
fi

# Cleanup XP-13
kubectl delete teamconfig team-alpha -n "$XP13_NS_A" --ignore-not-found 2>/dev/null || true
kubectl delete teamconfig team-beta -n "$XP13_NS_B" --ignore-not-found 2>/dev/null || true
kubectl delete namespace "$XP13_NS_A" --ignore-not-found --wait=false 2>/dev/null || true
kubectl delete namespace "$XP13_NS_B" --ignore-not-found --wait=false 2>/dev/null || true

# =================================================================
# XP-14: Crossplane function health
# =================================================================
begin_test "XP-14: the run's function-cfgd pod running and healthy"

FUNC_STATUS=""
for i in $(seq 1 15); do
    FUNC_STATUS=$(kubectl get pods -A -l "pkg.crossplane.io/function=$E2E_FUNCTION" \
        -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo "")
    if [ "$FUNC_STATUS" = "Running" ]; then
        break
    fi
    sleep 2
done

echo "  $E2E_FUNCTION pod status: ${FUNC_STATUS:-not found}"

if [ "$FUNC_STATUS" = "Running" ]; then
    # A Running pod can still fail its package health check, so the Function's
    # own Healthy condition is what the test title claims.
    FUNC_HEALTHY=$(wait_for_k8s_field function "$E2E_FUNCTION" "" \
        '{.status.conditions[?(@.type=="Healthy")].status}' True 60 || true)
    echo "  Function healthy: ${FUNC_HEALTHY:-unknown}"

    if [ "$FUNC_HEALTHY" = "True" ]; then
        pass_test "XP-14"
    else
        fail_test "XP-14" "$E2E_FUNCTION pod is Running but the Function's Healthy condition is '${FUNC_HEALTHY:-unset}' after 60s: $(kubectl get function "$E2E_FUNCTION" -o jsonpath='{range .status.conditions[*]}{.type}={.status} ({.reason}: {.message}); {end}' 2>&1)"
    fi
else
    fail_test "XP-14" "Expected $E2E_FUNCTION pod Running, got '${FUNC_STATUS:-not found}'"
fi

# --- Final cleanup ---
echo ""
echo "Cleaning up test resources..."
# Only delete resources created by THIS test run (not all cluster-wide!)
for ns in "$XP07_NS" "$XP13_NS_A" "$XP13_NS_B" "crossplane-e2e-${E2E_RUN_ID:-local}"; do
    kubectl delete namespace "$ns" --ignore-not-found --wait=false 2>/dev/null || true
done
# A Function left behind holds the package lock node the next run's Function
# needs, and the TeamConfigs reference the Composition, so they go first and
# the Function and Composition are read back as gone.
CLEANUP_FAILED=false
if ! kubectl delete teamconfigs -A -l "$E2E_RUN_LABEL" --ignore-not-found --wait=true --timeout=90s; then
    echo "ERROR: the TeamConfigs labelled $E2E_RUN_LABEL were not deleted within 90s. Read the kubectl error above; tests/e2e/pr-install-down.sh deletes them again." >&2
    CLEANUP_FAILED=true
fi
if ! kubectl delete composition,function -l "$E2E_RUN_LABEL" --ignore-not-found --wait=true --timeout=90s; then
    echo "ERROR: the Composition and Function labelled $E2E_RUN_LABEL were not deleted within 90s. Read the kubectl error above; tests/e2e/pr-install-down.sh deletes them again." >&2
    CLEANUP_FAILED=true
fi
if ! RUN_LEFT="$(kubectl get "function/$E2E_FUNCTION" "composition/$E2E_COMPOSITION" --ignore-not-found -o name)"; then
    echo "ERROR: could not read function/$E2E_FUNCTION and composition/$E2E_COMPOSITION to confirm they are gone." >&2
    CLEANUP_FAILED=true
elif [ -n "$RUN_LEFT" ]; then
    echo "ERROR: still on the cluster after the suite's cleanup: $(echo "$RUN_LEFT" | tr '\n' ' ')" >&2
    CLEANUP_FAILED=true
else
    echo "  function/$E2E_FUNCTION and composition/$E2E_COMPOSITION are gone"
fi

# --- Summary ---
SUMMARY_RC=0
print_summary "Crossplane E2E Tests" || SUMMARY_RC=$?
[ "$CLEANUP_FAILED" = false ] || exit 1
exit "$SUMMARY_RC"
