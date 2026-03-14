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
