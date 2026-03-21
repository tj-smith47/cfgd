#!/usr/bin/env bash
# Shared E2E test helpers for all cfgd components.
# Source this from any test script: source "$(dirname "$0")/../../common/helpers.sh"

set -euo pipefail

E2E_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$E2E_ROOT/../.." && pwd)"

KIND_CLUSTER="${KIND_CLUSTER:-cfgd-e2e}"
CFGD_NAMESPACE="${CFGD_NAMESPACE:-cfgd-system}"

PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

# --- Kind helpers ---

get_kind_node() {
    kind get nodes --name "$KIND_CLUSTER" 2>/dev/null | head -1
}

exec_on_node() {
    local node
    node="$(get_kind_node)"
    docker exec "$node" "$@"
}

copy_to_node() {
    local src="$1"
    local dest="$2"
    local node
    node="$(get_kind_node)"
    docker cp "$src" "$node:$dest"
}

# Extract a binary from a Docker image and install it on the kind node.
install_binary_on_node() {
    local image="$1"
    local binary_path="$2"
    local binary_name
    binary_name="$(basename "$binary_path")"

    local cid
    cid=$(docker create "$image" --help 2>/dev/null)
    docker cp "$cid:$binary_path" "/tmp/$binary_name"
    docker rm "$cid" > /dev/null

    local node
    node="$(get_kind_node)"
    docker cp "/tmp/$binary_name" "$node:/usr/local/bin/$binary_name"
    docker exec "$node" chmod +x "/usr/local/bin/$binary_name"
    rm -f "/tmp/$binary_name"
}

# Install OS packages on the kind node (idempotent).
install_packages_on_node() {
    exec_on_node bash -c \
        "apt-get update -qq && apt-get install -y -qq $* > /dev/null 2>&1" || true
}

# --- K8s helpers ---

wait_for_pod() {
    local namespace="$1"
    local label="$2"
    local timeout="${3:-120}"

    echo "  Waiting for pod $label in $namespace (timeout: ${timeout}s)..."
    local deadline=$((SECONDS + timeout))
    while [ $SECONDS -lt $deadline ]; do
        local status
        status=$(kubectl get pods -n "$namespace" -l "$label" \
            -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo "")
        if [ "$status" = "Running" ]; then
            echo "  Pod is Running"
            return 0
        fi
        sleep 2
    done
    echo "  Timed out waiting for pod"
    kubectl get pods -n "$namespace" -l "$label" -o wide 2>/dev/null || true
    return 1
}

wait_for_deployment() {
    local namespace="$1"
    local name="$2"
    local timeout="${3:-120}"
    kubectl wait --for=condition=available "deployment/$name" \
        -n "$namespace" --timeout="${timeout}s"
}

wait_for_daemonset() {
    local namespace="$1"
    local name="$2"
    local timeout="${3:-120}"

    echo "  Waiting for DaemonSet $name in $namespace (timeout: ${timeout}s)..."
    local deadline=$((SECONDS + timeout))
    while [ $SECONDS -lt $deadline ]; do
        local desired ready
        desired=$(kubectl get ds "$name" -n "$namespace" \
            -o jsonpath='{.status.desiredNumberScheduled}' 2>/dev/null || echo "0")
        ready=$(kubectl get ds "$name" -n "$namespace" \
            -o jsonpath='{.status.numberReady}' 2>/dev/null || echo "0")
        if [ "$desired" != "0" ] && [ "$desired" = "$ready" ]; then
            echo "  DaemonSet ready ($ready/$desired)"
            return 0
        fi
        sleep 2
    done
    echo "  Timed out waiting for DaemonSet"
    kubectl describe ds "$name" -n "$namespace" 2>/dev/null || true
    return 1
}

# Port-forward in the background. Echoes the PID; caller should kill it later.
port_forward() {
    local namespace="$1"
    local service="$2"
    local local_port="$3"
    local remote_port="${4:-$local_port}"

    kubectl port-forward -n "$namespace" "svc/$service" \
        "$local_port:$remote_port" &
    local pid=$!
    sleep 2  # let port-forward establish
    echo "$pid"
}

wait_for_url() {
    local url="$1"
    local timeout="${2:-60}"

    echo "  Waiting for $url (timeout: ${timeout}s)..."
    local deadline=$((SECONDS + timeout))
    while [ $SECONDS -lt $deadline ]; do
        if curl -sf "$url" > /dev/null 2>&1; then
            return 0
        fi
        sleep 2
    done
    echo "  Timed out waiting for URL"
    return 1
}

# --- Webhook/TLS helpers ---

WEBHOOK_CERT_DIR=""

# Generate self-signed CA and server certificate for webhook TLS.
# Sets WEBHOOK_CERT_DIR to a temp directory containing tls.crt and tls.key.
# Also sets CA_BUNDLE (base64-encoded CA cert) for webhook configurations.
generate_webhook_certs() {
    local service_name="${1:-cfgd-operator}"
    local namespace="${2:-cfgd-system}"

    WEBHOOK_CERT_DIR=$(mktemp -d)

    # Generate CA
    openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
        -keyout "$WEBHOOK_CERT_DIR/ca.key" \
        -out "$WEBHOOK_CERT_DIR/ca.crt" \
        -subj "/CN=cfgd-e2e-ca" 2>/dev/null

    # Generate server key + CSR
    openssl req -newkey rsa:2048 -nodes \
        -keyout "$WEBHOOK_CERT_DIR/tls.key" \
        -out "$WEBHOOK_CERT_DIR/tls.csr" \
        -subj "/CN=${service_name}.${namespace}.svc" 2>/dev/null

    # Sign with CA (include SANs for k8s webhook)
    cat > "$WEBHOOK_CERT_DIR/san.cnf" <<SANEOF
[req]
distinguished_name = req_dn
[req_dn]
[v3_ext]
subjectAltName = DNS:${service_name}.${namespace}.svc,DNS:${service_name}.${namespace}.svc.cluster.local,DNS:${service_name}.${namespace},DNS:${service_name}
SANEOF

    openssl x509 -req -in "$WEBHOOK_CERT_DIR/tls.csr" \
        -CA "$WEBHOOK_CERT_DIR/ca.crt" \
        -CAkey "$WEBHOOK_CERT_DIR/ca.key" \
        -CAcreateserial -days 1 \
        -extfile "$WEBHOOK_CERT_DIR/san.cnf" \
        -extensions v3_ext \
        -out "$WEBHOOK_CERT_DIR/tls.crt" 2>/dev/null

    CA_BUNDLE=$(base64 -w0 < "$WEBHOOK_CERT_DIR/ca.crt")
    export CA_BUNDLE
    echo "  Webhook certs generated in $WEBHOOK_CERT_DIR"
}

# Create the k8s Secret and webhook configurations for the operator webhook.
install_webhook_config() {
    local namespace="${1:-cfgd-system}"

    # Create TLS Secret from generated certs
    kubectl create secret tls cfgd-webhook-certs \
        --cert="$WEBHOOK_CERT_DIR/tls.crt" \
        --key="$WEBHOOK_CERT_DIR/tls.key" \
        -n "$namespace" 2>/dev/null || \
    kubectl create secret tls cfgd-webhook-certs \
        --cert="$WEBHOOK_CERT_DIR/tls.crt" \
        --key="$WEBHOOK_CERT_DIR/tls.key" \
        -n "$namespace" --dry-run=client -o yaml | kubectl apply -f -

    # Webhook Service
    kubectl apply -f - <<EOF
apiVersion: v1
kind: Service
metadata:
  name: cfgd-operator
  namespace: ${namespace}
spec:
  selector:
    app: cfgd-operator
  ports:
    - name: webhook
      port: 443
      targetPort: 9443
      protocol: TCP
EOF

    # Validating Webhook Configurations
    kubectl apply -f - <<EOF
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
        namespace: ${namespace}
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
        namespace: ${namespace}
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
        namespace: ${namespace}
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
        namespace: ${namespace}
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
        namespace: ${namespace}
        path: /validate-module
      caBundle: "${CA_BUNDLE}"
    rules:
      - apiGroups: ["cfgd.io"]
        apiVersions: ["v1alpha1"]
        operations: [CREATE, UPDATE]
        resources: [modules]
    failurePolicy: Fail
    sideEffects: None
EOF

    # Mutating Webhook Configuration
    kubectl apply -f - <<EOF
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
        namespace: ${namespace}
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
EOF

    echo "  Webhook configurations installed"
}

# --- OCI registry helpers ---

REGISTRY_NAME="cfgd-e2e-registry"
REGISTRY_PORT="${REGISTRY_PORT:-5001}"

# Start a local OCI registry container accessible from host (localhost:$REGISTRY_PORT)
# and from inside kind (cfgd-e2e-registry:5000).
start_local_registry() {
    if docker inspect "$REGISTRY_NAME" > /dev/null 2>&1; then
        echo "  Registry $REGISTRY_NAME already running"
        return 0
    fi

    docker run -d --restart=always \
        -p "${REGISTRY_PORT}:5000" \
        --name "$REGISTRY_NAME" \
        registry:2 > /dev/null

    # Connect to kind network so cluster nodes can reach it
    local kind_network="kind"
    docker network connect "$kind_network" "$REGISTRY_NAME" 2>/dev/null || true

    echo "  Local registry started at localhost:${REGISTRY_PORT}"
}

stop_local_registry() {
    docker rm -f "$REGISTRY_NAME" 2>/dev/null || true
}

# Configure kind nodes to trust the local registry (containerd mirror).
configure_registry_on_nodes() {
    local nodes
    nodes=$(kind get nodes --name "$KIND_CLUSTER" 2>/dev/null)
    for node in $nodes; do
        # Add registry mirror via containerd config
        docker exec "$node" bash -c "
            mkdir -p /etc/containerd/certs.d/localhost:${REGISTRY_PORT}
            cat > /etc/containerd/certs.d/localhost:${REGISTRY_PORT}/hosts.toml <<TOMLEOF
[host.\"http://${REGISTRY_NAME}:5000\"]
  capabilities = [\"pull\", \"resolve\"]
TOMLEOF
        " 2>/dev/null || true
    done
    echo "  Registry configured on kind nodes"
}

# Create a minimal test module directory for OCI push testing.
# Usage: create_test_module_dir /tmp/test-module "my-module" "1.0.0"
create_test_module_dir() {
    local dir="$1"
    local name="${2:-test-module}"
    local version="${3:-1.0.0}"

    mkdir -p "$dir/bin"
    cat > "$dir/module.yaml" <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: ${name}
spec:
  packages: []
  files:
    - source: bin/hello.sh
      target: bin/hello.sh
  env:
    - name: TEST_MODULE_LOADED
      value: "${name}-${version}"
EOF
    cat > "$dir/bin/hello.sh" <<'EOF'
#!/bin/sh
echo "hello from test module"
EOF
    chmod +x "$dir/bin/hello.sh"
}

# --- K8s field polling ---

# Wait for a k8s resource field to reach a desired state.
# If expected_value is empty, waits for field to be non-empty.
# Returns 0 if condition met, 1 on timeout. Echoes the final value to stdout.
# Usage: wait_for_k8s_field <kind> <name> <namespace> <jsonpath> [expected_value] [timeout]
# For cluster-scoped resources, pass "" for namespace.
wait_for_k8s_field() {
    local kind="$1"
    local name="$2"
    local namespace="$3"
    local jsonpath="$4"
    local expected="${5:-}"
    local timeout="${6:-60}"

    local ns_flag=""
    [ -n "$namespace" ] && ns_flag="-n $namespace"

    local deadline=$((SECONDS + timeout))
    local value=""

    while [ $SECONDS -lt $deadline ]; do
        value=$(kubectl get "$kind" "$name" $ns_flag \
            -o jsonpath="$jsonpath" 2>/dev/null || echo "")

        if [ -z "$expected" ]; then
            [ -n "$value" ] && echo "$value" && return 0
        else
            [ "$value" = "$expected" ] && echo "$value" && return 0
        fi
        sleep 1
    done
    echo "$value"
    return 1
}

# --- Assertion helpers ---

assert_contains() {
    local output="$1"
    local expected="$2"
    if echo "$output" | grep -qF "$expected"; then
        return 0
    fi
    echo "  ASSERT FAILED: output does not contain '$expected'"
    echo "  First 10 lines of output:"
    echo "$output" | head -10 | sed 's/^/    /'
    return 1
}

assert_not_contains() {
    local output="$1"
    local unexpected="$2"
    if echo "$output" | grep -qF "$unexpected"; then
        echo "  ASSERT FAILED: output contains unexpected '$unexpected'"
        return 1
    fi
    return 0
}

assert_equals() {
    local actual="$1"
    local expected="$2"
    if [ "$actual" = "$expected" ]; then
        return 0
    fi
    echo "  ASSERT FAILED: expected='$expected' actual='$actual'"
    return 1
}

assert_exit_code() {
    local actual="$1"
    local expected="${2:-0}"
    if [ "$actual" = "$expected" ]; then
        return 0
    fi
    echo "  ASSERT FAILED: expected exit code $expected, got $actual"
    return 1
}

# --- Test lifecycle ---

begin_test() {
    local name="$1"
    echo ""
    echo -e "${CYAN}━━━ $name ━━━${NC}"
}

pass_test() {
    local name="$1"
    PASS_COUNT=$((PASS_COUNT + 1))
    echo -e "  ${GREEN}PASS${NC}: $name"
}

fail_test() {
    local name="$1"
    local reason="${2:-}"
    FAIL_COUNT=$((FAIL_COUNT + 1))
    echo -e "  ${RED}FAIL${NC}: $name"
    if [ -n "$reason" ]; then
        echo "  Reason: $reason"
    fi
}

skip_test() {
    local name="$1"
    local reason="${2:-}"
    SKIP_COUNT=$((SKIP_COUNT + 1))
    echo -e "  ${YELLOW}SKIP${NC}: $name"
    if [ -n "$reason" ]; then
        echo "  Reason: $reason"
    fi
}

# Call at the end of a test suite script. Prints summary and exits non-zero on failures.
print_summary() {
    local suite="${1:-E2E}"
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo -e "$suite: ${GREEN}$PASS_COUNT passed${NC}, ${RED}$FAIL_COUNT failed${NC}, ${YELLOW}$SKIP_COUNT skipped${NC}"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

    if [ "$FAIL_COUNT" -gt 0 ]; then
        return 1
    fi
    return 0
}
