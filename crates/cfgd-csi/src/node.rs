use std::collections::HashMap;
use std::sync::Arc;

use tonic::{Request, Response, Status};

use crate::cache::Cache;
use crate::csi::v1::node_server::Node;
use crate::csi::v1::{
    NodeExpandVolumeRequest, NodeExpandVolumeResponse, NodeGetCapabilitiesRequest,
    NodeGetCapabilitiesResponse, NodeGetInfoRequest, NodeGetInfoResponse,
    NodeGetVolumeStatsRequest, NodeGetVolumeStatsResponse, NodePublishVolumeRequest,
    NodePublishVolumeResponse, NodeStageVolumeRequest, NodeStageVolumeResponse,
    NodeUnpublishVolumeRequest, NodeUnpublishVolumeResponse, NodeUnstageVolumeRequest,
    NodeUnstageVolumeResponse, NodeServiceCapability, node_service_capability,
};

pub struct CfgdNode {
    cache: Arc<Cache>,
    node_id: String,
}

impl CfgdNode {
    pub fn new(cache: Arc<Cache>, node_id: String) -> Self {
        Self { cache, node_id }
    }
}

#[allow(clippy::result_large_err)] // tonic::Status is inherently large (176 bytes)
fn require_attr<'a>(
    attrs: &'a HashMap<String, String>,
    key: &str,
) -> Result<&'a str, Status> {
    attrs
        .get(key)
        .map(|v| v.as_str())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            Status::invalid_argument(format!("missing required volume attribute: {key}"))
        })
}

#[tonic::async_trait]
impl Node for CfgdNode {
    async fn node_stage_volume(
        &self,
        request: Request<NodeStageVolumeRequest>,
    ) -> Result<Response<NodeStageVolumeResponse>, Status> {
        let req = request.into_inner();
        let attrs = &req.volume_context;
        let module = require_attr(attrs, "module")?;
        let version = require_attr(attrs, "version")?;

        let oci_ref = attrs.get("ociRef").cloned().unwrap_or_else(|| {
            format!("cfgd-modules/{module}:{version}")
        });

        tracing::info!(
            module = module,
            version = version,
            volume_id = req.volume_id,
            "staging volume — pulling to cache"
        );

        self.cache
            .get_or_pull(module, version, &oci_ref)
            .map_err(|e| Status::internal(format!("cache pull failed: {e}")))?;

        Ok(Response::new(NodeStageVolumeResponse {}))
    }

    async fn node_unstage_volume(
        &self,
        request: Request<NodeUnstageVolumeRequest>,
    ) -> Result<Response<NodeUnstageVolumeResponse>, Status> {
        let req = request.into_inner();
        tracing::debug!(volume_id = req.volume_id, "unstage volume (no-op, cache persists)");
        Ok(Response::new(NodeUnstageVolumeResponse {}))
    }

    async fn node_publish_volume(
        &self,
        request: Request<NodePublishVolumeRequest>,
    ) -> Result<Response<NodePublishVolumeResponse>, Status> {
        let req = request.into_inner();
        let attrs = &req.volume_context;
        let module = require_attr(attrs, "module")?;
        let version = require_attr(attrs, "version")?;
        let target_path = &req.target_path;

        if target_path.is_empty() {
            return Err(Status::invalid_argument("target_path is required"));
        }

        tracing::info!(
            module = module,
            version = version,
            target = target_path,
            volume_id = req.volume_id,
            "publishing volume"
        );

        // Get cached content (should have been staged already, but pull if needed)
        let oci_ref = attrs.get("ociRef").cloned().unwrap_or_else(|| {
            format!("cfgd-modules/{module}:{version}")
        });
        let source = self
            .cache
            .get_or_pull(module, version, &oci_ref)
            .map_err(|e| Status::internal(format!("cache pull failed: {e}")))?;

        // Create target directory
        std::fs::create_dir_all(target_path)
            .map_err(|e| Status::internal(format!("cannot create target dir: {e}")))?;

        // Bind mount source to target (read-only)
        bind_mount_readonly(&source, std::path::Path::new(target_path))?;

        Ok(Response::new(NodePublishVolumeResponse {}))
    }

    async fn node_unpublish_volume(
        &self,
        request: Request<NodeUnpublishVolumeRequest>,
    ) -> Result<Response<NodeUnpublishVolumeResponse>, Status> {
        let req = request.into_inner();
        let target_path = &req.target_path;

        if target_path.is_empty() {
            return Err(Status::invalid_argument("target_path is required"));
        }

        tracing::info!(
            target = target_path,
            volume_id = req.volume_id,
            "unpublishing volume"
        );

        unmount(std::path::Path::new(target_path))?;

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

/// Bind mount `source` to `target` as read-only.
///
/// Two-step operation on Linux: bind mount, then remount read-only.
/// (MS_BIND | MS_RDONLY in a single call does NOT work.)
#[cfg(target_os = "linux")]
#[allow(clippy::result_large_err)]
fn bind_mount_readonly(
    source: &std::path::Path,
    target: &std::path::Path,
) -> Result<(), Status> {
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
#[allow(clippy::result_large_err)]
fn bind_mount_readonly(
    _source: &std::path::Path,
    _target: &std::path::Path,
) -> Result<(), Status> {
    Err(Status::unimplemented(
        "bind mount only supported on Linux",
    ))
}

/// Unmount a target path.
#[cfg(target_os = "linux")]
#[allow(clippy::result_large_err)]
fn unmount(target: &std::path::Path) -> Result<(), Status> {
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
fn unmount(_target: &std::path::Path) -> Result<(), Status> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_cache(dir: &std::path::Path) -> Arc<Cache> {
        Arc::new(Cache::new(dir.to_path_buf(), 1024 * 1024).unwrap())
    }

    fn test_node(cache: Arc<Cache>) -> CfgdNode {
        CfgdNode::new(cache, "test-node".to_string())
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
    async fn node_stage_volume_missing_module_attr() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeStageVolumeRequest {
            volume_id: "vol-1".to_string(),
            volume_context: HashMap::new(),
            ..Default::default()
        };
        let err = node
            .node_stage_volume(Request::new(req))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn node_unstage_volume_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(test_cache(dir.path()));
        let req = NodeUnstageVolumeRequest {
            volume_id: "vol-1".to_string(),
            ..Default::default()
        };
        let resp = node
            .node_unstage_volume(Request::new(req))
            .await;
        assert!(resp.is_ok());
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
}
