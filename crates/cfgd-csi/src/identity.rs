use tonic::{Request, Response, Status};

use crate::csi::v1::identity_server::Identity;
use crate::csi::v1::{
    GetPluginCapabilitiesRequest, GetPluginCapabilitiesResponse, GetPluginInfoRequest,
    GetPluginInfoResponse, PluginCapability, ProbeRequest, ProbeResponse,
    plugin_capability,
};

pub const DRIVER_NAME: &str = "csi.cfgd.io";

pub struct CfgdIdentity;

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
        Ok(Response::new(GetPluginCapabilitiesResponse {
            capabilities: vec![PluginCapability {
                r#type: Some(plugin_capability::Type::VolumeExpansion(
                    plugin_capability::VolumeExpansion {
                        r#type: 0, // UNKNOWN — we don't support expansion
                    },
                )),
            }],
        }))
    }

    async fn probe(
        &self,
        _request: Request<ProbeRequest>,
    ) -> Result<Response<ProbeResponse>, Status> {
        tracing::debug!("Probe called");
        Ok(Response::new(ProbeResponse { ready: Some(true) }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn get_plugin_info_returns_driver_name() {
        let svc = CfgdIdentity;
        let resp = svc
            .get_plugin_info(Request::new(GetPluginInfoRequest {}))
            .await
            .unwrap();
        let info = resp.into_inner();
        assert_eq!(info.name, DRIVER_NAME);
        assert!(!info.vendor_version.is_empty());
    }

    #[tokio::test]
    async fn get_plugin_capabilities_returns_capabilities() {
        let svc = CfgdIdentity;
        let resp = svc
            .get_plugin_capabilities(Request::new(GetPluginCapabilitiesRequest {}))
            .await
            .unwrap();
        let caps = resp.into_inner();
        assert!(!caps.capabilities.is_empty());
    }

    #[tokio::test]
    async fn probe_returns_ready() {
        let svc = CfgdIdentity;
        let resp = svc
            .probe(Request::new(ProbeRequest {}))
            .await
            .unwrap();
        assert_eq!(resp.into_inner().ready, Some(true));
    }
}
