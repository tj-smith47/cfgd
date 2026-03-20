# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md).

---

## ~~Tier 1 — Operator hardening & existing CRD enhancement (no new dependencies)~~

Completed. Moved to [COMPLETED.md](COMPLETED.md).

---

## ~~Tier 2 — Module CRD & OCI foundation (needs Module CRD)~~

Completed. Moved to [COMPLETED.md](COMPLETED.md).

### Implementation notes

- OCI client implemented using `ureq` (sync HTTP) with hand-rolled OCI Distribution Spec client instead of `oci-client` crate — lighter weight, fewer dependencies, sync-friendly
- Signature verification checks PEM key format validity; actual cryptographic verification against OCI artifact signatures deferred to Tier 3 (OCI pipeline Phase C — signing) when `sigstore-rs` is integrated
- Integration tests (webhook policy enforcement against live cluster, push/pull against real registry, push --apply CRD creation) require a running Kubernetes cluster and OCI registry — these are exercised via E2E testing, not unit tests

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
