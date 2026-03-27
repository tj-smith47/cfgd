#!/usr/bin/env bash
# E2E tests for: cfgd secret
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd secret tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh

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

print_summary "Secret"
