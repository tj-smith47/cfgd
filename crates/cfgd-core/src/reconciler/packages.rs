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
/// The enumeration comes from `cx`, so a caller whose planner already read the
/// same managers through the same context pays for no second walk — and one
/// that ran an install or an uninstall in between is re-read, because that
/// moves the resolution generation every memo entry is stamped with.
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
        let installed = cx.installed_for(*manager)?;
        for id in tracked {
            if !installed.contains(id) {
                stale.push((manager.name().to_string(), id.to_string()));
            }
        }
    }
    Ok(stale)
}

/// The PATH directories cfgd owns for one manager — created by its bootstrap,
/// or by an install that had to make a prefix of its own — paired with that
/// manager.
///
/// Handed back rather than written where it is produced. The process-global
/// registration has to happen inside the lane, because the next action in that
/// lane resolves a binary through it; the row that records it is a SQLite
/// write, and SQLite has exactly one writer.
pub(super) struct BootstrapRecord {
    pub(super) manager: String,
    pub(super) dirs: Vec<String>,
    pub(super) kind: PathDirRecord,
}

/// How a queued [`BootstrapRecord`] meets the row already stored for its
/// manager.
///
/// The distinction is what keeps a narrow record from erasing a broad one: a
/// manager whose `install()` creates one prefix while its bootstrap declares
/// several would otherwise replace the row with the single directory, and the
/// rest would vanish from the generated env file.
pub(super) enum PathDirRecord {
    /// The manager's whole declaration of what it needs on PATH, from
    /// [`PackageManager::path_dirs`]. It replaces the row, so a corrected
    /// answer supersedes an earlier one.
    Declared,
    /// One directory cfgd created, from [`PackageManager::created_path_dirs`].
    /// It is added to the row and removes nothing.
    Created,
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

    /// Take the path-dir records queued so far, for the caller that owns the
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
    ///
    /// `via` is the method the plan line named, and it travels into the record
    /// context exactly as it travels into the bootstrap: a manager whose
    /// directories depend on the mediator (pipx lands in `~/.local/bin` under
    /// pip and inside the brew prefix under brew) would otherwise re-derive
    /// them from a live probe whose answer has already moved — the same
    /// bootstrap that made the manager available changes what the probe sees.
    fn record_bootstrap(&self, pm: &dyn PackageManager, via: &str) {
        let cx = self.cx().for_provision(via);
        self.record_path_dirs(pm.name(), pm.path_dirs(&cx), PathDirRecord::Declared);
    }

    /// Record the directories an `install()` just created — the manager's own
    /// [`PackageManager::created_path_dirs`].
    ///
    /// A directory cfgd made itself belongs in the generated env file however
    /// the manager got onto the machine, so this runs after every install and
    /// not only under a provision: npm's `~/.npm-global` is created during
    /// `install()`, and a user-installed npm reaches no bootstrap at all.
    ///
    /// A manager that created nothing queues no row at all, so an ordinary
    /// install costs no write; one that created something adds it to the row
    /// rather than replacing it, so a provision's other directories survive.
    fn record_created_path_dirs(&self, pm: &dyn PackageManager) {
        let cx = self.cx();
        let dirs = pm.created_path_dirs(&cx);
        if dirs.is_empty() {
            return;
        }
        self.record_path_dirs(pm.name(), dirs, PathDirRecord::Created);
    }

    /// Make an install's resolvable directories reachable to the REST OF THIS
    /// PROCESS, without persisting anything.
    ///
    /// [`PackageManager::path_dirs`] answers where a manager's binaries live
    /// however they got there, and normally only reaches the process registry
    /// through [`Self::record_bootstrap`], which fires when the manager's OWN
    /// bootstrap runs THIS run. A manager already available on a prior run, or
    /// baked into an image, never bootstraps — so a binary this run's install
    /// just landed in that directory (pipx, nvim, ...) is still unresolvable to
    /// the very next action or postApply script naming it, even though the
    /// manager reported the install successful seconds earlier.
    ///
    /// This closes that gap at the PROCESS level only: it registers the
    /// directories for [`crate::command_path`] resolution and deliberately
    /// mints no [`BootstrapRecord`] — the directory is the manager's own to
    /// have always had, not something cfgd created, so nothing about it
    /// belongs in the generated env file. [`Self::record_created_path_dirs`]
    /// (`PackageManager::created_path_dirs`) is the only path to that surface,
    /// and answers separately for whatever a manager genuinely made itself.
    fn register_install_path_dirs(&self, pm: &dyn PackageManager) {
        let cx = self.cx();
        let dirs: Vec<String> = pm
            .path_dirs(&cx)
            .iter()
            .map(|dir| crate::to_posix_string(std::path::Path::new(dir)))
            .collect();
        if dirs.is_empty() {
            return;
        }
        crate::register_bootstrapped_path_dirs(&dirs);
    }

    /// Install through `pm` and record whatever that install created, whichever
    /// way it went.
    ///
    /// The recording is not conditional on success, for the same reason
    /// [`Self::take_bootstrapped`] hands back its records after a failure: the
    /// directory is on disk either way. A failed `npm install` has already made
    /// `~/.npm-global`, and an unrecorded directory cfgd created is exactly the
    /// state where a binary lands somewhere no login shell reads. A record for
    /// a directory holding nothing yet is inert by comparison.
    fn install_recording_created(
        &self,
        pm: &dyn PackageManager,
        packages: &[String],
        cx: &PackageContext<'_>,
    ) -> Result<()> {
        let result = pm.install(packages, cx);
        // An install can land a binary in a directory that was already on
        // `PATH` — the whole point of `apt install curl` — which registers no
        // new directory and so would leave a memoized "not found" standing.
        crate::invalidate_command_resolution();
        self.register_install_path_dirs(pm);
        self.record_created_path_dirs(pm);
        result
    }

    /// Provision every manager in `members` with ONE `via` install.
    ///
    /// The packages come from each member's own
    /// [`PackageManager::mediated_packages`] — the same names its solo
    /// bootstrap hands the same mediator — so the merged command installs
    /// exactly the union of what the separate ones would have, and nothing a
    /// member never asked for. A member that answers `None` was never
    /// batchable and cannot be here; it fails naming itself rather than being
    /// silently dropped from a command the line says provisions it.
    ///
    /// The install runs through the mediator's own `install()` rather than
    /// through a member's bootstrap cascade because the cascade is per-manager
    /// by construction: `via` is already the method the plan bound, so there is
    /// nothing left for a cascade to decide.
    fn provision_batch(&self, members: &[&str], via: &str) -> Result<()> {
        let mediator = self
            .registry
            .package_managers()
            .iter()
            .find(|pm| pm.name() == via)
            .ok_or_else(|| crate::errors::PackageError::ManagerNotFound {
                manager: via.to_string(),
            })?;
        let mut packages: Vec<String> = Vec::new();
        for name in members {
            let pm = self
                .registry
                .package_managers()
                .iter()
                .find(|pm| pm.name() == *name)
                .ok_or_else(|| crate::errors::PackageError::ManagerNotFound {
                    manager: (*name).to_string(),
                })?;
            let mediated = pm.mediated_packages(via).ok_or_else(|| {
                crate::errors::PackageError::BootstrapFailed {
                    manager: (*name).to_string(),
                    message: format!("{name} cannot be provisioned by a plain {via} install"),
                }
            })?;
            for pkg in mediated {
                if !packages.contains(&pkg) {
                    packages.push(pkg);
                }
            }
        }
        let provision_cx = self.cx().for_provision(via);
        self.install_recording_created(mediator.as_ref(), &packages, &provision_cx)
            .map_err(|e| {
                crate::errors::PackageError::BootstrapFailed {
                    manager: members.join(", "),
                    message: format!(
                        "{via} install failed: {}",
                        crate::output::collapse_to_subject_line(&e)
                    ),
                }
                .into()
            })
    }

    /// The ONE registration-and-queue for both kinds of owned directory, so a
    /// bootstrap's and an install's records cannot be shaped differently.
    fn record_path_dirs(&self, manager: &str, dirs: Vec<String>, kind: PathDirRecord) {
        // The directories land in shell files that a Git-Bash and a PowerShell
        // session on the same Windows host both read, and in a state row those
        // reads are compared against.
        let dirs: Vec<String> = dirs
            .iter()
            .map(|dir| crate::to_posix_string(std::path::Path::new(dir)))
            .collect();
        crate::register_bootstrapped_path_dirs(&dirs);
        self.bootstrapped.borrow_mut().push(BootstrapRecord {
            manager: manager.to_string(),
            dirs,
            kind,
        });
    }

    /// The error for a manager an install/uninstall names that this exec
    /// cannot run — distinguishing "never registered" from "registered but
    /// not currently available", since only the latter names a recovery: a
    /// name typo has none, while an unprovisioned manager's fix is always
    /// the `Prerequisites` phase this run's filter skipped.
    fn package_manager_missing_error(&self, manager: &str) -> crate::errors::CfgdError {
        let registered = self
            .registry
            .package_managers()
            .iter()
            .any(|pm| pm.name() == manager);
        if registered {
            crate::errors::PackageError::ManagerNotAvailable {
                manager: manager.to_string(),
            }
            .into()
        } else {
            crate::errors::PackageError::ManagerNotFound {
                manager: manager.to_string(),
            }
            .into()
        }
    }

    /// What `pm` reports installed at THIS moment, or `None` when it cannot be
    /// asked.
    ///
    /// The planner elided every entry the manager already carried, but it did so
    /// before the `Prerequisites` phase ran, and that phase installs packages:
    /// `apt install npm pipx` provisions two managers and lands two apt packages
    /// a module is free to declare as well. Re-reading the machine is what keeps
    /// the two phases from installing one package twice — the truth, rather than
    /// a comparison against the names a provision happened to mention.
    ///
    /// One listing per manager per action, and never a stale one: the memo
    /// behind [`PackageContext::installed_for`] is keyed on
    /// [`crate::command_resolution_generation`], which every install, uninstall
    /// and provision this run performed has already moved.
    ///
    /// Fail-OPEN, exactly as the planner's own elision does: a manager cfgd
    /// cannot query is one whose declared entries must still be installed.
    fn installed_now(
        &self,
        pm: &dyn PackageManager,
        cx: &PackageContext<'_>,
    ) -> Option<std::sync::Arc<crate::providers::InstalledPackages>> {
        match cx.installed_for(pm) {
            Ok(installed) => Some(installed),
            Err(e) => {
                tracing::warn!(
                    manager = pm.name(),
                    error = %e,
                    "cannot re-read installed packages; installing the planned set in full"
                );
                None
            }
        }
    }

    /// Apply one profile-owned package action.
    ///
    /// The `bool` is whether the action CHANGED anything: an install whose every
    /// entry an earlier phase already landed ran and did nothing, which is a
    /// skip rather than a success.
    pub(super) fn apply_package_action(&self, action: &PackageAction) -> Result<(String, bool)> {
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
                        let pending: Vec<String> = match self.installed_now(pm, &cx) {
                            Some(installed) => packages
                                .iter()
                                .filter(|p| !installed.contains(&pm.package_identity(p)))
                                .cloned()
                                .collect(),
                            None => packages.clone(),
                        };
                        let changed = !pending.is_empty();
                        if changed {
                            self.install_recording_created(pm, &pending, &cx)?;
                        }
                        // The description names the whole DECLARED set either
                        // way: the entries this run did not have to install are
                        // on the machine and are still this action's managed
                        // resources.
                        let identities: Vec<String> =
                            packages.iter().map(|p| pm.package_identity(p)).collect();
                        return Ok((
                            format!("package:{}:install:{}", manager, identities.join(",")),
                            changed,
                        ));
                    }
                }
                Err(self.package_manager_missing_error(manager))
            }
            PackageAction::Uninstall {
                manager, packages, ..
            } => {
                for pm in self.registry.available_package_managers() {
                    if pm.name() == manager {
                        let removed = pm.uninstall(packages, &cx);
                        // A removal takes a binary OFF `PATH` — the mirror of
                        // the install side, and just as invisible to a memo
                        // keyed on `PATH` alone. Reported whether or not the
                        // command as a whole succeeded, because a partial
                        // uninstall has already deleted what it deleted.
                        crate::invalidate_command_resolution();
                        removed?;
                        return Ok((
                            format!("package:{}:uninstall:{}", manager, packages.join(",")),
                            true,
                        ));
                    }
                }
                Err(self.package_manager_missing_error(manager))
            }
            // A planned skip ran nothing by construction, so it neither counts
            // as a change nor triggers the onChange scripts a change gates.
            PackageAction::Skip { manager, .. } => Ok((format!("package:{}:skip", manager), false)),
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
                .package_managers()
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
                let refreshed = pm.refresh_index(&cx);
                // A refreshed index changes what the manager OFFERS, and the
                // available-version memo is keyed on the resolution generation
                // alone — so without this, every offer taken before the refresh
                // would still be answered to every caller after it. Reported
                // whether or not the refresh succeeded: a partial `apt-get
                // update` has already rewritten the lists it managed to fetch.
                crate::invalidate_command_resolution();
                if let Err(e) = refreshed {
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
            ManagerAction::Provision { via, .. } => {
                let members = action.provisioned_managers();
                // An earlier node may have provisioned one already. What the
                // node promises is an available manager, not a second run of
                // an installer that is minutes of work and not idempotent for
                // every manager.
                let mut pending = Vec::new();
                for name in &members {
                    if !lookup(name)?.is_available() {
                        pending.push(*name);
                    }
                }
                let outcome = match pending.as_slice() {
                    [] => Ok(()),
                    // A batch of one is the solo path exactly: its own cascade,
                    // its own fallback arm, its own error words. The merged
                    // command below is only reached when merging is what the
                    // line promised.
                    [one] => {
                        // The method travels into the bootstrap so the cascade
                        // runs the mediator the line named — which is also the
                        // mediator whose lane this action holds.
                        lookup(one)?.bootstrap(&cx.for_provision(via))
                    }
                    many => self.provision_batch(many, via),
                };
                // Before the outcome is propagated, and whatever it was: a
                // cascade that failed at its last step may still have put the
                // manager on the machine, and the check below asks a question
                // whose memoized answer predates the install either way. A node
                // whose members were all available already ran nothing and so
                // changed nothing.
                if !pending.is_empty() {
                    crate::invalidate_command_resolution();
                }
                outcome?;
                for name in &members {
                    let pm = lookup(name)?;
                    self.record_bootstrap(pm.as_ref(), via);
                    if !pm.is_available() {
                        return Err(crate::errors::PackageError::BootstrapFailed {
                            manager: (*name).to_string(),
                            message: format!("{name} still not available after bootstrap"),
                        }
                        .into());
                    }
                }
            }
            ManagerAction::Prerequisite {
                tool, installer, ..
            } => {
                let pm = lookup(installer)?;
                self.install_recording_created(pm.as_ref(), std::slice::from_ref(tool), &cx)?;
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
        // A manager-backed install counts as changed only for the entries the
        // machine still lacks. The planner dropped everything the manager
        // reported installed (`Reconciler::diffing_installed`), but it did so
        // BEFORE the `Prerequisites` phase ran, and that phase installs
        // packages — `apt install npm pipx` provisions two managers and lands
        // two apt packages this module may declare itself. The set is re-read
        // below; an action left with nothing to install ran and changed
        // nothing, which is a skip.
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
                    .package_managers()
                    .iter()
                    .find(|m| m.name() == first.manager);

                if let Some(pm) = pm {
                    let cx = self.cx();
                    let pending: Vec<String> = match self.installed_now(pm.as_ref(), &cx) {
                        Some(installed) => pkgs
                            .iter()
                            .filter(|pkg| {
                                super::Reconciler::package_survives_elision(
                                    pm.as_ref(),
                                    &installed,
                                    pkg,
                                )
                            })
                            .map(|pkg| pkg.resolved_name.clone())
                            .collect(),
                        None => pkg_names.clone(),
                    };
                    if !pending.is_empty() {
                        self.install_recording_created(pm.as_ref(), &pending, &cx)?;
                        manager_changed = true;
                    }
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
    ) -> Result<(String, bool)> {
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
            let written = match record.kind {
                PathDirRecord::Declared => self
                    .state
                    .record_bootstrapped_path_dirs(&record.manager, &record.dirs),
                PathDirRecord::Created => self
                    .state
                    .add_bootstrapped_path_dirs(&record.manager, &record.dirs),
            };
            if let Err(e) = written {
                tracing::warn!(
                    "cannot record PATH directories for bootstrapped {}: {e}",
                    record.manager
                );
            }
        }
    }
}
