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

# --- Secret backend detection tests (SEC06-SEC10) ---

begin_test "SEC06: 1Password backend, op not installed"
if command -v op > /dev/null 2>&1; then
    skip_test "SEC06" "op CLI is installed, cannot test missing provider"
else
    SEC06_CFG="$SCRATCH/sec06/cfg"
    SEC06_TGT="$SCRATCH/sec06/home"
    SEC06_STATE="$SCRATCH/sec06/state"
    mkdir -p "$SEC06_CFG/profiles" "$SEC06_TGT" "$SEC06_STATE"
    cat > "$SEC06_CFG/cfgd.yaml" << 'YAML'
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: sec06
spec:
  profile: sec06-profile
YAML
    cat > "$SEC06_CFG/profiles/sec06-profile.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: sec06-profile
spec:
  secrets:
    - source: "1password://vault/item/field"
      target: $SEC06_TGT/test-secret
YAML
    run --config "$SEC06_CFG/cfgd.yaml" --state-dir "$SEC06_STATE" --no-color apply --dry-run
    if assert_ok && assert_contains "$OUTPUT" "1password" && assert_contains "$OUTPUT" "not available"; then
        pass_test "SEC06"
    else fail_test "SEC06"; fi
fi

begin_test "SEC07: Bitwarden backend, bw not installed"
if command -v bw > /dev/null 2>&1; then
    skip_test "SEC07" "bw CLI is installed, cannot test missing provider"
else
    SEC07_CFG="$SCRATCH/sec07/cfg"
    SEC07_TGT="$SCRATCH/sec07/home"
    SEC07_STATE="$SCRATCH/sec07/state"
    mkdir -p "$SEC07_CFG/profiles" "$SEC07_TGT" "$SEC07_STATE"
    cat > "$SEC07_CFG/cfgd.yaml" << 'YAML'
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: sec07
spec:
  profile: sec07-profile
YAML
    cat > "$SEC07_CFG/profiles/sec07-profile.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: sec07-profile
spec:
  secrets:
    - source: "bitwarden://folder/item"
      target: $SEC07_TGT/test-secret
YAML
    run --config "$SEC07_CFG/cfgd.yaml" --state-dir "$SEC07_STATE" --no-color apply --dry-run
    if assert_ok && assert_contains "$OUTPUT" "bitwarden" && assert_contains "$OUTPUT" "not available"; then
        pass_test "SEC07"
    else fail_test "SEC07"; fi
fi

begin_test "SEC08: Vault backend, vault not installed"
if command -v vault > /dev/null 2>&1; then
    skip_test "SEC08" "vault CLI is installed, cannot test missing provider"
else
    SEC08_CFG="$SCRATCH/sec08/cfg"
    SEC08_TGT="$SCRATCH/sec08/home"
    SEC08_STATE="$SCRATCH/sec08/state"
    mkdir -p "$SEC08_CFG/profiles" "$SEC08_TGT" "$SEC08_STATE"
    cat > "$SEC08_CFG/cfgd.yaml" << 'YAML'
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: sec08
spec:
  profile: sec08-profile
YAML
    cat > "$SEC08_CFG/profiles/sec08-profile.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: sec08-profile
spec:
  secrets:
    - source: "vault://secret/path#field"
      target: $SEC08_TGT/test-secret
YAML
    run --config "$SEC08_CFG/cfgd.yaml" --state-dir "$SEC08_STATE" --no-color apply --dry-run
    if assert_ok && assert_contains "$OUTPUT" "vault" && assert_contains "$OUTPUT" "not available"; then
        pass_test "SEC08"
    else fail_test "SEC08"; fi
fi

begin_test "SEC09: Unknown backend name"
SEC09_CFG="$SCRATCH/sec09/cfg"
SEC09_TGT="$SCRATCH/sec09/home"
SEC09_STATE="$SCRATCH/sec09/state"
mkdir -p "$SEC09_CFG/profiles" "$SEC09_TGT" "$SEC09_STATE"
cat > "$SEC09_CFG/cfgd.yaml" << 'YAML'
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: sec09
spec:
  profile: sec09-profile
YAML
cat > "$SEC09_CFG/profiles/sec09-profile.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: sec09-profile
spec:
  secrets:
    - source: "nonexistent-provider://some/ref"
      target: $SEC09_TGT/test-secret
YAML
# Unknown provider scheme is not recognized by parse_secret_reference, so the
# reconciler treats it as a SOPS file path. Apply may exit 0 but with error output,
# or exit non-zero. Either way, the output should mention the failed decrypt.
run --config "$SEC09_CFG/cfgd.yaml" --state-dir "$SEC09_STATE" --no-color apply --dry-run
if echo "$OUTPUT" | grep -qiE "sops|decrypt|failed|error|non.existent|cannot operate"; then
    pass_test "SEC09"
elif assert_fail; then
    pass_test "SEC09"
else fail_test "SEC09" "expected error output for unknown provider scheme"; fi

begin_test "SEC10: 1Password full flow (gated)"
if [ -z "${OP_SERVICE_ACCOUNT_TOKEN:-}" ]; then
    skip_test "SEC10" "OP_SERVICE_ACCOUNT_TOKEN not set"
else
    skip_test "SEC10" "1Password full flow not yet implemented"
fi

print_summary "Secret"
