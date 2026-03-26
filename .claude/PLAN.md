# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md). Deferred future work in [future/](future/).

## Deep-dive completion audit

**Next session task:** Independently and thoroughly validate that each plan listed below is actually done in full — not just "looks done." Deep-dive each one: read the plan's checklist items, then verify the implementation exists, is wired up from entry points, has tests, and has no gaps or bugs. Even if a plan's checkboxes are all checked in COMPLETED.md, verify against the actual codebase.

Plans to audit (all under `.claude/plans/` unless noted):

1. **E2E Test Improvements** — `2026-03-25-e2e-test-improvements.md` — 7 tasks, 50+ subtasks. Verify every test ID exists in the actual test scripts and runs.
2. **E2E Test Reorganization** — `2026-03-25-e2e-test-reorganization.md` — 25+ domain files target. Verify if sharding happened or if tests are still monolithic.
3. **E2E K3s Migration** — `2026-03-23-e2e-k3s-migration.md` — 8 tasks. Verify setup-cluster.sh, helpers.sh, manifests, and CI workflows all reflect k3s (not KIND).
4. **Windows Plan 1 (Foundations)** — `2026-03-22-windows-support-plan-1-foundations.md` — 10 tasks. Verify platform abstractions in lib.rs, daemon IPC, script execution, self-upgrade, CI job, release targets.
5. **Windows Plan 2 (Features)** — `2026-03-22-windows-support-plan-2-features.md` — 10 tasks. Verify winget/chocolatey/scoop impls, PowerShell env, registry/service configurators, Windows daemon, docs, schema.
6. **Helm Chart Fixes** — `2026-03-23-helm-chart-fixes.md` — 11 tasks. Verify CRD schema changes, PackageRef typing, chart values, validation logic.
7. **Linux Desktop Configurators** — `2026-03-23-linux-desktop-configurators.md` — 3 tasks. Verify gsettings, kdeConfig, xfconf SystemConfigurator impls exist with tests.
8. **Module Controller** — `kubernetes-first-class.md` section 3.5. Verify if implemented or still deferred (expected: deferred until OCI pipeline).
9. **OCI Artifact Signing** — referenced in COMPLETED.md. Verify if sigstore-rs integrated or still deferred (expected: deferred to Phase C).
10. **Phase 9 (Multi-source)** — `sources/` and `composition/` modules. Verify if multi-source orchestration works end-to-end or is still prep-only.

For each plan: read the plan file, then grep/read the actual implementation files it references. Report: fully done, partially done (with specific gaps), or not started.
