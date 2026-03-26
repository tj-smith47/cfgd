# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md). Deferred future work in [future/](future/).

## Deep-dive completion audit (2026-03-26) — DONE

All 10 plans audited, gaps fixed, verified on k3s.

| # | Plan | Status | Notes |
|---|------|--------|-------|
| 1 | E2E Test Improvements | **Done** | 15 missing tests written and verified on k3s. Bug fix: MC controller drift condition clearing. |
| 2 | E2E Test Reorganization | **Not started** | 0/44 domain files. Monolithic scripts unchanged. |
| 3 | E2E K3s Migration | **Done** | Stale e2e-cli.yml path trigger fixed. |
| 4 | Windows Plan 1 (Foundations) | **Done** | CI Windows job fmt/clippy steps added. |
| 5 | Windows Plan 2 (Features) | **Done** | No gaps. |
| 6 | Helm Chart Fixes | **Done** | No gaps. |
| 7 | Linux Desktop Configurators | **Done** | No gaps. |
| 8 | Module Controller | **Done** | Was expected deferred — actually implemented. |
| 9 | OCI Artifact Signing | **Done** | Uses cosign CLI (not sigstore-rs) — documented design choice. |
| 10 | Phase 9 (Multi-source) | **Done** | End-to-end wired: CLI, daemon, state store. |

Fixes applied:
- Removed stale `.github/workflows/e2e-cli.yml` path trigger from e2e.yml
- Added `cargo fmt --check` and `cargo clippy` steps to CI Windows job
- Wrote 15 E2E tests: T52-T58 (node drift compliance) and T61-T68 (full-stack compliance)
- Fixed MC controller bug: DriftDetected condition wasn't cleared when DriftAlert deleted (generation skip short-circuit)
- Verified: drift tests 14 pass/2 skip, full-stack tests 23 pass/1 skip, 229 unit tests pass

## Remaining work

### E2E Test Reorganization — deferred

Plan `plans/2026-03-25-e2e-test-reorganization.md` calls for splitting monolithic test scripts into 44 domain files. Deferred — the monolithic scripts work and CI runs them successfully.
