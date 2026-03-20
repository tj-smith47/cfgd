# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md).

---

## ~~Tier 1 — Operator hardening & existing CRD enhancement (no new dependencies)~~

Completed. Moved to [COMPLETED.md](COMPLETED.md).

---

## Tier 2 — Module CRD & OCI foundation (needs Module CRD)

### Module CRD

- [ ] `ModuleSpec` struct with kube derive: cluster-scoped, group `cfgd.io/v1alpha1`, short name `mod`, category `cfgd`. Fields: packages (`Vec<PackageEntry>`), files (`Vec<ModuleFileSpec>`), scripts (`ModuleScripts`), env (`Vec<ModuleEnvVar>`), depends (`Vec<String>`), ociArtifact (`Option<String>`), signature (`Option<ModuleSignature>`)
- [ ] Supporting types: `PackageEntry` (name, platforms `BTreeMap`), `ModuleFileSpec` (source, target), `ModuleScripts` (postApply), `ModuleEnvVar` (name, value, append), `ModuleSignature`/`CosignSignature` (publicKey). All derive `Deserialize`, `Serialize`, `Clone`, `Debug`, `Default`, `JsonSchema`; all `#[serde(rename_all = "camelCase")]`
- [ ] `ModuleStatus` struct: `resolvedArtifact` (`Option<String>`), `availablePlatforms` (`Vec<String>`), `verified` (bool), `conditions` (`Vec<Condition>`)
- [ ] Printer columns: `NAME` (metadata.name), `ARTIFACT` (.spec.ociArtifact), `VERIFIED` (.status.verified), `PLATFORMS` (.status.availablePlatforms), `AGE` (metadata.creationTimestamp)
- [ ] `ModuleSpec::validate()`: non-empty package names, non-empty depends entries, valid OCI reference format when ociArtifact is set, valid PEM-encoded public key when signature.cosign.publicKey is set
- [ ] Module controller: watch Module CRDs, validate ociArtifact against trusted registries from all ClusterConfigPolicy resources, set `Available` condition (True when ociArtifact resolves to a valid digest in the registry, False with reason if missing/untrusted/unreachable). Verify cosign signature using `sigstore-rs`: if `spec.signature.cosign.publicKey` is set, verify OCI artifact signature against it and set `Verified` condition (True/SignatureValid or False/SignatureInvalid); if no signature config, set Verified=False/NotSigned. When `ClusterConfigPolicy.spec.security.allowUnsigned == false`, set Available=False/UnsignedNotAllowed for modules without valid signatures. Emit events: `Available`, `Verified`, `PullFailed`, `SignatureInvalid`, `TrustedRegistryViolation`, `UnsignedNotAllowed`
- [ ] MachineConfig controller enhancement: resolve each `moduleRef` against Module CRDs (cluster-scoped API lookup), set `ModulesResolved` condition to False with comma-separated missing module names, or True/AllResolved
- [ ] Update `gen_crds.rs` to include Module CRD; regenerate Helm CRD templates

### Validation webhook enhancements

- [ ] `/validate-module` endpoint: delegates to `ModuleSpec::validate()`, plus trusted registry enforcement (read all ClusterConfigPolicy resources, collect `trustedRegistries`, glob-match against `spec.ociArtifact`, reject if no pattern matches and registries are configured). When `allowUnsigned == false` in any ClusterConfigPolicy, reject Module creates/updates that lack `spec.signature.cosign.publicKey`
- [ ] ValidatingWebhookConfiguration rule in Helm chart for Module CRD pointing to `/validate-module`
- [ ] RBAC: add Module CRD verbs (get, list, watch, create, update, patch, delete) and ClusterConfigPolicy read (get, list) to operator ClusterRole
- [ ] Unit tests: accept valid Module, reject empty package name, reject malformed OCI reference, reject untrusted registry, reject invalid PEM key
- [ ] Integration test: create Module via webhook, verify accepted; create Module with untrusted ociArtifact, verify rejected

### OCI pipeline Phase A — push/pull

- [ ] Add `oci-distribution` and `sigstore-rs` crate dependencies to cfgd-core (shared by cfgd binary for push/pull CLI and cfgd-operator for controller artifact resolution + signature verification). Define media type constants: config `application/vnd.cfgd.module.config.v1+json`, layer `application/vnd.cfgd.module.layer.v1.tar+gzip`
- [ ] Registry auth module: parse `~/.docker/config.json` for registry credentials, support credential helper programs (`docker-credential-*`), support `REGISTRY_USERNAME`/`REGISTRY_PASSWORD` env vars as fallback
- [ ] `cfgd module push <dir> --artifact <ref>`: read module.yaml from dir, serialize as config blob, tar+gzip directory contents as single layer with `cfgd.io/platform` annotation (auto-detected from host), build OCI manifest, push to registry
- [ ] `cfgd module pull <ref> --output <dir>`: authenticate to registry, pull OCI manifest, download layer matching current platform (or only layer if single-platform), extract tar+gzip to output directory. If module has cosign signature, verify it (using cfgd-core's sigstore-rs integration); reject unsigned artifacts when `--require-signature` flag is set
- [ ] Wire `push` and `pull` subcommands into clap under `cfgd module` with argument parsing (required: dir/ref, artifact ref; optional: platform override, output dir)
- [ ] Unit tests: OCI reference format parsing, config blob serialization/deserialization round-trip, tar+gzip archive creation and extraction
- [ ] Integration test: push to mock/local registry, pull back, verify extracted contents match original directory

### OCI pipeline Phase D — CRD sync

- [ ] `cfgd module push --apply` flag: after successful push, construct Module CRD from module.yaml metadata + pushed ociArtifact reference, apply to cluster via server-side apply with field manager `cfgd`
- [ ] Kubeconfig discovery: check in-cluster service account first, then `KUBECONFIG` env var, then `~/.kube/config` default path
- [ ] Integration test: push --apply creates Module CRD with correct ociArtifact, re-push updates the existing CRD

---

## Tier 3 — OCI build & supply chain (needs OCI pipeline)

### OCI pipeline Phase B — module build (4-8 weeks)

- [ ] `cfgd module build --target <platform>`: resolve module, install into isolated root (container/chroot), collect binaries/config/env, package as OCI artifact
- [ ] Multi-platform builds (one layer per platform)
- [ ] Docker/Podman integration for container-based builds

### OCI pipeline Phase C — signing

- [ ] `cfgd module push --sign`: sign OCI artifact with cosign at push time (static key via `--key` flag)
- [ ] Keyless signing via Fulcio + Rekor (OIDC identity-based, no static key management)
- [ ] `cfgd module keys` subcommand: generate cosign key pairs, list keys, rotate

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

### Ecosystem integration — post-k8s updates

Once CRD enhancements (Tier 1) and Module CRD (Tier 2) land, update ecosystem files:

- [ ] Update `policies/` for new CRD fields: ClusterConfigPolicy CRD, Module CRD `spec.signature.cosign.publicKey`, `spec.security.trustedRegistries`, MachineConfig conditions split (Reconciled, DriftDetected, ModulesResolved, Compliant), `observedGeneration` on Condition struct, DriftAlert conditions (Acknowledged, Resolved, Escalated)
- [ ] Update `ecosystem/olm/` CSV to include ClusterConfigPolicy and Module CRDs, new webhook endpoints (`/validate-module`, `/validate-clusterconfigpolicy`, `/validate-driftalert`, `/mutate-pods`), printer columns, short names
- [ ] Update idiomatic naming in ecosystem files after naming audit: `moduleRef`/`configRef` style cross-references, TitleCase enums, camelCase CRD field names

### Upstream KEPs (blocked on v1 CRD graduation + production adoption)

Viable after Tier 5 completes and cfgd has months of production usage demonstrating the annotation-driven pattern works.

- [ ] KEP: `spec.modules[].moduleRef` pod spec field — native PodSpec field for declaring module dependencies, replaces `cfgd.io/modules` annotation. Scheduler-transparent, webhook-consumed. Graduation: alpha (feature-gated) → beta (default-on, deprecate annotation) → GA
- [ ] KEP: `cfgdModule:` volume type — native volume type alongside `configMap:` and `secret:`, simplifies pod YAML by eliminating explicit CSI volume definitions
- [ ] KEP: `kubectl debug --module` flag — extend `kubectl debug` to accept `--module name:version` for injecting modules into ephemeral debug containers, replaces `kubectl cfgd debug`
