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

# --- Step 1: Verify cluster access ---
echo "Verifying cluster access..."
kubectl cluster-info > /dev/null 2>&1 || {
    echo "ERROR: Cannot reach Kubernetes cluster. Check KUBECONFIG."
    exit 1
}

# --- Step 1b: Ensure runner SA has E2E permissions ---
# ARC runners use their pod's SA, which may not have CRD/node/Helm permissions.
# Apply the e2e ClusterRole first, then bind the current identity to it.
# Namespace must exist before the SA in e2e-rbac.yaml can be created.
kubectl create namespace cfgd-system 2>/dev/null || true
echo "Applying E2E ClusterRole..."
kubectl apply -f "$SCRIPT_DIR/manifests/e2e-rbac.yaml"

# Detect current identity and bind to cfgd-e2e ClusterRole if not already bound
CURRENT_USER=$(kubectl auth whoami -o jsonpath='{.status.userInfo.username}' 2>/dev/null || echo "")
if [ -n "$CURRENT_USER" ] && echo "$CURRENT_USER" | grep -q '^system:serviceaccount:'; then
    RUNNER_NS=$(echo "$CURRENT_USER" | cut -d: -f3)
    RUNNER_SA=$(echo "$CURRENT_USER" | cut -d: -f4)
    echo "  Runner identity: $RUNNER_SA in $RUNNER_NS"

    # Create a binding for the runner SA if it's not the cfgd-e2e SA
    if [ "$RUNNER_SA" != "cfgd-e2e" ] || [ "$RUNNER_NS" != "cfgd-system" ]; then
        BINDING_NAME="cfgd-e2e-runner-${RUNNER_SA}"
        # Truncate to 63 chars for k8s name limit
        BINDING_NAME="${BINDING_NAME:0:63}"
        if kubectl apply -f - <<RUNNEREOF 2>/dev/null
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: ${BINDING_NAME}
subjects:
  - kind: ServiceAccount
    name: ${RUNNER_SA}
    namespace: ${RUNNER_NS}
roleRef:
  kind: ClusterRole
  name: cfgd-e2e
  apiGroup: rbac.authorization.k8s.io
RUNNEREOF
        then
            echo "  Bound runner SA to cfgd-e2e ClusterRole"
            sleep 2
        else
            echo "  WARN: Could not self-bind (RBAC escalation prevention)."
            echo "  If CRD/node permissions fail, a cluster admin must run:"
            echo "    kubectl create clusterrolebinding ${BINDING_NAME} \\"
            echo "      --clusterrole=cfgd-e2e \\"
            echo "      --serviceaccount=${RUNNER_NS}:${RUNNER_SA}"
        fi
    fi
fi

# Pre-flight: verify CRD management permissions
if ! kubectl auth can-i create customresourcedefinitions --all-namespaces > /dev/null 2>&1; then
    echo ""
    echo "ERROR: Current identity cannot manage CRDs."
    echo "  Identity: ${CURRENT_USER:-unknown}"
    echo ""
    echo "  A cluster admin must apply the runner binding once:"
    echo "    kubectl apply -f tests/e2e/manifests/e2e-rbac.yaml"
    echo "    kubectl create clusterrolebinding cfgd-e2e-runner \\"
    echo "      --clusterrole=cfgd-e2e \\"
    echo "      --serviceaccount=<runner-namespace>:<runner-sa-name>"
    echo ""
    exit 1
fi

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

echo "Tagging and pushing images to $REGISTRY..."
# Also tag as :latest so ArgoCD-managed deployments pick up the new code
for img in cfgd cfgd-operator cfgd-csi; do
    docker tag "${REGISTRY}/${img}:${IMAGE_TAG}" "${REGISTRY}/${img}:latest"
    docker push "${REGISTRY}/${img}:${IMAGE_TAG}"
    docker push "${REGISTRY}/${img}:latest"
done

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
    kubectl rollout restart deployment/cfgd-operator -n cfgd-system 2>/dev/null || true
    kubectl rollout restart deployment/cfgd-server -n cfgd-system 2>/dev/null || true
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

echo ""
echo "=== E2E Setup Complete ==="
echo "  Operator:  $(kubectl get pods -n cfgd-system -l app=cfgd-operator -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo 'unknown')"
echo "  Gateway:   $(kubectl get pods -n cfgd-system -l app=cfgd-server -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo 'unknown')"
echo "  CSI:       $(kubectl get ds -n cfgd-system -l app.kubernetes.io/component=csi-driver -o jsonpath='{.items[0].status.numberReady}' 2>/dev/null || echo 'N/A') ready"
echo "  Images:    ${REGISTRY}/cfgd:${IMAGE_TAG}"
