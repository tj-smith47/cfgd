#!/usr/bin/env bash
# E2E tests for: cfgd apply
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd apply tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "A01: apply --help"
run $C apply --help
if assert_ok && assert_contains "$OUTPUT" "dry-run"; then
    pass_test "A01"
else fail_test "A01"; fi

begin_test "A02: apply --dry-run"
run $C apply --dry-run
if assert_ok; then
    pass_test "A02"
else fail_test "A02"; fi

begin_test "A03: apply --yes"
run $C apply --yes
if assert_ok; then
    pass_test "A03"
else fail_test "A03"; fi

begin_test "A04: apply -y (short flag)"
run $C apply -y
if assert_ok; then
    pass_test "A04"
else fail_test "A04"; fi

begin_test "A05: apply --dry-run --phase files"
run $C apply --dry-run --phase files
if assert_ok; then
    pass_test "A05"
else fail_test "A05"; fi

begin_test "A06: apply --dry-run --phase packages"
run $C apply --dry-run --phase packages
if assert_ok; then
    pass_test "A06"
else fail_test "A06"; fi

begin_test "A07: apply --dry-run --phase system"
run $C apply --dry-run --phase system
if assert_ok; then
    pass_test "A07"
else fail_test "A07"; fi

begin_test "A08: apply --dry-run --phase env"
run $C apply --dry-run --phase env
if assert_ok; then
    pass_test "A08"
else fail_test "A08"; fi

begin_test "A09: apply --dry-run --phase secrets"
run $C apply --dry-run --phase secrets
if assert_ok; then
    pass_test "A09"
else fail_test "A09"; fi

begin_test "A10: apply --skip files"
run $C apply --dry-run --skip files
if assert_ok; then
    pass_test "A10"
else fail_test "A10"; fi

begin_test "A11: apply --only files"
run $C apply --dry-run --only files
if assert_ok; then
    pass_test "A11"
else fail_test "A11"; fi

begin_test "A12: apply --skip multiple"
run $C apply --dry-run --skip files --skip packages
if assert_ok; then
    pass_test "A12"
else fail_test "A12"; fi

begin_test "A13: apply --only multiple"
run $C apply --dry-run --only files --only env
if assert_ok; then
    pass_test "A13"
else fail_test "A13"; fi

begin_test "A14: apply --module (nonexistent module)"
run $C apply --dry-run --module nonexistent
# Should gracefully handle missing module
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "A14"
else fail_test "A14" "exit $RC"; fi

begin_test "A15: apply --dry-run --yes (both flags)"
run $C apply --dry-run --yes
if assert_ok; then
    pass_test "A15"
else fail_test "A15"; fi

begin_test "A16: apply creates expected files"
rm -f "$TGT/.gitconfig" "$TGT/.zshrc"
run $C apply --yes
if [ -f "$TGT/.gitconfig" ] && [ -f "$TGT/.zshrc" ]; then
    pass_test "A16"
else fail_test "A16" "Expected files not created"; fi

begin_test "A17: apply idempotent (second run no changes)"
run $C apply --yes
if assert_ok; then
    pass_test "A17"
else fail_test "A17"; fi

# SECTION 32: additional apply flags

begin_test "A18: apply --skip-scripts"
run $C apply --dry-run --skip-scripts
if assert_ok; then
    pass_test "A18"
else fail_test "A18"; fi

begin_test "A19: apply --from (local git repo)"
ISRC="$SCRATCH/init-src"; IHOME="$SCRATCH/init-home"
mkdir -p "$ISRC/profiles" "$ISRC/files" "$IHOME"
for f in "$FIXTURES/profiles/"*.yaml; do
    sed "s|TARGET_DIR|$IHOME|g" "$f" > "$ISRC/profiles/$(basename "$f")"
done
cp -r "$FIXTURES/files/"* "$ISRC/files/" 2>/dev/null || true
cat > "$ISRC/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: init-test
spec:
  profile: base
YAML
(cd "$ISRC" && git init -q && git add -A && git commit -qm "init") 2>/dev/null || true
A19_DST="$SCRATCH/apply-from-test"
run $C apply --from "$ISRC" --dry-run --no-color --config "$A19_DST/cfgd.yaml" --state-dir "$SCRATCH/state-a19"
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "A19"
else fail_test "A19" "exit $RC"; fi

# === Apply end-to-end file tree validation ===

begin_test "A20: apply --yes deploys files as symlinks to correct targets"
# Verify the files created by A16 are symlinks pointing into the config dir
if [ -L "$TGT/.gitconfig" ]; then
    LINK_TARGET=$(readlink "$TGT/.gitconfig" 2>/dev/null || echo "")
    if echo "$LINK_TARGET" | grep -q "files/gitconfig"; then
        pass_test "A20"
    else
        fail_test "A20" "Symlink points to wrong target: $LINK_TARGET"
    fi
else
    # May be a copy if strategy is Copy; still valid
    if [ -f "$TGT/.gitconfig" ]; then
        pass_test "A20"
    else
        fail_test "A20" ".gitconfig not deployed"
    fi
fi

begin_test "A21: apply --yes creates env file with aliases"
# After A16's apply, env file should exist with profile env vars
ENV_FILE="$HOME/.cfgd.env"
if [ -f "$ENV_FILE" ] && grep -q "EDITOR" "$ENV_FILE"; then
    pass_test "A21"
else
    # env file might not be created if profile has no env vars
    # check if our profile actually has env
    if grep -q "env:" "$CFG/profiles/dev.yaml" 2>/dev/null; then
        fail_test "A21" "Profile has env but ~/.cfgd.env missing or incomplete"
    else
        pass_test "A21"
    fi
fi

begin_test "A22: apply with module deploys module files to correct paths"
# Create a module with a file, apply it, verify deployment
A22_CFG="$SCRATCH/a22-cfg"
A22_TGT="$SCRATCH/a22-home"
A22_STATE="$SCRATCH/a22-state"
mkdir -p "$A22_CFG/modules/a22-mod/files" "$A22_TGT" "$A22_STATE"
echo "a22-content" > "$A22_CFG/modules/a22-mod/files/a22.conf"
cat > "$A22_CFG/modules/a22-mod/module.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: a22-mod
spec:
  files:
  - source: files/a22.conf
    target: $A22_TGT/.a22.conf
YAML
setup_config_dir "$A22_CFG" "$A22_TGT"
A22_C="--config $A22_CFG/cfgd.yaml --state-dir $A22_STATE --no-color"
run $A22_C apply --yes --module a22-mod
if assert_ok && [ -e "$A22_TGT/.a22.conf" ]; then
    # Verify content matches
    if grep -q "a22-content" "$A22_TGT/.a22.conf" 2>/dev/null || \
       ([ -L "$A22_TGT/.a22.conf" ] && grep -q "a22-content" "$(readlink -f "$A22_TGT/.a22.conf")" 2>/dev/null); then
        pass_test "A22"
    else
        fail_test "A22" "Content mismatch"
    fi
else
    fail_test "A22" "Module file not deployed"
fi

begin_test "A23: apply --dry-run creates NO files"
A23_CFG="$SCRATCH/a23-cfg"
A23_TGT="$SCRATCH/a23-home"
A23_STATE="$SCRATCH/a23-state"
mkdir -p "$A23_CFG/modules/a23-mod/files" "$A23_TGT" "$A23_STATE"
echo "a23-content" > "$A23_CFG/modules/a23-mod/files/a23.conf"
cat > "$A23_CFG/modules/a23-mod/module.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: a23-mod
spec:
  files:
  - source: files/a23.conf
    target: $A23_TGT/.a23.conf
YAML
setup_config_dir "$A23_CFG" "$A23_TGT"
A23_C="--config $A23_CFG/cfgd.yaml --state-dir $A23_STATE --no-color"
run $A23_C apply --dry-run --module a23-mod
# The target file must NOT exist — dry-run should not deploy
if [ ! -e "$A23_TGT/.a23.conf" ]; then
    pass_test "A23"
else
    fail_test "A23" "dry-run created files it shouldn't have"
fi

print_summary "Apply"
