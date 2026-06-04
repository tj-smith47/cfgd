use crate::config::MergedProfile;
use crate::errors::Result;
use crate::output::{Printer, Role};

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
