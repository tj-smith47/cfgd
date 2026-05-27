// Self-update — query GitHub releases, download, verify, atomic install

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;

use crate::errors::{Result, UpgradeError};
use crate::output::{Printer, Role};

const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_API_BASE_ENV: &str = "CFGD_GITHUB_API_BASE";
const DEFAULT_REPO: &str = "tj-smith47/cfgd";

/// Resolve the GitHub Releases API base URL. Tests set CFGD_GITHUB_API_BASE
/// to redirect at a mockito server; production calls fall through to the
/// real api.github.com base.
fn github_api_base() -> String {
    std::env::var(GITHUB_API_BASE_ENV).unwrap_or_else(|_| GITHUB_API_BASE.to_string())
}
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

/// How the upgrade checksum file was verified. Surfaced in the structured
/// `UpgradeOutput` payload so consumers (CI, alerting) can detect when an
/// upgrade silently fell back to SHA256-only and react.
///
/// * `Cosign` — full cosign signature verified against the release's bundle +
///   public key. Strongest guarantee: a publisher-compromise attacker without
///   the cosign private key cannot forge a passing release.
/// * `Sha256Only` — cosign bundle, public key, or the `cosign` CLI was
///   unavailable; verification fell through to `checksums.txt` SHA256
///   comparison only. Trusts the GitHub Releases publisher chain.
/// * `StrictCosignRequired` — strict cosign mode was requested by the caller
///   (`--require-cosign` / `CFGD_REQUIRE_COSIGN=1`) and verification
///   succeeded under that policy. Distinct from `Cosign` so audit consumers
///   can tell apart "strict was demanded" from "strict happened by accident."
///
/// JSON wire values are hyphenated (`cosign`, `sha256-only`,
/// `strict-cosign-required`) — chosen for legibility in structured payloads.
/// Variants spell the rename out per-variant rather than via a blanket
/// `rename_all` because the workspace audit gate forbids the blanket form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VerificationMode {
    #[serde(rename = "cosign")]
    Cosign,
    #[serde(rename = "sha256-only")]
    Sha256Only,
    #[serde(rename = "strict-cosign-required")]
    StrictCosignRequired,
}

impl VerificationMode {
    /// The wire/JSON form of the mode, matching the per-variant serde renames.
    /// Used by callers that emit ad-hoc JSON payloads (e.g. the upgrade CLI)
    /// without round-tripping through `serde_json::to_value`.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            VerificationMode::Cosign => "cosign",
            VerificationMode::Sha256Only => "sha256-only",
            VerificationMode::StrictCosignRequired => "strict-cosign-required",
        }
    }
}

/// Outcome of [`download_and_install`] — the installed path plus the
/// verification mode that was actually exercised. The latter lets the CLI
/// surface a `verificationMode` field in its structured-output payload so
/// downstream consumers can alert on silent SHA256-only fallback.
#[derive(Debug, Clone)]
pub struct InstallReport {
    pub installed_path: PathBuf,
    pub verification_mode: VerificationMode,
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
    fetch_latest_release_from(&github_api_base(), repo, printer)
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
        let _ = s.finish_ok("Checked latest release");
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
/// all pieces are present and the `cosign` CLI is installed.
///
/// Behavior depends on `require_cosign`:
///
/// * **`require_cosign = false`** (default): graceful degradation. Missing
///   bundle, missing public key, or missing cosign CLI all return
///   `Ok(VerificationMode::Sha256Only)` after surfacing a `Role::Warn` so the
///   caller falls back to SHA256-only verification. A successful cosign
///   verify returns `Ok(VerificationMode::Cosign)`. An *explicit* cosign
///   verify failure (binary present, pieces present, bad signature) returns
///   `Err` — never proceed in that case.
///
/// * **`require_cosign = true`** (caller opted into strict mode via
///   `--require-cosign` / `CFGD_REQUIRE_COSIGN`): any of the three skip
///   conditions returns `Err(UpgradeError::CosignRequired { .. })` naming the
///   specific missing piece, blocking the upgrade. A successful verify
///   returns `Ok(VerificationMode::StrictCosignRequired)` so the structured
///   payload records that strict mode was honored.
fn verify_cosign_bundle(
    checksums_path: &Path,
    release: &ReleaseInfo,
    tmp_dir: &Path,
    require_cosign: bool,
    printer: Option<&Printer>,
) -> std::result::Result<VerificationMode, UpgradeError> {
    let Some(bundle_asset) = find_cosign_bundle_asset(release) else {
        let reason = "no cosign bundle attached to release";
        if require_cosign {
            return Err(UpgradeError::CosignRequired {
                reason: reason.to_string(),
            });
        }
        if let Some(p) = printer {
            p.status_simple(Role::Warn, "no cosign bundle attached to release — falling back to SHA256-only checksum verification. Downgrades publisher-compromise resistance to GitHub Releases trust.");
        }
        return Ok(VerificationMode::Sha256Only);
    };
    let Some(pub_key_asset) = find_cosign_public_key_asset(release) else {
        let reason = "cosign bundle found but no cosign.pub attached to release";
        if require_cosign {
            return Err(UpgradeError::CosignRequired {
                reason: reason.to_string(),
            });
        }
        if let Some(p) = printer {
            p.status_simple(Role::Warn, "cosign bundle found but no public key attached to release — cannot verify without cosign.pub. Falling back to SHA256-only.");
        }
        return Ok(VerificationMode::Sha256Only);
    };
    if crate::require_cosign().is_err() {
        let reason = "cosign CLI is not installed on this host";
        if require_cosign {
            return Err(UpgradeError::CosignRequired {
                reason: reason.to_string(),
            });
        }
        if let Some(p) = printer {
            p.status_simple(Role::Warn, "cosign bundle found but the cosign CLI is not installed — install cosign (https://docs.sigstore.dev/cosign/system_config/installation/) to enable signature verification. Falling back to SHA256-only.");
        }
        return Ok(VerificationMode::Sha256Only);
    }

    let bundle_path = tmp_dir.join(&bundle_asset.name);
    download_to_file(&bundle_asset.download_url, &bundle_path, printer)?;
    let pub_key_path = tmp_dir.join(&pub_key_asset.name);
    download_to_file(&pub_key_asset.download_url, &pub_key_path, printer)?;

    let verify_spinner = printer.map(|p| p.spinner("Verifying cosign signature..."));
    let outcome = run_cosign_verify_blob(checksums_path, &bundle_path, &pub_key_path);
    match &outcome {
        Ok(()) => {
            if let Some(s) = verify_spinner {
                let _ = s.finish_ok("Verified cosign signature");
            }
        }
        Err(e) => {
            if let Some(s) = verify_spinner {
                let _ = s
                    .finish_fail("Failed to verify cosign signature")
                    .detail(crate::output::collapse_to_subject_line(e));
            }
        }
    }
    outcome.map(|()| {
        tracing::info!(asset = %bundle_asset.name, "cosign signature verified");
        if require_cosign {
            VerificationMode::StrictCosignRequired
        } else {
            VerificationMode::Cosign
        }
    })
}

/// Run `cosign verify-blob --key ... --bundle ... -- <checksums>` and translate
/// the outcome into `Ok(())` / `Err(UpgradeError::DownloadFailed)`.
///
/// Extracted from [`verify_cosign_bundle`] so the cosign-shelling branches are
/// testable through the `CFGD_COSIGN_BIN` shim (see `oci/sign/tests.rs`)
/// without staging downloads through a mock HTTP server.
fn run_cosign_verify_blob(
    checksums_path: &Path,
    bundle_path: &Path,
    pub_key_path: &Path,
) -> std::result::Result<(), UpgradeError> {
    let output = crate::cosign_cmd()
        .arg("verify-blob")
        .arg(format!("--key={}", pub_key_path.display()))
        .arg(format!("--bundle={}", bundle_path.display()))
        .arg("--")
        .arg(checksums_path)
        .output();

    match output {
        Ok(o) if o.status.success() => Ok(()),
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
            pb.finish();
        }
        (Some(p), None) => {
            let spinner = p.spinner(format!("Downloading {url}..."));
            std::io::copy(&mut reader, &mut tmp).map_err(|e| UpgradeError::DownloadFailed {
                message: format!("stream to disk: {}", e),
            })?;
            let _ = spinner.finish_ok(format!("Downloaded {url}"));
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

/// Verify that the archive at `archive_path` matches the SHA256 listed for
/// `asset_name` inside the goreleaser-style `checksums.txt` body.
///
/// Three error branches, each distinct on the wire so operators can tell them
/// apart in incident triage:
/// * `ChecksumsEmpty` — `parse_checksums` produced no entries (truncation /
///   wrong file served).
/// * `ChecksumMissing` — the file parsed but `asset_name` is not in the list
///   (stripped-line attack or upload race).
/// * `ChecksumMismatch` — the file is listed but the local SHA differs
///   (genuine corruption or interception).
///
/// Pure helper — split out so the three branches are testable without
/// downloading anything.
fn verify_archive_checksum(
    archive_path: &Path,
    checksums_content: &str,
    asset_name: &str,
) -> std::result::Result<(), UpgradeError> {
    let checksums = parse_checksums(checksums_content);
    if checksums.is_empty() {
        return Err(UpgradeError::ChecksumsEmpty);
    }
    let Some(expected) = checksums.get(asset_name) else {
        return Err(UpgradeError::ChecksumMissing {
            file: asset_name.to_string(),
        });
    };
    let actual = sha256_file(archive_path)?;
    if actual != *expected {
        return Err(UpgradeError::ChecksumMismatch {
            file: asset_name.to_string(),
        });
    }
    Ok(())
}

/// Download, verify checksum, extract, and atomically install the new binary
/// over the running executable.
///
/// `require_cosign` switches the cosign verifier into strict mode: when set,
/// any missing cosign artifact (bundle, public key, or local CLI) blocks the
/// upgrade with [`UpgradeError::CosignRequired`] instead of silently falling
/// back to SHA256-only. The returned [`InstallReport`] records which mode was
/// actually exercised so structured-output consumers can detect fallbacks.
pub fn download_and_install(
    release: &ReleaseInfo,
    asset: &ReleaseAsset,
    require_cosign: bool,
    printer: Option<&Printer>,
) -> Result<InstallReport> {
    let current_exe = std::env::current_exe().map_err(|e| UpgradeError::InstallFailed {
        message: format!("cannot determine current binary path: {}", e),
    })?;
    download_and_install_to(release, asset, &current_exe, require_cosign, printer)
}

/// Same as [`download_and_install`], but installs over `target` instead of
/// `current_exe()`. Crate-internal so tests can drive the full HTTP +
/// cosign + checksum + extract flow against a tempdir without overwriting
/// the running test binary.
pub(crate) fn download_and_install_to(
    release: &ReleaseInfo,
    asset: &ReleaseAsset,
    target: &Path,
    require_cosign: bool,
    printer: Option<&Printer>,
) -> Result<InstallReport> {
    // Create temp directory for download
    let tmp_dir = tempfile::tempdir().map_err(|e| UpgradeError::DownloadFailed {
        message: format!("create temp dir: {}", e),
    })?;

    let archive_path = tmp_dir.path().join(&asset.name);

    // Download archive
    download_to_file(&asset.download_url, &archive_path, printer)?;

    // Download and verify checksum if available
    let verification_mode = if let Some(checksums_asset) = find_checksums_asset(release) {
        let checksums_path = tmp_dir.path().join(&checksums_asset.name);
        download_to_file(&checksums_asset.download_url, &checksums_path, printer)?;

        // Best-effort cosign verification of the checksums file. Bounds
        // publisher-compromise risk: a malicious release uploader cannot
        // forge a valid cosign signature over a tampered checksums.txt
        // without the private key. When `require_cosign` is true, any of
        // the three skip conditions surfaces as Err here instead of a
        // silent fallback to SHA256-only.
        let mode = verify_cosign_bundle(
            &checksums_path,
            release,
            tmp_dir.path(),
            require_cosign,
            printer,
        )?;

        let checksums_content =
            fs::read_to_string(&checksums_path).map_err(|e| UpgradeError::DownloadFailed {
                message: format!("read checksums: {}", e),
            })?;

        let verify_spinner = printer.map(|p| p.spinner("Verifying checksum..."));
        let verify_result = verify_archive_checksum(&archive_path, &checksums_content, &asset.name);
        match &verify_result {
            Ok(()) => {
                if let Some(s) = verify_spinner {
                    let _ = s.finish_ok("Checksum verified");
                }
            }
            Err(e) => {
                if let Some(s) = verify_spinner {
                    let _ = s
                        .finish_fail("Checksum verification failed")
                        .detail(crate::output::collapse_to_subject_line(e));
                }
            }
        }
        verify_result?;
        tracing::debug!("checksum verified for {}", asset.name);
        mode
    } else {
        return Err(UpgradeError::ChecksumMissing {
            file: asset.name.clone(),
        }
        .into());
    };

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
        let _ = s.finish_ok("Extracted archive");
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
    atomic_replace(&new_binary, target)?;

    Ok(InstallReport {
        installed_path: target.to_path_buf(),
        verification_mode,
    })
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
        let now = crate::unix_secs_now();

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
        checked_at_secs: crate::unix_secs_now(),
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
