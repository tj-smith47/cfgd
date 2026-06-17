#!/usr/bin/env bash
# Post-edit hook: lightweight checks on edited .rs files
set -euo pipefail

FILE="${1:-}"
[[ -z "$FILE" ]] && exit 0
[[ ! -f "$FILE" ]] && exit 0

WARNINGS=""
CRITICAL=""

# Check for println!/eprintln! outside allowed modules
# output/ owns all terminal interaction; main.rs is allowed for startup panics;
# build.rs files are allowed because println! is the cargo build-script protocol
# (emits `cargo:...` directives parsed by cargo, not terminal output)
if [[ "$FILE" != *"src/output/"* && "$FILE" != *"src/main.rs"* && "$(basename "$FILE")" != "build.rs" ]]; then
    if grep -qE 'println!\(|eprintln!\(' "$FILE" 2>/dev/null; then
        # Exclude test code (lines inside cfg(test) blocks — approximate by checking the line itself)
        VIOLATIONS=$(grep -n 'println!\|eprintln!' "$FILE" | grep -v '#\[test\]\|mod tests\|#\[cfg(test)\]\|assert' || true)
        if [[ -n "$VIOLATIONS" ]]; then
            CRITICAL+="OUTPUT VIOLATION in $FILE: println!/eprintln! found outside output/ module. Use Printer instead.\n"
        fi
    fi
fi

# Check for unwrap()/expect() outside main.rs, tests, and lib.rs
# lib.rs (cfgd-core shared utilities) has legitimate uses in test blocks only;
# the per-line grep can't distinguish block boundaries so we exclude the whole file.
if [[ "$FILE" != *"src/main.rs"* && "$FILE" != *"src/lib.rs"* ]]; then
    VIOLATIONS=$(grep -n '\.unwrap()\|\.expect(' "$FILE" | grep -v '#\[test\]\|mod tests\|#\[cfg(test)\]' || true)
    if [[ -n "$VIOLATIONS" ]]; then
        WARNINGS+="UNWRAP VIOLATION in $FILE: use ? with proper error types instead of unwrap()/expect().\n"
    fi
fi

# Check for direct console/indicatif/syntect usage outside output/
if [[ "$FILE" != *"src/output/"* ]]; then
    if grep -qE 'use (console|indicatif|syntect)::' "$FILE" 2>/dev/null; then
        WARNINGS+="ENCAPSULATION VIOLATION in $FILE: console/indicatif/syntect must only be used in output/ module.\n"
    fi
fi

# Check for anyhow outside cli/, mcp/, and main.rs
# CLAUDE.md: "anyhow::Result is only used in main.rs and cli/" — mcp/ is also a CLI boundary
if [[ "$FILE" != *"src/main.rs"* && "$FILE" != *"src/cli/"* && "$FILE" != *"src/mcp/"* ]]; then
    if grep -qE 'anyhow::' "$FILE" 2>/dev/null; then
        WARNINGS+="ERROR TYPE VIOLATION in $FILE: anyhow only allowed in main.rs, cli/, and mcp/. Use thiserror.\n"
    fi
fi

# Check for #[allow(dead_code)] — unused code should be deleted, not silenced
VIOLATIONS=$(grep -n '#\[allow(dead_code)\]' "$FILE" | grep -v '#\[cfg(test)\]\|mod tests' || true)
if [[ -n "$VIOLATIONS" ]]; then
    WARNINGS+="DEAD CODE VIOLATION in $FILE: #[allow(dead_code)] found. Delete unused code instead of suppressing the warning.\n"
fi

# Check for non-camelCase serde rename_all attributes
# CLAUDE.md: config structs use camelCase to match Kubernetes ecosystem conventions
if grep -qE 'rename_all\s*=\s*"(kebab-case|lowercase)"' "$FILE" 2>/dev/null; then
    WARNINGS+="SERDE CONVENTION in $FILE: rename_all = \"kebab-case\" or \"lowercase\" found. Config structs should use camelCase (or remove rename_all for PascalCase enums).\n"
fi

if [[ -n "$CRITICAL" ]]; then
    echo -e "$CRITICAL"
    [[ -n "$WARNINGS" ]] && echo -e "$WARNINGS"
    exit 2
fi

if [[ -n "$WARNINGS" ]]; then
    echo -e "$WARNINGS"
fi

# --- output banned patterns -------------------------------------------------
# Mirrors .claude/scripts/audit.sh rules but per-file (fast).
# Runs AFTER the CRITICAL/WARNINGS block above so its `exit 2` still wins.
# Regex shape uses an ANSI-C $'...' literal so the alternation matches
# two-or-more spaces, a real tab byte (0x09), or a backslash-t escape — the
# three canonical indent-hack shapes. Plain `grep -E` does NOT interpret \t
# inside a normal single-quoted pattern, hence the $'...'.
EDITED_FILE="${1:-}"
if [ -n "$EDITED_FILE" ] && [ -f "$EDITED_FILE" ]; then
    # Defense in depth: only inspect Rust source. The harness already
    # filters by *.rs at settings.json before invoking the hook, but
    # nothing prevents a future caller from invoking us with .md/.toml.
    case "$EDITED_FILE" in
        *.rs) ;;
        *) exit 0 ;;
    esac

    # Skip if file is one of the output module(s), or any test file.
    # Globs use `*/` (or unanchored relative) so absolute paths
    # (`/opt/repos/cfgd/crates/...`) and bare-relative paths
    # (`crates/...`) both match, without falsely exempting unrelated
    # paths that happen to contain "crates/cfgd-core/src/output/".
    case "$EDITED_FILE" in
        */crates/cfgd-core/src/output/*) exit 0 ;;
        crates/cfgd-core/src/output/*) exit 0 ;;
        *tests.rs|*/tests/*) exit 0 ;;
    esac

    # Same regexes as audit.sh.
    if grep -nE 'printer\.(success|warning|info|error|header|subheader|key_value|newline|plan_phase|stdout_line)\(' "$EDITED_FILE" > /dev/null 2>&1; then
        echo
        echo "BANNED OLD-API CALL in $EDITED_FILE"
        echo "  Replace with output vocabulary. See:"
        echo "    .claude/rules/output-module.md  (Printer surface)"
        exit 1
    fi
    if grep -nE $'printer\\.\\w+\\(\\s*&?(format!\\()?"(  |\t|\\\\t)' "$EDITED_FILE" > /dev/null 2>&1; then
        echo
        echo "INDENT HACK in $EDITED_FILE"
        echo "  This is a printer call whose arg starts with two-or-more spaces,"
        echo "  a literal tab byte, or a backslash-t escape."
        echo "  Use a section instead:"
        echo "    let sec = printer.section(\"...\");"
        echo "    sec.bullet(format!(\"{}\", x));"
        echo "  See:"
        echo "    .claude/rules/output-module.md"
        exit 1
    fi
    if grep -nE $'\\.kv\\(\\s*&?(format!\\()?"(  |\t|\\\\t)' "$EDITED_FILE" > /dev/null 2>&1; then
        echo
        echo "KV KEY INDENT HACK in $EDITED_FILE"
        echo "  kv keys must not start with whitespace. Use a subsection if you"
        echo "  need nesting:"
        echo "    .section(\"Origins\", |s| s.subsection(\"Primary\", |o| o.kv(\"Branch\", ...)))"
        exit 1
    fi
fi
# --- end output banned-patterns block ----------------------------------------

# --- path-handling: fold to '/' at cross-OS string boundaries ----------------
# A native-separator path render that becomes a resource-id / state key /
# snapshot golden / effective path / env-file body silently breaks on Windows
# (the '\' never matches its Unix-authored counterpart → drift never reconciles).
# util/paths.rs folds to '/' in one place. See .claude/rules/path-handling.md.
#
# Enforced on the DELTA, not the baseline: only newly-added native renders trip
# this, so the documented legacy uses (swept separately) don't block edits.
# output/ owns terminal rendering (native correct); tests carry no cross-OS keys.
if [ -n "${EDITED_FILE:-}" ] && [ -f "$EDITED_FILE" ]; then
    case "$EDITED_FILE" in
        */crates/cfgd-core/src/output/*|*tests.rs|*/tests/*) ;;
        */crates/cfgd-core/src/*)
            GITDIR=$(dirname "$EDITED_FILE")
            if git -C "$GITDIR" ls-files --error-unmatch "$EDITED_FILE" >/dev/null 2>&1; then
                # tracked: inspect only added lines, preserving the legacy baseline
                LEAK=$(git -C "$GITDIR" diff -U0 HEAD -- "$EDITED_FILE" 2>/dev/null \
                         | grep -E '^\+[^+]' \
                         | grep -E '\.display\(\)|to_string_lossy\(\)' \
                         | grep -v 'native-ok' || true)
            else
                # untracked new file: no legacy baseline, scan whole file
                LEAK=$(grep -E '\.display\(\)|to_string_lossy\(\)' "$EDITED_FILE" \
                         | grep -v 'native-ok' || true)
            fi
            if [ -n "$LEAK" ]; then
                echo
                echo "PATH-HANDLING in $EDITED_FILE"
                echo "  New native path render in the cross-OS library core. A path that"
                echo "  becomes a resource-id / state key / snapshot / env-file body must"
                echo "  fold to '/', or it never matches its Unix counterpart on Windows:"
                echo "    path.posix()           instead of  path.display()"
                echo "    to_posix_string(path)  instead of  path.to_string_lossy()"
                echo "    normalize_for_snapshot(captured, &[(p, label)])  for snapshot goldens"
                echo "  Genuine terminal/log output: append  // native-ok: <why>"
                echo "  See .claude/rules/path-handling.md"
                echo "$LEAK"
                exit 2
            fi
            ;;
    esac
fi
# --- end path-handling block -------------------------------------------------

exit 0
