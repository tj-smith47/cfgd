#!/usr/bin/env bash
# E2E tests for: cfgd compliance
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd compliance tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

# Compliance tests need a config with compliance enabled
CO_CFG="$SCRATCH/co-cfg"
CO_TGT="$SCRATCH/co-home"
CO_STATE="$SCRATCH/co-state"
mkdir -p "$CO_STATE"
setup_config_dir "$CO_CFG" "$CO_TGT"
CO_CONF="$CO_CFG/cfgd.yaml"
# Enable compliance in config
cat > "$CO_CONF" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: compliance-e2e
spec:
  profile: dev
  compliance:
    enabled: true
    export:
      format: Json
      path: $SCRATCH/co-export
YAML
CO="--config $CO_CONF --state-dir $CO_STATE --no-color"

begin_test "CO01: compliance prints summary"
run $CO compliance
if assert_ok; then
    pass_test "CO01"
else fail_test "CO01"; fi

begin_test "CO02: compliance -o json"
run $CO -o json compliance
if assert_ok && assert_contains "$OUTPUT" "checks" && assert_contains "$OUTPUT" "summary"; then
    pass_test "CO02"
else fail_test "CO02"; fi

begin_test "CO03: compliance export writes snapshot file"
run $CO compliance export
if assert_ok; then
    # Check that a file was written to the export path
    if ls "$SCRATCH/co-export/"compliance-*.json >/dev/null 2>&1; then
        pass_test "CO03"
    else
        fail_test "CO03" "No snapshot file in export path"
    fi
else fail_test "CO03"; fi

begin_test "CO04: compliance history (empty initially)"
CO_STATE2="$SCRATCH/co-state2"
mkdir -p "$CO_STATE2"
run --config "$CO_CONF" --state-dir "$CO_STATE2" --no-color compliance history
if assert_ok; then
    pass_test "CO04"
else fail_test "CO04"; fi

begin_test "CO05: compliance history --since 1h"
run $CO compliance history --since 1h
if assert_ok; then
    pass_test "CO05"
else fail_test "CO05"; fi

begin_test "CO06: compliance then compliance history shows entry"
# CO01 already stored a snapshot in CO_STATE, so history should have at least 1
run $CO compliance history
if assert_ok && assert_not_contains "$OUTPUT" "No compliance snapshots"; then
    pass_test "CO06"
else fail_test "CO06"; fi

begin_test "CO07: compliance diff with nonexistent IDs"
run $CO compliance diff 99999 99998
if assert_fail; then
    pass_test "CO07"
else fail_test "CO07"; fi

print_summary "Compliance"
