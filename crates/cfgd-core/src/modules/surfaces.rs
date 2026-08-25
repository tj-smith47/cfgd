//! What a module DECLARES, tallied once per report.

use crate::config::{EnvVar, ModuleSpec, ShellAlias};

/// One lifecycle hook and the script bodies declared under it.
#[derive(Debug, Clone)]
pub struct HookScripts {
    /// The hook name as the YAML spells it (`preApply`).
    pub hook: &'static str,
    /// Each entry's `run` body, in declaration order — the order they run in.
    pub bodies: Vec<String>,
}

/// The declared surfaces of one module: the counts a summary line reports and
/// the items an inventory lists.
///
/// The ONE derivation both module-reporting surfaces read from, so a count in
/// a summary row and the list it summarizes can never disagree about what the
/// module declares. Everything here is DECLARED state — what the machine holds
/// is a separate question, answered by a scan.
#[derive(Debug, Clone, Default)]
pub struct ModuleSurfaces {
    pub packages: usize,
    pub files: usize,
    pub env: Vec<EnvVar>,
    pub aliases: Vec<ShellAlias>,
    /// Only the hooks that declare something, in execution order.
    pub scripts: Vec<HookScripts>,
    /// System configurators the module contributes settings to.
    pub system: Vec<String>,
    pub depends: Vec<String>,
}

impl ModuleSurfaces {
    pub fn of(spec: &ModuleSpec) -> Self {
        Self {
            packages: spec.packages.len(),
            files: spec.files.len(),
            env: spec.env.clone(),
            aliases: spec.aliases.clone(),
            scripts: spec
                .scripts
                .as_ref()
                .map(|s| {
                    s.hooks()
                        .into_iter()
                        .filter(|(_, entries)| !entries.is_empty())
                        .map(|(hook, entries)| HookScripts {
                            hook,
                            bodies: entries.iter().map(|e| e.run_str().to_string()).collect(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            system: spec.system.keys().cloned().collect(),
            depends: spec.depends.clone(),
        }
    }

    /// The same tally taken from a RESOLVED module, for a surface that holds
    /// one rather than the document it was loaded from (the fleet-wide
    /// `cfgd status`, which resolves the profile's modules and never re-reads
    /// their specs). Resolution copies each surface across verbatim, so the
    /// two constructors describe the same module — except a platform-skipped
    /// one, whose resolved surfaces are empty because nothing about it applies
    /// on this host.
    pub fn of_resolved(module: &super::ResolvedModule) -> Self {
        Self {
            packages: module.packages.len(),
            files: module.files.len(),
            env: module.env.clone(),
            aliases: module.aliases.clone(),
            scripts: module
                .script_hooks()
                .into_iter()
                .filter(|(_, entries)| !entries.is_empty())
                .map(|(hook, entries)| HookScripts {
                    hook,
                    bodies: entries.iter().map(|e| e.run_str().to_string()).collect(),
                })
                .collect(),
            system: module.system.keys().cloned().collect(),
            depends: module.depends.clone(),
        }
    }

    /// The per-hook script tally a summary row renders: `3 preApply, 6
    /// postApply`, in execution order. `None` when the module declares no
    /// scripts at all, so the row is left out rather than reading empty.
    pub fn script_summary(&self) -> Option<String> {
        if self.scripts.is_empty() {
            return None;
        }
        Some(
            self.scripts
                .iter()
                .map(|h| format!("{} {}", h.bodies.len(), h.hook))
                .collect::<Vec<_>>()
                .join(", "),
        )
    }

    /// The per-hook script counts, in execution order — the breakdown rows a
    /// report renders beneath its total, and the `scriptCounts` payload field.
    /// Empty when the module declares no scripts, the same condition
    /// [`Self::script_summary`] answers `None` to.
    pub fn script_counts(&self) -> Vec<(String, usize)> {
        self.scripts
            .iter()
            .map(|h| (h.hook.to_string(), h.bodies.len()))
            .collect()
    }

    /// How many script entries the module declares across every hook — the
    /// total the per-hook breakdown sums to.
    pub fn script_total(&self) -> usize {
        self.scripts.iter().map(|h| h.bodies.len()).sum()
    }

    /// The names of the hooks that declare something, in execution order.
    pub fn hook_names(&self) -> Vec<String> {
        self.scripts.iter().map(|h| h.hook.to_string()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ScriptEntry, ScriptSpec};

    fn spec_with_scripts(scripts: ScriptSpec) -> ModuleSpec {
        ModuleSpec {
            scripts: Some(scripts),
            ..Default::default()
        }
    }

    #[test]
    fn script_summary_counts_each_hook_in_execution_order() {
        let surfaces = ModuleSurfaces::of(&spec_with_scripts(ScriptSpec {
            // Declared out of order on purpose: the summary reports the order
            // the hooks RUN in, not the order the YAML happened to list them.
            post_apply: vec![
                ScriptEntry::Simple("a".into()),
                ScriptEntry::Simple("b".into()),
            ],
            pre_apply: vec![ScriptEntry::Simple("c".into())],
            ..Default::default()
        }));
        assert_eq!(
            surfaces.script_summary().as_deref(),
            Some("1 preApply, 2 postApply")
        );
        assert_eq!(surfaces.hook_names(), vec!["preApply", "postApply"]);
    }

    /// Each hook's body names the hook it was declared under, so a resolved
    /// vec wired to the wrong name renders a summary that says so — the whole
    /// risk in mirroring `ScriptSpec::hooks()` on the resolved side.
    #[test]
    fn a_resolved_module_tallies_the_same_hooks_its_spec_declared() {
        let spec = spec_with_scripts(ScriptSpec {
            pre_apply: vec![ScriptEntry::Simple("preApply".into())],
            post_apply: vec![
                ScriptEntry::Simple("postApply".into()),
                ScriptEntry::Simple("postApply".into()),
            ],
            pre_reconcile: vec![ScriptEntry::Simple("preReconcile".into())],
            post_reconcile: vec![ScriptEntry::Simple("postReconcile".into())],
            on_drift: vec![ScriptEntry::Simple("onDrift".into())],
            on_change: vec![ScriptEntry::Simple("onChange".into())],
        });
        let scripts = spec.scripts.clone().unwrap_or_default();
        // The same copy-across `modules::resolve` performs.
        let resolved = crate::modules::ResolvedModule {
            pre_apply_scripts: scripts.pre_apply.clone(),
            post_apply_scripts: scripts.post_apply.clone(),
            pre_reconcile_scripts: scripts.pre_reconcile.clone(),
            post_reconcile_scripts: scripts.post_reconcile.clone(),
            on_drift_scripts: scripts.on_drift.clone(),
            on_change_scripts: scripts.on_change.clone(),
            packages: Vec::new(),
            files: Vec::new(),
            ..crate::test_helpers::make_resolved_module("dev-tools")
        };
        let surfaces = ModuleSurfaces::of_resolved(&resolved);
        for hook in &surfaces.scripts {
            assert!(
                hook.bodies.iter().all(|b| b == hook.hook),
                "hook {} was tallied from another hook's entries: {:?}",
                hook.hook,
                hook.bodies
            );
        }
        assert_eq!(
            surfaces.script_summary(),
            ModuleSurfaces::of(&spec).script_summary(),
            "one module, one tally, whichever side it is read from"
        );
    }

    /// The breakdown a report renders under its total row, and the total that
    /// row carries, are the same tally the one-line summary is built from.
    #[test]
    fn script_counts_break_the_total_down_per_hook_in_execution_order() {
        let surfaces = ModuleSurfaces::of(&spec_with_scripts(ScriptSpec {
            post_apply: vec![
                ScriptEntry::Simple("a".into()),
                ScriptEntry::Simple("b".into()),
            ],
            pre_apply: vec![ScriptEntry::Simple("c".into())],
            ..Default::default()
        }));
        assert_eq!(
            surfaces.script_counts(),
            vec![("preApply".to_string(), 1), ("postApply".to_string(), 2)]
        );
        // The literals the fixture declares, not a sum re-derived from the very
        // list above: summing `script_counts` restates the implementation and
        // would pass whatever both sides drifted to together.
        assert_eq!(surfaces.script_total(), 3);
        assert_eq!(
            surfaces.script_summary().as_deref(),
            Some("1 preApply, 2 postApply")
        );
    }

    #[test]
    fn a_module_with_no_scripts_has_no_summary() {
        assert!(
            ModuleSurfaces::of(&ModuleSpec::default())
                .script_summary()
                .is_none()
        );
        // An empty hook is not a declared hook — it opens no phase and has
        // nothing to report.
        assert!(
            ModuleSurfaces::of(&spec_with_scripts(ScriptSpec::default()))
                .script_summary()
                .is_none()
        );
    }
}
