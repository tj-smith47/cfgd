//! Nix package manager (`nix profile` and `nix-env`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::errors::Result;
use cfgd_core::providers::{BootstrapPlan, PackageManager};

use super::shared::{
    bootstrap_via_shell_script, install_batch_then_per_package, partition_already_installed,
    resolve_tool_with_fallbacks, run_pkg_cmd, run_pkg_cmd_live, run_pkg_query,
    strip_version_suffix, tool_cmd_with_resolver, upgrade_each,
};

pub struct NixManager;

pub(super) fn find_nix() -> Option<PathBuf> {
    resolve_tool_with_fallbacks("nix", &[])
}

pub(super) fn find_nix_env() -> Option<PathBuf> {
    resolve_tool_with_fallbacks("nix-env", &[])
}

pub(super) fn nix_available() -> bool {
    find_nix().is_some()
}

pub(super) fn nix_env_available() -> bool {
    find_nix_env().is_some()
}

pub(super) fn nix_cmd() -> Command {
    tool_cmd_with_resolver("nix", find_nix)
}

pub(super) fn nix_env_cmd() -> Command {
    tool_cmd_with_resolver("nix-env", find_nix_env)
}

// Single source for the multi-user installer's profile bin dir, so
// `bootstrap_plan`'s declaration and `path_dirs`'s recording can never
// drift apart.
const NIX_PROFILE_BIN_DIR: &str = "/nix/var/nix/profiles/default/bin";

impl PackageManager for NixManager {
    fn name(&self) -> &str {
        "nix"
    }

    fn tool_version(&self) -> Option<String> {
        super::shared::tool_version_from(nix_cmd().arg("--version"))
    }

    fn is_available(&self) -> bool {
        nix_env_available() || nix_available()
    }

    fn bootstrap_plan_given(&self, _delivered: &dyn Fn(&str) -> bool) -> Option<BootstrapPlan> {
        // The multi-user (`--daemon`) install puts the nix binaries in the
        // default profile; a per-user profile only appears once something is
        // installed into it.
        Some(
            BootstrapPlan::new("nix installer")
                .requiring(["curl"])
                .creating([NIX_PROFILE_BIN_DIR]),
        )
    }

    fn path_dirs(&self, _cx: &cfgd_core::providers::PackageContext<'_>) -> Vec<String> {
        vec![cfgd_core::to_posix_string(NIX_PROFILE_BIN_DIR)]
    }

    // bootstrap-arm-ok: the nixos.org installer is nix's only route
    fn bootstrap(&self, cx: &cfgd_core::providers::PackageContext<'_>) -> Result<()> {
        bootstrap_via_shell_script(
            cx,
            "nix",
            "Installing Nix",
            "curl -L https://nixos.org/nix/install | sh -s -- --daemon",
        )
    }

    fn installed_packages(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<HashSet<String>> {
        // Prefer `nix profile list --json`: the plain-text format changed to
        // a multi-line `Key: value` block in nix 2.20+, which no line-oriented
        // parser can reliably read; the JSON shape is stable across versions.
        if nix_available() {
            let output = run_pkg_query("nix", nix_cmd().args(["profile", "list", "--json"]))?;

            if output.status.success() {
                return Ok(parse_nix_profile_list_json(&String::from_utf8_lossy(
                    &output.stdout,
                )));
            }
        }

        // Fallback: nix-env -q
        let output = run_pkg_cmd(
            "nix",
            nix_env_cmd().args(["-q", "--no-name", "--attr-path"]),
            "list",
        )?;
        Ok(parse_nix_env_query(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }

    /// `nix profile list --json` carries no explicit version field — the
    /// version is the trailing segment of each element's store path
    /// (`/nix/store/<hash>-ripgrep-14.1.0`). The legacy `nix-env` profile
    /// (older installs with no `nix profile` subcommand) states no version
    /// at all, so it falls to the trait default.
    fn installed_packages_with_versions(
        &self,
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        if nix_available() {
            let output = run_pkg_query("nix", nix_cmd().args(["profile", "list", "--json"]))?;
            if output.status.success() {
                return Ok(parse_nix_profile_list_versions(&String::from_utf8_lossy(
                    &output.stdout,
                )));
            }
        }
        Ok(self
            .installed_packages(cx)?
            .into_iter()
            .map(|name| cfgd_core::providers::PackageInfo {
                name,
                version: cfgd_core::providers::UNKNOWN_PACKAGE_VERSION.into(),
            })
            .collect())
    }

    fn install(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        // Both install forms take many packages in one invocation (`nix profile
        // install [option...] installables...`; `nix-env -iA args...`), and the
        // nix-vs-nix-env choice is a property of the host, not of a package —
        // so it is decided once, outside the batch closure.
        let (held, fresh) = partition_already_installed(self, packages, cx);
        if nix_available() {
            install_batch_then_per_package(cx, "nix", &fresh, |pkgs| {
                let mut cmd = nix_cmd();
                cmd.args(["profile", "install"]);
                cmd.args(pkgs.iter().map(|p| format!("nixpkgs#{}", p)));
                cmd
            })?;
            // `nix profile install` no-ops on an element already held; raising
            // it takes `nix profile upgrade`.
            upgrade_each(cx, "nix", &held, "nix profile upgrade", |pkg| {
                let mut cmd = nix_cmd();
                cmd.args(["profile", "upgrade", &format!("nixpkgs#{}", pkg)]);
                Some(cmd)
            })?;
        } else {
            install_batch_then_per_package(cx, "nix", &fresh, |pkgs| {
                let mut cmd = nix_env_cmd();
                cmd.arg("-iA");
                cmd.args(pkgs.iter().map(|p| format!("nixpkgs.{}", p)));
                cmd
            })?;
            // The legacy `nix-env -iA` no-ops on a package already held;
            // raising it takes `nix-env -u`.
            upgrade_each(cx, "nix", &held, "nix-env -u", |pkg| {
                let mut cmd = nix_env_cmd();
                cmd.args(["-u", pkg]);
                Some(cmd)
            })?;
        }
        Ok(())
    }

    fn uninstall(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        for pkg in packages {
            if nix_available() {
                // nix 2.20+ removes by profile element NAME, not by flake
                // selector: `nix profile remove nixpkgs#<pkg>` matches nothing
                // and exits 0 (silent no-op). cfgd installs via
                // `nix profile install nixpkgs#<pkg>`, which names the element
                // `<pkg>` (final attrPath segment), so the package string equals
                // the element name.
                let label = format!("nix profile remove {}", pkg);
                run_pkg_cmd_live(
                    cx,
                    "nix",
                    nix_cmd().args(["profile", "remove", pkg]),
                    &label,
                    "uninstall",
                )?;
            } else {
                let label = format!("nix-env -e {}", pkg);
                run_pkg_cmd_live(
                    cx,
                    "nix",
                    nix_env_cmd().args(["-e", pkg]),
                    &label,
                    "uninstall",
                )?;
            }
        }
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // nix search nixpkgs <pkg> --json → parse version from first matching result
        if nix_available() {
            let output = run_pkg_query(
                "nix",
                nix_cmd().args(["search", "nixpkgs", package, "--json"]),
            )?;
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if let Some(v) = parse_nix_search_version(&stdout) {
                    return Ok(Some(v));
                }
            }
        }
        Ok(None)
    }
}

/// Parse `nix profile list --json` stdout into a `HashSet` of profile element
/// names. Handles both JSON shapes nix has emitted: the modern (`version` 3)
/// object form where `elements` is keyed by element name, and the legacy
/// (`version` 1/2) array form where each entry is named from its `attrPath`'s
/// final `.`-segment (falling back to the flake fragment after `#` in
/// `originalUrl`/`url`). Entries that cannot be named are dropped. Returns an
/// empty set on missing/empty/malformed JSON.
pub(super) fn parse_nix_profile_list_json(stdout: &str) -> HashSet<String> {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return HashSet::new();
    };
    let Some(elements) = parsed.get("elements") else {
        return HashSet::new();
    };

    if let Some(obj) = elements.as_object() {
        return obj.keys().cloned().collect();
    }

    if let Some(arr) = elements.as_array() {
        return arr.iter().filter_map(element_name_from_value).collect();
    }

    HashSet::new()
}

/// Parse `nix profile list --json` stdout into `(name, version)` pairs,
/// reading the version off the FIRST store path's trailing `-<version>`
/// segment (after stripping the store's leading `<hash>-`, then the
/// element's own name if the store path repeats it). An element naming no
/// readable store path lists as [`UNKNOWN_PACKAGE_VERSION`](cfgd_core::providers::UNKNOWN_PACKAGE_VERSION).
pub(super) fn parse_nix_profile_list_versions(
    stdout: &str,
) -> Vec<cfgd_core::providers::PackageInfo> {
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return Vec::new();
    };
    let Some(elements) = parsed.get("elements").and_then(|v| v.as_object()) else {
        return Vec::new();
    };

    elements
        .iter()
        .map(|(name, entry)| {
            let version = entry
                .get("storePaths")
                .and_then(|v| v.as_array())
                .and_then(|paths| paths.first())
                .and_then(|v| v.as_str())
                .and_then(|path| nix_store_path_version(path, name))
                .unwrap_or_else(|| cfgd_core::providers::UNKNOWN_PACKAGE_VERSION.to_string());
            cfgd_core::providers::PackageInfo {
                name: name.clone(),
                version,
            }
        })
        .collect()
}

/// Extract the version segment from a nix store path basename
/// (`/nix/store/<32-char-hash>-<name>-<version>` → `<version>`): strip the
/// store hash prefix up to its first `-` (the hash itself never contains
/// one), then strip a leading `<name>-` if the remainder still carries it.
fn nix_store_path_version(store_path: &str, name: &str) -> Option<String> {
    let basename = store_path.rsplit('/').next()?;
    let after_hash = basename.split_once('-')?.1;
    let version = after_hash
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('-'))
        .unwrap_or(after_hash);
    if version.is_empty() {
        None
    } else {
        Some(version.to_string())
    }
}

/// Derive a profile element name from a legacy (array-shape) `elements` entry.
/// Prefers the final `.`-segment of `attrPath` (e.g.
/// `legacyPackages.x86_64-linux.hello` → `hello`); falls back to the flake
/// fragment after `#` in `originalUrl` then `url`. Returns `None` when neither
/// yields a non-empty name.
fn element_name_from_value(value: &serde_json::Value) -> Option<String> {
    if let Some(attr) = value.get("attrPath").and_then(|v| v.as_str())
        && let Some(last) = attr.rsplit('.').next()
        && !last.is_empty()
    {
        return Some(last.to_string());
    }
    for key in ["originalUrl", "url"] {
        if let Some(url) = value.get(key).and_then(|v| v.as_str())
            && let Some((_, frag)) = url.rsplit_once('#')
            && !frag.is_empty()
        {
            return Some(frag.to_string());
        }
    }
    None
}

/// Parse `nix-env -q --no-name --attr-path` stdout into a `HashSet` of
/// package names. Each line is `name-version`; the trailing version suffix
/// is stripped via `strip_version_suffix`.
pub(super) fn parse_nix_env_query(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| strip_version_suffix(l.trim()))
        .collect()
}

/// Parse version from `nix search nixpkgs <pkg> --json` output.
/// JSON format: `{"nixpkgs.pkg": {"version": "1.2.3", ...}, ...}`
/// Returns the version of the first result.
pub(super) fn parse_nix_search_version(output: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(output).ok()?;
    let obj = parsed.as_object()?;
    for value in obj.values() {
        if let Some(version) = value.get("version").and_then(|v| v.as_str())
            && !version.is_empty()
        {
            return Some(version.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use cfgd_core::providers::{PackageManager, PackageManagerExt};

    use super::*;

    #[test]
    fn nix_manager_name_and_traits() {
        let mgr = NixManager;
        assert_eq!(mgr.name(), "nix");
    }

    #[test]
    fn parse_nix_profile_list_versions_real_world() {
        let stdout = r#"{
            "elements": {
                "ripgrep": {
                    "active": true,
                    "attrPath": "legacyPackages.x86_64-linux.ripgrep",
                    "storePaths": ["/nix/store/9r9z5r5r5r5r5r5r5r5r5r5r5r5r5r5r-ripgrep-14.1.0"]
                }
            },
            "version": 3
        }"#;
        let versions = parse_nix_profile_list_versions(stdout);
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].name, "ripgrep");
        assert_eq!(versions[0].version, "14.1.0");
    }

    #[test]
    fn parse_nix_profile_list_versions_missing_store_paths_is_unknown() {
        let stdout = r#"{"elements": {"hello": {"active": true}}, "version": 3}"#;
        let versions = parse_nix_profile_list_versions(stdout);
        assert_eq!(versions.len(), 1);
        assert_eq!(
            versions[0].version,
            cfgd_core::providers::UNKNOWN_PACKAGE_VERSION
        );
    }

    #[test]
    fn parse_nix_profile_list_versions_invalid_json_is_empty() {
        assert!(parse_nix_profile_list_versions("not json").is_empty());
    }

    #[test]
    fn nix_store_path_version_strips_hash_and_name() {
        assert_eq!(
            nix_store_path_version(
                "/nix/store/9r9z5r5r5r5r5r5r5r5r5r5r5r5r5r5r-ripgrep-14.1.0",
                "ripgrep"
            ),
            Some("14.1.0".to_string())
        );
    }

    #[test]
    fn nix_store_path_version_name_with_internal_hyphens() {
        // A package name carrying its own hyphens (`python3.11-numpy`) still
        // strips cleanly because the known element name is stripped whole.
        assert_eq!(
            nix_store_path_version(
                "/nix/store/9r9z5r5r5r5r5r5r5r5r5r5r5r5r5r5r-python3.11-numpy-1.26.4",
                "python3.11-numpy"
            ),
            Some("1.26.4".to_string())
        );
    }

    #[test]
    fn nix_declares_no_index_to_refresh() {
        let mgr = NixManager;
        let printer = cfgd_core::test_helpers::test_printer();
        let state = cfgd_core::test_helpers::test_state();
        let cx = cfgd_core::test_helpers::test_package_context(&printer, &state);
        assert!(
            !mgr.has_index(),
            "nix packages are pinned; channels are managed separately"
        );
        mgr.refresh_index(&cx).unwrap();
    }

    #[test]
    fn parse_nix_search_version_single_result() {
        let output = r#"{"legacyPackages.x86_64-linux.ripgrep":{"pname":"ripgrep","version":"14.1.0","description":"A utility that combines the usability of The Silver Searcher with the raw speed of grep"}}"#;
        assert_eq!(parse_nix_search_version(output), Some("14.1.0".to_string()));
    }

    #[test]
    fn parse_nix_search_version_multiple_results() {
        let output = r#"{"legacyPackages.x86_64-linux.bat":{"version":"0.24.0"},"legacyPackages.x86_64-linux.bat-extras":{"version":"2024.08.24"}}"#;
        let v = parse_nix_search_version(output);
        // Returns first result — either is valid since JSON object order is unspecified
        assert!(v.is_some());
    }

    #[test]
    fn parse_nix_search_version_empty_version() {
        let output = r#"{"legacyPackages.x86_64-linux.thing":{"version":""}}"#;
        assert_eq!(parse_nix_search_version(output), None);
    }

    #[test]
    fn parse_nix_search_version_no_version_field() {
        let output = r#"{"legacyPackages.x86_64-linux.thing":{"pname":"thing"}}"#;
        assert_eq!(parse_nix_search_version(output), None);
    }

    #[test]
    fn parse_nix_search_version_invalid_json() {
        assert_eq!(parse_nix_search_version("not json"), None);
    }

    #[test]
    fn parse_nix_search_version_nested_package_key_format() {
        // Real nix search output uses deeply nested keys like legacyPackages.SYSTEM.NAME
        let output = r#"{"legacyPackages.aarch64-darwin.ripgrep":{"pname":"ripgrep","version":"14.1.0","description":"fast grep"}}"#;
        assert_eq!(
            parse_nix_search_version(output),
            Some("14.1.0".to_string()),
            "should work with aarch64-darwin platform prefix"
        );
    }

    #[test]
    fn parse_nix_search_version_empty_object() {
        let output = "{}";
        assert_eq!(parse_nix_search_version(output), None);
    }

    #[test]
    fn parse_nix_search_version_null_version() {
        let output = r#"{"legacyPackages.x86_64-linux.thing":{"version":null}}"#;
        assert_eq!(parse_nix_search_version(output), None);
    }

    #[test]
    fn parse_nix_search_version_numeric_version() {
        let output = r#"{"legacyPackages.x86_64-linux.thing":{"version":123}}"#;
        assert_eq!(parse_nix_search_version(output), None);
    }

    #[test]
    fn parse_nix_search_version_cross_platform() {
        let output = r#"{
            "legacyPackages.x86_64-linux.ripgrep": {"version": "14.1.0"},
            "legacyPackages.aarch64-linux.ripgrep": {"version": "14.1.0"},
            "legacyPackages.x86_64-darwin.ripgrep": {"version": "14.1.0"}
        }"#;
        let v = parse_nix_search_version(output);
        assert_eq!(v, Some("14.1.0".to_string()));
    }

    #[test]
    fn nix_bootstrap_plan_names_curl_and_the_default_profile_bin() {
        // Both sides read `PATH`; without the guard a concurrent test's
        // `PATH` mutation can land between them and they disagree.
        let _path = cfgd_core::test_helpers::path_env_read_guard();
        let plan = NixManager
            .bootstrap_plan()
            .expect("the cascade is unconditional");
        // Feasibility is a separate question, asked of the same plan.
        assert_eq!(
            NixManager.feasible_bootstrap_plan().is_some(),
            cfgd_core::providers::prerequisite_obtainable("curl")
        );
        // What `bootstrap` runs: the nixos.org installer in --daemon mode,
        // fetched with curl, whose binaries land in the default profile.
        assert_eq!(plan.method, "nix installer");
        assert_eq!(plan.requires, ["curl"]);
        assert_eq!(
            plan.creates_path_dirs,
            ["/nix/var/nix/profiles/default/bin"]
        );
    }

    #[test]
    fn nix_path_dirs_matches_the_bootstrap_plans_declaration() {
        let plan = NixManager
            .bootstrap_plan()
            .expect("the cascade is unconditional");
        let printer = cfgd_core::test_helpers::test_printer();
        let state = cfgd_core::test_helpers::test_state();
        let cx = cfgd_core::test_helpers::test_package_context(&printer, &state);
        let mgr: Box<dyn PackageManager> = Box::new(NixManager);
        assert_eq!(mgr.path_dirs(&cx), plan.creates_path_dirs);
    }

    #[test]
    #[serial_test::serial]
    fn nix_manager_is_available_for_either_nix_binary() {
        // The seam env vars are cleared for the whole test: with either set,
        // this asserts about whichever ToolShim ran last rather than about the
        // PATH probe.
        let _seam_nix = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_NIX_BIN");
        let _seam_nix_env = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_NIX_ENV_BIN");
        let _path_lock = cfgd_core::test_helpers::path_env_mutation_guard();
        let _dirs = cfgd_core::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
        let mgr = NixManager;

        {
            let _empty = cfgd_core::test_helpers::EnvVarGuard::set("PATH", "");
            assert!(
                !mgr.is_available(),
                "a host resolving no binaries has no nix"
            );
        }

        // A classic install ships `nix-env`, a flakes-era one may ship only
        // `nix`; each is asserted alone, so dropping either arm is caught on a
        // host that happens to carry both.
        #[cfg(unix)]
        for binary in ["nix-env", "nix"] {
            let _probe = cfgd_core::test_helpers::ProbePath::containing(&[binary]);
            assert!(
                mgr.is_available(),
                "{binary} alone must make this manager available"
            );
        }
    }

    // --- parse_nix_profile_list_json ---

    #[test]
    fn parse_nix_profile_list_json_v3_object_uses_keys() {
        // nix 2.34 (version 3): `elements` is an object keyed by element name.
        let stdout = r#"{"elements":{"hello":{"active":true,"attrPath":"legacyPackages.x86_64-linux.hello","originalUrl":"flake:nixpkgs","outputs":null,"priority":5,"storePaths":["/nix/store/x-hello-2.12.3"],"url":"github:NixOS/nixpkgs/abc?narHash=sha256-y"},"nix":{"active":true,"priority":5,"storePaths":["/nix/store/x-nix-2.34.7"]}},"version":3}"#;
        let pkgs = parse_nix_profile_list_json(stdout);
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.contains("hello"));
        assert!(pkgs.contains("nix"));
    }

    #[test]
    fn parse_nix_profile_list_json_v3_multi_package_object() {
        let stdout = r#"{"elements":{"ripgrep":{"storePaths":["/nix/store/a"]},"fd":{"storePaths":["/nix/store/b"]},"bat":{"storePaths":["/nix/store/c"]}},"version":3}"#;
        let pkgs = parse_nix_profile_list_json(stdout);
        assert_eq!(pkgs.len(), 3);
        assert!(pkgs.contains("ripgrep"));
        assert!(pkgs.contains("fd"));
        assert!(pkgs.contains("bat"));
    }

    #[test]
    fn parse_nix_profile_list_json_legacy_array_names_from_attr_path() {
        // pre-2.20 (version 1/2): `elements` is an array; derive name from the
        // final '.'-segment of attrPath.
        let stdout = r#"{"elements":[{"active":true,"attrPath":"legacyPackages.x86_64-linux.hello","originalUrl":"flake:nixpkgs","storePaths":["/nix/store/x-hello-2.12.3"],"url":"github:NixOS/nixpkgs/abc"}]}"#;
        let pkgs = parse_nix_profile_list_json(stdout);
        assert_eq!(pkgs.len(), 1);
        assert!(pkgs.contains("hello"));
    }

    #[test]
    fn parse_nix_profile_list_json_legacy_array_falls_back_to_url_fragment() {
        // No attrPath → name from the flake fragment after '#'.
        let stdout = r#"{"elements":[{"originalUrl":"flake:nixpkgs#ripgrep","storePaths":["/nix/store/x"]},{"url":"github:NixOS/nixpkgs/abc#fd","storePaths":["/nix/store/y"]}]}"#;
        let pkgs = parse_nix_profile_list_json(stdout);
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.contains("ripgrep"));
        assert!(pkgs.contains("fd"));
    }

    #[test]
    fn parse_nix_profile_list_json_legacy_array_drops_unnameable_entries() {
        // Neither attrPath nor a '#'-bearing url → entry cannot be named.
        let stdout = r#"{"elements":[{"storePaths":["/nix/store/x"]},{"attrPath":"legacyPackages.x86_64-linux.git","storePaths":["/nix/store/y"]}]}"#;
        let pkgs = parse_nix_profile_list_json(stdout);
        assert_eq!(pkgs.len(), 1);
        assert!(pkgs.contains("git"));
    }

    #[test]
    fn parse_nix_profile_list_json_empty_object_elements() {
        assert!(parse_nix_profile_list_json(r#"{"elements":{},"version":3}"#).is_empty());
    }

    #[test]
    fn parse_nix_profile_list_json_empty_array_elements() {
        assert!(parse_nix_profile_list_json(r#"{"elements":[]}"#).is_empty());
    }

    #[test]
    fn parse_nix_profile_list_json_missing_elements_key() {
        assert!(parse_nix_profile_list_json(r#"{"version":3}"#).is_empty());
    }

    #[test]
    fn parse_nix_profile_list_json_malformed_returns_empty_set() {
        assert!(parse_nix_profile_list_json("not json at all").is_empty());
        assert!(parse_nix_profile_list_json("").is_empty());
    }

    // --- parse_nix_env_query ---

    #[test]
    fn parse_nix_env_query_strips_version_suffix() {
        // nix-env -q --no-name --attr-path emits `attr-path` lines; we strip
        // the trailing `-X.Y.Z` per the strip_version_suffix contract.
        let stdout = "ripgrep-14.1.0\nfd-9.0.0\n";
        let pkgs = parse_nix_env_query(stdout);
        assert!(pkgs.contains("ripgrep"));
        assert!(pkgs.contains("fd"));
    }

    #[test]
    fn parse_nix_env_query_drops_empty_lines() {
        let stdout = "\nripgrep-14.1.0\n\n\nfd-9.0.0\n";
        let pkgs = parse_nix_env_query(stdout);
        assert_eq!(pkgs.len(), 2);
    }

    #[test]
    fn parse_nix_env_query_empty_input_returns_empty_set() {
        assert!(parse_nix_env_query("").is_empty());
    }

    // ---------------------------------------------------------------------
    // PackageManager-impl tests via CFGD_NIX_BIN / CFGD_NIX_ENV_BIN ToolShim.
    // Mirrors the brew/cargo/npm/pipx/go pattern: each test installs a shim
    // for whichever binary the code path under test should select, asserts
    // the expected argv landed at the shim, and tears the shim down via
    // Drop. #[serial] gates env-var mutation across the process.
    // ---------------------------------------------------------------------

    #[cfg(unix)]
    mod nix_shim {
        use super::*;
        use cfgd_core::providers::PackageManager;
        use cfgd_core::test_helpers::{ToolShim, test_package_context, test_printer, test_state};
        use serial_test::serial;

        const SHIM_ENV: &str = "CFGD_NIX_BIN";
        const SHIM_ENV_NIX_ENV: &str = "CFGD_NIX_ENV_BIN";

        #[test]
        #[serial]
        fn nix_install_batches_all_packages_into_one_nix_profile_spawn() {
            // CFGD_NIX_BIN is set → nix_available() returns true → install
            // takes the `nix profile install` path. CFGD_NIX_ENV_BIN must
            // stay unset so the test fails loudly if the wrong branch fires.
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            NixManager
                .install(&["ripgrep".into(), "fd".into(), "bat".into()], &cx)
                .expect("Ok");
            // Filter to the lines naming this test's own subject: the seam is
            // a process-global env var, so an unfiltered count also measures
            // whatever a parallel test spawned through the same shim.
            let lines = s.argv_lines_naming("nixpkgs#ripgrep");
            assert_eq!(
                lines.len(),
                1,
                "three packages must produce ONE spawn: {}",
                s.argv_log()
            );
            assert!(
                lines[0].contains("profile install")
                    && lines[0].contains("nixpkgs#fd")
                    && lines[0].contains("nixpkgs#bat"),
                "the one spawn must carry every installable: {}",
                lines[0]
            );
        }

        #[test]
        #[serial]
        fn nix_install_batch_failure_falls_back_to_per_package_attribution() {
            // The shim fails any argv naming the bad package: the batch line
            // carries it (so the batch fails), then the per-package retry
            // isolates it while the valid ones install.
            let s = ToolShim::install_failing_on(
                SHIM_ENV,
                "nixpkgs#nope",
                "error: flake 'nixpkgs' does not provide attribute 'nope'",
            );
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let err = NixManager
                .install(&["ripgrep".into(), "nope".into()], &cx)
                .expect_err("the bad package must fail after the retry");
            let msg = err.to_string();
            assert!(
                msg.contains("nope") && msg.contains("does not provide attribute"),
                "the error must name the failed package and its cause: {msg}"
            );
            assert!(
                !msg.contains("ripgrep ("),
                "the valid package must not be attributed a failure: {msg}"
            );
            // One batch spawn naming both, then one retry per package.
            assert_eq!(
                s.argv_lines_naming("nixpkgs#ripgrep").len(),
                2,
                "batch + its own retry: {}",
                s.argv_log()
            );
            assert_eq!(
                s.argv_lines_naming("nixpkgs#nope").len(),
                2,
                "batch + its own retry: {}",
                s.argv_log()
            );
        }

        #[test]
        #[serial]
        fn nix_uninstall_routes_through_nix_profile_when_nix_available() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            NixManager.uninstall(&["hello".into()], &cx).expect("Ok");
            let argv = s.argv_log();
            // nix 2.20+ removes by element NAME; `nix profile remove
            // nixpkgs#hello` matches nothing and exits 0, silently failing the
            // declarative prune.
            assert!(
                argv.contains("profile remove hello"),
                "argv must remove by element name: {argv}"
            );
            assert!(
                !argv.contains("nixpkgs#hello"),
                "argv must NOT use the flake selector that nix 2.20+ rejects: {argv}"
            );
        }

        #[test]
        #[serial]
        fn nix_installed_packages_uses_nix_profile_list_when_nix_available() {
            // nix 2.34 `nix profile list --json` (version 3) object shape.
            let stdout = r#"{"elements":{"ripgrep":{"storePaths":["/nix/store/abc-ripgrep"]},"fd":{"storePaths":["/nix/store/def-fd"]}},"version":3}"#;
            let s = ToolShim::install(SHIM_ENV, 0, stdout, "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = NixManager.installed_packages(&cx).expect("Ok");
            assert_eq!(pkgs.len(), 2);
            assert!(pkgs.contains("ripgrep"));
            assert!(pkgs.contains("fd"));
            assert!(
                s.argv_log().contains("profile list --json"),
                "must query JSON, not the version-fragile text format: {}",
                s.argv_log()
            );
        }

        #[test]
        #[serial]
        fn nix_installed_packages_falls_back_to_nix_env_when_profile_list_exits_nonzero() {
            // nix shim returns non-zero on `profile list` → installed_packages
            // falls through to nix-env path. Both shims must be installed.
            // Use the SAME tempdir tracking — but ToolShim::install creates
            // its own tempdir per call, so each shim is independent.
            let _nix = ToolShim::install(SHIM_ENV, 1, "", "profile list unsupported on this nix");
            let _nix_env = ToolShim::install(SHIM_ENV_NIX_ENV, 0, "ripgrep-14.1.0\nfd-9.0.0\n", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = NixManager.installed_packages(&cx).expect("Ok");
            assert!(pkgs.contains("ripgrep"));
            assert!(pkgs.contains("fd"));
        }

        #[test]
        #[serial]
        fn nix_available_version_uses_nix_search_when_nix_available() {
            let json = r#"{"legacyPackages.x86_64-linux.ripgrep":{"version":"14.1.0"}}"#;
            let s = ToolShim::install(SHIM_ENV, 0, json, "");
            let v = NixManager.available_version("ripgrep").expect("Ok");
            assert_eq!(v.as_deref(), Some("14.1.0"));
            let argv = s.argv_log();
            assert!(
                argv.contains("search nixpkgs ripgrep --json"),
                "argv must include `search nixpkgs <pkg> --json`: {argv}"
            );
        }

        #[test]
        #[serial]
        fn nix_available_version_returns_none_on_nonzero_exit() {
            let _s = ToolShim::install(SHIM_ENV, 1, "", "search service unavailable");
            let v = NixManager
                .available_version("anything")
                .expect("non-zero → Ok(None)");
            assert_eq!(v, None);
        }

        #[test]
        #[serial]
        fn nix_install_raises_a_held_element_via_nix_profile_upgrade_not_install() {
            // `nix profile list --json` already carries `ripgrep`, so
            // `install` partitions it into `held` and raises it through
            // `nix profile upgrade nixpkgs#ripgrep` instead of re-running
            // `nix profile install`, which would no-op; `fd` is unheld and
            // still installs.
            let json = r#"{"elements":{"ripgrep":{"storePaths":["/nix/store/a-ripgrep-14.1.0"]}},"version":3}"#;
            let s = ToolShim::install(SHIM_ENV, 0, json, "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            NixManager
                .install(&["ripgrep".into(), "fd".into()], &cx)
                .expect("Ok");
            let argv = s.argv_log();
            assert!(
                argv.contains("profile upgrade nixpkgs#ripgrep"),
                "held element must be raised via `nix profile upgrade`: {argv}"
            );
            assert!(
                argv.contains("nixpkgs#fd"),
                "unheld element must still install: {argv}"
            );
        }

        #[test]
        #[serial]
        fn nix_install_raises_a_held_package_via_nix_env_dash_u_not_ia() {
            // The legacy `nix-env -q` listing already carries `ripgrep`, so
            // `install` raises it through `nix-env -u ripgrep` instead of
            // re-running `nix-env -iA`, which would no-op.
            let s = ToolShim::install(SHIM_ENV_NIX_ENV, 0, "ripgrep\n", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            NixManager.install(&["ripgrep".into()], &cx).expect("Ok");
            let argv = s.argv_log();
            assert!(
                argv.contains("-u ripgrep"),
                "held package must be raised via `nix-env -u`: {argv}"
            );
            assert!(
                !argv.contains("-iA"),
                "held package must not be re-run through `nix-env -iA`: {argv}"
            );
        }

        #[test]
        #[serial]
        fn nix_install_uses_nix_env_when_only_nix_env_seam_set() {
            // Shim ONLY on CFGD_NIX_ENV_BIN — nix_available() is false, so
            // install routes through the nix-env -iA fallback path.
            let s = ToolShim::install(SHIM_ENV_NIX_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            NixManager
                .install(&["ripgrep".into(), "fd".into()], &cx)
                .expect("Ok");
            let lines = s.argv_lines_naming("nixpkgs.ripgrep");
            assert_eq!(lines.len(), 1, "one batched spawn: {}", s.argv_log());
            assert!(
                lines[0].contains("-iA nixpkgs.ripgrep nixpkgs.fd"),
                "fallback argv must batch `nix-env -iA nixpkgs.<pkg>...`: {}",
                lines[0]
            );
        }

        #[test]
        #[serial]
        fn nix_uninstall_uses_nix_env_when_only_nix_env_seam_set() {
            let s = ToolShim::install(SHIM_ENV_NIX_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            NixManager.uninstall(&["ripgrep".into()], &cx).expect("Ok");
            assert!(
                s.argv_log().contains("-e ripgrep"),
                "fallback argv must use `nix-env -e <pkg>`: {}",
                s.argv_log()
            );
        }

        use cfgd_core::test_helpers::install_named_path_shim;

        #[test]
        #[serial]
        fn nix_bootstrap_runs_sh_install_pipeline_ok() {
            let (_bin, _path) = install_named_path_shim("sh", 0, "", "");
            let p = test_printer();
            NixManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context(&p))
                .expect("bootstrap Ok via shim");
        }

        #[test]
        #[serial]
        fn nix_bootstrap_propagates_nonzero_exit_as_bootstrap_failed() {
            let (_bin, _path) = install_named_path_shim("sh", 1, "", "nix install failed");
            let p = test_printer();
            let err = NixManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context(&p))
                .expect_err("non-zero sh must error");
            let msg = err.to_string();
            assert!(
                msg.contains("nix") || msg.contains("bootstrap"),
                "error must surface bootstrap context: {msg}"
            );
        }
    }
}
