#!/usr/bin/env bash
# E2E tests for: `strategy: Patch` (partial-file edits) through cfgd apply
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== strategy: Patch tests ==="

# Dedicated config: Patch entries need targets seeded with hand-written content
PT_DIR="$SCRATCH/pt"
PT_CFG="$PT_DIR/cfg"
PT_TGT="$PT_DIR/home"
PT_STATE="$PT_DIR/state"
mkdir -p "$PT_CFG/profiles" "$PT_CFG/scripts" "$PT_TGT" "$PT_STATE"
printf '{\n  "runtimeToken": "keep-me",\n  "telemetry": true\n}\n' > "$PT_TGT/settings.json"
printf 'editor: vim\n' > "$PT_TGT/prefs.yaml"
cat > "$PT_CFG/scripts/upper.sh" << 'SCRIPT'
#!/bin/sh
tr '[:lower:]' '[:upper:]'
SCRIPT
chmod +x "$PT_CFG/scripts/upper.sh"
cat > "$PT_CFG/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: patch-e2e
spec:
  profile: base
YAML
cat > "$PT_CFG/profiles/base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  files:
    managed:
      - target: $PT_TGT/settings.json
        strategy: Patch
        patch:
          ensure:
            telemetry: false
      - target: $PT_TGT/prefs.yaml
        strategy: Patch
        patch:
          ensure:
            theme: dark
      - target: $PT_TGT/created.json
        strategy: Patch
        patch:
          format: Json
          ensure:
            fresh: true
      - target: $PT_TGT/shout.txt
        strategy: Patch
        patch:
          script: scripts/upper.sh
YAML
PC="--config $PT_CFG/cfgd.yaml --state-dir $PT_STATE --no-color"
printf 'quiet text\n' > "$PT_TGT/shout.txt"

begin_test "PT01: apply with Patch entries succeeds"
run $PC apply --yes
if assert_ok; then
    pass_test "PT01"
else fail_test "PT01"; fi

begin_test "PT02: ensure merges into JSON, unmentioned keys survive"
if grep -q '"keep-me"' "$PT_TGT/settings.json" && grep -q '"telemetry": false' "$PT_TGT/settings.json"; then
    pass_test "PT02"
else fail_test "PT02" "$(cat "$PT_TGT/settings.json")"; fi

begin_test "PT03: format inferred from .yaml extension, existing keys survive"
if grep -q 'editor: vim' "$PT_TGT/prefs.yaml" && grep -q 'theme: dark' "$PT_TGT/prefs.yaml"; then
    pass_test "PT03"
else fail_test "PT03" "$(cat "$PT_TGT/prefs.yaml")"; fi

begin_test "PT04: missing target — ensure writes a minimal document"
if grep -q '"fresh": true' "$PT_TGT/created.json"; then
    pass_test "PT04"
else fail_test "PT04" "$(cat "$PT_TGT/created.json" 2>/dev/null)"; fi

begin_test "PT05: script patch transforms current content via stdin/stdout"
if grep -q 'QUIET TEXT' "$PT_TGT/shout.txt"; then
    pass_test "PT05"
else fail_test "PT05" "$(cat "$PT_TGT/shout.txt")"; fi

begin_test "PT06: second apply is idempotent"
SNAP_BEFORE=$(cat "$PT_TGT/settings.json" "$PT_TGT/prefs.yaml" "$PT_TGT/shout.txt")
run $PC apply --yes
SNAP_AFTER=$(cat "$PT_TGT/settings.json" "$PT_TGT/prefs.yaml" "$PT_TGT/shout.txt")
if assert_ok && [ "$SNAP_BEFORE" = "$SNAP_AFTER" ]; then
    pass_test "PT06"
else fail_test "PT06"; fi

begin_test "PT07: encryption on a Patch entry is a validation error"
PV_DIR="$SCRATCH/pt-invalid"
mkdir -p "$PV_DIR/cfg/profiles" "$PV_DIR/state"
cat > "$PV_DIR/cfg/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: patch-invalid-e2e
spec:
  profile: base
YAML
cat > "$PV_DIR/cfg/profiles/base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  files:
    managed:
      - target: $PV_DIR/out.json
        strategy: Patch
        encryption: sops
        patch:
          ensure:
            a: 1
YAML
run --config "$PV_DIR/cfg/cfgd.yaml" --state-dir "$PV_DIR/state" --no-color apply --yes
if assert_fail; then
    pass_test "PT07"
else fail_test "PT07"; fi

print_summary "Patch"
