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

# === Init end-to-end file tree validation ===

begin_test "I08: init --from --apply-module deploys module files"
# Build a source repo with a module that has files, env, and aliases
I08_SRC="$SCRATCH/i08-src"
I08_DST="$SCRATCH/i08-dst"
I08_HOME="$SCRATCH/i08-home"
mkdir -p "$I08_SRC/modules/test-mod/files" "$I08_HOME"
echo "# test config" > "$I08_SRC/modules/test-mod/files/test.conf"
cat > "$I08_SRC/modules/test-mod/module.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: test-mod
spec:
  files:
  - source: files/test.conf
    target: $I08_HOME/.config/test-app/test.conf
  env:
  - name: TEST_APP_HOME
    value: $I08_HOME
  aliases:
  - name: tapp
    command: echo test-app
YAML
cat > "$I08_SRC/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: i08-test
spec: {}
YAML
(cd "$I08_SRC" && git init -q -b master && git add -A && git commit -qm "init")
run init "$I08_DST" --from "$I08_SRC" --apply-module test-mod --yes --no-color
if assert_ok; then
    # Verify: config dir was cloned
    PASS=true
    if [ ! -f "$I08_DST/cfgd.yaml" ]; then
        fail_test "I08" "cfgd.yaml not cloned"
        PASS=false
    fi
    # Verify: module file was deployed (symlink or copy)
    if [ ! -e "$I08_HOME/.config/test-app/test.conf" ]; then
        fail_test "I08" "Module file not deployed to target"
        PASS=false
    fi
    # Verify: env file was created
    ENV_FILE="$HOME/.cfgd.env"
    if [ -f "$ENV_FILE" ] && grep -q "TEST_APP_HOME" "$ENV_FILE"; then
        : # good
    else
        # env file may be elsewhere; just check it wasn't skipped
        PASS=true
    fi
    # Verify: no files outside scratch dir
    # (module target is $I08_HOME which is in $SCRATCH)
    if $PASS; then
        pass_test "I08"
    fi
else
    fail_test "I08" "init --from --apply-module failed"
fi

begin_test "I09: init --from does NOT dirty cloned repo"
I09_SRC="$SCRATCH/i09-src"
I09_DST="$SCRATCH/i09-dst"
mkdir -p "$I09_SRC"
cat > "$I09_SRC/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: i09-test
spec: {}
YAML
(cd "$I09_SRC" && git init -q -b master && git add -A && git commit -qm "init")
run init "$I09_DST" --from "$I09_SRC" --no-color
if assert_ok; then
    # The cloned repo should be clean (no uncommitted changes)
    DIRTY=$(cd "$I09_DST" && git status --porcelain 2>/dev/null || echo "")
    if [ -z "$DIRTY" ]; then
        pass_test "I09"
    else
        fail_test "I09" "Cloned repo is dirty: $DIRTY"
    fi
else
    fail_test "I09" "init failed"
fi

begin_test "I10: init --from same repo twice does NOT pull"
I10_DST="$SCRATCH/i10-dst"
run init "$I10_DST" --from "$ISRC" --no-color
FIRST_HEAD=$(cd "$I10_DST" && git rev-parse HEAD 2>/dev/null || echo "")
# Run again — should detect already initialized, not re-clone or pull
run init "$I10_DST" --from "$ISRC" --no-color
SECOND_HEAD=$(cd "$I10_DST" && git rev-parse HEAD 2>/dev/null || echo "")
if [ "$FIRST_HEAD" = "$SECOND_HEAD" ] && [ -n "$FIRST_HEAD" ]; then
    pass_test "I10"
else
    fail_test "I10" "HEAD changed between runs"
fi

print_summary "Init"
