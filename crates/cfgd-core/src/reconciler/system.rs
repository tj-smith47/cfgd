use crate::config::MergedProfile;
use crate::errors::Result;
use crate::modules::ResolvedModule;
use crate::output::Printer;
use crate::providers::{NoteSink, SystemContext};

use super::types::SystemAction;

impl<'a> super::Reconciler<'a> {
    pub(super) fn apply_system_action(
        &self,
        action: &SystemAction,
        profile: &MergedProfile,
        modules: &[ResolvedModule],
        printer: &Printer,
        notes: &NoteSink,
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
                    // The caller settles this action's one `system:<name>.<key>`
                    // line and drains the sink under it, so the configurator's
                    // narration renders attached to the work it describes.
                    let cx = SystemContext::with_notes(printer, notes);
                    for sc in self.registry.available_system_configurators() {
                        if sc.name() == configurator {
                            sc.apply(desired_value, &cx)?;
                            return Ok(format!(
                                "system:{} ({} → {})",
                                super::system_resource_key(configurator, key),
                                current,
                                desired
                            ));
                        }
                    }
                }
                Ok(format!(
                    "system:{}",
                    super::system_resource_key(configurator, key)
                ))
            }
            SystemAction::Skip { configurator, .. } => {
                Ok(format!("system:{} (skipped)", configurator))
            }
        }
    }
}
