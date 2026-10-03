#!/usr/bin/env bash
# Removes this run's PR install of the operator and CSI driver: the run's
# cfgd.io objects, then the Helm release, then its namespace. Every step runs
# even when one before it failed, and the script exits 1 when any failed.
#
# Usage: tests/e2e/pr-install-down.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=tests/e2e/common/helpers.sh
source "$SCRIPT_DIR/common/helpers.sh"
trap '[ -z "${E2E_SCRATCH_OWNED:-}" ] || rm -rf "$E2E_SCRATCH_OWNED"' EXIT

failed=()

echo "Removing the PR install $E2E_INSTALL_RELEASE in $E2E_INSTALL_NS..."

# The run's operator holds a finalizer on the objects it reconciles and is the
# only thing that clears it, so they are deleted while it still runs; once the
# release is gone they would stay Terminating.
for kinds in machineconfigs,configpolicies,driftalerts,backuppolicies clusterconfigpolicies,modules; do
    if ! kubectl delete "$kinds" -A -l "$E2E_RUN_LABEL" --wait=true --timeout=90s; then
        echo "ERROR: the $kinds labelled $E2E_RUN_LABEL were not deleted within 90s. Read the kubectl error above and the operator's log (kubectl logs -n $E2E_INSTALL_NS deploy/$E2E_OPERATOR_DEPLOY)." >&2
        failed+=("delete $kinds")
    fi
done

# A setup that failed before its helm install leaves no release, which is
# already the state this step wants.
if ! release_found=$(helm list -n "$E2E_INSTALL_NS" -a -q --filter "^${E2E_INSTALL_RELEASE}\$"); then
    echo "ERROR: could not list the Helm releases in $E2E_INSTALL_NS. Check that the runner can read secrets there, then rerun this teardown." >&2
    failed+=("find release")
elif [ -z "$release_found" ]; then
    echo "  release $E2E_INSTALL_RELEASE is not installed in $E2E_INSTALL_NS; nothing to uninstall"
elif ! helm uninstall "$E2E_INSTALL_RELEASE" -n "$E2E_INSTALL_NS" --wait --timeout=120s; then
    echo "ERROR: helm uninstall $E2E_INSTALL_RELEASE in $E2E_INSTALL_NS failed. Read the Helm error above, then rerun this teardown." >&2
    failed+=("helm uninstall")
fi

if ! kubectl delete namespace "$E2E_INSTALL_NS" --ignore-not-found --wait=false; then
    echo "ERROR: could not delete namespace $E2E_INSTALL_NS. Read the kubectl error above, then rerun this teardown." >&2
    failed+=("delete namespace")
fi

# Read back what the uninstall removes: a CSIDriver or webhook configuration
# left behind blocks the next run's setup or rejects its objects.
if ! csi_owner_ns=$(kubectl get csidriver "$CSI_DRIVER_NAME" --ignore-not-found \
    -o jsonpath='{.metadata.annotations.meta\.helm\.sh/release-namespace}'); then
    echo "ERROR: could not read csidriver/$CSI_DRIVER_NAME to confirm it is gone." >&2
    failed+=("read csidriver")
elif [ "$csi_owner_ns" = "$E2E_INSTALL_NS" ]; then
    echo "ERROR: csidriver/$CSI_DRIVER_NAME from $E2E_INSTALL_NS is still registered; delete it (kubectl delete csidriver $CSI_DRIVER_NAME) before the next run's setup." >&2
    failed+=("csidriver left")
else
    echo "  csidriver/$CSI_DRIVER_NAME from $E2E_INSTALL_NS is gone"
fi
if ! webhooks_left=$(kubectl get "validatingwebhookconfiguration/$E2E_VALIDATING_WEBHOOK" \
    "mutatingwebhookconfiguration/$E2E_MUTATING_WEBHOOK" --ignore-not-found -o name); then
    echo "ERROR: could not read the PR install's webhook configurations to confirm they are gone." >&2
    failed+=("read webhooks")
elif [ -n "$webhooks_left" ]; then
    echo "ERROR: the PR install's webhook configurations are still registered: $(echo "$webhooks_left" | tr '\n' ' ')" >&2
    failed+=("webhooks left")
else
    echo "  validatingwebhookconfiguration/$E2E_VALIDATING_WEBHOOK and mutatingwebhookconfiguration/$E2E_MUTATING_WEBHOOK are gone"
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
