#!/usr/bin/env bash
# cfgd code quality audit
# Uses block-aware test filtering: an awk pass strips #[cfg(test)] blocks
# by tracking brace depth, so violations inside test modules are correctly ignored.
#
# Workspace layout: crates/{cfgd-crd,cfgd-core,cfgd,cfgd-csi,cfgd-operator}/src/
set -euo pipefail
cd "$(dirname "$0")/../.."

ERRORS=0
WARNINGS=0

SRC_ROOTS=(crates/cfgd-crd/src crates/cfgd-core/src crates/cfgd/src crates/cfgd-csi/src crates/cfgd-operator/src)

# --- Formatting helpers ---

_color()  { printf '\033[%sm' "$1"; }
_reset()  { printf '\033[0m'; }
_red()    { _color "0;31"; }
_yellow() { _color "0;33"; }
_green()  { _color "0;32"; }
_bold()   { _color "1"; }

log_error()   { _red;    printf "ERROR"; _reset; printf ": %s\n" "$1"; ERRORS=$((ERRORS + 1)); }
log_warn()    { _yellow; printf "WARN";  _reset; printf ":  %s\n" "$1"; WARNINGS=$((WARNINGS + 1)); }
log_ok()      { _green;  printf "OK";    _reset; printf ":    %s\n" "$1"; }
log_section() { printf "\n--- %s ---\n" "$1"; }

# --- Strip test blocks from a file and output non-test lines ---
strip_test_blocks_from_file() {
    local filepath="$1"
    awk -v filepath="$filepath" '
    BEGIN { in_test = 0; test_depth = 0 }
    /^[[:space:]]*#\[cfg\(test\)\]/ {
        in_test = 1
        test_depth = 0
        next
    }
    in_test {
        opens = gsub(/{/, "{")
        test_depth += opens
        closes = gsub(/}/, "}")
        test_depth -= closes
        if (test_depth <= 0 && opens + closes > 0) {
            in_test = 0
            test_depth = 0
        }
        next
    }
    { print filepath ":" NR ":" $0 }
    ' "$filepath"
}

# --- Core check function ---
# Usage: check_pattern <severity> <label> <pattern> <exclude_pattern>
#   Searches ALL .rs files across all workspace crates (excluding test blocks).
#   exclude_pattern: grep -v pattern to exclude allowed directories/files (optional)
check_pattern() {
    local severity="$1"
    local label="$2"
    local pattern="$3"
    local exclude_pattern="${4:-}"

    local results=""
    while IFS= read -r -d '' rsfile; do
        local file_results
        file_results=$(strip_test_blocks_from_file "$rsfile" | grep -E "$pattern" || true)
        if [[ -n "$file_results" ]]; then
            results="${results}${file_results}"$'\n'
        fi
    done < <(find "${SRC_ROOTS[@]}" -name '*.rs' -print0 2>/dev/null)

    # Apply exclude filter
    if [[ -n "$exclude_pattern" ]]; then
        results=$(echo "$results" | grep -v -E "$exclude_pattern" || true)
    fi

    # Remove blank lines
    results=$(echo "$results" | sed '/^$/d')

    if [[ -n "$results" ]]; then
        case "$severity" in
            error) log_error "$label" ;;
            warn)  log_warn "$label"  ;;
        esac
        echo "$results" | head -20
    else
        log_ok "$label"
    fi
}

# --- Module boundary check for cfgd-core ---
# Usage: check_core_boundary <module> <forbidden_imports>
#   module:            directory name under crates/cfgd-core/src/ (e.g., "sources")
#   forbidden_imports: colon-separated crate module names that must not be imported
check_core_boundary() {
    local module="$1"
    local forbidden="$2"
    local module_dir="crates/cfgd-core/src/$module"

    [[ ! -d "$module_dir" ]] && return 0

    IFS=':' read -ra imports <<< "$forbidden"
    local pattern
    pattern=$(printf 'use crate::%s|' "${imports[@]}")
    pattern="${pattern%|}"

    local results=""
    while IFS= read -r -d '' rsfile; do
        local file_results
        file_results=$(strip_test_blocks_from_file "$rsfile" | grep -E "$pattern" || true)
        if [[ -n "$file_results" ]]; then
            results="${results}${file_results}"$'\n'
        fi
    done < <(find "$module_dir" -name '*.rs' -print0 2>/dev/null)

    results=$(echo "$results" | sed '/^$/d')

    if [[ -n "$results" ]]; then
        log_error "$module/ must not import ${forbidden//:/, }"
        echo "$results" | head -10
    fi
}

# --- Run all checks ---

_bold; printf "=== cfgd Code Quality Audit ===\n"; _reset

log_section "Output Centralization"
check_pattern error \
    "No println!/eprintln! outside output/ and main.rs" \
    'println!\(|eprintln!\(' \
    'output/|main\.rs:'

log_section "No Unwrap in Library Code"
# Match .unwrap() but NOT .unwrap_or(), .unwrap_or_default(), .unwrap_or_else()
# Exclusions:
#   - main.rs / gen_crds.rs: binary entry points (expect is acceptable)
#   - test_helpers.rs: shared test scaffolding
#   - tests.rs / *_test.rs: inline #[cfg(test)] modules — test code is allowed
#     to unwrap freely (matches the anodizer anti-patterns convention).
#   - test_*.rs / tests_*.rs: test-only modules gated by #![cfg(test)]
#     (e.g. test_kube_harness.rs, tests_drift_alert.rs).
check_pattern error \
    "No .unwrap()/.expect() in library code" \
    '\.unwrap\(\)[^_]|\.unwrap\(\)$|\.expect\(' \
    'main\.rs:|gen_crds\.rs:|test_helpers\.rs:|/tests\.rs:|_test\.rs:|/test_[^/]*\.rs:|/tests_[^/]*\.rs:'

log_section "Console/Indicatif Encapsulation"
check_pattern error \
    "console/indicatif/syntect only used in output/" \
    'use (console|indicatif|syntect)::' \
    'output/'

log_section "Controlled Shell Execution"
# sources/ allowed for git SSH fallback (git2 doesn't support all SSH configs)
# gateway/ allowed for SSH/GPG enrollment signature verification
# output/ allowed for Printer::run (controlled execution layer for progress UI)
# generate/ allowed for tool inspection (--version checks) and system settings scanning
# oci/ allowed for Docker credential helper execution (docker-credential-*)
# daemon/ allowed for sc.exe Windows Service lifecycle management
check_pattern warn \
    "std::process::Command confined to packages/, secrets/, system/, reconciler/, sources/, platform/, cli/, gateway/, output/, generate/, oci, daemon/" \
    'std::process::Command|Command::new' \
    'packages/|secrets/|system/|reconciler/|sources/|platform/|cli/|gateway/|output/|generate/|oci|daemon/|lib\.rs:'

log_section "Error Type Discipline"
check_pattern error \
    "anyhow confined to CLI boundary (main.rs, cli/, mcp/)" \
    'anyhow::' \
    'main\.rs:|cli/|mcp/|cfgd-operator/src/app\.rs:'

log_section "No Dead Code Allowances"
check_pattern warn \
    "No #[allow(dead_code)] on individual items — delete unused code instead" \
    '#[^!]\[allow\(dead_code\)' \
    ""

log_section "Module Boundaries (cfgd-core)"
check_core_boundary "providers"   "files:packages:secrets:sources:composition:reconciler:state:daemon"
check_core_boundary "sources"     "files:packages:secrets:reconciler:providers"
check_core_boundary "composition" "files:packages:secrets:reconciler:daemon:providers"
check_core_boundary "modules"     "files:packages:secrets:reconciler:state:daemon:composition:sources"
check_core_boundary "reconciler"  "files:packages:secrets"

log_section "Dead Error Variants"
# For each error enum in errors/ files, extract variant names and check if they're
# ever constructed anywhere. Accounts for:
#   - Direct construction: ::Variant { or ::Variant(
#   - #[from] auto-conversion: variant has (#[from] ...) in definition
dead_variants=""
for errors_file in $(find "${SRC_ROOTS[@]}" -path '*/errors*' -name '*.rs' 2>/dev/null); do
    # Extract PascalCase variant names (excluding #[from] variants which are auto-constructed)
    variants=$(grep -oP '^\s+([A-Z][a-zA-Z]+)\s*[\{(]' "$errors_file" \
        | sed 's/[[:space:]]*//g; s/[{(]$//' | sort -u || true)
    # Get list of #[from] variants — #[from] appears on the same line as the variant
    from_variants=$(grep '#\[from\]' "$errors_file" \
        | grep -oP '([A-Z][a-zA-Z]+)\s*\(' | sed 's/\s*($//' || true)
    for variant in $variants; do
        # Skip #[from] variants — they're constructed via the ? operator
        if echo "$from_variants" | grep -qw "$variant" 2>/dev/null; then
            continue
        fi
        # Count construction sites: ::Variant { or ::Variant( across all source
        uses=$(grep -r "::${variant}\s*{\\|::${variant}\s*(" "${SRC_ROOTS[@]}" \
            --include='*.rs' 2>/dev/null \
            | grep -v '#\[error' | grep -v 'enum ' || true)
        if [[ -z "$uses" ]]; then
            dead_variants="${dead_variants}  ${errors_file}: ${variant}\n"
        fi
    done
done
if [[ -n "$dead_variants" ]]; then
    log_warn "Error variants never constructed (wire up or delete):"
    printf "$dead_variants"
else
    log_ok "All error variants are constructed somewhere"
fi

log_section "DRY — Repeated String Literals"
dupes=$(while IFS= read -r -d '' rsfile; do
    strip_test_blocks_from_file "$rsfile" \
        | grep -oh '"[^"]\{30,\}"' || true
done < <(find "${SRC_ROOTS[@]}" -name '*.rs' -print0 2>/dev/null) \
    | sort | uniq -c | sort -rn \
    | awk '$1 > 2 {print}' \
    | grep -v -E 'and_then.*unwrap_or|\.status\.conditions\[\?\(@\.type|width=device-width|spec\.[a-z]+\[.{1,5}\]\.[a-z]+ must not be empty|apple\.com/DTDs/PropertyList|Kubernetes CRD|Mode: profile|cannot determine state directory|skipping (env var|alias) with unsafe name|detect_brew_system_method' \
    | head -5 || true)
if [[ -n "$dupes" ]]; then
    log_warn "Repeated string literals (>2 occurrences, >30 chars):"
    echo "$dupes"
else
    log_ok "No obvious string literal duplication"
fi

log_section "DRY — Duplicated Function Definitions"
# Extract fn names from non-test code across all crates, flag any name defined in >1 file.
# Excludes trait-standard method names that legitimately repeat across impls.
fn_dupes=""
while IFS= read -r -d '' rsfile; do
    strip_test_blocks_from_file "$rsfile" \
        | grep -E '^\S+:[0-9]+:\s*(pub\s+)?(async\s+)?fn [a-z_]+\(' \
        | sed 's/.*fn \([a-z_]*\)(.*/\1/' \
        || true
done < <(find "${SRC_ROOTS[@]}" -name '*.rs' -print0 2>/dev/null) \
    | sort | uniq -c | sort -rn \
    | awk '$1 > 1 && \
        $2 != "new" && $2 != "default" && $2 != "from" && $2 != "fmt" && $2 != "drop" && \
        $2 != "name" && $2 != "is_available" && $2 != "can_bootstrap" && $2 != "bootstrap" && \
        $2 != "installed_packages" && $2 != "install" && $2 != "uninstall" && $2 != "update" && \
        $2 != "diff" && $2 != "apply" && $2 != "current_state" && \
        $2 != "scan_source" && $2 != "scan_target" && \
        $2 != "get" && $2 != "set" && $2 != "delete" && $2 != "list" && $2 != "resolve" && \
        $2 != "open" && $2 != "init_tables" && $2 != "run" && $2 != "build" && $2 != "test" && \
        $2 != "validate" && $2 != "main" && $2 != "plan_packages" && $2 != "from_str" && \
        $2 != "expand_tilde" && $2 != "encrypt_file" && $2 != "edit_file" && \
        $2 != "decrypt_file" && $2 != "build_registry" && $2 != "as_str" && \
        $2 != "router" && $2 != "set_device_config" && $2 != "record_drift_event" && \
        $2 != "list_drift_events" && $2 != "list_fleet_events" && $2 != "read_current_config" && \
        $2 != "load_profile" && $2 != "plan" && $2 != "plan_files" && \
        $2 != "list_devices" && $2 != "get_device" && $2 != "enroll" && \
        $2 != "display_name" && $2 != "config_path" && $2 != "checkin" && \
        $2 != "from_spec" && $2 != "extend_registry_custom_managers" && \
        $2 != "available_version" && \
        $2 != "load_module" && \
        $2 != "installed_packages_with_versions" && $2 != "success" && \
        $2 != "run_migrations" && $2 != "request_challenge" && $2 != "path_dirs" && \
        $2 != "package_aliases" && $2 != "is_empty" && $2 != "expecting" && \
        $2 != "error" && $2 != "enroll_info" && $2 != "parse" && \
        $2 != "cmd_status" && \
        $2 != "terminate_process" && $2 != "set_file_permissions" && \
        $2 != "is_same_inode" && $2 != "is_root" && $2 != "is_executable" && \
        $2 != "run_health_server" && $2 != "run_as_windows_service" && \
        $2 != "read" && \
        $2 != "home_dir_var" && $2 != "file_permissions_mode" && \
        $2 != "create_symlink_impl" && $2 != "cleanup_old_binary" && \
        $2 != "atomic_replace" && $2 != "acquire_apply_lock" && \
        $2 != "recv_sighup" && $2 != "recv_sigterm" && $2 != "read_command_output" && \
        $2 != "unavailable" && $2 != "set_fail_apply" \
        {print}' \
    > /tmp/cfgd_fn_dupes 2>/dev/null || true
fn_dupes=$(cat /tmp/cfgd_fn_dupes 2>/dev/null || true)
rm -f /tmp/cfgd_fn_dupes
if [[ -n "$fn_dupes" ]]; then
    log_warn "Function names defined in multiple files (potential duplication):"
    echo "$fn_dupes" | head -10
else
    log_ok "No duplicated function definitions across files"
fi

log_section "Naming Convention — No kebab-case in serde or user-visible strings"
# Detect any remaining kebab-case serde attributes (should all be camelCase now)
serde_kebab=$(grep -rn 'rename_all = "kebab-case"\|rename_all = "lowercase"' "${SRC_ROOTS[@]}" --include='*.rs' 2>/dev/null | grep -v 'output/' || true)
if [[ -n "$serde_kebab" ]]; then
    log_error "Found kebab-case/lowercase serde attributes (should be camelCase or removed):"
    echo "$serde_kebab"
else
    log_ok "No kebab-case/lowercase serde attributes"
fi

# Detect explicit serde rename attributes that use kebab-case (should be camelCase)
bad_renames=$(grep -rn '#\[serde(rename = "' "${SRC_ROOTS[@]}" --include='*.rs' 2>/dev/null \
    | grep -E 'rename = "[a-z]+-[a-z]+"' \
    || true)
if [[ -n "$bad_renames" ]]; then
    log_error "Found kebab-case explicit serde rename attributes (should be camelCase):"
    echo "$bad_renames" | head -10
else
    log_ok "No kebab-case explicit serde rename attributes"
fi

# Detect kebab-case config field names in user-visible strings (not comments, not CLI flags, not file paths)
# Dynamically generate field name patterns from config struct definitions across config/*.rs.
# This auto-updates as new fields are added — no manual list to maintain.
config_fields=$(grep -rE '^\s+pub [a-z_]+:' crates/cfgd-core/src/config/ --include='*.rs' \
    | sed 's/.*pub \([a-z_]*\):.*/\1/' \
    | grep '_' \
    | sed 's/_/-/g' \
    | sort -u \
    | sed 's/^/"/' | sed 's/$/"/' \
    | paste -sd '|' - \
    || true)
if [[ -n "$config_fields" ]]; then
    kebab_fields=$(grep -rn "$config_fields" "${SRC_ROOTS[@]}" --include='*.rs' 2>/dev/null \
        | grep -v '#\[arg(long' \
        | grep -v '#\[serde(' \
        | grep -v '\.txt\|\.key\|keygen\|\.json' \
        || true)
    if [[ -n "$kebab_fields" ]]; then
        log_error "Found kebab-case config field names in string literals (should be camelCase):"
        echo "$kebab_fields" | head -10
    else
        log_ok "No kebab-case config field names in string literals"
    fi
else
    log_warn "Could not extract config field names from config/*.rs — skipping kebab-case field check"
fi

log_section "Config Parsing Boundary"
# CLAUDE.md rule #5: all config parsing must live in config/.
# Check cfgd-core for serde_yaml::from_* calls outside config/, generate/, and lib.rs.
# generate/ and schema/ legitimately validate YAML documents (not loading
# application config) so both are excluded — they are two halves of one
# validation pipeline (schema/ parses raw YAML to extract apiVersion/spec for the
# KIND_REGISTRY validators; generate/validate.rs delegates straight into schema/).
# modules/ legitimately parses lockfiles (not application config) so it is excluded.
# Test blocks are stripped before checking.
config_parse_violations=""
while IFS= read -r -d '' rsfile; do
    case "$rsfile" in
        */config/*|*/generate/*|*/modules/*|*/schema/*|*/lib.rs) continue ;;
    esac
    violations=$(strip_test_blocks_from_file "$rsfile" \
        | grep -E 'serde_yaml::from_(str|reader|value)' \
        || true)
    if [[ -n "$violations" ]]; then
        config_parse_violations="${config_parse_violations}${violations}"$'\n'
    fi
done < <(find crates/cfgd-core/src -name '*.rs' -print0 2>/dev/null)
if [[ -n "$config_parse_violations" ]]; then
    log_warn "serde_yaml::from_* found in cfgd-core outside config/, generate/, schema/, or modules/ (CLAUDE.md rule #5):"
    printf "%s" "$config_parse_violations" | head -10
else
    log_ok "Config parsing confined to config/, generate/, schema/, and modules/ in cfgd-core"
fi

log_section "Effective-state routing (module↔profile coherence)"
# Read-back commands (diff/status/live-drift/verify/compliance) must derive
# desired state from cfgd_core::effective::* — the single source that merges
# module resources into the profile's desired state. A read path that reads
# profile-only views instead silently drops every module-contributed resource,
# making module packages/system-settings/files invisible to that command. Bans:
#   desired_packages_for( / desired_packages_for_spec(  → effective_desired_packages
#   .merged.system / profile.system  (direct field reads) → effective_system_map
#   .files.managed                                        → effective_files
# Passing &resolved.merged or profile as an ARGUMENT to effective_* is fine and
# does not match (the bans target the .system FIELD read, not .merged itself).
# crates/cfgd-core/src/effective.rs is intentionally exempt — it IS the source
# of truth and is simply not in the scanned list below.
effective_read_paths=(
    crates/cfgd/src/cli/diff.rs
    crates/cfgd/src/cli/status.rs
    crates/cfgd/src/cli/live_drift.rs
    crates/cfgd-core/src/reconciler/verify.rs
    crates/cfgd-core/src/compliance/mod.rs
)
effective_pattern='desired_packages_for\(|desired_packages_for_spec\(|\.merged\.system|profile\.system|\.files\.managed'
effective_violations=""
for rsfile in "${effective_read_paths[@]}"; do
    [[ -f "$rsfile" ]] || continue
    file_results=$(strip_test_blocks_from_file "$rsfile" | grep -E "$effective_pattern" || true)
    if [[ -n "$file_results" ]]; then
        effective_violations="${effective_violations}${file_results}"$'\n'
    fi
done
effective_violations=$(echo "$effective_violations" | sed '/^$/d')
if [[ -n "$effective_violations" ]]; then
    log_error "Read path reads profile-only desired state (use cfgd_core::effective::* so module resources stay visible):"
    echo "$effective_violations" | head -20
else
    log_ok "Read paths route desired state through cfgd_core::effective::*"
fi

log_section "DRY — Timestamp/Hash/Command Wrappers"
# Detect local wrappers around shared lib.rs functions.
check_pattern warn \
    "No local timestamp wrappers (use cfgd_core::utc_now_iso8601 directly)" \
    'fn (chrono_now|local_now|get_now|timestamp_now|now_utc)\(' \
    ""

# --- output banned patterns -------------------------------------------------
# Block the indent-hack and old-API patterns the output module forbids.
#
# CFGD_AUDIT_PATH: replace `crates/`, do NOT append. The audit-tests driver
# sets this per-fixture so each fixture is scanned in isolation; appending
# would mix in 1000+ hits from crates/ and make every good_*.txt fixture
# spuriously fail.

# 1. Banned old-API method calls outside the output module(s).
banned_methods='printer\.(success|warning|info|error|header|subheader|key_value|newline|plan_phase|stdout_line)\('
if violations=$(rg --type-add 'rust:*.txt' --type rust -n "$banned_methods" \
      "${CFGD_AUDIT_PATH:-crates/}" \
      --glob '!crates/cfgd-core/src/output/**' \
      --glob '!**/tests.rs' \
      --glob '!**/tests/**' 2>/dev/null) && [ -n "$violations" ]; then
  log_error "BANNED OLD-API CALLS (Printer methods removed in output):"
  echo "$violations"
fi

# 2. Indent hack in printer args. Catches:
#      printer.X("  …               (two-or-more leading spaces)
#      printer.X("<TAB>…            (literal tab byte in source)
#      printer.X("\t…               (backslash-t escape)
#      printer.X(&format!("  …
#      printer.X(format!("  …
#      printer.X(&"  …".to_string())
#    Pattern "(  |\t|\\t) catches the three canonical hack shapes; a lone
#    single leading space is normal prose and is NOT a hack.
if hack=$(rg --type-add 'rust:*.txt' --type rust -n 'printer\.\w+\(\s*&?(format!\()?"(  |\t|\\t)' \
      "${CFGD_AUDIT_PATH:-crates/}" \
      --glob '!crates/cfgd-core/src/output/**' \
      --glob '!**/tests.rs' \
      --glob '!**/tests/**' 2>/dev/null) && [ -n "$hack" ]; then
  log_error "INDENT HACK (>=2 spaces, tab byte, or \\t escape leading printer arg):"
  echo "$hack"
fi

# 3. KV key-indent hack — same shapes.
if kv_hack=$(rg --type-add 'rust:*.txt' --type rust -n '\.kv\(\s*&?(format!\()?"(  |\t|\\t)' \
      "${CFGD_AUDIT_PATH:-crates/}" \
      --glob '!crates/cfgd-core/src/output/**' \
      --glob '!**/tests.rs' \
      --glob '!**/tests/**' 2>/dev/null) && [ -n "$kv_hack" ]; then
  log_error "KV KEY INDENT HACK (>=2 spaces, tab byte, or \\t escape leading kv key):"
  echo "$kv_hack"
fi

# 4. Direct console::* / indicatif::*::new outside the output module(s).
#    Hard Rule #1 extended to the new types.
if direct=$(rg --type-add 'rust:*.txt' --type rust -n '(console::|indicatif::(ProgressBar|MultiProgress)::new)' \
      "${CFGD_AUDIT_PATH:-crates/}" \
      --glob '!crates/cfgd-core/src/output/**' \
      --glob '!**/tests.rs' \
      --glob '!**/tests/**' 2>/dev/null) && [ -n "$direct" ]; then
  log_error "DIRECT TERMINAL TYPES (console::* / indicatif::*::new) outside output module:"
  echo "$direct"
fi

# 5. Structured-output coverage table — every cmd_* function in cli/ must
#    appear in .claude/rules/output-module.md's coverage table.
#    Only match file-scope definitions (no leading whitespace) to avoid
#    matching test helper functions inside #[cfg(test)] blocks.
# LC_ALL=C: comm requires both inputs in the same collation as the sort that
# produced them; locale-aware sort/comm skew can falsely flag interleaved
# `_`-bearing rows as unsorted, so pin byte collation across both sorts and comm.
cmds_in_code=$(rg --type rust --color never -n \
      '^(pub(\(crate\)|(\(super\)))? fn |fn )cmd_' \
      crates/cfgd/src/cli/ --glob '!**/tests.rs' --glob '!**/tests/**' \
      2>/dev/null \
      | sed -E 's/.*fn cmd_([a-z_]+).*/\1/' | LC_ALL=C sort -u)
rule_file=".claude/rules/output-module.md"
if [ -f "$rule_file" ]; then
    cmds_in_table=$(awk '/^## Structured-output coverage/,0' "$rule_file" \
        | grep -E '^\| [a-z]' | awk -F'|' '{print $2}' | tr -d ' ' | LC_ALL=C sort -u)
    missing=$(LC_ALL=C comm -23 <(echo "$cmds_in_code") <(echo "$cmds_in_table" | tr ' ' '_'))
    if [ -n "$missing" ]; then
        log_error "Commands missing from structured-output coverage table in $rule_file:"
        echo "$missing"
    fi
fi
# --- end output audit block -------------------------------------------------

# --- Path-handling consolidation gates ---
# Lock in the migrations from `.claude/specs/2026-05-26-path-handling-consolidation.md`.
# Each gate forbids a pattern the corresponding wave migrated away from.

log_section "Path-handling consolidation (cross-OS portability)"

# Wave 2: no inline `format!("file://...")` outside cfgd_core::to_file_url itself
# (and its test_helpers::file_url alias). Anything else must go through
# `cfgd_core::to_file_url(...)`.
if w2=$(rg --type rust -n 'format!\("file://' \
      "${CFGD_AUDIT_PATH:-crates/}" \
      --glob '!crates/cfgd-core/src/util/paths.rs' \
      --glob '!crates/cfgd-core/src/test_helpers.rs' \
      2>/dev/null) && [ -n "$w2" ]; then
  log_error "Wave 2 violation: inline file:// formatter (use cfgd_core::to_file_url):"
  echo "$w2"
fi

# Wave 5 (production): no ad-hoc `replace('\\', "/")` outside paths.rs in
# production code. Tests are excluded because some snapshot-mask helpers
# legitimately fold the `sha256-` separator etc.; the gate would over-fire on
# them. Production paths must use `cfgd_core::to_posix_string` / `posixify_text`
# / `from_user_input` instead.
if w5=$(rg --type rust -n "replace\('\\\\\\\\', \"/\"\)" \
      "${CFGD_AUDIT_PATH:-crates/}" \
      --glob '!crates/cfgd-core/src/util/paths.rs' \
      --glob '!**/tests.rs' \
      --glob '!**/tests/**' \
      2>/dev/null) && [ -n "$w5" ]; then
  log_error "Wave 5 violation: inline backslash fold in production (use cfgd_core::to_posix_string / posixify_text / from_user_input):"
  echo "$w5"
fi

# Wave 3 (tests): no ad-hoc CRLF strips. Use cfgd_core::normalize_line_endings
# or normalize_for_snapshot. Exclude paths.rs itself (where the helper lives)
# and the output module (whose renderer has its own buffered handling).
if w3=$(rg --type rust -n 'replace\("\\\\r\\\\n", "\\\\n"\)' \
      "${CFGD_AUDIT_PATH:-crates/}" \
      --glob '!crates/cfgd-core/src/util/paths.rs' \
      --glob '!crates/cfgd-core/src/output/**' \
      2>/dev/null) && [ -n "$w3" ]; then
  log_error "Wave 3 violation: ad-hoc CRLF strip (use cfgd_core::normalize_line_endings or normalize_for_snapshot):"
  echo "$w3"
fi

# Wave 1: `.display()` / `.to_string_lossy()` flowing into a serialization
# boundary (serde_json::json!, rusqlite, yaml emitter, axum response) on the
# same line. Coarse heuristic — same-line co-occurrence — excludes tests.
if w1=$(rg --type rust -n '(serde_json::json!|rusqlite::|conn\.execute|to_yaml|axum::)' \
      "${CFGD_AUDIT_PATH:-crates/}" \
      --glob '!**/tests.rs' \
      --glob '!**/tests/**' \
      --glob '!crates/cfgd-core/src/test_helpers.rs' \
      2>/dev/null \
      | grep -E '\.display\(\)|\.to_string_lossy\(\)') && [ -n "$w1" ]; then
  log_error "Wave 1 violation: path-to-string at serialization boundary (use cfgd_core::to_posix_string):"
  echo "$w1"
fi

# Wave 4: `.display()` on the same line as a user-facing surface (printer
# methods, tracing::{info,warn,error}!, anyhow!/bail!). Those should route
# through cfgd_core::PathDisplayExt::posix() / .display_posix() so Windows
# folds `\` → `/`. tracing::debug!/trace! is intentionally excluded — debug
# tooling should see paths in OS-native form. Tests + paths.rs (trait
# definition) are excluded.
if w4=$(rg --type rust -n '(tracing::(info|warn|error)!|anyhow!|bail!|printer\.(status|kv|data_line|note|hint|heading|section|run|progress_bar|spinner))' \
      "${CFGD_AUDIT_PATH:-crates/}" \
      --glob '!**/tests.rs' \
      --glob '!**/tests/**' \
      --glob '!crates/cfgd-core/src/test_helpers.rs' \
      --glob '!crates/cfgd-core/src/util/paths.rs' \
      2>/dev/null \
      | grep -E '\.display\(\)') && [ -n "$w4" ]; then
  log_error "Wave 4 violation: path .display() on user-facing surface (use cfgd_core::PathDisplayExt::posix() / .display_posix()):"
  echo "$w4"
fi

log_section "CLI long_about/Examples coverage (every top-level Command variant)"
# CLAUDE.md convention: "Every top-level Command variant carries long_about
# with an Examples: block." This gate enforces it as a regression guard so the
# `cfgd skill` / `cfgd <kind> validate` surfaces (and every future variant)
# can't ship without a worked example in `--help`.
#
# Detection (robust, errs toward flagging): walk the `pub enum Command {` body
# by brace depth. At depth 1, accumulate the pending `#[command(...)]`
# attribute (multi-line — tracked by paren balance) and, on reaching a variant
# declaration (a depth-1 `Pascal` line), assert that pending attribute carried
# `long_about` AND that the long_about text contained the literal `Examples:`.
# A variant with no `#[command(...)]`, no `long_about`, or a `long_about`
# lacking `Examples:` is flagged by name + source line. Only the top-level
# enum is scanned — nested subcommand enums are out of scope for the convention.
cli_mod="crates/cfgd/src/cli/mod.rs"
if [[ -f "$cli_mod" ]]; then
    long_about_gaps=$(awk '
    # Locate the top-level command enum opening brace.
    !in_enum && /^pub enum Command[[:space:]]*\{/ { in_enum = 1; depth = 1; next }
    !in_enum { next }

    {
        # Track brace depth across the enum body (ignores nested struct/enum
        # bodies so only depth-1 lines are treated as variants).
        line = $0
        opens  = gsub(/{/, "{", line)
        closes = gsub(/}/, "}", line)
    }

    # Accumulate a (possibly multi-line) #[command(...)] attribute at depth 1.
    depth == 1 && !collecting && /^[[:space:]]*#\[command\(/ {
        collecting = 1
        attr = ""
        paren = 0
    }
    collecting {
        attr = attr "\n" $0
        paren += gsub(/\(/, "(")
        paren -= gsub(/\)/, ")")
        if (paren <= 0) { collecting = 0 }
        # advance depth AFTER buffering (attr lines carry no enum-body braces)
        depth += opens - closes
        next
    }

    # A depth-1 PascalCase token starting a line is a variant declaration.
    depth == 1 && /^[[:space:]]{4}[A-Z][A-Za-z0-9]*([[:space:]]*[({,]|[[:space:]]*$)/ {
        match($0, /[A-Z][A-Za-z0-9]*/)
        variant = substr($0, RSTART, RLENGTH)
        has_la = (attr ~ /long_about[[:space:]]*=/)
        # Examples: must appear inside the long_about string, which is the only
        # multi-line prose the attribute carries; a plain substring test on the
        # buffered attribute is sufficient and conservative.
        has_ex = (attr ~ /Examples:/)
        if (!has_la) {
            printf "  %s:%d: %s — missing long_about\n", FILENAME, NR, variant
        } else if (!has_ex) {
            printf "  %s:%d: %s — long_about lacks an \"Examples:\" block\n", FILENAME, NR, variant
        }
        attr = ""
        depth += opens - closes
        if (depth <= 0) { in_enum = 0 }
        next
    }

    {
        depth += opens - closes
        if (in_enum && depth <= 0) { in_enum = 0 }
    }
    ' "$cli_mod")
    if [[ -n "$long_about_gaps" ]]; then
        log_error "Top-level Command variants missing long_about/Examples: (CLAUDE.md CLI convention):"
        printf "%s\n" "$long_about_gaps"
    else
        log_ok "Every top-level Command variant has long_about with an Examples: block"
    fi
else
    log_error "CLI enum file not found: $cli_mod (long_about gate could not run)"
fi

# --- Summary ---
printf "\n"
_bold; printf "=== Audit Complete: %d errors, %d warnings ===\n" "$ERRORS" "$WARNINGS"; _reset

[[ "$ERRORS" -gt 0 ]] && exit 1
exit 0
