#!/usr/bin/env bash
# E2E tests for: cfgd init
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd init tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "I01: init --from local git repo"
ISRC="$SCRATCH/init-src"; IDST="$SCRATCH/init-dst"; IHOME="$SCRATCH/init-home"
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
(cd "$ISRC" && git init -q && git add -A && git commit -qm "init")
run init "$IDST" --from "$ISRC" --no-color
if assert_ok && [ -f "$IDST/cfgd.yaml" ] && [ -d "$IDST/profiles" ]; then
    pass_test "I01"
else fail_test "I01" "Config not created"; fi

begin_test "I02: init --from with --branch"
run init "$SCRATCH/init-branch" --from "$ISRC" --branch main --no-color
if [ -f "$SCRATCH/init-branch/cfgd.yaml" ]; then
    pass_test "I02"
else fail_test "I02"; fi

begin_test "I03: init --from with --theme"
run init "$SCRATCH/init-theme" --from "$ISRC" --theme minimal --no-color
if [ -f "$SCRATCH/init-theme/cfgd.yaml" ]; then
    pass_test "I03"
else fail_test "I03"; fi

begin_test "I04: init --from with --apply-module"
run init "$SCRATCH/init-mod" --from "$ISRC" --apply-module nvim --no-color
# Module may not exist in source, but init should still succeed
if [ -f "$SCRATCH/init-mod/cfgd.yaml" ]; then
    pass_test "I04"
else fail_test "I04"; fi

begin_test "I05: init from existing config dir"
RDST="$SCRATCH/init-existing"
mkdir -p "$RDST"
cp "$CFG/cfgd.yaml" "$RDST/"
run --config "$RDST/cfgd.yaml" init --no-color
if [ -f "$RDST/cfgd.yaml" ]; then
    pass_test "I05"
else fail_test "I05"; fi

begin_test "I06: init --name"
I06_DIR="$SCRATCH/init-name"
run init "$I06_DIR" --from "$ISRC" --name my-custom-config --no-color
if assert_ok && [ -f "$I06_DIR/cfgd.yaml" ]; then
    pass_test "I06"
else fail_test "I06"; fi

begin_test "I07: init --apply-profile"
I07_DIR="$SCRATCH/init-apply-profile"
run init "$I07_DIR" --from "$ISRC" --apply-profile base --no-color
if [ -f "$I07_DIR/cfgd.yaml" ]; then
    pass_test "I07"
else fail_test "I07"; fi

print_summary "Init"
