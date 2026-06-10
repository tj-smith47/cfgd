use crate::config::{ResolvedProfile, ScriptShell};
use crate::errors::Result;
use crate::output::Printer;

use super::scripts::{build_script_env, execute_script, script_default_workdir};
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
            ScriptAction::Run { entry, phase, .. } => {
                let profile_name = resolved
                    .layers
                    .last()
                    .map(|l| l.profile_name.as_str())
                    .unwrap_or("unknown");

                let env_vars =
                    build_script_env(config_dir, profile_name, context, phase, None, None);

                let working = script_default_workdir(config_dir);
                let (_desc, changed, captured) = execute_script(
                    entry,
                    config_dir,
                    &working,
                    &env_vars,
                    crate::PROFILE_SCRIPT_TIMEOUT,
                    printer,
                    shell_override,
                    Some(abort),
                )?;

                let phase_name = phase.display_name();
                Ok((
                    format!("script:{}:{}", phase_name, entry.run_str()),
                    changed,
                    captured,
                ))
            }
        }
    }
}
