#!/usr/bin/env bash
# E2E tests for: cfgd backup (run / list / restore)
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd backup tests ==="

# Dedicated config: spec.backups needs its own profile plus a real source file
BK_DIR="$SCRATCH/bk"
BK_CFG="$BK_DIR/cfg"
BK_STATE="$BK_DIR/state"
BK_SRC="$BK_DIR/data/notes.txt"
BK_MARKER="$BK_DIR/hook-marker"
mkdir -p "$BK_CFG/profiles" "$BK_STATE" "$(dirname "$BK_SRC")"
echo "generation-one" > "$BK_SRC"
cat > "$BK_CFG/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: backup-e2e
spec:
  profile: base
YAML
cat > "$BK_CFG/profiles/base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  backups:
    - name: notes
      source: $BK_SRC
      retention: 2
      preBackup:
        - echo "pre \$CFGD_OPERATION" >> $BK_MARKER
      postBackup:
        - echo "post \$CFGD_OPERATION" >> $BK_MARKER
YAML
BC="--config $BK_CFG/cfgd.yaml --state-dir $BK_STATE --no-color"
SNAP_DIR="$BK_STATE/backups/notes"

begin_test "BK01: backup --help"
run $BC backup --help
if assert_ok && assert_contains "$OUTPUT" "backup"; then
    pass_test "BK01"
else fail_test "BK01"; fi

begin_test "BK02: backup list shows the declared unit"
run $BC backup list
if assert_ok && assert_contains "$OUTPUT" "notes" && assert_contains "$OUTPUT" "$BK_SRC"; then
    pass_test "BK02"
else fail_test "BK02"; fi

begin_test "BK03: backup run creates a snapshot in the state dir"
run $BC backup run notes
if assert_ok && [ -n "$(ls "$SNAP_DIR" 2>/dev/null)" ]; then
    pass_test "BK03"
else fail_test "BK03" "no snapshot under $SNAP_DIR"; fi

begin_test "BK04: hooks ran with CFGD_OPERATION=backup"
if grep -q "pre backup" "$BK_MARKER" && grep -q "post backup" "$BK_MARKER"; then
    pass_test "BK04"
else fail_test "BK04" "marker: $(cat "$BK_MARKER" 2>/dev/null)"; fi

begin_test "BK05: backup run unknown name fails and names valid units"
run $BC backup run missing-name
if assert_fail && assert_contains "$OUTPUT" "notes"; then
    pass_test "BK05"
else fail_test "BK05"; fi

begin_test "BK06: backup list <name> --snapshots lists snapshot names"
run $BC backup list notes --snapshots
if assert_ok && assert_contains "$OUTPUT" "notes.txt."; then
    pass_test "BK06"
else fail_test "BK06"; fi

begin_test "BK07: bare backup list --snapshots is a usage error"
run $BC backup list --snapshots
if assert_fail; then
    pass_test "BK07"
else fail_test "BK07"; fi

begin_test "BK08: -o json backup run reports destinationPath"
run $BC --output json backup run notes
if assert_ok && assert_contains "$OUTPUT" '"destinationPath"'; then
    pass_test "BK08"
else fail_test "BK08"; fi

begin_test "BK09: retention prunes to the declared count"
echo "generation-two" > "$BK_SRC"
run $BC backup run notes
echo "generation-three" > "$BK_SRC"
run $BC backup run notes
COUNT=$(ls "$SNAP_DIR" | wc -l)
if [ "$COUNT" -eq 2 ]; then
    pass_test "BK09"
else fail_test "BK09" "expected 2 snapshots after retention, got $COUNT"; fi

begin_test "BK10: restore without --yes on non-interactive stdin is an error"
run $BC backup restore notes < /dev/null
if assert_fail; then
    pass_test "BK10"
else fail_test "BK10"; fi

begin_test "BK11: restore --yes puts the newest snapshot back and leaves a sidecar safety copy"
echo "clobbered-live-data" > "$BK_SRC"
SNAP_COUNT_BEFORE=$(ls "$SNAP_DIR" | wc -l)
run $BC backup restore notes --yes
# The displaced contents go beside the source as the <source>.cfgd-backup
# sidecar, never into the unit's snapshot history — so the snapshot set the
# operator restores FROM does not grow when they restore.
SNAP_COUNT_AFTER=$(ls "$SNAP_DIR" | wc -l)
if assert_ok && [ "$(cat "$BK_SRC")" = "generation-three" ] \
    && grep -q "clobbered-live-data" "$BK_SRC.cfgd-backup" \
    && [ "$SNAP_COUNT_AFTER" -eq "$SNAP_COUNT_BEFORE" ]; then
    pass_test "BK11"
else fail_test "BK11" "content=$(cat "$BK_SRC"), sidecar=$(cat "$BK_SRC.cfgd-backup" 2>/dev/null), snapshots=$SNAP_COUNT_BEFORE->$SNAP_COUNT_AFTER"; fi

begin_test "BK12: restore hooks ran with CFGD_OPERATION=restore"
if grep -q "pre restore" "$BK_MARKER" && grep -q "post restore" "$BK_MARKER"; then
    pass_test "BK12"
else fail_test "BK12" "marker: $(cat "$BK_MARKER" 2>/dev/null)"; fi

begin_test "BK13: restore --to elsewhere leaves the live source untouched"
echo "live-stays" > "$BK_SRC"
run $BC backup restore notes --to "$BK_DIR/inspect" --yes
if assert_ok && [ "$(cat "$BK_SRC")" = "live-stays" ] && ls "$BK_DIR/inspect" >/dev/null 2>&1; then
    pass_test "BK13"
else fail_test "BK13"; fi

begin_test "BK14: restore --at with an unknown value fails and lists snapshots"
run $BC backup restore notes --at 19990101T000000Z --yes
if assert_fail && assert_contains "$OUTPUT" "notes.txt."; then
    pass_test "BK14"
else fail_test "BK14"; fi

begin_test "BK15: apply runs schedule-less backups"
AP_DIR="$SCRATCH/bk-apply"
mkdir -p "$AP_DIR/cfg/profiles" "$AP_DIR/state" "$AP_DIR/data"
echo "apply-me" > "$AP_DIR/data/app.db"
cat > "$AP_DIR/cfg/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: backup-apply-e2e
spec:
  profile: base
YAML
cat > "$AP_DIR/cfg/profiles/base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  backups:
    - name: appdb
      source: $AP_DIR/data/app.db
      retention: 3
YAML
run --config "$AP_DIR/cfg/cfgd.yaml" --state-dir "$AP_DIR/state" --no-color apply --yes
if assert_ok && [ -n "$(ls "$AP_DIR/state/backups/appdb" 2>/dev/null)" ]; then
    pass_test "BK15"
else fail_test "BK15" "no snapshot after apply"; fi

begin_test "BK16: apply --dry-run previews but takes no snapshot"
rm -rf "$AP_DIR/state/backups"
run --config "$AP_DIR/cfg/cfgd.yaml" --state-dir "$AP_DIR/state" --no-color apply --dry-run
if assert_ok && [ ! -d "$AP_DIR/state/backups/appdb" ]; then
    pass_test "BK16"
else fail_test "BK16"; fi

begin_test "BK17: bare backup rollback lists what has a copy to put back"
run $BC backup rollback
if assert_ok && assert_contains "$OUTPUT" "notes" && assert_contains "$OUTPUT" "cfgd-backup"; then
    pass_test "BK17"
else fail_test "BK17"; fi

begin_test "BK18: rollback --yes puts the sidecar copy back over the source"
# Read both sides first: the rollback swaps them, and BK19 asserts the swap
# back, so the expectations come from the machine rather than from a literal
# an earlier cell happened to write.
RB_COPY=$(cat "$BK_SRC.cfgd-backup")
RB_DISPLACED=$(cat "$BK_SRC")
SNAP_COUNT_BEFORE=$(ls "$SNAP_DIR" | wc -l)
# Both cells assert a swap, so equal fixture values would let a rollback that
# moved nothing pass either one.
run $BC backup rollback notes --yes
if assert_ok && [ "$RB_COPY" != "$RB_DISPLACED" ] \
    && [ "$(cat "$BK_SRC")" = "$RB_COPY" ] \
    && [ "$(ls "$SNAP_DIR" | wc -l)" -eq "$SNAP_COUNT_BEFORE" ]; then
    pass_test "BK18"
else fail_test "BK18" "content=$(cat "$BK_SRC"), expected=$RB_COPY, displaced=$RB_DISPLACED"; fi

begin_test "BK19: a second rollback undoes the first"
run $BC backup rollback notes --yes
if assert_ok && [ "$RB_COPY" != "$RB_DISPLACED" ] \
    && [ "$(cat "$BK_SRC")" = "$RB_DISPLACED" ]; then
    pass_test "BK19"
else fail_test "BK19" "content=$(cat "$BK_SRC"), expected=$RB_DISPLACED, copy=$RB_COPY"; fi

print_summary "Backup"
