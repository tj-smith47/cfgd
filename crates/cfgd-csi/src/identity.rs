use std::path::PathBuf;

use tonic::{Request, Response, Status};

use crate::csi::v1::identity_server::Identity;
use crate::csi::v1::{
    GetPluginCapabilitiesRequest, GetPluginCapabilitiesResponse, GetPluginInfoRequest,
    GetPluginInfoResponse, ProbeRequest, ProbeResponse,
};

pub const DRIVER_NAME: &str = cfgd_core::CSI_DRIVER_NAME;

pub struct CfgdIdentity {
    cache_dir: PathBuf,
}

impl CfgdIdentity {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }
}

#[tonic::async_trait]
impl Identity for CfgdIdentity {
    async fn get_plugin_info(
        &self,
        _request: Request<GetPluginInfoRequest>,
    ) -> Result<Response<GetPluginInfoResponse>, Status> {
        tracing::debug!("GetPluginInfo called");
        Ok(Response::new(GetPluginInfoResponse {
            name: DRIVER_NAME.to_string(),
            vendor_version: env!("CARGO_PKG_VERSION").to_string(),
            manifest: Default::default(),
        }))
    }

    async fn get_plugin_capabilities(
        &self,
        _request: Request<GetPluginCapabilitiesRequest>,
    ) -> Result<Response<GetPluginCapabilitiesResponse>, Status> {
        tracing::debug!("GetPluginCapabilities called");
        // Node-only plugin — no plugin-level capabilities to advertise.
        // Node capabilities (STAGE_UNSTAGE_VOLUME) are reported via NodeGetCapabilities.
        Ok(Response::new(GetPluginCapabilitiesResponse {
            capabilities: vec![],
        }))
    }

    async fn probe(
        &self,
        _request: Request<ProbeRequest>,
    ) -> Result<Response<ProbeResponse>, Status> {
        tracing::debug!("Probe called");
        let ready = self.cache_dir.is_dir();
        if !ready {
            tracing::warn!(
                cache_dir = %self.cache_dir.display(),
                "probe: cache directory not accessible"
            );
        }
        Ok(Response::new(ProbeResponse { ready: Some(ready) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_identity(dir: &std::path::Path) -> CfgdIdentity {
        CfgdIdentity::new(dir.to_path_buf())
    }

    #[tokio::test]
    async fn get_plugin_info_returns_driver_name_and_version() {
        let dir = tempfile::tempdir().unwrap();
        let svc = test_identity(dir.path());
        let resp = svc
            .get_plugin_info(Request::new(GetPluginInfoRequest {}))
            .await
            .unwrap();
        let info = resp.into_inner();
        assert_eq!(info.name, DRIVER_NAME);
        assert_eq!(info.vendor_version, env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn get_plugin_capabilities_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let svc = test_identity(dir.path());
        let resp = svc
            .get_plugin_capabilities(Request::new(GetPluginCapabilitiesRequest {}))
            .await
            .unwrap();
        assert!(
            resp.into_inner().capabilities.is_empty(),
            "Node-only plugin should not advertise plugin-level capabilities"
        );
    }

    #[tokio::test]
    async fn probe_returns_ready_when_cache_dir_exists() {
        let dir = tempfile::tempdir().unwrap();
        let svc = test_identity(dir.path());
        let resp = svc.probe(Request::new(ProbeRequest {})).await.unwrap();
        assert_eq!(resp.into_inner().ready, Some(true));
    }

    #[tokio::test]
    async fn probe_returns_not_ready_when_cache_dir_missing() {
        let svc = CfgdIdentity::new(PathBuf::from("/nonexistent/cfgd-csi-test-path"));
        let resp = svc.probe(Request::new(ProbeRequest {})).await.unwrap();
        assert_eq!(resp.into_inner().ready, Some(false));
    }
}
