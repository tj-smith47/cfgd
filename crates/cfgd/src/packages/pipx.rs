//! pipx-based package manager.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::command_available;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::providers::{BootstrapPlan, PackageManager};

use super::shared::{
    MediatedArms, bootstrap_via_brew_then_system, bootstrap_via_system_manager,
    command_failure_reason, detect_brew_system_method, partition_already_installed,
    pip_user_scripts_dir, pkg_run, planned_method_failed, planned_method_unavailable,
    planned_step_failed, resolve_tool_with_fallbacks, run_pkg_cmd, run_pkg_cmd_live, run_pkg_query,
    tool_cmd_with_resolver, tool_seam_var, upgrade_each,
};

pub struct PipxManager;

/// pipx's own bootstrap arm, reached when no brew/system mediator is present.
/// The ONE spelling, for the same reason npm has one: the planner resolves the
/// method against it and the cascade declines toward it.
const PIPX_FALLBACK_METHOD: &str = "pip";

/// What a mediator installs to deliver pipx — same role as npm's table.
const PIPX_MEDIATED: MediatedArms = MediatedArms {
    brew: Some("pipx"),
    arms: &[
        ("apt", &["pipx"]),
        ("dnf", &["pipx"]),
        // no-driven-route-ok: RHEL 7's repositories carry no pipx at all, and
        // yum is the manager only on releases that old.
        ("yum", &[]),
        // openSUSE ships a binary RPM per Python flavour; the `python3-` name is
        // the capability that resolves to the default flavour.
        ("zypper", &["python3-pipx"]),
        ("pacman", &["python-pipx"]),
        ("apk", &["pipx"]),
        ("pkg", &["devel/py-pipx"]),
        // winget publishes no pipx of its own, so this arm delivers the
        // interpreter and the `pip` arm below is what then installs pipx with
        // it. The same two-step shape cargo's winget arm takes for rustup.
        ("winget", &["Python.Python.3.13"]),
        ("chocolatey", &["pipx"]),
        ("scoop", &["pipx"]),
    ],
};

/// The one winget arm whose package is not pipx, named so the bootstrap and the
/// plan agree on which arm needs the pip step behind it.
const PIPX_WINGET_METHOD: &str = "winget";

/// The arm this run installs pipx through: the method the plan bound, or the
/// cascade's own answer for a caller outside a plan (`cfgd doctor`).
fn pipx_method<'a>(cx: &'a cfgd_core::providers::PackageContext<'_>) -> &'a str {
    cx.planned_method().unwrap_or_else(|| {
        detect_brew_system_method(&PIPX_MEDIATED, PIPX_FALLBACK_METHOD, &|_| false)
    })
}

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

// The names the pip fallback looks for, most likely first. A CPython install on
// Windows writes `pip.exe` into its Scripts directory and no `pip3` alias, while
// a Linux distribution ships `pip3` and often reserves `pip` for Python 2.
fn pip_tool_order() -> [&'static str; 2] {
    if cfg!(windows) {
        ["pip", "pip3"]
    } else {
        ["pip3", "pip"]
    }
}

// The tool the pip fallback would run: whichever of the two above is present,
// else the preferred name. Shared by `bootstrap_plan` and `path_dirs` so
// both always name the same interpreter.
fn pipx_pip_tool() -> &'static str {
    let order = pip_tool_order();
    order
        .into_iter()
        .find(|t| command_available(t))
        .unwrap_or(order[0])
}

/// How this run reaches pip: the program to spawn, the arguments that come
/// before pip's own, and the name a message calls it by.
///
/// The Windows `py` launcher is the one route that is not a pip of its own, so
/// it carries the `-m pip` that makes it one.
struct PipRoute {
    program: PathBuf,
    leading: &'static [&'static str],
    tool: &'static str,
}

impl PipRoute {
    fn direct(tool: &'static str, program: PathBuf) -> Self {
        PipRoute {
            program,
            leading: &[],
            tool,
        }
    }

    fn launcher(program: PathBuf) -> Self {
        PipRoute {
            program,
            leading: &["-m", "pip"],
            tool: "py -m pip",
        }
    }

    /// The directory holding the interpreter's own console scripts, which is
    /// where this pip was found. `None` for the launcher, which lives with
    /// Windows rather than with any interpreter.
    fn tools_dir(&self) -> Option<&std::path::Path> {
        if self.leading.is_empty() {
            self.program.parent()
        } else {
            None
        }
    }

    fn command(&self) -> Command {
        let program = self.program.clone();
        let mut cmd = tool_cmd_with_resolver("pip", move || Some(program));
        cmd.args(self.leading);
        cmd
    }
}

/// Where this run's pip is, or `None` when the machine has none.
///
/// The seam answers ahead of every probe: the name walk below would otherwise
/// resolve the host's own pip before a test's planted one was ever reached.
/// After that it is `$PATH` and the directories a bootstrap registered, then the
/// places a Windows Python install leaves pip without touching either.
fn find_pip() -> Option<PipRoute> {
    if let Ok(seam) = std::env::var(tool_seam_var("pip")) {
        let planted = PathBuf::from(seam);
        if planted.is_file() {
            return Some(PipRoute::direct("pip", planted));
        }
    }
    let fallbacks = pip_fallbacks();
    pip_tool_order()
        .into_iter()
        .find_map(|tool| {
            resolve_tool_with_fallbacks(tool, &fallbacks).map(|path| PipRoute::direct(tool, path))
        })
        .or_else(|| windows_py_launcher().map(PipRoute::launcher))
}

fn pip_fallbacks() -> Vec<PathBuf> {
    if cfg!(windows) {
        windows_pip_candidates()
    } else {
        Vec::new()
    }
}

/// Every `pip.exe` a Windows Python install could have left under
/// `%LOCALAPPDATA%\Programs\Python`, which is where the winget and Microsoft
/// Store interpreters land.
///
/// The tree is READ rather than composed: the directory carries the
/// interpreter's own minor version, and the arm that just installed it edits the
/// user's `PATH` in the registry, which this process cannot see.
pub(super) fn windows_pip_candidates() -> Vec<PathBuf> {
    let Some(root) =
        std::env::var_os("LOCALAPPDATA").map(|a| PathBuf::from(a).join("Programs").join("Python"))
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path().join("Scripts").join("pip.exe"))
        .collect()
}

/// The `py` launcher the Python installer puts beside Windows itself, the one
/// route to pip that does not depend on which minor was installed or on where
/// its `Scripts` directory ended up.
pub(super) fn windows_py_launcher() -> Option<PathBuf> {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let launcher = PathBuf::from(root).join("py.exe");
    launcher.is_file().then_some(launcher)
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

    fn upgrade_verb(&self) -> Option<&'static str> {
        Some("upgrade")
    }

    fn tool_version(&self) -> Option<String> {
        super::shared::tool_version_from(pipx_cmd().arg("--version"))
    }

    fn is_available(&self) -> bool {
        pipx_available()
    }

    fn bootstrap_plan_given(&self, delivered: &dyn Fn(&str) -> bool) -> Option<BootstrapPlan> {
        match detect_brew_system_method(&PIPX_MEDIATED, PIPX_FALLBACK_METHOD, delivered) {
            // Only the pip fallback installs into the user's own tree; brew and
            // the system managers land pipx on the system PATH.
            // The tool the pip arm would run: whichever is present, else the
            // preferred name. Naming it even when it is absent is what lets the
            // planner say WHY pipx cannot be provisioned instead of dropping it
            // — `pip3` is not installable under that name from any system
            // manager, so `feasible_bootstrap_plan` still answers `None`.
            // every-platform-ok: the pip arm spawns `pip3`/`pip` itself with no
            // shell in between, and every mediated arm is named only because
            // the probe above found its mediator on this host.
            "pip" => Some(
                BootstrapPlan::new("pip")
                    .requiring([pipx_pip_tool()])
                    .creating(pipx_pip_scripts_dir()),
            ),
            // The winget arm installs a Python interpreter, not pipx, so the
            // run does not end there: the pip step behind it lands pipx in the
            // user's own scripts directory, which is the directory this plan
            // promises. It names no required tool because the arm itself
            // delivers the `pip` that step runs.
            PIPX_WINGET_METHOD => {
                Some(BootstrapPlan::new(PIPX_WINGET_METHOD).creating(pipx_pip_scripts_dir()))
            }
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
        match pipx_method(cx) {
            // The winget arm joins the pip arm here: winget delivers the
            // interpreter and pip puts pipx in the user's scripts directory.
            PIPX_FALLBACK_METHOD | PIPX_WINGET_METHOD => pipx_pip_scripts_dir()
                .into_iter()
                .map(cfgd_core::to_posix_string)
                .collect(),
            // brew/system installs land pipx on the system PATH; nothing new
            // to declare.
            _ => Vec::new(),
        }
    }

    fn bootstrap(&self, cx: &cfgd_core::providers::PackageContext<'_>) -> Result<()> {
        if pipx_method(cx) == PIPX_WINGET_METHOD {
            bootstrap_via_system_manager(cx, &PIPX_MEDIATED, "pipx")?;
            // The interpreter winget just installed put `pip` on the machine,
            // and the pip step below resolves it through the memoized
            // `command_path`, which still holds the miss from before the arm ran.
            cfgd_core::invalidate_command_resolution();
        } else if bootstrap_via_brew_then_system(cx, "pipx", &PIPX_MEDIATED, PIPX_FALLBACK_METHOD)?
        {
            // Returns false without probing anything when the plan named `pip`,
            // pipx's own fallback arm, which is the next thing below.
            return Ok(());
        }

        // Fall back to pip. Resolved to a full path rather than spawned by bare
        // name: the winget arm above leaves an interpreter whose directory a
        // Windows installer adds to the user's registry `PATH`, which this
        // process cannot see, so a bare-name spawn would miss what just landed.
        let Some(pip) = find_pip() else {
            return Err(match cx.planned_method() {
                Some(PIPX_WINGET_METHOD) => planned_step_failed(
                    "pipx",
                    PIPX_WINGET_METHOD,
                    "pip",
                    "it is on neither PATH nor any directory the interpreter creates",
                ),
                Some(method) => planned_method_unavailable("pipx", method),
                None => PackageError::BootstrapFailed {
                    manager: "pipx".into(),
                    message: "no method available to install pipx".into(),
                },
            }
            .into());
        };
        // The interpreter's own scripts directory, so the rest of this run
        // resolves the pip it holds and, once the install below lands, the pipx
        // beside it. `command_path` searches what a bootstrap registered as well
        // as `$PATH`.
        if let Some(dir) = pip.tools_dir() {
            cfgd_core::register_bootstrapped_path_dirs(&[cfgd_core::to_posix_string(dir)]);
        }

        let label = format!("Installing pipx via {}", pip.tool);
        let result = pkg_run(
            cx,
            pip.command().args(["install", "--user", "pipx"]),
            &label,
        )
        .map_err(|e| PackageError::BootstrapFailed {
            manager: "pipx".into(),
            message: format!("{} install failed: {}", pip.tool, e),
        })?;
        if !result.status.success() {
            return Err(match cx.planned_method() {
                // The mediator delivered the interpreter it packages; pip is
                // what then failed, so the refusal names pip rather than sending
                // the reader to check a winget that worked.
                Some(PIPX_WINGET_METHOD) => planned_step_failed(
                    "pipx",
                    PIPX_WINGET_METHOD,
                    pip.tool,
                    &command_failure_reason(&result),
                ),
                Some(method) => planned_method_failed("pipx", method, &result),
                None => PackageError::BootstrapFailed {
                    manager: "pipx".into(),
                    message: format!("{} install --user pipx failed", pip.tool),
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
        if packages.is_empty() {
            return Ok(());
        }
        let (held, fresh) = partition_already_installed(self, packages, cx);
        for pkg in &fresh {
            let label = format!("pipx install {}", pkg);
            run_pkg_cmd_live(
                cx,
                "pipx",
                pipx_cmd().args(["install", pkg]),
                &label,
                "install",
            )?;
        }
        upgrade_each(cx, "pipx", &held, "pipx upgrade", |pkg| {
            let mut cmd = pipx_cmd();
            cmd.args(["upgrade", pkg]);
            cmd
        })?;
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
        let output = run_pkg_query("pipx", Command::new("curl").args(["-fsSL", &url]))?;
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
                .unwrap_or(cfgd_core::providers::UNKNOWN_PACKAGE_VERSION)
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
    use cfgd_core::providers::PackageManager;
    use cfgd_core::providers::PackageManagerExt;

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

    /// Every arm pipx can be planned through, judged against pipx's own
    /// declaration rather than a list of manager names typed here: the arms
    /// `PIPX_MEDIATED` populates for this host's own manager table, plus brew,
    /// plus the pip arm that answers when none of them is present.
    #[test]
    fn pipx_is_planned_only_through_an_arm_this_host_can_run() {
        // The probes below assert what THIS host resolves, so hold the read
        // guard: a sibling test empties PATH under the write guard.
        let _path = cfgd_core::test_helpers::path_env_read_guard();
        // The plan always exists: the cascade's pip fallback names the tool it
        // would need even when no pip is present, so the planner can say WHY
        // pipx cannot be provisioned (`feasible_bootstrap_plan` answers the
        // `None`).
        let plan = PipxManager
            .bootstrap_plan()
            .expect("pipx plans on every host via the pip fallback");
        let mediators: Vec<&str> = super::super::shared::host_arms()
            .iter()
            .filter(|(method, _)| PIPX_MEDIATED.system_packages_for(method).is_some())
            .filter(|(_, tool)| super::super::shared::system_tool_available(tool))
            .map(|(method, _)| *method)
            .collect();
        // The user tree is not the same directory on every platform: Windows
        // sends console scripts to CPython's `nt_user` scheme under roaming
        // AppData.
        let is_user_scripts_dir = |d: &String| {
            if cfg!(windows) {
                d.contains("/Python/Python") && d.ends_with("/Scripts")
            } else {
                d.ends_with("/.local/bin")
            }
        };
        if !brew_available() && mediators.is_empty() {
            assert_eq!(
                plan.method, PIPX_FALLBACK_METHOD,
                "no mediator on this host packages pipx, so the pip arm is the only route left"
            );
        }
        match plan.method.as_str() {
            // Only the pip arm installs into the user's own tree
            // (`pip install --user`), and it is the one arm that names the tool
            // it spawns.
            PIPX_FALLBACK_METHOD => {
                // The names, not `pipx_pip_tool()`: the plan IS that function's
                // value, so reading it back here would compare the producer
                // with itself and let the arm name a tool no pip is called.
                assert_eq!(plan.requires.len(), 1, "{:?}", plan.requires);
                assert!(
                    ["pip3", "pip"].contains(&plan.requires[0].as_str()),
                    "the pip arm names the interpreter's own pip: {:?}",
                    plan.requires
                );
                assert!(
                    plan.creates_path_dirs.iter().all(is_user_scripts_dir),
                    "{:?}",
                    plan.creates_path_dirs
                );
            }
            // winget publishes no pipx, so its arm delivers the interpreter and
            // the pip step behind it lands pipx in that same user tree.
            PIPX_WINGET_METHOD => {
                assert!(
                    mediators.contains(&PIPX_WINGET_METHOD),
                    "winget was planned on a host whose own probe does not find it"
                );
                assert!(plan.requires.is_empty());
                assert!(
                    plan.creates_path_dirs.iter().all(is_user_scripts_dir),
                    "{:?}",
                    plan.creates_path_dirs
                );
            }
            // brew and every other mediator land pipx on the system PATH, so
            // they declare no directory and name no tool of their own.
            method => {
                assert!(
                    method == "brew" || mediators.contains(&method),
                    "pipx was planned through {method}, which this host cannot run"
                );
                assert!(plan.requires.is_empty());
                assert!(plan.creates_path_dirs.is_empty());
            }
        }
    }

    /// The pip arm's prerequisite, as the names pip actually goes by rather
    /// than as whatever the arm currently declares: a plan naming a tool no pip
    /// is called would be approved and then die looking for it.
    ///
    /// Driven with nothing on `PATH` and brew seamed to a file that is not
    /// there, because the cascade reaches this arm only where no mediator
    /// answers, and a host carrying brew or apt would never run the comparison.
    #[test]
    #[serial_test::serial]
    fn the_pip_arm_requires_a_tool_pip_is_actually_called() {
        let _path_excl = cfgd_core::test_helpers::path_env_mutation_guard();
        let _path = cfgd_core::test_helpers::EnvVarGuard::set("PATH", "");
        let _no_brew = cfgd_core::test_helpers::EnvVarGuard::set(
            "CFGD_BREW_BIN",
            "/nonexistent/cfgd-no-brew-on-this-host",
        );
        let plan = PipxManager
            .bootstrap_plan()
            .expect("the pip arm is the route left when no mediator answers");
        assert_eq!(plan.method, PIPX_FALLBACK_METHOD, "{plan:?}");
        assert_eq!(plan.requires.len(), 1, "{:?}", plan.requires);
        assert!(
            ["pip3", "pip"].contains(&plan.requires[0].as_str()),
            "the pip arm names the interpreter's own pip: {:?}",
            plan.requires
        );
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

    /// A winget or Microsoft Store interpreter lands under
    /// `%LOCALAPPDATA%\Programs\Python`, and the minor in that path belongs to
    /// whichever one was installed, so the tree is read rather than composed.
    #[test]
    #[serial_test::serial]
    fn windows_pip_candidates_come_from_the_programs_python_tree() {
        let local = tempfile::tempdir().unwrap();
        let scripts = local
            .path()
            .join("Programs")
            .join("Python")
            .join("Python313")
            .join("Scripts");
        std::fs::create_dir_all(&scripts).unwrap();
        let _guard = cfgd_core::test_helpers::EnvVarGuard::set(
            "LOCALAPPDATA",
            local.path().to_string_lossy().as_ref(),
        );

        assert_eq!(
            windows_pip_candidates(),
            vec![scripts.join("pip.exe")],
            "the pip an installed interpreter carries must be findable off PATH"
        );
    }

    #[test]
    #[serial_test::serial]
    fn windows_pip_candidates_are_empty_without_a_programs_python_tree() {
        let local = tempfile::tempdir().unwrap();
        let _guard = cfgd_core::test_helpers::EnvVarGuard::set(
            "LOCALAPPDATA",
            local.path().to_string_lossy().as_ref(),
        );

        assert!(windows_pip_candidates().is_empty());
    }

    /// The launcher is the route that survives not knowing which minor was
    /// installed, and it is a route only when it is really there.
    #[test]
    #[serial_test::serial]
    fn the_py_launcher_is_read_from_the_windows_directory() {
        let root = tempfile::tempdir().unwrap();
        let _guard = cfgd_core::test_helpers::EnvVarGuard::set(
            "SystemRoot",
            root.path().to_string_lossy().as_ref(),
        );
        assert_eq!(
            windows_py_launcher(),
            None,
            "a Windows directory with no launcher in it is no route to pip"
        );

        let launcher = root.path().join("py.exe");
        std::fs::write(&launcher, "").unwrap();
        assert_eq!(windows_py_launcher(), Some(launcher));
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
            // One listing (which packages does pipx already hold, so a held
            // one is raised rather than re-installed) plus one install per
            // package.
            assert_eq!(s.invocation_count(), 3, "one pipx invocation per pkg");
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
