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
