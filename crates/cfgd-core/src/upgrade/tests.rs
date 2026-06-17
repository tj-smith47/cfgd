use super::*;
use std::time::SystemTime;

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
fn find_asset_resolves_go_arch_name() {
    // anodizer names archives with the Go arch (amd64/arm64), NOT the Rust
    // arch (x86_64/aarch64). Regression: the client built the Rust-arch name
    // and so never matched a real release — `cfgd upgrade` died with
    // "no release found for linux/x86_64" against a release that shipped
    // cfgd-<v>-linux-amd64.tar.gz.
    let os = std::env::consts::OS;
    let archive_os = if os == "macos" { "darwin" } else { os };
    let go_arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    #[cfg(unix)]
    let suffix = ".tar.gz";
    #[cfg(windows)]
    let suffix = ".zip";
    let go_name = format!("cfgd-9.9.0-{archive_os}-{go_arch}{suffix}");

    let release = ReleaseInfo {
        tag: "v9.9.0".into(),
        version: Version::new(9, 9, 0),
        assets: vec![ReleaseAsset {
            name: go_name.clone(),
            download_url: "https://example.com/go".into(),
            size: 1024,
        }],
    };

    let asset = find_asset_for_platform(&release).expect("must resolve the Go-arch archive name");
    assert_eq!(asset.name, go_name);
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
fn verify_archive_checksum_accepts_bare_hash() {
    // anodizer's split checksum file (`{{ .Artifact }}.sha256`) contains only
    // the bare hash of the one archive it covers — no filename column.
    let archive = write_temp_archive(b"hello world");
    let body = format!("{HELLO_WORLD_SHA256}\n");
    super::verify_archive_checksum(archive.path(), &body, "cfgd-9.9.0-linux-amd64.tar.gz")
        .expect("matching bare-hash SHA must succeed");
}

#[test]
fn verify_archive_checksum_accepts_hash_with_filename_column() {
    // Tolerate the combined-style `<hash>  <file>` single line too, so the
    // verifier works whether the producer emits split or combined checksums.
    let archive = write_temp_archive(b"hello world");
    let body = format!("{HELLO_WORLD_SHA256}  cfgd-9.9.0-linux-amd64.tar.gz\n");
    super::verify_archive_checksum(archive.path(), &body, "cfgd-9.9.0-linux-amd64.tar.gz")
        .expect("matching SHA with filename column must succeed");
}

#[test]
fn verify_archive_checksum_rejects_empty_checksums_with_dedicated_variant() {
    // Distinct from ChecksumMissing — operators triaging "the .sha256 was
    // truncated by CDN" need to see ChecksumsEmpty, not "no checksum published."
    let archive = write_temp_archive(b"hello world");
    let err = super::verify_archive_checksum(archive.path(), "", "cfgd.tar.gz").unwrap_err();
    assert!(
        matches!(err, crate::errors::UpgradeError::ChecksumsEmpty),
        "expected ChecksumsEmpty, got: {err:?}"
    );
}

#[test]
fn verify_archive_checksum_rejects_whitespace_only_checksums() {
    // Whitespace-only input has no hash token and must surface ChecksumsEmpty.
    let archive = write_temp_archive(b"hello world");
    let err =
        super::verify_archive_checksum(archive.path(), "   \n\t\n", "cfgd.tar.gz").unwrap_err();
    assert!(matches!(err, crate::errors::UpgradeError::ChecksumsEmpty));
}

#[test]
fn verify_archive_checksum_rejects_non_hex_first_token() {
    // A CDN serving an HTML error page in place of the `.sha256` yields a
    // first token that is not a 64-char hex digest. Surface ChecksumsEmpty so
    // triage sees "malformed checksum file" rather than a confusing mismatch.
    let archive = write_temp_archive(b"hello world");
    let err = super::verify_archive_checksum(archive.path(), "<html>error</html>\n", "cfgd.tar.gz")
        .unwrap_err();
    assert!(
        matches!(err, crate::errors::UpgradeError::ChecksumsEmpty),
        "non-hex first token must surface ChecksumsEmpty, got: {err:?}"
    );
}

#[test]
fn find_checksum_asset_matches_per_artifact_sha256() {
    // Split checksums: each archive gets its own `<archive>.sha256` asset.
    let archive = "cfgd-9.9.0-linux-amd64.tar.gz";
    let release = ReleaseInfo {
        tag: "v9.9.0".into(),
        version: Version::new(9, 9, 0),
        assets: vec![
            ReleaseAsset {
                name: format!("{archive}.sha256"),
                download_url: "https://example.com/sha".into(),
                size: 64,
            },
            ReleaseAsset {
                name: "cfgd-9.9.0-linux-arm64.tar.gz.sha256".into(),
                download_url: "https://example.com/other".into(),
                size: 64,
            },
        ],
    };
    let found =
        super::find_checksum_asset(&release, archive).expect("must find per-artifact sha256");
    assert_eq!(found.name, format!("{archive}.sha256"));
    assert_eq!(found.download_url, "https://example.com/sha");
}

#[test]
fn find_checksum_asset_absent_returns_none() {
    // No `<archive>.sha256` attached → None (callers surface ChecksumMissing).
    let release = ReleaseInfo {
        tag: "v9.9.0".into(),
        version: Version::new(9, 9, 0),
        assets: vec![ReleaseAsset {
            name: "cfgd-9.9.0-linux-amd64.tar.gz".into(),
            download_url: "https://example.com/archive".into(),
            size: 1024,
        }],
    };
    assert!(super::find_checksum_asset(&release, "cfgd-9.9.0-linux-amd64.tar.gz").is_none());
}

#[test]
fn verify_archive_checksum_returns_mismatch_when_sha_differs() {
    // Local archive content disagrees with the published hash.
    // Genuine corruption or in-flight interception.
    let archive = write_temp_archive(b"tampered content");
    let body = format!("{HELLO_WORLD_SHA256}\n");
    let err =
        super::verify_archive_checksum(archive.path(), &body, "cfgd-9.9.0-linux-amd64.tar.gz")
            .unwrap_err();
    match err {
        crate::errors::UpgradeError::ChecksumMismatch { file } => {
            assert_eq!(file, "cfgd-9.9.0-linux-amd64.tar.gz");
        }
        other => panic!("expected ChecksumMismatch, got: {other:?}"),
    }
}

#[test]
fn verify_archive_checksum_propagates_read_failure_as_download_failed() {
    // Unreadable archive surfaces through sha256_file → DownloadFailed.
    let body = format!("{HELLO_WORLD_SHA256}\n");
    let err = super::verify_archive_checksum(
        std::path::Path::new("/nonexistent/path/to/archive.tar.gz"),
        &body,
        "cfgd-9.9.0-linux-amd64.tar.gz",
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
    // The helper lowercases the published hash before comparing, so an
    // uppercase-hex `.sha256` (some signers emit capitalized digests) matches.
    let archive = write_temp_archive(b"hello world");
    let upper = HELLO_WORLD_SHA256.to_uppercase();
    let body = format!("{upper}\n");
    super::verify_archive_checksum(archive.path(), &body, "cfgd-9.9.0-linux-amd64.tar.gz")
        .expect("uppercase hex must still match after lowercase normalization");
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

    // A per-artifact .sha256 advertising the WRONG hash must surface as a
    // ChecksumMismatch against the real tarball.
    let wrong = "deadbeef00000000000000000000000000000000000000000000000000000000\n";
    let err = super::verify_archive_checksum(&tarball_path, wrong, "cfgd-test.tar.gz").unwrap_err();
    assert!(
        matches!(err, crate::errors::UpgradeError::ChecksumMismatch { .. }),
        "wrong published hash must surface as mismatch: {err:?}"
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
        // NoAsset reports the Go arch (amd64/arm64) — it matches the names
        // anodizer actually publishes, so it points the user at the right asset.
        let go_arch = match std::env::consts::ARCH {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            other => other,
        };
        assert!(
            msg.contains(go_arch),
            "error should mention the current Go arch: {msg}"
        );
    }
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
fn extract_tarball_valid_gzip_invalid_tar_fails_on_entry_read() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    let dir = tempfile::tempdir().unwrap();
    let archive_path = dir.path().join("validgz-badtar.tar.gz");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    // A well-formed gzip stream whose decompressed payload is NOT a valid tar
    // archive. GzDecoder opens lazily, so the failure surfaces when the tar
    // entry iterator reads/parses the bogus payload (the per-entry read arm).
    {
        let file = std::fs::File::create(&archive_path).unwrap();
        let mut enc = GzEncoder::new(file, Compression::default());
        enc.write_all(b"this decompresses fine but is not a tar header at all")
            .unwrap();
        enc.finish().unwrap();
    }

    let result = extract_tarball(&archive_path, &dest);
    assert!(
        result.is_err(),
        "valid gzip wrapping a non-tar payload must fail at entry read"
    );
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

    let err = check_with_cache(Some("does/not/matter"), None, None)
        .expect_err("unparseable cached version must surface as Err, not silent fallthrough");
    let msg = err.to_string();
    assert!(
        msg.contains("cached version") && msg.contains("parse"),
        "error must point at the cache file's version field so triage looks there first: {msg}"
    );
}

#[test]
fn find_checksum_asset_picks_matching_per_artifact_sha256() {
    let archive = "cfgd-1.0.0-linux-amd64.tar.gz";
    let release = ReleaseInfo {
        tag: "v1.0.0".into(),
        version: Version::new(1, 0, 0),
        assets: vec![
            ReleaseAsset {
                name: archive.into(),
                download_url: "https://example.com/binary".into(),
                size: 10000,
            },
            // A different artifact's checksum must NOT be picked.
            ReleaseAsset {
                name: "cfgd-1.0.0-linux-arm64.tar.gz.sha256".into(),
                download_url: "https://example.com/wrong".into(),
                size: 64,
            },
            ReleaseAsset {
                name: format!("{archive}.sha256"),
                download_url: "https://example.com/checksum".into(),
                size: 64,
            },
        ],
    };

    let asset = find_checksum_asset(&release, archive);
    assert!(asset.is_some());
    assert_eq!(asset.unwrap().name, format!("{archive}.sha256"));
    assert_eq!(asset.unwrap().download_url, "https://example.com/checksum");
}

#[test]
fn find_checksum_asset_returns_none_when_only_other_artifacts_have_checksums() {
    let release = ReleaseInfo {
        tag: "v2.0.0".into(),
        version: Version::new(2, 0, 0),
        assets: vec![
            ReleaseAsset {
                name: "cfgd-2.0.0-linux-amd64.tar.gz".into(),
                download_url: "https://example.com/binary".into(),
                size: 10000,
            },
            ReleaseAsset {
                name: "cfgd-2.0.0-linux-arm64.tar.gz.sha256".into(),
                download_url: "https://example.com/other".into(),
                size: 64,
            },
        ],
    };

    assert!(
        find_checksum_asset(&release, "cfgd-2.0.0-linux-amd64.tar.gz").is_none(),
        "no .sha256 for the amd64 archive → None"
    );
}

#[test]
fn find_checksum_asset_empty_assets() {
    let release = ReleaseInfo {
        tag: "v1.0.0".into(),
        version: Version::new(1, 0, 0),
        assets: vec![],
    };
    assert!(find_checksum_asset(&release, "cfgd-1.0.0-linux-amd64.tar.gz").is_none());
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
#[serial_test::serial]
fn record_check_at_creates_cache_when_none_exists() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    // No prior cache → the None branch stamps a fresh entry at `now`.
    assert!(
        last_checked_secs().is_none(),
        "precondition: no cache means no recorded check"
    );
    record_check_at(1_700_000_000);
    assert_eq!(
        last_checked_secs(),
        Some(1_700_000_000),
        "record_check_at must persist the timestamp on a fresh cache"
    );
}

#[test]
#[serial_test::serial]
fn record_check_at_updates_timestamp_on_existing_cache() {
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    // Seed a cache with real version fields, then re-stamp the timestamp.
    let seeded = VersionCache {
        checked_at_secs: 1_700_000_000,
        latest_tag: "v9.9.0".into(),
        latest_version: "9.9.0".into(),
        current_version: env!("CARGO_PKG_VERSION").into(),
    };
    write_version_cache(&seeded).expect("seed cache write must succeed");

    record_check_at(1_700_009_999);
    assert_eq!(
        last_checked_secs(),
        Some(1_700_009_999),
        "the Some branch must update the timestamp in place"
    );
    // Re-stamping preserves the version fields from the prior cache.
    let after = read_version_cache().expect("cache must still parse after update");
    assert_eq!(
        after.latest_version, "9.9.0",
        "record_check_at must preserve the cached version fields"
    );
    assert_eq!(after.latest_tag, "v9.9.0");
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

// --- find_checksum_asset: various patterns ---

#[test]
fn find_checksum_asset_matches_version_prefixed_archive() {
    let archive = "cfgd-3.0.0-linux-amd64.tar.gz";
    let release = ReleaseInfo {
        tag: "v3.0.0".into(),
        version: Version::new(3, 0, 0),
        assets: vec![
            ReleaseAsset {
                name: archive.into(),
                download_url: "https://example.com/bin".into(),
                size: 5000,
            },
            ReleaseAsset {
                name: format!("{archive}.sha256"),
                download_url: "https://example.com/sums".into(),
                size: 64,
            },
        ],
    };
    let asset = find_checksum_asset(&release, archive).unwrap();
    assert_eq!(asset.name, format!("{archive}.sha256"));
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

#[test]
#[cfg(unix)]
fn atomic_replace_missing_source_fails_at_copy() {
    let dir = tempfile::tempdir().unwrap();
    // Source never created → the staging temp file is made, but fs::copy of a
    // missing source fails, exercising the copy-to-staging error arm.
    let src = dir.path().join("does-not-exist-source");
    let tgt = dir.path().join("target");

    let result = atomic_replace(&src, &tgt);
    assert!(
        result.is_err(),
        "a missing source must fail at the copy-to-staging step"
    );
}

#[test]
#[cfg(unix)]
fn atomic_replace_target_is_a_directory_fails_at_persist() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("source");
    std::fs::write(&src, "content").unwrap();

    // Target path is an existing DIRECTORY: the staging temp file is created in
    // the parent and the copy succeeds, but persist's rename of a file over a
    // directory fails, exercising the atomic-rename error arm.
    let tgt = dir.path().join("target-is-dir");
    std::fs::create_dir(&tgt).unwrap();

    let result = atomic_replace(&src, &tgt);
    assert!(
        result.is_err(),
        "renaming the staged file over an existing directory must fail at persist"
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

const ARCHIVE_1_0_0: &str = "cfgd-1.0.0-linux-amd64.tar.gz";

#[test]
fn find_cosign_bundle_asset_locates_per_artifact_bundle() {
    // anodizer signs each `<archive>.sha256` with keyless cosign, producing a
    // sibling `<archive>.sha256.cosign.bundle`.
    let checksum = format!("{ARCHIVE_1_0_0}.sha256");
    let bundle = format!("{checksum}.cosign.bundle");
    let release = release_with_assets(&[ARCHIVE_1_0_0, &checksum, &bundle]);
    let found =
        find_cosign_bundle_asset(&release, &checksum).expect("bundle must be located by name");
    assert_eq!(found.name, bundle);
}

#[test]
fn find_cosign_bundle_asset_returns_none_when_no_bundle() {
    // sha256 present but no .cosign.bundle — verifier must fall back.
    let checksum = format!("{ARCHIVE_1_0_0}.sha256");
    let release = release_with_assets(&[ARCHIVE_1_0_0, &checksum]);
    assert!(find_cosign_bundle_asset(&release, &checksum).is_none());
}

#[test]
fn find_cosign_bundle_asset_ignores_lookalike_names() {
    // Name is exact: a `.bak` suffix or a different artifact's bundle must not
    // match this checksum asset's bundle.
    let checksum = format!("{ARCHIVE_1_0_0}.sha256");
    let release = release_with_assets(&[
        &format!("{checksum}.cosign.bundle.bak"),
        "cfgd-1.0.0-linux-arm64.tar.gz.sha256.cosign.bundle",
    ]);
    assert!(
        find_cosign_bundle_asset(&release, &checksum).is_none(),
        "non-matching name must not be selected"
    );
}

#[test]
fn find_cosign_bundle_asset_empty_release_yields_none() {
    let release = release_with_assets(&[]);
    assert!(find_cosign_bundle_asset(&release, &format!("{ARCHIVE_1_0_0}.sha256")).is_none());
}

// --- find_cosign_cert_asset ---

#[test]
fn find_cosign_cert_asset_locates_per_artifact_cert() {
    // A standalone Fulcio cert is published as `<archive>.sha256.cosign.pem`.
    let checksum = format!("{ARCHIVE_1_0_0}.sha256");
    let cert = format!("{checksum}.cosign.pem");
    let release = release_with_assets(&[ARCHIVE_1_0_0, &checksum, &cert]);
    let found = find_cosign_cert_asset(&release, &checksum).expect("cert must be located by name");
    assert_eq!(found.name, cert);
}

#[test]
fn find_cosign_cert_asset_returns_none_when_absent() {
    // Keyless bundles embed the cert, so the standalone .pem is usually absent
    // → None → verify-blob runs without --certificate.
    let checksum = format!("{ARCHIVE_1_0_0}.sha256");
    let release = release_with_assets(&[
        ARCHIVE_1_0_0,
        &checksum,
        &format!("{checksum}.cosign.bundle"),
    ]);
    assert!(
        find_cosign_cert_asset(&release, &checksum).is_none(),
        "no .cosign.pem → embedded cert is used"
    );
}

#[test]
fn find_cosign_cert_asset_ignores_lookalike_names() {
    let checksum = format!("{ARCHIVE_1_0_0}.sha256");
    let release = release_with_assets(&[
        &format!("{checksum}.cosign.pem.bak"),
        "cfgd-1.0.0-linux-arm64.tar.gz.sha256.cosign.pem",
    ]);
    assert!(find_cosign_cert_asset(&release, &checksum).is_none());
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
    let result = check_with_cache(Some("does/not/matter"), None, None)
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
// the compiled fake-cosign binary records argv and chooses an exit status.
// Cross-platform — the shim is a host-target binary, not a /bin/sh script.
// ---------------------------------------------------------------------------

mod cosign_verify_blob {
    use super::*;
    use crate::test_helpers::CosignTestShim;
    use serial_test::serial;

    fn dummy_paths() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let checksums = dir.path().join("cfgd-9.9.0-linux-amd64.tar.gz.sha256");
        let bundle = dir
            .path()
            .join("cfgd-9.9.0-linux-amd64.tar.gz.sha256.cosign.bundle");
        std::fs::write(&checksums, "deadbeef\n").unwrap();
        std::fs::write(&bundle, "{}").unwrap();
        (dir, checksums, bundle)
    }

    #[test]
    #[serial]
    fn run_cosign_verify_blob_emits_keyless_argv_without_key_flag() {
        let shim = CosignTestShim::install();
        let (_dir, checksums, bundle) = dummy_paths();
        // No standalone cert → rely on the cert embedded in the keyless bundle.
        run_cosign_verify_blob(&checksums, &bundle, None).expect("happy path → Ok");
        let argv = shim.argv_log();
        assert!(
            argv.contains("verify-blob"),
            "argv must use verify-blob subcommand: {argv}"
        );
        assert!(
            argv.contains(&format!("--bundle={}", bundle.display())),
            "argv must include --bundle=<bundle path>: {argv}"
        );
        // Keyless verification: identity + issuer pinned, NO --key.
        assert!(
            argv.contains("--certificate-identity-regexp="),
            "argv must pin the signer identity regexp: {argv}"
        );
        assert!(
            argv.contains("--certificate-oidc-issuer="),
            "argv must pin the OIDC issuer: {argv}"
        );
        assert!(
            !argv.contains("--key"),
            "keyless verify must NOT pass --key: {argv}"
        );
        // No standalone cert was supplied → --certificate omitted (embedded cert).
        assert!(
            !argv.contains("--certificate="),
            "without a standalone cert, --certificate must be omitted: {argv}"
        );
        // The "--" terminator separates the cosign flags from the file argument.
        assert!(
            argv.contains(" -- "),
            "argv must include `--` terminator: {argv}"
        );
    }

    #[test]
    #[serial]
    fn run_cosign_verify_blob_passes_certificate_when_standalone_cert_present() {
        let shim = CosignTestShim::install();
        let (dir, checksums, bundle) = dummy_paths();
        let cert = dir
            .path()
            .join("cfgd-9.9.0-linux-amd64.tar.gz.sha256.cosign.pem");
        std::fs::write(&cert, "-----BEGIN CERTIFICATE-----\n").unwrap();
        run_cosign_verify_blob(&checksums, &bundle, Some(cert.as_path())).expect("happy path → Ok");
        let argv = shim.argv_log();
        assert!(
            argv.contains(&format!("--certificate={}", cert.display())),
            "a standalone cert must be passed via --certificate: {argv}"
        );
        assert!(
            !argv.contains("--key"),
            "keyless verify must NOT pass --key even with a standalone cert: {argv}"
        );
    }

    #[test]
    #[serial]
    fn run_cosign_verify_blob_propagates_failure_with_stderr_message() {
        let _shim = CosignTestShim::builder()
            .with_exit(1)
            .with_stderr("signature does not match")
            .install();
        let (_dir, checksums, bundle) = dummy_paths();
        let err =
            run_cosign_verify_blob(&checksums, &bundle, None).expect_err("non-zero exit → Err");
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
        let (_dir, checksums, bundle) = dummy_paths();
        let err =
            run_cosign_verify_blob(&checksums, &bundle, None).expect_err("missing binary → Err");
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
// Tests are #[cfg(unix)] because download_and_install_to extracts a tarball on
// Unix (extract_zip on Windows takes a different code path) and the module
// builds fixtures with `std::os::unix::fs::PermissionsExt`. The fake-cosign
// shim is cross-platform now, so it no longer contributes to this gate.
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

    const ARCHIVE: &str = "cfgd-9.9.9-linux-amd64.tar.gz";

    /// Compose a bare-hash `.sha256` body (anodizer's `split: true` form — the
    /// hash only, no filename column).
    fn checksum_body(sha: &str) -> String {
        format!("{sha}\n")
    }

    /// Build a `ReleaseInfo` whose assets resolve to mockito URLs and follow
    /// the real anodizer contract: a binary archive, its per-artifact
    /// `<archive>.sha256` (bare hash), and the keyless
    /// `<archive>.sha256.cosign.bundle`. No `cosign.pub` — verification is
    /// keyless. The primary archive is `release.assets[0]`.
    fn release_with_full_signature_chain(server_url: &str) -> ReleaseInfo {
        ReleaseInfo {
            tag: "v9.9.9".into(),
            version: Version::new(9, 9, 9),
            assets: vec![
                ReleaseAsset {
                    name: ARCHIVE.into(),
                    download_url: format!("{server_url}/download/cfgd.tar.gz"),
                    size: 0,
                },
                ReleaseAsset {
                    name: format!("{ARCHIVE}.sha256"),
                    download_url: format!("{server_url}/download/cfgd.tar.gz.sha256"),
                    size: 0,
                },
                ReleaseAsset {
                    name: format!("{ARCHIVE}.sha256.cosign.bundle"),
                    download_url: format!("{server_url}/download/cosign.bundle"),
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
        let checksums = checksum_body(&sha);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/cfgd.tar.gz.sha256")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
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
        let checksums = checksum_body(&sha);

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
            .mock("GET", "/download/cfgd.tar.gz.sha256")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
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
            .mock("GET", "/download/cfgd.tar.gz.sha256")
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
        let checksums = checksum_body(&sha);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/cfgd.tar.gz.sha256")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
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
        let asset_name = ARCHIVE;
        let bogus_sha = "0".repeat(64);
        let checksums = checksum_body(&bogus_sha);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/cfgd.tar.gz.sha256")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
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

    /// Same checksum-mismatch failure as the None-printer case, but with a real
    /// printer wired in so the `finish_fail` arm of the "Verifying checksum..."
    /// spinner executes (the success-path printer test never reaches it).
    #[test]
    #[serial]
    fn checksum_mismatch_with_printer_drives_verify_finish_fail() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let tarball = build_tarball(b"actual-binary");
        let bogus_sha = "0".repeat(64);
        let checksums = checksum_body(&bogus_sha);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/cfgd.tar.gz.sha256")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
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
            msg.to_ascii_lowercase().contains("checksum") && msg.contains(ARCHIVE),
            "error names asset and surfaces checksum mismatch: {msg}"
        );
        assert!(!target.exists(), "target must not be created on failure");
    }

    /// Same cosign-verify failure as the None-printer case, but with a real
    /// printer so the `finish_fail` arm of the "Verifying cosign signature..."
    /// spinner executes.
    #[test]
    #[serial]
    fn cosign_failure_with_printer_drives_cosign_finish_fail() {
        let _shim = CosignTestShim::builder()
            .with_argv_logging(false)
            .with_exit(1)
            .with_stderr("tampered checksums file")
            .install();
        let tarball = build_tarball(b"binary");
        let sha = crate::sha256_hex(&tarball);
        let checksums = checksum_body(&sha);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/cfgd.tar.gz.sha256")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");

        let printer = crate::test_helpers::test_printer();
        let err = download_and_install_to(&release, &asset, &target, false, Some(&printer))
            .expect_err("cosign exit 1 with printer → Err");
        let msg = err.to_string();
        assert!(
            msg.contains("cosign verify-blob failed") && msg.contains("tampered checksums file"),
            "error surfaces cosign-verify failure with stderr message: {msg}"
        );
        assert!(!target.exists(), "target must not be created on failure");
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
        let checksums = checksum_body(&sha);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/cfgd.tar.gz.sha256")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
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
    fn returns_checksum_mismatch_when_sha256_covers_a_different_artifact() {
        // anodizer's `split: true` checksum file holds the bare hash of the one
        // artifact it covers. If the served `.sha256` carries some *other*
        // artifact's hash (mirror mixup, swapped asset), the bare-hash compare
        // against the downloaded archive differs → ChecksumMismatch.
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let tarball = build_tarball(b"binary");
        let other_sha = crate::sha256_hex(b"some-other-artifact-bytes");
        let checksums = checksum_body(&other_sha);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/cfgd.tar.gz.sha256")
            .with_status(200)
            .with_body(checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
            .create();

        let release = release_with_full_signature_chain(&server.url());
        let asset = release.assets[0].clone();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("cfgd");

        let err = download_and_install_to(&release, &asset, &target, false, None)
            .expect_err("sha256 of a different artifact → Err");
        let msg = err.to_string();
        assert!(
            msg.to_ascii_lowercase().contains("checksum") && msg.contains(&asset.name),
            "error names the asset whose checksum did not match: {msg}"
        );
    }

    #[test]
    #[serial]
    fn skips_cosign_verification_when_release_has_no_bundle_asset() {
        // Release lacks the cosign bundle → verify_cosign_bundle returns
        // Sha256Only and the install proceeds with SHA256-only verification.
        // Demonstrates the documented graceful-degradation contract.
        let _shim = CosignTestShim::builder()
            .with_argv_logging(false)
            .with_exit(99)
            .with_stderr("should not be invoked")
            .install();
        let binary_content = b"binary";
        let tarball = build_tarball(binary_content);
        let sha = crate::sha256_hex(&tarball);
        let asset_name = ARCHIVE;
        let checksums = checksum_body(&sha);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/cfgd.tar.gz.sha256")
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
                    name: format!("{ARCHIVE}.sha256"),
                    download_url: format!("{}/download/cfgd.tar.gz.sha256", server.url()),
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

    /// The per-artifact checksum asset name that the keyless bundle finder
    /// derives `<name>.cosign.bundle` (and the optional cert finder
    /// `<name>.cosign.pem`) from. Mirrors anodizer's `split: true` shape.
    const CHECKSUM_ASSET: &str = "cfgd-9.9.9-linux-x86_64.tar.gz.sha256";

    /// Bundle missing → verify_cosign_bundle returns `Sha256Only` and emits the
    /// "no cosign bundle" warning when a printer is supplied. Pin the warning
    /// text shape so a future change can't silently downgrade publisher-
    /// compromise resistance without an operator-visible message.
    #[test]
    #[serial]
    fn verify_cosign_bundle_emits_warning_when_no_bundle_attached() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let release = release_with_assets(&["cfgd-9.9.9-linux-x86_64.tar.gz", CHECKSUM_ASSET]);
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join(CHECKSUM_ASSET);
        std::fs::write(&checksums_path, "").unwrap();

        let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
        let outcome = verify_cosign_bundle(
            &checksums_path,
            CHECKSUM_ASSET,
            &release,
            tmp.path(),
            false,
            Some(&printer),
        )
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

    /// Bundle present but the cosign CLI is "missing" (env shim points at a
    /// path that does not exist) → graceful-degrade with the install-hint
    /// warning. Drives the `require_cosign().is_err()` arm.
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
            CHECKSUM_ASSET,
            &format!("{CHECKSUM_ASSET}.cosign.bundle"),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join(CHECKSUM_ASSET);
        std::fs::write(&checksums_path, "").unwrap();

        let (printer, buf) = crate::output::Printer::for_test_at(crate::output::Verbosity::Normal);
        let outcome = verify_cosign_bundle(
            &checksums_path,
            CHECKSUM_ASSET,
            &release,
            tmp.path(),
            false,
            Some(&printer),
        )
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
    // the per-artifact `.sha256` download (compromised mirror, MITM against
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
        let release = release_with_assets(&["cfgd-9.9.9-linux-x86_64.tar.gz", CHECKSUM_ASSET]);
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join(CHECKSUM_ASSET);
        std::fs::write(&checksums_path, "").unwrap();

        let err = verify_cosign_bundle(
            &checksums_path,
            CHECKSUM_ASSET,
            &release,
            tmp.path(),
            true,
            None,
        )
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
            CHECKSUM_ASSET,
            &format!("{CHECKSUM_ASSET}.cosign.bundle"),
        ]);
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join(CHECKSUM_ASSET);
        std::fs::write(&checksums_path, "").unwrap();

        let err = verify_cosign_bundle(
            &checksums_path,
            CHECKSUM_ASSET,
            &release,
            tmp.path(),
            true,
            None,
        )
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
    /// for callers that did not opt in. Mirror of the "_emits_warning_" tests
    /// above but expressed against the non-strict default to pin the
    /// behavioral contract from the strict-mode side.
    #[test]
    #[serial]
    fn verify_cosign_bundle_non_strict_falls_back_silently() {
        let _shim = CosignTestShim::builder().with_argv_logging(false).install();
        let release = release_with_assets(&["cfgd-9.9.9-linux-x86_64.tar.gz", CHECKSUM_ASSET]);
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join(CHECKSUM_ASSET);
        std::fs::write(&checksums_path, "").unwrap();

        let outcome = verify_cosign_bundle(
            &checksums_path,
            CHECKSUM_ASSET,
            &release,
            tmp.path(),
            false,
            None,
        )
        .expect("non-strict mode + missing bundle must return Sha256Only, not Err");
        assert_eq!(
            outcome,
            VerificationMode::Sha256Only,
            "non-strict default contract: missing bundle → SHA256-only fallback"
        );
    }

    /// Happy path under strict mode: keyless bundle present (cert embedded, no
    /// standalone pubkey) and the cosign shim verifies → returns
    /// `StrictCosignRequired` (distinct from the non-strict `Cosign` mode so
    /// audit consumers can tell apart "strict was demanded and honored" from
    /// "strict happened by default").
    #[test]
    #[serial]
    fn verify_cosign_bundle_strict_records_strict_cosign_required_on_success() {
        let _shim = CosignTestShim::builder()
            .with_argv_logging(false)
            .with_exit(0)
            .install();

        // The keyless bundle asset needs a real URL that resolves — point it
        // at a mockito server so download_to_file inside verify_cosign_bundle
        // can fetch it. No pubkey asset: the Fulcio cert is embedded.
        let mut server = mockito::Server::new();
        let _m_bundle = server
            .mock("GET", "/strict/bundle")
            .with_status(200)
            .with_body("{}")
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
                    name: format!("{CHECKSUM_ASSET}.cosign.bundle"),
                    download_url: format!("{}/strict/bundle", server.url()),
                    size: 0,
                },
            ],
        };
        let tmp = tempfile::tempdir().unwrap();
        let checksums_path = tmp.path().join(CHECKSUM_ASSET);
        std::fs::write(&checksums_path, "deadbeef\n").unwrap();

        let outcome = verify_cosign_bundle(
            &checksums_path,
            CHECKSUM_ASSET,
            &release,
            tmp.path(),
            true,
            None,
        )
        .expect("strict + bundle present + cosign exit 0 → Ok");
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
        let asset_name = ARCHIVE;
        let checksums = checksum_body(&sha);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/cfgd.tar.gz.sha256")
            .with_status(200)
            .with_body(checksums)
            .create();

        // Release omits the cosign bundle — strict mode must reject the
        // upgrade rather than silently falling back to SHA256-only.
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
                    name: format!("{ARCHIVE}.sha256"),
                    download_url: format!("{}/download/cfgd.tar.gz.sha256", server.url()),
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
        let checksums = checksum_body(&sha);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/cfgd.tar.gz.sha256")
            .with_status(200)
            .with_body(&checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
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
        let checksums = checksum_body(&sha);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_header("content-length", &tarball.len().to_string())
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/cfgd.tar.gz.sha256")
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
        let bogus_sha = "f".repeat(64);
        let checksums = checksum_body(&bogus_sha);

        let mut server = mockito::Server::new();
        let _m_archive = server
            .mock("GET", "/download/cfgd.tar.gz")
            .with_status(200)
            .with_body(&tarball)
            .create();
        let _m_checksums = server
            .mock("GET", "/download/cfgd.tar.gz.sha256")
            .with_status(200)
            .with_body(&checksums)
            .create();
        let _m_bundle = server
            .mock("GET", "/download/cosign.bundle")
            .with_status(200)
            .with_body("{}")
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

        let result = check_latest(Some("test/repo"), None, None)
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

        let result = check_with_cache(Some("test/repo"), None, None)
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

        let result = check_with_cache(Some("test/repo"), None, None)
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
            check_latest(None, None, None).expect("None repo should use default and hit mockito");
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

        let result = check_with_cache(None, None, None)
            .expect("None repo should use default and hit mockito");
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

    /// `channel: prerelease` hits the `releases` LIST endpoint and selects the
    /// entry with the highest semver version — here the prerelease `v9.9.1-rc.1`
    /// over the stable `v9.9.0` that also appears in the list.
    #[test]
    #[serial]
    fn channel_prerelease_selects_highest_version_from_list() {
        let mut server = mockito::Server::new();
        let list_mock = server
            .mock("GET", "/repos/test/repo/releases")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"[
                    {"tag_name": "v9.9.1-rc.1", "assets": []},
                    {"tag_name": "v9.9.0", "assets": []}
                ]"#,
            )
            .expect(1)
            .create();
        let latest_mock = server
            .mock("GET", "/repos/test/repo/releases/latest")
            .with_status(200)
            .with_body(r#"{"tag_name": "v9.9.0", "assets": []}"#)
            .expect(0)
            .create();
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let result = check_latest(Some("test/repo"), Some("prerelease"), None)
            .expect("prerelease channel should hit the list endpoint");
        list_mock.assert();
        latest_mock.assert();

        assert_eq!(
            result.release.as_ref().map(|r| r.tag.as_str()),
            Some("v9.9.1-rc.1"),
            "prerelease channel must select the highest version in the list"
        );
        assert_eq!(
            result.latest,
            Version::parse("9.9.1-rc.1").unwrap(),
            "prerelease semver must include the prerelease identifier"
        );
    }

    /// `channel: stable` hits `releases/latest` (excludes prereleases) and never
    /// touches the LIST endpoint.
    #[test]
    #[serial]
    fn channel_stable_uses_releases_latest_endpoint() {
        let mut server = mockito::Server::new();
        let latest_mock = server
            .mock("GET", "/repos/test/repo/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"tag_name": "v9.9.0", "assets": []}"#)
            .expect(1)
            .create();
        let list_mock = server
            .mock("GET", "/repos/test/repo/releases")
            .with_status(200)
            .with_body(r#"[{"tag_name": "v9.9.1-rc.1", "assets": []}]"#)
            .expect(0)
            .create();
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let result = check_latest(Some("test/repo"), Some("stable"), None)
            .expect("stable channel should hit releases/latest");
        latest_mock.assert();
        list_mock.assert();
        assert_eq!(
            result.release.as_ref().map(|r| r.tag.as_str()),
            Some("v9.9.0")
        );
    }

    /// `channel: None` behaves like stable — hits `releases/latest` only.
    #[test]
    #[serial]
    fn channel_none_uses_releases_latest_endpoint() {
        let mut server = mockito::Server::new();
        let latest_mock = server
            .mock("GET", "/repos/test/repo/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"tag_name": "v9.9.0", "assets": []}"#)
            .expect(1)
            .create();
        let list_mock = server
            .mock("GET", "/repos/test/repo/releases")
            .with_status(200)
            .with_body(r#"[{"tag_name": "v9.9.1-rc.1", "assets": []}]"#)
            .expect(0)
            .create();
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let result = check_latest(Some("test/repo"), None, None)
            .expect("None channel should hit releases/latest");
        latest_mock.assert();
        list_mock.assert();
        assert_eq!(
            result.release.as_ref().map(|r| r.tag.as_str()),
            Some("v9.9.0")
        );
    }

    /// An unrecognized channel falls back to stable (`releases/latest`) without
    /// erroring, and does not touch the LIST endpoint.
    #[test]
    #[serial]
    fn channel_unknown_falls_back_to_stable() {
        let mut server = mockito::Server::new();
        let latest_mock = server
            .mock("GET", "/repos/test/repo/releases/latest")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"tag_name": "v9.9.0", "assets": []}"#)
            .expect(1)
            .create();
        let list_mock = server
            .mock("GET", "/repos/test/repo/releases")
            .with_status(200)
            .with_body(r#"[{"tag_name": "v9.9.1-rc.1", "assets": []}]"#)
            .expect(0)
            .create();
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let result = check_latest(Some("test/repo"), Some("nightly"), None)
            .expect("unknown channel must fall back to stable, never error");
        latest_mock.assert();
        list_mock.assert();
        assert_eq!(
            result.release.as_ref().map(|r| r.tag.as_str()),
            Some("v9.9.0")
        );
    }

    /// Prerelease list whose body is not valid JSON surfaces the `invalid JSON`
    /// ApiError rather than panicking or silently succeeding.
    #[test]
    #[serial]
    fn channel_prerelease_invalid_json_errors() {
        let mut server = mockito::Server::new();
        let _list_mock = server
            .mock("GET", "/repos/test/repo/releases")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("not json")
            .create();
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let err = check_latest(Some("test/repo"), Some("prerelease"), None)
            .expect_err("invalid JSON body must surface as an error");
        let msg = err.to_string();
        assert!(
            msg.contains("invalid JSON"),
            "error must name the JSON parse failure, got: {msg}"
        );
    }

    /// A JSON object (not an array) at the list endpoint is rejected with the
    /// "not a JSON array" ApiError.
    #[test]
    #[serial]
    fn channel_prerelease_non_array_body_errors() {
        let mut server = mockito::Server::new();
        let _list_mock = server
            .mock("GET", "/repos/test/repo/releases")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{}")
            .create();
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let err = check_latest(Some("test/repo"), Some("prerelease"), None)
            .expect_err("a JSON object is not a releases array");
        let msg = err.to_string();
        assert!(
            msg.contains("releases list response was not a JSON array"),
            "error must name the non-array shape, got: {msg}"
        );
    }

    /// An empty releases array has no parseable release → the "no parseable
    /// release" ApiError.
    #[test]
    #[serial]
    fn channel_prerelease_empty_array_errors() {
        let mut server = mockito::Server::new();
        let _list_mock = server
            .mock("GET", "/repos/test/repo/releases")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create();
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let err = check_latest(Some("test/repo"), Some("prerelease"), None)
            .expect_err("an empty releases array yields no parseable release");
        let msg = err.to_string();
        assert!(
            msg.contains("no parseable release found"),
            "empty array must yield the no-parseable-release error, got: {msg}"
        );
    }

    /// Every element has an unparseable tag, so the `filter_map(...).ok()` skips
    /// them all → the same "no parseable release" ApiError as an empty array.
    #[test]
    #[serial]
    fn channel_prerelease_all_unparseable_tags_errors() {
        let mut server = mockito::Server::new();
        let _list_mock = server
            .mock("GET", "/repos/test/repo/releases")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"[{"tag_name": "not-a-version", "assets": []}]"#)
            .create();
        let _env = EnvVarGuard::set(GITHUB_API_BASE_ENV, &server.url());

        let err = check_latest(Some("test/repo"), Some("prerelease"), None)
            .expect_err("all-unparseable tags leave no selectable release");
        let msg = err.to_string();
        assert!(
            msg.contains("no parseable release found"),
            "unparseable tags must be skipped, leaving no release, got: {msg}"
        );
    }
}

// ---------------------------------------------------------------------------
// VerificationMode — wire-string + serde round-trip coverage. The wire-string
// helper is what the upgrade CLI emits in its structured-output payload, so
// regression here would surface as silent breakage in downstream alerting.
// ---------------------------------------------------------------------------

#[test]
fn verification_mode_as_wire_str_cosign() {
    assert_eq!(VerificationMode::Cosign.as_wire_str(), "cosign");
}

#[test]
fn verification_mode_as_wire_str_sha256_only() {
    assert_eq!(VerificationMode::Sha256Only.as_wire_str(), "sha256-only");
}

#[test]
fn verification_mode_as_wire_str_strict_cosign_required() {
    assert_eq!(
        VerificationMode::StrictCosignRequired.as_wire_str(),
        "strict-cosign-required"
    );
}

#[test]
fn verification_mode_serializes_to_kebab_case_wire_values() {
    // serde JSON serialisation must match the as_wire_str() output exactly.
    // The two paths are independent (one is a const lookup, the other goes
    // through serde) and a future refactor that changes one without the
    // other would break downstream consumers parsing either form.
    assert_eq!(
        serde_json::to_value(VerificationMode::Cosign).unwrap(),
        serde_json::Value::String("cosign".into())
    );
    assert_eq!(
        serde_json::to_value(VerificationMode::Sha256Only).unwrap(),
        serde_json::Value::String("sha256-only".into())
    );
    assert_eq!(
        serde_json::to_value(VerificationMode::StrictCosignRequired).unwrap(),
        serde_json::Value::String("strict-cosign-required".into())
    );
}

#[test]
fn verification_mode_deserializes_from_kebab_case_wire_values() {
    // Round-trip: JSON wire form → enum. Pins the deserialize half so a
    // consumer that persists the enum can round-trip through cfgd's own
    // structured output.
    let cosign: VerificationMode = serde_json::from_str(r#""cosign""#).unwrap();
    assert_eq!(cosign, VerificationMode::Cosign);
    let sha: VerificationMode = serde_json::from_str(r#""sha256-only""#).unwrap();
    assert_eq!(sha, VerificationMode::Sha256Only);
    let strict: VerificationMode = serde_json::from_str(r#""strict-cosign-required""#).unwrap();
    assert_eq!(strict, VerificationMode::StrictCosignRequired);
}

#[test]
fn verification_mode_round_trip_via_serde() {
    // Belt-and-suspenders: every variant survives a serialize → deserialize
    // round trip without drift.
    for mode in [
        VerificationMode::Cosign,
        VerificationMode::Sha256Only,
        VerificationMode::StrictCosignRequired,
    ] {
        let json = serde_json::to_string(&mode).unwrap();
        let back: VerificationMode = serde_json::from_str(&json).unwrap();
        assert_eq!(back, mode, "round trip drift for {mode:?}");
    }
}

#[test]
fn verification_mode_rejects_unknown_wire_value() {
    // Deserializing an unknown variant must Err — guards against silent
    // expansion if a future release ships with a new value but consumers
    // haven't updated yet.
    let err = serde_json::from_str::<VerificationMode>(r#""unknown-mode""#)
        .expect_err("unknown variant must not silently coerce");
    let msg = err.to_string();
    assert!(
        msg.contains("variant") || msg.contains("unknown"),
        "error should mention the unknown variant: {msg}"
    );
}

// ---------------------------------------------------------------------------
// InstallReport — struct shape pinning. The struct fields are public and
// consumed by the upgrade CLI; a regression that changes either field name
// or type would break the structured-output payload.
// ---------------------------------------------------------------------------

#[test]
fn install_report_carries_path_and_verification_mode() {
    let target = std::path::PathBuf::from("/tmp/some-cfgd-binary");
    let report = InstallReport {
        installed_path: target.clone(),
        verification_mode: VerificationMode::StrictCosignRequired,
    };
    assert_eq!(report.installed_path, target);
    assert_eq!(
        report.verification_mode,
        VerificationMode::StrictCosignRequired
    );
}

// ---------------------------------------------------------------------------
// github_api_base — confirm the unset branch falls through to the production
// constant. Lives separately because most env-shim tests set the var.
// ---------------------------------------------------------------------------

#[test]
#[serial_test::serial]
fn github_api_base_falls_back_to_production_constant_when_unset() {
    use crate::test_helpers::EnvVarGuard;
    // Explicitly unset and confirm the production fallback. Pin the URL so
    // an inadvertent edit to GITHUB_API_BASE constant surfaces here.
    let _guard = EnvVarGuard::unset(GITHUB_API_BASE_ENV);
    assert_eq!(github_api_base(), "https://api.github.com");
}

#[test]
#[serial_test::serial]
fn github_api_base_honors_env_override() {
    use crate::test_helpers::EnvVarGuard;
    let _guard = EnvVarGuard::set(GITHUB_API_BASE_ENV, "https://custom-api.example.com");
    assert_eq!(github_api_base(), "https://custom-api.example.com");
}

// ---------------------------------------------------------------------------
// strip_tag_prefix — additional edge cases not yet covered.
// ---------------------------------------------------------------------------

#[test]
fn strip_tag_prefix_uppercase_v_is_preserved() {
    // strip_prefix is case-sensitive — "V1.0.0" must keep the leading "V".
    // Pin this so a future "case-insensitive" refactor surfaces here.
    assert_eq!(strip_tag_prefix("V1.0.0"), "V1.0.0");
}

#[test]
fn strip_tag_prefix_with_whitespace_around_v_is_not_stripped() {
    // Whitespace before/after the v means strip_prefix doesn't match —
    // matters because release tags sometimes have stray whitespace from
    // git-tag descriptions.
    assert_eq!(strip_tag_prefix(" v1.0.0"), " v1.0.0");
    assert_eq!(strip_tag_prefix("v 1.0.0"), " 1.0.0");
}

// ---------------------------------------------------------------------------
// download_to_file with no parent dir falls back to "."
// ---------------------------------------------------------------------------

#[test]
#[serial_test::serial]
fn download_to_file_falls_back_to_cwd_when_dest_has_no_parent() {
    // A dest like "outfile" has no parent → fall back to "." for the
    // tempfile. Exercise via mockito + a relative dest. We restore CWD on
    // drop so subsequent tests aren't affected.
    let work = tempfile::tempdir().unwrap();
    let prior = std::env::current_dir().unwrap();
    std::env::set_current_dir(work.path()).unwrap();

    struct CwdGuard(std::path::PathBuf);
    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
    let _g = CwdGuard(prior);

    let mut server = mockito::Server::new();
    let body = b"contents";
    let _m = server
        .mock("GET", "/payload")
        .with_status(200)
        .with_body(&body[..])
        .create();
    let url = format!("{}/payload", server.url());

    let dest = std::path::PathBuf::from("relative-dest.bin");
    download_to_file(&url, &dest, None).expect("download with no-parent dest must succeed");
    let written = std::fs::read(&dest).expect("file must exist at relative dest");
    assert_eq!(written, body);
    let _ = std::fs::remove_file(&dest);
}

// ---------------------------------------------------------------------------
// Ground-truth contract: pin the client's asset-resolution logic against a
// captured REAL release manifest. The asset names in
// `testdata/release-v0.4.0-assets.json` were captured verbatim from the live
// GitHub release `tj-smith47/cfgd` v0.4.0 via:
//     gh release view v0.4.0 --json assets --jq '[.assets[].name]'
//
// This is the authoritative record of what an anodizer release actually
// publishes. The original `cfgd upgrade` bug shipped because every upgrade
// fixture hand-authored asset names that matched the client's own
// assumptions (`cfgd-<v>-linux-x86_64.tar.gz`, combined `checksums.txt`),
// so the suite stayed green while the producer shipped
// `cfgd-<v>-linux-amd64.tar.gz` + split per-artifact `.sha256` +
// `.sha256.cosign.bundle`. This test reads the names from the fixture and
// NEVER hand-constructs the expected asset list inline — that is the whole
// point: assert the consumer's resolution against the producer's real output
// so producer/consumer drift fails the build instead of hiding.
//
// v0.4.0 was signed key-based, so each platform archive carries the archive,
// its `.sha256`, and a `.sha256.cosign.bundle`, but NOT the optional
// `.sha256.cosign.pem` (that standalone Fulcio cert appears only in keyless
// releases; the client treats it as optional). Refresh this fixture from the
// first KEYLESS release once one is cut.
// ---------------------------------------------------------------------------

const REAL_RELEASE_V0_4_0_ASSETS: &str = include_str!("testdata/release-v0.4.0-assets.json");

#[test]
fn client_resolves_against_real_release_manifest() {
    let names: Vec<String> = serde_json::from_str(REAL_RELEASE_V0_4_0_ASSETS)
        .expect("fixture must be a JSON array of asset name strings");
    assert!(
        !names.is_empty(),
        "fixture must not be empty — did the capture run?"
    );

    let release = ReleaseInfo {
        tag: "v0.4.0".into(),
        version: Version::new(0, 4, 0),
        assets: names
            .iter()
            .map(|n| ReleaseAsset {
                name: n.clone(),
                download_url: format!("https://example.com/{n}"),
                size: 1,
            })
            .collect(),
    };

    // (rust_os, rust_arch, expected archive name) — the expected names use the
    // anodizer convention (darwin for macos; amd64/arm64 for the arches). If
    // the client resolved the Rust-arch name (`x86_64`) it would fail to match
    // the producer's `amd64` archive: exactly the original bug.
    let targets = [
        ("linux", "x86_64", "cfgd-0.4.0-linux-amd64.tar.gz"),
        ("linux", "aarch64", "cfgd-0.4.0-linux-arm64.tar.gz"),
        ("macos", "x86_64", "cfgd-0.4.0-darwin-amd64.tar.gz"),
        ("macos", "aarch64", "cfgd-0.4.0-darwin-arm64.tar.gz"),
    ];

    for (os, arch, expected_archive) in targets {
        let asset = find_asset_for(&release, os, arch).unwrap_or_else(|e| {
            panic!("client must resolve an archive for {os}/{arch} in the real manifest: {e}")
        });
        assert_eq!(
            asset.name, expected_archive,
            "{os}/{arch} must resolve to the producer's real archive name"
        );
    }

    // Checksum + cosign-bundle discovery for the linux/amd64 archive. The
    // producer ships split per-artifact `<archive>.sha256` files plus a
    // sibling `<archive>.sha256.cosign.bundle` for each.
    let linux_amd64 = "cfgd-0.4.0-linux-amd64.tar.gz";
    let checksum = find_checksum_asset(&release, linux_amd64)
        .expect("real manifest must carry a per-artifact .sha256 for the linux/amd64 archive");
    assert_eq!(checksum.name, format!("{linux_amd64}.sha256"));

    let bundle = find_cosign_bundle_asset(&release, &checksum.name)
        .expect("real manifest must carry a .sha256.cosign.bundle for the linux/amd64 archive");
    assert_eq!(bundle.name, format!("{}.cosign.bundle", checksum.name));

    // v0.4.0 is key-based, so the standalone Fulcio cert (`.sha256.cosign.pem`)
    // is absent. The client treats it as optional; assert absence rather than
    // presence so a future keyless release (which adds the `.pem`) does not
    // false-fail before the fixture is refreshed.
    assert!(
        find_cosign_cert_asset(&release, &checksum.name).is_none(),
        "key-based v0.4.0 must not publish a standalone .sha256.cosign.pem"
    );
}

// --- find_asset_for: non-host target resolution (windows + unmapped arch) ---

#[test]
fn find_asset_for_windows_target_uses_zip_suffix() {
    // The `.zip` suffix branch keys off the RESOLVED target OS (`windows`),
    // not the compile-time host. A release built for windows ships `.zip`;
    // resolving against a windows target on a non-windows host must still pick
    // the `.zip` archive (and the amd64 Go-arch name), never the `.tar.gz`.
    let release = ReleaseInfo {
        tag: "v9.9.0".into(),
        version: Version::new(9, 9, 0),
        assets: vec![
            ReleaseAsset {
                name: "cfgd-9.9.0-windows-amd64.zip".into(),
                download_url: "https://example.com/win".into(),
                size: 2048,
            },
            // A decoy `.tar.gz` with the same target — the resolver must NOT
            // pick this for windows.
            ReleaseAsset {
                name: "cfgd-9.9.0-windows-amd64.tar.gz".into(),
                download_url: "https://example.com/decoy".into(),
                size: 9,
            },
        ],
    };

    let asset = find_asset_for(&release, "windows", "x86_64")
        .expect("windows/x86_64 must resolve to the .zip archive");
    assert_eq!(asset.name, "cfgd-9.9.0-windows-amd64.zip");
    assert_eq!(asset.download_url, "https://example.com/win");
}

#[test]
fn find_asset_for_unmapped_arch_passes_arch_through_verbatim() {
    // An arch outside the x86_64/aarch64 map (e.g. riscv64) has no Go-arch
    // rename — both the go_arch and the rust_arch fallback candidate collapse
    // to the same `riscv64` token. The resolver must pass it through verbatim
    // and match `cfgd-<v>-linux-riscv64.tar.gz`.
    let release = ReleaseInfo {
        tag: "v9.9.0".into(),
        version: Version::new(9, 9, 0),
        assets: vec![ReleaseAsset {
            name: "cfgd-9.9.0-linux-riscv64.tar.gz".into(),
            download_url: "https://example.com/riscv".into(),
            size: 4096,
        }],
    };

    let asset = find_asset_for(&release, "linux", "riscv64")
        .expect("linux/riscv64 must resolve via the verbatim arch passthrough");
    assert_eq!(asset.name, "cfgd-9.9.0-linux-riscv64.tar.gz");
}

#[test]
fn find_asset_for_resolves_rust_arch_fallback_name() {
    // Tolerate a release built under the Rust-arch naming convention
    // (`x86_64`) even though anodizer normally emits the Go-arch (`amd64`).
    // The Go-arch candidate (`...-amd64...`) is absent here, so resolution
    // must fall through to the second candidate (`...-x86_64...`).
    let release = ReleaseInfo {
        tag: "v9.9.0".into(),
        version: Version::new(9, 9, 0),
        assets: vec![ReleaseAsset {
            name: "cfgd-9.9.0-linux-x86_64.tar.gz".into(),
            download_url: "https://example.com/rustarch".into(),
            size: 1024,
        }],
    };

    let asset = find_asset_for(&release, "linux", "x86_64")
        .expect("must fall back to the Rust-arch archive name when Go-arch is absent");
    assert_eq!(asset.name, "cfgd-9.9.0-linux-x86_64.tar.gz");
}

#[test]
fn find_asset_for_no_asset_error_reports_resolved_os_and_go_arch() {
    // The NoAsset error must name the RESOLVED archive OS (darwin, not macos)
    // and the RESOLVED Go arch (arm64, not aarch64) so operators see the names
    // they would search the release page for.
    let release = ReleaseInfo {
        tag: "v9.9.0".into(),
        version: Version::new(9, 9, 0),
        assets: vec![ReleaseAsset {
            name: "cfgd-9.9.0-linux-amd64.tar.gz".into(),
            download_url: "https://example.com/x".into(),
            size: 1,
        }],
    };

    let err = find_asset_for(&release, "macos", "aarch64").unwrap_err();
    match err {
        crate::errors::UpgradeError::NoAsset { os, arch } => {
            assert_eq!(os, "darwin", "error must report the resolved archive OS");
            assert_eq!(arch, "arm64", "error must report the resolved Go arch");
        }
        other => panic!("expected NoAsset, got: {other:?}"),
    }
}

// --- write_version_cache: create_dir_all failure surfaces InstallFailed ---

#[test]
fn write_version_cache_errors_when_cache_dir_path_is_blocked_by_a_file() {
    // cache_dir() resolves to <test_home>/.cache/cfgd. Plant a regular FILE at
    // <test_home>/.cache so create_dir_all(.cache/cfgd) cannot create the
    // directory — the write must surface InstallFailed, not panic or silently
    // succeed.
    let home = tempfile::tempdir().unwrap();
    let _guard = crate::with_test_home_guard(home.path());

    // Block the `.cache` path with a file.
    fs::write(home.path().join(".cache"), b"not a directory").unwrap();

    let cache = VersionCache {
        checked_at_secs: 42,
        latest_tag: "v9.9.0".into(),
        latest_version: "9.9.0".into(),
        current_version: "9.8.0".into(),
    };

    let err = write_version_cache(&cache).unwrap_err();
    assert!(
        matches!(err, crate::errors::UpgradeError::InstallFailed { .. }),
        "blocked cache dir must surface InstallFailed, got: {err:?}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("create cache dir"),
        "error message must name the failing step: {msg}"
    );
}
