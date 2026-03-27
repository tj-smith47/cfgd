#!/usr/bin/env bash
# E2E tests for: cfgd profile
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd profile tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

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

# SECTION 36: profile create additional flags

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

print_summary "Profile"
