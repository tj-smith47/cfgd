#!/usr/bin/env bash
# run-all.sh for node tests — sources domain files in same process
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

# The real binary runs as the invoking user; keep it out of that user's home.
source "$SCRIPT_DIR/../../common/scratch-home.sh"
source "$SCRIPT_DIR/setup-node-env.sh"
trap 'cleanup_e2e' EXIT

# Domain files are sourced, not executed
source "$SCRIPT_DIR/test-apply.sh"
source "$SCRIPT_DIR/test-init.sh"
source "$SCRIPT_DIR/test-sysctl.sh"
source "$SCRIPT_DIR/test-kernel-modules.sh"
source "$SCRIPT_DIR/test-seccomp.sh"
source "$SCRIPT_DIR/test-certificates.sh"
source "$SCRIPT_DIR/test-daemon.sh"

SUMMARY_RC=0
print_summary "Node Tests" || SUMMARY_RC=1
assert_real_config_dir_unchanged || SUMMARY_RC=1
exit "$SUMMARY_RC"
