#!/usr/bin/env bash
# Idempotent pre-flight script for cfgd E2E tests.
# Builds images, pushes to registry, ensures persistent infrastructure is current.
# Works both in CI (ARC runners) and locally.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
source "$SCRIPT_DIR/common/helpers.sh"

RESET="${1:-}"

echo "=== cfgd E2E Setup ==="
echo "Registry: $REGISTRY"
echo "Image tag: $IMAGE_TAG"

# --- Step 0: Clean up stale E2E resources from previous runs ---
echo "Cleaning up stale E2E resources..."

# Kill stale port-forwards from previous gateway/server test runs
pkill -f "kubectl.*port-forward.*cfgd" 2>/dev/null || true

# Delete stale E2E namespaces (cfgd-e2e-* but not cfgd-system)
for ns in $(kubectl get ns -o name 2>/dev/null | grep 'namespace/cfgd-e2e' | sed 's|namespace/||'); do
    echo "  Deleting stale namespace: $ns"
    kubectl delete namespace "$ns" --ignore-not-found --wait=false 2>/dev/null || true
done

# Delete stale Helm test ClusterRoles/ClusterRoleBindings
for res in clusterrole clusterrolebinding; do
    for name in $(kubectl get "$res" -o name 2>/dev/null | grep 'cfgd-test' | sed "s|${res}/||" | sed "s|${res}.rbac.authorization.k8s.io/||"); do
        echo "  Deleting stale $res: $name"
        kubectl delete "$res" "$name" --ignore-not-found 2>/dev/null || true
    done
done

# Delete stale cluster-scoped CRD instances labeled with old E2E runs
for kind in machineconfig configpolicy driftalert clusterconfigpolicy module; do
    kubectl delete "$kind" -l cfgd.io/e2e-run --all-namespaces --ignore-not-found 2>/dev/null || true
done

echo "  Cleanup complete"

# --- Step 1: Verify cluster access ---
echo "Verifying cluster access..."
kubectl cluster-info > /dev/null 2>&1 || {
    echo "ERROR: Cannot reach Kubernetes cluster. Check KUBECONFIG."
    exit 1
}

# --- Step 1b: Pre-flight permission checks ---
# RBAC is managed by ArgoCD (see /db/manifests/k3s/namespaces/cfgd-system/e2e-rbac.yaml).
# This script only verifies the runner SA has what it needs; it does NOT apply RBAC.
kubectl create namespace cfgd-system 2>/dev/null || true

echo "Checking runner permissions..."
PREFLIGHT_OK=true
for check in \
    "create customresourcedefinitions" \
    "create clusterroles" \
    "get nodes" \
    "create csidrivers"; do
    if ! kubectl auth can-i $check --all-namespaces > /dev/null 2>&1; then
        echo "  MISSING: $check"
        PREFLIGHT_OK=false
    fi
done

if [ "$PREFLIGHT_OK" = "false" ]; then
    CURRENT_USER=$(kubectl auth whoami -o jsonpath='{.status.userInfo.username}' 2>/dev/null || echo "unknown")
    echo ""
    echo "ERROR: Runner SA lacks required permissions."
    echo "  Identity: $CURRENT_USER"
    echo ""
    echo "  Update the cfgd-e2e ClusterRole in your GitOps manifests and ensure"
    echo "  the runner SA is bound to it. See tests/e2e/manifests/e2e-rbac.yaml"
    echo "  for the required permissions."
    exit 1
fi
echo "  All pre-flight checks passed"

# --- Step 2: Build cfgd-gen-crds (other binaries built inside Dockerfiles) ---
echo "Building cfgd-gen-crds..."
cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" \
    --bin cfgd-gen-crds 2>&1 | tail -5

# --- Step 3: Build and push images ---
echo "Building Docker images..."
docker build -f "$REPO_ROOT/Dockerfile" \
    -t "${REGISTRY}/cfgd:${IMAGE_TAG}" "$REPO_ROOT"
docker build -f "$REPO_ROOT/Dockerfile.operator" \
    -t "${REGISTRY}/cfgd-operator:${IMAGE_TAG}" "$REPO_ROOT"
docker build -f "$REPO_ROOT/Dockerfile.csi" \
    -t "${REGISTRY}/cfgd-csi:${IMAGE_TAG}" "$REPO_ROOT"
docker build -t "${REGISTRY}/function-cfgd:${IMAGE_TAG}" "$REPO_ROOT/function-cfgd"

echo "Tagging and pushing images to $REGISTRY..."
# Also tag as :latest so ArgoCD-managed deployments pick up the new code
for img in cfgd cfgd-operator cfgd-csi; do
    docker tag "${REGISTRY}/${img}:${IMAGE_TAG}" "${REGISTRY}/${img}:latest"
    docker push "${REGISTRY}/${img}:${IMAGE_TAG}"
    docker push "${REGISTRY}/${img}:latest"
done

# Build and push function-cfgd as a Crossplane xpkg (package + embedded runtime)
docker tag "${REGISTRY}/function-cfgd:${IMAGE_TAG}" "${REGISTRY}/function-cfgd:latest"
docker push "${REGISTRY}/function-cfgd:${IMAGE_TAG}"
docker push "${REGISTRY}/function-cfgd:latest"
echo "Building function-cfgd xpkg..."
crossplane xpkg build \
    --package-root="$REPO_ROOT/function-cfgd/package" \
    --embed-runtime-image="${REGISTRY}/function-cfgd:${IMAGE_TAG}" \
    -o /tmp/function-cfgd.xpkg
crossplane xpkg push "${REGISTRY}/function-cfgd:${IMAGE_TAG}" -f /tmp/function-cfgd.xpkg
crossplane xpkg push "${REGISTRY}/function-cfgd:latest" -f /tmp/function-cfgd.xpkg

# Restart the function-cfgd deployment so it picks up the new embedded runtime image.
# The xpkg push doesn't trigger a redeploy when the tag is unchanged.
FUNC_DEPLOY=$(kubectl get deployment -n crossplane-system -l pkg.crossplane.io/function=function-cfgd \
    -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
if [ -n "$FUNC_DEPLOY" ]; then
    echo "  Restarting function-cfgd deployment ($FUNC_DEPLOY)..."
    kubectl rollout restart "deployment/$FUNC_DEPLOY" -n crossplane-system 2>/dev/null || true
    kubectl rollout status "deployment/$FUNC_DEPLOY" -n crossplane-system --timeout=60s 2>/dev/null || true
fi

# (Namespace and RBAC already created in Step 1b above)

# --- Step 6: Generate and apply CRDs ---
echo "Generating and applying CRDs..."
CRD_YAML=$("$REPO_ROOT/target/release/cfgd-gen-crds")
if [ -z "$CRD_YAML" ]; then
    echo "ERROR: cfgd-gen-crds produced no output"
    exit 1
fi
# Use kubectl replace to ensure full CRD schema updates (kubectl apply can
# fail to update nested schema fields due to 3-way merge conflicts).
# Fall back to apply for the initial creation (replace fails if CRD doesn't exist).
echo "$CRD_YAML" | kubectl replace -f - 2>/dev/null || echo "$CRD_YAML" | kubectl apply -f -

# Wait for CRDs to be established
for crd in machineconfigs.cfgd.io configpolicies.cfgd.io driftalerts.cfgd.io \
           modules.cfgd.io clusterconfigpolicies.cfgd.io; do
    kubectl wait --for=condition=established "crd/$crd" --timeout=30s 2>/dev/null || true
done

# --- Step 7: Apply cert-manager webhook TLS ---
echo "Applying webhook TLS (cert-manager)..."
kubectl apply -f "$SCRIPT_DIR/manifests/e2e-webhook-tls.yaml"

# --- Step 8: Update operator image ---
echo "Updating operator image..."
# Detect if deployments are managed by ArgoCD. If so, don't apply E2E manifests
# directly — they'll be reverted by ArgoCD. Instead, restart to pick up :latest.
ARGOCD_MANAGED=false
if kubectl get deployment cfgd-operator -n cfgd-system \
    -o jsonpath='{.metadata.annotations.argocd\.argoproj\.io/tracking-id}' 2>/dev/null | grep -q .; then
    ARGOCD_MANAGED=true
fi

if [ "$ARGOCD_MANAGED" = "true" ] || { [ -n "${CFGD_DEPLOY_MANIFESTS:-}" ] && [ -d "$CFGD_DEPLOY_MANIFESTS" ]; }; then
    echo "  Deployments managed by ArgoCD — restarting to pick up :latest images..."

    for deploy in cfgd-operator cfgd-server; do
        if kubectl get deployment "$deploy" -n cfgd-system > /dev/null 2>&1; then
            kubectl rollout restart "deployment/$deploy" -n cfgd-system 2>/dev/null || true
            # Wait for old pods to terminate (handles RWO PVC conflicts)
            kubectl rollout status "deployment/$deploy" -n cfgd-system --timeout=120s 2>/dev/null || {
                echo "  Rollout stuck for $deploy — deleting old pods to release PVC..."
                kubectl delete pods -n cfgd-system -l "app=$deploy" --ignore-not-found --grace-period=5 --wait=false 2>/dev/null || true
                sleep 5
                kubectl rollout status "deployment/$deploy" -n cfgd-system --timeout=120s 2>/dev/null || true
            }
        fi
    done
else
    echo "  Applying E2E manifests..."
    sed "s|REGISTRY_PLACEHOLDER|${REGISTRY}|g; s|IMAGE_PLACEHOLDER|${IMAGE_TAG}|g" \
        "$SCRIPT_DIR/operator/manifests/operator-deployment.yaml" | kubectl apply -f -
    sed "s|REGISTRY_PLACEHOLDER|${REGISTRY}|g; s|IMAGE_PLACEHOLDER|${IMAGE_TAG}|g" \
        "$SCRIPT_DIR/node/manifests/cfgd-server.yaml" | kubectl apply -f -
fi

# --- Step 10: Apply webhook configurations ---
echo "Applying webhook configurations..."
# Get the CA bundle from the cert-manager-generated secret
echo "  Waiting for webhook TLS secret..."
CA_BUNDLE=""
for i in $(seq 1 60); do
    CA_BUNDLE=$(kubectl get secret cfgd-webhook-certs -n cfgd-system \
        -o jsonpath='{.data.ca\.crt}' 2>/dev/null || echo "")
    if [ -n "$CA_BUNDLE" ]; then
        break
    fi
    sleep 2
done

if [ -z "$CA_BUNDLE" ]; then
    echo "ERROR: Webhook TLS secret not created by cert-manager after 120s"
    exit 1
fi

export CA_BUNDLE
# Generate webhook configs using the CA bundle
WEBHOOK_FILE=$(mktemp /tmp/cfgd-e2e-webhooks.XXXXXX.yaml)
trap "rm -f '$WEBHOOK_FILE'" EXIT
cat > "$WEBHOOK_FILE" <<WEBHOOKEOF
apiVersion: v1
kind: Service
metadata:
  name: cfgd-operator
  namespace: cfgd-system
spec:
  selector:
    app: cfgd-operator
  ports:
    - name: webhook
      port: 443
      targetPort: 9443
      protocol: TCP
---
apiVersion: admissionregistration.k8s.io/v1
kind: ValidatingWebhookConfiguration
metadata:
  name: cfgd-validating-webhooks
webhooks:
  - name: validate-machineconfig.cfgd.io
    admissionReviewVersions: [v1]
    clientConfig:
      service:
        name: cfgd-operator
        namespace: cfgd-system
        path: /validate-machineconfig
      caBundle: "${CA_BUNDLE}"
    rules:
      - apiGroups: ["cfgd.io"]
        apiVersions: ["v1alpha1"]
        operations: [CREATE, UPDATE]
        resources: [machineconfigs]
    failurePolicy: Fail
    sideEffects: None
  - name: validate-configpolicy.cfgd.io
    admissionReviewVersions: [v1]
    clientConfig:
      service:
        name: cfgd-operator
        namespace: cfgd-system
        path: /validate-configpolicy
      caBundle: "${CA_BUNDLE}"
    rules:
      - apiGroups: ["cfgd.io"]
        apiVersions: ["v1alpha1"]
        operations: [CREATE, UPDATE]
        resources: [configpolicies]
    failurePolicy: Fail
    sideEffects: None
  - name: validate-clusterconfigpolicy.cfgd.io
    admissionReviewVersions: [v1]
    clientConfig:
      service:
        name: cfgd-operator
        namespace: cfgd-system
        path: /validate-clusterconfigpolicy
      caBundle: "${CA_BUNDLE}"
    rules:
      - apiGroups: ["cfgd.io"]
        apiVersions: ["v1alpha1"]
        operations: [CREATE, UPDATE]
        resources: [clusterconfigpolicies]
    failurePolicy: Fail
    sideEffects: None
  - name: validate-driftalert.cfgd.io
    admissionReviewVersions: [v1]
    clientConfig:
      service:
        name: cfgd-operator
        namespace: cfgd-system
        path: /validate-driftalert
      caBundle: "${CA_BUNDLE}"
    rules:
      - apiGroups: ["cfgd.io"]
        apiVersions: ["v1alpha1"]
        operations: [CREATE, UPDATE]
        resources: [driftalerts]
    failurePolicy: Fail
    sideEffects: None
  - name: validate-module.cfgd.io
    admissionReviewVersions: [v1]
    clientConfig:
      service:
        name: cfgd-operator
        namespace: cfgd-system
        path: /validate-module
      caBundle: "${CA_BUNDLE}"
    rules:
      - apiGroups: ["cfgd.io"]
        apiVersions: ["v1alpha1"]
        operations: [CREATE, UPDATE]
        resources: [modules]
    failurePolicy: Fail
    sideEffects: None
---
apiVersion: admissionregistration.k8s.io/v1
kind: MutatingWebhookConfiguration
metadata:
  name: cfgd-mutating-webhooks
webhooks:
  - name: inject-modules.cfgd.io
    admissionReviewVersions: [v1]
    clientConfig:
      service:
        name: cfgd-operator
        namespace: cfgd-system
        path: /mutate-pods
      caBundle: "${CA_BUNDLE}"
    rules:
      - apiGroups: [""]
        apiVersions: ["v1"]
        operations: [CREATE]
        resources: [pods]
    namespaceSelector:
      matchExpressions:
        - key: cfgd.io/inject-modules
          operator: In
          values: ["true"]
    objectSelector:
      matchExpressions:
        - key: cfgd.io/skip-injection
          operator: DoesNotExist
    failurePolicy: Fail
    sideEffects: None
    reinvocationPolicy: IfNeeded
    timeoutSeconds: 10
WEBHOOKEOF

kubectl apply -f "$WEBHOOK_FILE"
rm -f "$WEBHOOK_FILE"

# --- Step 11: Deploy CSI driver via Helm ---
echo "Deploying CSI driver..."
helm upgrade --install cfgd-csi "$REPO_ROOT/chart/cfgd" \
    -n cfgd-system \
    --set operator.enabled=false \
    --set agent.enabled=false \
    --set webhook.enabled=false \
    --set mutatingWebhook.enabled=false \
    --set installCRDs=false \
    --set csiDriver.enabled=true \
    --set "csiDriver.image.repository=${REGISTRY}/cfgd-csi" \
    --set "csiDriver.image.tag=${IMAGE_TAG}" \
    --set csiDriver.image.pullPolicy=Always \
    --set "csiDriver.extraEnv[0].name=OCI_INSECURE_REGISTRIES" \
    --set "csiDriver.extraEnv[0].value=${REGISTRY}:5000" \
    --set "csiDriver.imagePullSecrets[0].name=registry-credentials" \
    --set "csiDriver.extraEnv[1].name=DOCKER_CONFIG" \
    --set "csiDriver.extraEnv[1].value=/etc/cfgd/docker" \
    --set "csiDriver.extraVolumes[0].name=docker-config" \
    --set "csiDriver.extraVolumes[0].secret.secretName=registry-credentials" \
    --set "csiDriver.extraVolumes[0].secret.items[0].key=.dockerconfigjson" \
    --set "csiDriver.extraVolumes[0].secret.items[0].path=config.json" \
    --set "csiDriver.extraVolumeMounts[0].name=docker-config" \
    --set "csiDriver.extraVolumeMounts[0].mountPath=/etc/cfgd/docker" \
    --set "csiDriver.extraVolumeMounts[0].readOnly=true" \
    --wait --timeout=120s 2>&1 || {
        echo "WARN: CSI driver Helm install failed — full-stack CSI tests will be skipped"
    }

# --- Step 12: Wait for all components ---
echo "Waiting for components..."
wait_for_deployment cfgd-system cfgd-operator 120
wait_for_deployment cfgd-system cfgd-server 120
# CSI DaemonSet readiness is optional — full-stack tests gracefully skip if CSI isn't ready

# --- Step 13: Reset gateway DB for clean E2E state ---
# Call the admin reset endpoint to wipe stale device/event data from prior runs.
# This is safe: the endpoint is behind admin auth and only deletes data rows,
# not the SQLite file (avoids Longhorn volume corruption from rm -f on live DB).
GW_API_KEY=$(kubectl get deployment cfgd-server -n cfgd-system \
    -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="CFGD_API_KEY")].value}' 2>/dev/null || echo "")
if [ -n "$GW_API_KEY" ]; then
    # Port-forward to gateway, reset, then clean up
    kubectl port-forward -n cfgd-system svc/cfgd-server 18099:8080 > /dev/null 2>&1 &
    PF_PID=$!
    sleep 2
    RESET_RESP=$(curl -sf -X POST "http://localhost:18099/api/v1/admin/reset" \
        -H "Authorization: Bearer $GW_API_KEY" 2>/dev/null || echo "")
    kill "$PF_PID" 2>/dev/null || true
    wait "$PF_PID" 2>/dev/null || true
    if [ -n "$RESET_RESP" ]; then
        echo "  Gateway DB reset: $RESET_RESP"
    else
        echo "  WARN: Gateway DB reset failed (endpoint may not exist yet)"
    fi
fi

echo ""
echo "=== E2E Setup Complete ==="
echo "  Operator:  $(kubectl get pods -n cfgd-system -l app=cfgd-operator -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo 'unknown')"
echo "  Gateway:   $(kubectl get pods -n cfgd-system -l app=cfgd-server -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo 'unknown')"
echo "  CSI:       $(kubectl get ds -n cfgd-system -l app.kubernetes.io/component=csi-driver -o jsonpath='{.items[0].status.numberReady}' 2>/dev/null || echo 'N/A') ready"
echo "  Images:    ${REGISTRY}/cfgd:${IMAGE_TAG}"
