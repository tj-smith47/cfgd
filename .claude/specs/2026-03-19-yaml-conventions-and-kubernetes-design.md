# YAML Convention Alignment & Kubernetes Implementation

Standardize cfgd's YAML conventions to match Kubernetes ecosystem norms, then implement the Kubernetes support tiers from PLAN.md.

## Context

cfgd uses `kebab-case` for local config YAML fields and mixed enum serialization styles. The entire Kubernetes ecosystem (core API, Helm, Crossplane, Flux, ArgoCD, Kustomize) uses `camelCase` fields and PascalCase enum values. cfgd's CRDs already follow this convention — local configs do not.

Nobody uses cfgd yet, so breaking changes are free.

## Part 1: YAML Convention Changes

### 1.1 Field Naming — kebab-case to camelCase

All config structs switch from `#[serde(rename_all = "kebab-case")]` to `#[serde(rename_all = "camelCase")]`.

**Scope:** Every struct/enum with `rename_all = "kebab-case"` or `rename_all = "lowercase"` across the entire codebase. The authoritative list is the grep output — don't enumerate structs by name since new ones get added. The implementation should mechanically find and change every instance.

**Files with `rename_all = "kebab-case"` or `rename_all = "lowercase"`:**
- `cfgd-core/src/config/mod.rs` — ~63 instances (all config structs, enums, inner deserializer structs)
- `cfgd-core/src/server_client.rs` — 11 structs (API contract with gateway — must change in lockstep with gateway)
- `cfgd-core/src/daemon/mod.rs` — 3 structs
- `cfgd-core/src/upgrade.rs` — 1 struct
- `cfgd-core/src/reconciler/mod.rs` — 1 enum (`PhaseName`, lowercase → PascalCase; safe because SQLite uses `as_str()`/`from_str()`, not serde)
- `cfgd-core/src/state/mod.rs` — 1 enum (`ApplyStatus`, lowercase → PascalCase; safe because SQLite uses `as_str()`/`from_str()`, not serde)
- `cfgd-operator/src/crds/mod.rs` — 1 enum (`DriftSeverity`, lowercase → PascalCase; this IS a CRD change)
- `cfgd-operator/src/gateway/db.rs` — 9 structs
- `cfgd-operator/src/gateway/api.rs` — 14+ structs (API responses — JSON wire format changes)

**apiVersion simplification:** With `camelCase` structs, `api_version` naturally serializes as `apiVersion`. Remove the explicit `#[serde(rename = "apiVersion")]` from `CfgdConfig`, `ProfileDocument`, `ModuleDocument`, and `ConfigSourceDocument`.

**Gateway API coordination:** The `server_client.rs` structs (cfgd client) and `gateway/api.rs` + `gateway/db.rs` structs (operator server) form two sides of an HTTP API. Both must change together. Since nobody uses this yet, the wire format change is free.

### 1.2 Enum Standardization to PascalCase

Remove `rename_all` from all enums. Rust enum variants are already PascalCase — just stop renaming them.

| Enum | Current | Remove | Result |
|---|---|---|---|
| `FileStrategy` | kebab-case (direct) | `rename_all = "kebab-case"` | `Symlink`, `Copy`, `Template`, `Hardlink` |
| `PolicyAction` | kebab-case | `rename_all = "kebab-case"` | `Notify`, `Accept`, `Reject`, `Ignore` |
| `OriginType` | kebab-case | `rename_all = "kebab-case"` | `Git`, `Server` |
| `NotifyMethod` | kebab-case | `rename_all = "kebab-case"` | `Desktop`, `Stdout`, `Webhook` |
| `LayerPolicy` | lowercase | `rename_all = "lowercase"` | `Required`, `Recommended`, `Optional`, `Locked` |
| `DriftSeverity` (CRD) | lowercase | `rename_all = "lowercase"` | `Low`, `Medium`, `High`, `Critical` |
| `ApplyStatus` | lowercase | `rename_all = "lowercase"` | `Success`, `Partial`, `Failed` |
| `PhaseName` | lowercase | `rename_all = "lowercase"` | `Modules`, `System`, `Packages`, `Files`, `Env`, `Secrets`, `Scripts` |
| `DriftPolicy` | PascalCase | No change needed | `Auto`, `NotifyOnly`, `Prompt` |
| `ReconcilePatchKind` | PascalCase | No change needed | `Module`, `Profile` |

### 1.3 Cascading Updates

Every change to serde attributes requires updating:
- `CLAUDE.md` — update the "Config serde" style rule from `kebab-case` to `camelCase`
- All example YAML files in `examples/`
- All test fixtures in `tests/`
- All inline YAML in unit tests (especially extensive tests in `config/mod.rs`)
- All documentation in `docs/` (configuration.md, profiles.md, modules.md, templates.md, secrets.md, packages.md, system-configurators.md, sources.md, daemon.md, reconciliation.md, cli-reference.md, bootstrap.md)
- The `cfgd generate` schema export (reflects struct definitions automatically)
- Crossplane XRD schema in `manifests/crossplane/xrd-teamconfig.yaml` (already camelCase — verify consistency)
- CRD YAML templates in `chart/` (DriftSeverity enum values change)

### 1.4 What Does NOT Change

- CRD struct field naming in `cfgd-operator/src/crds/mod.rs` — already `camelCase` (but `DriftSeverity` enum values DO change)
- Helm `values.yaml` — already `camelCase`
- Crossplane manifests — already `camelCase`
- The `apiVersion` and `kind` top-level fields (values unchanged, just the serde mechanism)
- SQLite stored values — `ApplyStatus` and `PhaseName` use manual `as_str()`/`from_str()` for DB storage, not serde

## Part 2: Kubernetes Implementation

After YAML conventions are aligned, implement the Kubernetes tiers from PLAN.md in order. Full design detail is in `.claude/kubernetes-first-class.md`.

### Tier 1 — Operator Hardening & CRD Enhancement

**Operator operational readiness:**
- Leader election via `coordination.k8s.io/v1` Lease
- Graceful shutdown with SIGTERM drain (30s grace)
- Health probes on port 8081 (`/healthz`, `/readyz`)
- Security contexts (`runAsNonRoot`, `readOnlyRootFilesystem`, `capabilities.drop: [ALL]`, UID 65532)
- PodDisruptionBudget (conditional on `replicaCount >= 2`)
- NetworkPolicy (ingress: webhook/gateway/probe ports; egress: kube-apiserver)

**CRD enhancements (existing 3 CRDs):**
- Printer columns for `kubectl get` output
- Short names (`mc`, `cpol`, `da`) and category `cfgd`
- MachineConfig conditions: `Reconciled`, `DriftDetected`, `ModulesResolved`, `Compliant`
- DriftAlert conditions: `Acknowledged`, `Resolved`, `Escalated`
- `observedGeneration` on Condition struct
- CEL validation rules on MachineConfig
- Finalizer: `cfgd.io/machine-config-cleanup`
- Owner references: TeamConfig → MachineConfig → DriftAlert cascade

**ClusterConfigPolicy CRD:**
- Cluster-scoped with `namespaceSelector`, `security.trustedRegistries`, `security.allowUnsigned`
- Controller evaluates all MachineConfigs in matching namespaces
- Merge semantics: packages/modules union, settings/versions cluster-wins

**Kubernetes Events:**
- MachineConfig: `Reconciled`, `ReconcileError`, `DriftDetected`, `DriftResolved`, `PolicyViolation`
- ConfigPolicy: `Evaluated`, `NonCompliantTargets`
- Via `kube::runtime::events::Recorder`

**Observability:**
- Prometheus `/metrics` on port 8443 via `prometheus-client`
- Metrics: reconciliations_total, duration_seconds, drift_events, devices_compliant, webhook_requests, etc.
- ServiceMonitor template (conditional)
- OpenTelemetry tracing via `tracing-opentelemetry`

**Helm chart restructure:**
- Consolidate operator chart from `crates/cfgd-operator/chart/cfgd-operator/` and agent chart from `charts/cfgd/` into unified `chart/cfgd/` at repo root
- Restructure values.yaml with operator/csi/security/metrics/probes sections
- `values.schema.json`, `NOTES.txt`, test hook, example values files

**Multi-tenancy RBAC:**
- Helm-templated RBAC examples: platform admin, team lead, team member, module publisher
- Namespace isolation documentation

**Crossplane testing:**
- E2E: kind cluster + Crossplane, apply XRD/Composition/Function, verify CRD generation
- CI pipeline for function-cfgd image
- XRD v2 compatibility verification

**Server-side apply:**
- Field manager annotations (Crossplane → spec, operator → status, policy → annotations)
- Structured merge diff annotations on CRD OpenAPI schema

**Idiomatic naming audit:**
- Cross-references use `moduleRef`/`configRef` style (objects, not bare strings)
- Enum values PascalCase
- CLI flags and config fields align with K8s conventions
- All CRD field names camelCase

### Tier 2 — Module CRD & OCI Foundation

- Module CRD (cluster-scoped): packages, files, scripts, env, depends, `ociArtifact`, `signature.cosign.publicKey`
- Validation webhook endpoints for DriftAlert, ClusterConfigPolicy, Module
- OCI push/pull via `oci-distribution` or `oras-rs`
- `cfgd module push/pull` commands
- CRD sync via `cfgd module push --apply`

### Tier 3 — OCI Build & Supply Chain

- `cfgd module build --target <platform>` with container isolation
- Multi-platform OCI artifacts
- Cosign signing (`cfgd module push --sign`)
- Verification at pull time
- SLSA Level 3 provenance attestations
- In-toto attestation on Module OCI artifacts

### Tier 4 — Pod Module Injection

- CSI driver (`crates/cfgd-csi/`): Node plugin, OCI pull + read-only mount, LRU cache
- Pod mutation webhook: parse `cfgd.io/modules` annotation, inject CSI volumes + init containers
- `kubectl cfgd` plugin: debug, exec, inject, status, version subcommands

### Tier 5 — CRD Versioning & Graduation

- Conversion webhook: v1alpha1 → v1beta1
- Both versions served, v1beta1 storage
- Migration runbook
- Graduation criteria: 3+ months production, stable schema 1 month, E2E passing

## Implementation Order

1. YAML convention changes (Part 1) — foundation, must be first
2. Tier 1 sections — each independently implementable, can be parallelized via subagents
3. Tiers 2-5 — sequential, each depends on the previous

## Success Criteria

- `cargo build` and `cargo test` pass after every section
- `cargo clippy -- -D warnings` clean
- All example YAML, docs, and test fixtures updated
- No `kebab-case` or `lowercase` in any `rename_all` serde attribute (search the codebase to verify)
- Each PLAN.md checkbox checked as completed
