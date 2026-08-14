use std::cell::RefCell;
use std::collections::HashSet;

use crate::config::{ResolvedProfile, ScriptEntry, ScriptShell};
use crate::errors::{ConfigError, Result};
use crate::modules::ResolvedModule;
use crate::output::{LaneOutput, Printer};
use crate::providers::{
    NoteSink, PackageAction, PackageContext, PackageManager, PackageStateStore, ProviderRegistry,
};

use super::scripts::{
    MODULE_SCRIPT_TIMEOUT, ScriptEnvContext, ScriptReport, build_module_script_env, execute_script,
    script_default_workdir,
};
use super::types::{ManagerAction, ModuleAction, ModuleActionKind, ReconcileContext, ScriptPhase};

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

/// The PATH directories one bootstrap made resolvable, paired with the manager
/// that owns them.
///
/// Handed back rather than written where it is produced. The process-global
/// registration has to happen inside the lane, because the next action in that
/// lane resolves a binary through it; the row that records it is a SQLite
/// write, and SQLite has exactly one writer.
pub(super) struct BootstrapRecord {
    pub(super) manager: String,
    pub(super) dirs: Vec<String>,
}

/// Everything the module-install arm needs beyond the package plumbing: a
/// `prefer: [script]` package runs a script rather than a manager command, and
/// a script needs the profile it was planned under, its module's directory and
/// the environment cfgd builds for it.
pub(super) struct ModuleInstallContext<'x> {
    pub(super) config_dir: &'x std::path::Path,
    pub(super) resolved: &'x ResolvedProfile,
    pub(super) module_actions: &'x [ResolvedModule],
    pub(super) context: ReconcileContext,
    pub(super) shell_override: Option<ScriptShell>,
    pub(super) abort: &'x crate::AbortFlag,
    /// Bootstrapped-manager PATH directories as of this action's DISPATCH.
    ///
    /// Read by whoever dispatches rather than by the work itself, because
    /// reading it here would be a SQLite read from a worker thread. The serial
    /// gate around any action whose manager is not currently available is what
    /// makes the snapshot current: a bootstrap has been collected before
    /// anything that could observe its directories is dispatched.
    pub(super) path_dirs: &'x [String],
}

/// One package action's whole execution environment, holding no `&Reconciler`.
///
/// The reconciler owns a `StateStore` whose `rusqlite::Connection` is `Send`
/// and not `Sync`, so `&Reconciler` cannot be shared with a worker thread at
/// all. What the work actually needs is this: the registry (whose provider
/// traits already carry `Send + Sync`), a package-state view — the real store
/// on the coordinator, a channel proxy inside a lane — and the sink its child
/// output goes to.
pub(super) struct PackageExec<'x> {
    registry: &'x ProviderRegistry,
    state: &'x dyn PackageStateStore,
    printer: &'x Printer,
    notes: &'x NoteSink,
    lane: Option<&'x dyn LaneOutput>,
    /// Every bootstrap this exec performed, drained by whoever owns the SQLite
    /// connection. A `RefCell` because the trait methods take `&self` and one
    /// exec is only ever used from the thread that built it.
    bootstrapped: RefCell<Vec<BootstrapRecord>>,
}

impl<'x> PackageExec<'x> {
    pub(super) fn new(
        registry: &'x ProviderRegistry,
        state: &'x dyn PackageStateStore,
        printer: &'x Printer,
        notes: &'x NoteSink,
    ) -> Self {
        Self {
            registry,
            state,
            printer,
            notes,
            lane: None,
            bootstrapped: RefCell::new(Vec::new()),
        }
    }

    /// Route every command this exec runs into `lane` instead of into a window
    /// at the ambient depth.
    #[must_use]
    pub(super) fn in_lane(mut self, lane: &'x dyn LaneOutput) -> Self {
        self.lane = Some(lane);
        self
    }

    /// Take the bootstraps performed so far, for the caller that owns the
    /// state connection to persist. Returned even when the action failed: the
    /// directories are already registered in this process, so a row that
    /// omitted them would disagree with what the run can actually resolve.
    pub(super) fn take_bootstrapped(&self) -> Vec<BootstrapRecord> {
        std::mem::take(&mut self.bootstrapped.borrow_mut())
    }

    fn cx(&self) -> PackageContext<'_> {
        let cx =
            PackageContext::with_notes(self.printer, self.state, self.notes).caller_owns_status();
        match self.lane {
            Some(lane) => cx.in_lane(lane),
            None => cx,
        }
    }

    /// Make a just-bootstrapped manager's directories resolvable to the rest of
    /// this process, and queue the row that records them.
    ///
    /// The registration is unconditional and comes first: the very next action
    /// may install through a binary that just landed in one of these
    /// directories, and a state write that fails later is no reason to leave
    /// the running process unable to find it.
    fn record_bootstrap(&self, pm: &dyn PackageManager) {
        let cx = self.cx();
        // The directories land in shell files that a Git-Bash and a PowerShell
        // session on the same Windows host both read, and in a state row those
        // reads are compared against.
        let dirs: Vec<String> = pm
            .path_dirs(&cx)
            .iter()
            .map(|dir| crate::to_posix_string(std::path::Path::new(dir)))
            .collect();
        crate::register_bootstrapped_path_dirs(&dirs);
        self.bootstrapped.borrow_mut().push(BootstrapRecord {
            manager: pm.name().to_string(),
            dirs,
        });
    }

    /// Apply one profile-owned package action.
    pub(super) fn apply_package_action(&self, action: &PackageAction) -> Result<String> {
        let cx = self.cx();
        match action {
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

    /// Apply one `cfgd:managers` node.
    ///
    /// Each node does exactly what its plan line said and nothing else — a
    /// provision installs, and does not also refresh, because a manager it just
    /// installed carries a fresh index. The description returned is the node's
    /// own id, so the journal row and the DAG edge naming it are the same
    /// string.
    pub(super) fn apply_manager_action(&self, action: &ManagerAction) -> Result<(String, bool)> {
        let cx = self.cx();
        // A provision's manager is by definition not available yet, so the
        // lookup spans every registered manager rather than the available ones.
        // It is resolved per arm rather than up front, because a refusal names
        // work the planner already ruled out and must not be answered with a
        // different failure when the manager is not registered at all.
        let lookup = |name: &str| {
            self.registry
                .package_managers
                .iter()
                .find(|pm| pm.name() == name)
                .ok_or_else(|| crate::errors::PackageError::ManagerNotFound {
                    manager: name.to_string(),
                })
        };
        let mut changed = true;
        match action {
            // An index refresh is best-effort and never fails the phase: a
            // flaky mirror must not turn a run into a failure the installs
            // below it would have survived, and the install that follows
            // reports its own error with better words. The line settles as
            // unchanged — which is what a failed refresh leaves behind — with
            // the cause attached beneath it.
            ManagerAction::RefreshIndex { manager } => {
                let pm = lookup(manager)?;
                if let Err(e) = pm.update(&cx) {
                    cx.report(
                        crate::output::Role::Warn,
                        manager,
                        format!(
                            "index refresh failed: {}",
                            crate::output::collapse_to_subject_line(&e)
                        ),
                    );
                    changed = false;
                }
            }
            ManagerAction::Provision { manager, .. } => {
                let pm = lookup(manager)?;
                // An earlier node — or a module's own install — may have
                // provisioned it already. What the node promises is an
                // available manager, not a second run of an installer that is
                // minutes of work and not idempotent for every manager.
                if !pm.is_available() {
                    pm.bootstrap(&cx)?;
                }
                self.record_bootstrap(pm.as_ref());
                if !pm.is_available() {
                    return Err(crate::errors::PackageError::BootstrapFailed {
                        manager: manager.clone(),
                        message: format!("{manager} still not available after bootstrap"),
                    }
                    .into());
                }
            }
            ManagerAction::Prerequisite {
                tool, installer, ..
            } => {
                lookup(installer)?.install(std::slice::from_ref(tool), &cx)?;
            }
            // Nothing to run: the node IS the refusal. It fails rather than
            // succeeding at nothing, because the packages that named this
            // manager are not going to be installed either. The reason is
            // restated for the journal; the line itself does not reprint it.
            ManagerAction::Refuse { manager, reason } => {
                return Err(crate::errors::PackageError::BootstrapFailed {
                    manager: manager.clone(),
                    message: reason.clone(),
                }
                .into());
            }
        }
        Ok((action.node_id(), changed))
    }

    /// Apply one module-owned `InstallPackages` action.
    pub(super) fn install_module_packages(
        &self,
        action: &ModuleAction,
        pkgs: &[crate::modules::ResolvedPackage],
        mcx: &ModuleInstallContext<'_>,
    ) -> Result<(String, bool)> {
        // Packages in each InstallPackages action are already grouped by
        // manager in plan_modules(), so just collect names and install.
        let pkg_names: Vec<String> = pkgs.iter().map(|p| p.resolved_name.clone()).collect();
        let resolved_mod = mcx
            .module_actions
            .iter()
            .find(|m| m.name == action.module_name);
        let module_dir = resolved_mod.map(|m| m.dir.clone());
        let module_env = resolved_mod.map(|m| m.env.as_slice()).unwrap_or(&[]);

        // A `prefer: [script]` install has no queryable installed-state, so
        // idempotency is the script's own responsibility. When all of a
        // package's guards (creates/onlyIf/unless) say "skip", the install
        // is a clean no-op — `changed` stays false so apply reports it as
        // unchanged rather than a re-run. Without guards the script runs
        // every apply (changed=true), which is the author's responsibility.
        let mut script_changed = false;
        // A manager-backed install always counts as changed (the package
        // managers own their own idempotency at the package level, but the
        // action having reached the install call means work was attempted).
        let mut manager_changed = false;

        if let Some(first) = pkgs.first() {
            if first.manager == "script" {
                for pkg in pkgs {
                    if let Some(ref script_content) = pkg.script {
                        let profile_name = mcx
                            .resolved
                            .layers
                            .last()
                            .map(|l| l.profile_name.as_str())
                            .unwrap_or("unknown");
                        let env_vars = build_module_script_env(
                            &ScriptEnvContext {
                                config_dir: mcx.config_dir,
                                profile_name,
                                context: mcx.context,
                                phase: &ScriptPhase::PostApply,
                                module_name: Some(&action.module_name),
                                module_dir: module_dir.as_deref(),
                                path_dirs: mcx.path_dirs,
                            },
                            module_env,
                        );
                        // Build a Full entry so the package's idempotency
                        // guards run through the same guard-evaluation path
                        // as lifecycle scripts (creates → onlyIf → unless);
                        // a guard that says "skip" yields changed=false.
                        let script_entry = ScriptEntry::Full {
                            run: script_content.clone(),
                            timeout: None,
                            idle_timeout: None,
                            continue_on_error: None,
                            shell: ScriptShell::Auto,
                            only_if: pkg.only_if.clone(),
                            unless: pkg.unless.clone(),
                            creates: pkg.creates.clone(),
                            interactive: false,
                            workdir: None,
                        };
                        let source = module_dir.as_deref().unwrap_or(mcx.config_dir);
                        let working = script_default_workdir(mcx.config_dir);
                        let (_label, changed, _captured) = execute_script(
                            &script_entry,
                            source,
                            &working,
                            &env_vars,
                            MODULE_SCRIPT_TIMEOUT,
                            self.printer,
                            mcx.shell_override,
                            Some(mcx.abort),
                            // In a lane the script's output is the lane's and
                            // its status line is the coordinator's; off a lane
                            // this is `None` and nothing about the sequential
                            // path changes.
                            ScriptReport {
                                lane: self.lane,
                                ..ScriptReport::default()
                            },
                        )
                        .map_err(|e| {
                            // The script's own message is carried rather than
                            // discarded: inside a lane this error IS the status
                            // line, because the script settles none of its own,
                            // so dropping it leaves the first line the reader
                            // sees saying only that something failed. Its FIRST
                            // line only — the tail is the captured script
                            // output, which already renders as the action's
                            // body, and a status detail that repeated it would
                            // put a whole build log on one line.
                            let rendered = e.to_string();
                            // The first NON-EMPTY line, and the suffix is dropped
                            // when there is none: a leading blank would otherwise
                            // render a dangling "failed: " with nothing after it.
                            let cause = rendered.lines().map(str::trim).find(|l| !l.is_empty());
                            let subject = format!(
                                "module {} install script for '{}' failed",
                                action.module_name, pkg.canonical_name,
                            );
                            crate::errors::CfgdError::Config(ConfigError::Invalid {
                                message: match cause {
                                    Some(cause) => format!("{subject}: {cause}"),
                                    None => subject,
                                },
                            })
                        })?;
                        script_changed |= changed;
                    }
                }
            } else {
                // Find the manager — check all registered, not just available
                let pm = self
                    .registry
                    .package_managers
                    .iter()
                    .find(|m| m.name() == first.manager);

                if let Some(pm) = pm {
                    let cx = self.cx();
                    pm.install(&pkg_names, &cx)?;
                    manager_changed = true;
                }
            }
        }

        Ok((
            format!(
                "module:{}:packages:{}",
                action.module_name,
                pkg_names.join(",")
            ),
            script_changed || manager_changed,
        ))
    }
}

/// The REGISTERED name of the manager an action's work drives. `None` for an
/// action that runs no manager command at all.
///
/// Not the lane key: the lane is the name's family (`Slot::lane`), so that
/// `brew-cask` queues behind `brew`. This name is what display, the journal and
/// the availability sub-gate use, all three of which have to name the manager
/// the user declared rather than the binary underneath it.
///
/// A `prefer: [script]` module install keys on the literal `script`, the same
/// pseudo-manager name the planner grouped it under: two of them do run
/// concurrently with everything else, and serializing them against each other
/// is the honest reading of a manager whose "binary" is the user's own shell.
pub(super) fn action_manager(action: &crate::reconciler::types::Action) -> Option<&str> {
    use crate::reconciler::types::Action;
    match action {
        Action::Package(
            PackageAction::Install { manager, .. }
            | PackageAction::Uninstall { manager, .. }
            | PackageAction::Skip { manager, .. },
        ) => Some(manager.as_str()),
        Action::Module(ModuleAction {
            kind: ModuleActionKind::InstallPackages { resolved },
            ..
        }) => resolved.first().map(|p| p.manager.as_str()),
        // The manager whose COMMAND the node runs — apt for `apt install curl`,
        // not the manager that needed curl. That is what must not run twice at
        // once, and the prerequisite's own subject already names the tool.
        Action::Manager(node) => Some(node.manager()),
        _ => None,
    }
}

impl super::Reconciler<'_> {
    pub(super) fn apply_package_action(
        &self,
        action: &PackageAction,
        printer: &Printer,
        notes: &NoteSink,
    ) -> Result<String> {
        let exec = PackageExec::new(self.registry, self.state, printer, notes);
        let result = exec.apply_package_action(action);
        self.persist_bootstraps(exec.take_bootstrapped());
        result
    }

    pub(super) fn apply_manager_action(
        &self,
        action: &ManagerAction,
        printer: &Printer,
        notes: &NoteSink,
    ) -> Result<(String, bool)> {
        let exec = PackageExec::new(self.registry, self.state, printer, notes);
        let result = exec.apply_manager_action(action);
        self.persist_bootstraps(exec.take_bootstrapped());
        result
    }

    /// Write the rows a run of [`PackageExec`] queued. The ONE writer of
    /// `bootstrapped_path_dirs` outside the coordinator's own collection point.
    pub(super) fn persist_bootstraps(&self, records: Vec<BootstrapRecord>) {
        for record in records {
            if let Err(e) = self
                .state
                .record_bootstrapped_path_dirs(&record.manager, &record.dirs)
            {
                tracing::warn!(
                    "cannot record PATH directories for bootstrapped {}: {e}",
                    record.manager
                );
            }
        }
    }
}
