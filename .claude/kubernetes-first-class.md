# cfgd: Kubernetes Ecosystem Integration

Design spec for making cfgd a first-class participant in the Kubernetes ecosystem. Covers API conventions, pod module injection, supply chain security, observability, multi-tenancy, and ecosystem integration.

For Crossplane integration, device enrollment, and fleet distribution, see [team-config-controller.md](team-config-controller.md).

## Decisions Record

| Decision | Resolution |
|---|---|
| API group | `cfgd.io/v1alpha1` everywhere — local files and Kubernetes resources |
| Module kind | One Module kind. Same `apiVersion` and `kind` locally and in-cluster. In-cluster adds `ociArtifact` and `signature` fields (populated by `cfgd module build/push`, not hand-authored) |
| Module CRD scope | Cluster-scoped. RBAC controls who can publish. |
| cfgd-server | Merged into cfgd-operator. One cluster-side binary. Device gateway is an optional feature of the operator, enabled via Helm values. |
| Helm chart | One chart at `chart/cfgd/` (repo root). Subcomponents toggled via `values.yaml` (`operator.enabled`, `csiDriver.enabled`, `webhook.enabled`, `deviceGateway.enabled`). |
| kubectl plugin | `kubectl cfgd debug/exec/inject`. Distributed via Krew and installed alongside cfgd. Same binary. |
| Pod module injection | CSI driver always mounts module content as read-only volume. If module has `scripts.post-apply`, webhook also injects an init container that runs scripts against the mounted volume. One path, not either/or. |
| Identity / auth | Standard Kubernetes RBAC. No custom identity binding. If a user can create ephemeral containers in a namespace, they can use `kubectl cfgd`. |
| Module references | `--module name:version` repeatable flag. Version required. Module must exist as a CRD on the cluster. |
| Trusted registries | `spec.security.trustedRegistries` on ConfigPolicy/ClusterConfigPolicy. Webhook validates Module CRD `ociArtifact` references against approved registries. |
| cfgd-server in pod flow | Not involved. Pod module injection is Kubernetes-native (CRDs, CSI, webhook). cfgd-server (now part of operator) handles device checkins for fleet management only. |
| ConfigPolicy + modules | ConfigPolicy can mandate modules on pods via `spec.requiredModules[].moduleRef`. Webhook reads ConfigPolicy in the pod's namespace and injects required modules alongside any pod-level annotations. |
| ClusterConfigPolicy | Cluster-scoped policy for org-wide mandates. Operator merges with namespace-scoped ConfigPolicy; cluster always wins. |
| CRD versioning | Start at v1alpha1. Conversion webhooks added when graduating to v1beta1. |

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│  ArgoCD (Platform GitOps)                               │
│  Manages: cfgd-operator, Crossplane func, CRDs,         │
│           RBAC, Helm chart                              │
├─────────────────────────────────────────────────────────┤
│  Crossplane (Resource Generation) — optional            │
│  TeamConfig XR → fans out to:                           │
│    - per-user MachineConfig CRDs                        │
│    - per-user RBAC bindings                             │
│    - per-team ConfigPolicy CRDs                         │
│    - per-team namespace resources                       │
├─────────────────────────────────────────────────────────┤
│  cfgd-operator (single cluster-side binary)             │
│  Components (toggled via Helm values):                  │
│    - CRD controllers: MachineConfig, ConfigPolicy,      │
│      DriftAlert, Module reconciliation                  │
│    - Admission webhook: CRD spec validation,            │
│      trusted registry enforcement                       │
│    - Pod module webhook: annotation-driven module        │
│      injection into pods                                │
│    - CSI driver: serves pre-built module layers as       │
│      read-only volumes from OCI registry                │
│    - Device gateway: checkin API, enrollment,            │
│      drift aggregation, web dashboard                   │
├─────────────────────────────────────────────────────────┤
│  cfgd daemon (On-Device)                                │
│  Pulls config from git, reconciles, optionally          │
│  reports drift to operator's device gateway             │
└─────────────────────────────────────────────────────────┘
```

ArgoCD owns the platform layer. Crossplane (optional) owns resource generation. cfgd-operator owns reconciliation, module delivery, and device communication.

Exception: standalone MachineConfigs (CI runners, bare metal not in a team) can be ArgoCD-managed directly since no TeamConfig XR generates them.

---

## 1. Kubernetes API Conventions

### Condition Lifecycle
- `MachineConfig`: `Reconciled`, `DriftDetected`, `ModulesResolved`, `Compliant`
- `ConfigPolicy`: `Enforced`, `Violated`
- `DriftAlert`: `Acknowledged`, `Resolved`, `Escalated`
- Each with `lastTransitionTime`, `reason`, `message`
- Set atomically via `/status` subresource

### Finalizers
- `MachineConfig` deletion triggers:
  1. Signal target device daemon to un-manage resources
  2. Optional rollback (`spec.cleanup.rollback: true`)
  3. Remove finalizer after cleanup completes

### Owner References & GC
- `TeamConfig` (XR) → `MachineConfig` → `DriftAlert`
- Remove team member from XR → MachineConfig garbage collected → DriftAlerts cascade

### Server-Side Apply
- Field managers: Crossplane sets `spec`, operator sets `status`, policy controller adds annotations
- Structured merge diff annotations on CRD OpenAPI schema

---

## 2. Modules as Pod/Container Primitives

### Problem

Kubernetes has ConfigMaps (files/env vars) and Secrets (sensitive files/env vars). Both are passive data. There is no native concept of "add this capability to this pod" — binaries, tools, config that requires setup. Developers shell into containers and get bare `sh`. Every sidecar injection system (Istio, Datadog, Vault agent) invented its own ad-hoc webhook. No standard exists for composable, injectable environment capabilities.

**ConfigMaps deliver configuration data. Secrets deliver sensitive data. cfgd Modules deliver environment capabilities.**

### The Module Kind

One kind, everywhere. Same `apiVersion`, same `spec`. In-cluster adds fields populated by tooling:

```yaml
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: network-debug
spec:
  # Core fields — same as local module.yaml
  packages:
    - name: tcpdump
    - name: dig
    - name: curl
    - name: netcat
    - name: mtr
  files:
    - source: config/
      target: /etc/cfgd/network-debug/
  scripts:
    post-apply:
      - /etc/cfgd/network-debug/setup.sh
  env:
    - name: PATH
      append: /cfgd-modules/network-debug/bin

  # Populated by `cfgd module build/push` — not hand-authored
  ociArtifact: registry.cfgd.io/modules/network-debug:1.2
  signature:
    cosign:
      publicKey: ...
```

### Declarative Pod Integration

Modules on pods are declared in the pod spec, committed to git, reviewed in PRs — GitOps native:

```yaml
# Annotation-driven (works with existing K8s, no upstream changes)
metadata:
  annotations:
    cfgd.io/modules: "network-debug:1.2,audit-logger:1.0"

# Native pod spec field (requires KEP)
spec:
  modules:
  - moduleRef:
      name: network-debug
      version: "1.2"
  - moduleRef:
      name: audit-logger
      version: "1.0"
```

ConfigPolicy can also mandate modules on all pods in a namespace:

```yaml
apiVersion: cfgd.io/v1alpha1
kind: ConfigPolicy
metadata:
  name: backend-policy
spec:
  requiredModules:
  - moduleRef:
      name: audit-logger
      version: "1.0"
```

The webhook injects required modules even if the pod spec doesn't declare them.

### Ad-hoc Injection

For unplanned situations (incidents, debugging), `kubectl cfgd` provides ad-hoc module injection via ephemeral containers:

```bash
kubectl cfgd debug pod/my-service --module network-debug:1.2 --module go-profiler:2.0
kubectl cfgd exec pod/my-service --module nvim:1.9 -- bash
kubectl cfgd inject pod/my-service --module network-debug:1.2
```

`--module name:version`, repeatable. Version required. Module must exist as a CRD on the cluster.

### Mechanics

#### CSI Driver (`csi.cfgd.io`)

The CSI driver is the primary delivery mechanism:

1. Reads the Module CRD to find the OCI artifact reference
2. Pulls the pre-built module layer from the OCI registry (cached on node after first pull)
3. Mounts it as a read-only volume at `/cfgd-modules/<module-name>/`

No runtime package installation. Startup cost is a volume mount + OCI pull (cached on node). Milliseconds after first pull.

```yaml
# Generated by the webhook — user doesn't write this
volumes:
- name: cfgd-module-network-debug
  csi:
    driver: csi.cfgd.io
    readOnly: true
    volumeAttributes:
      module: network-debug
      version: "1.2"
```

#### Init Container (scripts)

If a module has `scripts.post-apply`, the webhook also injects an init container that runs those scripts against the CSI-mounted volume. The init container writes results to a shared emptyDir. This handles runtime-dependent setup (e.g., updating a trust store based on the pod's environment).

CSI volume always mounts. Init container added only when scripts need to run. One path, not either/or.

#### Mutating Webhook

The pod module webhook watches for pods with `cfgd.io/modules` annotations or pods in namespaces with ConfigPolicy-mandated modules. It:

1. Parses module references from annotation and/or ConfigPolicy
2. Validates modules exist as CRDs and reference approved registries
3. Injects CSI volume definitions for each module
4. Injects volume mounts on target containers
5. Extends PATH and other env vars per module spec
6. If module has scripts, injects init container + shared emptyDir
7. Records injection as a Kubernetes Event on the pod

#### kubectl Plugin (`kubectl cfgd`)

Distributed via Krew for non-cfgd users. Installed alongside cfgd for cfgd users. Same binary.

Handles:
- Creating ephemeral containers with module CSI volumes attached
- Setting up the shell environment (PS1, aliases, PATH)
- RBAC check: if the user can create ephemeral containers, they can use the plugin

### Module Content Pipeline

#### Build

```bash
cfgd module build --target debian:bookworm --target alpine:3.19
```

1. Resolves the module for target platforms
2. Installs packages into an isolated root
3. Collects binaries, config files, env var definitions
4. Packages as an OCI artifact (ORAS-compliant)
5. Signs with cosign

#### Publish

```bash
cfgd module push registry.cfgd.io/modules/network-debug:1.2
```

Modules are OCI artifacts in any OCI-compliant registry (Harbor, ECR, GCR, GHCR, Docker Hub). Same distribution infrastructure organizations already have.

#### Consume

On workstations: same `module.yaml`, resolved locally by cfgd. One module definition, both targets.

In-cluster: Module CRD references the OCI artifact. CSI driver pulls it. `cfgd module push` can create/update the in-cluster CRD.

### Use Cases

#### Developer Inner-Loop

**QA environments.**
QA namespace ConfigPolicy: all pods get the `qa-tools` module (test runners, coverage tools, profilers). QA engineers shell into any pod and tools are present. No custom images per service.

**Team-standard tooling.**
A team's ConfigPolicy declares `spec.requiredModules: [{moduleRef: {name: network-debug}}, {moduleRef: {name: go-debug}}]`. Any pod in their namespace automatically gets these tools via the webhook.

#### Operational

**On-demand observability injection.**
Production incident. `kubectl cfgd inject pod/my-service --module go-profiler:2.0`. Ephemeral container with Delve, pprof, async-profiler appears, connected to the pod's process namespace.

**Database tooling.**
Module `postgres-tools` injects psql, pgcli, pg_dump. Connection env vars already in the pod's environment.

**Network debugging.**
Module `network-debug` gives tcpdump, dig, curl, netcat in the pod's network namespace, with the pod's DNS config.

#### Production

**Compliance and audit injection.**
ClusterConfigPolicy mandates `audit-logger` module on all pods in regulated namespaces. Module injects a sidecar logging file access, network connections, process execution.

**Certificate/TLS provisioning.**
Module `tls-bootstrap` mounts a cert-manager Secret, runs a post-apply script to update the container's CA trust store, sets SSL_CERT_DIR. The glue between "cert exists" and "app trusts cert."

**Sidecar standardization.**
One webhook, one annotation scheme, standard lifecycle. A module defines what to inject. Organizations with multiple injection systems get one mechanism for the cases not covered by dedicated projects.

### Questions, Concerns, and Answers

**Q: Doesn't injecting tools at runtime violate container immutability?**

The CSI driver adds a read-only volume; the container image is untouched. Binaries live in the volume, PATH is extended via env vars. The image layer is not modified. Immutability is about reproducibility — pinned module version + deterministic OCI artifact = reproducible, regardless of delivery mechanism.

**Q: Security — a webhook injecting volumes into pods is a large attack surface.**

Established pattern (Istio, Vault agent injection):
- Webhook only fires on explicit opt-in (annotation or ConfigPolicy)
- Module content from signed OCI artifacts (Sigstore/cosign)
- RBAC controls who can annotate pods
- Kyverno/OPA policies restrict which modules are allowed where
- Every injection is an auditable Kubernetes Event

**Q: Startup latency from init containers?**

CSI driver with pre-built OCI layers: milliseconds (cached on node). DaemonSet can pre-pull popular modules. For ad-hoc debug via ephemeral containers, latency is acceptable.

**Q: Module updates on running pods?**

Same as ConfigMap updates. Dev/debug pods: restart. Production: version pinned in policy, update means rollout. CSI driver can support rotation for long-running pods.

**Q: Istio/Datadog won't rewrite as cfgd modules.**

They don't have to. cfgd modules handle the 80% of injection use cases without a dedicated project: debug tools, profilers, database clients, compliance agents, internal tooling.

**Q: Why not just build custom debug images?**

Per-service maintenance burden. 40 microservices = 40 debug Dockerfiles. Modules are per-capability, not per-service. They compose additively — independent volumes, independent capabilities.

**Q: Ephemeral containers already solve debugging.**

Ephemeral containers are the mechanism. Modules are the content. Ephemeral containers give you a bare shell with no tools, no persistence across sessions, no team standardization. Modules fill all of those gaps.

**Q: Cross-platform compatibility?**

Modules are pre-built per target platform as OCI artifacts. `cfgd module build --target debian:bookworm --target alpine:3.19`. CSI driver pulls the correct one. Missing platform → clear error with available targets.

**Q: Scope creep — too many targets?**

Core engine doesn't change: desired state → diff → apply. What changes is the adapter layer. Machine adapter: native package managers + filesystem. CSI adapter: volume mount from OCI registry. One module definition, multiple delivery mechanisms.

### What This Is NOT

- **Not a package manager for containers.** Augments pods with capabilities; doesn't replace Dockerfiles.
- **Not a service mesh.** No traffic routing, mTLS between services, or service discovery.
- **Not a replacement for Helm/Kustomize.** Operates at the pod level, not the application manifest level.
- **Not a build system.** `cfgd module build` produces OCI artifacts from module definitions; doesn't replace Docker/BuildKit.

---

## 3. Supply Chain Security

### Sigstore Integration
- Every `ConfigSource` manifest and `Module` spec carries a cosign signature
- `SourceManager` verifies signatures as a gate, not a warning (fixes current TODO in `sources/mod.rs`)
- Unsigned sources/modules rejected unless `spec.security.allowUnsigned: true`

### SLSA Build Provenance
- cfgd binaries get SLSA Level 3 provenance attestations (GitHub Actions + SLSA generator)
- Users verify provenance before trusting cfgd on their machines

### In-toto Attestations
- Module authors publish in-toto layouts: what files included, what scripts run, what packages installed
- cfgd verifies layout at module resolution time

### Admission Webhook Hardening
- Existing webhook validates CRD specs structurally
- Add: verify MachineConfig `moduleRef` references point to modules from `trustedRegistries`
- ConfigPolicy/ClusterConfigPolicy declare `spec.security.trustedRegistries: [registry.cfgd.io/acme-corp/*]`

For signature verification design on config sources specifically, see [team-config-controller.md § Signature Verification](team-config-controller.md#signature-verification).

---

## 4. Multi-Tenancy

### Namespace-per-Team Model
- Each team gets a namespace for TeamConfig, ConfigPolicy, MachineConfig resources
- RBAC: team leads get `edit` on their namespace, platform team gets `cluster-admin` on `cfgd.io` API group

### Cross-Namespace Policy Aggregation
- `ClusterConfigPolicy` (cluster-scoped) for org-wide mandates
- `ConfigPolicy` (namespace-scoped) for team-specific additions
- Operator merges both, cluster policies always win

### Device Identity Binding
- MachineConfig references a ServiceAccount or cert identity
- Operator validates device checking in actually owns the MachineConfig it claims

### Network Security
- NetworkPolicy restricts operator device gateway ingress to enrolled devices
- mTLS between daemon and operator for zero-trust environments

---

## 5. Observability

### Prometheus Metrics
```
cfgd_reconciliations_total{status, device}
cfgd_drift_events_total{resource_type, severity}
cfgd_devices_compliant{policy, namespace}
cfgd_devices_enrolled_total
cfgd_source_sync_duration_seconds{source}
cfgd_module_resolution_duration_seconds{module}
cfgd_module_injections_total{module, namespace}
cfgd_csi_pull_duration_seconds{module}
```

### Kubernetes Events
- Emitted on MachineConfig resources: `Reconciled`, `DriftDetected`, `PolicyApplied`
- Emitted on pods: `ModuleInjected`, `ModuleInjectionFailed`
- Visible via `kubectl describe`

### Audit Logging
- Device gateway: who enrolled which device, created which token, approved which decision
- CRD mutations captured by Kubernetes audit log
- Module injections logged as Events on pods

### OpenTelemetry Tracing
- Distributed traces across: Crossplane composition → operator → device checkin → local apply
- `tracing-opentelemetry` in Rust, W3C trace context propagation

---

## 6. Ecosystem Integration

### OPA/Kyverno Policy Library
- Published policy examples: require signed modules, enforce trusted registries, mandate security baselines
- Organizations enforce cfgd policies with their existing policy engine

### OCI Registry for Modules
- `cfgd module push registry.cfgd.io/acme-corp/nvim:v2.1.0`
- Versioned, signed, cached distribution through existing registry infra
- ORAS-compliant OCI artifacts

### Helm Chart
- Single chart at `chart/cfgd/` (repo root)
- Subcomponents: `operator.enabled`, `csiDriver.enabled`, `webhook.enabled`, `deviceGateway.enabled`
- Values schema, upgrade hooks
- OLM bundle for OpenShift

### Krew Plugin
- `kubectl cfgd` distributed via Krew index
- Also installed by cfgd itself on managed workstations

### CI Runner Integrations
- GitHub Actions, GitLab CI, Tekton task wrappers around `cfgd apply --module`

---

## 7. Developer-Facing Integration

### kubectl-Native Machine Interaction
- `kubectl get machineconfig my-laptop` — status, drift, last reconcile
- `kubectl describe machineconfig my-laptop` — conditions, events, module list

### Environment Parity via Unified Module Targets
- Same module spec configures a workstation, drives a DevContainer, builds an OCI artifact, or injects into a pod
- One declaration, every target

### CRD-Driven Machine Lifecycle
- `spec.role: offboarding` triggers: revoke cloud creds, remove VPN, wipe secrets, generate compliance report
- CVE response: ClusterConfigPolicy update propagates patched package to every machine within sync interval

### Portable Dev Environment Sharing
- `cfgd init --from acme-corp/backend-team` — one command onboarding
- Machine converges to team baseline with personal customizations layered via policy tiers
- Ongoing drift detection keeps machines in compliance

---

## 8. Maturity Requirements

### Governance
- CNCF sandbox → incubating → graduated pathway
- Open governance, public roadmap, RFC process, contributor ladder
- Conformance tests

### Documentation
- API reference from OpenAPI schemas
- ADRs, runbooks, migration guides

### Distribution
- Helm chart, Kustomize overlays, OLM bundle
- Homebrew, apt repos, static binaries with Sigstore attestations

### Testing
- E2E against real clusters (kind/k3s in CI)
- Chaos testing (operator device gateway down → daemon continues with cached config)
- CRD schema migration testing, multi-version skew testing

### Security
- CVE disclosure process
- `cargo audit` in CI
- Fuzzing for YAML config parsing
- Penetration testing for device gateway enrollment/checkin APIs

---

## Positioning

cfgd is to machine configuration what Flux is to cluster configuration — a GitOps-native reconciliation loop targeting the OS layer instead of the Kubernetes API.

Modules extend this into a new category: **portable, declarative environment capabilities** that work across machines, containers, and pods. ConfigMaps deliver configuration data. Secrets deliver sensitive data. cfgd Modules deliver environment capabilities.
