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
    assert!(rpc_types.contains(&(node_service_capability::rpc::Type::StageUnstageVolume as i32)));
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
    // unstage is a no-op (cache persists) — just verify it returns Ok
    let _resp = node
        .node_unstage_volume(Request::new(req))
        .await
        .expect("unstage should succeed for a valid volume_id");
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
    assert!(
        err.message().contains("volume_id"),
        "error should mention volume_id: {}",
        err.message()
    );
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
    // Should return Ok(()) for any non-empty string
    require_volume_id("vol-1").expect("non-empty volume_id should be accepted");
    require_volume_id("a").expect("single-char volume_id should be accepted");
}

#[test]
fn require_volume_id_rejects_empty() {
    let err = require_volume_id("").unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message().contains("volume_id"),
        "error should mention volume_id: {}",
        err.message()
    );
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

// -------------------------------------------------------------------------
// registry_of — OCI reference parsing.
// -------------------------------------------------------------------------

#[test]
fn registry_of_extracts_dotted_host() {
    // "first path segment contains '.'" → host registry
    assert_eq!(registry_of("ghcr.io/org/mod:tag"), "ghcr.io");
}

#[test]
fn registry_of_extracts_host_with_port() {
    // colon in head → registry with explicit port
    assert_eq!(
        registry_of("myreg.local:5000/org/mod:tag"),
        "myreg.local:5000"
    );
}

#[test]
fn registry_of_returns_empty_for_default_namespace() {
    // No dot, no colon, not localhost → no explicit registry; ref is in the
    // reserved cfgd-modules namespace and registry is the empty string.
    assert_eq!(registry_of("cfgd-modules/nettools:v1"), "");
}

#[test]
fn registry_of_treats_localhost_as_registry() {
    // `localhost` is the documented exception: no dot, but explicitly a
    // registry host.
    assert_eq!(registry_of("localhost/org/mod:tag"), "localhost");
}

#[test]
fn registry_of_returns_empty_for_unslashed_ref() {
    // No '/' in the ref → first_slash == ref.len(), head == whole ref. As
    // long as it has no dot/colon and isn't `localhost`, registry is "".
    assert_eq!(registry_of("just-a-name"), "");
}

// -------------------------------------------------------------------------
// check_registry_allowed — allow-list semantics.
// -------------------------------------------------------------------------

#[test]
fn check_registry_allowed_passes_when_allow_list_unset() {
    // None means "no enforcement" — every ref passes regardless of registry.
    assert!(check_registry_allowed("ghcr.io/org/mod:v1", None).is_ok());
    assert!(check_registry_allowed("cfgd-modules/foo:v1", None).is_ok());
}

#[test]
fn check_registry_allowed_passes_default_namespace_under_allow_list() {
    // Ref with no explicit registry (default cfgd-modules namespace) is
    // always allowed: it can't reach the network without caller-provided
    // registry, so allow-listing it would be redundant.
    let allow = vec!["ghcr.io".to_string()];
    assert!(check_registry_allowed("cfgd-modules/foo:v1", Some(&allow)).is_ok());
}

#[test]
fn check_registry_allowed_passes_when_registry_in_list() {
    let allow = vec!["ghcr.io".to_string(), "quay.io".to_string()];
    assert!(check_registry_allowed("quay.io/org/mod:tag", Some(&allow)).is_ok());
}

#[test]
fn check_registry_allowed_rejects_when_registry_not_in_list() {
    let allow = vec!["ghcr.io".to_string()];
    let err = check_registry_allowed("docker.io/org/mod:tag", Some(&allow))
        .expect_err("docker.io not in allow-list");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(err.message().contains("docker.io"));
    assert!(
        err.message().contains("CFGD_CSI_ALLOWED_REGISTRIES"),
        "error should reference the env var so the operator can fix it: {}",
        err.message()
    );
}

// -------------------------------------------------------------------------
// parse_allowed_registries_from_env — env-var parsing.
// -------------------------------------------------------------------------

#[test]
#[serial_test::serial]
fn parse_allowed_registries_from_env_returns_none_when_unset() {
    // SAFETY: serialised — no other test mutates this var concurrently.
    unsafe {
        std::env::remove_var(ALLOWED_REGISTRIES_ENV);
    }
    assert!(parse_allowed_registries_from_env().is_none());
}

#[test]
#[serial_test::serial]
fn parse_allowed_registries_from_env_returns_none_for_wildcard() {
    // SAFETY: serialised.
    unsafe {
        std::env::set_var(ALLOWED_REGISTRIES_ENV, "*");
    }
    let got = parse_allowed_registries_from_env();
    unsafe {
        std::env::remove_var(ALLOWED_REGISTRIES_ENV);
    }
    assert!(
        got.is_none(),
        "'*' means 'allow any' and must collapse to None"
    );
}

#[test]
#[serial_test::serial]
fn parse_allowed_registries_from_env_splits_csv_with_trimming() {
    // SAFETY: serialised.
    unsafe {
        std::env::set_var(ALLOWED_REGISTRIES_ENV, " ghcr.io , quay.io ,, docker.io ");
    }
    let got = parse_allowed_registries_from_env();
    unsafe {
        std::env::remove_var(ALLOWED_REGISTRIES_ENV);
    }
    assert_eq!(
        got,
        Some(vec![
            "ghcr.io".to_string(),
            "quay.io".to_string(),
            "docker.io".to_string()
        ]),
        "CSV split + trim + drop empty"
    );
}

#[test]
#[serial_test::serial]
fn parse_allowed_registries_from_env_returns_none_for_empty_string() {
    // SAFETY: serialised.
    unsafe {
        std::env::set_var(ALLOWED_REGISTRIES_ENV, "   ");
    }
    let got = parse_allowed_registries_from_env();
    unsafe {
        std::env::remove_var(ALLOWED_REGISTRIES_ENV);
    }
    assert!(got.is_none(), "whitespace-only → None");
}
