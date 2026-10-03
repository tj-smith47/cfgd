#!/usr/bin/env bash
# Idempotent pre-flight script for cfgd E2E tests.
# Builds images, pushes to registry, ensures persistent infrastructure is current.
# Works both in CI (ARC runners) and locally.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
# shellcheck source=tests/e2e/common/helpers.sh
source "$SCRIPT_DIR/common/helpers.sh"

echo "=== cfgd E2E Setup ==="
echo "Registry: $REGISTRY"
echo "Image references (ArgoCD-owned components run the release their manifests pin):"
for img in cfgd cfgd-operator cfgd-csi function-cfgd; do
    echo "  $(e2e_image "$img")"
done

# Where something other than setup owns a component's spec, a tag override for
# it cannot take effect there, so setup says so.
warn_override_unused() {
    local image="$1" owner="$2" kind="$3" name="$4" container="$5"
    e2e_override_unused_warning "$image" "$owner" "$kind/$name" \
        "$(running_image "$kind" "$name" "$container")"
}

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
ensure_namespace cfgd-system

# --- Step 1c: Serialize on shared cluster state via a coordination Lease ---
# Two near-simultaneous setups mutate the same cluster-scoped state (CRDs,
# webhooks, the singleton operator/server deployments, the PR install's
# CSIDriver). A coordination.k8s.io/Lease named cfgd-e2e-setup serializes
# them: the holder identity is GITHUB_RUN_ID and a background renewer advances
# renewTime every third of the duration. If the holder dies, renewTime stops
# and any waiter steals the lease once it expires — auto-release on holder
# death without a permanent lock.
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
# The scratch root helpers.sh made is removed here: this script exits through
# this trap, which does not call cleanup_e2e.
trap 'release_lease; [ -z "${E2E_SCRATCH_OWNED:-}" ] || rm -rf "$E2E_SCRATCH_OWNED"' EXIT

acquire_lease
start_lease_renewer

echo "Checking runner permissions..."
PREFLIGHT_OK=true
for check in \
    "get customresourcedefinitions" \
    "create clusterroles" \
    "get nodes" \
    "create csidrivers"; do
    read -ra verb <<<"$check"
    if ! kubectl auth can-i "${verb[@]}" --all-namespaces >/dev/null 2>&1; then
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
# Registry-backed buildx cache flags for one image scope, or nothing when
# caching is disabled (local runs). Emitted onto a buildx arg array via mapfile.
# `type=registry` (a `:buildcache` tag beside the image in ghcr) replaces
# `type=gha`: GitHub's Actions cache is a 10GB/repo LRU pool, and three
# mode=max image caches evict one another between runs, so every push
# recompiled the cargo-chef dependency layer from cold and overran the setup
# timeout. The registry cache has no LRU eviction, so an unchanged dependency
# layer restores across runs and the build stays warm.
buildcache_args() {
    [ "${SCCACHE_GHA_ENABLED:-}" = "true" ] || return 0
    local ref
    ref="$(e2e_image_repo "$1"):buildcache"
    # ignore-error: the cache export is an optimization, and a registry-side
    # blob rejection during it aborts an otherwise-successful build — which
    # fails setup, which cascades every E2E suite to "skipped" without a
    # single test having run. A cold next build is the correct penalty.
    printf '%s\n' "--cache-from" "type=registry,ref=${ref}" \
                  "--cache-to" "type=registry,ref=${ref},mode=max,ignore-error=true"
}

echo "Extracting cfgd-gen-crds from Dockerfile.operator..."
mkdir -p "$REPO_ROOT/target/release"
crds_args=(buildx build --target crds
    --output "type=local,dest=$REPO_ROOT/target/release"
    -f "$REPO_ROOT/Dockerfile.operator")
mapfile -t _crds_cache < <(buildcache_args cfgd-operator)
crds_args+=("${_crds_cache[@]}")
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
    kubectl create configmap "$LAST_GREEN_CM" -n cfgd-system >/dev/null 2>&1 || true # rc-ok: idempotent ensure; the merge patch below is what records the value
    kubectl patch configmap "$LAST_GREEN_CM" -n cfgd-system --type merge \
        -p "{\"data\":{\"${image}_${E2E_BRANCH_KEY}\":\"${GIT_SHA}\"}}" >/dev/null 2>&1 || true # rc-ok: last-green bookkeeping for the next run; no case asserts on it
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

# buildx + registry cache: unchanged layers restore from the per-image
# `:buildcache` tag instead of recompiling (see buildcache_args for why
# registry, not gha). Gated on SCCACHE_GHA_ENABLED so local invocations without
# a registry fall back to a plain `docker buildx build --load`.
build_image() {
    local dockerfile="$1" tag="$2" context="$3" scope="$4"
    local args=(buildx build --load -f "$dockerfile" -t "$tag" "$context")
    mapfile -t _img_cache < <(buildcache_args "$scope")
    args+=("${_img_cache[@]}")
    docker "${args[@]}"
}

# Build + push an image only when its inputs changed since last-green; otherwise
# verify the previously-pushed image reference still exists in the registry. If the
# skip-candidate tag is missing (GC raced, registry wiped), fall through to a
# build so a green run never ships a dangling tag. Rust images also retag :latest
# so ArgoCD-managed deployments pick up new code.
#
# Cargo.lock + Cargo.toml are workspace-shared; cfgd-core is linked by every Rust
# binary, so a change there rebuilds all three Rust images.
RUST_SHARED_PATHS=(Cargo.lock Cargo.toml crates/cfgd-core)

# IMAGE_BUILT[<image>] = "true" when this run rebuilt+pushed the image, "false"
# when it was skipped. The operator rollout restart reads it to skip a no-op
# restart on an unchanged image.
declare -A IMAGE_BUILT

# An overridden image is used as it is: never built, pushed or retagged. Setup
# stops when the registry does not hold it, since a deploy of that reference
# could not pull.
use_overridden_image() {
    local ref
    ref="$(e2e_image "$1")"
    if ! docker manifest inspect "$ref" >/dev/null 2>&1; then
        echo "ERROR: $ref is not in the registry. Its tag comes from an override, so setup uses it as it is and never builds it." >&2
        exit 1
    fi
    echo "  USE ${1}: $ref (tag override, used as it is)"
}

build_and_push() {
    local image="$1" dockerfile="$2" context="$3" scope="$4" retag_latest="$5"; shift 5
    local input_paths=("$@")
    local df_rel="${dockerfile#"$REPO_ROOT/"}"
    local ref latest
    ref="$(e2e_image "$image")"
    latest="$(e2e_image_repo "$image"):latest"

    if e2e_image_overridden "$image"; then
        use_overridden_image "$image"
        IMAGE_BUILT[$image]="false"
        return 0
    fi

    local decision
    decision="$(image_decision "$image" "$df_rel" "${input_paths[@]}")"

    if [ "$decision" = "skip" ]; then
        # Confirm the tag the deploys reference actually exists before trusting
        # the skip — fail OPEN to a build if the registry lost it.
        if docker manifest inspect "$ref" >/dev/null 2>&1; then
            echo "  SKIP ${image}: no source change since $(e2e_image_repo "$image") last-green"
            if [ "$retag_latest" = "true" ]; then
                docker pull "$ref" >/dev/null 2>&1 || true
                docker tag "$ref" "$latest" 2>/dev/null || true
                docker push "$latest" 2>/dev/null || true
            fi
            IMAGE_BUILT[$image]="false"
            return 0
        fi
        echo "  ${image}: last-green unchanged but $ref missing from registry — rebuilding"
    fi

    echo "  BUILD ${image}..."
    build_image "$dockerfile" "$ref" "$context" "$scope"
    docker push "$ref"
    if [ "$retag_latest" = "true" ]; then
        docker tag "$ref" "$latest"
        docker push "$latest"
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
FUNCTION_IMAGE="$(e2e_image function-cfgd)"
FUNCTION_LATEST="$(e2e_image_repo function-cfgd):latest"
FUNCTION_DECISION="$(image_decision function-cfgd function-cfgd/Dockerfile function-cfgd)"
if e2e_image_overridden function-cfgd; then
    use_overridden_image function-cfgd
    FUNCTION_DECISION="override"
elif [ "$FUNCTION_DECISION" = "skip" ] && docker manifest inspect "$FUNCTION_IMAGE" >/dev/null 2>&1; then
    echo "  SKIP function-cfgd: no source change since last-green"
else
    echo "  BUILD function-cfgd..."
    build_image "$REPO_ROOT/function-cfgd/Dockerfile" \
        "$FUNCTION_IMAGE" "$REPO_ROOT/function-cfgd" function-cfgd
    docker push "$FUNCTION_IMAGE"
    docker tag "$FUNCTION_IMAGE" "$FUNCTION_LATEST"
    docker push "$FUNCTION_LATEST"
    FUNCTION_DECISION="build"
fi

# The xpkg repackages function-cfgd's embedded runtime image. When the image
# was rebuilt this run, the xpkg must follow; when skipped, the existing xpkg
# tag is still valid and we avoid the crank install + build + push entirely.
if [ "$FUNCTION_DECISION" = "build" ]; then
    # Ensure crossplane CLI (crank). In CI the checksum-verified pinned binary
    # is already on PATH via .github/actions/setup-crossplane (the pin's SSOT);
    # this fallback exists for local runs only. Same version, and checksum
    # verification on amd64 (the only arch the action pins a hash for) —
    # the upstream install.sh from `main` is unpinned and unchecked, and
    # rejects `linux / x86_64` as of late May 2026.
    if ! which crossplane &>/dev/null; then
        CROSSPLANE_VERSION="v2.3.2"
        # One pinned digest per arch this installs on, so no arch reaches the
        # xpkg steps unverified: an `if amd64` guard left the arm64 binary
        # checked by nothing at all. An arch with no pinned digest is refused
        # rather than trusted.
        case "$(uname -m)" in
            x86_64|amd64)
                CROSSPLANE_ARCH=amd64
                CROSSPLANE_SHA256="42ce17e97dff7ea28b624bf23cde836abb0783d07c5e02fc6de08f67dbd509eb"
                ;;
            aarch64|arm64)
                CROSSPLANE_ARCH=arm64
                CROSSPLANE_SHA256="a03182321957d178f19a3218520112a8967fbc1f88e748097d950660f1ec9a47"
                ;;
            *) echo "unsupported arch: $(uname -m)"; exit 1 ;;
        esac
        curl -fsSL --retry 5 --retry-all-errors --retry-delay 2 --connect-timeout 10 \
            -o /usr/local/bin/crossplane \
            "https://releases.crossplane.io/stable/${CROSSPLANE_VERSION}/bin/linux_${CROSSPLANE_ARCH}/crank"
        echo "${CROSSPLANE_SHA256}  /usr/local/bin/crossplane" | sha256sum -c -
        chmod +x /usr/local/bin/crossplane
        echo "Installed crossplane:"
        crossplane version
    fi

    echo "Building function-cfgd xpkg..."
    XPKG_OUT="${RUNNER_TEMP:-/tmp}/function-cfgd.xpkg"
    crossplane xpkg build \
        --package-root="$REPO_ROOT/function-cfgd/package" \
        --embed-runtime-image="$FUNCTION_IMAGE" \
        -o "$XPKG_OUT"
    crossplane xpkg push "$FUNCTION_IMAGE" -f "$XPKG_OUT"
    crossplane xpkg push "$FUNCTION_LATEST" -f "$XPKG_OUT"

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

# --- Step 4: Check the PR's CRDs against the cluster's ---
echo "Comparing the PR's CRDs with the cluster's..."
CRD_YAML=$("$REPO_ROOT/target/release/cfgd-gen-crds")
if [ -z "$CRD_YAML" ]; then
    echo "ERROR: cfgd-gen-crds produced no output"
    exit 1
fi
printf '%s\n' "$CRD_YAML" | check_pr_crds || exit 1

# --- Step 5: Apply cert-manager webhook TLS ---
echo "Applying webhook TLS (cert-manager)..."
kubectl apply -f "$SCRIPT_DIR/manifests/e2e-webhook-tls.yaml"

# --- Step 6: Install the PR's operator and CSI driver ---
# The live release in cfgd-system runs whatever its owner pins, so this run's
# operator and CSI images are installed as a second release beside it. That
# release watches, validates and injects only for objects and namespaces that
# carry this run's label, and registers its own CSIDriver.
echo "Installing the PR operator and CSI driver ($E2E_INSTALL_RELEASE in $E2E_INSTALL_NS)..."

# Every run's install registers the same CSIDriver name, and Helm's
# release-namespace annotation says whose it is. An owner whose namespace is
# gone died without uninstalling, so its CSIDriver is removed.
if ! csi_driver_obj=$(kubectl get csidriver "$CSI_DRIVER_NAME" --ignore-not-found -o name); then
    echo "ERROR: could not read csidriver/$CSI_DRIVER_NAME. Check that the runner can get csidrivers, then rerun setup."
    exit 1
fi
if [ -n "$csi_driver_obj" ]; then
    if ! csi_owner_ns=$(kubectl get csidriver "$CSI_DRIVER_NAME" \
        -o jsonpath='{.metadata.annotations.meta\.helm\.sh/release-namespace}'); then
        echo "ERROR: could not read the Helm owner of csidriver/$CSI_DRIVER_NAME. Check that the runner can get csidrivers, then rerun setup."
        exit 1
    fi
    if [ -z "$csi_owner_ns" ]; then
        echo "ERROR: csidriver/$CSI_DRIVER_NAME exists and no Helm release owns it, so the PR install cannot take it over."
        echo "  Delete it (kubectl delete csidriver $CSI_DRIVER_NAME) and rerun setup."
        exit 1
    fi
    if [ "$csi_owner_ns" != "$E2E_INSTALL_NS" ]; then
        if ! csi_owner_ns_phase=$(kubectl get namespace "$csi_owner_ns" --ignore-not-found \
            -o jsonpath='{.status.phase}'); then
            echo "ERROR: could not check whether namespace $csi_owner_ns, which owns csidriver/$CSI_DRIVER_NAME, still exists. Check that the runner can get namespaces, then rerun setup."
            exit 1
        fi
        if [ "$csi_owner_ns_phase" = "Terminating" ]; then
            echo "ERROR: namespace $csi_owner_ns, which owns csidriver/$CSI_DRIVER_NAME, is being deleted; rerun setup once it is gone"
            exit 1
        fi
        if [ -n "$csi_owner_ns_phase" ]; then
            echo "ERROR: $CSI_DRIVER_NAME belongs to the live install in $csi_owner_ns; one PR install runs at a time"
            echo "  Rerun setup once that run has finished. If that run is dead, delete namespace $csi_owner_ns and csidriver/$CSI_DRIVER_NAME first."
            exit 1
        fi
        echo "  csidriver/$CSI_DRIVER_NAME was left by an install in $csi_owner_ns, which no longer exists; deleting it"
        if ! kubectl delete csidriver "$CSI_DRIVER_NAME"; then
            echo "ERROR: could not delete the leftover csidriver/$CSI_DRIVER_NAME. Delete it by hand and rerun setup."
            exit 1
        fi
    fi
fi

# The subshell lets the namespace helper act on the install namespace without
# changing $E2E_NAMESPACE for the rest of setup. Its heartbeat loop is stopped
# there because its PID would be lost when the subshell returns; each suite's
# own heartbeat refreshes every namespace with the run label. The helper skips
# a namespace that already exists and only warns on a missing pull secret, so
# the label, the janitor's two annotations and the secret are read back after it.
(E2E_NAMESPACE="$E2E_INSTALL_NS"; create_e2e_namespace; stop_heartbeat)
if ! install_ns_meta=$(kubectl get namespace "$E2E_INSTALL_NS" \
    -o jsonpath='{.metadata.labels.cfgd\.io/e2e-run}|{.metadata.annotations.cfgd\.io/created-at}|{.metadata.annotations.cfgd\.io/heartbeat}'); then
    install_ns_meta="||"
fi
IFS='|' read -r install_ns_run install_ns_created install_ns_heartbeat <<<"$install_ns_meta"
install_ns_missing=()
[ "$install_ns_run" = "$E2E_RUN_ID" ] || install_ns_missing+=("label $E2E_RUN_LABEL")
[ -n "$install_ns_created" ] || install_ns_missing+=("annotation cfgd.io/created-at")
[ -n "$install_ns_heartbeat" ] || install_ns_missing+=("annotation cfgd.io/heartbeat")
if [ "${#install_ns_missing[@]}" -gt 0 ]; then
    echo "ERROR: namespace $E2E_INSTALL_NS is missing or does not carry: $(printf '%s, ' "${install_ns_missing[@]}" | sed 's/, $//'). Check that the runner can create, label and annotate namespaces, then rerun setup."
    exit 1
fi
if ! kubectl get secret registry-credentials -n "$E2E_INSTALL_NS" -o name >/dev/null; then
    echo "ERROR: secret registry-credentials did not reach $E2E_INSTALL_NS within 30s; the PR install pulls its images and the CSI driver's registry login with it."
    echo "  Check that Reflector is running and that registry-credentials allows reflection to every namespace, then rerun setup."
    exit 1
fi

if ! helm upgrade --install "$E2E_INSTALL_RELEASE" "$REPO_ROOT/chart/cfgd" -n "$E2E_INSTALL_NS" \
    --skip-crds -f "$SCRIPT_DIR/manifests/pr-install-values.yaml" \
    --set "operator.image.repository=$(e2e_image_repo cfgd-operator)" \
    --set "operator.image.tag=$(e2e_image_tag cfgd-operator)" \
    --set "csiDriver.image.repository=$(e2e_image_repo cfgd-csi)" \
    --set "csiDriver.image.tag=$(e2e_image_tag cfgd-csi)" \
    --set "csiDriver.extraEnv[0].name=OCI_INSECURE_REGISTRIES" \
    --set "csiDriver.extraEnv[0].value=${REGISTRY}:5000" \
    --set "csiDriver.extraEnv[1].name=DOCKER_CONFIG" \
    --set "csiDriver.extraEnv[1].value=/etc/cfgd/docker" \
    --set-string "csiDriver.name=$CSI_DRIVER_NAME" \
    --set-string "operator.watchLabelSelector=cfgd.io/e2e-run=${E2E_RUN_ID}" \
    --set-json "webhook.objectSelector={\"matchLabels\":{\"cfgd.io/e2e-run\":\"${E2E_RUN_ID}\"}}" \
    --set-json "mutatingWebhook.namespaceSelector={\"matchExpressions\":[{\"key\":\"cfgd.io/inject-modules\",\"operator\":\"In\",\"values\":[\"true\"]},{\"key\":\"cfgd.io/e2e-run\",\"operator\":\"In\",\"values\":[\"${E2E_RUN_ID}\"]}]}" \
    --wait --timeout=180s; then
    echo "ERROR: helm upgrade --install $E2E_INSTALL_RELEASE in $E2E_INSTALL_NS failed. Read the Helm error above and the pods in $E2E_INSTALL_NS (kubectl get pods -n $E2E_INSTALL_NS), fix the cause and rerun setup."
    exit 1
fi

# The webhooks fail closed, so until cert-manager's CA injector fills in their
# caBundle the API server rejects every labelled object the suites create.
for pr_webhook in "validatingwebhookconfiguration/$E2E_VALIDATING_WEBHOOK" \
    "mutatingwebhookconfiguration/$E2E_MUTATING_WEBHOOK"; do
    pr_ca_bundle=""
    for _ in $(seq 1 60); do
        pr_ca_bundle=$(kubectl get "$pr_webhook" \
            -o jsonpath='{.webhooks[0].clientConfig.caBundle}' 2>/dev/null || echo "")
        if [ -n "$pr_ca_bundle" ]; then
            break
        fi
        sleep 2
    done
    if [ -z "$pr_ca_bundle" ]; then
        echo "ERROR: $pr_webhook has no caBundle after 120s, so the API server cannot call it."
        echo "  certificate/$E2E_WEBHOOK_CERT in $E2E_INSTALL_NS reports:"
        kubectl get certificate "$E2E_WEBHOOK_CERT" -n "$E2E_INSTALL_NS" \
            -o jsonpath='{range .status.conditions[*]}    {.type}={.status} {.reason}: {.message}{"\n"}{end}' \
            || echo "    (the certificate could not be read)"
        echo "  Check that cert-manager and its CA injector are running, then rerun setup."
        exit 1
    fi
done

if ! wait_for_daemonset "$E2E_INSTALL_NS" "$E2E_CSI_DS" 120; then
    echo "ERROR: daemonset/$E2E_CSI_DS in $E2E_INSTALL_NS is not ready after 120s. Read the description above, fix the cause and rerun setup."
    exit 1
fi
pr_operator_image="$(running_image deployment "$E2E_OPERATOR_DEPLOY" operator "$E2E_INSTALL_NS")"
pr_csi_image="$(running_image daemonset "$E2E_CSI_DS" cfgd-csi "$E2E_INSTALL_NS")"
if [ "$pr_operator_image" != "$(e2e_image cfgd-operator)" ] || [ "$pr_csi_image" != "$(e2e_image cfgd-csi)" ]; then
    echo "ERROR: the PR install does not run this run's images."
    echo "  deployment/$E2E_OPERATOR_DEPLOY runs $pr_operator_image, want $(e2e_image cfgd-operator)"
    echo "  daemonset/$E2E_CSI_DS runs $pr_csi_image, want $(e2e_image cfgd-csi)"
    echo "  Check the image flags of the helm upgrade above, then rerun setup."
    exit 1
fi
echo "  PR operator runs $pr_operator_image"
echo "  PR CSI driver runs $pr_csi_image"

# --- Step 7: Update operator image ---
echo "Updating operator image..."
# ArgoCD owns the shared cluster's operator and gateway Deployments and runs
# the release /db/manifests pins, reverting anything applied here, so nothing
# this run builds reaches them and a restart would only re-pull that release.
ARGOCD_MANAGED=false
argocd_rc=0
argocd_owner deployment cfgd-operator cfgd-system "rerun setup" || argocd_rc=$?
case "$argocd_rc" in
    0) ARGOCD_MANAGED=true ;;
    1) ;;
    *) exit 1 ;;
esac

if [ "$ARGOCD_MANAGED" = "true" ]; then
    for deploy in cfgd-operator cfgd-server; do
        echo "  deployment/$deploy is managed by ArgoCD and runs $(running_image deployment "$deploy" cfgd-operator)"
    done
    warn_override_unused cfgd-operator ArgoCD deployment cfgd-server cfgd-operator
elif [ -n "${CFGD_DEPLOY_MANIFESTS:-}" ] && [ -d "$CFGD_DEPLOY_MANIFESTS" ]; then
    # A tree `task deploy:operator` applied owns these Deployments, so setup
    # leaves their spec alone. They name the :latest tag this run pushes, and a
    # restart is what makes them pull a rebuilt image. Skipping it when the
    # image was not rebuilt is the bulk of the no-source-change time saving.
    warn_override_unused cfgd-operator "the tree at $CFGD_DEPLOY_MANIFESTS" \
        deployment cfgd-server cfgd-operator
    if [ "${IMAGE_BUILT[cfgd-operator]:-true}" != "true" ]; then
        echo "  cfgd-operator image unchanged — skipping operator/server rollout restart"
    else
    echo "  Deployments applied from $CFGD_DEPLOY_MANIFESTS — restarting to pull :latest..."

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
    sed "s|IMAGE_PLACEHOLDER|$(e2e_image cfgd-operator)|g" \
        "$SCRIPT_DIR/operator/manifests/operator-deployment.yaml" | kubectl apply -f -
    sed "s|IMAGE_PLACEHOLDER|$(e2e_image cfgd-operator)|g" \
        "$SCRIPT_DIR/node/manifests/cfgd-server.yaml" | kubectl apply -f -
fi

# --- Step 8: Apply webhook configurations ---
# The release's webhooks leave every object and namespace carrying a run label
# to that run's install. Setup can only keep that scoping on objects it owns: an
# object ArgoCD tracks gets reverted on the next sync, so setup stops there.
for release_webhook in "validatingwebhookconfiguration/$E2E_RELEASE_VALIDATING_WEBHOOK" \
    "mutatingwebhookconfiguration/$E2E_RELEASE_MUTATING_WEBHOOK"; do
    argocd_rc=0
    argocd_owner "${release_webhook%%/*}" "${release_webhook#*/}" - "rerun setup" || argocd_rc=$?
    case "$argocd_rc" in
        0)
            echo "ERROR: $release_webhook carries an argocd.argoproj.io/tracking-id annotation, so ArgoCD owns it and would revert what setup applies."
            echo "  Add the cfgd.io/e2e-run DoesNotExist selectors from this step to its manifest in the GitOps repo, drop it from the heredoc this step applies, then rerun setup."
            exit 1
            ;;
        1) ;;
        *) exit 1 ;;
    esac
done
echo "Applying webhook configurations..."
# Get the CA bundle from the cert-manager-generated secret
echo "  Waiting for webhook TLS secret..."
CA_BUNDLE=""
for _ in $(seq 1 60); do
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
trap 'rm -f "$WEBHOOK_FILE"; release_lease; [ -z "${E2E_SCRATCH_OWNED:-}" ] || rm -rf "$E2E_SCRATCH_OWNED"' EXIT
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
  name: ${E2E_RELEASE_VALIDATING_WEBHOOK}
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
    objectSelector:
      matchExpressions:
        - key: cfgd.io/e2e-run
          operator: DoesNotExist
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
    objectSelector:
      matchExpressions:
        - key: cfgd.io/e2e-run
          operator: DoesNotExist
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
    objectSelector:
      matchExpressions:
        - key: cfgd.io/e2e-run
          operator: DoesNotExist
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
    objectSelector:
      matchExpressions:
        - key: cfgd.io/e2e-run
          operator: DoesNotExist
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
    objectSelector:
      matchExpressions:
        - key: cfgd.io/e2e-run
          operator: DoesNotExist
  - name: validate-backuppolicy.cfgd.io
    admissionReviewVersions: [v1]
    clientConfig:
      service:
        name: cfgd-operator
        namespace: cfgd-system
        path: /validate-backuppolicy
      caBundle: "${CA_BUNDLE}"
    rules:
      - apiGroups: ["cfgd.io"]
        apiVersions: ["v1alpha1"]
        operations: [CREATE, UPDATE]
        resources: [backuppolicies]
    failurePolicy: Fail
    sideEffects: None
    objectSelector:
      matchExpressions:
        - key: cfgd.io/e2e-run
          operator: DoesNotExist
---
apiVersion: admissionregistration.k8s.io/v1
kind: MutatingWebhookConfiguration
metadata:
  name: ${E2E_RELEASE_MUTATING_WEBHOOK}
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
        - {key: cfgd.io/e2e-run, operator: DoesNotExist}
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

# --- Step 9: Wait for all components ---
echo "Waiting for components..."
wait_for_deployment cfgd-system cfgd-operator 120
wait_for_deployment cfgd-system cfgd-server 120

# --- Step 10: Reset gateway DB for clean E2E state ---
# Call the admin reset endpoint to wipe stale device/event data from prior runs.
# This is safe: the endpoint is behind admin auth and only deletes data rows,
# not the SQLite file (avoids Longhorn volume corruption from rm -f on live DB).
GW_API_KEY=$(kubectl get deployment cfgd-server -n cfgd-system \
    -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="CFGD_API_KEY")].value}' 2>/dev/null || echo "")
if [ -n "$GW_API_KEY" ]; then
    # Port-forward to gateway, reset, then clean up
    RESET_RESP=""
    if PF_PID=$(port_forward cfgd-system svc/cfgd-server 18099 8080); then
        RESET_RESP=$(curl -sf -X POST "http://localhost:18099/api/v1/admin/reset" \
            -H "Authorization: Bearer $GW_API_KEY" 2>/dev/null || echo "")
        stop_port_forward "$PF_PID"
    fi
    if [ -n "$RESET_RESP" ]; then
        echo "  Gateway DB reset: $RESET_RESP"
    else
        echo "  WARN: Gateway DB reset failed (endpoint may not exist yet)"
    fi
fi

# --- Step 11: Record last-green SHA per image ---
# Reached only after every prior step succeeded (set -e). Persisting HEAD as the
# last-green SHA here is what lets the NEXT run's image_decision skip unchanged
# images. Best-effort writes — a ConfigMap write failure just forces a rebuild
# next run, never a stale skip.
if [ -n "$GIT_SHA" ]; then
    echo "Recording last-green SHA ($GIT_SHA) for branch $E2E_BRANCH..."
    for img in cfgd cfgd-operator cfgd-csi function-cfgd; do
        # This run never built an overridden image, so HEAD is not green for it.
        e2e_image_overridden "$img" && continue
        record_green_sha "$img"
    done
fi

echo ""
echo "=== E2E Setup Complete ==="
echo "  Operator:  $(kubectl get pods -n cfgd-system -l app=cfgd-operator -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo 'unknown')"
echo "  Gateway:   $(kubectl get pods -n cfgd-system -l app=cfgd-server -o jsonpath='{.items[0].status.phase}' 2>/dev/null || echo 'unknown')"
echo "  CSI:       $(kubectl get ds -n cfgd-system -l app.kubernetes.io/component=csi-driver -o jsonpath='{.items[0].status.numberReady}' 2>/dev/null || echo 'N/A') ready"
echo "  Running:   operator $(running_image deployment cfgd-operator cfgd-operator)"
echo "             gateway $(running_image deployment cfgd-server cfgd-operator)"
echo "             csi $(running_image daemonset cfgd-csi-csi cfgd-csi)"
echo "  PR:        $E2E_INSTALL_RELEASE in $E2E_INSTALL_NS, CSI driver $CSI_DRIVER_NAME"
echo "             operator $(running_image deployment "$E2E_OPERATOR_DEPLOY" operator "$E2E_INSTALL_NS")"
echo "             csi $(running_image daemonset "$E2E_CSI_DS" cfgd-csi "$E2E_INSTALL_NS")"
echo "  Test pod:  $(e2e_image cfgd)"
echo "  Function:  $(e2e_image function-cfgd)"
