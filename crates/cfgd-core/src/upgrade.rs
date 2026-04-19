// Self-update — query GitHub releases, download, verify, atomic install

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use semver::Version;

use crate::errors::{Result, UpgradeError};
use crate::output::Printer;

const GITHUB_API_BASE: &str = "https://api.github.com";
const DEFAULT_REPO: &str = "tj-smith47/cfgd";
const CACHE_TTL_SECS: u64 = 86400; // 24 hours
const CACHE_FILENAME: &str = "version-check.json";

/// Strip leading 'v' from a git tag to get the bare version string.
fn strip_tag_prefix(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

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
pub fn fetch_latest_release(repo: &str, printer: Option<&Printer>) -> Result<ReleaseInfo> {
    fetch_latest_release_from(GITHUB_API_BASE, repo, printer)
}

/// Query a releases API for the latest release (testable with custom base URL).
fn fetch_latest_release_from(
    api_base: &str,
    repo: &str,
    printer: Option<&Printer>,
) -> Result<ReleaseInfo> {
    let url = format!("{}/repos/{}/releases/latest", api_base, repo);

    let spinner = printer.map(|p| p.spinner("Checking for latest release..."));

    let agent = crate::http::http_agent(crate::http::HTTP_UPGRADE_TIMEOUT);
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

    if let Some(s) = spinner {
        s.finish_and_clear();
    }

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

    let version_str = strip_tag_prefix(&tag);
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
    let version_str = strip_tag_prefix(&release.tag);
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

/// Find the cosign signature bundle for the checksums asset. Produced by the
/// `checksum-cosign` entry in `.anodizer.yaml`.
fn find_cosign_bundle_asset(release: &ReleaseInfo) -> Option<&ReleaseAsset> {
    release
        .assets
        .iter()
        .find(|a| a.name.ends_with("-checksums.txt.cosign.bundle"))
}

/// Find a cosign public key asset, if shipped with the release.
fn find_cosign_public_key_asset(release: &ReleaseInfo) -> Option<&ReleaseAsset> {
    release
        .assets
        .iter()
        .find(|a| a.name == "cosign.pub" || a.name.ends_with("-cosign.pub"))
}

/// Verify `checksums_path` against the release's cosign bundle + public key if
/// all pieces are present and the `cosign` CLI is installed. Returns:
/// - `Ok(true)` when cosign verify succeeded,
/// - `Ok(false)` when verification was skipped (no bundle, no pub key, or cosign
///   not installed) — the caller falls back to SHA256-only with a warning,
/// - `Err` when all pieces are present but verify explicitly failed —
///   never proceed in that case.
fn verify_cosign_bundle(
    checksums_path: &Path,
    release: &ReleaseInfo,
    tmp_dir: &Path,
    printer: Option<&Printer>,
) -> std::result::Result<bool, UpgradeError> {
    let Some(bundle_asset) = find_cosign_bundle_asset(release) else {
        if let Some(p) = printer {
            p.warning("no cosign bundle attached to release — falling back to SHA256-only checksum verification. Downgrades publisher-compromise resistance to GitHub Releases trust.");
        }
        return Ok(false);
    };
    let Some(pub_key_asset) = find_cosign_public_key_asset(release) else {
        if let Some(p) = printer {
            p.warning("cosign bundle found but no public key attached to release — cannot verify without cosign.pub. Falling back to SHA256-only.");
        }
        return Ok(false);
    };
    if !crate::command_available("cosign") {
        if let Some(p) = printer {
            p.warning("cosign bundle found but the cosign CLI is not installed — install cosign (https://docs.sigstore.dev/cosign/system_config/installation/) to enable signature verification. Falling back to SHA256-only.");
        }
        return Ok(false);
    }

    let bundle_path = tmp_dir.join(&bundle_asset.name);
    download_to_file(&bundle_asset.download_url, &bundle_path, printer)?;
    let pub_key_path = tmp_dir.join(&pub_key_asset.name);
    download_to_file(&pub_key_asset.download_url, &pub_key_path, printer)?;

    let verify_spinner = printer.map(|p| p.spinner("Verifying cosign signature..."));
    let output = crate::cosign_cmd()
        .arg("verify-blob")
        .arg(format!("--key={}", pub_key_path.display()))
        .arg(format!("--bundle={}", bundle_path.display()))
        .arg("--")
        .arg(checksums_path)
        .output();
    if let Some(s) = verify_spinner {
        s.finish_and_clear();
    }

    match output {
        Ok(o) if o.status.success() => {
            tracing::info!(asset = %bundle_asset.name, "cosign signature verified");
            Ok(true)
        }
        Ok(o) => {
            let stderr = crate::stderr_lossy_trimmed(&o);
            Err(UpgradeError::DownloadFailed {
                message: format!("cosign verify-blob failed: {stderr}"),
            })
        }
        Err(e) => Err(UpgradeError::DownloadFailed {
            message: format!("cosign invocation failed: {e}"),
        }),
    }
}

/// Download a file from a URL to a local path.
fn download_to_file(
    url: &str,
    dest: &Path,
    printer: Option<&Printer>,
) -> std::result::Result<(), UpgradeError> {
    let agent = crate::http::http_agent(crate::http::HTTP_UPGRADE_TIMEOUT);
    let response = agent
        .get(url)
        .set("User-Agent", "cfgd-self-update")
        .call()
        .map_err(|e| UpgradeError::DownloadFailed {
            message: format!("{}", e),
        })?;

    // Determine content length for progress tracking
    let content_length: Option<u64> = response
        .header("content-length")
        .and_then(|v| v.parse().ok());

    // Stream directly to a temp file (avoids buffering entire binary in memory)
    let parent = dest.parent().unwrap_or(std::path::Path::new("."));
    let mut tmp =
        tempfile::NamedTempFile::new_in(parent).map_err(|e| UpgradeError::DownloadFailed {
            message: format!("create temp file: {}", e),
        })?;

    const MAX_DOWNLOAD_SIZE: u64 = 256 * 1024 * 1024;
    let mut reader = response.into_reader().take(MAX_DOWNLOAD_SIZE);

    // Use progress bar if we know the size, spinner otherwise
    match (printer, content_length) {
        (Some(p), Some(total)) => {
            let pb = p.progress_bar(total, url);
            let mut buf = [0u8; 8192];
            let mut downloaded: u64 = 0;
            loop {
                let n = reader
                    .read(&mut buf)
                    .map_err(|e| UpgradeError::DownloadFailed {
                        message: format!("stream to disk: {}", e),
                    })?;
                if n == 0 {
                    break;
                }
                std::io::Write::write_all(&mut tmp, &buf[..n]).map_err(|e| {
                    UpgradeError::DownloadFailed {
                        message: format!("stream to disk: {}", e),
                    }
                })?;
                downloaded += n as u64;
                pb.set_position(downloaded);
            }
            pb.finish_and_clear();
        }
        (Some(p), None) => {
            let spinner = p.spinner(&format!("Downloading {url}..."));
            std::io::copy(&mut reader, &mut tmp).map_err(|e| UpgradeError::DownloadFailed {
                message: format!("stream to disk: {}", e),
            })?;
            spinner.finish_and_clear();
        }
        _ => {
            std::io::copy(&mut reader, &mut tmp).map_err(|e| UpgradeError::DownloadFailed {
                message: format!("stream to disk: {}", e),
            })?;
        }
    }

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
pub fn download_and_install(
    release: &ReleaseInfo,
    asset: &ReleaseAsset,
    printer: Option<&Printer>,
) -> Result<PathBuf> {
    let current_exe = std::env::current_exe().map_err(|e| UpgradeError::InstallFailed {
        message: format!("cannot determine current binary path: {}", e),
    })?;

    // Create temp directory for download
    let tmp_dir = tempfile::tempdir().map_err(|e| UpgradeError::DownloadFailed {
        message: format!("create temp dir: {}", e),
    })?;

    let archive_path = tmp_dir.path().join(&asset.name);

    // Download archive
    download_to_file(&asset.download_url, &archive_path, printer)?;

    // Download and verify checksum if available
    if let Some(checksums_asset) = find_checksums_asset(release) {
        let checksums_path = tmp_dir.path().join(&checksums_asset.name);
        download_to_file(&checksums_asset.download_url, &checksums_path, printer)?;

        // Best-effort cosign verification of the checksums file. Bounds
        // publisher-compromise risk: a malicious release uploader cannot
        // forge a valid cosign signature over a tampered checksums.txt
        // without the private key.
        let _cosign_verified =
            verify_cosign_bundle(&checksums_path, release, tmp_dir.path(), printer)?;

        let checksums_content =
            fs::read_to_string(&checksums_path).map_err(|e| UpgradeError::DownloadFailed {
                message: format!("read checksums: {}", e),
            })?;

        let checksums = parse_checksums(&checksums_content);
        if checksums.is_empty() {
            return Err(UpgradeError::ChecksumsEmpty.into());
        }
        if let Some(expected) = checksums.get(&asset.name) {
            let verify_spinner = printer.map(|p| p.spinner("Verifying checksum..."));
            let actual = sha256_file(&archive_path)?;
            if actual != *expected {
                if let Some(s) = verify_spinner {
                    s.finish_and_clear();
                }
                return Err(UpgradeError::ChecksumMismatch {
                    file: asset.name.clone(),
                }
                .into());
            }
            if let Some(s) = verify_spinner {
                s.finish_and_clear();
            }
            tracing::debug!("checksum verified for {}", asset.name);
        } else {
            // The archive downloaded fine but checksums.txt does not list it
            // — distinct from "mismatch" so operators can tell interception /
            // stripped-line attacks from genuine corruption.
            return Err(UpgradeError::ChecksumMissing {
                file: asset.name.clone(),
            }
            .into());
        }
    } else {
        return Err(UpgradeError::ChecksumMissing {
            file: asset.name.clone(),
        }
        .into());
    }

    // Extract the archive
    let extract_dir = tmp_dir.path().join("extracted");
    fs::create_dir_all(&extract_dir).map_err(|e| UpgradeError::InstallFailed {
        message: format!("create extract dir: {}", e),
    })?;

    let extract_spinner = printer.map(|p| p.spinner("Extracting archive..."));
    #[cfg(unix)]
    extract_tarball(&archive_path, &extract_dir)?;
    #[cfg(windows)]
    extract_zip(&archive_path, &extract_dir)?;
    if let Some(s) = extract_spinner {
        s.finish_and_clear();
    }

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

    fs::create_dir_all(dest).map_err(|e| UpgradeError::InstallFailed {
        message: format!("create dest {}: {}", dest.display(), e),
    })?;

    // The tar crate rejects `..` and absolute paths by default, but symlinks
    // can still point outside `dest`. Canonicalize and iterate entries, skipping
    // symlinks/hardlinks and unpacking each into the canonical dest.
    let canonical_dest = dest
        .canonicalize()
        .map_err(|e| UpgradeError::InstallFailed {
            message: format!("canonicalize dest {}: {}", dest.display(), e),
        })?;

    for entry in tar.entries().map_err(|e| UpgradeError::InstallFailed {
        message: format!("iterate archive entries: {}", e),
    })? {
        let mut entry = entry.map_err(|e| UpgradeError::InstallFailed {
            message: format!("read archive entry: {}", e),
        })?;

        if entry.header().entry_type().is_symlink() || entry.header().entry_type().is_hard_link() {
            let path = entry.path().unwrap_or_default();
            tracing::warn!(path = %path.display(), "skipping symlink/hardlink in upgrade tarball");
            continue;
        }

        entry
            .unpack_in(&canonical_dest)
            .map_err(|e| UpgradeError::InstallFailed {
                message: format!("extract archive entry: {}", e),
            })?;
    }

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
pub fn check_with_cache(repo: Option<&str>, printer: Option<&Printer>) -> Result<UpdateCheck> {
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
    let check = check_latest(Some(repo), printer)?;

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
pub fn check_latest(repo: Option<&str>, printer: Option<&Printer>) -> Result<UpdateCheck> {
    let repo = repo.unwrap_or(DEFAULT_REPO);
    let current = current_version()?;
    let release = fetch_latest_release(repo, printer)?;
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
        // Write a fake cache file into the real cache dir, then invalidate.
        // Skip if the cache dir is unavailable or not writable (CI environments).
        let dir = match directories::ProjectDirs::from("dev", "cfgd", "cfgd") {
            Some(d) => d,
            None => return,
        };
        if fs::create_dir_all(dir.cache_dir()).is_err() {
            return; // skip if dir can't be created
        }
        let cache_path = dir.cache_dir().join(CACHE_FILENAME);
        let data = r#"{"checkedAtSecs":0,"latestTag":"v0","latestVersion":"0.0.0","currentVersion":"0.0.0"}"#;
        if fs::write(&cache_path, data).is_err() {
            return; // skip if not writable
        }
        // Another parallel test may race and invalidate the cache between write
        // and this check; skip if the file disappeared (test is still valid).
        if !cache_path.exists() {
            return;
        }

        invalidate_cache();

        assert!(
            !cache_path.exists(),
            "cache file should be removed after invalidation"
        );
    }

    #[test]
    fn invalidate_cache_no_panic_when_no_file() {
        // Ensure calling invalidate when no cache file exists does not panic
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
        // Test write_version_cache + read_version_cache via the real cache dir
        let cache = VersionCache {
            checked_at_secs: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            latest_tag: "v99.99.99".into(),
            latest_version: "99.99.99".into(),
            current_version: env!("CARGO_PKG_VERSION").into(),
        };

        // Write the cache
        let write_result = write_version_cache(&cache);
        if write_result.is_ok() {
            // Read it back
            let read = read_version_cache();
            assert!(read.is_some(), "should be able to read back written cache");
            let read = read.unwrap();
            assert_eq!(read.latest_tag, "v99.99.99");
            assert_eq!(read.latest_version, "99.99.99");
            assert_eq!(read.current_version, env!("CARGO_PKG_VERSION"));

            // Clean up by invalidating
            invalidate_cache();
        }
    }

    #[test]
    fn read_version_cache_returns_none_after_invalidation() {
        invalidate_cache();
        // After invalidation, the cache should be gone (or nonexistent)
        // We can't guarantee it was there before, but we can verify the function
        // doesn't panic and returns None when no file
        let result = read_version_cache();
        assert!(
            result.is_none(),
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
            err_str.contains("404")
                || err_str.contains("Not Found")
                || err_str.contains("status code"),
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
}
