#!/usr/bin/env bash
# E2E tests for Crossplane TeamConfig composition: XRD installation,
# MachineConfig fan-out via function-cfgd, ConfigPolicy generation,
# and member add/remove lifecycle.
# Prereqs: kind cluster running, cfgd CRDs installed, Crossplane installed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
MANIFESTS_DIR="$SCRIPT_DIR/../manifests"
CROSSPLANE_DIR="$REPO_ROOT/manifests/crossplane"

echo "=== Crossplane E2E Tests ==="

# =================================================================
# XP-01: Install Crossplane
# =================================================================
begin_test "XP-01: Crossplane installation"
helm repo add crossplane-stable https://charts.crossplane.io/stable
helm install crossplane crossplane-stable/crossplane \
    --namespace crossplane-system --create-namespace --wait --timeout 120s
wait_for_deployment crossplane-system crossplane 120
pass_test "XP-01"

# --- Setup: Install cfgd CRDs ---
echo "Generating and installing cfgd CRDs..."
CRD_YAML=$(cargo run --release --bin cfgd-gen-crds --manifest-path "$REPO_ROOT/Cargo.toml" 2>/dev/null)
echo "$CRD_YAML" | kubectl apply -f -
for crd in machineconfigs.cfgd.io configpolicies.cfgd.io driftalerts.cfgd.io clusterconfigpolicies.cfgd.io; do
    kubectl wait --for=condition=established "crd/$crd" --timeout=30s 2>/dev/null || true
done

# --- Setup: Apply Crossplane XRD, Composition, and Function ---
echo "Applying XRD, Composition, and Function..."
kubectl apply -f "$CROSSPLANE_DIR/xrd-teamconfig.yaml"
kubectl apply -f "$CROSSPLANE_DIR/composition.yaml"
kubectl apply -f "$CROSSPLANE_DIR/function-cfgd.yaml"

# Wait for XRD to be established
echo "Waiting for TeamConfig XRD to be established..."
for i in $(seq 1 30); do
    XRD_READY=$(kubectl get xrd teamconfigs.cfgd.io \
        -o jsonpath='{.status.conditions[?(@.type=="Established")].status}' 2>/dev/null || echo "")
    if [ "$XRD_READY" = "True" ]; then
        break
    fi
    sleep 2
done

# =================================================================
# XP-02: Create TeamConfig with 2 members
# =================================================================
begin_test "XP-02: TeamConfig generates MachineConfigs"
kubectl apply -f "$MANIFESTS_DIR/teamconfig-sample.yaml"

MC_COUNT=""
for i in $(seq 1 30); do
    MC_COUNT=$(kubectl get mc -A --no-headers 2>/dev/null | wc -l)
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
    CP_COUNT=$(kubectl get cpol -A --no-headers 2>/dev/null | wc -l)
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

kubectl apply -f - <<'EOF'
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: test-team
spec:
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
    MC_COUNT=$(kubectl get mc -A --no-headers 2>/dev/null | wc -l)
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

kubectl apply -f - <<'EOF'
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: test-team
spec:
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
    MC_COUNT=$(kubectl get mc -A --no-headers 2>/dev/null | wc -l)
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

# --- Cleanup ---
echo ""
echo "Cleaning up test resources..."
kubectl delete teamconfig test-team 2>/dev/null || true
kubectl delete mc --all -A 2>/dev/null || true
kubectl delete cpol --all -A 2>/dev/null || true

# --- Summary ---
print_summary "Crossplane E2E Tests"
