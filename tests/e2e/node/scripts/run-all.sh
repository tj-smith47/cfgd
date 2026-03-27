#!/usr/bin/env bash
# run-all.sh for node tests — sources domain files in same process
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
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

print_summary "Node Tests"
