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

(none)

## Resolved

### Resolved 2026-05-07 — Apply/Plan `--context` parity + 4 surfaced spec items

User decision on the `--context` asymmetry: **(A) add `--context` to `ApplyArgs`**
to match the "apply what you previewed" intuition. Resolved alongside the four
surfaced spec/prompt items:

  `crates/cfgd/src/cli/mod.rs` — `ApplyArgs` gained `--context` (default
  `"apply"`, accepts `apply`/`reconcile`) mirroring `PlanArgs`. `cli/apply.rs`
  parses the value into `ReconcileContext` once and threads the parsed value
  through both `reconciler.plan(...)` and `reconciler.apply(...)` (replacing
  the two prior hardcoded `ReconcileContext::Apply` sites). Apply's
  `long_about` Examples block now lists `cfgd apply --context reconcile`. All
  25 `ApplyArgs { ... }` test fixtures updated with `context: "apply".to_string()`.

  `chart/cfgd/templates/agent-daemonset.yaml` — `host-proc` and `host-sys`
  mountPaths relocated from `/proc`/`/sys` to `/host/proc`/`/host/sys` per
  archived 2026-03-23 helm-chart-fixes-design.md. The agent providers
  (`system/node/{seccomp,kernel_modules,apparmor,sysctl}.rs`) continue to
  read `/proc`/`/sys` directly — privileged + hostPID gives the container
  the host kernel view through its own procfs/sysfs, and `/host/proc`/
  `/host/sys` are now explicit "this is the host view" accessors (no
  shadowing). Pre-launch v1alpha1, no migration.

  `crates/cfgd-core/src/reconciler/env_files.rs` — added `fish_in_use()`
  helper: Unix branch keeps the `$SHELL contains "fish"` check (canonical on
  Unix); Windows branch consults `command_available("fish")` (covers
  `fish.exe` automatically via the function's Windows extension fallback).
  `reconciler/env.rs` and `reconciler/verify.rs` both call the new helper
  at their respective fish-env-file generation sites, so planning and
  verification stay symmetric.

  `docs/installation.md` consolidating all install channels: Linux/macOS
  (Homebrew, install script, cargo, direct download), Windows (winget,
  Scoop, Chocolatey, direct download — including the
  `Microsoft.VCRedist.2015+.x64` runtime requirement note), cosign
  signature verification, and the cluster-side Helm/Krew paths. Linked
  from README.md "Documentation" table. Surfaces winget package id
  `TJSmith.cfgd`, scoop bucket `tj-smith47/scoop-bucket`, and the
  chocolatey package id from `.anodizer.yaml`.

- [x] **Windows: file logging + opt-in Event Log subscriber.** User
  redirected the prior session's "doc-only deviation" call: instead of
  documenting "we picked file over Event Log," BOTH sinks are now
  supported. The default remains the file appender at
  `%LOCALAPPDATA%\cfgd\daemon.log` (the safer path — no `unsafe` FFI needed
  to run cfgd); `spec.daemon.windowsEventLog: true` opts into a second
  `tracing` Layer that mirrors every event into the Windows Event Log
  under the `cfgd` source. Layer impl in
  `crates/cfgd-core/src/daemon/service/windows_eventlog.rs` (90-line
  `tracing` Layer wrapping `RegisterEventSourceW` / `ReportEventW` /
  `DeregisterEventSource`); composition in
  `crates/cfgd-core/src/daemon/service/windows.rs::init_windows_logging`
  (file always installs first, Event Log layer wraps via
  `tracing-subscriber::registry`); registration in
  `install_windows_service` (reads the config flag, bakes
  `--enable-event-log` into the service binPath, and sets
  `HKLM\SYSTEM\CurrentControlSet\Services\EventLog\Application\cfgd`'s
  `EventMessageFile=%SystemRoot%\System32\EventCreate.exe` so Event
  Viewer renders messages cleanly without a custom resource DLL);
  cleanup in `uninstall_windows_service` (deregisters the source for
  clean removal). `DaemonConfig` gained `windows_event_log: bool` (no-op
  on Unix); `cfgd daemon install` surfaces the active sink mode in its
  success output. `CFGD_WINDOWS_EVENT_LOG=1` env var allows ad-hoc
  enabling without reinstalling the service. Spec §Logging rewritten in
  the archived `2026-03-22-windows-support-design.md` to describe both
  sinks; user-facing docs in `docs/daemon.md` walk through both modes,
  including `Get-WinEvent` / Event Viewer access patterns and the
  enterprise-vs-solo tradeoff table. `.claude/specs/` remains empty;
  the spec stays archived.

## Archived

### Resolved 2026-05-06 (state/ decomposition follow-ups + missed-by-triage)

  (N-1 from 2026-05-06 state/ decomposition audit).** Two types that derived
  `Serialize` without `#[serde(rename_all = "camelCase")]` (`ModuleStateRecord`,
  `ComplianceHistoryRow`) gained the attribute. Five internal-only DAOs
  (`SourceConflictRecord`, `SourceConfigHash`, `FileBackupRecord`, `JournalEntry`,
  `ModuleFileRecord`) gained doc comments declaring "not exposed via -o json"
  with a pointer to add `Serialize` + camelCase if surfaced through a CLI
  command. `FileBackupRecord` doc additionally calls out the `Vec<u8>` content
  blob that justifies its non-serializable status. No behavior change; tighter
  policy contract.

  — `StateError::DirectoryNotWritable` mis-applied to read/unlink paths
  (N-2) and `StateError::Database` mis-applied to JSON encoder failures
  (EXTRA_FOUND).** Added two new `StateError` variants:
  `FilesystemIo { path, source: std::io::Error }` for read/atomic_write/unlink
  paths in `pending_config.rs`, and `Serialize { context: &'static str, source:
  serde_json::Error }` for the `to_string_pretty` / `from_str` round-trip in
  the same file. `pending_config.rs` re-routed all 5 error sites to the
  new variants. User-facing error messages now correctly state
  `state filesystem I/O failed at <path>: <io::Error>` and
  `state serialization failed (<context>): <serde::Error>` instead of the
  misleading "directory not writable" / "database error".

  documentation split (ux-consistency cycle 2 SUGGEST).** Already addressed in
  commit `6e81def`; missed by 2026-05-06 triage agent (line-range mismatch
  caused by intervening file decomposition). Both fields carry rustdoc paragraphs
  explaining the asymmetry: Init defaults to `"master"` because it materializes
  the config dir up-front and needs a concrete ref; SourceAdd stays
  `Option<String>` so syncs follow `origin/HEAD`. Verified resolved 2026-05-06.

### Resolved 2026-05-06 (Wave B cycle 2 audit drains — confirmed via triage)

  triage on 2026-05-06 against current master confirmed all 12 non-S6 findings
  fixed in master since 2026-04-18 (mostly 2026-04-19 → 2026-04-30):
  - B-1 (web auth non-constant-time): `gateway/web/mod.rs:18-23` `secret_eq`
    helper using `sha256_hex + ct_eq`
  - B-2 (admin username charset): `validate_username` at `gateway/api/mod.rs:375`,
    called from `tokens.rs:12`, `user_keys.rs:12`, `enroll.rs:117`
  - B-3 (session cookie raw API key + Secure): `gateway/web/mod.rs:26-30`
    random session id; cookie includes `Secure; HttpOnly; SameSite=Strict`
  - W-1 (signature-verify panic swallowed): `gateway/api/enroll.rs:211-233`
    matches join, logs panic/cancel, returns Internal
  - W-2 (SSE serialize swallow): `gateway/api/fleet.rs:96-108` matches result,
    logs error, skips frame
  - W-3 (device_id_js JS escape): `gateway/web/mod.rs:390` uses `serde_json::to_string`
  - W-4 (record_source_conflict swallow): `cli/helpers.rs:507-521` logs on Err
  - W-5 (touch_atime swallow): `cfgd-csi/src/cache.rs:216` returns `io::Result`,
    callers log; tests added
  - W-6 (operator main eprintln): `cfgd-operator/src/main.rs:243-269` defers
    via captured Option, emits via `tracing::warn!` once subscriber up
  - S-1 (gen_crds print exemption): rustdoc with explicit Hard-Rule reasoning
  - S-2 (stderr_lossy leftovers): zero remaining ad-hoc callers
  - S-3 (controller error_policy_*): `controllers/mod.rs:61` `make_error_policy<K>`
    generic; 5 callers use it
  - S-4 (CFGD_DRY_RUN partial): env var deleted entirely
  - S-5 (GPG diagnostics thin): `gateway/api/enroll.rs:447-475` uses `.output() +
    stderr_lossy_trimmed`
  - S-6 (cli/mod.rs 22k lines): see prior Resolved 2026-04-30 + 2026-05-05/06
    Steps 11–12 (now 1,672 lines)

  triage 2026-05-06 confirmed all 8 findings addressed:
  - W1 (find_X / X_available / X_cmd 4× duplication): `packages/shared/mod.rs:18`
    `resolve_tool_with_fallbacks` shared helper
  - W2 (CLI config-YAML mutation pattern): `cli/source/helpers.rs:236`
    `with_source_config`; module registry uses `for_each_yaml_file`
  - W3 (`<profiles_dir>/*.yaml` loop 6×): `cfgd-core/src/config/root.rs:114`
    `for_each_yaml_file`; 6 callers migrated
  - W4 (Prometheus histogram boilerplate): `cfgd-csi/src/metrics.rs:29`
    `long_duration_histogram`; `cfgd-operator/src/metrics.rs:48`
    `short_duration_histogram`
  - S1 (parse_duration wrappers): kept + documented per audit decision
  - S2 (stderr_lossy at 3 sites): zero ad-hoc callers
  - S3 (label key strings): `util/constants.rs:8-13` `LABEL_*` constants
  - S4 (yaml||yml ext predicate): `is_yaml_ext` at `config/root.rs:102`

  SUGGEST)** — triage 2026-05-06 confirmed all 3 WARNs fixed:
  - WARN (handle_health_connection lock-across-await): `daemon/health_ipc.rs:70-169`
    clones out of guard before await; `/drift` wraps StateStore in spawn_blocking
  - WARN (file-watcher lock-across-notify): `daemon/mod.rs:586-606` mutex scope
    ended before notification
  - WARN (CSI node blocking on async runtime): `cfgd-csi/src/node/mod.rs:289-322,349-357`
    publish/unpublish wrapped in spawn_blocking
  - 3 SUGGESTs were originally "no action" decisions; remain so.

  — triage 2026-05-06 confirmed all 9 BLOCKERs and 7 of 8 WARNs fixed; 1 WARN
  recategorized to SUGGEST and tracked in Active above:
  - All 5 long_about/Examples truthfulness BLOCKERs (`module`/`secret`/`daemon`/
    `source`/`generate`): rewritten in `cli/mod.rs:441,450,487,496,623`
  - Workflow generate `branches: [main]`: `cli/workflow.rs:16-17` derives via
    `cfgd_core::detect_default_branch`
  - `secret decrypt` stdout via `printer.info`: `cli/secret.rs:43`
    `printer.stdout_line(plaintext)`
  - `Printer::progress_bar` Quiet-gate: `output/progress.rs:9-14` gates on
    Quiet + `stderr_is_terminal`
  - `kubectl-cfgd` plugin long_about/Examples gap: `cli/plugin/mod.rs` covers
    PluginCli + 5 PluginCommand variants
  - 7 WARNs (`generate --yes` env, `inquire::*` bypass, `prompt_*` is_structured,
    `--jsonpath` deprecation help-text, `USER` env, `CFGD_BOOTSTRAP_TOKEN`
    naming, `module export --format` shadowing): all addressed
  - 1 WARN (Apply/Plan asymmetry): `--from` parity closed, `--context`
    asymmetry remains as Active SUGGEST above
  - 5 of 6 SUGGESTs (compliance diff arg names, verbose count flag, source/
    registry rm alias, registry verb-noun doc, `Printer::spinner` is_terminal):
    fixed
  - 1 SUGGEST (Init.branch / SourceAddArgs.branch documentation split):
    remains as Active SUGGEST above

### Resolved 2026-04-30


### Resolved 2026-05-03 — disk: stale agent worktrees consuming 87 GB

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

### Archived 2026-04-18

