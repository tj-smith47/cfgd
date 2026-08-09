use std::collections::HashSet;

use crate::errors::Result;
use crate::output::Printer;
use crate::providers::{NoteSink, PackageAction, PackageContext, PackageManager};

/// Compute stale package-tracking rows to garbage-collect: cfgd-tracked packages
/// whose identity is no longer reported by their manager's `installed_packages`.
///
/// A partial uninstall failure or an out-of-band removal can leave a
/// `package`/`<manager>/<identity>` row whose package is gone, which prune alone
/// never reaps (prune requires the package to still be installed). Returns the
/// `(manager, identity)` pairs the caller should delete from the tracking table.
/// Only available managers are inspected — an unavailable one cannot confirm
/// absence, so its rows are left intact. Run only on a FULL unscoped apply.
///
/// `cfgd_installed` holds `"<manager>/<identity>"` entries.
pub fn stale_tracked_packages(
    managers: &[&dyn PackageManager],
    cfgd_installed: &HashSet<String>,
    cx: &PackageContext<'_>,
) -> Result<Vec<(String, String)>> {
    let mut stale = Vec::new();
    for manager in managers {
        if !manager.is_available() {
            continue;
        }
        let prefix = format!("{}/", manager.name());
        let tracked: Vec<&str> = cfgd_installed
            .iter()
            .filter_map(|id| id.strip_prefix(&prefix))
            .collect();
        if tracked.is_empty() {
            continue;
        }
        let installed = manager.installed_packages(cx)?;
        for id in tracked {
            if !installed.contains(id) {
                stale.push((manager.name().to_string(), id.to_string()));
            }
        }
    }
    Ok(stale)
}

impl<'a> super::Reconciler<'a> {
    pub(super) fn apply_package_action(
        &self,
        action: &PackageAction,
        printer: &Printer,
        notes: &NoteSink,
    ) -> Result<String> {
        let cx = PackageContext::with_notes(printer, self.state, notes).caller_owns_status();
        match action {
            PackageAction::Bootstrap { manager, .. } => {
                // Find in ALL managers (not just available — it isn't available yet)
                for pm in &self.registry.package_managers {
                    if pm.name() == manager {
                        // A module's implicit bootstrap dispatches ahead of this
                        // planned one, so by the time it runs the manager is
                        // often already installed. Re-running the installer is
                        // not idempotent for every manager and is minutes of
                        // work for some; the action still completes, because
                        // what it promises is an available manager, not an
                        // installation.
                        if !pm.is_available() {
                            pm.bootstrap(&cx)?;
                        }
                        // Profile-level packages reach bootstrap through here
                        // rather than through the Modules phase, so this site
                        // owes the same record — without it a profile that names
                        // only `spec.packages` never gets the manager on PATH.
                        // It precedes the availability check because that check
                        // resolves the binary, and a manager installed into a
                        // prefix this process never inherited only becomes
                        // resolvable once its directories are registered.
                        self.record_bootstrap_path_dirs(pm.as_ref(), printer);
                        if !pm.is_available() {
                            return Err(crate::errors::PackageError::BootstrapFailed {
                                manager: manager.clone(),
                                message: format!("{} still not available after bootstrap", manager),
                            }
                            .into());
                        }
                        return Ok(format!("package:{}:bootstrap", manager));
                    }
                }
                Err(crate::errors::PackageError::ManagerNotFound {
                    manager: manager.clone(),
                }
                .into())
            }
            PackageAction::Install {
                manager, packages, ..
            } => {
                for pm in self.registry.available_package_managers() {
                    if pm.name() == manager {
                        // Install with the original entries (go needs the full
                        // module path), but build the tracking description from
                        // IDENTITIES so the tracked key matches what prune later
                        // compares against (`go/2fa`, not `go/rsc.io/2fa`).
                        pm.install(packages, &cx)?;
                        let identities: Vec<String> =
                            packages.iter().map(|p| pm.package_identity(p)).collect();
                        return Ok(format!(
                            "package:{}:install:{}",
                            manager,
                            identities.join(",")
                        ));
                    }
                }
                Err(crate::errors::PackageError::ManagerNotFound {
                    manager: manager.clone(),
                }
                .into())
            }
            PackageAction::Uninstall {
                manager, packages, ..
            } => {
                for pm in self.registry.available_package_managers() {
                    if pm.name() == manager {
                        pm.uninstall(packages, &cx)?;
                        return Ok(format!(
                            "package:{}:uninstall:{}",
                            manager,
                            packages.join(",")
                        ));
                    }
                }
                Err(crate::errors::PackageError::ManagerNotFound {
                    manager: manager.clone(),
                }
                .into())
            }
            PackageAction::Skip { manager, .. } => Ok(format!("package:{}:skip", manager)),
        }
    }
}
