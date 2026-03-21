# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md).

---

## E2E test gaps

Existing E2E suites cover Tier 1 CRDs and CLI. Module, ClusterConfigPolicy, webhook mutation, CSI driver, OCI supply chain, and kubectl plugin have unit tests but no cluster-level validation.

### Operator E2E (`tests/e2e/operator/`) — expand existing script

- [x] Module CRD: create, verify controller sets status (verified, resolvedArtifact), webhook rejects invalid OCI refs and malformed PEM keys
- [x] ClusterConfigPolicy: create with namespaceSelector, verify only matching namespaces evaluated, verify cluster-wins merge with namespace ConfigPolicy
- [x] Validation webhooks: Module, ClusterConfigPolicy, DriftAlert endpoints reject invalid specs
- [x] Mutating webhook: pod with `cfgd.io/modules` annotation in labeled namespace gets CSI volumes injected, mountPolicy Debug skips volumeMount, env vars set on containers
- [x] OCI supply chain: push module to test registry (kind-hosted), pull with signature verification, verify content integrity
- [x] Update CRD wait loop to include all 5 CRDs (currently only waits for 3)

### Full-stack E2E (`tests/e2e/full-stack/`) — expand existing script

- [x] CSI driver: deploy DaemonSet via Helm, create pod referencing CSI volume, verify module content mounted read-only, verify unmount on pod delete
- [x] kubectl cfgd plugin: `inject deployment/test -m mod:v1` patches annotation, `status` lists modules, `version` returns server version
- [x] Debug flow: pod with mountPolicy Debug module, `kubectl cfgd debug` creates ephemeral container that accesses debug-only volume

## Ecosystem integration

- [ ] Update `policies/` for new CRD fields: ClusterConfigPolicy CRD, Module CRD `spec.signature.cosign.publicKey`, `spec.security.trustedRegistries`, MachineConfig conditions split (Reconciled, DriftDetected, ModulesResolved, Compliant), `observedGeneration` on Condition struct, DriftAlert conditions (Acknowledged, Resolved, Escalated)
- [ ] Update idiomatic naming in ecosystem files after naming audit: `moduleRef`/`configRef` style cross-references, TitleCase enums, camelCase CRD field names

## CRD versioning

Trigger: 3+ months production usage with stable schema (no breaking CRD field changes for 1 month). Do not start before that threshold — premature graduation creates conversion debt.

- [ ] Conversion webhook for v1alpha1 → v1beta1
- [ ] Both versions served simultaneously, v1beta1 as storage version
- [ ] Migration runbook: deploy, convert on read, storage migration, remove v1alpha1
- [ ] Graduation criteria documented and gates enforced in CI

## Upstream KEPs

Trigger: v1 CRD graduation complete + months of production usage demonstrating the annotation-driven pattern works. These are proposals to upstream Kubernetes, not cfgd implementation work.

- [ ] KEP: `spec.modules[].moduleRef` pod spec field — native PodSpec field for declaring module dependencies, replaces `cfgd.io/modules` annotation
- [ ] KEP: `cfgdModule:` volume type — native volume type alongside `configMap:` and `secret:`
- [ ] KEP: `kubectl debug --module` flag — extend `kubectl debug` for module injection into ephemeral debug containers

---

## Source management improvements

Platform-aware source profile auto-selection. Cross-platform sources (e.g., a team config with separate macOS/Ubuntu/Fedora profiles) should auto-detect the correct profile when none is explicitly specified during `cfgd source add`. Infrastructure exists in `config::match_platform_profile()`, `PlatformInfo`, and `detect_platform()` but is unwired and missing schema support.

- [ ] Add `platform_profiles: HashMap<String, String>` field to `ConfigSourceProvides` in `config/mod.rs` (maps platform identifier → profile name, e.g., `{"debian": "linux-debian", "macos": "macos-arm"}`)
- [ ] Update source manifest documentation in `docs/sources.md` to describe the new field with examples
- [ ] Wire `detect_platform()` → `match_platform_profile()` into `cmd_source_add` as fallback when user doesn't specify `--profile` and the source manifest provides `platform_profiles`
- [ ] Add tests for platform matching (exact distro match, OS fallback, no-match returns None)

---

## Windows support

Full design in [windows-support.md](windows-support.md). 26 unguarded `std::os::unix` uses across 13 files. No Kubernetes dependency — pick up when targeting Windows users.

- [ ] Phase 1 — compilation gates: `#[cfg(unix)]` on all 26 unix-specific sites. Add `cargo build --target x86_64-pc-windows-msvc` CI job
- [ ] Phase 2 — file management: symlink fallback to copy, skip Unix permission bits on NTFS
- [ ] Phase 3 — package managers: winget, chocolatey, scoop
- [ ] Phase 4 — PowerShell env integration
- [ ] Phase 5 — Windows Service daemon
- [ ] Phase 6 — CI and release: cross-compile job, `.zip` release artifact, Windows docs
