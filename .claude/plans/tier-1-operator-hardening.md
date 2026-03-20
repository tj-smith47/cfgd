# Tier 1: Operator Hardening & CRD Enhancement — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Harden the cfgd-operator for production readiness: leader election, graceful shutdown, health probes, security contexts, restructured CRDs with proper K8s API conventions, new ClusterConfigPolicy CRD, Kubernetes Events, Prometheus metrics, unified Helm chart, RBAC examples, Crossplane E2E tests, and server-side apply annotations.

**Architecture:** The operator (`crates/cfgd-operator/`) gains HA via Lease-based leader election, a dedicated health probe HTTP server, and Prometheus metrics. CRD data models are refactored to follow K8s API conventions (typed references, proper conditions, printer columns). A new ClusterConfigPolicy CRD provides cluster-scoped policy. The two Helm charts (`crates/cfgd-operator/chart/cfgd-operator/` + `charts/cfgd/`) are consolidated into `chart/cfgd/`.

**Tech Stack:** Rust, kube-rs 0.98, k8s-openapi 0.24, tokio, axum, prometheus-client, tracing-opentelemetry, Helm 3

**Spec references:**
- `.claude/specs/2026-03-19-yaml-conventions-and-kubernetes-design.md` (Part 2)
- `.claude/kubernetes-first-class.md` (full design)
- `.claude/PLAN.md` (Tier 1 checklist)

**IMPORTANT:** All code must follow CLAUDE.md conventions. Read `crates/cfgd-operator/src/crds/mod.rs`, `controllers/mod.rs`, `webhook.rs`, `main.rs`, `errors.rs`, and `lib.rs` before modifying. All YAML uses camelCase fields, PascalCase enums. No `unwrap()`/`expect()` in library code. Use `cfgd_core::utc_now_iso8601()` for timestamps. Use `cfgd_core::API_VERSION` for the API version string — never hardcode `"cfgd.io/v1alpha1"` in Rust code (the `#[kube(version = "v1alpha1")]` macro attributes are the only exception since they require string literals).

---

## File Structure

**New files:**
- `crates/cfgd-operator/src/health.rs` — Dedicated HTTP health probe server (port 8081)
- `crates/cfgd-operator/src/leader.rs` — Lease-based leader election
- `crates/cfgd-operator/src/metrics.rs` — Prometheus metrics registry + HTTP endpoint
- `chart/cfgd/` — Unified Helm chart (replaces both `crates/cfgd-operator/chart/cfgd-operator/` and `charts/cfgd/`)

**Modified files:**
- `crates/cfgd-operator/Cargo.toml` — Add `prometheus-client`, `tracing-opentelemetry`, `opentelemetry`, `opentelemetry-otlp`
- `crates/cfgd-operator/src/lib.rs` — Declare new modules
- `crates/cfgd-operator/src/crds/mod.rs` — CRD type refactoring, new CRDs, printer columns, conditions
- `crates/cfgd-operator/src/controllers/mod.rs` — New controllers, events, conditions, finalizers
- `crates/cfgd-operator/src/webhook.rs` — New validation endpoints
- `crates/cfgd-operator/src/errors.rs` — New error variants
- `crates/cfgd-operator/src/main.rs` — Leader election, graceful shutdown, health/metrics startup
- `crates/cfgd-operator/src/gen_crds.rs` — Add ClusterConfigPolicy to CRD generation
- `crates/cfgd-operator/src/gateway/api.rs` — Update for new CRD types if referenced

---

### Task 1: Condition Struct Enhancement — Add observedGeneration

**Files:**
- Modify: `crates/cfgd-operator/src/crds/mod.rs:72-81`

Per KEP-1623, every Condition should carry `observedGeneration` so consumers know which generation the condition reflects.

- [x] **Step 1: Write the failing test**

Add to the `tests` module in `crds/mod.rs`:

```rust
#[test]
fn condition_has_observed_generation() {
    let c = Condition {
        condition_type: "Ready".to_string(),
        status: "True".to_string(),
        reason: "Test".to_string(),
        message: "test".to_string(),
        last_transition_time: "2024-01-01T00:00:00Z".to_string(),
        observed_generation: Some(1),
    };
    assert_eq!(c.observed_generation, Some(1));
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p cfgd-operator condition_has_observed_generation`
Expected: FAIL — `observed_generation` field doesn't exist on Condition

- [x] **Step 3: Add observedGeneration to Condition struct**

In `crds/mod.rs`, add the field to `Condition`:

```rust
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Condition {
    #[serde(rename = "type")]
    pub condition_type: String,
    pub status: String,
    pub reason: String,
    pub message: String,
    pub last_transition_time: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_generation: Option<i64>,
}
```

- [x] **Step 4: Update all Condition construction sites**

In `controllers/mod.rs`, update every `Condition { ... }` to include `observed_generation: current_generation` (the value from `obj.meta().generation`). There are 3 sites:
1. `reconcile_machine_config` (~line 151) — set to `current_generation`
2. `reconcile_drift_alert` (~line 249) — set to the MachineConfig's generation (from `mc.meta().generation`)
3. `reconcile_config_policy` (~line 427) — set to `obj.meta().generation`

- [x] **Step 5: Run all tests**

Run: `cargo test -p cfgd-operator`
Expected: All pass including new test

- [x] **Step 6: Commit**

```bash
git add crates/cfgd-operator/src/crds/mod.rs crates/cfgd-operator/src/controllers/mod.rs
git commit -m "feat(operator): add observedGeneration to Condition struct per KEP-1623"
```

---

### Task 2: CRD API Design Fixes — Type Refactoring

**Files:**
- Modify: `crates/cfgd-operator/src/crds/mod.rs`
- Modify: `crates/cfgd-operator/src/controllers/mod.rs`
- Modify: `crates/cfgd-operator/src/webhook.rs`

This task implements all CRD API design fixes from PLAN.md. These changes cascade through controllers/webhook/tests so they must be done atomically.

**Changes:**
1. Replace `packages: Vec<String>` with `packages: Vec<PackageRef>` in both MachineConfigSpec and ConfigPolicySpec
2. Move `package_versions: BTreeMap` from MachineConfigSpec to MachineConfigStatus (spec is desired, status is observed)
3. Remove `ConfigPolicySpec.name` (duplicates `metadata.name`)
4. Replace `target_selector: BTreeMap<String, String>` with typed `LabelSelector { match_labels, match_expressions }`
5. Replace `machine_config_ref: String` in DriftAlertSpec with `MachineConfigReference { name, namespace }`
6. Replace `required_modules: Vec<String>` in ConfigPolicySpec with `Vec<ModuleRef>`
7. Remove `drift_detected: bool` from MachineConfigStatus (expressed via condition instead)
8. Replace `system_settings: BTreeMap<String, String>` with `BTreeMap<String, serde_json::Value>` for richer settings

- [x] **Step 1: Write tests for new types**

Add to `crds/mod.rs` tests:

```rust
#[test]
fn package_ref_serialization() {
    let pr = PackageRef { name: "vim".to_string(), version: Some("1.0.0".to_string()) };
    let json = serde_json::to_value(&pr).unwrap();
    assert_eq!(json["name"], "vim");
    assert_eq!(json["version"], "1.0.0");
}

#[test]
fn label_selector_empty_matches_all() {
    let sel = LabelSelector::default();
    assert!(sel.match_labels.is_empty());
    assert!(sel.match_expressions.is_empty());
}

#[test]
fn machine_config_reference_serialization() {
    let r = MachineConfigReference { name: "mc-1".to_string(), namespace: Some("team-a".to_string()) };
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["name"], "mc-1");
    assert_eq!(json["namespace"], "team-a");
}

#[test]
fn mc_status_has_package_versions() {
    let status = MachineConfigStatus {
        last_reconciled: None,
        observed_generation: None,
        package_versions: {
            let mut m = BTreeMap::new();
            m.insert("kubectl".to_string(), "1.28.3".to_string());
            m
        },
        conditions: vec![],
    };
    assert_eq!(status.package_versions["kubectl"], "1.28.3");
}

#[test]
fn cp_validate_no_spec_name() {
    // ConfigPolicySpec no longer has a `name` field
    let spec = ConfigPolicySpec {
        required_modules: vec![],
        packages: vec![],
        package_versions: Default::default(),
        settings: Default::default(),
        target_selector: Default::default(),
    };
    assert!(spec.validate().is_ok());
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd-operator`
Expected: Compilation errors — types don't exist yet

- [x] **Step 3: Define new types and refactor CRD structs**

In `crds/mod.rs`, add new types and refactor existing structs:

```rust
/// Typed package reference (replaces bare string lists).
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackageRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Kubernetes-style label selector with matchLabels and matchExpressions.
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelector {
    #[serde(default)]
    pub match_labels: BTreeMap<String, String>,
    #[serde(default)]
    pub match_expressions: Vec<LabelSelectorRequirement>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabelSelectorRequirement {
    pub key: String,
    pub operator: String,
    #[serde(default)]
    pub values: Vec<String>,
}

/// Typed reference to a MachineConfig resource.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MachineConfigReference {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}
```

Refactor `MachineConfigSpec`:
```rust
pub struct MachineConfigSpec {
    pub hostname: String,
    pub profile: String,
    #[serde(default)]
    pub module_refs: Vec<ModuleRef>,
    #[serde(default)]
    pub packages: Vec<PackageRef>,          // was Vec<String>
    #[serde(default)]
    pub files: Vec<FileSpec>,
    #[serde(default)]
    pub system_settings: BTreeMap<String, serde_json::Value>,  // was BTreeMap<String, String>
}
// NOTE: package_versions REMOVED from spec — moves to status
```

Refactor `MachineConfigStatus`:
```rust
pub struct MachineConfigStatus {
    pub last_reconciled: Option<String>,
    // drift_detected: bool REMOVED — expressed via DriftDetected condition
    #[serde(default)]
    pub observed_generation: Option<i64>,
    #[serde(default)]
    pub package_versions: BTreeMap<String, String>,  // MOVED from spec — observed state
    #[serde(default)]
    pub conditions: Vec<Condition>,
}
```

Refactor `ConfigPolicySpec`:
```rust
pub struct ConfigPolicySpec {
    // name: String REMOVED — duplicates metadata.name
    #[serde(default)]
    pub required_modules: Vec<ModuleRef>,    // was Vec<String>
    #[serde(default)]
    pub packages: Vec<PackageRef>,           // was Vec<String>
    #[serde(default)]
    pub package_versions: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    #[serde(default)]
    pub target_selector: LabelSelector,      // was BTreeMap<String, String>
}
```

Refactor `DriftAlertSpec`:
```rust
pub struct DriftAlertSpec {
    pub device_id: String,
    pub machine_config_ref: MachineConfigReference,  // was String
    #[serde(default)]
    pub drift_details: Vec<DriftDetail>,
    pub severity: DriftSeverity,
}
```

- [x] **Step 4: Update validation methods**

Update `MachineConfigSpec::validate()`:
- Remove `package_versions` validation (moved to status)
- Validate `packages[i].name` not empty instead of the string itself

Update `ConfigPolicySpec::validate()`:
- Remove `self.name.is_empty()` check
- Update package validation to use `PackageRef.name`
- Update `required_modules` validation to use `ModuleRef.name`

- [x] **Step 5: Update controllers for new types**

In `controllers/mod.rs`:
- `reconcile_machine_config`: Remove `drift_detected` from status JSON, remove `package_versions` references in spec
- `reconcile_drift_alert`: Use `obj.spec.machine_config_ref.name` instead of `obj.spec.machine_config_ref` (String), use namespace from `machine_config_ref.namespace` when available
- `reconcile_config_policy`: Update `matches_selector` to use `LabelSelector`, update `validate_policy_compliance` to accept status
- `matches_selector`: Rewrite to match on `LabelSelector.match_labels` (check against MachineConfig's labels, not spec fields) and `match_expressions`
- `validate_policy_compliance`: Change signature to also accept `&MachineConfigStatus` (or the full `&MachineConfig`) since `package_versions` moved from spec to status. Read `status.package_versions` instead of `spec.package_versions`. Update the call site in `reconcile_config_policy` to pass the MachineConfig's status.
- **Settings type mismatch**: Since `spec.system_settings` is now `BTreeMap<String, serde_json::Value>` but `ConfigPolicySpec.settings` remains `BTreeMap<String, String>`, the settings comparison in `validate_policy_compliance` must convert types. Update the comparison to: `serde_json::Value::String(value.clone()) == *v` (or equivalently, match on `serde_json::Value::String(s) if s == value`). This applies to the `for (key, value) in settings` loop.

- [x] **Step 6: Update webhook for new types**

No structural changes needed in `webhook.rs` — it delegates to `spec.validate()` which is already updated.

- [x] **Step 7: Update all tests**

Fix all existing tests in `crds/mod.rs` and `controllers/mod.rs`:
- `minimal_mc_spec` / `mc_spec` helpers: remove `package_versions`, update `packages` to `Vec<PackageRef>`, `system_settings` to `BTreeMap<String, Value>`
- `cp_validate_*` tests: remove `name` field, use `PackageRef` and `ModuleRef`
- `matches_selector_*` tests: use `LabelSelector` instead of BTreeMap (note: selector now matches against resource labels, not spec fields — update logic accordingly)
- `policy_compliance_*` tests: update to use `PackageRef` and `ModuleRef`
- `module_compliance_*` tests: already using `ModuleRef` from `crate::crds::ModuleRef`

- [x] **Step 8: Run all tests**

Run: `cargo test -p cfgd-operator`
Expected: All pass

- [x] **Step 9: Run clippy**

Run: `cargo clippy -p cfgd-operator -- -D warnings`
Expected: Clean

- [x] **Step 10: Commit**

```bash
git add crates/cfgd-operator/src/crds/mod.rs crates/cfgd-operator/src/controllers/mod.rs crates/cfgd-operator/src/webhook.rs
git commit -m "feat(operator): refactor CRD API types — PackageRef, LabelSelector, typed refs, remove spec.name"
```

---

### Task 3: MachineConfig Conditions Split + DriftAlert Conditions

**Depends on:** Task 1 (observedGeneration on Condition)

**Files:**
- Modify: `crates/cfgd-operator/src/crds/mod.rs`
- Modify: `crates/cfgd-operator/src/controllers/mod.rs`

Split the single `Ready` condition on MachineConfig into 4 conditions: `Reconciled`, `DriftDetected`, `ModulesResolved`, `Compliant`. Add `conditions: Vec<Condition>` to DriftAlertStatus with `Acknowledged`, `Resolved`, `Escalated`. Add `Resolved` printer column support to DriftAlertStatus.

- [x] **Step 1: Add DriftAlert conditions to status struct**

In `crds/mod.rs`, update `DriftAlertStatus` to include `conditions: Vec<Condition>` (it currently has no conditions field):
```rust
#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DriftAlertStatus {
    pub detected_at: Option<String>,
    pub resolved_at: Option<String>,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}
```

- [x] **Step 2: Write tests for conditions**

```rust
#[test]
fn mc_reconciled_condition_set_on_success() {
    let status = MachineConfigStatus {
        last_reconciled: Some("2024-01-01T00:00:00Z".to_string()),
        observed_generation: Some(1),
        package_versions: Default::default(),
        conditions: vec![
            Condition {
                condition_type: "Reconciled".to_string(),
                status: "True".to_string(),
                reason: "ReconcileSuccess".to_string(),
                message: "Reconciled successfully".to_string(),
                last_transition_time: "2024-01-01T00:00:00Z".to_string(),
                observed_generation: Some(1),
            },
            Condition {
                condition_type: "DriftDetected".to_string(),
                status: "False".to_string(),
                reason: "NoDrift".to_string(),
                message: "No drift detected".to_string(),
                last_transition_time: "2024-01-01T00:00:00Z".to_string(),
                observed_generation: Some(1),
            },
        ],
    };
    assert_eq!(status.conditions.len(), 2);
    assert_eq!(status.conditions[0].condition_type, "Reconciled");
}
```

- [x] **Step 3: Update MachineConfig controller to emit 4 conditions**

In `reconcile_machine_config`, replace the single `Ready` condition with:
- `Reconciled`: True/False, reasons `ReconcileSuccess`/`ReconcileError`
- `DriftDetected`: True/False, reasons `DriftActive`/`NoDrift`
- `ModulesResolved`: True (all module refs exist — for now always True; later will check Module CRDs)
- `Compliant`: True (policy compliance — for now always True; ConfigPolicy controller sets this separately)

- [x] **Step 4: Update DriftAlert controller to set conditions**

In `reconcile_drift_alert`, after patching MachineConfig status, also patch DriftAlert status with conditions:
- `Resolved`: False initially, True when drift is resolved

- [x] **Step 5: Update DriftAlert controller to use Reconciled/DriftDetected instead of Ready**

When the DriftAlert controller patches MachineConfig status to mark drift, it should set:
- `DriftDetected` condition to True (reason: `DriftActive`)
- NOT set `Ready` (that condition is gone)

- [x] **Step 6: Run all tests**

Run: `cargo test -p cfgd-operator`
Expected: All pass

- [x] **Step 7: Commit**

```bash
git add crates/cfgd-operator/src/crds/mod.rs crates/cfgd-operator/src/controllers/mod.rs
git commit -m "feat(operator): split MachineConfig Ready into 4 conditions, add DriftAlert conditions"
```

---

### Task 4: CRD Metadata — Printer Columns, Short Names, Categories, CEL Validation

**Depends on:** Task 3 (conditions must exist before printer columns reference them)

**Files:**
- Modify: `crates/cfgd-operator/src/crds/mod.rs`
- Modify: `crates/cfgd-operator/src/gen_crds.rs` (for CEL post-processing)

Add `#[kube()]` attributes for printer columns, short names, and categories to all 3 CRDs. Add CEL validation rules to MachineConfig CRD.

- [x] **Step 1: Write test for CRD metadata**

```rust
#[test]
fn machineconfig_crd_has_printer_columns() {
    use kube::CustomResourceExt;
    let crd = MachineConfig::crd();
    let version = &crd.spec.versions[0];
    let columns = version.additional_printer_columns.as_ref().unwrap();
    let col_names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
    assert!(col_names.contains(&"Hostname"));
    assert!(col_names.contains(&"Profile"));
    assert!(col_names.contains(&"Age"));
}

#[test]
fn machineconfig_crd_has_short_names() {
    use kube::CustomResourceExt;
    let crd = MachineConfig::crd();
    let short_names = &crd.spec.names.short_names;
    assert!(short_names.as_ref().unwrap().contains(&"mc".to_string()));
}

#[test]
fn configpolicy_crd_has_short_names() {
    use kube::CustomResourceExt;
    let crd = ConfigPolicy::crd();
    let short_names = &crd.spec.names.short_names;
    assert!(short_names.as_ref().unwrap().contains(&"cpol".to_string()));
}

#[test]
fn driftalert_crd_has_short_names() {
    use kube::CustomResourceExt;
    let crd = DriftAlert::crd();
    let short_names = &crd.spec.names.short_names;
    assert!(short_names.as_ref().unwrap().contains(&"da".to_string()));
}
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd-operator crd_has`
Expected: FAIL

- [x] **Step 3: Add kube attributes**

On MachineConfig:
```rust
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "MachineConfig",
    namespaced,
    status = "MachineConfigStatus",
    shortname = "mc",
    category = "cfgd",
    printcolumn = r#"{"name": "Hostname", "type": "string", "jsonPath": ".spec.hostname"}"#,
    printcolumn = r#"{"name": "Profile", "type": "string", "jsonPath": ".spec.profile"}"#,
    printcolumn = r#"{"name": "Reconciled", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"Reconciled\")].status"}"#,
    printcolumn = r#"{"name": "Drift", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"DriftDetected\")].status"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
```

On ConfigPolicy:
```rust
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "ConfigPolicy",
    namespaced,
    status = "ConfigPolicyStatus",
    shortname = "cpol",
    category = "cfgd",
    printcolumn = r#"{"name": "Compliant", "type": "integer", "jsonPath": ".status.compliantCount"}"#,
    printcolumn = r#"{"name": "NonCompliant", "type": "integer", "jsonPath": ".status.nonCompliantCount"}"#,
    printcolumn = r#"{"name": "Enforced", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"Enforced\")].status"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
```

On DriftAlert:
```rust
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "DriftAlert",
    namespaced,
    status = "DriftAlertStatus",
    shortname = "da",
    category = "cfgd",
    printcolumn = r#"{"name": "Device", "type": "string", "jsonPath": ".spec.deviceId"}"#,
    printcolumn = r#"{"name": "Severity", "type": "string", "jsonPath": ".spec.severity"}"#,
    printcolumn = r#"{"name": "Resolved", "type": "string", "jsonPath": ".status.conditions[?(@.type==\"Resolved\")].status"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
```

- [x] **Step 4: Run tests**

Run: `cargo test -p cfgd-operator`
Expected: All pass

- [x] **Step 5: Regenerate CRD YAML and update Helm chart CRD templates**

Run: `cargo run --bin cfgd-gen-crds > /tmp/crds.yaml`
Split the output into the 3 CRD files under the Helm chart's `templates/crds/` directory.

- [x] **Step 6: Add CEL validation rules to MachineConfig**

CEL validation must be added to the generated CRD YAML since kube-rs derive macros don't natively support `x-kubernetes-validations`. Update `gen_crds.rs` to post-process the MachineConfig CRD JSON:

```rust
// After generating the CRD, inject CEL rules into the OpenAPI schema
fn inject_cel_rules(crd: &mut serde_json::Value) {
    if let Some(schema) = crd.pointer_mut("/spec/versions/0/schema/openAPIV3Schema/properties/spec") {
        schema["x-kubernetes-validations"] = serde_json::json!([
            {
                "rule": "self.hostname.size() > 0",
                "message": "hostname must not be empty"
            }
        ]);
        // Add file-level validation
        if let Some(files) = schema.pointer_mut("/properties/files/items") {
            files["x-kubernetes-validations"] = serde_json::json!([
                {
                    "rule": "has(self.content) || has(self.source)",
                    "message": "each file must have content or source"
                }
            ]);
        }
    }
}
```

- [x] **Step 7: Regenerate CRD YAML and update Helm chart CRD templates**

Run: `cargo run --bin cfgd-gen-crds > /tmp/crds.yaml`
Split the output into the CRD files under the Helm chart's `templates/crds/` directory.

- [x] **Step 8: Run tests**

Run: `cargo test -p cfgd-operator`
Expected: All pass

- [x] **Step 9: Commit**

```bash
git add crates/cfgd-operator/src/crds/mod.rs crates/cfgd-operator/src/gen_crds.rs
git commit -m "feat(operator): add printer columns, short names, categories, CEL validation to CRDs"
```

---

### Task 5: MachineConfig Finalizer + Owner References

**Files:**
- Modify: `crates/cfgd-operator/src/controllers/mod.rs`

Add finalizer `cfgd.io/machine-config-cleanup` to MachineConfig. On deletion: signal device, remove finalizer. Set owner references on DriftAlerts pointing to their parent MachineConfig for cascading deletion.

- [x] **Step 1: Write finalizer handling test**

```rust
#[test]
fn finalizer_name_constant() {
    assert_eq!(MACHINE_CONFIG_FINALIZER, "cfgd.io/machine-config-cleanup");
}
```

- [x] **Step 2: Add finalizer constant and handling**

```rust
const MACHINE_CONFIG_FINALIZER: &str = "cfgd.io/machine-config-cleanup";
```

In `reconcile_machine_config`, add finalizer logic at the top:
1. If object has `deletion_timestamp` and our finalizer is present:
   - Log that cleanup is happening
   - Remove the finalizer via JSON patch
   - Return (don't continue normal reconciliation)
2. If object does NOT have our finalizer and no `deletion_timestamp`:
   - Add the finalizer via JSON merge patch

Use `kube::api::Patch::Json` for finalizer add/remove to avoid conflicts with SSA.

- [x] **Step 3: Set owner references on DriftAlerts**

In `reconcile_drift_alert`, when a DriftAlert references a MachineConfig, set an `ownerReference` on the DriftAlert pointing to the MachineConfig. This enables cascading deletion (deleting a MachineConfig automatically deletes its DriftAlerts).

```rust
use kube::api::ObjectMeta;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;

// After looking up the MachineConfig:
// Use the shared constant — do NOT hardcode the API version string
let owner_ref = OwnerReference {
    api_version: cfgd_core::API_VERSION.to_string(),
    kind: "MachineConfig".to_string(),
    name: mc.name_any(),
    uid: mc.metadata.uid.clone().unwrap_or_default(),
    controller: Some(true),
    block_owner_deletion: Some(true),
};

// Check if owner ref already set; if not, patch the DriftAlert metadata
let existing_owners = obj.metadata.owner_references.as_deref().unwrap_or(&[]);
if !existing_owners.iter().any(|r| r.uid == owner_ref.uid) {
    let patch = serde_json::json!({
        "metadata": {
            "ownerReferences": [owner_ref]
        }
    });
    alerts.patch(&name, &PatchParams::apply(FIELD_MANAGER_OPERATOR), &Patch::Merge(patch))
        .await
        .map_err(|e| OperatorError::Reconciliation(format!("set owner ref: {e}")))?;
}
```

- [x] **Step 4: Run tests**

Run: `cargo test -p cfgd-operator`
Expected: All pass

- [x] **Step 5: Commit**

```bash
git add crates/cfgd-operator/src/controllers/mod.rs
git commit -m "feat(operator): add MachineConfig finalizer and DriftAlert owner references"
```

---

### Task 6: ClusterConfigPolicy CRD + Controller + Webhook

**Files:**
- Modify: `crates/cfgd-operator/src/crds/mod.rs`
- Modify: `crates/cfgd-operator/src/controllers/mod.rs`
- Modify: `crates/cfgd-operator/src/webhook.rs`
- Modify: `crates/cfgd-operator/src/errors.rs`
- Modify: `crates/cfgd-operator/src/gen_crds.rs`
- Modify: `crates/cfgd-operator/src/lib.rs` (re-export)

New cluster-scoped CRD with `namespaceSelector`, security settings, and a controller that evaluates MachineConfigs across namespaces.

- [x] **Step 1: Write CRD struct tests**

```rust
#[test]
fn cluster_config_policy_is_cluster_scoped() {
    use kube::CustomResourceExt;
    let crd = ClusterConfigPolicy::crd();
    assert_eq!(crd.spec.scope, "Cluster");
}

#[test]
fn cluster_config_policy_has_short_name() {
    use kube::CustomResourceExt;
    let crd = ClusterConfigPolicy::crd();
    let short_names = crd.spec.names.short_names.as_ref().unwrap();
    assert!(short_names.contains(&"ccpol".to_string()));
}

#[test]
fn ccp_validate_accepts_minimal() {
    let spec = ClusterConfigPolicySpec {
        namespace_selector: Default::default(),
        required_modules: vec![],
        packages: vec![],
        package_versions: Default::default(),
        settings: Default::default(),
        security: Default::default(),
    };
    assert!(spec.validate().is_ok());
}
```

- [x] **Step 2: Define ClusterConfigPolicy CRD**

```rust
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "cfgd.io",
    version = "v1alpha1",
    kind = "ClusterConfigPolicy",
    status = "ClusterConfigPolicyStatus",
    shortname = "ccpol",
    category = "cfgd",
    printcolumn = r#"{"name": "Compliant", "type": "integer", "jsonPath": ".status.compliantCount"}"#,
    printcolumn = r#"{"name": "NonCompliant", "type": "integer", "jsonPath": ".status.nonCompliantCount"}"#,
    printcolumn = r#"{"name": "Age", "type": "date", "jsonPath": ".metadata.creationTimestamp"}"#
)]
#[serde(rename_all = "camelCase")]
pub struct ClusterConfigPolicySpec {
    #[serde(default)]
    pub namespace_selector: LabelSelector,
    #[serde(default)]
    pub required_modules: Vec<ModuleRef>,
    #[serde(default)]
    pub packages: Vec<PackageRef>,
    #[serde(default)]
    pub package_versions: BTreeMap<String, String>,
    #[serde(default)]
    pub settings: BTreeMap<String, String>,
    #[serde(default)]
    pub security: SecurityPolicy,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SecurityPolicy {
    #[serde(default)]
    pub trusted_registries: Vec<String>,
    #[serde(default)]
    pub allow_unsigned: bool,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClusterConfigPolicyStatus {
    pub compliant_count: u32,
    pub non_compliant_count: u32,
    #[serde(default)]
    pub conditions: Vec<Condition>,
}
```

Add validation:
```rust
impl ClusterConfigPolicySpec {
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();
        for (pkg, req_str) in &self.package_versions {
            if pkg.is_empty() {
                errors.push("spec.packageVersions key must not be empty".to_string());
            }
            if VersionReq::parse(req_str).is_err() {
                errors.push(format!(
                    "spec.packageVersions['{pkg}'] = '{req_str}' is not a valid semver requirement"
                ));
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(errors) }
    }
}
```

- [x] **Step 3: Implement ClusterConfigPolicy merge semantics**

Add a function `merge_cluster_and_namespace_policies` that implements the merge rules from the spec:
1. `packages` and `requiredModules`: union of both policies (both applied)
2. `settings`: ClusterConfigPolicy overrides ConfigPolicy (cluster wins)
3. `packageVersions`: ClusterConfigPolicy overrides (cluster wins)
4. `trustedRegistries`: ClusterConfigPolicy is canonical (ConfigPolicy cannot expand)

```rust
fn merge_policy_requirements(
    cluster: &ClusterConfigPolicySpec,
    namespace: Option<&ConfigPolicySpec>,
) -> MergedPolicyRequirements {
    let mut packages: Vec<PackageRef> = cluster.packages.clone();
    let mut modules: Vec<ModuleRef> = cluster.required_modules.clone();
    let mut settings = cluster.settings.clone();
    let mut versions = cluster.package_versions.clone();

    if let Some(ns) = namespace {
        // Union: add namespace packages/modules not already present
        for pkg in &ns.packages {
            if !packages.iter().any(|p| p.name == pkg.name) {
                packages.push(pkg.clone());
            }
        }
        for m in &ns.required_modules {
            if !modules.iter().any(|mr| mr.name == m.name) {
                modules.push(m.clone());
            }
        }
        // Cluster wins on settings/versions — only add keys not in cluster
        for (k, v) in &ns.settings {
            settings.entry(k.clone()).or_insert_with(|| v.clone());
        }
        for (k, v) in &ns.package_versions {
            versions.entry(k.clone()).or_insert_with(|| v.clone());
        }
    }

    MergedPolicyRequirements { packages, modules, settings, versions }
}
```

- [x] **Step 4: Add ClusterConfigPolicy controller**

In `controllers/mod.rs`, add `reconcile_cluster_config_policy`:
1. List all namespaces (filter by `namespace_selector.match_labels` if set)
2. For each namespace, list MachineConfigs AND namespace-scoped ConfigPolicies
3. For each MachineConfig, merge cluster + namespace policy requirements using `merge_policy_requirements`
4. Evaluate compliance against merged requirements
5. Update ClusterConfigPolicy status with aggregate counts
6. Set `Enforced` condition
7. Requeue after 60s

Add to `run()`: start a 4th controller watching `ClusterConfigPolicy` with `Api::all()`.

- [x] **Step 4: Add webhook endpoint**

In `webhook.rs`, add handler `handle_validate_cluster_config_policy` and route `/validate-clusterconfigpolicy`. Implement `Validatable` for `ClusterConfigPolicySpec`.

- [x] **Step 5: Update gen_crds.rs**

Add ClusterConfigPolicy to CRD generation output.

- [x] **Step 6: Run tests**

Run: `cargo test -p cfgd-operator`
Run: `cargo clippy -p cfgd-operator -- -D warnings`
Expected: All pass

- [x] **Step 7: Commit**

```bash
git add crates/cfgd-operator/src/crds/mod.rs crates/cfgd-operator/src/controllers/mod.rs \
  crates/cfgd-operator/src/webhook.rs crates/cfgd-operator/src/gen_crds.rs crates/cfgd-operator/src/errors.rs
git commit -m "feat(operator): add ClusterConfigPolicy CRD, controller, and webhook"
```

---

### Task 7: Kubernetes Events

**Files:**
- Modify: `crates/cfgd-operator/src/controllers/mod.rs`

Emit Kubernetes Events from all controllers using `kube::runtime::events::Recorder`.

- [x] **Step 1: Add event recorder to ControllerContext and update run() signature**

```rust
use kube::runtime::events::{Recorder, Reporter, Event, EventType};

pub struct ControllerContext {
    pub client: Client,
    pub recorder: Recorder,
}
```

Update `run()` to construct the recorder internally:
```rust
pub async fn run(client: Client) -> Result<(), OperatorError> {
    let reporter = Reporter {
        controller: "cfgd-operator".into(),
        instance: std::env::var("POD_NAME").ok(),
    };
    let recorder = Recorder::new(client.clone(), reporter);

    let ctx = Arc::new(ControllerContext {
        client: client.clone(),
        recorder,
    });
    // ... rest unchanged
}
```

- [x] **Step 2: Emit events from MachineConfig controller**

After successful reconciliation (note: `&Event`, not `Event` — kube-rs takes by reference):
```rust
ctx.recorder.publish(
    &Event {
        type_: EventType::Normal,
        reason: "Reconciled".into(),
        note: Some(format!("MachineConfig {} reconciled successfully", name)),
        action: "Reconcile".into(),
        secondary: None,
    },
    &obj.object_ref(&()),
).await.ok();
```

On drift detection:
```rust
ctx.recorder.publish(
    &Event {
        type_: EventType::Warning,
        reason: "DriftDetected".into(),
        note: Some(format!("Drift detected on device for MachineConfig {}", name)),
        action: "DriftCheck".into(),
        secondary: None,
    },
    &obj.object_ref(&()),
).await.ok();
```

- [x] **Step 3: Emit events from ConfigPolicy controller**

Always emit `Evaluated` event on every policy evaluation. Additionally emit `NonCompliantTargets` when non-compliant targets exist:
```rust
// Always emit Evaluated
ctx.recorder.publish(
    &Event {
        type_: EventType::Normal,
        reason: "Evaluated".into(),
        note: Some(format!("{} compliant, {} non-compliant", compliant_count, non_compliant_count)),
        action: "Evaluate".into(),
        secondary: None,
    },
    &obj.object_ref(&()),
).await.ok();

if non_compliant_count > 0 {
    ctx.recorder.publish(
        &Event {
            type_: EventType::Warning,
            reason: "NonCompliantTargets".into(),
            note: Some(format!("{} non-compliant MachineConfigs", non_compliant_count)),
            action: "Evaluate".into(),
            secondary: None,
        },
        &obj.object_ref(&()),
    ).await.ok();
}
```

- [x] **Step 4: Emit events from DriftAlert and ClusterConfigPolicy controllers**

Same pattern:
- **DriftAlert controller**: emit `DriftDetected` (Warning) when marking a MachineConfig as drifted, `DriftResolved` (Normal) when deleting a resolved alert
- **ClusterConfigPolicy controller**: emit `Evaluated` (Normal) on every evaluation, `NonCompliantTargets` (Warning) when non-compliant targets exist
- **MachineConfig controller**: emit `PolicyViolation` (Warning) when the `Compliant` condition is set to False (i.e., when a ConfigPolicy finds this MachineConfig non-compliant). This can be emitted from the ConfigPolicy controller targeting the non-compliant MachineConfig's object ref.

- [x] **Step 5: Update RBAC for events.k8s.io API group**

kube-rs 0.98's `Recorder` uses the `events.k8s.io` API group (Events v1 API). The existing RBAC template only grants `apiGroups: [""]` for events. Update the RBAC (in the Helm chart template or the operator's ClusterRole) to include both API groups for backward compatibility:

```yaml
- apiGroups: ["", "events.k8s.io"]
  resources: ["events"]
  verbs: ["create", "patch"]
```

- [x] **Step 6: Run tests**

Run: `cargo test -p cfgd-operator`
Expected: All pass (event publishing returns `Result` which we `.ok()` to ignore in tests without a cluster)

- [x] **Step 7: Commit**

```bash
git add crates/cfgd-operator/src/controllers/mod.rs
git commit -m "feat(operator): emit Kubernetes Events from all controllers"
```

---

### Task 8: Health Probes — Dedicated HTTP Server

**Files:**
- Create: `crates/cfgd-operator/src/health.rs`
- Modify: `crates/cfgd-operator/src/lib.rs`
- Modify: `crates/cfgd-operator/src/main.rs`

Dedicated HTTP server on port 8081 for `/healthz` (liveness) and `/readyz` (readiness). `/readyz` returns 503 until leader lease is acquired.

- [x] **Step 1: Write health module tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn healthz_returns_ok() {
        let state = HealthState::new();
        let resp = healthz_handler(axum::extract::State(state)).await;
        assert_eq!(resp.0, axum::http::StatusCode::OK);
    }

    #[tokio::test]
    async fn readyz_returns_503_when_not_ready() {
        let state = HealthState::new();
        let resp = readyz_handler(axum::extract::State(state)).await;
        assert_eq!(resp.0, axum::http::StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn readyz_returns_200_when_ready() {
        let state = HealthState::new();
        state.set_ready();
        let resp = readyz_handler(axum::extract::State(state)).await;
        assert_eq!(resp.0, axum::http::StatusCode::OK);
    }
}
```

- [x] **Step 2: Implement health module**

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone)]
pub struct HealthState(Arc<AtomicBool>);

impl HealthState {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn set_ready(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

async fn healthz_handler(
    axum::extract::State(_state): axum::extract::State<HealthState>,
) -> (axum::http::StatusCode, &'static str) {
    (axum::http::StatusCode::OK, "ok")
}

async fn readyz_handler(
    axum::extract::State(state): axum::extract::State<HealthState>,
) -> (axum::http::StatusCode, &'static str) {
    if state.0.load(Ordering::SeqCst) {
        (axum::http::StatusCode::OK, "ready")
    } else {
        (axum::http::StatusCode::SERVICE_UNAVAILABLE, "not ready")
    }
}

pub async fn run_health_server(port: u16, state: HealthState) -> Result<(), crate::errors::OperatorError> {
    let app = axum::Router::new()
        .route("/healthz", axum::routing::get(healthz_handler))
        .route("/readyz", axum::routing::get(readyz_handler))
        .with_state(state);

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| crate::errors::OperatorError::Health(format!("bind {addr}: {e}")))?;

    tracing::info!(%addr, "Health probe server listening");
    axum::serve(listener, app)
        .await
        .map_err(|e| crate::errors::OperatorError::Health(format!("serve: {e}")))?;
    Ok(())
}
```

- [x] **Step 3: Add `Health` variant to OperatorError**

In `errors.rs`:
```rust
#[error("Health server error: {0}")]
Health(String),
```

- [x] **Step 4: Wire into main.rs**

Start health server before controllers:
```rust
let health_state = health::HealthState::new();
let health_port: u16 = std::env::var("HEALTH_PORT")
    .unwrap_or_else(|_| "8081".to_string())
    .parse()
    .unwrap_or(8081);

tokio::spawn({
    let hs = health_state.clone();
    async move {
        if let Err(e) = health::run_health_server(health_port, hs).await {
            tracing::error!(error = %e, "Health server failed");
        }
    }
});
```

After leader election (or immediately if no HA): `health_state.set_ready();`

- [x] **Step 5: Add `pub mod health;` to lib.rs**

- [x] **Step 6: Run tests**

Run: `cargo test -p cfgd-operator`
Expected: All pass

- [x] **Step 7: Commit**

```bash
git add crates/cfgd-operator/src/health.rs crates/cfgd-operator/src/lib.rs \
  crates/cfgd-operator/src/main.rs crates/cfgd-operator/src/errors.rs
git commit -m "feat(operator): add dedicated health probe server on port 8081"
```

---

### Task 9: Leader Election

**Files:**
- Create: `crates/cfgd-operator/src/leader.rs`
- Modify: `crates/cfgd-operator/src/lib.rs`
- Modify: `crates/cfgd-operator/src/main.rs`

Lease-based leader election via `coordination.k8s.io/v1` Lease. The operator acquires the lease before starting controllers.

- [x] **Step 1: Write leader election module**

```rust
use std::time::Duration;

use k8s_openapi::api::coordination::v1::Lease;
use kube::api::{Api, Patch, PatchParams, PostParams};
use kube::Client;

use crate::errors::OperatorError;

const LEASE_NAME: &str = "cfgd-operator-leader";
const LEASE_DURATION_SECS: i32 = 15;
const RENEW_DEADLINE_SECS: u64 = 10;
const RETRY_PERIOD_SECS: u64 = 2;

pub struct LeaderElection {
    client: Client,
    namespace: String,
    identity: String,
}

impl LeaderElection {
    pub fn new(client: Client, namespace: String, identity: String) -> Self {
        Self { client, namespace, identity }
    }

    /// Try to acquire or renew the leader lease.
    /// Returns Ok(true) if this instance is the leader, Ok(false) if not.
    pub async fn try_acquire(&self) -> Result<bool, OperatorError> {
        let leases: Api<Lease> = Api::namespaced(self.client.clone(), &self.namespace);
        let now = chrono::Utc::now();

        match leases.get(LEASE_NAME).await {
            Ok(existing) => {
                let spec = existing.spec.as_ref();
                let holder = spec.and_then(|s| s.holder_identity.as_deref());
                let renew_time = spec.and_then(|s| s.renew_time.as_ref());
                let duration = spec.and_then(|s| s.lease_duration_seconds);

                // Check if current holder's lease has expired
                let is_expired = match (renew_time, duration) {
                    (Some(t), Some(d)) => {
                        let expiry = t.0 + chrono::Duration::seconds(d as i64);
                        now > expiry
                    }
                    _ => true,
                };

                let is_leader = holder == Some(&self.identity);

                if is_leader || is_expired {
                    // Acquire or renew
                    let patch = serde_json::json!({
                        "spec": {
                            "holderIdentity": self.identity,
                            "leaseDurationSeconds": LEASE_DURATION_SECS,
                            "renewTime": now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
                            "acquireTime": if is_leader { None } else { Some(now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)) },
                        }
                    });
                    leases.patch(LEASE_NAME, &PatchParams::apply("cfgd-operator"), &Patch::Merge(patch))
                        .await
                        .map_err(|e| OperatorError::Leader(format!("patch lease: {e}")))?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            Err(kube::Error::Api(resp)) if resp.code == 404 => {
                // Create new lease
                let lease = serde_json::from_value(serde_json::json!({
                    "apiVersion": "coordination.k8s.io/v1",
                    "kind": "Lease",
                    "metadata": {
                        "name": LEASE_NAME,
                        "namespace": self.namespace,
                    },
                    "spec": {
                        "holderIdentity": self.identity,
                        "leaseDurationSeconds": LEASE_DURATION_SECS,
                        "acquireTime": now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
                        "renewTime": now.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
                    }
                })).map_err(|e| OperatorError::Leader(format!("serialize lease: {e}")))?;

                leases.create(&PostParams::default(), &lease)
                    .await
                    .map_err(|e| OperatorError::Leader(format!("create lease: {e}")))?;
                Ok(true)
            }
            Err(e) => Err(OperatorError::Leader(format!("get lease: {e}"))),
        }
    }

    /// Run the leader election loop. Calls `on_started` when leadership is acquired.
    pub async fn run<F, Fut>(&self, on_started: F) -> Result<(), OperatorError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<(), OperatorError>>,
    {
        // Acquire leadership
        loop {
            match self.try_acquire().await {
                Ok(true) => {
                    tracing::info!(identity = %self.identity, "Leader lease acquired");
                    break;
                }
                Ok(false) => {
                    tracing::info!("Not the leader, retrying in {}s", RETRY_PERIOD_SECS);
                    tokio::time::sleep(Duration::from_secs(RETRY_PERIOD_SECS)).await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Leader election error, retrying");
                    tokio::time::sleep(Duration::from_secs(RETRY_PERIOD_SECS)).await;
                }
            }
        }

        // Spawn renewal loop
        let renew_client = self.client.clone();
        let renew_ns = self.namespace.clone();
        let renew_id = self.identity.clone();
        tokio::spawn(async move {
            let le = LeaderElection::new(renew_client, renew_ns, renew_id);
            loop {
                tokio::time::sleep(Duration::from_secs(RENEW_DEADLINE_SECS / 2)).await;
                if let Err(e) = le.try_acquire().await {
                    tracing::error!(error = %e, "Failed to renew leader lease");
                }
            }
        });

        on_started().await
    }
}
```

- [x] **Step 2: Add `Leader` variant to OperatorError**

```rust
#[error("Leader election error: {0}")]
Leader(String),
```

- [x] **Step 3: Add `chrono` dependency**

In `Cargo.toml`:
```toml
chrono = { version = "0.4", default-features = false, features = ["clock", "serde"] }
```

- [x] **Step 4: Add RBAC for Lease objects**

In the RBAC template, add:
```yaml
- apiGroups: ["coordination.k8s.io"]
  resources: ["leases"]
  verbs: ["get", "list", "watch", "create", "update", "patch"]
```

- [x] **Step 5: Wire into main.rs**

```rust
let leader_enabled = std::env::var("LEADER_ELECTION_ENABLED")
    .map(|v| v == "true" || v == "1")
    .unwrap_or(false);

if leader_enabled {
    let namespace = std::env::var("POD_NAMESPACE").unwrap_or_else(|_| "cfgd-system".to_string());
    let identity = std::env::var("POD_NAME").unwrap_or_else(|_| uuid::Uuid::new_v4().to_string());
    let le = leader::LeaderElection::new(client.clone(), namespace, identity);
    let hs = health_state.clone();
    le.run(|| async move {
        hs.set_ready();
        controllers::run(client).await.map_err(|e| e.into())
    }).await?;
} else {
    health_state.set_ready();
    controllers::run(client).await?;
}
```

- [x] **Step 6: Add `pub mod leader;` to lib.rs**

- [x] **Step 7: Run tests**

Run: `cargo test -p cfgd-operator`
Run: `cargo clippy -p cfgd-operator -- -D warnings`
Expected: All pass

- [x] **Step 8: Commit**

```bash
git add crates/cfgd-operator/src/leader.rs crates/cfgd-operator/src/lib.rs \
  crates/cfgd-operator/src/main.rs crates/cfgd-operator/src/errors.rs crates/cfgd-operator/Cargo.toml
git commit -m "feat(operator): add Lease-based leader election for HA"
```

---

### Task 10: Graceful Shutdown

**Files:**
- Modify: `crates/cfgd-operator/src/main.rs`

Register SIGTERM/SIGINT handlers. On signal: stop accepting webhook connections, wait for in-flight work (30s grace), exit cleanly.

- [x] **Step 1: Add shutdown signal handling**

In `main.rs`, create a shutdown signal future:
```rust
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to install SIGTERM handler");

    tokio::select! {
        _ = ctrl_c => tracing::info!("Received SIGINT, initiating graceful shutdown"),
        _ = sigterm.recv() => tracing::info!("Received SIGTERM, initiating graceful shutdown"),
    }
}
```

- [x] **Step 2: Wrap main loop with shutdown**

Use `tokio::select!` to race controllers against the shutdown signal:
```rust
tokio::select! {
    _ = run_controllers_and_gateway(client, health_state, gateway_config) => {},
    _ = shutdown_signal() => {
        tracing::info!("Draining in-flight reconciliations (30s grace)...");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        tracing::info!("Shutdown complete");
    },
}
```

For the gateway path, use `axum::serve(...).with_graceful_shutdown(shutdown_signal())`.

- [x] **Step 3: Run tests**

Run: `cargo test -p cfgd-operator`
Expected: All pass

- [x] **Step 4: Commit**

```bash
git add crates/cfgd-operator/src/main.rs
git commit -m "feat(operator): add graceful shutdown with SIGTERM/SIGINT handling"
```

---

### Task 11: Prometheus Metrics + ServiceMonitor

**Files:**
- Create: `crates/cfgd-operator/src/metrics.rs`
- Modify: `crates/cfgd-operator/src/lib.rs`
- Modify: `crates/cfgd-operator/src/main.rs`
- Modify: `crates/cfgd-operator/src/controllers/mod.rs`
- Modify: `crates/cfgd-operator/Cargo.toml`

Prometheus `/metrics` endpoint on separate port (8443 default). Instrument all controllers.

- [x] **Step 1: Add prometheus-client dependency**

```toml
prometheus-client = "0.22"
```

- [x] **Step 2: Define metrics registry**

```rust
use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::{exponential_buckets, Histogram};
use prometheus_client::registry::Registry;
use std::sync::Arc;

#[derive(Clone, Hash, PartialEq, Eq, prometheus_client::encoding::EncodeLabelSet, Debug)]
pub struct ReconcileLabels {
    pub controller: String,
    pub result: String,
}

#[derive(Clone, Hash, PartialEq, Eq, prometheus_client::encoding::EncodeLabelSet, Debug)]
pub struct DriftLabels {
    pub severity: String,
    pub namespace: String,
}

#[derive(Clone, Hash, PartialEq, Eq, prometheus_client::encoding::EncodeLabelSet, Debug)]
pub struct WebhookLabels {
    pub operation: String,
    pub result: String,
}

#[derive(Clone)]
pub struct Metrics {
    pub reconciliations_total: Family<ReconcileLabels, Counter>,
    pub reconciliation_duration_seconds: Family<ReconcileLabels, Histogram>,
    pub drift_events_total: Family<DriftLabels, Counter>,
    pub webhook_requests_total: Family<WebhookLabels, Counter>,
    pub webhook_duration_seconds: Family<WebhookLabels, Histogram>,
    pub devices_compliant: Family<ReconcileLabels, prometheus_client::metrics::gauge::Gauge>,
    pub devices_enrolled_total: Counter,
}

impl Metrics {
    pub fn new(registry: &mut Registry) -> Self {
        let reconciliations_total = Family::<ReconcileLabels, Counter>::default();
        registry.register(
            "cfgd_operator_reconciliations_total",
            "Total reconciliation attempts",
            reconciliations_total.clone(),
        );

        let reconciliation_duration_seconds =
            Family::<ReconcileLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(exponential_buckets(0.001, 2.0, 15))
            });
        registry.register(
            "cfgd_operator_reconciliation_duration_seconds",
            "Reconciliation duration in seconds",
            reconciliation_duration_seconds.clone(),
        );

        let drift_events_total = Family::<DriftLabels, Counter>::default();
        registry.register(
            "cfgd_operator_drift_events_total",
            "Total drift events",
            drift_events_total.clone(),
        );

        let webhook_requests_total = Family::<WebhookLabels, Counter>::default();
        registry.register(
            "cfgd_operator_webhook_requests_total",
            "Total webhook requests",
            webhook_requests_total.clone(),
        );

        let webhook_duration_seconds =
            Family::<WebhookLabels, Histogram>::new_with_constructor(|| {
                Histogram::new(exponential_buckets(0.001, 2.0, 15))
            });
        registry.register(
            "cfgd_operator_webhook_duration_seconds",
            "Webhook request duration in seconds",
            webhook_duration_seconds.clone(),
        );

        let devices_compliant = Family::<ReconcileLabels, prometheus_client::metrics::gauge::Gauge>::default();
        registry.register(
            "cfgd_operator_devices_compliant",
            "Number of compliant devices per policy",
            devices_compliant.clone(),
        );

        let devices_enrolled_total = Counter::default();
        registry.register(
            "cfgd_operator_devices_enrolled_total",
            "Total device enrollments",
            devices_enrolled_total.clone(),
        );

        Self {
            reconciliations_total,
            reconciliation_duration_seconds,
            drift_events_total,
            webhook_requests_total,
            webhook_duration_seconds,
            devices_compliant,
            devices_enrolled_total,
        }
    }
}
```

- [x] **Step 3: Add metrics HTTP endpoint**

```rust
pub async fn run_metrics_server(
    port: u16,
    registry: Arc<tokio::sync::Mutex<Registry>>,
) -> Result<(), crate::errors::OperatorError> {
    let app = axum::Router::new()
        .route("/metrics", axum::routing::get(move |_: axum::extract::State<Arc<tokio::sync::Mutex<Registry>>>| {
            let reg = registry.clone();
            async move {
                let reg = reg.lock().await;
                let mut buf = String::new();
                encode(&mut buf, &reg).unwrap();
                (
                    [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
                    buf,
                )
            }
        }))
        .with_state(registry);

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| crate::errors::OperatorError::Metrics(format!("bind {addr}: {e}")))?;

    tracing::info!(%addr, "Metrics server listening");
    axum::serve(listener, app)
        .await
        .map_err(|e| crate::errors::OperatorError::Metrics(format!("serve: {e}")))?;
    Ok(())
}
```

- [x] **Step 4: Add `Metrics` variant to OperatorError**

```rust
#[error("Metrics server error: {0}")]
Metrics(String),
```

- [x] **Step 5: Add metrics to ControllerContext and instrument controllers**

In `controllers/mod.rs`, add `pub metrics: Metrics` to `ControllerContext`. Update `run()` to accept `Metrics` as a parameter:

```rust
pub async fn run(client: Client, metrics: Metrics) -> Result<(), OperatorError> {
    // ... construct ctx with metrics ...
}
```

Update `main.rs` call sites to pass the metrics instance (constructed in main alongside the registry for the metrics HTTP endpoint).

In each reconcile function, record timing and counts:
- `ctx.metrics.reconciliations_total.get_or_create(&labels).inc()`
- `ctx.metrics.reconciliation_duration_seconds.get_or_create(&labels).observe(elapsed)`
- `ctx.metrics.drift_events_total` on drift detection
- `ctx.metrics.devices_compliant` updated in ConfigPolicy/ClusterConfigPolicy controllers

- [x] **Step 6: Wire into main.rs**

Start metrics server on port 8443 (configurable via `METRICS_PORT` env var).

- [x] **Step 7: Run tests**

Run: `cargo test -p cfgd-operator`
Expected: All pass

- [x] **Step 8: Commit**

```bash
git add crates/cfgd-operator/src/metrics.rs crates/cfgd-operator/src/lib.rs \
  crates/cfgd-operator/src/main.rs crates/cfgd-operator/src/controllers/mod.rs \
  crates/cfgd-operator/src/errors.rs crates/cfgd-operator/Cargo.toml
git commit -m "feat(operator): add Prometheus metrics endpoint on port 8443"
```

---

### Task 12: OpenTelemetry Tracing

**Files:**
- Modify: `crates/cfgd-operator/Cargo.toml`
- Modify: `crates/cfgd-operator/src/main.rs`

Add OpenTelemetry tracing support via `tracing-opentelemetry`. Configuration via `OTEL_EXPORTER_OTLP_ENDPOINT` env var.

- [x] **Step 1: Add dependencies**

```toml
opentelemetry = "0.28"
opentelemetry_sdk = { version = "0.28", features = ["rt-tokio"] }
opentelemetry-otlp = "0.28"
tracing-opentelemetry = "0.29"
```

**IMPORTANT:** Check crates.io for latest compatible versions. The versions above are illustrative — the OTel Rust ecosystem underwent significant API changes between 0.20-0.28 (pipeline builder removal, SDK restructuring). The `opentelemetry_otlp::new_pipeline()` API may not exist in newer versions. Use the actual API from whichever version you choose. The initialization code below is illustrative — adapt to the real API.

- [x] **Step 2: Initialize OTel in main.rs**

```rust
fn init_tracing() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(env_filter);

    if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok() {
        // OpenTelemetry is configured — add tracing layer
        let tracer = opentelemetry_otlp::new_pipeline()
            .tracing()
            .with_exporter(opentelemetry_otlp::new_exporter().tonic())
            .install_batch(opentelemetry_sdk::runtime::Tokio);

        match tracer {
            Ok(tracer) => {
                let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);
                use tracing_subscriber::layer::SubscriberExt;
                let subscriber = tracing_subscriber::Registry::default()
                    .with(env_filter)
                    .with(tracing_subscriber::fmt::layer())
                    .with(otel_layer);
                tracing::subscriber::set_global_default(subscriber)
                    .expect("set tracing subscriber");
                tracing::info!("OpenTelemetry tracing initialized");
                return;
            }
            Err(e) => {
                eprintln!("Failed to initialize OpenTelemetry: {e}");
            }
        }
    }

    subscriber.init();
}
```

- [x] **Step 3: Run tests**

Run: `cargo test -p cfgd-operator`
Run: `cargo clippy -p cfgd-operator -- -D warnings`
Expected: All pass

- [x] **Step 4: Commit**

```bash
git add crates/cfgd-operator/Cargo.toml crates/cfgd-operator/src/main.rs
git commit -m "feat(operator): add OpenTelemetry tracing via OTEL_EXPORTER_OTLP_ENDPOINT"
```

---

### Task 13: Helm Chart Restructure

**Files:**
- Create: `chart/cfgd/Chart.yaml`
- Create: `chart/cfgd/values.yaml`
- Create: `chart/cfgd/templates/_helpers.tpl`
- Create: `chart/cfgd/templates/operator-deployment.yaml`
- Create: `chart/cfgd/templates/operator-service.yaml`
- Create: `chart/cfgd/templates/serviceaccount.yaml`
- Create: `chart/cfgd/templates/rbac.yaml`
- Create: `chart/cfgd/templates/webhook-config.yaml`
- Create: `chart/cfgd/templates/webhook-service.yaml`
- Create: `chart/cfgd/templates/webhook-cert.yaml`
- Create: `chart/cfgd/templates/gateway-pvc.yaml`
- Create: `chart/cfgd/templates/gateway-service.yaml`
- Create: `chart/cfgd/templates/agent-daemonset.yaml`
- Create: `chart/cfgd/templates/pdb.yaml`
- Create: `chart/cfgd/templates/networkpolicy.yaml`
- Create: `chart/cfgd/templates/servicemonitor.yaml`
- Create: `chart/cfgd/templates/crds/` (copy from gen_crds output)
- Delete references to old charts (don't delete yet — keep until verified)

Consolidates the operator chart (`crates/cfgd-operator/chart/cfgd-operator/`) and agent chart (`charts/cfgd/`) into a unified `chart/cfgd/`.

- [x] **Step 1: Create chart directory structure**

```bash
mkdir -p chart/cfgd/templates/crds chart/cfgd/examples
```

- [x] **Step 2: Write Chart.yaml**

```yaml
apiVersion: v2
name: cfgd
description: cfgd — declarative machine configuration management (operator + agent)
type: application
version: 0.1.0
appVersion: "0.1.0"
keywords:
  - kubernetes
  - operator
  - config
  - gitops
  - crd
  - daemonset
home: https://github.com/tj-smith47/cfgd
maintainers:
  - name: cfgd
    url: https://github.com/tj-smith47/cfgd
```

- [x] **Step 3: Write values.yaml**

Restructured with sections: `operator`, `agent`, `webhook`, `deviceGateway`, `csiDriver`, `podSecurityContext`, `containerSecurityContext`, `podDisruptionBudget`, `networkPolicy`, `metrics`, `probes`.

See `.claude/kubernetes-first-class.md` §5.2 for the complete values structure. Key additions:
- `operator.leaderElection.enabled` (default true)
- `podSecurityContext` with `runAsNonRoot`, UID 65532
- `containerSecurityContext` with `readOnlyRootFilesystem`, drop ALL
- `podDisruptionBudget.enabled` (default false)
- `networkPolicy.enabled` (default false)
- `metrics.enabled`, `metrics.port`, `metrics.serviceMonitor.enabled`
- `probes.port` (default 8081)

- [x] **Step 4: Write _helpers.tpl**

Same patterns as existing `_helpers.tpl` but with `cfgd` prefix instead of `cfgd-operator`.

- [x] **Step 5: Write operator-deployment.yaml**

Based on existing `deployment.yaml` but with:
- Security contexts from values
- Health probes on dedicated port (not webhook port)
- Metrics port
- Leader election env vars
- OpenTelemetry env vars

- [x] **Step 6: Write PDB template**

```yaml
{{- if and .Values.podDisruptionBudget.enabled (gt (int .Values.operator.replicaCount) 1) }}
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata:
  name: {{ include "cfgd.fullname" . }}-operator
  labels:
    {{- include "cfgd.labels" . | nindent 4 }}
spec:
  minAvailable: {{ .Values.podDisruptionBudget.minAvailable }}
  selector:
    matchLabels:
      {{- include "cfgd.operatorSelectorLabels" . | nindent 6 }}
{{- end }}
```

- [x] **Step 7: Write NetworkPolicy template**

```yaml
{{- if .Values.networkPolicy.enabled }}
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: {{ include "cfgd.fullname" . }}-operator
spec:
  podSelector:
    matchLabels:
      {{- include "cfgd.operatorSelectorLabels" . | nindent 6 }}
  policyTypes: [Ingress, Egress]
  ingress:
    - ports:
        - port: {{ .Values.webhook.port }}
        {{- if .Values.deviceGateway.enabled }}
        - port: {{ .Values.deviceGateway.port }}
        {{- end }}
        - port: {{ .Values.probes.port }}
      from:
        - namespaceSelector: {}
  egress:
    - ports:
        - port: 443
        - port: 6443
{{- end }}
```

- [x] **Step 8: Write ServiceMonitor template**

```yaml
{{- if and .Values.metrics.enabled .Values.metrics.serviceMonitor.enabled }}
apiVersion: monitoring.coreos.com/v1
kind: ServiceMonitor
metadata:
  name: {{ include "cfgd.fullname" . }}-operator
  labels:
    {{- include "cfgd.labels" . | nindent 4 }}
spec:
  selector:
    matchLabels:
      {{- include "cfgd.operatorSelectorLabels" . | nindent 6 }}
  endpoints:
    - port: metrics
      interval: {{ .Values.metrics.serviceMonitor.interval }}
{{- end }}
```

- [x] **Step 9: Write agent-daemonset.yaml**

Port existing `charts/cfgd/templates/daemonset.yaml` into the unified chart, gated on `agent.enabled`.

- [x] **Step 10: Write remaining templates**

Port and update: `serviceaccount.yaml`, `rbac.yaml`, `webhook-config.yaml`, `webhook-service.yaml`, `webhook-cert.yaml`, `gateway-pvc.yaml`, `gateway-service.yaml`.

Update RBAC to include:
- Lease objects for leader election: `apiGroups: ["coordination.k8s.io"]`, `resources: ["leases"]`, `verbs: ["get", "list", "watch", "create", "update", "patch"]`
- ClusterConfigPolicy CRD: add `clusterconfigpolicies` and `clusterconfigpolicies/status` to cfgd.io rules
- Events v1 API: `apiGroups: ["", "events.k8s.io"]`, `resources: ["events"]`, `verbs: ["create", "patch"]`
- New CRD status subresources

- [x] **Step 11: Generate and copy CRD templates**

Run `cargo run --bin cfgd-gen-crds` and split output into `chart/cfgd/templates/crds/`.

- [x] **Step 12: Verify Helm lint**

Run: `helm lint chart/cfgd/`
Expected: No errors

- [x] **Step 13: Commit**

```bash
git add chart/cfgd/
git commit -m "feat(helm): consolidate operator + agent charts into unified chart/cfgd/"
```

---

### Task 14: Helm Extras — Schema, NOTES.txt, Test Hook, Example Values

**Files:**
- Create: `chart/cfgd/values.schema.json`
- Create: `chart/cfgd/templates/NOTES.txt`
- Create: `chart/cfgd/templates/tests/test-connection.yaml`
- Create: `chart/cfgd/examples/operator-only.yaml`
- Create: `chart/cfgd/examples/with-gateway.yaml`
- Create: `chart/cfgd/examples/full.yaml`

- [x] **Step 1: Write values.schema.json**

JSON Schema for values.yaml. Validates types, required fields, enum constraints. Enables `helm lint` validation.

- [x] **Step 2: Write NOTES.txt**

```
Thank you for installing {{ .Chart.Name }}!

{{- if .Values.operator.enabled }}
Operator: {{ include "cfgd.fullname" . }}-operator
  Verify: kubectl get pods -n {{ .Release.Namespace }} -l app.kubernetes.io/name={{ include "cfgd.name" . }}
  CRDs:   kubectl get crd | grep cfgd.io
{{- end }}

{{- if .Values.deviceGateway.enabled }}
Gateway URL: http://{{ include "cfgd.fullname" . }}-gateway.{{ .Release.Namespace }}:{{ .Values.deviceGateway.port }}
{{- end }}

Documentation: https://github.com/tj-smith47/cfgd
```

- [x] **Step 3: Write test hook**

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: "{{ include "cfgd.fullname" . }}-test"
  labels:
    {{- include "cfgd.labels" . | nindent 4 }}
  annotations:
    "helm.sh/hook": test
spec:
  containers:
    - name: test
      image: bitnami/kubectl:latest
      command: ['sh', '-c']
      args:
        - |
          echo "Checking CRDs..."
          kubectl get crd machineconfigs.cfgd.io || exit 1
          kubectl get crd configpolicies.cfgd.io || exit 1
          kubectl get crd driftalerts.cfgd.io || exit 1
          kubectl get crd clusterconfigpolicies.cfgd.io || exit 1
          echo "All CRDs established!"
          echo "Checking operator..."
          kubectl get pods -n {{ .Release.Namespace }} -l app.kubernetes.io/name={{ include "cfgd.name" . }} | grep Running || exit 1
          echo "Operator running!"
  restartPolicy: Never
```

- [x] **Step 4: Write example values files**

`operator-only.yaml`: minimal operator deployment.
`with-gateway.yaml`: operator + device gateway.
`full.yaml`: operator + gateway + agent + metrics + PDB + NetworkPolicy.

- [x] **Step 5: Run helm lint**

Run: `helm lint chart/cfgd/ -f chart/cfgd/examples/operator-only.yaml`
Run: `helm lint chart/cfgd/ -f chart/cfgd/examples/full.yaml`
Expected: No errors

- [x] **Step 6: Commit**

```bash
git add chart/cfgd/values.schema.json chart/cfgd/templates/NOTES.txt \
  chart/cfgd/templates/tests/ chart/cfgd/examples/
git commit -m "feat(helm): add values schema, NOTES.txt, test hook, example values"
```

---

### Task 15: Multi-Tenancy RBAC Templates

**Files:**
- Create: `chart/cfgd/templates/rbac-examples/platform-admin.yaml`
- Create: `chart/cfgd/templates/rbac-examples/team-lead.yaml`
- Create: `chart/cfgd/templates/rbac-examples/team-member.yaml`
- Create: `chart/cfgd/templates/rbac-examples/module-publisher.yaml`

Helm-templated RBAC examples for 4 personas. These are gated on `rbacExamples.enabled` (default false) so they're opt-in.

- [x] **Step 1: Write RBAC templates**

Platform admin (ClusterRole):
```yaml
{{- if .Values.rbacExamples.enabled }}
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRole
metadata:
  name: {{ include "cfgd.fullname" . }}-platform-admin
  labels:
    {{- include "cfgd.labels" . | nindent 4 }}
rules:
  - apiGroups: ["cfgd.io"]
    resources: ["*"]
    verbs: ["*"]
{{- end }}
```

Team lead (Role, namespace-scoped):
```yaml
{{- if .Values.rbacExamples.enabled }}
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: {{ include "cfgd.fullname" . }}-team-lead
rules:
  - apiGroups: ["cfgd.io"]
    resources: ["machineconfigs", "configpolicies", "driftalerts"]
    verbs: ["get", "list", "watch", "create", "update", "patch", "delete"]
  - apiGroups: ["cfgd.io"]
    resources: ["machineconfigs/status", "configpolicies/status"]
    verbs: ["get"]
{{- end }}
```

Team member (read-only Role) and module publisher (cluster-scoped) follow the same pattern from the spec (§11 of kubernetes-first-class.md).

- [x] **Step 2: Add `rbacExamples.enabled: false` to values.yaml**

- [x] **Step 3: Write namespace isolation documentation**

Create `docs/multi-tenancy.md` documenting:
- Each team gets a namespace
- ConfigPolicy is namespace-scoped, applies only within its namespace
- ClusterConfigPolicy applies org-wide across all namespaces
- ClusterConfigPolicy always wins on conflicts (settings, packageVersions, trustedRegistries)
- Operator watches all namespaces via `Api::all()`
- RBAC examples reference: platform admin, team lead, team member, module publisher

- [x] **Step 4: Commit**

```bash
git add chart/cfgd/templates/rbac-examples/ chart/cfgd/values.yaml docs/multi-tenancy.md
git commit -m "feat(helm): add multi-tenancy RBAC example templates and namespace isolation docs"
```

---

### Task 16: Crossplane E2E Test

**Files:**
- Create: `tests/e2e/crossplane/scripts/run-crossplane-tests.sh`
- Create: `tests/e2e/crossplane/manifests/teamconfig-sample.yaml`

E2E test for Crossplane integration: kind cluster + Crossplane, apply XRD/Composition/Function, create TeamConfig with 2 members, verify MachineConfig + ConfigPolicy CRDs generated.

- [x] **Step 1: Write test script**

```bash
#!/usr/bin/env bash
# E2E test for Crossplane integration
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/../../common/helpers.sh"
MANIFESTS_DIR="$SCRIPT_DIR/../manifests"
CROSSPLANE_DIR="$REPO_ROOT/manifests/crossplane"

echo "=== Crossplane E2E Tests ==="

# T-XP01: Install Crossplane
begin_test "T-XP01: Crossplane installation"
helm repo add crossplane-stable https://charts.crossplane.io/stable
helm install crossplane crossplane-stable/crossplane --namespace crossplane-system --create-namespace --wait
wait_for_deployment crossplane-system crossplane 120
pass_test "T-XP01"

# Install CRDs
CRD_YAML=$(cargo run --release --bin cfgd-gen-crds --manifest-path "$REPO_ROOT/Cargo.toml" 2>/dev/null)
echo "$CRD_YAML" | kubectl apply -f -

# Apply XRD, Composition, Function
kubectl apply -f "$CROSSPLANE_DIR/xrd-teamconfig.yaml"
kubectl apply -f "$CROSSPLANE_DIR/composition.yaml"
kubectl apply -f "$CROSSPLANE_DIR/function-cfgd.yaml"

# T-XP02: Create TeamConfig with 2 members
begin_test "T-XP02: TeamConfig generates MachineConfigs"
kubectl apply -f "$MANIFESTS_DIR/teamconfig-sample.yaml"
sleep 10
MC_COUNT=$(kubectl get mc -A --no-headers 2>/dev/null | wc -l)
if [ "$MC_COUNT" -ge 2 ]; then
    pass_test "T-XP02"
else
    fail_test "T-XP02" "Expected >=2 MachineConfigs, got $MC_COUNT"
fi

# T-XP03: Verify ConfigPolicy created
begin_test "T-XP03: TeamConfig generates ConfigPolicy"
CP_COUNT=$(kubectl get cpol -A --no-headers 2>/dev/null | wc -l)
if [ "$CP_COUNT" -ge 1 ]; then
    pass_test "T-XP03"
else
    fail_test "T-XP03" "Expected >=1 ConfigPolicy, got $CP_COUNT"
fi

echo "=== Crossplane E2E Tests Complete ==="
```

- [x] **Step 2: Write TeamConfig sample manifest**

```yaml
apiVersion: cfgd.io/v1alpha1
kind: TeamConfig
metadata:
  name: test-team
spec:
  teamName: test-team
  members:
    - hostname: dev-laptop-1
      profile: developer
      username: alice
    - hostname: dev-laptop-2
      profile: developer
      username: bob
  policy:
    packages:
      - name: kubectl
      - name: git
    requiredModules:
      - name: corp-vpn
```

- [x] **Step 3: Add member addition/removal tests**

Add to the test script:

```bash
# T-XP04: Add a member → new MachineConfig created
begin_test "T-XP04: Member addition creates MachineConfig"
kubectl patch teamconfig test-team --type=merge -p '{"spec":{"members":[{"hostname":"dev-laptop-1","profile":"developer","username":"alice"},{"hostname":"dev-laptop-2","profile":"developer","username":"bob"},{"hostname":"dev-laptop-3","profile":"developer","username":"charlie"}]}}'
sleep 10
MC_COUNT=$(kubectl get mc -A --no-headers 2>/dev/null | wc -l)
if [ "$MC_COUNT" -ge 3 ]; then
    pass_test "T-XP04"
else
    fail_test "T-XP04" "Expected >=3 MachineConfigs after adding member, got $MC_COUNT"
fi

# T-XP05: Remove a member → MachineConfig garbage-collected
begin_test "T-XP05: Member removal garbage-collects MachineConfig"
kubectl patch teamconfig test-team --type=merge -p '{"spec":{"members":[{"hostname":"dev-laptop-1","profile":"developer","username":"alice"},{"hostname":"dev-laptop-2","profile":"developer","username":"bob"}]}}'
sleep 15
MC_COUNT=$(kubectl get mc -A --no-headers 2>/dev/null | wc -l)
if [ "$MC_COUNT" -eq 2 ]; then
    pass_test "T-XP05"
else
    fail_test "T-XP05" "Expected 2 MachineConfigs after removing member, got $MC_COUNT"
fi
```

- [x] **Step 4: Verify XRD uses v2 API**

Check that `manifests/crossplane/xrd-teamconfig.yaml` uses `apiextensions.crossplane.io/v2` and is compatible with the target Crossplane version.

- [x] **Step 5: Verify function-cfgd CI pipeline exists**

The function-cfgd build/push already exists in `.github/workflows/release.yml` (xpkg build + push to GHCR). No new workflow needed. Just verify the existing pipeline references are correct.

- [x] **Step 6: Commit**

```bash
git add tests/e2e/crossplane/
git commit -m "feat(test): add Crossplane E2E tests for TeamConfig generation"
```

---

### Task 17: Server-Side Apply — Field Managers + Structured Merge Diff

**Files:**
- Modify: `crates/cfgd-operator/src/controllers/mod.rs`
- Modify: `crates/cfgd-operator/src/gen_crds.rs`

Use proper field manager names for SSA so Crossplane (spec owner), operator (status owner), and policy controller (annotations) don't conflict. Add structured merge diff annotations to CRD schemas for proper list merge behavior.

- [x] **Step 1: Define field manager constants**

```rust
const FIELD_MANAGER_OPERATOR: &str = "cfgd-operator";
const FIELD_MANAGER_STATUS: &str = "cfgd-operator/status";
```

- [x] **Step 2: Update all PatchParams to use correct field managers**

All status patches use `PatchParams::apply(FIELD_MANAGER_STATUS)`.
All spec patches (finalizer, etc.) use `PatchParams::apply(FIELD_MANAGER_OPERATOR)`.

Note: Crossplane composition function uses its own field manager (`crossplane-composition-function`) for spec fields. This is already set by Crossplane — no code change needed.

- [x] **Step 3: Add structured merge diff annotations to CRD schemas**

In `gen_crds.rs`, post-process the generated CRD JSON to add strategic merge patch annotations. Without these, SSA merge behavior on lists is replace-all instead of merge-by-key.

```rust
fn inject_smd_annotations(crd: &mut serde_json::Value) {
    // conditions list: merge by "type" key
    let conditions_paths = [
        "/spec/versions/0/schema/openAPIV3Schema/properties/status/properties/conditions",
    ];
    for path in &conditions_paths {
        if let Some(conditions) = crd.pointer_mut(path) {
            conditions["x-kubernetes-list-type"] = serde_json::json!("map");
            conditions["x-kubernetes-list-map-keys"] = serde_json::json!(["type"]);
        }
    }

    // packages list: merge by "name" key
    if let Some(packages) = crd.pointer_mut(
        "/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/packages"
    ) {
        packages["x-kubernetes-list-type"] = serde_json::json!("map");
        packages["x-kubernetes-list-map-keys"] = serde_json::json!(["name"]);
    }

    // moduleRefs list: merge by "name" key
    if let Some(refs) = crd.pointer_mut(
        "/spec/versions/0/schema/openAPIV3Schema/properties/spec/properties/moduleRefs"
    ) {
        refs["x-kubernetes-list-type"] = serde_json::json!("map");
        refs["x-kubernetes-list-map-keys"] = serde_json::json!(["name"]);
    }
}
```

Apply to all CRDs (MachineConfig, ConfigPolicy, ClusterConfigPolicy, DriftAlert).

- [x] **Step 4: Run tests**

Run: `cargo test -p cfgd-operator`
Expected: All pass

- [x] **Step 5: Commit**

```bash
git add crates/cfgd-operator/src/controllers/mod.rs crates/cfgd-operator/src/gen_crds.rs
git commit -m "feat(operator): SSA field managers and structured merge diff annotations"
```

---

### Task 18: Update PLAN.md + Final Verification

**Files:**
- Modify: `.claude/PLAN.md`
- Modify: `.claude/COMPLETED.md`

- [x] **Step 1: Run full test suite**

```bash
cargo test -p cfgd-operator
cargo clippy -p cfgd-operator -- -D warnings
cargo fmt -p cfgd-operator -- --check
```

- [x] **Step 2: Run audit scripts**

```bash
bash .claude/scripts/audit.sh
bash .claude/scripts/completeness-check.sh
```

- [x] **Step 3: Check off all Tier 1 items in PLAN.md**

Mark every Tier 1 checkbox as complete: `- [x]`

- [x] **Step 4: Move completed Tier 1 section to COMPLETED.md**

- [x] **Step 5: Regenerate CRDs for Helm chart**

```bash
cargo run --bin cfgd-gen-crds > /tmp/crds.yaml
```
Split and copy into `chart/cfgd/templates/crds/`.

- [x] **Step 6: Commit**

```bash
git add .claude/PLAN.md .claude/COMPLETED.md chart/cfgd/templates/crds/
git commit -m "feat(operator): complete Tier 1 — operator hardening & CRD enhancement"
```
