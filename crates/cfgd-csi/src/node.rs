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

pub struct CfgdNode {
    cache: Arc<Cache>,
    metrics: Arc<CsiMetrics>,
    node_id: String,
}

impl CfgdNode {
    pub fn new(cache: Arc<Cache>, metrics: Arc<CsiMetrics>, node_id: String) -> Self {
        Self {
            cache,
            metrics,
            node_id,
        }
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
        let source = self
            .cache
            .get_or_pull(module, version, &oci_ref)
            .map_err(|e| Status::internal(format!("cache pull failed: {e}")))?;

        std::fs::create_dir_all(target_path)
            .map_err(|e| Status::internal(format!("cannot create target dir: {e}")))?;

        let result = bind_mount_readonly(&source, target);
        let result_str = if result.is_ok() { "success" } else { "error" };
        self.metrics
            .volume_publish_total
            .get_or_create(&PublishLabels {
                module: module.to_string(),
                result: result_str.to_string(),
            })
            .inc();
        if result.is_err() {
            // Clean up the target directory we created if mount failed
            let _ = std::fs::remove_dir(target);
        }
        result?;

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

        let target = Path::new(target_path);
        unmount(target)?;

        // CSI spec: SP MUST delete the file or directory it created at this path
        if let Err(e) = std::fs::remove_dir(target) {
            tracing::warn!(target = %target.display(), error = %e, "failed to remove target directory after unmount");
        }

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
        Err(nix::errno::Errno::EINVAL) | Err(nix::errno::Errno::ENOENT) => {
            // Not mounted or doesn't exist — idempotent success
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
mod tests {
    use super::*;

    fn test_cache(dir: &std::path::Path) -> Arc<Cache> {
        Arc::new(Cache::new(dir.to_path_buf(), 1024 * 1024).unwrap())
    }

    fn test_node(cache: Arc<Cache>) -> CfgdNode {
        let mut registry = prometheus_client::registry::Registry::default();
        let metrics = Arc::new(CsiMetrics::new(&mut registry));
        CfgdNode::new(cache, metrics, "test-node".to_string())
    }

    #[tokio::test]
    async fn node_get_capabilities_returns_expected() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let resp = node
            .node_get_capabilities(Request::new(NodeGetCapabilitiesRequest {}))
            .await
            .unwrap();
        let caps = resp.into_inner().capabilities;
        assert_eq!(caps.len(), 2);

        let rpc_types: Vec<i32> = caps
            .iter()
            .filter_map(|c| match &c.r#type {
                Some(node_service_capability::Type::Rpc(rpc)) => Some(rpc.r#type),
                _ => None,
            })
            .collect();
        assert!(
            rpc_types.contains(&(node_service_capability::rpc::Type::StageUnstageVolume as i32))
        );
        assert!(rpc_types.contains(&(node_service_capability::rpc::Type::GetVolumeStats as i32)));
    }

    #[tokio::test]
    async fn node_get_info_returns_node_id() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let resp = node
            .node_get_info(Request::new(NodeGetInfoRequest {}))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().node_id, "test-node");
    }

    #[tokio::test]
    async fn node_publish_volume_missing_module_attr() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodePublishVolumeRequest {
            volume_id: "vol-1".to_string(),
            target_path: "/tmp/target".to_string(),
            volume_context: HashMap::new(),
            ..Default::default()
        };
        let err = node
            .node_publish_volume(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("module"));
    }

    #[tokio::test]
    async fn node_publish_volume_missing_version_attr() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodePublishVolumeRequest {
            volume_id: "vol-1".to_string(),
            target_path: "/tmp/target".to_string(),
            volume_context: [("module".to_string(), "nettools".to_string())]
                .into_iter()
                .collect(),
            ..Default::default()
        };
        let err = node
            .node_publish_volume(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("version"));
    }

    #[tokio::test]
    async fn node_publish_volume_missing_target_path() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodePublishVolumeRequest {
            volume_id: "vol-1".to_string(),
            target_path: String::new(),
            volume_context: [
                ("module".to_string(), "nettools".to_string()),
                ("version".to_string(), "1.0".to_string()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let err = node
            .node_publish_volume(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn node_publish_volume_missing_volume_id() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodePublishVolumeRequest {
            volume_id: String::new(),
            target_path: "/tmp/target".to_string(),
            volume_context: [
                ("module".to_string(), "nettools".to_string()),
                ("version".to_string(), "1.0".to_string()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let err = node
            .node_publish_volume(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("volume_id"));
    }

    #[tokio::test]
    async fn node_stage_volume_missing_module_attr() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeStageVolumeRequest {
            volume_id: "vol-1".to_string(),
            staging_target_path: "/tmp/staging".to_string(),
            volume_context: HashMap::new(),
            ..Default::default()
        };
        let err = node.node_stage_volume(Request::new(req)).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn node_stage_volume_missing_volume_id() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeStageVolumeRequest {
            volume_id: String::new(),
            staging_target_path: "/tmp/staging".to_string(),
            volume_context: HashMap::new(),
            ..Default::default()
        };
        let err = node.node_stage_volume(Request::new(req)).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("volume_id"));
    }

    #[tokio::test]
    async fn node_stage_volume_missing_staging_path() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeStageVolumeRequest {
            volume_id: "vol-1".to_string(),
            staging_target_path: String::new(),
            volume_context: [
                ("module".to_string(), "nettools".to_string()),
                ("version".to_string(), "1.0".to_string()),
            ]
            .into_iter()
            .collect(),
            ..Default::default()
        };
        let err = node.node_stage_volume(Request::new(req)).await.unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("staging_target_path"));
    }

    #[tokio::test]
    async fn node_unstage_volume_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeUnstageVolumeRequest {
            volume_id: "vol-1".to_string(),
            ..Default::default()
        };
        assert!(node.node_unstage_volume(Request::new(req)).await.is_ok());
    }

    #[tokio::test]
    async fn node_unstage_volume_missing_volume_id() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeUnstageVolumeRequest {
            volume_id: String::new(),
            ..Default::default()
        };
        let err = node
            .node_unstage_volume(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn node_unpublish_volume_missing_target_path() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeUnpublishVolumeRequest {
            volume_id: "vol-1".to_string(),
            target_path: String::new(),
        };
        let err = node
            .node_unpublish_volume(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn node_unpublish_volume_missing_volume_id() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeUnpublishVolumeRequest {
            volume_id: String::new(),
            target_path: "/tmp/target".to_string(),
        };
        let err = node
            .node_unpublish_volume(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn node_get_volume_stats_returns_usage() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));

        // Create a fake volume directory with some content
        let vol_dir = dir.path().join("vol-content");
        std::fs::create_dir_all(&vol_dir).unwrap();
        std::fs::write(vol_dir.join("file1.txt"), "hello").unwrap();
        std::fs::write(vol_dir.join("file2.txt"), "world!").unwrap();

        let req = NodeGetVolumeStatsRequest {
            volume_id: "vol-1".to_string(),
            volume_path: vol_dir.to_str().unwrap().to_string(),
            ..Default::default()
        };
        let resp = node.node_get_volume_stats(Request::new(req)).await.unwrap();
        let usage = resp.into_inner().usage;
        assert_eq!(usage.len(), 2);

        // Bytes entry
        let bytes_entry = usage
            .iter()
            .find(|u| u.unit == volume_usage::Unit::Bytes as i32)
            .unwrap();
        assert_eq!(bytes_entry.used, 11); // "hello" (5) + "world!" (6)
        assert_eq!(bytes_entry.total, 11);
        assert_eq!(bytes_entry.available, 0);

        // Inodes entry
        let inodes_entry = usage
            .iter()
            .find(|u| u.unit == volume_usage::Unit::Inodes as i32)
            .unwrap();
        assert_eq!(inodes_entry.used, 3); // root dir + 2 files
        assert_eq!(inodes_entry.total, 3);
    }

    #[tokio::test]
    async fn node_get_volume_stats_missing_volume_id() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeGetVolumeStatsRequest {
            volume_id: String::new(),
            volume_path: "/tmp".to_string(),
            ..Default::default()
        };
        let err = node
            .node_get_volume_stats(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn node_get_volume_stats_missing_volume_path() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeGetVolumeStatsRequest {
            volume_id: "vol-1".to_string(),
            volume_path: String::new(),
            ..Default::default()
        };
        let err = node
            .node_get_volume_stats(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn node_get_volume_stats_nonexistent_path() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeGetVolumeStatsRequest {
            volume_id: "vol-1".to_string(),
            volume_path: "/nonexistent/cfgd-test-path".to_string(),
            ..Default::default()
        };
        let err = node
            .node_get_volume_stats(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn node_expand_volume_unimplemented() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeExpandVolumeRequest {
            volume_id: "vol-1".to_string(),
            ..Default::default()
        };
        let err = node
            .node_expand_volume(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unimplemented);
    }

    #[test]
    fn resolve_oci_ref_uses_provided_value() {
        let attrs: HashMap<String, String> =
            [("ociRef".to_string(), "ghcr.io/myorg/mod:v1".to_string())]
                .into_iter()
                .collect();
        assert_eq!(resolve_oci_ref(&attrs, "mod", "v1"), "ghcr.io/myorg/mod:v1");
    }

    #[test]
    fn resolve_oci_ref_falls_back_to_default() {
        let attrs: HashMap<String, String> = HashMap::new();
        assert_eq!(
            resolve_oci_ref(&attrs, "nettools", "1.0"),
            "cfgd-modules/nettools:1.0"
        );
    }

    #[test]
    fn is_mountpoint_false_for_regular_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_mountpoint(dir.path()));
    }

    #[test]
    fn is_mountpoint_false_for_nonexistent() {
        assert!(!is_mountpoint(Path::new("/nonexistent/path/cfgd-test")));
    }

    #[tokio::test]
    async fn node_unpublish_volume_nonexistent_target_succeeds() {
        // CSI spec: unpublish of a path that doesn't exist should be idempotent success
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let target = dir.path().join("does-not-exist");
        let req = NodeUnpublishVolumeRequest {
            volume_id: "vol-1".to_string(),
            target_path: target.to_str().unwrap().to_string(),
        };
        // Should succeed (unmount returns Ok for ENOENT, remove_dir warns but doesn't fail)
        assert!(node.node_unpublish_volume(Request::new(req)).await.is_ok());
    }

    #[tokio::test]
    async fn node_unpublish_volume_empty_dir_cleans_up() {
        // Unpublish on a non-mounted directory should succeed and remove the dir
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let target = dir.path().join("empty-target");
        std::fs::create_dir_all(&target).unwrap();
        assert!(target.exists());

        let req = NodeUnpublishVolumeRequest {
            volume_id: "vol-1".to_string(),
            target_path: target.to_str().unwrap().to_string(),
        };
        assert!(node.node_unpublish_volume(Request::new(req)).await.is_ok());
        // The directory should be cleaned up after unpublish
        assert!(
            !target.exists(),
            "target dir should be removed after unpublish"
        );
    }

    #[tokio::test]
    async fn node_get_volume_stats_with_subdirectories() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));

        let vol = dir.path().join("nested-vol");
        let subdir = vol.join("subdir");
        std::fs::create_dir_all(&subdir).unwrap();
        std::fs::write(vol.join("root.txt"), "abc").unwrap(); // 3 bytes
        std::fs::write(subdir.join("nested.txt"), "defgh").unwrap(); // 5 bytes

        let req = NodeGetVolumeStatsRequest {
            volume_id: "vol-1".to_string(),
            volume_path: vol.to_str().unwrap().to_string(),
            ..Default::default()
        };
        let resp = node.node_get_volume_stats(Request::new(req)).await.unwrap();
        let usage = resp.into_inner().usage;

        let bytes_entry = usage
            .iter()
            .find(|u| u.unit == volume_usage::Unit::Bytes as i32)
            .unwrap();
        assert_eq!(bytes_entry.used, 8); // 3 + 5

        let inodes_entry = usage
            .iter()
            .find(|u| u.unit == volume_usage::Unit::Inodes as i32)
            .unwrap();
        // root dir (1) + subdir entry in readdir (1) + root.txt (1) + nested.txt (1) + subdir itself walked = total 4
        // Actually: walk counts root(1), then readdir(root): subdir(+1), root.txt(+1), then walk(subdir): nested.txt(+1) = 4
        assert_eq!(inodes_entry.used, 4);
    }

    #[tokio::test]
    async fn node_get_volume_stats_with_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));

        let vol = dir.path().join("symlink-vol");
        std::fs::create_dir_all(&vol).unwrap();
        std::fs::write(vol.join("real.txt"), "data").unwrap(); // 4 bytes

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(vol.join("real.txt"), vol.join("link.txt")).unwrap();

            let req = NodeGetVolumeStatsRequest {
                volume_id: "vol-1".to_string(),
                volume_path: vol.to_str().unwrap().to_string(),
                ..Default::default()
            };
            let resp = node.node_get_volume_stats(Request::new(req)).await.unwrap();
            let usage = resp.into_inner().usage;

            let inodes_entry = usage
                .iter()
                .find(|u| u.unit == volume_usage::Unit::Inodes as i32)
                .unwrap();
            // root dir (1) + real.txt (1) + link.txt symlink (1) = 3
            assert_eq!(inodes_entry.used, 3);

            let bytes_entry = usage
                .iter()
                .find(|u| u.unit == volume_usage::Unit::Bytes as i32)
                .unwrap();
            // real.txt (4) + symlink size (varies by OS, but should include it)
            assert!(bytes_entry.used >= 4, "should include real file bytes");
        }
    }

    #[test]
    fn walk_volume_stats_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let (bytes, inodes) = walk_volume_stats(dir.path());
        assert_eq!(bytes, 0);
        assert_eq!(inodes, 1); // just the root dir
    }

    #[test]
    fn walk_volume_stats_nonexistent() {
        let (bytes, inodes) = walk_volume_stats(Path::new("/nonexistent/cfgd-test-walk"));
        assert_eq!(bytes, 0);
        assert_eq!(inodes, 1); // root counted, walk fails silently
    }

    #[test]
    fn require_volume_id_accepts_nonempty() {
        assert!(require_volume_id("vol-1").is_ok());
    }

    #[test]
    fn require_volume_id_rejects_empty() {
        let err = require_volume_id("").unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(err.message().contains("volume_id"));
    }

    #[test]
    fn require_attr_accepts_present() {
        let attrs: HashMap<String, String> = [("module".to_string(), "nettools".to_string())]
            .into_iter()
            .collect();
        assert_eq!(require_attr(&attrs, "module").unwrap(), "nettools");
    }

    #[test]
    fn require_attr_rejects_missing() {
        let attrs: HashMap<String, String> = HashMap::new();
        let err = require_attr(&attrs, "module").unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn require_attr_rejects_empty_value() {
        let attrs: HashMap<String, String> = [("module".to_string(), String::new())]
            .into_iter()
            .collect();
        let err = require_attr(&attrs, "module").unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn resolve_oci_ref_ignores_empty_override() {
        let attrs: HashMap<String, String> = [("ociRef".to_string(), String::new())]
            .into_iter()
            .collect();
        assert_eq!(
            resolve_oci_ref(&attrs, "nettools", "1.0"),
            "cfgd-modules/nettools:1.0"
        );
    }
}
