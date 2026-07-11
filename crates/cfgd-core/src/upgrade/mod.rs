// Self-update — query GitHub releases, download, verify, atomic install

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use semver::Version;

use crate::PathDisplayExt;
use crate::errors::{Result, UpgradeError};
use crate::output::{Printer, Role};

mod check;
mod dedup;
pub use check::{
    UpdateAction, UpdateCheckEffects, UpdateCheckOutcome, resolve_action, resolved_interval,
    run_update_check, should_check,
};
pub use dedup::{
    RideAlongOutcome, SkillStaleness, StandaloneSkillAction, StandaloneSkillOutcome,
    UpdateSurfaces, aggregate_skill_staleness, compute_update_surfaces,
    consolidated_skill_stale_message, refresh_user_scope_skills, resolve_standalone_skill_action,
    run_standalone_skill_action,
};

const GITHUB_API_BASE: &str = "https://api.github.com";
const GITHUB_API_BASE_ENV: &str = "CFGD_GITHUB_API_BASE";
const DEFAULT_REPO: &str = "tj-smith47/cfgd";

/// OIDC issuer asserted by the keyless cosign signature: the GitHub Actions
/// OIDC provider that mints the workflow identity token during the release run.
const COSIGN_OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// Certificate-identity regexp pinning the signer to cfgd's own signing
/// workflows, each on its legitimate ref. Exactly three workflows in the
/// canonical repo ever hold `id-token: write` and sign release assets:
///
/// * `publish-crate.yml@refs/heads/master` — the per-crate release leg that
///   `release.yml` invokes; the real signer of every published stable asset.
/// * `release.yml@refs/tags/v*` — the release orchestrator, pinned to version
///   tags in case a future asset is signed there directly.
/// * `nightly.yml@refs/heads/master` — signs the rolling `nightly` prerelease
///   that the prerelease channel's endpoint can list.
///
/// The per-workflow ref pin is the security win over a bare repo-level pin: a
/// signature minted by any other workflow in this repo (e.g. `ci.yml`), by any
/// signing workflow running on a non-canonical ref (e.g. a topic branch), or
/// by a fork/foreign repo is rejected even if it chains to a valid Fulcio
/// root.
const COSIGN_IDENTITY_REGEXP: &str = r"^https://github\.com/tj-smith47/cfgd/\.github/workflows/(publish-crate\.ya?ml@refs/heads/master$|release\.ya?ml@refs/tags/v[0-9]|nightly\.ya?ml@refs/heads/master$)";

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
/// * `Cosign` — keyless cosign signature verified (Fulcio/OIDC + Rekor)
///   against the release's per-artifact bundle. Strongest guarantee: a
///   publisher-compromise attacker cannot mint a signature whose Fulcio
///   identity matches the pinned signing-workflow regexp.
/// * `Sha256Only` — the cosign bundle or the `cosign` CLI was unavailable;
///   verification fell through to the `<archive>.sha256` SHA256 comparison
///   only. Trusts the GitHub Releases publisher chain.
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

/// Query a releases API for the latest *stable* release (testable with custom
/// base URL). Hits `releases/latest`, which GitHub defines to exclude
/// prereleases and drafts.
fn fetch_latest_release_from(
    api_base: &str,
    repo: &str,
    printer: Option<&Printer>,
) -> Result<ReleaseInfo> {
    let url = format!("{}/repos/{}/releases/latest", api_base, repo);
    let body = github_get(
        &url,
        printer,
        "Checking for latest release...",
        "Checked latest release",
    )?;
    parse_release_json(&body)
}

/// Query a releases API for the newest release *including prereleases*
/// (testable with custom base URL). Hits the `releases` LIST endpoint and
/// returns the entry with the highest semver version. Tags that don't parse as
/// semver are skipped; if no parseable release exists, returns an `ApiError`.
fn fetch_newest_release_from(
    api_base: &str,
    repo: &str,
    printer: Option<&Printer>,
) -> Result<ReleaseInfo> {
    let url = format!("{}/repos/{}/releases", api_base, repo);
    let body = github_get(
        &url,
        printer,
        "Checking for newest release (incl. prereleases)...",
        "Checked newest release",
    )?;

    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| UpgradeError::ApiError {
            message: format!("invalid JSON: {}", e),
        })?;
    let arr = json.as_array().ok_or_else(|| UpgradeError::ApiError {
        message: "releases list response was not a JSON array".into(),
    })?;

    arr.iter()
        .filter_map(|elem| parse_one_release(elem).ok())
        .max_by(|a, b| a.version.cmp(&b.version))
        .ok_or_else(|| {
            UpgradeError::ApiError {
                message: "no parseable release found in releases list".into(),
            }
            .into()
        })
}

/// Dispatch the release fetch by channel. `None`, `"stable"`, and any
/// unrecognized value resolve to the stable `releases/latest` path;
/// `"prerelease"` resolves to the prerelease-inclusive list path. Matching is
/// case-insensitive. An unrecognized non-stable channel logs a warning and
/// falls back to stable.
fn fetch_release_for_channel(
    api_base: &str,
    repo: &str,
    channel: Option<&str>,
    printer: Option<&Printer>,
) -> Result<ReleaseInfo> {
    match channel.map(str::to_ascii_lowercase).as_deref() {
        Some("prerelease") => fetch_newest_release_from(api_base, repo, printer),
        None | Some("stable") => fetch_latest_release_from(api_base, repo, printer),
        Some(other) => {
            tracing::warn!(
                channel = other,
                "unknown update channel; tracking stable releases"
            );
            fetch_latest_release_from(api_base, repo, printer)
        }
    }
}

/// Issue the GitHub GET with the upgrade agent + headers, surfacing the spinner
/// when a printer is supplied, and return the response body. `start_label` is the
/// in-flight spinner text; `finish_label` is the completion text — both reflect
/// the release channel being queried so the wording stays accurate.
fn github_get(
    url: &str,
    printer: Option<&Printer>,
    start_label: &str,
    finish_label: &str,
) -> Result<String> {
    let spinner = printer.map(|p| p.spinner(start_label.to_string()));

    let agent = crate::http::http_agent(crate::http::HTTP_UPGRADE_TIMEOUT);
    let mut response = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "cfgd-self-update")
        .call()
        .map_err(|e| UpgradeError::ApiError {
            message: format!("{}", e),
        })?;

    let body: String =
        response
            .body_mut()
            .read_to_string()
            .map_err(|e| UpgradeError::ApiError {
                message: format!("failed to read response body: {}", e),
            })?;

    if let Some(s) = spinner {
        let _ = s.finish_ok(finish_label.to_string());
    }

    Ok(body)
}

fn parse_release_json(body: &str) -> Result<ReleaseInfo> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| UpgradeError::ApiError {
            message: format!("invalid JSON: {}", e),
        })?;
    parse_one_release(&json)
}

/// Parse a single GitHub release object (`{tag_name, assets}`) into a
/// [`ReleaseInfo`], shared by the single-object (`releases/latest`) and
/// list-element (`releases`) parse paths.
fn parse_one_release(json: &serde_json::Value) -> Result<ReleaseInfo> {
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
    find_asset_for(release, std::env::consts::OS, std::env::consts::ARCH)
}

/// Resolve the binary archive asset for an explicit Rust `OS`/`ARCH` pair.
///
/// Split out from [`find_asset_for_platform`] (which passes the host's
/// `std::env::consts::{OS,ARCH}`) so callers can resolve assets for a platform
/// other than the one the process runs on — the contract test exercises every
/// supported target against a captured real-release manifest.
fn find_asset_for<'a>(
    release: &'a ReleaseInfo,
    rust_os: &str,
    rust_arch: &str,
) -> std::result::Result<&'a ReleaseAsset, UpgradeError> {
    let archive_os = match rust_os {
        "macos" => "darwin",
        other => other,
    };

    // anodizer names archives with the Go arch (`{{ .Arch }}`: amd64/arm64),
    // not the Rust arch (`x86_64`/`aarch64`). Match the Go name first; tolerate
    // the Rust-arch name as a fallback so a release built under either naming
    // convention still resolves.
    let go_arch = match rust_arch {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };

    // Windows ships `.zip`; every other target ships `.tar.gz`. Key off the
    // resolved target OS rather than the compile-time host so a non-host
    // platform resolves correctly regardless of where the lookup runs.
    let version_str = strip_tag_prefix(&release.tag);
    let archive_suffix = if archive_os == "windows" {
        ".zip"
    } else {
        ".tar.gz"
    };
    let candidates = [
        format!("cfgd-{version_str}-{archive_os}-{go_arch}{archive_suffix}"),
        format!("cfgd-{version_str}-{archive_os}-{rust_arch}{archive_suffix}"),
    ];

    release
        .assets
        .iter()
        .find(|a| candidates.iter().any(|c| c == &a.name))
        .ok_or_else(|| UpgradeError::NoAsset {
            os: archive_os.to_string(),
            arch: go_arch.to_string(),
        })
}

/// Find the per-artifact checksum asset (`<archive>.sha256`) for `archive_name`.
/// anodizer signs checksums with `split: true`, producing one bare-hash file
/// per artifact rather than a single combined `checksums.txt`.
fn find_checksum_asset<'a>(
    release: &'a ReleaseInfo,
    archive_name: &str,
) -> Option<&'a ReleaseAsset> {
    let expected = format!("{archive_name}.sha256");
    release.assets.iter().find(|a| a.name == expected)
}

/// Find the keyless cosign signature bundle for the per-artifact checksum
/// asset. anodizer signs each `<archive>.sha256` file, producing a sibling
/// `<archive>.sha256.cosign.bundle`.
fn find_cosign_bundle_asset<'a>(
    release: &'a ReleaseInfo,
    checksum_asset_name: &str,
) -> Option<&'a ReleaseAsset> {
    let expected = format!("{checksum_asset_name}.cosign.bundle");
    release.assets.iter().find(|a| a.name == expected)
}

/// Find a separately-published Fulcio certificate for the checksum asset, if
/// the release attaches one (`<archive>.sha256.cosign.pem`). Keyless bundles
/// normally embed the certificate, so this is usually absent — when present it
/// is passed to `cosign verify-blob --certificate`.
fn find_cosign_cert_asset<'a>(
    release: &'a ReleaseInfo,
    checksum_asset_name: &str,
) -> Option<&'a ReleaseAsset> {
    let expected = format!("{checksum_asset_name}.cosign.pem");
    release.assets.iter().find(|a| a.name == expected)
}

/// Verify `checksums_path` against the release's keyless cosign bundle if the
/// bundle is attached and the `cosign` CLI is installed. The bundle signs the
/// per-artifact `<archive>.sha256` file named by `checksum_asset_name`.
///
/// Verification is keyless (Fulcio/OIDC + Rekor): there is no published public
/// key. The signer identity is pinned by [`COSIGN_OIDC_ISSUER`] and
/// [`COSIGN_IDENTITY_REGEXP`]. A separately-published Fulcio certificate
/// (`<archive>.sha256.cosign.pem`), if present, is passed via `--certificate`;
/// otherwise the cert embedded in the bundle is used.
///
/// Behavior depends on `require_cosign`:
///
/// * **`require_cosign = false`** (default): graceful degradation. A missing
///   bundle or missing cosign CLI returns `Ok(VerificationMode::Sha256Only)`
///   after surfacing a `Role::Warn` so the caller falls back to SHA256-only
///   verification. A successful cosign verify returns
///   `Ok(VerificationMode::Cosign)`. An *explicit* cosign verify failure
///   (binary present, bundle present, bad signature) returns `Err` — never
///   proceed in that case.
///
/// * **`require_cosign = true`** (caller opted into strict mode via
///   `--require-cosign` / `CFGD_REQUIRE_COSIGN`): either skip condition
///   returns `Err(UpgradeError::CosignRequired { .. })` naming the specific
///   missing piece, blocking the upgrade. A successful verify returns
///   `Ok(VerificationMode::StrictCosignRequired)` so the structured payload
///   records that strict mode was honored.
fn verify_cosign_bundle(
    checksums_path: &Path,
    checksum_asset_name: &str,
    release: &ReleaseInfo,
    tmp_dir: &Path,
    require_cosign: bool,
    printer: Option<&Printer>,
) -> std::result::Result<VerificationMode, UpgradeError> {
    let Some(bundle_asset) = find_cosign_bundle_asset(release, checksum_asset_name) else {
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

    // Keyless bundles embed the Fulcio cert, so a separate cert asset is
    // usually absent; download and pass it only when the release publishes one.
    let cert_path = if let Some(cert_asset) = find_cosign_cert_asset(release, checksum_asset_name) {
        let path = tmp_dir.join(&cert_asset.name);
        download_to_file(&cert_asset.download_url, &path, printer)?;
        Some(path)
    } else {
        None
    };

    let verify_spinner = printer.map(|p| p.spinner("Verifying cosign signature..."));
    let outcome = run_cosign_verify_blob(checksums_path, &bundle_path, cert_path.as_deref());
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

/// Run keyless `cosign verify-blob --bundle ... [--certificate ...]
/// --certificate-oidc-issuer ... --certificate-identity-regexp ... --
/// <checksums>` and translate the outcome into `Ok(())` /
/// `Err(UpgradeError::DownloadFailed)`.
///
/// `cert_path` is supplied only when the release publishes a standalone Fulcio
/// certificate; keyless bundles normally embed the cert, in which case the
/// `--certificate` flag is omitted.
///
/// Extracted from [`verify_cosign_bundle`] so the cosign-shelling branches are
/// testable through the `CFGD_COSIGN_BIN` shim (see `test_helpers`) without
/// staging downloads through a mock HTTP server.
fn run_cosign_verify_blob(
    checksums_path: &Path,
    bundle_path: &Path,
    cert_path: Option<&Path>,
) -> std::result::Result<(), UpgradeError> {
    let mut cmd = crate::cosign_cmd();
    cmd.arg("verify-blob")
        .arg(format!("--bundle={}", bundle_path.display()));
    if let Some(cert) = cert_path {
        cmd.arg(format!("--certificate={}", cert.display()));
    }
    let output = cmd
        .arg(format!("--certificate-oidc-issuer={COSIGN_OIDC_ISSUER}"))
        .arg(format!(
            "--certificate-identity-regexp={COSIGN_IDENTITY_REGEXP}"
        ))
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
        .header("User-Agent", "cfgd-self-update")
        .call()
        .map_err(|e| UpgradeError::DownloadFailed {
            message: format!("{}", e),
        })?;

    // Determine content length for progress tracking
    let content_length: Option<u64> = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());

    // Stream directly to a temp file (avoids buffering entire binary in memory)
    let parent = dest.parent().unwrap_or(std::path::Path::new("."));
    let mut tmp =
        tempfile::NamedTempFile::new_in(parent).map_err(|e| UpgradeError::DownloadFailed {
            message: format!("create temp file: {}", e),
        })?;

    const MAX_DOWNLOAD_SIZE: u64 = 256 * 1024 * 1024;
    let mut reader = response.into_body().into_reader().take(MAX_DOWNLOAD_SIZE);

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
            message: format!("rename to {}: {}", dest.posix(), e.error),
        })?;

    Ok(())
}

/// Compute the SHA256 hex digest of a file.
fn sha256_file(path: &Path) -> std::result::Result<String, UpgradeError> {
    let bytes = fs::read(path).map_err(|e| UpgradeError::DownloadFailed {
        message: format!("read {}: {}", path.posix(), e),
    })?;
    Ok(crate::sha256_hex(&bytes))
}

/// Verify that the archive at `archive_path` matches the SHA256 published in
/// its per-artifact checksum file (anodizer's split `{{ .Artifact }}.sha256`,
/// which holds the bare hash of the single archive it covers). An optional
/// trailing filename column (`<hash>  <file>`) is tolerated so a
/// combined-style single line verifies too.
///
/// Two error branches, distinct on the wire so operators can tell them apart
/// in incident triage:
/// * `ChecksumsEmpty` — the checksum file was empty / whitespace-only, or its
///   first token is not a 64-char lowercase-hex SHA256 (truncation, wrong file
///   served, or a CDN error page delivered in place of the `.sha256`).
/// * `ChecksumMismatch` — a well-formed hash was present but the local SHA
///   differs (genuine corruption or interception).
///
/// `ChecksumMissing` is surfaced one layer up (in `download_and_install_to`)
/// when no `<archive>.sha256` asset is attached to the release at all.
///
/// Pure helper — split out so the branches are testable without downloading
/// anything.
fn verify_archive_checksum(
    archive_path: &Path,
    checksum_body: &str,
    asset_name: &str,
) -> std::result::Result<(), UpgradeError> {
    let expected = checksum_body
        .split_whitespace()
        .next()
        .ok_or(UpgradeError::ChecksumsEmpty)?
        .to_lowercase();
    // Reject anything that isn't a bare SHA256 hex digest before comparing.
    // A CDN serving an HTML error page as the `.sha256` would otherwise fall
    // through to a confusing ChecksumMismatch; ChecksumsEmpty triages cleanly.
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(UpgradeError::ChecksumsEmpty);
    }
    let actual = sha256_file(archive_path)?;
    if actual != expected {
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
/// either missing cosign artifact (bundle or local CLI) blocks the
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

    // Download and verify the per-artifact checksum (`<archive>.sha256`).
    let verification_mode = if let Some(checksum_asset) = find_checksum_asset(release, &asset.name)
    {
        let checksums_path = tmp_dir.path().join(&checksum_asset.name);
        download_to_file(&checksum_asset.download_url, &checksums_path, printer)?;

        // Best-effort keyless cosign verification of the per-artifact
        // `.sha256` file. Bounds publisher-compromise risk: a malicious
        // release uploader cannot mint a Fulcio-backed signature whose
        // identity matches the pinned signing-workflow regexp over a
        // tampered `.sha256`. When `require_cosign` is true, either skip
        // condition (no bundle, no cosign CLI) surfaces as Err here instead
        // of a silent fallback to SHA256-only.
        let mode = verify_cosign_bundle(
            &checksums_path,
            &checksum_asset.name,
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

/// Outcome of [`install_release`]: the underlying [`InstallReport`] plus
/// whether a running daemon was terminated so the service manager restarts it
/// on the new binary.
#[derive(Debug, Clone)]
pub struct AppliedUpdate {
    pub report: InstallReport,
    pub daemon_restarted: bool,
    /// Outcome of the user-scope skill ride-along run as part of this apply.
    pub skill_refresh: RideAlongOutcome,
}

/// Apply an available update: download + verify + install the resolved `asset`,
/// then run the post-install invariant tail — invalidate the version cache so
/// the next check re-queries, and restart a running daemon onto the new binary.
///
/// This is the single owner of the install-then-cache-then-restart ordering;
/// every apply site (CLI `upgrade`, CLI startup check, daemon auto-update)
/// calls it so the invariant cannot drift. Callers resolve their own `asset`
/// (so each keeps its distinct no-asset error/presentation) and supply only
/// their own success/failure surface around the returned [`AppliedUpdate`].
///
/// `cfg` carries the effective [`UpdateConfig`] so the **ride-along** skill
/// refresh ([spec §9] rule 2) runs here, in the single apply owner: every apply
/// site inherits it, making a separate skill prompt after a binary upgrade
/// unrepresentable. The refresh touches **user-scope skills only** and only
/// those already installed — never a tracked project file, never a new kind.
pub fn install_release(
    release: &ReleaseInfo,
    asset: &ReleaseAsset,
    require_cosign: bool,
    cfg: &crate::config::UpdateConfig,
    printer: Option<&Printer>,
) -> Result<AppliedUpdate> {
    let report = download_and_install(release, asset, require_cosign, printer)?;
    // Ordering invariant: invalidate the cache before restarting, so a daemon
    // that comes back up does not read a stale "update available" entry for the
    // version it just installed.
    invalidate_cache();
    // Ride-along: refresh already-present user-scope skills as part of this same
    // applied upgrade (no second prompt). Gated by the effective skills policy;
    // project scope is never touched. Best-effort — a refresh failure must not
    // unwind a binary upgrade that already succeeded.
    let skill_refresh = refresh_user_scope_skills(cfg);
    let daemon_restarted = restart_daemon_if_running();
    Ok(AppliedUpdate {
        report,
        daemon_restarted,
        skill_refresh,
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
            message: format!("create temp file in {}: {}", target_dir.posix(), e),
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
            message: format!("rename {} -> {}: {}", target.posix(), old.posix(), e),
        })?;
    }
    // Copy new binary into place
    fs::copy(source, target).map_err(|e| UpgradeError::InstallFailed {
        message: format!("copy {} -> {}: {}", source.posix(), target.posix(), e),
    })?;
    Ok(())
}

/// Extract a .tar.gz archive to a directory.
#[cfg(unix)]
fn extract_tarball(archive: &Path, dest: &Path) -> std::result::Result<(), UpgradeError> {
    let file = fs::File::open(archive).map_err(|e| UpgradeError::InstallFailed {
        message: format!("open archive {}: {}", archive.posix(), e),
    })?;

    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);

    fs::create_dir_all(dest).map_err(|e| UpgradeError::InstallFailed {
        message: format!("create dest {}: {}", dest.posix(), e),
    })?;

    // The tar crate rejects `..` and absolute paths by default, but symlinks
    // can still point outside `dest`. Canonicalize and iterate entries, skipping
    // symlinks/hardlinks and unpacking each into the canonical dest.
    let canonical_dest = dest
        .canonicalize()
        .map_err(|e| UpgradeError::InstallFailed {
            message: format!("canonicalize dest {}: {}", dest.posix(), e),
        })?;

    for entry in tar.entries().map_err(|e| UpgradeError::InstallFailed {
        message: format!("iterate archive entries: {}", e),
    })? {
        let mut entry = entry.map_err(|e| UpgradeError::InstallFailed {
            message: format!("read archive entry: {}", e),
        })?;

        if entry.header().entry_type().is_symlink() || entry.header().entry_type().is_hard_link() {
            let path = entry.path().unwrap_or_default();
            tracing::warn!(path = %path.posix(), "skipping symlink/hardlink in upgrade tarball");
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
        message: format!("open archive {}: {}", archive.posix(), e),
    })?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| UpgradeError::InstallFailed {
        message: format!("read zip {}: {}", archive.posix(), e),
    })?;
    zip.extract(dest).map_err(|e| UpgradeError::InstallFailed {
        message: format!("extract zip: {}", e),
    })?;
    Ok(())
}

/// Check if the daemon is running and restart it.
/// Returns true if the daemon was restarted, false if it wasn't running.
pub fn restart_daemon_if_running() -> bool {
    // No CLI runtime override available in the self-upgrade path; env/default.
    let status = match crate::daemon::query_daemon_status(None, crate::Scope::User) {
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
///
/// `channel` selects which release stream to track on a cache miss (see
/// [`check_latest`]).
pub fn check_with_cache(
    repo: Option<&str>,
    channel: Option<&str>,
    printer: Option<&Printer>,
) -> Result<UpdateCheck> {
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
    let check = check_latest(Some(repo), channel, printer)?;

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
///
/// `channel` selects which release stream to track: `None`, `Some("stable")`,
/// or any unrecognized value tracks stable releases (`releases/latest`, which
/// excludes prereleases); `Some("prerelease")` tracks the newest release
/// including prereleases. Matching is case-insensitive.
pub fn check_latest(
    repo: Option<&str>,
    channel: Option<&str>,
    printer: Option<&Printer>,
) -> Result<UpdateCheck> {
    let repo = repo.unwrap_or(DEFAULT_REPO);
    let current = current_version()?;
    let release = fetch_release_for_channel(&github_api_base(), repo, channel, printer)?;
    let update_available = release.version > current;

    Ok(UpdateCheck {
        current,
        latest: release.version.clone(),
        update_available,
        release: Some(release),
    })
}

fn cache_dir() -> Option<PathBuf> {
    // Share the crate-wide cache root so the update-check timestamp honors
    // `CFGD_CACHE_DIR` (and systemd's `$CACHE_DIRECTORY`, and the test-home
    // override) exactly like the source and module caches. A headless or
    // sandboxed run that redirects the cache via env would otherwise fall
    // through to a `ProjectDirs` lookup that can fail to resolve a home,
    // surfacing a spurious "cannot determine cache directory" warning.
    crate::default_cache_dir_for(crate::Scope::User).ok()
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

/// Unix-seconds timestamp of the last recorded version check, read from the
/// persisted version cache. `None` when no check has ever run (no cache file).
///
/// This is the `last_checked` input to [`run_update_check`]'s interval gate,
/// shared by the CLI startup hook and the daemon so both gate against one
/// persisted timestamp rather than each tracking its own.
pub fn last_checked_secs() -> Option<u64> {
    read_version_cache().map(|c| c.checked_at_secs)
}

/// Record that a version check ran at `now` (Unix seconds), updating only the
/// timestamp on the persisted cache. Best-effort: a write failure is logged and
/// swallowed so a non-writable cache dir never fails a normal command.
///
/// Preserves the cached version strings when a prior cache exists; otherwise it
/// stamps the timestamp against the running version with empty latest fields
/// (which a subsequent real check overwrites via [`check_with_cache`]).
pub fn record_check_at(now: u64) {
    let cache = match read_version_cache() {
        Some(mut c) => {
            c.checked_at_secs = now;
            c
        }
        None => VersionCache {
            checked_at_secs: now,
            latest_tag: String::new(),
            latest_version: current_version().map(|v| v.to_string()).unwrap_or_default(),
            current_version: env!("CARGO_PKG_VERSION").to_string(),
        },
    };
    if let Err(e) = write_version_cache(&cache) {
        tracing::warn!(error = %e, "failed to record update-check timestamp");
    }
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
