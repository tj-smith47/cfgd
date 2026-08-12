use crate::config::{ResolvedProfile, ScriptShell};
use crate::errors::Result;
use crate::output::Printer;

use super::scripts::{
    ScriptEnvContext, ScriptReport, build_script_env, effective_continue_on_error, execute_script,
    script_default_workdir,
};
use super::types::{ReconcileContext, ScriptAction};

impl<'a> super::Reconciler<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_script_action(
        &self,
        action: &ScriptAction,
        resolved: &ResolvedProfile,
        config_dir: &std::path::Path,
        printer: &Printer,
        context: ReconcileContext,
        shell_override: Option<ScriptShell>,
        abort: &crate::AbortFlag,
    ) -> Result<(String, bool, Option<String>)> {
        match action {
            ScriptAction::Run {
                entry,
                phase,
                origin,
            } => {
                let profile_name = resolved
                    .layers
                    .last()
                    .map(|l| l.profile_name.as_str())
                    .unwrap_or("unknown");

                let env_vars = build_script_env(&ScriptEnvContext {
                    config_dir,
                    profile_name,
                    context,
                    phase,
                    module_name: None,
                    module_dir: None,
                    path_dirs: &super::all_recorded_path_dirs(self.state),
                });

                let working = script_default_workdir(config_dir);
                // The action's ONE display subject, from the same derivation
                // the preview bullet and the phase's alignment column use — a
                // subject spelled locally here is what let the executed line
                // read shorter than the column every trailing field pads to.
                let subject = super::format::script_run_subject(entry.run_str(), phase, origin);
                let (_desc, changed, captured) = execute_script(
                    entry,
                    config_dir,
                    &working,
                    &env_vars,
                    crate::PROFILE_SCRIPT_TIMEOUT,
                    printer,
                    shell_override,
                    Some(abort),
                    ScriptReport {
                        subject: super::scripts::ScriptSubject::Planned(&subject),
                        non_fatal: effective_continue_on_error(entry, phase),
                        ..ScriptReport::default()
                    },
                )?;

                let phase_name = phase.display_name();
                // Resource-id / state-matching key, NOT a display string: this
                // return value becomes `ActionResult.description`, which
                // `parse_resource_from_description` parses back into a
                // managed-resource id. Condensing `run_str()` here would
                // reshape the id and break drift matching against every
                // already-recorded state row for a module with a multi-line
                // inline script — leave it byte-identical (mirrors
                // `format_action_description` in `format.rs`).
                Ok((
                    format!("script:{}:{}", phase_name, entry.run_str()),
                    changed,
                    captured,
                ))
            }
        }
    }
}
