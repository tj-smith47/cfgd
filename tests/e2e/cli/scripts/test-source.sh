#!/usr/bin/env bash
# E2E tests for: cfgd source
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd source tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh
# NOTE: SRC01-SRC26 are ORDER-DEPENDENT (later tests depend on sources added by earlier tests)

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

# SECTION 38: source additional flags

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

print_summary "Source"
