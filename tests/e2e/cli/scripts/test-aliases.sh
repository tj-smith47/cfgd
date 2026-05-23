#!/usr/bin/env bash
# E2E tests for cfgd.yaml `spec.aliases` expansion: argv rewrite at CLI entry
# plus literal-dash preservation in the alias argument (e.g. `remove -/path`).
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd alias tests ==="

# Per-suite isolated config so AL01/AL02 don't fight other suites' state.
ALIAS_CFG="$SCRATCH/alias-cfg"
ALIAS_TGT="$SCRATCH/alias-home"
ALIAS_STATE="$SCRATCH/alias-state"
mkdir -p "$ALIAS_STATE"
setup_config_dir "$ALIAS_CFG" "$ALIAS_TGT"
ALIAS_CONF="$ALIAS_CFG/cfgd.yaml"
ALIAS_PROFILE="$ALIAS_CFG/profiles/dev.yaml"
ALIAS_C="--config $ALIAS_CONF --state-dir $ALIAS_STATE --no-color"

ALIAS_FILE="$ALIAS_TGT/foo.txt"
touch "$ALIAS_FILE"

begin_test "AL01: cfgd add <path> appends to spec.files"
run $ALIAS_C add "$ALIAS_FILE"
if ! assert_ok; then
    fail_test "AL01" "alias exit code != 0"
elif ! grep -qE "^[[:space:]]*target: ${ALIAS_FILE//\//\\/}\$" "$ALIAS_PROFILE"; then
    echo "  ASSERT FAILED: spec.files missing entry for $ALIAS_FILE"
    echo "  Profile YAML:"
    sed 's/^/    /' "$ALIAS_PROFILE"
    fail_test "AL01" "spec.files did not gain the added entry"
else
    pass_test "AL01"
fi

begin_test "AL02: cfgd remove -<path> drops the spec.files entry"
run $ALIAS_C remove "-$ALIAS_FILE"
if ! assert_ok; then
    fail_test "AL02" "alias exit code != 0"
elif grep -qE "^[[:space:]]*target: ${ALIAS_FILE//\//\\/}\$" "$ALIAS_PROFILE"; then
    echo "  ASSERT FAILED: spec.files still contains $ALIAS_FILE"
    echo "  Profile YAML:"
    sed 's/^/    /' "$ALIAS_PROFILE"
    fail_test "AL02" "spec.files retained the entry after remove"
else
    pass_test "AL02"
fi

print_summary "Aliases"
