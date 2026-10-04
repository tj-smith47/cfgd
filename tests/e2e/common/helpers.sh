#!/usr/bin/env bash
# Shared E2E test helpers for all cfgd components.
# Source this from any test script: source "$(dirname "$0")/../../common/helpers.sh"

set -euo pipefail

E2E_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO_ROOT="$(cd "$E2E_ROOT/../.." && pwd)"

# Before anything below reads $HOME, a registry credential or a tool config.
# shellcheck source=tests/e2e/common/scratch-home.sh
source "$E2E_ROOT/common/scratch-home.sh"

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

# Each crate releases at its own version and its image is tagged with it, so
# one IMAGE_TAG cannot name a released set.
# IMAGE_TAG stays every image's default and each override replaces one. Every
# image reference the suites compose comes from e2e_image / e2e_image_repo /
# e2e_image_tag, so an override reaches every place its image is named.
e2e_image_override_var() {
    case "$1" in
        cfgd) echo CFGD_IMAGE_TAG ;;
        cfgd-operator) echo OPERATOR_IMAGE_TAG ;;
        cfgd-csi) echo CSI_IMAGE_TAG ;;
        function-cfgd) echo FUNCTION_IMAGE_TAG ;;
        *)
            echo "e2e_image_tag: unknown image '$1'" >&2
            return 1
            ;;
    esac
}

e2e_image_tag() {
    local var
    var="$(e2e_image_override_var "$1")" || return 1
    printf '%s\n' "${!var:-$IMAGE_TAG}"
}

# An override names an image the caller wants used as it is, often a released
# one, so setup must never build over it: true when the image's override is set
# and non-empty.
e2e_image_overridden() {
    local var
    var="$(e2e_image_override_var "$1")" || return 1
    [ -n "${!var:-}" ]
}

# The warning setup prints when an override is set for a component whose spec
# another owner controls: <image> <owner> <object> <image the object runs>.
# Prints nothing when the image has no override, or when the object already
# runs the overridden reference, since the override then took effect.
e2e_override_unused_warning() {
    local image="$1" owner="$2" object="$3" running="$4"
    if e2e_image_overridden "$image" && [ "$running" != "$(e2e_image "$image")" ]; then
        echo "  WARN: $(e2e_image_override_var "$image") is set, but $owner owns $object, which runs $running; the override does not reach it"
    fi
}

e2e_image_repo() {
    printf '%s/%s\n' "$REGISTRY" "$1"
}

e2e_image() {
    local tag
    tag="$(e2e_image_tag "$1")" || return 1
    printf '%s:%s\n' "$(e2e_image_repo "$1")" "$tag"
}
E2E_NAMESPACE="${E2E_NAMESPACE:-cfgd-e2e-${GITHUB_RUN_ID:-$(date +%s)-$$}}"
# A local run id comes from the checkout, so the setup process and each suite
# process name the same PR install.
E2E_RUN_ID="${GITHUB_RUN_ID:-local-$(git -C "$REPO_ROOT" rev-parse --short HEAD 2>/dev/null || echo dev)}"
E2E_RUN_LABEL="cfgd.io/e2e-run=$E2E_RUN_ID"
# Job-specific label for cluster-scoped resources (prevents parallel job cleanup races)
E2E_JOB_LABEL="cfgd.io/e2e-job=$E2E_NAMESPACE"
# YAML-friendly forms for embedding in heredoc labels (key: "value" instead of key=value)
export E2E_RUN_LABEL_YAML="cfgd.io/e2e-run: \"$E2E_RUN_ID\""
export E2E_JOB_LABEL_YAML="cfgd.io/e2e-job: \"$E2E_NAMESPACE\""

# The PR-owned install of the operator and CSI driver, beside the live release
# in cfgd-system. The chart names each object <release>-<component>; that rule
# and the run id are spelled here only.
export E2E_INSTALL_RELEASE="cfgd-e2e-$E2E_RUN_ID"
export E2E_INSTALL_NS="$E2E_INSTALL_RELEASE-sys"
export E2E_OPERATOR_DEPLOY="$E2E_INSTALL_RELEASE-operator"
export E2E_CSI_DS="$E2E_INSTALL_RELEASE-csi"
export E2E_WEBHOOK_SVC="$E2E_INSTALL_RELEASE-webhook"
export E2E_WEBHOOK_CERT="$E2E_INSTALL_RELEASE-webhook-tls"
export E2E_VALIDATING_WEBHOOK="$E2E_INSTALL_RELEASE"
export E2E_MUTATING_WEBHOOK="$E2E_INSTALL_RELEASE-pod-injector"
export E2E_OPERATOR_PODS="app.kubernetes.io/instance=$E2E_INSTALL_RELEASE,app.kubernetes.io/component=operator"
export E2E_CSI_PODS="app.kubernetes.io/instance=$E2E_INSTALL_RELEASE,app.kubernetes.io/component=csi-driver"
# The Crossplane suite's own Function and Composition, beside ArgoCD's
# function-cfgd and teamconfig-to-machineconfigs.
export E2E_FUNCTION="function-cfgd-$E2E_RUN_ID"
export E2E_COMPOSITION="teamconfig-to-machineconfigs-$E2E_RUN_ID"
# The live release's webhook configurations, which setup scopes away from
# run-labelled objects and namespaces.
export E2E_RELEASE_VALIDATING_WEBHOOK="cfgd-validating-webhooks"
export E2E_RELEASE_MUTATING_WEBHOOK="cfgd-mutating-webhooks"

TEST_POD=""

# Heartbeat refresh interval. Must be well under the janitor's freshness window
# (HEARTBEAT_STALE_SECONDS=900s in e2e-cleanup-cronjob.yaml) so that even a
# missed beat or two never lets a live run's namespace look stale to the reaper.
HEARTBEAT_INTERVAL_SECONDS="${HEARTBEAT_INTERVAL_SECONDS:-120}"
HEARTBEAT_PID=""

# Refresh cfgd.io/heartbeat=<unix-epoch> on every namespace this run owns
# (selected by the run label, so operator-suite secondary namespaces are
# covered too, and $E2E_NAMESPACE with them). A failed annotation is retried
# on the next beat.
_heartbeat_beat() {
    kubectl annotate namespace -l "$E2E_RUN_LABEL" \
        "cfgd.io/heartbeat=$(date -u +%s)" --overwrite >/dev/null 2>&1 # rc-ok: run_every ignores a failed beat and the next one retries it
}

# Keep the run's heartbeat fresh in the background. The janitor treats a fresh
# heartbeat as proof the owning run is alive and skips the namespace whatever
# its age, which protects a run that outlives the age gate.
start_heartbeat() {
    [ -n "$HEARTBEAT_PID" ] && return 0
    run_every "$HEARTBEAT_INTERVAL_SECONDS" _heartbeat_beat
    HEARTBEAT_PID=$!
}

# Stop the heartbeat loop. Idempotent; safe to call from cleanup_e2e even if the
# loop never started.
stop_heartbeat() {
    [ -n "$HEARTBEAT_PID" ] && kill "$HEARTBEAT_PID" 2>/dev/null || true
    HEARTBEAT_PID=""
}

# Deploy the privileged test pod and wait for it to be Running.
# Exports TEST_POD with the pod name.
ensure_test_pod() {
    local pod_name="cfgd-e2e-node-${E2E_RUN_ID}" image
    image="$(e2e_image cfgd)" || return 1

    create_e2e_namespace

    kubectl apply -n "$E2E_NAMESPACE" -f - <<EOF || {
apiVersion: v1
kind: Pod
metadata:
  name: ${pod_name}
  labels:
    app: cfgd-e2e-node
    ${E2E_RUN_LABEL_YAML}
spec:
  restartPolicy: Never
  imagePullSecrets:
    - name: registry-credentials
  affinity:
    nodeAffinity:
      requiredDuringSchedulingIgnoredDuringExecution:
        nodeSelectorTerms:
          - matchExpressions:
              - key: node-role.kubernetes.io/control-plane
                operator: DoesNotExist
  containers:
    - name: cfgd
      image: ${image}
      command: ["sleep", "infinity"]
      securityContext:
        privileged: true
        runAsUser: 0
      volumeMounts:
        - name: host-proc
          mountPath: /host-proc
        - name: host-sys
          mountPath: /host-sys
        - name: host-etc
          mountPath: /host-etc
        - name: host-lib-modules
          mountPath: /lib/modules
          readOnly: true
  volumes:
    - name: host-proc
      hostPath:
        path: /proc
    - name: host-sys
      hostPath:
        path: /sys
    - name: host-etc
      hostPath:
        path: /etc
    - name: host-lib-modules
      hostPath:
        path: /lib/modules
EOF
        echo "ERROR: could not apply test pod $pod_name in $E2E_NAMESPACE. Check that the runner can create pods there and that $image is in the registry." >&2
        return 1
    }

    echo "  Waiting for test pod $pod_name..."
    kubectl wait --for=condition=Ready "pod/$pod_name" \
        -n "$E2E_NAMESPACE" --timeout=120s || {
        kubectl describe pod "$pod_name" -n "$E2E_NAMESPACE" >&2 || true
        echo "ERROR: test pod $pod_name is not Ready after 120s. Read the description above (image pull, scheduling, privileged admission) and rerun." >&2
        return 1
    }

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

# Label a resource, and fail the caller when the label does not take.
#
# A label is what a later selector matches on — an injection webhook's
# namespace label, a policy's targetSelector. `kubectl label … || true` reads
# the same whether the label landed or the API server refused, and a case
# asserting the ABSENCE of an effect (FS-CSI-05: the pod must not run;
# FS-CSI-09: no mount is left behind) then passes because nothing was ever
# injected. The rc is read here so a caller can put it in its verdict.
ensure_label() {
  local rc=0
  kubectl label "$@" >/dev/null 2>&1 || rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "FAIL: could not label: kubectl label $* (rc=$rc). Check that the runner can label $1 objects." >&2
    return 1
  fi
}

# Ensure a namespace exists, and fail the case when it genuinely cannot.
#
# `kubectl create namespace X || true` reads the same either way: the namespace
# was already there from an earlier run, or the API server refused and every
# resource the case creates into it is about to fail with nothing saying why.
# The rc is captured and re-checked with a `get`, so only the second one stops
# the case.
#
# A namespace it creates carries the run label: the PR install's mutating
# webhook selects on it, the heartbeat refreshes by it and the janitor reaps by
# it. cfgd-system (or $CFGD_NAMESPACE) belongs to the live release and outlives
# every run, so it never gets the label even on a cluster where setup creates it.
ensure_namespace() {
  local ns="$1" rc=0
  kubectl create namespace "$ns" >/dev/null 2>&1 || rc=$?
  if [ "$rc" -ne 0 ]; then
    if ! kubectl get namespace "$ns" >/dev/null 2>&1; then
      echo "FAIL: namespace $ns could not be created (rc=$rc). Check that the runner can create and label namespaces." >&2
      return 1
    fi
  else
    case "$ns" in
      cfgd-system | "$CFGD_NAMESPACE") ;;
      *)
        ensure_label namespace "$ns" "$E2E_RUN_LABEL" --overwrite || {
          echo "FAIL: could not label namespace $ns. Check that the runner can create and label namespaces." >&2
          return 1
        }
        ;;
    esac
  fi
}

# Each write returns on its own failure, so the message names the step and no
# heartbeat loop starts for a namespace that is not there. A plain return also
# works where the caller runs it inside a condition, where set -e is off.
create_e2e_namespace() {
    local hint="Check that the runner can create, label and annotate namespaces." phase
    if ! phase=$(kubectl get namespace "$E2E_NAMESPACE" --ignore-not-found -o jsonpath='{.status.phase}'); then
        echo "ERROR: could not read namespace $E2E_NAMESPACE. Check that the runner can get namespaces." >&2
        return 1
    fi
    # A re-run with the same run id names the namespace an earlier teardown is
    # still deleting. It keeps its labels and annotations but refuses new
    # objects, so taking it as created fails later on a secret or install that
    # cannot land in it.
    if [ "$phase" = Terminating ]; then
        echo "  namespace $E2E_NAMESPACE is being deleted; waiting up to 120s for it to go"
        kubectl wait --for=delete "namespace/$E2E_NAMESPACE" --timeout=120s || {
            echo "ERROR: namespace $E2E_NAMESPACE is still being deleted after 120s; rerun once it is gone" >&2; return 1; }
        phase=""
    fi
    if [ -z "$phase" ]; then
        kubectl create namespace "$E2E_NAMESPACE" || {
            echo "ERROR: could not create namespace $E2E_NAMESPACE. $hint" >&2; return 1; }
        kubectl label namespace "$E2E_NAMESPACE" "$E2E_RUN_LABEL" --overwrite || {
            echo "ERROR: could not label namespace $E2E_NAMESPACE. $hint" >&2; return 1; }
        # Stamp creation time so the cfgd-e2e-janitor CronJob can age out
        # leaked namespaces from crashed runs (RFC3339 UTC).
        kubectl annotate namespace "$E2E_NAMESPACE" \
            "cfgd.io/created-at=$(date -u +%Y-%m-%dT%H:%M:%SZ)" --overwrite || {
            echo "ERROR: could not annotate namespace $E2E_NAMESPACE. $hint" >&2; return 1; }
        # Seed a heartbeat immediately so the namespace is protected before the
        # background loop's first tick, then keep it refreshed for the run.
        kubectl annotate namespace "$E2E_NAMESPACE" \
            "cfgd.io/heartbeat=$(date -u +%s)" --overwrite || {
            echo "ERROR: could not annotate namespace $E2E_NAMESPACE. $hint" >&2; return 1; }
    fi
    start_heartbeat
    wait_for_registry_credentials "$E2E_NAMESPACE" ||
        echo "  WARN: registry-credentials not replicated to $E2E_NAMESPACE (Reflector may not be running)"
}

cleanup_e2e() {
    echo "Cleaning up E2E resources for run $E2E_RUN_ID..."

    # Stop refreshing the heartbeat first: once the run is tearing down, the
    # namespace SHOULD become reapable if cascade deletion is interrupted.
    stop_heartbeat

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

    # Last: the scratch root holds this run's $HOME, and every kubectl above
    # resolves its discovery cache under it. Removed here when scratch-home.sh
    # made the root, rather than by a suite that owns its own removal.
    if [ -n "${E2E_SCRATCH_OWNED:-}" ]; then
        rm -rf "$E2E_SCRATCH_OWNED"
    fi
}

# --- Waiting ---
#
# A wait polls the state the next step reads, up to a deadline, and says on
# timeout what it waited for. common/test-waits.sh fails on a `sleep` outside a
# function of this file whose body tests a deadline, apart from run_every.

# run_every <interval_s> <command...>: run the command in a background subshell
# now and then every interval_s seconds until the subshell is killed; the
# caller reads its pid from $!. A failed run is ignored and the next one runs
# on time. This is the one background cadence the e2e scripts keep (a refresh
# that has to outlive the step that starts it), so it is the one sleep
# test-waits.sh lets run without a deadline, listed under Cadences. The
# subshell clears the inherited EXIT trap, so killing it never re-enters the
# caller's cleanup.
run_every() {
    local interval="$1"
    shift
    (
        trap - EXIT
        while true; do
            "$@" || true
            sleep "$interval"
        done
    ) &
}

# wait_until <timeout_s> <interval_s> <what> <command...>: run the command every
# interval until it exits 0. When the timeout passes first, prints "Timed out
# after <timeout_s>s waiting for <what>" to stderr and returns 1. The command
# runs in this shell, so a predicate function can leave what it last read in a
# variable for the caller's verdict.
wait_until() {
    local timeout="$1" interval="$2" what="$3"
    shift 3
    local deadline=$((SECONDS + timeout))
    until "$@"; do
        if [ "$SECONDS" -ge "$deadline" ]; then
            echo "  Timed out after ${timeout}s waiting for $what" >&2
            return 1
        fi
        sleep "$interval"
    done
}

# retry_tries <tries> <interval_s> <command...>: run the command up to <tries>
# times, <interval_s> apart, and return 0 on its first success. RETRY_ATTEMPT
# holds the number of attempts made. Returns 1 without a message when every
# attempt failed, since the caller's verdict names what the last one saw.
retry_tries() {
    local tries="$1" interval="$2"
    shift 2
    RETRY_ATTEMPT=0
    while [ "$RETRY_ATTEMPT" -lt "$tries" ]; do
        [ "$RETRY_ATTEMPT" -eq 0 ] || sleep "$interval"
        RETRY_ATTEMPT=$((RETRY_ATTEMPT + 1))
        "$@" && return 0
    done
    return 1
}

# k8s_exists <kubectl get args...>: 0 when kubectl reads the object.
k8s_exists() {
    kubectl get "$@" > /dev/null 2>&1 # rc-ok: its status is the function's answer
}

# wait_for_registry_credentials <namespace> [timeout_s, default 30]: wait for
# Reflector to copy registry-credentials, which every pod pulling a first-party
# image names in imagePullSecrets, into the namespace.
wait_for_registry_credentials() {
    wait_until "${2:-30}" 1 "Reflector to copy registry-credentials into namespace $1" \
        k8s_exists secret registry-credentials -n "$1"
}

# wait_for_deleted <timeout_s> <kubectl get args...>: wait until the objects
# named are gone. Some kubectl versions report an object that went before the
# wait looked as a NotFound error, so a failed wait is read back before it
# counts. Prints what is left and returns 1 on timeout.
wait_for_deleted() {
    local timeout="$1" left
    shift
    kubectl wait --for=delete "$@" --timeout="${timeout}s" > /dev/null 2>&1 && return 0
    left="$(kubectl get "$@" --ignore-not-found -o name 2>&1)" || true
    [ -n "$left" ] || return 0
    echo "  Timed out after ${timeout}s waiting for deletion; still there: $(paste -sd ' ' <<<"$left")" >&2
    return 1
}

# wait_for_pod_log <file> <ERE> [timeout_s, default 30]: wait for a line
# matching ERE in a file inside the test pod, such as a daemon's log. On
# timeout prints the file's last 15 lines to stderr and returns 1.
wait_for_pod_log() {
    local file="$1" pattern="$2" timeout="${3:-30}"
    if wait_until "$timeout" 1 "/$pattern/ in $file on pod/$TEST_POD" \
        exec_in_pod grep -qE -- "$pattern" "$file"; then
        return 0
    fi
    {
        echo "  Last 15 lines of $file:"
        exec_in_pod tail -n 15 "$file" 2>&1 | sed 's/^/    /'
    } >&2
    return 1
}

# pod_pid_gone <pid>: 0 when the process is gone from the test pod or is a
# zombie. The pod's PID 1 reaps nothing, so a daemon started under nohup stays
# a zombie after it exits and kill -0 still finds it. A kubectl failure reads
# as not gone.
pod_pid_gone() {
    # shellcheck disable=SC2016 # the script runs in the pod's sh, which expands $1 and $state
    exec_in_pod sh -c 'state=$(sed -n "s/^State:[[:space:]]*//p" "/proc/$1/status" 2>/dev/null); [ -z "$state" ] || [ "${state#Z}" != "$state" ]' _ "$1"
}

# stop_pod_process <pid> [timeout_s, default 15]: SIGTERM a process in the test
# pod and wait for it to go. One that outlasts the timeout is sent SIGKILL and
# given 5s more, and the call returns 1; the timeout message is already on
# stderr, so a caller that only needs the process gone before its next case
# may ignore the status.
stop_pod_process() {
    local pid="$1" timeout="${2:-15}"
    exec_in_pod kill "$pid" > /dev/null 2>&1 || true
    wait_until "$timeout" 1 "pid $pid to exit after SIGTERM in pod/$TEST_POD" pod_pid_gone "$pid" && return 0
    exec_in_pod kill -KILL "$pid" > /dev/null 2>&1 || true
    wait_until 5 1 "pid $pid to exit after SIGKILL in pod/$TEST_POD" pod_pid_gone "$pid" || true
    return 1
}

_socket_or_exited() {
    [ -S "$1" ] || ! kill -0 "$2" 2>/dev/null
}

# wait_for_daemon_socket <socket> <pid> <timeout_s>: wait for a daemon this
# shell started as <pid> to create its IPC socket. Returns 1 as soon as the
# daemon exits without one, and on timeout.
wait_for_daemon_socket() {
    wait_until "$3" 0.2 "pid $2 to create $1" _socket_or_exited "$1" "$2" || return 1
    [ -S "$1" ] || {
        echo "  pid $2 exited before creating $1" >&2
        return 1
    }
}

# stop_background_job <pid> <grace_s>: SIGTERM a job this shell started and
# reap it, leaving its exit status in REAPED_RC. `wait` has to be what reaps
# it, as a reaped job's status is gone, so the SIGKILL for a job that outlives
# the grace comes from a watchdog subshell, whose kill -0 polls reap nothing.
# Returns 1 when the process is still there afterwards.
stop_background_job() {
    local pid="$1" grace="$2" watchdog
    kill -TERM "$pid" 2>/dev/null || true
    (
        trap - EXIT
        _wait_gone "$pid" $((grace * 10)) || kill -KILL "$pid" 2>/dev/null
    ) &
    watchdog=$!
    # shellcheck disable=SC2034  # read by the caller after the reap
    if wait "$pid" 2>/dev/null; then REAPED_RC=0; else REAPED_RC=$?; fi
    kill -KILL "$watchdog" 2>/dev/null || true
    wait "$watchdog" 2>/dev/null || true
    ! kill -0 "$pid" 2>/dev/null
}

# _injects_csi <namespace> <cfgd.io/modules value>: 0 when a server-side dry
# run of a pod carrying the annotation in the namespace comes back with a
# $CSI_DRIVER_NAME volume. INJECTION_PROBE keeps what the dry run returned.
_injects_csi() {
    INJECTION_PROBE=$(kubectl create --dry-run=server -n "$1" \
        -o jsonpath='{.spec.volumes[*].csi.driver}' -f - 2>&1 <<EOF
apiVersion: v1
kind: Pod
metadata:
  generateName: cfgd-e2e-injection-probe-
  annotations:
    cfgd.io/modules: "$2"
spec:
  restartPolicy: Never
  containers:
    - name: probe
      image: busybox:1.36
EOF
) || return 1
    [[ " $INJECTION_PROBE " == *" $CSI_DRIVER_NAME "* ]]
}

# wait_for_injection <namespace> <cfgd.io/modules value> [timeout_s, default 60]:
# wait until the pod injector adds a $CSI_DRIVER_NAME volume to a pod created
# in the namespace with that annotation, read from a server-side dry run (the
# injector declares sideEffects: None). The API server matches a webhook's
# namespaceSelector against its own cache of namespaces, which trails a label
# write, and the namespace's default ServiceAccount, which a pod needs, appears
# after the namespace; the dry run passes once both have. On timeout prints
# what the last dry run returned.
wait_for_injection() {
    INJECTION_PROBE=""
    wait_until "${3:-60}" 1 "the pod injector to add a $CSI_DRIVER_NAME volume for '$2' in namespace $1" \
        _injects_csi "$1" "$2" && return 0
    echo "  The last dry run returned: ${INJECTION_PROBE:-no volumes}" >&2
    return 1
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

# The image a live workload runs, read off the object itself: <kind> <name>
# <container> [namespace, default cfgd-system]. The container is chosen by name
# so a sidecar listed first is never reported as the component.
running_image() {
    local kind="$1" name="$2" container="$3" namespace="${4:-cfgd-system}" image
    image="$(kubectl get "$kind" "$name" -n "$namespace" \
        -o jsonpath="{.spec.template.spec.containers[?(@.name==\"$container\")].image}" 2>/dev/null || true)"
    printf '%s\n' "${image:-not deployed}"
}

# argocd_managed <kind> <name> [namespace]: 0 when ArgoCD tracks the object
# (its tracking-id annotation is set), so it runs what /db/manifests pins and
# reverts changes; 1 when it does not, or the object is absent; 2 when kubectl
# cannot read it, whose error is left on stderr. The namespace defaults to
# cfgd-system when omitted; `-` names a cluster-scoped kind. An empty namespace
# is refused with 2: read in the kubeconfig's own namespace, a namespaced
# object would look absent, and absent is the answer a caller writes on.
argocd_managed() {
    local id ns="${3-cfgd-system}"
    if [ -z "$ns" ]; then
        echo "ERROR: argocd_managed $1 $2 was given an empty namespace. Pass the namespace, or - for a cluster-scoped kind." >&2
        return 2
    fi
    [ "$ns" != - ] || ns=""
    id="$(kubectl get "$1" "$2" ${ns:+-n "$ns"} --ignore-not-found \
        -o jsonpath='{.metadata.annotations.argocd\.argoproj\.io/tracking-id}')" || return 2
    [ -n "$id" ]
}

# argocd_owner <kind> <name> <namespace, - when cluster-scoped> <rerun>:
# argocd_managed's status, with the ERROR every caller stops or fails on when
# the object cannot be read printed to stderr.
argocd_owner() {
    local rc=0 where=""
    if [ "$#" -ne 4 ] || [ -z "$3" ]; then
        echo "ERROR: argocd_owner takes a kind, a name, a namespace (- for a cluster-scoped kind) and the rerun advice; got [$*]." >&2
        return 2
    fi
    argocd_managed "$1" "$2" "$3" || rc=$?
    if [ "$rc" -eq 2 ]; then
        [ "$3" = - ] || where=" in $3"
        echo "ERROR: could not read $1/$2$where. Check that the runner can get $1 objects${where:+ there}, then $4." >&2
    fi
    return "$rc"
}

# require_release_webhooks_scoped: 0 when the release's webhook configurations
# leave every run-labelled object and namespace to the PR install: each
# $E2E_RELEASE_VALIDATING_WEBHOOK entry's objectSelector and each
# $E2E_RELEASE_MUTATING_WEBHOOK entry's namespaceSelector holds the
# cfgd.io/e2e-run DoesNotExist expression. Otherwise prints an ERROR and
# returns 1. A setup run from a branch without that scoping re-applies both
# configurations without it, and the release operator then admits and mutates
# what this run creates. A configuration ArgoCD tracks is refused as setup
# refuses it, since setup cannot scope it.
require_release_webhooks_scoped() {
    local rerun="rerun setup from this branch" entry kind name selector rc doc unscoped
    for entry in "validatingwebhookconfiguration $E2E_RELEASE_VALIDATING_WEBHOOK objectSelector" \
        "mutatingwebhookconfiguration $E2E_RELEASE_MUTATING_WEBHOOK namespaceSelector"; do
        read -r kind name selector <<<"$entry"
        rc=0
        argocd_owner "$kind" "$name" - "$rerun" || rc=$?
        case "$rc" in
            0)
                echo "ERROR: $kind/$name carries an argocd.argoproj.io/tracking-id annotation, so ArgoCD owns it and setup does not scope it. Add the cfgd.io/e2e-run DoesNotExist selectors from setup-cluster.sh's webhook step to its manifest in the GitOps repo, drop it from that step's heredoc, then $rerun." >&2
                return 1
                ;;
            1) ;;
            *) return 1 ;;
        esac
        doc="$(kubectl get "$kind" "$name" --ignore-not-found -o json)" || {
            echo "ERROR: could not read $kind/$name. Check that the runner can get $kind objects, then $rerun." >&2
            return 1
        }
        if [ -z "$doc" ]; then
            echo "ERROR: $kind/$name is missing; setup applies it, so $rerun." >&2
            return 1
        fi
        unscoped="$(jq -r --arg sel "$selector" '.webhooks[]?
            | select([.[$sel].matchExpressions[]? | select(.key == "cfgd.io/e2e-run" and .operator == "DoesNotExist")] | length == 0)
            | .name' <<<"$doc")" || {
            echo "ERROR: could not read the webhooks of $kind/$name as JSON: kubectl returned something other than JSON, or jq is not on PATH. Check both, then $rerun." >&2
            return 1
        }
        if [ -n "$unscoped" ]; then
            echo "ERROR: a setup from a branch without the PR-install scoping re-applied the release webhooks; $rerun. $kind/$name entries whose $selector lacks cfgd.io/e2e-run DoesNotExist: $(paste -sd ' ' <<<"$unscoped")" >&2
            return 1
        fi
    done
}

# Installs Crossplane into crossplane-system with Helm unless ArgoCD runs it
# there. Returns 1 with an ERROR when the crossplane Deployment cannot be read
# or the install fails, before or after any Helm call.
crossplane_install() {
    local rc=0
    argocd_owner deployment crossplane crossplane-system "rerun the Crossplane suite" || rc=$?
    case "$rc" in
        0)
            echo "  deployment/crossplane in crossplane-system is ArgoCD's; installing nothing"
            ;;
        1)
            helm repo add crossplane-stable https://charts.crossplane.io/stable || {
                echo "ERROR: helm could not add the crossplane-stable repository. Read the Helm error above, then rerun the Crossplane suite." >&2
                return 1
            }
            helm upgrade --install crossplane crossplane-stable/crossplane \
                --namespace crossplane-system --create-namespace --wait --timeout 120s || {
                echo "ERROR: helm could not install Crossplane into crossplane-system. Read the Helm error above, then rerun the Crossplane suite." >&2
                return 1
            }
            ;;
        *)
            return 1
            ;;
    esac
}

# --- CRD check ---

# ArgoCD applies the cluster's CRDs from this file, so no e2e script writes
# them; setup and the suites that need them compare the PR's CRDs with them.
export E2E_CRD_MANIFEST="/db/manifests/k3s/namespaces/crossplane-system/cfgd-crds.yaml"

# CRD YAML on stdin; one compact JSON object per CustomResourceDefinition on
# stdout. `kubectl create --dry-run=client` asks the API server to map each
# kind, while `annotate --local` reads the YAML offline; removing an annotation
# the documents do not carry leaves them as written.
crd_docs_json() {
    local json
    json="$(kubectl annotate --local -o json -f - cfgd.io/e2e-unset-)" || return 1
    jq -c '.items[]? // . | select(.kind == "CustomResourceDefinition")' <<<"$json"
}

# One CRD as JSON on stdin; its spec with every description string removed and
# keys sorted, so a CRD whose only change is documentation compares equal. The
# API server fills names.listKind, names.singular, conversion and
# preserveUnknownFields when a CRD omits them, and the generated file omits
# some of them, so both sides get those defaults before they are compared. A
# key named `description` inside `properties` holds an object, so it stays.
crd_shape() {
    jq -S 'walk(if type == "object" and (.description | type) == "string" then del(.description) else . end)
           | .spec
           | .names.listKind //= (.names.kind + "List")
           | .names.singular //= (.names.kind | ascii_downcase)
           | .conversion //= {strategy: "None"}
           | .preserveUnknownFields //= false'
}

# Why a live CRD (JSON on stdin) is not Established, or nothing when it is.
crd_not_established() {
    jq -r 'first(.status.conditions[]? | select(.type == "Established")) // {}
           | if .status == "True" then empty
             elif .status == null then "no Established condition"
             else .reason // "Established is \(.status)" end'
}

# check_pr_crds [source] [rerun] < <CRD YAML>: compares the spec of each CRD in
# the YAML, descriptions aside, with the cluster's copy, and checks that copy is
# Established. source names the YAML in messages (default: the cfgd-gen-crds
# output) and rerun is the advice that ends them (default: rerun setup).
# Prints an ERROR to stderr for each CRD that is missing, differs (followed by
# up to 40 lines of diff, cluster first), is not Established or cannot be read.
# Returns 1 when any did, or when the YAML holds no CRD to compare.
check_pr_crds() {
    local source="${1:-the cfgd-gen-crds output}" rerun="${2:-rerun setup}"
    local docs doc name want live_doc live why status=0 checked=0
    local fix="ArgoCD owns the cluster's CRDs; copy schemas/crds.yaml over $E2E_CRD_MANIFEST, push it and let ArgoCD sync, then $rerun."
    if ! docs="$(crd_docs_json)"; then
        echo "ERROR: could not read $source as CRDs. Check that it is valid YAML and that kubectl and jq are on PATH, then $rerun." >&2
        return 1
    fi
    while IFS= read -r doc; do
        [ -n "$doc" ] || continue
        name="$(jq -r '.metadata.name // empty' <<<"$doc")"
        if [ -z "$name" ]; then
            echo "ERROR: a CRD in $source has no metadata.name. Check $source, then $rerun." >&2
            status=1
            continue
        fi
        checked=$((checked + 1))
        if ! want="$(crd_shape <<<"$doc")" || ! jq -e '(.versions | length) > 0' <<<"$want" >/dev/null; then
            echo "ERROR: $name in $source has no versions to compare. Check $source, then $rerun." >&2
            status=1
            continue
        fi
        if ! live_doc="$(kubectl get crd "$name" --ignore-not-found -o json)"; then
            echo "ERROR: could not read crd/$name. Check that the runner can get customresourcedefinitions, then $rerun." >&2
            status=1
            continue
        fi
        if [ -z "$live_doc" ]; then
            echo "ERROR: this PR adds $name, which the cluster does not have. $fix" >&2
            status=1
            continue
        fi
        if ! live="$(crd_shape <<<"$live_doc")" || ! why="$(crd_not_established <<<"$live_doc")"; then
            echo "ERROR: could not read the cluster's crd/$name as JSON. Check that kubectl get crd $name -o json prints a CustomResourceDefinition, then $rerun." >&2
            status=1
            continue
        fi
        if [ "$want" != "$live" ]; then
            echo "ERROR: this PR changes the spec of $name (descriptions aside). $fix" >&2
            diff <(printf '%s\n' "$live") <(printf '%s\n' "$want") | head -40 >&2 || true # rc-ok: diff exits 1 on the difference being shown
            status=1
        fi
        if [ -n "$why" ]; then
            echo "ERROR: crd/$name is not Established on the cluster ($why). Check ArgoCD's cfgd-crds sync and the CRD's status.conditions, then $rerun." >&2
            status=1
        fi
    done <<<"$docs"
    if [ "$checked" -eq 0 ]; then
        echo "ERROR: $source holds no CustomResourceDefinition to compare with the cluster. Check $source, then $rerun." >&2
        return 1
    fi
    return "$status"
}

export E2E_XRD_MANIFEST="/db/manifests/k3s/namespaces/crossplane-system/xrd-teamconfig.yaml"

# check_pr_xrd [xrd file]: compares the spec of the TeamConfig XRD in the file
# (default: manifests/crossplane/xrd-teamconfig.yaml) with the cluster's copy
# and checks that copy is Established. Crossplane fills
# defaultCompositeDeletePolicy and defaultCompositionUpdatePolicy when an XRD
# omits them, so both sides get those defaults before they are compared.
# Prints an ERROR to stderr when the XRD cannot be read, is missing, differs
# (followed by up to 40 lines of diff, cluster first) or is not Established,
# and returns 1.
check_pr_xrd() {
    local file="${1:-$REPO_ROOT/manifests/crossplane/xrd-teamconfig.yaml}"
    local doc name want live_doc live why status=0
    local shape='.spec | .defaultCompositeDeletePolicy //= "Background" | .defaultCompositionUpdatePolicy //= "Automatic"'
    local fix="ArgoCD owns the cluster's XRD; copy manifests/crossplane/xrd-teamconfig.yaml over $E2E_XRD_MANIFEST (keep its ArgoCD annotations) and push, then rerun the Crossplane suite."
    if ! doc="$(kubectl annotate --local -o json -f "$file" cfgd.io/e2e-unset-)" ||
        ! name="$(jq -er 'select(.kind == "CompositeResourceDefinition") | .metadata.name' <<<"$doc")" ||
        ! want="$(jq -S "$shape" <<<"$doc")"; then
        echo "ERROR: could not read $file as one CompositeResourceDefinition. Check that it is valid YAML and that kubectl and jq are on PATH, then rerun the Crossplane suite." >&2
        return 1
    fi
    if ! live_doc="$(kubectl get xrd "$name" --ignore-not-found -o json)"; then
        echo "ERROR: could not read xrd/$name. Check that the runner can get compositeresourcedefinitions, then rerun the Crossplane suite." >&2
        return 1
    fi
    if [ -z "$live_doc" ]; then
        echo "ERROR: the cluster has no xrd/$name. $fix" >&2
        return 1
    fi
    if ! live="$(jq -S "$shape" <<<"$live_doc")" || ! why="$(crd_not_established <<<"$live_doc")"; then
        echo "ERROR: could not read the cluster's xrd/$name as JSON. Check that kubectl get xrd $name -o json prints a CompositeResourceDefinition, then rerun the Crossplane suite." >&2
        return 1
    fi
    if [ "$want" != "$live" ]; then
        echo "ERROR: this PR changes the TeamConfig XRD. $fix" >&2
        diff <(printf '%s\n' "$live") <(printf '%s\n' "$want") | head -40 >&2 || true # rc-ok: diff exits 1 on the difference being shown
        status=1
    fi
    if [ -n "$why" ]; then
        echo "ERROR: xrd/$name is not Established on the cluster ($why). Check ArgoCD's crossplane-system sync and the XRD's status.conditions, then rerun the Crossplane suite." >&2
        status=1
    fi
    return "$status"
}

# render_run_composition [composition file]: prints the Composition (default:
# manifests/crossplane/composition.yaml) as this run's copy: named
# $E2E_COMPOSITION, calling $E2E_FUNCTION and carrying the run label. The
# runner has no yq, so sed rewrites the exact lines; when a line it rewrites
# is not in the file exactly once, it prints an ERROR naming that line and
# returns 1.
render_run_composition() {
    local file="${1:-$REPO_ROOT/manifests/crossplane/composition.yaml}" anchor count
    for anchor in '  name: teamconfig-to-machineconfigs' '  labels:' '      name: function-cfgd'; do
        if ! count="$(grep -cxF -- "$anchor" "$file")" || [ "$count" -ne 1 ]; then
            echo "ERROR: $file holds the line '$anchor' ${count:-0} times (want once), so the run's Composition cannot be rendered from it. Update render_run_composition in tests/e2e/common/helpers.sh to the file's layout." >&2
            return 1
        fi
    done
    sed -e "s/^  name: teamconfig-to-machineconfigs\$/  name: $E2E_COMPOSITION/" \
        -e "/^  labels:\$/a\\    $E2E_RUN_LABEL_YAML" \
        -e "s/^      name: function-cfgd\$/      name: $E2E_FUNCTION/" "$file"
}

# Port-forward to `svc/<name>` or `pod/<name>` in the background and echo the
# kubectl PID once the local port accepts a connection; stop it with
# stop_port_forward. A fixed sleep races a slow kubectl start. On timeout, or
# when kubectl exits first, prints kubectl's own output to stderr and returns 1.
# E2E_PORT_FORWARD_TRIES sets how many half-second probes are made (default 30).
port_forward() {
    local namespace="$1"
    local target="$2"
    local local_port="$3"
    local remote_port="${4:-$local_port}"

    case "$target" in
        svc/?* | pod/?*) ;;
        *)
            echo "port_forward: target must be svc/<name> or pod/<name>, got '$target'" >&2
            return 1
            ;;
    esac
    local max_tries="${E2E_PORT_FORWARD_TRIES:-30}"
    if ! [[ $max_tries =~ ^[1-9][0-9]*$ ]]; then
        echo "port_forward: E2E_PORT_FORWARD_TRIES must be a positive integer, got '$max_tries'" >&2
        return 1
    fi

    local log
    log="$(mktemp "$CLI_SCRATCH/port-forward.XXXXXX")"
    kubectl port-forward -n "$namespace" "$target" \
        "$local_port:$remote_port" > "$log" 2>&1 &
    local pid=$!
    local tries=0
    while [ "$tries" -lt "$max_tries" ]; do
        if ! kill -0 "$pid" 2>/dev/null; then
            echo "port_forward: kubectl port-forward $target exited before localhost:$local_port opened:" >&2
            sed 's/^/    /' "$log" >&2
            return 1
        fi
        if (: < "/dev/tcp/127.0.0.1/$local_port") 2>/dev/null; then
            echo "$pid"
            return 0
        fi
        sleep 0.5
        tries=$((tries + 1))
    done
    echo "port_forward: localhost:$local_port for $target did not accept a connection within $((max_tries / 2)).$((max_tries % 2 * 5))s; kubectl said:" >&2
    sed 's/^/    /' "$log" >&2
    stop_port_forward "$pid"
    return 1
}

# Stop a port_forward and return once kubectl has exited. A PID captured
# through a command substitution is not this shell's child, so `wait` cannot
# reap it and its exit is watched for instead: up to 5s after SIGTERM, then
# SIGKILL and up to 1s more, so no tunnel outlives the suite.
stop_port_forward() {
    local pid="$1"
    kill "$pid" 2>/dev/null || return 0
    wait "$pid" 2>/dev/null || true
    if ! _wait_gone "$pid" 50; then
        echo "stop_port_forward: $pid ignored SIGTERM, sending SIGKILL" >&2
        kill -9 "$pid" 2>/dev/null || true
        _wait_gone "$pid" 10 || echo "stop_port_forward: $pid still running after SIGKILL" >&2
    fi
}

# Return 0 once PID $1 no longer exists, checking every 0.1s up to $2 times.
_wait_gone() {
    local tries=0
    while kill -0 "$1" 2>/dev/null; do
        [ "$tries" -lt "$2" ] || return 1
        sleep 0.1
        tries=$((tries + 1))
    done
}

# GET $1 into file $2 and print "<http_code> <content_type>"; the code is 000
# when no response arrived. The body is kept whatever the status, so a failed
# check can show what the endpoint actually served; curl's exit code and
# stderr go to "$2.err" for the case where nothing did.
http_get_to_file() {
    local meta rc=0
    meta="$(curl -sS --max-time 10 -o "$2" -w '%{http_code} %{content_type}' "$1" 2> "$2.err.tmp")" || rc=$?
    { echo "$rc"; cat "$2.err.tmp"; } > "$2.err"
    rm -f "$2.err.tmp"
    [ -f "$2" ] || : > "$2"
    meta="${meta% }"
    printf '%s\n' "${meta:-000}"
}

# Describe a response kept by http_get_to_file for a fail reason: status,
# content type, line count and the first 15 lines of the body, or curl's exit
# code and error when no response arrived.
http_evidence() {
    local code="$1" content_type="$2" body="$3"
    if [ "$code" = "000" ] && [ -f "$body.err" ]; then
        printf 'curl exit %s: %s\n' "$(head -n 1 "$body.err")" "$(tail -n +2 "$body.err" | tr '\n' ' ' | sed 's/ *$//')"
    fi
    printf 'HTTP %s, content-type %s, %s body lines; first 15:\n' \
        "$code" "${content_type:-none}" "$(wc -l < "$body" | tr -d ' ')"
    head -n 15 "$body" | sed 's/^/    | /'
}

# Print every sample line of counter family $1 in the exposition body in file
# $2; returns 1 when there is none. Every caller scrapes the PR install's
# operator or CSI driver, built from this checkout, which render a counter's
# samples as `<family>_total`, so that is the only sample name read; a sample
# with a doubled suffix is a registration bug for the check to fail on.
metric_sample_lines() {
    grep -E "^$1_total(\{|[[:blank:]])" "$2"
}

# Print the value of counter family $1's sample with label set $2 (the text
# between the braces, e.g. `module="m",result="success"`; empty for a sample
# without labels) in the body in file $3, or 0 when that sample is absent.
# The lines come from metric_sample_lines so the two readers cannot disagree on
# which lines are samples; `|| true` keeps an absent family a 0 under pipefail.
metric_sample_value() {
    local labels=""
    [ -z "$2" ] || labels="{$2}"
    { metric_sample_lines "$1" "$3" || true; } |
        awk -v a="$1_total$labels" '$1 == a { v = $2 } END { print v + 0 }'
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

# The CSIDriver the PR install registers, passed to helm as
# --set-string csiDriver.name=$CSI_DRIVER_NAME, so its pods never resolve to the
# live release's csi.cfgd.io.
export CSI_DRIVER_NAME="e2e.csi.cfgd.io"
export MODULES_ANNOTATION="cfgd.io/modules"

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

    local ns_args=()
    [ -z "$namespace" ] || ns_args=(-n "$namespace")

    local deadline=$((SECONDS + timeout))
    local value=""

    while [ $SECONDS -lt $deadline ]; do
        value=$(kubectl get "$kind" "$name" "${ns_args[@]}" \
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

# Wait until a Service has ready Endpoints (webhook-backed services need this:
# Deployment-level Available=True can return before Endpoints repopulate during
# a rolling update, causing admission webhook calls to fail with
# "no endpoints available for service"). Poll the Endpoints object for any
# address in the subsets.addresses list.
# Usage: wait_for_service_endpoints <namespace> <service> [timeout_seconds]
wait_for_service_endpoints() {
    local namespace="$1"
    local service="$2"
    local timeout="${3:-120}"
    local deadline=$((SECONDS + timeout))
    while [ $SECONDS -lt $deadline ]; do
        local addrs
        addrs=$(kubectl get endpoints "$service" -n "$namespace" \
            -o jsonpath='{.subsets[*].addresses[*].ip}' 2>/dev/null || echo "")
        if [ -n "$addrs" ]; then
            return 0
        fi
        sleep 2
    done
    {
        echo "  ERROR: Service $namespace/$service has no ready endpoints after ${timeout}s"
        kubectl get endpoints "$service" -n "$namespace" -o yaml 2>&1 | head -20 || true
    } >&2
    return 1
}

# --- Build helpers ---

# Ensure the cfgd binary is built (idempotent). Sets CFGD_BIN, and returns
# non-zero when there is no binary to set it to.
#
# The build's stderr is kept and its status is read: a CI job that compiles cfgd
# here has nothing else to report a cargo failure, and a discarded one surfaces
# cases later as an opaque `rc=127` from whichever case runs the binary first.
ensure_cfgd_binary() {
    CFGD_BIN="$REPO_ROOT/target/release/cfgd"
    export CFGD_BIN

    if [ -x "$CFGD_BIN" ]; then
        return 0
    fi

    echo "  Building cfgd..."
    if ! cargo build --release --manifest-path "$REPO_ROOT/Cargo.toml" --bin cfgd; then
        echo "  ERROR: cargo build --release --bin cfgd failed" >&2
        return 1
    fi
    if [ ! -x "$CFGD_BIN" ]; then
        echo "  ERROR: no executable at $CFGD_BIN after a successful build" >&2
        return 1
    fi
}

# --- Assertion helpers ---

# The shell mirror of the Rust suite's `captured_text`: these assertions are
# about TEXT, and cfgd emits attribute SGR (bold/italic) even under
# --no-color — colour off is not attrs off — so a style boundary inside a
# token (the phase heading's "Phase" + ":") breaks a plain grep over the raw
# bytes, and a negative grep over them passes vacuously.
strip_sgr() {
    sed -e $'s/\x1b\\[[0-9;]*m//g'
}

assert_contains() {
    local output
    output=$(printf '%s' "$1" | strip_sgr)
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
    local output
    output=$(printf '%s' "$1" | strip_sgr)
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

# Print the head of a command's captured stderr ($1), indented, so a case that
# kept stderr out of the document it parses still shows it for diagnosis.
print_stderr_head() {
    head -c 400 "$1" | sed 's/^/    stderr: /'
}

# Run `cfgd compliance -o json` in the test pod against config $1 and print
# its stdout alone, the document jq reads. stderr carries advisories that
# would corrupt the JSON if merged into it, so it goes to a scratch file and is
# echoed to this shell's stderr: this function's stdout is what callers capture.
pod_compliance_json() {
    local err="$CLI_SCRATCH/pod-compliance.stderr"
    exec_in_pod cfgd --config "$1" compliance -o json --no-color 2> "$err" || true
    print_stderr_head "$err" >&2
}

# The compliance status of one sysctl key ($2) in a `compliance -o json`
# document ($1). A drifted key is a `system` row of its own keyed
# `sysctl.<key>`. A key that is not drifted has no row of its own, so it takes
# the sysctl configurator's answer: its `sysctl` row when nothing drifted, or
# Compliant when only other sysctl keys did. Prints `absent` when the document
# holds no sysctl answer and `unparsable` when it is not JSON.
sysctl_compliance_status() {
    # jq reads an empty input as no document at all and exits 0 silently.
    [ -n "$1" ] || { echo unparsable; return 0; }
    printf '%s' "$1" | jq -r --arg key "sysctl.$2" '
        [.snapshot.checks[] | select(.category == "system")] as $system
        | ($system | map(select(.key == $key)) | first | .status)
          // ($system | map(select(.key == "sysctl")) | first | .status)
          // (if any($system[]; .key | startswith("sysctl.")) then "Compliant" else "absent" end)
    ' 2>/dev/null || echo unparsable
}

# One sysctl drift case, run against config $2 in the test pod: key $3 must
# read Compliant while it holds its applied value, and Violation after it is
# written to $4, so the case fails when compliance stops noticing the drift as
# well as when it stops reporting the key. $5 is written back afterwards. The
# key MUST be one only this pod can move (a network-namespaced net.* key): a
# host-global key can be written by another suite's pod on the same node
# between the two reads, so any other key fails the case without running it.
sysctl_drift_case() {
    local id="$1" config="$2" key="$3" drift="$4" restore="$5"
    local before after json
    case "$key" in
        net.*) ;;
        *) fail_test "$id" "$key is host-global; drift a pod-private net.* key instead"; return ;;
    esac
    before=$(sysctl_compliance_status "$(pod_compliance_json "$config")" "$key")
    exec_in_pod sysctl -w "$key=$drift" > /dev/null 2>&1 || true
    json=$(pod_compliance_json "$config")
    after=$(sysctl_compliance_status "$json" "$key")
    echo "  $key: before=$before after=$after"
    echo "$json" | jq -c '.snapshot.checks[]? | select(.category == "system")' 2>/dev/null | sed 's/^/    /' || true

    if assert_equals "$before" "Compliant" && assert_equals "$after" "Violation"; then
        pass_test "$id"
    else
        fail_test "$id" "Compliance should read $key Compliant when applied and Violation once drifted"
    fi
    exec_in_pod sysctl -w "$key=$restore" > /dev/null 2>&1 || true
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

# Whether cfgd would find brew here, asking the same three questions
# brew_available() asks in crates/cfgd/src/packages/shared/mod.rs: the
# CFGD_BREW_BIN seam, then PATH, then the prefixes the installer uses. A brew
# that is installed but not exported still counts, so `command -v brew` alone
# answers this wrong.
cfgd_finds_brew() {
    if [ -n "${CFGD_BREW_BIN:-}" ]; then
        [ -f "$CFGD_BREW_BIN" ]
        return
    fi
    if command -v brew > /dev/null 2>&1; then
        return 0
    fi
    for candidate in /home/linuxbrew/.linuxbrew/bin/brew /opt/homebrew/bin/brew /usr/local/bin/brew; do
        if [ -f "$candidate" ]; then
            return 0
        fi
    done
    return 1
}

# Whether any manager on this host packages a secret backend's CLI. The op, bw
# and vault entries of INSTALLABLE_TOOLS (crates/cfgd-core/src/providers/mod.rs)
# name brew, winget, chocolatey and scoop and decline every Linux distribution
# manager, because no distribution packages those CLIs.
secret_cli_install_route_available() {
    if cfgd_finds_brew; then
        return 0
    fi
    for manager in winget choco scoop; do
        if command -v "$manager" > /dev/null 2>&1; then
            return 0
        fi
    done
    return 1
}

# What a plan says about a declared secret whose backend CLI is missing. Which
# of the two rows cfgd writes is the host's decision, not a choice: with a
# manager that packages the CLI, the planner adds the install and names the
# backend that asked for it; with none, it cannot install anything, so it
# writes the skip row saying the provider is out of reach and why.
assert_missing_secret_cli() {
    local output="$1"
    local provider="$2"
    local tool="$3"
    if secret_cli_install_route_available; then
        assert_contains "$output" "required by secret:$provider"
    else
        assert_contains "$output" "provider '$provider' not available" &&
            assert_contains "$output" "$tool is not installed"
    fi
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
