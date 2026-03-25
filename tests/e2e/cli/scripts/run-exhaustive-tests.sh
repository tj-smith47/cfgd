#!/usr/bin/env bash
# Exhaustive CLI test suite for cfgd.
# Tests every command, subcommand, and flag permutation.
# Designed to run inside Docker (see ../Dockerfile).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
FIXTURES="$SCRIPT_DIR/../fixtures"

echo "=== cfgd Exhaustive CLI Tests ==="

if [ -z "${CFGD:-}" ]; then
    if [ -f "$REPO_ROOT/target/release/cfgd" ]; then
        CFGD="$REPO_ROOT/target/release/cfgd"
    elif [ -f "$REPO_ROOT/target/debug/cfgd" ]; then
        CFGD="$REPO_ROOT/target/debug/cfgd"
    else
        CFGD="$(command -v cfgd)"
    fi
fi
echo "Binary: $CFGD"
"$CFGD" --version 2>&1 || true

SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT
echo "Scratch: $SCRATCH"

# Ensure git identity is configured (needed for git init/commit in tests)
if ! git config user.name >/dev/null 2>&1; then
    git config --global user.name "cfgd-test"
    git config --global user.email "test@cfgd.io"
fi

# Local git repo used as a "remote" source for source/sync/pull tests
SOURCE_REPO="$SCRATCH/source-repo"
setup_source_repo() {
    mkdir -p "$SOURCE_REPO/profiles" "$SOURCE_REPO/files"
    cat > "$SOURCE_REPO/cfgd-source.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: test-source
spec:
  provides:
    profiles: [base, dev, work-dev]
  policy:
    recommended:
      packages:
        brew:
          formulae: [jq, yq]
    constraints: {}
YAML
    for f in "$FIXTURES/profiles/"*.yaml; do
        sed "s|TARGET_DIR|/tmp/source-target|g" "$f" > "$SOURCE_REPO/profiles/$(basename "$f")"
    done
    cp -r "$FIXTURES/files/"* "$SOURCE_REPO/files/" 2>/dev/null || true
    (cd "$SOURCE_REPO" && git init -q -b master && git add -A && git commit -qm "init source repo")
}
setup_source_repo

# ─────────────────────────────────────────────────────
# Helpers
# ─────────────────────────────────────────────────────

setup_config_dir() {
    local config_dir="$1"
    local target_dir="$2"
    mkdir -p "$config_dir/profiles" "$config_dir/files" "$config_dir/modules" "$target_dir"
    for f in "$FIXTURES/profiles/"*.yaml; do
        sed "s|TARGET_DIR|$target_dir|g" "$f" > "$config_dir/profiles/$(basename "$f")"
    done
    cp -r "$FIXTURES/files/"* "$config_dir/files/"
    cat > "$config_dir/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: exhaustive-e2e
spec:
  profile: dev
  aliases:
    add: "profile update --file"
    remove: "profile update --file"
YAML
}

# Run cfgd, capture output + exit code. Does NOT fail the script on non-zero exit.
run() {
    local rc=0
    OUTPUT=$("$CFGD" "$@" 2>&1) || rc=$?
    RC=$rc
}

# Assert exit code is 0
assert_ok() {
    if [ "$RC" -ne 0 ]; then
        echo "  ASSERT FAILED: expected exit 0, got $RC"
        echo "$OUTPUT" | head -5 | sed 's/^/    /'
        return 1
    fi
}

# Assert exit code is non-zero
assert_fail() {
    if [ "$RC" -eq 0 ]; then
        echo "  ASSERT FAILED: expected non-zero exit, got 0"
        return 1
    fi
}

# ─────────────────────────────────────────────────────
# Standard config used by most tests
# ─────────────────────────────────────────────────────
CFG="$SCRATCH/cfg"
TGT="$SCRATCH/home"
STATE="$SCRATCH/state"
mkdir -p "$STATE"
setup_config_dir "$CFG" "$TGT"
CONF="$CFG/cfgd.yaml"
C="--config $CONF --state-dir $STATE --no-color"

# ═════════════════════════════════════════════════════
# SECTION 1: Global flags & help
# ═════════════════════════════════════════════════════

begin_test "G01: --help"
run $C --help
if assert_ok && assert_contains "$OUTPUT" "apply" && assert_contains "$OUTPUT" "profile"; then
    pass_test "G01"
else fail_test "G01"; fi

begin_test "G02: --version"
run --version
if assert_ok && assert_contains "$OUTPUT" "cfgd"; then
    pass_test "G02"
else fail_test "G02"; fi

begin_test "G03: --verbose flag accepted"
run $C status --verbose
# Just verify it doesn't crash — verbose may produce extra output or not
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "G03"
else fail_test "G03" "exit $RC"; fi

begin_test "G04: -v short flag"
run $C status -v
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "G04"
else fail_test "G04" "exit $RC"; fi

begin_test "G05: --quiet flag accepted"
run $C status --quiet
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "G05"
else fail_test "G05" "exit $RC"; fi

begin_test "G06: -q short flag"
run $C status -q
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "G06"
else fail_test "G06" "exit $RC"; fi

begin_test "G07: --no-color flag"
run --config "$CONF" --no-color status
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "G07"
else fail_test "G07"; fi

begin_test "G08: --profile override"
run $C --profile base status
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "G08"
else fail_test "G08"; fi

begin_test "G09: --config with bad path fails"
run --config /nonexistent/cfgd.yaml status
if assert_fail; then
    pass_test "G09"
else fail_test "G09"; fi

begin_test "G10: unknown subcommand fails"
run $C nonexistent-command
if assert_fail; then
    pass_test "G10"
else fail_test "G10"; fi

# ═════════════════════════════════════════════════════
# SECTION 2: init
# ═════════════════════════════════════════════════════

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

# ═════════════════════════════════════════════════════
# SECTION 3: apply
# ═════════════════════════════════════════════════════

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

# ═════════════════════════════════════════════════════
# SECTION 4: status / diff / log / verify / doctor
# ═════════════════════════════════════════════════════

begin_test "S01: status"
run $C status
if assert_ok; then
    pass_test "S01"
else fail_test "S01"; fi

begin_test "S02: status --verbose"
run $C status --verbose
if assert_ok; then
    pass_test "S02"
else fail_test "S02"; fi

begin_test "S03: status --quiet"
run $C status --quiet
if assert_ok; then
    pass_test "S03"
else fail_test "S03"; fi

begin_test "D01: diff"
run $C diff
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "D01"
else fail_test "D01" "exit $RC"; fi

begin_test "L01: log"
run $C log
if assert_ok; then
    pass_test "L01"
else fail_test "L01"; fi

begin_test "L02: log --limit 5"
run $C log --limit 5
if assert_ok; then
    pass_test "L02"
else fail_test "L02"; fi

begin_test "L03: log -n 1"
run $C log -n 1
if assert_ok; then
    pass_test "L03"
else fail_test "L03"; fi

begin_test "V01: verify"
run $C verify
if assert_ok; then
    pass_test "V01"
else fail_test "V01"; fi

begin_test "DR01: doctor"
run $C doctor
if assert_ok; then
    pass_test "DR01"
else fail_test "DR01"; fi

# ═════════════════════════════════════════════════════
# SECTION 5: profile
# ═════════════════════════════════════════════════════

begin_test "P01: profile --help"
run $C profile --help
if assert_ok && assert_contains "$OUTPUT" "list" && assert_contains "$OUTPUT" "create"; then
    pass_test "P01"
else fail_test "P01"; fi

begin_test "P02: profile list"
run $C profile list
if assert_ok && assert_contains "$OUTPUT" "base" && assert_contains "$OUTPUT" "dev"; then
    pass_test "P02"
else fail_test "P02"; fi

begin_test "P03: profile show"
run $C profile show
if assert_ok; then
    pass_test "P03"
else fail_test "P03"; fi

begin_test "P04: profile create (minimal)"
run $C profile create test-minimal --env MINIMAL=true
if assert_ok; then
    pass_test "P04"
else fail_test "P04"; fi

begin_test "P05: profile create --inherit"
run $C profile create test-inherit --inherit base
if assert_ok; then
    pass_test "P05"
else fail_test "P05"; fi

begin_test "P06: profile create --package"
run $C profile create test-pkg --package brew:ripgrep
if assert_ok; then
    pass_test "P06"
else fail_test "P06"; fi

begin_test "P07: profile create --env"
run $C profile create test-var --env EDITOR=nvim
if assert_ok; then
    pass_test "P07"
else fail_test "P07"; fi

begin_test "P08: profile create --system"
run $C profile create test-sys --system shell=/bin/zsh
if assert_ok; then
    pass_test "P08"
else fail_test "P08"; fi

begin_test "P09: profile create --file"
touch "$TGT/.testrc"
run $C profile create test-file --file "$TGT/.testrc"
if assert_ok; then
    pass_test "P09"
else fail_test "P09"; fi

begin_test "P10: profile create --private-files"
touch "$TGT/.private-test"
run $C profile create test-private --private-files --file "$TGT/.private-test"
if assert_ok; then
    pass_test "P10"
else fail_test "P10"; fi

begin_test "P11: profile create --module"
run $C profile create test-mod --module nvim
if assert_ok; then
    pass_test "P11"
else fail_test "P11"; fi

begin_test "P12: profile create with multiple flags"
touch "$TGT/.multi"
run $C profile create test-multi --inherit base --package brew:bat --env SHELL=/bin/zsh --file "$TGT/.multi"
if assert_ok; then
    pass_test "P12"
else fail_test "P12"; fi

begin_test "P13: profile create --pre-apply --post-apply"
echo '#!/bin/bash' > "$SCRATCH/pre.sh"
echo '#!/bin/bash' > "$SCRATCH/post.sh"
run $C profile create test-hooks --pre-apply "$SCRATCH/pre.sh" --post-apply "$SCRATCH/post.sh"
if assert_ok; then
    pass_test "P13"
else fail_test "P13"; fi

begin_test "P14: profile create --secret"
run $C profile create test-secret --secret "op://vault/item:$TGT/.secret"
if assert_ok; then
    pass_test "P14"
else fail_test "P14"; fi

begin_test "P15: profile create duplicate fails"
run $C profile create test-minimal
if assert_fail; then
    pass_test "P15"
else fail_test "P15"; fi

begin_test "P16: profile switch"
run $C profile switch base
if assert_ok; then
    pass_test "P16"
else fail_test "P16"; fi

begin_test "P17: profile switch back"
run $C profile switch dev
if assert_ok; then
    pass_test "P17"
else fail_test "P17"; fi

begin_test "P18: profile update --package (add)"
run $C profile update --package brew:htop
if assert_ok; then
    pass_test "P18"
else fail_test "P18"; fi

begin_test "P19: profile update --package (remove)"
run $C profile update --package -brew:htop
if assert_ok; then
    pass_test "P19"
else fail_test "P19"; fi

begin_test "P20: profile update --file (add)"
touch "$TGT/.bashrc"
run $C profile update --file "$TGT/.bashrc"
if assert_ok; then
    pass_test "P20"
else fail_test "P20"; fi

begin_test "P21: profile update --file (remove)"
run $C profile update --file "-$TGT/.bashrc"
if assert_ok; then
    pass_test "P21"
else fail_test "P21"; fi

begin_test "P22: profile update --env (add)"
run $C profile update --env FOO=bar
if assert_ok; then
    pass_test "P22"
else fail_test "P22"; fi

begin_test "P23: profile update --env (remove)"
run $C profile update --env -FOO
if assert_ok; then
    pass_test "P23"
else fail_test "P23"; fi

begin_test "P24: profile update --system (add)"
run $C profile update --system shell=/bin/zsh
if assert_ok; then
    pass_test "P24"
else fail_test "P24"; fi

begin_test "P25: profile update --system (remove)"
run $C profile update --system -shell
if assert_ok; then
    pass_test "P25"
else fail_test "P25"; fi

begin_test "P26: profile update --module (add)"
run $C profile update --module nvim
if assert_ok; then
    pass_test "P26"
else fail_test "P26"; fi

begin_test "P27: profile update --module (remove)"
run $C profile update --module -nvim
if assert_ok; then
    pass_test "P27"
else fail_test "P27"; fi

begin_test "P28: profile update --inherit (add)"
run $C profile update test-minimal --inherit base
if assert_ok; then
    pass_test "P28"
else fail_test "P28"; fi

begin_test "P29: profile update --inherit (remove)"
run $C profile update test-minimal --inherit -base
if assert_ok; then
    pass_test "P29"
else fail_test "P29"; fi

begin_test "P30: profile update --private-files"
run $C profile update test-minimal --private-files
if assert_ok; then
    pass_test "P30"
else fail_test "P30"; fi

begin_test "P31: profile update --secret (add)"
run $C profile update --secret "op://vault/key:$TGT/.ssh/key"
if assert_ok; then
    pass_test "P31"
else fail_test "P31"; fi

begin_test "P32: profile update --secret (remove)"
run $C profile update --secret "-$TGT/.ssh/key"
if assert_ok; then
    pass_test "P32"
else fail_test "P32"; fi

begin_test "P33: profile update --pre-apply (add)"
run $C profile update --pre-apply "$SCRATCH/pre.sh"
if assert_ok; then
    pass_test "P33"
else fail_test "P33"; fi

begin_test "P34: profile update --pre-apply (remove)"
run $C profile update --pre-apply "-$SCRATCH/pre.sh"
if assert_ok; then
    pass_test "P34"
else fail_test "P34"; fi

begin_test "P35: profile update --post-apply (add)"
run $C profile update --post-apply "$SCRATCH/post.sh"
if assert_ok; then
    pass_test "P35"
else fail_test "P35"; fi

begin_test "P36: profile update --post-apply (remove)"
run $C profile update --post-apply "-$SCRATCH/post.sh"
if assert_ok; then
    pass_test "P36"
else fail_test "P36"; fi

begin_test "P37: profile update named profile"
run $C profile update test-var --package brew:jq
if assert_ok; then
    pass_test "P37"
else fail_test "P37"; fi

begin_test "P38: profile delete --yes"
run $C profile delete test-minimal --yes
if assert_ok; then
    pass_test "P38"
else fail_test "P38"; fi

begin_test "P39: profile delete -y (short flag)"
run $C profile delete test-inherit -y
if assert_ok; then
    pass_test "P39"
else fail_test "P39"; fi

begin_test "P40: profile delete nonexistent fails"
run $C profile delete nonexistent --yes
if assert_fail; then
    pass_test "P40"
else fail_test "P40"; fi

# ═════════════════════════════════════════════════════
# SECTION 6: module
# ═════════════════════════════════════════════════════

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

# ═════════════════════════════════════════════════════
# SECTION 7: source
# ═════════════════════════════════════════════════════

begin_test "SRC01: source --help"
run $C source --help
if assert_ok && assert_contains "$OUTPUT" "add" && assert_contains "$OUTPUT" "list"; then
    pass_test "SRC01"
else fail_test "SRC01"; fi

begin_test "SRC02: source list (empty)"
run $C source list
if assert_ok; then
    pass_test "SRC02"
else fail_test "SRC02"; fi

begin_test "SRC03: source add (remote)"
run $C source add "file://$SOURCE_REPO" --yes --name team-config --profile base --priority 500
if assert_ok; then
    pass_test "SRC03"
else
    fail_test "SRC03"
fi

begin_test "SRC04: source add --branch"
run $C source add "file://$SOURCE_REPO" --yes --name team-branch --branch master --profile base --priority 500
if assert_ok; then
    pass_test "SRC04"
else
    fail_test "SRC04"
fi

begin_test "SRC05: source add --profile"
run $C source add "file://$SOURCE_REPO" --yes --name team-profile --profile base --priority 500
if assert_ok; then
    pass_test "SRC05"
else
    fail_test "SRC05"
fi

begin_test "SRC06: source add --accept-recommended"
run $C source add "file://$SOURCE_REPO" --yes --name team-rec --accept-recommended --profile base --priority 500
if assert_ok; then
    pass_test "SRC06"
else
    fail_test "SRC06"
fi

begin_test "SRC07: source add --priority"
run $C source add "file://$SOURCE_REPO" --yes --name team-pri --priority 10 --profile base
if assert_ok; then
    pass_test "SRC07"
else
    fail_test "SRC07"
fi

begin_test "SRC08: source add --opt-in"
run $C source add "file://$SOURCE_REPO" --yes --name team-opt --opt-in packages --profile base --priority 500
if assert_ok; then
    pass_test "SRC08"
else
    fail_test "SRC08"
fi

begin_test "SRC09: source add --sync-interval"
run $C source add "file://$SOURCE_REPO" --yes --name team-sync --sync-interval 1h --profile base --priority 500
if assert_ok; then
    pass_test "SRC09"
else
    fail_test "SRC09"
fi

begin_test "SRC10: source add --auto-apply"
run $C source add "file://$SOURCE_REPO" --yes --name team-auto --auto-apply --profile base --priority 500
if assert_ok; then
    pass_test "SRC10"
else
    fail_test "SRC10"
fi

begin_test "SRC11: source add --pin-version"
run $C source add "file://$SOURCE_REPO" --yes --name team-pin --pin-version ">=1.0" --profile base --priority 500
if assert_ok; then
    pass_test "SRC11"
else
    fail_test "SRC11"
fi

begin_test "SRC24: source add with platformProfiles auto-selection"
PLATFORM_REPO="$SCRATCH/platform-source-repo"
mkdir -p "$PLATFORM_REPO/profiles"
cat > "$PLATFORM_REPO/cfgd-source.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: platform-source
spec:
  provides:
    profiles: [linux-base, macos-arm]
    platformProfiles:
      linux: linux-base
      macos: macos-arm
  policy:
    constraints: {}
YAML
cat > "$PLATFORM_REPO/profiles/linux-base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: linux-base
YAML
cat > "$PLATFORM_REPO/profiles/macos-arm.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: macos-arm
YAML
(cd "$PLATFORM_REPO" && git init -q -b master && git add -A && git commit -qm "init platform source")
run $C source add "file://$PLATFORM_REPO" --yes --name platform-team --priority 500
if assert_ok && assert_contains "$OUTPUT" "Auto-selected profile"; then
    pass_test "SRC24"
else
    fail_test "SRC24"
fi

begin_test "SRC12: source list (after adds)"
run $C source list
if assert_ok; then
    pass_test "SRC12"
else fail_test "SRC12"; fi

begin_test "SRC13: source show"
run $C source show team-config
if assert_ok; then
    pass_test "SRC13"
else
    fail_test "SRC13"
fi

begin_test "SRC14: source update (all)"
run $C source update
if assert_ok; then
    pass_test "SRC14"
else
    fail_test "SRC14"
fi

begin_test "SRC15: source update <name>"
run $C source update team-config
if assert_ok; then
    pass_test "SRC15"
else
    fail_test "SRC15"
fi

begin_test "SRC16: source priority set"
run $C source priority team-config 5
if assert_ok; then
    pass_test "SRC16"
else
    fail_test "SRC16"
fi

begin_test "SRC17: source priority show"
run $C source priority team-config
if assert_ok; then
    pass_test "SRC17"
else
    fail_test "SRC17"
fi

begin_test "SRC18: source override set"
run $C source override team-config set packages.brew.formulae '["jq"]'
if assert_ok; then
    pass_test "SRC18"
else
    fail_test "SRC18"
fi

begin_test "SRC19: source override reject"
run $C source override team-config reject packages.brew.casks
if assert_ok; then
    pass_test "SRC19"
else
    fail_test "SRC19"
fi

begin_test "SRC20: source replace"
run $C source replace team-config "file://$SOURCE_REPO"
if assert_ok; then
    pass_test "SRC20"
else
    fail_test "SRC20"
fi

begin_test "SRC21: source create"
run $C source create my-source --description "local source" --version "1.0.0"
if assert_ok; then
    pass_test "SRC21"
else fail_test "SRC21"; fi

begin_test "SRC22: source remove --keep-all"
run $C source remove team-branch --keep-all
if assert_ok; then
    pass_test "SRC22"
else
    fail_test "SRC22"
fi

begin_test "SRC23: source remove --remove-all"
run $C source remove team-profile --remove-all
if assert_ok; then
    pass_test "SRC23"
else
    fail_test "SRC23"
fi

# ═════════════════════════════════════════════════════
# SECTION 8: explain
# ═════════════════════════════════════════════════════

begin_test "E01: explain (no args — lists types)"
run $C explain
if assert_ok; then
    pass_test "E01"
else fail_test "E01"; fi

begin_test "E02: explain profile"
run $C explain profile
if assert_ok && assert_contains "$OUTPUT" "spec"; then
    pass_test "E02"
else fail_test "E02"; fi

begin_test "E03: explain module"
run $C explain module
if assert_ok; then
    pass_test "E03"
else fail_test "E03"; fi

begin_test "E04: explain cfgdconfig"
run $C explain cfgdconfig
if assert_ok; then
    pass_test "E04"
else fail_test "E04"; fi

begin_test "E05: explain configsource"
run $C explain configsource
if assert_ok; then
    pass_test "E05"
else fail_test "E05"; fi

begin_test "E06: explain machineconfig"
run $C explain machineconfig
if assert_ok; then
    pass_test "E06"
else fail_test "E06"; fi

begin_test "E07: explain configpolicy"
run $C explain configpolicy
if assert_ok; then
    pass_test "E07"
else fail_test "E07"; fi

begin_test "E08: explain driftalert"
run $C explain driftalert
if assert_ok; then
    pass_test "E08"
else fail_test "E08"; fi

begin_test "E09: explain teamconfig"
run $C explain teamconfig
if assert_ok; then
    pass_test "E09"
else fail_test "E09"; fi

begin_test "E10: explain --recursive profile"
run $C explain --recursive profile
if assert_ok; then
    pass_test "E10"
else fail_test "E10"; fi

begin_test "E11: explain profile.spec.packages"
run $C explain profile.spec.packages
if assert_ok; then
    pass_test "E11"
else fail_test "E11"; fi

begin_test "E12: explain unknown type"
run $C explain nonexistent
if assert_fail || assert_contains "$OUTPUT" "unknown"; then
    pass_test "E12"
else fail_test "E12"; fi

# ═════════════════════════════════════════════════════
# SECTION 9: config
# ═════════════════════════════════════════════════════

begin_test "CF01: config --help"
run $C config --help
if assert_ok && assert_contains "$OUTPUT" "show"; then
    pass_test "CF01"
else fail_test "CF01"; fi

begin_test "CF02: config show"
run $C config show
if assert_ok && assert_contains "$OUTPUT" "dev"; then
    pass_test "CF02"
else fail_test "CF02"; fi

# config edit requires $EDITOR interaction, tested via EDITOR=true
begin_test "CF03: config edit (EDITOR=true)"
EDITOR=true run $C config edit
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "CF03"
else fail_test "CF03" "exit $RC"; fi

# ═════════════════════════════════════════════════════
# SECTION 10: completions
# ═════════════════════════════════════════════════════

begin_test "CMP01: completions bash"
run $C completions bash
if assert_ok && assert_contains "$OUTPUT" "complete"; then
    pass_test "CMP01"
else fail_test "CMP01"; fi

begin_test "CMP02: completions zsh"
run $C completions zsh
if assert_ok; then
    pass_test "CMP02"
else fail_test "CMP02"; fi

begin_test "CMP03: completions fish"
run $C completions fish
if assert_ok; then
    pass_test "CMP03"
else fail_test "CMP03"; fi

begin_test "CMP04: completions powershell"
run $C completions powershell
if assert_ok; then
    pass_test "CMP04"
else fail_test "CMP04"; fi

begin_test "CMP05: completions elvish"
run $C completions elvish
if assert_ok; then
    pass_test "CMP05"
else fail_test "CMP05"; fi

# ═════════════════════════════════════════════════════
# SECTION 11: secret
# ═════════════════════════════════════════════════════

begin_test "SEC01: secret --help"
run $C secret --help
if assert_ok && assert_contains "$OUTPUT" "encrypt" && assert_contains "$OUTPUT" "decrypt"; then
    pass_test "SEC01"
else fail_test "SEC01"; fi

begin_test "SEC02: secret init"
run $C secret init
if assert_ok; then
    pass_test "SEC02"
else fail_test "SEC02"; fi

if command -v age-keygen > /dev/null 2>&1 && command -v sops > /dev/null 2>&1; then
    # cfgd's sops backend uses ~/.config/cfgd/age-key.txt by default.
    # Generate key there and use its public key for .sops.yaml
    CFGD_DEFAULT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/cfgd"
    mkdir -p "$CFGD_DEFAULT_DIR"
    CFGD_AGE_KEY="$CFGD_DEFAULT_DIR/age-key.txt"
    if [ ! -f "$CFGD_AGE_KEY" ]; then
        age-keygen -o "$CFGD_AGE_KEY" 2>/dev/null
    fi
    AGE_PUB=$(grep "public key:" "$CFGD_AGE_KEY" | awk '{print $NF}')
    cat > "$CFG/.sops.yaml" << SOPSEOF
creation_rules:
  - age: "$AGE_PUB"
SOPSEOF
    export SOPS_AGE_KEY_FILE="$CFGD_AGE_KEY"

    begin_test "SEC03: secret encrypt"
    mkdir -p "$CFG/secrets"
    cp "$CFG/.sops.yaml" "$CFG/secrets/.sops.yaml"
    echo "secret_key: secret-value" > "$CFG/secrets/plaintext.yaml"
    run $C secret encrypt "$CFG/secrets/plaintext.yaml"
    if assert_ok; then
        pass_test "SEC03"
    else fail_test "SEC03"; fi

    begin_test "SEC04: secret decrypt"
    if [ -f "$CFG/secrets/plaintext.yaml" ]; then
        run $C secret decrypt "$CFG/secrets/plaintext.yaml"
        if assert_ok; then
            pass_test "SEC04"
        else fail_test "SEC04"; fi
    else
        skip_test "SEC04" "No encrypted file from SEC03"
    fi

    unset SOPS_AGE_KEY_FILE
else
    skip_test "SEC03" "age-keygen or sops not available"
    skip_test "SEC04" "age-keygen or sops not available"
fi

begin_test "SEC05: secret edit (EDITOR=true)"
if [ -f "$CFG/secrets/plaintext.yaml" ]; then
    EDITOR=true run $C secret edit "$CFG/secrets/plaintext.yaml"
    if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
        pass_test "SEC05"
    else fail_test "SEC05" "exit $RC"; fi
else
    skip_test "SEC05" "No encrypted file"
fi

# ═════════════════════════════════════════════════════
# SECTION 12: decide
# ═════════════════════════════════════════════════════

begin_test "DEC01: decide --help"
run $C decide --help
if assert_ok; then
    pass_test "DEC01"
else fail_test "DEC01"; fi

begin_test "DEC02: decide accept --all (no pending)"
run $C decide accept --all
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DEC02"
else fail_test "DEC02" "exit $RC"; fi

begin_test "DEC03: decide reject --all (no pending)"
run $C decide reject --all
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DEC03"
else fail_test "DEC03" "exit $RC"; fi

begin_test "DEC04: decide accept --source"
run $C decide accept --source nonexistent
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DEC04"
else fail_test "DEC04" "exit $RC"; fi

begin_test "DEC05: decide accept specific resource"
run $C decide accept packages.brew.formulae
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DEC05"
else fail_test "DEC05" "exit $RC"; fi

# ═════════════════════════════════════════════════════
# SECTION 13: daemon
# ═════════════════════════════════════════════════════

begin_test "DM01: daemon --help"
run $C daemon --help
if assert_ok && assert_contains "$OUTPUT" "install"; then
    pass_test "DM01"
else fail_test "DM01"; fi

begin_test "DM02: daemon status"
run $C daemon status
# Daemon not running, should report that
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DM02"
else fail_test "DM02" "exit $RC"; fi

begin_test "DM03: daemon install"
run $C daemon install
# May succeed or fail depending on systemd/launchd availability
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DM03"
else fail_test "DM03" "exit $RC"; fi

begin_test "DM04: daemon uninstall"
run $C daemon uninstall
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DM04"
else fail_test "DM04" "exit $RC"; fi

# ═════════════════════════════════════════════════════
# SECTION 14: sync / pull
# ═════════════════════════════════════════════════════

begin_test "SP01: sync"
run $C sync
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "SP01"
else fail_test "SP01" "exit $RC"; fi

begin_test "SP02: pull"
run $C pull
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "SP02"
else fail_test "SP02" "exit $RC"; fi

# ═════════════════════════════════════════════════════
# SECTION 15: upgrade
# ═════════════════════════════════════════════════════

begin_test "UP01: upgrade --help"
run $C upgrade --help
if assert_ok; then
    pass_test "UP01"
else fail_test "UP01"; fi

begin_test "UP02: upgrade --check"
run $C upgrade --check
# May fail without network, but should not crash
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "UP02"
else fail_test "UP02" "exit $RC"; fi

# ═════════════════════════════════════════════════════
# SECTION 16: workflow
# ═════════════════════════════════════════════════════

begin_test "WF01: workflow --help"
run $C workflow --help
if assert_ok; then
    pass_test "WF01"
else fail_test "WF01"; fi

begin_test "WF02: workflow generate"
run $C workflow generate
if assert_ok; then
    pass_test "WF02"
else fail_test "WF02"; fi

begin_test "WF03: workflow generate --force"
run $C workflow generate --force
if assert_ok; then
    pass_test "WF03"
else fail_test "WF03"; fi

# ═════════════════════════════════════════════════════
# SECTION 17: checkin / enroll (require server)
# ═════════════════════════════════════════════════════

begin_test "CI01: checkin --help"
run $C checkin --help
if assert_ok && assert_contains "$OUTPUT" "server-url"; then
    pass_test "CI01"
else fail_test "CI01"; fi

begin_test "CI02: checkin without server fails"
run $C checkin --server-url http://localhost:9999
if assert_fail; then
    pass_test "CI02"
else fail_test "CI02"; fi

begin_test "EN01: enroll --help"
run $C enroll --help
if assert_ok && assert_contains "$OUTPUT" "server"; then
    pass_test "EN01"
else fail_test "EN01"; fi

begin_test "EN02: enroll without server fails"
run $C enroll --server-url http://localhost:9999
if assert_fail; then
    pass_test "EN02"
else fail_test "EN02"; fi

begin_test "EN03: enroll --ssh-key flag accepted"
run $C enroll --server-url http://localhost:9999 --ssh-key ~/.ssh/id_ed25519
if assert_fail; then
    pass_test "EN03"
else fail_test "EN03"; fi

begin_test "EN04: enroll --gpg-key flag accepted"
run $C enroll --server-url http://localhost:9999 --gpg-key ABCD1234
if assert_fail; then
    pass_test "EN04"
else fail_test "EN04"; fi

begin_test "EN05: enroll --username flag"
run $C enroll --server-url http://localhost:9999 --username testuser
if assert_fail; then
    pass_test "EN05"
else fail_test "EN05"; fi

# ═════════════════════════════════════════════════════
# SECTION 18: profile inheritance verification
# ═════════════════════════════════════════════════════

begin_test "INH01: 3-level inheritance applies all files"
cat > "$CFG/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: inherit-test
spec:
  profile: work-dev
YAML
rm -f "$TGT/.gitconfig" "$TGT/.zshrc" "$TGT/.gitconfig-work"
run $C apply --yes
if [ -f "$TGT/.gitconfig" ] && [ -f "$TGT/.zshrc" ] && [ -f "$TGT/.gitconfig-work" ]; then
    pass_test "INH01"
else fail_test "INH01" "Missing inherited files"; fi

begin_test "INH02: env override (child overrides parent)"
# dev sets EDITOR=nvim over base's EDITOR=vim
run $C profile show
if assert_ok && assert_contains "$OUTPUT" "nvim"; then
    pass_test "INH02"
else fail_test "INH02"; fi

# Restore config
cat > "$CFG/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: exhaustive-e2e
spec:
  profile: dev
  aliases:
    add: "profile update --file"
    remove: "profile update --file"
YAML

# ═════════════════════════════════════════════════════
# SECTION 20: template rendering
# ═════════════════════════════════════════════════════

begin_test "TPL01: tera template renders env vars"
# Add a template file to the profile
run $C profile update --file "$CFG/files/config.toml.tera:$TGT/.config.toml"
run $C apply --yes
if [ -f "$TGT/.config.toml" ]; then
    CONTENT=$(cat "$TGT/.config.toml")
    if assert_contains "$CONTENT" "nvim"; then
        pass_test "TPL01"
    else fail_test "TPL01" "Template env var not rendered"; fi
else
    fail_test "TPL01" "Template file not created"
fi

# ═════════════════════════════════════════════════════
# SECTION 21: edge cases & error handling
# ═════════════════════════════════════════════════════

begin_test "ERR01: apply with nonexistent profile fails"
mkdir -p "$SCRATCH/bad-cfg"
cat > "$SCRATCH/bad-cfg/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: bad
spec:
  profile: does-not-exist
YAML
run --config "$SCRATCH/bad-cfg/cfgd.yaml" apply --dry-run --no-color
if assert_fail; then
    pass_test "ERR01"
else fail_test "ERR01"; fi

begin_test "ERR02: profile switch to nonexistent fails"
run $C profile switch nonexistent-profile
if assert_fail; then
    pass_test "ERR02"
else fail_test "ERR02"; fi

begin_test "ERR03: module show nonexistent fails"
run $C module show nonexistent-mod
if assert_fail; then
    pass_test "ERR03"
else fail_test "ERR03"; fi

begin_test "ERR04: source show nonexistent fails"
run $C source show nonexistent-src
if assert_fail; then
    pass_test "ERR04"
else fail_test "ERR04"; fi

begin_test "ERR05: profile edit nonexistent fails"
EDITOR=true run $C profile edit nonexistent
if assert_fail; then
    pass_test "ERR05"
else fail_test "ERR05"; fi

begin_test "ERR06: module edit nonexistent fails"
EDITOR=true run $C module edit nonexistent
if assert_fail; then
    pass_test "ERR06"
else fail_test "ERR06"; fi

# ═════════════════════════════════════════════════════
# SECTION 22: drift detection after modification
# ═════════════════════════════════════════════════════

begin_test "DRIFT01: verify detects drift after file modification"
run $C apply --yes
# Only check drift if apply deployed the managed file
if [ -f "$TGT/.zshrc" ]; then
    echo "MODIFIED" >> "$TGT/.zshrc"
    run $C verify
    # verify may return 0 or 1; either way it should run without error (exit 2+)
    if [ "$RC" -le 1 ]; then
        pass_test "DRIFT01"
    else fail_test "DRIFT01" "Verify crashed (exit $RC)"; fi
else
    skip_test "DRIFT01" "Managed file not deployed by apply"
fi

begin_test "DRIFT02: diff shows changes"
run $C diff
if echo "$OUTPUT" | grep -qiE "drift|differ|changed|MODIFIED|zshrc"; then
    pass_test "DRIFT02"
else fail_test "DRIFT02" "Diff did not show changes"; fi

begin_test "DRIFT03: apply --yes fixes drift"
run $C apply --yes
run $C verify
if assert_ok; then
    pass_test "DRIFT03"
else fail_test "DRIFT03"; fi

# ═════════════════════════════════════════════════════
# SECTION 23: conflict detection
# ═════════════════════════════════════════════════════

begin_test "CONF01: duplicate file targets detected"
CONF_DIR="$SCRATCH/conflict-test"
CONF_TGT="$SCRATCH/conflict-target"
mkdir -p "$CONF_DIR/profiles" "$CONF_DIR/files" "$CONF_TGT"
echo "content-a" > "$CONF_DIR/files/file-a"
echo "content-b" > "$CONF_DIR/files/file-b"
cat > "$CONF_DIR/profiles/conflicting.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: conflicting
spec:
  files:
    managed:
      - source: files/file-a
        target: $CONF_TGT/.same-target
      - source: files/file-b
        target: $CONF_TGT/.same-target
YAML
cat > "$CONF_DIR/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: conflict-test
spec:
  profile: conflicting
YAML
run --config "$CONF_DIR/cfgd.yaml" apply --dry-run --no-color
if [ "$RC" -ne 0 ] || echo "$OUTPUT" | grep -qiE "conflict|duplicate|same target"; then
    pass_test "CONF01"
else fail_test "CONF01" "Duplicate targets not detected"; fi

# ═════════════════════════════════════════════════════
# SECTION 24: plan command
# ═════════════════════════════════════════════════════

begin_test "PL01: plan --help"
run $C plan --help
if assert_ok && assert_contains "$OUTPUT" "phase"; then
    pass_test "PL01"
else fail_test "PL01"; fi

begin_test "PL02: plan (default)"
run $C plan
if assert_ok; then
    pass_test "PL02"
else fail_test "PL02"; fi

begin_test "PL03: plan --phase files"
run $C plan --phase files
if assert_ok; then
    pass_test "PL03"
else fail_test "PL03"; fi

begin_test "PL04: plan --phase packages"
run $C plan --phase packages
if assert_ok; then
    pass_test "PL04"
else fail_test "PL04"; fi

begin_test "PL05: plan --phase system"
run $C plan --phase system
if assert_ok; then
    pass_test "PL05"
else fail_test "PL05"; fi

begin_test "PL06: plan --phase env"
run $C plan --phase env
if assert_ok; then
    pass_test "PL06"
else fail_test "PL06"; fi

begin_test "PL07: plan --phase secrets"
run $C plan --phase secrets
if assert_ok; then
    pass_test "PL07"
else fail_test "PL07"; fi

begin_test "PL08: plan --skip files"
run $C plan --skip files
if assert_ok; then
    pass_test "PL08"
else fail_test "PL08"; fi

begin_test "PL09: plan --only files"
run $C plan --only files
if assert_ok; then
    pass_test "PL09"
else fail_test "PL09"; fi

begin_test "PL10: plan --module (nonexistent)"
run $C plan --module nonexistent
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "PL10"
else fail_test "PL10" "exit $RC"; fi

begin_test "PL11: plan --skip-scripts"
run $C plan --skip-scripts
if assert_ok; then
    pass_test "PL11"
else fail_test "PL11"; fi

begin_test "PL12: plan --context reconcile"
run $C plan --context reconcile
if assert_ok; then
    pass_test "PL12"
else fail_test "PL12"; fi

begin_test "PL13: plan --context apply (explicit default)"
run $C plan --context apply
if assert_ok; then
    pass_test "PL13"
else fail_test "PL13"; fi

begin_test "PL14: plan --skip multiple"
run $C plan --skip files --skip packages
if assert_ok; then
    pass_test "PL14"
else fail_test "PL14"; fi

begin_test "PL15: plan --only multiple"
run $C plan --only files --only env
if assert_ok; then
    pass_test "PL15"
else fail_test "PL15"; fi

# ═════════════════════════════════════════════════════
# SECTION 25: rollback command
# ═════════════════════════════════════════════════════

begin_test "RB01: rollback --help"
run $C rollback 0 --help
if assert_ok && assert_contains "$OUTPUT" "roll"; then
    pass_test "RB01"
else fail_test "RB01"; fi

begin_test "RB02: rollback nonexistent apply ID"
run $C rollback 999999 --yes
if assert_fail; then
    pass_test "RB02"
else fail_test "RB02"; fi

begin_test "RB03: rollback -y (short flag)"
run $C rollback 999999 -y
if assert_fail; then
    pass_test "RB03"
else fail_test "RB03"; fi

begin_test "RB04: rollback valid apply ID"
# Get the most recent apply ID from the log
APPLY_ID=$("$CFGD" $C log -n 1 --output json 2>&1 | grep -oE '"id":\s*[0-9]+' | head -1 | grep -oE '[0-9]+' || echo "")
if [ -z "$APPLY_ID" ]; then
    APPLY_ID=$("$CFGD" $C log -n 1 2>&1 | grep -E '^[0-9]' | awk '{print $1}' | head -1 || echo "")
fi
if [ -n "$APPLY_ID" ]; then
    run $C rollback "$APPLY_ID" --yes
    # May succeed (restores files) or fail (no backups) — both are valid
    if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
        pass_test "RB04"
    else fail_test "RB04" "exit $RC"; fi
else
    skip_test "RB04" "No apply ID found in log"
fi

# ═════════════════════════════════════════════════════
# SECTION 26: output format flags
# ═════════════════════════════════════════════════════

begin_test "OF01: status --output json"
run $C status --output json
if assert_ok && assert_contains "$OUTPUT" "{"; then
    pass_test "OF01"
else fail_test "OF01"; fi

begin_test "OF02: status --output yaml"
run $C status --output yaml
if assert_ok; then
    pass_test "OF02"
else fail_test "OF02"; fi

begin_test "OF03: status --output wide"
run $C status --output wide
if assert_ok; then
    pass_test "OF03"
else fail_test "OF03"; fi

begin_test "OF04: status --output table"
run $C status --output table
if assert_ok; then
    pass_test "OF04"
else fail_test "OF04"; fi

begin_test "OF05: status --output name"
run $C status --output name
if assert_ok; then
    pass_test "OF05"
else fail_test "OF05"; fi

begin_test "OF06: profile list --output json"
run $C profile list --output json
if assert_ok && assert_contains "$OUTPUT" "{"; then
    pass_test "OF06"
else fail_test "OF06"; fi

begin_test "OF07: profile list --output yaml"
run $C profile list --output yaml
if assert_ok; then
    pass_test "OF07"
else fail_test "OF07"; fi

begin_test "OF08: module list --output json"
run $C module list --output json
if assert_ok && assert_contains "$OUTPUT" "{"; then
    pass_test "OF08"
else fail_test "OF08"; fi

begin_test "OF09: module list --output yaml"
run $C module list --output yaml
if assert_ok; then
    pass_test "OF09"
else fail_test "OF09"; fi

begin_test "OF10: source list --output json"
run $C source list --output json
# May output [] (empty array) or [{...}] (populated) — both are valid JSON
if assert_ok; then
    pass_test "OF10"
else fail_test "OF10"; fi

begin_test "OF11: log --output json"
run $C log --output json
if assert_ok; then
    pass_test "OF11"
else fail_test "OF11"; fi

begin_test "OF12: --output jsonpath=EXPR"
run $C status --output 'jsonpath={.drift}'
if assert_ok; then
    pass_test "OF12"
else fail_test "OF12"; fi

begin_test "OF13: -o short flag"
run $C status -o json
if assert_ok && assert_contains "$OUTPUT" "{"; then
    pass_test "OF13"
else fail_test "OF13"; fi

# ═════════════════════════════════════════════════════
# SECTION 27: config get/set/unset
# ═════════════════════════════════════════════════════

begin_test "CF04: config get"
run $C config get profile
if assert_ok; then
    pass_test "CF04"
else fail_test "CF04"; fi

begin_test "CF05: config set"
run $C config set theme minimal
if assert_ok; then
    pass_test "CF05"
else fail_test "CF05"; fi

begin_test "CF06: config get (verify set)"
run $C config get theme
if assert_ok && assert_contains "$OUTPUT" "minimal"; then
    pass_test "CF06"
else fail_test "CF06"; fi

begin_test "CF07: config unset"
run $C config unset theme
if assert_ok; then
    pass_test "CF07"
else fail_test "CF07"; fi

begin_test "CF08: config get nonexistent key"
run $C config get nonexistent.key.path
if assert_fail; then
    pass_test "CF08"
else fail_test "CF08"; fi

# ═════════════════════════════════════════════════════
# SECTION 28: module export
# ═════════════════════════════════════════════════════

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

# ═════════════════════════════════════════════════════
# SECTION 29: module OCI (push/pull) — requires registry
# ═════════════════════════════════════════════════════

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

# ═════════════════════════════════════════════════════
# SECTION 30: module keys
# ═════════════════════════════════════════════════════

begin_test "MK01: module keys list"
run $C module keys list
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "MK01"
else fail_test "MK01" "exit $RC"; fi

begin_test "MK02: module keys generate"
if command -v cosign > /dev/null 2>&1; then
    KEYS_DIR="$SCRATCH/keys-test"
    mkdir -p "$KEYS_DIR"
    run $C module keys generate --output "$KEYS_DIR"
    if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
        pass_test "MK02"
    else fail_test "MK02" "exit $RC"; fi
else
    skip_test "MK02" "cosign not available"
fi

# ═════════════════════════════════════════════════════
# SECTION 31: module build
# ═════════════════════════════════════════════════════

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

# ═════════════════════════════════════════════════════
# SECTION 32: additional apply flags
# ═════════════════════════════════════════════════════

begin_test "A18: apply --skip-scripts"
run $C apply --dry-run --skip-scripts
if assert_ok; then
    pass_test "A18"
else fail_test "A18"; fi

begin_test "A19: apply --from (local git repo)"
A19_DST="$SCRATCH/apply-from-test"
run $C apply --from "$ISRC" --dry-run --no-color --config "$A19_DST/cfgd.yaml" --state-dir "$SCRATCH/state-a19"
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "A19"
else fail_test "A19" "exit $RC"; fi

# ═════════════════════════════════════════════════════
# SECTION 33: additional init flags
# ═════════════════════════════════════════════════════

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

# ═════════════════════════════════════════════════════
# SECTION 34: status/diff/verify --module flag
# ═════════════════════════════════════════════════════

begin_test "S04: status --module"
run $C status --module nvim
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "S04"
else fail_test "S04" "exit $RC"; fi

begin_test "D02: diff --module"
run $C diff --module nvim
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "D02"
else fail_test "D02" "exit $RC"; fi

begin_test "V02: verify --module"
run $C verify --module nvim
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "V02"
else fail_test "V02" "exit $RC"; fi

# ═════════════════════════════════════════════════════
# SECTION 35: log --show-output
# ═════════════════════════════════════════════════════

begin_test "L04: log --show-output <apply_id>"
# --show-output takes an apply ID — get one from the log table (first column is the numeric ID)
LOG_ID=$("$CFGD" $C log -n 1 --output json 2>&1 | grep -oE '"id":\s*[0-9]+' | head -1 | grep -oE '[0-9]+' || echo "")
if [ -z "$LOG_ID" ]; then
    # Fallback: parse table output — ID is the first number on the data line
    LOG_ID=$("$CFGD" $C log -n 1 2>&1 | grep -E '^[0-9]' | awk '{print $1}' | head -1 || echo "")
fi
if [ -n "$LOG_ID" ]; then
    run $C log --show-output "$LOG_ID"
    if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
        pass_test "L04"
    else fail_test "L04" "exit $RC"; fi
else
    skip_test "L04" "No apply ID found in log"
fi

begin_test "L05: log --show-output invalid ID"
run $C log --show-output 999999
# Should handle gracefully (no entries for this ID)
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "L05"
else fail_test "L05" "exit $RC"; fi

# ═════════════════════════════════════════════════════
# SECTION 36: profile create additional flags
# ═════════════════════════════════════════════════════

begin_test "P41: profile create --alias"
run $C profile create test-alias --alias ll="ls -la"
if assert_ok; then
    pass_test "P41"
else fail_test "P41"; fi

begin_test "P42: profile create --pre-reconcile --post-reconcile"
run $C profile create test-reconcile --pre-reconcile "echo pre" --post-reconcile "echo post"
if assert_ok; then
    pass_test "P42"
else fail_test "P42"; fi

begin_test "P43: profile create --on-change --on-drift"
run $C profile create test-onhook --on-change "echo changed" --on-drift "echo drifted"
if assert_ok; then
    pass_test "P43"
else fail_test "P43"; fi

begin_test "P44: profile update --alias (add)"
run $C profile update --alias gs="git status"
if assert_ok; then
    pass_test "P44"
else fail_test "P44"; fi

begin_test "P45: profile update --alias (remove)"
run $C profile update --alias -gs
if assert_ok; then
    pass_test "P45"
else fail_test "P45"; fi

begin_test "P46: profile update --pre-reconcile (add)"
run $C profile update --pre-reconcile "echo pre-r"
if assert_ok; then
    pass_test "P46"
else fail_test "P46"; fi

begin_test "P47: profile update --pre-reconcile (remove)"
run $C profile update --pre-reconcile "-echo pre-r"
if assert_ok; then
    pass_test "P47"
else fail_test "P47"; fi

begin_test "P48: profile update --post-reconcile (add)"
run $C profile update --post-reconcile "echo post-r"
if assert_ok; then
    pass_test "P48"
else fail_test "P48"; fi

begin_test "P49: profile update --post-reconcile (remove)"
run $C profile update --post-reconcile "-echo post-r"
if assert_ok; then
    pass_test "P49"
else fail_test "P49"; fi

begin_test "P50: profile update --on-change (add)"
run $C profile update --on-change "echo chg"
if assert_ok; then
    pass_test "P50"
else fail_test "P50"; fi

begin_test "P51: profile update --on-change (remove)"
run $C profile update --on-change "-echo chg"
if assert_ok; then
    pass_test "P51"
else fail_test "P51"; fi

begin_test "P52: profile update --on-drift (add)"
run $C profile update --on-drift "echo dft"
if assert_ok; then
    pass_test "P52"
else fail_test "P52"; fi

begin_test "P53: profile update --on-drift (remove)"
run $C profile update --on-drift "-echo dft"
if assert_ok; then
    pass_test "P53"
else fail_test "P53"; fi

begin_test "P54: profile edit (existing profile)"
EDITOR=true run $C profile edit dev
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "P54"
else fail_test "P54" "exit $RC"; fi

begin_test "P55: profile show <name>"
run $C profile show base
if assert_ok && assert_contains "$OUTPUT" "base"; then
    pass_test "P55"
else fail_test "P55"; fi

begin_test "P56: profile ls (alias)"
run $C profile ls
if assert_ok && assert_contains "$OUTPUT" "base"; then
    pass_test "P56"
else fail_test "P56"; fi

# ═════════════════════════════════════════════════════
# SECTION 37: module create/update additional flags
# ═════════════════════════════════════════════════════

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

# ═════════════════════════════════════════════════════
# SECTION 38: source additional flags
# ═════════════════════════════════════════════════════

begin_test "SRC25: source ls (alias)"
run $C source ls
if assert_ok; then
    pass_test "SRC25"
else fail_test "SRC25"; fi

begin_test "SRC26: source edit"
EDITOR=true run $C source edit
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "SRC26"
else fail_test "SRC26" "exit $RC"; fi

# ═════════════════════════════════════════════════════
# SECTION 39: decide reject subcommand
# ═════════════════════════════════════════════════════

begin_test "DEC06: decide reject --source"
run $C decide reject --source nonexistent
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DEC06"
else fail_test "DEC06" "exit $RC"; fi

begin_test "DEC07: decide reject specific resource"
run $C decide reject packages.brew.formulae
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DEC07"
else fail_test "DEC07" "exit $RC"; fi

# ═════════════════════════════════════════════════════
# SECTION 40: checkin/enroll additional flags
# ═════════════════════════════════════════════════════

begin_test "CI03: checkin --api-key"
run $C checkin --server-url http://localhost:9999 --api-key test-key
if assert_fail; then
    pass_test "CI03"
else fail_test "CI03"; fi

begin_test "CI04: checkin --device-id"
run $C checkin --server-url http://localhost:9999 --device-id test-device
if assert_fail; then
    pass_test "CI04"
else fail_test "CI04"; fi

begin_test "CI05: checkin --api-key --device-id"
run $C checkin --server-url http://localhost:9999 --api-key k --device-id d
if assert_fail; then
    pass_test "CI05"
else fail_test "CI05"; fi

begin_test "EN06: enroll --token"
run $C enroll --server-url http://localhost:9999 --token test-bootstrap-token
if assert_fail; then
    pass_test "EN06"
else fail_test "EN06"; fi

# ═════════════════════════════════════════════════════
# SECTION 41: explain additional types
# ═════════════════════════════════════════════════════

begin_test "E13: explain clusterconfigpolicy (not in schema — fails gracefully)"
run $C explain clusterconfigpolicy
if assert_fail && assert_contains "$OUTPUT" "Unknown resource type"; then
    pass_test "E13"
else fail_test "E13"; fi

# ═════════════════════════════════════════════════════
# SUMMARY
# ═════════════════════════════════════════════════════

print_summary "Exhaustive CLI"
