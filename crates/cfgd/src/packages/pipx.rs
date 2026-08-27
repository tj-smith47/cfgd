//! pipx-based package manager.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::providers::{BootstrapPlan, PackageManager};

use super::shared::{
    MediatedArms, bootstrap_via_brew_then_system, brew_then_system_arms, detect_brew_system_method,
    pip_user_scripts_dir, pkg_run, planned_method_failed, planned_method_unavailable,
    resolve_tool_with_fallbacks, run_pkg_cmd, run_pkg_cmd_live, tool_cmd_with_resolver,
};

pub struct PipxManager;

/// pipx's own bootstrap arm, reached when no brew/system mediator is present.
/// The ONE spelling, for the same reason npm has one: the planner resolves the
/// method against it and the cascade declines toward it.
const PIPX_FALLBACK_METHOD: &str = "pip";

/// What a mediator installs to deliver pipx — same role as npm's table.
const PIPX_MEDIATED: MediatedArms = brew_then_system_arms("pipx", &["pipx"]);

fn pipx_fallbacks() -> Vec<PathBuf> {
    let mut fallbacks: Vec<PathBuf> = std::env::var_os("HOME")
        .map(|h| pipx_fallbacks_for_home(std::path::Path::new(&h)))
        .unwrap_or_default();
    if cfg!(windows) {
        fallbacks.extend(windows_user_pipx_candidates());
    }
    fallbacks
}

/// Every `pipx.exe` a `pip install --user pipx` could have left under roaming
/// AppData — the same `nt_user` tree the bootstrap plan declares, so a pipx this
/// machine installed is still found when its `Scripts` directory never reached
/// `PATH`.
///
/// The version segment belongs to whichever interpreter pip ran, so the tree is
/// READ rather than probed: this resolver answers `is_available()`, which a
/// single run asks many times, and must not spawn a process to do it.
pub(super) fn windows_user_pipx_candidates() -> Vec<PathBuf> {
    let Some(root) = std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("Python")) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path().join("Scripts").join("pipx.exe"))
        .collect()
}

/// `pipx_fallbacks` with the `$HOME` directory injected — split out so tests
/// exercise the path-construction contract without mutating process env state.
pub(super) fn pipx_fallbacks_for_home(home: &std::path::Path) -> Vec<PathBuf> {
    vec![home.join(".local/bin/pipx")]
}

pub(super) fn find_pipx() -> Option<PathBuf> {
    resolve_tool_with_fallbacks("pipx", &pipx_fallbacks())
}

pub(super) fn pipx_available() -> bool {
    find_pipx().is_some()
}

// The tool the pip fallback would run: whichever of pip3/pip is present,
// else the preferred name. Shared by `bootstrap_plan` and `path_dirs` so
// both always name the same interpreter.
fn pipx_pip_tool() -> &'static str {
    ["pip3", "pip"]
        .into_iter()
        .find(|t| command_available(t))
        .unwrap_or("pip3")
}

// Single source for the pip fallback's user-scripts dir, so
// `bootstrap_plan`'s declaration and `path_dirs`'s recording can never
// drift apart.
fn pipx_pip_scripts_dir() -> Option<PathBuf> {
    pip_user_scripts_dir(pipx_pip_tool())
}

pub(super) fn pipx_cmd() -> Command {
    tool_cmd_with_resolver("pipx", find_pipx)
}

impl PackageManager for PipxManager {
    fn name(&self) -> &str {
        "pipx"
    }

    fn is_available(&self) -> bool {
        pipx_available()
    }

    fn bootstrap_plan(&self) -> Option<BootstrapPlan> {
        match detect_brew_system_method(PIPX_FALLBACK_METHOD) {
            // Only the pip fallback installs into the user's own tree; brew and
            // the system managers land pipx on the system PATH.
            // The tool the pip arm would run: whichever is present, else the
            // preferred name. Naming it even when it is absent is what lets the
            // planner say WHY pipx cannot be provisioned instead of dropping it
            // — `pip3` is not installable under that name from any system
            // manager, so `feasible_bootstrap_plan` still answers `None`.
            "pip" => Some(
                BootstrapPlan::new("pip")
                    .requiring([pipx_pip_tool()])
                    .creating(pipx_pip_scripts_dir()),
            ),
            method => Some(BootstrapPlan::new(method)),
        }
    }

    fn path_dirs(&self, cx: &cfgd_core::providers::PackageContext<'_>) -> Vec<String> {
        // The method this run already decided, not a fresh probe: the plan
        // resolves the method once and binds the bootstrap to it, so re-probing
        // here can name a directory the plan never promised — brew appearing
        // between the two calls is enough. A context carrying no planned method
        // belongs to a caller outside a plan (`cfgd doctor`, a direct caller),
        // which has no decision to read and resolves the cascade as before.
        let method = cx
            .planned_method()
            .unwrap_or_else(|| detect_brew_system_method(PIPX_FALLBACK_METHOD));
        match method {
            "pip" => pipx_pip_scripts_dir()
                .into_iter()
                .map(cfgd_core::to_posix_string)
                .collect(),
            // brew/system installs land pipx on the system PATH; nothing new
            // to declare.
            _ => Vec::new(),
        }
    }

    fn bootstrap(&self, cx: &cfgd_core::providers::PackageContext<'_>) -> Result<()> {
        // Returns false without probing anything when the plan named `pip` —
        // pipx's own fallback arm, which is the next thing below.
        if bootstrap_via_brew_then_system(
            cx,
            "pipx",
            PIPX_MEDIATED.brew.unwrap_or("pipx"),
            PIPX_MEDIATED.system,
            PIPX_FALLBACK_METHOD,
        )? {
            return Ok(());
        }

        // Fall back to pip. Resolved to a full path rather than spawned by bare
        // name: `command_path` searches the directories cfgd bootstrapped this
        // run as well as `$PATH`, and a bare-name spawn searches only `$PATH`.
        let Some((pip_cmd, pip_path)) = ["pip3", "pip"]
            .into_iter()
            .find_map(|tool| cfgd_core::command_path(tool).map(|path| (tool, path)))
        else {
            return Err(match cx.planned_method() {
                Some(method) => planned_method_unavailable("pipx", method),
                None => PackageError::BootstrapFailed {
                    manager: "pipx".into(),
                    message: "no method available to install pipx".into(),
                },
            }
            .into());
        };

        let label = format!("Installing pipx via {}", pip_cmd);
        let result = pkg_run(
            cx,
            Command::new(pip_path).args(["install", "--user", "pipx"]),
            &label,
        )
        .map_err(|e| PackageError::BootstrapFailed {
            manager: "pipx".into(),
            message: format!("{} install failed: {}", pip_cmd, e),
        })?;
        if !result.status.success() {
            return Err(match cx.planned_method() {
                Some(method) => planned_method_failed("pipx", method, &result),
                None => PackageError::BootstrapFailed {
                    manager: "pipx".into(),
                    message: format!("{} install --user pipx failed", pip_cmd),
                },
            }
            .into());
        }

        Ok(())
    }

    fn mediated_packages(&self, via: &str) -> Option<Vec<String>> {
        PIPX_MEDIATED.packages_for(via)
    }

    fn installed_packages(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<HashSet<String>> {
        let output = run_pkg_cmd("pipx", pipx_cmd().args(["list", "--json"]), "list")?;
        parse_pipx_list_packages(&String::from_utf8_lossy(&output.stdout))
    }

    fn install(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        for pkg in packages {
            let label = format!("pipx install {}", pkg);
            run_pkg_cmd_live(
                cx,
                "pipx",
                pipx_cmd().args(["install", pkg]),
                &label,
                "install",
            )?;
        }
        Ok(())
    }

    fn uninstall(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        for pkg in packages {
            let label = format!("pipx uninstall {}", pkg);
            run_pkg_cmd_live(
                cx,
                "pipx",
                pipx_cmd().args(["uninstall", pkg]),
                &label,
                "uninstall",
            )?;
        }
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // Query PyPI JSON API: https://pypi.org/pypi/<pkg>/json → .info.version
        let url = format!("https://pypi.org/pypi/{}/json", package);
        let output = Command::new("curl")
            .args(["-fsSL", &url])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "pipx".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        parse_pypi_version(&String::from_utf8_lossy(&output.stdout))
    }

    fn installed_packages_with_versions(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        let output = run_pkg_cmd("pipx", pipx_cmd().args(["list", "--json"]), "list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| PackageError::ListFailed {
                manager: "pipx".into(),
                message: format!("failed to parse pipx list output: {}", e),
            })?;
        Ok(parse_pipx_list_versions(&parsed))
    }
}

/// Parse `pipx list --json` venvs object into a name-only `HashSet`.
/// Shared with `installed_packages`; the JSON-string boundary is the natural
/// contract since `pipx list --json` is the production input.
pub(super) fn parse_pipx_list_packages(stdout: &str) -> Result<HashSet<String>> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).map_err(|e| PackageError::ListFailed {
            manager: "pipx".into(),
            message: format!("failed to parse pipx list output: {}", e),
        })?;
    let mut packages = HashSet::new();
    if let Some(venvs) = parsed.get("venvs").and_then(|v| v.as_object()) {
        for key in venvs.keys() {
            packages.insert(key.clone());
        }
    }
    Ok(packages)
}

/// Parse the PyPI JSON API response for the latest version.
/// Returns `Ok(None)` when `/info/version` is absent or non-string —
/// callers treat that as "version unknown" rather than an error.
pub(super) fn parse_pypi_version(stdout: &str) -> Result<Option<String>> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).map_err(|e| PackageError::ListFailed {
            manager: "pipx".into(),
            message: format!("failed to parse PyPI response: {}", e),
        })?;
    Ok(parsed
        .pointer("/info/version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()))
}

/// Parse `pipx list --json` venvs object into PackageInfo.
/// JSON format: `{"venvs": {"pkg": {"metadata": {"main_package": {"package_version": "1.2.3"}}}}}`
pub(super) fn parse_pipx_list_versions(
    parsed: &serde_json::Value,
) -> Vec<cfgd_core::providers::PackageInfo> {
    let mut packages = Vec::new();
    if let Some(venvs) = parsed.get("venvs").and_then(|v| v.as_object()) {
        for (name, info) in venvs {
            let version = info
                .pointer("/metadata/main_package/package_version")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            packages.push(cfgd_core::providers::PackageInfo {
                name: name.clone(),
                version,
            });
        }
    }
    packages
}

#[cfg(test)]
mod tests {
    use cfgd_core::command_available;
    use cfgd_core::providers::PackageManager;

    use super::super::shared::brew_available;
    use super::*;

    #[test]
    fn test_parse_pipx_list_versions_basic() {
        let json = serde_json::json!({
            "venvs": {
                "black": {
                    "metadata": {
                        "main_package": {
                            "package_version": "24.1.1"
                        }
                    }
                },
                "httpie": {
                    "metadata": {
                        "main_package": {
                            "package_version": "3.2.2"
                        }
                    }
                }
            }
        });
        let pkgs = parse_pipx_list_versions(&json);
        assert_eq!(pkgs.len(), 2);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "black" && p.version == "24.1.1")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "httpie" && p.version == "3.2.2")
        );
    }

    #[test]
    fn test_parse_pipx_list_versions_no_venvs() {
        let json = serde_json::json!({"venvs": {}});
        let pkgs = parse_pipx_list_versions(&json);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn test_parse_pipx_list_versions_missing_version_field() {
        let json = serde_json::json!({
            "venvs": {
                "awscli": {
                    "metadata": {
                        "main_package": {}
                    }
                }
            }
        });
        let pkgs = parse_pipx_list_versions(&json);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].version, "unknown");
    }

    #[test]
    fn parse_pipx_list_versions_null_root() {
        let json = serde_json::json!(null);
        let pkgs = parse_pipx_list_versions(&json);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_pipx_list_versions_missing_metadata() {
        let json = serde_json::json!({
            "venvs": {
                "tool": {}
            }
        });
        let pkgs = parse_pipx_list_versions(&json);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "tool");
        assert_eq!(pkgs[0].version, "unknown");
    }

    #[test]
    fn pipx_manager_name() {
        let mgr = PipxManager;
        assert_eq!(mgr.name(), "pipx");
    }

    #[test]
    fn parse_pipx_list_versions_multiple_venvs() {
        let json = serde_json::json!({
            "venvs": {
                "black": {"metadata": {"main_package": {"package_version": "24.1.1"}}},
                "httpie": {"metadata": {"main_package": {"package_version": "3.2.2"}}},
                "ruff": {"metadata": {"main_package": {"package_version": "0.2.0"}}},
                "mypy": {"metadata": {"main_package": {"package_version": "1.8.0"}}}
            }
        });
        let pkgs = parse_pipx_list_versions(&json);
        assert_eq!(pkgs.len(), 4);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "ruff" && p.version == "0.2.0")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "mypy" && p.version == "1.8.0")
        );
    }

    #[test]
    fn parse_pipx_list_versions_no_venvs_key() {
        let json = serde_json::json!({"pipx_spec_version": "0.1"});
        let pkgs = parse_pipx_list_versions(&json);
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_pipx_list_versions_real_world_output() {
        let json = serde_json::json!({
            "pipx_spec_version": "0.1",
            "venvs": {
                "black": {
                    "metadata": {
                        "main_package": {
                            "package": "black",
                            "package_version": "24.1.1",
                            "pip_args": [],
                            "include_apps": true,
                            "include_dependencies": false
                        },
                        "python_version": "Python 3.12.1"
                    }
                },
                "ruff": {
                    "metadata": {
                        "main_package": {
                            "package": "ruff",
                            "package_version": "0.2.0",
                            "pip_args": [],
                            "include_apps": true,
                            "include_dependencies": false
                        },
                        "python_version": "Python 3.12.1"
                    }
                }
            }
        });
        let pkgs = parse_pipx_list_versions(&json);
        assert_eq!(pkgs.len(), 2);
        assert!(
            pkgs.iter()
                .any(|p| p.name == "black" && p.version == "24.1.1")
        );
        assert!(
            pkgs.iter()
                .any(|p| p.name == "ruff" && p.version == "0.2.0")
        );
    }

    #[test]
    fn parse_pipx_list_versions_with_injected_packages() {
        let json = serde_json::json!({
            "venvs": {
                "black": {
                    "metadata": {
                        "main_package": {"package_version": "24.1.1"},
                        "injected_packages": {
                            "black[jupyter]": {"package_version": "24.1.1"}
                        }
                    }
                }
            }
        });
        let pkgs = parse_pipx_list_versions(&json);
        // Only main_package is extracted, not injected
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].name, "black");
        assert_eq!(pkgs[0].version, "24.1.1");
    }

    #[test]
    fn pipx_bootstrap_plan_follows_the_brew_system_pip_cascade() {
        // The probes below assert what THIS host resolves, so hold the read
        // guard — a sibling test empties PATH under the write guard.
        let _path = cfgd_core::test_helpers::path_env_read_guard();
        // The plan always exists: the cascade's pip fallback names the tool it
        // would need even when no pip is present, so the planner can say WHY
        // pipx cannot be provisioned (`feasible_bootstrap_plan` answers the
        // `None`).
        let plan = PipxManager
            .bootstrap_plan()
            .expect("pipx plans on every host via the pip fallback");
        // A host with no brew and no system arm (FreeBSD CI) lands on the pip
        // fallback. The system probes are the production arms' probe binaries
        // (`apt-get`, not `apt` — BREW_SYSTEM_ARMS).
        if !brew_available() && !command_available("apt-get") && !command_available("dnf") {
            assert_eq!(plan.method, "pip");
        }
        // Only `bootstrap`'s pip fallback installs into the user's own tree
        // (`pip install --user`); brew and the system managers put pipx on
        // the system PATH, so they declare no directory. The user tree is
        // not the same directory on every platform — Windows sends console
        // scripts to CPython's `nt_user` scheme under roaming AppData.
        if plan.method == "pip" {
            assert_eq!(plan.requires.len(), 1);
            assert!(["pip3", "pip"].contains(&plan.requires[0].as_str()));
            let is_user_scripts_dir = |d: &String| {
                if cfg!(windows) {
                    d.contains("/Python/Python") && d.ends_with("/Scripts")
                } else {
                    d.ends_with("/.local/bin")
                }
            };
            assert!(
                plan.creates_path_dirs.iter().all(is_user_scripts_dir),
                "{:?}",
                plan.creates_path_dirs
            );
        } else {
            assert!(["brew", "apt", "dnf"].contains(&plan.method.as_str()));
            assert!(plan.requires.is_empty());
            assert!(plan.creates_path_dirs.is_empty());
        }
    }

    /// One host, two planned methods, two answers: `path_dirs` reads the
    /// decision the run already made rather than the machine as it looks right
    /// now. Re-deriving here is what let the plan promise one directory while
    /// the record written after the bootstrap named another — brew appearing
    /// between the two calls is enough to move a live probe.
    #[test]
    fn pipx_path_dirs_answers_from_the_planned_method() {
        let printer = cfgd_core::test_helpers::test_printer();
        let state = cfgd_core::test_helpers::test_state();

        let via_brew =
            cfgd_core::test_helpers::test_package_context(&printer, &state).for_provision("brew");
        assert!(
            PipxManager.path_dirs(&via_brew).is_empty(),
            "a brew-mediated pipx lands on the system PATH and declares nothing"
        );

        // The pip arm's directory is `~/.local/bin` on every Unix; on Windows it
        // carries the interpreter's own version and is unnameable without a pip
        // to ask, so only the method-dispatch half of the claim holds there.
        #[cfg(unix)]
        {
            let via_pip = cfgd_core::test_helpers::test_package_context(&printer, &state)
                .for_provision("pip");
            let dirs = PipxManager.path_dirs(&via_pip);
            assert_eq!(dirs.len(), 1, "{dirs:?}");
            assert!(dirs[0].ends_with("/.local/bin"), "{dirs:?}");
        }
    }

    #[test]
    fn pipx_path_dirs_matches_the_bootstrap_plans_declaration() {
        let plan = PipxManager
            .bootstrap_plan()
            .expect("pipx always declares a bootstrap plan");
        let printer = cfgd_core::test_helpers::test_printer();
        let state = cfgd_core::test_helpers::test_state();
        let cx = cfgd_core::test_helpers::test_package_context(&printer, &state);
        let mgr: Box<dyn PackageManager> = Box::new(PipxManager);
        assert_eq!(mgr.path_dirs(&cx), plan.creates_path_dirs);
    }

    #[test]
    #[serial_test::serial]
    fn pipx_manager_is_available_checks_pipx() {
        let mgr = PipxManager;
        let available = mgr.is_available();
        assert_eq!(available, pipx_available());
    }

    // --- parse_pipx_list_packages ---

    #[test]
    fn parse_pipx_list_packages_returns_venv_names() {
        let stdout = r#"{"venvs":{"black":{},"ruff":{},"httpie":{}}}"#;
        let pkgs = parse_pipx_list_packages(stdout).unwrap();
        assert_eq!(pkgs.len(), 3);
        assert!(pkgs.contains("black"));
        assert!(pkgs.contains("ruff"));
        assert!(pkgs.contains("httpie"));
    }

    #[test]
    fn parse_pipx_list_packages_no_venvs_field_yields_empty() {
        let stdout = r#"{"pipx_spec_version":"0.1"}"#;
        let pkgs = parse_pipx_list_packages(stdout).unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_pipx_list_packages_empty_venvs_yields_empty() {
        let stdout = r#"{"venvs":{}}"#;
        let pkgs = parse_pipx_list_packages(stdout).unwrap();
        assert!(pkgs.is_empty());
    }

    #[test]
    fn parse_pipx_list_packages_errors_on_invalid_json() {
        let err = parse_pipx_list_packages("garbage").expect_err("invalid JSON must error");
        let msg = err.to_string();
        assert!(
            msg.contains("pipx") && msg.contains("failed to parse pipx list output"),
            "error must include 'pipx' and parse-failure context, got: {msg}"
        );
    }

    // --- parse_pypi_version ---

    #[test]
    fn parse_pypi_version_extracts_info_version() {
        let stdout = r#"{"info":{"name":"black","version":"24.1.1"}}"#;
        let v = parse_pypi_version(stdout).unwrap();
        assert_eq!(v.as_deref(), Some("24.1.1"));
    }

    #[test]
    fn parse_pypi_version_returns_none_when_field_missing() {
        let stdout = r#"{"info":{"name":"black"}}"#;
        let v = parse_pypi_version(stdout).unwrap();
        assert!(v.is_none());
    }

    #[test]
    fn parse_pypi_version_returns_none_when_value_is_non_string() {
        // Tolerate broken/non-conforming PyPI responses (e.g. integer field).
        let stdout = r#"{"info":{"version":42}}"#;
        let v = parse_pypi_version(stdout).unwrap();
        assert!(v.is_none());
    }

    #[test]
    fn parse_pypi_version_errors_on_invalid_json() {
        let err = parse_pypi_version("not-json").expect_err("invalid JSON must error");
        let msg = err.to_string();
        assert!(
            msg.contains("pipx") && msg.contains("failed to parse PyPI response"),
            "error must attribute to pipx + name PyPI source, got: {msg}"
        );
    }

    // --- pipx_fallbacks_for_home ---

    #[test]
    fn pipx_fallbacks_for_home_contains_local_bin_path() {
        let home = std::path::Path::new("/some/home");
        let fallbacks = pipx_fallbacks_for_home(home);
        assert_eq!(fallbacks.len(), 1);
        assert_eq!(fallbacks[0], home.join(".local/bin/pipx"));
    }

    #[test]
    #[serial_test::serial]
    fn windows_pipx_candidates_come_from_the_roaming_python_tree() {
        let appdata = tempfile::tempdir().unwrap();
        let scripts = appdata
            .path()
            .join("Python")
            .join("Python314")
            .join("Scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        let _guard = cfgd_core::test_helpers::EnvVarGuard::set(
            "APPDATA",
            appdata.path().to_string_lossy().as_ref(),
        );

        assert_eq!(
            windows_user_pipx_candidates(),
            vec![scripts.join("pipx.exe")],
            "a pipx installed by `pip install --user` must be findable off PATH"
        );
    }

    #[test]
    #[serial_test::serial]
    fn windows_pipx_candidates_are_empty_without_a_roaming_python_tree() {
        let appdata = tempfile::tempdir().unwrap();
        let _guard = cfgd_core::test_helpers::EnvVarGuard::set(
            "APPDATA",
            appdata.path().to_string_lossy().as_ref(),
        );

        assert!(windows_user_pipx_candidates().is_empty());
    }

    // ---------------------------------------------------------------------
    // PackageManager-impl tests via CFGD_PIPX_BIN ToolShim. The seam is
    // honored automatically by `tool_cmd_with_resolver` / `find_pipx`.
    // ---------------------------------------------------------------------

    #[cfg(unix)]
    mod pipx_shim {
        use super::*;
        use cfgd_core::providers::PackageManager;
        use cfgd_core::test_helpers::{
            ToolShim, install_named_path_shim, test_package_context, test_printer, test_state,
        };
        use serial_test::serial;

        const SHIM_ENV: &str = "CFGD_PIPX_BIN";

        #[test]
        #[serial]
        fn pipx_install_runs_install_subcommand_per_package() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            PipxManager
                .install(&["black".into(), "ruff".into()], &cx)
                .expect("Ok");
            assert_eq!(s.invocation_count(), 2, "one pipx invocation per pkg");
            let argv = s.argv_log();
            assert!(argv.contains("install black"));
            assert!(argv.contains("install ruff"));
        }

        #[test]
        #[serial]
        fn pipx_uninstall_runs_uninstall_subcommand_per_package() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            PipxManager.uninstall(&["black".into()], &cx).expect("Ok");
            assert!(s.argv_log().contains("uninstall black"));
        }

        #[test]
        #[serial]
        fn pipx_declares_no_index_and_refreshing_upgrades_nothing() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            assert!(!PipxManager.has_index(), "pipx resolves PyPI per install");
            PipxManager.refresh_index(&cx).expect("Ok");
            assert_eq!(
                s.invocation_count(),
                0,
                "`pipx upgrade-all` upgrades every venv on the machine: {}",
                s.argv_log()
            );
        }

        #[test]
        #[serial]
        fn pipx_installed_packages_parses_venvs_json() {
            // pipx list --json: { "venvs": { "black": { ... }, "ruff": { ... } } }
            let json = r#"{"venvs":{"black":{"metadata":{"main_package":{"package":"black","package_version":"24.1.0"}}},"ruff":{"metadata":{"main_package":{"package":"ruff","package_version":"0.2.1"}}}}}"#;
            let _s = ToolShim::install(SHIM_ENV, 0, json, "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = PipxManager.installed_packages(&cx).expect("Ok");
            assert_eq!(pkgs.len(), 2);
            assert!(pkgs.contains("black"));
            assert!(pkgs.contains("ruff"));
        }

        // available_version shells out to `curl` rather than the pipx shim.
        fn install_curl_shim(
            exit_code: u8,
            stdout: &str,
            stderr: &str,
        ) -> (tempfile::TempDir, cfgd_core::test_helpers::PathShimGuard) {
            install_named_path_shim("curl", exit_code, stdout, stderr)
        }

        #[test]
        #[serial]
        fn pipx_available_version_parses_pypi_json_on_success() {
            let body = r#"{"info":{"version":"24.1.1","name":"black"}}"#;
            let (_bin, _path) = install_curl_shim(0, body, "");
            let v = PipxManager.available_version("black").expect("Ok");
            assert_eq!(v.as_deref(), Some("24.1.1"));
        }

        #[test]
        #[serial]
        fn pipx_available_version_returns_none_on_curl_nonzero_exit() {
            let (_bin, _path) = install_curl_shim(22, "", "404 not found");
            let v = PipxManager
                .available_version("nonexistent-pkg")
                .expect("non-zero curl → Ok(None)");
            assert_eq!(v, None);
        }

        #[test]
        #[serial]
        fn pipx_installed_packages_with_versions_extracts_versions() {
            let json = r#"{"venvs":{"black":{"metadata":{"main_package":{"package":"black","package_version":"24.1.0"}}}}}"#;
            let _s = ToolShim::install(SHIM_ENV, 0, json, "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = PipxManager
                .installed_packages_with_versions(&cx)
                .expect("Ok");
            let black = pkgs
                .iter()
                .find(|p| p.name == "black")
                .expect("black present");
            assert_eq!(black.version, "24.1.0");
        }

        // bootstrap: brew-first cascade. A successful brew shim exercises the
        // early-return path through `bootstrap_via_brew_then_system`.
        #[test]
        #[serial]
        fn pipx_bootstrap_via_brew_returns_ok() {
            let s = ToolShim::install("CFGD_BREW_BIN", 0, "", "");
            let p = test_printer();
            PipxManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context(&p))
                .expect("bootstrap Ok via brew");
            assert!(
                s.argv_log().contains("install pipx"),
                "brew argv must include `install pipx`: {}",
                s.argv_log()
            );
        }
    }
}
