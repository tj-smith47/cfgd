#!/usr/bin/env bash
# Shared setup for node E2E tests. Sourced by run-all.sh (NOT by domain files directly).
# Domain files are also sourced by run-all.sh into the same process to share the test pod.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
FIXTURES="$SCRIPT_DIR/../fixtures"

# Trap is set ONCE by run-all.sh (the outermost script)
# Domain files must NOT set their own traps

echo "Setting up test pod..."
ensure_test_pod

echo "Copying test fixtures to test pod..."
exec_in_pod mkdir -p /etc/cfgd/profiles
cp_to_pod "$FIXTURES/configs/cfgd.yaml" /etc/cfgd/cfgd.yaml
for f in "$FIXTURES/profiles/"*.yaml; do
    cp_to_pod "$f" "/etc/cfgd/profiles/$(basename "$f")"
done
