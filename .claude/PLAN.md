# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md).

---

## Active Plans

- [Test Coverage — Target 90%](plans/2026-03-30-test-coverage.md) — 79% → 90% with meaningful tests
- [Upstream Kubernetes KEP](plans/upstream-kubernetes.md) — KEP for upstream k8s node configuration

## Single-item future work

- **Kubernetes Tier 5 — CRD versioning & graduation.** Conversion webhook
  v1alpha1 → v1beta1, dual-served storage, migration runbook. Gated on
  3+ months production-use per `kubernetes-first-class.md:1117`. Not
  implementable today; tracked here so it isn't lost when graduation
  criteria approach.

## E2E Test Infrastructure

All suites passing as of 2026-04-04 (GHA run 23987616584):
- Node: 40 pass | Helm: 6 pass | Server: 7 pass
- Operator: 48 pass | Full-Stack: 46 pass
- Crossplane: 14 pass | Gateway: 30 pass | CLI: all suites pass
