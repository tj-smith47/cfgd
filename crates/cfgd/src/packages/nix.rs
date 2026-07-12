//! Nix package manager (`nix profile` and `nix-env`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::PackageManager;

use super::shared::{
    bootstrap_via_shell_script, resolve_tool_with_fallbacks, run_pkg_cmd, run_pkg_cmd_live,
    strip_version_suffix, tool_cmd_with_resolver,
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

impl PackageManager for NixManager {
    fn name(&self) -> &str {
        "nix"
    }

    fn is_available(&self) -> bool {
        nix_env_available() || nix_available()
    }

    fn can_bootstrap(&self) -> bool {
        command_available("curl")
    }

    fn bootstrap(&self, printer: &Printer) -> Result<()> {
        bootstrap_via_shell_script(
            printer,
            "nix",
            "Installing Nix",
            "curl -L https://nixos.org/nix/install | sh -s -- --daemon",
        )
    }

    fn installed_packages(&self) -> Result<HashSet<String>> {
        // Prefer `nix profile list --json`: the plain-text format changed to
        // a multi-line `Key: value` block in nix 2.20+, which no line-oriented
        // parser can reliably read; the JSON shape is stable across versions.
        if nix_available() {
            let output = nix_cmd()
                .args(["profile", "list", "--json"])
                .output()
                .map_err(|e| PackageError::CommandFailed {
                    manager: "nix".into(),
                    source: e,
                })?;

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

    fn install(&self, packages: &[String], printer: &Printer) -> Result<()> {
        for pkg in packages {
            if nix_available() {
                let label = format!("nix profile install nixpkgs#{}", pkg);
                run_pkg_cmd_live(
                    printer,
                    "nix",
                    nix_cmd().args(["profile", "install", &format!("nixpkgs#{}", pkg)]),
                    &label,
                    "install",
                )?;
            } else {
                let label = format!("nix-env -iA nixpkgs.{}", pkg);
                run_pkg_cmd_live(
                    printer,
                    "nix",
                    nix_env_cmd().args(["-iA", &format!("nixpkgs.{}", pkg)]),
                    &label,
                    "install",
                )?;
            }
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], printer: &Printer) -> Result<()> {
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
                    printer,
                    "nix",
                    nix_cmd().args(["profile", "remove", pkg]),
                    &label,
                    "uninstall",
                )?;
            } else {
                let label = format!("nix-env -e {}", pkg);
                run_pkg_cmd_live(
                    printer,
                    "nix",
                    nix_env_cmd().args(["-e", pkg]),
                    &label,
                    "uninstall",
                )?;
            }
        }
        Ok(())
    }

    fn update(&self, _printer: &Printer) -> Result<()> {
        // Nix packages are pinned; update is a no-op (channels are managed separately)
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // nix search nixpkgs <pkg> --json → parse version from first matching result
        if nix_available() {
            let output = nix_cmd()
                .args(["search", "nixpkgs", package, "--json"])
                .output()
                .map_err(|e| PackageError::CommandFailed {
                    manager: "nix".into(),
                    source: e,
                })?;
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
    use cfgd_core::command_available;
    use cfgd_core::providers::PackageManager;

    use super::*;

    #[test]
    fn nix_manager_name_and_traits() {
        let mgr = NixManager;
        assert_eq!(mgr.name(), "nix");
    }

    #[test]
    fn nix_manager_update_is_noop() {
        let mgr = NixManager;
        let printer = cfgd_core::test_helpers::test_printer();
        mgr.update(&printer).unwrap();
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
    fn nix_manager_can_bootstrap_checks_curl() {
        let mgr = NixManager;
        let can = mgr.can_bootstrap();
        assert_eq!(can, command_available("curl"));
    }

    #[test]
    #[serial_test::serial]
    fn nix_manager_is_available_checks_nix_env_or_nix() {
        // Snapshot + clear the seam env vars so this assertion mirrors the
        // PATH-only contract (mgr.is_available() == command_available union).
        // Without this, parallel ToolShim tests setting CFGD_NIX_BIN /
        // CFGD_NIX_ENV_BIN would race with this assertion.
        let prev_nix = std::env::var_os("CFGD_NIX_BIN");
        let prev_nix_env = std::env::var_os("CFGD_NIX_ENV_BIN");
        // SAFETY: serial.
        unsafe {
            std::env::remove_var("CFGD_NIX_BIN");
            std::env::remove_var("CFGD_NIX_ENV_BIN");
        }
        let mgr = NixManager;
        let available = mgr.is_available();
        let expected = command_available("nix-env") || command_available("nix");
        // Restore before any panic so other tests aren't poisoned.
        // SAFETY: serial.
        unsafe {
            if let Some(v) = prev_nix {
                std::env::set_var("CFGD_NIX_BIN", v);
            }
            if let Some(v) = prev_nix_env {
                std::env::set_var("CFGD_NIX_ENV_BIN", v);
            }
        }
        assert_eq!(available, expected);
    }

    #[test]
    fn nix_update_returns_ok() {
        let mgr = NixManager;
        let printer = cfgd_core::test_helpers::test_printer();
        mgr.update(&printer).unwrap();
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
        use cfgd_core::test_helpers::{ToolShim, test_printer};
        use serial_test::serial;

        const SHIM_ENV: &str = "CFGD_NIX_BIN";
        const SHIM_ENV_NIX_ENV: &str = "CFGD_NIX_ENV_BIN";

        #[test]
        #[serial]
        fn nix_install_routes_through_nix_profile_when_nix_available() {
            // CFGD_NIX_BIN is set → nix_available() returns true → install
            // takes the `nix profile install` path. CFGD_NIX_ENV_BIN must
            // stay unset so the test fails loudly if the wrong branch fires.
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            NixManager
                .install(&["ripgrep".into(), "fd".into()], &p)
                .expect("Ok");
            // is_available() consults nix_env_available() first; install
            // hits nix_available() per package. With the shim set only on
            // CFGD_NIX_BIN, install should call the shim 2× via
            // `nix profile install nixpkgs#<pkg>`.
            let argv = s.argv_log();
            assert!(
                argv.contains("profile install nixpkgs#ripgrep"),
                "ripgrep argv must use `nix profile install nixpkgs#`: {argv}"
            );
            assert!(
                argv.contains("profile install nixpkgs#fd"),
                "fd argv must use `nix profile install nixpkgs#`: {argv}"
            );
        }

        #[test]
        #[serial]
        fn nix_uninstall_routes_through_nix_profile_when_nix_available() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            NixManager.uninstall(&["hello".into()], &p).expect("Ok");
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
            let pkgs = NixManager.installed_packages().expect("Ok");
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
            let pkgs = NixManager.installed_packages().expect("Ok");
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
        fn nix_install_uses_nix_env_when_only_nix_env_seam_set() {
            // Shim ONLY on CFGD_NIX_ENV_BIN — nix_available() is false, so
            // install routes through the nix-env -iA fallback path.
            let s = ToolShim::install(SHIM_ENV_NIX_ENV, 0, "", "");
            let p = test_printer();
            NixManager.install(&["ripgrep".into()], &p).expect("Ok");
            let argv = s.argv_log();
            assert!(
                argv.contains("-iA nixpkgs.ripgrep"),
                "fallback argv must use `nix-env -iA nixpkgs.<pkg>`: {argv}"
            );
        }

        #[test]
        #[serial]
        fn nix_uninstall_uses_nix_env_when_only_nix_env_seam_set() {
            let s = ToolShim::install(SHIM_ENV_NIX_ENV, 0, "", "");
            let p = test_printer();
            NixManager.uninstall(&["ripgrep".into()], &p).expect("Ok");
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
            NixManager.bootstrap(&p).expect("bootstrap Ok via shim");
        }

        #[test]
        #[serial]
        fn nix_bootstrap_propagates_nonzero_exit_as_bootstrap_failed() {
            let (_bin, _path) = install_named_path_shim("sh", 1, "", "nix install failed");
            let p = test_printer();
            let err = NixManager
                .bootstrap(&p)
                .expect_err("non-zero sh must error");
            let msg = err.to_string();
            assert!(
                msg.contains("nix") || msg.contains("bootstrap"),
                "error must surface bootstrap context: {msg}"
            );
        }
    }
}
