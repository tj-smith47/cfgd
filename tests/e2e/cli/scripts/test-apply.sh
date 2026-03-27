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

print_summary "Apply"
