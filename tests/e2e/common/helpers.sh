#!/usr/bin/env bash
# Shared E2E test helpers for all cfgd components.
# Source this from any test script: source "$(dirname "$0")/../../common/helpers.sh"

set -euo pipefail

E2E_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$E2E_ROOT/../.." && pwd)"

CFGD_NAMESPACE="${CFGD_NAMESPACE:-cfgd-system}"

PASS_COUNT=0
FAIL_COUNT=0
SKIP_COUNT=0

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

# --- Pod helpers (replaces KIND node helpers) ---

REGISTRY="${REGISTRY:?E2E_REGISTRY must be set (e.g. export REGISTRY=your.registry.io)}"
IMAGE_TAG="${IMAGE_TAG:-e2e-$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo latest)}"
E2E_NAMESPACE="${E2E_NAMESPACE:-cfgd-e2e-${GITHUB_RUN_ID:-$(date +%s)-$$}}"
E2E_RUN_ID="${GITHUB_RUN_ID:-local-$$}"
E2E_RUN_LABEL="cfgd.io/e2e-run=$E2E_RUN_ID"
# Job-specific label for cluster-scoped resources (prevents parallel job cleanup races)
E2E_JOB_LABEL="cfgd.io/e2e-job=$E2E_NAMESPACE"
# YAML-friendly forms for embedding in heredoc labels (key: "value" instead of key=value)
E2E_RUN_LABEL_YAML="cfgd.io/e2e-run: \"$E2E_RUN_ID\""
E2E_JOB_LABEL_YAML="cfgd.io/e2e-job: \"$E2E_NAMESPACE\""

TEST_POD=""

# Deploy the privileged test pod and wait for it to be Running.
# Exports TEST_POD with the pod name.
ensure_test_pod() {
    local pod_name="cfgd-e2e-node-${E2E_RUN_ID}"
    local manifest="$E2E_ROOT/manifests/privileged-test-pod.yaml"

    create_e2e_namespace

    # Substitute placeholders and apply (image first to avoid double-sub)
    sed "s|REGISTRY_PLACEHOLDER|${REGISTRY}|g; s|IMAGE_PLACEHOLDER|${IMAGE_TAG}|g; s|RUN_PLACEHOLDER|${E2E_RUN_ID}|g" \
        "$manifest" | kubectl apply -n "$E2E_NAMESPACE" -f -

    echo "  Waiting for test pod $pod_name..."
    kubectl wait --for=condition=Ready "pod/$pod_name" \
        -n "$E2E_NAMESPACE" --timeout=120s

    TEST_POD="$pod_name"
    export TEST_POD
    echo "  Test pod ready: $TEST_POD"
}

exec_in_pod() {
    kubectl exec "$TEST_POD" -n "$E2E_NAMESPACE" -- "$@"
}

cp_to_pod() {
    local src="$1"
    local dest="$2"
    kubectl cp "$src" "$E2E_NAMESPACE/$TEST_POD:$dest"
}

# --- Namespace & cleanup helpers ---

create_e2e_namespace() {
    if ! kubectl get namespace "$E2E_NAMESPACE" > /dev/null 2>&1; then
        kubectl create namespace "$E2E_NAMESPACE"
        kubectl label namespace "$E2E_NAMESPACE" "$E2E_RUN_LABEL" --overwrite
    fi
    # Wait for Reflector to replicate registry-credentials (annotated on source secret)
    local deadline=$((SECONDS + 30))
    while [ $SECONDS -lt $deadline ]; do
        if kubectl get secret registry-credentials -n "$E2E_NAMESPACE" > /dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done
    echo "  WARN: registry-credentials not replicated to $E2E_NAMESPACE (Reflector may not be running)"
}

cleanup_e2e() {
    echo "Cleaning up E2E resources for run $E2E_RUN_ID..."

    # Clean up host files FIRST (while pod still exists, before namespace deletion)
    if [ -n "$TEST_POD" ] && kubectl get pod "$TEST_POD" -n "$E2E_NAMESPACE" > /dev/null 2>&1; then
        exec_in_pod rm -f /host-etc/sysctl.d/99-cfgd.conf /host-etc/modules-load.d/cfgd.conf 2>/dev/null || true
    fi

    # Delete ephemeral namespace (cascade deletes all namespaced resources)
    kubectl delete namespace "$E2E_NAMESPACE" --ignore-not-found --wait=false 2>/dev/null || true

    # Delete cluster-scoped resources by job-specific label (not run label,
    # which is shared across parallel jobs and would nuke other jobs' resources)
    for kind in module clusterconfigpolicy; do
        kubectl delete "$kind" -l "$E2E_JOB_LABEL" --ignore-not-found 2>/dev/null || true
    done
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
        "$local_port:$remote_port" > /dev/null 2>&1 &
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

# --- OCI / Module helpers ---

CSI_DRIVER_NAME="csi.cfgd.io"
MODULES_ANNOTATION="cfgd.io/modules"

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

# --- Build helpers ---

# Ensure the cfgd binary is built (idempotent). Sets CFGD_BIN.
ensure_cfgd_binary() {
    if [ ! -f "$REPO_ROOT/target/release/cfgd" ]; then
        echo "  Building cfgd..."
        cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" --bin cfgd 2>/dev/null
    fi
    CFGD_BIN="$REPO_ROOT/target/release/cfgd"
    export CFGD_BIN
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

assert_rejected() {
    local output="$1"
    local description="$2"
    if echo "$output" | grep -qi "denied\|error\|invalid\|rejected"; then
        return 0
    fi
    echo "  ASSERT FAILED: '$description' was not rejected by webhook"
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
