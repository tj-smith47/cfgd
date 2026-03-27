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

begin_test "CO08: compliance detects drift (missing managed file)"
# Apply to create managed files, then remove one → compliance should show violation
CO08_CFG="$SCRATCH/co08-cfg"
CO08_TGT="$SCRATCH/co08-home"
CO08_STATE="$SCRATCH/co08-state"
mkdir -p "$CO08_STATE"
setup_config_dir "$CO08_CFG" "$CO08_TGT"
CO08_CONF="$CO08_CFG/cfgd.yaml"
cat > "$CO08_CONF" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: co08-e2e
spec:
  profile: dev
  compliance:
    enabled: true
YAML
CO08="--config $CO08_CONF --state-dir $CO08_STATE --no-color"
# Apply to deploy managed files
run $CO08 apply --yes
if assert_ok; then
    # Remove a managed file to simulate drift
    rm -f "$CO08_TGT/.zshrc"
    run $CO08 compliance
    # Summary line should show non-zero violation count (e.g. "1 violation")
    if assert_ok && assert_contains "$OUTPUT" "Summary:" && assert_contains "$OUTPUT" "violation"; then
        pass_test "CO08"
    else fail_test "CO08" "Expected non-zero violation in summary after removing managed file"; fi
else fail_test "CO08" "Apply failed"; fi

begin_test "CO09: compliance after restore shows compliant"
# Re-apply to restore the file, then check compliance
run $CO08 apply --yes
if assert_ok; then
    run $CO08 compliance
    # All-compliant message: "All N check(s) compliant"
    if assert_ok && assert_contains "$OUTPUT" "check(s) compliant"; then
        pass_test "CO09"
    else fail_test "CO09" "Expected all-compliant after restore"; fi
else fail_test "CO09" "Apply failed"; fi

begin_test "CO10: compliance export JSON format is valid"
CO10_EXPORT="$SCRATCH/co10-export"
CO10_STATE="$SCRATCH/co10-state"
mkdir -p "$CO10_STATE" "$CO10_EXPORT"
CO10_CFG="$SCRATCH/co10-cfg"
CO10_TGT="$SCRATCH/co10-home"
setup_config_dir "$CO10_CFG" "$CO10_TGT"
CO10_CONF="$CO10_CFG/cfgd.yaml"
cat > "$CO10_CONF" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: co10-e2e
spec:
  profile: dev
  compliance:
    enabled: true
    export:
      format: Json
      path: $CO10_EXPORT
YAML
run --config "$CO10_CONF" --state-dir "$CO10_STATE" --no-color compliance export
if assert_ok; then
    EXPORT_FILE=$(ls "$CO10_EXPORT/"compliance-*.json 2>/dev/null | head -1)
    if [ -n "$EXPORT_FILE" ] && python3 -c "import json; json.load(open('$EXPORT_FILE'))" 2>/dev/null; then
        pass_test "CO10"
    else fail_test "CO10" "Export file missing or not valid JSON"; fi
else fail_test "CO10"; fi

begin_test "CO11: compliance diff shows changes between snapshots"
CO11_STATE="$SCRATCH/co11-state"
CO11_CFG="$SCRATCH/co11-cfg"
CO11_TGT="$SCRATCH/co11-home"
mkdir -p "$CO11_STATE"
setup_config_dir "$CO11_CFG" "$CO11_TGT"
CO11_CONF="$CO11_CFG/cfgd.yaml"
cat > "$CO11_CONF" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: co11-e2e
spec:
  profile: dev
  compliance:
    enabled: true
YAML
CO11="--config $CO11_CONF --state-dir $CO11_STATE --no-color"
# Apply to create files, take first snapshot
run $CO11 apply --yes
run $CO11 compliance
# Remove a managed file to change state, take second snapshot
rm -f "$CO11_TGT/.zshrc"
run $CO11 compliance
# Diff the two snapshots (IDs 1 and 2)
run $CO11 compliance diff 1 2
if assert_ok && assert_contains "$OUTPUT" "Diff"; then
    pass_test "CO11"
else fail_test "CO11"; fi

begin_test "CO12: compliance history --since filters by recent time"
# Use the CO11 state which already has 2 snapshots
run $CO11 compliance history --since 1h
if assert_ok && assert_not_contains "$OUTPUT" "No compliance snapshots"; then
    pass_test "CO12"
else fail_test "CO12"; fi

begin_test "CO13: compliance with scope filtering"
CO13_STATE="$SCRATCH/co13-state"
CO13_CFG="$SCRATCH/co13-cfg"
CO13_TGT="$SCRATCH/co13-home"
mkdir -p "$CO13_STATE"
setup_config_dir "$CO13_CFG" "$CO13_TGT"
CO13_CONF="$CO13_CFG/cfgd.yaml"
# Config that only checks files (packages, system, secrets disabled)
cat > "$CO13_CONF" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: co13-e2e
spec:
  profile: dev
  compliance:
    enabled: true
    scope:
      files: true
      packages: false
      system: false
      secrets: false
YAML
CO13="--config $CO13_CONF --state-dir $CO13_STATE --no-color"
run $CO13 apply --yes
run $CO13 -o json compliance
if assert_ok; then
    # Verify checks only contain file category (no package/system/secret)
    if assert_contains "$OUTPUT" "file" && assert_not_contains "$OUTPUT" "\"category\":\"package\""; then
        pass_test "CO13"
    else fail_test "CO13" "Scope filtering did not restrict to files only"; fi
else fail_test "CO13"; fi

begin_test "CO14: compliance JSON output matches table content"
run $CO -o json compliance
if assert_ok && assert_contains "$OUTPUT" "checks" && assert_contains "$OUTPUT" "summary" \
    && assert_contains "$OUTPUT" "compliant" && assert_contains "$OUTPUT" "warning" \
    && assert_contains "$OUTPUT" "violation"; then
    pass_test "CO14"
else fail_test "CO14"; fi

print_summary "Compliance"
