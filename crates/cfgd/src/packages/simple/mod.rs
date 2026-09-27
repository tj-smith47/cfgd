//! `SimpleManager` — a data-driven `PackageManager` covering apt, dnf, yum,
//! apk, pacman, zypper, and pkg.
//!
//! Each constructor (`apt_manager`, `dnf_manager`, ...) wires up the manager
//! name, list/install/uninstall/update commands, parser, and version-query
//! function. Behavioural overrides (yum-only-when-no-dnf, dnf check-update
//! exit-code-100) are encoded as struct fields, not subclasses.

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::providers::{BootstrapPlan, PackageContext, PackageManager};
use cfgd_core::{command_available, command_available_with_seam, tool_cmd};

use super::parsers::{
    parse_apk_lines, parse_dnf_lines, parse_pkg_lines, parse_simple_lines, parse_yum_lines,
    parse_zypper_lines,
};
use super::shared::{
    pkg_run, run_pkg_cmd, run_pkg_cmd_live, strip_sudo_for_exec, strip_version_suffix,
};
use super::versions::{
    APK_BIN_ENV, APT_CACHE_BIN_ENV, DNF_BIN_ENV, DPKG_QUERY_BIN_ENV, PACMAN_BIN_ENV, PKG_BIN_ENV,
    RPM_BIN_ENV, YUM_BIN_ENV, ZYPPER_BIN_ENV, apt_aliases, distro_comparable,
    distro_version_meets_minimum, dnf_aliases, list_apt_with_versions, list_dnf_with_versions,
    pkg_version_meets_minimum, query_version_apk, query_version_apt, query_version_info,
    query_version_pkg,
};

pub(super) const APT_GET_BIN_ENV: &str = "CFGD_APT_GET_BIN";

/// Map a SimpleManager `mgr_name` to the `CFGD_*_BIN` env-var seam that targets
/// the SAME binary. Used so `is_available()` honors the same test-shim seam
/// `query_version_*` honors — without this, a test that shims CFGD_DNF_BIN
/// cannot make dnf_manager.is_available() return true on a host without real
/// dnf on PATH. Returns None when the manager binary differs from any seamed
/// query tool (e.g., apt's mgr_name is "apt" but the query backend is
/// apt-cache / dpkg-query — those use their own seams in versions/mod.rs).
fn mgr_seam_env(mgr_name: &str) -> Option<&'static str> {
    match mgr_name {
        "apk" => Some(APK_BIN_ENV),
        "dnf" => Some(DNF_BIN_ENV),
        "yum" => Some(YUM_BIN_ENV),
        "pacman" => Some(PACMAN_BIN_ENV),
        "zypper" => Some(ZYPPER_BIN_ENV),
        _ => None,
    }
}

/// Build a `Command` for a package-manager binary, routing through the same
/// `CFGD_*_BIN` seams the query helpers honor. Unknown binaries (most commonly
/// `"sudo"`) fall through to plain `Command::new`. This is the single entry
/// point for install / uninstall / update / list shell-outs in this module,
/// so a test that shims CFGD_DPKG_QUERY_BIN sees its shim drive both
/// `installed_packages` and `list_apt_with_versions`.
fn cmd_with_seam(prog: &str) -> Command {
    let env = match prog {
        "apt-cache" => APT_CACHE_BIN_ENV,
        "apt-get" => APT_GET_BIN_ENV,
        "apk" => APK_BIN_ENV,
        "dnf" => DNF_BIN_ENV,
        "yum" => YUM_BIN_ENV,
        "pacman" => PACMAN_BIN_ENV,
        "zypper" => ZYPPER_BIN_ENV,
        "pkg" => PKG_BIN_ENV,
        "dpkg-query" => DPKG_QUERY_BIN_ENV,
        "rpm" => RPM_BIN_ENV,
        _ => return Command::new(prog),
    };
    tool_cmd(env, prog)
}

/// Function pointer type for `installed_packages_with_versions` overrides.
type ListWithVersionsFn = fn(&str) -> Result<Vec<cfgd_core::providers::PackageInfo>>;

/// A data-driven package manager for system package managers that follow a
/// uniform pattern: list installed, install, uninstall, update.
/// Replaces individual structs for apt, dnf, yum, apk, pacman, zypper, pkg.
pub struct SimpleManager {
    pub(super) mgr_name: &'static str,
    pub(super) list_cmd: &'static [&'static str],
    pub(super) install_cmd: &'static [&'static str],
    pub(super) uninstall_cmd: &'static [&'static str],
    pub(super) update_cmd: Option<&'static [&'static str]>,
    /// The family's distinct verb for raising an already-held package
    /// (`apk upgrade`), for a family whose install verb no-ops on one already
    /// installed. `None` for a family whose install verb upgrades on its own
    /// (apt/dnf/yum/pacman/zypper/pkg all raise a held package via their
    /// ordinary install command).
    pub(super) upgrade_cmd: Option<&'static [&'static str]>,
    /// The token of `upgrade_cmd`/`install_cmd` that names the raise, for
    /// `upgrade_verb`.
    pub(super) raise_verb: &'static str,
    /// When true, non-zero exit from the update command is ignored (dnf/yum
    /// check-update returns 100 when updates are available).
    pub(super) ignore_update_exit: bool,
    /// Variables every command of the family carries, for a prompt the
    /// family's `-y` / `--noconfirm` flag does not answer (apt's debconf and
    /// needrestart questions, pkg(7)'s bootstrap confirmation). Empty for a
    /// family whose flags already cover every prompt.
    pub(super) env: &'static [(&'static str, &'static str)],
    pub(super) parse_list: fn(&str) -> HashSet<String>,
    pub(super) query_version: fn(&str, &str) -> Result<Option<String>>,
    /// Custom availability check. When None, uses `command_available(mgr_name)`.
    pub(super) is_available_fn: Option<fn() -> bool>,
    /// Override for installed_packages_with_versions. When None, falls back to
    /// the default trait implementation (wraps installed_packages with the
    /// unknown-version sentinel).
    pub(super) list_with_versions: Option<ListWithVersionsFn>,
    /// Override for package_aliases. When None, returns empty vec (default).
    pub(super) aliases_fn: Option<fn(&str) -> Vec<String>>,
    /// Per-run memo of `pkg version -t` outcomes, keyed by the `(available,
    /// floor)` pair compared. `all_package_managers()` builds a fresh
    /// `SimpleManager` per invocation, so this field's lifetime IS "for the
    /// run" with no TTL or process-global state required. Every manager but
    /// `pkg` has a pure comparator and never touches this field.
    pub(super) pkg_version_memo: std::sync::Mutex<
        std::collections::HashMap<(String, String), std::result::Result<bool, String>>,
    >,
}

impl SimpleManager {
    pub(super) fn display_cmd(&self, cmd_parts: &[&str], packages: &[String]) -> String {
        join_cmd(&self.effective_argv(cmd_parts), packages)
    }

    /// The argv cfgd spawns for `cmd_parts` on this host: `sudo` dropped where
    /// [`strip_sudo_for_exec`] drops it, the rest spelled by
    /// [`argv_after_strip`](Self::argv_after_strip).
    pub(super) fn effective_argv(&self, cmd_parts: &[&str]) -> Vec<String> {
        self.argv_after_strip(strip_sudo_for_exec(cmd_parts))
    }

    /// `effective` as spawned: when it still opens on `sudo`, the family's
    /// [`env`](Self::env) rides behind it through `env(1)`. sudo resets the
    /// environment, and `sudo K=V` is refused without a SETENV tag most
    /// sudoers entries lack, so `sudo env K=V` is the form every sudoers runs.
    pub(super) fn argv_after_strip(&self, effective: &[&str]) -> Vec<String> {
        let mut argv: Vec<String> = Vec::with_capacity(effective.len() + self.env.len() + 1);
        match effective.split_first() {
            Some((&"sudo", rest)) if !self.env.is_empty() => {
                argv.push("sudo".into());
                argv.push("env".into());
                argv.extend(self.env.iter().map(|(k, v)| format!("{k}={v}")));
                argv.extend(rest.iter().map(|s| s.to_string()));
            }
            _ => argv.extend(effective.iter().map(|s| s.to_string())),
        }
        argv
    }

    /// The `Command` for `cmd_parts`, the one place a family's spawn is
    /// composed: [`effective_argv`](Self::effective_argv) routed through the
    /// tool's `CFGD_*_BIN` seam, carrying the family's `env` itself for the
    /// bare shape (root, or a seam) where no `sudo` stands between.
    pub(super) fn spawn_command(&self, cmd_parts: &[&str]) -> Command {
        let argv = self.effective_argv(cmd_parts);
        let mut cmd = match argv.split_first() {
            Some((prog, args)) => {
                let mut cmd = cmd_with_seam(prog);
                cmd.args(args);
                cmd
            }
            None => Command::new("true"),
        };
        cmd.envs(self.env.iter().copied());
        cmd
    }

    /// The program the family's install runs, its `sudo` wrapper set aside:
    /// `apt-get` for apt, the family's own name for the rest.
    pub(super) fn install_program(&self) -> &'static str {
        self.install_cmd
            .strip_prefix(&["sudo"])
            .unwrap_or(self.install_cmd)
            .first()
            .copied()
            .unwrap_or(self.mgr_name)
    }

    /// The same line for a script cfgd EMITS for ANOTHER host to run
    /// (`module export`), where `sudo` is stripped unconditionally: the
    /// question is what the consuming build script runs as (a container build,
    /// already root), never what this host happens to be. `display_cmd` keeps
    /// the `is_root`/seam logic because it labels what cfgd RUNS here.
    ///
    /// The family's `env` leads the line as shell assignments, the form a root
    /// shell hands the variables to the one command it runs.
    pub(super) fn export_cmd(&self, cmd_parts: &[&str], packages: &[String]) -> String {
        let bare = cmd_parts.strip_prefix(&["sudo"]).unwrap_or(cmd_parts);
        let argv: Vec<String> = self
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .chain(bare.iter().map(|s| s.to_string()))
            .collect();
        join_cmd(&argv, packages)
    }
}

fn join_cmd(effective: &[String], packages: &[String]) -> String {
    effective
        .iter()
        .chain(packages)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" ")
}

impl PackageManager for SimpleManager {
    fn name(&self) -> &str {
        self.mgr_name
    }

    fn upgrade_verb(&self) -> Option<&'static str> {
        // A family with no distinct upgrade command still raises a held
        // package — through its ordinary INSTALL verb (see `install`'s own
        // doc above `upgrade_cmd`). `raise_verb` names the token of whichever
        // command actually performs the raise, not the program name.
        Some(self.raise_verb)
    }

    fn tool_version(&self) -> Option<String> {
        super::shared::tool_version_from(cmd_with_seam(self.mgr_name).arg("--version"))
    }

    fn is_available(&self) -> bool {
        if let Some(f) = self.is_available_fn {
            f()
        } else if let Some(env) = mgr_seam_env(self.mgr_name) {
            command_available_with_seam(env, self.mgr_name)
        } else {
            command_available(self.mgr_name)
        }
    }

    fn bootstrap_plan_given(&self, _delivered: &dyn Fn(&str) -> bool) -> Option<BootstrapPlan> {
        // A native system manager ships with its distribution: there is no host
        // where cfgd could install `apt` or `pacman` from something else.
        None
    }

    // bootstrap-arm-ok: a native system manager ships with its distribution
    fn bootstrap(&self, _cx: &PackageContext<'_>) -> Result<()> {
        Ok(())
    }

    fn installed_packages(&self, _cx: &PackageContext<'_>) -> Result<HashSet<String>> {
        let (prog, args) = self.list_cmd.split_first().unwrap_or((&"true", &[]));
        let output = run_pkg_cmd(self.mgr_name, cmd_with_seam(prog).args(args), "list")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok((self.parse_list)(&stdout))
    }

    fn install(&self, packages: &[String], cx: &PackageContext<'_>) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        // Only a family with a distinct upgrade verb (apk) needs the held/fresh
        // split at all — every other family's install verb already raises a
        // held package, so partitioning would just spawn a second, redundant
        // command.
        let (held, fresh) = match self.upgrade_cmd {
            Some(_) => super::shared::partition_already_installed(self, packages, cx),
            None => (Vec::new(), packages.to_vec()),
        };
        if !fresh.is_empty() {
            let label = self.display_cmd(self.install_cmd, &fresh);
            run_pkg_cmd_live(
                cx,
                self.mgr_name,
                self.spawn_command(self.install_cmd).args(&fresh),
                &label,
                "install",
            )?;
        }
        if let Some(upgrade_parts) = self.upgrade_cmd {
            let verb_label = self.display_cmd(upgrade_parts, &[]);
            super::shared::upgrade_each(cx, self.mgr_name, &held, &verb_label, |pkg| {
                let mut cmd = self.spawn_command(upgrade_parts);
                cmd.arg(pkg);
                cmd
            })?;
        }
        Ok(())
    }

    fn uninstall(&self, packages: &[String], cx: &PackageContext<'_>) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let label = self.display_cmd(self.uninstall_cmd, packages);
        run_pkg_cmd_live(
            cx,
            self.mgr_name,
            self.spawn_command(self.uninstall_cmd).args(packages),
            &label,
            "uninstall",
        )?;
        Ok(())
    }

    fn has_index(&self) -> bool {
        self.update_cmd.is_some()
    }

    fn refresh_index(&self, cx: &PackageContext<'_>) -> Result<()> {
        let Some(update_parts) = self.update_cmd else {
            return Ok(());
        };
        let label = self.display_cmd(update_parts, &[]);
        if self.ignore_update_exit {
            // dnf/yum check-update returns 100 when updates are available
            let _ = pkg_run(cx, &mut self.spawn_command(update_parts), &label).map_err(|e| {
                PackageError::CommandFailed {
                    manager: self.mgr_name.into(),
                    source: e,
                }
            })?;
        } else {
            run_pkg_cmd_live(
                cx,
                self.mgr_name,
                &mut self.spawn_command(update_parts),
                &label,
                "update",
            )?;
        }
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        (self.query_version)(self.mgr_name, package)
    }

    fn installed_packages_with_versions(
        &self,
        cx: &PackageContext<'_>,
    ) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        if let Some(f) = self.list_with_versions {
            f(self.mgr_name)
        } else {
            // Default: wrap installed_packages with the unknown sentinel
            Ok(self
                .installed_packages(cx)?
                .into_iter()
                .map(|name| cfgd_core::providers::PackageInfo {
                    name,
                    version: cfgd_core::providers::UNKNOWN_PACKAGE_VERSION.into(),
                })
                .collect())
        }
    }

    fn package_aliases(&self, canonical_name: &str) -> Result<Vec<String>> {
        if let Some(f) = self.aliases_fn {
            Ok(f(canonical_name))
        } else {
            Ok(vec![])
        }
    }

    fn package_identity(&self, entry: &str) -> String {
        if self.mgr_name == "pkg" {
            // FreeBSD `pkg info -q` lists `name-version` (`brotli-1.2.0,1`), and
            // the installed-side parse (`parse_pkg_lines`) strips it to the bare
            // name. Normalize a declared entry the same way so a versioned name a
            // user writes converges against the bare installed identity instead of
            // re-planning an install on every reconcile.
            strip_version_suffix(entry)
        } else {
            entry.to_string()
        }
    }

    /// The infallible trait method stays implemented — `modules::resolve_package`
    /// (the manager-candidate resolution run while a PLAN is built, never on the
    /// verify path) is its one production caller, choosing among managers that
    /// could satisfy a declared package by asking whether each one's AVAILABLE
    /// version clears the floor. A `pkg version -t` spawn failure there folds to
    /// `false` — "this candidate does not satisfy the floor" — which only drops
    /// this manager from consideration; a sibling candidate or `Unreadable`'s own
    /// check-error report (via [`version_meets_minimum_checked`](Self::version_meets_minimum_checked),
    /// which the LIVE floor check in `reconciler::package_version_floor` calls
    /// instead) still surfaces the failure. Folding here would be wrong on the
    /// verify path, where a spawn failure must never be reported as `Below`.
    fn version_meets_minimum(&self, available: &str, min_version: &str) -> bool {
        self.version_meets_minimum_checked(available, min_version)
            .unwrap_or(false)
    }

    fn version_meets_minimum_checked(
        &self,
        available: &str,
        min_version: &str,
    ) -> std::result::Result<bool, String> {
        if self.mgr_name != "pkg" {
            // Every other manager reaching this type is a distro family
            // (apt/dnf/yum/apk/pacman/zypper), whose listings carry the
            // packaging's epoch and revision around the upstream version, and
            // whose comparison is pure — nothing here can fail to spawn.
            return Ok(distro_version_meets_minimum(available, min_version));
        }
        // FreeBSD pkg versions carry PORTEPOCH (`,N`) / PORTREVISION (`_N`) and
        // are not semver, so the default semver comparison mis-orders them.
        // `pkg version -t` genuinely shells out and can fail to spawn — that
        // is a check that could not run, never a verdict that the floor was
        // missed, so the failure is propagated rather than folded to `false`.
        let key = (available.to_string(), min_version.to_string());
        if let Ok(memo) = self.pkg_version_memo.lock()
            && let Some(cached) = memo.get(&key)
        {
            return cached.clone();
        }
        let result = pkg_version_meets_minimum(available, min_version).map_err(|e| e.to_string());
        // A spawn failure is transient — memoizing it would poison this pair for
        // the registry's whole lifetime. Only a successful comparison is cached.
        if let Ok(v) = &result
            && let Ok(mut memo) = self.pkg_version_memo.lock()
        {
            memo.insert(key, Ok(*v));
        }
        result
    }

    fn version_comparable(&self, version: &str) -> bool {
        // `pkg version -t` understands FreeBSD's own scheme, so nothing this
        // manager lists is uncomparable to it.
        self.mgr_name == "pkg" || distro_comparable(version)
    }

    fn floor_comparable(&self, floor: &str) -> bool {
        if self.mgr_name != "pkg" {
            return distro_comparable(floor);
        }
        // The tool owns FreeBSD's grammar, so a floor is refused only for what
        // NO version grammar allows: `pkg version -t` settles a range
        // expression by collation and answers `<` or `>` with equal
        // confidence, which is an arbitrary verdict rather than a comparison.
        // A letter suffix stays comparable — pkg orders `1.2.x` itself.
        !cfgd_core::declared_floor_is_range_shaped(floor)
    }
}

// --- SimpleManager constructors ---

/// One [`SIMPLE_FAMILIES`] row: a family name and the constructor it resolves to.
pub(super) type FamilyRow = (&'static str, fn() -> SimpleManager);

/// Every data-driven family, as `(name, constructor)`: the one table
/// [`simple_manager`] resolves a name from and a caller iterates to reach every
/// family. Row order is the order a mediated bootstrap tries the families in.
pub(super) const SIMPLE_FAMILIES: &[FamilyRow] = &[
    ("apt", apt_manager),
    ("dnf", dnf_manager),
    ("yum", yum_manager),
    ("zypper", zypper_manager),
    ("pacman", pacman_manager),
    ("apk", apk_manager),
    ("pkg", pkg_manager),
];

/// The `SimpleManager` a family name resolves to, for a caller that needs the
/// family's own command spellings rather than a live provider.
pub(super) fn simple_manager(name: &str) -> Option<SimpleManager> {
    SIMPLE_FAMILIES
        .iter()
        .find(|(family, _)| *family == name)
        .map(|(_, build)| build())
}

/// The spawn that installs `pkgs` through the `name` family, composed from that
/// family's own [`SimpleManager::install_cmd`] declaration.
///
/// The one way a mediated bootstrap installs through a Unix family, so pacman's
/// `-S --noconfirm` and apk's `add` are spelled where the family declares them
/// and a provision cannot reach for a verb the family does not use. `None` for a
/// name no family here holds.
pub(super) fn family_install_command(name: &str, pkgs: &[&str]) -> Option<Command> {
    let mgr = simple_manager(name)?;
    let mut cmd = mgr.spawn_command(mgr.install_cmd);
    cmd.args(pkgs);
    Some(cmd)
}

/// `-y` answers apt-get's own questions but not debconf's (a package's
/// configuration, tzdata's region) or needrestart's service-restart menu on
/// Ubuntu; both read these variables instead.
pub(super) const APT_ENV: &[(&str, &str)] = &[
    ("DEBIAN_FRONTEND", "noninteractive"),
    ("NEEDRESTART_MODE", "a"),
];

/// pkg(7), the stub a fresh FreeBSD ships, asks before fetching pkg(8) on the
/// first command, and `pkg update` has no `-y` to answer it.
pub(super) const PKG_ENV: &[(&str, &str)] = &[("ASSUME_ALWAYS_YES", "yes")];

pub(super) fn apt_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "apt",
        list_cmd: &["dpkg-query", "-W", "-f", "${Package}\n"],
        // dpkg asks what to do with a configuration file the admin edited, a
        // question neither `-y` nor debconf answers: confdef takes dpkg's default
        // answer where one exists, and confold keeps the edited file where none
        // does.
        install_cmd: &[
            "sudo",
            "apt-get",
            "install",
            "-y",
            "-o",
            "Dpkg::Options::=--force-confdef",
            "-o",
            "Dpkg::Options::=--force-confold",
        ],
        uninstall_cmd: &[
            "sudo",
            "apt-get",
            "remove",
            "-y",
            "-o",
            "Dpkg::Options::=--force-confdef",
            "-o",
            "Dpkg::Options::=--force-confold",
        ],
        update_cmd: Some(&["sudo", "apt-get", "update"]),
        upgrade_cmd: None,
        raise_verb: "install",
        ignore_update_exit: false,
        env: APT_ENV,
        parse_list: parse_simple_lines,
        query_version: query_version_apt,
        is_available_fn: None,
        list_with_versions: Some(list_apt_with_versions),
        aliases_fn: Some(apt_aliases),
        pkg_version_memo: std::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

pub(super) fn dnf_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "dnf",
        // `--installed` (flag) not `installed` (positional): dnf5 (Fedora 41+)
        // reads a positional `installed` as a package spec and exits 1; the flag
        // form is accepted by both dnf4 and dnf5.
        list_cmd: &["dnf", "list", "--installed", "--quiet"],
        install_cmd: &["sudo", "dnf", "install", "-y"],
        uninstall_cmd: &["sudo", "dnf", "remove", "-y"],
        update_cmd: Some(&["sudo", "dnf", "check-update"]),
        upgrade_cmd: None,
        raise_verb: "install",
        ignore_update_exit: true,
        env: &[],
        parse_list: parse_dnf_lines,
        query_version: query_version_info,
        is_available_fn: None,
        list_with_versions: Some(list_dnf_with_versions),
        aliases_fn: Some(dnf_aliases),
        pkg_version_memo: std::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

pub(super) fn yum_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "yum",
        list_cmd: &["yum", "list", "installed", "--quiet"],
        install_cmd: &["sudo", "yum", "install", "-y"],
        uninstall_cmd: &["sudo", "yum", "remove", "-y"],
        update_cmd: Some(&["sudo", "yum", "check-update"]),
        upgrade_cmd: None,
        raise_verb: "install",
        ignore_update_exit: true,
        env: &[],
        parse_list: parse_yum_lines,
        query_version: query_version_info,
        is_available_fn: Some(|| {
            !command_available_with_seam(DNF_BIN_ENV, "dnf")
                && command_available_with_seam(YUM_BIN_ENV, "yum")
        }),
        list_with_versions: Some(list_dnf_with_versions),
        aliases_fn: Some(dnf_aliases),
        pkg_version_memo: std::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

pub(super) fn apk_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "apk",
        list_cmd: &["apk", "list", "--installed", "--quiet"],
        install_cmd: &["sudo", "apk", "add"],
        uninstall_cmd: &["sudo", "apk", "del"],
        update_cmd: Some(&["sudo", "apk", "update"]),
        upgrade_cmd: Some(&["sudo", "apk", "upgrade"]),
        raise_verb: "upgrade",
        ignore_update_exit: false,
        env: &[],
        parse_list: parse_apk_lines,
        query_version: query_version_apk,
        is_available_fn: None,
        list_with_versions: None,
        aliases_fn: None,
        pkg_version_memo: std::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

pub(super) fn pacman_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "pacman",
        list_cmd: &["pacman", "-Qq"],
        install_cmd: &["sudo", "pacman", "-S", "--noconfirm"],
        uninstall_cmd: &["sudo", "pacman", "-R", "--noconfirm"],
        update_cmd: Some(&["sudo", "pacman", "-Sy", "--noconfirm"]),
        upgrade_cmd: None,
        raise_verb: "-S",
        ignore_update_exit: false,
        env: &[],
        parse_list: parse_simple_lines,
        query_version: query_version_info,
        is_available_fn: None,
        list_with_versions: None,
        aliases_fn: None,
        pkg_version_memo: std::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

pub(super) fn zypper_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "zypper",
        list_cmd: &[
            "zypper",
            "se",
            "--installed-only",
            "--type",
            "package",
            "-s",
        ],
        install_cmd: &["sudo", "zypper", "install", "-y"],
        uninstall_cmd: &["sudo", "zypper", "remove", "-y"],
        // `refresh` takes no `-y`, so the global `--non-interactive` is what
        // keeps a new repository key's trust prompt from waiting on stdin: zypper
        // takes the prompt's default answer instead.
        update_cmd: Some(&["sudo", "zypper", "--non-interactive", "refresh"]),
        upgrade_cmd: None,
        raise_verb: "install",
        ignore_update_exit: false,
        env: &[],
        parse_list: parse_zypper_lines,
        query_version: query_version_info,
        is_available_fn: None,
        list_with_versions: None,
        aliases_fn: None,
        pkg_version_memo: std::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

pub(super) fn pkg_manager() -> SimpleManager {
    SimpleManager {
        mgr_name: "pkg",
        list_cmd: &["pkg", "info", "-q"],
        install_cmd: &["sudo", "pkg", "install", "-y"],
        uninstall_cmd: &["sudo", "pkg", "remove", "-y"],
        update_cmd: Some(&["sudo", "pkg", "update"]),
        upgrade_cmd: None,
        raise_verb: "install",
        ignore_update_exit: false,
        env: PKG_ENV,
        parse_list: parse_pkg_lines,
        query_version: query_version_pkg,
        is_available_fn: None,
        list_with_versions: None,
        aliases_fn: None,
        pkg_version_memo: std::sync::Mutex::new(std::collections::HashMap::new()),
    }
}

#[cfg(test)]
mod tests;
