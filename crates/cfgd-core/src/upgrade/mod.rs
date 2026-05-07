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
    // Tests that install a test-home override get a tempdir-scoped cache
    // directory so they don't pollute (or race against each other in) the
    // real user cache. Production callers see the real ProjectDirs path.
    if let Some(home) = crate::test_home_override() {
        return Some(home.join(".cache").join("cfgd"));
    }
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
mod tests;
