---
name: audit
description: Run the cfgd code quality audit and fix any violations found
allowed-tools: ["Bash(bash *)", "Bash(cargo *)", "Read", "Edit", "Grep", "Glob", "Agent"]
user-invocable: true
---

## Code Quality Audit

Full codebase audit — mechanical checks AND manual design review. **Quality over speed** — every violation must be resolved, not suppressed.

### Part 1: Automated checks

1. Run `bash /opt/repos/cfgd/.claude/scripts/audit.sh`
2. Run `cargo clippy --workspace -- -D warnings`
3. Run `cargo test --workspace`
4. Fix all violations before proceeding to Part 2.

### Part 2: Cross-codebase design review

Read ALL source files across ALL crates. Do not limit your review to recently changed code — compare every module against every other module for consistency.

Check for these specific issues:

#### DRY violations (code duplication)
- **First**: read `cfgd-core/src/lib.rs` and the "Shared Utilities" section of CLAUDE.md. Check whether any module re-implements a function that already exists there (timestamps, YAML merge, command checks, etc.)
- Functions with the same shape across modules (e.g., similar error mapping patterns, similar file-read-parse-return flows)
- Duplicated logic that could share a common function in lib.rs
- Local wrappers that just delegate to a shared function (e.g., `fn chrono_now() -> String { cfgd_core::utc_now_iso8601() }`)
- Copy-pasted test setup code that could use shared helpers
- Identical struct construction patterns that suggest a builder or `Default` impl is missing

#### Design pattern deviations
- Modules that handle errors inconsistently (e.g., one returns `Result`, another panics, another swallows errors)
- Inconsistent use of `&str` vs `String` in similar function signatures
- Some modules using `tracing::` and others not, for similar operations
- Inconsistent approach to the same problem (e.g., one module uses `serde_json::Value` indexing while another deserializes into structs for the same kind of data)
- Functions that take `&Printer` but never use it (dead parameter)
- Functions that duplicate what a `Printer` method already provides
- Inconsistent hashing: all hashing should use `Sha256::digest()` one-liner
- Inconsistent DB patterns: all SQLite databases must set WAL mode and use versioned migrations
- Inconsistent timestamps: all code must use `cfgd_core::utc_now_iso8601()` directly, no wrappers
- Naming convention violations: config structs must use `#[serde(rename_all = "camelCase")]`; no `rename_all = "kebab-case"` or `rename_all = "lowercase"` anywhere; enum variants serialize as PascalCase by default (no rename needed); no kebab-case field names in user-visible strings or YAML examples

#### Cohesion issues
- Public functions/types that are only used by one caller — should they be private or moved closer?
- Modules that depend on each other's internals instead of going through public APIs
- Config structs that have grown fields belonging to different concerns
- Test code that tests implementation details instead of behavior

#### Unused or dead code

For every unused item (function, struct, error variant, config field, trait method), apply this decision tree:

1. **Is there a code path that should use this but doesn't?** (missing validation, missing enforcement, incomplete feature) → **Implement the missing code path and wire it up.** This is the most common case — the definition was correct but the call site was never written.
2. **Does the code path exist but use something else?** (e.g., CLI uses `anyhow::bail!` instead of the domain error, or inlines the logic instead of calling the function) → **Fix the call site to use the defined item**, or if the defined item is at the wrong abstraction level, delete it and keep what works.
3. **Is this purely speculative with no clear caller?** → **Delete it.** Don't keep code around for hypothetical future use.
4. **Is this part of a public API contract** (e.g., a struct field that downstream crates might use)? → **Leave it**, but add a test that constructs it.

Specific things to check:
- Public functions with no callers outside their module
- Error variants that are never constructed (check `#[from]` variants separately — those are constructed via `?` operator)
- Config struct fields that are deserialized but never read in any code path
- Trait methods that no implementation actually uses in a meaningful way
- Structs/enums defined but never instantiated
- Imports that are used only in dead paths

### Part 3: Report

For each issue found, report:
- **File:line** — exact location
- **Issue** — what's wrong
- **Fix** — specific action to take (not "consider refactoring")

Then fix all issues and re-run Part 1 to confirm nothing broke.

### When NOT to abstract
Per CLAUDE.md: "Three similar lines > a premature abstraction." Do NOT flag repetition as a DRY violation if:
- The repeated code is 3 lines or fewer
- The cases differ in types, field names, or return shapes enough that a generic abstraction would be more complex than the repetition
- The repetition is in test setup code that's clearer when explicit
