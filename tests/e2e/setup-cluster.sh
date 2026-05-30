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
# Stale-resource cleanup (former Step 0) moved to the async cfgd-e2e-janitor
# CronJob in cfgd-system: it deletes aged cfgd-e2e-* namespaces, cfgd-test
# RBAC, and orphaned CRD instances off the GHA critical path. See
# /db/manifests/k3s/namespaces/cfgd-system/e2e-cleanup-cronjob.yaml.
echo "Verifying cluster access..."
kubectl cluster-info >/dev/null 2>&1 || {
    echo "ERROR: Cannot reach Kubernetes cluster. Check KUBECONFIG."
    exit 1
}

# --- Step 1b: Pre-flight permission checks ---
# RBAC is managed by ArgoCD (see /db/manifests/k3s/namespaces/cfgd-system/e2e-rbac.yaml).
# This script only verifies the runner SA has what it needs; it does NOT apply RBAC.
kubectl create namespace cfgd-system 2>/dev/null || true

# --- Step 1c: Serialize on shared cluster state via a coordination Lease ---
# Two near-simultaneous setups mutate the same cluster-scoped state (CRDs,
# webhooks, the singleton operator/server deployments, the CSI release). A
# coordination.k8s.io/Lease named cfgd-e2e-setup serializes them: the holder
# identity is GITHUB_RUN_ID and a background renewer advances renewTime every
# third of the duration. If the holder dies, renewTime stops and any waiter
# steals the lease once it expires — auto-release on holder death without a
# permanent lock.
LEASE_NAME="cfgd-e2e-setup"
LEASE_NS="cfgd-system"
LEASE_HOLDER="${GITHUB_RUN_ID:-local-$$}"
LEASE_DURATION_SECONDS=1200
LEASE_RENEW_PID=""

# Epoch seconds for an RFC3339 (microTime) timestamp, or 0 if unparseable.
lease_epoch() {
    local ts="$1"
    [ -z "$ts" ] && { echo 0; return; }
    date -u -d "$ts" +%s 2>/dev/null || echo 0
}

# Current UTC time in the microTime format the Lease API expects.
lease_now_rfc3339() {
    date -u +%Y-%m-%dT%H:%M:%S.000000Z
}

# Lease manifest body with us as holder and a fresh renewTime. A
# resourceVersion line is injected by callers that need an optimistic-
# concurrency precondition.
lease_manifest() {
    local resource_version="${1:-}"
    local rv_line=""
    [ -n "$resource_version" ] && rv_line="  resourceVersion: \"${resource_version}\""
    cat <<LEASEEOF
apiVersion: coordination.k8s.io/v1
kind: Lease
metadata:
  name: ${LEASE_NAME}
  namespace: ${LEASE_NS}
${rv_line}
spec:
  holderIdentity: "${LEASE_HOLDER}"
  leaseDurationSeconds: ${LEASE_DURATION_SECONDS}
  renewTime: "$(lease_now_rfc3339)"
LEASEEOF
}

# Atomic create — fails (non-zero) if the Lease already exists. Only one
# concurrent waiter observing an absent lease can win this.
lease_create() {
    lease_manifest | kubectl create -f - >/dev/null 2>&1
}

# Optimistic replace — fails if the object changed since we read it (the
# embedded resourceVersion no longer matches), so a steal can't clobber a
# write another waiter landed first.
lease_replace_at() {
    local resource_version="$1"
    lease_manifest "$resource_version" | kubectl replace -f - >/dev/null 2>&1
}

# True only if the live Lease still names us as holder.
lease_held_by_us() {
    local holder
    holder=$(kubectl get lease "$LEASE_NAME" -n "$LEASE_NS" \
        -o jsonpath='{.spec.holderIdentity}' 2>/dev/null || echo "")
    [ "$holder" = "$LEASE_HOLDER" ]
}

# Block until we hold the lease. Acquisition is atomic — never a bare apply:
#   absent  → kubectl create (fails if another waiter created it first)
#   expired → kubectl replace guarded by the observed resourceVersion
# After any write that returns success we RE-READ and confirm we are the holder
# before returning; losing the race loops back and waits. No deadlock: a dead
# holder stops renewing, the lease expires, and the next waiter steals it.
acquire_lease() {
    echo "Acquiring setup lease ${LEASE_NS}/${LEASE_NAME} (holder ${LEASE_HOLDER})..."
    local deadline=$((SECONDS + LEASE_DURATION_SECONDS))
    while [ $SECONDS -lt $deadline ]; do
        local raw holder renew rv
        raw=$(kubectl get lease "$LEASE_NAME" -n "$LEASE_NS" \
            -o jsonpath='{.spec.holderIdentity}|{.spec.renewTime}|{.metadata.resourceVersion}' \
            2>/dev/null || echo "__absent__")

        if [ "$raw" = "__absent__" ]; then
            # No lease object yet — create it atomically.
            if lease_create && lease_held_by_us; then
                echo "  Lease acquired (created)"
                return 0
            fi
            sleep 5
            continue
        fi

        holder="${raw%%|*}"
        rv="${raw##*|}"
        renew="${raw#*|}"; renew="${renew%|*}"

        if [ -z "$holder" ]; then
            # Object exists but holder was cleared — replace under its RV.
            if lease_replace_at "$rv" && lease_held_by_us; then
                echo "  Lease acquired (claimed released lease)"
                return 0
            fi
            sleep 5
            continue
        fi

        if [ "$holder" = "$LEASE_HOLDER" ]; then
            echo "  Lease already held by us"
            return 0
        fi

        local renew_epoch now_epoch
        renew_epoch=$(lease_epoch "$renew")
        now_epoch=$(date -u +%s)
        if [ $((now_epoch - renew_epoch)) -gt "$LEASE_DURATION_SECONDS" ]; then
            echo "  Lease held by ${holder} is expired — attempting steal"
            # Guarded replace: only succeeds if the lease hasn't changed (e.g.
            # the dead holder revived, or another waiter stole first) since read.
            if lease_replace_at "$rv" && lease_held_by_us; then
                echo "  Lease acquired (stolen from expired ${holder})"
                return 0
            fi
            echo "  Steal lost the race; retrying"
        else
            echo "  Lease held by ${holder}; waiting..."
        fi
        sleep 5
    done
    echo "ERROR: Could not acquire setup lease within ${LEASE_DURATION_SECONDS}s"
    exit 1
}

# Renew in the background so a long setup never lets the lease expire under it.
# The subshell clears the inherited EXIT trap so killing it can't re-enter
# release_lease. Each renewal is a guarded replace at the current RV and only
# proceeds while we are still the holder — if a steal happened (we were
# wrongly presumed dead), the renewer stops touching the lease.
start_lease_renewer() {
    (
        trap - EXIT
        while true; do
            sleep $((LEASE_DURATION_SECONDS / 3))
            local raw holder rv
            raw=$(kubectl get lease "$LEASE_NAME" -n "$LEASE_NS" \
                -o jsonpath='{.spec.holderIdentity}|{.metadata.resourceVersion}' \
                2>/dev/null || echo "")
            holder="${raw%%|*}"
            rv="${raw##*|}"
            if [ "$holder" = "$LEASE_HOLDER" ] && [ -n "$rv" ]; then
                lease_replace_at "$rv" || true
            fi
        done
    ) &
    LEASE_RENEW_PID=$!
}

# Release on any exit: stop the renewer, then delete the Lease only if we still
# hold it (never yank a lease another run legitimately stole after our death).
release_lease() {
    [ -n "$LEASE_RENEW_PID" ] && kill "$LEASE_RENEW_PID" 2>/dev/null || true
    local holder
    holder=$(kubectl get lease "$LEASE_NAME" -n "$LEASE_NS" \
        -o jsonpath='{.spec.holderIdentity}' 2>/dev/null || echo "")
    if [ "$holder" = "$LEASE_HOLDER" ]; then
        kubectl delete lease "$LEASE_NAME" -n "$LEASE_NS" --ignore-not-found >/dev/null 2>&1 || true
    fi
}
trap release_lease EXIT

acquire_lease
start_lease_renewer

echo "Checking runner permissions..."
PREFLIGHT_OK=true
for check in \
    "create customresourcedefinitions" \
    "create clusterroles" \
    "get nodes" \
    "create csidrivers"; do
    if ! kubectl auth can-i $check --all-namespaces >/dev/null 2>&1; then
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

# --- Step 2: Extract cfgd-gen-crds binary from the operator's build stage ---
# Dockerfile.operator now compiles cfgd-operator AND cfgd-gen-crds in a
# single `cargo build` pass; the `crds` stage exposes just the gen-crds
# binary. Buildx extracts it into the local filesystem — populates the
# layer cache that the subsequent runtime build reuses (no second compile).
echo "Extracting cfgd-gen-crds from Dockerfile.operator..."
mkdir -p "$REPO_ROOT/target/release"
crds_args=(buildx build --target crds
    --output "type=local,dest=$REPO_ROOT/target/release"
    -f "$REPO_ROOT/Dockerfile.operator")
if [ "${SCCACHE_GHA_ENABLED:-}" = "true" ]; then
    crds_args+=(--cache-from "type=gha,scope=cfgd-operator"
                --cache-to "type=gha,mode=max,scope=cfgd-operator")
fi
docker "${crds_args[@]}" "$REPO_ROOT"

# --- Step 3: Build and push images ---
# Pre-pull base images so the Dockerfile `FROM`s hit the local cache. If
# docker.io rate-limits us (HTTP 429), fall back to mirror.gcr.io and retag
# under the bare name so `FROM debian:bookworm-slim` resolves locally.
pull_with_fallback() {
    local image="$1"
    if docker pull "$image"; then
        return 0
    fi
    echo "  Docker Hub pull failed for ${image}; trying mirror.gcr.io..."
    if docker pull "mirror.gcr.io/library/${image}"; then
        docker tag "mirror.gcr.io/library/${image}" "$image"
        return 0
    fi
    return 1
}
echo "Pre-pulling base images..."
pull_with_fallback debian:bookworm-slim
pull_with_fallback rust:1.94-slim-bookworm
pull_with_fallback golang:1.25

echo "Building Docker images..."

# Last-green SHA is persisted per image+branch in a cfgd-system ConfigMap so it
# survives ARC runner churn (runners are ephemeral; no host state persists).
# A registry annotation store was rejected: distribution v2 (registry:2) has no
# arbitrary key-value annotation API, only image manifests.
LAST_GREEN_CM="cfgd-e2e-last-green"
E2E_BRANCH="${GITHUB_REF_NAME:-$(git -C "$REPO_ROOT" rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)}"
# ConfigMap keys must match [-._a-zA-Z0-9]; branch names may contain '/'.
E2E_BRANCH_KEY="${E2E_BRANCH//[^-._a-zA-Z0-9]/_}"

# Read the last-green SHA for an image on this branch. Empty on any miss
# (no ConfigMap, no key, unreachable apiserver) so callers fail OPEN → build.
last_green_sha() {
    local image="$1"
    kubectl get configmap "$LAST_GREEN_CM" -n cfgd-system \
        -o jsonpath="{.data.${image}_${E2E_BRANCH_KEY}}" 2>/dev/null || echo ""
}

# Persist the current HEAD as the last-green SHA for an image on this branch.
# Best-effort: a write failure must not fail the run (next run just rebuilds).
record_green_sha() {
    local image="$1"
    # Ensure the CM exists, then merge-patch only this image's key. Applying a
    # single-key generated manifest would replace the managed `data` and clobber
    # the sibling images' keys (so only the last image recorded would persist);
    # a merge patch is additive per key.
    kubectl create configmap "$LAST_GREEN_CM" -n cfgd-system >/dev/null 2>&1 || true
    kubectl patch configmap "$LAST_GREEN_CM" -n cfgd-system --type merge \
        -p "{\"data\":{\"${image}_${E2E_BRANCH_KEY}\":\"${GIT_SHA}\"}}" >/dev/null 2>&1 || true
}

GIT_SHA="$(git -C "$REPO_ROOT" rev-parse HEAD 2>/dev/null || echo "")"

# Decide whether an image's inputs changed since its last-green SHA. Prints
# "build" or "skip". Fails OPEN (prints "build") on ANY uncertainty: missing
# last-green SHA, unreadable git history, or an empty diff range. Never skips
# and ships a stale image. Inputs = the image's own crate dir(s), the shared
# Cargo.lock + Cargo.toml, and the image's Dockerfile — all relative to
# REPO_ROOT so `git diff` paths resolve regardless of CWD.
# Usage: image_decision <image> <dockerfile_relpath> <path>...
image_decision() {
    local image="$1" dockerfile="$2"; shift 2
    local paths=("$dockerfile" "$@")

    local last_green
    last_green="$(last_green_sha "$image")"
    if [ -z "$last_green" ]; then
        echo "build"
        return 0
    fi
    if [ -z "$GIT_SHA" ]; then
        echo "build"
        return 0
    fi
    # A last-green SHA absent from local history (force-push, shallow clone)
    # means we cannot trust the diff → build.
    if ! git -C "$REPO_ROOT" cat-file -e "${last_green}^{commit}" 2>/dev/null; then
        echo "build"
        return 0
    fi
    if git -C "$REPO_ROOT" diff --quiet "$last_green" "$GIT_SHA" -- "${paths[@]}" 2>/dev/null; then
        echo "skip"
        return 0
    fi
    echo "build"
}

# buildx + GHA cache: unchanged layers restore from per-image scope cache
# instead of recompiling. Gated on SCCACHE_GHA_ENABLED so local invocations
# without GHA cache fall back to a plain `docker buildx build --load`.
build_image() {
    local dockerfile="$1" tag="$2" context="$3" scope="$4"
    local args=(buildx build --load -f "$dockerfile" -t "$tag" "$context")
    if [ "${SCCACHE_GHA_ENABLED:-}" = "true" ]; then
        args+=(--cache-from "type=gha,scope=${scope}" --cache-to "type=gha,mode=max,scope=${scope}")
    fi
    docker "${args[@]}"
}

# Build + push an image only when its inputs changed since last-green; otherwise
# verify the previously-pushed :IMAGE_TAG still exists in the registry. If the
# skip-candidate tag is missing (GC raced, registry wiped), fall through to a
# build so a green run never ships a dangling tag. Rust images also retag :latest
# so ArgoCD-managed deployments pick up new code.
#
# Cargo.lock + Cargo.toml are workspace-shared; cfgd-core is linked by every Rust
# binary, so a change there rebuilds all three Rust images.
RUST_SHARED_PATHS=(Cargo.lock Cargo.toml crates/cfgd-core)

# IMAGE_BUILT[<image>] = "true" when this run rebuilt+pushed the image, "false"
# when it was skipped. Downstream steps (rollout restart, CSI Helm redeploy)
# read it to skip no-op restarts on unchanged images.
declare -A IMAGE_BUILT

build_and_push() {
    local image="$1" dockerfile="$2" context="$3" scope="$4" retag_latest="$5"; shift 5
    local input_paths=("$@")
    local df_rel="${dockerfile#"$REPO_ROOT/"}"

    local decision
    decision="$(image_decision "$image" "$df_rel" "${input_paths[@]}")"

    if [ "$decision" = "skip" ]; then
        # Confirm the tag the deploys reference actually exists before trusting
        # the skip — fail OPEN to a build if the registry lost it.
        if docker manifest inspect "${REGISTRY}/${image}:${IMAGE_TAG}" >/dev/null 2>&1; then
            echo "  SKIP ${image}: no source change since ${REGISTRY}/${image} last-green"
            if [ "$retag_latest" = "true" ]; then
                docker pull "${REGISTRY}/${image}:${IMAGE_TAG}" >/dev/null 2>&1 || true
                docker tag "${REGISTRY}/${image}:${IMAGE_TAG}" "${REGISTRY}/${image}:latest" 2>/dev/null || true
                docker push "${REGISTRY}/${image}:latest" 2>/dev/null || true
            fi
            IMAGE_BUILT[$image]="false"
            return 0
        fi
        echo "  ${image}: last-green unchanged but :${IMAGE_TAG} missing from registry — rebuilding"
    fi

    echo "  BUILD ${image}..."
    build_image "$dockerfile" "${REGISTRY}/${image}:${IMAGE_TAG}" "$context" "$scope"
    docker push "${REGISTRY}/${image}:${IMAGE_TAG}"
    if [ "$retag_latest" = "true" ]; then
        docker tag "${REGISTRY}/${image}:${IMAGE_TAG}" "${REGISTRY}/${image}:latest"
        docker push "${REGISTRY}/${image}:latest"
    fi
    IMAGE_BUILT[$image]="true"
}

build_and_push cfgd "$REPO_ROOT/Dockerfile" "$REPO_ROOT" cfgd true \
    crates/cfgd "${RUST_SHARED_PATHS[@]}"
build_and_push cfgd-operator "$REPO_ROOT/Dockerfile.operator" "$REPO_ROOT" cfgd-operator true \
    crates/cfgd-operator "${RUST_SHARED_PATHS[@]}"
build_and_push cfgd-csi "$REPO_ROOT/Dockerfile.csi" "$REPO_ROOT" cfgd-csi true \
    crates/cfgd-csi "${RUST_SHARED_PATHS[@]}"

# function-cfgd is a self-contained Go module: its dir holds go.mod/go.sum and
# its Dockerfile, so the crate dir alone is the full input set. It is pushed as
# a Crossplane xpkg (below), not via the plain image push, so retag_latest=false.
FUNCTION_DECISION="$(image_decision function-cfgd function-cfgd/Dockerfile function-cfgd)"
if [ "$FUNCTION_DECISION" = "skip" ] && docker manifest inspect "${REGISTRY}/function-cfgd:${IMAGE_TAG}" >/dev/null 2>&1; then
    echo "  SKIP function-cfgd: no source change since last-green"
else
    echo "  BUILD function-cfgd..."
    build_image "$REPO_ROOT/function-cfgd/Dockerfile" \
        "${REGISTRY}/function-cfgd:${IMAGE_TAG}" "$REPO_ROOT/function-cfgd" function-cfgd
    docker push "${REGISTRY}/function-cfgd:${IMAGE_TAG}"
    docker tag "${REGISTRY}/function-cfgd:${IMAGE_TAG}" "${REGISTRY}/function-cfgd:latest"
    docker push "${REGISTRY}/function-cfgd:latest"
    FUNCTION_DECISION="build"
fi

# The xpkg repackages function-cfgd's embedded runtime image. When the image
# was rebuilt this run, the xpkg must follow; when skipped, the existing xpkg
# tag is still valid and we avoid the crank install + build + push entirely.
if [ "$FUNCTION_DECISION" = "build" ]; then
    # Ensure crossplane CLI (crank). Pinned to a stable release; the upstream
    # install.sh from `main` rejects `linux / x86_64` as of late May 2026.
    if ! which crossplane &>/dev/null; then
        CROSSPLANE_VERSION="v2.3.1"
        case "$(uname -m)" in
            x86_64|amd64) CROSSPLANE_ARCH=amd64 ;;
            aarch64|arm64) CROSSPLANE_ARCH=arm64 ;;
            *) echo "unsupported arch: $(uname -m)"; exit 1 ;;
        esac
        curl -sL -o /usr/local/bin/crossplane \
            "https://releases.crossplane.io/stable/${CROSSPLANE_VERSION}/bin/linux_${CROSSPLANE_ARCH}/crank"
        chmod +x /usr/local/bin/crossplane
        echo "Installed crossplane:"
        crossplane version
    fi

    echo "Building function-cfgd xpkg..."
    XPKG_OUT="${RUNNER_TEMP:-/tmp}/function-cfgd.xpkg"
    crossplane xpkg build \
        --package-root="$REPO_ROOT/function-cfgd/package" \
        --embed-runtime-image="${REGISTRY}/function-cfgd:${IMAGE_TAG}" \
        -o "$XPKG_OUT"
    crossplane xpkg push "${REGISTRY}/function-cfgd:${IMAGE_TAG}" -f "$XPKG_OUT"
    crossplane xpkg push "${REGISTRY}/function-cfgd:latest" -f "$XPKG_OUT"

    # Restart the function-cfgd deployment so it picks up the new embedded runtime image.
    # The xpkg push doesn't trigger a redeploy when the tag is unchanged.
    FUNC_DEPLOY=$(kubectl get deployment -n crossplane-system -l pkg.crossplane.io/function=function-cfgd \
        -o jsonpath='{.items[0].metadata.name}' 2>/dev/null || echo "")
    if [ -n "$FUNC_DEPLOY" ]; then
        echo "  Restarting function-cfgd deployment ($FUNC_DEPLOY)..."
        kubectl rollout restart "deployment/$FUNC_DEPLOY" -n crossplane-system 2>/dev/null || true
        kubectl rollout status "deployment/$FUNC_DEPLOY" -n crossplane-system --timeout=60s 2>/dev/null || true
    fi
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
    # Both cfgd-operator and cfgd-server run the cfgd-operator :latest image.
    # A rollout restart only matters when that image was rebuilt this run;
    # skipping a no-op restart is the bulk of the no-source-change time saving.
    if [ "${IMAGE_BUILT[cfgd-operator]:-true}" != "true" ]; then
        echo "  cfgd-operator image unchanged — skipping operator/server rollout restart"
    else
    echo "  Deployments managed by ArgoCD — restarting to pick up :latest images..."

    for deploy in cfgd-operator cfgd-server; do
        if kubectl get deployment "$deploy" -n cfgd-system >/dev/null 2>&1; then
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
    fi
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
WEBHOOK_FILE=$(mktemp "${RUNNER_TEMP:-/tmp}/cfgd-e2e-webhooks.XXXXXX.yaml")
# Chain both cleanups into the single EXIT trap (the lease release is already
# registered) — a bare `trap ... EXIT` here would drop the lease release.
trap 'rm -f "$WEBHOOK_FILE"; release_lease' EXIT
cat >"$WEBHOOK_FILE" <<WEBHOOKEOF
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
# Skip the Helm redeploy when the CSI image is unchanged AND a release already
# exists (fresh clusters with no release still install). The DaemonSet keeps
# running the existing :IMAGE_TAG image, so a re-upgrade would be a no-op.
CSI_HELM_NEEDED=true
if [ "${IMAGE_BUILT[cfgd-csi]:-true}" != "true" ] \
    && helm status cfgd-csi -n cfgd-system >/dev/null 2>&1; then
    CSI_HELM_NEEDED=false
fi

if [ "$CSI_HELM_NEEDED" != "true" ]; then
    echo "Deploying CSI driver... cfgd-csi image unchanged and release present — skipping Helm upgrade"
else
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
fi

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
    kubectl port-forward -n cfgd-system svc/cfgd-server 18099:8080 >/dev/null 2>&1 &
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

# --- Step 14: Record last-green SHA per image ---
# Reached only after every prior step succeeded (set -e). Persisting HEAD as the
# last-green SHA here is what lets the NEXT run's image_decision skip unchanged
# images. Best-effort writes — a ConfigMap write failure just forces a rebuild
# next run, never a stale skip.
if [ -n "$GIT_SHA" ]; then
    echo "Recording last-green SHA ($GIT_SHA) for branch $E2E_BRANCH..."
    for img in cfgd cfgd-operator cfgd-csi function-cfgd; do
        record_green_sha "$img"
    done
fi

echo ""
echo "=== E2E Setup Complete ==="
echo "  Operator:  $(kubectl get pods -n cfgd-system -l app=cfgd-operator -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo 'unknown')"
echo "  Gateway:   $(kubectl get pods -n cfgd-system -l app=cfgd-server -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo 'unknown')"
echo "  CSI:       $(kubectl get ds -n cfgd-system -l app.kubernetes.io/component=csi-driver -o jsonpath='{.items[0].status.numberReady}' 2>/dev/null || echo 'N/A') ready"
echo "  Images:    ${REGISTRY}/cfgd:${IMAGE_TAG}"
