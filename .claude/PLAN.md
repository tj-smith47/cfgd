# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md).

## E2E test migration (KIND → k3s) — verification and CI

The E2E test code migration is complete (all scripts, workflows, helpers, manifests rewritten). What remains is fixing infrastructure issues discovered during local verification, running all test suites to green, and pushing to CI.

**What's done:**
- `tests/e2e/common/helpers.sh` rewritten (KIND helpers → kubectl pod/namespace/cleanup helpers)
- `tests/e2e/setup-cluster.sh` created (idempotent pre-flight: build, push, diff-and-apply)
- All 4 workflows rewritten (`e2e-cli.yml`, `e2e-node.yml`, `e2e-operator.yml`, `e2e-full-stack.yml`) — call reusable `e2e-setup.yml`, `runs-on: arc-cfgd`
- All test scripts migrated (`exec_on_node` → `exec_in_pod`, ephemeral namespaces, run-labeling, label-scoped cleanup)
- K8s manifests: RBAC, privileged test pod template, cert-manager webhook TLS, Helm test values
- KIND files deleted, Taskfile e2e targets added
- Production manifests updated (`/db/manifests/k3s/namespaces/cfgd-system/deployment.yaml` now uses `cfgd-operator` image with gateway env vars)
- CoreDNS fix on k3s nodes (systemd-resolved routes `cluster.local` to CoreDNS 10.43.0.10)
- Reflector annotated to replicate `registry-credentials` to all namespaces

**What remains:**

- [ ] Fix cfgd-server (gateway) health check 500 on the k3s cluster. The new `cfgd-operator:latest` image starts controllers + gateway but returns 500 on `/api/v1/devices`. The old `cfgd-server` binary (different image) works fine. Likely a DB schema mismatch — the gateway SQLite DB at `/data/cfgd-server.db` may have been created by the old binary's schema. The cfgd-operator may apply different migrations. Investigate the logs, possibly delete the PVC to start fresh, or run migrations.
- [ ] Run node server tests (T30-T35) — blocked on gateway fix above
- [ ] Run node drift tests (T40-T50)
- [ ] Run operator tests (T01-T18)
- [ ] Run full-stack tests (T01-T16)
- [ ] Verify test counts match baselines: CLI 201, binary 11, helm 6, server 6, drift 7, operator 18, full-stack 16
- [ ] Run `setup-cluster.sh` twice to verify idempotency
- [ ] Push to master and verify all 4 E2E workflows pass on ARC runners
- [ ] Verify README badges are green

**Constraints:**
- `REGISTRY` env var must be set (no hardcoded registry in code). Source `.env` for local runs.
- `CFGD_DEPLOY_MANIFESTS` env var (also in `.env`) tells setup-cluster.sh to use rollout restarts instead of applying E2E manifests directly (ArgoCD manages production deployments).
- Production manifests are in `/db/manifests/k3s/namespaces/cfgd-system/` — push to git, ArgoCD syncs. Never `kubectl set image` on ArgoCD-managed resources.
- All cluster-scoped test resources must have `cfgd.io/e2e-run` labels. Cleanup uses label selectors, never `--all`.
- Registry credentials replicated to all namespaces via Reflector (source: `jarvispro/registry-credentials`).
- k3s nodes resolve `cluster.local` via CoreDNS (systemd-resolved drop-in at `/etc/systemd/resolved.conf.d/cluster-local.conf` on all 4 nodes).
- Commit messages end with `#minor` for the final squash/push.

**Tests already passing:** CLI (201/201), binary (11/11), helm (6/6).

---

## Upstream Kubernetes work

Deferred until after adoption. CRD versioning (v1alpha1→v1beta1 conversion webhook, dual-version serving, migration runbook) and 3 upstream KEPs (native moduleRef pod spec field, cfgdModule volume type, kubectl debug --module). Full plan in [plans/upstream-kubernetes.md](plans/upstream-kubernetes.md).

- [ ] CRD versioning and upstream KEPs (see plan for details and trigger criteria)
