use std::fs;
use std::path::Path;
use std::process::Command;

use cfgd_core::errors::{CfgdError, Result};
use cfgd_core::output_v2::{Printer, Role};
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

// ---------------------------------------------------------------------------
// KernelModuleConfigurator
// ---------------------------------------------------------------------------

/// Manages kernel module loading.
///
/// Config format:
/// ```yaml
/// kernel-modules:
///   - br_netfilter
///   - overlay
///   - ip_vs
/// ```
pub struct KernelModuleConfigurator;

impl KernelModuleConfigurator {
    fn is_module_loaded(module: &str) -> bool {
        let proc_modules = Path::new("/proc/modules");
        if proc_modules.exists()
            && let Ok(content) = fs::read_to_string(proc_modules)
        {
            return content.lines().any(|line| {
                line.split_whitespace()
                    .next()
                    .is_some_and(|name| name == module)
            });
        }

        Command::new("lsmod")
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .any(|line| line.split_whitespace().next().is_some_and(|n| n == module))
            })
            .unwrap_or(false)
    }

    fn load_module(module: &str) -> Result<()> {
        let output = Command::new("modprobe")
            .arg(module)
            .output()
            .map_err(CfgdError::Io)?;

        if !output.status.success() {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "modprobe {} failed: {}",
                module,
                cfgd_core::stderr_lossy_trimmed(&output)
            ))));
        }
        Ok(())
    }

    fn persist_modules(desired_modules: &[&str]) -> Result<()> {
        Self::persist_modules_to(Path::new("/etc/modules-load.d"), desired_modules)
    }

    pub(super) fn persist_modules_to(conf_dir: &Path, desired_modules: &[&str]) -> Result<()> {
        if !conf_dir.exists() {
            fs::create_dir_all(conf_dir)?;
        }
        let conf_path = conf_dir.join("cfgd.conf");

        if desired_modules.is_empty() {
            let _ = fs::remove_file(&conf_path);
            return Ok(());
        }

        let mut content = String::from("# Managed by cfgd — do not edit manually\n");
        for m in desired_modules {
            content.push_str(m);
            content.push('\n');
        }

        cfgd_core::atomic_write_str(&conf_path, &content)?;
        Ok(())
    }
}

impl SystemConfigurator for KernelModuleConfigurator {
    fn name(&self) -> &str {
        "kernelModules"
    }

    fn is_available(&self) -> bool {
        Path::new("/proc/modules").exists()
            || Command::new("lsmod")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Sequence(Vec::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mut drifts = Vec::new();

        let modules = match desired.as_sequence() {
            Some(s) => s,
            None => return Ok(drifts),
        };

        for module_val in modules {
            let module = match module_val.as_str() {
                Some(m) => m,
                None => continue,
            };

            if !Self::is_module_loaded(module) {
                drifts.push(SystemDrift {
                    key: module.to_string(),
                    expected: "loaded".to_string(),
                    actual: "not loaded".to_string(),
                });
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let modules = match desired.as_sequence() {
            Some(s) => s,
            None => return Ok(()),
        };

        let mut desired_names: Vec<&str> = Vec::new();

        for module_val in modules {
            let module = match module_val.as_str() {
                Some(m) => m,
                None => continue,
            };

            desired_names.push(module);

            if Self::is_module_loaded(module) {
                continue;
            }

            printer.status_simple(Role::Info, format!("modprobe {}", module));
            Self::load_module(module)?;
        }

        if let Err(e) = Self::persist_modules(&desired_names) {
            printer.status_simple(
                Role::Warn,
                format!("Failed to persist modules: {} (runtime loaded)", e),
            );
        }

        Ok(())
    }
}
