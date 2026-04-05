# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md).

---

## Active Plans

- [Test Coverage — Target 90%](plans/2026-03-30-test-coverage.md) — 79% → 90% with meaningful tests
- [File Size Audit and Decomposition](plans/file-size-audit-and-decomposition.md) — Split oversized modules into focused submodules
- [Upstream Kubernetes KEP](plans/upstream-kubernetes.md) — KEP for upstream k8s node configuration

## E2E Test Infrastructure

All suites passing as of 2026-04-04 (GHA run 23987616584):
- Node: 40 pass | Helm: 6 pass | Server: 7 pass
- Operator: 48 pass | Full-Stack: 46 pass
- Crossplane: 14 pass | Gateway: 30 pass | CLI: all suites pass
