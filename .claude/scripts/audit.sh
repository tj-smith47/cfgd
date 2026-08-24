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

# --- Shared awk library ---
# Prepend to any awk program that counts braces or honours an `<x>-ok:` marker,
# so every gate answers "is this code?" and "is this exempt?" the same way. A
# brace inside a string literal, a `'{'` char literal or a `//` comment is not
# code structure: counted raw, one `let closer = "}";` ends a span early and
# everything after it stops being scanned. A marker inside a message string is
# not an annotation either — that would let a call exempt itself by naming the
# escape hatch in its own text.
AWK_LIB='
BEGIN { RAW_HASHES = -1; IN_STR = 0 }
function hashes_str(n,   s) { s = ""; while (n-- > 0) s = s "#"; return s }
# The code half of one line, with every literal and comment removed, plus the
# comment half in LAST_COMMENT. Raw-string state carries across calls, so a
# caller must invoke this exactly ONCE per line and feed the lines of a file in
# order — twice on one line double-advances the state machine.
function code_only(line,   q, out, i, j, n, c, h, closer, p) {
    LAST_COMMENT = ""
    # Fast path: nothing on this line can open a literal or a comment.
    if (RAW_HASHES < 0 && !IN_STR && line !~ /["\047\/]/) return line
    q = sprintf("%c", 39)
    out = ""
    i = 1
    n = length(line)
    while (i <= n) {
        # A plain Rust string literal spans lines freely (with or without a
        # trailing backslash), so an unterminated one carries its state to the
        # next line. Read as code, its continuation lines pair the wrong quotes
        # and a `layer";` tail parses as an `r"` raw-string opener that then
        # swallows the rest of the file.
        if (IN_STR) {
            while (i <= n) {
                c = substr(line, i, 1)
                if (c == "\\") { i += 2; continue }
                i++
                if (c == "\"") { IN_STR = 0; break }
            }
            if (IN_STR) return out
            continue
        }
        if (RAW_HASHES >= 0) {
            closer = "\"" hashes_str(RAW_HASHES)
            p = index(substr(line, i), closer)
            if (p == 0) return out
            i += p - 1 + length(closer)
            RAW_HASHES = -1
            continue
        }
        c = substr(line, i, 1)
        if (c == "/") {
            if (substr(line, i + 1, 1) == "/") { LAST_COMMENT = substr(line, i); return out }
            out = out c
            i++
            continue
        }
        if (c == "r" || c == "b") {
            h = raw_open_hashes(line, i)
            if (h >= 0) {
                j = i + RAW_OPEN_LEN
                closer = "\"" hashes_str(h)
                p = index(substr(line, j), closer)
                if (p == 0) { RAW_HASHES = h; return out }
                i = j + p - 1 + length(closer)
                continue
            }
            out = out c
            i++
            continue
        }
        if (c == q) {
            # A char literal is two or three characters inside the quotes;
            # anything else opening with a quote is a lifetime, which is code.
            if (substr(line, i + 1, 1) == "\\" && substr(line, i + 3, 1) == q) { i += 4; continue }
            if (substr(line, i + 2, 1) == q) { i += 3; continue }
            out = out q
            i++
            continue
        }
        if (c == "\"") {
            j = i + 1
            while (j <= n) {
                c = substr(line, j, 1)
                if (c == "\\") { j += 2; continue }
                if (c == "\"") break
                j++
            }
            if (j > n) { IN_STR = 1; return out }
            i = j + 1
            continue
        }
        out = out c
        i++
    }
    return out
}
# Hash count of a raw-string opener (`r"`, `r#"`, `br##"`, …) starting at `i`,
# or -1 when this is an ordinary identifier. Sets RAW_OPEN_LEN to the length of
# the opener.
function raw_open_hashes(line, i,   j, h) {
    j = i
    if (substr(line, j, 1) == "b") j++
    if (substr(line, j, 1) != "r") return -1
    j++
    h = 0
    while (substr(line, j, 1) == "#") { h++; j++ }
    if (substr(line, j, 1) != "\"") return -1
    RAW_OPEN_LEN = j - i + 1
    return h
}
function is_comment_line(line) {
    return (line ~ /^[^:]*:[0-9]+:[[:space:]]*\/\//)
}
# A marker counts only inside a comment and only with a reason after it, so a
# call cannot exempt itself by naming the escape hatch in its own message.
function carries_marker(comment, marker) {
    return (comment ~ (marker "[[:space:]]*[^[:space:]]"))
}
# The line above only lends its marker when it IS a comment line: a previous
# CALL carrying its own marker used to exempt the unmarked call beneath it.
function marker_applies(comment, prev_line, prev_comment, marker) {
    if (carries_marker(comment, marker)) return 1
    if (is_comment_line(prev_line) && carries_marker(prev_comment, marker)) return 1
    return 0
}
'

# --- Fail loudly when a hard-coded scan directory no longer exists ---
# A gate whose scope directory was renamed finds nothing and prints OK forever,
# which is the one failure mode signature/domain anchoring exists to avoid.
require_dirs() {
    local label="$1"
    shift
    local missing=() d
    for d in "$@"; do
        [[ -d "$d" ]] || missing+=("$d")
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        log_error "$label: scan directory missing — this gate is scanning nothing (rename it here too): ${missing[*]}"
        return 1
    fi
    return 0
}

# The file-list counterpart: a gate driven by an enumerated set of paths skips a
# renamed member (`[[ -f ]] || continue`) and still reports the set clean.
require_files() {
    local label="$1"
    shift
    local missing=() f
    for f in "$@"; do
        [[ -f "$f" ]] || missing+=("$f")
    done
    if [[ ${#missing[@]} -gt 0 ]]; then
        log_error "$label: scanned file missing — this gate is skipping it (rename it here too): ${missing[*]}"
        return 1
    fi
    return 0
}

require_dirs "workspace source scan" "${SRC_ROOTS[@]}" || true

# --- Fail loudly when a tool a gate SEARCHES WITH is missing ---
# Nine gates below are shaped `if hits=$(rg …) && [ -n "$hits" ]`, whose failure
# arm is indistinguishable from "found nothing": on a host without ripgrep the
# banned-old-API, indent-hack, kv-indent, direct-terminal-types,
# structured-output-coverage, all four path-handling waves and the owner
# comparator gates report a clean run having searched nothing at all. That is the
# same vacuous-green shape require_dirs exists to prevent, so it is checked the
# same way rather than trusted to the environment.
if ! command -v rg >/dev/null 2>&1; then
    log_error "ripgrep (rg) not found — the rg-based gates (banned old-API calls, indent hacks, direct terminal types, structured-output coverage, path-handling waves, owner ordering) would search nothing and pass silently"
fi

# --- Strip test blocks from a file and output non-test lines ---
# Every gate below re-strips the same files, and the strip now parses literals
# character by character, so the result is cached per file for the run.
STRIP_CACHE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/cfgd-audit.XXXXXX")"
trap 'rm -rf "$STRIP_CACHE_DIR"' EXIT

strip_test_blocks_from_file() {
    local filepath="$1"
    local cached="$STRIP_CACHE_DIR/${filepath//\//__}"
    if [[ -f "$cached" ]]; then
        cat "$cached"
        return 0
    fi
    _strip_test_blocks_uncached "$filepath" | tee "$cached"
}

_strip_test_blocks_uncached() {
    local filepath="$1"
    awk -v filepath="$filepath" "$AWK_LIB"'
    BEGIN { in_test = 0; test_depth = 0 }
    { code = code_only($0) }
    /^[[:space:]]*#\[cfg\(test\)\]/ {
        in_test = 1
        test_depth = 0
        next
    }
    in_test {
        opens = gsub(/{/, "{", code)
        closes = gsub(/}/, "}", code)
        test_depth += opens - closes
        if (test_depth <= 0 && opens + closes > 0) {
            in_test = 0
            test_depth = 0
        }
        next
    }
    { print filepath ":" NR ":" $0 }
    ' "$filepath"
}

# --- Extract test blocks from a file (the inverse of strip) ---
# The test-hygiene gates (sleep-ok, raw-capture-ok, path-guard-ok) anchor to
# TEST code only — a raw sleep or a raw capture-buffer read is a production
# concern nowhere else. A whole-file test module (the same naming convention
# `strip_test_blocks_from_file`'s callers exclude) is entirely in scope; an
# inline `#[cfg(test)] mod tests { … }` block within a production file
# contributes only its own span, tracked the same brace-depth way the strip
# does. Cached per file for the run, same as the strip.
extract_test_blocks_from_file() {
    local filepath="$1"
    local cached="$STRIP_CACHE_DIR/${filepath//\//__}.testonly"
    if [[ -f "$cached" ]]; then
        cat "$cached"
        return 0
    fi
    _extract_test_blocks_uncached "$filepath" | tee "$cached"
}

_extract_test_blocks_uncached() {
    local filepath="$1"
    case "$filepath" in
        */tests.rs|*_test.rs|*/test_*.rs|*/tests_*.rs|*/test_helpers.rs|*/tests/*)
            awk -v filepath="$filepath" '{ print filepath ":" NR ":" $0 }' "$filepath"
            return 0
            ;;
    esac
    awk -v filepath="$filepath" "$AWK_LIB"'
    BEGIN { in_test = 0; test_depth = 0 }
    { code = code_only($0) }
    /^[[:space:]]*#\[cfg\(test\)\]/ {
        in_test = 1
        test_depth = 0
        next
    }
    in_test {
        opens = gsub(/{/, "{", code)
        closes = gsub(/}/, "}", code)
        test_depth += opens - closes
        print filepath ":" NR ":" $0
        if (test_depth <= 0 && opens + closes > 0) {
            in_test = 0
            test_depth = 0
        }
        next
    }
    ' "$filepath"
}

# --- Core check function ---
# Usage: check_pattern <severity> <label> <pattern> <exclude_pattern>
#   Searches ALL .rs files across all workspace crates (excluding test blocks).
#   exclude_pattern: grep -v pattern to exclude allowed directories/files (optional)
# The corpus every check_pattern gate reads: the workspace source roots
# normally, and EXACTLY the path CFGD_AUDIT_PATH names when the audit-tests
# driver scopes a run to one fixture. Without the second arm a check_pattern
# gate is unreachable from a fixture — it would keep scanning crates/ and
# report OK no matter what the fixture contains, so a `bad_*.txt` written to
# prove such a gate proves nothing. The rg-based gates already honour the same
# variable; this makes the scoping mechanism one mechanism.
# Extension-blind on purpose: fixtures are `.txt` so they sit outside the cargo
# source tree.
audit_scan_files() {
    if [[ -n "${CFGD_AUDIT_PATH:-}" ]]; then
        find "$CFGD_AUDIT_PATH" -type f -print0 2>/dev/null
    else
        find "${SRC_ROOTS[@]}" -name '*.rs' -print0 2>/dev/null
    fi
}

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
    done < <(audit_scan_files)

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
# All four write macros, not just the `ln` pair: Hard Rule #1 bans `print!` and
# `eprint!` by name, and a gate anchored on `println!\(` never sees either —
# `print!("{}", …)` is one character away from the banned call and was invisible
# here. The leading class keeps `eprint` from matching as `print`'s suffix, so
# each macro is judged on its own name.
# `src/bin/` joins main.rs: a `[[bin]]` entry point owns its stdout the same way
# the crate's own main does (gen-crds streams a CRD document to a pipe).
check_pattern error \
    "No print!/println!/eprint!/eprintln! outside output/, main.rs and src/bin/" \
    '(^|[^[:alnum:]_])e?print(ln)?!\(' \
    'output/|main\.rs:|src/bin/'

log_section "systemctl Goes Through One Factory"
# Five call sites across three crates spawn systemctl. Two shapes defeat both
# the test seam and the timeout, so both are named:
#   Command::new("systemctl")        — unshimmable AND unbounded, so a test
#                                      reaches the host's own manager and a
#                                      systemd-less host pays the ~90s D-Bus
#                                      connect timeout per unit
#   command_available("systemctl")   — answers from PATH while the spawn beside
#                                      it answers from CFGD_SYSTEMCTL_BIN, so a
#                                      shimmed test reports "unavailable" and
#                                      silently skips the branch it shimmed
# `util/process.rs` is the factory's own home and is where the string belongs.
check_pattern error \
    "systemctl spawned only via cfgd_core::systemctl_cmd (no raw Command/command_available)" \
    'Command::new\("systemctl"\)|command_available\("systemctl"\)' \
    'util/process\.rs:'

log_section "One Rendering of a Child's Exit"
# `status.code()` is None for a process a signal killed, so `unwrap_or(-1)`
# prints `exit code -1` — a number no process ever returned, and the shape an
# interrupted run produces most often. The sites that matter most are the ones
# `command_output_with_timeout` KILLS on timeout: they are guaranteed to reach
# the None arm. `cfgd_core::exit_status_reason` is the one rendering; it names
# the signal instead.
# `util/process.rs` is the function's own home, where the banned idiom is
# quoted in its doc comment as the thing not to write.
check_pattern error \
    "Child-exit wording via cfgd_core::exit_status_reason (no status.code().unwrap_or(-1))" \
    'code\(\)[[:space:]]*\.unwrap_or\(-1\)' \
    'util/process\.rs:'

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

log_section "User-Facing Advisories (config/module/source domains)"
# tracing::info!/warn!/error! is invisible without RUST_LOG — an advisory routed
# there is one the user never sees, and `info!` is the least visible of the
# three: the cfgd binary's own default filter is `warn`, so an info event needs
# both RUST_LOG and a reader. This is what happened to
# warn_on_legacy_theme_keys before it was rerouted through
# CfgdConfig.deprecations + printer.deprecation() (see output-module.md).
#
# Anchored on the DOMAIN: every non-test .rs under the three directories whose
# whole job is turning user-authored YAML/TOML into cfgd's typed config. An
# earlier revision anchored on a `fn (parse|load)_<name>(` signature and walked
# the function's brace span, which selected the wrong set twice over:
# warn_on_legacy_theme_keys — the exemplar this gate exists to prevent — is
# named neither parse_ nor load_, and neither is any of the advisory helpers a
# parse function calls (check_yaml_anchor_limit, read_manifest, …). Scanning the
# whole domain covers all of them, and needs no span walk to be defeated by a
# brace in a string literal or by a body-less trait signature. The domain
# carries no legitimate tracing::info!/warn!/error! today, so the marker below is the
# whole allow-list; the separate "Config Parsing Boundary" gate above keeps
# config-struct parsing from migrating out of these directories in the first
# place.
#
# Escape hatch (mirrors native-ok / spawn-blocking-ok): a genuinely internal
# diagnostic — one no interactive user is meant to read — stays legal when the
# call line or the line directly above it carries
#   // tracing-ok: <why this diagnostic is not user-facing>
# The marker counts only inside a comment, and only with a reason after it, so
# a call cannot exempt itself by naming the hatch in its own message string.
advisory_scope_dirs=(crates/cfgd-core/src/config crates/cfgd-core/src/modules crates/cfgd-core/src/sources)
require_dirs "user-facing advisory scan" "${advisory_scope_dirs[@]}" || true
advisory_violations=""
while IFS= read -r -d '' rsfile; do
    case "$rsfile" in
        */tests.rs|*_test.rs|*/test_*.rs|*/tests_*.rs|*/test_helpers.rs) continue ;;
    esac
    file_hits=$(strip_test_blocks_from_file "$rsfile" | awk "$AWK_LIB"'
        { code = code_only($0); comment = LAST_COMMENT }
        code ~ /tracing::(info|warn|error)!/ &&
        !is_comment_line($0) &&
        !marker_applies(comment, prev, prev_comment, "tracing-ok:") { print }
        { prev = $0; prev_comment = comment }
    ')
    if [[ -n "$file_hits" ]]; then
        advisory_violations="${advisory_violations}${file_hits}"$'\n'
    fi
done < <(find "${advisory_scope_dirs[@]}" -name '*.rs' -print0 2>/dev/null)
advisory_violations=$(echo "$advisory_violations" | sed '/^$/d')
if [[ -n "$advisory_violations" ]]; then
    log_error "tracing::info!/warn!/error! in the config/module/source domains (invisible without RUST_LOG — route through the deprecations-Vec + printer.deprecation() pattern, or mark // tracing-ok: <why> if genuinely internal):"
    echo "$advisory_violations" | head -20
else
    log_ok "No tracing::info!/warn!/error! in the config/module/source domains"
fi

log_section "Duplicate Narration (tracing::info! outside daemon/)"
# An `info!` is cfgd narrating itself, and every user-facing thing it has to say
# is already a Printer line. What the tracing channel adds at that level is a
# second copy of the same sentence, written to the one stream the live region
# repaints — which strands the last paint of whatever bar is on screen (cfgd
# module push printed its result three times that way and froze its spinner).
# The binary's default filter is `warn` for exactly that reason, so an info!
# outside the daemon is a line nobody sees AND a strand risk when they do.
#
# daemon/ is the whole exemption, and not a grandfathered one: there the log IS
# the output — a service under systemd/launchd prints its ticks to journald
# through this channel and no other, which is why `cfgd daemon run` keeps `info`
# as its tracing floor (main.rs::runs_reconcile_loop).
#
# Same // tracing-ok: <why> hatch as the domain gate above.
narration_scope_dirs=(crates/cfgd-core/src crates/cfgd/src)
require_dirs "duplicate narration scan" "${narration_scope_dirs[@]}" || true
narration_violations=""
while IFS= read -r -d '' rsfile; do
    case "$rsfile" in
        */daemon/*) continue ;;
        */tests.rs|*_test.rs|*/test_*.rs|*/tests_*.rs|*/test_helpers.rs) continue ;;
    esac
    file_hits=$(strip_test_blocks_from_file "$rsfile" | awk "$AWK_LIB"'
        { code = code_only($0); comment = LAST_COMMENT }
        code ~ /tracing::info!/ &&
        !is_comment_line($0) &&
        !marker_applies(comment, prev, prev_comment, "tracing-ok:") { print }
        { prev = $0; prev_comment = comment }
    ')
    if [[ -n "$file_hits" ]]; then
        narration_violations="${narration_violations}${file_hits}"$'\n'
    fi
done < <(find "${narration_scope_dirs[@]}" -name '*.rs' -print0 2>/dev/null)
narration_violations=$(echo "$narration_violations" | sed '/^$/d')
if [[ -n "$narration_violations" ]]; then
    log_error "tracing::info! outside daemon/ (a second copy of a line the Printer already prints, on the stream the live region repaints — demote to debug!, delete it, or mark // tracing-ok: <why>):"
    echo "$narration_violations" | head -20
else
    log_ok "No tracing::info! outside daemon/"
fi

log_section "Controlled Shell Execution"
# gateway/ allowed for SSH/GPG enrollment signature verification
# output/ allowed for Printer::run (controlled execution layer for progress UI)
# generate/ allowed for tool inspection (--version checks) and system settings scanning
# oci/ allowed for Docker credential helper execution (docker-credential-*)
# daemon/ allowed for sc.exe Windows Service lifecycle management
# util/{git,process,env_session}.rs are the cfgd-core controlled-execution seams
#   catalogued in .claude/rules/module-boundaries.md (git_cmd_*/cosign_cmd,
#   command_output_with_timeout, launchctl/systemctl/setx session refresh).
# test_helpers.rs is test scaffolding (Command::new appears only in #[cfg(test)]
# submodules and doc comments).
# providers/mod.rs only NAMES the type, in SystemContext::run_silent's signature,
#   and forwards to output/; it constructs and spawns nothing. The exclusion is
#   anchored to that exact parameter line so any other Command use there is caught.
check_pattern warn \
    "std::process::Command confined to packages/, secrets/, system/, reconciler/, platform/, cli/, gateway/, output/, generate/, oci, daemon/, util/{git,process,env_session}.rs" \
    'std::process::Command|Command::new' \
    'packages/|secrets/|system/|reconciler/|platform/|cli/|gateway/|output/|generate/|oci|daemon/|util/git\.rs:|util/process\.rs:|util/env_session\.rs:|providers/mod\.rs:[0-9]+:[[:space:]]+cmd: &mut std::process::Command,$|test_helpers\.rs:|lib\.rs:'

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
# A construction site inside the crate's own tests does not make a variant live:
# the point of the gate is that PRODUCTION code reaches the variant, and an error
# only ever built by the test that asserts its Display string is exactly the dead
# variant this looks for. So the scan runs over the same test-stripped view every
# other gate uses, with whole-file test modules skipped outright.
# Concatenated ONCE, not per variant: this file is scanned for every variant of
# every error enum, and re-stripping the tree inside that loop turns one pass
# into (variants × files) of them.
PRODUCTION_CORPUS="$STRIP_CACHE_DIR/production-corpus"
build_production_corpus() {
    local rsfile
    : > "$PRODUCTION_CORPUS"
    while IFS= read -r -d '' rsfile; do
        case "$rsfile" in
            */tests.rs|*_test.rs|*/test_*.rs|*/tests_*.rs|*/test_helpers.rs) continue ;;
        esac
        strip_test_blocks_from_file "$rsfile" >> "$PRODUCTION_CORPUS"
    done < <(find "${SRC_ROOTS[@]}" -name '*.rs' -print0 2>/dev/null)
}
build_production_corpus
production_construction_sites() {
    grep -E "::${1}[[:space:]]*[{(]" "$PRODUCTION_CORPUS" \
        | grep -v '#\[error' | grep -v 'enum ' || true
}
for errors_file in $(find "${SRC_ROOTS[@]}" -path '*/errors*' -name '*.rs' 2>/dev/null); do
    # Extract PascalCase variant names (excluding #[from] variants which are
    # auto-constructed). Digits are part of a variant name — `Sha256Mismatch`
    # matched neither regex below, so a digit-bearing variant could never be
    # reported dead however unreachable it became.
    variants=$(grep -oP '^\s+([A-Z][a-zA-Z0-9]+)\s*[\{(]' "$errors_file" \
        | sed 's/[[:space:]]*//g; s/[{(]$//' | sort -u || true)
    # Get list of #[from] variants — #[from] appears on the same line as the variant
    from_variants=$(grep '#\[from\]' "$errors_file" \
        | grep -oP '([A-Z][a-zA-Z0-9]+)\s*\(' | sed 's/\s*($//' || true)
    for variant in $variants; do
        # Skip #[from] variants — they're constructed via the ? operator
        if echo "$from_variants" | grep -qw "$variant" 2>/dev/null; then
            continue
        fi
        uses=$(production_construction_sites "$variant")
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
# Whole-file test modules (tests.rs, *_test.rs, test_*.rs, tests_*.rs,
# test_helpers.rs) carry no inline #[cfg(test)] marker, so strip_test_blocks
# cannot strip them. Skip them outright: this gate measures production-code DRY,
# and test fixtures legitimately repeat the same scaffold strings.
# output/ is the deliberate parallel-builder API (Printer/SectionGuard/Doc mirror
# each other per output-module.md) — its repeated #[must_use]/role strings are
# by-design, not copy-paste.
# Attribute strings (#[schemars(with=...)], #[must_use=...], #[error(...)], …)
# are Rust-mandated literals that cannot be replaced by a const, so skip them.
dupes=$(while IFS= read -r -d '' rsfile; do
    case "$rsfile" in
        */tests.rs|*_test.rs|*/test_*.rs|*/tests_*.rs|*/test_helpers.rs|*/output/*) continue ;;
    esac
    strip_test_blocks_from_file "$rsfile" \
        | grep -vE ':[0-9]+:[[:space:]]*#\[' \
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
# Emits "<fn> <file>" pairs and dedups them (sort -u) so the per-name count is a
# count of DISTINCT FILES — many impls of one method inside a single file (e.g.
# the per-struct profile merge_from layering) are not cross-file duplication.
# Whole-file test modules are skipped (same rationale as the literal gate above).
# output/ is skipped: Printer/SectionGuard/Doc/StatusBuilder deliberately mirror
# one fluent method surface (output-module.md), so a method name shared across
# those builders is intentional API symmetry, not duplicated logic.
#
# `len` and `is_empty` are excluded together: clippy's `len_without_is_empty`
# requires a type offering one to offer the other, so any collection-shaped
# type in the workspace defines both, and excusing only half of the pair makes
# the gate fire on the idiom it forced.
# ALLOWED_FN_PAIRS excuses one *specific* definition rather than a bare name, so
# the name keeps its budget: `is_clean` is deliberately shared by the two backup
# outcome types (BackupRunRecord, RestoreOutcome) which answer the exit-code
# question under one name, but dropping only the second site means a THIRD
# definition still trips the gate. Adding a name to the awk list below instead
# would blind the check to that name forever.
# `Owner`'s constructors are named after the kind they mint, which is the whole
# point of the closed vocabulary — the collisions are with unrelated
# constructors on other types (`PatchBindings::profile`, `BackupJob::source`).
# Excusing the `Owner` site keeps each name's budget for a real duplicate.
# `ApplyRun::execute` runs one reconcile; `cli::execute` dispatches clap
# subcommands. Nothing is shared between them but the verb.
#
# The remaining pairs excuse a name two unrelated TYPES both answer, where
# nothing but the verb is shared: `Slot::lane` names a package-manager family
# while `PackageContext::lane` hands back a live output region; `MemberState`'s
# `node_id` delegates to `ManagerAction`'s own `*_node` derivations rather than
# re-deriving them; the `cli::output_types` accessors (`token`, `owner`) read a
# rendered payload's fields, not the reconciler types they name.
ALLOWED_FN_PAIRS=(
    "is_clean crates/cfgd-core/src/backup/restore.rs"
    "is_clean crates/cfgd-core/src/backup/mod.rs"
    "profile crates/cfgd-core/src/reconciler/types.rs"
    "module crates/cfgd-core/src/reconciler/types.rs"
    "source crates/cfgd-core/src/reconciler/types.rs"
    "execute crates/cfgd-core/src/reconciler/run.rs"
    "lane crates/cfgd-core/src/providers/mod.rs"
    "node_id crates/cfgd-core/src/reconciler/managers.rs"
    "push crates/cfgd-core/src/daemon/service/windows_eventlog.rs"
    "token crates/cfgd/src/cli/output_types.rs"
    "owner crates/cfgd/src/cli/output_types.rs"
    "actions crates/cfgd/src/cli/output_types.rs"
)
allowed_pairs_file="$STRIP_CACHE_DIR/allowed-fn-pairs"
printf '%s\n' "${ALLOWED_FN_PAIRS[@]}" > "$allowed_pairs_file"
# A digit is a legal character in a Rust fn name, so the extraction takes
# `[a-z0-9_]` — anchored on `[a-z_]` it read `fn sha256_hex(` as a definition of
# `sha` and could never count the real name.
fn_dupes=$(while IFS= read -r -d '' rsfile; do
    case "$rsfile" in
        */tests.rs|*_test.rs|*/test_*.rs|*/tests_*.rs|*/test_helpers.rs|*/output/*) continue ;;
    esac
    strip_test_blocks_from_file "$rsfile" \
        | grep -E '^\S+:[0-9]+:\s*(pub\s+)?(async\s+)?fn [a-z0-9_]+\(' \
        | sed 's|^\([^:]*\):[0-9]*:.*fn \([a-z0-9_]*\)(.*|\2 \1|' \
        || true
done < <(find "${SRC_ROOTS[@]}" -name '*.rs' -print0 2>/dev/null) \
    | sort -u | grep -vxF -f "$allowed_pairs_file" \
    | awk '{print $1}' | sort | uniq -c | sort -rn \
    | awk '$1 > 1 && \
        $2 != "new" && $2 != "default" && $2 != "from" && $2 != "fmt" && $2 != "drop" && \
        $2 != "name" && $2 != "is_available" && $2 != "bootstrap_plan" && $2 != "bootstrap" && \
        $2 != "installed_packages" && $2 != "install" && $2 != "uninstall" && \
        $2 != "mediated_packages" && \
        $2 != "refresh_index" && $2 != "has_index" && $2 != "listed_identity" && \
        $2 != "plan_packages_observed" && \
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
        $2 != "version_meets_minimum" && \
        $2 != "resolved_prefix" && $2 != "record_resolved_prefix" && \
        $2 != "run_migrations" && $2 != "request_challenge" && $2 != "path_dirs" && \
        $2 != "created_path_dirs" && \
        $2 != "package_aliases" && $2 != "is_empty" && $2 != "len" && $2 != "expecting" && \
        $2 != "error" && $2 != "enroll_info" && $2 != "parse" && \
        $2 != "cmd_status" && \
        $2 != "terminate_process" && $2 != "set_file_permissions" && \
        $2 != "is_same_inode" && $2 != "is_root" && $2 != "is_executable" && \
        $2 != "run_health_server" && $2 != "run_as_windows_service" && \
        $2 != "read" && $2 != "write" && $2 != "flush" && \
        $2 != "home_dir_var" && $2 != "file_permissions_mode" && \
        $2 != "create_symlink_impl" && $2 != "cleanup_old_binary" && \
        $2 != "atomic_replace" && $2 != "acquire_apply_lock" && \
        $2 != "recv_sighup" && $2 != "recv_sigterm" && $2 != "read_command_output" && \
        $2 != "unavailable" && $2 != "set_fail_apply" && \
        $2 != "status" && $2 != "label" && \
        $2 != "render" && $2 != "detect" && $2 != "target_path" && $2 != "id" && \
        $2 != "package_identity" && $2 != "persisted_uninstall" && \
        $2 != "set_config_dir" && $2 != "content_drift" && \
        $2 != "build_file_manager" && $2 != "prune_orphaned_packages" && \
        $2 != "manager_names" && $2 != "aborted" && $2 != "failed" && \
        $2 != "skipped" && $2 != "metrics_handler" && $2 != "compose" && \
        $2 != "default_cache_dir" && $2 != "default_cache_dir_for" && \
        $2 != "field_tree" && $2 != "resolve_runtime_dir" && \
        $2 != "probe_dir_writable" && $2 != "surface_stale_skills" \
        {print}' || true)
rm -f "$allowed_pairs_file"
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
# lockfile.rs (modules/ and sources/) parses lock artifacts (resolved commit SHAs),
# not application config, so every lockfile loader is excluded.
# Inline #[cfg(test)] blocks are stripped; whole-file test modules (tests.rs,
# *_test.rs, test_helpers.rs) carry no inline marker, so skip them outright —
# tests deserialize fixtures freely.
config_parse_violations=""
while IFS= read -r -d '' rsfile; do
    case "$rsfile" in
        */config/*|*/generate/*|*/lockfile.rs|*/schema/*|*/lib.rs) continue ;;
        */tests.rs|*_test.rs|*/test_*.rs|*/tests_*.rs|*/test_helpers.rs) continue ;;
    esac
    # Deserializing into an untyped serde_yaml::Value is document inspection
    # (e.g. SOPS-marker detection), not config-struct parsing — exempt it.
    violations=$(strip_test_blocks_from_file "$rsfile" \
        | grep -E 'serde_yaml::from_(str|reader|value)' \
        | grep -v 'serde_yaml::Value' \
        || true)
    if [[ -n "$violations" ]]; then
        config_parse_violations="${config_parse_violations}${violations}"$'\n'
    fi
done < <(find crates/cfgd-core/src -name '*.rs' -print0 2>/dev/null)
if [[ -n "$config_parse_violations" ]]; then
    log_warn "serde_yaml::from_* found in cfgd-core outside config/, generate/, schema/, or lockfile.rs (CLAUDE.md rule #5):"
    printf "%s" "$config_parse_violations" | head -10
else
    log_ok "Config parsing confined to config/, generate/, schema/, and lockfile.rs in cfgd-core"
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
# The list is every command whose composition mode is `Report` in
# output-module.md's table — the read paths — plus the two core engines they
# share. It is enumerated rather than derived because "is this a read path?" is
# a question about what the command MEANS, not about what it imports; the
# maintenance rule is that a new `Report`-mode command joins this list in the
# commit that adds it. `checkin.rs` was the gap that proves it matters: it
# scanned drift against `resolved.merged.system`, so every system setting a
# module contributed was invisible to the gateway and to the config hash.
effective_read_paths=(
    crates/cfgd/src/cli/diff.rs
    crates/cfgd/src/cli/status.rs
    crates/cfgd/src/cli/live_drift.rs
    crates/cfgd/src/cli/checkin.rs
    crates/cfgd/src/cli/compliance.rs
    crates/cfgd/src/cli/decide.rs
    crates/cfgd/src/cli/verify.rs
    crates/cfgd/src/cli/backup.rs
    crates/cfgd-core/src/reconciler/verify.rs
    crates/cfgd-core/src/compliance/mod.rs
)
require_files "effective-state routing scan" "${effective_read_paths[@]}" || true
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

# 4b. Unconditional version pricing outside the two surfaces that render a
#     version per DECLARED package (cfgd doctor, cfgd module show). Every
#     PLANNING path routes through Reconciler::fill_planned_versions, the
#     survivor-gated form — an unconditional fill there re-prices packages the
#     plan elides, one subprocess per declared package per invocation (the
#     converged-plan multi-second wait). See shared-utils.md's pricing entry.
if unfenced=$(rg --type-add 'rust:*.txt' --type rust -n 'fill_available_versions\(' \
      "${CFGD_AUDIT_PATH:-crates/}" \
      --glob '!crates/cfgd-core/src/modules/resolve.rs' \
      --glob '!crates/cfgd/src/cli/doctor.rs' \
      --glob '!crates/cfgd/src/cli/module/list_show.rs' \
      --glob '!**/tests.rs' \
      --glob '!**/tests/**' 2>/dev/null) && [ -n "$unfenced" ]; then
  log_error "UNCONDITIONAL VERSION PRICING (fill_available_versions is for doctor/module-show only — planning paths take Reconciler::fill_planned_versions):"
  echo "$unfenced"
fi

# 5. Structured-output coverage table — every cmd_* function in cli/ must
#    appear in .claude/rules/structured-output-coverage.md's table.
#    Only match file-scope definitions (no leading whitespace) to avoid
#    matching test helper functions inside #[cfg(test)] blocks.
# LC_ALL=C: comm requires both inputs in the same collation as the sort that
# produced them; locale-aware sort/comm skew can falsely flag interleaved
# `_`-bearing rows as unsorted, so pin byte collation across both sorts and comm.
cmds_in_code=$(rg --type rust --color never -n \
      '^(pub(\(crate\)|(\(super\)))? fn |fn )cmd_' \
      crates/cfgd/src/cli/ --glob '!**/tests.rs' --glob '!**/tests/**' \
      2>/dev/null \
      | sed -E 's/.*fn cmd_([a-z0-9_]+).*/\1/' | LC_ALL=C sort -u)
rule_file=".claude/rules/structured-output-coverage.md"
if [ -f "$rule_file" ]; then
    # The whole file is the table; row cells are lowercase, the header cell is not.
    # `|| true` keeps an empty match from tripping pipefail into aborting the run —
    # an empty table must be reported as total coverage loss, not swallowed.
    cmds_in_table=$(grep -E '^\| [a-z]' "$rule_file" \
        | awk -F'|' '{print $2}' | tr -d ' ' | LC_ALL=C sort -u) || cmds_in_table=""
    missing=$(LC_ALL=C comm -23 <(echo "$cmds_in_code") <(echo "$cmds_in_table" | tr ' ' '_'))
    if [ -n "$missing" ]; then
        log_error "Commands missing from structured-output coverage table in $rule_file:"
        echo "$missing"
    fi
    # The other direction: a row for a command that no longer exists. The table
    # is read as the inventory of what cfgd exposes, so a stale row describes a
    # payload no consumer can ever receive — and it hides the rename that left
    # it behind, since the new name trips the check above while the old one sits
    # there looking answered.
    stale=$(LC_ALL=C comm -13 <(echo "$cmds_in_code") <(echo "$cmds_in_table" | tr ' ' '_'))
    if [ -n "$stale" ]; then
        log_error "Rows in $rule_file naming a cmd_* that no longer exists in crates/cfgd/src/cli/:"
        echo "$stale"
    fi
else
    log_error "Structured-output coverage table missing: $rule_file"
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

log_section "Test-home-safe blocking dispatch (workspace)"
# Raw `tokio::task::spawn_blocking` drops the test-home
# thread-local on the worker thread, so any closure that resolves `~`/$HOME
# (default_state_dir, default_config_dir, …) silently touches the real
# filesystem under tests. Production code must use
# `crate::spawn_blocking_with_test_home` instead. Escape hatch (mirrors the
# native-ok convention): when the closure provably resolves no home paths,
# annotate the call line or the line directly above it with
#   // spawn-blocking-ok: <why the closure resolves no home paths>
# util/paths.rs (the wrapper's own home) and test files are excluded.
# Marker handling is the shared one (see AWK_LIB): in a comment, with a reason,
# and inherited only from a comment line directly above — never from a previous
# call that happened to carry its own marker.
raw_spawns=$(while IFS= read -r -d '' rsfile; do
    case "$rsfile" in
        */util/paths.rs|*/tests.rs|*_test.rs|*/test_*.rs|*/tests_*.rs|*/test_helpers.rs|*/tests/*) continue ;;
    esac
    strip_test_blocks_from_file "$rsfile" | awk "$AWK_LIB"'
        { code = code_only($0); comment = LAST_COMMENT }
        code ~ /tokio::task::spawn_blocking/ &&
        !is_comment_line($0) &&
        !marker_applies(comment, prev, prev_comment, "spawn-blocking-ok:") { print }
        { prev = $0; prev_comment = comment }
    '
done < <(find crates/*/src -name '*.rs' -print0 2>/dev/null))
if [[ -n "$raw_spawns" ]]; then
    log_error "Raw tokio::task::spawn_blocking (use cfgd_core::spawn_blocking_with_test_home, or annotate // spawn-blocking-ok: <why>):"
    echo "$raw_spawns" | head -10
else
    log_ok "No raw spawn_blocking in workspace production code"
fi

log_section "Sleep-as-synchronization in test code (sleep-ok:)"
# `thread::sleep`/`tokio::time::sleep` used as a guess at how long some other
# thread needs is the flaky-timing-assertion shape (a 408.9ms-vs-400ms
# concurrency failure under a loaded suite). Reach for the observables
# catalogued in shared-utils.md instead: ConcurrencyWitness, a channel/oneshot
# handshake, await_queued_path_writer, await_blocking_source_acquire, or a
# bounded deadline-poll on the thing that actually changed. Escape hatch for
# a genuinely real-time subject (a token-bucket refill, a deliberate timeout
# exercise): the call line or the line directly above it, single-line —
#   // sleep-ok: <why no observable exists>
# Anchored to TEST CODE ONLY via extract_test_blocks_from_file — a sleep in
# production is a different concern (and none exist outside a controlled
# retry/backoff loop already reviewed elsewhere).
sleep_violations=$(while IFS= read -r -d '' rsfile; do
    extract_test_blocks_from_file "$rsfile" | awk "$AWK_LIB"'
        { code = code_only($0); comment = LAST_COMMENT }
        code ~ /thread::sleep\(|tokio::time::sleep\(/ &&
        !is_comment_line($0) &&
        !marker_applies(comment, prev, prev_comment, "sleep-ok:") { print }
        { prev = $0; prev_comment = comment }
    '
done < <(find crates/*/src -name '*.rs' -print0 2>/dev/null) | sed '/^$/d')
if [[ -n "$sleep_violations" ]]; then
    log_error "thread::sleep/tokio::time::sleep in test code (flaky timing sync — use the observables catalogued in shared-utils.md: ConcurrencyWitness, a channel/oneshot handshake, await_queued_path_writer, await_blocking_source_acquire, a bounded deadline-poll — or annotate // sleep-ok: <why no observable exists>):"
    echo "$sleep_violations" | head -20
else
    log_ok "No unguarded sleep-as-synchronization in test code"
fi

log_section "Raw Printer capture-buffer reads in test code (raw-capture-ok:)"
# A test's Printer::for_test* capture buffer (conventionally named `buf`) is
# an Arc<Mutex<String>> the printer writes into; reading it any way OTHER than
# cfgd_core::test_helpers::captured_text(&buf) bypasses its ANSI-stripping and
# poison-recovery, which is exactly the raw-lock idiom this gate rejects:
# `<recv>.lock().unwrap()` / `.expect("...")` / `.unwrap_or_else(...)`, where
# <recv>'s own name contains "buf" (the codebase-wide convention for a
# Printer capture handle — see shared-utils.md's Test guards section).
# `.clear()` is a WRITE (resetting the buffer for reuse), not a read, and is
# not gated. Escape hatch, single-line, on the call line or directly above:
#   // raw-capture-ok: <asserting ON the escapes>
# — for a test whose subject IS the raw ANSI bytes (captured_text would strip
# the very thing being asserted on), or a buffer that is provably not a
# Printer text capture (e.g. an Arc<Mutex<Vec<u8>>> tracing-log sink, which
# captured_text does not even type-check against).
# test_helpers.rs is excluded: it is captured_text's own implementation, the
# one legitimate raw read the helper itself performs.
raw_capture_violations=$(while IFS= read -r -d '' rsfile; do
    case "$rsfile" in
        */test_helpers.rs) continue ;;
    esac
    extract_test_blocks_from_file "$rsfile" | awk "$AWK_LIB"'
        { code = code_only($0); comment = LAST_COMMENT }
        code ~ /([A-Za-z_][A-Za-z0-9_.]*)?[Bb]uf[A-Za-z0-9_]*\.lock\(\)\.(unwrap\(\)|unwrap_or_else\(|expect\()/ &&
        code !~ /\.clear\(\)/ &&
        !is_comment_line($0) &&
        !marker_applies(comment, prev, prev_comment, "raw-capture-ok:") { print }
        { prev = $0; prev_comment = comment }
    '
done < <(find crates/*/src -name '*.rs' -print0 2>/dev/null) | sed '/^$/d')
if [[ -n "$raw_capture_violations" ]]; then
    log_error "Raw Printer capture-buffer read in test code (use cfgd_core::test_helpers::captured_text(&buf), or annotate // raw-capture-ok: <why>):"
    echo "$raw_capture_violations" | head -20
else
    log_ok "No raw Printer capture-buffer reads in test code"
fi

log_section "PATH-guarded resolution asserts in test code (path-guard-ok:)"
# A test asserting a SUCCESSFUL command_path/command_available/require_tool
# resolution reads the process-global PATH; without path_env_read_guard() (or
# the mutation guard) a concurrent test emptying PATH to drive a
# command-not-found branch can land between the read and the assertion and
# flip a should-pass resolution to a false negative. A test asserting FAILURE
# needs no guard — an empty PATH cannot turn a miss into a hit.
#
# Function-scoped: walks each #[test] fn's body (brace-depth tracked) inside
# the test-only corpus, and flags it only if a positive-assertion shape
# appears WITHOUT a guard call anywhere in the same function body. An assertion
# is tracked from `assert…!(` to its closing paren, so the call it asserts on
# counts wherever inside it the formatter put it. Escape hatch, anywhere in
# that same span — on the call line, on the `assert…!(` line, or on a comment
# line directly above either:
#   // path-guard-ok: <negative assertion / guard held by harness>
path_guard_violations=$(while IFS= read -r -d '' rsfile; do
    extract_test_blocks_from_file "$rsfile" | awk "$AWK_LIB"'
        function reset_fn() {
            in_fn = 0; depth = 0; has_positive = 0; has_guard = 0
            delete positive_lines; positive_lines_n = 0
            delete positive_marked; in_assert = 0; assert_bal = 0; assert_marked = 0
        }
        function flush_fn() {
            if (in_fn && has_positive && !has_guard) {
                for (k = 1; k <= positive_lines_n; k++) {
                    if (!positive_marked[k]) print positive_lines[k]
                }
            }
            reset_fn()
        }
        # A resolution call whose SUCCESS is being claimed. The `file:line:`
        # prefix, the crate path and ALL whitespace come off first, so
        # `! crate::command_available(` reads as the negative it is and a
        # parenthesised `(command_available(` still reads as positive.
        function positive_call(line,   c) {
            c = line
            sub(/^[^:]*:[0-9]+:/, "", c)
            gsub(/cfgd_core::|crate::/, "", c)
            gsub(/[[:space:]]/, "", c)
            if (c ~ /(^|[^!])command_available\(/) return 1
            if (c ~ /(^|[^!])command_path\([^)]*\)\.is_some\(\)/) return 1
            if (c ~ /(^|[^!])require_tool\([^)]*\)\.is_ok\(\)/) return 1
            return 0
        }
        BEGIN { reset_fn() }
        {
            code = code_only($0); comment = LAST_COMMENT
            is_fn_start = (code ~ /^[^:]*:[0-9]+:[[:space:]]*(pub[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+[A-Za-z_][A-Za-z0-9_]*[[:space:]]*\(/)
            if (!in_fn && is_fn_start) {
                in_fn = 1; depth = 0
            }
            if (in_fn) {
                # Outside an assertion, only an unwrapping resolution claims
                # success on its own.
                is_positive = (code ~ /(cfgd_core::|crate::)?command_path\([^)]*\)\.(expect|unwrap)\(/) || \
                    (code ~ /(cfgd_core::|crate::)?require_tool\([^)]*\)\.(expect|unwrap|is_ok)\(/)
                # An assertion is a SPAN, not a line: rustfmt wraps a long one
                # over several, and a gate that only ever read the `assert!(`
                # line reported OK forever for every wrapped shape. Balance the
                # parens from the macro to its close and judge each line inside.
                line_marked = 0
                if (!in_assert) {
                    if (match(code, /assert(_eq|_ne)?!\(/)) {
                        rest = substr(code, RSTART)
                        assert_bal = gsub(/\(/, "(", rest) - gsub(/\)/, ")", rest)
                        if (positive_call(code)) is_positive = 1
                        if (assert_bal > 0) {
                            in_assert = 1
                            # The marker written where the docs say to write it
                            # — on the macro line or directly above it — has to
                            # reach the call line the span later flags.
                            assert_marked = marker_applies(comment, prev, prev_comment, "path-guard-ok:")
                            line_marked = assert_marked
                        }
                    }
                } else {
                    if (positive_call(code)) is_positive = 1
                    if (carries_marker(comment, "path-guard-ok:")) assert_marked = 1
                    line_marked = assert_marked
                    assert_bal += gsub(/\(/, "(", code) - gsub(/\)/, ")", code)
                    # A marker belongs to ONE assertion; the next one in the
                    # same function argues for itself.
                    if (assert_bal <= 0) { in_assert = 0; assert_marked = 0 }
                }
                if (is_positive) {
                    positive_lines_n++
                    positive_lines[positive_lines_n] = $0
                    positive_marked[positive_lines_n] = line_marked || \
                        marker_applies(comment, prev, prev_comment, "path-guard-ok:")
                    has_positive = 1
                }
                if (code ~ /path_env_read_guard\(\)|path_env_mutation_guard\(\)/) has_guard = 1
                opens = gsub(/{/, "{", code)
                closes = gsub(/}/, "}", code)
                depth += opens - closes
                if (depth <= 0 && opens + closes > 0) flush_fn()
            }
            prev = $0; prev_comment = comment; prev_code = code
        }
        END { flush_fn() }
    '
done < <(find crates/*/src -name '*.rs' -print0 2>/dev/null) | sed '/^$/d')
if [[ -n "$path_guard_violations" ]]; then
    log_error "Test asserts a successful command_path/command_available/require_tool resolution without path_env_read_guard()/path_env_mutation_guard() (races a concurrent test's PATH mutation — see shared-utils.md's ProbePath section, or annotate // path-guard-ok: <why>):"
    echo "$path_guard_violations" | head -20
else
    log_ok "Every positive command_path/command_available/require_tool assertion is PATH-guarded"
fi

cli_mod="crates/cfgd/src/cli/mod.rs"
cli_ref="docs/cli-reference.md"

# ONE walk of `pub enum Command { … }`, consumed by both gates below.
#
# Emits one TAB-separated record per top-level variant:
#     <line>\t<Variant>\t<command-name>\t<long_about state>
# where the state is one of `ok` / `missing` / `not-inline` / `no-examples`,
# preceded by a `count\t<n>` record and, on failure, `__ENUM_NOT_FOUND__`.
#
# Derivation: walk the enum body by brace depth; at depth 1 accumulate the
# pending `#[command(...)]` attribute (multi-line, tracked by paren balance)
# and attach it to the next depth-1 `Pascal` line. The command NAME is that
# attribute's `name = "..."` when present (MachineConfig → machineconfig,
# McpServer → mcp-server) and the lowercased variant otherwise. The `name` key
# is matched with a leading-boundary anchor so `value_name = "WHEN"` — a key
# every flag-carrying variant may set — cannot be mistaken for it.
#
# Only the top-level enum is scanned; nested subcommand enums are out of scope.
# Assumes rustfmt's default 4-space variant indent — the count cross-check
# below is the tripwire if that ever drifts.
cli_command_records() {
    awk '
        !in_enum && /^pub enum Command[[:space:]]*\{/ { in_enum = 1; entered = 1; depth = 1; next }
        !in_enum { next }
        { line = $0; opens = gsub(/{/, "{", line); closes = gsub(/}/, "}", line) }

        depth == 1 && !collecting && /^[[:space:]]*#\[command\(/ {
            collecting = 1; attr = ""; paren = 0; pending = ""
        }
        collecting {
            attr = attr "\n" $0
            if (match($0, /(^|[^_[:alnum:]])name[[:space:]]*=[[:space:]]*"[^"]+"/)) {
                nm = substr($0, RSTART, RLENGTH)
                sub(/^.*[^_[:alnum:]]?name[[:space:]]*=[[:space:]]*"/, "", nm)
                sub(/"$/, "", nm)
                pending = nm
            }
            paren += gsub(/\(/, "(")
            paren -= gsub(/\)/, ")")
            if (paren <= 0) { collecting = 0 }
            depth += opens - closes
            next
        }

        depth == 1 && /^[[:space:]]{4}[A-Z][A-Za-z0-9]*([[:space:]]*[({,]|[[:space:]]*$)/ {
            variant = $0
            sub(/^[[:space:]]+/, "", variant)
            sub(/[[:space:]]*[({,].*$/, "", variant)
            sub(/[[:space:]]+$/, "", variant)

            # Isolate the long_about VALUE so `Examples:` is tested against IT
            # and not against some other key (`about = "… Examples: …"`).
            la = attr
            has_la = (attr ~ /long_about[[:space:]]*=/)
            sub(/.*long_about[[:space:]]*=[[:space:]]*/, "", la)
            state = !has_la ? "missing" : (la !~ /^"/ ? "not-inline" : (la !~ /Examples:/ ? "no-examples" : "ok"))

            printf "%d\t%s\t%s\t%s\n", NR, variant, (pending != "" ? pending : tolower(variant)), state
            seen++
            attr = ""; pending = ""
            depth += opens - closes
            if (depth <= 0) { in_enum = 0 }
            next
        }

        { depth += opens - closes; if (in_enum && depth <= 0) in_enum = 0 }
        END {
            if (!entered) { print "__ENUM_NOT_FOUND__"; exit }
            printf "count\t%d\n", seen + 0
        }
    ' "$cli_mod"
}

# Ground-truth variant count that CANNOT be truncated the way the walk above
# can. rustfmt closes a top-level item with a bare `}` at column 0, so the enum
# body is a line RANGE, and counting variants inside it never consults a brace:
# an unbalanced `{` or `}` inside a doc comment or a `long_about` literal ends
# the depth walk early, and a walker that stops after one variant reports every
# name it saw as documented — a green, vacuous pass. The earlier cross-check
# derived its "independent" count from the same depth walk, so it agreed with
# the walker exactly when the walker was wrong.
#
# Attribute bodies are skipped by bracket balance (`#[` … `]`), which is a
# different delimiter from the one at risk, so a brace inside an attribute's
# string cannot reach this count either.
cli_ground_truth_variant_count() {
    awk '
        !in_enum && /^pub enum Command[[:space:]]*\{/ { in_enum = 1; entered = 1; next }
        !in_enum { next }
        /^\}/ { in_enum = 0; next }
        !in_attr && /^[[:space:]]*#\[/ { in_attr = 1; bracket = 0 }
        in_attr {
            bracket += gsub(/\[/, "[")
            bracket -= gsub(/\]/, "]")
            if (bracket <= 0) { in_attr = 0 }
            next
        }
        /^[[:space:]]*\/\// { next }
        /^[[:space:]]{4}[A-Z][A-Za-z0-9]*([[:space:]]*[({,]|[[:space:]]*$)/ { n++ }
        END { if (!entered) print "__ENUM_NOT_FOUND__"; else print n + 0 }
    ' "$cli_mod"
}

log_section "CLI long_about/Examples coverage (every top-level Command variant)"
# CLAUDE.md convention: "Every top-level Command variant carries long_about
# with an Examples: block." This gate enforces it as a regression guard so the
# `cfgd skill` / `cfgd <kind> validate` surfaces (and every future variant)
# can't ship without a worked example in `--help`.
#
# Each mis-shape gets a DISTINCT, accurate message:
#   - no `#[command(...)]` / no `long_about=`     → "missing long_about"
#   - value is not an inline `"..."`              → "must be an inline string
#     literal …" (include_str!/const are rejected so the Examples: block stays
#     greppable — the whole point of this gate; do not relax this).
#   - inline value lacks `Examples:`              → "long_about lacks …"
#
# Never a silent pass: a missing enum, and any disagreement with the
# brace-independent ground-truth count, are both hard errors.
if [[ -f "$cli_mod" ]]; then
    cli_records=$(cli_command_records)
    cli_ground_truth=$(cli_ground_truth_variant_count)
    cli_walked=$(awk -F'\t' '$1 == "count" { print $2 }' <<<"$cli_records")
    if grep -q '__ENUM_NOT_FOUND__' <<<"$cli_records$cli_ground_truth"; then
        log_error "CLI gates could not locate 'pub enum Command {' in $cli_mod (renamed or brace reflowed?); gates did not run"
        cli_records=""
    elif [[ "$cli_walked" != "$cli_ground_truth" ]]; then
        log_error "Command-variant count mismatch (walker:$cli_walked ground-truth:$cli_ground_truth) in $cli_mod — a brace inside a doc comment or long_about literal can truncate the walk, hiding variants from both CLI gates"
        cli_records=""
    elif [[ "${cli_ground_truth:-0}" -eq 0 ]]; then
        log_error "Extracted zero variants from 'pub enum Command' in $cli_mod (CLI gates could not run)"
        cli_records=""
    fi

    if [[ -n "$cli_records" ]]; then
        long_about_gaps=$(awk -F'\t' -v f="$cli_mod" '
            $1 == "count" { next }
            $4 == "missing"     { printf "  %s:%s: %s — missing long_about\n", f, $1, $2 }
            $4 == "not-inline"  { printf "  %s:%s: %s — long_about must be an inline string literal containing an Examples: block (found include_str!/const)\n", f, $1, $2 }
            $4 == "no-examples" { printf "  %s:%s: %s — long_about lacks an \"Examples:\" block\n", f, $1, $2 }
        ' <<<"$cli_records")
        if [[ -n "$long_about_gaps" ]]; then
            log_error "Top-level Command variants missing long_about/Examples: (CLAUDE.md CLI convention):"
            printf "%s\n" "$long_about_gaps"
        else
            log_ok "Every top-level Command variant has long_about with an Examples: block"
        fi
    fi
else
    log_error "CLI enum file not found: $cli_mod (long_about gate could not run)"
fi

log_section "cli-reference.md covers every top-level Command variant"
# docs/cli-reference.md opens by promising "Every top-level command has an entry
# here". That promise decayed silently once already — eleven commands (alias,
# backup, compliance, daemon, rollback, secret, state, man, and the three CRD
# kinds) shipped with no heading, and three more carried a bare code block where
# every sibling carried a full entry. A reader who runs `cfgd rollback --help`
# and then searches the reference has no way to tell a missing entry from a
# missing feature, so completeness is enforced here rather than promised there.
#
# A command is covered when some Markdown heading names it as `cfgd <name>` —
# one heading may cover several (the three CRD kinds share one), which is why
# the match is on the heading LINE rather than on its opening token.
#
# `cfgd mcp` is injected at runtime from brontes rather than declared in this
# enum, so it is outside this gate's reach; it is documented, and the gate that
# would cover it is a brontes-side concern.
if [[ ! -f "$cli_ref" ]]; then
    log_error "Missing $cli_ref (cli-reference coverage gate could not run)"
elif [[ -z "$cli_records" ]]; then
    log_error "No Command variants available (cli-reference coverage gate could not run — see the error above)"
else
    undocumented=""
    while IFS=$'\t' read -r _line _variant cmd _state; do
        [[ -z "$cmd" || "$_line" == "count" ]] && continue
        grep -qE "^#+ .*\`cfgd ${cmd}[\`[:space:]]" "$cli_ref" || undocumented="${undocumented}${undocumented:+, }${cmd}"
    done <<< "$cli_records"
    if [[ -n "$undocumented" ]]; then
        log_error "Top-level commands with no heading in $cli_ref: $undocumented"
    else
        log_ok "Every top-level Command variant has a heading in $cli_ref"
    fi
fi

log_section "Publisher-secret env lockstep (release.yml preflight ↔ publish-crate.yml)"
# The anodizer-action publisher secrets are enumerated as an `env:` block in
# TWO workflows: release.yml's preflight (--preflight-secrets) validates the
# full set up front, and publish-crate.yml's publish leg feeds the same set to
# the actual managers. GHA forbids sharing the block (no YAML anchors; a
# composite action has no `secrets` context; anodizer-action reads creds from
# process env by fixed names), so the two lists must stay identical by hand. A
# secret added to one but not the other is a SILENT drift: preflight passes,
# then ~40min into a real release a manager publish fails on a missing token,
# or a manager silently no-ops. This gate extracts the key set from each file
# and fails loud on any divergence, before it can reach a release run.
#
# Selector: the block is the LONGEST contiguous run of
# `KEY: ${{ secrets.* }}` env lines in each file — that isolates the 12-key
# publisher block from the isolated single-GITHUB_TOKEN env lines other jobs
# carry. Compares the sorted KEY names (not the secret values: GITHUB_TOKEN
# legitimately maps to secrets.GH_PAT).
rel_wf=".github/workflows/release.yml"
pub_wf=".github/workflows/publish-crate.yml"
longest_secret_env_block() {
    # Emit the sorted KEY names of the longest contiguous run of
    # `<KEY>: ${{ secrets.* }}` env lines in "$1".
    awk '
        /^[[:space:]]+[A-Z_]+:[[:space:]]*\$\{\{[[:space:]]*secrets\./ {
            key = $0; sub(/^[[:space:]]+/, "", key); sub(/:.*/, "", key)
            if (NR == prev + 1) { run = run "\n" key; cnt++ }
            else                { run = key;          cnt = 1 }
            if (cnt > best) { best = cnt; bestrun = run }
            prev = NR
        }
        END { if (best > 0) print bestrun }
    ' "$1" | sort
}
if [[ -f "$rel_wf" && -f "$pub_wf" ]]; then
    rel_keys=$(longest_secret_env_block "$rel_wf")
    pub_keys=$(longest_secret_env_block "$pub_wf")
    if [[ -z "$rel_keys" || -z "$pub_keys" ]]; then
        log_error "Publisher-secret env block not found in one of the workflows (selector drifted): release=$(printf %s "$rel_keys" | grep -c .) publish-crate=$(printf %s "$pub_keys" | grep -c .) keys"
    elif [[ "$rel_keys" != "$pub_keys" ]]; then
        log_error "Publisher-secret env blocks drifted between $rel_wf and $pub_wf:"
        diff <(printf '%s\n' "$rel_keys") <(printf '%s\n' "$pub_keys") | grep -E '^[<>]' || true
    else
        log_ok "Publisher-secret env blocks identical ($(printf '%s\n' "$rel_keys" | grep -c .) keys)"
    fi
else
    log_error "Publisher-secret lockstep gate could not run (missing $rel_wf or $pub_wf)"
fi

# --- One owner comparator ---
# `Owner::sort_key` is the single rule for which owner precedes which, and it is
# applied exactly once — where a phase's groups are built. A second call site is
# a second comparator: the plan preview, the `-o json` payload and the apply
# transcript all read the same groups, so one of them ordering owners for itself
# is how two surfaces come to disagree about who owns what.
#
# `Phase.groups` is a private field, so rustc already rejects a struct literal
# and a direct `groups.sort()` outside `types.rs`. What the compiler cannot see
# is a caller re-sorting what `groups()` hands back, or applying `sort_key` to
# owners it collected itself — that is what this grep catches.

log_section "Owner ordering (one comparator)"

owner_cmp_glob='!crates/cfgd-core/src/reconciler/types.rs'
if oc=$(rg --type rust -n 'sort_key\(\)|groups(\(\))?[^;]*\.sort' \
      "${CFGD_AUDIT_PATH:-crates/}" \
      --glob "$owner_cmp_glob" \
      --glob '!**/tests.rs' \
      --glob '!**/tests/**' \
      2>/dev/null) && [ -n "$oc" ]; then
  log_error "Second owner comparator (Owner::sort_key is applied only where Phase::from_actions builds the groups):"
  echo "$oc"
else
  log_ok "Owner::sort_key applied at exactly one site"
fi

# --- Every demo tape has a Taskfile target ---
# A tape is recorded through `demo/scripts/record.sh <name>`, and the Taskfile
# target wrapping it is the only thing that also runs the tape's font gate and
# the environment the tape assumes (image build, cluster or machine rig, and
# its teardown). A tape without one is re-recordable only by hand, and the
# hand-run skips exactly the checks the target exists to enforce — sync.tape
# shipped that way and sat behind a setup script nobody could reach from
# `task --list`. `init.tape` is the bare `record.sh` call (the script's
# default name), so it is matched by its own spelling.

# Every install channel anodizer publishes to has a row in README's
# Distribution table (and, for the per-user channels, a section in
# docs/installation.md); a channel anodizer has switched off (`skip: true`)
# has no row. The Nix publisher shipped for months with no row anywhere,
# and the Snap row would have outlived its publisher the same way.
log_section "Install channels (README/installation.md ↔ .anodizer.yaml publishers)"

channel_gap="$(python3 - <<'PY'
import re, sys, yaml

doc = yaml.safe_load(open(".anodizer.yaml"))

# publisher key in .anodizer.yaml -> (README marker, installation.md marker or None)
CHANNELS = {
    "homebrew_casks": ("Homebrew", "### Homebrew"),
    "aur_source": ("AUR", "### AUR"),
    "winget": ("winget", "### winget"),
    "scoop": ("Scoop", "### Scoop"),
    "chocolatey": ("Chocolatey", "### Chocolatey"),
    "nix": ("Nix", "### Nix"),
    "nfpm": ("deb / rpm / apk", "deb / rpm / apk"),
    "cloudsmiths": ("CloudSmith", None),
    "krew": ("Krew", None),
    "docker_digest": ("GHCR", None),
    "binstall": ("binstall", None),
    "mcp": ("MCP", None),
    "snapcrafts": ("Snap", "### Snap"),
}

def enabled(block):
    if isinstance(block, list):
        return any(enabled(b) for b in block)
    if not isinstance(block, dict):
        return False
    if block.get("skip") is True or block.get("disable") is True:
        return False
    return block.get("enabled", True) is not False

found = {}
def walk(node):
    if isinstance(node, dict):
        for k, v in node.items():
            if k in CHANNELS:
                found[k] = found.get(k, False) or enabled(v)
            walk(v)
    elif isinstance(node, list):
        for v in node:
            walk(v)
walk(doc)

readme = open("README.md").read()
m = re.search(r"^## Distribution\n(.*?)(?=^## )", readme, re.S | re.M)
table = m.group(1) if m else ""
install = open("docs/installation.md").read()

gaps = []
for key, (readme_marker, install_marker) in CHANNELS.items():
    if key not in found:
        continue
    has_row = readme_marker in table
    if found[key] and not has_row:
        gaps.append(f"{key}: enabled in .anodizer.yaml but README Distribution table has no '{readme_marker}' row")
    if not found[key] and has_row:
        gaps.append(f"{key}: disabled in .anodizer.yaml but README Distribution table still has a '{readme_marker}' row")
    if install_marker:
        has_section = install_marker in install
        if found[key] and not has_section:
            gaps.append(f"{key}: enabled in .anodizer.yaml but docs/installation.md has no '{install_marker}'")
        if not found[key] and has_section:
            gaps.append(f"{key}: disabled in .anodizer.yaml but docs/installation.md still has '{install_marker}'")
print("\n".join(gaps))
PY
)"
if [ -n "$channel_gap" ]; then
    log_error "Install channels out of sync with .anodizer.yaml publishers:"
    printf '%s\n' "$channel_gap"
else
    log_ok "README Distribution table and docs/installation.md match the enabled publishers"
fi

log_section "Demo tapes (one Taskfile target each)"

tape_gap=""
for tape in demo/*.tape; do
    [ -e "$tape" ] || continue
    name="$(basename "$tape" .tape)"
    if [ "$name" = "init" ]; then
        pat='^\s*- bash demo/scripts/record\.sh\s*$'
    else
        pat="^\s*- bash demo/scripts/record\.sh ${name}\s*$"
    fi
    grep -Eq "$pat" Taskfile.yml || tape_gap="${tape_gap}${tape}"$'\n'
done
if [ -n "$tape_gap" ]; then
    log_error "Demo tapes with no Taskfile target running demo/scripts/record.sh <name>:"
    printf '%s' "$tape_gap"
else
    log_ok "Every demo/*.tape is recorded by a Taskfile target"
fi

# --- Host-recorded tapes pin the theme ---
# A tape that runs cfgd on the HOST (the kind-cluster demos reach the
# demo-k8s kubeconfig instead of a container) reads the recording operator's
# own ~/.config/cfgd, so an operator whose config names a preset publishes a
# GIF in that preset. Two GIFs shipped in dracula that way. The export line
# that points at demo-k8s must also carry CFGD_THEME=default.
log_section "Demo tapes (host-recorded tapes pin CFGD_THEME)"

theme_gap=""
for tape in demo/*.tape; do
    [ -e "$tape" ] || continue
    grep -q 'cfgd-debug/demo-k8s' "$tape" || continue
    grep -Eq '^Type "export .*CFGD_THEME=default' "$tape" || theme_gap="${theme_gap}${tape}"$'\n'
done
if [ -n "$theme_gap" ]; then
    log_error "Host-recorded demo tapes whose export line does not pin CFGD_THEME=default:"
    printf '%s' "$theme_gap"
else
    log_ok "Every host-recorded demo tape pins CFGD_THEME=default"
fi

# --- Summary ---
printf "\n"
_bold; printf "=== Audit Complete: %d errors, %d warnings ===\n" "$ERRORS" "$WARNINGS"; _reset

[[ "$ERRORS" -gt 0 ]] && exit 1
exit 0
