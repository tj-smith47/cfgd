// Self-update — query GitHub releases, download, verify, atomic install

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use semver::Version;

use crate::errors::{Result, UpgradeError};

const GITHUB_API_BASE: &str = "https://api.github.com";
const DEFAULT_REPO: &str = "tj-smith47/cfgd";
const CACHE_TTL_SECS: u64 = 86400; // 24 hours
const CACHE_FILENAME: &str = "version-check.json";

/// Information about a GitHub release.
#[derive(Debug, Clone)]
pub struct ReleaseInfo {
    pub tag: String,
    pub version: Version,
    pub assets: Vec<ReleaseAsset>,
}

/// A downloadable asset attached to a release.
#[derive(Debug, Clone)]
pub struct ReleaseAsset {
    pub name: String,
    pub download_url: String,
    pub size: u64,
}

/// Cached version check result, persisted to disk.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct VersionCache {
    checked_at_secs: u64,
    latest_tag: String,
    latest_version: String,
    current_version: String,
}

/// Result of a version check.
#[derive(Debug, Clone)]
pub struct UpdateCheck {
    pub current: Version,
    pub latest: Version,
    pub update_available: bool,
    pub release: Option<ReleaseInfo>,
}

/// Return the compiled-in version of cfgd.
pub fn current_version() -> std::result::Result<Version, UpgradeError> {
    Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| UpgradeError::VersionParse {
        message: format!("cannot parse compiled version: {}", e),
    })
}

/// Query the GitHub Releases API for the latest release.
pub fn fetch_latest_release(repo: &str) -> Result<ReleaseInfo> {
    let url = format!("{}/repos/{}/releases/latest", GITHUB_API_BASE, repo);

    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(300))
        .build();
    let response = agent
        .get(&url)
        .set("Accept", "application/vnd.github+json")
        .set("User-Agent", "cfgd-self-update")
        .call()
        .map_err(|e| UpgradeError::ApiError {
            message: format!("{}", e),
        })?;

    let body: String = response.into_string().map_err(|e| UpgradeError::ApiError {
        message: format!("failed to read response body: {}", e),
    })?;

    parse_release_json(&body)
}

fn parse_release_json(body: &str) -> Result<ReleaseInfo> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| UpgradeError::ApiError {
            message: format!("invalid JSON: {}", e),
        })?;

    let tag = json["tag_name"]
        .as_str()
        .ok_or_else(|| UpgradeError::ApiError {
            message: "missing tag_name in release".into(),
        })?
        .to_string();

    let version_str = tag.strip_prefix('v').unwrap_or(&tag);
    let version = Version::parse(version_str).map_err(|e| UpgradeError::VersionParse {
        message: format!("cannot parse release version '{}': {}", tag, e),
    })?;

    let assets = json["assets"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    Some(ReleaseAsset {
                        name: a["name"].as_str()?.to_string(),
                        download_url: a["browser_download_url"].as_str()?.to_string(),
                        size: a["size"].as_u64().unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(ReleaseInfo {
        tag,
        version,
        assets,
    })
}

/// Find the correct binary asset for the current OS and architecture.
pub fn find_asset_for_platform(
    release: &ReleaseInfo,
) -> std::result::Result<&ReleaseAsset, UpgradeError> {
    let os = std::env::consts::OS;
    let archive_arch = std::env::consts::ARCH;

    let archive_os = match os {
        "macos" => "darwin",
        other => other,
    };

    // Look for: cfgd-<version>-<os>-<arch>.tar.gz (Unix) or .zip (Windows)
    let version_str = release.tag.strip_prefix('v').unwrap_or(&release.tag);
    #[cfg(unix)]
    let archive_suffix = ".tar.gz";
    #[cfg(windows)]
    let archive_suffix = ".zip";
    let expected_name = format!(
        "cfgd-{}-{}-{}{}",
        version_str, archive_os, archive_arch, archive_suffix
    );

    release
        .assets
        .iter()
        .find(|a| a.name == expected_name)
        .ok_or_else(|| UpgradeError::NoAsset {
            os: archive_os.to_string(),
            arch: archive_arch.to_string(),
        })
}

/// Find the checksums asset for a release.
fn find_checksums_asset(release: &ReleaseInfo) -> Option<&ReleaseAsset> {
    release
        .assets
        .iter()
        .find(|a| a.name.ends_with("-checksums.txt"))
}

/// Download a file from a URL to a local path.
fn download_to_file(url: &str, dest: &Path) -> std::result::Result<(), UpgradeError> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(300))
        .build();
    let response = agent
        .get(url)
        .set("User-Agent", "cfgd-self-update")
        .call()
        .map_err(|e| UpgradeError::DownloadFailed {
            message: format!("{}", e),
        })?;

    // Stream directly to a temp file (avoids buffering entire binary in memory)
    let parent = dest.parent().unwrap_or(std::path::Path::new("."));
    let mut tmp =
        tempfile::NamedTempFile::new_in(parent).map_err(|e| UpgradeError::DownloadFailed {
            message: format!("create temp file: {}", e),
        })?;

    const MAX_DOWNLOAD_SIZE: u64 = 256 * 1024 * 1024;
    let mut reader = response.into_reader().take(MAX_DOWNLOAD_SIZE);
    std::io::copy(&mut reader, &mut tmp).map_err(|e| UpgradeError::DownloadFailed {
        message: format!("stream to disk: {}", e),
    })?;

    tmp.persist(dest)
        .map_err(|e| UpgradeError::DownloadFailed {
            message: format!("rename to {}: {}", dest.display(), e.error),
        })?;

    Ok(())
}

/// Parse a checksums.txt file into a map of filename -> hex SHA256.
fn parse_checksums(content: &str) -> HashMap<String, String> {
    content
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let filename = parts.next()?;
            Some((filename.to_string(), hash.to_lowercase()))
        })
        .collect()
}

/// Compute the SHA256 hex digest of a file.
fn sha256_file(path: &Path) -> std::result::Result<String, UpgradeError> {
    let bytes = fs::read(path).map_err(|e| UpgradeError::DownloadFailed {
        message: format!("read {}: {}", path.display(), e),
    })?;
    Ok(crate::sha256_hex(&bytes))
}

/// Download, verify checksum, extract, and atomically install the new binary.
///
/// Returns the path to the newly installed binary.
pub fn download_and_install(release: &ReleaseInfo, asset: &ReleaseAsset) -> Result<PathBuf> {
    let current_exe = std::env::current_exe().map_err(|e| UpgradeError::InstallFailed {
        message: format!("cannot determine current binary path: {}", e),
    })?;

    // Create temp directory for download
    let tmp_dir = tempfile::tempdir().map_err(|e| UpgradeError::DownloadFailed {
        message: format!("create temp dir: {}", e),
    })?;

    let archive_path = tmp_dir.path().join(&asset.name);

    // Download archive
    download_to_file(&asset.download_url, &archive_path)?;

    // Download and verify checksum if available
    if let Some(checksums_asset) = find_checksums_asset(release) {
        let checksums_path = tmp_dir.path().join(&checksums_asset.name);
        download_to_file(&checksums_asset.download_url, &checksums_path)?;

        let checksums_content =
            fs::read_to_string(&checksums_path).map_err(|e| UpgradeError::DownloadFailed {
                message: format!("read checksums: {}", e),
            })?;

        let checksums = parse_checksums(&checksums_content);
        if let Some(expected) = checksums.get(&asset.name) {
            let actual = sha256_file(&archive_path)?;
            if actual != *expected {
                return Err(UpgradeError::ChecksumMismatch {
                    file: asset.name.clone(),
                }
                .into());
            }
            tracing::debug!("checksum verified for {}", asset.name);
        } else {
            return Err(UpgradeError::ChecksumMismatch {
                file: asset.name.clone(),
            }
            .into());
        }
    } else {
        return Err(UpgradeError::ChecksumMismatch {
            file: asset.name.clone(),
        }
        .into());
    }

    // Extract the archive
    let extract_dir = tmp_dir.path().join("extracted");
    fs::create_dir_all(&extract_dir).map_err(|e| UpgradeError::InstallFailed {
        message: format!("create extract dir: {}", e),
    })?;

    #[cfg(unix)]
    extract_tarball(&archive_path, &extract_dir)?;
    #[cfg(windows)]
    extract_zip(&archive_path, &extract_dir)?;

    // Find the cfgd binary in the extracted contents
    #[cfg(unix)]
    let binary_name = "cfgd";
    #[cfg(windows)]
    let binary_name = "cfgd.exe";
    let new_binary = extract_dir.join(binary_name);
    if !new_binary.exists() {
        return Err(UpgradeError::InstallFailed {
            message: format!(
                "extracted archive does not contain '{}' binary",
                binary_name
            ),
        }
        .into());
    }

    // Make it executable (no-op on Windows)
    crate::set_file_permissions(&new_binary, 0o755).map_err(|e| UpgradeError::InstallFailed {
        message: format!("set permissions: {}", e),
    })?;

    // Install new binary over old.
    // Unix: atomic rename via tempfile. Windows: rename-dance (can't overwrite running exe).
    let target = &current_exe;
    atomic_replace(&new_binary, target)?;

    Ok(target.clone())
}

/// Atomically replace `target` with `source`.
/// Copies source to a NamedTempFile in the target directory, then persists it
/// over the target (atomic rename on the same filesystem).
#[cfg(unix)]
fn atomic_replace(source: &Path, target: &Path) -> std::result::Result<(), UpgradeError> {
    let target_dir = target.parent().ok_or_else(|| UpgradeError::InstallFailed {
        message: "target has no parent directory".into(),
    })?;

    // Create a temp file in the target directory so rename is same-FS
    let tmp =
        tempfile::NamedTempFile::new_in(target_dir).map_err(|e| UpgradeError::InstallFailed {
            message: format!("create temp file in {}: {}", target_dir.display(), e),
        })?;

    // Copy source to the temp file
    fs::copy(source, tmp.path()).map_err(|e| UpgradeError::InstallFailed {
        message: format!("copy to staging: {}", e),
    })?;

    // Persist (atomic rename) temp file to target
    tmp.persist(target)
        .map_err(|e| UpgradeError::InstallFailed {
            message: format!("atomic rename: {}", e),
        })?;

    Ok(())
}

/// Replace `target` with `source` using the Windows rename-dance.
/// Windows cannot overwrite a running executable, so we rename the current
/// binary to `.exe.old`, copy the new one into place, and clean up `.old`
/// on next startup via `cleanup_old_binary`.
#[cfg(windows)]
fn atomic_replace(source: &Path, target: &Path) -> std::result::Result<(), UpgradeError> {
    // with_extension replaces .exe → .exe.old (not appends)
    let old = target.with_extension("exe.old");
    // Clean up from previous upgrades
    let _ = fs::remove_file(&old);
    // Rename running binary out of the way (can't overwrite running exe on Windows)
    if target.exists() {
        fs::rename(target, &old).map_err(|e| UpgradeError::InstallFailed {
            message: format!("rename {} -> {}: {}", target.display(), old.display(), e),
        })?;
    }
    // Copy new binary into place
    fs::copy(source, target).map_err(|e| UpgradeError::InstallFailed {
        message: format!("copy {} -> {}: {}", source.display(), target.display(), e),
    })?;
    Ok(())
}

/// Extract a .tar.gz archive to a directory.
#[cfg(unix)]
fn extract_tarball(archive: &Path, dest: &Path) -> std::result::Result<(), UpgradeError> {
    let file = fs::File::open(archive).map_err(|e| UpgradeError::InstallFailed {
        message: format!("open archive {}: {}", archive.display(), e),
    })?;

    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);

    tar.unpack(dest).map_err(|e| UpgradeError::InstallFailed {
        message: format!("extract archive: {}", e),
    })?;

    Ok(())
}

/// Extract a .zip archive to a directory.
#[cfg(windows)]
fn extract_zip(archive: &Path, dest: &Path) -> std::result::Result<(), UpgradeError> {
    let file = fs::File::open(archive).map_err(|e| UpgradeError::InstallFailed {
        message: format!("open archive {}: {}", archive.display(), e),
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| UpgradeError::InstallFailed {
        message: format!("read zip {}: {}", archive.display(), e),
    })?;
    zip.extract(dest).map_err(|e| UpgradeError::InstallFailed {
        message: format!("extract zip: {}", e),
    })?;
    Ok(())
}

/// Check if the daemon is running and restart it.
/// Returns true if the daemon was restarted, false if it wasn't running.
pub fn restart_daemon_if_running() -> bool {
    let status = match crate::daemon::query_daemon_status() {
        Ok(Some(s)) => s,
        _ => return false,
    };

    // Daemon is running — terminate so the service manager restarts it
    // with the new binary.
    crate::terminate_process(status.pid);
    tracing::info!("terminated daemon (pid {})", status.pid);
    true
}

/// Clean up the old binary left behind by the Windows rename-dance upgrade.
/// Call this on startup. No-op on Unix.
#[cfg(windows)]
pub fn cleanup_old_binary() {
    if let Ok(exe) = std::env::current_exe() {
        let old = exe.with_extension("exe.old");
        let _ = fs::remove_file(old);
    }
}

/// Clean up the old binary left behind by the Windows rename-dance upgrade.
/// Call this on startup. No-op on Unix.
#[cfg(unix)]
pub fn cleanup_old_binary() {
    // Unix atomic_replace doesn't leave old files
}

/// Check for an update, using a 24h disk cache to avoid excessive API calls.
pub fn check_with_cache(repo: Option<&str>) -> Result<UpdateCheck> {
    let repo = repo.unwrap_or(DEFAULT_REPO);
    let current = current_version()?;

    // Try reading from cache
    if let Some(cache) = read_version_cache() {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        if now.saturating_sub(cache.checked_at_secs) < CACHE_TTL_SECS {
            let cached_version =
                Version::parse(&cache.latest_version).map_err(|e| UpgradeError::VersionParse {
                    message: format!("cached version: {}", e),
                })?;

            return Ok(UpdateCheck {
                update_available: cached_version > current,
                current,
                latest: cached_version,
                release: None,
            });
        }
    }

    // Cache miss or expired — fall through to fresh check + update cache
    let check = check_latest(Some(repo))?;

    let _ = write_version_cache(&VersionCache {
        checked_at_secs: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        latest_tag: check
            .release
            .as_ref()
            .map(|r| r.tag.clone())
            .unwrap_or_default(),
        latest_version: check.latest.to_string(),
        current_version: check.current.to_string(),
    });

    Ok(check)
}

/// Check for an update without using cache. Always queries the API.
pub fn check_latest(repo: Option<&str>) -> Result<UpdateCheck> {
    let repo = repo.unwrap_or(DEFAULT_REPO);
    let current = current_version()?;
    let release = fetch_latest_release(repo)?;
    let update_available = release.version > current;

    Ok(UpdateCheck {
        current,
        latest: release.version.clone(),
        update_available,
        release: Some(release),
    })
}

fn cache_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("dev", "cfgd", "cfgd").map(|dirs| dirs.cache_dir().to_path_buf())
}

fn read_version_cache() -> Option<VersionCache> {
    let dir = cache_dir()?;
    let path = dir.join(CACHE_FILENAME);
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn write_version_cache(cache: &VersionCache) -> std::result::Result<(), UpgradeError> {
    let dir = cache_dir().ok_or_else(|| UpgradeError::InstallFailed {
        message: "cannot determine cache directory".into(),
    })?;

    fs::create_dir_all(&dir).map_err(|e| UpgradeError::InstallFailed {
        message: format!("create cache dir: {}", e),
    })?;

    let path = dir.join(CACHE_FILENAME);
    let json = serde_json::to_string(cache).map_err(|e| UpgradeError::InstallFailed {
        message: format!("serialize cache: {}", e),
    })?;

    crate::atomic_write_str(&path, &json).map_err(|e| UpgradeError::InstallFailed {
        message: format!("write cache: {}", e),
    })?;

    Ok(())
}

/// Invalidate the version check cache so the next check queries the API.
pub fn invalidate_cache() {
    if let Some(dir) = cache_dir() {
        let _ = fs::remove_file(dir.join(CACHE_FILENAME));
    }
}

/// Duration for the daemon's version check timer.
pub fn version_check_interval() -> Duration {
    Duration::from_secs(CACHE_TTL_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_is_valid_semver() {
        let _v = current_version().expect("CARGO_PKG_VERSION should be valid semver");
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
        assert!(parse_release_json(json).is_err());
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

        let result = find_asset_for_platform(&release);
        assert!(result.is_err());
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

    #[test]
    fn version_cache_round_trip() {
        let cache = VersionCache {
            checked_at_secs: 1000,
            latest_tag: "v0.2.0".into(),
            latest_version: "0.2.0".into(),
            current_version: "0.1.0".into(),
        };
        let json = serde_json::to_string(&cache).expect("serialize");
        let restored: VersionCache = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.latest_version, "0.2.0");
        assert_eq!(restored.checked_at_secs, 1000);
    }

    #[test]
    fn update_check_detects_newer() {
        let current = Version::new(0, 1, 0);
        let latest = Version::new(0, 2, 0);
        assert!(latest > current, "newer version should compare greater");
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
}
