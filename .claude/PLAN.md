# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in `INITIAL-PLAN.md`. Design detail in [kubernetes-first-class.md](kubernetes-first-class.md).

---

## Tier 1 — Operator hardening & existing CRD enhancement (no new dependencies)

### Operator operational readiness

- [ ] Leader election via `coordination.k8s.io/v1` Lease — `main.rs` acquires lease before starting controllers; Helm `replicaCount` > 1 enabled
- [ ] Graceful shutdown — SIGTERM handler, drain in-flight reconciliations (30s grace), stop webhook, flush metrics
- [ ] Health probes on dedicated HTTP port (8081) — `/healthz` liveness, `/readyz` readiness (503 until leader lease acquired)
- [ ] Security contexts in Helm deployment template — `runAsNonRoot`, `readOnlyRootFilesystem`, `capabilities.drop: [ALL]`, UID 65532
- [ ] PodDisruptionBudget (conditional on `replicaCount >= 2`)
- [ ] NetworkPolicy restricting ingress to webhook/gateway/probe ports, egress to kube-apiserver

### CRD enhancements (existing 3 CRDs)

- [ ] Printer columns: MachineConfig (`NAME HOSTNAME PROFILE DRIFT READY AGE`), ConfigPolicy (`NAME COMPLIANT NON-COMPLIANT ENFORCED AGE`), DriftAlert (`NAME DEVICE SEVERITY RESOLVED AGE`)
- [ ] Short names: `mc`, `cpol`, `da`. Categories: `cfgd` for all
- [ ] MachineConfig conditions split: `Ready` → `Reconciled`, `DriftDetected`, `ModulesResolved`, `Compliant`
- [ ] DriftAlert: add `status.conditions` array with `Acknowledged`, `Resolved`, `Escalated`
- [ ] Add `observedGeneration` field to Condition struct
- [ ] CEL validation rules on MachineConfig (non-empty hostname, files have content or source)
- [ ] MachineConfig finalizer: `cfgd.io/machine-config-cleanup` — signal device, optional rollback, remove after cleanup
- [ ] Owner references: TeamConfig (XR) → MachineConfig → DriftAlert cascade

### ClusterConfigPolicy CRD

- [ ] Cluster-scoped CRD with `namespaceSelector`, `security.trustedRegistries`, `security.allowUnsigned`
- [ ] Controller: watches changes, evaluates all MachineConfigs in matching namespaces
- [ ] Merge semantics: packages/modules union, settings/versions cluster-wins, trustedRegistries canonical

### Kubernetes Events

- [ ] MachineConfig controller emits: `Reconciled`, `ReconcileError`, `DriftDetected`, `DriftResolved`, `PolicyViolation`
- [ ] ConfigPolicy controller emits: `Evaluated`, `NonCompliantTargets`
- [ ] Use `kube::runtime::events::Recorder` (RBAC already grants events create/patch)

### Observability

- [ ] Prometheus `/metrics` endpoint on separate HTTP port (8443 default) — `prometheus-client` crate
- [ ] Metrics: `cfgd_operator_reconciliations_total`, `reconciliation_duration_seconds`, `drift_events_total`, `devices_compliant`, `devices_enrolled_total`, `webhook_requests_total`, `webhook_duration_seconds`
- [ ] ServiceMonitor template (conditional on `metrics.serviceMonitor.enabled`)
- [ ] OpenTelemetry tracing via `tracing-opentelemetry`, W3C trace context propagation

### Helm chart restructure

- [ ] Move chart to `chart/cfgd/` at repo root
- [ ] Restructure values.yaml: `operator.leaderElection`, `csiDriver`, `podSecurityContext`, `containerSecurityContext`, `podDisruptionBudget`, `networkPolicy`, `metrics`, `probes`
- [ ] `values.schema.json` for `helm lint` validation
- [ ] `NOTES.txt` post-install message
- [ ] Helm test hook pod (verifies CRDs established, operator running)
- [ ] Example values: `operator-only.yaml`, `with-gateway.yaml`, `full.yaml`

### Multi-tenancy RBAC

- [ ] Helm-templated RBAC examples: platform admin, team lead, team member, module publisher
- [ ] Document namespace isolation model (ConfigPolicy scoped per-namespace, ClusterConfigPolicy org-wide)

### Crossplane testing

- [ ] E2E test: kind cluster + Crossplane, apply XRD/Composition/Function, create TeamConfig with 2 members → verify MachineConfig + ConfigPolicy CRDs generated
- [ ] Test member addition/removal → MachineConfig created/garbage-collected
- [ ] CI pipeline to build and push function-cfgd image
- [ ] Verify XRD `apiextensions.crossplane.io/v2` compatibility with target Crossplane version

### Server-side apply

- [ ] Field manager annotations: Crossplane sets `spec`, operator sets `status`, policy controller adds annotations
- [ ] Structured merge diff annotations on CRD OpenAPI schema

### Idiomatic naming audit

- [ ] Cross-references use `moduleRef`/`configRef` style
- [ ] Enum values use TitleCase (`IfNotPresent`, `Always`, `Symlink`)
- [ ] CLI flags and config fields align with k8s conventions
- [ ] All CRD field names use camelCase

---

## Tier 2 — Module CRD & OCI foundation (needs Module CRD)

### Module CRD

- [ ] Cluster-scoped CRD: packages, files, scripts, env, depends, `ociArtifact`, `signature.cosign.publicKey`
- [ ] Status: `resolvedArtifact`, `availablePlatforms`, `verified`, conditions (`Available`, `Verified`)
- [ ] Printer columns: `NAME ARTIFACT VERIFIED PLATFORMS AGE`. Short name: `mod`
- [ ] Module controller: validate OCI ref against trusted registries, verify cosign signature, set conditions

### Validation webhook enhancements

- [ ] `/validate-driftalert` endpoint
- [ ] `/validate-clusterconfigpolicy` endpoint
- [ ] `/validate-module` endpoint (OCI reference format, signature fields)
- [ ] Trusted registry enforcement: reject Module CRD with untrusted `ociArtifact` prefix

### OCI pipeline Phase A — push/pull (2-4 weeks)

- [ ] OCI manifest/layer structure: `application/vnd.cfgd.module.v1+tar+gzip`, config blob with module metadata
- [ ] Integrate `oci-distribution` or `oras-rs` crate
- [ ] Registry auth via Docker config.json / credential helpers
- [ ] `cfgd module push <dir>`: push directory as OCI artifact
- [ ] `cfgd module pull <ref>`: download OCI artifact

### OCI pipeline Phase D — CRD sync (1-2 weeks)

- [ ] `cfgd module push --apply`: create/update Module CRD on cluster from pushed artifact

---

## Tier 3 — OCI build & supply chain (needs OCI pipeline)

### OCI pipeline Phase B — module build (4-8 weeks)

- [ ] `cfgd module build --target <platform>`: resolve module, install into isolated root (container/chroot), collect binaries/config/env, package as OCI artifact
- [ ] Multi-platform builds (one layer per platform)
- [ ] Docker/Podman integration for container-based builds

### OCI pipeline Phase C — signing (1-2 weeks)

- [ ] `cfgd module push --sign` with cosign
- [ ] Verification at pull time; reject unsigned when policy requires
- [ ] Key management: static keys and keyless (Fulcio + Rekor)

### Supply chain security

- [ ] SLSA Level 3 provenance attestations for binaries and container images via `slsa-framework/slsa-github-generator`
- [ ] In-toto attestation support on Module OCI artifacts (verify at resolution time)

---

## Tier 4 — Pod module injection (needs CSI driver)

### CSI driver (3-6 months)

- [ ] Separate binary in `crates/cfgd-csi/`, Node plugin only, `tonic` gRPC
- [ ] Identity RPCs: `GetPluginInfo`, `GetPluginCapabilities`, `Probe`
- [ ] Node RPCs: `NodePublishVolume` (pull OCI artifact, mount read-only), `NodeUnpublishVolume`, `NodeStageVolume` (cache), `NodeUnstageVolume`
- [ ] DaemonSet deployment with `node-driver-registrar` sidecar
- [ ] Node-level cache at `/var/lib/cfgd-csi/cache/` with LRU eviction (default 5Gi)
- [ ] CSI metrics: `cfgd_csi_volume_publish_total`, `pull_duration_seconds`, `cache_size_bytes`, `cache_hits_total`

### Pod module mutating webhook

- [ ] `POST /mutate-pods` endpoint — parse `cfgd.io/modules` annotation + ConfigPolicy `requiredModules`
- [ ] MutatingWebhookConfiguration: `failurePolicy: Ignore`, `sideEffects: None`, namespace label selector
- [ ] Inject CSI volumes, volumeMounts, env vars per module spec
- [ ] Inject init container + shared emptyDir for modules with `scripts.postApply`
- [ ] Emit `ModuleInjected` / `ModuleInjectionFailed` Events on pods

### kubectl cfgd plugin

- [ ] `debug`, `exec`, `inject`, `status`, `version` subcommands
- [ ] Same binary as cfgd, plugin mode via argv[0] detection
- [ ] Ephemeral container creation with CSI volumes, PATH extension, custom PS1
- [ ] Krew manifest and distribution

---

## Tier 5 — Stability & graduation (needs production time)

### CRD versioning

- [ ] Conversion webhook for v1alpha1 → v1beta1
- [ ] Both versions served simultaneously, v1beta1 as storage version
- [ ] Migration runbook: deploy, convert on read, storage migration, remove v1alpha1
- [ ] Graduation criteria: 3+ months production, stable schema for 1 month, E2E passing

---

## Independent work (no tier dependencies)

### Windows support

Full design in [windows-support.md](windows-support.md). 26 unguarded `std::os::unix` uses across 13 files.

- [ ] Phase 1 — compilation gates: `#[cfg(unix)]` on all 26 unix-specific sites (reconciler symlink restore, upgrade chmod, daemon signals, files `PermissionsExt`, system configurators, secrets perms). Add `cargo build --target x86_64-pc-windows-msvc` CI job
- [ ] Phase 2 — file management: symlink → try `std::os::windows::fs::symlink_file`/`symlink_dir`, fallback to copy if permission denied. Skip Unix permission bits on NTFS (different ACL model)
- [ ] Phase 3 — package managers: `winget.rs` (`winget list`/`install --accept-agreements`/`uninstall`), `chocolatey.rs` (`choco list --local-only`/`install -y`/`uninstall -y`, bootstrap via PowerShell), `scoop.rs` (`scoop list`/`install`/`uninstall`, bootstrap via `irm get.scoop.sh | iex`). Config schema: `spec.packages.winget`, `.chocolatey`, `.scoop`
- [ ] Phase 4 — PowerShell env integration: generate `~/.cfgd-env.ps1` with `$env:NAME = "value"` syntax, inject `. ~/.cfgd-env.ps1` into `$PROFILE` (idempotent). System env via `setx` for registry-level vars
- [ ] Phase 5 — Windows Service daemon: `windows-service` crate behind `#[cfg(windows)]`, `cfgd daemon install` → SCM registration, `cfgd daemon start/stop` → SCM control, Event Log integration instead of syslog
- [ ] Phase 6 — CI and release: cross-compile job (`x86_64-pc-windows-msvc`), `.zip` release artifact alongside `.tar.gz`, Windows-specific docs section

### Ecosystem integration

- [ ] OPA/Kyverno policy library at `policies/` in repo root — Rego/YAML examples for: trusted registry enforcement on Module CRDs, signed module requirement, security baseline checks (e.g., require `capabilities.drop: [ALL]`). Publish to artifact hub
- [ ] OLM bundle for OpenShift — package cfgd-operator as OperatorHub-installable operator, define update channel strategy (stable/candidate)
- [ ] GitHub Actions action: `cfgd-org/actions/apply@v1` — runs `cfgd apply --dry-run --output json` in PRs, posts plan diff as PR comment. Docker-based action wrapping cfgd binary
- [ ] GitLab CI template: `.gitlab-ci.yml` include template with `cfgd apply` stage
- [ ] Tekton task: Task definition for `cfgd apply` in Tekton pipelines
- [ ] DevContainer Feature adapter: `cfgd module export --format=devcontainer` converts a `module.yaml` into a DevContainer Feature (install.sh + devcontainer-feature.json) publishable to GHCR

### Upstream KEPs (blocked on v1 CRD graduation + production adoption)

Viable after Tier 5 completes and cfgd has months of production usage demonstrating the annotation-driven pattern works.

- [ ] KEP: `spec.modules[].moduleRef` pod spec field — native PodSpec field for declaring module dependencies, replaces `cfgd.io/modules` annotation. Scheduler-transparent, webhook-consumed. Graduation: alpha (feature-gated) → beta (default-on, deprecate annotation) → GA
- [ ] KEP: `cfgdModule:` volume type — native volume type alongside `configMap:` and `secret:`, simplifies pod YAML by eliminating explicit CSI volume definitions
- [ ] KEP: `kubectl debug --module` flag — extend `kubectl debug` to accept `--module name:version` for injecting modules into ephemeral debug containers, replaces `kubectl cfgd debug`
