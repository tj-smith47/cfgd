#!/usr/bin/env bash
# Sets cfgd.io/heartbeat on every namespace of this run to now minus
# HEARTBEAT_STALE_SECONDS, so the run reads as dead to the next setup's
# reclaim_stale_install and to the janitor. It exercises the stale-install
# takeover on the cluster; a run's own suites refresh the heartbeat again.
#
# Usage: tests/e2e/age-heartbeat.sh
#        GITHUB_RUN_ID=<run id> tests/e2e/age-heartbeat.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=tests/e2e/common/helpers.sh
source "$SCRIPT_DIR/common/helpers.sh"
trap '[ -z "${E2E_SCRATCH_OWNED:-}" ] || rm -rf "$E2E_SCRATCH_OWNED"' EXIT

if ! namespaces=$(kubectl get namespaces -l "$E2E_RUN_LABEL" -o name); then
    echo "ERROR: could not list the namespaces labelled $E2E_RUN_LABEL. Read the kubectl error above." >&2
    exit 1
fi
if [ -z "$namespaces" ]; then
    echo "ERROR: no namespace carries $E2E_RUN_LABEL (run $E2E_RUN_ID, from $E2E_RUN_ID_SOURCE); there is no heartbeat to age." >&2
    exit 1
fi
aged=$(($(date -u +%s) - HEARTBEAT_STALE_SECONDS))
if ! kubectl annotate namespace -l "$E2E_RUN_LABEL" "cfgd.io/heartbeat=$aged" --overwrite; then
    echo "ERROR: could not set cfgd.io/heartbeat on the namespaces labelled $E2E_RUN_LABEL. Read the kubectl error above." >&2
    exit 1
fi
echo "Aged the heartbeat of run $E2E_RUN_ID (from $E2E_RUN_ID_SOURCE) to $aged, ${HEARTBEAT_STALE_SECONDS}s ago, on: $(paste -sd ' ' <<<"$namespaces")"
