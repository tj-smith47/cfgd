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

echo "Pushing images to $REGISTRY..."
docker push "${REGISTRY}/cfgd:${IMAGE_TAG}"
docker push "${REGISTRY}/cfgd-operator:${IMAGE_TAG}"
docker push "${REGISTRY}/cfgd-csi:${IMAGE_TAG}"

# --- Step 4: Ensure cfgd-system namespace ---
kubectl create namespace cfgd-system 2>/dev/null || true

# --- Step 5: Apply RBAC ---
echo "Applying E2E RBAC..."
kubectl apply -f "$SCRIPT_DIR/manifests/e2e-rbac.yaml"

# --- Step 6: Generate and apply CRDs ---
echo "Generating and applying CRDs..."
CRD_YAML=$("$REPO_ROOT/target/release/cfgd-gen-crds")
if [ -z "$CRD_YAML" ]; then
    echo "ERROR: cfgd-gen-crds produced no output"
    exit 1
fi
if [ "$RESET" = "--reset" ]; then
    echo "$CRD_YAML" | kubectl apply -f -
else
    echo "$CRD_YAML" | kubectl diff -f - > /dev/null 2>&1 || echo "$CRD_YAML" | kubectl apply -f -
fi

# Wait for CRDs to be established
for crd in machineconfigs.cfgd.io configpolicies.cfgd.io driftalerts.cfgd.io \
           modules.cfgd.io clusterconfigpolicies.cfgd.io; do
    kubectl wait --for=condition=established "crd/$crd" --timeout=30s 2>/dev/null || true
done

# --- Step 7: Apply cert-manager webhook TLS ---
echo "Applying webhook TLS (cert-manager)..."
kubectl apply -f "$SCRIPT_DIR/manifests/e2e-webhook-tls.yaml"

# --- Step 8: Apply operator deployment ---
echo "Applying operator deployment..."
sed "s|REGISTRY_PLACEHOLDER|${REGISTRY}|g; s|IMAGE_PLACEHOLDER|${IMAGE_TAG}|g" \
    "$SCRIPT_DIR/operator/manifests/operator-deployment.yaml" | kubectl apply -f -

# --- Step 9: Apply device gateway deployment ---
echo "Applying device gateway..."
sed "s|REGISTRY_PLACEHOLDER|${REGISTRY}|g; s|IMAGE_PLACEHOLDER|${IMAGE_TAG}|g" \
    "$SCRIPT_DIR/node/manifests/cfgd-server.yaml" | kubectl apply -f -

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
