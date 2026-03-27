#!/usr/bin/env bash
# E2E tests for: cfgd module
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd module tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

begin_test "M01: module --help"
run $C module --help
if assert_ok && assert_contains "$OUTPUT" "create" && assert_contains "$OUTPUT" "list"; then
    pass_test "M01"
else fail_test "M01"; fi

begin_test "M02: module create (minimal)"
run $C module create nvim --description "Neovim config"
if assert_ok; then
    pass_test "M02"
else fail_test "M02"; fi

begin_test "M03: module create --description"
run $C module create tmux --description "tmux configuration"
if assert_ok; then
    pass_test "M03"
else fail_test "M03"; fi

begin_test "M04: module create --depends"
run $C module create shell --depends nvim
if assert_ok; then
    pass_test "M04"
else fail_test "M04"; fi

begin_test "M05: module create --package"
run $C module create editor --package brew:neovim
if assert_ok; then
    pass_test "M05"
else fail_test "M05"; fi

begin_test "M06: module create --file"
mkdir -p "$TGT/.config/nvim" && touch "$TGT/.config/nvim/init.lua"
run $C module create dotfiles --file "$TGT/.config/nvim/init.lua"
if assert_ok; then
    pass_test "M06"
else fail_test "M06"; fi

begin_test "M07: module create --private-files"
touch "$TGT/.secret-conf"
run $C module create secret-mod --private-files --file "$TGT/.secret-conf"
if assert_ok; then
    pass_test "M07"
else fail_test "M07"; fi

begin_test "M08: module create --post-apply"
run $C module create scripted --post-apply "echo done"
if assert_ok; then
    pass_test "M08"
else fail_test "M08"; fi

begin_test "M09: module create --set"
run $C module create overridden --package brew:neovim --set "package.neovim.minVersion=0.9"
if assert_ok; then
    pass_test "M09"
else fail_test "M09"; fi

begin_test "M10: module create with all flags"
touch "$TGT/.tmux.conf"
run $C module create full-mod --description "full" --depends nvim --package brew:bat --file "$TGT/.tmux.conf" --post-apply "echo test"
if assert_ok; then
    pass_test "M10"
else fail_test "M10"; fi

begin_test "M11: module create duplicate fails"
run $C module create nvim
if assert_fail; then
    pass_test "M11"
else fail_test "M11"; fi

begin_test "M12: module list"
run $C module list
if assert_ok && assert_contains "$OUTPUT" "nvim"; then
    pass_test "M12"
else fail_test "M12"; fi

begin_test "M13: module show"
run $C module show nvim
if assert_ok && assert_contains "$OUTPUT" "nvim"; then
    pass_test "M13"
else fail_test "M13"; fi

begin_test "M14: module show nonexistent fails"
run $C module show nonexistent
if assert_fail; then
    pass_test "M14"
else fail_test "M14"; fi

begin_test "M15: module update --package (add)"
run $C module update nvim --package brew:ripgrep
if assert_ok; then
    pass_test "M15"
else fail_test "M15"; fi

begin_test "M16: module update --package (remove)"
run $C module update nvim --package -brew:ripgrep
if assert_ok; then
    pass_test "M16"
else fail_test "M16"; fi

begin_test "M17: module update --file (add)"
mkdir -p "$TGT/.config/nvim/after/plugin" && touch "$TGT/.config/nvim/after/plugin/test.lua"
run $C module update nvim --file "$TGT/.config/nvim/after/plugin/test.lua"
if assert_ok; then
    pass_test "M17"
else fail_test "M17"; fi

begin_test "M18: module update --file (remove)"
run $C module update nvim --file "-$TGT/.config/nvim/after/plugin/test.lua"
if assert_ok; then
    pass_test "M18"
else fail_test "M18"; fi

begin_test "M19: module update --depends (add)"
run $C module update nvim --depends tmux
if assert_ok; then
    pass_test "M19"
else fail_test "M19"; fi

begin_test "M20: module update --depends (remove)"
run $C module update nvim --depends -tmux
if assert_ok; then
    pass_test "M20"
else fail_test "M20"; fi

begin_test "M21: module update --description"
run $C module update nvim --description "updated neovim config"
if assert_ok; then
    pass_test "M21"
else fail_test "M21"; fi

begin_test "M22: module update --post-apply (add)"
run $C module update nvim --post-apply "echo updated"
if assert_ok; then
    pass_test "M22"
else fail_test "M22"; fi

begin_test "M23: module update --post-apply (remove)"
run $C module update nvim --post-apply "-echo updated"
if assert_ok; then
    pass_test "M23"
else fail_test "M23"; fi

begin_test "M24: module update --set"
run $C module update editor --set "package.neovim.minVersion=0.9"
if assert_ok; then
    pass_test "M24"
else fail_test "M24"; fi

begin_test "M25: module update --private-files"
run $C module update nvim --private-files
if assert_ok; then
    pass_test "M25"
else fail_test "M25"; fi

begin_test "M26: module delete --yes"
run $C module delete scripted --yes
if assert_ok; then
    pass_test "M26"
else fail_test "M26"; fi

begin_test "M27: module delete -y"
run $C module delete secret-mod -y
if assert_ok; then
    pass_test "M27"
else fail_test "M27"; fi

begin_test "M28: module delete nonexistent fails"
run $C module delete nonexistent --yes
if assert_fail; then
    pass_test "M28"
else fail_test "M28"; fi

begin_test "M29: module upgrade (no remote — should fail gracefully)"
run $C module upgrade nvim --yes
# No remote source for local module, should handle gracefully
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "M29"
else fail_test "M29" "exit $RC"; fi

begin_test "M30: module upgrade --ref"
run $C module upgrade nvim --ref main --yes
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "M30"
else fail_test "M30" "exit $RC"; fi

begin_test "M31: module upgrade --allow-unsigned"
run $C module upgrade nvim --allow-unsigned --yes
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "M31"
else fail_test "M31" "exit $RC"; fi

begin_test "M32: module search"
run $C module search neovim
# May fail without registry, but should not crash
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "M32"
else fail_test "M32" "exit $RC"; fi

begin_test "M33: module registry list"
run $C module registry list
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "M33"
else fail_test "M33" "exit $RC"; fi

begin_test "M34: module registry add"
run $C module registry add "$SOURCE_REPO" --name test-registry
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "M34"
else fail_test "M34" "exit $RC"; fi

begin_test "M35: module registry remove"
run $C module registry remove test-registry
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "M35"
else fail_test "M35" "exit $RC"; fi

# SECTION 37: module create/update additional flags

begin_test "M36: module create --env"
run $C module create env-mod --env TEST_VAR=hello
if assert_ok; then
    pass_test "M36"
else fail_test "M36"; fi

begin_test "M37: module create --alias"
run $C module create alias-mod --alias myalias="echo hi"
if assert_ok; then
    pass_test "M37"
else fail_test "M37"; fi

begin_test "M38: module update --env (add)"
run $C module update nvim --env NVIM_VAR=test
if assert_ok; then
    pass_test "M38"
else fail_test "M38"; fi

begin_test "M39: module update --env (remove)"
run $C module update nvim --env -NVIM_VAR
if assert_ok; then
    pass_test "M39"
else fail_test "M39"; fi

begin_test "M40: module update --alias (add)"
run $C module update nvim --alias nv="nvim ."
if assert_ok; then
    pass_test "M40"
else fail_test "M40"; fi

begin_test "M41: module update --alias (remove)"
run $C module update nvim --alias -nv
if assert_ok; then
    pass_test "M41"
else fail_test "M41"; fi

begin_test "M42: module show --show-values"
run $C module show nvim --show-values
if assert_ok; then
    pass_test "M42"
else fail_test "M42"; fi

begin_test "M43: module delete --purge"
# Create a module with a file, then delete with --purge
touch "$TGT/.purge-test"
run $C module create purge-mod --file "$TGT/.purge-test"
if [ "$RC" -ne 0 ]; then
    fail_test "M43" "module create failed (exit $RC)"
else
    run $C module delete purge-mod --yes --purge
    if assert_ok; then
        pass_test "M43"
    else fail_test "M43"; fi
fi

begin_test "M44: module edit (existing module)"
EDITOR=true run $C module edit nvim
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "M44"
else fail_test "M44" "exit $RC"; fi

begin_test "M45: module ls (alias)"
run $C module ls
if assert_ok && assert_contains "$OUTPUT" "nvim"; then
    pass_test "M45"
else fail_test "M45"; fi

begin_test "M46: module registry rename"
run $C module registry add "$SOURCE_REPO" --name rename-src
run $C module registry rename rename-src renamed-src
if assert_ok; then
    pass_test "M46"
else fail_test "M46"; fi
# cleanup
"$CFGD" $C module registry remove renamed-src > /dev/null 2>&1 || true

# SECTION 28: module export

begin_test "MX01: module export --format devcontainer"
EXPORT_DIR="$SCRATCH/export-test"
mkdir -p "$EXPORT_DIR"
run $C module export nvim --format devcontainer --dir "$EXPORT_DIR"
if assert_ok; then
    pass_test "MX01"
else fail_test "MX01"; fi

begin_test "MX02: module export nonexistent fails"
run $C module export nonexistent --format devcontainer
if assert_fail; then
    pass_test "MX02"
else fail_test "MX02"; fi

# SECTION 29: module OCI (push/pull) — requires registry

begin_test "OCI01: module push"
OCI_DIR="$SCRATCH/oci-push-test"
OCI_PUSH_OK=false
mkdir -p "$OCI_DIR/bin"
cat > "$OCI_DIR/module.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: oci-cli-test
spec:
  packages: []
  files:
    - source: bin/hello.sh
      target: bin/hello.sh
YAML
echo '#!/bin/sh' > "$OCI_DIR/bin/hello.sh"
echo 'echo "hello from oci test"' >> "$OCI_DIR/bin/hello.sh"
chmod +x "$OCI_DIR/bin/hello.sh"
run $C module push "$OCI_DIR" --artifact "${REGISTRY}/cfgd-e2e/cli-oci-test:v1.0"
if assert_ok; then
    OCI_PUSH_OK=true
    pass_test "OCI01"
else fail_test "OCI01"; fi

begin_test "OCI02: module pull"
if [ "$OCI_PUSH_OK" = "true" ]; then
    OCI_PULL_DIR="$SCRATCH/oci-pull-test"
    mkdir -p "$OCI_PULL_DIR"
    run $C module pull "${REGISTRY}/cfgd-e2e/cli-oci-test:v1.0" --dir "$OCI_PULL_DIR"
    if assert_ok && [ -f "$OCI_PULL_DIR/module.yaml" ]; then
        pass_test "OCI02"
    else fail_test "OCI02"; fi
else
    skip_test "OCI02" "OCI01 push failed — no artifact to pull"
fi

begin_test "OCI03: module push --platform"
if [ "$OCI_PUSH_OK" = "true" ]; then
    run $C module push "$OCI_DIR" --artifact "${REGISTRY}/cfgd-e2e/cli-oci-platform:v1.0" --platform linux/amd64
    if assert_ok; then
        pass_test "OCI03"
    else fail_test "OCI03"; fi
else
    skip_test "OCI03" "OCI01 push failed — registry unavailable"
fi

# SECTION 30: module keys

begin_test "MK01: module keys list"
run $C module keys list
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "MK01"
else fail_test "MK01" "exit $RC"; fi

begin_test "MK02: module keys generate"
if command -v cosign > /dev/null 2>&1; then
    KEYS_DIR="$SCRATCH/keys-test"
    mkdir -p "$KEYS_DIR"
    run $C module keys generate --dir "$KEYS_DIR"
    if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
        pass_test "MK02"
    else fail_test "MK02" "exit $RC"; fi
else
    skip_test "MK02" "cosign not available"
fi

# SECTION 31: module build

begin_test "MB01: module build --help"
run $C module build --help
if assert_ok && assert_contains "$OUTPUT" "base-image"; then
    pass_test "MB01"
else fail_test "MB01"; fi

begin_test "MB02: module build (no docker — graceful fail)"
BUILD_DIR="$SCRATCH/build-test"
mkdir -p "$BUILD_DIR"
cp "$OCI_DIR/module.yaml" "$BUILD_DIR/"
mkdir -p "$BUILD_DIR/bin"
cp "$OCI_DIR/bin/hello.sh" "$BUILD_DIR/bin/"
# Build requires docker/podman; test that flag parsing works
run $C module build "$BUILD_DIR" --target linux/amd64
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "MB02"
else fail_test "MB02" "exit $RC"; fi

print_summary "Module"
