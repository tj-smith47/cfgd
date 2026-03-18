# cfgd — Remaining Work

Single source of truth for all incomplete work. Completed work is in `INITIAL-PLAN.md`.

---

## Kubernetes API conventions

Full condition lifecycle, finalizers, owner references, server-side apply, and idiomatic naming alignment. Prerequisite for pod module injection. Design detail in [kubernetes-first-class.md § 1](kubernetes-first-class.md#1-kubernetes-api-conventions).

- [ ] Full condition lifecycle on all CRDs: `MachineConfig` (Reconciled, DriftDetected, ModulesResolved, Compliant), `ConfigPolicy` (Enforced, Violated), `DriftAlert` (Acknowledged, Resolved, Escalated) — each with `lastTransitionTime`, `reason`, `message`, set via `/status` subresource
- [ ] Finalizers on MachineConfig: signal device daemon to un-manage resources, optional rollback, remove finalizer after cleanup
- [ ] Owner references: TeamConfig (XR) → MachineConfig → DriftAlert cascade
- [ ] Server-side apply: field manager annotations, structured merge diff on CRD OpenAPI schema
- [ ] ClusterConfigPolicy CRD (cluster-scoped): org-wide mandates, operator merges with namespace-scoped ConfigPolicy
- [ ] Idiomatic Kubernetes naming audit: cross-references use `moduleRef`/`configRef` style, enum values use TitleCase (`IfNotPresent`, `Always`, `Symlink`), CLI flags and config fields align with k8s conventions (`--dry-run=server`, `imagePullPolicy`-style patterns), all CRD field names use camelCase per k8s API conventions

---

## Helm chart restructure

Move from `crates/cfgd-operator/chart/cfgd-operator/` to `chart/cfgd/` at repo root. Chart represents the product, not a single crate.

- [ ] Move chart to `chart/cfgd/`
- [ ] Add subcomponent toggles: `operator.enabled`, `csiDriver.enabled`, `webhook.enabled`, `deviceGateway.enabled`
- [ ] Update templates for merged operator (includes device gateway routes, ports, ingress)
- [ ] Add documented `values.yaml` examples per use case (operator-only, pod-modules-only, full)
- [ ] Update all doc references to chart location

---

## Module OCI pipeline

`cfgd module build` and `cfgd module push` — package modules as OCI artifacts for in-cluster consumption. Design detail in [kubernetes-first-class.md § 2](kubernetes-first-class.md#module-content-pipeline).

- [ ] `cfgd module build --target <platform>`: resolve module for target platform, install packages into isolated root, collect binaries/config/env, package as ORAS-compliant OCI artifact, sign with cosign
- [ ] `cfgd module push <registry>/<name>:<tag>`: push signed OCI artifact to registry
- [ ] `cfgd module pull <registry>/<name>:<tag>`: pull OCI artifact
- [ ] Module CRD: add `ociArtifact` and `signature` fields to Module kind (populated by push, not hand-authored)

---

## Pod module injection

CSI driver, mutating webhook, and kubectl plugin for injecting modules into pods. Design detail in [kubernetes-first-class.md § 2](kubernetes-first-class.md#2-modules-as-podcontainer-primitives).

- [ ] CSI driver (`csi.cfgd.io`): read Module CRD → pull OCI artifact from registry → mount as read-only volume at `/cfgd-modules/<name>/`, node-level cache
- [ ] Pod module mutating webhook: parse `cfgd.io/modules` annotation, read ConfigPolicy `requiredModules`, inject CSI volumes + volume mounts + env vars, inject init container if module has scripts, validate against `trustedRegistries`, emit Kubernetes Events
- [ ] kubectl plugin (`kubectl cfgd`): `debug`, `exec`, `inject` subcommands with `--module name:version` repeatable flag, ephemeral container creation, Krew manifest
- [ ] DaemonSet node-level cache: pre-pull popular module layers
- [ ] Prometheus metrics: `cfgd_module_injections_total`, `cfgd_csi_pull_duration_seconds`

---

## Observability

- [ ] Prometheus metrics endpoint on operator (`/metrics`): reconciliation counts, drift events, compliance, enrollment, module injection, CSI pull latency
- [ ] Kubernetes Events on MachineConfig (Reconciled, DriftDetected, PolicyApplied) and pods (ModuleInjected, ModuleInjectionFailed)
- [ ] Audit logging: device gateway enrollment/token/decision events surfaced as queryable API
- [ ] OpenTelemetry tracing: `tracing-opentelemetry` in Rust, W3C trace context propagation across Crossplane → operator → device checkin → local apply

---

## CLI UX improvements

- [ ] Buffered script output: post-apply and lifecycle scripts should render output in a bounded terminal region (like Docker build layers) that scrolls in place during execution and collapses to a summary line on completion. Currently uses inherited stdio which mixes script output with cfgd output.
- [ ] `module show` — display apply status (applied/unapplied, last applied timestamp) from state store
- [ ] `module show` — mask env values by default, `--show-values` flag to reveal
- [ ] `cfgd status` / `cfgd verify` — work without a profile when modules are applied directly

---

## Windows support

Full Windows CLI support: compile gates, file management, package managers (winget, chocolatey, scoop), PowerShell env integration, Windows Service daemon. Design detail in [windows-support.md](windows-support.md).

- [ ] Phase 1: `#[cfg(unix)]` guards — compile on Windows, CI Windows job
- [ ] Phase 2: Windows file management — symlink fallback to copy, skip Unix permissions
- [ ] Phase 3: Package managers — winget, chocolatey, scoop trait implementations
- [ ] Phase 4: PowerShell env/profile integration, `setx` for system env
- [ ] Phase 5: Windows Service daemon
- [ ] Phase 6: CI cross-compile, release artifacts (.zip), docs

---

## Ecosystem integration

- [ ] OPA/Kyverno policy library: published examples for trusted registries, signed modules, security baselines
- [ ] OLM bundle for OpenShift
- [ ] CI runner integrations: GitHub Actions action, GitLab CI template, Tekton task
- [ ] DevContainer Feature generation from cfgd modules (export adapter)

---

## Upstream KEPs (blocked on adoption of annotation-driven approach)

- [ ] KEP draft: `spec.modules[].moduleRef` pod spec field
- [ ] KEP draft: `cfgdModule:` volume type
- [ ] KEP draft: `kubectl debug --module` flag
