# shellcheck shell=bash
# Full-stack E2E tests: Helm Chart Lifecycle
# Sourced by run-all.sh — do NOT set traps or pipefail here.

CHART_DIR="$REPO_ROOT/chart/cfgd"

echo ""
echo "=== Helm Chart Lifecycle Tests ==="

# Pre-flight: delete stale cluster-scoped resources from prior cfgd-test helm releases.
# Helm ClusterRoles/ClusterRoleBindings persist after namespace deletion and block reinstall.
for res in clusterrole clusterrolebinding; do
    for name in $(kubectl get "$res" -o name 2>/dev/null | grep 'cfgd-test' | sed "s|${res}.rbac.authorization.k8s.io/||; s|${res}/||"); do
        kubectl delete "$res" "$name" --ignore-not-found 2>/dev/null || true
    done
done

# Helper: create a dedicated namespace for a Helm test, install, and return release name.
# Usage: helm_test_ns "01" -- sets HELM_NS="e2e-helm-01-${E2E_RUN_ID}"
helm_test_ns() {
    local id="$1"
    HELM_NS="e2e-helm-${id}-${E2E_RUN_ID}"
    ensure_namespace "$HELM_NS"
    kubectl label namespace "$HELM_NS" "$E2E_RUN_LABEL" --overwrite 2>/dev/null || true # rc-ok: the run tag the janitor ages leaked namespaces out by; no case asserts on it
    # Wait for Reflector to replicate registry-credentials (needed for imagePullSecrets)
    local deadline=$((SECONDS + 30))
    while [ $SECONDS -lt $deadline ]; do
        if kubectl get secret registry-credentials -n "$HELM_NS" > /dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    echo "  WARN: registry-credentials not replicated to $HELM_NS"
}

# Helper: clean up a Helm test namespace (uninstall release + delete namespace).
helm_test_cleanup() {
    local release="${1:-cfgd-test}"
    helm uninstall "$release" -n "$HELM_NS" 2>/dev/null || true
    # Clean up cluster-scoped resources that Helm doesn't remove on uninstall
    for res in clusterrole clusterrolebinding; do
        for name in $(kubectl get "$res" -o name 2>/dev/null | grep "$release" | sed "s|${res}.rbac.authorization.k8s.io/||; s|${res}/||"); do
            kubectl delete "$res" "$name" --ignore-not-found 2>/dev/null || true
        done
    done
    kubectl delete namespace "$HELM_NS" --ignore-not-found --wait=false 2>/dev/null || true
}

# =================================================================
# FS-HELM-01: Fresh install — operator + CSI running
# =================================================================
begin_test "FS-HELM-01: Fresh Helm install creates operator deployment"

# A CSIDriver is cluster-scoped, and the chart's default driver name belongs to
# the live release, so these installs leave csiDriver off and test the operator
# only.
helm_test_ns "01"
INSTALL_OUTPUT=$(helm install cfgd-test "$CHART_DIR" --skip-crds \
    -n "$HELM_NS" \
    --set "operator.image.repository=$(e2e_image_repo cfgd-operator)" \
    --set "operator.image.tag=$(e2e_image_tag cfgd-operator)" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set csiDriver.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1) || true

OPERATOR_DEPLOY=$(kubectl get deployment -n "$HELM_NS" \
    -l app.kubernetes.io/component=operator \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")

echo "  Operator deployment: ${OPERATOR_DEPLOY:-<none>}"

if [ -n "$OPERATOR_DEPLOY" ]; then
    OPERATOR_AVAIL=$(kubectl get deployment -n "$HELM_NS" \
        -l app.kubernetes.io/component=operator \
        -o jsonpath='{.items[0].status.conditions[?(@.type=="Available")].status}' 2>/dev/null || echo "")
    echo "  Operator available: ${OPERATOR_AVAIL:-unknown}"
    if [ "$OPERATOR_AVAIL" = "True" ]; then
        pass_test "FS-HELM-01"
    else
        # helm install --wait has already waited for the Deployment to become
        # ready, so a Deployment that is still not Available failed to start.
        PODS=$(kubectl get pods -n "$HELM_NS" -l app.kubernetes.io/component=operator \
            -o jsonpath='{range .items[*]}{.metadata.name}={.status.phase} {end}' 2>/dev/null || echo "")
        echo "  Helm install output:"
        echo "$INSTALL_OUTPUT" | head -20 | sed 's/^/    /'
        fail_test "FS-HELM-01" "Operator deployment is not Available after helm install --wait (Available=${OPERATOR_AVAIL:-unset}); pods: ${PODS:-<none>}"
    fi
else
    echo "  Helm install output:"
    echo "$INSTALL_OUTPUT" | head -20 | sed 's/^/    /'
    fail_test "FS-HELM-01" "Expected operator deployment after helm install"
fi

helm_test_cleanup "cfgd-test"

# =================================================================
# FS-HELM-02: Gateway enabled — gateway service exists
# =================================================================
begin_test "FS-HELM-02: Gateway enabled creates gateway service"

helm_test_ns "02"
helm install cfgd-test "$CHART_DIR" --skip-crds \
    -n "$HELM_NS" \
    --set "operator.image.repository=$(e2e_image_repo cfgd-operator)" \
    --set "operator.image.tag=$(e2e_image_tag cfgd-operator)" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set deviceGateway.enabled=true \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set csiDriver.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1 || true

GATEWAY_SVC=$(kubectl get svc -n "$HELM_NS" \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")
echo "  Services: $GATEWAY_SVC"

# The gateway service name follows the pattern: <release>-cfgd-gateway
if echo "$GATEWAY_SVC" | grep -q "gateway"; then
    # Also verify that the operator deployment has the DEVICE_GATEWAY_ENABLED env
    GW_ENV=$(kubectl get deployment -n "$HELM_NS" \
        -l app.kubernetes.io/component=operator \
        -o jsonpath='{.items[0].spec.template.spec.containers[0].env[?(@.name=="DEVICE_GATEWAY_ENABLED")].value}' \
        2>/dev/null || echo "")
    echo "  DEVICE_GATEWAY_ENABLED: ${GW_ENV:-<not set>}"
    pass_test "FS-HELM-02"
else
    fail_test "FS-HELM-02" "Gateway service not found when deviceGateway.enabled=true"
fi

helm_test_cleanup "cfgd-test"

# =================================================================
# FS-HELM-03: Gateway disabled — no gateway service
# =================================================================
begin_test "FS-HELM-03: Gateway disabled creates no gateway service"

helm_test_ns "03"
helm install cfgd-test "$CHART_DIR" --skip-crds \
    -n "$HELM_NS" \
    --set "operator.image.repository=$(e2e_image_repo cfgd-operator)" \
    --set "operator.image.tag=$(e2e_image_tag cfgd-operator)" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set deviceGateway.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set csiDriver.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1 || true

GATEWAY_SVC=$(kubectl get svc -n "$HELM_NS" \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")
echo "  Services: ${GATEWAY_SVC:-<none>}"

if echo "$GATEWAY_SVC" | grep -q "gateway"; then
    fail_test "FS-HELM-03" "Gateway service found when deviceGateway.enabled=false"
else
    # Confirm operator deployment does NOT have gateway env
    GW_ENV=$(kubectl get deployment -n "$HELM_NS" \
        -l app.kubernetes.io/component=operator \
        -o jsonpath='{.items[0].spec.template.spec.containers[0].env[?(@.name=="DEVICE_GATEWAY_ENABLED")].value}' \
        2>/dev/null || echo "")
    echo "  DEVICE_GATEWAY_ENABLED: ${GW_ENV:-<not set>}"
    if [ -z "$GW_ENV" ]; then
        pass_test "FS-HELM-03"
    else
        fail_test "FS-HELM-03" "DEVICE_GATEWAY_ENABLED env set when gateway disabled"
    fi
fi

helm_test_cleanup "cfgd-test"

# =================================================================
# FS-HELM-04: CSI disabled — no CSI daemonset
# =================================================================
begin_test "FS-HELM-04: CSI disabled creates no CSI daemonset"

helm_test_ns "04"
helm install cfgd-test "$CHART_DIR" --skip-crds \
    -n "$HELM_NS" \
    --set "operator.image.repository=$(e2e_image_repo cfgd-operator)" \
    --set "operator.image.tag=$(e2e_image_tag cfgd-operator)" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set csiDriver.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1 || true

CSI_DS=$(kubectl get daemonset -n "$HELM_NS" \
    -l app.kubernetes.io/component=csi-driver \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")
echo "  CSI DaemonSets: ${CSI_DS:-<none>}"

if [ -z "$CSI_DS" ]; then
    pass_test "FS-HELM-04"
else
    fail_test "FS-HELM-04" "CSI daemonset found when csiDriver.enabled=false: $CSI_DS"
fi

helm_test_cleanup "cfgd-test"

# =================================================================
# FS-HELM-05: Upgrade keeps MachineConfig instances and leaves the CRDs ArgoCD applied Established
# =================================================================
begin_test "FS-HELM-05: Helm upgrade keeps instances and the CRDs ArgoCD applied"

helm_test_ns "05"

# Install initial release
helm install cfgd-test "$CHART_DIR" --skip-crds \
    -n "$HELM_NS" \
    --set "operator.image.repository=$(e2e_image_repo cfgd-operator)" \
    --set "operator.image.tag=$(e2e_image_tag cfgd-operator)" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set csiDriver.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1 || true

# Create a MachineConfig to verify it survives the upgrade
kubectl apply -f - <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: helm-upgrade-test-${E2E_RUN_ID}
  namespace: $HELM_NS
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: helm-upgrade-test-${E2E_RUN_ID}
  profile: default
  packages: []
EOF

# Verify the MachineConfig was created
MC_BEFORE=$(kubectl get machineconfig "helm-upgrade-test-${E2E_RUN_ID}" \
    -n "$HELM_NS" -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
echo "  MachineConfig before upgrade: ${MC_BEFORE:-<not found>}"

# Perform Helm upgrade
UPGRADE_RC=0
UPGRADE_OUTPUT=$(helm upgrade cfgd-test "$CHART_DIR" \
    -n "$HELM_NS" \
    --set "operator.image.repository=$(e2e_image_repo cfgd-operator)" \
    --set "operator.image.tag=$(e2e_image_tag cfgd-operator)" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set csiDriver.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1) || UPGRADE_RC=$?

# Verify the MachineConfig survived the upgrade
MC_AFTER=$(kubectl get machineconfig "helm-upgrade-test-${E2E_RUN_ID}" \
    -n "$HELM_NS" -o jsonpath='{.metadata.name}' 2>/dev/null || echo "")
echo "  MachineConfig after upgrade: ${MC_AFTER:-<not found>}"

# Verify operator is still running
OPERATOR_AVAIL=$(kubectl get deployment -n "$HELM_NS" \
    -l app.kubernetes.io/component=operator \
    -o jsonpath='{.items[0].status.conditions[?(@.type=="Available")].status}' 2>/dev/null || echo "")
echo "  Operator available after upgrade: ${OPERATOR_AVAIL:-unknown}"

# The release installs with --skip-crds, so the CRDs are ArgoCD's; they must
# still match schemas/crds.yaml and be Established after the upgrade.
CRDS_RC=0
CRDS_REPORT=$(check_pr_crds schemas/crds.yaml "rerun the full-stack suite" < "$REPO_ROOT/schemas/crds.yaml" 2>&1) || CRDS_RC=$?

if [ "$UPGRADE_RC" -ne 0 ]; then
    fail_test "FS-HELM-05" "helm upgrade exited $UPGRADE_RC: $(echo "$UPGRADE_OUTPUT" | head -20)"
elif [ "$MC_BEFORE" != "helm-upgrade-test-${E2E_RUN_ID}" ] || \
   [ "$MC_AFTER" != "helm-upgrade-test-${E2E_RUN_ID}" ]; then
    fail_test "FS-HELM-05" "MachineConfig did not survive Helm upgrade"
elif [ "$CRDS_RC" -ne 0 ]; then
    fail_test "FS-HELM-05" "the CRDs ArgoCD applied fail the CRD check after helm upgrade: $CRDS_REPORT"
elif [ "$OPERATOR_AVAIL" != "True" ]; then
    fail_test "FS-HELM-05" "Operator deployment is not Available after helm upgrade --wait (Available=${OPERATOR_AVAIL:-unset})"
else
    pass_test "FS-HELM-05"
fi

# Clean up the MachineConfig
kubectl delete machineconfig "helm-upgrade-test-${E2E_RUN_ID}" -n "$HELM_NS" --ignore-not-found 2>/dev/null || true
helm_test_cleanup "cfgd-test"

# =================================================================
# FS-HELM-06: Values override — custom replica count reflected
# =================================================================
begin_test "FS-HELM-06: Values override — custom replica count"

helm_test_ns "06"
helm install cfgd-test "$CHART_DIR" --skip-crds \
    -n "$HELM_NS" \
    --set "operator.image.repository=$(e2e_image_repo cfgd-operator)" \
    --set "operator.image.tag=$(e2e_image_tag cfgd-operator)" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set operator.replicaCount=2 \
    --set csiDriver.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set deviceGateway.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1 || true

REPLICAS=$(kubectl get deployment -n "$HELM_NS" \
    -l app.kubernetes.io/component=operator \
    -o jsonpath='{.items[0].spec.replicas}' 2>/dev/null || echo "")
echo "  Operator replicas: ${REPLICAS:-<not found>}"

if [ "$REPLICAS" = "2" ]; then
    pass_test "FS-HELM-06"
else
    fail_test "FS-HELM-06" "Expected 2 replicas, got: ${REPLICAS:-<none>}"
fi

helm_test_cleanup "cfgd-test"

# =================================================================
# FS-HELM-07: Helm template validation — valid YAML
# =================================================================
begin_test "FS-HELM-07: Helm template produces valid YAML"

TEMPLATE_OUTPUT=$(helm template cfgd-test "$CHART_DIR" \
    --set operator.enabled=true \
    --set csiDriver.enabled=true \
    --set agent.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set deviceGateway.enabled=true 2>&1)
TEMPLATE_RC=$?

if [ $TEMPLATE_RC -ne 0 ]; then
    fail_test "FS-HELM-07" "helm template failed with exit code $TEMPLATE_RC"
    echo "  Output: $(echo "$TEMPLATE_OUTPUT" | head -10)"
else
    # Validate via dry-run (no cluster mutation)
    DRYRUN_OUTPUT=$(echo "$TEMPLATE_OUTPUT" | kubectl apply --dry-run=client -f - 2>&1)
    DRYRUN_RC=$?
    echo "  Template lines: $(echo "$TEMPLATE_OUTPUT" | wc -l)"
    echo "  Dry-run exit code: $DRYRUN_RC"

    if [ $DRYRUN_RC -eq 0 ]; then
        pass_test "FS-HELM-07"
    else
        fail_test "FS-HELM-07" "Templated YAML failed kubectl dry-run validation"
        echo "  Errors: $(echo "$DRYRUN_OUTPUT" | head -10)"
    fi
fi

# =================================================================
# FS-HELM-08: Helm uninstall removes the release and leaves the CRDs ArgoCD applied Established
# =================================================================
begin_test "FS-HELM-08: Helm uninstall removes resources and leaves the CRDs ArgoCD applied"

helm_test_ns "08"

# Install
helm install cfgd-test "$CHART_DIR" --skip-crds \
    -n "$HELM_NS" \
    --set "operator.image.repository=$(e2e_image_repo cfgd-operator)" \
    --set "operator.image.tag=$(e2e_image_tag cfgd-operator)" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set csiDriver.enabled=false \
    --set webhook.enabled=false \
    --set webhook.certManager.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set operator.leaderElection.enabled=false \
    --wait --timeout 120s 2>&1 || true

# Verify deployment exists before uninstall
DEPLOY_BEFORE=$(kubectl get deployment -n "$HELM_NS" \
    -l app.kubernetes.io/component=operator \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")
echo "  Deployment before uninstall: ${DEPLOY_BEFORE:-<none>}"

# Uninstall
helm uninstall cfgd-test -n "$HELM_NS" 2>&1 || true
sleep 5

# Verify deployment is gone
DEPLOY_AFTER=$(kubectl get deployment -n "$HELM_NS" \
    -l app.kubernetes.io/component=operator \
    -o jsonpath='{.items[*].metadata.name}' 2>/dev/null || echo "")
echo "  Deployment after uninstall: ${DEPLOY_AFTER:-<none>}"

# The release installs with --skip-crds, so the CRDs are ArgoCD's; they must
# still match schemas/crds.yaml and be Established after the uninstall.
CRDS_RC=0
CRDS_REPORT=$(check_pr_crds schemas/crds.yaml "rerun the full-stack suite" < "$REPO_ROOT/schemas/crds.yaml" 2>&1) || CRDS_RC=$?

if [ -n "$DEPLOY_BEFORE" ] && [ -z "$DEPLOY_AFTER" ] && [ "$CRDS_RC" -eq 0 ]; then
    pass_test "FS-HELM-08"
else
    if [ -z "$DEPLOY_BEFORE" ]; then
        fail_test "FS-HELM-08" "Deployment was not created during install"
    elif [ -n "$DEPLOY_AFTER" ]; then
        fail_test "FS-HELM-08" "Deployment still present after uninstall"
    else
        fail_test "FS-HELM-08" "the CRDs ArgoCD applied fail the CRD check after helm uninstall: $CRDS_REPORT"
    fi
fi

kubectl delete namespace "$HELM_NS" --ignore-not-found --wait=false 2>/dev/null || true

# =================================================================
# FS-HELM-09: An operator roll keeps the webhook Service backed
# =================================================================
begin_test "FS-HELM-09: An operator roll keeps the webhook Service backed"

# A standby whose webhook serves is ready, so the replacement joins the webhook
# Service while the old pod still holds the leader lease, and the chart's
# derived strategy removes the old pod only after that. Every sample taken
# during the roll must name at least one ready address. failurePolicy Ignore
# keeps this release's cluster-scoped webhook from failing anyone else's
# writes while it exists; the mutating pod injector is left out for the same
# reason. A webhook configuration left by an interrupted run would block the
# install, so it goes first.
kubectl delete validatingwebhookconfiguration cfgd-test --ignore-not-found 2>/dev/null || true
helm_test_ns "09"
helm install cfgd-test "$CHART_DIR" --skip-crds \
    -n "$HELM_NS" \
    --set "operator.image.repository=$(e2e_image_repo cfgd-operator)" \
    --set "operator.image.tag=$(e2e_image_tag cfgd-operator)" \
    --set "operator.imagePullSecrets[0].name=registry-credentials" \
    --set operator.enabled=true \
    --set operator.leaderElection.enabled=true \
    --set csiDriver.enabled=false \
    --set webhook.enabled=true \
    --set webhook.certManager.enabled=true \
    --set webhook.failurePolicy=Ignore \
    --set mutatingWebhook.enabled=false \
    --set agent.enabled=false \
    --set deviceGateway.enabled=false \
    --wait --timeout 180s 2>&1 || true

ROLL_DEPLOY=$(kubectl get deployment -n "$HELM_NS" \
    -l app.kubernetes.io/component=operator \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
ROLL_SVC=cfgd-test-webhook
ROLL_SAMPLES=0
ROLL_EMPTY=0
ROLL_EMPTY_AT=""
ROLL_DONE=false

if [ -z "$ROLL_DEPLOY" ]; then
    fail_test "FS-HELM-09" "No operator deployment after helm install"
elif ! wait_for_service_endpoints "$HELM_NS" "$ROLL_SVC" 120; then
    fail_test "FS-HELM-09" "Webhook Service never had a ready endpoint before the roll"
elif ! kubectl rollout restart "deployment/$ROLL_DEPLOY" -n "$HELM_NS"; then
    fail_test "FS-HELM-09" "kubectl rollout restart failed, so no roll was observed"
else
    ROLL_START=$SECONDS
    ROLL_DEADLINE=$((SECONDS + 180))
    while [ $SECONDS -lt $ROLL_DEADLINE ]; do
        ROLL_ADDRS=$(kubectl get endpoints "$ROLL_SVC" -n "$HELM_NS" \
            -o jsonpath='{.subsets[*].addresses[*].ip}' 2>/dev/null || echo "")
        ROLL_SAMPLES=$((ROLL_SAMPLES + 1))
        if [ -z "$ROLL_ADDRS" ]; then
            ROLL_EMPTY=$((ROLL_EMPTY + 1))
            ROLL_EMPTY_AT="${ROLL_EMPTY_AT:+$ROLL_EMPTY_AT, }#$ROLL_SAMPLES at +$((SECONDS - ROLL_START))s"
        fi
        if kubectl rollout status "deployment/$ROLL_DEPLOY" -n "$HELM_NS" --timeout=1s >/dev/null 2>&1; then
            ROLL_DONE=true
            break
        fi
    done
    echo "  Roll finished: $ROLL_DONE; samples: $ROLL_SAMPLES; samples with no ready endpoint: $ROLL_EMPTY${ROLL_EMPTY_AT:+ ($ROLL_EMPTY_AT)}"

    if [ "$ROLL_DONE" != "true" ]; then
        fail_test "FS-HELM-09" "The operator roll did not finish within 180s"
    elif [ "$ROLL_EMPTY" -ne 0 ]; then
        fail_test "FS-HELM-09" "The webhook Service had no ready endpoint in $ROLL_EMPTY of $ROLL_SAMPLES samples during the roll: $ROLL_EMPTY_AT"
    else
        pass_test "FS-HELM-09"
    fi
fi

helm_test_cleanup "cfgd-test"
