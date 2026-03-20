use std::collections::HashMap;
use std::sync::Arc;

use cfgd_csi::cache::Cache;
use cfgd_csi::csi::v1::identity_server::IdentityServer;
use cfgd_csi::csi::v1::node_server::NodeServer;
use cfgd_csi::csi::v1::{
    GetPluginCapabilitiesRequest, GetPluginInfoRequest, NodeGetCapabilitiesRequest,
    NodeGetInfoRequest, NodePublishVolumeRequest, ProbeRequest, node_service_capability,
};
use cfgd_csi::identity::CfgdIdentity;
use cfgd_csi::metrics::CsiMetrics;
use cfgd_csi::node::CfgdNode;
use tokio::net::UnixListener;
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::{Channel, Endpoint, Server, Uri};
use tower::service_fn;

async fn start_server(tmp: &tempfile::TempDir) -> Channel {
    let cache_dir = tmp.path().join("cache");
    let socket_path = tmp.path().join("csi.sock");

    std::fs::create_dir_all(&cache_dir).unwrap();

    let cache = Arc::new(Cache::new(cache_dir.clone(), 1024 * 1024).unwrap());
    let mut registry = prometheus_client::registry::Registry::default();
    let metrics = Arc::new(CsiMetrics::new(&mut registry));

    let listener = UnixListener::bind(&socket_path).unwrap();
    let stream = UnixListenerStream::new(listener);

    tokio::spawn(async move {
        Server::builder()
            .add_service(IdentityServer::new(CfgdIdentity::new(cache_dir)))
            .add_service(NodeServer::new(CfgdNode::new(
                cache,
                metrics,
                "test-node".to_string(),
            )))
            .serve_with_incoming(stream)
            .await
            .unwrap();
    });

    // Small delay for server to start accepting
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let sock = socket_path.clone();
    Endpoint::try_from("http://[::]:50051")
        .unwrap()
        .connect_with_connector(service_fn(move |_: Uri| {
            let sock = sock.clone();
            async move {
                let stream = tokio::net::UnixStream::connect(sock).await?;
                Ok::<_, std::io::Error>(hyper_util::rt::TokioIo::new(stream))
            }
        }))
        .await
        .unwrap()
}

#[tokio::test]
async fn identity_get_plugin_info() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::identity_client::IdentityClient::new(channel);

    let resp = client
        .get_plugin_info(GetPluginInfoRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.name, "csi.cfgd.io");
    assert!(!resp.vendor_version.is_empty());
}

#[tokio::test]
async fn identity_probe() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::identity_client::IdentityClient::new(channel);

    let resp = client
        .probe(ProbeRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.ready, Some(true));
}

#[tokio::test]
async fn identity_plugin_capabilities_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::identity_client::IdentityClient::new(channel);

    let resp = client
        .get_plugin_capabilities(GetPluginCapabilitiesRequest {})
        .await
        .unwrap()
        .into_inner();
    assert!(resp.capabilities.is_empty());
}

#[tokio::test]
async fn node_get_capabilities() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::node_client::NodeClient::new(channel);

    let resp = client
        .node_get_capabilities(NodeGetCapabilitiesRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.capabilities.len(), 1);
    match &resp.capabilities[0].r#type {
        Some(node_service_capability::Type::Rpc(rpc)) => {
            assert_eq!(
                rpc.r#type,
                node_service_capability::rpc::Type::StageUnstageVolume as i32
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[tokio::test]
async fn node_get_info() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::node_client::NodeClient::new(channel);

    let resp = client
        .node_get_info(NodeGetInfoRequest {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(resp.node_id, "test-node");
}

#[tokio::test]
async fn node_publish_volume_missing_module_returns_invalid_argument() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::node_client::NodeClient::new(channel);

    let err = client
        .node_publish_volume(NodePublishVolumeRequest {
            volume_id: "vol-1".to_string(),
            target_path: "/tmp/target".to_string(),
            volume_context: HashMap::new(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(err.message().contains("module"));
}
