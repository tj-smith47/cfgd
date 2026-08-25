#!/usr/bin/env bash
# Bring up (and tear down) the throwaway kind cluster the k8s demo tape records
# against: operator + mutating webhook + CSI driver, a local OCI registry, and
# the module/pod fixture files the tape types.
#
# Everything this script touches is scoped to its own kubeconfig
# ($CFGD_DEMO_K8S_DIR/kubeconfig) and its own kind cluster. It never reads or
# writes the caller's default kubeconfig, so recording the demo cannot point a
# single kubectl at a real cluster.
#
# Usage:
#   setup-k8s-demo.sh up     # create cluster, build+load images, install chart
#   setup-k8s-demo.sh down   # delete cluster, registry container, staged dirs
# -E so up()'s ERR trap is inherited by the stage functions it calls; without it
# bash runs no ERR trap for a failure inside a function, which is where every
# stage of the bring-up runs.
set -Eeuo pipefail

cd "$(dirname "$0")/../.."
REPO_ROOT="$PWD"

CLUSTER_NAME="cfgd-demo"
REGISTRY_NAME="kind-registry"
REGISTRY_HOST_PORT="5001"
IMAGE_TAG="demo"
NAMESPACE="cfgd-system"
DEMO_NAMESPACE="demo"

# Kept out of the repo tree so a half-finished run leaves nothing for git to
# see, and out of /tmp because that is tmpfs on this host.
WORK_DIR="${CFGD_DEMO_K8S_DIR:-$HOME/.cache/cfgd-debug/demo-k8s}"
export KUBECONFIG="$WORK_DIR/kubeconfig"

# cert-manager provides the webhook's serving certificate and the caBundle
# injection the chart's MutatingWebhookConfiguration annotation asks for; the
# operator's webhook cannot serve TLS without it.
CERT_MANAGER_VERSION="v1.16.2"

log() { printf '\n==> %s\n' "$1"; }

require() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "$1 is required and was not found on PATH." >&2
        exit 1
    }
}

# `$CFGD_DEMO_K8S_DIR` is an override, so the path teardown recursively deletes
# is caller-supplied. Refuse anything that is not a real subdirectory of $HOME:
# an empty or unset value would make the delete `rm -rf /`-shaped, and `$HOME`
# itself is the one subdirectory-of-$HOME that must never be the target.
assert_work_dir_deletable() {
    case "$WORK_DIR" in
        "") echo "CFGD_DEMO_K8S_DIR resolved to an empty path; refusing to delete." >&2; return 1 ;;
        "$HOME") echo "CFGD_DEMO_K8S_DIR is \$HOME; refusing to delete." >&2; return 1 ;;
        "$HOME"/?*) return 0 ;;
        *) echo "CFGD_DEMO_K8S_DIR ($WORK_DIR) is outside \$HOME; refusing to delete." >&2; return 1 ;;
    esac
}

teardown() {
    log "Tearing down"
    # `kind delete` needs a kubeconfig path it may write to; a missing file is
    # fine, a missing directory is not.
    mkdir -p "$WORK_DIR"
    kind delete cluster --name "$CLUSTER_NAME" >/dev/null 2>&1 || true
    docker rm -f "$REGISTRY_NAME" >/dev/null 2>&1 || true
    if assert_work_dir_deletable; then
        rm -rf "$WORK_DIR"
    fi
    echo "Deleted kind cluster '$CLUSTER_NAME', registry '$REGISTRY_NAME', and $WORK_DIR"
}

# ---------------------------------------------------------------------------

start_registry() {
    log "Starting local OCI registry ($REGISTRY_NAME, host port $REGISTRY_HOST_PORT)"
    if [ "$(docker inspect -f '{{.State.Running}}' "$REGISTRY_NAME" 2>/dev/null || true)" != "true" ]; then
        docker rm -f "$REGISTRY_NAME" >/dev/null 2>&1 || true
        docker run -d --restart=no \
            -p "127.0.0.1:${REGISTRY_HOST_PORT}:5000" \
            --name "$REGISTRY_NAME" \
            registry:2 >/dev/null
    fi
}

create_cluster() {
    log "Creating kind cluster '$CLUSTER_NAME'"
    mkdir -p "$WORK_DIR"
    # Always start from nothing. A cluster left over from an earlier run carries
    # that run's Module CRDs and pods, and the tape's `kubectl apply` beats then
    # print `unchanged` instead of `created`.
    kind delete cluster --name "$CLUSTER_NAME" >/dev/null 2>&1 || true
    kind create cluster --name "$CLUSTER_NAME" --kubeconfig "$KUBECONFIG" --wait 120s

    # kind's documented local-registry pattern: the registry container joins the
    # cluster's docker network so the node (and, via the Service below, pods)
    # can reach it by name.
    if [ "$(docker inspect -f '{{json .NetworkSettings.Networks.kind}}' "$REGISTRY_NAME")" = "null" ]; then
        docker network connect kind "$REGISTRY_NAME"
    fi
}

# The CSI driver pulls module artifacts itself over HTTP, from inside a pod on
# the cluster network — not through containerd. Pods resolve names through
# CoreDNS, which has no view of docker's embedded DNS, so the registry's docker
# name is not resolvable there. A selector-less Service plus a hand-written
# EndpointSlice pointing at the registry container's address on the kind network
# is what makes `kind-registry:5000` mean the same thing inside a cfgd-system pod
# as it does on the node. EndpointSlice rather than the v1 Endpoints it replaces:
# Endpoints is deprecated as of k8s 1.33, so a kind node-image bump would retire
# the demo's only route to the registry.
publish_registry_service() {
    log "Publishing $REGISTRY_NAME as a Service in $NAMESPACE"
    local reg_ip
    reg_ip="$(docker inspect -f '{{.NetworkSettings.Networks.kind.IPAddress}}' "$REGISTRY_NAME")"
    if [ -z "$reg_ip" ]; then
        echo "Could not determine $REGISTRY_NAME's address on the kind network." >&2
        return 1
    fi
    kubectl create namespace "$NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -
    kubectl apply -f - <<EOF
apiVersion: v1
kind: Service
metadata:
  name: ${REGISTRY_NAME}
  namespace: ${NAMESPACE}
spec:
  ports:
    - name: registry
      port: 5000
      targetPort: 5000
      protocol: TCP
---
apiVersion: discovery.k8s.io/v1
kind: EndpointSlice
metadata:
  name: ${REGISTRY_NAME}
  namespace: ${NAMESPACE}
  labels:
    kubernetes.io/service-name: ${REGISTRY_NAME}
addressType: IPv4
ports:
  - name: registry
    port: 5000
    protocol: TCP
endpoints:
  - addresses:
      - ${reg_ip}
    conditions:
      ready: true
EOF
}

# The release Dockerfiles expect a GoReleaser-shaped context: the already-built
# binary at <ctx>/<os>/<arch>/<name>. Building that way keeps the image build to
# a COPY instead of a full in-container cargo build, which is what the dev
# Dockerfiles do.
#
# Built through cargo-zigbuild against an explicit glibc floor rather than
# plain `cargo build`: the release images are debian:bookworm-slim (glibc 2.36)
# and a natively linked binary from a host on a newer glibc dies at startup
# with `version GLIBC_2.39 not found`, which surfaces as a CrashLoopBackOff
# with no other clue. 2.36 is bookworm's, so the floor tracks the base image.
GLIBC_FLOOR="2.36"
BUILD_TARGET="x86_64-unknown-linux-gnu"

build_and_load_images() {
    log "Building cfgd-operator and cfgd-csi (release, glibc $GLIBC_FLOOR floor)"
    cargo zigbuild --release --target "${BUILD_TARGET}.${GLIBC_FLOOR}" \
        -p cfgd-operator -p cfgd-csi --bin cfgd-operator --bin cfgd-csi

    local built="$REPO_ROOT/target/${BUILD_TARGET}/release"
    local ctx="$WORK_DIR/imgctx"
    rm -rf "$ctx"
    mkdir -p "$ctx/linux/amd64"
    cp "$built/cfgd-operator" "$ctx/linux/amd64/cfgd-operator"
    cp "$built/cfgd-csi" "$ctx/linux/amd64/cfgd-csi"

    log "Building images"
    docker build -q --build-arg TARGETARCH=amd64 \
        -f "$REPO_ROOT/Dockerfile.operator.release" \
        -t "cfgd-operator:${IMAGE_TAG}" "$ctx"
    docker build -q --build-arg TARGETARCH=amd64 \
        -f "$REPO_ROOT/Dockerfile.csi.release" \
        -t "cfgd-csi:${IMAGE_TAG}" "$ctx"
    rm -rf "$ctx"

    log "Loading images into the cluster"
    kind load docker-image "cfgd-operator:${IMAGE_TAG}" "cfgd-csi:${IMAGE_TAG}" --name "$CLUSTER_NAME"
}

# The host `cfgd` the tape's first beat runs. `--bin cfgd` is not optional: a
# plain `cargo build -p cfgd` unifies features across the workspace and links a
# test-helpers cfgd-core, whose binary resolves a fake $HOME and so pushes from
# somewhere the recording never shows.
build_host_cli() {
    log "Building the host cfgd binary"
    cargo build --release --bin cfgd
    if [ ! -x "$REPO_ROOT/target/release/cfgd" ]; then
        echo "cfgd was not built at $REPO_ROOT/target/release/cfgd" >&2
        return 1
    fi
}

install_cert_manager() {
    log "Installing cert-manager $CERT_MANAGER_VERSION"
    kubectl apply -f \
        "https://github.com/cert-manager/cert-manager/releases/download/${CERT_MANAGER_VERSION}/cert-manager.yaml"
    kubectl wait --for=condition=Available --timeout=300s \
        -n cert-manager deployment/cert-manager \
        deployment/cert-manager-webhook deployment/cert-manager-cainjector
    # Available says the Deployment has ready replicas, not that the webhook's
    # Service has an endpoint behind it yet. cert-manager's own validating
    # webhook intercepts the chart's Certificate, so a create landing in that
    # window fails with a 500 that `helm --wait` cannot retry past.
    kubectl wait --for=condition=Ready --timeout=180s \
        -n cert-manager pod -l app.kubernetes.io/component=webhook
    wait_for_webhook_endpoint
}

# Poll rather than `kubectl wait`: the EndpointSlice does not exist at all until
# the first endpoint is published, and `kubectl wait` on an absent resource
# fails immediately instead of waiting for it to appear.
wait_for_webhook_endpoint() {
    local deadline=$((SECONDS + 180))
    while [ "$SECONDS" -lt "$deadline" ]; do
        local ready
        ready="$(kubectl get endpointslice -n cert-manager \
            -l kubernetes.io/service-name=cert-manager-webhook \
            -o jsonpath='{.items[*].endpoints[*].conditions.ready}' 2>/dev/null || true)"
        case "$ready" in
            *true*) return 0 ;;
        esac
        sleep 2
    done
    echo "cert-manager's webhook Service never got a ready endpoint." >&2
    return 1
}

install_chart() {
    log "Installing the cfgd chart into $NAMESPACE"
    # failurePolicy=Fail, against the chart's Ignore default: an injection the
    # webhook silently skipped produces a pod that runs with no module and no
    # error, which is a worse demo (and a worse validation signal) than a loud
    # refusal.
    # Retried because the Certificate create still races cert-manager's webhook
    # even after its endpoint is ready: the caBundle the API server holds for
    # that webhook is refreshed asynchronously, and the failure is a transient
    # 500 that `helm --wait` reports as a dead install. `upgrade --install` is
    # idempotent, so a retry resumes rather than duplicating.
    local attempt
    for attempt in 1 2 3; do
        if helm_install_attempt; then
            break
        fi
        if [ "$attempt" = 3 ]; then
            echo "helm install failed three times." >&2
            return 1
        fi
        log "helm install failed (attempt $attempt); retrying"
        sleep 10
    done

    log "Waiting for the operator, the CSI driver, and the webhook certificate"
    kubectl -n "$NAMESPACE" rollout status deployment/cfgd-operator --timeout=180s
    kubectl -n "$NAMESPACE" rollout status daemonset/cfgd-csi --timeout=180s
    # The MutatingWebhookConfiguration's caBundle is written by cert-manager's
    # cainjector after the Certificate is issued. Creating the demo pod before
    # that lands fails admission outright under failurePolicy=Fail.
    kubectl wait --for=condition=Ready --timeout=180s \
        -n "$NAMESPACE" certificate/cfgd-webhook-tls
}

helm_install_attempt() {
    helm upgrade --install cfgd "$REPO_ROOT/chart/cfgd" \
        -n "$NAMESPACE" --create-namespace \
        --set operator.enabled=true \
        --set "operator.image.repository=cfgd-operator" \
        --set "operator.image.tag=${IMAGE_TAG}" \
        --set operator.image.pullPolicy=Never \
        --set csiDriver.enabled=true \
        --set "csiDriver.image.repository=cfgd-csi" \
        --set "csiDriver.image.tag=${IMAGE_TAG}" \
        --set csiDriver.image.pullPolicy=Never \
        --set "csiDriver.extraEnv[0].name=OCI_INSECURE_REGISTRIES" \
        --set "csiDriver.extraEnv[0].value=${REGISTRY_NAME}:5000" \
        --set mutatingWebhook.enabled=true \
        --set mutatingWebhook.failurePolicy=Fail \
        --set agent.enabled=false \
        --set deviceGateway.enabled=false \
        --wait --timeout=300s
}

prepare_namespace() {
    log "Preparing the $DEMO_NAMESPACE namespace"
    kubectl create namespace "$DEMO_NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -
    kubectl label namespace "$DEMO_NAMESPACE" cfgd.io/inject-modules=true --overwrite
    # Pins the tape's kubectl beats to the injection-enabled namespace without a
    # `-n demo` on every line, which would say nothing about what cfgd does.
    kubectl config set-context --current --namespace="$DEMO_NAMESPACE"
}

# The second fixture, for connect.tape: a module whose CRD carries
# `mountPolicy: Debug`, and a pod that asks for it. Debug is what makes that
# tape's claim real — the CSI volume is staged on the pod and mounted into no
# declared container, so the module is genuinely absent from the app until an
# ephemeral debug container mounts it. A separate module name from `tools`
# because Module is cluster-scoped: one name cannot carry both policies.
write_connect_fixture() {
    local fixture="$1"
    mkdir -p "$fixture/nettools/bin"

    cat > "$fixture/nettools/module.yaml" <<'EOF'
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nettools
spec:
  files:
    - source: bin/netcheck.sh
      target: bin/netcheck.sh
EOF

    # Reads the pod's own network namespace, so its output is evidence the tool
    # is running beside the app rather than somewhere else.
    cat > "$fixture/nettools/bin/netcheck.sh" <<'EOF'
#!/bin/sh
echo "netcheck: pod $(hostname)"
ip -4 addr show eth0 2>/dev/null | awk '/inet /{print "  eth0     " $2}'
awk '/^nameserver/{print "  resolver " $2}' /etc/resolv.conf
EOF
    chmod +x "$fixture/nettools/bin/netcheck.sh"

    cat > "$fixture/debug-module.yaml" <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nettools
spec:
  ociArtifact: "${REGISTRY_NAME}:5000/demo/nettools:v1"
  mountPolicy: Debug
EOF

    cat > "$fixture/app-pod.yaml" <<'EOF'
apiVersion: v1
kind: Pod
metadata:
  name: app
  annotations:
    cfgd.io/modules: "nettools:v1"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
EOF
}

# The tape types `cfgd module push ./tools`, `kubectl apply -f module-crd.yaml` and
# `kubectl apply -f pod.yaml` against these. They are written here rather than
# in the tape so the recording opens on a ready fixture instead of on a wall of
# heredocs.
write_fixture() {
    log "Writing the demo fixture into $WORK_DIR/fixture"
    local fixture="$WORK_DIR/fixture"
    rm -rf "$fixture"
    mkdir -p "$fixture/tools/bin"

    cat > "$fixture/tools/module.yaml" <<'EOF'
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: tools
spec:
  files:
    - source: bin/hello.sh
      target: bin/hello.sh
EOF

    # A greeting with a name in it, so the tape's v2 act (sed TJ -> Taylor,
    # push, roll the pod) has a one-word diff the viewer can track across
    # detection and convergence.
    cat > "$fixture/tools/bin/hello.sh" <<'EOF'
#!/bin/sh
echo "Hello, TJ!"
EOF
    chmod +x "$fixture/tools/bin/hello.sh"

    # The signing key the tape's `cfgd module push --sign --key` beats use,
    # generated fresh per bring-up so no key material outlives the demo. An
    # empty COSIGN_PASSWORD keeps generation (and the tape's signing pushes)
    # non-interactive.
    (cd "$fixture" && COSIGN_PASSWORD="" cosign generate-key-pair >/dev/null 2>&1)
    if [ ! -s "$fixture/cosign.pub" ]; then
        echo "cosign generate-key-pair left no cosign.pub in $fixture." >&2
        return 1
    fi

    # `ociArtifact` names the registry the way the CLUSTER reaches it. The host
    # pushes to the same registry through its published port
    # (localhost:5001), so the two spellings address one registry. The CRD
    # carries the public half of the demo key: that is what the operator's
    # verification check reads, and what flips `kubectl cfgd status` from
    # `unverified` to `verified`.
    # Not `module.yaml`: the module directory's own manifest is also
    # `kind: Module`, and two files of one basename and one kind read as one
    # file on camera.
    cat > "$fixture/module-crd.yaml" <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: tools
spec:
  ociArtifact: "${REGISTRY_NAME}:5000/demo/tools:v1"
  mountPolicy: Always
  signature:
    cosign:
      publicKey: |
$(sed 's/^/        /' "$fixture/cosign.pub")
EOF

    cat > "$fixture/pod.yaml" <<'EOF'
apiVersion: v1
kind: Pod
metadata:
  name: demo-pod
  annotations:
    cfgd.io/modules: "tools:v1"
spec:
  # busybox's sleep as PID 1 ignores SIGTERM
  terminationGracePeriodSeconds: 1
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
EOF

    write_connect_fixture "$fixture"

    # The tape runs `cfgd` and `kubectl cfgd` from the freshly built tree, never
    # from whatever version happens to be on the recording host's PATH. Checked
    # rather than assumed: `ln -sf` succeeds against a missing target, and the
    # shell then skips the dangling link and resolves whatever cfgd is installed
    # on the recording host — recording a different binary than the tree under
    # test, silently.
    if [ ! -x "$REPO_ROOT/target/release/cfgd" ]; then
        echo "cfgd is missing at $REPO_ROOT/target/release/cfgd; refusing to link it." >&2
        return 1
    fi
    mkdir -p "$WORK_DIR/bin"
    ln -sf "$REPO_ROOT/target/release/cfgd" "$WORK_DIR/bin/cfgd"
    ln -sf "$REPO_ROOT/target/release/cfgd" "$WORK_DIR/bin/kubectl-cfgd"
}

up() {
    require kind
    require kubectl
    require helm
    require docker
    require cargo
    require cargo-zigbuild
    require cosign

    # A stage that fails half way leaves a kind cluster, a registry container
    # and a work dir standing; the caller's next run then starts from a state
    # nobody described. Cleared on success so `up` does not tear down what it
    # just built.
    trap 'teardown; exit 1' ERR

    start_registry
    create_cluster
    publish_registry_service
    build_host_cli
    build_and_load_images
    install_cert_manager
    install_chart
    prepare_namespace
    write_fixture

    trap - ERR

    log "Ready"
    echo "  KUBECONFIG=$KUBECONFIG"
    echo "  fixture:   $WORK_DIR/fixture"
    echo "  binaries:  $WORK_DIR/bin"
}

case "${1:-up}" in
    up) up ;;
    down) teardown ;;
    *)
        echo "usage: $0 [up|down]" >&2
        exit 1
        ;;
esac
