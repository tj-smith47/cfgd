# Known bugs & unfixed review findings

This file is the **source of truth for unresolved issues** in this repo.

**The user-level pre-bash hook refuses `git push` while this file has unchecked
items**, unless the command includes `--allow-unfixed`.

## Workflow

When any audit, review, test failure, or manual code-read surfaces something:

1. Add to **Active** with: `<date> <source> <short description> — <file:line if known>`
2. Fix it (or get explicit user approval to defer; record the defer in the line).
3. On fix: move to **Resolved** with the resolution date.

Sources include: code-audit, deep-audit, parity-audit, gap-analysis, dedup,
security-review, claude-md-improver, manual code review, failing tests, hook
violations, user-reported issues.

## Active

### Wave B cycle 2 — 2026-04-18 — deep-audit (3 BLOCKER, 6 WARN, 6 SUGGEST)

Audit file: `.claude/audits/2026-04-v0.x/deep-audit.md`. `findings_already_tracked=0`.

- [ ] ⚠ autofix blocked 2026-04-18 deep-audit (SUGGEST S-6): `cfgd/src/cli/mod.rs` is 22,403 lines; carve out `apply`, `diff`, `status`, `verify`, `upgrade`, `init` per the pattern already established for `module`, `profile` — crates/cfgd/src/cli/mod.rs — blocked pending implementer carve plan. 22k-line refactor requires upstream design (which exports/types go pub, shared state threading, test movement); the task description explicitly permits blocking this item. Deliberately excluded from push-gate / fix-loop counters.

### Wave B cycle 2 — 2026-04-18 — dedup (0 BLOCKER, 4 WARN, 3 SUGGEST)

Audit file: `.claude/audits/2026-04-v0.x/dedup.md`. `findings_already_tracked=0`. Note: auditor scoped to actual crates (cfgd, cfgd-core, cfgd-csi, cfgd-operator); brief had listed cfgd-agent/cfgd-drift which don't exist. Dedup S2 (stderr_lossy leftovers) is rolled into deep-audit S-2 above.


### Wave B cycle 2 — 2026-04-18 — rust-safety-scanner (0 BLOCKER, 3 WARN)

Audit file: `.claude/audits/2026-04-v0.x/safety.md`. `findings_already_tracked=0`. Cycle-1 Gateway DB rework confirmed holding. No `unsafe`/`static mut`/`unwrap` in lib code; clippy correctness/suspicious/perf baseline is zero. The 3 SUGGESTs are decided no-action (accept-loop JoinHandle drop is benign; Windows FFI `unsafe` blocks carry correct SAFETY comments; `#[cfg(test)]` env unsafes are Rust 2024 requirement) — not tracked here.


### Wave B cycle 2 — 2026-04-18 — ux-consistency (9 BLOCKER, 8 WARN, 6 SUGGEST)

Audit file: `.claude/audits/2026-04-v0.x/ux-consistency.md`. `findings_already_tracked=0`. BLOCKERs 1–5 are regressions against cycle-1's "long_about + Examples on all subcommands" fix (Examples were added but never validated against the actual subcommand surface). BLOCKER 9 is a GAP in that same cycle-1 fix (secondary binary was not swept).


## Resolved

### Archived 2026-04-18

