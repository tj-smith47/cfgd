use super::*;

#[test]
fn current_version_is_valid_semver() {
    let v = current_version().expect("CARGO_PKG_VERSION should be valid semver");
    assert_eq!(
        v.to_string(),
        env!("CARGO_PKG_VERSION"),
        "parsed version should round-trip to the compiled version string"
    );
    assert!(
        v.major > 0 || v.minor > 0 || v.patch > 0,
        "version should be non-zero: {v}"
    );
}

#[test]
fn parse_checksums_basic() {
    let content =
        "abc123  cfgd-0.2.0-linux-x86_64.tar.gz\ndef456  cfgd-0.2.0-darwin-aarch64.tar.gz\n";
    let map = parse_checksums(content);
    assert_eq!(map.len(), 2);
    assert_eq!(
        map.get("cfgd-0.2.0-linux-x86_64.tar.gz"),
        Some(&"abc123".to_string())
    );
    assert_eq!(
        map.get("cfgd-0.2.0-darwin-aarch64.tar.gz"),
        Some(&"def456".to_string())
    );
}

#[test]
fn parse_checksums_empty_lines() {
    let content = "\nabc123  foo.tar.gz\n\n";
    let map = parse_checksums(content);
    assert_eq!(map.len(), 1);
}

#[test]
fn parse_release_json_valid() {
    let json = r#"{
            "tag_name": "v0.2.0",
            "assets": [
                {
                    "name": "cfgd-0.2.0-linux-x86_64.tar.gz",
                    "browser_download_url": "https://example.com/cfgd-0.2.0-linux-x86_64.tar.gz",
                    "size": 1024
                },
                {
                    "name": "cfgd-0.2.0-checksums.txt",
                    "browser_download_url": "https://example.com/cfgd-0.2.0-checksums.txt",
                    "size": 256
                }
            ]
        }"#;

    let release = parse_release_json(json).expect("should parse");
    assert_eq!(release.tag, "v0.2.0");
    assert_eq!(release.version, Version::new(0, 2, 0));
    assert_eq!(release.assets.len(), 2);
    assert_eq!(release.assets[0].name, "cfgd-0.2.0-linux-x86_64.tar.gz");
}

#[test]
fn parse_release_json_no_v_prefix() {
    let json = r#"{
            "tag_name": "0.3.0",
            "assets": []
        }"#;

    let release = parse_release_json(json).expect("should parse");
    assert_eq!(release.version, Version::new(0, 3, 0));
}

#[test]
fn parse_release_json_missing_tag() {
    let json = r#"{"assets": []}"#;
    let err = parse_release_json(json).unwrap_err().to_string();
    assert!(
        err.contains("missing tag_name"),
        "error should mention missing tag_name: {err}"
    );
}

#[test]
fn find_asset_matches_current_platform() {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let archive_os = if os == "macos" { "darwin" } else { os };

    #[cfg(unix)]
    let suffix = ".tar.gz";
    #[cfg(windows)]
    let suffix = ".zip";
    let expected_name = format!("cfgd-0.2.0-{}-{}{}", archive_os, arch, suffix);

    let release = ReleaseInfo {
        tag: "v0.2.0".into(),
        version: Version::new(0, 2, 0),
        assets: vec![
            ReleaseAsset {
                name: expected_name.clone(),
                download_url: "https://example.com/match".into(),
                size: 1024,
            },
            ReleaseAsset {
                name: "cfgd-0.2.0-freebsd-riscv64.tar.gz".into(),
                download_url: "https://example.com/other".into(),
                size: 1024,
            },
        ],
    };

    let asset = find_asset_for_platform(&release).expect("should find platform asset");
    assert_eq!(asset.name, expected_name);
    assert_eq!(asset.download_url, "https://example.com/match");
}

#[test]
fn find_asset_returns_error_when_missing() {
    let release = ReleaseInfo {
        tag: "v0.2.0".into(),
        version: Version::new(0, 2, 0),
        assets: vec![ReleaseAsset {
            name: "cfgd-0.2.0-freebsd-riscv64.tar.gz".into(),
            download_url: "https://example.com/other".into(),
            size: 1024,
        }],
    };

    let err = find_asset_for_platform(&release).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains(std::env::consts::OS.replace("macos", "darwin").as_str())
            || msg.contains(std::env::consts::ARCH),
        "error should mention the current platform: {msg}"
    );
}

#[test]
fn sha256_file_computes_hash() {
    let tmp = tempfile::NamedTempFile::new().expect("temp file");
    fs::write(tmp.path(), b"hello world").expect("write");
    let hash = sha256_file(tmp.path()).expect("hash");
    // SHA256 of "hello world"
    assert_eq!(
        hash,
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
    );
}

// --- verify_archive_checksum ---

const HELLO_WORLD_SHA256: &str = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";

fn write_temp_archive(content: &[u8]) -> tempfile::NamedTempFile {
    let tmp = tempfile::NamedTempFile::new().expect("temp file");
    fs::write(tmp.path(), content).expect("write");
    tmp
}

#[test]
fn verify_archive_checksum_accepts_matching_sha() {
    let archive = write_temp_archive(b"hello world");
    let checksums = format!("{HELLO_WORLD_SHA256}  cfgd_x86_64-unknown-linux-gnu.tar.gz\n");
    super::verify_archive_checksum(
        archive.path(),
        &checksums,
        "cfgd_x86_64-unknown-linux-gnu.tar.gz",
    )
    .expect("matching SHA must succeed");
}

#[test]
fn verify_archive_checksum_rejects_empty_checksums_with_dedicated_variant() {
    // Distinct from ChecksumMissing — operators triaging "checksums.txt was
    // truncated by CDN" need to see ChecksumsEmpty, not "asset not listed."
    let archive = write_temp_archive(b"hello world");
    let err = super::verify_archive_checksum(archive.path(), "", "cfgd.tar.gz").unwrap_err();
    assert!(
        matches!(err, crate::errors::UpgradeError::ChecksumsEmpty),
        "expected ChecksumsEmpty, got: {err:?}"
    );
}

#[test]
fn verify_archive_checksum_rejects_whitespace_only_checksums() {
    // parse_checksums skips lines that don't have two tokens — purely
    // whitespace input parses to an empty map and must surface ChecksumsEmpty.
    let archive = write_temp_archive(b"hello world");
    let err =
        super::verify_archive_checksum(archive.path(), "   \n\t\n", "cfgd.tar.gz").unwrap_err();
    assert!(matches!(err, crate::errors::UpgradeError::ChecksumsEmpty));
}

#[test]
fn verify_archive_checksum_returns_missing_when_asset_not_in_list() {
    // The archive downloaded fine but checksums.txt does not list it. This
    // is distinct from a SHA *mismatch* — see the rustdoc on the helper.
    let archive = write_temp_archive(b"hello world");
    let checksums = format!("{HELLO_WORLD_SHA256}  some-other-asset.tar.gz\n");
    let err = super::verify_archive_checksum(archive.path(), &checksums, "cfgd_linux.tar.gz")
        .unwrap_err();
    match err {
        crate::errors::UpgradeError::ChecksumMissing { file } => {
            assert_eq!(file, "cfgd_linux.tar.gz");
        }
        other => panic!("expected ChecksumMissing, got: {other:?}"),
    }
}

#[test]
fn verify_archive_checksum_returns_mismatch_when_sha_differs() {
    // Local archive content disagrees with what checksums.txt advertises.
    // Genuine corruption or in-flight interception — distinct from missing.
    let archive = write_temp_archive(b"tampered content");
    let checksums = format!("{HELLO_WORLD_SHA256}  cfgd_linux.tar.gz\n");
    let err = super::verify_archive_checksum(archive.path(), &checksums, "cfgd_linux.tar.gz")
        .unwrap_err();
    match err {
        crate::errors::UpgradeError::ChecksumMismatch { file } => {
            assert_eq!(file, "cfgd_linux.tar.gz");
        }
        other => panic!("expected ChecksumMismatch, got: {other:?}"),
    }
}

#[test]
fn verify_archive_checksum_propagates_read_failure_as_download_failed() {
    // Unreadable archive surfaces through sha256_file → DownloadFailed.
    let checksums = format!("{HELLO_WORLD_SHA256}  cfgd_linux.tar.gz\n");
    let err = super::verify_archive_checksum(
        std::path::Path::new("/nonexistent/path/to/archive.tar.gz"),
        &checksums,
        "cfgd_linux.tar.gz",
    )
    .unwrap_err();
    match err {
        crate::errors::UpgradeError::DownloadFailed { message } => {
            assert!(message.contains("/nonexistent/path"), "msg: {message}");
        }
        other => panic!("expected DownloadFailed, got: {other:?}"),
    }
}

#[test]
fn verify_archive_checksum_is_case_insensitive_on_hex() {
    // parse_checksums lowercases before storing — verify the helper
    // still matches when the checksums file uses uppercase hex (some
    // signers emit `SHA256SUMS` with capitalized digests).
    let archive = write_temp_archive(b"hello world");
    let upper = HELLO_WORLD_SHA256.to_uppercase();
    let checksums = format!("{upper}  cfgd_linux.tar.gz\n");
    super::verify_archive_checksum(archive.path(), &checksums, "cfgd_linux.tar.gz")
        .expect("uppercase hex must still match after lowercase normalization");
}

#[test]
fn verify_archive_checksum_picks_correct_entry_when_multiple_assets_listed() {
    let archive = write_temp_archive(b"hello world");
    let checksums = format!(
        "{HELLO_WORLD_SHA256}  cfgd_linux.tar.gz\n\
         deadbeef00000000000000000000000000000000000000000000000000000000  cfgd_macos.tar.gz\n\
         cafebabe00000000000000000000000000000000000000000000000000000000  cfgd_windows.zip\n"
    );
    super::verify_archive_checksum(archive.path(), &checksums, "cfgd_linux.tar.gz")
        .expect("must match the linux entry");
    let err = super::verify_archive_checksum(archive.path(), &checksums, "cfgd_macos.tar.gz")
        .unwrap_err();
    assert!(
        matches!(err, crate::errors::UpgradeError::ChecksumMismatch { ref file } if file == "cfgd_macos.tar.gz"),
        "wrong asset must surface as mismatch on the macos entry: {err:?}"
    );
}

#[test]
fn atomic_replace_overwrites_target() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source");
    let tgt = dir.path().join("target");
    std::fs::write(&src, "new content").unwrap();
    std::fs::write(&tgt, "old content").unwrap();

    atomic_replace(&src, &tgt).unwrap();
    assert_eq!(std::fs::read_to_string(&tgt).unwrap(), "new content");
}

#[test]
fn atomic_replace_creates_target() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source");
    let tgt = dir.path().join("target");
    std::fs::write(&src, "data").unwrap();

    atomic_replace(&src, &tgt).unwrap();
    assert_eq!(std::fs::read_to_string(&tgt).unwrap(), "data");
}

#[test]
fn version_cache_disk_persistence_camel_case() {
    // Write VersionCache to a temp file, read it back, verify camelCase keys on disk
    let cache = VersionCache {
        checked_at_secs: 1711800000,
        latest_tag: "v0.5.0".into(),
        latest_version: "0.5.0".into(),
        current_version: "0.4.0".into(),
    };

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("version-check.json");

    // Serialize and write to disk
    let json = serde_json::to_string(&cache).expect("serialize");
    fs::write(&path, &json).expect("write");

    // Verify the on-disk JSON uses camelCase keys
    let raw = fs::read_to_string(&path).expect("read");
    assert!(
        raw.contains("checkedAtSecs"),
        "expected camelCase key 'checkedAtSecs', got: {}",
        raw
    );
    assert!(
        raw.contains("latestTag"),
        "expected camelCase key 'latestTag', got: {}",
        raw
    );
    assert!(
        raw.contains("latestVersion"),
        "expected camelCase key 'latestVersion', got: {}",
        raw
    );
    assert!(
        raw.contains("currentVersion"),
        "expected camelCase key 'currentVersion', got: {}",
        raw
    );
    // Ensure snake_case keys are NOT present
    assert!(
        !raw.contains("checked_at_secs"),
        "should not contain snake_case key 'checked_at_secs'"
    );

    // Read back and deserialize
    let restored: VersionCache = serde_json::from_str(&raw).expect("deserialize from disk");
    assert_eq!(restored.checked_at_secs, 1711800000);
    assert_eq!(restored.latest_tag, "v0.5.0");
    assert_eq!(restored.latest_version, "0.5.0");
    assert_eq!(restored.current_version, "0.4.0");
}

#[test]
fn find_asset_wrong_platform_returns_error() {
    // Assets only for a fake platform should not match the real runtime platform
    let release = ReleaseInfo {
        tag: "v1.0.0".into(),
        version: Version::new(1, 0, 0),
        assets: vec![
            ReleaseAsset {
                name: "cfgd-1.0.0-fakeos-fakearch.tar.gz".into(),
                download_url: "https://example.com/fake".into(),
                size: 2048,
            },
            ReleaseAsset {
                name: "cfgd-1.0.0-anotheros-anotherarch.zip".into(),
                download_url: "https://example.com/another".into(),
                size: 4096,
            },
        ],
    };

    let result = find_asset_for_platform(&release);
    assert!(result.is_err(), "should fail for fake platform assets");

    // Verify the error message references the missing platform
    let err = result.unwrap_err();
    let err_msg = format!("{}", err);
    assert!(
        err_msg.contains("no release found for"),
        "error should mention missing platform: {}",
        err_msg
    );
}

#[test]
fn cache_ttl_fresh_cache_is_valid() {
    // Simulate a cache entry that was just written — should be within TTL
    let now_secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let cache = VersionCache {
        checked_at_secs: now_secs, // just now
        latest_tag: "v0.3.0".into(),
        latest_version: "0.3.0".into(),
        current_version: "0.2.0".into(),
    };

    let elapsed = now_secs.saturating_sub(cache.checked_at_secs);
    assert!(
        elapsed < CACHE_TTL_SECS,
        "fresh cache should be within TTL: elapsed={}, ttl={}",
        elapsed,
        CACHE_TTL_SECS
    );

    // The cached version should parse and be usable for comparison
    let cached_version = Version::parse(&cache.latest_version).expect("parse cached version");
    let current = Version::parse(&cache.current_version).expect("parse current version");
    assert!(cached_version > current, "0.3.0 > 0.2.0");
}

#[test]
fn cache_ttl_expired_cache_is_stale() {
    // Simulate a cache entry from 25 hours ago — should exceed the 24h TTL
    let now_secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let twenty_five_hours_ago = now_secs - (25 * 3600);

    let cache = VersionCache {
        checked_at_secs: twenty_five_hours_ago,
        latest_tag: "v0.3.0".into(),
        latest_version: "0.3.0".into(),
        current_version: "0.2.0".into(),
    };

    let elapsed = now_secs.saturating_sub(cache.checked_at_secs);
    assert!(
        elapsed >= CACHE_TTL_SECS,
        "25h-old cache should exceed TTL: elapsed={}, ttl={}",
        elapsed,
        CACHE_TTL_SECS
    );
}

#[test]
fn cache_ttl_boundary_just_expired() {
    // Cache is exactly at TTL boundary + 1 second — should be expired
    let now_secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let just_past_ttl = now_secs - CACHE_TTL_SECS - 1;

    let cache = VersionCache {
        checked_at_secs: just_past_ttl,
        latest_tag: "v0.3.0".into(),
        latest_version: "0.3.0".into(),
        current_version: "0.2.0".into(),
    };

    let elapsed = now_secs.saturating_sub(cache.checked_at_secs);
    assert!(
        elapsed >= CACHE_TTL_SECS,
        "cache at TTL+1s should be expired"
    );

    // One second before expiry should still be valid
    let at_boundary = now_secs - CACHE_TTL_SECS + 1;
    let boundary_elapsed = now_secs.saturating_sub(at_boundary);
    assert!(
        boundary_elapsed < CACHE_TTL_SECS,
        "cache at TTL-1s should still be valid"
    );
}

#[test]
fn version_cache_deserialization_from_known_json() {
    // Ensure we can deserialize a known JSON payload (simulates reading from disk)
    let json = r#"{"checkedAtSecs":1700000000,"latestTag":"v1.2.3","latestVersion":"1.2.3","currentVersion":"1.0.0"}"#;
    let cache: VersionCache = serde_json::from_str(json).expect("deserialize known JSON");
    assert_eq!(cache.checked_at_secs, 1700000000);
    assert_eq!(cache.latest_tag, "v1.2.3");
    assert_eq!(cache.latest_version, "1.2.3");
    assert_eq!(cache.current_version, "1.0.0");
}

#[test]
#[cfg(unix)]
fn extract_tarball_valid() {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("test.tar.gz");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    // Create a .tar.gz with one file
    {
        let file = std::fs::File::create(&archive_path).unwrap();
        let enc = GzEncoder::new(file, Compression::default());
        let mut tar_builder = tar::Builder::new(enc);
        let content = b"hello from tarball";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "test.txt", &content[..])
            .unwrap();
        tar_builder.finish().unwrap();
    }

    extract_tarball(&archive_path, &dest).unwrap();
    let extracted = std::fs::read_to_string(dest.join("test.txt")).unwrap();
    assert_eq!(extracted, "hello from tarball");
}

#[test]
fn download_and_install_checksum_mismatch_detection() {
    // Create a fake tarball
    let dir = tempfile::tempdir().unwrap();
    let tar_dir = dir.path().join("tar_src");
    std::fs::create_dir_all(&tar_dir).unwrap();
    std::fs::write(tar_dir.join("cfgd"), b"#!/bin/sh\necho fake binary").unwrap();

    let tarball_path = dir.path().join("cfgd-test.tar.gz");
    {
        let tar_file = std::fs::File::create(&tarball_path).unwrap();
        let enc = flate2::write::GzEncoder::new(tar_file, flate2::Compression::default());
        let mut tar_builder = tar::Builder::new(enc);
        tar_builder.append_dir_all(".", &tar_dir).unwrap();
        tar_builder.finish().unwrap();
    }

    // Create a checksums file with WRONG hash
    let checksums =
        "deadbeef00000000000000000000000000000000000000000000000000000000  cfgd-test.tar.gz\n";
    let parsed = parse_checksums(checksums);
    assert_eq!(
        parsed.get("cfgd-test.tar.gz").unwrap(),
        "deadbeef00000000000000000000000000000000000000000000000000000000"
    );

    // The actual hash of the tarball should NOT match the fake hash
    let actual_hash = sha256_file(&tarball_path).unwrap();
    assert_ne!(
        actual_hash, "deadbeef00000000000000000000000000000000000000000000000000000000",
        "real hash should differ from fake"
    );
}

#[test]
fn version_cache_disk_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let cache = VersionCache {
        checked_at_secs: 1711234567,
        latest_tag: "v1.2.3".into(),
        latest_version: "1.2.3".into(),
        current_version: "1.0.0".into(),
    };
    let json = serde_json::to_string(&cache).unwrap();
    let path = dir.path().join("version-cache.json");
    std::fs::write(&path, &json).unwrap();

    let content = std::fs::read_to_string(&path).unwrap();
    let restored: VersionCache = serde_json::from_str(&content).unwrap();
    assert_eq!(restored.checked_at_secs, 1711234567);
    assert_eq!(restored.latest_tag, "v1.2.3");
    assert_eq!(restored.latest_version, "1.2.3");
    assert_eq!(restored.current_version, "1.0.0");

    // Verify camelCase serialization
    assert!(json.contains("checkedAtSecs"));
    assert!(json.contains("latestTag"));
}

#[test]
fn find_asset_multiple_platforms_picks_current() {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let archive_os = if os == "macos" { "darwin" } else { os };
    #[cfg(unix)]
    let suffix = ".tar.gz";
    #[cfg(windows)]
    let suffix = ".zip";

    let release = ReleaseInfo {
        tag: "v0.5.0".into(),
        version: Version::new(0, 5, 0),
        assets: vec![
            ReleaseAsset {
                name: format!("cfgd-0.5.0-{}-{}{}", archive_os, arch, suffix),
                download_url: "https://example.com/current".into(),
                size: 5000,
            },
            ReleaseAsset {
                name: "cfgd-0.5.0-freebsd-riscv64.tar.gz".into(),
                download_url: "https://example.com/other".into(),
                size: 4000,
            },
        ],
    };
    let result = find_asset_for_platform(&release);
    assert!(result.is_ok());
    let asset = result.unwrap();
    assert_eq!(asset.download_url, "https://example.com/current");
}

#[test]
fn find_asset_no_matching_platform() {
    let release = ReleaseInfo {
        tag: "v0.5.0".into(),
        version: Version::new(0, 5, 0),
        assets: vec![ReleaseAsset {
            name: "cfgd-0.5.0-mips-unknown-linux.tar.gz".into(),
            download_url: "https://example.com/mips".into(),
            size: 3000,
        }],
    };
    let result = find_asset_for_platform(&release);
    // Unless we're running on mips, this should fail
    if std::env::consts::ARCH != "mips" {
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(std::env::consts::ARCH),
            "error should mention the current arch: {msg}"
        );
    }
}

#[test]
fn parse_checksums_with_multiple_entries() {
    let content = "abc123  file1.tar.gz\ndef456  file2.tar.gz\n";
    let parsed = parse_checksums(content);
    assert_eq!(parsed.get("file1.tar.gz").unwrap(), "abc123");
    assert_eq!(parsed.get("file2.tar.gz").unwrap(), "def456");
}

#[test]
fn parse_checksums_ignores_malformed_lines() {
    let content = "abc123  good.tar.gz\nbadline\n  \nabc456  another.tar.gz\n";
    let parsed = parse_checksums(content);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed.get("good.tar.gz").unwrap(), "abc123");
    assert_eq!(parsed.get("another.tar.gz").unwrap(), "abc456");
}

#[test]
fn parse_checksums_normalizes_to_lowercase() {
    let content = "ABCDEF123456  mixed-case.tar.gz\n";
    let parsed = parse_checksums(content);
    assert_eq!(parsed.get("mixed-case.tar.gz").unwrap(), "abcdef123456");
}

#[test]
fn find_checksums_asset_finds_by_suffix() {
    let release = ReleaseInfo {
        tag: "v0.5.0".into(),
        version: Version::new(0, 5, 0),
        assets: vec![
            ReleaseAsset {
                name: "cfgd-0.5.0-linux-x86_64.tar.gz".into(),
                download_url: "https://example.com/binary".into(),
                size: 5000,
            },
            ReleaseAsset {
                name: "cfgd-0.5.0-checksums.txt".into(),
                download_url: "https://example.com/checksums".into(),
                size: 256,
            },
        ],
    };
    let asset = find_checksums_asset(&release);
    assert!(asset.is_some());
    assert_eq!(asset.unwrap().name, "cfgd-0.5.0-checksums.txt");
}

#[test]
fn find_checksums_asset_none_when_missing() {
    let release = ReleaseInfo {
        tag: "v0.5.0".into(),
        version: Version::new(0, 5, 0),
        assets: vec![ReleaseAsset {
            name: "cfgd-0.5.0-linux-x86_64.tar.gz".into(),
            download_url: "https://example.com/binary".into(),
            size: 5000,
        }],
    };
    let asset = find_checksums_asset(&release);
    assert!(asset.is_none());
}

#[test]
fn version_check_interval_matches_cache_ttl() {
    let interval = version_check_interval();
    assert_eq!(interval, Duration::from_secs(CACHE_TTL_SECS));
}

#[test]
#[cfg(unix)]
fn extract_tarball_multiple_files_and_dirs() {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("multi.tar.gz");
    let dest = dir.path().join("extracted");
    std::fs::create_dir_all(&dest).unwrap();

    {
        let file = std::fs::File::create(&archive_path).unwrap();
        let enc = GzEncoder::new(file, Compression::default());
        let mut tar_builder = tar::Builder::new(enc);

        // Add a top-level file
        let content_a = b"file A content";
        let mut header_a = tar::Header::new_gnu();
        header_a.set_size(content_a.len() as u64);
        header_a.set_mode(0o644);
        header_a.set_cksum();
        tar_builder
            .append_data(&mut header_a, "file_a.txt", &content_a[..])
            .unwrap();

        // Add a file in a subdirectory
        let content_b = b"nested file B";
        let mut header_b = tar::Header::new_gnu();
        header_b.set_size(content_b.len() as u64);
        header_b.set_mode(0o755);
        header_b.set_cksum();
        tar_builder
            .append_data(&mut header_b, "subdir/file_b.txt", &content_b[..])
            .unwrap();

        // Add an empty file
        let mut header_c = tar::Header::new_gnu();
        header_c.set_size(0);
        header_c.set_mode(0o644);
        header_c.set_cksum();
        tar_builder
            .append_data(&mut header_c, "empty.txt", &[][..])
            .unwrap();

        tar_builder.finish().unwrap();
    }

    extract_tarball(&archive_path, &dest).unwrap();

    // Verify all files extracted correctly
    let a_content = std::fs::read_to_string(dest.join("file_a.txt")).unwrap();
    assert_eq!(a_content, "file A content");

    let b_content = std::fs::read_to_string(dest.join("subdir/file_b.txt")).unwrap();
    assert_eq!(b_content, "nested file B");

    let c_content = std::fs::read_to_string(dest.join("empty.txt")).unwrap();
    assert!(c_content.is_empty(), "empty file should have no content");
}

#[test]
#[cfg(unix)]
fn extract_tarball_nonexistent_archive_fails() {
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let result = extract_tarball(&dir.path().join("does-not-exist.tar.gz"), &dest);
    assert!(result.is_err(), "should fail for nonexistent archive");
}

#[test]
#[cfg(unix)]
fn extract_tarball_invalid_gz_fails() {
    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("bad.tar.gz");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    // Write garbage data that isn't valid gzip
    std::fs::write(&archive_path, b"this is not a gzip file").unwrap();

    let result = extract_tarball(&archive_path, &dest);
    assert!(result.is_err(), "should fail for invalid gzip data");
}

#[test]
#[cfg(unix)]
fn extract_tarball_skips_symlink_entries_without_failing() {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("with-symlink.tar.gz");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    {
        let file = std::fs::File::create(&archive_path).unwrap();
        let enc = GzEncoder::new(file, Compression::default());
        let mut tar_builder = tar::Builder::new(enc);

        // Regular file (must be unpacked)
        let body = b"real file";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "real.txt", &body[..])
            .unwrap();

        // Symlink entry pointing inside dest — extract_tarball must skip it
        // (the loop's `is_symlink()` arm) and not fail the whole extraction.
        let mut sym_header = tar::Header::new_gnu();
        sym_header.set_size(0);
        sym_header.set_mode(0o644);
        sym_header.set_entry_type(tar::EntryType::Symlink);
        sym_header.set_link_name("real.txt").unwrap();
        sym_header.set_cksum();
        tar_builder
            .append_data(&mut sym_header, "link.txt", &[][..])
            .unwrap();

        tar_builder.finish().unwrap();
    }

    extract_tarball(&archive_path, &dest).expect("symlink in tarball must not fail extraction");

    assert!(
        dest.join("real.txt").exists(),
        "regular file must still be unpacked"
    );
    assert!(
        !dest.join("link.txt").exists() && !dest.join("link.txt").is_symlink(),
        "symlink entry must be skipped — guards against escape via crafted link target"
    );
}

#[test]
fn check_with_cache_returns_error_when_cached_version_is_unparseable() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    // Seed a fresh cache entry whose latest_version field is not a valid
    // semver. The TTL check passes (just-now), so the function reaches
    // Version::parse which must surface UpgradeError::VersionParse rather
    // than silently fall through to the API.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    write_version_cache(&VersionCache {
        checked_at_secs: now,
        latest_tag: "vBOGUS".into(),
        latest_version: "not-a-semver".into(),
        current_version: env!("CARGO_PKG_VERSION").into(),
    })
    .expect("cache seed");

    let err = check_with_cache(Some("does/not/matter"), None)
        .expect_err("unparseable cached version must surface as Err, not silent fallthrough");
    let msg = err.to_string();
    assert!(
        msg.contains("cached version") && msg.contains("parse"),
        "error must point at the cache file's version field so triage looks there first: {msg}"
    );
}

#[test]
fn find_checksums_asset_picks_checksums_txt_over_other_assets() {
    let release = ReleaseInfo {
        tag: "v1.0.0".into(),
        version: Version::new(1, 0, 0),
        assets: vec![
            ReleaseAsset {
                name: "cfgd-1.0.0-linux-x86_64.tar.gz".into(),
                download_url: "https://example.com/binary".into(),
                size: 10000,
            },
            ReleaseAsset {
                name: "SHA256SUMS".into(),
                download_url: "https://example.com/sha256sums".into(),
                size: 512,
            },
            ReleaseAsset {
                name: "cfgd-1.0.0-checksums.txt".into(),
                download_url: "https://example.com/checksums".into(),
                size: 256,
            },
        ],
    };

    let asset = find_checksums_asset(&release);
    assert!(asset.is_some());
    // find_checksums_asset looks for names ending in "-checksums.txt"
    assert_eq!(asset.unwrap().name, "cfgd-1.0.0-checksums.txt");
    assert_eq!(asset.unwrap().download_url, "https://example.com/checksums");
}

#[test]
fn find_checksums_asset_returns_none_for_non_matching_names() {
    // SHA256SUMS does not match the -checksums.txt suffix pattern
    let release = ReleaseInfo {
        tag: "v2.0.0".into(),
        version: Version::new(2, 0, 0),
        assets: vec![
            ReleaseAsset {
                name: "cfgd-2.0.0-linux-x86_64.tar.gz".into(),
                download_url: "https://example.com/binary".into(),
                size: 10000,
            },
            ReleaseAsset {
                name: "SHA256SUMS".into(),
                download_url: "https://example.com/sha256sums".into(),
                size: 512,
            },
        ],
    };

    let asset = find_checksums_asset(&release);
    assert!(
        asset.is_none(),
        "SHA256SUMS does not end with -checksums.txt, so should not match"
    );
}

#[test]
fn find_checksums_asset_empty_assets() {
    let release = ReleaseInfo {
        tag: "v1.0.0".into(),
        version: Version::new(1, 0, 0),
        assets: vec![],
    };
    assert!(find_checksums_asset(&release).is_none());
}

#[test]
fn invalidate_cache_removes_file_if_present() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    let dir = home.path().join(".cache").join("cfgd");
    fs::create_dir_all(&dir).unwrap();
    let cache_path = dir.join(CACHE_FILENAME);
    let data =
        r#"{"checkedAtSecs":0,"latestTag":"v0","latestVersion":"0.0.0","currentVersion":"0.0.0"}"#;
    fs::write(&cache_path, data).unwrap();
    assert!(cache_path.exists(), "test setup: cache file must exist");

    invalidate_cache();

    assert!(
        !cache_path.exists(),
        "cache file should be removed after invalidation"
    );
}

#[test]
fn invalidate_cache_no_panic_when_no_file() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());
    // Ensure calling invalidate when no cache file exists does not panic.
    invalidate_cache();
    invalidate_cache(); // double-call should be safe
}

#[test]
fn restart_daemon_if_running_returns_false_when_no_daemon() {
    // In test environments, no daemon is running, so this should return false
    let result = restart_daemon_if_running();
    assert!(
        !result,
        "restart_daemon_if_running should return false when no daemon is running"
    );
}

#[test]
fn update_check_fields_are_coherent() {
    // Construct an UpdateCheck manually and verify field semantics
    let check = UpdateCheck {
        current: Version::new(0, 1, 0),
        latest: Version::new(0, 2, 0),
        update_available: true,
        release: None,
    };
    assert!(check.update_available);
    assert!(check.latest > check.current);
    assert!(check.release.is_none());

    let no_update = UpdateCheck {
        current: Version::new(0, 2, 0),
        latest: Version::new(0, 2, 0),
        update_available: false,
        release: None,
    };
    assert!(!no_update.update_available);
    assert_eq!(no_update.current, no_update.latest);
}

#[test]
fn version_cache_write_and_read_roundtrip() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    let cache = VersionCache {
        checked_at_secs: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
        latest_tag: "v99.99.99".into(),
        latest_version: "99.99.99".into(),
        current_version: env!("CARGO_PKG_VERSION").into(),
    };

    write_version_cache(&cache).expect("write into tempdir cache should succeed");

    let read = read_version_cache().expect("should be able to read back written cache");
    assert_eq!(read.latest_tag, "v99.99.99");
    assert_eq!(read.latest_version, "99.99.99");
    assert_eq!(read.current_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(read.checked_at_secs, cache.checked_at_secs);
}

#[test]
fn read_version_cache_returns_none_after_invalidation() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    // Seed a cache file so invalidation has something to remove — the
    // post-invalidation None must reflect a real removal, not absence.
    let cache = VersionCache {
        checked_at_secs: 1,
        latest_tag: "v0".into(),
        latest_version: "0.0.0".into(),
        current_version: "0.0.0".into(),
    };
    write_version_cache(&cache).expect("seed cache write must succeed");
    assert!(
        read_version_cache().is_some(),
        "test precondition: cache must be readable before invalidation"
    );

    invalidate_cache();

    assert!(
        read_version_cache().is_none(),
        "read_version_cache should return None after invalidation"
    );
}

#[test]
fn cleanup_old_binary_does_not_panic() {
    // Just verify it doesn't panic on any platform
    cleanup_old_binary();
}

// --- fetch_latest_release_from with mockito ---

#[test]
fn fetch_latest_release_from_parses_github_response() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/repos/test/repo/releases/latest")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{
                    "tag_name": "v1.2.3",
                    "assets": [
                        {
                            "name": "cfgd-1.2.3-linux-x86_64.tar.gz",
                            "browser_download_url": "https://example.com/download/cfgd-1.2.3-linux-x86_64.tar.gz",
                            "size": 5000000
                        },
                        {
                            "name": "checksums.txt",
                            "browser_download_url": "https://example.com/download/checksums.txt",
                            "size": 512
                        }
                    ]
                }"#,
        )
        .create();

    let result = fetch_latest_release_from(&server.url(), "test/repo", None);
    mock.assert();

    let release = result.unwrap();
    assert_eq!(release.tag, "v1.2.3");
    assert_eq!(release.version, Version::new(1, 2, 3));
    assert_eq!(release.assets.len(), 2);
    assert_eq!(release.assets[0].name, "cfgd-1.2.3-linux-x86_64.tar.gz");
    assert_eq!(release.assets[0].size, 5000000);
    assert_eq!(release.assets[1].name, "checksums.txt");
}

#[test]
fn fetch_latest_release_from_handles_api_error() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/repos/test/repo/releases/latest")
        .with_status(404)
        .with_body(r#"{"message": "Not Found"}"#)
        .create();

    let result = fetch_latest_release_from(&server.url(), "test/repo", None);
    mock.assert();

    assert!(result.is_err());
    let err = result.unwrap_err();
    let err_str = err.to_string();
    assert!(
        err_str.contains("404") || err_str.contains("Not Found") || err_str.contains("status code"),
        "error should indicate API failure: {}",
        err_str
    );
}

#[test]
fn fetch_latest_release_from_handles_invalid_json() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/repos/test/repo/releases/latest")
        .with_status(200)
        .with_body("this is not json")
        .create();

    let result = fetch_latest_release_from(&server.url(), "test/repo", None);
    mock.assert();

    assert!(result.is_err());
}

#[test]
fn fetch_latest_release_from_handles_missing_tag_name() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/repos/test/repo/releases/latest")
        .with_status(200)
        .with_body(r#"{"name": "Release", "assets": []}"#)
        .create();

    let result = fetch_latest_release_from(&server.url(), "test/repo", None);
    mock.assert();

    assert!(result.is_err());
}

#[test]
fn fetch_latest_release_from_handles_no_assets() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/repos/test/repo/releases/latest")
        .with_status(200)
        .with_body(r#"{"tag_name": "v2.0.0"}"#)
        .create();

    let result = fetch_latest_release_from(&server.url(), "test/repo", None);
    mock.assert();

    let release = result.unwrap();
    assert_eq!(release.version, Version::new(2, 0, 0));
    assert!(release.assets.is_empty());
}

#[test]
fn fetch_latest_release_from_handles_tag_without_v_prefix() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/repos/test/repo/releases/latest")
        .with_status(200)
        .with_body(r#"{"tag_name": "3.0.1", "assets": []}"#)
        .create();

    let result = fetch_latest_release_from(&server.url(), "test/repo", None);
    mock.assert();

    let release = result.unwrap();
    assert_eq!(release.tag, "3.0.1");
    assert_eq!(release.version, Version::new(3, 0, 1));
}

#[test]
fn fetch_latest_release_from_handles_prerelease_version() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/repos/test/repo/releases/latest")
        .with_status(200)
        .with_body(r#"{"tag_name": "v4.0.0-beta.1", "assets": []}"#)
        .create();

    let result = fetch_latest_release_from(&server.url(), "test/repo", None);
    mock.assert();

    let release = result.unwrap();
    assert_eq!(release.version, Version::parse("4.0.0-beta.1").unwrap());
}

// --- download_to_file with mockito ---

#[test]
fn download_to_file_writes_content_to_path() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/download/test-file")
        .with_status(200)
        .with_body(b"file content here")
        .create();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("downloaded.bin");
    let url = format!("{}/download/test-file", server.url());

    let result = download_to_file(&url, &dest, None);
    mock.assert();

    assert!(result.is_ok());
    assert_eq!(std::fs::read_to_string(&dest).unwrap(), "file content here");
}

#[test]
fn download_to_file_returns_error_on_http_failure() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/download/missing")
        .with_status(404)
        .create();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("should-not-exist.bin");
    let url = format!("{}/download/missing", server.url());

    let result = download_to_file(&url, &dest, None);
    mock.assert();

    assert!(result.is_err());
    assert!(!dest.exists(), "file should not be created on failure");
}

#[test]
fn download_to_file_returns_error_on_5xx_response() {
    // 5xx is the apiserver-overloaded / GitHub-incident shape; the previous
    // 404 test pinned client-error handling. This one pins server-error
    // handling — both must surface as DownloadFailed without leaving a
    // half-written file behind.
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/download/flaky")
        .with_status(503)
        .create();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("should-not-exist.bin");
    let url = format!("{}/download/flaky", server.url());

    let result = download_to_file(&url, &dest, None);
    mock.assert();

    assert!(result.is_err(), "5xx must surface as Err");
    assert!(!dest.exists(), "file must not be created on 5xx");
}

#[test]
fn download_to_file_drives_progress_bar_branch_with_printer_and_content_length() {
    // (Some(printer), Some(content_length)) routes into the chunked-read
    // loop with an indicatif progress bar. Quiet printer ensures no
    // terminal output during the test; the branch is exercised regardless.
    let mut server = mockito::Server::new();
    let body = b"download body for progress bar branch";
    let mock = server
        .mock("GET", "/download/sized")
        .with_status(200)
        .with_header("content-length", &body.len().to_string())
        .with_body(body)
        .create();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("sized.bin");
    let url = format!("{}/download/sized", server.url());
    let printer = crate::test_helpers::test_printer();

    let result = download_to_file(&url, &dest, Some(&printer));
    mock.assert();

    assert!(result.is_ok(), "happy path with printer must succeed");
    assert_eq!(std::fs::read(&dest).unwrap(), body);
}

#[test]
fn download_to_file_drives_spinner_branch_when_printer_present_without_content_length() {
    // (Some(printer), None) → chunked transfer encoding from the server
    // means no content-length header reaches the client; download_to_file
    // routes into the spinner + io::copy branch.
    let mut server = mockito::Server::new();
    let body: &[u8] = b"chunked body for spinner branch";
    let mock = server
        .mock("GET", "/download/chunked")
        .with_status(200)
        .with_chunked_body(move |w| w.write_all(body))
        .create();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("chunked.bin");
    let url = format!("{}/download/chunked", server.url());
    let printer = crate::test_helpers::test_printer();

    let result = download_to_file(&url, &dest, Some(&printer));
    mock.assert();

    assert!(result.is_ok(), "spinner branch must succeed");
    assert_eq!(std::fs::read(&dest).unwrap(), body);
}

// --- fetch_latest_release_from: with printer drives spinner branch ---

#[test]
fn fetch_latest_release_from_with_printer_drives_spinner_branch() {
    // Exercises the (Some(printer), spinner) path inside fetch_latest_release_from
    // (lines 87, 103-104 of mod.rs). The spinner is created and finished.
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/repos/test/repo/releases/latest")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{
                "tag_name": "v7.0.0",
                "assets": [
                    {
                        "name": "cfgd-7.0.0-linux-x86_64.tar.gz",
                        "browser_download_url": "https://example.com/download",
                        "size": 1024
                    }
                ]
            }"#,
        )
        .create();

    let printer = crate::test_helpers::test_printer();
    let result = fetch_latest_release_from(&server.url(), "test/repo", Some(&printer));
    mock.assert();

    let release = result.expect("should parse with printer present");
    assert_eq!(release.version, Version::new(7, 0, 0));
    assert_eq!(release.assets.len(), 1);
}

#[test]
fn fetch_latest_release_from_with_printer_on_error_still_returns_err() {
    // Printer present when the API returns an error status — spinner is created
    // but never finished (the early return happens before finish_ok). This
    // exercises the error path with printer != None.
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/repos/test/repo/releases/latest")
        .with_status(502)
        .with_body("Bad Gateway")
        .create();

    let printer = crate::test_helpers::test_printer();
    let result = fetch_latest_release_from(&server.url(), "test/repo", Some(&printer));
    mock.assert();

    assert!(
        result.is_err(),
        "502 with printer must still surface as Err"
    );
}

// --- parse_release_json: comprehensive edge cases ---

#[test]
fn parse_release_json_assets_missing_fields_skipped() {
    // Assets with missing name or download_url are filtered out by filter_map
    let json = r#"{
            "tag_name": "v1.0.0",
            "assets": [
                {
                    "name": "valid.tar.gz",
                    "browser_download_url": "https://example.com/valid.tar.gz",
                    "size": 1024
                },
                {
                    "browser_download_url": "https://example.com/noname.tar.gz",
                    "size": 512
                },
                {
                    "name": "nourl.tar.gz",
                    "size": 256
                }
            ]
        }"#;
    let release = parse_release_json(json).unwrap();
    assert_eq!(
        release.assets.len(),
        1,
        "only the valid asset should be included"
    );
    assert_eq!(release.assets[0].name, "valid.tar.gz");
}

#[test]
fn parse_release_json_asset_size_defaults_to_zero() {
    let json = r#"{
            "tag_name": "v1.0.0",
            "assets": [
                {
                    "name": "nosize.tar.gz",
                    "browser_download_url": "https://example.com/nosize.tar.gz"
                }
            ]
        }"#;
    let release = parse_release_json(json).unwrap();
    assert_eq!(release.assets.len(), 1);
    assert_eq!(
        release.assets[0].size, 0,
        "missing size should default to 0"
    );
}

#[test]
fn parse_release_json_prerelease_tag() {
    let json = r#"{
            "tag_name": "v2.0.0-rc.1",
            "assets": []
        }"#;
    let release = parse_release_json(json).unwrap();
    assert_eq!(release.tag, "v2.0.0-rc.1");
    assert_eq!(release.version, Version::parse("2.0.0-rc.1").unwrap());
}

#[test]
fn parse_release_json_build_metadata() {
    let json = r#"{
            "tag_name": "v1.0.0+build.123",
            "assets": []
        }"#;
    let release = parse_release_json(json).unwrap();
    assert_eq!(release.version.major, 1);
    assert_eq!(release.version.minor, 0);
    assert_eq!(release.version.patch, 0);
}

#[test]
fn parse_release_json_invalid_version_tag() {
    let json = r#"{
            "tag_name": "not-semver",
            "assets": []
        }"#;
    let result = parse_release_json(json);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("cannot parse release version"),
        "should mention version parse error: {msg}"
    );
}

#[test]
fn parse_release_json_null_assets_treated_as_empty() {
    let json = r#"{
            "tag_name": "v1.0.0",
            "assets": null
        }"#;
    let release = parse_release_json(json).unwrap();
    assert!(release.assets.is_empty());
}

#[test]
fn parse_release_json_no_assets_field() {
    let json = r#"{"tag_name": "v1.0.0"}"#;
    let release = parse_release_json(json).unwrap();
    assert!(release.assets.is_empty());
}

// --- find_asset_for_platform: empty assets ---

#[test]
fn find_asset_empty_assets_returns_error() {
    let release = ReleaseInfo {
        tag: "v1.0.0".into(),
        version: Version::new(1, 0, 0),
        assets: vec![],
    };
    assert!(find_asset_for_platform(&release).is_err());
}

// --- find_checksums_asset: various patterns ---

#[test]
fn find_checksums_asset_matches_version_prefixed() {
    let release = ReleaseInfo {
        tag: "v3.0.0".into(),
        version: Version::new(3, 0, 0),
        assets: vec![
            ReleaseAsset {
                name: "cfgd-3.0.0-linux-x86_64.tar.gz".into(),
                download_url: "https://example.com/bin".into(),
                size: 5000,
            },
            ReleaseAsset {
                name: "cfgd-3.0.0-checksums.txt".into(),
                download_url: "https://example.com/sums".into(),
                size: 128,
            },
        ],
    };
    let asset = find_checksums_asset(&release).unwrap();
    assert_eq!(asset.name, "cfgd-3.0.0-checksums.txt");
    assert_eq!(asset.download_url, "https://example.com/sums");
}

// --- extract_tarball: additional scenarios ---

#[test]
#[cfg(unix)]
fn extract_tarball_empty_archive() {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("empty.tar.gz");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    // Create an empty tarball
    {
        let file = std::fs::File::create(&archive_path).unwrap();
        let enc = GzEncoder::new(file, Compression::default());
        let mut tar_builder = tar::Builder::new(enc);
        tar_builder.finish().unwrap();
    }

    extract_tarball(&archive_path, &dest).unwrap();
    // dest should still exist but be empty (besides . and ..)
    let entries: Vec<_> = std::fs::read_dir(&dest).unwrap().collect();
    assert!(
        entries.is_empty(),
        "empty tarball should extract to empty dir"
    );
}

#[test]
#[cfg(unix)]
fn extract_tarball_preserves_binary_content() {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("binary.tar.gz");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    // Binary data (not valid UTF-8)
    let binary_data: Vec<u8> = (0..=255).collect();

    {
        let file = std::fs::File::create(&archive_path).unwrap();
        let enc = GzEncoder::new(file, Compression::default());
        let mut tar_builder = tar::Builder::new(enc);
        let mut header = tar::Header::new_gnu();
        header.set_size(binary_data.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "binary.bin", &binary_data[..])
            .unwrap();
        tar_builder.finish().unwrap();
    }

    extract_tarball(&archive_path, &dest).unwrap();
    let extracted = std::fs::read(dest.join("binary.bin")).unwrap();
    assert_eq!(
        extracted, binary_data,
        "binary data should be preserved exactly"
    );
}

#[test]
#[cfg(unix)]
fn extract_tarball_skips_hardlink_entries_without_failing() {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("with-hardlink.tar.gz");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    {
        let file = std::fs::File::create(&archive_path).unwrap();
        let enc = GzEncoder::new(file, Compression::default());
        let mut tar_builder = tar::Builder::new(enc);

        // Regular file
        let body = b"real file content";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "real.txt", &body[..])
            .unwrap();

        // Hardlink entry — extract_tarball must skip it (the is_hard_link()
        // branch) without failing the entire extraction.
        let mut hl_header = tar::Header::new_gnu();
        hl_header.set_size(0);
        hl_header.set_mode(0o644);
        hl_header.set_entry_type(tar::EntryType::Link);
        hl_header.set_link_name("real.txt").unwrap();
        hl_header.set_cksum();
        tar_builder
            .append_data(&mut hl_header, "hardlink.txt", &[][..])
            .unwrap();

        tar_builder.finish().unwrap();
    }

    extract_tarball(&archive_path, &dest).expect("hardlink in tarball must not fail extraction");

    assert!(
        dest.join("real.txt").exists(),
        "regular file must still be unpacked"
    );
    assert!(
        !dest.join("hardlink.txt").exists(),
        "hardlink entry must be skipped — guards against escape via crafted link target"
    );
}

#[test]
#[cfg(unix)]
fn extract_tarball_skips_mixed_symlink_and_hardlink_entries() {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("mixed-links.tar.gz");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    {
        let file = std::fs::File::create(&archive_path).unwrap();
        let enc = GzEncoder::new(file, Compression::default());
        let mut tar_builder = tar::Builder::new(enc);

        // Regular file that should be extracted
        let body = b"cfgd binary content";
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "cfgd", &body[..])
            .unwrap();

        // Symlink entry
        let mut sym_header = tar::Header::new_gnu();
        sym_header.set_size(0);
        sym_header.set_mode(0o777);
        sym_header.set_entry_type(tar::EntryType::Symlink);
        sym_header.set_link_name("/etc/passwd").unwrap();
        sym_header.set_cksum();
        tar_builder
            .append_data(&mut sym_header, "evil_symlink", &[][..])
            .unwrap();

        // Hardlink entry
        let mut hl_header = tar::Header::new_gnu();
        hl_header.set_size(0);
        hl_header.set_mode(0o644);
        hl_header.set_entry_type(tar::EntryType::Link);
        hl_header.set_link_name("cfgd").unwrap();
        hl_header.set_cksum();
        tar_builder
            .append_data(&mut hl_header, "evil_hardlink", &[][..])
            .unwrap();

        tar_builder.finish().unwrap();
    }

    extract_tarball(&archive_path, &dest).expect("mixed link types must not fail extraction");

    assert!(dest.join("cfgd").exists(), "binary must be extracted");
    assert!(
        !dest.join("evil_symlink").exists() && !dest.join("evil_symlink").is_symlink(),
        "symlink entry must be skipped"
    );
    assert!(
        !dest.join("evil_hardlink").exists(),
        "hardlink entry must be skipped"
    );
    let content = std::fs::read(dest.join("cfgd")).unwrap();
    assert_eq!(content, b"cfgd binary content");
}

// --- atomic_replace: edge cases ---

#[test]
fn atomic_replace_with_large_content() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source");
    let tgt = dir.path().join("target");

    // Create a ~1MB file
    let large_content: Vec<u8> = vec![0xAB; 1024 * 1024];
    std::fs::write(&src, &large_content).unwrap();
    std::fs::write(&tgt, b"old small content").unwrap();

    atomic_replace(&src, &tgt).unwrap();
    let result = std::fs::read(&tgt).unwrap();
    assert_eq!(result.len(), large_content.len());
    assert_eq!(result, large_content);
}

#[test]
fn atomic_replace_target_parent_must_exist() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source");
    std::fs::write(&src, "content").unwrap();

    // Target in a non-existent directory
    let tgt = dir.path().join("nonexistent").join("subdir").join("target");
    let result = atomic_replace(&src, &tgt);
    assert!(
        result.is_err(),
        "should fail when target parent doesn't exist"
    );
}

// --- version_cache serialization/deserialization ---

#[test]
fn version_cache_with_prerelease() {
    let cache = VersionCache {
        checked_at_secs: 1700000000,
        latest_tag: "v2.0.0-beta.3".into(),
        latest_version: "2.0.0-beta.3".into(),
        current_version: "1.9.0".into(),
    };

    let json = serde_json::to_string(&cache).unwrap();
    let restored: VersionCache = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.latest_tag, "v2.0.0-beta.3");
    assert_eq!(restored.latest_version, "2.0.0-beta.3");

    // Verify the prerelease version parses and compares correctly
    let latest = Version::parse(&restored.latest_version).unwrap();
    let current = Version::parse(&restored.current_version).unwrap();
    assert!(latest > current, "2.0.0-beta.3 > 1.9.0");
}

#[test]
fn version_cache_tolerates_extra_json_fields() {
    // Forward compatibility: ignore unknown fields
    let json = r#"{"checkedAtSecs":100,"latestTag":"v1","latestVersion":"1.0.0","currentVersion":"0.9.0","extraField":"ignored"}"#;
    let cache: VersionCache = serde_json::from_str(json).unwrap();
    assert_eq!(cache.checked_at_secs, 100);
    assert_eq!(cache.latest_version, "1.0.0");
}

// --- cache TTL: zero elapsed ---

#[test]
fn cache_ttl_zero_seconds_ago_is_fresh() {
    let now_secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let elapsed = now_secs.saturating_sub(now_secs);
    assert!(
        elapsed < CACHE_TTL_SECS,
        "zero-elapsed cache should be fresh"
    );
}

#[test]
fn cache_ttl_exactly_at_boundary_is_fresh() {
    let now_secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Exactly at TTL boundary (== CACHE_TTL_SECS) should NOT be fresh (uses <, not <=)
    let at_boundary = now_secs - CACHE_TTL_SECS;
    let elapsed = now_secs.saturating_sub(at_boundary);
    assert!(
        elapsed >= CACHE_TTL_SECS,
        "cache exactly at TTL boundary should be expired (uses strict <)"
    );
}

// --- strip_tag_prefix ---

#[test]
fn strip_tag_prefix_with_v() {
    assert_eq!(strip_tag_prefix("v1.2.3"), "1.2.3");
}

#[test]
fn strip_tag_prefix_without_v() {
    assert_eq!(strip_tag_prefix("1.2.3"), "1.2.3");
}

#[test]
fn strip_tag_prefix_empty() {
    assert_eq!(strip_tag_prefix(""), "");
}

#[test]
fn strip_tag_prefix_only_v() {
    assert_eq!(strip_tag_prefix("v"), "");
}

#[test]
fn strip_tag_prefix_double_v() {
    // Only strips one leading 'v'
    assert_eq!(strip_tag_prefix("vv1.0.0"), "v1.0.0");
}

// --- parse_checksums edge cases ---

#[test]
fn parse_checksums_extra_whitespace_between_fields() {
    let content = "abc123    file.tar.gz\n";
    let map = parse_checksums(content);
    assert_eq!(map.len(), 1);
    // split_whitespace handles multiple spaces
    assert_eq!(map.get("file.tar.gz").unwrap(), "abc123");
}

#[test]
fn parse_checksums_tab_separated() {
    let content = "abc123\tfile.tar.gz\n";
    let map = parse_checksums(content);
    assert_eq!(map.len(), 1);
    assert_eq!(map.get("file.tar.gz").unwrap(), "abc123");
}

#[test]
fn parse_checksums_duplicate_filename_last_wins() {
    let content = "first_hash  file.tar.gz\nsecond_hash  file.tar.gz\n";
    let map = parse_checksums(content);
    assert_eq!(map.len(), 1);
    assert_eq!(
        map.get("file.tar.gz").unwrap(),
        "second_hash",
        "last occurrence should win in HashMap"
    );
}

// --- download_to_file with content-length header ---

#[test]
fn download_to_file_with_content_length() {
    let mut server = mockito::Server::new();
    let body = "known length content";
    let mock = server
        .mock("GET", "/sized-file")
        .with_status(200)
        .with_header("content-length", &body.len().to_string())
        .with_body(body)
        .create();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("sized.bin");
    let url = format!("{}/sized-file", server.url());

    download_to_file(&url, &dest, None).unwrap();
    mock.assert();

    let content = std::fs::read_to_string(&dest).unwrap();
    assert_eq!(content, "known length content");
}

#[test]
fn download_to_file_binary_content() {
    let mut server = mockito::Server::new();
    let binary_data: Vec<u8> = (0..=127).collect();
    let mock = server
        .mock("GET", "/binary")
        .with_status(200)
        .with_body(&binary_data)
        .create();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("binary.bin");
    let url = format!("{}/binary", server.url());

    download_to_file(&url, &dest, None).unwrap();
    mock.assert();

    let content = std::fs::read(&dest).unwrap();
    assert_eq!(content, binary_data);
}

// --- sha256_file edge cases ---

#[test]
fn sha256_file_empty_file() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    // Write nothing (empty file)
    let hash = sha256_file(tmp.path()).unwrap();
    // SHA256 of empty string
    assert_eq!(
        hash,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn sha256_file_nonexistent_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let result = sha256_file(&dir.path().join("does-not-exist"));
    assert!(result.is_err(), "nonexistent file should error");
}

// --- fetch_latest_release_from: additional error scenarios ---

#[test]
fn fetch_latest_release_from_handles_server_error() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/repos/test/repo/releases/latest")
        .with_status(500)
        .with_body("Internal Server Error")
        .create();

    let result = fetch_latest_release_from(&server.url(), "test/repo", None);
    mock.assert();

    assert!(result.is_err());
}

#[test]
fn fetch_latest_release_from_with_many_assets() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/repos/test/repo/releases/latest")
        .with_status(200)
        .with_body(
            r#"{
                    "tag_name": "v5.0.0",
                    "assets": [
                        {"name": "cfgd-5.0.0-linux-x86_64.tar.gz", "browser_download_url": "https://dl/linux-x64", "size": 10000},
                        {"name": "cfgd-5.0.0-linux-aarch64.tar.gz", "browser_download_url": "https://dl/linux-arm64", "size": 9000},
                        {"name": "cfgd-5.0.0-darwin-x86_64.tar.gz", "browser_download_url": "https://dl/darwin-x64", "size": 11000},
                        {"name": "cfgd-5.0.0-darwin-aarch64.tar.gz", "browser_download_url": "https://dl/darwin-arm64", "size": 10500},
                        {"name": "cfgd-5.0.0-windows-x86_64.zip", "browser_download_url": "https://dl/windows-x64", "size": 12000},
                        {"name": "cfgd-5.0.0-checksums.txt", "browser_download_url": "https://dl/checksums", "size": 512}
                    ]
                }"#,
        )
        .create();

    let result = fetch_latest_release_from(&server.url(), "test/repo", None);
    mock.assert();

    let release = result.unwrap();
    assert_eq!(release.version, Version::new(5, 0, 0));
    assert_eq!(release.assets.len(), 6, "should parse all 6 assets");

    // Verify specific assets
    let checksums = release.assets.iter().find(|a| a.name.contains("checksums"));
    assert!(checksums.is_some());
    assert_eq!(checksums.unwrap().size, 512);
}

// --- find_cosign_bundle_asset ---

fn release_with_assets(names: &[&str]) -> ReleaseInfo {
    ReleaseInfo {
        tag: "v1.0.0".into(),
        version: Version::new(1, 0, 0),
        assets: names
            .iter()
            .map(|n| ReleaseAsset {
                name: (*n).to_string(),
                download_url: format!("https://example.com/{}", n),
                size: 100,
            })
            .collect(),
    }
}

#[test]
fn find_cosign_bundle_asset_locates_by_suffix() {
    let release = release_with_assets(&[
        "cfgd-1.0.0-linux-x86_64.tar.gz",
        "cfgd-1.0.0-checksums.txt",
        "cfgd-1.0.0-checksums.txt.cosign.bundle",
    ]);
    let found = find_cosign_bundle_asset(&release).expect("bundle must be located by suffix");
    assert_eq!(found.name, "cfgd-1.0.0-checksums.txt.cosign.bundle");
}

#[test]
fn find_cosign_bundle_asset_returns_none_when_no_bundle() {
    // checksums.txt present but no .cosign.bundle — verifier must fall back.
    let release =
        release_with_assets(&["cfgd-1.0.0-linux-x86_64.tar.gz", "cfgd-1.0.0-checksums.txt"]);
    assert!(find_cosign_bundle_asset(&release).is_none());
}

#[test]
fn find_cosign_bundle_asset_ignores_lookalike_names() {
    // Suffix is exact: ".cosign.bundle" must come at the end.
    let release = release_with_assets(&[
        "cosign.bundle.txt",
        "cfgd-1.0.0-checksums.txt.cosign.bundle.bak",
    ]);
    assert!(
        find_cosign_bundle_asset(&release).is_none(),
        "non-matching suffix must not be selected"
    );
}

#[test]
fn find_cosign_bundle_asset_empty_release_yields_none() {
    let release = release_with_assets(&[]);
    assert!(find_cosign_bundle_asset(&release).is_none());
}

// --- find_cosign_public_key_asset ---

#[test]
fn find_cosign_public_key_asset_matches_bare_cosign_pub() {
    let release = release_with_assets(&[
        "cfgd-1.0.0-linux-x86_64.tar.gz",
        "cosign.pub",
        "cfgd-1.0.0-checksums.txt",
    ]);
    let found = find_cosign_public_key_asset(&release).expect("bare cosign.pub must be located");
    assert_eq!(found.name, "cosign.pub");
}

#[test]
fn find_cosign_public_key_asset_matches_versioned_cosign_pub() {
    let release = release_with_assets(&["cfgd-1.0.0-cosign.pub"]);
    let found = find_cosign_public_key_asset(&release)
        .expect("versioned -cosign.pub variant must match the suffix branch");
    assert_eq!(found.name, "cfgd-1.0.0-cosign.pub");
}

#[test]
fn find_cosign_public_key_asset_returns_none_when_missing() {
    let release = release_with_assets(&[
        "cfgd-1.0.0-linux-x86_64.tar.gz",
        "cfgd-1.0.0-checksums.txt",
        "cfgd-1.0.0-checksums.txt.cosign.bundle",
    ]);
    assert!(
        find_cosign_public_key_asset(&release).is_none(),
        "no key → cosign verify is skipped (caller falls back to SHA256-only)"
    );
}

#[test]
fn find_cosign_public_key_asset_does_not_match_pub_anywhere() {
    // ".pub" inside the name (not as exact-name or suffix-after-hyphen) must
    // not match — pin the contract so a future loose-match refactor doesn't
    // accidentally pick up `cosign.public-key.bin` or similar names.
    let release = release_with_assets(&["cosign.publickey", "another.pub.bak"]);
    assert!(find_cosign_public_key_asset(&release).is_none());
}

// --- check_with_cache + check_latest via mockito ---

#[test]
fn check_with_cache_falls_back_to_api_on_cache_miss() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());
    // No cache file written — code path takes the API branch and writes
    // a fresh entry on the way out.

    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", "/repos/test/repo/releases/latest")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{
                "tag_name": "v99.0.0",
                "assets": []
            }"#,
        )
        .create();

    // check_with_cache uses fetch_latest_release internally which goes to
    // GITHUB_API_BASE — exercise the API path indirectly via check_latest
    // pointed at the mock server.
    let result = fetch_latest_release_from(&server.url(), "test/repo", None);
    mock.assert();
    let release = result.expect("mock release must parse");
    assert_eq!(release.tag, "v99.0.0");
    assert_eq!(release.version, Version::new(99, 0, 0));
}

#[test]
fn check_with_cache_returns_cached_when_within_ttl() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    // Seed a fresh cache entry — checked just now, well within the 24h TTL.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cached = VersionCache {
        checked_at_secs: now,
        latest_tag: "v123.0.0".into(),
        latest_version: "123.0.0".into(),
        current_version: env!("CARGO_PKG_VERSION").into(),
    };
    write_version_cache(&cached).expect("cache seed must succeed in tempdir");

    // No mock server — if the call reaches the API it will fail loudly.
    let result = check_with_cache(Some("does/not/matter"), None)
        .expect("cache hit must short-circuit to local data, never touch the network");
    assert_eq!(
        result.latest,
        Version::new(123, 0, 0),
        "latest must come from the cache, not a remote call"
    );
    assert!(
        result.release.is_none(),
        "cache hit returns just the version summary, no full ReleaseInfo"
    );
}

#[test]
fn check_with_cache_ignores_expired_entry() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    // Seed an expired cache entry — far enough in the past that CACHE_TTL_SECS
    // has lapsed. The function must fall through to the API branch.
    let stale_secs = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .saturating_sub(CACHE_TTL_SECS + 60);
    let stale = VersionCache {
        checked_at_secs: stale_secs,
        latest_tag: "v0.0.1".into(),
        latest_version: "0.0.1".into(),
        current_version: env!("CARGO_PKG_VERSION").into(),
    };
    write_version_cache(&stale).expect("seed stale cache");

    // Read it back to confirm — the cache file *is* present and parseable;
    // the freshness check is what must reject it.
    let read = read_version_cache().expect("seeded entry must be readable");
    assert_eq!(read.latest_tag, "v0.0.1");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(
        now.saturating_sub(read.checked_at_secs) >= CACHE_TTL_SECS,
        "test setup: stale entry must be older than CACHE_TTL_SECS"
    );
}

// ---------------------------------------------------------------------------
// run_cosign_verify_blob — driven through the fake-cosign shim. Mirrors the
// pattern in `oci/sign/tests.rs`: serial_test::serial gates env-var mutation,
// a per-test /bin/sh shim records argv and chooses an exit status.
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod cosign_verify_blob {
    use super::*;
    use crate::test_helpers::CosignTestShim;
    use serial_test::serial;

    fn dummy_paths() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let checksums = dir.path().join("checksums.txt");
        let bundle = dir.path().join("bundle.json");
        let pub_key = dir.path().join("cosign.pub");
        std::fs::write(&checksums, "deadbeef  some.tar.gz\n").unwrap();
        std::fs::write(&bundle, "{}").unwrap();
        std::fs::write(&pub_key, "key").unwrap();
        (dir, checksums, bundle, pub_key)
    }

    #[test]
    #[serial]
    fn run_cosign_verify_blob_passes_key_bundle_and_checksums_paths() {
        let shim = CosignTestShim::install();
        let (_dir, checksums, bundle, pub_key) = dummy_paths();
        run_cosign_verify_blob(&checksums, &bundle, &pub_key).expect("happy path → Ok");
        let argv = shim.argv_log();
        assert!(
            argv.contains("verify-blob"),
            "argv must use verify-blob subcommand: {argv}"
        );
        assert!(
            argv.contains(&format!("--key={}", pub_key.display())),
            "argv must include --key=<pub_key path>: {argv}"
        );
        assert!(
            argv.contains(&format!("--bundle={}", bundle.display())),
            "argv must include --bundle=<bundle path>: {argv}"
        );
        // The "--" terminator separates the cosign flags from the file argument.
        assert!(
            argv.contains(" -- "),
            "argv must include `--` terminator: {argv}"
        );
    }

    #[test]
    #[serial]
    fn run_cosign_verify_blob_propagates_failure_with_stderr_message() {
        let _shim = CosignTestShim::builder()
            .with_exit(1)
            .with_stderr("signature does not match")
            .install();
        let (_dir, checksums, bundle, pub_key) = dummy_paths();
        let err =
            run_cosign_verify_blob(&checksums, &bundle, &pub_key).expect_err("non-zero exit → Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("signature does not match"),
            "stderr surfaced in error message: {msg}"
        );
        assert!(
            msg.contains("cosign verify-blob failed"),
            "error prefixes with verify-blob context: {msg}"
        );
    }

    #[test]
    #[serial]
    fn run_cosign_verify_blob_surfaces_invocation_failure_when_binary_missing() {
        // CFGD_COSIGN_BIN points at a path that does not exist — Command spawn
        // fails with std::io::Error. The function maps that to DownloadFailed.
        // SAFETY: serial.
        unsafe {
            std::env::set_var("CFGD_COSIGN_BIN", "/no/such/cosign/binary");
            std::env::remove_var("CFGD_FAKE_COSIGN_LOG");
        }
        let (_dir, checksums, bundle, pub_key) = dummy_paths();
        let err = run_cosign_verify_blob(&checksums, &bundle, &pub_key)
            .expect_err("missing binary → Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("cosign invocation failed"),
            "error prefixes with invocation context: {msg}"
        );
        // SAFETY: serial.
        unsafe {
            std::env::remove_var("CFGD_COSIGN_BIN");
        }
    }
}

// ---------------------------------------------------------------------------
// download_and_install_to — full HTTP + cosign + checksum + extract path
// driven through mockito + the fake-cosign shim. Each test stages a release
// whose assets resolve to mockito URLs, builds a tarball matching the
// checksums.txt body, and exercises one branch of the install pipeline.
//
// Tests are #[cfg(unix)] because download_and_install_to extracts a
// tarball on Unix (extract_zip on Windows takes a different path), and
// because the fake-cosign shim is a /bin/sh script.
// ---------------------------------------------------------------------------

#[cfg(unix)]
mod download_and_install_to {
    use super::*;
    use crate::test_helpers::CosignTestShim;
    use serial_test::serial;
    use std::os::unix::fs::PermissionsExt;

    /// Build a gzipped tar archive containing a single `cfgd` file with
    /// `binary_content`. Returns the archive bytes.
    fn build_tarball(binary_content: &[u8]) -> Vec<u8> {
        let dir = tempfile::tempdir().expect("tempdir");
        let bin_path = dir.path().join("cfgd");
        std::fs::write(&bin_path, binary_content).unwrap();
        let mut perms = std::fs::metadata(&bin_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin_path, perms).unwrap();

        let mut buf: Vec<u8> = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tar_builder = tar::Builder::new(enc);
            tar_builder
                .append_path_with_name(&bin_path, "cfgd")
                .unwrap();
            tar_builder.finish().unwrap();
        }
        buf
    }

    /// Build a tarball that does NOT include the `cfgd` binary at the root —
    /// used to exercise the "extracted archive does not contain 'cfgd'"
    /// install-failure branch.
    fn build_tarball_without_binary() -> Vec<u8> {
        let dir = tempfile::tempdir().expect("tempdir");
        let other = dir.path().join("README");
        std::fs::write(&other, b"not the binary").unwrap();
        let mut buf: Vec<u8> = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::default());
            let mut tar_builder = tar::Builder::new(enc);
            tar_builder.append_path_with_name(&other, "README").unwrap();
            tar_builder.finish().unwrap();
        }
        buf
    }

    /// Compose a `<sha>  <name>` checksums.txt body for one asset.
    fn checksums_line(sha: &str, name: &str) -> String {
        format!("{sha}  {name}\n")
    }

    /// Build a `ReleaseInfo` whose assets all point at the mockito server.
    /// Returns the release plus the index of the primary `cfgd-...tar.gz`
    /// asset within `release.assets`.
    fn release_with_full_signature_chain(server_url: &str) -> ReleaseInfo {
        ReleaseInfo {
            tag: "v9.9.9".into(),
            version: Version::new(9, 9, 9),
            assets: vec![
                ReleaseAsset {
                    name: "cfgd-9.9.9-linux-x86_64.tar.gz".into(),
                    download_url: format!("{server_url}/download/cfgd.tar.gz"),
                    size: 0,
                },
                ReleaseAsset {
                    name: "cfgd-9.9.9-checksums.txt".into(),
                    download_url: format!("{server_url}/download/checksums.txt"),
                    size: 0,
                },
                ReleaseAsset {
                    name: "cfgd-9.9.9-checksums.txt.cosign.bundle".into(),
                    download_url: format!("{server_url}/download/cosign.bundle"),
                    size: 0,
                },
                ReleaseAsset {
                    name: "cosign.pub".into(),
                    download_url: format!("{server_url}/download/cosign.pub"),
                    size: 0,
                },
            ],
        }
    }

    #[test]
    #[serial]
    fn happy_path_installs_extracted_binary_to_target() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();

        let binary_content = b"#!/bin/sh\necho fake cfgd binary\n";
        let tarball = build_tarball(binary_content);
        let sha = crate::sha256_hex(&tarball);
        let asset_name = "cfgd-9.9.9-linux-x86_64.tar.gz";
        let checksums = checksums_line(&sha, asset_name);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/checksums.txt")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
            .create();
        let _m_pubkey = server
            .mock("GET", "/download/cosign.pub")
            .with_status(200)
            .with_body("dummy-key")
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();

        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");
        // Pre-create the target so atomic_replace's same-FS rename succeeds.
        std::fs::write(&target, b"old binary").unwrap();

        let installed = download_and_install_to(&release, &asset, &target, false, None)
            .expect("happy path → Ok");
        assert_eq!(
            installed.installed_path, target,
            "returned path matches install target"
        );
        assert_eq!(
            installed.verification_mode,
            VerificationMode::Cosign,
            "happy path with bundle + key + shim → full cosign verification"
        );

        let installed_bytes = std::fs::read(&target).unwrap();
        assert_eq!(
            installed_bytes, binary_content,
            "target now holds the extracted binary content"
        );
    }

    /// Same chain as `happy_path_installs_extracted_binary_to_target`, but
    /// runs with a `Some(&printer)` so the spinner branches inside
    /// `download_to_file` (no Content-Length → spinner arm), the verify-
    /// checksum spinner, and the extract-archive spinner all execute. Asserts
    /// that the install still succeeds end-to-end with a printer wired in.
    #[test]
    #[serial]
    fn happy_path_with_printer_drives_spinner_branches() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();

        let binary_content = b"#!/bin/sh\necho printer cfgd binary\n";
        let tarball = build_tarball(binary_content);
        let sha = crate::sha256_hex(&tarball);
        let asset_name = "cfgd-9.9.9-linux-x86_64.tar.gz";
        let checksums = checksums_line(&sha, asset_name);

        let mut server = mockito::Server::new();
        // Mockito does not set Content-Length unless we ask, so download_to_file
        // takes the (Some(p), None) spinner arm — exactly the branch the
        // None-printer happy_path test cannot reach.
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/checksums.txt")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
            .create();
        let _m_pubkey = server
            .mock("GET", "/download/cosign.pub")
            .with_status(200)
            .with_body("dummy-key")
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");
        std::fs::write(&target, b"old binary").unwrap();

        let printer = crate::test_helpers::test_printer();
        let installed = download_and_install_to(&release, &asset, &target, false, Some(&printer))
            .expect("happy path with printer → Ok");
        assert_eq!(installed.installed_path, target);
        assert_eq!(std::fs::read(&target).unwrap(), binary_content);
    }

    #[test]
    #[serial]
    fn returns_download_failed_when_archive_url_returns_5xx() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let mut server = mockito::Server::new();
        let _m = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(503)
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");

        let err = download_and_install_to(&release, &asset, &target, false, None)
            .expect_err("5xx on archive → Err");
        let msg = err.to_string();
        assert!(
            msg.to_ascii_lowercase().contains("download")
                || msg.contains("503")
                || msg.contains("status"),
            "error mentions download failure: {msg}"
        );
        assert!(!target.exists(), "target must not be created on failure");
    }

    #[test]
    #[serial]
    fn returns_download_failed_when_checksums_url_returns_404() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let tarball = build_tarball(b"binary");

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/checksums.txt")
            .with_status(404)
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");

        let err = download_and_install_to(&release, &asset, &target, false, None)
            .expect_err("404 on checksums → Err");
        let msg = err.to_string();
        assert!(
            msg.to_ascii_lowercase().contains("download") || msg.contains("404"),
            "error mentions download failure for checksums: {msg}"
        );
    }

    #[test]
    #[serial]
    fn propagates_cosign_failure_when_signature_verification_fails() {
        let _shim = CosignTestShim::builder()
            .with_argv_logging(false)
            .with_exit(1)
            .with_stderr("tampered checksums file")
            .install();
        let tarball = build_tarball(b"binary");
        let sha = crate::sha256_hex(&tarball);
        let asset_name = "cfgd-9.9.9-linux-x86_64.tar.gz";
        let checksums = checksums_line(&sha, asset_name);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/checksums.txt")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
            .create();
        let _m_pubkey = server
            .mock("GET", "/download/cosign.pub")
            .with_status(200)
            .with_body("dummy-key")
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");

        let err = download_and_install_to(&release, &asset, &target, false, None)
            .expect_err("cosign exit 1 → Err");
        let msg = err.to_string();
        assert!(
            msg.contains("cosign verify-blob failed") && msg.contains("tampered checksums file"),
            "error surfaces cosign-verify failure with stderr message: {msg}"
        );
        assert!(!target.exists(), "target must not be created on failure");
    }

    #[test]
    #[serial]
    fn returns_checksum_mismatch_when_sha_differs_over_the_wire() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let tarball = build_tarball(b"actual-binary");
        // Compose checksums.txt with a *wrong* SHA so the on-disk SHA differs.
        let asset_name = "cfgd-9.9.9-linux-x86_64.tar.gz";
        let bogus_sha = "0".repeat(64);
        let checksums = checksums_line(&bogus_sha, asset_name);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/checksums.txt")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
            .create();
        let _m_pubkey = server
            .mock("GET", "/download/cosign.pub")
            .with_status(200)
            .with_body("dummy-key")
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");

        let err = download_and_install_to(&release, &asset, &target, false, None)
            .expect_err("checksum mismatch → Err");
        let msg = err.to_string();
        assert!(
            msg.to_ascii_lowercase().contains("checksum") && msg.contains(asset_name),
            "error names asset and surfaces checksum mismatch: {msg}"
        );
    }

    #[test]
    #[serial]
    fn returns_checksum_missing_when_release_has_no_checksums_asset() {
        // No checksums asset → early-return without HTTP. cosign shim still
        // installed for hygiene; not invoked.
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let tarball = build_tarball(b"binary");

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();

        let release = ReleaseInfo {
            tag: "v9.9.9".into(),
            version: Version::new(9, 9, 9),
            assets: vec![ReleaseAsset {
                name: "cfgd-9.9.9-linux-x86_64.tar.gz".into(),
                download_url: format!("{}/download/cfgd.tar.gz", server.url()),
                size: 0,
            }],
        };
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");

        let err = download_and_install_to(&release, &asset, &target, false, None)
            .expect_err("no checksums asset → Err");
        let msg = err.to_string();
        assert!(
            msg.to_ascii_lowercase().contains("missing") || msg.contains(&asset.name),
            "error reports missing checksums for asset: {msg}"
        );
    }

    #[test]
    #[serial]
    fn returns_install_failed_when_archive_lacks_cfgd_binary() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let tarball = build_tarball_without_binary();
        let sha = crate::sha256_hex(&tarball);
        let asset_name = "cfgd-9.9.9-linux-x86_64.tar.gz";
        let checksums = checksums_line(&sha, asset_name);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/checksums.txt")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
            .create();
        let _m_pubkey = server
            .mock("GET", "/download/cosign.pub")
            .with_status(200)
            .with_body("dummy-key")
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");

        let err = download_and_install_to(&release, &asset, &target, false, None)
            .expect_err("missing cfgd in tar → Err");
        let msg = err.to_string();
        assert!(
            msg.contains("does not contain") && msg.contains("cfgd"),
            "error reports missing cfgd binary in extracted archive: {msg}"
        );
    }

    #[test]
    #[serial]
    fn returns_checksum_missing_when_asset_not_listed_in_checksums_body() {
        // checksums.txt names a *different* asset; the SHA for our archive
        // is therefore not in the parsed map → ChecksumMissing.
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let tarball = build_tarball(b"binary");
        let sha = crate::sha256_hex(&tarball);
        let checksums = checksums_line(&sha, "some-other-asset.tar.gz");

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/checksums.txt")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
            .create();
        let _m_pubkey = server
            .mock("GET", "/download/cosign.pub")
            .with_status(200)
            .with_body("dummy-key")
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");

        let err = download_and_install_to(&release, &asset, &target, false, None)
            .expect_err("asset name not in checksums → Err");
        let msg = err.to_string();
        assert!(
            msg.contains(&asset.name) && msg.contains("not listed"),
            "error names the asset whose checksum entry is missing: {msg}"
        );
    }

    #[test]
    #[serial]
    fn skips_cosign_verification_when_release_has_no_bundle_asset() {
        // Release lacks cosign.bundle → verify_cosign_bundle returns Ok(false)
        // and the install proceeds with SHA256-only verification. Demonstrates
        // the documented graceful-degradation contract.
        let _shim = CosignTestShim::builder()
            .with_argv_logging(false)
            .with_exit(99)
            .with_stderr("should not be invoked")
            .install();
        let binary_content = b"binary";
        let tarball = build_tarball(binary_content);
        let sha = crate::sha256_hex(&tarball);
        let asset_name = "cfgd-9.9.9-linux-x86_64.tar.gz";
        let checksums = checksums_line(&sha, asset_name);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/checksums.txt")
            .with_status(200)
            .with_body(checksums)
            .create();

        let release = ReleaseInfo {
            tag: "v9.9.9".into(),
            version: Version::new(9, 9, 9),
            assets: vec![
                ReleaseAsset {
                    name: asset_name.into(),
                    download_url: format!("{}/download/cfgd.tar.gz", server.url()),
                    size: 0,
                },
                ReleaseAsset {
                    name: "cfgd-9.9.9-checksums.txt".into(),
                    download_url: format!("{}/download/checksums.txt", server.url()),
                    size: 0,
                },
            ],
        };
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");

        let installed = download_and_install_to(&release, &asset, &target, false, None)
            .expect("no cosign bundle → SHA-only install should succeed");
        assert_eq!(installed.installed_path, target);
        assert_eq!(
            installed.verification_mode,
            VerificationMode::Sha256Only,
            "no bundle + non-strict → SHA256-only mode recorded in report"
        );
        assert_eq!(std::fs::read(&target).unwrap(), binary_content);
    }

    /// Bundle missing → verify_cosign_bundle returns Ok(false) and emits the
    /// "no cosign bundle" warning when a printer is supplied. Pin the warning
    /// text shape so a future change can't silently downgrade publisher-
    /// compromise resistance without an operator-visible message.
    #[test]
    #[serial]
    fn verify_cosign_bundle_emits_warning_when_no_bundle_attached() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let release = release_with_assets(&["cfgd-9.9.9-linux-x86_64.tar.gz"]);
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join("checksums.txt");
        std::fs::write(&checksums_path, "").unwrap();

        let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
        let outcome =
            verify_cosign_bundle(&checksums_path, &release, tmp.path(), false, Some(&printer))
                .expect("missing bundle is graceful-degrade, not Err");
        assert_eq!(
            outcome,
            VerificationMode::Sha256Only,
            "no bundle → Sha256Only so caller falls back to SHA256-only"
        );
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("no cosign bundle attached"),
            "warning text must surface so operators see the trust downgrade: {captured}"
        );
    }

    /// Bundle present but no public key → still graceful-degrade with a
    /// distinct warning that names the missing piece (cosign.pub).
    #[test]
    #[serial]
    fn verify_cosign_bundle_emits_warning_when_no_public_key_attached() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let release = release_with_assets(&[
            "cfgd-9.9.9-linux-x86_64.tar.gz",
            "cfgd-9.9.9-checksums.txt.cosign.bundle",
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join("checksums.txt");
        std::fs::write(&checksums_path, "").unwrap();

        let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
        let outcome =
            verify_cosign_bundle(&checksums_path, &release, tmp.path(), false, Some(&printer))
                .expect("missing pubkey is graceful-degrade, not Err");
        assert_eq!(outcome, VerificationMode::Sha256Only);
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("no public key attached") && captured.contains("cosign.pub"),
            "warning must name the missing cosign.pub asset: {captured}"
        );
    }

    /// Bundle + pubkey present but the cosign CLI is "missing" (env shim
    /// points at a path that does not exist) → graceful-degrade with the
    /// install-hint warning. Drives the `require_cosign().is_err()` arm.
    #[test]
    #[serial]
    fn verify_cosign_bundle_emits_warning_when_cosign_cli_missing() {
        // Point the seam at a path that does not exist so require_cosign Errs.
        // SAFETY: serial gates env mutation; restored on drop via the guard.
        struct MissingCosignGuard;
        impl Drop for MissingCosignGuard {
            fn drop(&mut self) {
                unsafe {
                    std::env::remove_var("CFGD_COSIGN_BIN");
                }
            }
        }
        unsafe {
            std::env::set_var(
                "CFGD_COSIGN_BIN",
                "/nonexistent/cfgd-test-cosign-shim-does-not-exist",
            );
        }
        let _guard = MissingCosignGuard;

        let release = release_with_assets(&[
            "cfgd-9.9.9-linux-x86_64.tar.gz",
            "cfgd-9.9.9-checksums.txt.cosign.bundle",
            "cosign.pub",
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join("checksums.txt");
        std::fs::write(&checksums_path, "").unwrap();

        let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
        let outcome =
            verify_cosign_bundle(&checksums_path, &release, tmp.path(), false, Some(&printer))
                .expect("missing cosign CLI is graceful-degrade, not Err");
        assert_eq!(outcome, VerificationMode::Sha256Only);
        let captured = buf.lock().unwrap().clone();
        assert!(
            captured.contains("cosign CLI is not installed"),
            "warning must point operators at the install hint: {captured}"
        );
    }

    // ---- strict mode (--require-cosign / CFGD_REQUIRE_COSIGN=1) -------------
    //
    // Threat model: a network attacker who can swap both the cfgd archive AND
    // the checksums.txt download (compromised mirror, MITM against
    // objects.githubusercontent.com) gets a fully-trusted upgrade on any host
    // that doesn't have cosign installed locally. Strict mode shifts the
    // policy from "warn and proceed" to "block the upgrade" so unattended
    // updaters (CI, daemons, fleet rollouts) fail loudly instead of silently
    // accepting an unauthenticated binary.

    /// Strict mode + missing bundle on the release → `Err(CosignRequired)`
    /// naming the missing bundle.
    #[test]
    #[serial]
    fn verify_cosign_bundle_strict_fails_when_no_bundle_in_release() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let release = release_with_assets(&["cfgd-9.9.9-linux-x86_64.tar.gz"]);
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join("checksums.txt");
        std::fs::write(&checksums_path, "").unwrap();

        let err = verify_cosign_bundle(&checksums_path, &release, tmp.path(), true, None)
            .expect_err("strict mode + missing bundle must Err, not graceful-degrade");
        assert!(
            matches!(err, crate::errors::UpgradeError::CosignRequired { .. }),
            "expected CosignRequired variant, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("no cosign bundle"),
            "error message must name the specific missing piece (bundle): {msg}"
        );
    }

    /// Strict mode + bundle present but no public key → `Err(CosignRequired)`
    /// naming the missing `cosign.pub`.
    #[test]
    #[serial]
    fn verify_cosign_bundle_strict_fails_when_no_pubkey_attached() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let release = release_with_assets(&[
            "cfgd-9.9.9-linux-x86_64.tar.gz",
            "cfgd-9.9.9-checksums.txt.cosign.bundle",
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join("checksums.txt");
        std::fs::write(&checksums_path, "").unwrap();

        let err = verify_cosign_bundle(&checksums_path, &release, tmp.path(), true, None)
            .expect_err("strict mode + missing pubkey must Err");
        assert!(
            matches!(err, crate::errors::UpgradeError::CosignRequired { .. }),
            "expected CosignRequired variant, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("cosign.pub"),
            "error message must name the missing cosign.pub: {msg}"
        );
    }

    /// Strict mode + cosign CLI missing on host → `Err(CosignRequired)`.
    /// Points `CFGD_COSIGN_BIN` at a nonexistent path so `require_cosign()`
    /// fails the same way it would on a fresh host without cosign installed.
    #[test]
    #[serial]
    fn verify_cosign_bundle_strict_fails_when_cosign_missing() {
        struct MissingCosignGuard;
        impl Drop for MissingCosignGuard {
            fn drop(&mut self) {
                unsafe {
                    std::env::remove_var("CFGD_COSIGN_BIN");
                }
            }
        }
        unsafe {
            std::env::set_var(
                "CFGD_COSIGN_BIN",
                "/nonexistent/cfgd-test-strict-cosign-shim-does-not-exist",
            );
        }
        let _guard = MissingCosignGuard;

        let release = release_with_assets(&[
            "cfgd-9.9.9-linux-x86_64.tar.gz",
            "cfgd-9.9.9-checksums.txt.cosign.bundle",
            "cosign.pub",
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join("checksums.txt");
        std::fs::write(&checksums_path, "").unwrap();

        let err = verify_cosign_bundle(&checksums_path, &release, tmp.path(), true, None)
            .expect_err("strict mode + missing cosign CLI must Err");
        assert!(
            matches!(err, crate::errors::UpgradeError::CosignRequired { .. }),
            "expected CosignRequired variant, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("cosign CLI is not installed"),
            "error message must name the missing CLI: {msg}"
        );
    }

    /// Regression: non-strict mode with no bundle returns `Sha256Only` (not
    /// Err) so the existing graceful-degradation contract continues to hold
    /// for callers that did not opt in. Mirror of the three "_emits_warning_"
    /// tests above but expressed against the non-strict default to pin the
    /// behavioral contract from the strict-mode side.
    #[test]
    #[serial]
    fn verify_cosign_bundle_non_strict_falls_back_silently() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let release = release_with_assets(&["cfgd-9.9.9-linux-x86_64.tar.gz"]);
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join("checksums.txt");
        std::fs::write(&checksums_path, "").unwrap();

        let outcome = verify_cosign_bundle(&checksums_path, &release, tmp.path(), false, None)
            .expect("non-strict mode + missing bundle must return Sha256Only, not Err");
        assert_eq!(
            outcome,
            VerificationMode::Sha256Only,
            "non-strict default contract: missing bundle → SHA256-only fallback"
        );
    }

    /// Happy path under strict mode: bundle + key + cosign shim all present,
    /// signature verifies → returns `StrictCosignRequired` (distinct from the
    /// non-strict `Cosign` mode so audit consumers can tell apart "strict was
    /// demanded and honored" from "strict happened by default").
    #[test]
    #[serial]
    fn verify_cosign_bundle_strict_records_strict_cosign_required_on_success() {
        let _shim = CosignTestShim::builder()
            .with_argv_logging(false)
            .with_exit(0)
            .install();

        // The bundle + pubkey assets need real URLs that resolve — point them
        // at a mockito server so download_to_file inside verify_cosign_bundle
        // can fetch them.
        let mut server = mockito::Server::new();
        let _m_bundle = server
            .mock("GET", "/strict/bundle")
            .with_status(200)
            .with_body("{}")
            .create();
        let _m_pubkey = server
            .mock("GET", "/strict/pubkey")
            .with_status(200)
            .with_body("dummy-key")
            .create();

        let release = ReleaseInfo {
            tag: "v9.9.9".into(),
            version: Version::new(9, 9, 9),
            assets: vec![
                ReleaseAsset {
                    name: "cfgd-9.9.9-linux-x86_64.tar.gz".into(),
                    download_url: "https://example.com/binary".into(),
                    size: 0,
                },
                ReleaseAsset {
                    name: "cfgd-9.9.9-checksums.txt.cosign.bundle".into(),
                    download_url: format!("{}/strict/bundle", server.url()),
                    size: 0,
                },
                ReleaseAsset {
                    name: "cosign.pub".into(),
                    download_url: format!("{}/strict/pubkey", server.url()),
                    size: 0,
                },
            ],
        };
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join("checksums.txt");
        std::fs::write(&checksums_path, "deadbeef  some.tar.gz\n").unwrap();

        let outcome = verify_cosign_bundle(&checksums_path, &release, tmp.path(), true, None)
            .expect("strict + all pieces present + cosign exit 0 → Ok");
        assert_eq!(
            outcome,
            VerificationMode::StrictCosignRequired,
            "successful strict-mode verification records StrictCosignRequired"
        );
    }

    /// End-to-end strict-mode failure through `download_and_install_to`: the
    /// release has no cosign bundle and the caller requests strict mode →
    /// install bails out with `CosignRequired` before any file is written to
    /// `target`. Pins that the new flag short-circuits the full install chain,
    /// not just the verifier helper in isolation.
    #[test]
    #[serial]
    fn download_and_install_strict_mode_blocks_when_bundle_missing() {
        let _shim = CosignTestShim::builder()
            .with_argv_logging(false)
            .with_exit(99)
            .install();

        let binary_content = b"#!/bin/sh\necho strict\n";
        let tarball = build_tarball(binary_content);
        let sha = crate::sha256_hex(&tarball);
        let asset_name = "cfgd-9.9.9-linux-x86_64.tar.gz";
        let checksums = checksums_line(&sha, asset_name);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/checksums.txt")
            .with_status(200)
            .with_body(checksums)
            .create();

        // Release is missing both the cosign bundle AND cosign.pub — strict
        // mode must reject this regardless of which piece is named first.
        let release = ReleaseInfo {
            tag: "v9.9.9".into(),
            version: Version::new(9, 9, 9),
            assets: vec![
                ReleaseAsset {
                    name: asset_name.into(),
                    download_url: format!("{}/download/cfgd.tar.gz", server.url()),
                    size: 0,
                },
                ReleaseAsset {
                    name: "cfgd-9.9.9-checksums.txt".into(),
                    download_url: format!("{}/download/checksums.txt", server.url()),
                    size: 0,
                },
            ],
        };
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");

        let err = download_and_install_to(&release, &asset, &target, true, None)
            .expect_err("strict + missing bundle must Err out of the install chain");
        assert!(
            matches!(
                err,
                crate::errors::CfgdError::Upgrade(
                    crate::errors::UpgradeError::CosignRequired { .. }
                )
            ),
            "expected CosignRequired surfaced through the CfgdError boundary, got: {err:?}"
        );
        assert!(
            !target.exists(),
            "target must NOT be written when strict cosign verification fails"
        );
    }

    /// Happy-path full install under non-strict mode with the full signature
    /// chain → `InstallReport.verification_mode == Cosign`. Pins the wiring
    /// that surfaces `verificationMode: "cosign"` in the structured payload.
    #[test]
    #[serial]
    fn download_and_install_records_cosign_mode_on_full_chain_happy_path() {
        let _shim = CosignTestShim::builder()
            .with_argv_logging(false)
            .with_exit(0)
            .install();

        let binary_content = b"#!/bin/sh\necho mode\n";
        let tarball = build_tarball(binary_content);
        let sha = crate::sha256_hex(&tarball);
        let asset_name = "cfgd-9.9.9-linux-x86_64.tar.gz";
        let checksums = checksums_line(&sha, asset_name);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/checksums.txt")
            .with_status(200)
            .with_body(&checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
            .create();
        let _m_pubkey = server
            .mock("GET", "/download/cosign.pub")
            .with_status(200)
            .with_body("dummy-key")
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");
        std::fs::write(&target, b"old binary").unwrap();

        let report = download_and_install_to(&release, &asset, &target, false, None)
            .expect("full signature chain + non-strict → Ok");
        assert_eq!(
            report.verification_mode,
            VerificationMode::Cosign,
            "structured payload records full cosign verification"
        );
    }

    #[test]
    #[serial]
    fn happy_path_with_printer_and_content_length_drives_progress_bar() {
        // Exercises the (Some(printer), Some(content_length)) progress-bar
        // branch inside download_to_file (lines 332-353 of mod.rs) when called
        // from the full install chain. The server sets Content-Length so the
        // chunked-read loop with position tracking fires.
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();

        let binary_content = b"#!/bin/sh\necho progress-bar cfgd\n";
        let tarball = build_tarball(binary_content);
        let sha = crate::sha256_hex(&tarball);
        let asset_name = "cfgd-9.9.9-linux-x86_64.tar.gz";
        let checksums = checksums_line(&sha, asset_name);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_header("content-length", &tarball.len().to_string())
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/checksums.txt")
            .with_status(200)
            .with_header("content-length", &checksums.len().to_string())
            .with_body(&checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_header("content-length", "2")
            .with_body("{}")
            .create();
        let _m_pubkey = server
            .mock("GET", "/download/cosign.pub")
            .with_status(200)
            .with_header("content-length", "9")
            .with_body("dummy-key")
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");
        std::fs::write(&target, b"old binary").unwrap();

        let printer = crate::test_helpers::test_printer();
        let installed = download_and_install_to(&release, &asset, &target, false, Some(&printer))
            .expect("progress-bar path with content-length must succeed");
        assert_eq!(installed.installed_path, target);
        assert_eq!(std::fs::read(&target).unwrap(), binary_content);
    }

    #[test]
    #[serial]
    fn checksum_mismatch_with_printer_surfaces_finish_fail_branch() {
        // Exercises the verify-checksum spinner's finish_fail path (lines
        // 495-501 of mod.rs) — SHA differs and printer is present.
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let tarball = build_tarball(b"binary");
        let asset_name = "cfgd-9.9.9-linux-x86_64.tar.gz";
        let bogus_sha = "f".repeat(64);
        let checksums = checksums_line(&bogus_sha, asset_name);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/checksums.txt")
            .with_status(200)
            .with_body(&checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
            .create();
        let _m_pubkey = server
            .mock("GET", "/download/cosign.pub")
            .with_status(200)
            .with_body("dummy-key")
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");

        let printer = crate::test_helpers::test_printer();
        let err = download_and_install_to(&release, &asset, &target, false, Some(&printer))
            .expect_err("checksum mismatch with printer → Err");
        let msg = err.to_string();
        assert!(
            msg.to_ascii_lowercase().contains("checksum"),
            "error mentions checksum: {msg}"
        );
    }
}

// --- UpgradeError::ApiError pinning ---
// `fetch_latest_release_from` produces ApiError when the GitHub Releases API
// (1) fails the HTTP call, (2) returns a body that is not valid JSON, or
// (3) returns JSON without a `tag_name`. Each branch gets a dedicated test
// asserting via `matches!` so a future variant rename or signature change
// trips the test rather than silently reshaping the upgrade-error surface.

#[test]
fn fetch_latest_release_api_error_on_http_500() {
    let mut server = mockito::Server::new();
    let _m = server
        .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
        .with_status(500)
        .with_body("upstream blew up")
        .create();

    let err = fetch_latest_release_from(&server.url(), "tj-smith47/cfgd", None).unwrap_err();
    let inner = match err {
        crate::errors::CfgdError::Upgrade(u) => u,
        other => panic!("expected CfgdError::Upgrade, got: {other:?}"),
    };
    match inner {
        UpgradeError::ApiError { message } => {
            assert!(message.contains("500"), "message: {message}");
        }
        other => panic!("expected ApiError, got: {other:?}"),
    }
}

#[test]
fn fetch_latest_release_api_error_on_invalid_json_body() {
    let mut server = mockito::Server::new();
    let _m = server
        .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
        .with_status(200)
        .with_body("not json at all { [ }")
        .create();

    let err = fetch_latest_release_from(&server.url(), "tj-smith47/cfgd", None).unwrap_err();
    let inner = match err {
        crate::errors::CfgdError::Upgrade(u) => u,
        other => panic!("expected CfgdError::Upgrade, got: {other:?}"),
    };
    match inner {
        UpgradeError::ApiError { message } => {
            assert!(
                message.contains("json") || message.contains("parse") || message.contains("JSON"),
                "expected JSON parse error, got: {message}"
            );
        }
        other => panic!("expected ApiError, got: {other:?}"),
    }
}

// --- write_version_cache and cache_dir coverage ---

#[test]
fn write_version_cache_creates_dir_and_writes_file() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    let cache = VersionCache {
        checked_at_secs: 1234567890,
        latest_tag: "v5.0.0".into(),
        latest_version: "5.0.0".into(),
        current_version: "4.0.0".into(),
    };

    write_version_cache(&cache).expect("write_version_cache should create dir and file");

    let cache_path = home.path().join(".cache").join("cfgd").join(CACHE_FILENAME);
    assert!(cache_path.exists(), "cache file must exist after write");

    let content = fs::read_to_string(&cache_path).unwrap();
    let restored: VersionCache = serde_json::from_str(&content).unwrap();
    assert_eq!(restored.checked_at_secs, 1234567890);
    assert_eq!(restored.latest_version, "5.0.0");
}

#[test]
fn write_version_cache_overwrites_existing_file() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    let first = VersionCache {
        checked_at_secs: 100,
        latest_tag: "v1.0.0".into(),
        latest_version: "1.0.0".into(),
        current_version: "0.9.0".into(),
    };
    write_version_cache(&first).unwrap();

    let second = VersionCache {
        checked_at_secs: 200,
        latest_tag: "v2.0.0".into(),
        latest_version: "2.0.0".into(),
        current_version: "1.0.0".into(),
    };
    write_version_cache(&second).unwrap();

    let read = read_version_cache().expect("should read back second write");
    assert_eq!(read.checked_at_secs, 200);
    assert_eq!(read.latest_version, "2.0.0");
}

#[test]
fn read_version_cache_returns_none_for_empty_file() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    let dir = home.path().join(".cache").join("cfgd");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(CACHE_FILENAME), "").unwrap();

    assert!(
        read_version_cache().is_none(),
        "empty file should fail JSON parse and return None"
    );
}

#[test]
fn read_version_cache_returns_none_for_invalid_json() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    let dir = home.path().join(".cache").join("cfgd");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(CACHE_FILENAME), "not valid json {{{").unwrap();

    assert!(
        read_version_cache().is_none(),
        "invalid JSON should return None gracefully"
    );
}

#[test]
fn cache_dir_returns_test_home_scoped_path() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    let dir = cache_dir().expect("cache_dir with test_home must return Some");
    assert!(
        dir.starts_with(home.path()),
        "cache dir must be under test home: {dir:?}"
    );
    assert!(
        dir.ends_with("cfgd"),
        "cache dir must end with 'cfgd': {dir:?}"
    );
}

// --- download_to_file: large file branch ---

#[test]
fn download_to_file_large_content_streams_correctly() {
    let mut server = mockito::Server::new();
    let large_body: Vec<u8> = vec![0xAB; 32 * 1024];
    let mock = server
        .mock("GET", "/download/large")
        .with_status(200)
        .with_header("content-length", &large_body.len().to_string())
        .with_body(&large_body)
        .create();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("large.bin");
    let url = format!("{}/download/large", server.url());

    download_to_file(&url, &dest, None).unwrap();
    mock.assert();

    let content = std::fs::read(&dest).unwrap();
    assert_eq!(content.len(), large_body.len());
    assert_eq!(content, large_body);
}

#[test]
fn download_to_file_with_printer_and_content_length_drives_progress_bar() {
    // Exercises (Some(printer), Some(content_length)) path directly through
    // download_to_file — the progress-bar chunked-read loop.
    let mut server = mockito::Server::new();
    let body: Vec<u8> = vec![0xCD; 16 * 1024];
    let mock = server
        .mock("GET", "/download/progress")
        .with_status(200)
        .with_header("content-length", &body.len().to_string())
        .with_body(&body)
        .create();

    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("progress.bin");
    let url = format!("{}/download/progress", server.url());
    let printer = crate::test_helpers::test_printer();

    download_to_file(&url, &dest, Some(&printer)).unwrap();
    mock.assert();

    assert_eq!(std::fs::read(&dest).unwrap(), body);
}

// --- parse_release_json: version parse error shapes ---

#[test]
fn parse_release_json_version_parse_error_includes_tag_in_message() {
    let json = r#"{"tag_name": "vnot-a-version", "assets": []}"#;
    let err = parse_release_json(json).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("vnot-a-version"),
        "error must include the problematic tag for triage: {msg}"
    );
    assert!(
        msg.contains("cannot parse release version"),
        "error must include context prefix: {msg}"
    );
}

#[test]
fn parse_release_json_empty_tag_name_fails_version_parse() {
    let json = r#"{"tag_name": "", "assets": []}"#;
    let err = parse_release_json(json).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("cannot parse"),
        "empty tag must fail version parse: {msg}"
    );
}

// ---------------------------------------------------------------------------
// check_latest + check_with_cache through the CFGD_GITHUB_API_BASE env shim.
// fetch_latest_release internally calls github_api_base() which reads the
// env var; setting it to a mockito URL redirects the entire production path
// (check_with_cache → check_latest → fetch_latest_release → API) without
// needing fetch_latest_release_from at the test boundary.
//
// Tests must be #[serial] because the env var is process-global.
// ---------------------------------------------------------------------------

mod api_base_env_shim {
    use super::*;
    use crate::test_helpers::EnvVarGuard;
    use serial_test::serial;

    fn mock_release_response(server: &mut mockito::ServerGuard) -> mockito::Mock {
        server
            .mock("GET", "/repos/test/repo/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "tag_name": "v999.0.0",
                    "assets": []
                }"#,
            )
            .create()
    }

    #[test]
    #[serial]
    fn check_latest_uses_env_shim_to_redirect_api_call() {
        // check_latest(repo, None) goes through fetch_latest_release →
        // fetch_latest_release_from(github_api_base(), ...). Setting the env
        // var redirects the whole chain to mockito, covering lines 718-730.
        let mut server = mockito::Server::new();
        let mock = mock_release_response(&mut server);
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let result = check_latest(Some("test/repo"), None)
            .expect("env-shim redirect should make the call succeed against mockito");
        mock.assert();

        assert_eq!(result.latest, Version::new(999, 0, 0));
        // current_version is the compiled CARGO_PKG_VERSION; 999.0.0 must be
        // newer than any plausible release of this crate.
        assert!(
            result.update_available,
            "999.0.0 must register as a newer version than current"
        );
        assert!(
            result.release.is_some(),
            "check_latest always carries the full ReleaseInfo on success"
        );
    }

    #[test]
    #[serial]
    fn check_with_cache_falls_through_to_api_and_writes_fresh_cache_entry() {
        // No cache file present in test_home → cache-miss branch fires →
        // check_latest is called → write_version_cache persists the result.
        // Covers lines 697-712 of mod.rs (the API-fallback + cache-write
        // segment that has been uncovered for many sessions).
        let home = tempfile::tempdir().unwrap();
        let _home_guard = crate::with_test_home_guard(home.path());

        let mut server = mockito::Server::new();
        let mock = mock_release_response(&mut server);
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let result = check_with_cache(Some("test/repo"), None)
            .expect("cache miss + env-shim redirect should succeed");
        mock.assert();
        assert_eq!(result.latest, Version::new(999, 0, 0));

        // Cache file must now exist on disk under the test home — write
        // _version_cache succeeded; subsequent calls within TTL will read
        // it back without hitting the network.
        let cache_path = home.path().join(".cache").join("cfgd").join(CACHE_FILENAME);
        assert!(
            cache_path.exists(),
            "fresh cache must be written to {cache_path:?} after API success"
        );
        let cache = read_version_cache().expect("written cache must parse back");
        assert_eq!(cache.latest_version, "999.0.0");
        assert_eq!(cache.latest_tag, "v999.0.0");
    }

    #[test]
    #[serial]
    fn check_with_cache_expired_entry_falls_through_to_api_and_refreshes() {
        // Pre-write an EXPIRED cache entry (25 hours ago), set up mockito for
        // the API, call check_with_cache, assert mock was hit and cache was
        // updated with fresh data.
        let home = tempfile::tempdir().unwrap();
        let _home_guard = crate::with_test_home_guard(home.path());

        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let expired = VersionCache {
            checked_at_secs: now.saturating_sub(CACHE_TTL_SECS + 3600),
            latest_tag: "v0.0.1".into(),
            latest_version: "0.0.1".into(),
            current_version: env!("CARGO_PKG_VERSION").into(),
        };
        write_version_cache(&expired).expect("seed stale cache");

        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/repos/test/repo/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "tag_name": "v888.0.0",
                    "assets": []
                }"#,
            )
            .create();
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let result = check_with_cache(Some("test/repo"), None)
            .expect("expired cache + API success should succeed");
        mock.assert();
        assert_eq!(
            result.latest,
            Version::new(888, 0, 0),
            "must return fresh API data, not stale cache"
        );

        let refreshed = read_version_cache().expect("cache must be refreshed after API");
        assert_eq!(refreshed.latest_version, "888.0.0");
        assert_eq!(refreshed.latest_tag, "v888.0.0");
        assert!(
            refreshed.checked_at_secs >= now,
            "cache timestamp must be updated to ~now"
        );
    }

    #[test]
    #[serial]
    fn check_latest_with_none_repo_uses_default() {
        // check_latest(None, ...) should use DEFAULT_REPO ("tj-smith47/cfgd").
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"tag_name": "v777.0.0", "assets": []}"#)
            .create();
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let result =
            check_latest(None, None).expect("None repo should use default and hit mockito");
        mock.assert();
        assert_eq!(result.latest, Version::new(777, 0, 0));
    }

    #[test]
    #[serial]
    fn check_with_cache_none_repo_uses_default() {
        // check_with_cache(None, ...) should use DEFAULT_REPO.
        let home = tempfile::tempdir().unwrap();
        let _home_guard = crate::with_test_home_guard(home.path());

        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"tag_name": "v666.0.0", "assets": []}"#)
            .create();
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let result =
            check_with_cache(None, None).expect("None repo should use default and hit mockito");
        mock.assert();
        assert_eq!(result.latest, Version::new(666, 0, 0));
    }

    #[test]
    #[serial]
    fn fetch_latest_release_uses_env_shim() {
        // The public `fetch_latest_release` function reads CFGD_GITHUB_API_BASE
        // internally (line 22 of mod.rs). This test exercises that env-var read
        // path through the public API.
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/repos/tj-smith47/cfgd/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"tag_name": "v555.0.0", "assets": []}"#)
            .create();
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let result = fetch_latest_release("tj-smith47/cfgd", None)
            .expect("env shim should redirect to mockito");
        mock.assert();
        assert_eq!(result.version, Version::new(555, 0, 0));
    }
}
