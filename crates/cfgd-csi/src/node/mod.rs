#![allow(clippy::result_large_err)] // tonic::Status is inherently 176 bytes

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::cache::Cache;
use crate::csi::v1::node_server::Node;
use crate::csi::v1::{
    NodeExpandVolumeRequest, NodeExpandVolumeResponse, NodeGetCapabilitiesRequest,
    NodeGetCapabilitiesResponse, NodeGetInfoRequest, NodeGetInfoResponse,
    NodeGetVolumeStatsRequest, NodeGetVolumeStatsResponse, NodePublishVolumeRequest,
    NodePublishVolumeResponse, NodeServiceCapability, NodeStageVolumeRequest,
    NodeStageVolumeResponse, NodeUnpublishVolumeRequest, NodeUnpublishVolumeResponse,
    NodeUnstageVolumeRequest, NodeUnstageVolumeResponse, VolumeUsage, node_service_capability,
    volume_usage,
};
use crate::metrics::{CsiMetrics, ModuleLabels, PublishLabels, PullLabels};

/// Env var holding the registry allow-list for CSI module pulls.
/// Comma-separated list of host[:port] entries; `*` disables the check.
/// Unset leaves the check disabled but emits a startup warning.
pub const ALLOWED_REGISTRIES_ENV: &str = "CFGD_CSI_ALLOWED_REGISTRIES";

pub struct CfgdNode {
    cache: Arc<Cache>,
    metrics: Arc<CsiMetrics>,
    node_id: String,
    // `None` means "no allow-list configured" (log a startup warn). `Some(empty)`
    // means "allow-list explicitly empty — refuse every ref" (not a normal
    // deploy, but useful for fail-closed dev). `Some(non-empty)` means the ref
    // host must match one of the entries.
    allowed_registries: Option<Vec<String>>,
}

impl CfgdNode {
    pub fn new(cache: Arc<Cache>, metrics: Arc<CsiMetrics>, node_id: String) -> Self {
        let allowed_registries = parse_allowed_registries_from_env();
        match &allowed_registries {
            None => tracing::warn!(
                env = ALLOWED_REGISTRIES_ENV,
                "CSI registry allow-list is not configured — accepting any ociRef from volume context. In multi-tenant clusters set this env (comma-separated host[:port]) to restrict pulls."
            ),
            Some(list) if list.is_empty() => tracing::warn!(
                env = ALLOWED_REGISTRIES_ENV,
                "CSI registry allow-list is explicitly empty — all module pulls will be refused."
            ),
            Some(list) => tracing::info!(
                env = ALLOWED_REGISTRIES_ENV,
                count = list.len(),
                "CSI registry allow-list active"
            ),
        }
        Self {
            cache,
            metrics,
            node_id,
            allowed_registries,
        }
    }
}

fn parse_allowed_registries_from_env() -> Option<Vec<String>> {
    let raw = std::env::var(ALLOWED_REGISTRIES_ENV).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed == "*" {
        // Wildcard = explicit "any registry"; keep it distinct from "unset"
        // for log clarity but collapse to no-check here.
        return None;
    }
    Some(
        trimmed
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// Extract the registry host[:port] from a full `oci_ref` like
/// `ghcr.io/org/mod:tag` or `myregistry.local:5000/org/mod:tag`. Refs without
/// an explicit registry (e.g. `cfgd-modules/foo:v1`) return the empty string.
fn registry_of(oci_ref: &str) -> &str {
    // An OCI reference has a registry iff its first path segment contains
    // `.`, `:`, or equals `localhost`.
    let first_slash = oci_ref.find('/').unwrap_or(oci_ref.len());
    let head = &oci_ref[..first_slash];
    if head.contains('.') || head.contains(':') || head == "localhost" {
        head
    } else {
        ""
    }
}

fn require_attr<'a>(attrs: &'a HashMap<String, String>, key: &str) -> Result<&'a str, Status> {
    attrs
        .get(key)
        .map(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            Status::invalid_argument(format!("missing required volume attribute: {key}"))
        })
}

fn require_volume_id(volume_id: &str) -> Result<(), Status> {
    if volume_id.is_empty() {
        return Err(Status::invalid_argument("volume_id is required"));
    }
    Ok(())
}

fn resolve_oci_ref(attrs: &HashMap<String, String>, module: &str, version: &str) -> String {
    attrs
        .get("ociRef")
        .filter(|v| !v.is_empty())
        .cloned()
        .unwrap_or_else(|| format!("cfgd-modules/{module}:{version}"))
}

/// Check whether `oci_ref` is permitted under `allowed_registries`.
/// `None` (unset) → allow (legacy behavior, with startup warn already emitted).
/// `Some(list)` → the registry host of the ref must be in `list`. Refs with no
/// explicit registry (default module namespace) are treated as the reserved
/// `"cfgd-modules"` namespace and are always allowed — those can't reach the
/// network without a caller-provided registry anyway.
fn check_registry_allowed(
    oci_ref: &str,
    allowed_registries: Option<&[String]>,
) -> Result<(), Status> {
    let Some(list) = allowed_registries else {
        return Ok(());
    };
    let registry = registry_of(oci_ref);
    if registry.is_empty() {
        return Ok(());
    }
    if list.iter().any(|r| r == registry) {
        return Ok(());
    }
    Err(Status::permission_denied(format!(
        "registry '{registry}' is not in the CSI allow-list (set {ALLOWED_REGISTRIES_ENV})"
    )))
}

#[tonic::async_trait]
impl Node for CfgdNode {
    async fn node_stage_volume(
        &self,
        request: Request<NodeStageVolumeRequest>,
    ) -> Result<Response<NodeStageVolumeResponse>, Status> {
        let req = request.into_inner();
        require_volume_id(&req.volume_id)?;
        if req.staging_target_path.is_empty() {
            return Err(Status::invalid_argument("staging_target_path is required"));
        }

        let attrs = &req.volume_context;
        let module = require_attr(attrs, "module")?;
        let version = require_attr(attrs, "version")?;
        let oci_ref = resolve_oci_ref(attrs, module, version);
        check_registry_allowed(&oci_ref, self.allowed_registries.as_deref())?;

        tracing::info!(
            module = module,
            version = version,
            volume_id = req.volume_id,
            "staging volume — pulling to cache"
        );

        let start = std::time::Instant::now();
        let cached = self.cache.get(module, version).is_some();
        self.cache
            .get_or_pull(module, version, &oci_ref)
            .map_err(|e| Status::internal(format!("cache pull failed: {e}")))?;

        let duration = start.elapsed().as_secs_f64();
        self.metrics
            .pull_duration_seconds
            .get_or_create(&PullLabels {
                module: module.to_string(),
                cached: cached.to_string(),
            })
            .observe(duration);

        if cached {
            self.metrics
                .cache_hits_total
                .get_or_create(&ModuleLabels {
                    module: module.to_string(),
                })
                .inc();
        }

        self.metrics
            .cache_size_bytes
            .set(self.cache.current_size_bytes() as i64);

        Ok(Response::new(NodeStageVolumeResponse {}))
    }

    async fn node_unstage_volume(
        &self,
        request: Request<NodeUnstageVolumeRequest>,
    ) -> Result<Response<NodeUnstageVolumeResponse>, Status> {
        let req = request.into_inner();
        require_volume_id(&req.volume_id)?;
        tracing::debug!(
            volume_id = req.volume_id,
            "unstage volume (no-op, cache persists)"
        );
        Ok(Response::new(NodeUnstageVolumeResponse {}))
    }

    async fn node_publish_volume(
        &self,
        request: Request<NodePublishVolumeRequest>,
    ) -> Result<Response<NodePublishVolumeResponse>, Status> {
        let req = request.into_inner();
        require_volume_id(&req.volume_id)?;

        let attrs = &req.volume_context;
        let module = require_attr(attrs, "module")?;
        let version = require_attr(attrs, "version")?;
        let target_path = &req.target_path;

        if target_path.is_empty() {
            return Err(Status::invalid_argument("target_path is required"));
        }

        let target = Path::new(target_path);

        // Idempotent: if already mounted as read-only, return success
        if is_mountpoint(target) {
            if is_readonly_mount(target) {
                tracing::debug!(
                    target = target_path,
                    "already mounted read-only, returning success"
                );
                return Ok(Response::new(NodePublishVolumeResponse {}));
            }
            // Mounted but not read-only — attempt remount
            tracing::warn!(
                target = target_path,
                "mount exists but is not read-only, attempting remount"
            );
            #[cfg(target_os = "linux")]
            {
                use nix::mount::{MsFlags, mount};
                mount(
                    None::<&str>,
                    target,
                    None::<&str>,
                    MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY,
                    None::<&str>,
                )
                .map_err(|e| Status::internal(format!("read-only remount failed: {e}")))?;
            }
            return Ok(Response::new(NodePublishVolumeResponse {}));
        }

        tracing::info!(
            module = module,
            version = version,
            target = target_path,
            volume_id = req.volume_id,
            "publishing volume"
        );

        // Get cached content (should have been staged already, but pull if needed)
        let oci_ref = resolve_oci_ref(attrs, module, version);
        check_registry_allowed(&oci_ref, self.allowed_registries.as_deref())?;

        // OCI pull, `std::fs::create_dir_all`, and the `bind_mount` syscall are
        // all blocking. Running them inline on the tokio runtime starves other
        // worker threads under kubelet concurrency. Move the whole blocking
        // sequence into `spawn_blocking` with owned clones of the relevant state.
        let cache = Arc::clone(&self.cache);
        let metrics = Arc::clone(&self.metrics);
        let module = module.to_string();
        let version = version.to_string();
        let oci_ref_owned = oci_ref.clone();
        let target_path_owned: std::path::PathBuf = target.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let source = cache
                .get_or_pull(&module, &version, &oci_ref_owned)
                .map_err(|e| Status::internal(format!("cache pull failed: {e}")))?;

            std::fs::create_dir_all(&target_path_owned)
                .map_err(|e| Status::internal(format!("cannot create target dir: {e}")))?;

            match bind_mount_readonly(&source, &target_path_owned) {
                Ok(()) => {
                    metrics
                        .volume_publish_total
                        .get_or_create(&PublishLabels {
                            module: module.clone(),
                            result: "success".to_string(),
                        })
                        .inc();
                    Ok(())
                }
                Err(e) => {
                    metrics
                        .volume_publish_total
                        .get_or_create(&PublishLabels {
                            module: module.clone(),
                            result: "error".to_string(),
                        })
                        .inc();
                    let _ = std::fs::remove_dir(&target_path_owned);
                    Err(e)
                }
            }
        })
        .await
        .map_err(|e| Status::internal(format!("publish task join failed: {e}")))??;

        Ok(Response::new(NodePublishVolumeResponse {}))
    }

    async fn node_unpublish_volume(
        &self,
        request: Request<NodeUnpublishVolumeRequest>,
    ) -> Result<Response<NodeUnpublishVolumeResponse>, Status> {
        let req = request.into_inner();
        require_volume_id(&req.volume_id)?;

        let target_path = &req.target_path;
        if target_path.is_empty() {
            return Err(Status::invalid_argument("target_path is required"));
        }

        tracing::info!(
            target = target_path,
            volume_id = req.volume_id,
            "unpublishing volume"
        );

        // `unmount` (umount2 syscall) and `remove_dir` both block — run them
        // off the tokio runtime so kubelet-driven concurrency cannot starve
        // other csi workers.
        let target_path_owned: std::path::PathBuf = target_path.into();
        tokio::task::spawn_blocking(move || -> Result<(), Status> {
            unmount(&target_path_owned)?;
            if let Err(e) = std::fs::remove_dir(&target_path_owned) {
                tracing::warn!(target = %target_path_owned.display(), error = %e, "failed to remove target directory after unmount");
            }
            Ok(())
        })
        .await
        .map_err(|e| Status::internal(format!("unpublish task join failed: {e}")))??;

        Ok(Response::new(NodeUnpublishVolumeResponse {}))
    }

    async fn node_get_volume_stats(
        &self,
        request: Request<NodeGetVolumeStatsRequest>,
    ) -> Result<Response<NodeGetVolumeStatsResponse>, Status> {
        let req = request.into_inner();
        require_volume_id(&req.volume_id)?;

        let volume_path = &req.volume_path;
        if volume_path.is_empty() {
            return Err(Status::invalid_argument("volume_path is required"));
        }

        let path = Path::new(volume_path);
        if !path.exists() {
            return Err(Status::not_found(format!(
                "volume path does not exist: {volume_path}"
            )));
        }

        let (bytes, inodes) = walk_volume_stats(path);

        tracing::debug!(
            volume_id = req.volume_id,
            volume_path = volume_path,
            bytes = bytes,
            inodes = inodes,
            "volume stats"
        );

        Ok(Response::new(NodeGetVolumeStatsResponse {
            usage: vec![
                VolumeUsage {
                    total: bytes as i64,
                    used: bytes as i64,
                    available: 0,
                    unit: volume_usage::Unit::Bytes as i32,
                },
                VolumeUsage {
                    total: inodes as i64,
                    used: inodes as i64,
                    available: 0,
                    unit: volume_usage::Unit::Inodes as i32,
                },
            ],
            volume_condition: None,
        }))
    }

    async fn node_expand_volume(
        &self,
        _request: Request<NodeExpandVolumeRequest>,
    ) -> Result<Response<NodeExpandVolumeResponse>, Status> {
        Err(Status::unimplemented("NodeExpandVolume not supported"))
    }

    async fn node_get_capabilities(
        &self,
        _request: Request<NodeGetCapabilitiesRequest>,
    ) -> Result<Response<NodeGetCapabilitiesResponse>, Status> {
        tracing::debug!("NodeGetCapabilities called");
        Ok(Response::new(NodeGetCapabilitiesResponse {
            capabilities: vec![
                NodeServiceCapability {
                    r#type: Some(node_service_capability::Type::Rpc(
                        node_service_capability::Rpc {
                            r#type: node_service_capability::rpc::Type::StageUnstageVolume.into(),
                        },
                    )),
                },
                NodeServiceCapability {
                    r#type: Some(node_service_capability::Type::Rpc(
                        node_service_capability::Rpc {
                            r#type: node_service_capability::rpc::Type::GetVolumeStats.into(),
                        },
                    )),
                },
            ],
        }))
    }

    async fn node_get_info(
        &self,
        _request: Request<NodeGetInfoRequest>,
    ) -> Result<Response<NodeGetInfoResponse>, Status> {
        tracing::debug!(node_id = %self.node_id, "NodeGetInfo called");
        Ok(Response::new(NodeGetInfoResponse {
            node_id: self.node_id.clone(),
            max_volumes_per_node: 0, // no limit
            accessible_topology: None,
        }))
    }
}

/// Check if a path is a mountpoint by comparing device IDs with parent.
fn is_mountpoint(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(path_meta) = std::fs::metadata(path) else {
            return false;
        };
        let Some(parent) = path.parent() else {
            return true; // root is always a mountpoint
        };
        let Ok(parent_meta) = std::fs::metadata(parent) else {
            return false;
        };
        path_meta.dev() != parent_meta.dev()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

/// Check if a path is mounted read-only using the statvfs syscall.
#[cfg(target_os = "linux")]
fn is_readonly_mount(path: &Path) -> bool {
    use nix::sys::statvfs::{FsFlags, statvfs};
    statvfs(path)
        .map(|stat| stat.flags().contains(FsFlags::ST_RDONLY))
        .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn is_readonly_mount(_path: &Path) -> bool {
    false
}

/// Bind mount `source` to `target` as read-only.
///
/// Two-step operation on Linux: bind mount, then remount read-only.
/// (MS_BIND | MS_RDONLY in a single call does NOT work.)
#[cfg(target_os = "linux")]
fn bind_mount_readonly(source: &Path, target: &Path) -> Result<(), Status> {
    use nix::mount::{MsFlags, mount};

    mount(
        Some(source),
        target,
        None::<&str>,
        MsFlags::MS_BIND,
        None::<&str>,
    )
    .map_err(|e| Status::internal(format!("bind mount failed: {e}")))?;

    if let Err(e) = mount(
        None::<&str>,
        target,
        None::<&str>,
        MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY,
        None::<&str>,
    ) {
        // Clean up the bind mount if remount fails
        let _ = nix::mount::umount2(target, nix::mount::MntFlags::MNT_DETACH);
        return Err(Status::internal(format!("read-only remount failed: {e}")));
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn bind_mount_readonly(_source: &Path, _target: &Path) -> Result<(), Status> {
    Err(Status::unimplemented("bind mount only supported on Linux"))
}

/// Unmount a target path with MNT_DETACH (lazy unmount).
#[cfg(target_os = "linux")]
fn unmount(target: &Path) -> Result<(), Status> {
    use nix::mount::{MntFlags, umount2};

    match umount2(target, MntFlags::MNT_DETACH) {
        Ok(()) => Ok(()),
        Err(nix::errno::Errno::EINVAL)
        | Err(nix::errno::Errno::ENOENT)
        | Err(nix::errno::Errno::EPERM) => {
            // EINVAL = not mounted, ENOENT = doesn't exist, EPERM = not a mount point
            // (non-root gets EPERM instead of EINVAL) — all idempotent success
            Ok(())
        }
        Err(e) => Err(Status::internal(format!("unmount failed: {e}"))),
    }
}

#[cfg(not(target_os = "linux"))]
fn unmount(_target: &Path) -> Result<(), Status> {
    // Non-linux stub for compilation — CSI driver only runs on Linux
    Ok(())
}

/// Recursively walk a directory, returning (total_bytes, total_inodes).
/// Uses symlink_metadata to avoid following symlinks (prevents infinite loops).
fn walk_volume_stats(path: &Path) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut inodes = 0u64;

    fn walk(path: &Path, bytes: &mut u64, inodes: &mut u64) {
        let entries = match std::fs::read_dir(path) {
            Ok(rd) => rd,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            *inodes += 1;
            let p = entry.path();
            let Ok(meta) = p.symlink_metadata() else {
                continue;
            };
            if meta.is_symlink() {
                // Count the symlink inode but don't follow it
                *bytes = bytes.saturating_add(meta.len());
            } else if meta.is_dir() {
                walk(&p, bytes, inodes);
            } else {
                *bytes = bytes.saturating_add(meta.len());
            }
        }
    }

    // Count the root directory itself
    inodes += 1;
    walk(path, &mut bytes, &mut inodes);
    (bytes, inodes)
}

#[cfg(test)]
mod tests;
