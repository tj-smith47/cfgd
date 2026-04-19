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

exit 0
