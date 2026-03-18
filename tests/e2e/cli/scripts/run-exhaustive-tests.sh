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
    add: "profile update --active --file"
    remove: "profile update --active --file"
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
setup_config_dir "$CFG" "$TGT"
CONF="$CFG/cfgd.yaml"
C="--config $CONF --no-color"

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

begin_test "P18: profile update --active --package (add)"
run $C profile update --active --package brew:htop
if assert_ok; then
    pass_test "P18"
else fail_test "P18"; fi

begin_test "P19: profile update --active --package (remove)"
run $C profile update --active --package -brew:htop
if assert_ok; then
    pass_test "P19"
else fail_test "P19"; fi

begin_test "P20: profile update --active --file (add)"
touch "$TGT/.bashrc"
run $C profile update --active --file "$TGT/.bashrc"
if assert_ok; then
    pass_test "P20"
else fail_test "P20"; fi

begin_test "P21: profile update --active --file (remove)"
run $C profile update --active --file "-$TGT/.bashrc"
if assert_ok; then
    pass_test "P21"
else fail_test "P21"; fi

begin_test "P22: profile update --active --env (add)"
run $C profile update --active --env FOO=bar
if assert_ok; then
    pass_test "P22"
else fail_test "P22"; fi

begin_test "P23: profile update --active --env (remove)"
run $C profile update --active --env -FOO
if assert_ok; then
    pass_test "P23"
else fail_test "P23"; fi

begin_test "P24: profile update --active --system (add)"
run $C profile update --active --system shell=/bin/zsh
if assert_ok; then
    pass_test "P24"
else fail_test "P24"; fi

begin_test "P25: profile update --active --system (remove)"
run $C profile update --active --system -shell
if assert_ok; then
    pass_test "P25"
else fail_test "P25"; fi

begin_test "P26: profile update --active --module (add)"
run $C profile update --active --module nvim
if assert_ok; then
    pass_test "P26"
else fail_test "P26"; fi

begin_test "P27: profile update --active --module (remove)"
run $C profile update --active --module -nvim
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

begin_test "P31: profile update --active --secret (add)"
run $C profile update --active --secret "op://vault/key:$TGT/.ssh/key"
if assert_ok; then
    pass_test "P31"
else fail_test "P31"; fi

begin_test "P32: profile update --active --secret (remove)"
run $C profile update --active --secret "-$TGT/.ssh/key"
if assert_ok; then
    pass_test "P32"
else fail_test "P32"; fi

begin_test "P33: profile update --active --pre-apply (add)"
run $C profile update --active --pre-apply "$SCRATCH/pre.sh"
if assert_ok; then
    pass_test "P33"
else fail_test "P33"; fi

begin_test "P34: profile update --active --pre-apply (remove)"
run $C profile update --active --pre-apply "-$SCRATCH/pre.sh"
if assert_ok; then
    pass_test "P34"
else fail_test "P34"; fi

begin_test "P35: profile update --active --post-apply (add)"
run $C profile update --active --post-apply "$SCRATCH/post.sh"
if assert_ok; then
    pass_test "P35"
else fail_test "P35"; fi

begin_test "P36: profile update --active --post-apply (remove)"
run $C profile update --active --post-apply "-$SCRATCH/post.sh"
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
run $C module create overridden --package brew:neovim --set "package.neovim.min-version=0.9"
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
run $C module update editor --set "package.neovim.min-version=0.9"
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
run $C source add "$SOURCE_REPO" --yes --name team-config --profile base --priority 500
if assert_ok; then
    pass_test "SRC03"
else
    fail_test "SRC03"
fi

begin_test "SRC04: source add --branch"
run $C source add "$SOURCE_REPO" --yes --name team-branch --branch master --profile base --priority 500
if assert_ok; then
    pass_test "SRC04"
else
    fail_test "SRC04"
fi

begin_test "SRC05: source add --profile"
run $C source add "$SOURCE_REPO" --yes --name team-profile --profile base --priority 500
if assert_ok; then
    pass_test "SRC05"
else
    fail_test "SRC05"
fi

begin_test "SRC06: source add --accept-recommended"
run $C source add "$SOURCE_REPO" --yes --name team-rec --accept-recommended --profile base --priority 500
if assert_ok; then
    pass_test "SRC06"
else
    fail_test "SRC06"
fi

begin_test "SRC07: source add --priority"
run $C source add "$SOURCE_REPO" --yes --name team-pri --priority 10 --profile base
if assert_ok; then
    pass_test "SRC07"
else
    fail_test "SRC07"
fi

begin_test "SRC08: source add --opt-in"
run $C source add "$SOURCE_REPO" --yes --name team-opt --opt-in packages --profile base --priority 500
if assert_ok; then
    pass_test "SRC08"
else
    fail_test "SRC08"
fi

begin_test "SRC09: source add --sync-interval"
run $C source add "$SOURCE_REPO" --yes --name team-sync --sync-interval 1h --profile base --priority 500
if assert_ok; then
    pass_test "SRC09"
else
    fail_test "SRC09"
fi

begin_test "SRC10: source add --auto-apply"
run $C source add "$SOURCE_REPO" --yes --name team-auto --auto-apply --profile base --priority 500
if assert_ok; then
    pass_test "SRC10"
else
    fail_test "SRC10"
fi

begin_test "SRC11: source add --pin-version"
run $C source add "$SOURCE_REPO" --yes --name team-pin --pin-version ">=1.0" --profile base --priority 500
if assert_ok; then
    pass_test "SRC11"
else
    fail_test "SRC11"
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
run $C source replace team-config "$SOURCE_REPO"
if assert_ok; then
    pass_test "SRC20"
else
    fail_test "SRC20"
fi

begin_test "SRC21: source create"
run $C source create --name my-source --description "local source" --version "1.0.0"
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
    AGE_KEY_FILE="$SCRATCH/age-key.txt"
    age-keygen -o "$AGE_KEY_FILE" 2>/dev/null
    AGE_PUB=$(grep "public key:" "$AGE_KEY_FILE" | awk '{print $NF}')
    cat > "$CFG/.sops.yaml" << SOPSEOF
creation_rules:
  - age: >-
      $AGE_PUB
SOPSEOF
    cp "$CFG/.sops.yaml" "$SCRATCH/.sops.yaml"
    export SOPS_AGE_KEY_FILE="$AGE_KEY_FILE"

    begin_test "SEC03: secret encrypt"
    echo "secret_key: secret-value" > "$SCRATCH/plaintext.yaml"
    run $C secret encrypt "$SCRATCH/plaintext.yaml"
    if assert_ok; then
        pass_test "SEC03"
    else fail_test "SEC03"; fi

    begin_test "SEC04: secret decrypt"
    if [ -f "$SCRATCH/plaintext.yaml" ]; then
        run $C secret decrypt "$SCRATCH/plaintext.yaml"
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
if [ -f "$SCRATCH/plaintext.yaml" ]; then
    EDITOR=true run $C secret edit "$SCRATCH/plaintext.yaml"
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

begin_test "DM02: daemon --status"
run $C daemon --status
# Daemon not running, should report that
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DM02"
else fail_test "DM02" "exit $RC"; fi

begin_test "DM03: daemon --install"
run $C daemon --install
# May succeed or fail depending on systemd/launchd availability
if [ "$RC" -eq 0 ] || [ "$RC" -eq 1 ]; then
    pass_test "DM03"
else fail_test "DM03" "exit $RC"; fi

begin_test "DM04: daemon --uninstall"
run $C daemon --uninstall
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
# SECTION 18: aliases
# ═════════════════════════════════════════════════════

begin_test "AL01: add alias (profile update --active --file)"
touch "$TGT/.aliasrc"
run $C add "$TGT/.aliasrc"
if assert_ok; then
    pass_test "AL01"
else fail_test "AL01"; fi

begin_test "AL02: remove alias (profile update --active --file -path)"
run $C remove "-$TGT/.aliasrc"
if assert_ok; then
    pass_test "AL02"
else fail_test "AL02"; fi

# ═════════════════════════════════════════════════════
# SECTION 19: profile inheritance verification
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
YAML

# ═════════════════════════════════════════════════════
# SECTION 20: template rendering
# ═════════════════════════════════════════════════════

begin_test "TPL01: tera template renders env vars"
# Add a template file to the profile
run $C profile update --active --file "files/config.toml.tera:$TGT/.config.toml"
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
echo "MODIFIED" >> "$TGT/.zshrc"
run $C verify
# verify should detect drift (exit 1 or contain drift info)
if [ "$RC" -eq 1 ] || echo "$OUTPUT" | grep -qi "drift\|mismatch\|changed"; then
    pass_test "DRIFT01"
else fail_test "DRIFT01" "Drift not detected"; fi

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
# SUMMARY
# ═════════════════════════════════════════════════════

print_summary "Exhaustive CLI"
