# E2E Test Coverage Expansion — Design Spec

**Status:** APPROVED
**Date:** 2026-03-27
**Goal:** Full E2E coverage across all cfgd components. Every feature tested so regressions are visible.

## Current State

394 E2E tests across 5 suites:
- **CLI:** 307 tests, 30 domain files — comprehensive command/flag coverage
- **Node:** 40 tests, 7 domain files — binary, sysctl, kernel modules, seccomp, certs, daemon
- **Operator:** 18 tests, 8 domain files — CRDs, basic webhooks, OCI
- **Full-Stack:** 24 tests, 6 domain files — health, fleet, drift, CSI, kubectl plugin, debug
- **Crossplane:** 5 tests, 1 script — TeamConfig fan-out only

Infrastructure is solid: k3s cluster, deterministic image tags, label-scoped cleanup, per-run namespaces, reusable CI setup workflow.

## Coverage Gaps

Organized into three risk tiers by "likelihood of silent breakage x blast radius."

### Tier 1 — High Risk / Zero Coverage
- Gateway HTTP API (20 endpoints, no dedicated tests)
- Webhook admission (5 endpoints, only 3 basic tests)
- CSI driver edge cases (only 2 happy-path tests)
- Multi-namespace policy evaluation (never tested across namespace boundaries)

### Tier 2 — Medium Risk / Partial Coverage
- Operator controller lifecycle (leader election, graceful shutdown, metrics)
- OCI supply chain cluster-side (CLI push → Module CRD → CSI mount)
- Daemon reconciliation loop (drift detect → auto-apply → hooks → checkin)
- Source composition & merge conflicts
- Helm chart lifecycle (upgrade, rollback, values combinations)

### Tier 3 — Low Risk / Thin Coverage
- Crossplane error paths and status propagation
- Rollback depth (multi-step, permissions, symlinks)
- Compliance depth (drift interaction, export format, diff verification)
- Generate/MCP server (stub files, no actual tests)
- Error paths and edge cases across all suites
- Secret backend detection and error handling

## Approach

Risk-prioritized tiers. Each tier is a self-contained deliverable — all tests in a tier can be implemented and merged independently. Tier 1 first because it buys the most confidence per test written.

## Test Design

### Conventions

All new tests follow the existing framework:
- Use `begin_test`, `pass_test`, `fail_test`, `skip_test` from `helpers.sh`
- CLI tests: standalone bash scripts in `tests/e2e/cli/scripts/`, run as subprocesses
- K8s tests: sourced domain files in their suite's `scripts/` directory
- Test IDs are globally unique prefixes (no collisions across suites)
- Tests gated behind credentials use `skip_test` when env var is unset
- Each new domain file is independently runnable for local debugging

### Credential-Gated Tests

Three tests require external service accounts:
- **GEN05, GEN06** — require `ANTHROPIC_API_KEY` for AI generate flow
- **SEC10** — requires `OP_SERVICE_ACCOUNT_TOKEN` for 1Password integration

Pattern: check env var at test start, `skip_test` with clear message if unset. CI adds secrets when accounts are provisioned.

---

## Tier 1: High Risk / Zero Coverage (46 new tests)

### 1.1 Gateway HTTP API — New Suite

**Location:** `tests/e2e/gateway/scripts/`
**Infrastructure:** Dedicated ephemeral namespace, gateway deployment via Helm chart values, port-forward for HTTP access, curl-based assertions.
**CI:** New `gateway-tests` job in `e2e.yml`, depends on `setup`, 20 min timeout.
**Files:**
- `setup-gateway-env.sh` — namespace, deploy gateway, port-forward, bootstrap token
- `test-health.sh` — GW-01
- `test-enrollment.sh` — GW-02 through GW-06, GW-19, GW-20
- `test-checkin.sh` — GW-07 through GW-10, GW-18
- `test-api.sh` — GW-11 through GW-13, GW-16, GW-17
- `test-streaming.sh` — GW-14
- `test-dashboard.sh` — GW-15
- `run-all.sh` — sources all domain files

| ID | Test | Assertion |
|---|---|---|
| GW-01 | Health endpoints | `/healthz` and `/readyz` return 200 |
| GW-02 | Token-based enrollment | `POST /api/v1/enroll` with valid bootstrap token returns 200, API key in response |
| GW-03 | Enrollment with invalid token | Returns 401 or 403 |
| GW-04 | Enrollment with SSH key signature | Signs challenge with ssh-keygen, enrollment succeeds |
| GW-05 | Enrollment with GPG key signature | Signs challenge with gpg, enrollment succeeds |
| GW-06 | Duplicate enrollment rejection | Second enroll with same device returns conflict |
| GW-07 | Device checkin (happy path) | `POST /api/v1/checkin` with valid API key returns 200, device status updated |
| GW-08 | Checkin with drift report | Drift details in payload → DriftAlert CRD created in cluster |
| GW-09 | Checkin with compliance data | Compliance snapshot in payload → stored, queryable via API |
| GW-10 | Checkin with invalid API key | Returns 401 |
| GW-11 | Device list API | `GET /api/v1/devices` returns enrolled devices as JSON array |
| GW-12 | Device detail API | `GET /api/v1/devices/:id` returns device with lastCheckin timestamp |
| GW-13 | Drift events API | `GET /api/v1/drift` returns drift events for enrolled device |
| GW-14 | SSE event stream | Connect to `/api/v1/events/stream`, trigger checkin, receive event within 10s |
| GW-15 | Web dashboard loads | `GET /` returns 200 with HTML containing device inventory elements |
| GW-16 | Admin device removal | `DELETE /api/v1/admin/devices/:id` removes device, subsequent GET returns 404 |
| GW-17 | Fleet status aggregation | Multiple enrolled devices → `/api/v1/fleet/status` returns aggregate counts |
| GW-18 | Checkin updates MachineConfig status | Enrolled device checks in → MachineConfig conditions updated via operator |
| GW-19 | Enrollment credential rotation | Re-enroll with new key → old API key returns 401 |
| GW-20 | Auth boundary | Unauthenticated requests to `/api/v1/devices` return 401 |

### 1.2 Webhook Admission — Expand Operator Suite

**Location:** `tests/e2e/operator/scripts/test-webhooks.sh` (append to existing)

| ID | Test | Assertion |
|---|---|---|
| OP-WH-04 | MachineConfig: missing hostname | kubectl apply rejected by validation webhook |
| OP-WH-05 | MachineConfig: invalid moduleRef format | Rejected |
| OP-WH-06 | MachineConfig: valid spec accepted | Passes webhook validation |
| OP-WH-07 | ConfigPolicy: empty targetSelector | Rejected |
| OP-WH-08 | ConfigPolicy: valid spec accepted | Passes |
| OP-WH-09 | DriftAlert: missing machineConfigRef | Rejected |
| OP-WH-10 | DriftAlert: valid spec accepted | Passes |
| OP-WH-11 | ClusterConfigPolicy: invalid namespaceSelector | Rejected |
| OP-WH-12 | ClusterConfigPolicy: valid spec accepted | Passes |
| OP-WH-13 | Module: invalid OCI reference format | Rejected |
| OP-WH-14 | Module: valid spec accepted | Passes |
| OP-WH-15 | Mutation webhook: defaults injected | Create minimal MachineConfig → stored object has defaults populated |

### 1.3 CSI Driver Edge Cases — Expand Full-Stack Suite

**Location:** `tests/e2e/full-stack/scripts/test-csi.sh` (append to existing)

| ID | Test | Assertion |
|---|---|---|
| FS-CSI-03 | Multi-module volume mount | Pod with 2 module volumes → both mounted, contents correct |
| FS-CSI-04 | Module cache hit | Mount same module twice → second uses cache (verify via metrics counter) |
| FS-CSI-05 | Invalid module reference | Volume referencing nonexistent module → pod Pending with Warning event |
| FS-CSI-06 | Module update propagation | Update Module CRD → new pod gets updated content |
| FS-CSI-07 | CSI driver metrics | `/metrics` endpoint returns `csi_operations_total` counters |
| FS-CSI-08 | CSI identity probe | gRPC Probe() returns ready (via health endpoint) |
| FS-CSI-09 | Volume unmount cleanup | Delete pod → volume unmounted cleanly, no orphan mounts |
| FS-CSI-10 | ReadOnly volume enforcement | Mount with readOnly=true → writes inside container fail |

### 1.4 Multi-Namespace Policy Evaluation — Expand Operator Suite

**Location:** `tests/e2e/operator/scripts/test-clusterconfigpolicy.sh` (append to existing)

| ID | Test | Assertion |
|---|---|---|
| OP-NS-01 | ConfigPolicy scoped to namespace | Policy in ns-a has no effect on MachineConfig in ns-b |
| OP-NS-02 | ClusterConfigPolicy spans namespaces | Cluster policy matches MachineConfigs across ns-a and ns-b |
| OP-NS-03 | Namespace selector filtering | ClusterConfigPolicy with label selector → only labeled namespaces matched |
| OP-NS-04 | Policy priority resolution | Namespace policy + cluster policy on same MC → correct winner per priority |
| OP-NS-05 | Policy compliance counting | ClusterConfigPolicy status shows compliant/total across all matched namespaces |
| OP-NS-06 | Namespace deletion cleanup | Delete namespace → cluster policy status count decreases |

---

## Tier 2: Medium Risk / Partial Coverage (39 new tests)

### 2.1 Operator Controller Lifecycle — Expand Operator Suite

**Location:** `tests/e2e/operator/scripts/test-lifecycle.sh` (new domain file, sourced by run-all.sh)

| ID | Test | Assertion |
|---|---|---|
| OP-LC-01 | Operator metrics endpoint | `/metrics` returns Prometheus text with `reconcile_total` counters |
| OP-LC-02 | Leader election lease | Lease object exists in cfgd-system with holder identity matching operator pod |
| OP-LC-03 | Graceful shutdown | Delete operator pod → new pod acquires lease, reconciliation resumes |
| OP-LC-04 | MachineConfig reconcile loop | Create MachineConfig → Reconciled condition set within 30s |
| OP-LC-05 | ConfigPolicy re-evaluation | Update MachineConfig packages → ConfigPolicy compliance count updates |
| OP-LC-06 | DriftAlert lifecycle | Create → status set → acknowledge → status updates to Acknowledged |
| OP-LC-07 | Module CRD status tracking | Create Module with ociArtifact → status shows available platforms and digest |
| OP-LC-08 | Operator health probes | `/healthz` and `/readyz` return 200 |

### 2.2 OCI Supply Chain End-to-End — Expand Full-Stack Suite

**Location:** `tests/e2e/full-stack/scripts/test-oci-e2e.sh` (new domain file)

Requires: in-cluster registry (already available at `registry.jarvispro.io`), cosign for signing tests.

| ID | Test | Assertion |
|---|---|---|
| OCI-E2E-01 | Push → Module CRD → CSI mount | cfgd module push → Module CRD created → pod mounts via CSI → content matches pushed module |
| OCI-E2E-02 | Signed artifact verification | Push with --sign → Module CRD with signature → CSI verifies before mount |
| OCI-E2E-03 | Unsigned artifact rejected | Module CRD with requireSignature → unsigned artifact → pod mount fails with event |
| OCI-E2E-04 | Multi-platform artifact | Push --platform linux/amd64,linux/arm64 → CSI selects correct platform |
| OCI-E2E-05 | Artifact digest pinning | Module CRD references digest → CSI pulls exact version |
| OCI-E2E-06 | Registry auth flow | Push to registry → Module CRD → CSI uses imagePullSecrets to pull |

### 2.3 Daemon Reconciliation Loop — Expand Node Suite

**Location:** `tests/e2e/node/scripts/test-daemon.sh` (append to existing)

All tests run inside the privileged test pod.

| ID | Test | Assertion |
|---|---|---|
| DAEMON-10 | Config file watch triggers reconcile | Daemon running → modify profile YAML in pod → daemon reconciles within interval |
| DAEMON-11 | Drift detection and auto-apply | Daemon running → externally modify managed file → daemon restores correct state |
| DAEMON-12 | Drift policy: alert-only | driftPolicy=alert → modify file → daemon logs drift, doesn't auto-apply |
| DAEMON-13 | Drift policy: ignore | driftPolicy=ignore → modify file → daemon takes no action |
| DAEMON-14 | Reconcile interval respected | interval=5s → verify reconcile happens roughly every 5s (±2s tolerance) |
| DAEMON-15 | Pre/post-reconcile hooks execute | Hooks configured → daemon reconciles → hook script output file exists |
| DAEMON-16 | On-drift hook fires | Managed file modified → on-drift hook runs → artifact file exists |
| DAEMON-17 | Daemon checkin with gateway | Daemon configured with server URL → daemon checks in periodically → gateway records device |
| DAEMON-18 | Daemon graceful stop | Send SIGTERM → daemon completes in-flight reconcile → exits 0 |

### 2.4 Source Composition & Conflicts — Expand CLI Suite

**Location:** `tests/e2e/cli/scripts/test-source.sh` (append to existing)

All tests use local git repos as source fixtures.

| ID | Test | Assertion |
|---|---|---|
| SRC-MERGE-01 | Two sources, no conflict | Disjoint packages from both → apply merges both sets |
| SRC-MERGE-02 | Package conflict, priority wins | Both provide brew:ripgrep → higher priority source's version deployed |
| SRC-MERGE-03 | File conflict, priority wins | Both manage ~/.gitconfig → higher priority source's content deployed |
| SRC-MERGE-04 | Env var conflict, priority wins | Both set EDITOR → higher priority value used |
| SRC-MERGE-05 | Override rejects source item | `source override X reject packages.brew.kubectx` → item excluded |
| SRC-MERGE-06 | Override replaces value | `source override X set env.EDITOR nvim` → overridden value used in apply |
| SRC-MERGE-07 | Opt-in filtering | Source added with --opt-in packages → only packages merged, files ignored |
| SRC-MERGE-08 | Pin version prevents upgrade | Source pinned ~1.0 → source bumps to 2.0 → update shows rejection message |

### 2.5 Helm Chart Lifecycle — Expand Full-Stack Suite

**Location:** `tests/e2e/full-stack/scripts/test-helm.sh` (new domain file)

Uses dedicated namespace per test to avoid interfering with persistent operator deployment.

| ID | Test | Assertion |
|---|---|---|
| FS-HELM-01 | Fresh install with defaults | helm install → operator + CSI daemonset running, CRDs present |
| FS-HELM-02 | Install with gateway enabled | --set gateway.enabled=true → gateway deployment + service exist |
| FS-HELM-03 | Install with gateway disabled | --set gateway.enabled=false → no gateway resources |
| FS-HELM-04 | Install with CSI disabled | --set csi.enabled=false → no CSI daemonset |
| FS-HELM-05 | Upgrade preserves CRDs | helm upgrade with new values → existing CRD instances survive |
| FS-HELM-06 | Values override propagation | Custom replica count, resources → reflected in deployment spec |
| FS-HELM-07 | Helm template validation | helm template → valid YAML for all value combinations |
| FS-HELM-08 | Helm uninstall cleanup | helm uninstall → operator/CSI/gateway removed, CRDs preserved |

---

## Tier 3: Low Risk / Thin Coverage (53 new tests)

### 3.1 Crossplane Depth — Expand Crossplane Suite

**Location:** `tests/e2e/crossplane/scripts/run-crossplane-tests.sh` (append)

| ID | Test | Assertion |
|---|---|---|
| XP-06 | Invalid TeamConfig rejected | Missing required fields → XR not created, error event |
| XP-07 | Policy tier generates ConfigPolicy | TeamConfig with policyTier → ConfigPolicy created with correct settings |
| XP-08 | Policy tier update propagates | Update policyTier → ConfigPolicy spec updated |
| XP-09 | TeamConfig status reflects members | Status shows member count and compliance summary |
| XP-10 | MachineConfig inherits team profile | Generated MachineConfig contains profile from TeamConfig spec |
| XP-11 | Duplicate member name rejected | Two members with same hostname → validation error |
| XP-12 | TeamConfig deletion cascades | Delete TeamConfig → all generated MachineConfigs and ConfigPolicy removed |
| XP-13 | Multiple TeamConfigs coexist | Two TeamConfigs in different namespaces → independent MachineConfig sets |
| XP-14 | Crossplane function health | function-cfgd pod running and healthy |

### 3.2 Rollback Depth — Expand CLI Suite

**Location:** `tests/e2e/cli/scripts/test-rollback.sh` (append)

| ID | Test | Assertion |
|---|---|---|
| RB05 | Rollback restores file content | Apply v1 → apply v2 → rollback to v1 → file content matches v1 |
| RB06 | Rollback restores file permissions | Apply mode 0600 → change → rollback → permissions restored |
| RB07 | Rollback with symlink files | Apply creates symlinks → rollback → symlinks correct |
| RB08 | Rollback log entry | Rollback → `cfgd log` shows rollback entry with apply-id |
| RB09 | Sequential rollbacks | v1 → v2 → v3 → rollback to v1 → state matches v1 |
| RB10 | Rollback of env/aliases | Apply with env vars → different env → rollback → env reverted |

### 3.3 Compliance Depth — Expand CLI Suite

**Location:** `tests/e2e/cli/scripts/test-compliance.sh` (append)

| ID | Test | Assertion |
|---|---|---|
| CO08 | Compliance after drift | Modify managed file → compliance shows non-compliant item |
| CO09 | Compliance after restore | Apply restores → compliance shows all compliant |
| CO10 | Compliance export JSON format | Export file is valid JSON with expected schema keys |
| CO11 | Compliance diff shows changes | Two snapshots → diff output shows specific changed items |
| CO12 | Compliance history --since filter | Multiple snapshots → --since 1s filters correctly |
| CO13 | Compliance with module scope | --module flag → only that module's items checked |
| CO14 | Compliance JSON matches table | -o json summary counts match table output values |

### 3.4 Generate & MCP Server — Expand CLI Suite

**Location:** `tests/e2e/cli/scripts/test-generate.sh` and `test-mcp-server.sh` (replace placeholders)

| ID | Test | Assertion |
|---|---|---|
| GEN01 | `generate --help` | Lists subcommands and flags |
| GEN02 | `generate --scan-only` | Outputs detected tools, no API call made |
| GEN03 | `generate module X --scan-only` | Detects tool-specific context |
| GEN04 | `generate` without API key | Error message mentions ANTHROPIC_API_KEY |
| GEN05 | `generate` with API key | Full conversation → produces valid module YAML (gated: ANTHROPIC_API_KEY) |
| GEN06 | `generate --model` override | Flag passed to provider (gated: ANTHROPIC_API_KEY) |
| MCP01 | `mcp-server --help` | Shows usage |
| MCP02 | MCP server initialize | Send JSON-RPC initialize on stdin → valid response on stdout |
| MCP03 | MCP server tools/list | Returns expected tool names |
| MCP04 | MCP server resources/list | Returns config/profile/module resources |
| MCP05 | MCP server invalid request | Malformed JSON-RPC → error response, server stays alive |

### 3.5 Error Paths & Edge Cases

**CLI errors — Location:** `tests/e2e/cli/scripts/test-behavioral.sh` (append)

| ID | Test | Assertion |
|---|---|---|
| ERR06 | Circular module dependency | Graceful error message, not infinite loop or stack overflow |
| ERR07 | Missing file source | Referenced file doesn't exist → clear error per file, process continues |
| ERR08 | Invalid template syntax | Tera error → message includes file path and line number |
| ERR09 | Reserved profile name | Create with reserved name → rejected with explanation |
| ERR10 | Path traversal in module file | `../../../etc/passwd` → rejected by validation |
| ERR11 | Unreachable source URL | source add with bad URL → timeout error, no hang |
| ERR12 | --skip and --only combined | Both flags → verify behavior matches documentation |

**Node errors — Location:** `tests/e2e/node/scripts/test-apply.sh` (append)

| ID | Test | Assertion |
|---|---|---|
| BIN-ERR-01 | Read-only sysctl parameter | Set immutable param → error, other params still applied |
| BIN-ERR-02 | Nonexistent kernel module | Load missing module → error with module name |
| BIN-ERR-03 | Invalid PEM certificate | Malformed cert → error, other certs still applied |
| BIN-ERR-04 | Insufficient permissions | Non-root on privileged op → clear permission error |

**Operator errors — Location:** `tests/e2e/operator/scripts/test-machineconfig.sh` (append)

| ID | Test | Assertion |
|---|---|---|
| OP-ERR-01 | Nonexistent module ref | MachineConfig status shows error condition, operator not crash-looping |
| OP-ERR-02 | Impossible selector | ConfigPolicy with no matches → status shows 0/0 |
| OP-ERR-03 | Dangling MachineConfig ref | DriftAlert for deleted MC → status shows orphaned |
| OP-ERR-04 | Rapid create/delete | Create then immediately delete CRD instance → no reconcile panic |

### 3.6 Secret Backend Detection — Expand CLI Suite

**Location:** `tests/e2e/cli/scripts/test-secret.sh` (append)

| ID | Test | Assertion |
|---|---|---|
| SEC06 | 1Password backend, op not installed | Error: "op CLI not found" |
| SEC07 | Bitwarden backend, bw not installed | Error: "bw CLI not found" |
| SEC08 | Vault backend, vault not installed | Error: "vault CLI not found" |
| SEC09 | Unknown backend name | Error: "unsupported secret backend" |
| SEC10 | 1Password full flow | op reads secret → injected (gated: OP_SERVICE_ACCOUNT_TOKEN) |

---

## CI Integration

### New CI Job: gateway-tests

Added to `.github/workflows/e2e.yml`:
- **Job name:** `gateway-tests`
- **Depends on:** `setup`
- **Runner:** `arc-cfgd`
- **Timeout:** 20 minutes
- **Script:** `tests/e2e/gateway/scripts/run-all.sh`

### Updated CI Jobs

Existing jobs run their expanded domain files automatically (sourced by existing run-all.sh):
- `operator-tests` — picks up new webhook, lifecycle, multi-namespace, error tests
- `full-stack-tests` — picks up new CSI, OCI E2E, Helm tests
- `node-tests` — picks up new daemon and error tests
- `cli-tests` — picks up expanded source, rollback, compliance, generate, MCP, error, secret tests
- `crossplane-tests` — picks up expanded Crossplane tests

### Credential-Gated Tests

| Env Var | Tests | CI Secret Name |
|---|---|---|
| `ANTHROPIC_API_KEY` | GEN05, GEN06 | `ANTHROPIC_API_KEY` |
| `OP_SERVICE_ACCOUNT_TOKEN` | SEC10 | `OP_SERVICE_ACCOUNT_TOKEN` |

Pattern in test scripts:
```bash
if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    skip_test "GEN05" "ANTHROPIC_API_KEY not set"
fi
```

## Summary

| Tier | New Tests | Cumulative |
|---|---|---|
| Tier 1 (high risk) | 46 | 440 |
| Tier 2 (medium risk) | 39 | 479 |
| Tier 3 (low risk) | 53 | 532 |
| **Total** | **138** | **532** |

New files:
- `tests/e2e/gateway/` — entire new suite (setup, 6 domain files, runner)
- `tests/e2e/operator/scripts/test-lifecycle.sh` — new domain file
- `tests/e2e/full-stack/scripts/test-oci-e2e.sh` — new domain file
- `tests/e2e/full-stack/scripts/test-helm.sh` — new domain file

Expanded files:
- `tests/e2e/operator/scripts/test-webhooks.sh`
- `tests/e2e/operator/scripts/test-clusterconfigpolicy.sh`
- `tests/e2e/operator/scripts/test-machineconfig.sh`
- `tests/e2e/full-stack/scripts/test-csi.sh`
- `tests/e2e/node/scripts/test-daemon.sh`
- `tests/e2e/node/scripts/test-apply.sh`
- `tests/e2e/cli/scripts/test-source.sh`
- `tests/e2e/cli/scripts/test-rollback.sh`
- `tests/e2e/cli/scripts/test-compliance.sh`
- `tests/e2e/cli/scripts/test-generate.sh`
- `tests/e2e/cli/scripts/test-mcp-server.sh`
- `tests/e2e/cli/scripts/test-behavioral.sh`
- `tests/e2e/cli/scripts/test-secret.sh`
- `tests/e2e/crossplane/scripts/run-crossplane-tests.sh`
- `.github/workflows/e2e.yml`
