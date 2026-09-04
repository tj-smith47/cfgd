//! Go install package manager (`go install <module>@version`).

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use cfgd_core::PathDisplayExt;
use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Role;
use cfgd_core::providers::{BootstrapPlan, PackageManager};

use super::shared::{
    MediatedArms, bootstrap_brew_arm, bootstrap_via_system_manager, detect_go_bootstrap_method,
    resolve_tool_with_fallbacks, run_pkg_cmd_live, run_pkg_query, system_manager_arms,
    tool_cmd_with_resolver,
};

pub struct GoInstallManager;

/// What a mediator installs to deliver the Go toolchain: brew calls it `go`,
/// every system manager calls it `golang`.
const GO_MEDIATED: MediatedArms = system_manager_arms(Some("go"), &["golang"]);

fn go_fallbacks() -> Vec<PathBuf> {
    let mut fallbacks = vec![
        PathBuf::from("/usr/local/go/bin/go"),
        PathBuf::from("/usr/local/bin/go"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        fallbacks.push(PathBuf::from(home).join("go/bin/go"));
    }
    fallbacks
}

pub(super) fn find_go() -> Option<PathBuf> {
    resolve_tool_with_fallbacks("go", &go_fallbacks())
}

pub(super) fn go_available() -> bool {
    find_go().is_some()
}

pub(super) fn go_cmd() -> Command {
    tool_cmd_with_resolver("go", find_go)
}

/// Where `go install` puts a binary — the ONE answer `installed_packages` and
/// `uninstall` both read, because the location the query scans and the
/// location the install writes to have to be the same directory or every
/// installed package is reported missing on every plan and reinstalled
/// forever.
///
/// The question is put to `go` itself rather than to the environment. `GOBIN`
/// outranks `$GOPATH/bin`, and both can be set three ways — the process
/// environment, `go env -w`'s config file, and the toolchain's own default —
/// of which only the first is visible to `env::var`. `go env` reports the
/// EFFECTIVE values, which is exactly what `go install` will act on.
///
/// A toolchain that cannot be spawned or answers nothing falls back to
/// `$GOPATH/bin`, then `~/go/bin`: the query then scans a directory that may
/// be the wrong one, but reporting nothing installed is the same answer a
/// missing directory already gives.
pub(super) fn go_bin_dir() -> PathBuf {
    let reported = run_pkg_query("go", go_cmd().args(["env", "GOBIN", "GOPATH"]))
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut lines = stdout.lines().map(str::trim);
            (
                lines.next().unwrap_or_default().to_string(),
                lines.next().unwrap_or_default().to_string(),
            )
        });
    let (gobin, gopath) = reported.unwrap_or_default();
    if !gobin.is_empty() {
        return PathBuf::from(gobin);
    }
    if !gopath.is_empty() {
        return PathBuf::from(gopath).join("bin");
    }
    fallback_go_bin_dir()
}

/// `$GOPATH/bin`, or `~/go/bin` — the location Go documents as the default,
/// used only when the toolchain itself could not be asked.
fn fallback_go_bin_dir() -> PathBuf {
    let gopath = std::env::var("GOPATH").ok().unwrap_or_else(|| {
        cfgd_core::expand_tilde(std::path::Path::new("~/go"))
            .to_string_lossy()
            .to_string()
    });
    PathBuf::from(gopath).join("bin")
}

impl PackageManager for GoInstallManager {
    fn name(&self) -> &str {
        "go"
    }

    fn upgrade_verb(&self) -> Option<&'static str> {
        // `go install <path>@<version>` always overwrites the binary — there
        // is no held/fresh split (see `install` below), so `install` itself
        // is the verb that raises an already-held one.
        Some("install")
    }

    fn tool_version(&self) -> Option<String> {
        super::shared::tool_version_from(go_cmd().arg("version"))
    }

    fn is_available(&self) -> bool {
        go_available()
    }

    fn bootstrap_plan_given(&self, delivered: &dyn Fn(&str) -> bool) -> Option<BootstrapPlan> {
        // The toolchain lands on the system PATH whichever manager installs it,
        // so the plan creates no directory of its own.
        //
        // Feasibility and method come from ONE probe of the mediators
        // `bootstrap` can actually spawn — brew, then apt/dnf/zypper. Asking a
        // wider question (is ANY system manager present?) and then naming a
        // fallback answered `via dnf` on a winget-only Windows host: a
        // mediator that cannot run, which under a binding plan is a guaranteed
        // failure rather than a provision. `go` has no bootstrap arm of its
        // own, so when none of them is present there is no plan.
        detect_go_bootstrap_method(delivered).map(BootstrapPlan::new)
    }

    fn bootstrap(&self, cx: &cfgd_core::providers::PackageContext<'_>) -> Result<()> {
        // Returns false without running brew when the plan named a system
        // manager, and errors rather than falling through when it named brew.
        if bootstrap_brew_arm(cx, "go", GO_MEDIATED.brew.unwrap_or("go"))? {
            return Ok(());
        }

        bootstrap_via_system_manager(cx, GO_MEDIATED.system[0], "go")
    }

    fn mediated_packages(&self, via: &str) -> Option<Vec<String>> {
        GO_MEDIATED.packages_for(via)
    }

    fn installed_packages(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<HashSet<String>> {
        Ok(scan_go_bin_dir(&go_bin_dir()))
    }

    /// `go install` leaves no listing of its own — the binary directory is
    /// scanned again, and every binary's module version is read in ONE
    /// `go version -m <p1> <p2> …` spawn rather than one per binary, which
    /// turned a bin dir of N tools into N process spawns on every plan and
    /// verify.
    fn installed_packages_with_versions(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        let bin_dir = go_bin_dir();
        let mut names: Vec<String> = scan_go_bin_dir(&bin_dir).into_iter().collect();
        if names.is_empty() {
            return Ok(Vec::new());
        }
        names.sort();
        let paths: Vec<String> = names
            .iter()
            .map(|name| bin_dir.join(name).to_string_lossy().into_owned())
            .collect();
        // `go version -m` exits 1 when ANY argument is not a Go binary (a
        // script or a hand-copied tool in `$GOBIN`) while still printing every
        // readable block, so the exit status says nothing about the blocks
        // that did come back: parse what arrived, and let the batch parser's
        // own omission mark the unreadable ones unknown.
        let versions = run_pkg_query("go", go_cmd().arg("version").arg("-m").args(&paths))
            .ok()
            .map(|output| {
                parse_go_version_m_batch(&String::from_utf8_lossy(&output.stdout), &paths)
            })
            .unwrap_or_default();
        Ok(names
            .into_iter()
            .zip(paths)
            .map(|(name, path)| {
                let version = versions
                    .get(&path)
                    .cloned()
                    .unwrap_or_else(|| cfgd_core::providers::UNKNOWN_PACKAGE_VERSION.to_string());
                cfgd_core::providers::PackageInfo { name, version }
            })
            .collect())
    }

    fn install(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        for pkg in packages {
            // `go install` requires a full module path with @version
            let install_path = go_install_path(pkg);
            let label = format!("go install {}", install_path);
            run_pkg_cmd_live(
                cx,
                "go",
                go_cmd().args(["install", &install_path]),
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
        // Go has no uninstall command; remove the binaries from wherever
        // `go install` put them.
        let bin_dir = go_bin_dir();
        for pkg in packages {
            // Derive the binary name from the module path (idempotent if `pkg`
            // is already a bare binary name from the prune path), then re-validate
            // it carries no path separators to prevent traversal.
            let raw_name = go_binary_name(pkg);
            let bin_name = std::path::Path::new(&raw_name)
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| PackageError::UninstallFailed {
                    manager: "go".into(),
                    message: format!("invalid binary name derived from package: {}", pkg),
                })?;
            let bin_path = bin_dir.join(bin_name);
            if bin_path.exists() {
                cx.report(Role::Info, "go", format!("removing {}", bin_path.posix()));
                std::fs::remove_file(&bin_path).map_err(|e| PackageError::UninstallFailed {
                    manager: "go".into(),
                    message: format!("failed to remove {}: {}", bin_path.posix(), e),
                })?;
            }
        }
        Ok(())
    }

    fn package_identity(&self, entry: &str) -> String {
        go_binary_name(entry)
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // go list -m -json <pkg>@latest → parse "Version" field
        let output = run_pkg_query(
            "go",
            go_cmd().args(["list", "-m", "-json", &format!("{}@latest", package)]),
        )?;
        if !output.status.success() {
            return Ok(None);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_go_module_version(&stdout))
    }
}

/// Scan `<gopath>/bin` and return the file names (binary names) it contains,
/// directories excluded — `go install` never writes one, so a stray
/// subdirectory is not a package. Returns an empty set when the directory is
/// missing or unreadable. Split out so tests can drive the scan against a
/// tempdir without mutating `$GOPATH` or `$HOME`.
pub(super) fn scan_go_bin_dir(bin_dir: &std::path::Path) -> HashSet<String> {
    let mut packages = HashSet::new();
    if let Ok(entries) = std::fs::read_dir(bin_dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                packages.insert(name.to_string());
            }
        }
    }
    packages
}

/// Derive the binary name `go install` produces from a module-path entry: the
/// last `/`-segment after stripping any `@<version>` suffix
/// (`rsc.io/2fa@v1.2.0` → `2fa`). This is what `installed_packages()` reports
/// (it scans `$GOPATH/bin` for binary names), so it is the identity used for
/// install-diffing, prune, and the per-package tracking key.
pub(super) fn go_binary_name(entry: &str) -> String {
    let without_version = entry.split('@').next().unwrap_or(entry);
    without_version
        .rsplit('/')
        .next()
        .unwrap_or(without_version)
        .to_string()
}

/// Derive the `go install` argument from a user-supplied package reference:
/// pin already-versioned refs as-is, and append `@latest` to bare module paths.
pub(super) fn go_install_path(pkg: &str) -> String {
    if pkg.contains('@') {
        pkg.to_string()
    } else {
        format!("{}@latest", pkg)
    }
}

/// Parse version from `go list -m -json pkg@latest` output.
/// JSON format: `{"Version": "v1.2.3", ...}`
/// Strips the "v" prefix for consistency.
pub(super) fn parse_go_module_version(output: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(output).ok()?;
    let version = parsed.get("Version").and_then(|v| v.as_str())?;
    let version = version.strip_prefix('v').unwrap_or(version);
    Some(version.to_string())
}

/// Parse the module version off `go version -m <binary>` output. The `mod`
/// line's second field is the module's version at build time (the `v`
/// prefix stripped for consistency with every other version slot); `None`
/// when the binary carries no `mod` line (a `go build` output with no
/// embedded module info, or an unreadable binary).
pub(super) fn parse_go_version_m_module_version(output: &str) -> Option<String> {
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        if fields.next() == Some("mod") {
            let _module_path = fields.next()?;
            let version = fields.next()?;
            return Some(version.strip_prefix('v').unwrap_or(version).to_string());
        }
    }
    None
}

/// Split a multi-file `go version -m <p1> <p2> …` transcript back into its
/// per-binary blocks and parse each one's module version. Each block opens
/// on a line naming one of `paths` followed by `:` (`<path>: go1.21.5`); a
/// path this host queried but that answers no `mod` line (a plain `go build`
/// with no embedded module info) is simply absent from the result, which the
/// caller reads as "unknown version" the same way a single-file lookup would.
pub(super) fn parse_go_version_m_batch(
    output: &str,
    paths: &[String],
) -> std::collections::HashMap<String, String> {
    let path_set: std::collections::HashSet<&str> = paths.iter().map(String::as_str).collect();
    let mut versions = std::collections::HashMap::new();
    let mut current: Option<&str> = None;
    let mut block = String::new();
    let flush = |current: Option<&str>,
                 block: &str,
                 versions: &mut std::collections::HashMap<String, String>| {
        if let (Some(path), Some(version)) = (current, parse_go_version_m_module_version(block)) {
            versions.insert(path.to_string(), version);
        }
    };
    for line in output.lines() {
        // Every line of a block but its header is tab-indented, and the header
        // is `<path>: go<version>` with the path echoed verbatim — so a header
        // is a non-indented line whose text before the first `: ` is one of
        // the queried paths (the first `: ` — a Windows path's own `:` is
        // followed by `\`, and a devel toolchain's header carries a timestamp
        // with colons of its own after the separator).
        let header = (!line.starts_with('\t'))
            .then(|| line.split_once(": "))
            .flatten()
            .map(|(path, _)| path)
            .filter(|path| path_set.contains(path));
        if let Some(path) = header {
            flush(current, &block, &mut versions);
            current = Some(path);
            block.clear();
        } else {
            block.push_str(line);
            block.push('\n');
        }
    }
    flush(current, &block, &mut versions);
    versions
}

#[cfg(test)]
mod tests {
    use cfgd_core::providers::PackageManager;
    use cfgd_core::providers::PackageManagerExt;

    use super::*;

    #[test]
    fn parse_go_version_m_module_version_real_world() {
        let output = "/home/user/go/bin/gopls: go1.21.5\n\
                       \tpath\tgolang.org/x/tools/gopls\n\
                       \tmod\tgolang.org/x/tools/gopls\tv0.15.3\th1:abcdef=\n\
                       \tdep\tgolang.org/x/mod\tv0.14.0\th1:xyz=\n\
                       \tbuild\t-compiler=gc\n";
        assert_eq!(
            parse_go_version_m_module_version(output),
            Some("0.15.3".to_string())
        );
    }

    #[test]
    fn parse_go_version_m_module_version_no_mod_line() {
        let output = "/home/user/go/bin/tool: go1.21.5\n\tbuild\t-compiler=gc\n";
        assert_eq!(parse_go_version_m_module_version(output), None);
    }

    #[test]
    fn parse_go_version_m_batch_matches_each_path_to_its_own_block() {
        let output = "/go/bin/a: go1.21.5\n\
                       \tpath\texample.com/a\n\
                       \tmod\texample.com/a\tv1.2.3\th1:aaa=\n\
                       /go/bin/b: go1.21.5\n\
                       \tpath\texample.com/b\n\
                       \tmod\texample.com/b\tv4.5.6\th1:bbb=\n";
        let paths = vec!["/go/bin/a".to_string(), "/go/bin/b".to_string()];
        let versions = parse_go_version_m_batch(output, &paths);
        assert_eq!(versions.get("/go/bin/a").map(String::as_str), Some("1.2.3"));
        assert_eq!(versions.get("/go/bin/b").map(String::as_str), Some("4.5.6"));
    }

    #[test]
    fn parse_go_version_m_batch_omits_a_path_with_no_mod_line() {
        let output = "/go/bin/a: go1.21.5\n\tbuild\t-compiler=gc\n\
                       /go/bin/b: go1.21.5\n\
                       \tpath\texample.com/b\n\
                       \tmod\texample.com/b\tv4.5.6\th1:bbb=\n";
        let paths = vec!["/go/bin/a".to_string(), "/go/bin/b".to_string()];
        let versions = parse_go_version_m_batch(output, &paths);
        assert!(!versions.contains_key("/go/bin/a"));
        assert_eq!(versions.get("/go/bin/b").map(String::as_str), Some("4.5.6"));
    }

    /// A Windows path's own drive-letter colon (`C:\Users\...`) is not the
    /// header delimiter — it is followed by `\`, never a space, so the FIRST
    /// `: ` on a non-indented line is unambiguously the true separator.
    #[test]
    fn parse_go_version_m_batch_matches_a_windows_path_header() {
        let output = "C:\\Users\\u\\go\\bin\\gopls.exe: go1.21.5\n\
                       \tpath\tgolang.org/x/tools/gopls\n\
                       \tmod\tgolang.org/x/tools/gopls\tv0.15.3\th1:xyz=\n";
        let paths = vec!["C:\\Users\\u\\go\\bin\\gopls.exe".to_string()];
        let versions = parse_go_version_m_batch(output, &paths);
        assert_eq!(
            versions
                .get("C:\\Users\\u\\go\\bin\\gopls.exe")
                .map(String::as_str),
            Some("0.15.3")
        );
    }

    /// A devel/gotip toolchain's header carries a timestamp with colons of
    /// its own AFTER the true separator (`devel go1.23-2f0f7bd2c8 Wed Nov 15
    /// 20:33:44 2023 +0000`) — the first `: `, not the last, is what keeps
    /// this header matched to its own path rather than merged into the
    /// following block.
    #[test]
    fn parse_go_version_m_batch_matches_a_devel_toolchain_header() {
        let output = "/go/bin/gotip: devel go1.23-2f0f7bd2c8 Wed Nov 15 20:33:44 2023 +0000\n\
                       \tpath\texample.com/gotip\n\
                       \tmod\texample.com/gotip\tv0.1.0\th1:aaa=\n\
                       /go/bin/stable: go1.21.5\n\
                       \tpath\texample.com/stable\n\
                       \tmod\texample.com/stable\tv2.0.0\th1:bbb=\n";
        let paths = vec!["/go/bin/gotip".to_string(), "/go/bin/stable".to_string()];
        let versions = parse_go_version_m_batch(output, &paths);
        assert_eq!(
            versions.get("/go/bin/gotip").map(String::as_str),
            Some("0.1.0")
        );
        assert_eq!(
            versions.get("/go/bin/stable").map(String::as_str),
            Some("2.0.0")
        );
    }

    #[test]
    fn go_install_manager_name_and_traits() {
        let mgr = GoInstallManager;
        assert_eq!(mgr.name(), "go");
    }

    /// A bootstrap is an action like any other, so under a caller-owned status
    /// its shell-out settles no line of its own. `go`'s brew arm is the one
    /// that reached `Printer::run` directly instead of going through `pkg_run`,
    /// which rendered the bootstrap twice: once as the window's own line and
    /// once as the tree's.
    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn caller_owned_bootstrap_settles_no_line_of_its_own() {
        let _shim = cfgd_core::test_helpers::ToolShim::install("CFGD_BREW_BIN", 0, "", "");
        let settled = |transcript: &str| {
            cfgd_core::test_helpers::settled_status_lines(&cfgd_core::output::strip_ansi(
                transcript,
            ))
            .len()
        };

        let notes = cfgd_core::providers::NoteSink::default();
        let (printer, buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        GoInstallManager
            .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context_with_notes(
                &printer, &notes,
            ))
            .expect("brew shim exits 0");
        let standalone = cfgd_core::test_helpers::captured_text(&buf);
        assert_eq!(
            settled(&standalone),
            1,
            "standalone, the window IS the bootstrap's only line: {standalone}"
        );

        let owned_notes = cfgd_core::providers::NoteSink::default();
        let (owned_printer, owned_buf) =
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
        GoInstallManager
            .bootstrap(
                &cfgd_core::test_helpers::test_bootstrap_context_with_notes(
                    &owned_printer,
                    &owned_notes,
                )
                .caller_owns_status(),
            )
            .expect("brew shim exits 0");
        let owned = cfgd_core::test_helpers::captured_text(&owned_buf);
        assert_eq!(
            settled(&owned),
            0,
            "the reconciler renders the bootstrap's line; the window must settle silently: {owned}"
        );
    }

    #[test]
    fn parse_go_module_version_strips_v_prefix() {
        let output = r#"{"Path":"golang.org/x/tools/gopls","Version":"v0.15.3"}"#;
        assert_eq!(parse_go_module_version(output), Some("0.15.3".to_string()));
    }

    #[test]
    fn parse_go_module_version_handles_pseudo_version() {
        // Go pseudo-versions include timestamps and commit hashes
        let output =
            r#"{"Path":"example.com/tool","Version":"v0.0.0-20240301120000-abcdef123456"}"#;
        assert_eq!(
            parse_go_module_version(output),
            Some("0.0.0-20240301120000-abcdef123456".to_string()),
            "should handle pseudo-versions with commit metadata"
        );
    }

    #[test]
    fn parse_go_module_version_extra_fields_ignored() {
        // Real go list -m output has many extra fields — only Version matters
        let output = r#"{"Path":"golang.org/x/tools","Version":"v0.20.0","Time":"2024-04-01T00:00:00Z","GoMod":"golang.org/x/tools@v0.20.0/go.mod"}"#;
        assert_eq!(parse_go_module_version(output), Some("0.20.0".to_string()));
    }

    #[test]
    fn parse_go_module_version_no_v_prefix() {
        // Unlikely but handles gracefully
        let output = r#"{"Path":"example.com/tool","Version":"1.0.0"}"#;
        assert_eq!(parse_go_module_version(output), Some("1.0.0".to_string()));
    }

    #[test]
    fn parse_go_module_version_invalid_json() {
        assert_eq!(parse_go_module_version("not json"), None);
    }

    #[test]
    fn parse_go_module_version_missing_version() {
        let output = r#"{"Path":"example.com/tool"}"#;
        assert_eq!(parse_go_module_version(output), None);
    }

    #[test]
    fn parse_go_module_version_empty_string() {
        assert_eq!(parse_go_module_version(""), None);
    }

    #[test]
    fn parse_go_module_version_null_version() {
        let output = r#"{"Path":"example.com/tool","Version":null}"#;
        assert_eq!(parse_go_module_version(output), None);
    }

    #[test]
    fn parse_go_module_version_real_world() {
        let output = r#"{
            "Path": "golang.org/x/tools/gopls",
            "Version": "v0.15.3",
            "Time": "2024-04-01T12:00:00Z",
            "GoMod": "golang.org/x/tools/gopls@v0.15.3/go.mod",
            "GoVersion": "1.21"
        }"#;
        assert_eq!(parse_go_module_version(output), Some("0.15.3".to_string()));
    }

    /// A plan's method is BINDING at execution, so `go` may only be planned
    /// through a mediator this host can spawn — and must not be dropped while
    /// one is present. Ground truth is spelled out here rather than read back
    /// from the detector, and probes the same seams the bootstrap spawns from.
    #[test]
    fn go_is_planned_only_through_a_mediator_this_host_can_actually_run() {
        let runnable = |tool: &str| {
            cfgd_core::command_available_with_seam(
                &format!("CFGD_{}_BIN", tool.to_uppercase().replace('-', "_")),
                tool,
            )
        };
        let brew = super::super::shared::brew_available();
        match GoInstallManager.bootstrap_plan() {
            Some(plan) => {
                // `bootstrap` installs the toolchain through brew or a system
                // manager, which put `go` on the system PATH — nothing to declare.
                let ok = match plan.method.as_str() {
                    "brew" => brew,
                    "apt" => runnable("apt-get"),
                    "dnf" => runnable("dnf"),
                    "zypper" => runnable("zypper"),
                    other => panic!("go planned through an unknown mediator: {other}"),
                };
                assert!(
                    ok,
                    "a plan may only name a mediator this host can run, got {}",
                    plan.method
                );
                assert!(plan.requires.is_empty());
                assert!(plan.creates_path_dirs.is_empty());
            }
            None => assert!(
                !brew && !["apt-get", "dnf", "zypper"].into_iter().any(runnable),
                "a runnable mediator must not be answered with no plan"
            ),
        }
    }

    #[test]
    #[serial_test::serial]
    fn go_install_manager_is_available_checks_go() {
        let mgr = GoInstallManager;
        let available = mgr.is_available();
        assert_eq!(available, go_available());
    }

    // --- scan_go_bin_dir ---

    #[test]
    fn scan_go_bin_dir_returns_file_names() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("gopls"), b"").unwrap();
        std::fs::write(dir.path().join("staticcheck"), b"").unwrap();
        let pkgs = scan_go_bin_dir(dir.path());
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.contains("gopls"));
        assert!(pkgs.contains("staticcheck"));
    }

    #[test]
    fn scan_go_bin_dir_excludes_subdirectories() {
        // `go install` never writes a directory into its bin dir, so a
        // stray one (a versioned SDK cache, an editor swap dir) is not a
        // package — the batched `go version -m` spawn would fail outright
        // if a directory path were handed to it as a binary.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("subdir")).unwrap();
        std::fs::write(dir.path().join("gopls"), b"").unwrap();
        let pkgs = scan_go_bin_dir(dir.path());
        assert!(pkgs.contains("gopls"));
        assert!(!pkgs.contains("subdir"));
    }

    #[test]
    fn scan_go_bin_dir_returns_empty_set_when_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        let pkgs = scan_go_bin_dir(&dir.path().join("nonexistent"));
        assert!(
            pkgs.is_empty(),
            "missing $GOPATH/bin must yield empty set, not error"
        );
    }

    #[test]
    fn scan_go_bin_dir_empty_dir_yields_empty_set() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan_go_bin_dir(dir.path()).is_empty());
    }

    // --- go_install_path ---

    // --- go_binary_name / package_identity ---

    #[test]
    fn go_binary_name_takes_last_segment() {
        assert_eq!(go_binary_name("rsc.io/2fa"), "2fa");
        assert_eq!(go_binary_name("golang.org/x/tools/gopls"), "gopls");
    }

    #[test]
    fn go_binary_name_strips_version() {
        assert_eq!(go_binary_name("rsc.io/2fa@v1.2.0"), "2fa");
        assert_eq!(go_binary_name("golang.org/x/tools/gopls@latest"), "gopls");
        assert_eq!(
            go_binary_name("example.com/pkg@v0.0.0-20240101000000-abcdef123456"),
            "pkg"
        );
    }

    #[test]
    fn go_binary_name_passthrough_for_bare_name() {
        // A bare binary name (from the prune path) maps to itself — idempotent.
        assert_eq!(go_binary_name("2fa"), "2fa");
    }

    #[test]
    fn go_package_identity_matches_binary_name() {
        // package_identity is the installed-DB identity; for go that is the
        // binary name, so install-diffing and prune compare like with like.
        let mgr = GoInstallManager;
        assert_eq!(mgr.package_identity("rsc.io/2fa@v1.2.0"), "2fa");
        assert_eq!(mgr.package_identity("golang.org/x/tools/gopls"), "gopls");
    }

    #[test]
    fn go_install_path_appends_at_latest_for_bare_module() {
        assert_eq!(
            go_install_path("golang.org/x/tools/gopls"),
            "golang.org/x/tools/gopls@latest"
        );
    }

    #[test]
    fn go_install_path_preserves_pinned_versions() {
        // User-supplied @version must round-trip unchanged so semver pins
        // (and pseudo-versions like @v0.0.0-20240301...-abcd) survive.
        assert_eq!(
            go_install_path("golang.org/x/tools/gopls@v0.15.0"),
            "golang.org/x/tools/gopls@v0.15.0"
        );
        assert_eq!(
            go_install_path("example.com/pkg@v0.0.0-20240101000000-abcdef123456"),
            "example.com/pkg@v0.0.0-20240101000000-abcdef123456"
        );
    }

    #[test]
    fn go_install_path_treats_at_anywhere_as_pre_pinned() {
        // The check is `contains('@')` — even if `@` is in the wrong place,
        // the input is left untouched (we trust the user's intent).
        assert_eq!(go_install_path("@oddly/placed"), "@oddly/placed");
    }

    // ---------------------------------------------------------------------
    // PackageManager-impl tests via CFGD_GO_BIN ToolShim.
    // ---------------------------------------------------------------------

    #[cfg(unix)]
    mod go_shim {
        use super::*;
        use cfgd_core::test_helpers::{ToolShim, test_package_context, test_printer, test_state};
        use serial_test::serial;

        const SHIM_ENV: &str = "CFGD_GO_BIN";

        /// A `go` whose `env GOBIN GOPATH` answers exactly these two values.
        /// Where `go install` puts a binary is the toolchain's answer, not the
        /// environment's, so a test about that directory states it here rather
        /// than inheriting whatever the host toolchain reports.
        fn go_env_shim(gobin: &str, gopath: &str) -> ToolShim {
            ToolShim::install(SHIM_ENV, 0, &format!("{gobin}\n{gopath}\n"), "")
        }

        /// `go install` writes to `$GOBIN` when the toolchain reports one, and
        /// `$GOPATH/bin` only when it does not — so the query has to ask `go`,
        /// not the environment. Reading `GOPATH` directly scanned a directory
        /// the install never wrote to, reporting every installed package
        /// missing on every plan and reinstalling it forever.
        #[test]
        #[serial]
        fn installed_packages_scans_the_bin_dir_go_itself_reports() {
            let gobin = tempfile::tempdir().expect("tempdir");
            std::fs::write(gobin.path().join("tool"), "").expect("seed installed binary");
            // A GOPATH whose `bin` holds nothing: if the scan reads the
            // environment instead of the toolchain, it finds an empty set.
            let gopath = tempfile::tempdir().expect("tempdir");
            let _gopath = cfgd_core::test_helpers::EnvVarGuard::set(
                "GOPATH",
                &gopath.path().to_string_lossy(),
            );
            let _s = ToolShim::install(
                SHIM_ENV,
                0,
                &format!("{}\n{}\n", gobin.path().display(), gopath.path().display()),
                "",
            );
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);

            let installed = GoInstallManager
                .installed_packages(&cx)
                .expect("installed_packages must succeed");

            assert!(
                installed.contains("tool"),
                "the query must scan the GOBIN `go install` writes to, got: {installed:?}"
            );
        }

        /// A bin dir of three binaries must cost ONE `go version -m` spawn,
        /// not three — the per-binary loop this replaces turned a 30-tool
        /// `$GOBIN` into 30 spawns on every plan and verify.
        #[test]
        #[serial]
        fn installed_packages_with_versions_batches_every_binary_into_one_spawn() {
            let gobin = tempfile::tempdir().expect("tempdir");
            for name in ["a", "b", "c"] {
                std::fs::write(gobin.path().join(name), "").expect("seed installed binary");
            }
            let shim_dir = tempfile::tempdir().expect("tempdir");
            let shim_path = cfgd_core::test_helpers::write_tool_shim(
                shim_dir.path(),
                "go",
                &[
                    cfgd_core::test_helpers::ShimArm::on(
                        "env GOBIN GOPATH",
                        &format!("{}\n\n", gobin.path().display()),
                    ),
                    cfgd_core::test_helpers::ShimArm::always("", "", 0),
                ],
            );
            let _shim_env =
                cfgd_core::test_helpers::EnvVarGuard::set(SHIM_ENV, &shim_path.to_string_lossy());
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);

            let infos = GoInstallManager
                .installed_packages_with_versions(&cx)
                .expect("installed_packages_with_versions must succeed");

            assert_eq!(infos.len(), 3, "every binary in the bin dir is reported");
            let argv =
                std::fs::read_to_string(shim_dir.path().join("argv.log")).unwrap_or_default();
            let version_calls = argv.lines().filter(|l| l.contains("version -m")).count();
            assert_eq!(
                version_calls, 1,
                "three binaries must be queried in ONE `go version -m` call, got: {argv}"
            );
        }

        /// `go version -m` exits 1 when ANY argument is not a Go binary while
        /// still printing every readable block on stdout — a `$GOBIN` file
        /// that is a script or a hand-copied tool must not blank the versions
        /// of the real binaries alongside it.
        #[test]
        #[serial]
        fn installed_packages_with_versions_keeps_every_readable_block_when_one_file_is_not_go() {
            let gobin = tempfile::tempdir().expect("tempdir");
            for name in ["dlv", "gopls", "notgo"] {
                std::fs::write(gobin.path().join(name), "").expect("seed installed binary");
            }
            let bin_dir = gobin.path().to_string_lossy().into_owned();
            let stdout = format!(
                "{bin_dir}/dlv: go1.21.5\n\
                 \tpath\tgithub.com/go-delve/delve/cmd/dlv\n\
                 \tmod\tgithub.com/go-delve/delve\tv1.21.0\th1:aaa=\n\
                 {bin_dir}/gopls: go1.21.5\n\
                 \tpath\tgolang.org/x/tools/gopls\n\
                 \tmod\tgolang.org/x/tools/gopls\tv0.15.3\th1:bbb=\n"
            );
            let shim_dir = tempfile::tempdir().expect("tempdir");
            let shim_path = cfgd_core::test_helpers::write_tool_shim(
                shim_dir.path(),
                "go",
                &[
                    cfgd_core::test_helpers::ShimArm::on(
                        "env GOBIN GOPATH",
                        &format!("{}\n\n", gobin.path().display()),
                    ),
                    cfgd_core::test_helpers::ShimArm {
                        matches: "version -m",
                        stdout: &stdout,
                        stderr: "notgo: could not read Go build info\n",
                        exit_code: 1,
                    },
                    cfgd_core::test_helpers::ShimArm::always("", "", 0),
                ],
            );
            let _shim_env =
                cfgd_core::test_helpers::EnvVarGuard::set(SHIM_ENV, &shim_path.to_string_lossy());
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);

            let infos = GoInstallManager
                .installed_packages_with_versions(&cx)
                .expect("installed_packages_with_versions must succeed");

            let by_name: std::collections::HashMap<&str, &str> = infos
                .iter()
                .map(|i| (i.name.as_str(), i.version.as_str()))
                .collect();
            assert_eq!(by_name.get("dlv"), Some(&"1.21.0"));
            assert_eq!(by_name.get("gopls"), Some(&"0.15.3"));
            assert_eq!(
                by_name.get("notgo"),
                Some(&cfgd_core::providers::UNKNOWN_PACKAGE_VERSION)
            );
        }

        #[test]
        #[serial]
        fn go_install_appends_at_latest_to_unversioned_package() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            GoInstallManager
                .install(&["github.com/example/tool".into()], &cx)
                .expect("Ok");
            assert!(
                s.argv_log()
                    .contains("install github.com/example/tool@latest"),
                "unversioned package gets @latest appended: {}",
                s.argv_log()
            );
        }

        #[test]
        #[serial]
        fn go_install_passes_through_pre_pinned_version() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            GoInstallManager
                .install(&["github.com/example/tool@v1.2.3".into()], &cx)
                .expect("Ok");
            assert!(
                s.argv_log()
                    .contains("install github.com/example/tool@v1.2.3"),
                "@version-pinned passes through: {}",
                s.argv_log()
            );
        }

        #[test]
        #[serial]
        fn go_install_runs_one_install_per_package() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            GoInstallManager
                .install(&["a.com/x".into(), "b.com/y".into()], &cx)
                .expect("Ok");
            assert_eq!(s.invocation_count(), 2);
        }

        #[test]
        #[serial]
        fn refreshing_the_index_declares_none_and_spawns_nothing() {
            let s = ToolShim::install(SHIM_ENV, 0, "", "");
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            assert!(
                !GoInstallManager.has_index(),
                "every install resolves against the remote, so there is no index to refresh"
            );
            GoInstallManager.refresh_index(&cx).expect("Ok");
            assert_eq!(
                s.invocation_count(),
                0,
                "a manager with no index must spawn nothing under a refresh"
            );
        }

        #[test]
        #[serial]
        fn go_available_version_strips_v_prefix_from_list_json_version_field() {
            // parse_go_module_version normalizes "v1.2.3" → "1.2.3" so versions
            // compare cleanly against profile entries (which don't include "v").
            let json = r#"{"Version":"v1.2.3","Path":"github.com/example/tool"}"#;
            let _s = ToolShim::install(SHIM_ENV, 0, json, "");
            let v = GoInstallManager
                .available_version("github.com/example/tool")
                .expect("Ok");
            assert_eq!(v.as_deref(), Some("1.2.3"));
        }

        #[test]
        #[serial]
        fn go_available_version_passes_list_m_json_with_at_latest() {
            let s = ToolShim::install(SHIM_ENV, 0, "{}", "");
            GoInstallManager
                .available_version("github.com/example/tool")
                .expect("Ok");
            assert!(
                s.argv_log().contains("list -m -json"),
                "argv must include `list -m -json`: {}",
                s.argv_log()
            );
            assert!(
                s.argv_log().contains("github.com/example/tool@latest"),
                "argv must append @latest: {}",
                s.argv_log()
            );
        }

        #[test]
        #[serial]
        fn go_available_version_returns_none_on_nonzero_exit() {
            let _s = ToolShim::install(SHIM_ENV, 1, "", "module not found");
            let v = GoInstallManager
                .available_version("nonexistent")
                .expect("non-zero → Ok(None)");
            assert_eq!(v, None);
        }

        #[test]
        #[serial]
        fn go_uninstall_removes_binary_from_gopath_bin() {
            let dir = tempfile::tempdir().unwrap();
            let bin_dir = dir.path().join("bin");
            std::fs::create_dir(&bin_dir).unwrap();
            std::fs::write(bin_dir.join("gopls"), b"fake-binary").unwrap();
            assert!(bin_dir.join("gopls").exists());

            let _s = go_env_shim("", dir.path().to_str().unwrap());
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            GoInstallManager
                .uninstall(&["golang.org/x/tools/gopls".into()], &cx)
                .expect("uninstall succeeds");
            assert!(
                !bin_dir.join("gopls").exists(),
                "binary must be removed from $GOPATH/bin"
            );
        }

        #[test]
        #[serial]
        fn go_uninstall_noop_when_binary_missing() {
            let dir = tempfile::tempdir().unwrap();
            let bin_dir = dir.path().join("bin");
            std::fs::create_dir(&bin_dir).unwrap();

            let _s = go_env_shim("", dir.path().to_str().unwrap());
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            GoInstallManager
                .uninstall(&["github.com/nonexistent/tool".into()], &cx)
                .expect("uninstall of missing binary is a no-op");
        }

        #[test]
        #[serial]
        fn go_uninstall_multiple_packages() {
            let dir = tempfile::tempdir().unwrap();
            let bin_dir = dir.path().join("bin");
            std::fs::create_dir(&bin_dir).unwrap();
            std::fs::write(bin_dir.join("gopls"), b"").unwrap();
            std::fs::write(bin_dir.join("staticcheck"), b"").unwrap();

            let _s = go_env_shim("", dir.path().to_str().unwrap());
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            GoInstallManager
                .uninstall(
                    &[
                        "golang.org/x/tools/gopls".into(),
                        "honnef.co/go/tools/cmd/staticcheck".into(),
                    ],
                    &cx,
                )
                .expect("multi-uninstall succeeds");
            assert!(!bin_dir.join("gopls").exists());
            assert!(!bin_dir.join("staticcheck").exists());
        }

        #[test]
        #[serial]
        fn go_installed_packages_scans_gopath_bin() {
            let dir = tempfile::tempdir().unwrap();
            let bin_dir = dir.path().join("bin");
            std::fs::create_dir(&bin_dir).unwrap();
            std::fs::write(bin_dir.join("gopls"), b"").unwrap();
            std::fs::write(bin_dir.join("dlv"), b"").unwrap();

            let _s = go_env_shim("", dir.path().to_str().unwrap());
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = GoInstallManager.installed_packages(&cx).expect("Ok");
            assert_eq!(pkgs.len(), 2);
            assert!(pkgs.contains("gopls"));
            assert!(pkgs.contains("dlv"));
        }

        #[test]
        #[serial]
        fn go_installed_packages_empty_when_no_bin_dir() {
            let dir = tempfile::tempdir().unwrap();
            let _s = go_env_shim("", dir.path().to_str().unwrap());
            let p = test_printer();
            let st = test_state();
            let cx = test_package_context(&p, &st);
            let pkgs = GoInstallManager.installed_packages(&cx).expect("Ok");
            assert!(pkgs.is_empty());
        }

        // Bootstrap covers two cascades: brew first, then system manager.
        // CFGD_BREW_BIN ToolShim makes brew_available() true and routes
        // `brew install go` through the shim, proving the brew branch.
        #[test]
        #[serial]
        fn go_bootstrap_via_brew_runs_brew_install_go() {
            let s = ToolShim::install("CFGD_BREW_BIN", 0, "", "");
            let p = test_printer();
            GoInstallManager
                .bootstrap(&cfgd_core::test_helpers::test_bootstrap_context(&p))
                .expect("bootstrap Ok via brew shim");
            assert!(
                s.argv_log().contains("install go"),
                "brew argv must include `install go`: {}",
                s.argv_log()
            );
        }
    }
}
