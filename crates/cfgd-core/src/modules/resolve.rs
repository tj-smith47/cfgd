//! Package and file resolution — turn LoadedModules into ResolvedModules.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use std::collections::HashSet;

use serde::Serialize;

use crate::config::ModulePackageEntry;
use crate::errors::{ModuleError, Result};
use crate::platform::Platform;
use crate::providers::{PackageManager, PackageManagerExt};

use crate::errors::CfgdError;

use super::git::{fetch_git_source, is_git_source, parse_git_source};
use super::loader::resolve_dependency_order;
use super::lockfile::load_all_modules;
use super::registry::resolve_profile_module_name;
use super::{LoadedModule, ResolvedFile, ResolvedModule, ResolvedPackage, SourceModuleRoot};

// ---------------------------------------------------------------------------
// Package resolution
// ---------------------------------------------------------------------------

/// A declared floor no available manager meets, for a package that is ALSO a
/// registered manager this host can bootstrap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FloorBootstrap {
    pub package: String,
    pub module: String,
    /// The manager that offered the best proven-below version, and what it offered.
    pub found_in: String,
    pub found: String,
    pub floor: String,
    /// `BootstrapPlan::method`: `rustup`, `nvm`, `homebrew installer`, or a mediator's name.
    pub via: String,
    /// The other modules whose declaration of this package the ONE question
    /// asked about it also answers for.
    ///
    /// Empty on every route [`resolve_package`] mints, which is about a single
    /// entry. Filled only where [`resolve_modules`] folds a run's routes for
    /// one package into the question it asks: the run can deliver one copy of
    /// a manager, so it asks once, and the reader is owed the whole list of
    /// modules that answer rides on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub also_declared_by: Vec<String>,
}

/// What a caller's policy answers a [`FloorBootstrap`] question with.
///
/// Three-way because the two refusals are not one refusal:
/// a person who answered no has already been asked, and telling them to re-run
/// on a terminal is nonsense.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorAnswer {
    /// Take the route: the manager joins the run and the floor rides with it.
    Yes,
    /// A human was asked and said no, or the prompt failed before it could be
    /// answered.
    Declined,
    /// There was nobody to ask: no `--yes`, and no human at a terminal.
    NobodyToAsk,
}

/// A caller's policy on floor bootstrap routes, taken by [`resolve_modules`].
///
/// Every call site states its own, with no default to inherit: `plan` and
/// `apply` may ask, and a read-only or scaffolding verb installs nothing, so a
/// `status` that prompted would be a trap in scripts.
pub type FloorConfirm<'a> = dyn Fn(&FloorBootstrap) -> FloorAnswer + 'a;

/// The policy of every surface that installs nothing, and of the daemon.
pub fn refuse_floor_bootstrap(_: &FloorBootstrap) -> FloorAnswer {
    FloorAnswer::NobodyToAsk
}

impl FloorBootstrap {
    /// The tail every sentence naming a version short of a declared floor ends
    /// on, so the offer this route states and the failure an install settles
    /// with cannot word the same shortfall two ways. A sentence built anywhere
    /// but this module composes it from here.
    pub const BELOW_DECLARED_FLOOR: &'static str = "below the declared minVersion";

    /// What this host offers and why it falls short, as ONE clause: the
    /// confirmation that asks whether to take the route, the plan row that
    /// states it and `cfgd doctor`'s unresolved row all read it, so one offer
    /// cannot be worded three ways.
    pub fn offer_clause(&self) -> String {
        format!(
            "{} offers {} {}, {} {}",
            self.found_in,
            self.package,
            self.found,
            Self::BELOW_DECLARED_FLOOR,
            self.floor
        )
    }

    /// Every module the answer to this question applies to, the one that
    /// declared it first.
    pub fn asking_modules(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.module.as_str())
            .chain(self.also_declared_by.iter().map(String::as_str))
    }

    /// What the confirmation opens on: the offer, and every module whose
    /// declared floor it falls short of. One question covers every module
    /// naming the package, so the sentence agrees with however many asked.
    pub fn asking_clause(&self) -> String {
        let names: Vec<String> = self.asking_modules().map(|m| format!("'{m}'")).collect();
        format!(
            "{} that {} {} {} for",
            self.offer_clause(),
            crate::plural_noun(names.len(), "module"),
            names.join(", "),
            crate::agreeing_verb(names.len(), "ask"),
        )
    }

    /// The route as a NOTE, asking nothing: what this host offers, and
    /// the bootstrap that would meet the floor. The surfaces that state a
    /// route without asking about it (`cfgd doctor`, `cfgd module show
    /// --resolved`, `cfgd status <module>`) read this, so none of them can
    /// turn a fact into a prompt.
    pub fn provisionable_clause(&self) -> String {
        crate::join_clauses([
            self.offer_clause(),
            format!("provisionable via {}", self.via),
        ])
    }

    /// Why a route nobody could be asked about is still a refusal, and what
    /// would let a later run take it.
    pub fn unasked_refusal(&self) -> String {
        crate::join_clauses([
            self.offer_clause(),
            format!(
                "{} can be provisioned via {}: re-run with --yes, or on a terminal",
                self.package, self.via
            ),
        ])
    }

    /// Why a route a person turned down is a refusal. It never tells a reader
    /// at a terminal to re-run on a terminal: they were asked, and the answer
    /// was no.
    pub fn declined_refusal(&self) -> String {
        crate::join_clauses([
            self.offer_clause(),
            format!(
                "the {} provision of {} was declined",
                self.via, self.package
            ),
        ])
    }

    /// What a provision settles with when the route it took delivered a version
    /// still short of the floor the confirmation was given for.
    ///
    /// An associated function: the node holds the four values as plain strings
    /// by then, the route itself having been folded into the plan. It shares
    /// [`Self::BELOW_DECLARED_FLOOR`] with [`Self::offer_clause`], so the
    /// question cfgd asked and the failure it answers with cannot word one
    /// shortfall two ways.
    pub fn delivery_shortfall(via: &str, package: &str, delivered: &str, floor: &str) -> String {
        format!(
            "{via} delivered {package} {delivered}, {} {floor}",
            Self::BELOW_DECLARED_FLOOR
        )
    }

    /// The same shortfall where this run installed NOTHING: the manager was on
    /// the machine already, below the floor a confirmation asked for. A
    /// replayed plan reaches it, and wording it as a delivery would credit the
    /// run with an install it never performed.
    pub fn present_shortfall(package: &str, found: &str, floor: &str) -> String {
        format!(
            "{package} was already present at {found}, {} {floor}",
            Self::BELOW_DECLARED_FLOOR
        )
    }

    /// A floor the run could not judge at all, and why.
    ///
    /// A comparator that cannot read its operands answers no question, so the
    /// node says the floor is unproven. It neither settles green nor claims a
    /// shortfall it did not measure: cfgd asked for a version and must not
    /// report success for an answer it never read.
    pub fn floor_unproven(package: &str, floor: &str, cause: &str) -> String {
        format!("cannot judge {package} against the declared minVersion {floor}: {cause}")
    }
}

/// A declared floor answered by the entry's OWN manager: the package names a
/// registered manager this host holds, and [`judgment`](Self::judgment) is what
/// that manager's binary reports, judged against the floor in its own grammar.
///
/// The judgment travels on the node; it never decides, at resolution time,
/// whether the entry resolves at all. A manager below its floor is a fact about
/// the machine, so every read surface reports it and only the install paths
/// refuse: the alternative ended `status`, `verify`, `diff` and every daemon
/// tick for every module the moment one toolchain slipped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeldManager {
    /// The package, which is also the registered manager's name.
    pub package: String,
    pub module: String,
    pub floor: String,
    pub judgment: FloorJudgment,
}

impl HeldManager {
    /// What this host holds and what that says about the declared floor, as ONE
    /// clause: `cfgd doctor`'s module row, `module show --resolved`'s package
    /// row, `status <module>`'s package row and the planner's refusal all read
    /// it, so one held manager cannot be worded four ways.
    ///
    /// `mgr` is the registered manager this entry names, where the caller has
    /// the registry in hand. It is read only to word the raise a shortfall
    /// names and the home variables an unreadable version asks about; `None`
    /// keeps both generic and still states them, because a surface with no
    /// registry still has to say what is wrong.
    pub fn clause(&self, mgr: Option<&dyn PackageManager>) -> String {
        match &self.judgment {
            FloorJudgment::Met { version } => format!(
                "{} {version} is on this host, at or above the declared minVersion {}",
                self.package, self.floor
            ),
            FloorJudgment::Short { version } => crate::join_clauses([
                format!(
                    "{} {version} is on this host, {} {}",
                    self.package,
                    FloorBootstrap::BELOW_DECLARED_FLOOR,
                    self.floor
                ),
                self.raise_clause(mgr),
            ]),
            FloorJudgment::Unproven { cause } => crate::join_clauses([
                FloorBootstrap::floor_unproven(&self.package, &self.floor, cause),
                self.readable_version_clause(mgr),
            ]),
        }
    }

    /// How the manager itself is raised. A bootstrap cannot raise a manager
    /// already on the machine and nothing cfgd plans installs one over itself,
    /// so the sentence names a real raise and stops there. A composed install
    /// command would put a second copy beside the one in use.
    ///
    /// The manager answers for its own copy first
    /// ([`PackageManager::own_raise`]), because a family whose binary is a
    /// shim is raised by the tool behind the shim; only a manager that raises
    /// itself the way it raises a package falls to
    /// [`PackageManager::upgrade_verb`].
    fn raise_clause(&self, mgr: Option<&dyn PackageManager>) -> String {
        if let Some(command) = mgr.and_then(PackageManager::own_raise) {
            return format!("raise it with `{command}`");
        }
        match mgr.and_then(PackageManager::upgrade_verb) {
            Some(verb) => format!("raise it with {}'s own {verb}", self.package),
            None => format!(
                "nothing cfgd can run raises {}, so it must be raised by hand",
                self.package
            ),
        }
    }

    /// What has to be true for the binary to answer at all, for a version the
    /// run could not read.
    ///
    /// A manager's binary is reached through `PATH`, and a shim resolves the
    /// copy it stands for through the family's own home variables, and a
    /// systemd unit carries neither unless the unit sets them, which is exactly
    /// where this verdict is reached. The variables are named by the manager
    /// ([`PackageManager::home_env_vars`]), with no list here, so a family
    /// that gains one joins the sentence with it.
    fn readable_version_clause(&self, mgr: Option<&dyn PackageManager>) -> String {
        let vars = mgr.map(PackageManager::home_env_vars).unwrap_or(&[]);
        let reach = format!("check that {} is on this process's PATH", self.package);
        if vars.is_empty() {
            return reach;
        }
        format!("{reach} and that {} are set for it", vars.join(" and "))
    }
}

/// What a manager's own version answers a declared floor with.
///
/// The four questions in the ONE order they must be asked: is there a version
/// at all, can this manager compare it, can it read the floor, and does the
/// comparison it then runs say yes. A comparator that could not judge its
/// operands has answered nothing, so [`Unproven`](Self::Unproven) is neither a
/// pass nor a shortfall — every caller words its own verdict from the answer
/// and none of them re-asks the questions.
///
/// It rides on [`HeldManager`], so it reaches the wire: internally tagged under
/// the same `state` key `PackageDisplay` names its own arms with, which keeps a
/// reader's match on one string whichever key a map happens to hold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum FloorJudgment {
    /// Comparable, and at or above the floor.
    Met { version: String },
    /// Comparable, and below the floor. The version is carried back because
    /// every caller's sentence names it.
    Short { version: String },
    /// The question could not be asked, and why.
    Unproven { cause: String },
}

impl FloorJudgment {
    /// Whether the floor was measured and cleared. The two other answers are
    /// different facts but one decision for a caller that has to act: a version
    /// below the floor and a version nothing could read both leave a declared
    /// floor unmet.
    pub fn met(&self) -> bool {
        matches!(self, Self::Met { .. })
    }

    /// The version the judgment was made against, or `None` where nothing
    /// could be read. A surface rendering the operand takes it from here. A
    /// second read of the binary would answer a later moment than the verdict
    /// beside it.
    pub fn version(&self) -> Option<&str> {
        match self {
            Self::Met { version } | Self::Short { version } => Some(version),
            Self::Unproven { .. } => None,
        }
    }
}

/// Judge `version` against a declared `floor` in `mgr`'s own version grammar.
///
/// The comparison belongs to the family that packages the tool: `1:2.30`,
/// `1.2.3,4567` and `2.2.2.0` are all versions a family reads and the shared
/// parser refuses, so a `false` from the shared parser would be an artifact of
/// the parse. It would say nothing about the machine.
pub fn judge_declared_floor(
    mgr: &dyn PackageManager,
    subject: &str,
    floor: &str,
    version: Option<&str>,
) -> FloorJudgment {
    let Some(version) = version else {
        return FloorJudgment::Unproven {
            cause: "it reports no version".to_string(),
        };
    };
    if !mgr.version_comparable(version) {
        return FloorJudgment::Unproven {
            cause: format!("{subject} reports {version}, which it cannot compare"),
        };
    }
    if !mgr.floor_comparable(floor) {
        return FloorJudgment::Unproven {
            cause: format!("{subject} cannot read that floor"),
        };
    }
    match mgr.version_meets_minimum_checked(version, floor) {
        Ok(true) => FloorJudgment::Met {
            version: version.to_string(),
        },
        Ok(false) => FloorJudgment::Short {
            version: version.to_string(),
        },
        Err(e) => FloorJudgment::Unproven {
            cause: crate::output::collapse_to_subject_line(&e),
        },
    }
}

/// What one declared package entry resolves to: the manager it lands on, the
/// route a floor no available manager meets could be met by, or the manager
/// this host already holds at that floor.
// `Debug` because a resolver test panics with the arm it did not expect;
// `ResolvedPackage` already derives it.
#[derive(Debug)]
pub enum PackageResolution {
    Package(Box<ResolvedPackage>),
    Bootstrap(FloorBootstrap),
    /// The entry names a manager this host holds at or above the floor: the
    /// manager itself is the delivery, so there is nothing to install and
    /// nothing to ask.
    HeldByManager(HeldManager),
}

impl From<ResolvedPackage> for PackageResolution {
    fn from(pkg: ResolvedPackage) -> Self {
        PackageResolution::Package(Box::new(pkg))
    }
}

/// Resolve a single module package entry to a concrete (manager, name, version).
///
/// Algorithm:
/// 0. If `platforms` is non-empty and current platform doesn't match → return None (skipped)
/// 1. Determine candidate managers: `prefer` list, or — for a bare entry — the
///    available manager that already HOLDS the package, falling back to
///    `[platform.native_manager()]` (see `holding_manager`)
/// 2. For each candidate:
///    a. If `"script"` — always available, uses the `script` field as installer
///    b. Otherwise: check available + alias resolve + min-version check
/// 3. First satisfying candidate wins
/// 4. If none satisfies, return error with details
///
/// A declared `minVersion` never makes a package unresolvable because a
/// manager could not say what it offers. A candidate whose offered version is
/// known and clears the floor wins where it always did, ahead of any earlier
/// candidate that stated nothing, because that choice is the one the author
/// can observe. A candidate that PROVES it offers below the floor is still
/// passed over. What is left is ignorance, which shows nothing about the
/// machine: the first such candidate in candidate order resolves with no
/// version, carrying its `min_version` on to the live floor check
/// ([`crate::reconciler::package_version_floor`]), which reports an
/// unanswerable floor as an erroring check rather than as a refusal to run at
/// all.
///
/// `installed` is the run's installed-state reader. A bare `- name: npm`
/// means "npm on this machine", not "npm through apt": with a reader wired,
/// a manager that is available and already reports the package installed
/// wins over the platform default, so the entry is satisfied rather than
/// re-installed as a second copy through the default. `None` (a surface with
/// no state to read) keeps the platform default. The in-run twin of this rule
/// is `Reconciler::provisioned`: a tool THIS run's own `Bootstrap` phase
/// delivered is not yet in any listing when the plan is read, so
/// `Reconciler::package_survives_elision` elides it from the run's own record
/// of what it provisioned instead. Resolution answers "already here before
/// the run", elision answers "landed by the run"; between them every copy
/// cfgd could know about is counted exactly once.
pub fn resolve_package(
    entry: &ModulePackageEntry,
    module_name: &str,
    platform: &Platform,
    managers: &HashMap<String, &dyn PackageManager>,
    installed: Option<&crate::providers::PackageContext<'_>>,
) -> Result<Option<PackageResolution>> {
    // Platform filter: skip entirely if platforms is non-empty and doesn't match
    if !crate::platform::PlatformGated::applies_to(entry, platform) {
        return Ok(None);
    }

    let candidates: Vec<String> = if entry.prefer.is_empty() {
        let default = platform.native_manager();
        vec![
            installed
                .and_then(|cx| holding_manager(entry, default, managers, cx))
                .unwrap_or(default)
                .to_string(),
        ]
    } else {
        entry.prefer.clone()
    };

    // Filter out denied managers
    let candidates: Vec<String> = candidates
        .into_iter()
        .filter(|c| !entry.deny.contains(c))
        .collect();

    // A candidate that is available and states nothing about what it offers,
    // kept in candidate order so an authored `prefer` still decides which of
    // them stands in. Used only once the whole walk has failed to find a
    // candidate that proves the floor met.
    let mut unproven: Option<ResolvedPackage> = None;
    // Whether some candidate proved it offers below the floor, which is the
    // only way a floor can make a package genuinely unresolvable.
    let mut proven_below = false;
    // The (manager, version) of the first candidate PROVEN below the floor, the
    // operands a route's own clause quotes. `None` until one proves it.
    let mut best_found: Option<(String, String)> = None;

    for candidate in &candidates {
        // Special "script" manager — always available, uses custom install script
        if candidate == crate::SCRIPT_SENTINEL {
            let script = entry
                .script
                .as_ref()
                .ok_or_else(|| ModuleError::InvalidSpec {
                    name: module_name.to_string(),
                    message: format!(
                        "package '{}' has 'script' in prefer list but no 'script' field defined",
                        entry.name
                    ),
                })?;
            return Ok(Some(
                ResolvedPackage {
                    canonical_name: entry.name.clone(),
                    resolved_name: entry.name.clone(),
                    manager: crate::SCRIPT_SENTINEL.to_string(),
                    // `script` only ever reaches a candidate list the author
                    // wrote: it is not any platform's native manager.
                    manager_declared: true,
                    version: None,
                    script: Some(script.clone()),
                    creates: entry.creates.clone(),
                    only_if: entry.only_if.clone(),
                    unless: entry.unless.clone(),
                    min_version: entry.min_version.clone(),
                }
                .into(),
            ));
        }

        let mgr = match managers.get(candidate.as_str()) {
            Some(m) => *m,
            None => continue,
        };

        // Asked once: `is_available()` is a PATH probe, and this runs per
        // candidate manager of every declared package.
        let available = mgr.is_available();
        let bootstrappable = !available && mgr.can_bootstrap();
        if !available && !bootstrappable {
            continue;
        }

        let resolved_name = entry
            .aliases
            .get(candidate)
            .cloned()
            .unwrap_or_else(|| entry.name.clone());
        // Whoever chose this manager: the author, when the candidate came out
        // of their own `prefer` list or their `aliases` map names it, and cfgd
        // otherwise. See `ResolvedPackage::manager_declared`.
        let manager_declared = !entry.prefer.is_empty() || entry.aliases.contains_key(candidate);

        // If the manager isn't installed yet but can be bootstrapped, resolve
        // optimistically — versions cannot be queried until it's installed.
        if bootstrappable {
            return Ok(Some(
                ResolvedPackage {
                    canonical_name: entry.name.clone(),
                    resolved_name,
                    manager: candidate.clone(),
                    manager_declared,
                    version: None,
                    script: None,
                    creates: None,
                    only_if: None,
                    unless: None,
                    min_version: entry.min_version.clone(),
                }
                .into(),
            ));
        }

        // A floor the manager cannot read rejects nothing: making every
        // candidate `continue` on it would delete the package from the plan
        // over a declaration whose only real problem is that nobody can parse
        // it. The verify pass owns that report (`VersionFloor::Unreadable`).
        let floor = entry
            .min_version
            .as_deref()
            .filter(|min| mgr.floor_comparable(min));
        if let Some(min_ver) = floor {
            match mgr.available_version_memoized(&resolved_name) {
                Ok(Some(ver)) => {
                    // Manager-aware: pkg (FreeBSD) versions are not semver, so the
                    // manager compares against its own scheme; everyone else falls
                    // through to the loose-semver default.
                    if !mgr.version_meets_minimum(&ver, min_ver) {
                        proven_below = true;
                        // First candidate wins, matching `prefer` order: the
                        // author's own ordering decides which offer is quoted.
                        best_found.get_or_insert_with(|| (candidate.clone(), ver));
                        continue;
                    }
                    return Ok(Some(
                        ResolvedPackage {
                            canonical_name: entry.name.clone(),
                            resolved_name,
                            manager: candidate.clone(),
                            manager_declared,
                            version: Some(ver),
                            script: None,
                            creates: None,
                            only_if: None,
                            unless: None,
                            min_version: entry.min_version.clone(),
                        }
                        .into(),
                    ));
                }
                // The manager answered nothing, or could not be asked at all.
                // Neither says the floor is unmet, so the candidate stands by
                // in case nothing better is found.
                Ok(None) | Err(_) => {
                    unproven.get_or_insert_with(|| ResolvedPackage {
                        canonical_name: entry.name.clone(),
                        resolved_name,
                        manager: candidate.clone(),
                        manager_declared,
                        version: None,
                        script: None,
                        creates: None,
                        only_if: None,
                        unless: None,
                        min_version: entry.min_version.clone(),
                    });
                }
            }
        } else {
            // No min-version: first available manager wins, and nothing about
            // that choice depends on what the manager currently offers — so the
            // version query is left to `fill_available_versions`, which the
            // paths that DISPLAY a version call and the read paths do not.
            return Ok(Some(
                ResolvedPackage {
                    canonical_name: entry.name.clone(),
                    resolved_name,
                    manager: candidate.clone(),
                    manager_declared,
                    version: None,
                    script: None,
                    creates: None,
                    only_if: None,
                    unless: None,
                    min_version: entry.min_version.clone(),
                }
                .into(),
            ));
        }
    }

    if let Some(pkg) = unproven {
        return Ok(Some(pkg.into()));
    }

    // Asked before the route and before the refusal, and only here: a run that
    // provisioned this manager to meet the floor leaves a machine where every
    // LISTING is still below it, so the walk above proves below on every later
    // run and the entry that converged the machine is the one that refuses.
    // What the manager's own binary reports is the fact that answers it.
    if proven_below && let Some(held) = held_manager_answer(entry, module_name, managers) {
        return Ok(Some(PackageResolution::HeldByManager(held)));
    }

    if proven_below
        // Only a proven-below floor can be rescued this way. "No manager at
        // all" is already answered by the optimistic `bootstrappable` arm
        // above, which resolves.
        && let Some(route) = floor_bootstrap_route(entry, module_name, managers, &best_found)
    {
        return Ok(Some(PackageResolution::Bootstrap(route)));
    }

    let reason = if proven_below {
        proven_below_reason(entry.min_version.as_deref().unwrap_or("any"))
    } else {
        "no manager for it is available on this host, and none can be bootstrapped".to_string()
    };
    Err(ModuleError::UnresolvablePackage {
        module: module_name.to_string(),
        package: entry.name.clone(),
        reason,
    }
    .into())
}

/// Why a package every available manager proved itself below the floor of
/// cannot be resolved, for the entry no bootstrap route and no manager on this
/// host can answer.
fn proven_below_reason(floor: &str) -> String {
    format!(
        "every available manager offers a version {} {floor}",
        FloorBootstrap::BELOW_DECLARED_FLOOR
    )
}

/// What the manager this entry NAMES, already on this host, answers the
/// declared floor with. `None` where the rule does not apply: the entry names
/// no registered manager, that manager is not here, or nothing declared a
/// floor.
///
/// Never a refusal, whatever the verdict: resolution is atomic, so refusing
/// here takes every other module's answer down with this one, and the surfaces
/// that only READ the machine are exactly the ones a reader reaches for when a
/// toolchain has slipped. The install paths refuse instead, at the planner,
/// for the one module holding the unmet floor.
///
/// `tool_version()` spawns, so this runs on the proven-below exit alone and
/// asks once: every entry reaching it has already failed every listing.
fn held_manager_answer(
    entry: &ModulePackageEntry,
    module_name: &str,
    managers: &HashMap<String, &dyn PackageManager>,
) -> Option<HeldManager> {
    let floor = entry.min_version.as_deref()?;
    let mgr = *managers.get(entry.name.as_str())?;
    if !mgr.is_available() {
        return None;
    }
    let name = entry.name.as_str();
    let version = mgr.tool_version();
    Some(HeldManager {
        package: name.to_string(),
        module: module_name.to_string(),
        floor: floor.to_string(),
        judgment: judge_declared_floor(mgr, name, floor, version.as_deref()),
    })
}

/// The route a proven-below floor could be met by: the package names a
/// REGISTERED manager that is not on this host, and that manager's own cascade
/// runs here.
fn floor_bootstrap_route(
    entry: &ModulePackageEntry,
    module_name: &str,
    managers: &HashMap<String, &dyn PackageManager>,
    found: &Option<(String, String)>,
) -> Option<FloorBootstrap> {
    let (mgr, route) = floor_bootstrap_via(&entry.name, entry, module_name, managers, found)?;
    // The manager the route runs through, asked of the value the derivation
    // resolved, with no second lookup: a manager already on this host
    // has nothing left to bootstrap, because a second copy of it would not
    // raise what it offers, and the refusal stands.
    (!mgr.is_available()).then_some(route)
}

/// That route through ONE named manager, asked without regard to whether this
/// host already has it, so a caller walking the registry can price every
/// manager that declares the package. The resolved manager comes back with the
/// route so its caller asks the host question of that same value.
///
/// `manager` is the package's own name wherever the resolver calls this, which
/// is what makes the route's `package` field name a registered manager. The
/// field is the ENTRY's name either way, so a caller naming a manager the entry
/// does not gets a route naming that entry.
///
/// `bootstrap_plan_given(&|_| false)` prices the cascade against the host as it
/// stands, so a manager whose only arm is a mediator this host lacks offers
/// nothing and the refusal stands.
pub fn floor_bootstrap_via<'m>(
    manager: &str,
    entry: &ModulePackageEntry,
    module_name: &str,
    managers: &HashMap<String, &'m dyn PackageManager>,
    found: &Option<(String, String)>,
) -> Option<(&'m dyn PackageManager, FloorBootstrap)> {
    let mgr = *managers.get(manager)?;
    let floor = entry.min_version.clone()?;
    let via = mgr.bootstrap_plan_given(&|_| false)?.method;
    let (found_in, found) = found.clone()?;
    Some((
        mgr,
        FloorBootstrap {
            package: entry.name.clone(),
            module: module_name.to_string(),
            found_in,
            found,
            floor,
            via,
            also_declared_by: Vec::new(),
        },
    ))
}

/// The available manager that already holds a bare entry's package, when one
/// does.
///
/// The platform default is asked first, so a converged machine pays one
/// listing and no more; only when the default does not hold the package are
/// the other available, non-denied managers asked, in name order so two
/// holders answer the same way on every run. A `prefer` list never reaches
/// here: an authored order is a statement about WHICH manager, and it is
/// honoured even when another manager holds the package. A manager that
/// cannot be enumerated answers "does not hold it", the same fail-open the
/// planner's own elision takes.
fn holding_manager<'m>(
    entry: &ModulePackageEntry,
    default: &'m str,
    managers: &HashMap<String, &'m dyn PackageManager>,
    cx: &crate::providers::PackageContext<'_>,
) -> Option<&'m str> {
    let holds = |name: &str| {
        let mgr = *managers.get(name)?;
        if entry.deny.iter().any(|d| d == name) || !mgr.is_available() {
            return None;
        }
        let resolved_name = entry
            .aliases
            .get(name)
            .map_or(entry.name.as_str(), String::as_str);
        cx.installed_for(mgr)
            .ok()?
            .contains(&mgr.package_identity(resolved_name))
            .then_some(mgr.name())
    };
    if let Some(found) = holds(default) {
        return Some(found);
    }
    let mut others: Vec<&str> = managers
        .keys()
        .map(String::as_str)
        .filter(|name| *name != default)
        .collect();
    others.sort_unstable();
    others.into_iter().find_map(holds)
}

/// Resolve all packages in a module spec.
/// Packages filtered out by platform constraints are silently skipped.
pub fn resolve_module_packages(
    module: &LoadedModule,
    platform: &Platform,
    managers: &HashMap<String, &dyn PackageManager>,
    installed: Option<&crate::providers::PackageContext<'_>>,
) -> Result<(Vec<ResolvedPackage>, Vec<FloorBootstrap>, Vec<HeldManager>)> {
    let mut resolved = Vec::with_capacity(module.spec.packages.len());
    let mut routes = Vec::new();
    let mut held = Vec::new();
    for entry in &module.spec.packages {
        match resolve_package(entry, &module.name, platform, managers, installed)? {
            Some(PackageResolution::Package(pkg)) => resolved.push(*pkg),
            Some(PackageResolution::Bootstrap(route)) => routes.push(route),
            // Nothing to install and nothing to plan: the delivery is the
            // manager, and it is already here.
            Some(PackageResolution::HeldByManager(entry)) => held.push(entry),
            None => {}
        }
    }
    Ok((resolved, routes, held))
}

/// Price every resolved package that does not already carry a version, so a
/// surface that RENDERS one has it.
///
/// Resolution itself no longer asks: a package with no `minVersion` is placed on
/// the first available manager whatever that manager currently offers, so the
/// query answered nothing about the outcome and was paid by `status`, `diff`,
/// `verify`, `compliance`, `checkin` and `decide` — none of which show a version
/// — once per declared package, on every invocation and every daemon tick.
///
/// Call it from the paths that consume the version and from no others. Two
/// surfaces do: `cfgd doctor` and `cfgd module show`, which print a version
/// per DECLARED package without planning. Every PLANNING path — `cfgd apply`,
/// `cfgd plan`, the daemon's reconcile tick, both `cfgd init` apply paths,
/// and `cfgd module create --apply` — routes through
/// [`crate::reconciler::Reconciler::fill_planned_versions`] instead, the
/// survivor-gated form of this fill: a package the machine already holds is
/// elided from the plan, so it renders and stores nothing and its version
/// query buys nothing, while a package that does get planned is priced
/// through the same memoized query and renders byte-identically.
///
/// The gating reproduces what resolution used to do exactly, so those surfaces
/// render byte-identically: a package already carrying a version (the
/// `minVersion` check found one) is left alone, `script` packages have no
/// manager to ask, and a manager that is not available is not asked — an
/// unavailable-but-bootstrappable manager resolved optimistically with no
/// version before this existed and still does.
pub fn fill_available_versions(
    packages: &mut [ResolvedPackage],
    managers: &HashMap<String, &dyn PackageManager>,
) {
    for pkg in packages {
        let Some(mgr) = priceable_manager(pkg, managers) else {
            continue;
        };
        price_package(pkg, mgr);
    }
}

/// The manager `pkg` can be priced through, when pricing applies at all:
/// `None` for a package already carrying a version (the `minVersion` check
/// found one), a `script` package (no manager to ask), an unregistered
/// manager, or one that is not available (a bootstrappable manager resolves
/// optimistically with no version). The ONE askability gate both
/// [`fill_available_versions`] and
/// [`crate::reconciler::Reconciler::fill_planned_versions`] read, so the
/// survivor gate stays their only difference.
pub(crate) fn priceable_manager<'m>(
    pkg: &ResolvedPackage,
    managers: &HashMap<String, &'m dyn PackageManager>,
) -> Option<&'m dyn PackageManager> {
    if pkg.version.is_some() || pkg.manager == crate::SCRIPT_SENTINEL {
        return None;
    }
    let mgr = *managers.get(pkg.manager.as_str())?;
    mgr.is_available().then_some(mgr)
}

/// Price `pkg` through `mgr`'s memoized offer. A manager that cannot answer
/// is not a failure — the version is a display detail, and the package still
/// installs.
pub(crate) fn price_package(pkg: &mut ResolvedPackage, mgr: &dyn PackageManager) {
    pkg.version = mgr
        .available_version_memoized(&pkg.resolved_name)
        .ok()
        .flatten();
}

// ---------------------------------------------------------------------------
// File resolution
// ---------------------------------------------------------------------------

/// Resolve module file entries to concrete local paths.
/// Local sources are resolved relative to the module directory.
/// Git sources are cloned/fetched to cache and resolved to the local cache path.
pub fn resolve_module_files(
    module: &LoadedModule,
    cache_base: &Path,
    printer: &crate::output::Printer,
) -> Result<Vec<ResolvedFile>> {
    let mut resolved = Vec::new();

    for entry in &module.spec.files {
        if is_git_source(&entry.source) {
            let git_src = parse_git_source(&entry.source)?;
            let local_path = fetch_git_source(&git_src, cache_base, &module.name, printer)?;

            resolved.push(ResolvedFile {
                source: local_path,
                target: crate::expand_tilde(Path::new(&entry.target)),
                is_git_source: true,
                strategy: entry.strategy,
                encryption: entry.encryption.clone(),
                permissions: entry.permissions.clone(),
                patch: entry.patch.clone(),
            });
        } else if entry.source.is_empty() {
            // A `strategy: Patch` entry needs no source. Joining an empty
            // relative path onto the module directory would yield the module
            // directory itself, which every downstream `source.is_dir()` /
            // `source.exists()` branch would read as a deployable payload.
            resolved.push(ResolvedFile {
                source: PathBuf::new(),
                target: crate::expand_tilde(Path::new(&entry.target)),
                is_git_source: false,
                strategy: entry.strategy,
                encryption: entry.encryption.clone(),
                permissions: entry.permissions.clone(),
                patch: entry.patch.clone(),
            });
        } else {
            // Local path — relative to module directory
            let rel = std::path::Path::new(&entry.source);
            // `source: .` is the module's own directory — the documented way to
            // deploy a module's whole tree, and a legitimate answer here even
            // though it names nothing of its own.
            crate::validate_no_traversal_allowing_self(rel).map_err(|e| {
                ModuleError::InvalidSpec {
                    name: module.name.clone(),
                    message: format!("file source '{}' is not usable: {e}", entry.source),
                }
            })?;
            let source = module.dir.join(rel);
            // Verify the resolved path stays within the module directory
            // (prevents symlink-based escape from module boundary)
            if source.exists()
                && let (Ok(canonical_src), Ok(canonical_dir)) =
                    (source.canonicalize(), module.dir.canonicalize())
                && !canonical_src.starts_with(&canonical_dir)
            {
                return Err(ModuleError::InvalidSpec {
                    name: module.name.clone(),
                    message: format!(
                        "file source '{}' resolves outside module directory",
                        entry.source
                    ),
                }
                .into());
            }
            // `private` marks the source local-only: on a machine where it
            // does not exist the entry resolves to nothing, so no downstream
            // consumer has to know the flag existed. An absent NON-private
            // source survives resolution on purpose — the plan refuses it
            // as `FileError::SourceNotFound`, the same refusal the profile
            // file path makes, instead of quietly deploying nothing.
            if entry.private && !source.exists() {
                continue;
            }
            resolved.push(ResolvedFile {
                source,
                target: crate::expand_tilde(Path::new(&entry.target)),
                is_git_source: false,
                strategy: entry.strategy,
                encryption: entry.encryption.clone(),
                permissions: entry.permissions.clone(),
                patch: entry.patch.clone(),
            });
        }
    }

    Ok(resolved)
}

// ---------------------------------------------------------------------------
// Full module resolution
// ---------------------------------------------------------------------------

/// Resolve a set of modules: load, sort dependencies, resolve packages and files.
/// Includes both local modules and remote modules from the lockfile.
///
/// `installed` is what [`resolve_package`] reads to satisfy a bare entry from
/// the manager that already holds it; every planning path passes the run's own
/// context so resolution and the plan's elision read one enumeration.
#[allow(clippy::too_many_arguments)]
pub fn resolve_modules(
    requested: &[String],
    config_dir: &Path,
    cache_base: &Path,
    source_roots: &[SourceModuleRoot],
    platform: &Platform,
    managers: &HashMap<String, &dyn PackageManager>,
    installed: Option<&crate::providers::PackageContext<'_>>,
    printer: &crate::output::Printer,
    confirm: &FloorConfirm<'_>,
) -> Result<Vec<ResolvedModule>> {
    let all_modules = load_all_modules(config_dir, cache_base, source_roots, printer)?;

    // Resolve profile references (e.g., "community/tmux" → "tmux") to actual module names
    let resolved_names: Vec<String> = requested
        .iter()
        .map(|r| resolve_profile_module_name(r).to_string())
        .collect();
    // tracing-ok: internal resolution-set diagnostic, not user-facing — nothing
    // else prints the requested module set before dependency order is walked
    tracing::debug!(names = ?resolved_names, "resolving modules");

    let order = resolve_dependency_order(&resolved_names, &all_modules)
        .map_err(|e| enrich_not_found(e, source_roots))?;
    // Claimed here because this is the one place both lists are in hand: the
    // set the caller asked for, and the order a `depends:` walk grew out of it.
    let dep_pulled = |name: &String| !resolved_names.contains(name);

    // Determine platform-skipped modules up front so an active module that
    // depends on a skipped one can be rejected as a config error before any
    // package/file resolution runs.
    let skipped: HashSet<&str> = order
        .iter()
        .filter(|name| {
            !crate::platform::PlatformGated::applies_to(&all_modules[*name].spec, platform)
        })
        .map(|name| name.as_str())
        .collect();

    validate_no_active_dependents_on_skipped(&order, &skipped, |name| {
        let spec = &all_modules[name].spec;
        (&spec.depends, &spec.platforms)
    })?;

    // Fail-closed: a source not permitted to run scripts may not deliver a
    // module body carrying lifecycle scripts or `prefer: [script]` package
    // installs. Judged over `order` — the modules THIS resolution actually
    // references, requested or depends-pulled — never over everything a
    // source's manifest merely offers: an unreferenced sibling module in the
    // same source carrying a script is not this subscriber's problem, and a
    // platform-skipped one never runs its body at all.
    let scripts_permitted_by_source: HashMap<&str, bool> = source_roots
        .iter()
        .map(|root| (root.source_name.as_str(), root.scripts_permitted))
        .collect();
    for name in &order {
        if skipped.contains(name.as_str()) {
            continue;
        }
        let module = &all_modules[name];
        let Some(source_name) = module.origin.as_deref() else {
            continue;
        };
        let permitted = scripts_permitted_by_source
            .get(source_name)
            .copied()
            .unwrap_or(false);
        if !permitted && let Some(kind) = super::lockfile::module_script_kind(module) {
            return Err(ModuleError::ScriptsNotAllowed {
                source_name: source_name.to_string(),
                module: name.clone(),
                kind,
            }
            .into());
        }
    }

    let mut resolved = Vec::new();
    // A module's own resolution is where this walk waits: a git file source
    // is cloned or fetched here and every manifest is read off disk. Narrated
    // per module at the one place every command's module walk goes through, so
    // the wait names what it is waiting on rather than standing silent.
    printer.narrate("Resolving modules", |sp| -> Result<()> {
        for name in &order {
            sp.set_message(format!("Resolving module:{name}"));
            let module = &all_modules[name];

            // Platform-gated out: emit a placeholder carrying the skip reason and
            // empty contents. The visible Skip action is produced by plan_modules.
            // Resolving packages/files here is wasteful and could error on the
            // other platform's assets, so it is deliberately skipped.
            if skipped.contains(name.as_str()) {
                resolved.push(ResolvedModule::skipped(
                    name.clone(),
                    module.dir.clone(),
                    module.spec.depends.clone(),
                    dep_pulled(name),
                    format!(
                        "platform not matched (requires: {})",
                        module.spec.platforms.join(", ")
                    ),
                    module.origin.clone(),
                ));
                continue;
            }

            let (packages, floor_bootstraps, held_managers) =
                resolve_module_packages(module, platform, managers, installed)?;
            let files = resolve_module_files(module, cache_base, printer)?;

            let scripts = module.spec.scripts.as_ref();
            let pre_apply_scripts = scripts.map(|s| s.pre_apply.clone()).unwrap_or_default();
            let post_apply_scripts = scripts.map(|s| s.post_apply.clone()).unwrap_or_default();
            let pre_reconcile_scripts =
                scripts.map(|s| s.pre_reconcile.clone()).unwrap_or_default();
            let post_reconcile_scripts = scripts
                .map(|s| s.post_reconcile.clone())
                .unwrap_or_default();
            let on_change_scripts = scripts.map(|s| s.on_change.clone()).unwrap_or_default();
            let on_drift_scripts = scripts.map(|s| s.on_drift.clone()).unwrap_or_default();

            resolved.push(ResolvedModule {
                name: name.clone(),
                packages,
                floor_bootstraps,
                held_managers,
                files,
                // Filtered here, beside the package filter above: a gated
                // entry is not part of this host's desired state, so it
                // reaches no surface rather than reaching them annotated.
                env: crate::platform::applicable_here(&module.spec.env, platform)
                    .cloned()
                    .collect(),
                aliases: crate::platform::applicable_here(&module.spec.aliases, platform)
                    .cloned()
                    .collect(),
                system: module.spec.system.clone(),
                pre_apply_scripts,
                post_apply_scripts,
                pre_reconcile_scripts,
                post_reconcile_scripts,
                on_change_scripts,
                on_drift_scripts,
                depends: module.spec.depends.clone(),
                dep_pulled: dep_pulled(name),
                dir: module.dir.clone(),
                platform_skip_reason: None,
                origin: module.origin.clone(),
            });
        }
        Ok(())
    })?;

    // Asked after the narrate block closes: `inquire` writes
    // straight to the terminal past the renderer, so a confirm drawn under a
    // live spinner is painted over by the next tick. Asked after every module
    // resolved, too — a hard refusal has already ended the run by then, which
    // is what keeps anybody from being asked to approve a toolchain install
    // for a configuration that cannot resolve anyway.
    confirm_floor_bootstraps(&resolved, managers, confirm)?;

    Ok(resolved)
}

/// Ask once per package about the floors a manager bootstrap would meet, and
/// let every route for that package stand or refuse the run on the one answer.
///
/// Two modules naming one manager are one question: the run can deliver a
/// single copy of it, so the question quotes the strictest floor any of them
/// asked for — the same [`crate::effective::stricter_floor`] the planner folds
/// the confirmed routes with, so a floor cannot survive here and lose there —
/// and names every module that asked.
///
/// The questions are matched by package through a linear scan: a run carries
/// at most a handful of floored packages, and the overwhelmingly common one
/// carries none and leaves before anything is allocated.
fn confirm_floor_bootstraps(
    resolved: &[ResolvedModule],
    managers: &HashMap<String, &dyn PackageManager>,
    confirm: &FloorConfirm<'_>,
) -> Result<()> {
    if resolved.iter().all(|m| m.floor_bootstraps.is_empty()) {
        return Ok(());
    }
    let mut questions: Vec<FloorBootstrap> = Vec::new();
    for route in resolved.iter().flat_map(|m| m.floor_bootstraps.iter()) {
        let Some(question) = questions.iter_mut().find(|q| q.package == route.package) else {
            questions.push(route.clone());
            continue;
        };
        if !question.asking_modules().any(|m| m == route.module) {
            question.also_declared_by.push(route.module.clone());
        }
        // Judged in the grammar of the manager being provisioned, since that
        // is whose versions both floors are written in. Read from borrows: a
        // comparator that answers `None` must leave the floor exactly as it
        // was, and a floor moved out first is gone by then.
        let kept = crate::effective::stricter_floor(
            &Some(question.floor.clone()),
            &Some(route.floor.clone()),
            managers.get(&route.package).copied(),
        );
        // The offer travels with the floor it falls short of. Two modules
        // naming one package can price it against different candidate sets
        // (their own `prefer` lists), so the best proven-below offer is per
        // ROUTE, and a sentence pairing one module's floor with the other's
        // offer states a shortfall neither module declared. `via` is the
        // manager's own bootstrap method and is identical on every route
        // naming it, so it has nothing to move. `stricter_floor` answers with
        // the earlier spelling where neither floor is stricter, which is what
        // leaves the offer already on the question in place.
        if let Some(kept) = kept
            && kept != question.floor
        {
            question.floor = kept;
            question.found_in = route.found_in.clone();
            question.found = route.found.clone();
        }
    }
    for question in &questions {
        let reason = match confirm(question) {
            FloorAnswer::Yes => continue,
            FloorAnswer::Declined => question.declined_refusal(),
            FloorAnswer::NobodyToAsk => question.unasked_refusal(),
        };
        return Err(ModuleError::UnresolvablePackage {
            module: question.module.clone(),
            package: question.package.clone(),
            reason,
        }
        .into());
    }
    Ok(())
}

/// Enrich a `ModuleError::NotFound` raised during dependency resolution: when the
/// missing name appears in some source root's `offered` allow-list, the publisher
/// declared it in `provides.modules` but failed to deliver the body — surface that
/// as `OfferedButMissing` naming the source. When several roots offer the name, the
/// highest-priority one is named (tie-break on source_name) so the message matches
/// the source that would have won the body race. All other errors pass through;
/// both variants keep the exit-6 NotFound code.
fn enrich_not_found(err: CfgdError, source_roots: &[SourceModuleRoot]) -> CfgdError {
    if let CfgdError::Module(ModuleError::NotFound { name }) = &err
        && let Some(root) = source_roots
            .iter()
            .filter(|root| root.offered.iter().any(|m| m == name))
            .max_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| b.source_name.cmp(&a.source_name))
            })
    {
        return ModuleError::OfferedButMissing {
            name: name.clone(),
            source_name: root.source_name.clone(),
        }
        .into();
    }
    err
}

/// Reject an active module that depends on a platform-skipped one.
///
/// Pure (no I/O): `order` is the resolution order, `skipped` the set of
/// lookup-names gated out on the current platform, and `lookup` returns each
/// module's `(depends, platforms)`. A skipped module's own dependencies are not
/// checked — it will never run. Dependency names pass through
/// `resolve_profile_module_name` before comparison for robustness, though the
/// loader has already required each `depends` entry to be a bare module key by
/// the time this runs.
pub(crate) fn validate_no_active_dependents_on_skipped<'a, F>(
    order: &'a [String],
    skipped: &HashSet<&str>,
    lookup: F,
) -> Result<()>
where
    F: Fn(&'a str) -> (&'a [String], &'a [String]),
{
    for name in order {
        if skipped.contains(name.as_str()) {
            continue;
        }
        let (depends, _) = lookup(name);
        for dep in depends {
            let dep_name = resolve_profile_module_name(dep);
            if skipped.contains(dep_name) {
                let (_, dep_platforms) = lookup(dep_name);
                return Err(ModuleError::DependencyPlatformSkipped {
                    module: name.clone(),
                    dependency: dep_name.to_string(),
                    platforms: dep_platforms.join(", "),
                }
                .into());
            }
        }
    }
    Ok(())
}
