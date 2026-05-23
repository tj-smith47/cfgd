#!/usr/bin/env bash
# E2E tests for: config-file aliases (cfgd add / cfgd remove)
#
# The aliases are scaffolded by `cfgd init` into the user's shell init AND
# into cfgd.yaml's `spec.aliases`. At CLI invocation, `expand_aliases`
# rewrites the argv before clap parses. `setup-cli-env.sh` already seeds
# `add: "profile update --file"` and `remove: "profile update --file"` into
# the test config, so these cases exercise the same expansion path users
# get after running `cfgd init`.
#
# The `--state-dir` flag is intentionally passed via the CFGD_STATE_DIR
# env var (not on argv) so the alias-expansion subcommand-finder sees the
# alias token as the first positional.
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
export CFGD_STATE_DIR="$ALIAS_STATE"

# The file the alias adds to / removes from the profile's spec.files.
ALIAS_FILE="$ALIAS_TGT/foo.txt"
touch "$ALIAS_FILE"

begin_test "AL01: cfgd add <path> appends to spec.files"
run --config "$ALIAS_CONF" --no-color add "$ALIAS_FILE"
if ! assert_ok; then
    fail_test "AL01" "alias exit code != 0"
elif ! grep -qF "target: $ALIAS_FILE" "$ALIAS_PROFILE"; then
    echo "  ASSERT FAILED: spec.files missing entry for $ALIAS_FILE"
    echo "  Profile YAML:"
    sed 's/^/    /' "$ALIAS_PROFILE"
    fail_test "AL01" "spec.files did not gain the added entry"
else
    pass_test "AL01"
fi

begin_test "AL02: cfgd remove -<path> drops the spec.files entry"
run --config "$ALIAS_CONF" --no-color remove "-$ALIAS_FILE"
if ! assert_ok; then
    fail_test "AL02" "alias exit code != 0"
elif grep -qF "target: $ALIAS_FILE" "$ALIAS_PROFILE"; then
    echo "  ASSERT FAILED: spec.files still contains $ALIAS_FILE"
    echo "  Profile YAML:"
    sed 's/^/    /' "$ALIAS_PROFILE"
    fail_test "AL02" "spec.files retained the entry after remove"
else
    pass_test "AL02"
fi

unset CFGD_STATE_DIR

print_summary "Aliases"
