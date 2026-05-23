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

begin_test "AL10: cfgd alias set <name> <command> writes spec.aliases.<name>"
run $ALIAS_C alias set greet "status"
if ! assert_ok; then
    fail_test "AL10" "alias set exit code != 0"
elif ! grep -qE "^[[:space:]]*greet: status\$" "$ALIAS_CONF"; then
    echo "  ASSERT FAILED: spec.aliases.greet not present in cfgd.yaml"
    echo "  Config YAML:"
    sed 's/^/    /' "$ALIAS_CONF"
    fail_test "AL10" "spec.aliases.greet missing after set"
else
    pass_test "AL10"
fi

begin_test "AL11: cfgd alias add <name> <command> (clap alias for set) writes spec.aliases.<name>"
run $ALIAS_C alias add greet2 "status"
if ! assert_ok; then
    fail_test "AL11" "alias add exit code != 0"
elif ! grep -qE "^[[:space:]]*greet2: status\$" "$ALIAS_CONF"; then
    echo "  ASSERT FAILED: spec.aliases.greet2 not present in cfgd.yaml"
    sed 's/^/    /' "$ALIAS_CONF"
    fail_test "AL11" "spec.aliases.greet2 missing after add"
else
    pass_test "AL11"
fi

begin_test "AL12: cfgd alias show <name> prints the alias command"
run $ALIAS_C alias show greet
if ! assert_ok; then
    fail_test "AL12" "alias show exit code != 0"
elif ! assert_contains "$OUTPUT" "status"; then
    fail_test "AL12" "alias show did not print the alias command"
else
    pass_test "AL12"
fi

begin_test "AL13: cfgd alias ls lists every alias by name and command"
run $ALIAS_C alias ls
if ! assert_ok; then
    fail_test "AL13" "alias ls exit code != 0"
elif ! assert_contains "$OUTPUT" "greet" \
    || ! assert_contains "$OUTPUT" "greet2" \
    || ! assert_contains "$OUTPUT" "status"; then
    fail_test "AL13" "alias ls output missing expected name/command pair"
else
    pass_test "AL13"
fi

begin_test "AL14: cfgd alias rm <name> (clap alias for delete) removes spec.aliases.<name>"
run $ALIAS_C alias rm greet
if ! assert_ok; then
    fail_test "AL14" "alias rm exit code != 0"
elif grep -qE "^[[:space:]]*greet: status\$" "$ALIAS_CONF"; then
    echo "  ASSERT FAILED: spec.aliases.greet still present after rm"
    sed 's/^/    /' "$ALIAS_CONF"
    fail_test "AL14" "spec.aliases.greet retained after rm"
else
    pass_test "AL14"
fi

begin_test "AL15: cfgd alias delete <name> (canonical) removes spec.aliases.<name>"
run $ALIAS_C alias delete greet2
if ! assert_ok; then
    fail_test "AL15" "alias delete exit code != 0"
elif grep -qE "^[[:space:]]*greet2: status\$" "$ALIAS_CONF"; then
    echo "  ASSERT FAILED: spec.aliases.greet2 still present after delete"
    sed 's/^/    /' "$ALIAS_CONF"
    fail_test "AL15" "spec.aliases.greet2 retained after delete"
else
    pass_test "AL15"
fi

begin_test "AL16: cfgd config rm <key> (clap alias on Unset) removes the key"
run $ALIAS_C alias set greet3 "status"
if [ "$RC" -ne 0 ]; then
    fail_test "AL16" "preflight alias set failed (exit $RC)"
else
    run $ALIAS_C config rm aliases.greet3
    if ! assert_ok; then
        fail_test "AL16" "config rm exit code != 0"
    elif grep -qE "^[[:space:]]*greet3: status\$" "$ALIAS_CONF"; then
        echo "  ASSERT FAILED: spec.aliases.greet3 still present after config rm"
        sed 's/^/    /' "$ALIAS_CONF"
        fail_test "AL16" "spec.aliases.greet3 retained after config rm"
    else
        pass_test "AL16"
    fi
fi

begin_test "AL17: cfgd config ls (clap alias on Show) returns the configuration view"
run $ALIAS_C config ls
if ! assert_ok; then
    fail_test "AL17" "config ls exit code != 0"
elif ! assert_contains "$OUTPUT" "dev"; then
    fail_test "AL17" "config ls output missing expected profile value"
else
    pass_test "AL17"
fi

print_summary "Aliases"
