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
    NodeUnstageVolumeRequest, NodeUnstageVolumeResponse, node_service_capability,
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

        // Idempotent: if already mounted, return success
        if is_mountpoint(target) {
            tracing::debug!(target = target_path, "already mounted, returning success");
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
        let _ = std::fs::remove_dir(target);

        Ok(Response::new(NodeUnpublishVolumeResponse {}))
    }

    async fn node_get_volume_stats(
        &self,
        _request: Request<NodeGetVolumeStatsRequest>,
    ) -> Result<Response<NodeGetVolumeStatsResponse>, Status> {
        Err(Status::unimplemented("NodeGetVolumeStats not supported"))
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
            capabilities: vec![NodeServiceCapability {
                r#type: Some(node_service_capability::Type::Rpc(
                    node_service_capability::Rpc {
                        r#type: node_service_capability::rpc::Type::StageUnstageVolume.into(),
                    },
                )),
            }],
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
        let Ok(parent_meta) = std::fs::metadata(path.join("..")) else {
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

    mount(
        None::<&str>,
        target,
        None::<&str>,
        MsFlags::MS_REMOUNT | MsFlags::MS_BIND | MsFlags::MS_RDONLY,
        None::<&str>,
    )
    .map_err(|e| Status::internal(format!("read-only remount failed: {e}")))?;

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
    async fn node_get_capabilities_returns_stage_unstage() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let resp = node
            .node_get_capabilities(Request::new(NodeGetCapabilitiesRequest {}))
            .await
            .unwrap();
        let caps = resp.into_inner().capabilities;
        assert_eq!(caps.len(), 1);
        match &caps[0].r#type {
            Some(node_service_capability::Type::Rpc(rpc)) => {
                assert_eq!(
                    rpc.r#type,
                    node_service_capability::rpc::Type::StageUnstageVolume as i32
                );
            }
            other => panic!("unexpected capability type: {other:?}"),
        }
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
    async fn node_get_volume_stats_unimplemented() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeGetVolumeStatsRequest {
            volume_id: "vol-1".to_string(),
            ..Default::default()
        };
        let err = node
            .node_get_volume_stats(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::Unimplemented);
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
}
