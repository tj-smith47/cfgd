# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md).

## Implementation order

| # | Section | Rationale |
|---|---------|-----------|
| 1 | CLI UX follow-up fixes | Review findings from completed CLI UX work |
| 2 | Documentation cleanup | Consolidation, cross-references, rename |
| 3 | Ecosystem integration | Align policies/docs after CLI + source changes settle |
| 4 | Windows support | Large standalone effort, no dependencies on above |
| 5 | Upstream Kubernetes work | Deferred until after adoption (explicit trigger criteria) |

---

## CLI UX follow-up fixes

Code fixes identified by code review, dedup analysis, and gap analysis after the CLI UX implementation.

- [ ] Extract shared helpers in cli/mod.rs: `strip_script_phases()`, `show_pending_decisions()`, `show_file_diffs()`, `show_plan_tail()` — deduplicate 60+ lines between cmd_apply dry-run and cmd_plan
- [ ] Delete duplicate `parse_duration_str` tests from reconciler (7 tests duplicating lib.rs)
- [ ] Remove `--jsonpath` deprecation warning — nobody uses this tool yet, no deprecation notices needed
- [ ] Wire up module-level preApply, preReconcile, postReconcile, onChange scripts (currently only postApply is extracted from ScriptSpec during module resolution; other hooks silently discarded)
- [ ] Implement `-o wide` with distinct behavior — add extra columns to list commands (profile list, module list, module search, registry list)
- [ ] Implement daemon auto-apply so preReconcile/postReconcile hooks actually execute (currently plan is built with Reconcile context but apply is never called)
- [ ] Make `-o name` work on all structured output types — add `name` field or fallback to other identity fields for types that lack it
- [ ] Add `#[serde(rename_all = "camelCase")]` to 11 pre-existing output structs: `RollbackOutput`, `DoctorOutput`, `DoctorCheckEntry`, `SourceListEntry`, `SourceShowOutput`, `SourceResourceEntry`, `StatusOutput`, `ModuleStatusEntry`, `LogOutput`, `VerifyOutput`, `VerifyResourceEntry`
- [ ] Fix 12 pre-existing clippy warnings in test code (useless `format!`, redundant binding, `assert_eq!` with literal bool, borrowed expressions)
- [ ] `script`-based package installs (`manager: script`) should respect `--skip-scripts` — currently they execute even when `--skip-scripts` is set because they are `InstallPackages` not `RunScript`
- [ ] `PROFILE_SCRIPT_TIMEOUT` re-export in reconciler/mod.rs — use `crate::PROFILE_SCRIPT_TIMEOUT` directly at call sites instead of re-exporting as module-local const
- [ ] Plan display loop in init.rs and module.rs (3 lines each) should call `display_plan_table(plan, printer, None)` instead of duplicating the loop

---

## Documentation cleanup

- [ ] Consolidate duplicate script lifecycle content (entry schema, hook tables, timeout/continueOnError defaults, env vars) to a single canonical location; cross-reference from other docs
- [ ] Add cross-references from user-facing docs to `docs/spec/` for detailed field documentation
- [ ] Rename all internal references to `spec-reference` → `spec` in `.claude/plans/` files

---

## Ecosystem integration

- [ ] Update `policies/` for new CRD fields: ClusterConfigPolicy CRD, Module CRD `spec.signature.cosign.publicKey`, `spec.security.trustedRegistries`, MachineConfig conditions split (Reconciled, DriftDetected, ModulesResolved, Compliant), `observedGeneration` on Condition struct, DriftAlert conditions (Acknowledged, Resolved, Escalated)
- [ ] Update idiomatic naming in ecosystem files after naming audit: `moduleRef`/`configRef` style cross-references, TitleCase enums, camelCase CRD field names

---

## Windows support

Full design in [windows-support.md](windows-support.md). 26 unguarded `std::os::unix` uses across 13 files. No Kubernetes dependency — pick up when targeting Windows users.

- [ ] Phase 1 — compilation gates: `#[cfg(unix)]` on all 26 unix-specific sites. Add `cargo build --target x86_64-pc-windows-msvc` CI job
- [ ] Phase 2 — file management: symlink fallback to copy, skip Unix permission bits on NTFS
- [ ] Phase 3 — package managers: winget, chocolatey, scoop
- [ ] Phase 4 — PowerShell env integration
- [ ] Phase 5 — Windows Service daemon
- [ ] Phase 6 — CI and release: cross-compile job, `.zip` release artifact, Windows docs

---

## Upstream Kubernetes work

Deferred until after adoption. CRD versioning (v1alpha1→v1beta1 conversion webhook, dual-version serving, migration runbook) and 3 upstream KEPs (native moduleRef pod spec field, cfgdModule volume type, kubectl debug --module). Full plan in [plans/upstream-kubernetes.md](plans/upstream-kubernetes.md).

- [ ] CRD versioning and upstream KEPs (see plan for details and trigger criteria)
