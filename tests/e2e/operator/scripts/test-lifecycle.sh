# shellcheck shell=bash
# Operator E2E tests: Controller Lifecycle
# Sourced by run-all.sh: do NOT set traps or pipefail here.

echo ""
echo "=== Controller Lifecycle Tests ==="

# Print the holderIdentity of the lease cfgd-operator-leader in the PR
# install's namespace, or nothing when the lease has no holder.
operator_leader() {
    kubectl get lease cfgd-operator-leader -n "$E2E_INSTALL_NS" \
        -o jsonpath='{.spec.holderIdentity}' 2>/dev/null || true
}

# Print the operator pod the leader lease names. When the holder matches no
# pod $E2E_OPERATOR_PODS selects, print the identity and the pod list instead
# and return 1.
operator_leader_pod() {
    local holder pods
    holder="$(operator_leader)"
    pods="$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" -o name 2>&1 || true)"
    if [ -n "$holder" ] && grep -qxF "pod/$holder" <<<"$pods"; then
        printf '%s\n' "$holder"
        return 0
    fi
    printf "lease cfgd-operator-leader holderIdentity '%s' names no pod among the operator pods: %s\n" \
        "$holder" "$(tr '\n' ' ' <<<"${pods:-none}")"
    return 1
}

# =================================================================
# OP-LC-01: Operator metrics endpoint
# =================================================================
begin_test "OP-LC-01: Operator metrics endpoint"

# A MachineConfig change makes the leader reconcile, and its counter has no
# sample until it has counted one, so each attempt touches this case's
# MachineConfig and then scrapes the pod holding the lease. The lease is read
# again every attempt because an operator restart hands it to a new pod.
LC01_LOCAL_PORT=18443
LC01_BODY="$CLI_SCRATCH/op-lc-01-metrics.txt"
LC01_MC="e2e-lc01-mc-${E2E_RUN_ID}"
LC01_TRIES="${E2E_METRICS_TRIES:-12}"
LC01_PASSED=false
LC01_REASON=""
LC01_LEADER=""
LC01_ATTEMPT=0
# lc01_attempt: one touch-and-scrape; LC01_REASON says why a failed one failed.
lc01_attempt() {
    LC01_ATTEMPT=$RETRY_ATTEMPT
    if ! kubectl annotate machineconfig "$LC01_MC" -n "$E2E_NAMESPACE" \
        "cfgd.io/e2e-touch=$LC01_ATTEMPT" --overwrite > /dev/null; then
        LC01_REASON="Could not annotate MachineConfig $LC01_MC to drive a reconcile (kubectl output above)"
        return 1
    fi
    if ! LC01_LEADER="$(operator_leader_pod)"; then
        LC01_REASON="$LC01_LEADER"
        LC01_LEADER=""
        return 1
    fi
    if ! LC01_PF_PID=$(port_forward "$E2E_INSTALL_NS" "pod/$LC01_LEADER" "$LC01_LOCAL_PORT" 8443); then
        LC01_REASON="Port-forward to pod/$LC01_LEADER never opened localhost:$LC01_LOCAL_PORT (kubectl output above)"
        return 1
    fi
    read -r LC01_CODE LC01_CONTENT_TYPE \
        <<<"$(http_get_to_file "http://localhost:$LC01_LOCAL_PORT/metrics" "$LC01_BODY")"
    stop_port_forward "$LC01_PF_PID"
    if [[ "$LC01_CODE" == 2* ]] && metric_sample_lines cfgd_operator_reconciliations "$LC01_BODY" > /dev/null; then
        return 0
    fi
    LC01_REASON="pod/$LC01_LEADER served no cfgd_operator_reconciliations sample: $(http_evidence "$LC01_CODE" "${LC01_CONTENT_TYPE:-}" "$LC01_BODY")"
    return 1
}
if ! [[ $LC01_TRIES =~ ^[1-9][0-9]*$ ]]; then
    fail_test "OP-LC-01" "E2E_METRICS_TRIES must be a positive integer, got '$LC01_TRIES'"
elif ! kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF; then
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: ${LC01_MC}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: e2e-lc01-host
  profile: dev-workstation
  packages:
    - name: vim
  systemSettings: {}
EOF
    fail_test "OP-LC-01" "Could not create MachineConfig $LC01_MC to drive a reconcile (kubectl output above)"
else
    if retry_tries "$LC01_TRIES" 5 lc01_attempt; then
        LC01_PASSED=true
    fi
    echo "  Leader pod: ${LC01_LEADER:-unresolved}, attempts: $LC01_ATTEMPT of $LC01_TRIES"

    if $LC01_PASSED; then
        pass_test "OP-LC-01"
    else
        LC01_POD_STATE=""
        [ -z "$LC01_LEADER" ] || LC01_POD_STATE=" Pod (age, restarts): $(kubectl get pod "$LC01_LEADER" -n "$E2E_INSTALL_NS" -o wide 2>&1 | tr '\n' ' ' || true)"
        fail_test "OP-LC-01" "No reconciliation sample after $LC01_TRIES attempts touching MachineConfig $LC01_MC.$LC01_POD_STATE Last attempt: $LC01_REASON"
    fi
fi
kubectl delete machineconfig "$LC01_MC" -n "$E2E_NAMESPACE" --ignore-not-found > /dev/null 2>&1 || true

# =================================================================
# OP-LC-02: Leader election lease
# =================================================================
begin_test "OP-LC-02: Leader election lease"

# The operator does not release the lease on shutdown, so after an operator
# pod is deleted the holder names that pod until the lease expires (15s by
# default) and a new pod takes it. The default attempts span 25s, past one
# lease duration plus the 2s retry period.
LC02_TRIES="${E2E_LEASE_TRIES:-6}"
LC02_ATTEMPT=0
LC02_PASSED=false
LC02_LEADER=""
if ! [[ $LC02_TRIES =~ ^[1-9][0-9]*$ ]]; then
    fail_test "OP-LC-02" "E2E_LEASE_TRIES must be a positive integer, got '$LC02_TRIES'"
else
    lc02_attempt() {
        LC02_LEADER="$(operator_leader_pod)"
    }
    if retry_tries "$LC02_TRIES" 5 lc02_attempt; then
        LC02_PASSED=true
    fi
    LC02_ATTEMPT=$RETRY_ATTEMPT
    if $LC02_PASSED; then
        echo "  Lease holder: pod/$LC02_LEADER (attempt $LC02_ATTEMPT of $LC02_TRIES)"
        pass_test "OP-LC-02"
    else
        fail_test "OP-LC-02" "After $LC02_TRIES attempts: $LC02_LEADER"
    fi
fi

# =================================================================
# OP-LC-03: Graceful shutdown recovery
# =================================================================
begin_test "OP-LC-03: Graceful shutdown recovery"

# Get current operator pod name
OLD_POD=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")

OLD_UID=""
[ -z "$OLD_POD" ] || OLD_UID=$(kubectl get pod "$OLD_POD" -n "$E2E_INSTALL_NS" \
    -o jsonpath='{.metadata.uid}' 2>/dev/null || echo "")

echo "  Current operator pod: ${OLD_POD:-unknown} (uid ${OLD_UID:-unknown})"

if [ -z "$OLD_POD" ] || [ -z "$OLD_UID" ]; then
    fail_test "OP-LC-03" "No operator pod found"
else
    # Delete the pod
    kubectl delete pod "$OLD_POD" -n "$E2E_INSTALL_NS" --wait=false --ignore-not-found 2>/dev/null

    # Wait for the deployment to become available again
    echo "  Waiting for operator deployment to recover..."
    wait_for_deployment "$E2E_INSTALL_NS" "$E2E_OPERATOR_DEPLOY" 120

    # A pod name can be reused, so the UID is what tells a replacement apart.
    NEW_POD=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
    NEW_UID=""
    [ -z "$NEW_POD" ] || NEW_UID=$(kubectl get pod "$NEW_POD" -n "$E2E_INSTALL_NS" \
        -o jsonpath='{.metadata.uid}' 2>/dev/null || echo "")

    echo "  New operator pod: ${NEW_POD:-unknown} (uid ${NEW_UID:-unknown})"

    if [ -z "$NEW_UID" ]; then
        fail_test "OP-LC-03" "Operator pod did not recover after deletion"
    elif [ "$NEW_UID" = "$OLD_UID" ]; then
        fail_test "OP-LC-03" "Operator pod $NEW_POD still has the deleted pod's uid $OLD_UID"
    else
        pass_test "OP-LC-03"
    fi
fi

# Wait for webhook endpoints to be available after OP-LC-03 pod restart.
# The deployment becomes Available before the new pod registers webhook
# endpoints, so kubectl apply can fail with "no endpoints available".
echo "  Waiting for webhook readiness after pod restart..."
kubectl wait --for=condition=Ready pod -l "$E2E_OPERATOR_PODS" \
    -n "$E2E_INSTALL_NS" --timeout=60s 2>/dev/null || true
wait_for_service_endpoints "$E2E_INSTALL_NS" "$E2E_WEBHOOK_SVC" 60 || true

# =================================================================
# OP-LC-04: MachineConfig reconcile loop
# =================================================================
begin_test "OP-LC-04: MachineConfig reconcile loop"

# Retry apply: the webhook endpoint may still be registering after the OP-LC-03 restart
lc04_apply() {
    kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF 2>/dev/null && return 0
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: e2e-lc-mc-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: e2e-lc-host
  profile: dev-workstation
  packages:
    - name: vim
    - name: git
  systemSettings: {}
EOF
    echo "  Webhook not ready (attempt $RETRY_ATTEMPT of 6)"
    return 1
}
LC04_APPLIED=false
if retry_tries 6 5 lc04_apply; then
    LC04_APPLIED=true
fi

if [ "$LC04_APPLIED" = "false" ]; then
    fail_test "OP-LC-04" "Failed to create MachineConfig after retries (webhook unavailable)"
fi

echo "  Waiting for Reconciled condition..."
RECONCILED=$(wait_for_k8s_field machineconfig "e2e-lc-mc-${E2E_RUN_ID}" "$E2E_NAMESPACE" \
    '{range .status.conditions[?(@.type=="Reconciled")]}{.status}/{.reason}{end}' "True/ReconcileSuccess" 60) || true

echo "  Reconciled condition: ${RECONCILED:-not set}"

if [ "$LC04_APPLIED" = "false" ]; then
    :
elif [ "$RECONCILED" = "True/ReconcileSuccess" ]; then
    pass_test "OP-LC-04"
else
    fail_test "OP-LC-04" "Expected Reconciled True/ReconcileSuccess, got '${RECONCILED:-none}'"
fi

# =================================================================
# OP-LC-05: ConfigPolicy re-evaluation on MC update
# =================================================================
begin_test "OP-LC-05: ConfigPolicy re-evaluation"

# Create a ConfigPolicy requiring curl
kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: e2e-lc-policy-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: curl
EOF

# Wait for initial policy evaluation
echo "  Waiting for initial ConfigPolicy evaluation..."
CP_STATUS=$(wait_for_k8s_field configpolicy "e2e-lc-policy-${E2E_RUN_ID}" "$E2E_NAMESPACE" \
    '{.status.nonCompliantCount}' "" 60) || true

INITIAL_NON_COMPLIANT=$(kubectl get configpolicy "e2e-lc-policy-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
    -o jsonpath='{.status.nonCompliantCount}' 2>/dev/null || echo "")

echo "  Initial nonCompliantCount: ${INITIAL_NON_COMPLIANT:-not set}"

# Update the MC to include curl (making it compliant)
LC05_PATCH_RC=0
kubectl patch machineconfig "e2e-lc-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --type=merge \
    -p '{"spec":{"packages":[{"name":"vim"},{"name":"git"},{"name":"curl"}]}}' \
    > /dev/null 2>&1 || LC05_PATCH_RC=$?
echo "  MC spec patch rc: $LC05_PATCH_RC"

# Wait for the policy to re-evaluate: poll until compliantCount changes or appears
echo "  Waiting for ConfigPolicy re-evaluation after MC update..."
lc05_compliant() {
    COMPLIANT_AFTER=$(kubectl get configpolicy "e2e-lc-policy-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
        -o jsonpath='{.status.compliantCount}' 2>/dev/null || echo "")
    [ -n "$COMPLIANT_AFTER" ] && [ "${COMPLIANT_AFTER:-0}" -ge 1 ] 2>/dev/null
}
COMPLIANT_AFTER=""
wait_until 60 1 "e2e-lc-policy-${E2E_RUN_ID} to count a compliant MachineConfig" lc05_compliant || true

echo "  compliantCount after update: ${COMPLIANT_AFTER:-not set}"

# The verdict reads the facts from AFTER the update: `CP_STATUS` is the initial
# evaluation, captured before the patch, so a policy that never re-evaluated
# passed on it.
if [ "$LC05_PATCH_RC" -eq 0 ] && [ -n "$CP_STATUS" ] && [ "${COMPLIANT_AFTER:-0}" -ge 1 ] 2>/dev/null; then
    pass_test "OP-LC-05"
else
    fail_test "OP-LC-05" "ConfigPolicy was not re-evaluated after MC update (patch rc=${LC05_PATCH_RC}, compliantCount after=${COMPLIANT_AFTER:-unset})"
fi

# =================================================================
# OP-LC-06: DriftAlert lifecycle
# =================================================================
begin_test "OP-LC-06: DriftAlert lifecycle"

kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: DriftAlert
metadata:
  name: e2e-lc-drift-${E2E_RUN_ID}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  deviceId: e2e-lc-host
  machineConfigRef:
    name: e2e-lc-mc-${E2E_RUN_ID}
  severity: High
  driftDetails:
    - field: sysctl.net.ipv4.ip_forward
      expected: "1"
      actual: "0"
EOF

# An unresolved, unacknowledged High alert: the controller writes these three
# conditions in this order, and sets the MachineConfig's DriftDetected in the
# same pass.
LC06_WANT="Acknowledged=False/NotAcknowledged Resolved=False/DriftActive Escalated=True/SeverityThreshold"
echo "  Waiting for DriftAlert status conditions..."
DA_STATUS=$(wait_for_k8s_field driftalert "e2e-lc-drift-${E2E_RUN_ID}" "$E2E_NAMESPACE" \
    '{range .status.conditions[*]}{.type}={.status}/{.reason} {end}' "$LC06_WANT " 90) || true
MC_DRIFT=$(wait_for_k8s_field machineconfig "e2e-lc-mc-${E2E_RUN_ID}" "$E2E_NAMESPACE" \
    '{range .status.conditions[?(@.type=="DriftDetected")]}{.status}/{.reason}{end}' "True/DriftActive" 30) || true

echo "  DriftAlert conditions: ${DA_STATUS:-not set}"
echo "  MC DriftDetected: ${MC_DRIFT:-not set}"

if [ "$DA_STATUS" = "$LC06_WANT " ] && [ "$MC_DRIFT" = "True/DriftActive" ]; then
    pass_test "OP-LC-06"
else
    # The DriftAlert controller is event-driven and sets both the MC
    # DriftDetected condition and the DriftAlert status in a single reconcile
    # pass, so an empty status after the poll window means that pass never
    # completed (missed watch event, transient patch error → backoff, or
    # informer-cache lag under load). Without this dump the failure is a black
    # box; capture the resource state + drift-related operator logs so a
    # recurrence is diagnosable from more than a bare "not set".
    echo "  --- OP-LC-06 diagnostics ---"
    echo "  DriftAlert object:"
    kubectl get driftalert "e2e-lc-drift-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
        -o yaml 2>&1 | sed 's/^/    /' | head -60 || true
    echo "  MachineConfig status:"
    kubectl get machineconfig "e2e-lc-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" \
        -o jsonpath='{.status}' 2>&1 | sed 's/^/    /' || true
    echo ""
    echo "  Operator logs (drift-related, last 40):"
    kubectl logs -n "$E2E_INSTALL_NS" deployment/"$E2E_OPERATOR_DEPLOY" --tail=300 2>/dev/null \
        | grep -iE "drift|e2e-lc-drift-${E2E_RUN_ID}|e2e-lc-mc-${E2E_RUN_ID}" \
        | tail -40 | sed 's/^/    /' || true
    fail_test "OP-LC-06" "Expected DriftAlert conditions '$LC06_WANT' and MC DriftDetected True/DriftActive, got '${DA_STATUS:-none}' and '${MC_DRIFT:-none}'"
fi

# =================================================================
# OP-LC-07: Module CRD status
# =================================================================
begin_test "OP-LC-07: Module CRD status"

LC07_CREATE_OUTPUT=$(kubectl apply -f - 2>&1 <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: e2e-lc-module-${E2E_RUN_ID}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  packages:
    - name: htop
  files:
    - source: bin/check.sh
      target: bin/check.sh
  ociArtifact: "${REGISTRY}/cfgd-e2e/lc-module:v1.0"
EOF
) && LC07_CREATE_RC=0 || LC07_CREATE_RC=$?

if [ "$LC07_CREATE_RC" -ne 0 ]; then
    # Webhook rejects unsigned modules, which is correct behavior
    if echo "$LC07_CREATE_OUTPUT" | grep -q "unsigned modules"; then
        echo "  Webhook correctly rejects unsigned modules (expected behavior)"
        pass_test "OP-LC-07"
    else
        fail_test "OP-LC-07" "Module creation failed: $LC07_CREATE_OUTPUT"
    fi
else
    # No signature block: Verified is False/NotSigned. The webhook admitted an
    # unsigned module, so no policy disallows unsigned ones and the reference is
    # available. A policy another run applies meanwhile can withhold it for one
    # requeue, which the 90s wait outlasts.
    LC07_WANT="false Available=True/ArtifactAvailable Verified=False/NotSigned"
    echo "  Waiting for Module status..."
    MOD_STATUS=$(wait_for_k8s_field module "e2e-lc-module-${E2E_RUN_ID}" "" \
        '{.status.verified}{range .status.conditions[*]} {.type}={.status}/{.reason}{end}' "$LC07_WANT" 90) || true

    echo "  verified and conditions: ${MOD_STATUS:-not set}"

    if [ "$MOD_STATUS" = "$LC07_WANT" ]; then
        pass_test "OP-LC-07"
    else
        fail_test "OP-LC-07" "Expected '$LC07_WANT', got '${MOD_STATUS:-none}'"
    fi
fi

# =================================================================
# OP-LC-08: Health probes
# =================================================================
begin_test "OP-LC-08: Health probes"

# Wait for operator to be fully ready (OP-LC-03 restarts the pod)
kubectl wait --for=condition=available deployment/"$E2E_OPERATOR_DEPLOY" \
    -n "$E2E_INSTALL_NS" --timeout=60s 2>/dev/null || true
kubectl wait --for=condition=Ready pod -l "$E2E_OPERATOR_PODS" \
    -n "$E2E_INSTALL_NS" --timeout=60s 2>/dev/null || true

# Get the newest running operator pod for port-forward
LC08_POD=$(kubectl get pods -n "$E2E_INSTALL_NS" -l "$E2E_OPERATOR_PODS" \
    --sort-by=.metadata.creationTimestamp --field-selector=status.phase=Running \
    -o jsonpath='{.items[-1:].metadata.name}' 2>/dev/null || echo "")

if [ -z "$LC08_POD" ]; then
    fail_test "OP-LC-08" "No operator pod found for health probe check"
else
    LC08_LOCAL_PORT=18181
    LC08_PF_PID=$(port_forward "$E2E_INSTALL_NS" "pod/$LC08_POD" "$LC08_LOCAL_PORT" 8081) || LC08_PF_PID=""

    HEALTHZ_CODE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "http://localhost:$LC08_LOCAL_PORT/healthz" 2>/dev/null) || HEALTHZ_CODE="000"
    # The pod OP-LC-03 restarted answers 503 on /readyz until its first
    # reconcile completes, so the case waits for 200 before judging it.
    lc08_ready() {
        READYZ_CODE=$(curl -s -o /dev/null -w '%{http_code}' --max-time 5 "http://localhost:$LC08_LOCAL_PORT/readyz" 2>/dev/null) || READYZ_CODE="000"
        [ "$READYZ_CODE" = "200" ]
    }
    READYZ_CODE="000"
    wait_until 30 1 "operator /readyz 200" lc08_ready || true

    if [ -n "$LC08_PF_PID" ]; then stop_port_forward "$LC08_PF_PID"; fi

    echo "  /healthz: HTTP $HEALTHZ_CODE"
    echo "  /readyz:  HTTP $READYZ_CODE"

    if [ "$HEALTHZ_CODE" = "200" ] && [ "$READYZ_CODE" = "200" ]; then
        pass_test "OP-LC-08"
    else
        fail_test "OP-LC-08" "Health probes failed: /healthz=$HEALTHZ_CODE /readyz=$READYZ_CODE"
    fi
fi

# --- Clean up lifecycle test resources ---
echo ""
echo "Cleaning up lifecycle test resources..."
kubectl delete machineconfig "e2e-lc-mc-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true
kubectl delete configpolicy "e2e-lc-policy-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true
kubectl delete driftalert "e2e-lc-drift-${E2E_RUN_ID}" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null || true
kubectl delete module "e2e-lc-module-${E2E_RUN_ID}" --ignore-not-found 2>/dev/null || true
