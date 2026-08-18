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
set -euo pipefail

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

teardown() {
    log "Tearing down"
    # `kind delete` needs a kubeconfig path it may write to; a missing file is
    # fine, a missing directory is not.
    mkdir -p "$WORK_DIR"
    kind delete cluster --name "$CLUSTER_NAME" >/dev/null 2>&1 || true
    docker rm -f "$REGISTRY_NAME" >/dev/null 2>&1 || true
    rm -rf "$WORK_DIR"
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
# Endpoints pointing at the registry container's address on the kind network is
# what makes `kind-registry:5000` mean the same thing inside a cfgd-system pod
# as it does on the node.
publish_registry_service() {
    log "Publishing $REGISTRY_NAME as a Service in $NAMESPACE"
    local reg_ip
    reg_ip="$(docker inspect -f '{{.NetworkSettings.Networks.kind.IPAddress}}' "$REGISTRY_NAME")"
    if [ -z "$reg_ip" ]; then
        echo "Could not determine $REGISTRY_NAME's address on the kind network." >&2
        exit 1
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
apiVersion: v1
kind: Endpoints
metadata:
  name: ${REGISTRY_NAME}
  namespace: ${NAMESPACE}
subsets:
  - addresses:
      - ip: ${reg_ip}
    ports:
      - name: registry
        port: 5000
        protocol: TCP
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

install_cert_manager() {
    log "Installing cert-manager $CERT_MANAGER_VERSION"
    kubectl apply -f \
        "https://github.com/cert-manager/cert-manager/releases/download/${CERT_MANAGER_VERSION}/cert-manager.yaml"
    kubectl wait --for=condition=Available --timeout=300s \
        -n cert-manager deployment/cert-manager \
        deployment/cert-manager-webhook deployment/cert-manager-cainjector
}

install_chart() {
    log "Installing the cfgd chart into $NAMESPACE"
    # failurePolicy=Fail, against the chart's Ignore default: an injection the
    # webhook silently skipped produces a pod that runs with no module and no
    # error, which is a worse demo (and a worse validation signal) than a loud
    # refusal.
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

    log "Waiting for the operator, the CSI driver, and the webhook certificate"
    kubectl -n "$NAMESPACE" rollout status deployment/cfgd-operator --timeout=180s
    kubectl -n "$NAMESPACE" rollout status daemonset/cfgd-csi --timeout=180s
    # The MutatingWebhookConfiguration's caBundle is written by cert-manager's
    # cainjector after the Certificate is issued. Creating the demo pod before
    # that lands fails admission outright under failurePolicy=Fail.
    kubectl wait --for=condition=Ready --timeout=180s \
        -n "$NAMESPACE" certificate/cfgd-webhook-tls
}

prepare_namespace() {
    log "Preparing the $DEMO_NAMESPACE namespace"
    kubectl create namespace "$DEMO_NAMESPACE" --dry-run=client -o yaml | kubectl apply -f -
    kubectl label namespace "$DEMO_NAMESPACE" cfgd.io/inject-modules=true --overwrite
    # Pins the tape's kubectl beats to the injection-enabled namespace without a
    # `-n demo` on every line, which would say nothing about what cfgd does.
    kubectl config set-context --current --namespace="$DEMO_NAMESPACE"
}

# The tape types `cfgd module push ./tools`, `kubectl apply -f module.yaml` and
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
  packages: []
  files:
    - source: bin/hello.sh
      target: bin/hello.sh
EOF

    cat > "$fixture/tools/bin/hello.sh" <<'EOF'
#!/bin/sh
echo "hello from the tools module"
EOF
    chmod +x "$fixture/tools/bin/hello.sh"

    # `ociArtifact` names the registry the way the CLUSTER reaches it. The host
    # pushes to the same registry through its published port
    # (localhost:5001), so the two spellings address one registry.
    cat > "$fixture/module.yaml" <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: tools
spec:
  packages: []
  ociArtifact: "${REGISTRY_NAME}:5000/demo/tools:v1"
  mountPolicy: Always
EOF

    cat > "$fixture/pod.yaml" <<'EOF'
apiVersion: v1
kind: Pod
metadata:
  name: demo-pod
  annotations:
    cfgd.io/modules: "tools:v1"
spec:
  containers:
    - name: app
      image: busybox:1.36
      command: ["sleep", "3600"]
EOF

    # The tape runs `cfgd` and `kubectl cfgd` from the freshly built tree, never
    # from whatever version happens to be on the recording host's PATH.
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

    start_registry
    create_cluster
    publish_registry_service
    build_and_load_images
    install_cert_manager
    install_chart
    prepare_namespace
    write_fixture

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
