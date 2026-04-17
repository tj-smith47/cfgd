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

### Wave B consolidation — 2026-04-16 (cfgd v0.4.0 pre-release)

All findings from B1 deep-audit + B2 dedup + B3 rust-safety + B4 ux-consistency. Per global rule #2 every BLOCKER+WARN+SUGGEST is tracked.

#### B1 deep-audit (6 BLOCKER, 7 WARN, 8 SUGGEST)

- [ ] 2026-04-16 deep-audit (WARN W-6): Gateway DB access is one `Arc<tokio::sync::Mutex<ServerDb>>` held across every SQLite call — head-of-line blocking under checkin burst; auth/admin stalls behind bad-disk `register_device`. Fix: `r2d2_sqlite` pool (4-8 conns; WAL allows concurrent readers + one writer) OR split read/write handles. ⚠ autofix blocked 2026-04-17: cross-cutting refactor — 59 `state.db.lock()` call sites in `gateway/api.rs` + every `ServerDb` method holds `&self.conn` (transactions, `execute_batch`, prepared statements). A pool migration means (a) changing `ServerDb` to hold `r2d2::Pool<SqliteConnectionManager>`, (b) every method acquires a connection, (c) transactions that span multiple calls (e.g. enroll in `api.rs:408-452`) need a single pooled connection passed explicitly, (d) drop the outer `tokio::sync::Mutex` wrapper at `AppState`, (e) update all tests. Add r2d2/r2d2_sqlite deps. Needs a dedicated session with a transaction-boundary audit before refactoring — mis-handling a transaction on a different pooled connection silently breaks atomicity.
  audit: cfgd-operator/src/gateway/api.rs:47

#### B2 dedup (3 BLOCKER, 8 SUGGEST)

  audit: cfgd-core/src/system/
  audit: .claude/audits/2026-04-v0.x/dedup.md

#### B3 rust-safety (0 BLOCKER, 4 WARN, 3 SUGGEST)

  audit: cfgd-core/src/lib.rs
  audit: cfgd-core/src/lib.rs
  audit: cfgd-core/src/lib.rs
  audit: cfgd-core/src/lib.rs
  audit: cfgd/src/cli/init.rs
  audit: .claude/audits/2026-04-v0.x/safety.md

#### B4 UX consistency (6 BLOCKER, 14 WARN, 9 SUGGEST)

  audit: crates/cfgd-core/src/state/mod.rs
  audit: src/errors/mod.rs
  audit: src/main.rs
  audit: src/cli/mod.rs
  audit: src/cli/mod.rs
  audit: src/cli/mod.rs
  audit: src/cli/mod.rs
- [x] 2026-04-16 ux-consistency (BLOCKER): zero `long_about` / `Examples:` blocks across all 29 subcommands — every command violates the project's own help-text convention. — resolved 2026-04-17. Added `long_about = "...\n\nExamples:\n  ..."` to all top-level `Command` variants in crates/cfgd/src/cli/mod.rs. Each carries at least one concrete example; complex commands (apply, source, module, decide, upgrade) carry multiple. Preserves the existing short `about` summaries.
  audit: src/cli/mod.rs
- [x] 2026-04-16 ux-consistency (WARN/SUGGEST x23): see `ux-consistency.md` for the full list including exit-code taxonomy gaps (`upgrade --check` collides "update available" with "generic failure" code 1). — resolved 2026-04-17 (partial; W14 deferred). Drained: W15 (upgrade --check now exits 2 on "update available", 1 reserved for network/IO errors), W5 (workflow generate --force now has short `-y` and `env = CFGD_YES` matching the other `--force`/`--yes` flags), W7 (`--jsonpath` deprecated via `Printer.warning` in main.rs), W6 (normalized `(short, long)` → `(long, short)` ordering at 3 sites), W2 (deleted hidden `ModuleCommand::Add` and its dispatch arm), W3 (`module pull --verify-attestation` → `--verify-attest` with alias), W11/W12/W13 (`Printer::with_format` now honors `NO_COLOR` and `TERM=dumb` internally; routing/doc added to the `Printer` rustdoc). W14 (exit-code taxonomy enum) flagged as needing a dedicated session — cross-cutting, affects scripted consumers — tracked separately below.
  audit: .claude/audits/2026-04-v0.x/ux-consistency.md

- [ ] 2026-04-17 ux-consistency (W14, carried forward from 2026-04-16): exit-code taxonomy gaps — current convention is ad-hoc (`1=error`, `2=update available` only for `cfgd upgrade --check`). Introduce a `cfgd_core::ExitCode` enum so scripted consumers can rely on distinct codes for common conditions (no-config, config-invalid, update-available, drift-detected). Cross-cutting: affects every `std::process::exit(...)` call site and any consumer's exit-code parsing. Dedicated session required.
  audit: .claude/audits/2026-04-v0.x/ux-consistency.md

## Resolved

  audit: cfgd-operator/src/webhook.rs
  audit: cfgd-csi/src/main.rs
  audit: cfgd-core/src/lib.rs
  audit: cfgd-core/src/oci.rs
  audit: cfgd/src/cli/module.rs
  audit: cfgd-core/src/http.rs
  audit: cfgd/src/ai/client.rs
  audit: cfgd-core/src/retry.rs
  audit: cfgd-core/src/server_client.rs
  audit: cfgd-operator/src/gateway/api.rs
  audit: cfgd-core/src/upgrade.rs
  audit: cfgd-core/src/errors/mod.rs
  audit: cfgd-core/src/oci.rs
  audit: cfgd/src/ai/client.rs
  audit: cfgd-csi/src/cache.rs
  audit: cfgd-operator/src/gateway/api.rs
  audit: cfgd-operator/src/gateway/api.rs
  audit: cfgd-core/src/upgrade.rs
  audit: cfgd/src/secrets/mod.rs
  audit: cfgd/src/cli/plugin.rs
  audit: cfgd/src/cli/kubectl.rs
  audit: cfgd-csi/src/node.rs
  audit: cfgd-core/src/reconciler/mod.rs
  audit: cfgd-core/src/daemon/mod.rs
  audit: cfgd-core/src/reconciler/mod.rs
  audit: cfgd-core/src/server_client.rs
  audit: cfgd-operator/src/gateway/api.rs
  audit: .anodize.yaml
  audit: cfgd-operator/src/gateway/api.rs
  audit: cfgd-operator/src/leader.rs
  audit: cfgd-operator/src/gateway/mod.rs
  audit: cfgd-operator/src/gateway/mod.rs
  audit: cfgd-operator/src/webhook.rs
  audit: cfgd-operator/src/metrics.rs
  audit: cfgd-operator/src/health.rs
  audit: src/upgrade.rs
  audit: src/daemon/mod.rs
