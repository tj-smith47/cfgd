//! Homebrew family of package managers: `brew`, `brew-tap`, `brew-cask`.
//!
//! All three are exposed as separate `PackageManager` implementations so a
//! profile can declare formulae, taps, and casks independently while the
//! actual `brew` CLI handles each via different subcommands.

use std::collections::HashSet;
use std::process::Command;

use cfgd_core::errors::{PackageError, Result};
use cfgd_core::output::Role;
use cfgd_core::providers::{BootstrapPlan, PackageManager};

use super::shared::{
    brew_available, brew_cmd, brew_path_dirs, command_failure_reason,
    install_batch_then_per_package, pkg_run, run_pkg_cmd, run_pkg_cmd_live, run_pkg_cmd_msg,
};

pub struct BrewManager;

impl BrewManager {
    fn run_brew(&self, args: &[&str]) -> std::result::Result<String, PackageError> {
        let output = run_pkg_cmd("brew", brew_cmd().args(args), "list")?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    pub(super) fn installed_taps(&self) -> Result<HashSet<String>> {
        Ok(parse_brew_list_set(&self.run_brew(&["tap"])?))
    }

    pub(super) fn installed_casks(&self) -> Result<HashSet<String>> {
        Ok(parse_brew_list_set(
            &self.run_brew(&["list", "--cask", "-1"])?,
        ))
    }
}

/// Parse newline-separated brew list output (taps, casks, formulae) into a
/// trimmed `HashSet`, dropping empty / whitespace-only lines. Shared by
/// `installed_taps`, `installed_casks`, and `installed_packages`.
pub(super) fn parse_brew_list_set(stdout: &str) -> HashSet<String> {
    stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Pull a string field out of `brew info --json=v2` output by JSON pointer.
/// Used for both formula version (`/formulae/0/versions/stable`) and cask
/// version (`/casks/0/version`). Returns `Ok(None)` when the pointer doesn't
/// resolve or doesn't refer to a string — brew uses a sparse schema, so a
/// missing field is "no version known" not a hard error.
pub(super) fn parse_brew_info_version(
    stdout: &str,
    json_pointer: &str,
    manager: &'static str,
) -> Result<Option<String>> {
    let parsed: serde_json::Value =
        serde_json::from_str(stdout).map_err(|e| PackageError::ListFailed {
            manager: manager.into(),
            message: format!("failed to parse brew info output: {}", e),
        })?;
    Ok(parsed
        .pointer(json_pointer)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()))
}

// --- BrewTapManager ---

pub struct BrewTapManager;

// Older brew has no `trust` subcommand and no trust gate either, so its
// "Unknown command: trust/untrust" refusal means there is nothing to record.
fn is_missing_trust_subcommand(message: &str) -> bool {
    message.to_lowercase().contains("unknown command")
}

impl BrewTapManager {
    // Current brew ignores formulae, casks and commands from a tap the user
    // has not trusted (`brew trust --tap` records the grant in trust.json,
    // non-interactively — the command takes no confirmation), so a tap cfgd
    // adds is only usable once trusted. A real trust failure leaves the tap's
    // formulae uninstallable, which fails the install it belongs to.
    fn trust_tap(&self, tap: &str) -> Result<()> {
        match run_pkg_cmd_msg(
            "brew-tap",
            brew_cmd().args(["trust", "--tap", tap]),
            "install",
            &format!("brew trust --tap {tap}"),
        ) {
            Err(PackageError::InstallFailed { message, .. })
                if is_missing_trust_subcommand(&message) =>
            {
                Ok(())
            }
            other => Ok(other.map(|_| ())?),
        }
    }

    // The untap already succeeded when this runs, so a failed untrust is
    // residue (a trust.json entry naming a tap that no longer exists), never
    // a failed uninstall — it is reported as a caveat instead of propagated,
    // which also keeps untap working for taps trusted before cfgd recorded
    // trust at all.
    fn untrust_tap(&self, tap: &str, cx: &cfgd_core::providers::PackageContext<'_>) {
        match run_pkg_cmd_msg(
            "brew-tap",
            brew_cmd().args(["untrust", "--tap", tap]),
            "uninstall",
            &format!("brew untrust --tap {tap}"),
        ) {
            Ok(_) => {}
            Err(e) => {
                if let PackageError::UninstallFailed { message, .. } = &e
                    && is_missing_trust_subcommand(message)
                {
                    return;
                }
                // The error's own Display opens on the manager name, because
                // it is read standalone in a `Result` chain; here the tag
                // already says `brew-tap`, so the note states what was left
                // behind instead of repeating the speaker.
                let detail = match &e {
                    PackageError::UninstallFailed { message, .. } => {
                        cfgd_core::output::collapse_to_subject_line(message)
                    }
                    other => cfgd_core::output::collapse_to_subject_line(other),
                };
                cx.report(
                    Role::Warn,
                    "brew-tap",
                    format!("could not untrust {tap}: {detail}"),
                );
            }
        }
    }
}

impl PackageManager for BrewTapManager {
    fn name(&self) -> &str {
        "brew-tap"
    }

    fn is_available(&self) -> bool {
        brew_available()
    }

    fn bootstrap_plan(&self) -> Option<BootstrapPlan> {
        // A sub-manager has no bootstrap of its own: `brew` provisions the one
        // binary all three share.
        None
    }

    fn bootstrap(&self, _cx: &cfgd_core::providers::PackageContext<'_>) -> Result<()> {
        Ok(())
    }

    fn installed_packages(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<HashSet<String>> {
        BrewManager.installed_taps()
    }

    fn install(
        &self,
        taps: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        for tap in taps {
            let label = format!("brew tap {}", tap);
            run_pkg_cmd_live(
                cx,
                "brew-tap",
                brew_cmd().args(["tap", tap]),
                &label,
                "install",
            )?;
            self.trust_tap(tap)?;
        }
        Ok(())
    }

    fn uninstall(
        &self,
        taps: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        for tap in taps {
            let label = format!("brew untap {}", tap);
            run_pkg_cmd_live(
                cx,
                "brew-tap",
                brew_cmd().args(["untap", tap]),
                &label,
                "uninstall",
            )?;
            self.untrust_tap(tap, cx);
        }
        Ok(())
    }

    fn available_version(&self, _package: &str) -> Result<Option<String>> {
        // Taps don't have versions
        Ok(None)
    }

    fn registers_family_sources(&self) -> bool {
        // A tap is a formula SOURCE: `brew`/`brew-cask` installs in the same
        // run may only resolve once it is added, so tap installs order first.
        true
    }
}

// --- BrewCaskManager ---

pub struct BrewCaskManager;

impl PackageManager for BrewCaskManager {
    fn name(&self) -> &str {
        "brew-cask"
    }

    fn is_available(&self) -> bool {
        brew_available()
    }

    fn bootstrap_plan(&self) -> Option<BootstrapPlan> {
        // A sub-manager has no bootstrap of its own: `brew` provisions the one
        // binary all three share.
        None
    }

    fn bootstrap(&self, _cx: &cfgd_core::providers::PackageContext<'_>) -> Result<()> {
        Ok(())
    }

    fn installed_packages(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<HashSet<String>> {
        BrewManager.installed_casks()
    }

    fn install(
        &self,
        casks: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        install_batch_then_per_package(cx, "brew-cask", casks, |pkgs| {
            let mut cmd = brew_cmd();
            cmd.arg("install").arg("--cask").args(pkgs);
            cmd
        })?;
        Ok(())
    }

    fn uninstall(
        &self,
        casks: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        if casks.is_empty() {
            return Ok(());
        }
        let label = format!("brew uninstall --cask {}", casks.join(" "));
        run_pkg_cmd_live(
            cx,
            "brew-cask",
            brew_cmd().arg("uninstall").arg("--cask").args(casks),
            &label,
            "uninstall",
        )?;
        Ok(())
    }

    fn available_version(&self, cask: &str) -> Result<Option<String>> {
        // brew info --json=v2 --cask <pkg> → .casks[0].version
        let output = brew_cmd()
            .args(["info", "--json=v2", "--cask", cask])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "brew-cask".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        parse_brew_info_version(
            &String::from_utf8_lossy(&output.stdout),
            "/casks/0/version",
            "brew-cask",
        )
    }
}

impl PackageManager for BrewManager {
    fn name(&self) -> &str {
        "brew"
    }

    fn is_available(&self) -> bool {
        brew_available()
    }

    fn bootstrap_plan(&self) -> Option<BootstrapPlan> {
        // `curl` is named, not gated on: the installer needs it, but brew has
        // always offered to provision itself regardless, and narrowing that here
        // would drop the manager instead of reporting the missing tool.
        Some(
            BootstrapPlan::new("homebrew installer")
                .requiring(["curl"])
                .creating(brew_path_dirs()),
        )
    }

    fn bootstrap(&self, cx: &cfgd_core::providers::PackageContext<'_>) -> Result<()> {
        let install_url = "https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh";

        if cfg!(target_os = "linux") && cfgd_core::is_root() {
            // Linuxbrew-as-root: create linuxbrew user, install as that user
            let user_status = Command::new("useradd")
                .args([
                    "--system",
                    "--create-home",
                    "--shell",
                    "/bin/bash",
                    "linuxbrew",
                ])
                .status()
                .map_err(|e| PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: format!("failed to create linuxbrew user: {}", e),
                })?;
            // Exit code 9 = user already exists, which is fine
            if !user_status.success() && user_status.code() != Some(9) {
                return Err(PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: "failed to create linuxbrew system user".into(),
                }
                .into());
            }
            if user_status.success() {
                cx.report(Role::Info, "brew", "created the 'linuxbrew' system user");
            }

            let result = pkg_run(
                cx,
                Command::new("sudo")
                    .args(["-u", "linuxbrew", "bash", "-c"])
                    .arg(format!(
                        "NONINTERACTIVE=1 /bin/bash -c \"$(curl -fsSL {})\"",
                        install_url
                    )),
                "Installing Homebrew as linuxbrew user",
            )
            .map_err(|e| PackageError::BootstrapFailed {
                manager: "brew".into(),
                message: format!("homebrew install failed: {}", e),
            })?;
            if !result.status.success() {
                return Err(PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: format!(
                        "homebrew install script failed: {}",
                        command_failure_reason(&result)
                    ),
                }
                .into());
            }

            // PATH for brew commands will be augmented via brew_cmd()
        } else {
            let result = pkg_run(
                cx,
                Command::new("bash").arg("-c").arg(format!(
                    "NONINTERACTIVE=1 /bin/bash -c \"$(curl -fsSL {})\"",
                    install_url
                )),
                "Installing Homebrew",
            )
            .map_err(|e| PackageError::BootstrapFailed {
                manager: "brew".into(),
                message: format!("homebrew install failed: {}", e),
            })?;
            if !result.status.success() {
                return Err(PackageError::BootstrapFailed {
                    manager: "brew".into(),
                    message: format!(
                        "homebrew install script failed: {}",
                        command_failure_reason(&result)
                    ),
                }
                .into());
            }

            // PATH for brew commands will be augmented via brew_cmd()
        }

        Ok(())
    }

    fn installed_packages(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<HashSet<String>> {
        Ok(parse_brew_list_set(&self.run_brew(&[
            "list",
            "--formulae",
            "-1",
        ])?))
    }

    fn install(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        install_batch_then_per_package(cx, "brew", packages, |pkgs| {
            let mut cmd = brew_cmd();
            cmd.arg("install").args(pkgs);
            cmd
        })?;
        Ok(())
    }

    fn uninstall(
        &self,
        packages: &[String],
        cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<()> {
        if packages.is_empty() {
            return Ok(());
        }
        let label = format!("brew uninstall {}", packages.join(" "));
        run_pkg_cmd_live(
            cx,
            "brew",
            brew_cmd().arg("uninstall").args(packages),
            &label,
            "uninstall",
        )?;
        Ok(())
    }

    fn has_index(&self) -> bool {
        true
    }

    fn refresh_index(&self, cx: &cfgd_core::providers::PackageContext<'_>) -> Result<()> {
        run_pkg_cmd_live(
            cx,
            "brew",
            brew_cmd().arg("update"),
            "brew update",
            "update",
        )?;
        Ok(())
    }

    fn available_version(&self, package: &str) -> Result<Option<String>> {
        // brew info --json=v2 <pkg> → .formulae[0].versions.stable
        let output = brew_cmd()
            .args(["info", "--json=v2", package])
            .output()
            .map_err(|e| PackageError::CommandFailed {
                manager: "brew".into(),
                source: e,
            })?;
        if !output.status.success() {
            return Ok(None);
        }
        parse_brew_info_version(
            &String::from_utf8_lossy(&output.stdout),
            "/formulae/0/versions/stable",
            "brew",
        )
    }

    fn path_dirs(&self, _cx: &cfgd_core::providers::PackageContext<'_>) -> Vec<String> {
        brew_path_dirs()
    }

    // `created_path_dirs` is deliberately NOT overridden: the default (empty)
    // answer is correct here. brew's prefix is never a directory cfgd itself
    // created — a fresh bootstrap installs brew there, but the prefix pre-dates
    // that install (Homebrew's own installer creates it, cfgd only ever runs
    // it), so it never belongs in the generated env file (see env.rs's
    // ownership invariant). The install-time PATH-resolution gap this
    // otherwise leaves — the next action can't resolve a binary brew's own
    // install just populated — is closed at the process level only, in
    // `reconciler::packages::register_install_path_dirs`, which never
    // persists anything.

    fn installed_packages_with_versions(
        &self,
        _cx: &cfgd_core::providers::PackageContext<'_>,
    ) -> Result<Vec<cfgd_core::providers::PackageInfo>> {
        // `--formulae` pins the same population `installed_packages` lists
        // (`brew list --formulae -1`), so the planner's versioned enumeration
        // and the identity one can never disagree about what is installed —
        // casks stay with the separate brew-cask manager either way.
        let output = run_pkg_cmd(
            "brew",
            brew_cmd().args(["list", "--formulae", "--versions"]),
            "list",
        )?;
        Ok(parse_brew_versions(&String::from_utf8_lossy(
            &output.stdout,
        )))
    }
}

/// Parse `brew list --versions` output (format: `package 1.2.3`) into PackageInfo.
/// Each line has package name followed by one or more version tokens separated by spaces.
/// We take the last version token as the installed version.
pub(super) fn parse_brew_versions(stdout: &str) -> Vec<cfgd_core::providers::PackageInfo> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            let mut parts = line.splitn(2, ' ');
            let name = parts.next()?.trim();
            let version = parts
                .next()
                .and_then(|v| v.split_whitespace().last())
                .unwrap_or("unknown");
            if name.is_empty() {
                return None;
            }
            Some(cfgd_core::providers::PackageInfo {
                name: name.to_string(),
                version: version.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
