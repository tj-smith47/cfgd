use crate::config::MergedProfile;
use crate::errors::Result;
use crate::output_v2::{Printer, Role};

use super::types::SystemAction;

impl<'a> super::Reconciler<'a> {
    pub(super) fn apply_system_action(
        &self,
        action: &SystemAction,
        profile: &MergedProfile,
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
                if let Some(desired_value) = profile.system.get(configurator.as_str()) {
                    for sc in self.registry.available_system_configurators() {
                        if sc.name() == configurator {
                            let v1_forwarder =
                                crate::output::Printer::new(crate::output::Verbosity::Quiet);
                            sc.apply(desired_value, &v1_forwarder)?;
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
                ..
            } => {
                printer.status_simple(Role::Warn, format!("{}: {}", configurator, reason));
                Ok(format!("system:{} (skipped)", configurator))
            }
        }
    }
}
