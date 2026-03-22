---
name: implement-phase
description: Implement the next incomplete section from cfgd PLAN.md
allowed-tools: ["Bash(cargo *)", "Read", "Edit", "Write", "Glob", "Grep"]
user-invocable: true
---

## Implement the next section of cfgd

### Find the work

1. Read `/opt/repos/cfgd/.claude/PLAN.md`
2. Find the first section that has unchecked `- [ ]` items
3. That's what you're implementing

### Before writing any code — read these files in full:

1. `/opt/repos/cfgd/CLAUDE.md` — coding standards, hard rules, module map, naming conventions (camelCase serde fields, PascalCase enum values)
2. `/opt/repos/cfgd/.claude/PLAN.md` — the section you're implementing and its checklist
3. If the section says "see kubernetes-first-class.md" — read `/opt/repos/cfgd/.claude/kubernetes-first-class.md` for full design detail

Then read all existing source files relevant to the work to understand what's already implemented. Plan your implementation order — implement in dependency order (leaf modules first).

### Quality Mandate

**Quality over speed.** Every module must be production-grade when committed. Do not leave stubs, `todo!()`, placeholder implementations, or partial features. If the section's checklist items aren't fully met, the section isn't done — keep working or report what remains.

### Hard rules — non-negotiable:

- ALL terminal output goes through `output::Printer`. No `println!`, `eprintln!`, `console::*`, `indicatif::*` anywhere else.
- No `unwrap()` or `expect()` in library code. Use `?` with proper error types.
- All providers implement their respective traits. The reconciler depends on `ProviderRegistry`, never concrete impls.
- `thiserror` for library errors, `anyhow` only in `main.rs`, `cli/`, and `mcp/`.
- Config structs in `config/` only, with `serde::Deserialize` + `serde::Serialize`. Use `#[serde(rename_all = "camelCase")]` on structs; enum variants use PascalCase by default (no rename needed). No `rename_all = "kebab-case"` anywhere.
- No `std::process::Command` outside `cli/`, `packages/`, `secrets/`, `system/`, `reconciler/`, `platform/`, `sources/`, `gateway/`, `output/`, and `generate/`. See CLAUDE.md for what each is allowed to shell out to.
- Group imports: std, external crates, internal modules (separated by blank lines).
- Write unit tests alongside code in `#[cfg(test)] mod tests {}`.

### While implementing:

- Run `cargo check` after each module to catch errors early.
- Do NOT add features beyond what the current section specifies.
- Do NOT leave `#[allow(dead_code)]` — if code is unused, delete it.
- **Before writing any helper function**, read `cfgd-core/src/lib.rs` and check if a shared version already exists. If you need a function that will be used by more than one module, add it to lib.rs. See the "Shared Utilities" section in CLAUDE.md for the current inventory.
- Use `cfgd_core::utc_now_iso8601()` for timestamps. Do NOT create local wrappers.
- Use `Sha256::digest()` for hashing. Do NOT use `Sha256::new()` + `update()` + `finalize()`.
- Use `cfgd_core::command_available()` to check CLI tool availability. Do NOT redefine it.

### After implementing — pre-completion gates (ALL required):

Every gate must pass before the section is considered complete. Do not skip any.

1. **Build and lint**
   - `cargo fmt`
   - `cargo clippy -- -D warnings`
   - `cargo test --workspace` (ALL tests, not just the crate you changed)
   - E2E tests: run any applicable shell-based test suites in `tests/e2e/*/scripts/` if the environment supports it (requires Docker/kind). If not runnable, note which E2E suites exist and what they cover.

2. **Quality scripts**
   - `bash .claude/scripts/audit.sh`
   - `bash .claude/scripts/completeness-check.sh`
   - Fix anything flagged, then re-run until clean.

3. **Skill-based review gates** — invoke each of these skills:
   - `/detecting-implementation-gaps` — verify all new code is reachable from entry points, no dead implementations
   - `/deduplicating-code` — verify no duplicated logic was introduced, check for DRY violations across the full codebase
   - `superpowers:requesting-code-review` — full code review of the changes against PLAN.md and CLAUDE.md standards
   - `superpowers:verification-before-completion` — run fresh verification commands and confirm every claim with evidence

   Fix all issues found by each skill before proceeding to the next.

4. **Checklist completion** — walk through every checklist item in the section:
   - Verify each item against evidence (not assumptions).
   - Move completed items from PLAN.md to COMPLETED.md (completed items are MOVED, not duplicated).
   - If any items remain incomplete, report what remains and why.
