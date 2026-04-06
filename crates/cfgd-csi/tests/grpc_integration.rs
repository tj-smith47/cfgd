use std::collections::HashMap;
use std::sync::Arc;

use cfgd_csi::cache::Cache;
use cfgd_csi::csi::v1::identity_server::IdentityServer;
use cfgd_csi::csi::v1::node_server::NodeServer;
use cfgd_csi::csi::v1::{
    GetPluginCapabilitiesRequest, GetPluginInfoRequest, NodeGetCapabilitiesRequest,
    NodeGetInfoRequest, NodeGetVolumeStatsRequest, NodePublishVolumeRequest,
    NodeUnpublishVolumeRequest, ProbeRequest, node_service_capability, volume_usage,
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

    let resp = client.probe(ProbeRequest {}).await.unwrap().into_inner();
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
    assert_eq!(resp.capabilities.len(), 2);
    let rpc_types: Vec<i32> = resp
        .capabilities
        .iter()
        .filter_map(|c| match &c.r#type {
            Some(node_service_capability::Type::Rpc(rpc)) => Some(rpc.r#type),
            _ => None,
        })
        .collect();
    assert!(rpc_types.contains(&(node_service_capability::rpc::Type::StageUnstageVolume as i32)));
    assert!(rpc_types.contains(&(node_service_capability::rpc::Type::GetVolumeStats as i32)));
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

#[tokio::test]
async fn node_unpublish_volume_nonexistent_target_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::node_client::NodeClient::new(channel);

    // Unpublishing a path that was never mounted should succeed (CSI idempotency)
    let target = tmp.path().join("never-mounted");
    let resp = client
        .node_unpublish_volume(NodeUnpublishVolumeRequest {
            volume_id: "vol-1".to_string(),
            target_path: target.to_str().unwrap().to_string(),
        })
        .await
        .expect("unpublish of nonexistent target should succeed per CSI idempotency");
    // NodeUnpublishVolumeResponse is intentionally empty per CSI spec;
    // verify the target was not spuriously created
    let _ = resp.into_inner();
    assert!(
        !target.exists(),
        "nonexistent target path should remain absent after unpublish"
    );
}

#[tokio::test]
async fn node_unpublish_volume_empty_dir_cleans_up() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::node_client::NodeClient::new(channel);

    let target = tmp.path().join("empty-mount-target");
    std::fs::create_dir_all(&target).unwrap();
    assert!(target.exists());

    let resp = client
        .node_unpublish_volume(NodeUnpublishVolumeRequest {
            volume_id: "vol-1".to_string(),
            target_path: target.to_str().unwrap().to_string(),
        })
        .await;
    assert!(resp.is_ok());
    assert!(!target.exists(), "target should be removed after unpublish");
}

#[tokio::test]
async fn node_unpublish_volume_missing_volume_id() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::node_client::NodeClient::new(channel);

    let err = client
        .node_unpublish_volume(NodeUnpublishVolumeRequest {
            volume_id: String::new(),
            target_path: "/tmp/target".to_string(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn node_unpublish_volume_missing_target_path() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::node_client::NodeClient::new(channel);

    let err = client
        .node_unpublish_volume(NodeUnpublishVolumeRequest {
            volume_id: "vol-1".to_string(),
            target_path: String::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn node_get_volume_stats_via_grpc() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::node_client::NodeClient::new(channel);

    // Create a directory with known content
    let vol_dir = tmp.path().join("stats-vol");
    std::fs::create_dir_all(&vol_dir).unwrap();
    std::fs::write(vol_dir.join("a.txt"), "hello").unwrap(); // 5 bytes
    std::fs::write(vol_dir.join("b.txt"), "world!").unwrap(); // 6 bytes

    let resp = client
        .node_get_volume_stats(NodeGetVolumeStatsRequest {
            volume_id: "vol-stats".to_string(),
            volume_path: vol_dir.to_str().unwrap().to_string(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();

    assert_eq!(resp.usage.len(), 2);

    let bytes_entry = resp
        .usage
        .iter()
        .find(|u| u.unit == volume_usage::Unit::Bytes as i32)
        .unwrap();
    assert_eq!(bytes_entry.used, 11);

    let inodes_entry = resp
        .usage
        .iter()
        .find(|u| u.unit == volume_usage::Unit::Inodes as i32)
        .unwrap();
    assert_eq!(inodes_entry.used, 3); // root dir + 2 files
}

#[tokio::test]
async fn node_get_volume_stats_nonexistent_path() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::node_client::NodeClient::new(channel);

    let err = client
        .node_get_volume_stats(NodeGetVolumeStatsRequest {
            volume_id: "vol-1".to_string(),
            volume_path: "/nonexistent/cfgd-grpc-test".to_string(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::NotFound);
}

#[tokio::test]
async fn node_get_volume_stats_missing_volume_id() {
    let tmp = tempfile::tempdir().unwrap();
    let channel = start_server(&tmp).await;
    let mut client = cfgd_csi::csi::v1::node_client::NodeClient::new(channel);

    let err = client
        .node_get_volume_stats(NodeGetVolumeStatsRequest {
            volume_id: String::new(),
            volume_path: "/tmp".to_string(),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}
