# Tier 4: Pod Module Injection — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Enable Kubernetes pods to consume cfgd modules via CSI driver volumes, with automatic injection via mutating webhook and manual injection via kubectl plugin.

**Architecture:** Three independent binaries/components: (1) CSI driver DaemonSet pulls OCI modules and mounts them as read-only volumes, (2) mutating webhook in cfgd-operator injects CSI volumes into pods based on annotations and ConfigPolicy, (3) kubectl plugin in cfgd binary provides debug/exec/inject/status/version subcommands via argv[0] detection.

**Tech Stack:** Rust, tonic (gRPC), kube-rs, axum, clap, Helm, CSI spec v1.9.0

---

## File Structure

### New Files

```
crates/cfgd-csi/
├── Cargo.toml                    # Binary crate: tonic, cfgd-core, tokio, tracing, prometheus-client
├── build.rs                      # tonic-build: compile csi.proto
├── proto/
│   └── csi.proto                 # CSI spec v1.9.0 Identity + Node service definitions
├── src/
│   ├── main.rs                   # Entry point: gRPC server on unix socket, signal handling
│   ├── identity.rs               # Identity service: GetPluginInfo, GetPluginCapabilities, Probe
│   ├── node.rs                   # Node service: Publish/Unpublish/Stage/Unstage/GetInfo/GetCapabilities
│   ├── cache.rs                  # LRU cache: pull, evict, size tracking, directory management
│   ├── metrics.rs                # Prometheus metrics: publish_total, pull_duration, cache_size, cache_hits
│   └── errors.rs                 # CsiError enum (thiserror)
chart/cfgd/templates/
├── csi-daemonset.yaml            # CSI driver DaemonSet + node-driver-registrar sidecar
├── csi-rbac.yaml                 # CSI driver ServiceAccount + ClusterRole
├── csi-driver.yaml               # CSIDriver object registration
├── mutating-webhook-config.yaml  # MutatingWebhookConfiguration for pod injection
```

### Modified Files

```
Cargo.toml                                      # Add crates/cfgd-csi to workspace members
crates/cfgd-operator/src/webhook.rs              # Add /mutate-pods endpoint
crates/cfgd-operator/src/main.rs                 # Wire mutating webhook route
crates/cfgd/src/main.rs                          # argv[0] detection for kubectl-cfgd mode
crates/cfgd/src/cli/mod.rs                       # Add Plugin subcommands (debug/exec/inject/status/version)
crates/cfgd/src/cli/plugin.rs                    # New: kubectl plugin command implementations
chart/cfgd/values.yaml                           # Add csiDriver section, mutatingWebhook section
CLAUDE.md                                        # Add crates/cfgd-csi/ to allowed Command locations
.claude/PLAN.md                                  # Check off completed items
```

---

### Task 1: CSI driver — project scaffold and gRPC codegen

**Files:**
- Create: `crates/cfgd-csi/Cargo.toml`
- Create: `crates/cfgd-csi/build.rs`
- Create: `crates/cfgd-csi/proto/csi.proto`
- Create: `crates/cfgd-csi/src/main.rs` (minimal — just compiles)
- Create: `crates/cfgd-csi/src/errors.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Create Cargo.toml for cfgd-csi**

```toml
[package]
name = "cfgd-csi"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true

[[bin]]
name = "cfgd-csi"
path = "src/main.rs"

[dependencies]
cfgd-core = { path = "../cfgd-core" }
tonic = "0.13"
prost = "0.13"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter", "json"] }
prometheus-client = "0.22"
sha2 = "0.10"
thiserror = "2"
nix = { version = "0.29", features = ["mount", "fs"] }
filetime = "0.2"

[build-dependencies]
tonic-build = "0.13"
```

- [ ] **Step 2: Download CSI proto and create build.rs**

Download the CSI spec proto from the official container-storage-interface repo (v1.9.0). Create `proto/csi.proto` with just the Identity and Node service definitions (no Controller — we don't need it).

`build.rs`:
```rust
fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(&["proto/csi.proto"], &["proto/"])?;
    Ok(())
}
```

- [ ] **Step 3: Create errors.rs**

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CsiError {
    #[error("module not found: {name}:{version}")]
    ModuleNotFound { name: String, version: String },

    #[error("OCI pull failed: {0}")]
    PullFailed(#[from] cfgd_core::errors::OciError),

    #[error("mount failed: {message}")]
    MountFailed { message: String },

    #[error("cache error: {message}")]
    CacheError { message: String },

    #[error("invalid volume attribute: {key}")]
    InvalidAttribute { key: String },

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
```

- [ ] **Step 4: Create minimal main.rs**

```rust
mod errors;

fn main() {
    // Placeholder — full implementation in Task 5
}
```

Note: No `println!` — CLAUDE.md Hard Rule #1 prohibits direct terminal output. The CSI driver uses `tracing` exclusively.

- [ ] **Step 5: Add to workspace and verify compilation**

Add `"crates/cfgd-csi"` to workspace `Cargo.toml` members.

Run: `cargo check -p cfgd-csi`
Expected: compiles (proto codegen runs, main compiles)

- [ ] **Step 6: Commit**

```
git add Cargo.toml crates/cfgd-csi/
git commit -m "feat(csi): scaffold CSI driver crate with gRPC codegen"
```

---

### Task 2: CSI driver — Identity service

**Files:**
- Create: `crates/cfgd-csi/src/identity.rs`
- Modify: `crates/cfgd-csi/src/main.rs`

- [ ] **Step 1: Write tests for Identity service RPCs**

In `identity.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    // Test GetPluginInfo returns correct name and version
    // Test GetPluginCapabilities returns VOLUME_CONDITION
    // Test Probe returns ready=true
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd-csi`
Expected: FAIL (functions not implemented)

- [ ] **Step 3: Implement Identity service**

```rust
use crate::csi::v1::{
    identity_server::Identity,
    GetPluginInfoRequest, GetPluginInfoResponse,
    GetPluginCapabilitiesRequest, GetPluginCapabilitiesResponse,
    ProbeRequest, ProbeResponse,
    plugin_capability, PluginCapability,
};

pub struct CfgdIdentity;

#[tonic::async_trait]
impl Identity for CfgdIdentity {
    async fn get_plugin_info(&self, _req: Request<GetPluginInfoRequest>)
        -> Result<Response<GetPluginInfoResponse>, Status> {
        Ok(Response::new(GetPluginInfoResponse {
            name: "csi.cfgd.io".to_string(),
            vendor_version: env!("CARGO_PKG_VERSION").to_string(),
            manifest: Default::default(),
        }))
    }
    // ... GetPluginCapabilities (VOLUME_CONDITION), Probe (ready: true)
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cfgd-csi`
Expected: PASS

- [ ] **Step 5: Commit**

```
git add crates/cfgd-csi/src/identity.rs crates/cfgd-csi/src/main.rs
git commit -m "feat(csi): Identity service — GetPluginInfo, GetPluginCapabilities, Probe"
```

---

### Task 3: CSI driver — cache module

**Files:**
- Create: `crates/cfgd-csi/src/cache.rs`

- [ ] **Step 1: Write tests for cache operations**

Test cases:
- `cache_pull_creates_directory` — pulling module creates `<cache_root>/<name>/<version>/<platform>/` and extracts content
- `cache_hit_returns_existing` — second pull of same module returns cached path without OCI call
- `cache_eviction_lru` — when cache exceeds max size, least recently used entries are removed
- `cache_size_tracking` — `current_size_bytes()` reflects actual disk usage

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd-csi -- cache`
Expected: FAIL

- [ ] **Step 3: Implement Cache struct**

```rust
pub struct Cache {
    root: PathBuf,
    max_bytes: u64,
}

impl Cache {
    pub fn new(root: PathBuf, max_bytes: u64) -> Result<Self, CsiError>;
    pub fn get_or_pull(&self, module: &str, version: &str, oci_ref: &str) -> Result<PathBuf, CsiError>;
    pub fn get(&self, module: &str, version: &str) -> Option<PathBuf>;
    pub fn evict_lru(&self) -> Result<(), CsiError>;
    pub fn current_size_bytes(&self) -> u64;
}
```

Cache key layout: `<root>/<module>/<version>/<platform>/`
LRU tracked via access time using the `filetime` crate — update atime on cache hit via `filetime::set_file_atime()`.
Eviction: sort entries by atime ascending, remove oldest until under max_bytes.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cfgd-csi -- cache`
Expected: PASS

- [ ] **Step 5: Commit**

```
git add crates/cfgd-csi/src/cache.rs
git commit -m "feat(csi): LRU cache for OCI module artifacts"
```

---

### Task 4: CSI driver — Node service

**Files:**
- Create: `crates/cfgd-csi/src/node.rs`
- Modify: `crates/cfgd-csi/src/main.rs`

- [ ] **Step 1: Write tests for Node service RPCs**

Test cases:
- `node_get_capabilities_returns_stage_unstage` — reports `STAGE_UNSTAGE_VOLUME`
- `node_get_info_returns_node_id` — returns hostname as node ID
- `node_publish_volume_missing_module_attr` — returns InvalidArgument
- `node_publish_volume_missing_version_attr` — returns InvalidArgument
- `node_stage_volume_pulls_to_cache` — stages (pulls) module into cache
- `node_unstage_volume_succeeds` — no-op (cache entry stays)
- `node_unpublish_volume_succeeds` — unmounts target path

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd-csi -- node`
Expected: FAIL

- [ ] **Step 3: Implement Node service**

```rust
pub struct CfgdNode {
    cache: Arc<Cache>,
    metrics: Arc<CsiMetrics>,
}

#[tonic::async_trait]
impl Node for CfgdNode {
    async fn node_stage_volume(&self, req: Request<NodeStageVolumeRequest>)
        -> Result<Response<NodeStageVolumeResponse>, Status> {
        // Extract volumeAttributes: module, version
        // Pull OCI artifact to cache via cache.get_or_pull()
        // Return success
    }

    async fn node_publish_volume(&self, req: Request<NodePublishVolumeRequest>)
        -> Result<Response<NodePublishVolumeResponse>, Status> {
        // Extract module/version from volumeAttributes
        // Get cached path
        // Bind mount cached content to target_path (read-only)
        // Record metric
    }

    async fn node_unpublish_volume(&self, req: Request<NodeUnpublishVolumeRequest>)
        -> Result<Response<NodeUnpublishVolumeResponse>, Status> {
        // Unmount target_path
    }

    async fn node_unstage_volume(&self, req: Request<NodeUnstageVolumeRequest>)
        -> Result<Response<NodeUnstageVolumeResponse>, Status> {
        // No-op — cache persists
        Ok(Response::new(NodeUnstageVolumeResponse {}))
    }

    async fn node_get_capabilities(&self, _req: Request<NodeGetCapabilitiesRequest>)
        -> Result<Response<NodeGetCapabilitiesResponse>, Status> {
        // Return STAGE_UNSTAGE_VOLUME
    }

    async fn node_get_info(&self, _req: Request<NodeGetInfoRequest>)
        -> Result<Response<NodeGetInfoResponse>, Status> {
        // Return hostname as node_id
    }
}
```

Bind mount is a two-step operation on Linux (MS_BIND|MS_RDONLY in a single call does NOT work):
```rust
// Step 1: bind mount
nix::mount::mount(Some(src), target, None::<&str>, MsFlags::MS_BIND, None::<&str>)?;
// Step 2: remount read-only
nix::mount::mount(None::<&str>, target, None::<&str>, MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY, None::<&str>)?;
```

Update CLAUDE.md to add `crates/cfgd-csi/` to the allowed `std::process::Command` locations list (Hard Rule #6). The CSI driver may need to fall back to `mount`/`umount` CLI if nix crate mount fails on some kernels.

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p cfgd-csi -- node`
Expected: PASS

- [ ] **Step 5: Commit**

```
git add crates/cfgd-csi/src/node.rs crates/cfgd-csi/src/main.rs
git commit -m "feat(csi): Node service — Publish/Unpublish/Stage/Unstage volumes"
```

---

### Task 5: CSI driver — metrics and main entry point

**Files:**
- Create: `crates/cfgd-csi/src/metrics.rs`
- Modify: `crates/cfgd-csi/src/main.rs`

- [ ] **Step 1: Write tests for metrics**

Test cases:
- `metrics_register_without_panic` — creating CsiMetrics succeeds
- `metrics_encode_produces_output` — encoding metrics produces valid text

- [ ] **Step 2: Implement CsiMetrics**

```rust
pub struct CsiMetrics {
    pub volume_publish_total: Family<Vec<(String, String)>, Counter>,
    pub pull_duration_seconds: Family<Vec<(String, String)>, Histogram>,
    pub cache_size_bytes: Gauge,
    pub cache_hits_total: Counter,
}
```

- [ ] **Step 3: Implement main.rs**

```rust
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Init tracing
    // Parse env vars: CSI_ENDPOINT (default /csi/csi.sock), CACHE_DIR, CACHE_MAX_BYTES, METRICS_PORT (default 9090)
    // Create cache
    // Create metrics registry
    // Remove existing socket file if present
    // Create UnixListener
    // Build tonic Server with Identity + Node services
    // Spawn metrics HTTP server
    // Serve with graceful shutdown on SIGTERM
}
```

Socket path: `CSI_ENDPOINT` env var, default `/csi/csi.sock` (container path; mapped to host via `hostPath` volume at `/var/lib/kubelet/plugins/csi.cfgd.io/csi.sock`).
Cache dir: `CACHE_DIR` env var, default `/var/lib/cfgd-csi/cache`.
Cache max: `CACHE_MAX_BYTES` env var, default `5368709120` (5Gi).
Metrics port: `METRICS_PORT` env var, default `9090`.

- [ ] **Step 4: Verify compilation**

Run: `cargo build -p cfgd-csi`
Expected: compiles

- [ ] **Step 5: Write integration test — full gRPC flow in-process**

Create `crates/cfgd-csi/tests/grpc_integration.rs`:
- Start gRPC server on a temp unix socket
- Connect with `tonic` client
- Call `GetPluginInfo` → verify name = `csi.cfgd.io`
- Call `Probe` → verify ready = true
- Call `NodeGetCapabilities` → verify `STAGE_UNSTAGE_VOLUME`
- Call `NodeGetInfo` → verify node_id is non-empty
- Call `NodePublishVolume` with missing `module` attr → verify InvalidArgument

- [ ] **Step 6: Run all CSI tests (unit + integration)**

Run: `cargo test -p cfgd-csi`
Expected: all PASS

- [ ] **Step 7: Commit**

```
git add crates/cfgd-csi/src/metrics.rs crates/cfgd-csi/src/main.rs crates/cfgd-csi/tests/
git commit -m "feat(csi): metrics, main entry point, gRPC integration test"
```

---

### Task 6: CSI driver — Helm templates

**Files:**
- Create: `chart/cfgd/templates/csi-daemonset.yaml`
- Create: `chart/cfgd/templates/csi-rbac.yaml`
- Create: `chart/cfgd/templates/csi-driver.yaml`
- Modify: `chart/cfgd/values.yaml`

- [ ] **Step 1: Add csiDriver values to values.yaml**

```yaml
csiDriver:
  enabled: false
  image:
    repository: ghcr.io/tj-smith47/cfgd-csi
    tag: ""
    pullPolicy: IfNotPresent
  cache:
    maxSizeGi: 5
    storageClass: ""
  resources:
    limits:
      cpu: 100m
      memory: 128Mi
    requests:
      cpu: 50m
      memory: 64Mi
  nodeSelector: {}
  tolerations:
    - operator: Exists
  registrar:
    image:
      repository: registry.k8s.io/sig-storage/csi-node-driver-registrar
      tag: v2.10.0
```

- [ ] **Step 2: Create csi-driver.yaml (CSIDriver object)**

```yaml
{{- if .Values.csiDriver.enabled }}
apiVersion: storage.k8s.io/v1
kind: CSIDriver
metadata:
  name: csi.cfgd.io
spec:
  attachRequired: false
  podInfoOnMount: true
  volumeLifecycleModes:
    - Ephemeral
    - Persistent
{{- end }}
```

- [ ] **Step 3: Create csi-daemonset.yaml**

DaemonSet with cfgd-csi container + node-driver-registrar sidecar.

Volume mapping (host ↔ container):
- `plugin-dir`: hostPath `/var/lib/kubelet/plugins/csi.cfgd.io/` → container `/csi/` (CSI socket lives here)
- `pods-mount-dir`: hostPath `/var/lib/kubelet/` → container `/var/lib/kubelet/` with `mountPropagation: Bidirectional` (required for bind mounts to propagate to kubelet)
- `registration-dir`: hostPath `/var/lib/kubelet/plugins_registry/` → node-driver-registrar container
- `cache`: emptyDir (or PVC if `csiDriver.cache.storageClass` set) → `/var/lib/cfgd-csi/cache`

SecurityContext: privileged: true (required for bind mount operations).

The `node-driver-registrar` sidecar registers the socket path with kubelet via the registration API.

- [ ] **Step 4: Create csi-rbac.yaml**

ServiceAccount + ClusterRole (get/list pods, modules.cfgd.io) + ClusterRoleBinding.

Note: Pre-pull of popular modules via operator-pushed ConfigMap (design spec line 713) is a follow-up optimization — not required for initial delivery. Can be added after the core CSI flow is validated in production.

- [ ] **Step 5: Commit**

```
git add chart/cfgd/templates/csi-daemonset.yaml chart/cfgd/templates/csi-rbac.yaml chart/cfgd/templates/csi-driver.yaml chart/cfgd/values.yaml
git commit -m "feat(csi): Helm templates — DaemonSet, CSIDriver, RBAC"
```

---

### Task 7: Pod module mutating webhook — endpoint

**Files:**
- Modify: `crates/cfgd-operator/src/webhook.rs`
- Modify: `crates/cfgd-operator/src/main.rs`

- [ ] **Step 1: Write tests for pod mutation logic**

Test cases in `webhook.rs`:
- `mutate_pod_no_annotation_no_policy` — pod without `cfgd.io/modules` annotation and no ConfigPolicy passes through unmodified
- `mutate_pod_with_annotation` — pod with `cfgd.io/modules: "nettools:1.0"` gets CSI volume + volumeMount injected on all containers
- `mutate_pod_with_env_injection` — module with env vars adds them to all containers
- `mutate_pod_with_post_apply_script` — module with `scripts.postApply` injects init container + emptyDir
- `mutate_pod_skip_label` — pod with `cfgd.io/skip-injection` label is not mutated
- `mutate_pod_multiple_modules` — multiple modules inject multiple volumes
- `mutate_pod_cluster_config_policy` — ClusterConfigPolicy requiredModules are injected even without pod annotation
- `parse_module_annotation_valid` — `"nettools:1.0,debug:2.1"` → `[("nettools","1.0"),("debug","2.1")]`
- `parse_module_annotation_empty` — `""` → `[]`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p cfgd-operator -- mutate_pod`
Expected: FAIL

- [ ] **Step 3: Implement parse_module_annotations**

```rust
fn parse_module_annotations(annotations: &BTreeMap<String, String>) -> Vec<(String, String)> {
    // Parse "cfgd.io/modules" annotation value: "name:version,name:version"
    // Returns Vec of (name, version) tuples
}
```

- [ ] **Step 4: Implement pod mutation logic**

```rust
async fn mutate_pod(
    client: &Client,
    pod: &k8s_openapi::api::core::v1::Pod,
    namespace: &str,
) -> Result<Vec<serde_json::Value>, OperatorError> {
    // 1. Parse cfgd.io/modules annotation
    // 2. Lookup ConfigPolicy in pod's namespace for requiredModules
    // 3. Lookup ALL ClusterConfigPolicy CRDs, filter by namespaceSelector
    //    matching pod's namespace, collect their requiredModules
    // 4. Merge annotation modules + ConfigPolicy required + ClusterConfigPolicy
    //    required (deduplicate by name, annotation takes precedence for version)
    // 5. For each module: lookup Module CRD (cluster-scoped), get ociArtifact
    // 6. Build JSON patch operations:
    //    a. Add CSI volume per module
    //    b. Add volumeMount per container per module
    //    c. Add/extend env vars per container per module
    //    d. If scripts.postApply: add init container + shared emptyDir
    // Return JSON patch array
}
```

- [ ] **Step 5: Implement /mutate-pods handler**

```rust
async fn handle_mutate_pods(
    axum::extract::State(state): axum::extract::State<WebhookState>,
    Json(review): Json<AdmissionReview<k8s_openapi::api::core::v1::Pod>>,
) -> Json<AdmissionReview<k8s_openapi::api::core::v1::Pod>> {
    // Extract pod from review
    // Call mutate_pod()
    // Build AdmissionResponse with JSON patch
    // Emit ModuleInjected/ModuleInjectionFailed events
}
```

- [ ] **Step 6: Wire /mutate-pods route in webhook router and main.rs**

Add `.route("/mutate-pods", post(handle_mutate_pods))` to the webhook Axum router.

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo test -p cfgd-operator -- mutate_pod`
Expected: PASS

- [ ] **Step 8: Run full test suite**

Run: `cargo test --workspace`
Expected: all PASS

- [ ] **Step 9: Commit**

```
git add crates/cfgd-operator/src/webhook.rs crates/cfgd-operator/src/main.rs
git commit -m "feat(operator): pod module mutating webhook — /mutate-pods endpoint"
```

---

### Task 8: Mutating webhook — Helm template and events

**Files:**
- Create: `chart/cfgd/templates/mutating-webhook-config.yaml`
- Modify: `chart/cfgd/values.yaml`

- [ ] **Step 1: Add mutatingWebhook values**

```yaml
mutatingWebhook:
  enabled: true
  failurePolicy: Ignore
  timeoutSeconds: 10
  namespaceSelector:
    matchExpressions:
      - key: cfgd.io/inject-modules
        operator: In
        values: ["true"]
```

- [ ] **Step 2: Create mutating-webhook-config.yaml**

```yaml
{{- if .Values.mutatingWebhook.enabled }}
apiVersion: admissionregistration.k8s.io/v1
kind: MutatingWebhookConfiguration
metadata:
  name: {{ include "cfgd.fullname" . }}-pod-injector
  {{- if .Values.webhook.certManager.enabled }}
  annotations:
    cert-manager.io/inject-ca-from: {{ .Release.Namespace }}/{{ include "cfgd.webhookCertSecret" . }}
  {{- end }}
webhooks:
  - name: inject-modules.cfgd.io
    admissionReviewVersions: ["v1"]
    sideEffects: None
    failurePolicy: {{ .Values.mutatingWebhook.failurePolicy }}
    reinvocationPolicy: IfNeeded
    timeoutSeconds: {{ .Values.mutatingWebhook.timeoutSeconds }}
    objectSelector:
      matchExpressions:
        - key: cfgd.io/skip-injection
          operator: DoesNotExist
    namespaceSelector:
      {{- toYaml .Values.mutatingWebhook.namespaceSelector | nindent 6 }}
    clientConfig:
      service:
        name: {{ include "cfgd.fullname" . }}-webhook
        namespace: {{ .Release.Namespace }}
        path: /mutate-pods
    rules:
      - apiGroups: [""]
        apiVersions: ["v1"]
        operations: ["CREATE"]
        resources: ["pods"]
{{- end }}
```

- [ ] **Step 3: Commit**

```
git add chart/cfgd/templates/mutating-webhook-config.yaml chart/cfgd/values.yaml
git commit -m "feat(helm): MutatingWebhookConfiguration for pod module injection"
```

---

### Task 9: kubectl cfgd plugin — argv[0] detection and command routing

**Files:**
- Modify: `crates/cfgd/src/main.rs`
- Modify: `crates/cfgd/src/cli/mod.rs`
- Create: `crates/cfgd/src/cli/plugin.rs`

- [ ] **Step 1: Define PluginCommand enum in cli/mod.rs**

```rust
#[derive(Subcommand)]
pub enum PluginCommand {
    /// Create an ephemeral debug container with cfgd modules
    Debug {
        /// Pod name
        pod: String,
        /// Module(s) to inject (format: name:version, repeatable)
        #[arg(long, short)]
        module: Vec<String>,
        /// Namespace
        #[arg(long, short, default_value = "default")]
        namespace: String,
        /// Container image for ephemeral container
        #[arg(long, default_value = "ubuntu:22.04")]
        image: String,
    },
    /// Execute a command in a pod with module environment
    Exec {
        /// Pod name
        pod: String,
        /// Module(s) to load
        #[arg(long, short)]
        module: Vec<String>,
        /// Namespace
        #[arg(long, short, default_value = "default")]
        namespace: String,
        /// Command to execute (after --)
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Inject modules into a running pod
    Inject {
        /// Pod name
        pod: String,
        /// Module(s) to inject
        #[arg(long, short)]
        module: Vec<String>,
        /// Namespace
        #[arg(long, short, default_value = "default")]
        namespace: String,
    },
    /// Show fleet module status
    Status,
    /// Show client and server version
    Version,
}
```

- [ ] **Step 2: Add argv[0] detection at the top of existing main.rs**

Insert at the very beginning of `fn main()`, before alias expansion and existing CLI parsing:

```rust
// kubectl plugin detection — must be before normal CLI parsing
let argv0 = std::env::args().next().unwrap_or_default();
if argv0.ends_with("kubectl-cfgd") || argv0.ends_with("kubectl-cfgd.exe") {
    return cli::plugin::plugin_main();
}
// ... existing cfgd CLI code continues unchanged
```

In `cli/plugin.rs`, define `plugin_main()`:
```rust
pub fn plugin_main() -> anyhow::Result<()> {
    let cli = PluginCli::parse();
    let printer = crate::output::Printer::new(/* ... */);
    // dispatch to plugin commands
}
```

- [ ] **Step 3: Create cli/plugin.rs with command implementations**

Implement each subcommand:
- `debug`: Create ephemeral container with CSI volumes via `kube::Api<Pod>.replace_ephemeral_containers()`
- `exec`: Lookup module env vars, exec into container with modified env
- `inject`: Patch pod spec to add CSI volumes/volumeMounts
- `status`: List Module CRDs, show fleet overview
- `version`: Print cfgd version + operator version (from cluster)

- [ ] **Step 4: Write tests**

Test cases:
- `parse_module_flag` — `"nettools:1.0"` → `("nettools", "1.0")`
- `parse_module_flag_no_version` — `"nettools"` → error (version required)
- `build_csi_volume_for_module` — generates correct CSI volume spec
- `build_ephemeral_container` — generates container with volumes, PATH, PS1

- [ ] **Step 5: Run tests**

Run: `cargo test -p cfgd -- plugin`
Expected: PASS

- [ ] **Step 6: Commit**

```
git add crates/cfgd/src/main.rs crates/cfgd/src/cli/mod.rs crates/cfgd/src/cli/plugin.rs
git commit -m "feat(cli): kubectl cfgd plugin — debug, exec, inject, status, version"
```

---

### Task 10: Krew manifest and PLAN.md update

**Files:**
- Create: `manifests/krew/cfgd.yaml`
- Modify: `.claude/PLAN.md`
- Modify: `.claude/COMPLETED.md`

- [ ] **Step 1: Create Krew manifest**

```yaml
apiVersion: krew.googlecontainertools.github.com/v1alpha2
kind: Plugin
metadata:
  name: cfgd
spec:
  version: "v0.1.0"
  shortDescription: "Manage cfgd modules on pods"
  homepage: https://github.com/tj-smith47/cfgd
  platforms:
    - selector:
        matchLabels:
          os: linux
          arch: amd64
      uri: https://github.com/tj-smith47/cfgd/releases/download/v0.1.0/kubectl-cfgd_linux_amd64.tar.gz
      sha256: "TBD"
      bin: kubectl-cfgd
    - selector:
        matchLabels:
          os: linux
          arch: arm64
      uri: https://github.com/tj-smith47/cfgd/releases/download/v0.1.0/kubectl-cfgd_linux_arm64.tar.gz
      sha256: "TBD"
      bin: kubectl-cfgd
    - selector:
        matchLabels:
          os: darwin
          arch: amd64
      uri: https://github.com/tj-smith47/cfgd/releases/download/v0.1.0/kubectl-cfgd_darwin_amd64.tar.gz
      sha256: "TBD"
      bin: kubectl-cfgd
    - selector:
        matchLabels:
          os: darwin
          arch: arm64
      uri: https://github.com/tj-smith47/cfgd/releases/download/v0.1.0/kubectl-cfgd_darwin_arm64.tar.gz
      sha256: "TBD"
      bin: kubectl-cfgd
```

- [ ] **Step 2: Update PLAN.md — check off all Tier 4 items**

- [ ] **Step 3: Update COMPLETED.md — move Tier 4 to completed**

- [ ] **Step 4: Run full test suite and audit**

Run: `cargo test --workspace && bash .claude/scripts/audit.sh && bash .claude/scripts/completeness-check.sh`
Expected: all pass, 0 errors, 0 warnings

- [ ] **Step 5: Commit**

```
git add manifests/krew/cfgd.yaml .claude/PLAN.md .claude/COMPLETED.md
git commit -m "docs: Krew manifest, move Tier 4 to COMPLETED.md"
```
