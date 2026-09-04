//! Version-query helpers used by `SimpleManager`.
//!
//! `query_version_*` functions answer "what version is available?" by shelling
//! out to the underlying package manager. `list_*_with_versions` enumerate
//! installed packages with versions for `installed_packages_with_versions`.
//! `*_aliases` map canonical package names to their distro-specific aliases.
//!
//! Every shell-out IN THIS MODULE routes through `cfgd_core::tool_cmd(env_var,
//! default)` so the `CFGD_*_BIN` test-shim seams can drive the binaries without
//! a real package manager installed. `query_version_info` dispatches the seam
//! per `manager` arg (pacman / dnf / yum / zypper) so each manager has an
//! independent override knob. The install/uninstall/list paths in
//! `packages::simple::mod` still shell out via raw `Command::new` and are not
//! yet seamed.

use cfgd_core::errors::Result;
use cfgd_core::tool_cmd;

use super::shared::{run_pkg_cmd, run_pkg_query};

pub(super) const APT_CACHE_BIN_ENV: &str = "CFGD_APT_CACHE_BIN";
pub(super) const APK_BIN_ENV: &str = "CFGD_APK_BIN";
pub(super) const PKG_BIN_ENV: &str = "CFGD_PKG_BIN";
pub(super) const PACMAN_BIN_ENV: &str = "CFGD_PACMAN_BIN";
pub(super) const DNF_BIN_ENV: &str = "CFGD_DNF_BIN";
pub(super) const YUM_BIN_ENV: &str = "CFGD_YUM_BIN";
pub(super) const ZYPPER_BIN_ENV: &str = "CFGD_ZYPPER_BIN";
pub(super) const DPKG_QUERY_BIN_ENV: &str = "CFGD_DPKG_QUERY_BIN";
pub(super) const RPM_BIN_ENV: &str = "CFGD_RPM_BIN";

/// Map an `info`-style manager name to its env-var seam. Unknown managers
/// debug-assert (catches typos in tests) and log a warning, then fall through
/// to an empty seam so production keeps working via PATH lookup of `default`.
fn info_bin_env(manager: &str) -> &'static str {
    match manager {
        "pacman" => PACMAN_BIN_ENV,
        "dnf" => DNF_BIN_ENV,
        "yum" => YUM_BIN_ENV,
        "zypper" => ZYPPER_BIN_ENV,
        other => {
            debug_assert!(
                false,
                "query_version_info called with unknown manager {other:?}; CFGD_*_BIN seam silently bypassed"
            );
            // tracing-ok: an internal seam gap beside its own debug_assert; nothing user-facing
            tracing::warn!(
                manager = other,
                "query_version_info: no CFGD_*_BIN seam registered; falling through to PATH"
            );
            ""
        }
    }
}

/// Query version via `<cmd> info <pkg>` and parse "Version:" field.
/// Used by dnf, yum, pacman (-Si), zypper.
pub(super) fn query_version_info(manager: &str, package: &str) -> Result<Option<String>> {
    let (cmd, args): (&str, &[&str]) = match manager {
        "pacman" => ("pacman", &["-Si"]),
        _ => (manager, &["info"]),
    };
    let output = run_pkg_query(
        manager,
        tool_cmd(info_bin_env(manager), cmd).args(args).arg(package),
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Version")
            && let Some(version) = rest.trim_start().strip_prefix(':')
        {
            return Ok(Some(version.trim().to_string()));
        }
    }
    Ok(None)
}

pub(super) fn query_version_apt(manager: &str, package: &str) -> Result<Option<String>> {
    let output = run_pkg_query(
        manager,
        tool_cmd(APT_CACHE_BIN_ENV, "apt-cache").args(["policy", package]),
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Candidate:") {
            let version = rest.trim();
            if version == "(none)" {
                return Ok(None);
            }
            // apt versions often have epoch:version-revision, strip to just version
            let version = version
                .split_once(':')
                .map_or(version, |(_, v)| v)
                .split_once('-')
                .map_or_else(
                    || version.split_once(':').map_or(version, |(_, v)| v),
                    |(v, _)| v,
                );
            return Ok(Some(version.to_string()));
        }
    }
    Ok(None)
}

pub(super) fn query_version_apk(manager: &str, package: &str) -> Result<Option<String>> {
    let output = run_pkg_query(
        manager,
        tool_cmd(APK_BIN_ENV, "apk").args(["policy", package]),
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // apk policy <pkg> output:
    //   <pkg> policy:        (column 0)
    //     <version>:          (indented)
    //       <repo-url>        (deeper indent)
    // Return the first indented line whose trimmed/colon-stripped value starts with a digit.
    for line in stdout.lines() {
        if !line.starts_with(char::is_whitespace) {
            continue;
        }
        let trimmed = line.trim().trim_end_matches(':');
        if trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return Ok(Some(trimmed.to_string()));
        }
    }
    Ok(None)
}

pub(super) fn query_version_pkg(manager: &str, package: &str) -> Result<Option<String>> {
    // `pkg rquery` reads the remote catalogue database that `pkg install`
    // resolves against. `pkg search` consults a *separate* search index that a
    // stock host never syncs (only `pkg update` populates the catalogue, not the
    // search index), so `pkg search -e` returns nothing even when 30k+ packages
    // are installable — silently starving version-constraint resolution. Query
    // `%n<TAB>%v` and match the name exactly so a pattern that expands to several
    // packages can never yield a sibling's version.
    let output = run_pkg_query(
        manager,
        tool_cmd(PKG_BIN_ENV, "pkg").args(["rquery", "%n\t%v", package]),
    )?;
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let mut parts = line.splitn(2, '\t');
        if parts.next() == Some(package)
            && let Some(version) = parts.next()
        {
            let version = version.trim();
            if !version.is_empty() {
                return Ok(Some(version.to_string()));
            }
        }
    }
    Ok(None)
}

/// Whether a FreeBSD package's `available` version satisfies a `>= min_version`
/// floor, using pkg's OWN comparator. `pkg version -t <a> <b>` prints a single
/// `<`, `=`, or `>` describing how `<a>` relates to `<b>`. FreeBSD versions carry
/// PORTEPOCH (`,N`) and PORTREVISION (`_N`) and are not semver, so a semver parse
/// would mis-order them (e.g. `1.2.0,1` vs `1.2.0`); deferring to `pkg version`
/// evaluates the floor exactly as `pkg install`'s own resolver would. Returns
/// `Ok(true)` when `available` is equal to or greater than the floor.
///
/// The declared floor is read through
/// [`cfgd_core::declared_floor_version`] first: pkg compares the leading `v`
/// of a `v1.2.0` declaration against a digit and answers `<`, which is this
/// manager's spelling of the perpetual re-plan the semver families strip the
/// prefix to avoid. This comparator answers for a manager whose
/// `version_comparable` is unconditionally true, so a mis-read floor here
/// cannot degrade to a check error — it invents drift instead.
pub(super) fn pkg_version_meets_minimum(available: &str, min_version: &str) -> Result<bool> {
    let output = run_pkg_query(
        "pkg",
        tool_cmd(PKG_BIN_ENV, "pkg").args([
            "version",
            "-t",
            available,
            cfgd_core::declared_floor_version(min_version),
        ]),
    )?;
    if !output.status.success() {
        return Ok(false);
    }
    // `pkg version -t` emits exactly one of `<`, `=`, `>` on stdout.
    Ok(matches!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "=" | ">"
    ))
}

/// The UPSTREAM part of a distro package version — the part a `minVersion`
/// declaration is written against.
///
/// A distro version is `[<epoch>:]<upstream>[-<revision>]`: `dpkg-query`
/// prints `1:2.34-0ubuntu3.4`, pacman `1.2.3-2`, apk `3.0.0-r0`. Loose semver
/// reads the revision as a PRERELEASE, which never satisfies a comparator that
/// has none, and an epoch as an unparseable version — so every pinned apt
/// package would be permanent drift. The packaging fields say nothing about
/// which upstream release is installed, so the floor is judged on the upstream
/// part alone.
///
/// Deliberately NOT a change to `parse_loose_version`, which is shared with the
/// self-upgrade and release-tag paths where `-rc1` ordering is load-bearing.
pub(super) fn distro_upstream_version(raw: &str) -> &str {
    distro_epoch_and_upstream(raw).1
}

/// Split a distro version into its epoch (default `0`, the same default
/// `dpkg`'s own comparator uses) and upstream part,
/// dropping the trailing `-<revision>` the same way [`distro_upstream_version`]
/// always has. One split so the epoch and the upstream part can never read
/// two different revisions of the same string.
fn distro_epoch_and_upstream(raw: &str) -> (u64, &str) {
    let (epoch, after_epoch) = match raw.split_once(':') {
        Some((epoch, rest)) if !epoch.is_empty() && epoch.bytes().all(|b| b.is_ascii_digit()) => {
            (epoch.parse().unwrap_or(0), rest)
        }
        _ => (0, raw),
    };
    let upstream = after_epoch
        .rsplit_once('-')
        .map_or(after_epoch, |(upstream, _)| upstream);
    (epoch, upstream)
}

/// Whether a distro-family manager's `available` version clears a `min_version`
/// floor.
///
/// The epoch compares FIRST, exactly as `dpkg`/`rpm` order it: a higher epoch
/// always outranks a lower one, whatever the upstream numbers say, because the
/// epoch exists precisely to let a packager declare "this release supersedes
/// every prior numbering scheme". Comparing [`distro_upstream_version`] alone
/// (the pre-fix behavior) judged `1:9.0` as clearing a `2:1.0` floor — 9.0 >=
/// 1.0 — when the floor's epoch of 2 means nothing below it clears, whatever
/// the upstream part reads.
pub(super) fn distro_version_meets_minimum(available: &str, min_version: &str) -> bool {
    let (available_epoch, available_upstream) = distro_epoch_and_upstream(available);
    let (floor_epoch, floor_upstream) = distro_epoch_and_upstream(min_version);
    if available_epoch != floor_epoch {
        return available_epoch > floor_epoch;
    }
    cfgd_core::version_meets_floor(available_upstream, floor_upstream)
}

/// Whether a distro-family version can be compared at all: a listing carrying
/// a date stamp or a git description has no upstream version to judge, which is
/// a check that could not run rather than a floor that was missed.
pub(super) fn distro_comparable(raw: &str) -> bool {
    cfgd_core::parse_loose_version(distro_upstream_version(raw)).is_some()
}

/// The UPSTREAM part of a Homebrew version — the part a `minVersion`
/// declaration is written against.
///
/// Homebrew states two things after the upstream release, and neither orders
/// releases: a formula carries the tap's own packaging revision as `_<n>`
/// (`neovim 0.12.5_1`), and a cask carries the vendor's build after a comma
/// (`1.2.3,4567`). Loose semver reads the revision as a PRERELEASE — which
/// never satisfies a comparator that has none — and the build as unparseable,
/// so before this fold every brew package with a declared floor both reported
/// an erroring check and re-planned as an install on a converged machine.
///
/// One fold serves both managers: the two grammars use disjoint separators, so
/// a formula version is untouched by the comma arm and a cask by the revision
/// one. Deliberately NOT a change to `parse_loose_version`, which is shared
/// with the self-upgrade and release-tag paths where `-rc1` ordering is
/// load-bearing.
pub(super) fn brew_upstream_version(raw: &str) -> &str {
    let before_build = raw.split_once(',').map_or(raw, |(version, _)| version);
    match before_build.rsplit_once('_') {
        Some((upstream, revision))
            if !upstream.is_empty()
                && !revision.is_empty()
                && revision.bytes().all(|b| b.is_ascii_digit()) =>
        {
            upstream
        }
        _ => before_build,
    }
}

/// Whether a Homebrew version clears a `min_version` floor, comparing
/// [`brew_upstream_version`] of each.
pub(super) fn brew_version_meets_minimum(available: &str, min_version: &str) -> bool {
    cfgd_core::version_meets_floor(
        brew_upstream_version(available),
        brew_upstream_version(min_version),
    )
}

/// Whether a Homebrew version can be compared at all: a `HEAD-<sha>` build or
/// a cask tracking `latest` states no upstream release to judge, which is a
/// check that could not run rather than a floor that was missed.
pub(super) fn brew_comparable(raw: &str) -> bool {
    cfgd_core::parse_loose_version(brew_upstream_version(raw)).is_some()
}

/// Parse a Windows-style four-part version (`133.0.6943.98`) into up to four
/// numeric components, padding missing trailing components with 0. `None` for
/// anything a component fails to parse as a plain integer — a range
/// expression (`>=1.2`) included.
///
/// winget and chocolatey package identifiers carry a fourth build component
/// (`<major>.<minor>.<build>.<revision>`) that semver has no field for and
/// refuses outright, so a `minVersion` against one such listing was a
/// permanent check error under the shared semver comparator.
fn fourpart_components(raw: &str) -> Option<[u64; 4]> {
    let parts: Vec<&str> = raw.trim().trim_start_matches('v').split('.').collect();
    if parts.is_empty() || parts.len() > 4 {
        return None;
    }
    let mut out = [0u64; 4];
    for (slot, part) in out.iter_mut().zip(parts.iter()) {
        *slot = part.parse().ok()?;
    }
    Some(out)
}

/// Whether `raw` parses as a four-part (or shorter) numeric version.
pub(super) fn fourpart_comparable(raw: &str) -> bool {
    fourpart_components(raw).is_some()
}

/// Whether `available` clears a `min_version` floor under the four-part
/// numeric ordering winget and chocolatey package versions use.
pub(super) fn fourpart_version_meets_minimum(available: &str, min_version: &str) -> bool {
    match (
        fourpart_components(available),
        fourpart_components(min_version),
    ) {
        (Some(a), Some(b)) => a >= b,
        _ => false,
    }
}

/// Parse `dpkg-query -W -f='${Package}\t${Version}\n'` output into PackageInfo.
/// Parse tab-separated `NAME\tVERSION` output into PackageInfo.
/// Used by both apt (dpkg-query) and rpm (rpm -qa --queryformat) parsers.
pub(super) fn parse_tab_separated_versions(stdout: &str) -> Vec<cfgd_core::providers::PackageInfo> {
    stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let name = parts.next()?.trim();
            let version = parts
                .next()
                .unwrap_or(cfgd_core::providers::UNKNOWN_PACKAGE_VERSION)
                .trim();
            if name.is_empty() {
                return None;
            }
            Some(cfgd_core::providers::PackageInfo {
                name: name.to_string(),
                version: if version.is_empty() {
                    cfgd_core::providers::UNKNOWN_PACKAGE_VERSION.to_string()
                } else {
                    version.to_string()
                },
            })
        })
        .collect()
}

pub(super) fn parse_apt_versions(stdout: &str) -> Vec<cfgd_core::providers::PackageInfo> {
    parse_tab_separated_versions(stdout)
}

pub(super) fn parse_rpm_versions(stdout: &str) -> Vec<cfgd_core::providers::PackageInfo> {
    parse_tab_separated_versions(stdout)
}

pub(super) fn list_apt_with_versions(
    manager: &str,
) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
    let output = run_pkg_cmd(
        manager,
        tool_cmd(DPKG_QUERY_BIN_ENV, "dpkg-query").args(["-W", "-f=${Package}\t${Version}\n"]),
        "list",
    )?;
    Ok(parse_apt_versions(&String::from_utf8_lossy(&output.stdout)))
}

pub(super) fn list_dnf_with_versions(
    manager: &str,
) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
    let output = run_pkg_cmd(
        manager,
        tool_cmd(RPM_BIN_ENV, "rpm").args([
            "--query",
            "--all",
            "--queryformat",
            "%{NAME}\t%{VERSION}\n",
        ]),
        "list",
    )?;
    Ok(parse_rpm_versions(&String::from_utf8_lossy(&output.stdout)))
}

pub(super) fn apt_aliases(canonical_name: &str) -> Vec<String> {
    match canonical_name {
        "fd" => vec!["fd-find".to_string()],
        "rg" => vec!["ripgrep".to_string()],
        "bat" => vec!["batcat".to_string()],
        "nvim" => vec!["neovim".to_string()],
        _ => vec![],
    }
}

pub(super) fn dnf_aliases(canonical_name: &str) -> Vec<String> {
    match canonical_name {
        "fd" => vec!["fd-find".to_string()],
        "nvim" => vec!["neovim".to_string()],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests;
