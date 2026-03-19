---
name: validate-k8s-conventions
description: Validate CRDs, configs, and code against Kubernetes API design conventions and community patterns
allowed-tools: ["Read", "Glob", "Grep"]
user-invocable: true
argument-hint: "[crds|config|helm|all]"
---

## Kubernetes API Design Convention Validation

Audit cfgd CRDs, config schemas, and Helm charts against the API design patterns used by the Kubernetes ecosystem. Compare with how core K8s APIs, cert-manager, Crossplane, Flux, ArgoCD, Istio, and Knative design their resources. Target: $ARGUMENTS (default: all).

The goal is to catch **API design smells** — structural decisions that would look wrong to someone experienced with the K8s ecosystem. This is NOT about naming conventions (validate-gitops and audit.sh handle that). This is about whether the **shape** of our APIs makes sense.

### CRD Spec Design Smells

Look for these anti-patterns in `crates/cfgd-operator/src/crds/mod.rs` and the CRD YAML templates:

**Denormalized parallel fields (critical smell):**
- [ ] Are there fields that describe the same entity but are split across separate structures? In K8s, the unit of declaration is always a self-contained object.
  - Bad: `packages: [kubectl, git]` + `packageVersions: {kubectl: "1.28"}` — the version is an attribute of the package, not a separate map
  - Good: `packages: [{name: kubectl, version: ">=1.28"}, {name: git}]` — each package is a complete unit (like a Pod's `containers[]`)
  - Compare: K8s Deployment has `containers: [{name, image, ports, resources}]`, not `containerNames: [a,b]` + `containerImages: {a: img}` + `containerPorts: {a: [80]}`
- [ ] Are there bare string lists that should be typed objects? A `Vec<String>` with no structure means the item can't grow attributes later without a breaking API change.
  - Bad: `requiredModules: [mod1, mod2]` — can't add version constraints later
  - Good: `requiredModules: [{name: mod1, version: ">=1.0"}]` — extensible from day one

**Spec/status boundary violations:**
- [ ] Is observed/reported state mixed into spec? Spec is desired, status is observed.
  - `packageVersions` (installed versions) in MachineConfigSpec is reported state — belongs in status
  - `hostname` — is this desired or discovered? If discovered at enrollment, it's status
- [ ] Is there anything in status that should be in spec? Status should never contain fields the user sets.

**Selector/reference patterns:**
- [ ] Does `targetSelector` use the K8s label selector pattern (`matchLabels`/`matchExpressions`)? A flat `BTreeMap<String, String>` can't express set-based selectors.
  - Compare: K8s uses `metav1.LabelSelector` with `matchLabels` AND `matchExpressions`
  - Flux Kustomization uses `sourceRef: {kind, name, namespace}` for typed cross-references
- [ ] Are cross-resource references typed? A bare string ref (`machineConfigRef: "my-config"`) can't distinguish between same-namespace and cross-namespace, and can't be validated by the API server.
  - Good: `machineConfigRef: {name: "my-config", namespace: "team-a"}`

**Field granularity:**
- [ ] Are there `BTreeMap<String, String>` fields that should be typed structs? Untyped maps lose schema validation.
  - `systemSettings: BTreeMap<String, String>` — what goes in here? Without schema, anything. Compare with how K8s types every field.
  - `settings: BTreeMap<String, String>` in ConfigPolicy — same problem
- [ ] Are there fields whose purpose overlaps? If two fields can express the same intent, one should be removed.

**Redundant identity fields:**
- [ ] Does the spec contain a `name` field that duplicates `metadata.name`?
  - ConfigPolicySpec has `name: String` — K8s resources get their name from metadata, never from spec
  - Compare: no K8s core resource has `spec.name`

### Condition Design

Compare with K8s API conventions doc and cert-manager's condition patterns:

- [ ] Are condition `type` values positive-polarity? `Ready` is preferred over `NotReady`. `Reconciled` over `ReconcileFailed`.
- [ ] Are condition `reason` values CamelCase machine-readable tokens? `Reconciled`, `DriftDetected`, not `reconciliation succeeded`
- [ ] Is `observedGeneration` on the Condition struct itself (not just on status)? Per KEP-1623, each condition should track which generation it applies to.
- [ ] Do conditions cover the full lifecycle? A resource should have conditions for: readiness, progress, and any error states.
- [ ] DriftAlert status — does it have conditions or just boolean flags? `resolved: bool` is less expressive than conditions.

### Config Resource Design Smells

Check local config documents (`Config`, `Profile`, `Module`) in `crates/cfgd-core/src/config/mod.rs`:

**Provider coupling in schemas:**
- [ ] Are package manager names hardcoded as struct fields (`brew: [...], apt: [...], cargo: [...]`)? This means adding a new package manager requires a schema change.
  - Compare: K8s doesn't have `docker: {...}, containerd: {...}` — it has `containerRuntime: {name, endpoint}`
  - Alternative: `packages: [{manager: brew, names: [bat]}, {manager: apt, names: [bat]}]` or a map `packages: {brew: [...], apt: [...]}`
- [ ] Is the `system` field an untyped `HashMap<String, Value>`? If so, there's no schema validation for what goes inside. This is a trade-off — flexibility vs safety — but worth noting.

**Inheritance and composition:**
- [ ] Is profile inheritance well-defined for every field type? What happens when two profiles both set `env`? Last-wins? Merge? Error?
- [ ] Can every field's merge behavior be explained in one sentence? If not, the semantics are too complex.

### Helm Chart Design

Compare with bitnami common chart patterns, cert-manager chart, ingress-nginx chart:

**values.yaml structure:**
- [ ] Standard top-level keys present: `image`, `replicaCount`, `resources`, `nodeSelector`, `tolerations`, `affinity`, `podAnnotations`, `podLabels`
- [ ] Security contexts separated: `podSecurityContext`, `containerSecurityContext` (not inline in deployment)
- [ ] Service account pattern: `serviceAccount.create`, `serviceAccount.name`, `serviceAccount.annotations`
- [ ] Feature toggles are nested: `webhook.enabled`, `metrics.enabled`, `gateway.enabled` (not flat `enableWebhook`)
- [ ] Probe configuration exposed: `livenessProbe`, `readinessProbe` with override support

**Template quality:**
- [ ] Standard labels (app.kubernetes.io/name, instance, version, component, managed-by)
- [ ] `values.schema.json` for schema validation
- [ ] NOTES.txt post-install instructions
- [ ] Helm test hook

### How to Report

For each smell:
1. **File:line** — where the smell is
2. **What it is** — describe the structural problem
3. **K8s ecosystem comparison** — what does the community do instead? Name a specific project/resource.
4. **Impact** — why this matters (API evolution, user confusion, tooling compatibility)
5. **Severity** — critical (will cause breaking changes later), moderate (confusing but functional), minor (style)
6. **Suggested fix** — concrete struct/field change

### After validation:

Provide a summary: smells found by severity, top 3 highest-impact fixes, and an overall assessment of how "K8s-native" the APIs feel.
