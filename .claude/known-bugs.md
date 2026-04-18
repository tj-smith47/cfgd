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

- [x] 2026-04-16 deep-audit (WARN W-6): Gateway DB access is one `Arc<tokio::sync::Mutex<ServerDb>>` held across every SQLite call — head-of-line blocking under checkin burst. — resolved 2026-04-17 (commits a89abc6, 9d9a81e, 67c42f2, 9616955, c1d22b6). Split read/write pool: r2d2 reader pool (default 16, env `CFGD_GATEWAY_DB_READ_POOL_SIZE`, `connection_timeout=5s`, `query_only=ON`) + dedicated writer behind `parking_lot::Mutex`. Every `ServerDb` method is async; SQLite work runs inside `tokio::task::spawn_blocking`; every statement uses `prepare_cached`. Multi-call atomicity via `with_read_tx` / `with_write_tx` closure APIs — migrated enroll, checkin (previously non-transactional — latent bug fixed), record_drift_event, dashboard, device_detail. PRAGMAs: `synchronous=FULL` preserved for credential+audit durability; `mmap_size=256MiB`, `temp_store=MEMORY`, `PRAGMA optimize` on reader release. `GatewayError::PoolExhausted` → HTTP 503 with `cfgd_operator_gateway_db_pool_wait_seconds` histogram; `cfgd_operator_gateway_db_writer_wait_seconds` + `cfgd_operator_gateway_db_pool_in_use{role=reader}` gauge (1-Hz sampler) expose writer-contention observability. Five regression tests added: rollback atomicity (enroll, checkin), reader/writer non-blocking, metric emission under contention, pool-exhaustion→503. `cargo test -p cfgd-operator`: 499 passed. `cargo clippy -p cfgd-operator --all-targets -- -D warnings`: clean. Also fixed bin/lib module dup in main.rs (declared parallel `mod X;` trees) — root cause for a temporary `#[allow(dead_code)]` workaround that was removed in 67c42f2.
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

- [x] 2026-04-17 ux-consistency (W14, carried forward from 2026-04-16): exit-code taxonomy gaps — current convention is ad-hoc (`1=error`, `2=update available` only for `cfgd upgrade --check`). Introduce a `cfgd_core::ExitCode` enum so scripted consumers can rely on distinct codes for common conditions (no-config, config-invalid, update-available, drift-detected). Cross-cutting: affects every `std::process::exit(...)` call site and any consumer's exit-code parsing. Dedicated session required. — resolved 2026-04-17. Introduced `cfgd_core::exit::{ExitCode, exit_code_for_error}` with a six-variant enum locked by unit test (`stable_wire_values`): `Success=0`, `Error=1`, `UpdateAvailable=2`, `NoConfig=3`, `ConfigInvalid=4`, `DriftDetected=5`. `CfgdError::Config(ConfigError::NotFound)` → `NoConfig`; other `CfgdError::Config(_)` → `ConfigInvalid`; everything else → `Error`. Kept `anyhow` at the CLI boundary (Hard Rule #4) — `exit_code_for_anyhow` lives in `cfgd/src/main.rs`, downcasts to `CfgdError` before mapping. Replaced ad-hoc `std::process::exit(1)` in `main.rs` and `exit(2)` in `cmd_upgrade` with the enum. Added `--exit-code` / `-e` flag to `cfgd diff`, `cfgd status`, and `cfgd verify` (git-diff convention: opt-in DriftDetected exit; default remains 0 for back-compat). Required refactors: `CfgdFileManager::diff` → `Result<bool>`, `print_package_drift` → `bool`. Long_about help text for upgrade/status/diff/verify documents exit codes inline. `plugin.rs` kubectl-exec passthrough left unchanged (forwards the inner tool's code — out of scope for cfgd's own taxonomy, documented in the module rustdoc). Coverage: 6 unit tests in `cfgd-core/src/exit.rs` + 6 integration tests in `crates/cfgd/tests/cli_integration.rs` that exercise the actual binary with assert_cmd to lock `NoConfig=3`, `ConfigInvalid=4`, `Success=0`, `--exit-code no-drift=0`, and help-text documentation. User-facing docs updated in `docs/cli-reference.md` with an Exit Codes section including a CI example. `cargo fmt` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` (4,666 tests) all clean.
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
