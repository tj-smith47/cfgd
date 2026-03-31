#!/usr/bin/env bash
# cfgd code quality audit
# Uses block-aware test filtering: an awk pass strips #[cfg(test)] blocks
# by tracking brace depth, so violations inside test modules are correctly ignored.
#
# Workspace layout: crates/{cfgd-core,cfgd,cfgd-operator}/src/
set -euo pipefail
cd "$(dirname "$0")/../.."

ERRORS=0
WARNINGS=0

SRC_ROOTS=(crates/cfgd-core/src crates/cfgd/src crates/cfgd-operator/src)

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
# Exclude main.rs and gen_crds.rs (binary entry points where expect is acceptable)
check_pattern error \
    "No .unwrap()/.expect() in library code" \
    '\.unwrap\(\)[^_]|\.unwrap\(\)$|\.expect\(' \
    'main\.rs:|gen_crds\.rs:|test_helpers\.rs:'

log_section "Console/Indicatif Encapsulation"
check_pattern error \
    "console/indicatif/syntect only used in output/" \
    'use (console|indicatif|syntect)::' \
    'output/'

log_section "Controlled Shell Execution"
# sources/ allowed for git SSH fallback (git2 doesn't support all SSH configs)
# gateway/ allowed for SSH/GPG enrollment signature verification
# output/ allowed for Printer::run_with_output (controlled execution layer for progress UI)
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
    'main\.rs:|cli/|mcp/'

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
        $2 != "recv_sighup" && $2 != "recv_sigterm" && $2 != "read_command_output" \
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
serde_kebab=$(grep -rn 'rename_all = "kebab-case"\|rename_all = "lowercase"' "${SRC_ROOTS[@]}" --include='*.rs' 2>/dev/null || true)
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
# Dynamically generate field name patterns from config struct definitions in config/mod.rs.
# This auto-updates as new fields are added — no manual list to maintain.
config_fields=$(grep -E '^\s+pub [a-z_]+:' crates/cfgd-core/src/config/mod.rs \
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
    log_warn "Could not extract config field names from config/mod.rs — skipping kebab-case field check"
fi

log_section "Config Parsing Boundary"
# CLAUDE.md rule #5: all config parsing must live in config/.
# Check cfgd-core for serde_yaml::from_* calls outside config/, generate/, and lib.rs.
# generate/ legitimately validates YAML (not loading application config) so it is excluded.
# modules/ legitimately parses lockfiles (not application config) so it is excluded.
# Test blocks are stripped before checking.
config_parse_violations=""
while IFS= read -r -d '' rsfile; do
    case "$rsfile" in
        */config/*|*/generate/*|*/modules/*|*/lib.rs) continue ;;
    esac
    violations=$(strip_test_blocks_from_file "$rsfile" \
        | grep -E 'serde_yaml::from_(str|reader|value)' \
        || true)
    if [[ -n "$violations" ]]; then
        config_parse_violations="${config_parse_violations}${violations}"$'\n'
    fi
done < <(find crates/cfgd-core/src -name '*.rs' -print0 2>/dev/null)
if [[ -n "$config_parse_violations" ]]; then
    log_warn "serde_yaml::from_* found in cfgd-core outside config/, generate/, or modules/ (CLAUDE.md rule #5):"
    printf "%s" "$config_parse_violations" | head -10
else
    log_ok "Config parsing confined to config/, generate/, and modules/ in cfgd-core"
fi

log_section "DRY — Timestamp/Hash/Command Wrappers"
# Detect local wrappers around shared lib.rs functions.
check_pattern warn \
    "No local timestamp wrappers (use cfgd_core::utc_now_iso8601 directly)" \
    'fn (chrono_now|local_now|get_now|timestamp_now|now_utc)\(' \
    ""

# --- Summary ---
printf "\n"
_bold; printf "=== Audit Complete: %d errors, %d warnings ===\n" "$ERRORS" "$WARNINGS"; _reset

[[ "$ERRORS" -gt 0 ]] && exit 1
exit 0
