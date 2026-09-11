//! What a module DECLARES, tallied once per report.

use crate::config::{EnvVar, ModuleSpec, ScriptEntry, ShellAlias};
use crate::output::{Doc, ScriptStep, ScriptsForm};

/// One lifecycle hook and the script steps declared under it.
#[derive(Debug, Clone)]
pub struct HookScripts {
    /// The hook name as the YAML spells it (`preApply`).
    pub hook: &'static str,
    /// Each entry, in declaration order — the order they run in.
    pub steps: Vec<DeclaredScript>,
}

/// One declared script entry: its body and the execution knobs it sets.
///
/// The knobs travel with the body because the full Scripts render states them
/// above it, and a surface holding only bodies would have to read the spec a
/// second time to find them.
#[derive(Debug, Clone)]
pub struct DeclaredScript {
    /// The `run` body, verbatim.
    pub body: String,
    /// `timeout`, as the YAML spells the duration (`120s`) — never reformatted,
    /// so the marker states the value the author wrote.
    pub timeout: Option<String>,
    /// `idleTimeout`, spelled the same way.
    pub idle_timeout: Option<String>,
    /// Whether the step declares `continueOnError: true`. A declared `false`
    /// is the default, and a marker naming it would read as a knob in force.
    pub continue_on_error: bool,
}

impl DeclaredScript {
    fn of(entry: &ScriptEntry) -> Self {
        match entry {
            ScriptEntry::Simple(body) => Self {
                body: body.clone(),
                timeout: None,
                idle_timeout: None,
                continue_on_error: false,
            },
            ScriptEntry::Full(cmd) => Self {
                body: cmd.run.clone(),
                timeout: cmd.timeout.clone(),
                idle_timeout: cmd.idle_timeout.clone(),
                continue_on_error: cmd.continue_on_error.unwrap_or(false),
            },
        }
    }

    /// The muted line above this step's body: its position among its hook's
    /// steps, then one clause per knob it declares, in the order
    /// `ScriptCommand` spells them.
    fn marker(&self, position: usize, total: usize) -> String {
        let mut marker = format!("{position}/{total}");
        if let Some(timeout) = &self.timeout {
            marker.push_str(&format!(" {MARKER_SEPARATOR} timeout {timeout}"));
        }
        if let Some(idle) = &self.idle_timeout {
            marker.push_str(&format!(" {MARKER_SEPARATOR} idle {idle}"));
        }
        if self.continue_on_error {
            marker.push_str(&format!(" {MARKER_SEPARATOR} continueOnError"));
        }
        marker
    }
}

/// What joins the clauses of a step's marker line.
const MARKER_SEPARATOR: &str = "\u{b7}";

/// The name of the section every surface lists a module's declared scripts
/// under.
const SCRIPTS_SECTION: &str = "Scripts";

/// The ONE render of the scripts a module declares, for every human surface
/// that shows them: `cfgd module show` and `cfgd status --module`.
///
/// One nested section per declaring hook, headed with the step count, in
/// execution order, because that order is the fact a reader needs. Under
/// [`ScriptsForm::Condensed`] each step is one row carrying its first line;
/// under [`ScriptsForm::Full`] each step states the knobs it declares and then
/// its whole body, highlighted. The renderer owns every coat, indent and blank
/// line (see [`Component::ScriptSteps`]), so two surfaces cannot render one
/// module's scripts as two different shapes.
///
/// Returns the doc untouched when the module declares no script: an empty
/// Scripts section would say the module has hooks that do nothing.
///
/// [`Component::ScriptSteps`]: crate::output::Component::ScriptSteps
pub fn scripts_section(doc: Doc, scripts: &[HookScripts], form: ScriptsForm) -> Doc {
    if scripts.is_empty() {
        return doc;
    }
    doc.section(SCRIPTS_SECTION, |section| {
        scripts.iter().fold(section, |section, hook| {
            let total = hook.steps.len();
            section.subsection_annotated(hook.hook, total.to_string(), |sub| {
                sub.script_steps(
                    hook.steps.iter().enumerate().map(|(index, step)| {
                        let body = match form {
                            ScriptsForm::Full => step.body.clone(),
                            ScriptsForm::Condensed => {
                                crate::output::condense_script_label(&step.body)
                            }
                        };
                        ScriptStep {
                            marker: match form {
                                ScriptsForm::Full => Some(step.marker(index + 1, total)),
                                ScriptsForm::Condensed => None,
                            },
                            body,
                        }
                    }),
                    form,
                )
            })
        })
    })
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
                            steps: entries.iter().map(DeclaredScript::of).collect(),
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
                    steps: entries.iter().map(DeclaredScript::of).collect(),
                })
                .collect(),
            system: module.system.keys().cloned().collect(),
            depends: module.depends.clone(),
        }
    }

    /// The per-hook script tally a summary row renders: `preApply (3 scripts),
    /// postApply (6 scripts)`, in execution order. `None` when the module
    /// declares no scripts at all, so the row is left out rather than reading
    /// empty.
    ///
    /// Subject first, count parenthesised: the row this lands in sits beside a
    /// module's file row (`/home/tj/.config/nvim (6 files)`), and a resource
    /// cell that led with its count would read as a different kind of fact than
    /// its neighbour.
    pub fn script_summary(&self) -> Option<String> {
        if self.scripts.is_empty() {
            return None;
        }
        Some(
            self.scripts
                .iter()
                .map(|h| format!("{} ({})", h.hook, crate::pluralize(h.steps.len(), "script")))
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
            .map(|h| (h.hook.to_string(), h.steps.len()))
            .collect()
    }

    /// How many script entries the module declares across every hook — the
    /// total the per-hook breakdown sums to.
    pub fn script_total(&self) -> usize {
        self.scripts.iter().map(|h| h.steps.len()).sum()
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
            Some("preApply (1 script), postApply (2 scripts)")
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
                hook.steps.iter().all(|s| s.body == hook.hook),
                "hook {} was tallied from another hook's entries: {:?}",
                hook.hook,
                hook.steps
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
            Some("preApply (1 script), postApply (2 scripts)")
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
