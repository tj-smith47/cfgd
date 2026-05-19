use std::fs;
use std::path::Path;

use cfgd_core::errors::Result;
use cfgd_core::output_v2::{Printer, Role};
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

use super::format::json_equal;

// ---------------------------------------------------------------------------
// SeccompConfigurator
// ---------------------------------------------------------------------------

/// Manages seccomp profiles for container runtimes.
///
/// Config format:
/// ```yaml
/// seccomp:
///   profilesDir: /etc/cfgd/seccomp
///   profiles:
///     - name: default-audit
///       file: default-audit.json
///       content: |
///         { "defaultAction": "SCMP_ACT_LOG" }
/// ```
pub struct SeccompConfigurator;

impl SeccompConfigurator {
    const DEFAULT_PROFILES_DIR: &'static str = "/etc/cfgd/seccomp";
}

impl SystemConfigurator for SeccompConfigurator {
    fn name(&self) -> &str {
        "seccomp"
    }

    fn is_available(&self) -> bool {
        Path::new("/proc/sys/kernel/seccomp").exists()
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mut drifts = Vec::new();

        let profiles_dir = desired
            .get("profilesDir")
            .and_then(|v| v.as_str())
            .unwrap_or(Self::DEFAULT_PROFILES_DIR);
        let profiles_dir = Path::new(profiles_dir);

        let profiles = match desired.get("profiles").and_then(|v| v.as_sequence()) {
            Some(s) => s,
            None => return Ok(drifts),
        };

        for profile in profiles {
            let name = match profile.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let file = match profile.get("file").and_then(|v| v.as_str()) {
                Some(f) => f,
                None => continue,
            };

            let profile_path = profiles_dir.join(file);
            if cfgd_core::validate_no_traversal(std::path::Path::new(file)).is_err() {
                continue;
            }

            if !profile_path.exists() {
                drifts.push(SystemDrift {
                    key: format!("seccomp.{}", name),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
                continue;
            }

            if let Some(desired_content) = profile.get("content").and_then(|v| v.as_str())
                && let Ok(current_content) = fs::read_to_string(&profile_path)
                && !json_equal(desired_content, &current_content)
            {
                drifts.push(SystemDrift {
                    key: format!("seccomp.{}.content", name),
                    expected: "updated".to_string(),
                    actual: "outdated".to_string(),
                });
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let profiles_dir = desired
            .get("profilesDir")
            .and_then(|v| v.as_str())
            .unwrap_or(Self::DEFAULT_PROFILES_DIR);
        let profiles_dir = Path::new(profiles_dir);

        let profiles = match desired.get("profiles").and_then(|v| v.as_sequence()) {
            Some(s) => s,
            None => return Ok(()),
        };

        fs::create_dir_all(profiles_dir)?;

        for profile in profiles {
            let name = match profile.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let file = match profile.get("file").and_then(|v| v.as_str()) {
                Some(f) => f,
                None => continue,
            };

            let content = match profile.get("content").and_then(|v| v.as_str()) {
                Some(c) => c,
                None => continue,
            };

            let profile_path = profiles_dir.join(file);
            if cfgd_core::validate_no_traversal(std::path::Path::new(file)).is_err() {
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "Skipping seccomp profile {}: path traversal in file name",
                        name
                    ),
                );
                continue;
            }
            printer.status_simple(
                Role::Info,
                format!(
                    "Writing seccomp profile {}: {}",
                    name,
                    profile_path.display()
                ),
            );
            cfgd_core::atomic_write_str(&profile_path, content)?;
        }

        Ok(())
    }
}
