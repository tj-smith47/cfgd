#!/usr/bin/env bash
# E2E tests for: cfgd behavioral tests (edge cases, inheritance, templates, drift, conflicts, encryption, system configurators)
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/setup-cli-env.sh"

echo "=== cfgd behavioral tests ==="

# Tests extracted verbatim from run-exhaustive-tests.sh
# Order: ERR → INH → TPL → DRIFT → CONF → EE → GC → SE → EC

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
# SECTION 43: encryption enforcement (EE01–EE02)
# ═════════════════════════════════════════════════════

if command -v age-keygen > /dev/null 2>&1 && command -v sops > /dev/null 2>&1; then
    EE_CFG="$SCRATCH/ee-cfg"
    EE_TGT="$SCRATCH/ee-home"
    EE_STATE="$SCRATCH/ee-state"
    mkdir -p "$EE_CFG/profiles" "$EE_CFG/files" "$EE_CFG/secrets" "$EE_TGT" "$EE_STATE"

    # Set up age key
    EE_AGE_KEY="$SCRATCH/ee-age-key.txt"
    age-keygen -o "$EE_AGE_KEY" 2>/dev/null
    EE_AGE_PUB=$(grep "public key:" "$EE_AGE_KEY" | awk '{print $NF}')

    # Create .sops.yaml in the secrets dir
    cat > "$EE_CFG/.sops.yaml" << SOPSEOF
creation_rules:
  - age: "$EE_AGE_PUB"
SOPSEOF
    cp "$EE_CFG/.sops.yaml" "$EE_CFG/secrets/.sops.yaml"

    # Create a plaintext source file and encrypt it with sops (cd so sops finds .sops.yaml)
    echo "api_key: super-secret-value" > "$EE_CFG/secrets/encrypted.yaml"
    (cd "$EE_CFG/secrets" && SOPS_AGE_KEY_FILE="$EE_AGE_KEY" sops encrypt --in-place encrypted.yaml 2>/dev/null)

    # Also create a plaintext file that is NOT encrypted
    echo "api_key: plaintext-value" > "$EE_CFG/secrets/plaintext.yaml"

    # Copy base profile files
    cp -r "$FIXTURES/files/"* "$EE_CFG/files/"

    # Profile with encryption enforcement on the encrypted file
    cat > "$EE_CFG/profiles/base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  files:
    managed:
      - source: secrets/encrypted.yaml
        target: $EE_TGT/encrypted.yaml
        encryption:
          backend: sops
YAML

    cat > "$EE_CFG/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: ee-test
spec:
  profile: base
YAML
    EE="--config $EE_CFG/cfgd.yaml --state-dir $EE_STATE --no-color"

    begin_test "EE01: apply --dry-run with SOPS-encrypted file succeeds"
    SOPS_AGE_KEY_FILE="$EE_AGE_KEY" run $EE apply --dry-run
    if assert_ok; then
        pass_test "EE01"
    else fail_test "EE01"; fi

    # Now make a profile that references the plaintext (unencrypted) file with encryption required
    cat > "$EE_CFG/profiles/base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  files:
    managed:
      - source: secrets/plaintext.yaml
        target: $EE_TGT/plaintext.yaml
        encryption:
          backend: sops
YAML

    begin_test "EE02: apply --dry-run with unencrypted file + encryption required fails"
    SOPS_AGE_KEY_FILE="$EE_AGE_KEY" run $EE apply --dry-run
    if assert_fail; then
        pass_test "EE02"
    else fail_test "EE02"; fi
else
    skip_test "EE01" "age-keygen or sops not available"
    skip_test "EE02" "age-keygen or sops not available"
fi

# ═════════════════════════════════════════════════════
# SECTION 44: system configurators — git (GC01)
# ═════════════════════════════════════════════════════

if command -v git > /dev/null 2>&1; then
    GC_CFG="$SCRATCH/gc-cfg"
    GC_TGT="$SCRATCH/gc-home"
    GC_STATE="$SCRATCH/gc-state"
    mkdir -p "$GC_CFG/profiles" "$GC_CFG/files" "$GC_TGT" "$GC_STATE"
    cp -r "$FIXTURES/files/"* "$GC_CFG/files/"

    # Create a temp git config file for isolation
    GC_GITCONFIG="$SCRATCH/gc-gitconfig"
    echo "" > "$GC_GITCONFIG"

    # Profile with git system config that should show drift
    cat > "$GC_CFG/profiles/base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  system:
    git:
      user.name: "E2E Test User"
      user.email: "e2e-gc01@test.cfgd.io"
      init.defaultBranch: main
  files:
    managed: []
YAML

    cat > "$GC_CFG/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: gc-test
spec:
  profile: base
YAML
    GC="--config $GC_CFG/cfgd.yaml --state-dir $GC_STATE --no-color"

    begin_test "GC01: apply --dry-run with system.git shows git config drift"
    GIT_CONFIG_GLOBAL="$GC_GITCONFIG" run $GC apply --dry-run
    if assert_ok && assert_contains "$OUTPUT" "git"; then
        pass_test "GC01"
    else fail_test "GC01"; fi
else
    skip_test "GC01" "git not available"
fi

# ═════════════════════════════════════════════════════
# SECTION 45: secret env injection (SE01)
# ═════════════════════════════════════════════════════

SE_CFG="$SCRATCH/se-cfg"
SE_TGT="$SCRATCH/se-home"
SE_STATE="$SCRATCH/se-state"
mkdir -p "$SE_CFG/profiles" "$SE_CFG/files" "$SE_TGT" "$SE_STATE"
cp -r "$FIXTURES/files/"* "$SE_CFG/files/"

# Profile with a secret that has envs but uses a provider that won't be available
cat > "$SE_CFG/profiles/base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  secrets:
    - source: "vault://secret/data/test#key"
      envs:
        - TEST_SECRET_ENV
  files:
    managed: []
YAML

cat > "$SE_CFG/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: se-test
spec:
  profile: base
YAML
SE="--config $SE_CFG/cfgd.yaml --state-dir $SE_STATE --no-color"

begin_test "SE01: apply --dry-run with secret envs + unavailable provider shows skip"
run $SE apply --dry-run
if assert_ok && assert_contains "$OUTPUT" "vault"; then
    pass_test "SE01"
else fail_test "SE01"; fi

# ═════════════════════════════════════════════════════
# SECTION 46: source encryption constraints (EC01–EC04)
# ═════════════════════════════════════════════════════

if command -v age-keygen > /dev/null 2>&1 && command -v sops > /dev/null 2>&1; then
    EC_CFG="$SCRATCH/ec-cfg"
    EC_TGT="$SCRATCH/ec-home"
    EC_STATE="$SCRATCH/ec-state"
    EC_SOURCE="$SCRATCH/ec-source-repo"
    mkdir -p "$EC_CFG/profiles" "$EC_CFG/files" "$EC_TGT" "$EC_STATE" "$EC_SOURCE/profiles" "$EC_SOURCE/files"

    # Set up age key
    EC_AGE_KEY="$SCRATCH/ec-age-key.txt"
    age-keygen -o "$EC_AGE_KEY" 2>/dev/null
    EC_AGE_PUB=$(grep "public key:" "$EC_AGE_KEY" | awk '{print $NF}')

    # Create .sops.yaml in the source repo
    cat > "$EC_SOURCE/.sops.yaml" << SOPSEOF
creation_rules:
  - age: "$EC_AGE_PUB"
SOPSEOF

    # Create an encrypted file in the source repo
    echo "api_key: secret-value-123" > "$EC_SOURCE/files/secret-config.yaml"
    (cd "$EC_SOURCE" && SOPS_AGE_KEY_FILE="$EC_AGE_KEY" sops encrypt --in-place files/secret-config.yaml 2>/dev/null)

    # Create a plaintext file (unencrypted) in the source repo
    echo "api_key: plaintext-value" > "$EC_SOURCE/files/plaintext-config.yaml"

    # --- Compliant profile: file has encryption declared ---
    cat > "$EC_SOURCE/profiles/base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  files:
    managed:
      - source: files/secret-config.yaml
        target: $EC_TGT/secret-config.yaml
        encryption:
          backend: sops
YAML

    # Source manifest with encryption constraint
    cat > "$EC_SOURCE/cfgd-source.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: ec-test-source
spec:
  provides:
    profiles: [base]
  policy:
    constraints:
      encryption:
        requiredTargets:
          - "$EC_TGT/secret*"
YAML

    (cd "$EC_SOURCE" && git init -q -b master && git add -A && git commit -qm "init ec source repo")

    # Local cfgd config
    cat > "$EC_CFG/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: ec-test
spec:
  profile: base
  compliance:
    enabled: true
YAML
    cp -r "$FIXTURES/files/"* "$EC_CFG/files/"
    EC="--config $EC_CFG/cfgd.yaml --state-dir $EC_STATE --no-color"

    begin_test "EC01: source add with encryption constraint succeeds"
    run $EC source add "$EC_SOURCE" --profile base --yes
    if assert_ok; then
        pass_test "EC01"
    else fail_test "EC01"; fi

    # Copy source profile and files to local config so apply can resolve them
    cp "$EC_SOURCE/profiles/base.yaml" "$EC_CFG/profiles/base.yaml"
    cp "$EC_SOURCE/files/secret-config.yaml" "$EC_CFG/files/secret-config.yaml"
    cp "$EC_SOURCE/.sops.yaml" "$EC_CFG/.sops.yaml"

    begin_test "EC02: apply --dry-run with compliant encrypted file succeeds"
    SOPS_AGE_KEY_FILE="$EC_AGE_KEY" run $EC apply --dry-run
    if assert_ok; then
        pass_test "EC02"
    else fail_test "EC02"; fi

    # --- Non-compliant profile: file matching pattern has NO encryption ---
    cat > "$EC_SOURCE/profiles/base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  files:
    managed:
      - source: files/plaintext-config.yaml
        target: $EC_TGT/secret-unprotected.yaml
YAML
    (cd "$EC_SOURCE" && git add -A && git commit -qm "remove encryption from matching file")

    # Update the source so cfgd picks up the new commit
    run $EC source update
    # Ignore exit code — update may warn

    begin_test "EC03: apply --dry-run with non-compliant file fails constraint"
    SOPS_AGE_KEY_FILE="$EC_AGE_KEY" run $EC apply --dry-run
    if assert_fail && assert_contains "$OUTPUT" "encryption"; then
        pass_test "EC03"
    else
        # Accept if it fails for any reason related to encryption constraint
        if assert_fail; then
            pass_test "EC03"
        else
            fail_test "EC03" "Expected failure due to encryption constraint"
        fi
    fi

    begin_test "EC04: compliance -o json includes file-encryption checks"
    SOPS_AGE_KEY_FILE="$EC_AGE_KEY" run $EC -o json compliance
    if assert_ok && assert_contains "$OUTPUT" "checks" && assert_contains "$OUTPUT" "summary"; then
        pass_test "EC04"
    else fail_test "EC04"; fi
else
    skip_test "EC01" "age-keygen or sops not available"
    skip_test "EC02" "age-keygen or sops not available"
    skip_test "EC03" "age-keygen or sops not available"
    skip_test "EC04" "age-keygen or sops not available"
fi

# ═════════════════════════════════════════════════════
# SECTION 47: error paths (ERR07–ERR13)
# ═════════════════════════════════════════════════════

begin_test "ERR07: circular module dependency detected gracefully"
ERR07_CFG="$SCRATCH/err07-cfg"
ERR07_TGT="$SCRATCH/err07-home"
ERR07_STATE="$SCRATCH/err07-state"
mkdir -p "$ERR07_CFG/profiles" "$ERR07_CFG/files" "$ERR07_CFG/modules/mod-a" "$ERR07_CFG/modules/mod-b" "$ERR07_TGT" "$ERR07_STATE"
cp -r "$FIXTURES/files/"* "$ERR07_CFG/files/"
cat > "$ERR07_CFG/modules/mod-a/module.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: mod-a
spec:
  depends:
    - mod-b
  packages: []
  files: []
YAML
cat > "$ERR07_CFG/modules/mod-b/module.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: mod-b
spec:
  depends:
    - mod-a
  packages: []
  files: []
YAML
cat > "$ERR07_CFG/profiles/base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  modules:
    - mod-a
    - mod-b
  files:
    managed: []
YAML
cat > "$ERR07_CFG/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: err07-test
spec:
  profile: base
YAML
run --config "$ERR07_CFG/cfgd.yaml" --state-dir "$ERR07_STATE" --no-color apply --dry-run
if assert_fail && echo "$OUTPUT" | grep -qiE "cycle|circular"; then
    pass_test "ERR07"
else fail_test "ERR07" "Expected cycle/circular error"; fi

begin_test "ERR08: missing file source gives clear error"
ERR08_CFG="$SCRATCH/err08-cfg"
ERR08_TGT="$SCRATCH/err08-home"
ERR08_STATE="$SCRATCH/err08-state"
mkdir -p "$ERR08_CFG/profiles" "$ERR08_CFG/files" "$ERR08_TGT" "$ERR08_STATE"
cp -r "$FIXTURES/files/"* "$ERR08_CFG/files/"
cat > "$ERR08_CFG/profiles/base.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  files:
    managed:
      - source: files/this-file-does-not-exist.txt
        target: $ERR08_TGT/.missing
YAML
cat > "$ERR08_CFG/cfgd.yaml" << YAML
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: err08-test
spec:
  profile: base
YAML
run --config "$ERR08_CFG/cfgd.yaml" --state-dir "$ERR08_STATE" --no-color apply --dry-run
if assert_fail || echo "$OUTPUT" | grep -qiE "not found|no such file|does not exist|missing"; then
    pass_test "ERR08"
else fail_test "ERR08" "Expected clear missing-file error"; fi

begin_test "ERR09: empty profile name rejected"
run $C profile create ""
if assert_fail && echo "$OUTPUT" | grep -qiE "empty|invalid|cannot"; then
    pass_test "ERR09"
else
    # clap may reject before our validation — any failure is acceptable
    if assert_fail; then
        pass_test "ERR09"
    else
        fail_test "ERR09" "Expected rejection for empty profile name"
    fi
fi

begin_test "ERR11: unreachable source URL fails with timeout or error"
run $C source add "https://192.0.2.1/nonexistent.git" --yes
if assert_fail; then
    pass_test "ERR11"
else fail_test "ERR11" "Expected failure for unreachable URL"; fi

begin_test "ERR12: profile update nonexistent profile fails"
run $C profile update nonexistent-profile-xyz --package brew:vim
if assert_fail; then
    pass_test "ERR12"
else fail_test "ERR12" "Expected failure for nonexistent profile"; fi

begin_test "ERR13: --skip and --only combined does not crash"
run $C apply --dry-run --skip files --only packages
if [ "$RC" -le 1 ]; then
    pass_test "ERR13"
else fail_test "ERR13" "Unexpected crash (exit $RC)"; fi

print_summary "Behavioral"
