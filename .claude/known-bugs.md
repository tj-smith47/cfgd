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

### 2026-05-03 — disk: stale agent worktrees consuming 87 GB — ✅ resolved

Source: out-of-band session (anodizer side) found `/` at 100% during a cfgd
snapshot rebuild. `du` traced 87 GB to `.claude/worktrees/` — 11 locked
agent worktrees, each carrying its own `target/` build cache. SHAs all
point at commits already on `master`; no in-flight agent work.

**Resolved 2026-05-03**: cleaned up via `git worktree remove --force` over
all 11 `.claude/worktrees/agent-*` paths + `git branch -D` over the matching
`worktree-agent-*` branches. Each branch tip was first verified against
master (direct ancestor or patch-id match for cherry-picks) — no work lost.
Disk freed: 149 GB → 70 GB. Final state: only the parent worktree at
`/opt/repos/cfgd` remains.

Standing question still open: should `audit-wave` / `parity-fix` /
file-decomposition skills auto-prune their worktrees on completion?
Currently they leak — every dispatch adds a fresh `agent-<hash>` dir
that never gets removed. Worth filing a follow-up against the skill
authors.

### Wave B cycle 2 — 2026-04-18 — dedup (0 BLOCKER, 4 WARN, 3 SUGGEST)

Audit file: `.claude/audits/2026-04-v0.x/dedup.md`. `findings_already_tracked=0`. Note: auditor scoped to actual crates (cfgd, cfgd-core, cfgd-csi, cfgd-operator); brief had listed cfgd-agent/cfgd-drift which don't exist. Dedup S2 (stderr_lossy leftovers) is rolled into deep-audit S-2 above.


### Wave B cycle 2 — 2026-04-18 — rust-safety-scanner (0 BLOCKER, 3 WARN)

Audit file: `.claude/audits/2026-04-v0.x/safety.md`. `findings_already_tracked=0`. Cycle-1 Gateway DB rework confirmed holding. No `unsafe`/`static mut`/`unwrap` in lib code; clippy correctness/suspicious/perf baseline is zero. The 3 SUGGESTs are decided no-action (accept-loop JoinHandle drop is benign; Windows FFI `unsafe` blocks carry correct SAFETY comments; `#[cfg(test)]` env unsafes are Rust 2024 requirement) — not tracked here.


### Wave B cycle 2 — 2026-04-18 — ux-consistency (9 BLOCKER, 8 WARN, 6 SUGGEST)

Audit file: `.claude/audits/2026-04-v0.x/ux-consistency.md`. `findings_already_tracked=0`. BLOCKERs 1–5 are regressions against cycle-1's "long_about + Examples on all subcommands" fix (Examples were added but never validated against the actual subcommand surface). BLOCKER 9 is a GAP in that same cycle-1 fix (secondary binary was not swept).


## Resolved

### Resolved 2026-04-30

- [x] Wave B cycle 2 — 2026-04-18 — deep-audit (SUGGEST S-6): `cfgd/src/cli/mod.rs` carve-out of `apply`, `diff`, `status`, `verify`, `upgrade`, `init`. Drained across commits `aa6db7c` (upgrade), `b085caf` (verify), `72c64de` (diff), `02a1dfc` (status), `8b72fb4` (apply). `init` was already carved before the batch (no-op). `mod.rs` shrank from 22,499 → 21,368 lines (−1,131). Reviewer-flagged cleanup items also drained in a follow-up: hoisted `ModuleStatus` in status.rs, dedup'd `open_state_store` in apply.rs, consolidated count computation + upgraded tests to capture printed output in verify.rs.

### Archived 2026-04-18

