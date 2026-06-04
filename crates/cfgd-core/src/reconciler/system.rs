use crate::config::MergedProfile;
use crate::errors::Result;
use crate::modules::ResolvedModule;
use crate::output::{Printer, Role};

use super::types::SystemAction;

impl<'a> super::Reconciler<'a> {
    pub(super) fn apply_system_action(
        &self,
        action: &SystemAction,
        profile: &MergedProfile,
        modules: &[ResolvedModule],
        printer: &Printer,
    ) -> Result<String> {
        match action {
            SystemAction::SetValue {
                configurator,
                key,
                desired,
                current,
                ..
            } => {
                // Resolve the desired value from the EFFECTIVE system map (profile ⊕
                // modules), the same source plan_system uses. Reading profile.system
                // alone would miss a module-contributed configurator key — the action
                // plans but the apply silently no-ops (the original module-vs-profile
                // coherence gap this branch closes).
                let system = crate::effective::effective_system_map(profile, modules);
                if let Some(desired_value) = system.get(configurator.as_str()) {
                    for sc in self.registry.available_system_configurators() {
                        if sc.name() == configurator {
                            sc.apply(desired_value, printer)?;
                            return Ok(format!(
                                "system:{}.{} ({} → {})",
                                configurator, key, current, desired
                            ));
                        }
                    }
                }
                Ok(format!("system:{}.{}", configurator, key))
            }
            SystemAction::Skip {
                configurator,
                reason,
                unknown,
                ..
            } => {
                // An unknown key (no configurator registered) is a likely typo —
                // warn so it is not missed. A registered-but-unavailable
                // configurator is expected, so render it neutrally.
                if *unknown {
                    printer.status_simple(
                        Role::Warn,
                        format!(
                            "unknown system key '{}' — no such configurator (ignored)",
                            configurator
                        ),
                    );
                } else {
                    printer.status_simple(Role::Skipped, format!("{}: {}", configurator, reason));
                }
                Ok(format!("system:{} (skipped)", configurator))
            }
        }
    }
}
