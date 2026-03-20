# cfgd: Kubernetes Ecosystem Integration

Engineering specification for cfgd as a first-class Kubernetes ecosystem participant. Covers operator operational readiness, CRD specifications, controllers, webhooks, pod module injection, OCI pipeline, CSI driver, observability, supply chain security, multi-tenancy, and operational procedures.

Crossplane integration (XRD, composition function, enrollment) is covered in § 12 of this document. For the phased work plan, see [PLAN.md](PLAN.md).

---

## Decisions Record

| Decision | Resolution | Rationale |
|---|---|---|
| API group | `cfgd.io/v1alpha1` everywhere — local files and Kubernetes resources | Single API group simplifies tooling and documentation |
| Module kind | One Module kind, same `apiVersion`/`kind` locally and in-cluster. In-cluster adds `ociArtifact` and `signature` fields (populated by `cfgd module push`, not hand-authored) | Eliminates kind translation between local and cluster |
| Module CRD scope | Cluster-scoped. RBAC controls who can publish | Modules are shared capabilities, not per-namespace resources |
| cfgd-server | Merged into cfgd-operator. Device gateway is an optional feature toggled via Helm values | One cluster-side binary reduces operational complexity |
| Helm chart | One chart at `chart/cfgd/` (repo root). Subcomponents toggled via `values.yaml` | Chart represents the product, not a single crate |
| kubectl plugin | `kubectl cfgd debug/exec/inject`. Distributed via Krew. Same binary as cfgd | No separate build; plugin mode via argv[0] detection |
| Pod module injection | CSI driver mounts content as read-only volume. If module has `scripts.post-apply`, webhook also injects init container. One path, not either/or | Avoids dual delivery mechanisms; CSI for content, init for setup |
| Identity / auth | Standard Kubernetes RBAC. No custom identity binding | Users who can create ephemeral containers can use `kubectl cfgd` |
| Module references | `--module name:version` repeatable flag. Version required. Module must exist as CRD | Explicit versioning prevents silent drift |
| Trusted registries | `spec.security.trustedRegistries` on ClusterConfigPolicy. Webhook validates Module CRD `ociArtifact` references against approved prefixes | Cluster-scoped trust is the canonical anchor; namespace policies cannot expand it |
| cfgd-server in pod flow | Not involved. Pod module injection is Kubernetes-native (CRDs, CSI, webhook). Gateway handles device checkins only | Clean separation: k8s-native for pods, gateway for devices |
| ConfigPolicy + modules | ConfigPolicy can mandate modules on pods via `spec.requiredModules[].moduleRef`. Webhook reads ConfigPolicy and injects required modules alongside pod-level annotations | Policy-driven injection without pod author cooperation |
| ClusterConfigPolicy | Cluster-scoped policy for org-wide mandates. Operator merges with namespace-scoped ConfigPolicy; cluster always wins | Org security mandates must not be overridable by namespace admins |
| CRD versioning | Start at v1alpha1. Conversion webhooks when graduating to v1beta1. Graduation criteria: 3+ months production use, stable schema for 1 month, E2E passing | Prevents premature stability promises |
| Leader election | Lease-based via `coordination.k8s.io/v1` Lease objects. Single active replica, standbys ready for failover | Required for HA; prevents dual-write on CRD status. Current `controllers/mod.rs` runs all 3 controllers without election |
| Metrics endpoint | Dedicated `/metrics` on separate HTTP port (configurable, default 8443). Prometheus text format. `prometheus-client` crate | Separate port avoids exposing metrics through webhook TLS listener |
| Graceful shutdown | `tokio::signal` for SIGTERM. Drain in-flight reconciliations (30s grace), stop webhook, flush metrics | Current `main.rs` has no signal handling. Kubernetes sends SIGTERM before SIGKILL |
| Security contexts | `runAsNonRoot: true`, `readOnlyRootFilesystem: true`, `allowPrivilegeEscalation: false`, `capabilities.drop: [ALL]`, UID 65532 | Missing from current `deployment.yaml`. Required for PSA restricted profile |
| CSI driver scope | Separate binary in `crates/cfgd-csi/`. Runs as DaemonSet. Node plugin only (no Controller plugin). Uses `tonic` for gRPC | CSI drivers must run on every node; operators run centrally. Different lifecycle, RBAC, failure domains |
| OCI artifact format | ORAS-compliant. Media type: `application/vnd.cfgd.module.v1+tar+gzip`. Config: `application/vnd.cfgd.module.config.v1+json`. One layer per target platform | Aligns with ORAS ecosystem. Same registry infrastructure orgs already have |
| CRD printer columns | All CRDs get printer columns. Short names: `mc`, `cpol`, `da`, `mod`. Category: `cfgd` for all | Current CRDs have empty `additionalPrinterColumns` and `shortNames` |

---

## Architecture

### Component Readiness

| Component | State | Code Location |
|---|---|---|
| CRD controllers (MC, CP, DA) | Implemented | `controllers/mod.rs` |
| Validation webhook | Implemented | `webhook.rs` |
| Device gateway (HTTP API) | Implemented | `gateway/` |
| Web dashboard | Implemented | `gateway/web.rs` |
| CRD definitions (3 CRDs) | Implemented | `crds/mod.rs` |
| Crossplane function (Go) | Implemented, untested | `function-cfgd/fn.go` |
| Crossplane XRD + Composition | Implemented, untested | `manifests/crossplane/` |
| Helm chart | Implemented | `chart/cfgd-operator/` |
| E2E tests (operator, full-stack) | Implemented | `tests/e2e/` |
| Server client (device side) | Implemented | `cfgd-core/src/server_client.rs` |
| Leader election | Not started | |
| Prometheus /metrics | Not started | |
| Graceful shutdown | Not started | |
| Security context hardening | Not started | |
| Module CRD | Not started | |
| ClusterConfigPolicy CRD | Not started | |
| Pod module mutating webhook | Not started | |
| CSI driver | Not started | |
| OCI module pipeline | Not started | |
| kubectl cfgd plugin | Not started | |
| Kubernetes Events emission | Not started | |
| PDB / NetworkPolicy | Not started | |

### System Diagram

```
┌─────────────────────────────────────────────────────────┐
│  ArgoCD / Flux (Platform GitOps)                        │
│  Manages: Helm chart, CRDs, RBAC, Crossplane resources │
├─────────────────────────────────────────────────────────┤
│  Crossplane (optional)                                  │
│  TeamConfig XR → composition function → per-user:       │
│    MachineConfig CRDs, ConfigPolicy CRDs, RBAC bindings │
├─────────────────────────────────────────────────────────┤
│  cfgd-operator (single cluster-side binary)             │
│    Controllers: MachineConfig, ConfigPolicy, DriftAlert,│
│                 ClusterConfigPolicy, Module              │
│    Validation webhook: CRD spec validation              │
│    Pod module webhook: annotation-driven injection      │
│    Device gateway: checkin, enrollment, drift, web UI   │
│    Metrics: /metrics on separate port                   │
├─────────────────────────────────────────────────────────┤
│  cfgd-csi (DaemonSet, separate binary)                  │
│    CSI node plugin: pull OCI artifacts, mount as volumes│
│    Node-level cache with LRU eviction                   │
├─────────────────────────────────────────────────────────┤
│  cfgd daemon (on-device)                                │
│    Pulls config from git, reconciles, reports drift     │
│    Optionally checks in to operator's device gateway    │
└─────────────────────────────────────────────────────────┘
```

---

## 1. Operator Operational Readiness

### 1.1 Leader Election

Use `coordination.k8s.io/v1` Lease objects for leader election.

- Lease name: `cfgd-operator-leader`
- Lease namespace: operator deployment namespace
- Lease duration: 15s, renew deadline: 10s, retry period: 2s
- Code change: `main.rs` acquires lease before starting controllers
- Helm: `replicaCount` can be >1 once leader election is in place

**Acceptance**: Deploy 2 replicas. Kill the leader pod. Second replica acquires lease within 15s and begins reconciling. Verified by checking Lease `holderIdentity`.

### 1.2 Graceful Shutdown

- Register `tokio::signal::ctrl_c()` and SIGTERM via `tokio::signal::unix::signal(SignalKind::terminate())`
- On signal: stop accepting webhook connections, drain in-flight reconciliations (30s configurable grace), flush metrics, exit 0
- Code change: `main.rs` wraps `tokio::select!` around controllers + signal
- Gateway's `axum::serve` supports `with_graceful_shutdown()`

**Acceptance**: Send SIGTERM to operator pod. Pod exits within 35s. No 500 errors on in-flight webhook requests.

### 1.3 Health Probes

Current state: `/healthz` on webhook HTTPS port only. Breaks when webhook is disabled.

Target:
- `/healthz` (liveness) on dedicated HTTP port (default 8081) — no TLS required
- `/readyz` (readiness) — returns 503 until leader lease acquired and CRD watches established
- Startup probe: `/healthz` with higher `failureThreshold` for slow first-reconcile

### 1.4 Security Contexts

Add to deployment template:

```yaml
securityContext:  # Pod level
  runAsNonRoot: true
  runAsUser: 65532
  runAsGroup: 65532
  fsGroup: 65532
  seccompProfile:
    type: RuntimeDefault
containers:
  - securityContext:  # Container level
      allowPrivilegeEscalation: false
      readOnlyRootFilesystem: true
      capabilities:
        drop: [ALL]
```

`readOnlyRootFilesystem: true` is safe because gateway SQLite DB is on a mounted PVC volume and webhook certs are mounted read-only.

### 1.5 PodDisruptionBudget

```yaml
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: cfgd-operator
spec:
  minAvailable: 1
  selector:
    matchLabels: <selectorLabels>
```

Only relevant when `replicaCount >= 2` (post-leader-election). Conditional on `podDisruptionBudget.enabled` in values.yaml.

### 1.6 NetworkPolicy

```yaml
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: cfgd-operator
spec:
  podSelector:
    matchLabels: <selectorLabels>
  policyTypes: [Ingress, Egress]
  ingress:
    - ports:
        - port: 9443  # webhook
        - port: 8080  # gateway (if enabled)
        - port: 8081  # health probes
      from:
        - namespaceSelector: {}  # kube-apiserver can call from any NS
  egress:
    - ports:
        - port: 443   # kube-apiserver
        - port: 6443  # kube-apiserver (common alt)
```

Conditional on `networkPolicy.enabled` in values.yaml.

### 1.7 Resource Sizing

| Scenario | Replicas | Requests | Limits |
|---|---|---|---|
| Operator-only (no gateway) | 1 | 100m CPU / 128Mi | 200m / 256Mi |
| With gateway (<100 devices) | 1 | 200m / 256Mi | 500m / 512Mi |
| With gateway (100-1000 devices) | 2 | 500m / 512Mi | 1000m / 1Gi |
| >5000 devices | Separate gateway deployment | 1Gi+ | Scale gateway horizontally |

---

## 2. CRD Specifications

### 2.1 MachineConfig (existing, needs enhancement)

**Current implementation**: `crds/mod.rs:12-81`

**Spec fields**:

| Field | Type | Required | Description |
|---|---|---|---|
| `spec.hostname` | `string` | Yes | Machine hostname |
| `spec.profile` | `string` | Yes | cfgd profile name |
| `spec.moduleRefs` | `[]ModuleRef` | No | Module references |
| `spec.moduleRefs[].name` | `string` | Yes | Module name |
| `spec.moduleRefs[].required` | `bool` | No (default false) | Policy-required flag |
| `spec.packages` | `[]string` | No | Package list |
| `spec.packageVersions` | `map[string]string` | No | Installed versions |
| `spec.files` | `[]FileSpec` | No | Managed files |
| `spec.files[].path` | `string` | Yes | Target path |
| `spec.files[].content` | `string` | No | Inline content (one of content/source) |
| `spec.files[].source` | `string` | No | Source path/URL |
| `spec.files[].mode` | `string` | No (default "0644") | Octal file mode |
| `spec.systemSettings` | `map[string]string` | No | System key-value settings |

**Status fields**:

| Field | Type | Description |
|---|---|---|
| `status.lastReconciled` | `string` (ISO 8601) | Last reconciliation timestamp |
| `status.driftDetected` | `bool` | Active drift exists |
| `status.observedGeneration` | `int64` | Last observed spec generation |
| `status.conditions` | `[]Condition` | Standard conditions |

**Enhancements needed**:

Printer columns: `NAME HOSTNAME PROFILE DRIFT READY AGE`

Short names: `mc`. Categories: `cfgd`.

Conditions (split from current single `Ready`):
- `Reconciled` — True/False, reasons: `ReconcileSuccess`, `ReconcileError`
- `DriftDetected` — True/False, reasons: `NoDrift`, `DriftActive`
- `ModulesResolved` — True/False, reasons: `AllResolved`, `ResolutionFailed`
- `Compliant` — True/False, reasons: `PolicyCompliant`, `PolicyViolation`

CEL validation:
```yaml
x-kubernetes-validations:
  - rule: "self.hostname.size() > 0"
    message: "hostname must not be empty"
  - rule: "self.files.all(f, f.content != '' || f.source != '')"
    message: "each file must have content or source"
```

Finalizer: `cfgd.io/machine-config-cleanup` — signals device daemon to un-manage resources, optional rollback via `spec.cleanup.rollback: true`, removes finalizer after cleanup.

Owner references: `TeamConfig` (XR) → `MachineConfig` → `DriftAlert` cascade. Removing a team member from the XR garbage-collects their MachineConfig, which cascades to DriftAlerts.

### 2.2 ConfigPolicy (existing, needs enhancement)

**Current implementation**: `crds/mod.rs:87-118`

**Spec fields**:

| Field | Type | Required | Description |
|---|---|---|---|
| `spec.name` | `string` | Yes | Policy display name |
| `spec.requiredModules` | `[]string` | No | Required module names |
| `spec.packages` | `[]string` | No | Required packages |
| `spec.packageVersions` | `map[string]string` | No | Semver version requirements |
| `spec.settings` | `map[string]string` | No | Required system settings |
| `spec.targetSelector` | `map[string]string` | No | Label-based target filter |

**Status fields**:

| Field | Type | Description |
|---|---|---|
| `status.compliantCount` | `uint32` | Compliant MachineConfig count |
| `status.nonCompliantCount` | `uint32` | Non-compliant count |
| `status.conditions` | `[]Condition` | Conditions |

Printer columns: `NAME COMPLIANT NON-COMPLIANT ENFORCED AGE`

Short names: `cpol`. Categories: `cfgd`.

Condition: `Enforced` (True/False). Already implemented in `controllers/mod.rs:429-441`.

Enhancement: Add `spec.security.trustedRegistries` for webhook enforcement (list of approved OCI registry prefixes, glob matching supported).

### 2.3 DriftAlert (existing, needs enhancement)

**Current implementation**: `crds/mod.rs:124-165`

**Spec fields**: `deviceId`, `machineConfigRef`, `driftDetails[]` (field, expected, actual), `severity` (enum: Low/Medium/High/Critical).

**Status fields**: `detectedAt`, `resolvedAt`, `resolved`.

Printer columns: `NAME DEVICE SEVERITY RESOLVED AGE`

Short names: `da`. Categories: `cfgd`.

Enhancement: Add `status.conditions` (currently missing). Conditions: `Acknowledged`, `Resolved`, `Escalated`.

### 2.4 ClusterConfigPolicy (new, cluster-scoped)

Cluster-scoped version of ConfigPolicy for org-wide mandates.

```rust
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "ClusterConfigPolicy",
    status = "ClusterConfigPolicyStatus"
)]
// No `namespaced` — cluster-scoped
```

**Spec fields** (superset of ConfigPolicy):

| Field | Type | Required | Description |
|---|---|---|---|
| `spec.name` | `string` | Yes | Policy display name |
| `spec.namespaceSelector` | `map[string]string` | No | Label selector for target namespaces |
| `spec.requiredModules` | `[]string` | No | Required module names |
| `spec.packages` | `[]string` | No | Required packages |
| `spec.packageVersions` | `map[string]string` | No | Version requirements |
| `spec.settings` | `map[string]string` | No | Required settings |
| `spec.security.trustedRegistries` | `[]string` | No | Approved OCI registry prefixes |
| `spec.security.allowUnsigned` | `bool` | No (default false) | Allow unsigned modules |

**Merge semantics** with namespace-scoped ConfigPolicy:
1. `packages` and `requiredModules`: union (both applied)
2. `settings`: ClusterConfigPolicy overrides ConfigPolicy (cluster wins)
3. `packageVersions`: ClusterConfigPolicy overrides (cluster wins)
4. `trustedRegistries`: ClusterConfigPolicy is canonical (ConfigPolicy cannot expand)

### 2.5 Module CRD (new, cluster-scoped)

```rust
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "Module",
    status = "ModuleStatus"
)]
// Cluster-scoped per decision record
```

**Spec fields**:

| Field | Type | Required | Description |
|---|---|---|---|
| `spec.packages` | `[]PackageEntry` | No | Packages this module provides |
| `spec.packages[].name` | `string` | Yes | Canonical package name |
| `spec.packages[].platforms` | `map[string]string` | No | Platform-specific name overrides |
| `spec.files` | `[]ModuleFileSpec` | No | Files provided by module |
| `spec.files[].source` | `string` | Yes | Source path within module |
| `spec.files[].target` | `string` | Yes | Target mount path |
| `spec.scripts.postApply` | `[]string` | No | Post-apply script paths |
| `spec.env` | `[]EnvVar` | No | Environment variables |
| `spec.env[].name` | `string` | Yes | Variable name |
| `spec.env[].value` | `string` | No | Variable value |
| `spec.env[].append` | `string` | No | Append to existing variable (e.g., PATH) |
| `spec.depends` | `[]string` | No | Module dependency names |
| `spec.ociArtifact` | `string` | No | OCI artifact reference (set by `cfgd module push`) |
| `spec.signature.cosign.publicKey` | `string` | No | Cosign verification public key |

**Status fields**:

| Field | Type | Description |
|---|---|---|
| `status.resolvedArtifact` | `string` | Resolved OCI digest |
| `status.availablePlatforms` | `[]string` | Platforms with built artifacts |
| `status.verified` | `bool` | Signature verification passed |
| `status.conditions` | `[]Condition` | Standard conditions |

Conditions: `Available` (True when OCI artifact pullable), `Verified` (True when signature checks pass).

Printer columns: `NAME ARTIFACT VERIFIED PLATFORMS AGE`

Short names: `mod`. Categories: `cfgd`.

### Shared: Condition struct

Currently defined at `crds/mod.rs:73-81`. Fields: `type`, `status`, `reason`, `message`, `lastTransitionTime`. Matches `metav1.Condition`. Enhancement: add `observedGeneration` to match upstream convention.

---

## 3. Controller Specifications

### 3.1 MachineConfig Controller (existing)

**Current implementation**: `controllers/mod.rs:85-176`

Current behavior:
1. Validate spec (delegates to `MachineConfigSpec::validate()`)
2. Check for active DriftAlerts via label selector
3. Skip reconcile if generation unchanged and no drift
4. Set `Ready` condition
5. Update `status.lastReconciled`, `driftDetected`, `observedGeneration`
6. Requeue after 60s

Enhancements:
- Emit Kubernetes Events on reconcile success, drift detection, validation failure
- Split `Ready` into 4 conditions: `Reconciled`, `DriftDetected`, `ModulesResolved`, `Compliant`
- Finalizer handling for deletion cleanup (signal device, optional rollback, remove finalizer)
- Module resolution check: verify all `moduleRefs` point to existing Module CRDs
- Cross-reference ConfigPolicy compliance: set `Compliant` condition based on matching policies
- Watch DriftAlert changes to trigger immediate re-reconcile (not just timer)

Error handling: `OperatorError::Reconciliation` with 30s requeue (`error_policy_mc` at line 187-194). kube-rs applies exponential backoff. Correct as-is.

### 3.2 DriftAlert Controller (existing)

**Current implementation**: `controllers/mod.rs:200-298`

Current behavior: Look up referenced MachineConfig, mark as drifted if not already, delete resolved alerts.

Enhancements:
- Emit Events on MachineConfig when drift detected/resolved
- Add `status.conditions` to DriftAlertStatus (currently has no conditions array)
- Set conditions: `Acknowledged`, `Resolved`, `Escalated`

### 3.3 ConfigPolicy Controller (existing)

**Current implementation**: `controllers/mod.rs:362-465`

Current behavior: List MachineConfigs, evaluate compliance, update counts, set `Enforced` condition.

Enhancements:
- Emit Events on non-compliant MachineConfigs
- Cross-reference ClusterConfigPolicy
- Watch MachineConfig changes to trigger policy re-evaluation (not just timer)

### 3.4 ClusterConfigPolicy Controller (new)

Watches ClusterConfigPolicy resources. On change:
1. List all namespaces matching `namespaceSelector`
2. In each namespace, list MachineConfigs
3. Evaluate compliance (same logic as ConfigPolicy but cluster-scoped)
4. Update status with aggregate counts
5. Emit Events on non-compliant resources

### 3.5 Module Controller (new, deferred until OCI pipeline exists)

Watches Module CRDs. On create/update:
1. Validate OCI artifact reference against trusted registries (from ClusterConfigPolicy)
2. Verify cosign signature if present
3. Set `Available` and `Verified` conditions
4. Optionally coordinate with DaemonSet to pre-pull popular module layers

---

## 4. Webhook Specifications

### 4.1 Validation Webhook (existing)

**Current implementation**: `webhook.rs:17-224`

Endpoints: `/validate-machineconfig`, `/validate-configpolicy`, `/healthz`

TLS: rustls with cert-manager-provisioned certs from `WEBHOOK_CERT_DIR`.

Current validation rules (in `crds/mod.rs`):
- MachineConfig: non-empty hostname/profile, files have content or source, valid octal mode, valid loose versions
- ConfigPolicy: non-empty name/packages, valid semver requirements, non-empty settings keys

Enhancements:
- `/validate-driftalert` endpoint (currently missing)
- `/validate-clusterconfigpolicy` endpoint
- `/validate-module` endpoint (verify OCI reference format, validate signature fields)
- Trusted registry enforcement: reject Module CRD creates/updates where `spec.ociArtifact` doesn't match any `trustedRegistries` from ClusterConfigPolicy

Certificate management: cert-manager Certificate resource in Helm chart creates/rotates TLS certs automatically. Duration: 1 year, renew 30 days before expiry. Webhook CA bundle injected via cert-manager's `cainjector`.

### 4.2 Pod Module Mutating Webhook (new)

Intercepts pod CREATE requests and injects CSI volumes + init containers for module injection.

Endpoint: `POST /mutate-pods`

```yaml
apiVersion: admissionregistration.k8s.io/v1
kind: MutatingWebhookConfiguration
metadata:
  name: cfgd-pod-module-injector
webhooks:
  - name: inject-modules.cfgd.io
    admissionReviewVersions: ["v1"]
    sideEffects: None
    failurePolicy: Ignore  # Pod creation must not be blocked by cfgd failure
    reinvocationPolicy: IfNeeded
    timeoutSeconds: 10
    objectSelector:
      matchExpressions:
        - key: cfgd.io/skip-injection
          operator: DoesNotExist
    namespaceSelector:
      matchExpressions:
        - key: cfgd.io/inject-modules
          operator: In
          values: ["true"]
    rules:
      - apiGroups: [""]
        apiVersions: ["v1"]
        operations: ["CREATE"]
        resources: ["pods"]
```

Logic:
1. Parse `cfgd.io/modules` annotation (format: `"name:version,name:version"`)
2. Lookup ConfigPolicy in pod's namespace for `requiredModules`
3. For each module: lookup Module CRD, get OCI artifact reference
4. Inject CSI volume definition per module
5. Inject volumeMount on all containers (target: `/cfgd-modules/<name>/`)
6. Extend PATH and other env vars per module spec
7. If module has `scripts.postApply`, inject init container + shared emptyDir
8. Record injection as Kubernetes Event on pod

Generated pod spec additions (user does not write this):
```yaml
volumes:
  - name: cfgd-module-network-debug
    csi:
      driver: csi.cfgd.io
      readOnly: true
      volumeAttributes:
        module: network-debug
        version: "1.2"
containers:
  - volumeMounts:
      - name: cfgd-module-network-debug
        mountPath: /cfgd-modules/network-debug
        readOnly: true
    env:
      - name: PATH
        value: "/cfgd-modules/network-debug/bin:$(PATH)"
```

**Dependencies**: CSI driver, Module CRD. Cannot function without both.

---

## 5. Helm Chart

### 5.1 Chart Relocation

Move from `crates/cfgd-operator/chart/cfgd-operator/` to `chart/cfgd/`. Chart name: `cfgd`.

### 5.2 values.yaml Structure

Current values (at existing path): `image`, `replicaCount`, `installCRDs`, `serviceAccount`, `resources`, `nodeSelector`, `tolerations`, `affinity`, `webhook`, `deviceGateway`.

New structure:
```yaml
operator:
  enabled: true
  leaderElection:
    enabled: true
    leaseDuration: 15s
    renewDeadline: 10s
    retryPeriod: 2s

csiDriver:
  enabled: false
  image:
    repository: ghcr.io/tj-smith47/cfgd-csi
    tag: ""
  resources:
    limits: { cpu: 100m, memory: 128Mi }
  tolerations:
    - operator: Exists  # CSI must run on all nodes

podSecurityContext:
  runAsNonRoot: true
  runAsUser: 65532
  runAsGroup: 65532
  fsGroup: 65532
  seccompProfile:
    type: RuntimeDefault

containerSecurityContext:
  allowPrivilegeEscalation: false
  readOnlyRootFilesystem: true
  capabilities:
    drop: [ALL]

podDisruptionBudget:
  enabled: false
  minAvailable: 1

networkPolicy:
  enabled: false

metrics:
  enabled: true
  port: 8443
  serviceMonitor:
    enabled: false
    interval: 30s

probes:
  port: 8081
```

### 5.3 Additional Artifacts

- `values.schema.json`: generated from values.yaml, enforces types/required/enums. Enables `helm lint` validation.
- `NOTES.txt`: post-install message showing CRD status, verification commands, gateway URL (if enabled).
- Test hook pod: verifies CRDs are established and operator pod is running.
- Example values files: `examples/operator-only.yaml`, `examples/with-gateway.yaml`, `examples/full.yaml`

---

## 6. Module OCI Pipeline

### Scope Assessment

This is a multi-month effort with 4 distinct phases. Ship incrementally: A → D → C → B. Users can manually create OCI artifacts (via `oras push`) and benefit from the pipeline before the full build system exists.

### Phase A: OCI Library Integration (2-4 weeks)

- Define OCI manifest and layer structure
- Integrate `oci-distribution` or `oras-rs` crate
- Registry authentication (Docker config.json, credential helpers)
- `cfgd module push <dir>`: push directory as OCI artifact
- `cfgd module pull <ref>`: download OCI artifact to directory

### Phase B: Module Build System (4-8 weeks)

- `cfgd module build --target <platform>`: resolve module for target platform, install packages into isolated root (container or chroot), collect binaries/config/env, package as OCI artifact
- Requires Docker/Podman for container-based builds or rootless approaches
- Multi-platform builds (one layer per platform)
- Comparable in complexity to a narrow `docker build` — do not underestimate

### Phase C: Signing and Verification (1-2 weeks)

- cosign integration for signing artifacts at push time
- Verification at pull time
- Key management: static keys (`spec.signature.cosign.publicKey`) or keyless (Fulcio + Rekor)

### Phase D: Module CRD Synchronization (1-2 weeks)

- `cfgd module push --apply`: creates/updates Module CRD on cluster
- CSI driver reads Module CRD to find OCI reference

### OCI Artifact Layer Structure

```json
{
  "schemaVersion": 2,
  "mediaType": "application/vnd.oci.image.manifest.v1+json",
  "config": {
    "mediaType": "application/vnd.cfgd.module.config.v1+json",
    "digest": "sha256:...",
    "size": 234
  },
  "layers": [
    {
      "mediaType": "application/vnd.cfgd.module.layer.v1.tar+gzip",
      "digest": "sha256:...",
      "size": 12345,
      "annotations": {
        "cfgd.io/platform": "linux/amd64",
        "cfgd.io/base-image": "debian:bookworm"
      }
    }
  ]
}
```

Config blob contains module metadata (name, version, env, scripts, dependencies) — same schema as the Module CRD spec.

---

## 7. CSI Driver

### Scope Assessment

A CSI driver is a separate binary deployed as a DaemonSet implementing the gRPC CSI spec.

### Architecture

- **Plugin type**: Node plugin only. No Controller plugin needed (volumes are ephemeral, not provisioned).
- **Binary**: Separate Rust crate in `crates/cfgd-csi/`
- **Deployment**: DaemonSet with `hostPath` volume for socket, `hostNetwork: false`
- **Socket**: `/var/lib/kubelet/plugins/csi.cfgd.io/csi.sock`
- **Registration**: CSI `node-driver-registrar` sidecar
- **Implementation**: `tonic` for gRPC. The CSI spec surface is small (6 RPCs).

### CSI RPCs

**Identity Service**:
- `GetPluginInfo`: returns driver name `csi.cfgd.io`
- `GetPluginCapabilities`: empty (Node-only plugin, no plugin-level capabilities)
- `Probe`: health check (validates cache directory accessibility)

**Node Service**:
- `NodeGetCapabilities`: `STAGE_UNSTAGE_VOLUME`
- `NodeGetInfo`: returns node ID
- `NodePublishVolume`: reads `volumeAttributes.module` and `volumeAttributes.version`, pulls OCI artifact (or reads from cache), mounts content to target path as read-only bind mount
- `NodeUnpublishVolume`: unmounts
- `NodeStageVolume`: pulls OCI artifact to node-level cache
- `NodeUnstageVolume`: optional cleanup

### Cache Strategy

- Node-level cache at `/var/lib/cfgd-csi/cache/`
- Key: `<module-name>/<version>/<platform>/`
- LRU eviction when cache exceeds configurable size (default 5Gi)
- Pre-pull popular modules via operator-pushed ConfigMap listing module refs

### DaemonSet Manifest

```yaml
kind: DaemonSet
spec:
  template:
    spec:
      containers:
        - name: cfgd-csi
          securityContext:
            privileged: true  # Required for mount operations
          volumeMounts:
            - name: plugin-dir
              mountPath: /csi
            - name: pods-mount-dir
              mountPath: /var/lib/kubelet
              mountPropagation: Bidirectional
            - name: cache
              mountPath: /var/lib/cfgd-csi/cache
        - name: node-driver-registrar
          image: registry.k8s.io/sig-storage/csi-node-driver-registrar:v2.10.0
```

---

## 8. kubectl Plugin

### Subcommands

```
kubectl cfgd debug <pod> --module <name:version> [--module ...]  # ephemeral container with modules
kubectl cfgd exec <pod> --module <name:version> -- <command>     # exec with module env
kubectl cfgd inject <pod> --module <name:version>                # inject into running pod
kubectl cfgd status                                               # fleet overview
kubectl cfgd version                                              # client/server version
```

Same binary as `cfgd`. When invoked as `kubectl-cfgd`, enters plugin mode. Uses `kube` crate with kubeconfig from environment.

`debug` creates an ephemeral container with CSI volumes for each module, PATH extended per module env spec, custom PS1 showing active modules.

`inject` patches pod spec to add CSI volume + volumeMount (limited to pods that support this).

### Krew Manifest

```yaml
apiVersion: krew.googlecontainertools.github.com/v1alpha2
kind: Plugin
metadata:
  name: cfgd
spec:
  version: "v0.1.0"
  shortDescription: "Manage cfgd modules on pods"
  platforms:
    - selector:
        matchLabels:
          os: linux
          arch: amd64
      uri: https://github.com/tj-smith47/cfgd/releases/download/v0.1.0/kubectl-cfgd_linux_amd64.tar.gz
      sha256: "..."
      bin: kubectl-cfgd
```

**Dependencies**: CSI driver (for volume-based commands), Module CRD (for lookups).

---

## 9. Observability

### 9.1 Prometheus Metrics

All metrics prefixed with `cfgd_operator_`. Use `prometheus-client` crate.

**Operator metrics**:

| Name | Type | Labels | Description |
|---|---|---|---|
| `cfgd_operator_reconciliations_total` | Counter | `controller`, `result` | Reconciliation attempts |
| `cfgd_operator_reconciliation_duration_seconds` | Histogram | `controller` | Time per reconciliation |
| `cfgd_operator_drift_events_total` | Counter | `severity`, `namespace` | Drift events created |
| `cfgd_operator_devices_compliant` | Gauge | `policy`, `namespace` | Compliant device count |
| `cfgd_operator_devices_enrolled_total` | Counter | `method` | Enrollment events |
| `cfgd_operator_webhook_requests_total` | Counter | `operation`, `result` | Webhook requests |
| `cfgd_operator_webhook_duration_seconds` | Histogram | `operation` | Webhook latency |

**CSI metrics** (separate binary):

| Name | Type | Labels | Description |
|---|---|---|---|
| `cfgd_csi_volume_publish_total` | Counter | `module`, `result` | Volume mount operations |
| `cfgd_csi_pull_duration_seconds` | Histogram | `module`, `cached` | OCI pull time |
| `cfgd_csi_cache_size_bytes` | Gauge | | Current cache usage |
| `cfgd_csi_cache_hits_total` | Counter | `module` | Cache hits |

Endpoint: `GET /metrics` on port 8443 (operator) or 9090 (CSI).

### 9.2 Kubernetes Events

Events via `kube::runtime::events::Recorder`. RBAC already grants `events: [create, patch]`.

| Resource | Type | Reason | When |
|---|---|---|---|
| MachineConfig | Normal | `Reconciled` | Successful reconciliation |
| MachineConfig | Warning | `ReconcileError` | Reconciliation failed |
| MachineConfig | Warning | `DriftDetected` | Drift found |
| MachineConfig | Normal | `DriftResolved` | Drift cleared |
| MachineConfig | Warning | `PolicyViolation` | Non-compliant with policy |
| Pod | Normal | `ModuleInjected` | Module CSI volume injected |
| Pod | Warning | `ModuleInjectionFailed` | Injection failed |
| ConfigPolicy | Normal | `Evaluated` | Policy evaluation completed |
| ConfigPolicy | Warning | `NonCompliantTargets` | Non-compliant targets found |

### 9.3 ServiceMonitor

```yaml
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: cfgd-operator
spec:
  selector:
    matchLabels: <selectorLabels>
  endpoints:
    - port: metrics
      interval: 30s
```

Conditional on `metrics.serviceMonitor.enabled`.

### 9.4 OpenTelemetry Tracing

`tracing-opentelemetry` dependency. W3C trace context propagation through:
- HTTP headers on gateway requests (device checkin/enrollment)
- gRPC metadata on CSI calls
- Controller reconciliation spans

Configuration via `OTEL_EXPORTER_OTLP_ENDPOINT` environment variable.

---

## 10. Supply Chain Security

### 10.1 cosign Integration

- `cfgd module push --sign` signs OCI artifact with cosign
- `cfgd module pull` verifies signature when policy requires
- CSI driver verifies signatures before mounting
- Key management: support static keys (`spec.signature.cosign.publicKey`) and keyless (Fulcio + Rekor)

### 10.2 Trusted Registries

ClusterConfigPolicy `spec.security.trustedRegistries` defines approved registry prefixes. Validation webhook rejects Module CRD creates where `spec.ociArtifact` doesn't match. Glob matching supported (`registry.example.com/acme-corp/*`).

### 10.3 SLSA Provenance

Release pipeline generates SLSA Level 3 provenance attestations for cfgd binary, cfgd-operator binary, cfgd-csi binary, and container images. Uses `slsa-framework/slsa-github-generator`.

### 10.4 In-toto Attestations

Module authors can attach in-toto layouts to OCI artifacts. cfgd verifies at resolution time. Deferred enhancement after core OCI pipeline ships.

---

## 11. Multi-Tenancy

### 11.1 RBAC Manifests

Helm-templated ClusterRole/Role examples:

**Platform admin** (cluster-admin on cfgd.io):
```yaml
rules:
  - apiGroups: [cfgd.io]
    resources: ["*"]
    verbs: ["*"]
```

**Team lead** (namespace-scoped):
```yaml
rules:
  - apiGroups: [cfgd.io]
    resources: [machineconfigs, configpolicies, driftalerts]
    verbs: [get, list, watch, create, update, patch, delete]
  - apiGroups: [cfgd.io]
    resources: [machineconfigs/status, configpolicies/status]
    verbs: [get]
```

**Team member** (read-only):
```yaml
rules:
  - apiGroups: [cfgd.io]
    resources: [machineconfigs, configpolicies, driftalerts]
    verbs: [get, list, watch]
```

**Module publisher** (cluster-scoped):
```yaml
rules:
  - apiGroups: [cfgd.io]
    resources: [modules]
    verbs: [get, list, watch, create, update, patch]
```

### 11.2 Namespace Isolation

Each team gets a namespace. ConfigPolicy is namespace-scoped and applies only to MachineConfigs in the same namespace. ClusterConfigPolicy applies org-wide. Operator watches all namespaces (current behavior via `Api::all()` at `controllers/mod.rs:25-27`).

### 11.3 Device Identity Binding

MachineConfig `spec.hostname` + device `deviceId` form the binding. Device gateway verifies at checkin that the device's API key matches the enrolled device ID. No cross-device impersonation.

### 11.4 ClusterConfigPolicy Merge Semantics

When both ClusterConfigPolicy and ConfigPolicy apply:
1. `packages` and `requiredModules`: union
2. `settings`: ClusterConfigPolicy overrides (cluster wins)
3. `packageVersions`: ClusterConfigPolicy overrides (cluster wins)
4. `trustedRegistries`: ClusterConfigPolicy is canonical (namespace cannot expand)

---

## 12. Crossplane Integration

### Current State

- XRD: `manifests/crossplane/xrd-teamconfig.yaml` — defines TeamConfig composite resource
- Composition: `manifests/crossplane/composition.yaml` — pipeline mode, uses function-cfgd
- Function: `function-cfgd/fn.go` (~455 lines) + `fn_test.go` (20+ test cases)
  - Generates per-member MachineConfig CRDs from TeamConfig spec
  - Generates ConfigPolicy CRDs for policy tiers
  - Handles package flattening, file collection, system settings, module refs

### Open Issues

1. No integration test against a real Crossplane installation
2. XRD uses `apiextensions.crossplane.io/v2` — verify target Crossplane version compatibility
3. Function image `ghcr.io/tj-smith47/function-cfgd:v0.1.0` needs CI pipeline to build/push
4. No status propagation back to TeamConfig XR

### Testing Strategy

1. kind cluster with Crossplane installed
2. Apply XRD, Composition, Function
3. Create TeamConfig XR with 2 members
4. Verify MachineConfig CRDs created (one per member)
5. Verify ConfigPolicy CRDs created
6. Add member → new MachineConfig appears
7. Remove member → MachineConfig garbage collected
8. Update policy tier → ConfigPolicy updated

### Enrollment Flow

1. Platform admin creates TeamConfig XR with member list
2. Crossplane generates MachineConfig per member + bootstrap token
3. Member runs `cfgd enroll --server <gateway-url> --token <token>`
4. Device checks in, gets desired config from MachineConfig CRD
5. Ongoing: daemon checks in periodically, gateway matches device to MachineConfig

---

## 13. CRD Versioning and Migration

### Strategy

- Current: `v1alpha1` (all CRDs)
- Next: `v1beta1` — stability guarantee, no breaking changes without deprecation
- Graduation criteria: 3+ months production use, E2E passing, no schema changes for 1 month

### Conversion Webhook

When v1beta1 is introduced:
1. v1alpha1 remains served (backwards compatibility)
2. v1beta1 becomes storage version
3. Conversion webhook converts between versions

Endpoint: `POST /convert`. Implementation: add `conversion: Webhook` to CRD spec.

### Migration Runbook

1. Deploy operator with v1beta1 CRDs (both versions served, v1beta1 storage)
2. Conversion webhook automatically converts on read
3. Trigger storage migration: `kubectl get mc --all-namespaces -o yaml | kubectl apply -f -`
4. After all objects migrated: remove v1alpha1 from served versions
5. Remove conversion webhook

### Backwards Compatibility

- v1alpha1 → v1beta1: additive fields only. No removals, no renames (use deprecated aliases)
- v1beta1 → v1: same rules

---

## 14. Testing Strategy

### Current Coverage

- Unit tests: CRD validation (36), controllers (13), gateway DB (11), fleet (2), server_client (14)
- E2E: operator (T01-T10), full-stack, CLI, node — all in CI
- CI: fmt, clippy, test with tarpaulin coverage, audit script

### Gaps

No integration tests for: webhook admission, gateway HTTP API, Crossplane in real cluster, multi-namespace policy evaluation.

### Test Plan

**Unit (CI, fast)**:
- CRD validation: every field constraint
- Policy compliance: every combination of requirements vs state
- Controller logic: mock kube client, verify status patches and event emissions
- Gateway DB: all CRUD, migrations, cleanup
- Composition engine: merge semantics, conflict resolution

**Integration (CI, kind cluster)**:
- T11: Webhook validation accepts/rejects correctly
- T12: ClusterConfigPolicy overrides namespace policy
- T13: Module CRD created, printer columns visible
- T14: Leader election failover
- T15: Graceful shutdown (SIGTERM → clean exit)

**Crossplane E2E (separate CI job, kind + Crossplane)**:
- T-XP01: TeamConfig → MachineConfigs generated
- T-XP02: Member removal → MachineConfig deleted
- T-XP03: Policy tier → ConfigPolicy generated

**Chaos**:
- Operator crash during reconciliation → no corrupt status
- Gateway down → daemon continues with cached config
- Webhook unavailable → pods still create (failurePolicy: Ignore)
- Leader lease expires → standby takes over within lease duration

**Multi-tenant isolation**:
- Team A resources in ns-a invisible to Team B
- ClusterConfigPolicy applies to both namespaces

---

## 15. Operational Runbooks

### 15.1 Installation

```bash
helm repo add cfgd https://tj-smith47.github.io/cfgd
helm install cfgd cfgd/cfgd -n cfgd-system --create-namespace
```

Prerequisites: cert-manager (if webhook enabled), Kubernetes 1.27+.

### 15.2 Upgrade

1. Review release notes for breaking changes
2. `helm diff upgrade cfgd cfgd/cfgd --version X.Y.Z`
3. If CRD schema changes: verify conversion webhook included
4. `helm upgrade cfgd cfgd/cfgd --version X.Y.Z`
5. Verify: `kubectl get pods -n cfgd-system`, `kubectl get crd`
6. Rollback: `helm rollback cfgd`

### 15.3 Certificate Rotation

cert-manager handles automatic rotation (1 year duration, renew 30 days before expiry). Manual rotation: delete the Certificate resource → cert-manager re-issues.

### 15.4 Disaster Recovery

- Gateway SQLite DB: backup via PVC snapshot or `sqlite3 .backup`
- CRDs: backed up by cluster backup tools (Velero)
- State recovery: cfgd daemon re-enrolls if credential lost; gateway re-creates device record

### 15.5 Troubleshooting

| Symptom | Investigation |
|---|---|
| Webhook not responding | Check cert-manager Certificate status, check webhook Service endpoints |
| Controllers not reconciling | Check leader election Lease, check RBAC, check operator logs |
| Device not checking in | Check gateway Service/Ingress, device credential file, API key |
| CRD not established | `kubectl get crd <name> -o yaml` — check conditions |
| Pods not getting modules | Check `cfgd.io/inject-modules` namespace label, Module CRD exists, CSI driver running |

---

## Implementation Order

```
Tier 1 — Ship now, no new dependencies:
  Operator readiness (leader election, graceful shutdown, security contexts, probes)
  CRD enhancements (printer columns, short names, conditions, CEL)
  ClusterConfigPolicy CRD + controller
  Helm chart restructure
  Observability (metrics, events)
  Multi-tenancy RBAC
  Crossplane testing
  Runbooks

Tier 2 — Needs Module CRD:
  Module CRD + controller
  Validation webhook enhancements
  OCI pipeline Phase A (push/pull)

Tier 3 — Needs OCI pipeline:
  OCI pipeline Phase B/C/D (build, signing, CRD sync)
  Supply chain security

Tier 4 — Needs CSI:
  CSI driver
  Pod module mutating webhook
  kubectl cfgd plugin

Tier 5 — Needs stability:
  CRD versioning (v1alpha1 → v1beta1 conversion)
```

---

## Positioning

cfgd is to machine configuration what Flux is to cluster configuration — a GitOps-native reconciliation loop targeting the OS layer instead of the Kubernetes API.

Modules extend this into a new category: **portable, declarative environment capabilities** that work across machines, containers, and pods. ConfigMaps deliver configuration data. Secrets deliver sensitive data. cfgd Modules deliver environment capabilities.
