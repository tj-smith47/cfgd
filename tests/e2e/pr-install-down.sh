#!/usr/bin/env bash
# Removes this run's PR install of the operator and CSI driver: the run's
# cfgd.io objects, the Crossplane suite's TeamConfigs, Composition and
# Function, then the Helm release, then its namespace. Every step runs
# even when one before it failed, and the script exits 1 when any failed.
#
# The run id comes from helpers.sh: GITHUB_RUN_ID when set, else the id a
# local setup recorded. When neither the run's release nor its namespace
# exists (a Terminating namespace counting as gone) and no object it removes
# or reads back is left, it prints what it looked for and exits 1 without
# touching anything.
#
# Usage: tests/e2e/pr-install-down.sh
#        GITHUB_RUN_ID=<run id> tests/e2e/pr-install-down.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=tests/e2e/common/helpers.sh
source "$SCRIPT_DIR/common/helpers.sh"
trap '[ -z "${E2E_SCRATCH_OWNED:-}" ] || rm -rf "$E2E_SCRATCH_OWNED"' EXIT

failed=()

echo "Removing the PR install $E2E_INSTALL_RELEASE in $E2E_INSTALL_NS (run $E2E_RUN_ID, from $E2E_RUN_ID_SOURCE)..."

# What the run owns, each spelled once here: the run-labelled kinds in delete
# order, cluster-scoped ones included, each group as <kinds>|<what to read when
# its delete fails> ("operator" stands for the run's operator log), and the
# cluster-scoped objects the uninstall removes by name. Every lookup, delete
# and read-back below reads these. A TeamConfig composite references the run's
# Composition, so the TeamConfig group comes before the Composition's.
delete_groups=(
    "machineconfigs,configpolicies,driftalerts,backuppolicies|operator"
    "clusterconfigpolicies,modules|operator"
    "teamconfigs|the composites' status.conditions"
    "composition,function|the Function's status.conditions"
)
labelled_kinds="$(for group in "${delete_groups[@]}"; do printf '%s\n' "${group%%|*}"; done | paste -sd ,)"
named_objects=("validatingwebhookconfiguration/$E2E_VALIDATING_WEBHOOK" "mutatingwebhookconfiguration/$E2E_MUTATING_WEBHOOK"
    "function/$E2E_FUNCTION" "composition/$E2E_COMPOSITION")

# The Helm owner of the CSIDriver every run's install registers.
csidriver_owner() {
    kubectl get csidriver "$CSI_DRIVER_NAME" --ignore-not-found \
        -o jsonpath='{.metadata.annotations.meta\.helm\.sh/release-namespace}'
}

# Every object this teardown removes or reads back is looked for first, so a
# rerun after a partly failed teardown still removes what is left. A namespace
# already Terminating is an earlier teardown's and counts as gone.
if ! ns_start_phase=$(kubectl get namespace "$E2E_INSTALL_NS" --ignore-not-found -o jsonpath='{.status.phase}'); then
    echo "ERROR: could not read namespace $E2E_INSTALL_NS. Read the kubectl error above, then rerun this teardown." >&2
    failed+=("find namespace")
fi
release_listed=true
if ! release_found=$(helm list -n "$E2E_INSTALL_NS" -a -q --filter "^${E2E_INSTALL_RELEASE}\$"); then
    echo "ERROR: could not list the Helm releases in $E2E_INSTALL_NS. Check that the runner can read secrets there, then rerun this teardown." >&2
    failed+=("find release")
    release_listed=false
fi
if ! labelled_found=$(kubectl get "$labelled_kinds" -A -l "$E2E_RUN_LABEL" -o name); then
    echo "ERROR: could not list the objects labelled $E2E_RUN_LABEL. Read the kubectl error above, then rerun this teardown." >&2
    failed+=("find labelled objects")
fi
if ! named_found=$(kubectl get "${named_objects[@]}" --ignore-not-found -o name); then
    echo "ERROR: could not read ${named_objects[*]}. Read the kubectl error above, then rerun this teardown." >&2
    failed+=("find named objects")
fi
if ! csi_start_owner=$(csidriver_owner); then
    echo "ERROR: could not read csidriver/$CSI_DRIVER_NAME. Read the kubectl error above, then rerun this teardown." >&2
    failed+=("find csidriver")
fi
if [ "${#failed[@]}" -eq 0 ] && [ -z "$release_found" ] && [ -z "$labelled_found" ] && [ -z "$named_found" ] &&
    [ "$csi_start_owner" != "$E2E_INSTALL_NS" ] && { [ -z "$ns_start_phase" ] || [ "$ns_start_phase" = Terminating ]; }; then
    {
        echo "ERROR: no PR install to remove for run $E2E_RUN_ID (from $E2E_RUN_ID_SOURCE): no Helm release $E2E_INSTALL_RELEASE, namespace $E2E_INSTALL_NS ${ns_start_phase:-absent}, no $labelled_kinds labelled $E2E_RUN_LABEL, none of ${named_objects[*]}, and csidriver/$CSI_DRIVER_NAME is not this install's."
        echo "  The run ids of the installs on the cluster: kubectl get namespaces -l cfgd.io/e2e-run -L cfgd.io/e2e-run"
        echo "  Remove one with: GITHUB_RUN_ID=<run id> bash tests/e2e/pr-install-down.sh"
    } >&2
    exit 1
fi

# The run's operator holds a finalizer on the objects it reconciles and is the
# only thing that clears it, so they are deleted while it still runs; once the
# release is gone they would stay Terminating.
for group in "${delete_groups[@]}"; do
    kinds="${group%%|*}"
    hint="${group#*|}"
    if [ "$hint" = operator ]; then
        hint="the operator's log (kubectl logs -n $E2E_INSTALL_NS -l $E2E_OPERATOR_PODS --prefix --tail=-1)"
    fi
    if ! kubectl delete "$kinds" -A -l "$E2E_RUN_LABEL" --wait=true --timeout=90s; then
        echo "ERROR: the $kinds labelled $E2E_RUN_LABEL were not deleted within 90s. Read the kubectl error above and $hint." >&2
        failed+=("delete $kinds")
    fi
done

if [ "$release_listed" = true ]; then
    if [ -z "$release_found" ]; then
        echo "  release $E2E_INSTALL_RELEASE is not installed in $E2E_INSTALL_NS; nothing to uninstall"
    elif ! helm uninstall "$E2E_INSTALL_RELEASE" -n "$E2E_INSTALL_NS" --wait --timeout=120s; then
        echo "ERROR: helm uninstall $E2E_INSTALL_RELEASE in $E2E_INSTALL_NS failed. Read the Helm error above, then rerun this teardown." >&2
        failed+=("helm uninstall")
    fi
fi

if ! kubectl delete namespace "$E2E_INSTALL_NS" --ignore-not-found --wait=false; then
    echo "ERROR: could not delete namespace $E2E_INSTALL_NS. Read the kubectl error above, then rerun this teardown." >&2
    failed+=("delete namespace")
fi

# Read back what the uninstall and the deletes remove: a CSIDriver or webhook
# configuration left behind blocks the next run's setup or rejects its
# objects, and a Function left behind holds the package lock node the next
# run's Function needs.
if ! csi_owner_ns=$(csidriver_owner); then
    echo "ERROR: could not read csidriver/$CSI_DRIVER_NAME to confirm it is gone." >&2
    failed+=("read csidriver")
elif [ "$csi_owner_ns" = "$E2E_INSTALL_NS" ]; then
    echo "ERROR: csidriver/$CSI_DRIVER_NAME from $E2E_INSTALL_NS is still registered; delete it (kubectl delete csidriver $CSI_DRIVER_NAME) before the next run's setup." >&2
    failed+=("csidriver left")
else
    echo "  csidriver/$CSI_DRIVER_NAME from $E2E_INSTALL_NS is gone"
fi
if ! named_left=$(kubectl get "${named_objects[@]}" --ignore-not-found -o name); then
    echo "ERROR: could not read ${named_objects[*]} to confirm they are gone." >&2
    failed+=("read named objects")
elif [ -n "$named_left" ]; then
    echo "ERROR: these are still on the cluster: $(paste -sd ' ' <<<"$named_left"). Read their status.conditions and finalizers, then rerun this teardown." >&2
    # `-o name` prints <kind>.<group>/<name>; the failure names each kind.
    while read -r left; do
        failed+=("${left%%[./]*} left")
    done <<<"$named_left"
else
    echo "  ${named_objects[*]} are gone"
fi
if ! ns_phase=$(kubectl get namespace "$E2E_INSTALL_NS" --ignore-not-found -o jsonpath='{.status.phase}'); then
    echo "ERROR: could not read namespace $E2E_INSTALL_NS to confirm its deletion." >&2
    failed+=("read namespace")
elif [ -z "$ns_phase" ]; then
    echo "  namespace $E2E_INSTALL_NS is gone"
else
    echo "  namespace $E2E_INSTALL_NS is $ns_phase; Kubernetes removes it once its objects are gone"
fi

if [ "${#failed[@]}" -gt 0 ]; then
    echo "ERROR: the PR install teardown failed at: $(printf '%s, ' "${failed[@]}" | sed 's/, $//')" >&2
    exit 1
fi
echo "Removed the PR install $E2E_INSTALL_RELEASE"
