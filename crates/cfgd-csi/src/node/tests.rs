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

// -------------------------------------------------------------------------
// node_stage_volume / node_publish_volume — exercise the post-validation
// branches by driving requests with a bogus ociRef so the cache pull fails.
// Catches the metrics emit + Status::internal mapping paths that pure
// validation tests don't reach.
// -------------------------------------------------------------------------

#[tokio::test]
#[serial_test::serial]
async fn node_stage_volume_cache_pull_failure_returns_internal_status() {
    // `test_node()` caches CFGD_CSI_ALLOWED_REGISTRIES at construction. A
    // parallel `*_rejects_disallowed_registry` test that sets it would
    // pin this Node to its allow-list, making 127.0.0.1 fail with
    // PermissionDenied instead of the Internal we test for. Force-unset
    // for the duration of this test.
    let _g = cfgd_core::test_helpers::EnvVarGuard::unset(ALLOWED_REGISTRIES_ENV);
    let dir = tempfile::tempdir().unwrap();
    let node = test_node(test_cache(dir.path()));
    let req = NodeStageVolumeRequest {
        volume_id: "vol-1".to_string(),
        staging_target_path: dir.path().join("staging").to_str().unwrap().to_string(),
        volume_context: [
            ("module".to_string(), "nettools".to_string()),
            ("version".to_string(), "1.0".to_string()),
            // Bogus ociRef pointing at a port nothing is listening on —
            // cfgd-core's pull_module will surface Err via Cache::get_or_pull.
            (
                "ociRef".to_string(),
                "127.0.0.1:1/cfgd-csi-test/nettools:1.0".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let err = node
        .node_stage_volume(Request::new(req))
        .await
        .expect_err("bogus oci_ref → cache pull fails → Status::internal");
    assert_eq!(err.code(), tonic::Code::Internal);
    assert!(
        err.message().contains("cache pull failed"),
        "internal error must mention cache pull failure: {}",
        err.message()
    );
}

#[tokio::test]
#[serial_test::serial]
async fn node_publish_volume_cache_pull_failure_returns_internal_status() {
    // Same race as the stage-volume counterpart: see comment above.
    let _g = cfgd_core::test_helpers::EnvVarGuard::unset(ALLOWED_REGISTRIES_ENV);
    let dir = tempfile::tempdir().unwrap();
    let node = test_node(test_cache(dir.path()));
    let target = dir.path().join("publish-target");
    let req = NodePublishVolumeRequest {
        volume_id: "vol-1".to_string(),
        target_path: target.to_str().unwrap().to_string(),
        volume_context: [
            ("module".to_string(), "nettools".to_string()),
            ("version".to_string(), "1.0".to_string()),
            (
                "ociRef".to_string(),
                "127.0.0.1:1/cfgd-csi-test/nettools:1.0".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let err = node
        .node_publish_volume(Request::new(req))
        .await
        .expect_err("bogus oci_ref → cache pull fails → Status::internal");
    assert_eq!(err.code(), tonic::Code::Internal);
    assert!(
        err.message().contains("cache pull failed"),
        "internal error must mention cache pull failure: {}",
        err.message()
    );
}

#[tokio::test]
#[serial_test::serial]
async fn node_stage_volume_rejects_disallowed_registry() {
    // SAFETY: serialised — env mutation is process-global.
    unsafe {
        std::env::set_var(ALLOWED_REGISTRIES_ENV, "ghcr.io");
    }
    let dir = tempfile::tempdir().unwrap();
    let node = test_node(test_cache(dir.path()));
    unsafe {
        std::env::remove_var(ALLOWED_REGISTRIES_ENV);
    }

    let req = NodeStageVolumeRequest {
        volume_id: "vol-1".to_string(),
        staging_target_path: dir.path().join("staging").to_str().unwrap().to_string(),
        volume_context: [
            ("module".to_string(), "nettools".to_string()),
            ("version".to_string(), "1.0".to_string()),
            (
                "ociRef".to_string(),
                "docker.io/cfgd-csi-test/nettools:1.0".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let err = node
        .node_stage_volume(Request::new(req))
        .await
        .expect_err("docker.io not in allow-list → PermissionDenied");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        err.message().contains("docker.io"),
        "rejection must name the disallowed host: {}",
        err.message()
    );
}

#[tokio::test]
#[serial_test::serial]
async fn node_publish_volume_rejects_disallowed_registry() {
    // SAFETY: serialised — env mutation is process-global.
    unsafe {
        std::env::set_var(ALLOWED_REGISTRIES_ENV, "ghcr.io");
    }
    let dir = tempfile::tempdir().unwrap();
    let node = test_node(test_cache(dir.path()));
    unsafe {
        std::env::remove_var(ALLOWED_REGISTRIES_ENV);
    }

    let req = NodePublishVolumeRequest {
        volume_id: "vol-1".to_string(),
        target_path: dir.path().join("target").to_str().unwrap().to_string(),
        volume_context: [
            ("module".to_string(), "nettools".to_string()),
            ("version".to_string(), "1.0".to_string()),
            (
                "ociRef".to_string(),
                "quay.io/cfgd-csi-test/nettools:1.0".to_string(),
            ),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let err = node
        .node_publish_volume(Request::new(req))
        .await
        .expect_err("quay.io not in allow-list → PermissionDenied");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        err.message().contains("quay.io"),
        "rejection must name the disallowed host: {}",
        err.message()
    );
}

// -------------------------------------------------------------------------
// CfgdNode::new — registry allow-list constructor paths. The current
// constructor logs a warn/info but otherwise stores the parsed list; assert
// the field is what the env parser produced so the logging-branch arm is
// covered.
// -------------------------------------------------------------------------

#[test]
#[serial_test::serial]
fn cfgd_node_new_with_empty_allow_list_stores_empty_vec() {
    // SAFETY: serialised — env mutation is process-global.
    unsafe {
        std::env::set_var(ALLOWED_REGISTRIES_ENV, ",,,");
    }
    let dir = tempfile::tempdir().unwrap();
    let mut registry = prometheus_client::registry::Registry::default();
    let metrics = Arc::new(CsiMetrics::new(&mut registry));
    let node = CfgdNode::new(test_cache(dir.path()), metrics, "test-node".to_string());
    unsafe {
        std::env::remove_var(ALLOWED_REGISTRIES_ENV);
    }
    // ",,," parses to Some(empty vec) — every entry is filtered out as empty
    // after split+trim. Constructor's "explicitly empty" branch logs a warn.
    assert_eq!(
        node.allowed_registries,
        Some(Vec::new()),
        "empty-but-set allow-list must be Some(empty), not None"
    );
}

#[test]
#[serial_test::serial]
fn cfgd_node_new_with_configured_allow_list_stores_parsed_entries() {
    // SAFETY: serialised.
    unsafe {
        std::env::set_var(ALLOWED_REGISTRIES_ENV, "ghcr.io,quay.io");
    }
    let dir = tempfile::tempdir().unwrap();
    let mut registry = prometheus_client::registry::Registry::default();
    let metrics = Arc::new(CsiMetrics::new(&mut registry));
    let node = CfgdNode::new(test_cache(dir.path()), metrics, "test-node".to_string());
    unsafe {
        std::env::remove_var(ALLOWED_REGISTRIES_ENV);
    }
    assert_eq!(
        node.allowed_registries,
        Some(vec!["ghcr.io".to_string(), "quay.io".to_string()])
    );
}

// -------------------------------------------------------------------------
// check_registry_allowed — extra edge cases for the explicit-empty list
// branch (every ref refused except default-namespace refs).
// -------------------------------------------------------------------------

#[test]
fn check_registry_allowed_empty_list_rejects_explicit_registry_refs() {
    // Some(empty) → fail-closed for refs with an explicit registry host.
    let allow: Vec<String> = Vec::new();
    let err = check_registry_allowed("ghcr.io/org/mod:tag", Some(&allow))
        .expect_err("empty allow-list must reject ghcr.io ref");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
}

#[test]
fn check_registry_allowed_empty_list_still_allows_default_namespace() {
    // Default-namespace refs (no explicit registry) bypass the allow-list
    // check because they can't reach the network without a caller-provided
    // registry. Pinning this means an over-zealous future refactor that
    // tightens the empty-list branch to reject *everything* must update
    // the documented contract.
    let allow: Vec<String> = Vec::new();
    assert!(check_registry_allowed("cfgd-modules/foo:v1", Some(&allow)).is_ok());
}

// -------------------------------------------------------------------------
// node_unpublish_volume — empty volume_id branch was already covered above;
// also test the spawn_blocking unmount happy path on a non-mounted target
// to exercise the join-await branch (the "warn on remove_dir" arm) by
// pre-creating a directory that does not get removable because of perms.
// -------------------------------------------------------------------------

#[tokio::test]
async fn node_unpublish_volume_succeeds_for_existing_dir_and_removes_it() {
    // Distinct from `node_unpublish_volume_empty_dir_cleans_up`: that test
    // confirms cleanup; this one pins the response shape (empty unpublish
    // response) so a future refactor that introduces a payload must update
    // both expectations.
    let dir = tempfile::tempdir().unwrap();
    let node = test_node(test_cache(dir.path()));
    let target = dir.path().join("ephemeral-target");
    std::fs::create_dir_all(&target).unwrap();

    let req = NodeUnpublishVolumeRequest {
        volume_id: "vol-1".to_string(),
        target_path: target.to_str().unwrap().to_string(),
    };
    let resp = node
        .node_unpublish_volume(Request::new(req))
        .await
        .expect("unpublish on a non-mounted existing dir must succeed");
    // Response is the empty unit message — accessing it pins the shape.
    let _: NodeUnpublishVolumeResponse = resp.into_inner();
    assert!(!target.exists(), "target removed after unpublish");
}

// -------------------------------------------------------------------------
// node_get_volume_stats — additional usage entry coverage for the bytes
// vs inodes unit dispatch. The default tests don't pin the unit ordering;
// this one checks the response contains exactly the expected two units
// independent of order so a renderer that re-orders the vec won't break
// downstream consumers.
// -------------------------------------------------------------------------

#[tokio::test]
async fn node_get_volume_stats_returns_exactly_two_units_bytes_and_inodes() {
    let dir = tempfile::tempdir().unwrap();
    let node = test_node(test_cache(dir.path()));

    let vol = dir.path().join("stats-vol");
    std::fs::create_dir_all(&vol).unwrap();

    let req = NodeGetVolumeStatsRequest {
        volume_id: "vol-1".to_string(),
        volume_path: vol.to_str().unwrap().to_string(),
        ..Default::default()
    };
    let resp = node.node_get_volume_stats(Request::new(req)).await.unwrap();
    let inner = resp.into_inner();
    let units: std::collections::HashSet<i32> = inner.usage.iter().map(|u| u.unit).collect();
    assert!(units.contains(&(volume_usage::Unit::Bytes as i32)));
    assert!(units.contains(&(volume_usage::Unit::Inodes as i32)));
    assert_eq!(units.len(), 2, "exactly two distinct units");
    assert!(
        inner.volume_condition.is_none(),
        "no volume_condition is exposed (kubelet treats None as healthy)"
    );
}

// -------------------------------------------------------------------------
// Edge-case branch coverage for require_volume_id / require_attr / resolve_oci_ref.
// These are tiny helpers but the cases below pin behaviour that downstream
// staging/publishing flows depend on.
// -------------------------------------------------------------------------

#[test]
fn require_volume_id_error_message_includes_volume_id_token() {
    let err = require_volume_id("").unwrap_err();
    // Error message format is fixed contract — kubelet's CSI client logs the
    // tonic Status message verbatim, so changing it would change operator logs.
    assert!(
        err.message().contains("volume_id is required"),
        "exact wording matters: {}",
        err.message()
    );
}

#[test]
fn require_attr_present_value_is_returned_verbatim() {
    let attrs: HashMap<String, String> = [
        ("module".to_string(), "nettools".to_string()),
        ("other".to_string(), "value".to_string()),
    ]
    .into_iter()
    .collect();
    let v = require_attr(&attrs, "module").expect("present key returns value");
    assert_eq!(v, "nettools");
}

#[test]
fn require_attr_missing_message_includes_key_name() {
    let attrs: HashMap<String, String> = HashMap::new();
    let err = require_attr(&attrs, "ociRef").unwrap_err();
    assert!(
        err.message().contains("ociRef"),
        "error must name the missing attr: {}",
        err.message()
    );
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[test]
fn resolve_oci_ref_provided_overrides_default_namespace() {
    // The provided ociRef wins over the synthetic cfgd-modules/{module}:{version}
    // even when both the override and module/version are present.
    let attrs: HashMap<String, String> = [
        (
            "ociRef".to_string(),
            "registry.example/team/mod:tag".to_string(),
        ),
        ("module".to_string(), "nettools".to_string()),
    ]
    .into_iter()
    .collect();
    let r = resolve_oci_ref(&attrs, "nettools", "1.0");
    assert_eq!(r, "registry.example/team/mod:tag");
}

#[test]
fn registry_of_returns_empty_for_localhost_subdir_prefix() {
    // `localhost-foo` does NOT match the localhost exception (which requires
    // the literal token "localhost"), so it's treated as a default-namespace
    // ref with no registry.
    assert_eq!(registry_of("localhost-foo/mod:tag"), "");
}

#[test]
fn registry_of_handles_ref_with_port_only_first_segment() {
    // First slash splits "host:port" from the rest — the colon rule kicks in.
    assert_eq!(
        registry_of("registry.internal:8443/foo/bar"),
        "registry.internal:8443"
    );
}

// -------------------------------------------------------------------------
// check_registry_allowed — additional explicit-empty + allow-list
// edge-case coverage. Pins the fail-closed posture of an empty allow-list
// for ANY explicit-registry ref.
// -------------------------------------------------------------------------

#[test]
fn check_registry_allowed_localhost_must_be_in_list_to_pass() {
    // `localhost` IS treated as a registry by registry_of, so an allow-list
    // that doesn't include `localhost` must reject it.
    let allow = vec!["ghcr.io".to_string()];
    let err = check_registry_allowed("localhost/team/mod:tag", Some(&allow))
        .expect_err("localhost not in allow-list must be rejected");
    assert_eq!(err.code(), tonic::Code::PermissionDenied);
    assert!(
        err.message().contains("localhost"),
        "error must name the rejected registry: {}",
        err.message()
    );
}

#[test]
fn check_registry_allowed_passes_when_localhost_in_list() {
    let allow = vec!["localhost".to_string()];
    check_registry_allowed("localhost/team/mod:tag", Some(&allow))
        .expect("localhost in allow-list must pass");
}

// -------------------------------------------------------------------------
// parse_allowed_registries_from_env — extra arms.
// -------------------------------------------------------------------------

#[test]
#[serial_test::serial]
fn parse_allowed_registries_from_env_single_entry_returns_some_with_one_item() {
    unsafe {
        std::env::set_var(ALLOWED_REGISTRIES_ENV, "ghcr.io");
    }
    let got = parse_allowed_registries_from_env();
    unsafe {
        std::env::remove_var(ALLOWED_REGISTRIES_ENV);
    }
    assert_eq!(got, Some(vec!["ghcr.io".to_string()]));
}

#[test]
#[serial_test::serial]
fn parse_allowed_registries_from_env_wildcard_with_whitespace_is_still_wildcard() {
    unsafe {
        std::env::set_var(ALLOWED_REGISTRIES_ENV, "  *  ");
    }
    let got = parse_allowed_registries_from_env();
    unsafe {
        std::env::remove_var(ALLOWED_REGISTRIES_ENV);
    }
    assert!(
        got.is_none(),
        "trimmed `*` must still collapse to wildcard (None)"
    );
}

// -------------------------------------------------------------------------
// walk_volume_stats — symlink-only directory + nested deep paths.
// -------------------------------------------------------------------------

#[cfg(unix)]
#[test]
fn walk_volume_stats_directory_of_symlinks_counts_each_symlink_inode() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("real.txt");
    std::fs::write(&target, "abc").unwrap();

    let links_dir = dir.path().join("links");
    std::fs::create_dir_all(&links_dir).unwrap();
    std::os::unix::fs::symlink(&target, links_dir.join("a")).unwrap();
    std::os::unix::fs::symlink(&target, links_dir.join("b")).unwrap();
    std::os::unix::fs::symlink(&target, links_dir.join("c")).unwrap();

    let (_bytes, inodes) = walk_volume_stats(&links_dir);
    // Root dir + 3 symlinks
    assert_eq!(inodes, 4);
}

#[test]
fn walk_volume_stats_deeply_nested_dirs_counts_all_entries() {
    let dir = tempfile::tempdir().unwrap();
    // 5-deep nested dirs each with one file
    let mut p = dir.path().to_path_buf();
    for i in 0..5 {
        p = p.join(format!("d{i}"));
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("f.txt"), format!("content-{i}")).unwrap();
    }
    let (bytes, inodes) = walk_volume_stats(dir.path());
    // Each of the 5 levels has: 1 subdir + 1 file = 2 entries; plus root.
    // root(1) + (d0 + f.txt)(2) + (d1 + f.txt)(2) + ... = 1 + 5*2 = 11
    assert_eq!(inodes, 11);
    // Each file has bytes "content-N" = 9 bytes; 5 files = 45 bytes.
    assert_eq!(bytes, 45);
}
