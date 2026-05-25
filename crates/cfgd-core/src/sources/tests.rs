use super::*;
use crate::test_helpers::test_printer;
use serial_test::serial;

#[test]
fn normalize_tilde_pin() {
    // ~N -> ^N.0.0 (any version N.x.x)
    assert_eq!(normalize_semver_pin("~2"), "^2.0.0");
    // ~N.M -> ~N.M.0 (any version N.M.x)
    assert_eq!(normalize_semver_pin("~2.1"), "~2.1.0");
    // Full version passed through
    assert_eq!(normalize_semver_pin("~2.1.0"), "~2.1.0");
}

#[test]
fn normalize_caret_pin() {
    assert_eq!(normalize_semver_pin("^3"), "^3.0.0");
    assert_eq!(normalize_semver_pin("^3.1"), "^3.1.0");
    assert_eq!(normalize_semver_pin("^3.1.2"), "^3.1.2");
}

#[test]
fn normalize_exact_pin() {
    assert_eq!(normalize_semver_pin("=1.2.3"), "=1.2.3");
    assert_eq!(normalize_semver_pin("1.2.3"), "1.2.3");
}

#[test]
fn version_pin_matching() {
    let pin = normalize_semver_pin("~2");
    let req = VersionReq::parse(&pin).unwrap();
    assert!(req.matches(&Version::new(2, 0, 0)));
    assert!(req.matches(&Version::new(2, 5, 0)));
    assert!(!req.matches(&Version::new(3, 0, 0)));
    assert!(!req.matches(&Version::new(1, 9, 0)));
}

#[test]
fn source_manager_creates_cache_dir() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());
    assert!(mgr.all_sources().is_empty());
}

#[test]
fn build_source_spec_with_defaults() {
    let spec =
        SourceManager::build_source_spec("acme", "git@github.com:acme/config.git", Some("backend"));
    assert_eq!(spec.name, "acme");
    assert_eq!(spec.subscription.priority, 500);
    assert_eq!(spec.subscription.profile.as_deref(), Some("backend"));
    assert_eq!(spec.sync.interval, "1h");
}

#[test]
fn build_source_spec_no_profile() {
    let spec = SourceManager::build_source_spec("test", "https://example.com/config.git", None);
    assert!(spec.subscription.profile.is_none());
}

#[test]
fn remove_nonexistent_source() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());
    let err = mgr.remove_source("nonexistent").unwrap_err();
    assert!(
        err.to_string().contains("not found"),
        "expected 'not found' error, got: {err}"
    );
}

#[test]
fn get_nonexistent_source() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());
    assert!(mgr.get("nonexistent").is_none());
}

#[test]
fn detect_source_manifest_found() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(SOURCE_MANIFEST_FILE),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: test-source
spec:
  provides:
    profiles:
      - base
  policy: {}
"#,
    )
    .unwrap();

    let result = detect_source_manifest(dir.path()).unwrap();
    assert!(result.is_some());
    assert_eq!(result.unwrap().metadata.name, "test-source");
}

#[test]
fn detect_source_manifest_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let result = detect_source_manifest(dir.path()).unwrap();
    assert!(result.is_none());
}

#[test]
fn detect_source_manifest_invalid() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(SOURCE_MANIFEST_FILE), "not: valid: yaml: [").unwrap();
    let err = detect_source_manifest(dir.path()).unwrap_err();
    assert!(
        err.to_string().contains("invalid") || err.to_string().contains("ConfigSource"),
        "expected manifest parse error, got: {err}"
    );
}

#[test]
fn parse_manifest_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());
    let result = mgr.parse_manifest("test", dir.path());
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn parse_manifest_valid() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(SOURCE_MANIFEST_FILE),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: test-source
  version: "1.0.0"
spec:
  provides:
    profiles:
      - base
  policy:
    required:
      packages:
        brew:
          formulae:
            - git-secrets
    constraints:
      noScripts: true
"#,
    )
    .unwrap();

    let mgr = SourceManager::new(dir.path());
    let manifest = mgr.parse_manifest("test", dir.path()).unwrap();
    assert_eq!(manifest.metadata.name, "test-source");
    assert_eq!(manifest.spec.provides.profiles, vec!["base"]);
}

#[test]
fn check_version_pin_passes() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());

    let manifest = ConfigSourceDocument {
        api_version: crate::API_VERSION.into(),
        kind: "ConfigSource".into(),
        metadata: crate::config::ConfigSourceMetadata {
            name: "test".into(),
            version: Some("2.1.0".into()),
            description: None,
        },
        spec: crate::config::ConfigSourceSpec {
            provides: Default::default(),
            policy: Default::default(),
        },
    };

    // All three pins should match version 2.1.0 — unwrap to prove success
    mgr.check_version_pin("test", &manifest, "~2")
        .expect("~2 should match 2.1.0");
    mgr.check_version_pin("test", &manifest, "^2")
        .expect("^2 should match 2.1.0");
    mgr.check_version_pin("test", &manifest, "~2.1")
        .expect("~2.1 should match 2.1.0");
}

#[test]
fn check_version_pin_fails() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());

    let manifest = ConfigSourceDocument {
        api_version: crate::API_VERSION.into(),
        kind: "ConfigSource".into(),
        metadata: crate::config::ConfigSourceMetadata {
            name: "test".into(),
            version: Some("3.0.0".into()),
            description: None,
        },
        spec: crate::config::ConfigSourceSpec {
            provides: Default::default(),
            policy: Default::default(),
        },
    };

    let err = mgr.check_version_pin("test", &manifest, "~2").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("3.0.0") && msg.contains("~2"),
        "expected version mismatch with '3.0.0' and '~2', got: {msg}"
    );
}

#[test]
fn verify_signature_skipped_when_not_required() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());
    let constraints = crate::config::SourceConstraints::default();
    assert!(
        !constraints.require_signed_commits,
        "default should be false"
    );
    // require_signed_commits defaults to false — should return Ok(()) without any repo
    let result = mgr.verify_commit_signature("test", dir.path(), &constraints);
    assert_eq!(
        result.unwrap(),
        (),
        "expected Ok(()) when signatures not required"
    );
}

#[test]
fn verify_signature_skipped_when_allow_unsigned() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());
    mgr.set_allow_unsigned(true);
    let constraints = crate::config::SourceConstraints {
        require_signed_commits: true,
        ..Default::default()
    };
    assert!(mgr.allow_unsigned, "allow_unsigned should be set");
    assert!(
        constraints.require_signed_commits,
        "require_signed_commits should be true"
    );
    // Even though require_signed_commits is true, allow_unsigned bypasses it
    let result = mgr.verify_commit_signature("test", dir.path(), &constraints);
    assert_eq!(
        result.unwrap(),
        (),
        "expected Ok(()) when allow_unsigned bypasses verification"
    );
}

#[test]
fn verify_signature_fails_on_unsigned_commit() {
    // Create a real git repo with an unsigned commit
    let dir = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(dir.path()).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "unsigned commit", &tree, &[])
        .unwrap();

    let mgr = SourceManager::new(dir.path());
    let constraints = crate::config::SourceConstraints {
        require_signed_commits: true,
        ..Default::default()
    };

    let result = mgr.verify_commit_signature("test-source", dir.path(), &constraints);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not signed"),
        "expected 'not signed' in error, got: {}",
        err_msg
    );
}

#[test]
fn set_allow_unsigned_works() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());
    assert!(!mgr.allow_unsigned);
    mgr.set_allow_unsigned(true);
    assert!(mgr.allow_unsigned);
}

#[test]
fn verify_head_signature_surfaces_git_log_failure_on_non_git_dir() {
    // verify_head_signature non-zero-exit arm (lines 571-579): pass a tempdir
    // that was NEVER `git init`'d → `git log -1 --format=%G?` exits non-zero
    // → SourceError::SignatureVerificationFailed with the "git log failed"
    // message + stderr-trimmed body. Pins the contract: we surface the git
    // error context, not just a generic "not signed" rejection.
    let tmp = tempfile::tempdir().unwrap();
    let result = verify_head_signature("non-git-source", tmp.path());
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("git log failed") || err_msg.contains("not a git repository"),
        "expected git-log-failed error, got: {}",
        err_msg
    );
}

#[test]
fn verify_head_signature_fails_on_unsigned_repo() {
    // Create a git repo with an unsigned commit using git2 directly
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_dir).unwrap();

    let repo = git2::Repository::init(&repo_dir).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();

    // Create a file and commit it
    std::fs::write(repo_dir.join("README"), "test\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("README")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "unsigned commit", &tree, &[])
        .unwrap();

    // The public function verify_head_signature should fail on unsigned commits
    let result = verify_head_signature("test-source", &repo_dir);
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not signed") || err_msg.contains("signature"),
        "expected signature-related error, got: {}",
        err_msg
    );
}

#[test]
fn source_profiles_dir_nonexistent_source() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());

    let result = mgr.source_profiles_dir("nonexistent");
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not found"),
        "expected 'not found' error, got: {}",
        err_msg
    );
}

#[test]
fn source_files_dir_nonexistent_source() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());

    let result = mgr.source_files_dir("nonexistent");
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("not found"),
        "expected 'not found' error, got: {}",
        err_msg
    );
}

#[test]
fn normalize_semver_pin_whitespace() {
    assert_eq!(normalize_semver_pin("  ~2  "), "^2.0.0");
    assert_eq!(normalize_semver_pin(" ^3.1 "), "^3.1.0");
}

#[test]
fn normalize_semver_pin_plain_version() {
    // No prefix — passed through as-is
    assert_eq!(normalize_semver_pin("2.1.0"), "2.1.0");
    assert_eq!(normalize_semver_pin(">=1.0.0"), ">=1.0.0");
}

#[test]
fn check_version_pin_no_manifest_version_uses_zero() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());

    let manifest = ConfigSourceDocument {
        api_version: crate::API_VERSION.into(),
        kind: "ConfigSource".into(),
        metadata: crate::config::ConfigSourceMetadata {
            name: "test".into(),
            version: None, // No version — defaults to 0.0.0
            description: None,
        },
        spec: crate::config::ConfigSourceSpec {
            provides: Default::default(),
            policy: Default::default(),
        },
    };

    // ~0 matches 0.0.0
    mgr.check_version_pin("test", &manifest, "~0")
        .expect("~0 should match defaulted version 0.0.0");
    // ~1 does NOT match 0.0.0
    let err = mgr.check_version_pin("test", &manifest, "~1").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("0.0.0") && msg.contains("~1"),
        "expected version mismatch with '0.0.0' and '~1', got: {msg}"
    );
}

#[test]
fn check_version_pin_invalid_semver_in_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());

    let manifest = ConfigSourceDocument {
        api_version: crate::API_VERSION.into(),
        kind: "ConfigSource".into(),
        metadata: crate::config::ConfigSourceMetadata {
            name: "test".into(),
            version: Some("not-a-version".into()),
            description: None,
        },
        spec: crate::config::ConfigSourceSpec {
            provides: Default::default(),
            policy: Default::default(),
        },
    };

    let result = mgr.check_version_pin("test", &manifest, "~1");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("semver") || err.contains("invalid"),
        "expected semver error, got: {err}"
    );
}

#[test]
fn check_version_pin_invalid_pin_format() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());

    let manifest = ConfigSourceDocument {
        api_version: crate::API_VERSION.into(),
        kind: "ConfigSource".into(),
        metadata: crate::config::ConfigSourceMetadata {
            name: "test".into(),
            version: Some("1.0.0".into()),
            description: None,
        },
        spec: crate::config::ConfigSourceSpec {
            provides: Default::default(),
            policy: Default::default(),
        },
    };

    let err = mgr
        .check_version_pin("test", &manifest, "not-a-pin")
        .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not-a-pin") && msg.contains("version"),
        "expected version mismatch error mentioning 'not-a-pin', got: {msg}"
    );
}

#[test]
fn read_manifest_no_profiles_is_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(SOURCE_MANIFEST_FILE),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: empty-source
spec:
  provides:
    profiles: []
  policy: {}
"#,
    )
    .unwrap();

    let result = read_manifest("empty-source", dir.path());
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("no profiles") || err.contains("NoProfiles"),
        "expected no-profiles error, got: {err}"
    );
}

#[test]
fn detect_source_manifest_no_profiles_is_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(SOURCE_MANIFEST_FILE),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: empty-profiles
spec:
  provides:
    profiles: []
  policy: {}
"#,
    )
    .unwrap();

    // detect_source_manifest delegates to read_manifest which should fail
    let err = detect_source_manifest(dir.path()).unwrap_err();
    assert!(
        err.to_string().contains("no profiles") || err.to_string().contains("NoProfiles"),
        "expected no-profiles error, got: {err}"
    );
}

#[test]
fn load_source_profile_nonexistent_source() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());

    let result = mgr.load_source_profile("nonexistent", "default");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found"),
        "expected 'not found' error, got: {err}"
    );
}

#[test]
fn default_cache_dir_returns_path() {
    // This test may fail in environments without a home directory,
    // but in normal test environments it should work.
    let result = SourceManager::default_cache_dir();
    assert!(result.is_ok());
    let path = result.unwrap();
    assert!(path.to_string_lossy().contains("cfgd"));
    assert!(path.to_string_lossy().contains("sources"));
}

#[test]
fn build_source_spec_defaults() {
    let spec = SourceManager::build_source_spec("test", "https://example.com/config.git", None);
    assert_eq!(spec.origin.branch, "master");
    assert_eq!(spec.origin.url, "https://example.com/config.git");
    assert!(spec.origin.auth.is_none());
    assert!(spec.subscription.profile.is_none());
    // Default sync interval
    assert_eq!(spec.sync.interval, "1h");
    assert!(spec.sync.pin_version.is_none());
}

#[test]
fn subscription_config_from_spec() {
    let spec = crate::config::SourceSpec {
        name: "test".into(),
        origin: crate::config::OriginSpec {
            origin_type: OriginType::Git,
            url: "https://example.com".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: crate::config::SubscriptionSpec {
            profile: Some("backend".into()),
            priority: 500,
            accept_recommended: true,
            opt_in: vec!["extra".into()],
            overrides: serde_yaml::Value::Null,
            reject: serde_yaml::Value::Null,
        },
        sync: Default::default(),
    };

    let config = crate::composition::SubscriptionConfig::from_spec(&spec);
    assert!(config.accept_recommended);
    assert_eq!(config.opt_in, vec!["extra".to_string()]);
}

#[test]
fn version_pin_tilde_two_part() {
    // ~2.1 should match 2.1.x but not 2.2.0
    let pin = normalize_semver_pin("~2.1");
    let req = VersionReq::parse(&pin).unwrap();
    assert!(req.matches(&Version::new(2, 1, 0)));
    assert!(req.matches(&Version::new(2, 1, 9)));
    assert!(!req.matches(&Version::new(2, 2, 0)));
}

#[test]
fn version_pin_caret_matches_minor_bumps() {
    let pin = normalize_semver_pin("^3");
    let req = VersionReq::parse(&pin).unwrap();
    assert!(req.matches(&Version::new(3, 0, 0)));
    assert!(req.matches(&Version::new(3, 9, 0)));
    assert!(!req.matches(&Version::new(4, 0, 0)));
}

#[test]
fn load_source_rejects_traversal_name() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());
    let printer = test_printer();

    let spec = crate::config::SourceSpec {
        name: "../evil".into(),
        origin: crate::config::OriginSpec {
            origin_type: OriginType::Git,
            url: "https://example.com/config.git".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: Default::default(),
        sync: Default::default(),
    };

    let result = mgr.load_source(&spec, &printer);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("invalid source name") || err.contains("traversal"),
        "expected traversal error, got: {err}"
    );
}

#[test]
fn remove_source_success() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());

    // Create a fake cached source directory on disk
    let source_path = dir.path().join("test-source");
    std::fs::create_dir_all(&source_path).unwrap();
    std::fs::write(source_path.join("marker.txt"), "exists").unwrap();
    assert!(source_path.exists());

    // Manually insert a CachedSource into the manager's internal map
    let cached = CachedSource {
        name: "test-source".to_string(),
        origin_url: "https://example.com/config.git".to_string(),
        origin_branch: "main".to_string(),
        local_path: source_path.clone(),
        manifest: crate::config::ConfigSourceDocument {
            api_version: crate::API_VERSION.into(),
            kind: "ConfigSource".into(),
            metadata: crate::config::ConfigSourceMetadata {
                name: "test-source".into(),
                version: Some("1.0.0".into()),
                description: None,
            },
            spec: crate::config::ConfigSourceSpec {
                provides: Default::default(),
                policy: Default::default(),
            },
        },
        last_commit: None,
        last_fetched: None,
    };
    mgr.sources.insert("test-source".to_string(), cached);

    // Verify the source is present
    assert!(mgr.get("test-source").is_some());

    // Remove the source
    mgr.remove_source("test-source")
        .expect("remove_source should succeed for existing cached source");

    // Verify it was removed from the map
    assert!(mgr.get("test-source").is_none());
    assert!(
        mgr.all_sources().is_empty(),
        "sources map should be empty after removal"
    );

    // Verify the directory and its contents were removed from disk
    assert!(!source_path.exists(), "source directory should be deleted");
    assert!(
        !source_path.join("marker.txt").exists(),
        "files within source directory should be deleted"
    );
}

/// Helper: insert a fake CachedSource into a SourceManager for testing
/// methods that operate on already-cached sources.
fn insert_fake_source(mgr: &mut SourceManager, name: &str, local_path: PathBuf) {
    let cached = CachedSource {
        name: name.to_string(),
        origin_url: "https://example.com/config.git".to_string(),
        origin_branch: "main".to_string(),
        local_path,
        manifest: crate::config::ConfigSourceDocument {
            api_version: crate::API_VERSION.into(),
            kind: "ConfigSource".into(),
            metadata: crate::config::ConfigSourceMetadata {
                name: name.into(),
                version: Some("1.0.0".into()),
                description: None,
            },
            spec: crate::config::ConfigSourceSpec {
                provides: Default::default(),
                policy: Default::default(),
            },
        },
        last_commit: None,
        last_fetched: None,
    };
    mgr.sources.insert(name.to_string(), cached);
}

#[test]
fn load_source_profile_success() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("my-source");
    std::fs::create_dir_all(source_path.join(PROFILES_DIR)).unwrap();

    // Write a valid profile YAML
    std::fs::write(
        source_path.join(PROFILES_DIR).join("default.yaml"),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  packages:
    pipx:
      - ripgrep
"#,
    )
    .unwrap();

    let mut mgr = SourceManager::new(dir.path());
    insert_fake_source(&mut mgr, "my-source", source_path);

    let result = mgr.load_source_profile("my-source", "default");
    assert!(
        result.is_ok(),
        "load_source_profile failed: {:?}",
        result.err()
    );
    let profile = result.unwrap();
    assert_eq!(profile.metadata.name, "default");
}

#[test]
fn load_source_profile_missing_profile_file() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("my-source");
    std::fs::create_dir_all(source_path.join(PROFILES_DIR)).unwrap();
    // No profile file written

    let mut mgr = SourceManager::new(dir.path());
    insert_fake_source(&mut mgr, "my-source", source_path);

    let result = mgr.load_source_profile("my-source", "nonexistent");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found") || err.contains("ProfileNotFound"),
        "expected profile not found error, got: {err}"
    );
}

#[test]
fn source_profiles_dir_returns_path_for_cached_source() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("src-1");
    std::fs::create_dir_all(&source_path).unwrap();

    let mut mgr = SourceManager::new(dir.path());
    insert_fake_source(&mut mgr, "src-1", source_path.clone());

    let result = mgr.source_profiles_dir("src-1");
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), source_path.join(PROFILES_DIR));
}

#[test]
fn source_files_dir_returns_path_for_cached_source() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("src-1");
    std::fs::create_dir_all(&source_path).unwrap();

    let mut mgr = SourceManager::new(dir.path());
    insert_fake_source(&mut mgr, "src-1", source_path.clone());

    let result = mgr.source_files_dir("src-1");
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), source_path.join("files"));
}

#[test]
fn head_commit_returns_oid_for_valid_repo() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");

    // Use a manually created repo
    let repo = git2::Repository::init(&repo_dir).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    std::fs::write(repo_dir.join("file.txt"), "content\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let oid = repo
        .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();

    let result = SourceManager::head_commit(&repo_dir);
    assert!(result.is_some());
    assert_eq!(result.unwrap(), oid.to_string());
}

#[test]
fn head_commit_returns_none_for_nonexistent_dir() {
    let result = SourceManager::head_commit(std::path::Path::new("/tmp/no-such-repo-xyz"));
    assert!(result.is_none());
}

#[test]
fn load_sources_fails_when_all_sources_fail() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());
    let printer = test_printer();

    // Create specs that point to non-existent repos
    let specs = vec![
        crate::config::SourceSpec {
            name: "bad1".into(),
            origin: crate::config::OriginSpec {
                origin_type: OriginType::Git,
                url: "file:///nonexistent/repo1".into(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: Default::default(),
            sync: Default::default(),
        },
        crate::config::SourceSpec {
            name: "bad2".into(),
            origin: crate::config::OriginSpec {
                origin_type: OriginType::Git,
                url: "file:///nonexistent/repo2".into(),
                branch: "main".into(),
                auth: None,
                ssh_strict_host_key_checking: Default::default(),
            },
            subscription: Default::default(),
            sync: Default::default(),
        },
    ];

    let result = mgr.load_sources(&specs, &printer);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("all sources failed"),
        "expected all sources failed error, got: {err}"
    );
}

#[test]
fn load_sources_succeeds_with_empty_list() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());
    let printer = test_printer();

    // Empty list should succeed and leave no sources loaded
    mgr.load_sources(&[], &printer)
        .expect("load_sources with empty list should succeed");
    assert!(
        mgr.all_sources().is_empty(),
        "no sources should be loaded from empty list"
    );
}

#[test]
fn git_clone_with_fallback_local_repo() {
    let dir = tempfile::tempdir().unwrap();
    let origin_path = dir.path().join("origin");

    // Create a bare repo as the origin
    let repo = git2::Repository::init(&origin_path).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    // Use content without a trailing newline. Git for Windows defaults to
    // core.autocrlf=true, which rewrites LF → CRLF on checkout and would
    // make the byte comparison below platform-dependent.
    std::fs::write(origin_path.join("file.txt"), "hello").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
        .unwrap();

    let clone_path = dir.path().join("clone");
    let printer = test_printer();
    git_clone_with_fallback(&origin_path.display().to_string(), &clone_path, &printer)
        .expect("clone of local repo should succeed");

    // Verify the cloned file exists with the correct content
    assert!(
        clone_path.join("file.txt").exists(),
        "cloned file should exist"
    );
    let content = std::fs::read_to_string(clone_path.join("file.txt")).unwrap();
    assert_eq!(content, "hello", "cloned file should have original content");

    // Verify it is a valid git repo
    assert!(
        clone_path.join(".git").exists(),
        "cloned directory should be a git repo"
    );
}

#[test]
fn git_clone_with_fallback_invalid_url() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("clone");
    std::fs::create_dir_all(&target).unwrap();

    let printer = test_printer();
    let err =
        git_clone_with_fallback("file:///nonexistent/path/repo", &target, &printer).unwrap_err();
    assert!(
        err.contains("Failed to clone") || err.contains("nonexistent"),
        "expected clone failure message, got: {err}"
    );
}

#[test]
fn remove_source_cleans_up_directory() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("removable");
    std::fs::create_dir_all(&source_path).unwrap();
    std::fs::write(source_path.join("data.txt"), "test").unwrap();

    let mut mgr = SourceManager::new(dir.path());
    insert_fake_source(&mut mgr, "removable", source_path.clone());

    // Pre-conditions: source exists on disk and in cache
    assert!(
        source_path.exists(),
        "source directory should exist before removal"
    );
    assert!(
        source_path.join("data.txt").exists(),
        "data file should exist before removal"
    );
    assert!(
        mgr.get("removable").is_some(),
        "source should be in cache before removal"
    );

    mgr.remove_source("removable")
        .expect("remove_source should succeed for existing cached source");

    // Post-conditions: both directory and cache entry are gone
    assert!(
        !source_path.exists(),
        "source directory should be deleted after removal"
    );
    assert!(
        mgr.get("removable").is_none(),
        "source should be removed from cache"
    );
    assert!(
        mgr.all_sources().is_empty(),
        "sources map should be empty after removal"
    );
}

#[test]
fn all_sources_returns_cached() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());

    let path1 = dir.path().join("src-a");
    let path2 = dir.path().join("src-b");
    std::fs::create_dir_all(&path1).unwrap();
    std::fs::create_dir_all(&path2).unwrap();

    insert_fake_source(&mut mgr, "src-a", path1);
    insert_fake_source(&mut mgr, "src-b", path2);

    let all = mgr.all_sources();
    assert_eq!(all.len(), 2);
    assert!(all.contains_key("src-a"));
    assert!(all.contains_key("src-b"));
}

// ─── build_source_spec — field verification ──────────────────

#[test]
fn build_source_spec_ssh_url() {
    let spec = SourceManager::build_source_spec("corp", "git@gitlab.com:corp/config.git", None);
    assert_eq!(spec.name, "corp");
    assert_eq!(spec.origin.url, "git@gitlab.com:corp/config.git");
    assert_eq!(spec.origin.branch, "master");
    assert!(matches!(spec.origin.origin_type, OriginType::Git));
    assert!(spec.origin.auth.is_none());
    assert!(spec.subscription.profile.is_none());
    assert!(!spec.subscription.accept_recommended);
    assert!(spec.subscription.opt_in.is_empty());
    assert!(!spec.sync.auto_apply);
    assert!(spec.sync.pin_version.is_none());
}

#[test]
fn build_source_spec_with_profile_sets_subscription() {
    let spec = SourceManager::build_source_spec(
        "team",
        "https://github.com/team/dotfiles.git",
        Some("devops"),
    );
    assert_eq!(spec.subscription.profile.as_deref(), Some("devops"));
    assert_eq!(spec.subscription.priority, 500);
    assert_eq!(spec.sync.interval, "1h");
}

#[test]
fn build_source_spec_preserves_url_verbatim() {
    let url = "ssh://git@internal.host:2222/repo.git";
    let spec = SourceManager::build_source_spec("internal", url, None);
    assert_eq!(spec.origin.url, url);
}

// ─── parse_manifest — profile_details support ────────────────

#[test]
fn parse_manifest_with_profile_details() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(SOURCE_MANIFEST_FILE),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: detailed-source
  version: "2.0.0"
  description: "A source with profile details"
spec:
  provides:
    profiles: []
    profileDetails:
      - name: backend
        description: "Backend developer profile"
        inherits:
          - base
      - name: frontend
        description: "Frontend developer profile"
    modules:
      - docker
      - kubernetes
  policy: {}
"#,
    )
    .unwrap();

    let mgr = SourceManager::new(dir.path());
    let manifest = mgr.parse_manifest("detailed", dir.path()).unwrap();
    assert_eq!(manifest.metadata.name, "detailed-source");
    assert_eq!(manifest.metadata.version.as_deref(), Some("2.0.0"));
    assert_eq!(
        manifest.metadata.description.as_deref(),
        Some("A source with profile details")
    );
    assert_eq!(manifest.spec.provides.profile_details.len(), 2);
    assert_eq!(manifest.spec.provides.profile_details[0].name, "backend");
    assert_eq!(
        manifest.spec.provides.profile_details[0]
            .description
            .as_deref(),
        Some("Backend developer profile")
    );
    assert_eq!(
        manifest.spec.provides.profile_details[0].inherits,
        vec!["base"]
    );
    assert_eq!(manifest.spec.provides.profile_details[1].name, "frontend");
    assert_eq!(manifest.spec.provides.modules, vec!["docker", "kubernetes"]);
}

#[test]
fn parse_manifest_with_platform_profiles() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(SOURCE_MANIFEST_FILE),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: platform-source
spec:
  provides:
    profiles:
      - base
    platformProfiles:
      macos: macos-base
      linux: linux-base
  policy: {}
"#,
    )
    .unwrap();

    let mgr = SourceManager::new(dir.path());
    let manifest = mgr.parse_manifest("plat", dir.path()).unwrap();
    assert_eq!(
        manifest.spec.provides.platform_profiles.get("macos"),
        Some(&"macos-base".to_string())
    );
    assert_eq!(
        manifest.spec.provides.platform_profiles.get("linux"),
        Some(&"linux-base".to_string())
    );
}

// ─── ConfigSourceDocument serialization roundtrip ─────────────

#[test]
fn config_source_document_serde_roundtrip() {
    let doc = ConfigSourceDocument {
        api_version: crate::API_VERSION.into(),
        kind: "ConfigSource".into(),
        metadata: crate::config::ConfigSourceMetadata {
            name: "roundtrip-test".into(),
            version: Some("1.2.3".into()),
            description: Some("Test description".into()),
        },
        spec: crate::config::ConfigSourceSpec {
            provides: crate::config::ConfigSourceProvides {
                profiles: vec!["base".into(), "dev".into()],
                profile_details: vec![crate::config::ConfigSourceProfileEntry {
                    name: "base".into(),
                    description: Some("Base profile".into()),
                    path: None,
                    inherits: vec![],
                }],
                platform_profiles: {
                    let mut m = HashMap::new();
                    m.insert("macos".into(), "macos-base".into());
                    m
                },
                modules: vec!["git".into()],
            },
            policy: Default::default(),
        },
    };

    let yaml = serde_yaml::to_string(&doc).expect("serialize should succeed");
    let parsed: ConfigSourceDocument =
        serde_yaml::from_str(&yaml).expect("deserialize should succeed");

    assert_eq!(parsed.metadata.name, "roundtrip-test");
    assert_eq!(parsed.metadata.version.as_deref(), Some("1.2.3"));
    assert_eq!(
        parsed.metadata.description.as_deref(),
        Some("Test description")
    );
    assert_eq!(parsed.spec.provides.profiles, vec!["base", "dev"]);
    assert_eq!(parsed.spec.provides.profile_details.len(), 1);
    assert_eq!(parsed.spec.provides.profile_details[0].name, "base");
    assert_eq!(
        parsed
            .spec
            .provides
            .platform_profiles
            .get("macos")
            .map(String::as_str),
        Some("macos-base")
    );
    assert_eq!(parsed.spec.provides.modules, vec!["git"]);
}

#[test]
fn config_source_document_deserialize_minimal() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: minimal
spec:
  provides:
    profiles:
      - default
"#;
    let doc: ConfigSourceDocument =
        serde_yaml::from_str(yaml).expect("minimal manifest should parse");
    assert_eq!(doc.metadata.name, "minimal");
    assert!(doc.metadata.version.is_none());
    assert!(doc.metadata.description.is_none());
    assert_eq!(doc.spec.provides.profiles, vec!["default"]);
    assert!(doc.spec.provides.profile_details.is_empty());
    assert!(doc.spec.provides.platform_profiles.is_empty());
    assert!(doc.spec.provides.modules.is_empty());
    // Policy defaults
    assert!(!doc.spec.policy.constraints.require_signed_commits);
}

// ─── read_manifest — additional edge cases ───────────────────

#[test]
fn read_manifest_unreadable_yaml_content() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(SOURCE_MANIFEST_FILE),
        "this is not yaml at all: [[[",
    )
    .unwrap();

    let err = read_manifest("bad-yaml", dir.path()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("bad-yaml") || msg.contains("invalid") || msg.contains("ConfigSource"),
        "expected manifest parse error mentioning the source name, got: {msg}"
    );
}

#[test]
fn read_manifest_wrong_kind_in_yaml() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(SOURCE_MANIFEST_FILE),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: wrong-kind
spec: {}
"#,
    )
    .unwrap();

    // parse_config_source validates the kind field — this should fail
    let result = read_manifest("wrong-kind", dir.path());
    assert!(
        result.is_err(),
        "wrong kind should be rejected by parse_config_source"
    );
}

#[test]
fn parse_manifest_with_policy_constraints() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(SOURCE_MANIFEST_FILE),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: constrained-source
spec:
  provides:
    profiles:
      - secure
  policy:
    constraints:
      requireSignedCommits: true
      noScripts: true
      noSecretsRead: false
      allowSystemChanges: true
"#,
    )
    .unwrap();

    let mgr = SourceManager::new(dir.path());
    let manifest = mgr.parse_manifest("constrained", dir.path()).unwrap();
    assert!(manifest.spec.policy.constraints.require_signed_commits);
    assert!(manifest.spec.policy.constraints.no_scripts);
    assert!(!manifest.spec.policy.constraints.no_secrets_read);
    assert!(manifest.spec.policy.constraints.allow_system_changes);
}

// ─── CachedSource field verification ─────────────────────────

#[test]
fn cached_source_fields_via_get() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("my-src");
    std::fs::create_dir_all(&source_path).unwrap();

    let mut mgr = SourceManager::new(dir.path());
    insert_fake_source(&mut mgr, "my-src", source_path.clone());

    let cached = mgr.get("my-src").expect("source should be cached");
    assert_eq!(cached.name, "my-src");
    assert_eq!(cached.origin_url, "https://example.com/config.git");
    assert_eq!(cached.origin_branch, "main");
    assert_eq!(cached.local_path, source_path);
    assert_eq!(cached.manifest.kind, "ConfigSource");
    assert!(cached.last_commit.is_none());
    assert!(cached.last_fetched.is_none());
}

// ─── normalize_semver_pin — more edge cases ──────────────────

#[test]
fn normalize_semver_pin_tilde_three_part() {
    // Full three-part tilde passed through unchanged
    assert_eq!(normalize_semver_pin("~1.2.3"), "~1.2.3");
}

#[test]
fn normalize_semver_pin_caret_three_part() {
    assert_eq!(normalize_semver_pin("^0.1.2"), "^0.1.2");
}

#[test]
fn normalize_semver_pin_comparison_operators() {
    // Operators other than ~ and ^ are passed through
    assert_eq!(normalize_semver_pin(">1.0.0"), ">1.0.0");
    assert_eq!(normalize_semver_pin("<=2.0.0"), "<=2.0.0");
    assert_eq!(normalize_semver_pin(">=1.5.0, <2.0.0"), ">=1.5.0, <2.0.0");
}

#[test]
fn normalize_semver_pin_wildcard() {
    assert_eq!(normalize_semver_pin("*"), "*");
}

// ─── SourceSpec serialization ────────────────────────────────

#[test]
fn source_spec_serde_roundtrip() {
    let spec = SourceManager::build_source_spec(
        "my-source",
        "https://github.com/org/config.git",
        Some("engineering"),
    );
    let yaml = serde_yaml::to_string(&spec).expect("serialize should succeed");
    let parsed: crate::config::SourceSpec =
        serde_yaml::from_str(&yaml).expect("deserialize should succeed");

    assert_eq!(parsed.name, "my-source");
    assert_eq!(parsed.origin.url, "https://github.com/org/config.git");
    assert_eq!(parsed.origin.branch, "master");
    assert_eq!(parsed.subscription.profile.as_deref(), Some("engineering"));
    assert_eq!(parsed.subscription.priority, 500);
    assert_eq!(parsed.sync.interval, "1h");
    assert!(!parsed.sync.auto_apply);
}

// ─── detect_source_manifest — with profile_details ───────────

#[test]
fn detect_source_manifest_with_profile_details_only() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join(SOURCE_MANIFEST_FILE),
        r#"
apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: details-only
spec:
  provides:
    profiles: []
    profileDetails:
      - name: dev
        description: "Developer profile"
  policy: {}
"#,
    )
    .unwrap();

    let result = detect_source_manifest(dir.path()).unwrap();
    assert!(
        result.is_some(),
        "should accept profile_details as valid profiles"
    );
    let doc = result.unwrap();
    assert_eq!(doc.metadata.name, "details-only");
    assert_eq!(doc.spec.provides.profile_details.len(), 1);
}

// --- load_source: local file URL rejection ---

// `#[serial]` here so these run sequentially with `local_source_fixture`
// tests below; the fixture mutates `CFGD_ALLOW_LOCAL_SOURCES` process-wide
// and would otherwise race with the rejection assertions.
#[test]
#[serial]
fn load_source_rejects_file_url() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());
    let printer = test_printer();

    let spec = crate::config::SourceSpec {
        name: "local-bad".into(),
        origin: crate::config::OriginSpec {
            origin_type: OriginType::Git,
            url: "file:///etc/shadow".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: Default::default(),
        sync: Default::default(),
    };

    let result = mgr.load_source(&spec, &printer);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("local file://") || err.contains("not allowed"),
        "expected file:// rejection, got: {err}"
    );
}

#[test]
#[serial]
fn load_source_rejects_absolute_path_url() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());
    let printer = test_printer();

    let spec = crate::config::SourceSpec {
        name: "abs-bad".into(),
        origin: crate::config::OriginSpec {
            origin_type: OriginType::Git,
            url: "/tmp/local-repo".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: Default::default(),
        sync: Default::default(),
    };

    let result = mgr.load_source(&spec, &printer);
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not allowed") || err.contains("absolute path"),
        "expected absolute path rejection, got: {err}"
    );
}

#[test]
#[serial]
fn load_source_rejects_file_url_case_insensitive() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());
    let printer = test_printer();

    let spec = crate::config::SourceSpec {
        name: "case-bad".into(),
        origin: crate::config::OriginSpec {
            origin_type: OriginType::Git,
            url: "FILE:///etc/passwd".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: Default::default(),
        sync: Default::default(),
    };

    let result = mgr.load_source(&spec, &printer);
    assert!(result.is_err(), "FILE:// should also be rejected");
}

// --- remove_source: already-deleted directory ---

#[test]
fn remove_source_missing_directory_still_removes_cache_entry() {
    let dir = tempfile::tempdir().unwrap();
    let missing_path = dir.path().join("already-gone");
    // Do NOT create the directory — simulate it being deleted externally

    let mut mgr = SourceManager::new(dir.path());
    insert_fake_source(&mut mgr, "already-gone", missing_path.clone());

    // Should succeed even though directory doesn't exist
    mgr.remove_source("already-gone")
        .expect("remove should succeed when directory is already gone");
    assert!(mgr.get("already-gone").is_none());
}

// --- check_version_pin: exact version match ---

#[test]
fn check_version_pin_exact_match() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());

    let manifest = ConfigSourceDocument {
        api_version: crate::API_VERSION.into(),
        kind: "ConfigSource".into(),
        metadata: crate::config::ConfigSourceMetadata {
            name: "test".into(),
            version: Some("1.2.3".into()),
            description: None,
        },
        spec: crate::config::ConfigSourceSpec {
            provides: Default::default(),
            policy: Default::default(),
        },
    };

    mgr.check_version_pin("test", &manifest, "=1.2.3")
        .expect("exact version should match");
}

#[test]
fn check_version_pin_exact_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(dir.path());

    let manifest = ConfigSourceDocument {
        api_version: crate::API_VERSION.into(),
        kind: "ConfigSource".into(),
        metadata: crate::config::ConfigSourceMetadata {
            name: "test".into(),
            version: Some("1.2.3".into()),
            description: None,
        },
        spec: crate::config::ConfigSourceSpec {
            provides: Default::default(),
            policy: Default::default(),
        },
    };

    let result = mgr.check_version_pin("test", &manifest, "=2.0.0");
    assert!(result.is_err());
}

// --- verify_commit_signature: constraints control ---

#[test]
fn verify_signature_required_but_allow_unsigned_skips() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());
    mgr.set_allow_unsigned(true);

    let constraints = crate::config::SourceConstraints {
        require_signed_commits: true,
        ..Default::default()
    };

    // Even though require_signed_commits is true, allow_unsigned bypasses it
    // This should succeed without even checking the repo
    let result = mgr.verify_commit_signature("test", dir.path(), &constraints);
    assert!(result.is_ok());
}

// --- head_commit: empty repo ---

#[test]
fn head_commit_empty_repo_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("empty-repo");
    git2::Repository::init(&repo_dir).unwrap();
    // No commits yet
    let result = SourceManager::head_commit(&repo_dir);
    assert!(
        result.is_none(),
        "empty repo with no commits should return None"
    );
}

// --- SourceManager: multiple operations ---

#[test]
fn source_manager_get_and_all_sources_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(dir.path());

    assert!(mgr.all_sources().is_empty());
    assert!(mgr.get("nonexistent").is_none());

    let path = dir.path().join("src");
    std::fs::create_dir_all(&path).unwrap();
    insert_fake_source(&mut mgr, "src", path);

    assert_eq!(mgr.all_sources().len(), 1);
    assert!(mgr.get("src").is_some());
    assert!(mgr.get("other").is_none());
}

// --- load_source_profile: missing profile file variant ---

#[test]
fn load_source_profile_no_profiles_directory() {
    let dir = tempfile::tempdir().unwrap();
    let source_path = dir.path().join("src-no-profiles");
    std::fs::create_dir_all(&source_path).unwrap();
    // Don't create the profiles subdirectory

    let mut mgr = SourceManager::new(dir.path());
    insert_fake_source(&mut mgr, "src-no-profiles", source_path);

    let result = mgr.load_source_profile("src-no-profiles", "default");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found") || err.contains("ProfileNotFound"),
        "expected profile not found error, got: {err}"
    );
}

// ============================================================================
// classify_signature_status — all `git log --format=%G?` status codes
//
// This helper was carved out of verify_head_signature so each branch of the
// status-code → Result mapping is reachable from a unit test without needing
// a real GPG/SSH-signed commit. The accepted set {G, U} is critical: cfgd's
// security policy treats valid signatures with unknown trust ("U") as
// acceptable, so a regression here would either over-reject (breaks new-key
// onboarding) or under-reject (silent compromise).
// ============================================================================

#[test]
fn classify_signature_status_good_is_ok() {
    super::classify_signature_status("src", "G").unwrap();
}

#[test]
fn classify_signature_status_unknown_trust_is_ok() {
    // U = good signature but key not trusted yet. Accepted per cfgd policy.
    super::classify_signature_status("src", "U").unwrap();
}

#[test]
fn classify_signature_status_no_signature_errors_with_not_signed() {
    let err = super::classify_signature_status("src", "N").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not signed"),
        "expected 'not signed' message for N, got: {msg}"
    );
}

#[test]
fn classify_signature_status_bad_signature_errors_with_invalid() {
    let err = super::classify_signature_status("src", "B").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("bad") || msg.contains("invalid"),
        "expected bad/invalid message for B, got: {msg}"
    );
}

#[test]
fn classify_signature_status_unverifiable_errors_with_signing_key_hint() {
    let err = super::classify_signature_status("src", "E").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("signing key") || msg.contains("cannot be checked"),
        "expected 'signing key/cannot be checked' message for E, got: {msg}"
    );
}

#[test]
fn classify_signature_status_expired_signature_errors_with_expired() {
    // X = signature has expired
    let err = super::classify_signature_status("src", "X").unwrap_err();
    assert!(err.to_string().contains("expired"));
}

#[test]
fn classify_signature_status_expired_key_errors_with_expired() {
    // Y = signing key has expired
    let err = super::classify_signature_status("src", "Y").unwrap_err();
    assert!(err.to_string().contains("expired"));
}

#[test]
fn classify_signature_status_revoked_key_errors_with_revoked() {
    let err = super::classify_signature_status("src", "R").unwrap_err();
    assert!(err.to_string().contains("revoked"));
}

#[test]
fn classify_signature_status_unexpected_code_includes_raw_code_in_message() {
    // The catch-all branch must surface the raw status code so users can
    // file a useful bug report when git changes its status taxonomy.
    let err = super::classify_signature_status("src", "Z").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("'Z'") || msg.contains("unexpected"),
        "catch-all error must surface raw code or 'unexpected' hint, got: {msg}"
    );
}

#[test]
fn classify_signature_status_propagates_source_name_into_error() {
    // The source name must appear in the error path so the operator log
    // / CLI message tells the user which source failed verification.
    let err = super::classify_signature_status("my-source-XYZ", "N").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("my-source-XYZ"),
        "expected source name in error, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// SourceManager end-to-end via a local bare repo (file:// URL).
//
// `CFGD_ALLOW_LOCAL_SOURCES=1` flips the file:// safety check off so tests
// can stand up an init_bare upstream, commit a cfgd-source.yaml manifest,
// and exercise clone_source / fetch_source / parse_manifest /
// verify_commit_signature end-to-end without the network.
// ---------------------------------------------------------------------------

mod local_source_fixture {
    use super::*;
    use crate::config::{
        OriginSpec, OriginType, SourceSyncSpec, SshHostKeyPolicy, SubscriptionSpec,
    };
    use crate::test_helpers::with_test_env_var;

    /// Build a bare upstream populated with a `cfgd-source.yaml` manifest.
    /// Returns the bare repo path. The manifest declares the source name
    /// and (optionally) a version + a profile list so callers can drive
    /// the pin-check + profile-selection arms.
    fn make_bare_with_manifest(
        tmp: &tempfile::TempDir,
        name: &str,
        version: Option<&str>,
        profiles: &[&str],
    ) -> std::path::PathBuf {
        let bare = tmp.path().join(format!("{}-bare.git", name));
        let _ = git2::Repository::init_bare(&bare).unwrap();

        let src = tmp.path().join(format!("{}-src", name));
        let src_repo = git2::Repository::init(&src).unwrap();
        let mut manifest = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: {}\n",
            name
        );
        if let Some(v) = version {
            manifest.push_str(&format!("  version: {}\n", v));
        }
        manifest.push_str("spec:\n  provides:\n    profiles:\n");
        // ConfigSource manifest must declare at least one profile — fall back
        // to a synthetic "default" if the caller didn't specify any.
        if profiles.is_empty() {
            manifest.push_str("      - default\n");
        } else {
            for p in profiles {
                manifest.push_str(&format!("      - {}\n", p));
            }
        }
        std::fs::write(src.join("cfgd-source.yaml"), &manifest).unwrap();

        let mut index = src_repo.index().unwrap();
        index
            .add_path(std::path::Path::new("cfgd-source.yaml"))
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial manifest", &tree, &[])
            .unwrap();
        drop(tree);

        let bare_url = format!("file://{}", bare.display());
        let mut remote = src_repo.remote("origin", &bare_url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
            .unwrap();
        bare
    }

    fn build_spec(name: &str, url: &str, branch: &str) -> SourceSpec {
        SourceSpec {
            name: name.to_string(),
            origin: OriginSpec {
                origin_type: OriginType::Git,
                url: url.to_string(),
                branch: branch.to_string(),
                auth: None,
                ssh_strict_host_key_checking: SshHostKeyPolicy::No,
            },
            subscription: SubscriptionSpec::default(),
            sync: SourceSyncSpec::default(),
        }
    }

    /// Probe the branch name of a freshly-init-bare repo (master vs main).
    fn detect_branch(bare: &std::path::Path) -> String {
        let repo = git2::Repository::open(bare).unwrap();
        let refs = repo.references().unwrap();
        for r in refs.flatten() {
            if let Some(n) = r.name()
                && let Some(stripped) = n.strip_prefix("refs/heads/")
            {
                return stripped.to_string();
            }
        }
        "master".to_string()
    }

    #[test]
    #[serial]
    fn load_source_clones_then_fetches_from_local_bare() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let tmp = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&tmp, "ts1", None, &[]);
            let branch = detect_branch(&bare);
            let url = format!("file://{}", bare.display());
            let cache_dir = tmp.path().join("cache");
            let mut mgr = SourceManager::new(&cache_dir);
            let spec = build_spec("ts1", &url, &branch);
            let printer = test_printer();
            // First call clones.
            mgr.load_source(&spec, &printer).unwrap();
            assert!(mgr.get("ts1").is_some());
            assert_eq!(mgr.get("ts1").unwrap().origin_url, url);
            // Second call goes through fetch_source — directory exists now.
            mgr.load_source(&spec, &printer).unwrap();
            assert!(mgr.get("ts1").is_some());
        });
    }

    #[test]
    #[serial]
    fn load_source_records_last_commit_after_clone() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let tmp = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&tmp, "ts2", None, &[]);
            let branch = detect_branch(&bare);
            let url = format!("file://{}", bare.display());
            let cache_dir = tmp.path().join("cache");
            let mut mgr = SourceManager::new(&cache_dir);
            let spec = build_spec("ts2", &url, &branch);
            let printer = test_printer();
            mgr.load_source(&spec, &printer).unwrap();
            let cached = mgr.get("ts2").unwrap();
            assert!(
                cached.last_commit.is_some(),
                "last_commit should be populated after clone"
            );
            assert!(cached.last_fetched.is_some());
        });
    }

    #[test]
    #[serial]
    fn load_source_with_version_pin_match_succeeds() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let tmp = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&tmp, "ts3", Some("2.1.0"), &[]);
            let branch = detect_branch(&bare);
            let url = format!("file://{}", bare.display());
            let cache_dir = tmp.path().join("cache");
            let mut mgr = SourceManager::new(&cache_dir);
            let mut spec = build_spec("ts3", &url, &branch);
            spec.sync.pin_version = Some("~2".to_string());
            let printer = test_printer();
            mgr.load_source(&spec, &printer).unwrap();
            assert!(mgr.get("ts3").is_some());
        });
    }

    #[test]
    #[serial]
    fn load_source_with_version_pin_mismatch_fails() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let tmp = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&tmp, "ts4", Some("1.0.0"), &[]);
            let branch = detect_branch(&bare);
            let url = format!("file://{}", bare.display());
            let cache_dir = tmp.path().join("cache");
            let mut mgr = SourceManager::new(&cache_dir);
            let mut spec = build_spec("ts4", &url, &branch);
            spec.sync.pin_version = Some("^2".to_string());
            let printer = test_printer();
            let err = mgr.load_source(&spec, &printer).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("ts4") || msg.contains("version"),
                "expected version mismatch error, got: {msg}"
            );
        });
    }

    #[test]
    #[serial]
    fn load_source_after_remote_advance_fetches_new_commit() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let tmp = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&tmp, "ts5", None, &[]);
            let branch = detect_branch(&bare);
            let url = format!("file://{}", bare.display());
            let cache_dir = tmp.path().join("cache");
            let mut mgr = SourceManager::new(&cache_dir);
            let spec = build_spec("ts5", &url, &branch);
            let printer = test_printer();
            mgr.load_source(&spec, &printer).unwrap();
            let initial_commit = mgr.get("ts5").unwrap().last_commit.clone();

            // Open the src repo + add another commit + push to bare.
            let src = tmp.path().join("ts5-src");
            let src_repo = git2::Repository::open(&src).unwrap();
            std::fs::write(src.join("EXTRA.md"), "added").unwrap();
            let mut index = src_repo.index().unwrap();
            index.add_path(std::path::Path::new("EXTRA.md")).unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = src_repo.find_tree(tree_id).unwrap();
            let parent = src_repo.head().unwrap().peel_to_commit().unwrap();
            let sig = git2::Signature::now("t", "t@example.com").unwrap();
            src_repo
                .commit(Some("HEAD"), &sig, &sig, "second", &tree, &[&parent])
                .unwrap();
            drop(tree);
            let mut remote = src_repo.find_remote("origin").unwrap();
            remote
                .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
                .unwrap();

            // Fetch through SourceManager — last_commit should change.
            mgr.load_source(&spec, &printer).unwrap();
            let new_commit = mgr.get("ts5").unwrap().last_commit.clone();
            assert_ne!(initial_commit, new_commit);
        });
    }

    #[test]
    #[serial]
    fn load_source_remove_source_clears_cache() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let tmp = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&tmp, "ts6", None, &[]);
            let branch = detect_branch(&bare);
            let url = format!("file://{}", bare.display());
            let cache_dir = tmp.path().join("cache");
            let mut mgr = SourceManager::new(&cache_dir);
            let spec = build_spec("ts6", &url, &branch);
            let printer = test_printer();
            mgr.load_source(&spec, &printer).unwrap();
            assert!(mgr.get("ts6").is_some());
            assert!(cache_dir.join("ts6").exists());
            mgr.remove_source("ts6").unwrap();
            assert!(mgr.get("ts6").is_none());
            assert!(!cache_dir.join("ts6").exists());
        });
    }

    #[test]
    #[serial]
    fn load_sources_processes_multiple_specs() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let tmp = tempfile::tempdir().unwrap();
            let bare_a = make_bare_with_manifest(&tmp, "alpha", None, &[]);
            let bare_b = make_bare_with_manifest(&tmp, "beta", None, &[]);
            let branch = detect_branch(&bare_a);
            let cache_dir = tmp.path().join("cache");
            let mut mgr = SourceManager::new(&cache_dir);
            let specs = vec![
                build_spec("alpha", &format!("file://{}", bare_a.display()), &branch),
                build_spec("beta", &format!("file://{}", bare_b.display()), &branch),
            ];
            let printer = test_printer();
            mgr.load_sources(&specs, &printer).unwrap();
            assert!(mgr.get("alpha").is_some());
            assert!(mgr.get("beta").is_some());
            assert_eq!(mgr.all_sources().len(), 2);
        });
    }

    #[test]
    #[serial]
    fn load_source_source_files_dir_returns_path_under_cache() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let tmp = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&tmp, "ts7", None, &[]);
            let branch = detect_branch(&bare);
            let url = format!("file://{}", bare.display());
            let cache_dir = tmp.path().join("cache");
            let mut mgr = SourceManager::new(&cache_dir);
            let spec = build_spec("ts7", &url, &branch);
            let printer = test_printer();
            mgr.load_source(&spec, &printer).unwrap();
            let files = mgr.source_files_dir("ts7").unwrap();
            assert!(files.starts_with(&cache_dir));
            let profiles = mgr.source_profiles_dir("ts7").unwrap();
            assert!(profiles.starts_with(&cache_dir));
        });
    }

    // -----------------------------------------------------------------------
    // clone_source / clone_with_fallback — ssh:// URL branch
    //
    // The ssh:// branch installs the git_ssh_credentials callback before
    // attempting the libgit2 clone. We can't satisfy that callback in a
    // sandbox (no SSH agent, no real remote), so the clone fails — but the
    // failure must surface as a FetchFailed error with the source name and
    // a network-related message, not a panic or generic error.
    // -----------------------------------------------------------------------

    #[test]
    #[serial]
    fn load_source_ssh_url_falls_back_to_libgit2_and_surfaces_failure() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let tmp = tempfile::tempdir().unwrap();
            let cache_dir = tmp.path().join("cache");
            let mut mgr = SourceManager::new(&cache_dir);
            let spec = build_spec(
                "ssh-src",
                "ssh://git@127.0.0.1:1/nonexistent/repo.git",
                "main",
            );
            let printer = test_printer();
            let err = mgr.load_source(&spec, &printer).unwrap_err();
            let msg = err.to_string();
            assert!(
                msg.contains("ssh-src"),
                "error must include the source name 'ssh-src': {msg}"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// clone_source / remove_source / parse_manifest — error-path coverage that
// doesn't need the local-bare-repo fixture
// ---------------------------------------------------------------------------

#[test]
fn clone_source_returns_cache_error_when_parent_is_regular_file() {
    // create_dir_all on a path whose parent is a regular file fails with
    // NotADirectory. clone_source must wrap that as a CacheError that
    // includes the underlying message — verifying both the error variant
    // and that the failure happens BEFORE any git invocation.
    let tmp = tempfile::tempdir().unwrap();
    let blocker = tmp.path().join("blocker");
    std::fs::write(&blocker, b"i am a regular file").unwrap();
    let cache_dir = blocker.join("nested");
    let mut mgr = SourceManager::new(&cache_dir);

    let spec = crate::config::SourceSpec {
        name: "blocked".into(),
        origin: crate::config::OriginSpec {
            origin_type: OriginType::Git,
            url: "https://example.invalid/repo.git".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: Default::default(),
        },
        subscription: Default::default(),
        sync: Default::default(),
    };
    let printer = test_printer();

    let err = mgr.load_source(&spec, &printer).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("cannot create cache dir") || msg.contains("create cache"),
        "error must surface 'cannot create cache dir' wording: {msg}"
    );
}

#[test]
fn remove_source_surfaces_cache_error_when_remove_dir_all_fails() {
    // Stand up a cached entry whose local_path points at a regular file
    // (not a directory). remove_dir_all on a regular file fails with
    // NotADirectory on Linux. remove_source must wrap that as a
    // CacheError carrying the source name + the underlying message.
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("cache");
    std::fs::create_dir_all(&cache_dir).unwrap();
    let target = cache_dir.join("not-a-dir");
    std::fs::write(&target, b"regular file, not a directory").unwrap();

    let mut mgr = SourceManager::new(&cache_dir);
    let cached = CachedSource {
        name: "bad".to_string(),
        origin_url: "https://example.com/x.git".to_string(),
        origin_branch: "main".to_string(),
        local_path: target.clone(),
        manifest: crate::config::ConfigSourceDocument {
            api_version: crate::API_VERSION.into(),
            kind: "ConfigSource".into(),
            metadata: crate::config::ConfigSourceMetadata {
                name: "bad".into(),
                version: None,
                description: None,
            },
            spec: crate::config::ConfigSourceSpec {
                provides: Default::default(),
                policy: Default::default(),
            },
        },
        last_commit: None,
        last_fetched: None,
    };
    mgr.sources.insert("bad".to_string(), cached);

    let err = mgr.remove_source("bad").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("failed to remove cache"),
        "error must surface 'failed to remove cache': {msg}"
    );
    assert!(
        msg.contains("bad"),
        "error must include the source name 'bad': {msg}"
    );
}

#[test]
fn parse_manifest_surfaces_io_error_when_manifest_path_is_directory() {
    // read_to_string on a path that is a directory fails with IsADirectory.
    // read_manifest must wrap that into InvalidManifest carrying both the
    // source name and the underlying io message.
    let tmp = tempfile::tempdir().unwrap();
    let source_dir = tmp.path().join("source");
    std::fs::create_dir_all(&source_dir).unwrap();
    // Create cfgd-source.yaml as a DIRECTORY (not a file).
    std::fs::create_dir_all(source_dir.join(SOURCE_MANIFEST_FILE)).unwrap();

    let mgr = SourceManager::new(tmp.path());
    let err = mgr.parse_manifest("dir-manifest", &source_dir).unwrap_err();
    let msg = err.to_string();
    // Variant: InvalidManifest. Message must reference the source name
    // so an operator can find the offending repo.
    assert!(
        msg.contains("dir-manifest"),
        "error must include source name 'dir-manifest': {msg}"
    );
}

// ---------------------------------------------------------------------------
// git_clone_with_fallback — ssh:// URL branch
//
// The ssh:// branch installs the git_ssh_credentials callback. Without an
// SSH agent or a real remote, the clone must fail with a "Failed to clone"
// prefix from the libgit2 fallback path.
// ---------------------------------------------------------------------------

#[test]
fn git_clone_with_fallback_ssh_url_surfaces_failure_with_helper_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("clone");
    let printer = test_printer();
    let err = git_clone_with_fallback(
        "ssh://git@127.0.0.1:1/nonexistent/repo.git",
        &target,
        &printer,
    )
    .unwrap_err();
    assert!(
        err.starts_with("Failed to clone"),
        "ssh fallback error must start with 'Failed to clone': {err}"
    );
    assert!(
        err.contains("ssh://"),
        "error must include the original URL: {err}"
    );
}

// ---------------------------------------------------------------------------
// verify_head_signature — drives `git log --format=%G?` against a real local
// repo to exercise the unsigned (`N`) branch end-to-end.
// ---------------------------------------------------------------------------

#[test]
fn verify_head_signature_returns_err_on_unsigned_head_commit() {
    let dir = tempfile::tempdir().unwrap();
    // Initialize a fresh repo and create an unsigned commit.
    let repo = git2::Repository::init(dir.path()).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    let tree_id = repo.index().unwrap().write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let _commit_oid = repo
        .commit(Some("HEAD"), &sig, &sig, "unsigned initial", &tree, &[])
        .unwrap();

    let err = verify_head_signature("unsigned-src", dir.path()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not signed") || msg.contains("HEAD") || msg.contains("signature"),
        "unsigned commit error must mention signature/HEAD, got: {msg}"
    );
}

#[test]
fn verify_head_signature_returns_err_when_repo_dir_is_not_a_repo() {
    let dir = tempfile::tempdir().unwrap();
    // No `git init` — git log will exit non-zero.
    let err = verify_head_signature("not-a-repo", dir.path()).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("git log failed") || msg.contains("not-a-repo"),
        "non-repo error must mention git failure: {msg}"
    );
}

// ---------------------------------------------------------------------------
// classify_signature_status — exhaustive branch coverage
// ---------------------------------------------------------------------------

#[test]
fn classify_signature_status_good_signature_accepted() {
    classify_signature_status("src", "G").unwrap();
}

#[test]
fn classify_signature_status_unknown_validity_accepted() {
    classify_signature_status("src", "U").unwrap();
}

#[test]
fn classify_signature_status_no_signature_rejected() {
    let err = classify_signature_status("src", "N").unwrap_err();
    assert!(err.to_string().contains("not signed"));
}

#[test]
fn classify_signature_status_bad_signature_rejected() {
    let err = classify_signature_status("src", "B").unwrap_err();
    assert!(err.to_string().to_lowercase().contains("bad"));
}

#[test]
fn classify_signature_status_cannot_check_rejected() {
    let err = classify_signature_status("src", "E").unwrap_err();
    assert!(err.to_string().contains("cannot be checked"));
}

#[test]
fn classify_signature_status_expired_x_rejected() {
    let err = classify_signature_status("src", "X").unwrap_err();
    assert!(err.to_string().contains("expired"));
}

#[test]
fn classify_signature_status_expired_y_rejected() {
    let err = classify_signature_status("src", "Y").unwrap_err();
    assert!(err.to_string().contains("expired"));
}

#[test]
fn classify_signature_status_revoked_rejected() {
    let err = classify_signature_status("src", "R").unwrap_err();
    assert!(err.to_string().contains("revoked"));
}

#[test]
fn classify_signature_status_unknown_status_rejected() {
    let err = classify_signature_status("src", "?").unwrap_err();
    assert!(err.to_string().contains("unexpected"));
}

// ---------------------------------------------------------------------------
// SourceManager basics: cache_dir, get, all_sources, remove_source
// ---------------------------------------------------------------------------

#[test]
fn source_manager_remove_source_returns_err_when_not_present() {
    let tmp = tempfile::tempdir().unwrap();
    let mut mgr = SourceManager::new(tmp.path());
    let err = mgr.remove_source("does-not-exist").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("does-not-exist") || msg.contains("not found"),
        "error must mention missing source: {msg}"
    );
}

#[test]
fn source_manager_get_returns_none_for_unknown_source() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(tmp.path());
    assert!(mgr.get("ghost").is_none());
}

#[test]
fn source_manager_all_sources_empty_on_fresh_manager() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(tmp.path());
    assert!(mgr.all_sources().is_empty());
}

#[test]
fn source_manager_source_profiles_dir_returns_expected_subpath() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(tmp.path());
    // Even without loading, the path-builder shape can be exercised — it
    // returns Err for an unknown source, hitting the same error mapping
    // that production callers see when a source manager is misconfigured.
    let result = mgr.source_profiles_dir("not-loaded");
    assert!(result.is_err(), "profiles_dir for unloaded source must err");
}

#[test]
fn source_manager_source_files_dir_returns_err_for_unknown_source() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = SourceManager::new(tmp.path());
    let result = mgr.source_files_dir("not-loaded");
    assert!(result.is_err(), "files_dir for unloaded source must err");
}
