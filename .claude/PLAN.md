# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in [COMPLETED.md](COMPLETED.md). Design detail in [kubernetes-first-class.md](kubernetes-first-class.md).

---

## Distribution & publishing gaps

Blocking real-world adoption of shipped Kubernetes features (Tiers 1–4). Do first.

- [ ] CSI driver container image: `crates/cfgd-csi/` is implemented but has no Dockerfile and no release workflow. Helm chart references `ghcr.io/tj-smith47/cfgd-csi` which doesn't exist. Add `Dockerfile.csi` and build job to `release.yml`.
- [ ] kubectl-cfgd binary: `main.rs` detects `argv[0] == "kubectl-cfgd"` but release workflow only builds `cfgd`. Add symlink/rename step to release job to produce `kubectl-cfgd` artifacts for all 4 platforms.
- [ ] Krew manifest: `manifests/krew/cfgd.yaml` has SHA256 values of `"TBD"`, version hardcoded to `v0.1.0`, URLs reference non-existent binaries. Populate dynamically in release workflow after kubectl-cfgd artifacts are built.
- [ ] Helm chart registry: chart at `chart/cfgd/` is not published. Add chart-releaser or OCI push job to `release.yml` so users can `helm install` from a registry instead of local checkout.
- [ ] OLM bundle: `ecosystem/olm/` CSV has hardcoded `v0.1.0`, no bundle image build, no OperatorHub submission workflow. Add OLM bundle build job to `release.yml`.

## Ecosystem integration

Tiers 1–2 landed. These updates are unblocked — pick up now.

- [ ] Update `policies/` for new CRD fields: ClusterConfigPolicy CRD, Module CRD `spec.signature.cosign.publicKey`, `spec.security.trustedRegistries`, MachineConfig conditions split (Reconciled, DriftDetected, ModulesResolved, Compliant), `observedGeneration` on Condition struct, DriftAlert conditions (Acknowledged, Resolved, Escalated)
- [ ] Update `ecosystem/olm/` CSV to include ClusterConfigPolicy and Module CRDs, new webhook endpoints (`/validate-module`, `/validate-clusterconfigpolicy`, `/validate-driftalert`, `/mutate-pods`), printer columns, short names
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

## Windows support

Full design in [windows-support.md](windows-support.md). 26 unguarded `std::os::unix` uses across 13 files. No Kubernetes dependency — pick up when targeting Windows users.

- [ ] Phase 1 — compilation gates: `#[cfg(unix)]` on all 26 unix-specific sites. Add `cargo build --target x86_64-pc-windows-msvc` CI job
- [ ] Phase 2 — file management: symlink fallback to copy, skip Unix permission bits on NTFS
- [ ] Phase 3 — package managers: winget, chocolatey, scoop
- [ ] Phase 4 — PowerShell env integration
- [ ] Phase 5 — Windows Service daemon
- [ ] Phase 6 — CI and release: cross-compile job, `.zip` release artifact, Windows docs
