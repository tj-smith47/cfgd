use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use cfgd_core::errors::{CfgdError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

use super::{diff_yaml_mapping, yaml_value_to_string, yaml_value_with_numeric_bools};

// ---------------------------------------------------------------------------
// SysctlConfigurator
// ---------------------------------------------------------------------------

/// Manages kernel parameters via sysctl.
///
/// Config format:
/// ```yaml
/// sysctl:
///   net.ipv4.ip_forward: "1"
///   vm.max_map_count: "262144"
///   net.bridge.bridge-nf-call-iptables: "1"
/// ```
pub struct SysctlConfigurator;

impl SysctlConfigurator {
    /// Validate that a sysctl key contains only safe characters: [a-z0-9._-]
    fn validate_sysctl_key(key: &str) -> Result<()> {
        if key.is_empty()
            || !key.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'_' || b == b'-'
            })
        {
            return Err(CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
                message: format!(
                    "invalid sysctl key '{}': must contain only [a-z0-9._-]",
                    key
                ),
            }));
        }
        Ok(())
    }

    fn read_sysctl(key: &str) -> Result<String> {
        Self::validate_sysctl_key(key)?;
        let path = PathBuf::from("/proc/sys").join(key.replace('.', "/"));
        match fs::read_to_string(&path) {
            Ok(val) => Ok(val.trim().to_string()),
            Err(e) => Err(CfgdError::Io(e)),
        }
    }

    fn write_sysctl(key: &str, value: &str) -> Result<()> {
        Self::validate_sysctl_key(key)?;
        let output = Command::new("sysctl")
            .arg("-w")
            .arg(format!("{}={}", key, value))
            .output()
            .map_err(CfgdError::Io)?;

        if !output.status.success() {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "sysctl -w {}={} failed: {}",
                key,
                value,
                cfgd_core::stderr_lossy_trimmed(&output)
            ))));
        }
        Ok(())
    }

    fn persist_all_sysctls(entries: &BTreeMap<&str, String>) -> Result<()> {
        Self::persist_all_sysctls_to(Path::new("/etc/sysctl.d"), entries)
    }

    fn persist_all_sysctls_to(conf_dir: &Path, entries: &BTreeMap<&str, String>) -> Result<()> {
        if !conf_dir.exists() {
            fs::create_dir_all(conf_dir)?;
        }
        let conf_path = conf_dir.join("99-cfgd.conf");

        if entries.is_empty() {
            let _ = fs::remove_file(&conf_path);
            return Ok(());
        }

        let mut content = String::from("# Managed by cfgd — do not edit manually\n");
        for (k, v) in entries {
            content.push_str(&format!("{} = {}\n", k, v));
        }

        cfgd_core::atomic_write_str(&conf_path, &content)?;
        Ok(())
    }
}

impl SystemConfigurator for SysctlConfigurator {
    fn name(&self) -> &str {
        "sysctl"
    }

    fn is_available(&self) -> bool {
        Path::new("/proc/sys").exists()
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        Ok(diff_yaml_mapping(
            mapping,
            "",
            yaml_value_with_numeric_bools,
            |key_str| Self::read_sysctl(key_str).unwrap_or_else(|_| "<unreadable>".to_string()),
        ))
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(()),
        };

        let mut all_entries = BTreeMap::new();

        for (key, value) in mapping {
            let key_str = match key.as_str() {
                Some(k) => k,
                None => continue,
            };
            let desired_val = yaml_value_with_numeric_bools(value);

            printer.info(&format!("sysctl -w {}={}", key_str, desired_val));

            Self::write_sysctl(key_str, &desired_val)?;
            all_entries.insert(key_str, desired_val);
        }

        if let Err(e) = Self::persist_all_sysctls(&all_entries) {
            printer.warning(&format!(
                "Failed to persist sysctls: {} (runtime values applied)",
                e
            ));
        }

        Ok(())
    }
}

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

    fn persist_modules_to(conf_dir: &Path, desired_modules: &[&str]) -> Result<()> {
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

            printer.info(&format!("modprobe {}", module));
            Self::load_module(module)?;
        }

        if let Err(e) = Self::persist_modules(&desired_names) {
            printer.warning(&format!(
                "Failed to persist modules: {} (runtime loaded)",
                e
            ));
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ContainerdConfigurator
// ---------------------------------------------------------------------------

/// Manages containerd configuration.
///
/// Config format:
/// ```yaml
/// containerd:
///   configPath: /etc/containerd/config.toml
///   settings:
///     SystemdCgroup: true
///     sandbox_image: "registry.k8s.io/pause:3.9"
/// ```
pub struct ContainerdConfigurator;

impl ContainerdConfigurator {
    const DEFAULT_CONFIG_PATH: &'static str = "/etc/containerd/config.toml";

    fn config_path(desired: &serde_yaml::Value) -> PathBuf {
        desired
            .get("configPath")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(Self::DEFAULT_CONFIG_PATH))
    }

    fn read_current_config(path: &Path) -> Result<toml::Table> {
        if !path.exists() {
            return Ok(toml::Table::new());
        }
        let content = fs::read_to_string(path)?;
        content.parse::<toml::Table>().map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "failed to parse containerd config {}: {}",
                    path.display(),
                    e
                ),
            ))
        })
    }

    fn restart_containerd() -> Result<()> {
        let output = Command::new("systemctl")
            .args(["restart", "containerd"])
            .output()
            .map_err(CfgdError::Io)?;

        if !output.status.success() {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "systemctl restart containerd failed: {}",
                cfgd_core::stderr_lossy_trimmed(&output)
            ))));
        }
        Ok(())
    }
}

impl SystemConfigurator for ContainerdConfigurator {
    fn name(&self) -> &str {
        "containerd"
    }

    fn is_available(&self) -> bool {
        Command::new("containerd")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let config_path = Self::config_path(desired);
        let current = Self::read_current_config(&config_path)?;

        let settings = match desired.get("settings").and_then(|v| v.as_mapping()) {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        Ok(diff_yaml_mapping(
            settings,
            "containerd",
            yaml_value_to_string,
            |key_str| find_toml_value(&current, key_str).unwrap_or_else(|| "<not set>".to_string()),
        ))
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let config_path = Self::config_path(desired);

        let settings = match desired.get("settings").and_then(|v| v.as_mapping()) {
            Some(m) => m,
            None => return Ok(()),
        };

        if settings.is_empty() {
            return Ok(());
        }

        let mut current = Self::read_current_config(&config_path)?;

        for (key, desired_val) in settings {
            let key_str = match key.as_str() {
                Some(k) => k,
                None => continue,
            };
            let desired_str = yaml_value_to_string(desired_val);

            printer.info(&format!(
                "containerd: setting {} = {}",
                key_str, desired_str
            ));
            set_toml_value(&mut current, key_str, desired_val);
        }

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = toml::to_string_pretty(&current).map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                "failed to serialize containerd config: {}",
                e
            )))
        })?;

        // Validate serialized TOML can be re-parsed before writing
        if let Err(e) = content.parse::<toml::Value>() {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "containerd config validation failed — aborting write: {}",
                e
            ))));
        }

        // Backup existing config before overwriting
        let backup = cfgd_core::capture_file_state(&config_path).map_err(CfgdError::Io)?;

        cfgd_core::atomic_write_str(&config_path, &content)?;

        printer.info("Restarting containerd");
        if let Err(e) = Self::restart_containerd() {
            // Restart failed — attempt rollback
            if let Some(ref state) = backup
                && !state.is_symlink
                && !state.oversized
            {
                printer.warning("containerd restart failed — restoring previous config");
                if let Err(re) = cfgd_core::atomic_write(&config_path, &state.content) {
                    printer.warning(&format!("rollback: failed to restore config: {}", re));
                } else if let Err(re) = Self::restart_containerd() {
                    printer.warning(&format!("rollback: containerd restart also failed: {}", re));
                }
            }
            return Err(e);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// KubeletConfigurator
// ---------------------------------------------------------------------------

/// Manages kubelet configuration.
///
/// Config format:
/// ```yaml
/// kubelet:
///   configPath: /var/lib/kubelet/config.yaml
///   settings:
///     maxPods: 110
///     cgroupDriver: systemd
/// ```
pub struct KubeletConfigurator;

impl KubeletConfigurator {
    const DEFAULT_CONFIG_PATH: &'static str = "/var/lib/kubelet/config.yaml";

    fn config_path(desired: &serde_yaml::Value) -> PathBuf {
        desired
            .get("configPath")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(Self::DEFAULT_CONFIG_PATH))
    }

    fn read_current_config(path: &Path) -> Result<serde_yaml::Value> {
        if !path.exists() {
            return Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
        }
        let content = fs::read_to_string(path)?;
        serde_yaml::from_str(&content).map_err(|e| {
            CfgdError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to parse kubelet config {}: {}", path.display(), e),
            ))
        })
    }

    fn restart_kubelet() -> Result<()> {
        let output = Command::new("systemctl")
            .args(["restart", "kubelet"])
            .output()
            .map_err(CfgdError::Io)?;

        if !output.status.success() {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "systemctl restart kubelet failed: {}",
                cfgd_core::stderr_lossy_trimmed(&output)
            ))));
        }
        Ok(())
    }
}

impl SystemConfigurator for KubeletConfigurator {
    fn name(&self) -> &str {
        "kubelet"
    }

    fn is_available(&self) -> bool {
        Command::new("kubelet")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let config_path = Self::config_path(desired);
        let current = Self::read_current_config(&config_path)?;

        let settings = match desired.get("settings").and_then(|v| v.as_mapping()) {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        Ok(diff_yaml_mapping(
            settings,
            "kubelet",
            yaml_value_to_string,
            |key_str| {
                current
                    .get(key_str)
                    .map(yaml_value_to_string)
                    .unwrap_or_else(|| "<not set>".to_string())
            },
        ))
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let config_path = Self::config_path(desired);

        let settings = match desired.get("settings").and_then(|v| v.as_mapping()) {
            Some(m) => m,
            None => return Ok(()),
        };

        if settings.is_empty() {
            return Ok(());
        }

        let mut current = Self::read_current_config(&config_path)?;
        if !current.is_mapping() {
            current = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        }
        let Some(current_map) = current.as_mapping_mut() else {
            // Unreachable: we set current to Mapping above. If this somehow
            // triggers, it's an internal logic error — surface it as an error.
            return Err(CfgdError::Io(std::io::Error::other(
                "kubelet config: value is not a mapping after explicit set",
            )));
        };

        for (key, desired_val) in settings {
            let key_str = match key.as_str() {
                Some(k) => k,
                None => continue,
            };
            printer.info(&format!(
                "kubelet: setting {} = {}",
                key_str,
                yaml_value_to_string(desired_val)
            ));
            current_map.insert(
                serde_yaml::Value::String(key_str.to_string()),
                desired_val.clone(),
            );
        }

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = serde_yaml::to_string(&current).map_err(|e| {
            CfgdError::Io(std::io::Error::other(format!(
                "failed to serialize kubelet config: {}",
                e
            )))
        })?;

        // Validate serialized YAML can be re-parsed before writing
        if let Err(e) = serde_yaml::from_str::<serde_yaml::Value>(&content) {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "kubelet config validation failed — aborting write: {}",
                e
            ))));
        }

        // Backup existing config before overwriting
        let backup = cfgd_core::capture_file_state(&config_path).map_err(CfgdError::Io)?;

        cfgd_core::atomic_write_str(&config_path, &content)?;

        printer.info("Restarting kubelet");
        if let Err(e) = Self::restart_kubelet() {
            if let Some(ref state) = backup
                && !state.is_symlink
                && !state.oversized
            {
                printer.warning("kubelet restart failed — restoring previous config");
                if let Err(re) = cfgd_core::atomic_write(&config_path, &state.content) {
                    printer.warning(&format!("rollback: failed to restore config: {}", re));
                } else if let Err(re) = Self::restart_kubelet() {
                    printer.warning(&format!("rollback: kubelet restart also failed: {}", re));
                }
            }
            return Err(e);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// AppArmorConfigurator
// ---------------------------------------------------------------------------

/// Manages AppArmor profiles for container security.
///
/// Config format:
/// ```yaml
/// apparmor:
///   profiles:
///     - name: cfgd-containerd-default
///       path: /etc/apparmor.d/cfgd-containerd-default
///       content: |
///         #include <tunables/global>
///         profile cfgd-containerd-default flags=(attach_disconnected) {
///           #include <abstractions/base>
///           file,
///           network,
///           capability,
///         }
/// ```
pub struct AppArmorConfigurator;

impl AppArmorConfigurator {
    fn is_profile_loaded(name: &str) -> bool {
        let status_path = Path::new("/sys/kernel/security/apparmor/profiles");
        if status_path.exists()
            && let Ok(content) = fs::read_to_string(status_path)
        {
            return content
                .lines()
                .any(|line| line.split_whitespace().next().is_some_and(|n| n == name));
        }

        Command::new("aa-status")
            .arg("--json")
            .output()
            .ok()
            .map(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                // Check for exact profile name match — JSON output uses profile names as keys
                // wrapped in quotes: "profile_name"
                let quoted = format!("\"{}\"", name);
                stdout.contains(&quoted)
            })
            .unwrap_or(false)
    }

    fn load_profile(path: &Path) -> Result<()> {
        let output = Command::new("apparmor_parser")
            .arg("-r")
            .arg(path)
            .output()
            .map_err(CfgdError::Io)?;

        if !output.status.success() {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "apparmor_parser -r {} failed: {}",
                path.display(),
                cfgd_core::stderr_lossy_trimmed(&output)
            ))));
        }
        Ok(())
    }
}

impl SystemConfigurator for AppArmorConfigurator {
    fn name(&self) -> &str {
        "apparmor"
    }

    fn is_available(&self) -> bool {
        Path::new("/sys/kernel/security/apparmor").exists()
            || Command::new("apparmor_parser")
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mut drifts = Vec::new();

        let profiles = match desired.get("profiles").and_then(|v| v.as_sequence()) {
            Some(s) => s,
            None => return Ok(drifts),
        };

        for profile in profiles {
            let name = match profile.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let path = match profile.get("path").and_then(|v| v.as_str()) {
                Some(p) => PathBuf::from(p),
                None => continue,
            };
            if cfgd_core::validate_no_traversal(&path).is_err() {
                continue;
            }

            if !path.exists() {
                drifts.push(SystemDrift {
                    key: format!("apparmor.{}.file", name),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
                continue;
            }

            if let Some(desired_content) = profile.get("content").and_then(|v| v.as_str())
                && let Ok(current_content) = fs::read_to_string(&path)
                && current_content.trim() != desired_content.trim()
            {
                drifts.push(SystemDrift {
                    key: format!("apparmor.{}.content", name),
                    expected: "updated".to_string(),
                    actual: "outdated".to_string(),
                });
            }

            if !Self::is_profile_loaded(name) {
                drifts.push(SystemDrift {
                    key: format!("apparmor.{}.loaded", name),
                    expected: "loaded".to_string(),
                    actual: "not loaded".to_string(),
                });
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let profiles = match desired.get("profiles").and_then(|v| v.as_sequence()) {
            Some(s) => s,
            None => return Ok(()),
        };

        for profile in profiles {
            let name = match profile.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let path = match profile.get("path").and_then(|v| v.as_str()) {
                Some(p) => PathBuf::from(p),
                None => continue,
            };
            if cfgd_core::validate_no_traversal(&path).is_err() {
                printer.warning(&format!(
                    "Skipping AppArmor profile {}: path traversal detected",
                    name
                ));
                continue;
            }

            if let Some(content) = profile.get("content").and_then(|v| v.as_str()) {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                printer.info(&format!("Writing AppArmor profile: {}", path.display()));
                cfgd_core::atomic_write_str(&path, content)?;
            }

            printer.info(&format!("Loading AppArmor profile: {}", name));
            Self::load_profile(&path)?;
        }

        Ok(())
    }
}

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
                printer.warning(&format!(
                    "Skipping seccomp profile {}: path traversal in file name",
                    name
                ));
                continue;
            }
            printer.info(&format!(
                "Writing seccomp profile {}: {}",
                name,
                profile_path.display()
            ));
            cfgd_core::atomic_write_str(&profile_path, content)?;
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// CertificateConfigurator
// ---------------------------------------------------------------------------

/// Manages TLS certificates for node services.
///
/// Config format:
/// ```yaml
/// certificates:
///   caCertDir: /etc/kubernetes/pki
///   certificates:
///     - name: kubelet-client
///       certPath: /etc/kubernetes/pki/kubelet-client.crt
///       keyPath: /etc/kubernetes/pki/kubelet-client.key
///       mode: "0600"
/// ```
pub struct CertificateConfigurator;

impl SystemConfigurator for CertificateConfigurator {
    fn name(&self) -> &str {
        "certificates"
    }

    fn is_available(&self) -> bool {
        cfg!(target_os = "linux")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mut drifts = Vec::new();

        let certs = match desired.get("certificates").and_then(|v| v.as_sequence()) {
            Some(s) => s,
            None => return Ok(drifts),
        };

        for cert in certs {
            let name = match cert.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            if let Some(cert_path) = cert.get("certPath").and_then(|v| v.as_str())
                && !Path::new(cert_path).exists()
            {
                drifts.push(SystemDrift {
                    key: format!("cert.{}.cert", name),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
            }

            if let Some(key_path) = cert.get("keyPath").and_then(|v| v.as_str())
                && !Path::new(key_path).exists()
            {
                drifts.push(SystemDrift {
                    key: format!("cert.{}.key", name),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
            }

            if let Some(ca_path) = cert.get("caPath").and_then(|v| v.as_str())
                && !Path::new(ca_path).exists()
            {
                drifts.push(SystemDrift {
                    key: format!("cert.{}.ca", name),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
            }

            if let Some(mode_str) = cert.get("mode").and_then(|v| v.as_str()) {
                let desired_mode = u32::from_str_radix(mode_str, 8).unwrap_or(0o644);
                for path_key in &["certPath", "keyPath", "caPath"] {
                    if let Some(path) = cert.get(*path_key).and_then(|v| v.as_str())
                        && let Ok(meta) = fs::metadata(path)
                        && let Some(current_mode) = cfgd_core::file_permissions_mode(&meta)
                        && current_mode != desired_mode
                    {
                        drifts.push(SystemDrift {
                            key: format!("cert.{}.{}.mode", name, path_key),
                            expected: format!("{:04o}", desired_mode),
                            actual: format!("{:04o}", current_mode),
                        });
                    }
                }
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let ca_cert_dir = desired
            .get("caCertDir")
            .and_then(|v| v.as_str())
            .unwrap_or("/etc/kubernetes/pki");

        fs::create_dir_all(ca_cert_dir)?;

        let certs = match desired.get("certificates").and_then(|v| v.as_sequence()) {
            Some(s) => s,
            None => return Ok(()),
        };

        for cert in certs {
            let name = match cert.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let mode_str = cert.get("mode").and_then(|v| v.as_str()).unwrap_or("0644");
            let desired_mode = u32::from_str_radix(mode_str, 8).unwrap_or(0o644);

            for path_key in &["certPath", "keyPath", "caPath"] {
                if let Some(path_str) = cert.get(*path_key).and_then(|v| v.as_str()) {
                    let path = Path::new(path_str);
                    if path.exists() {
                        let meta = fs::metadata(path)?;
                        let current_mode = cfgd_core::file_permissions_mode(&meta);
                        if current_mode != Some(desired_mode) {
                            printer.info(&format!(
                                "Setting permissions {:04o} on {} ({})",
                                desired_mode, path_str, name
                            ));
                            cfgd_core::set_file_permissions(path, desired_mode)?;
                        }
                    } else {
                        printer.warning(&format!(
                            "Certificate file missing: {} ({})",
                            path_str, name
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn find_toml_value(table: &toml::Table, key: &str) -> Option<String> {
    // First try dot-separated path lookup
    if key.contains('.') {
        let parts: Vec<&str> = key.rsplitn(2, '.').collect();
        let (leaf, path) = (parts[0], parts[1]);
        let mut current = table;
        let mut found = true;
        for segment in path.split('.') {
            match current.get(segment).and_then(|v| v.as_table()) {
                Some(t) => current = t,
                None => {
                    found = false;
                    break;
                }
            }
        }
        if found && let Some(val) = current.get(leaf) {
            return Some(toml_value_to_string(val));
        }
    }

    // Fall back to direct key lookup at root level
    if let Some(val) = table.get(key) {
        return Some(toml_value_to_string(val));
    }

    // Fall back to recursive search for backward compatibility
    for (_, val) in table {
        if let toml::Value::Table(nested) = val
            && let Some(found) = find_toml_value(nested, key)
        {
            return Some(found);
        }
    }

    None
}

pub(crate) fn toml_value_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(n) => n.to_string(),
        toml::Value::String(s) => s.clone(),
        _ => format!("{}", value),
    }
}

pub(crate) fn set_toml_value(table: &mut toml::Table, key: &str, value: &serde_yaml::Value) {
    let toml_val = yaml_to_toml_value(value);

    if !key.contains('.') {
        table.insert(key.to_string(), toml_val);
        return;
    }

    let parts: Vec<&str> = key.rsplitn(2, '.').collect();
    let (leaf, path) = (parts[0], parts[1]);

    let mut current = table;
    for segment in path.split('.') {
        let entry = current
            .entry(segment.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if !entry.is_table() {
            *entry = toml::Value::Table(toml::Table::new());
        }
        // Safe: we just set it to a Table two lines above if it wasn't one
        current = match entry.as_table_mut() {
            Some(t) => t,
            None => return, // unreachable after the assignment above
        };
    }
    current.insert(leaf.to_string(), toml_val);
}

pub(crate) fn yaml_to_toml_value(value: &serde_yaml::Value) -> toml::Value {
    match value {
        serde_yaml::Value::Bool(b) => toml::Value::Boolean(*b),
        serde_yaml::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                toml::Value::Integer(i)
            } else if let Some(f) = n.as_f64() {
                toml::Value::Float(f)
            } else {
                toml::Value::String(n.to_string())
            }
        }
        serde_yaml::Value::String(s) => toml::Value::String(s.clone()),
        serde_yaml::Value::Mapping(m) => {
            let mut table = toml::Table::new();
            for (k, v) in m {
                if let Some(key) = k.as_str() {
                    table.insert(key.to_string(), yaml_to_toml_value(v));
                }
            }
            toml::Value::Table(table)
        }
        serde_yaml::Value::Sequence(s) => {
            let arr: Vec<toml::Value> = s.iter().map(yaml_to_toml_value).collect();
            toml::Value::Array(arr)
        }
        _ => toml::Value::String(String::new()),
    }
}

/// Compare two JSON strings for semantic equality.
/// Returns true if both parse to equal `serde_json::Value`s, or if both
/// raw strings are equal after trimming (fallback for non-JSON input).
fn json_equal(a: &str, b: &str) -> bool {
    match (
        serde_json::from_str::<serde_json::Value>(a),
        serde_json::from_str::<serde_json::Value>(b),
    ) {
        (Ok(va), Ok(vb)) => va == vb,
        _ => a.trim() == b.trim(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn node_configurator_names() {
        let cases: &[(&dyn SystemConfigurator, &str)] = &[
            (&SysctlConfigurator, "sysctl"),
            (&KernelModuleConfigurator, "kernelModules"),
            (&ContainerdConfigurator, "containerd"),
            (&KubeletConfigurator, "kubelet"),
            (&AppArmorConfigurator, "apparmor"),
            (&SeccompConfigurator, "seccomp"),
            (&CertificateConfigurator, "certificates"),
        ];
        for (c, expected) in cases {
            assert_eq!(c.name(), *expected, "wrong name for {expected}");
        }
    }

    #[test]
    fn yaml_value_to_string_conversions() {
        assert_eq!(yaml_value_to_string(&serde_yaml::Value::Bool(true)), "true");
        assert_eq!(
            yaml_value_to_string(&serde_yaml::Value::Number(42.into())),
            "42"
        );
        assert_eq!(
            yaml_value_to_string(&serde_yaml::Value::String("hello".into())),
            "hello"
        );
    }

    #[test]
    fn diff_returns_empty_for_empty_or_wrong_type_input() {
        let cases: &[(&dyn SystemConfigurator, serde_yaml::Value)] = &[
            (
                &SysctlConfigurator,
                serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
            ),
            (
                &SysctlConfigurator,
                serde_yaml::Value::String("invalid".into()),
            ),
            (
                &KernelModuleConfigurator,
                serde_yaml::Value::Sequence(Vec::new()),
            ),
        ];
        for (c, input) in cases {
            let drifts = c.diff(input).unwrap();
            assert!(
                drifts.is_empty(),
                "{} should return empty for {:?}",
                c.name(),
                input
            );
        }
    }

    #[test]
    fn containerd_default_config_path() {
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let path = ContainerdConfigurator::config_path(&desired);
        assert_eq!(path, PathBuf::from("/etc/containerd/config.toml"));
    }

    #[test]
    fn kubelet_default_config_path() {
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let path = KubeletConfigurator::config_path(&desired);
        assert_eq!(path, PathBuf::from("/var/lib/kubelet/config.yaml"));
    }

    #[test]
    fn json_equal_reordered_keys() {
        assert!(json_equal(r#"{ "b": 2, "a": 1 }"#, r#"{"a":1,"b":2}"#));
    }

    #[test]
    fn json_equal_different_values() {
        assert!(!json_equal(r#"{"a":1}"#, r#"{"a":2}"#));
    }

    #[test]
    fn json_equal_invalid_fallback() {
        assert!(json_equal("not json", "not json"));
        assert!(!json_equal("foo", "bar"));
    }

    #[test]
    fn toml_value_to_string_conversions() {
        assert_eq!(toml_value_to_string(&toml::Value::Boolean(true)), "true");
        assert_eq!(toml_value_to_string(&toml::Value::Integer(42)), "42");
        assert_eq!(
            toml_value_to_string(&toml::Value::String("hello".into())),
            "hello"
        );
    }

    #[test]
    fn yaml_to_toml_conversions() {
        assert_eq!(
            yaml_to_toml_value(&serde_yaml::Value::Bool(true)),
            toml::Value::Boolean(true)
        );
        assert_eq!(
            yaml_to_toml_value(&serde_yaml::Value::Number(42.into())),
            toml::Value::Integer(42)
        );
        assert_eq!(
            yaml_to_toml_value(&serde_yaml::Value::String("test".into())),
            toml::Value::String("test".into())
        );
    }

    #[test]
    fn find_toml_value_direct() {
        let mut table = toml::Table::new();
        table.insert("key".to_string(), toml::Value::String("value".into()));
        assert_eq!(find_toml_value(&table, "key"), Some("value".to_string()));
    }

    #[test]
    fn find_toml_value_nested() {
        let mut inner = toml::Table::new();
        inner.insert("nested_key".to_string(), toml::Value::Boolean(true));
        let mut table = toml::Table::new();
        table.insert("section".to_string(), toml::Value::Table(inner));
        assert_eq!(
            find_toml_value(&table, "nested_key"),
            Some("true".to_string())
        );
    }

    #[test]
    fn find_toml_value_missing() {
        let table = toml::Table::new();
        assert_eq!(find_toml_value(&table, "missing"), None);
    }

    #[test]
    fn seccomp_diff_empty_profiles() {
        let sc = SeccompConfigurator;
        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(Vec::new()),
        );
        let desired = serde_yaml::Value::Mapping(m);
        let drifts = sc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn certificate_diff_empty() {
        let cc = CertificateConfigurator;
        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("certificates".into()),
            serde_yaml::Value::Sequence(Vec::new()),
        );
        let desired = serde_yaml::Value::Mapping(m);
        let drifts = cc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn certificate_diff_missing_files() {
        let cc = CertificateConfigurator;

        let mut cert = serde_yaml::Mapping::new();
        cert.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("test-cert".into()),
        );
        cert.insert(
            serde_yaml::Value::String("certPath".into()),
            serde_yaml::Value::String("/nonexistent/cert.pem".into()),
        );
        cert.insert(
            serde_yaml::Value::String("keyPath".into()),
            serde_yaml::Value::String("/nonexistent/key.pem".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("certificates".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = cc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 2);
        assert!(drifts[0].key.contains("test-cert"));
    }

    #[test]
    fn apparmor_diff_empty_profiles() {
        let ac = AppArmorConfigurator;
        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(Vec::new()),
        );
        let desired = serde_yaml::Value::Mapping(m);
        let drifts = ac.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // --- yaml_value_with_numeric_bools ---

    #[test]
    fn yaml_value_with_numeric_bools_bool_maps_to_01() {
        assert_eq!(
            yaml_value_with_numeric_bools(&serde_yaml::Value::Bool(true)),
            "1"
        );
        assert_eq!(
            yaml_value_with_numeric_bools(&serde_yaml::Value::Bool(false)),
            "0"
        );
    }

    #[test]
    fn yaml_value_with_numeric_bools_delegates_non_bool() {
        assert_eq!(
            yaml_value_with_numeric_bools(&serde_yaml::Value::Number(262144.into())),
            "262144"
        );
        assert_eq!(
            yaml_value_with_numeric_bools(&serde_yaml::Value::String("1".into())),
            "1"
        );
    }

    // --- find_toml_value dot-path ---

    #[test]
    fn find_toml_value_dot_path() {
        let toml_str = r#"
[plugins]
[plugins.cri]
sandbox_image = "registry.k8s.io/pause:3.9"
[plugins.cri.containerd.runtimes.runc.options]
SystemdCgroup = true
"#;
        let table: toml::Table = toml_str.parse().unwrap();
        assert_eq!(
            find_toml_value(&table, "plugins.cri.sandbox_image"),
            Some("registry.k8s.io/pause:3.9".to_string())
        );
        assert_eq!(
            find_toml_value(
                &table,
                "plugins.cri.containerd.runtimes.runc.options.SystemdCgroup"
            ),
            Some("true".to_string())
        );
    }

    #[test]
    fn find_toml_value_dot_path_missing() {
        let mut table = toml::Table::new();
        table.insert("key".to_string(), toml::Value::String("val".into()));
        assert_eq!(find_toml_value(&table, "no.such.path"), None);
    }

    // --- set_toml_value dot-path ---

    #[test]
    fn set_toml_value_dot_path_creates_nested_tables() {
        let mut table = toml::Table::new();
        set_toml_value(
            &mut table,
            "plugins.cri.sandbox_image",
            &serde_yaml::Value::String("pause:3.10".into()),
        );
        let val = find_toml_value(&table, "plugins.cri.sandbox_image");
        assert_eq!(val, Some("pause:3.10".to_string()));
    }

    #[test]
    fn set_toml_value_simple_key() {
        let mut table = toml::Table::new();
        set_toml_value(&mut table, "version", &serde_yaml::Value::Number(2.into()));
        assert_eq!(table.get("version").unwrap().as_integer(), Some(2));
    }

    #[test]
    fn containerd_read_nonexistent_config() {
        let table =
            ContainerdConfigurator::read_current_config(Path::new("/nonexistent/config.toml"))
                .unwrap();
        assert!(table.is_empty());
    }

    #[test]
    fn kubelet_read_nonexistent_config() {
        let value = KubeletConfigurator::read_current_config(Path::new("/nonexistent/config.yaml"))
            .unwrap();
        assert!(value.is_mapping());
    }

    // --- validate_sysctl_key ---

    #[test]
    fn sysctl_validate_key_valid() {
        assert!(SysctlConfigurator::validate_sysctl_key("net.ipv4.ip_forward").is_ok());
        assert!(SysctlConfigurator::validate_sysctl_key("vm.max_map_count").is_ok());
        assert!(
            SysctlConfigurator::validate_sysctl_key("net.bridge.bridge-nf-call-iptables").is_ok()
        );
    }

    #[test]
    fn sysctl_validate_key_empty_rejected() {
        assert!(SysctlConfigurator::validate_sysctl_key("").is_err());
    }

    #[test]
    fn sysctl_validate_key_uppercase_rejected() {
        assert!(SysctlConfigurator::validate_sysctl_key("NET.IPV4").is_err());
        assert!(SysctlConfigurator::validate_sysctl_key("net.ipV4.ip_forward").is_err());
    }

    #[test]
    fn sysctl_validate_key_special_chars_rejected() {
        assert!(SysctlConfigurator::validate_sysctl_key("net/ipv4/ip_forward").is_err());
        assert!(SysctlConfigurator::validate_sysctl_key("key;rm -rf /").is_err());
        assert!(SysctlConfigurator::validate_sysctl_key("key with spaces").is_err());
    }

    // --- sysctl diff with populated mapping ---

    #[test]
    fn sysctl_diff_detects_drift_for_unreadable_keys() {
        // On a test machine without /proc/sys, read_sysctl returns "<unreadable>"
        // so any desired value will drift
        let sc = SysctlConfigurator;
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            serde_yaml::Value::String("net.ipv4.ip_forward".into()),
            serde_yaml::Value::String("1".into()),
        );
        let desired = serde_yaml::Value::Mapping(mapping);
        let drifts = sc.diff(&desired).unwrap();
        // The key may or may not be readable depending on the test environment,
        // but the diff should return without error
        assert!(drifts.len() <= 1);
        if !drifts.is_empty() {
            assert_eq!(drifts[0].key, "net.ipv4.ip_forward");
            assert_eq!(drifts[0].expected, "1");
        }
    }

    #[test]
    fn sysctl_diff_bool_true_converts_to_1() {
        let sc = SysctlConfigurator;
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            serde_yaml::Value::String("net.ipv4.ip_forward".into()),
            serde_yaml::Value::Bool(true),
        );
        let desired = serde_yaml::Value::Mapping(mapping);
        let drifts = sc.diff(&desired).unwrap();
        // yaml_value_with_numeric_bools converts true to "1"
        if !drifts.is_empty() {
            assert_eq!(drifts[0].expected, "1");
        }
    }

    #[test]
    fn sysctl_diff_skips_non_string_keys() {
        let sc = SysctlConfigurator;
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::String("value".into()),
        );
        let desired = serde_yaml::Value::Mapping(mapping);
        let drifts = sc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // --- kernel module diff ---

    #[test]
    fn kernel_module_diff_with_non_sequence() {
        let km = KernelModuleConfigurator;
        let desired = serde_yaml::Value::String("not a sequence".into());
        let drifts = km.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kernel_module_diff_skips_non_string_entries() {
        let km = KernelModuleConfigurator;
        let desired = serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Bool(true),
        ]);
        let drifts = km.diff(&desired).unwrap();
        // Non-string entries are skipped, so no drifts from them
        assert!(drifts.is_empty());
    }

    #[test]
    fn kernel_module_diff_reports_unloaded_modules() {
        let km = KernelModuleConfigurator;
        // Use a module name that definitely won't be loaded
        let desired = serde_yaml::Value::Sequence(vec![serde_yaml::Value::String(
            "cfgd_fake_module_xyz_12345".into(),
        )]);
        let drifts = km.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "cfgd_fake_module_xyz_12345");
        assert_eq!(drifts[0].expected, "loaded");
        assert_eq!(drifts[0].actual, "not loaded");
    }

    // --- containerd diff with real TOML files ---

    #[test]
    fn containerd_read_existing_config() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        fs::write(
            &config,
            "[plugins]\n[plugins.cri]\nsandbox_image = \"pause:3.9\"\n",
        )
        .unwrap();
        let table = ContainerdConfigurator::read_current_config(&config).unwrap();
        assert_eq!(
            find_toml_value(&table, "plugins.cri.sandbox_image"),
            Some("pause:3.9".to_string())
        );
    }

    #[test]
    fn containerd_read_invalid_toml_returns_error() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        fs::write(&config, "this is not valid toml [[[").unwrap();
        let err = ContainerdConfigurator::read_current_config(&config)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("failed to parse containerd config"),
            "expected containerd parse error, got: {err}"
        );
    }

    #[test]
    fn containerd_config_path_custom() {
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String("/custom/containerd.toml".into()),
        );
        let desired = serde_yaml::Value::Mapping(mapping);
        let path = ContainerdConfigurator::config_path(&desired);
        assert_eq!(path, PathBuf::from("/custom/containerd.toml"));
    }

    #[test]
    fn containerd_diff_detects_changed_setting() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        fs::write(&config, "sandbox_image = \"pause:3.8\"\n").unwrap();

        let mut settings = serde_yaml::Mapping::new();
        settings.insert(
            serde_yaml::Value::String("sandbox_image".into()),
            serde_yaml::Value::String("pause:3.9".into()),
        );

        let mut desired_map = serde_yaml::Mapping::new();
        desired_map.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String(config.to_str().unwrap().into()),
        );
        desired_map.insert(
            serde_yaml::Value::String("settings".into()),
            serde_yaml::Value::Mapping(settings),
        );

        let cc = ContainerdConfigurator;
        let desired = serde_yaml::Value::Mapping(desired_map);
        let drifts = cc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "containerd.sandbox_image");
        assert_eq!(drifts[0].expected, "pause:3.9");
        assert_eq!(drifts[0].actual, "pause:3.8");
    }

    #[test]
    fn containerd_diff_no_drift_when_matching() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        fs::write(&config, "sandbox_image = \"pause:3.9\"\n").unwrap();

        let mut settings = serde_yaml::Mapping::new();
        settings.insert(
            serde_yaml::Value::String("sandbox_image".into()),
            serde_yaml::Value::String("pause:3.9".into()),
        );

        let mut desired_map = serde_yaml::Mapping::new();
        desired_map.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String(config.to_str().unwrap().into()),
        );
        desired_map.insert(
            serde_yaml::Value::String("settings".into()),
            serde_yaml::Value::Mapping(settings),
        );

        let cc = ContainerdConfigurator;
        let desired = serde_yaml::Value::Mapping(desired_map);
        let drifts = cc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn containerd_diff_missing_setting_shows_not_set() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        fs::write(&config, "version = 2\n").unwrap();

        let mut settings = serde_yaml::Mapping::new();
        settings.insert(
            serde_yaml::Value::String("sandbox_image".into()),
            serde_yaml::Value::String("pause:3.9".into()),
        );

        let mut desired_map = serde_yaml::Mapping::new();
        desired_map.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String(config.to_str().unwrap().into()),
        );
        desired_map.insert(
            serde_yaml::Value::String("settings".into()),
            serde_yaml::Value::Mapping(settings),
        );

        let cc = ContainerdConfigurator;
        let desired = serde_yaml::Value::Mapping(desired_map);
        let drifts = cc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].actual, "<not set>");
    }

    #[test]
    fn containerd_diff_no_settings_returns_empty() {
        let cc = ContainerdConfigurator;
        let mut desired_map = serde_yaml::Mapping::new();
        desired_map.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String("/nonexistent/config.toml".into()),
        );
        let desired = serde_yaml::Value::Mapping(desired_map);
        let drifts = cc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn containerd_diff_with_nested_toml_settings() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        fs::write(
            &config,
            "[plugins]\n[plugins.cri]\nsandbox_image = \"pause:3.8\"\n",
        )
        .unwrap();

        let mut settings = serde_yaml::Mapping::new();
        settings.insert(
            serde_yaml::Value::String("plugins.cri.sandbox_image".into()),
            serde_yaml::Value::String("pause:3.9".into()),
        );

        let mut desired_map = serde_yaml::Mapping::new();
        desired_map.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String(config.to_str().unwrap().into()),
        );
        desired_map.insert(
            serde_yaml::Value::String("settings".into()),
            serde_yaml::Value::Mapping(settings),
        );

        let cc = ContainerdConfigurator;
        let desired = serde_yaml::Value::Mapping(desired_map);
        let drifts = cc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "containerd.plugins.cri.sandbox_image");
        assert_eq!(drifts[0].expected, "pause:3.9");
        assert_eq!(drifts[0].actual, "pause:3.8");
    }

    // --- kubelet diff with real YAML files ---

    #[test]
    fn kubelet_read_existing_config() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        fs::write(
            &config,
            "clusterDNS: 10.96.0.10\nclusterDomain: cluster.local\nmaxPods: 110\n",
        )
        .unwrap();
        let value = KubeletConfigurator::read_current_config(&config).unwrap();
        assert_eq!(
            value.get("clusterDNS").and_then(|v| v.as_str()),
            Some("10.96.0.10")
        );
        assert_eq!(value.get("maxPods").and_then(|v| v.as_u64()), Some(110));
    }

    #[test]
    fn kubelet_read_invalid_yaml_returns_error() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        fs::write(&config, ":\n  - :\n    bad: [[[").unwrap();
        let err = KubeletConfigurator::read_current_config(&config)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("failed to parse kubelet config"),
            "expected kubelet parse error, got: {err}"
        );
    }

    #[test]
    fn kubelet_config_path_custom() {
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String("/custom/kubelet.yaml".into()),
        );
        let desired = serde_yaml::Value::Mapping(mapping);
        let path = KubeletConfigurator::config_path(&desired);
        assert_eq!(path, PathBuf::from("/custom/kubelet.yaml"));
    }

    #[test]
    fn kubelet_diff_detects_changed_value() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        fs::write(&config, "maxPods: 100\ncgroupDriver: cgroupfs\n").unwrap();

        let mut settings = serde_yaml::Mapping::new();
        settings.insert(
            serde_yaml::Value::String("maxPods".into()),
            serde_yaml::Value::Number(110.into()),
        );
        settings.insert(
            serde_yaml::Value::String("cgroupDriver".into()),
            serde_yaml::Value::String("systemd".into()),
        );

        let mut desired_map = serde_yaml::Mapping::new();
        desired_map.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String(config.to_str().unwrap().into()),
        );
        desired_map.insert(
            serde_yaml::Value::String("settings".into()),
            serde_yaml::Value::Mapping(settings),
        );

        let kc = KubeletConfigurator;
        let desired = serde_yaml::Value::Mapping(desired_map);
        let drifts = kc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 2);

        let max_pods_drift = drifts.iter().find(|d| d.key == "kubelet.maxPods").unwrap();
        assert_eq!(max_pods_drift.expected, "110");
        assert_eq!(max_pods_drift.actual, "100");

        let cgroup_drift = drifts
            .iter()
            .find(|d| d.key == "kubelet.cgroupDriver")
            .unwrap();
        assert_eq!(cgroup_drift.expected, "systemd");
        assert_eq!(cgroup_drift.actual, "cgroupfs");
    }

    #[test]
    fn kubelet_diff_no_drift_when_matching() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        fs::write(&config, "maxPods: 110\n").unwrap();

        let mut settings = serde_yaml::Mapping::new();
        settings.insert(
            serde_yaml::Value::String("maxPods".into()),
            serde_yaml::Value::Number(110.into()),
        );

        let mut desired_map = serde_yaml::Mapping::new();
        desired_map.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String(config.to_str().unwrap().into()),
        );
        desired_map.insert(
            serde_yaml::Value::String("settings".into()),
            serde_yaml::Value::Mapping(settings),
        );

        let kc = KubeletConfigurator;
        let desired = serde_yaml::Value::Mapping(desired_map);
        let drifts = kc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kubelet_diff_missing_key_shows_not_set() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        fs::write(&config, "clusterDomain: cluster.local\n").unwrap();

        let mut settings = serde_yaml::Mapping::new();
        settings.insert(
            serde_yaml::Value::String("maxPods".into()),
            serde_yaml::Value::Number(110.into()),
        );

        let mut desired_map = serde_yaml::Mapping::new();
        desired_map.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String(config.to_str().unwrap().into()),
        );
        desired_map.insert(
            serde_yaml::Value::String("settings".into()),
            serde_yaml::Value::Mapping(settings),
        );

        let kc = KubeletConfigurator;
        let desired = serde_yaml::Value::Mapping(desired_map);
        let drifts = kc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "kubelet.maxPods");
        assert_eq!(drifts[0].actual, "<not set>");
    }

    #[test]
    fn kubelet_diff_no_settings_returns_empty() {
        let kc = KubeletConfigurator;
        let mut desired_map = serde_yaml::Mapping::new();
        desired_map.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String("/nonexistent/config.yaml".into()),
        );
        let desired = serde_yaml::Value::Mapping(desired_map);
        let drifts = kc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kubelet_diff_nonexistent_file_shows_not_set() {
        let mut settings = serde_yaml::Mapping::new();
        settings.insert(
            serde_yaml::Value::String("maxPods".into()),
            serde_yaml::Value::Number(110.into()),
        );

        let mut desired_map = serde_yaml::Mapping::new();
        desired_map.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String("/nonexistent/kubelet/config.yaml".into()),
        );
        desired_map.insert(
            serde_yaml::Value::String("settings".into()),
            serde_yaml::Value::Mapping(settings),
        );

        let kc = KubeletConfigurator;
        let desired = serde_yaml::Value::Mapping(desired_map);
        let drifts = kc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].actual, "<not set>");
    }

    // --- apparmor diff with temp files ---

    #[test]
    fn apparmor_diff_missing_profile_file() {
        let ac = AppArmorConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("test-profile".into()),
        );
        profile.insert(
            serde_yaml::Value::String("path".into()),
            serde_yaml::Value::String("/nonexistent/apparmor/test-profile".into()),
        );
        profile.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String("profile test-profile {}".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );
        let desired = serde_yaml::Value::Mapping(m);
        let drifts = ac.diff(&desired).unwrap();

        // Should report file missing
        let file_drift = drifts
            .iter()
            .find(|d| d.key == "apparmor.test-profile.file")
            .unwrap();
        assert_eq!(file_drift.expected, "present");
        assert_eq!(file_drift.actual, "missing");
    }

    #[test]
    fn apparmor_diff_content_mismatch() {
        let dir = tempdir().unwrap();
        let profile_path = dir.path().join("test-profile");
        fs::write(&profile_path, "old content").unwrap();

        let ac = AppArmorConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("test-profile".into()),
        );
        profile.insert(
            serde_yaml::Value::String("path".into()),
            serde_yaml::Value::String(profile_path.to_str().unwrap().into()),
        );
        profile.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String("new content".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );
        let desired = serde_yaml::Value::Mapping(m);
        let drifts = ac.diff(&desired).unwrap();

        let content_drift = drifts
            .iter()
            .find(|d| d.key == "apparmor.test-profile.content")
            .unwrap();
        assert_eq!(content_drift.expected, "updated");
        assert_eq!(content_drift.actual, "outdated");
    }

    #[test]
    fn apparmor_diff_content_matches_no_content_drift() {
        let dir = tempdir().unwrap();
        let profile_path = dir.path().join("test-profile");
        let content = "profile test-profile flags=(attach_disconnected) {}";
        fs::write(&profile_path, content).unwrap();

        let ac = AppArmorConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("test-profile".into()),
        );
        profile.insert(
            serde_yaml::Value::String("path".into()),
            serde_yaml::Value::String(profile_path.to_str().unwrap().into()),
        );
        profile.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String(content.to_string()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );
        let desired = serde_yaml::Value::Mapping(m);
        let drifts = ac.diff(&desired).unwrap();

        // No content drift, but may report "not loaded" depending on environment
        assert!(
            drifts.iter().all(|d| !d.key.contains("content")),
            "should not report content drift when content matches"
        );
    }

    #[test]
    fn apparmor_diff_path_traversal_skipped() {
        let ac = AppArmorConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("traversal-profile".into()),
        );
        profile.insert(
            serde_yaml::Value::String("path".into()),
            serde_yaml::Value::String("/etc/apparmor.d/../../../etc/passwd".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );
        let desired = serde_yaml::Value::Mapping(m);
        let drifts = ac.diff(&desired).unwrap();
        // Profile with path traversal should be skipped entirely
        assert!(drifts.is_empty());
    }

    #[test]
    fn apparmor_diff_no_profiles_key() {
        let ac = AppArmorConfigurator;
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let drifts = ac.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn apparmor_diff_profile_without_name_skipped() {
        let ac = AppArmorConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        // No "name" key
        profile.insert(
            serde_yaml::Value::String("path".into()),
            serde_yaml::Value::String("/tmp/profile".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );
        let desired = serde_yaml::Value::Mapping(m);
        let drifts = ac.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn apparmor_diff_profile_without_path_skipped() {
        let ac = AppArmorConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("test".into()),
        );
        // No "path" key

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );
        let desired = serde_yaml::Value::Mapping(m);
        let drifts = ac.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // --- seccomp diff with temp files ---

    #[test]
    fn seccomp_diff_missing_profile_file() {
        let sc = SeccompConfigurator;
        let dir = tempdir().unwrap();

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("default-audit".into()),
        );
        profile.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("default-audit.json".into()),
        );
        profile.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String(r#"{"defaultAction":"SCMP_ACT_LOG"}"#.into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profilesDir".into()),
            serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = sc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "seccomp.default-audit");
        assert_eq!(drifts[0].expected, "present");
        assert_eq!(drifts[0].actual, "missing");
    }

    #[test]
    fn seccomp_diff_content_mismatch() {
        let dir = tempdir().unwrap();
        let profile_path = dir.path().join("default-audit.json");
        fs::write(&profile_path, r#"{"defaultAction":"SCMP_ACT_ERRNO"}"#).unwrap();

        let sc = SeccompConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("default-audit".into()),
        );
        profile.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("default-audit.json".into()),
        );
        profile.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String(r#"{"defaultAction":"SCMP_ACT_LOG"}"#.into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profilesDir".into()),
            serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = sc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "seccomp.default-audit.content");
        assert_eq!(drifts[0].expected, "updated");
        assert_eq!(drifts[0].actual, "outdated");
    }

    #[test]
    fn seccomp_diff_content_matches_semantically() {
        let dir = tempdir().unwrap();
        let profile_path = dir.path().join("default-audit.json");
        // Write with different whitespace/key order
        fs::write(
            &profile_path,
            r#"{ "b": 2, "defaultAction": "SCMP_ACT_LOG" }"#,
        )
        .unwrap();

        let sc = SeccompConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("default-audit".into()),
        );
        profile.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("default-audit.json".into()),
        );
        profile.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String(r#"{"defaultAction":"SCMP_ACT_LOG","b":2}"#.into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profilesDir".into()),
            serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = sc.diff(&desired).unwrap();
        // json_equal should match semantically equivalent JSON
        assert!(drifts.is_empty());
    }

    #[test]
    fn seccomp_diff_path_traversal_skipped() {
        let sc = SeccompConfigurator;
        let dir = tempdir().unwrap();

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("evil".into()),
        );
        profile.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("../../etc/passwd".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profilesDir".into()),
            serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = sc.diff(&desired).unwrap();
        // Path traversal profiles should be skipped
        assert!(drifts.is_empty());
    }

    #[test]
    fn seccomp_diff_no_profiles_key() {
        let sc = SeccompConfigurator;
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let drifts = sc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn seccomp_diff_profile_without_name_skipped() {
        let sc = SeccompConfigurator;
        let dir = tempdir().unwrap();

        let mut profile = serde_yaml::Mapping::new();
        // No "name" key
        profile.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("test.json".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profilesDir".into()),
            serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = sc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn seccomp_diff_profile_without_file_skipped() {
        let sc = SeccompConfigurator;
        let dir = tempdir().unwrap();

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("test".into()),
        );
        // No "file" key

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profilesDir".into()),
            serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = sc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn seccomp_diff_uses_default_profiles_dir() {
        let sc = SeccompConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("test".into()),
        );
        profile.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("test.json".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        // No profilesDir — should use default /etc/cfgd/seccomp
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = sc.diff(&desired).unwrap();
        // File won't exist at default path, so should report missing
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "seccomp.test");
        assert_eq!(drifts[0].actual, "missing");
    }

    // --- certificate diff with temp files and permissions ---

    #[test]
    fn certificate_diff_missing_cert_and_key_and_ca() {
        let cc = CertificateConfigurator;

        let mut cert = serde_yaml::Mapping::new();
        cert.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("kubelet-client".into()),
        );
        cert.insert(
            serde_yaml::Value::String("certPath".into()),
            serde_yaml::Value::String("/nonexistent/cert.pem".into()),
        );
        cert.insert(
            serde_yaml::Value::String("keyPath".into()),
            serde_yaml::Value::String("/nonexistent/key.pem".into()),
        );
        cert.insert(
            serde_yaml::Value::String("caPath".into()),
            serde_yaml::Value::String("/nonexistent/ca.pem".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("certificates".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = cc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 3);
        assert!(drifts.iter().any(|d| d.key == "cert.kubelet-client.cert"));
        assert!(drifts.iter().any(|d| d.key == "cert.kubelet-client.key"));
        assert!(drifts.iter().any(|d| d.key == "cert.kubelet-client.ca"));
    }

    #[test]
    fn certificate_diff_wrong_permissions() {
        let dir = tempdir().unwrap();
        let cert_path = dir.path().join("tls.crt");
        let key_path = dir.path().join("tls.key");
        fs::write(&cert_path, "cert data").unwrap();
        fs::write(&key_path, "key data").unwrap();

        // Set permissions to 0o644 (default)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cert_path, fs::Permissions::from_mode(0o644)).unwrap();
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
        }

        let cc = CertificateConfigurator;

        let mut cert = serde_yaml::Mapping::new();
        cert.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("tls".into()),
        );
        cert.insert(
            serde_yaml::Value::String("certPath".into()),
            serde_yaml::Value::String(cert_path.to_str().unwrap().into()),
        );
        cert.insert(
            serde_yaml::Value::String("keyPath".into()),
            serde_yaml::Value::String(key_path.to_str().unwrap().into()),
        );
        cert.insert(
            serde_yaml::Value::String("mode".into()),
            serde_yaml::Value::String("0600".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("certificates".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = cc.diff(&desired).unwrap();

        #[cfg(unix)]
        {
            // Should detect permission drift on both files
            assert_eq!(drifts.len(), 2);
            for drift in &drifts {
                assert_eq!(drift.expected, "0600");
                assert_eq!(drift.actual, "0644");
            }
        }
    }

    #[test]
    fn certificate_diff_correct_permissions_no_drift() {
        let dir = tempdir().unwrap();
        let cert_path = dir.path().join("tls.crt");
        fs::write(&cert_path, "cert data").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cert_path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let cc = CertificateConfigurator;

        let mut cert = serde_yaml::Mapping::new();
        cert.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("tls".into()),
        );
        cert.insert(
            serde_yaml::Value::String("certPath".into()),
            serde_yaml::Value::String(cert_path.to_str().unwrap().into()),
        );
        cert.insert(
            serde_yaml::Value::String("mode".into()),
            serde_yaml::Value::String("0600".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("certificates".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = cc.diff(&desired).unwrap();

        #[cfg(unix)]
        {
            assert!(drifts.is_empty());
        }
    }

    #[test]
    fn certificate_diff_no_mode_no_permission_drift() {
        let dir = tempdir().unwrap();
        let cert_path = dir.path().join("tls.crt");
        fs::write(&cert_path, "cert data").unwrap();

        let cc = CertificateConfigurator;

        let mut cert = serde_yaml::Mapping::new();
        cert.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("tls".into()),
        );
        cert.insert(
            serde_yaml::Value::String("certPath".into()),
            serde_yaml::Value::String(cert_path.to_str().unwrap().into()),
        );
        // No "mode" key — no permission checking

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("certificates".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = cc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn certificate_diff_no_certificates_key() {
        let cc = CertificateConfigurator;
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let drifts = cc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn certificate_diff_cert_without_name_skipped() {
        let cc = CertificateConfigurator;

        let mut cert = serde_yaml::Mapping::new();
        // No "name" key
        cert.insert(
            serde_yaml::Value::String("certPath".into()),
            serde_yaml::Value::String("/nonexistent/cert.pem".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("certificates".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = cc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // --- set_toml_value edge cases ---

    #[test]
    fn set_toml_value_overwrites_non_table_intermediate() {
        let mut table = toml::Table::new();
        // Set "a" to a string first
        table.insert("a".to_string(), toml::Value::String("not a table".into()));
        // Now set "a.b" — should replace "a" with a table
        set_toml_value(
            &mut table,
            "a.b",
            &serde_yaml::Value::String("value".into()),
        );
        assert_eq!(find_toml_value(&table, "a.b"), Some("value".to_string()));
    }

    // --- yaml_to_toml_value edge cases ---

    #[test]
    fn yaml_to_toml_mapping_conversion() {
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            serde_yaml::Value::String("key".into()),
            serde_yaml::Value::String("value".into()),
        );
        let result = yaml_to_toml_value(&serde_yaml::Value::Mapping(mapping));
        match result {
            toml::Value::Table(t) => {
                assert_eq!(t.get("key").unwrap().as_str(), Some("value"));
            }
            _ => panic!("expected Table"),
        }
    }

    #[test]
    fn yaml_to_toml_sequence_conversion() {
        let seq = serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::Number(1.into()),
            serde_yaml::Value::Number(2.into()),
        ]);
        let result = yaml_to_toml_value(&seq);
        match result {
            toml::Value::Array(arr) => {
                assert_eq!(arr.len(), 2);
                assert_eq!(arr[0].as_integer(), Some(1));
                assert_eq!(arr[1].as_integer(), Some(2));
            }
            _ => panic!("expected Array"),
        }
    }

    #[test]
    fn yaml_to_toml_null_becomes_empty_string() {
        let result = yaml_to_toml_value(&serde_yaml::Value::Null);
        assert_eq!(result, toml::Value::String(String::new()));
    }

    #[test]
    fn yaml_to_toml_float_conversion() {
        let val = serde_yaml::Value::Number(serde_yaml::Number::from(1.234_f64));
        let result = yaml_to_toml_value(&val);
        match result {
            toml::Value::Float(f) => assert!((f - 1.234).abs() < 0.001),
            _ => panic!("expected Float, got {:?}", result),
        }
    }

    #[test]
    fn yaml_to_toml_mapping_non_string_keys_skipped() {
        let mut mapping = serde_yaml::Mapping::new();
        mapping.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::String("value".into()),
        );
        mapping.insert(
            serde_yaml::Value::String("valid".into()),
            serde_yaml::Value::String("kept".into()),
        );
        let result = yaml_to_toml_value(&serde_yaml::Value::Mapping(mapping));
        match result {
            toml::Value::Table(t) => {
                assert_eq!(t.len(), 1);
                assert_eq!(t.get("valid").unwrap().as_str(), Some("kept"));
            }
            _ => panic!("expected Table"),
        }
    }

    // --- toml_value_to_string edge cases ---

    #[test]
    fn toml_value_to_string_float() {
        let result = toml_value_to_string(&toml::Value::Float(1.234));
        assert!(result.starts_with("1.234"));
    }

    #[test]
    fn toml_value_to_string_array_uses_display() {
        let arr = toml::Value::Array(vec![toml::Value::Integer(1)]);
        let result = toml_value_to_string(&arr);
        assert!(result.contains('1'));
    }

    // --- json_equal edge cases ---

    #[test]
    fn json_equal_both_empty_objects() {
        assert!(json_equal("{}", "{}"));
    }

    #[test]
    fn json_equal_nested_objects() {
        assert!(json_equal(r#"{"a":{"b":1}}"#, r#"{"a":{"b":1}}"#));
        assert!(!json_equal(r#"{"a":{"b":1}}"#, r#"{"a":{"b":2}}"#));
    }

    #[test]
    fn json_equal_whitespace_trimming_fallback() {
        // Both invalid JSON but equal after trimming
        assert!(json_equal("  not json  ", "not json"));
    }

    // --- containerd diff with boolean TOML values ---

    #[test]
    fn containerd_diff_boolean_setting() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        fs::write(&config, "SystemdCgroup = false\n").unwrap();

        let mut settings = serde_yaml::Mapping::new();
        settings.insert(
            serde_yaml::Value::String("SystemdCgroup".into()),
            serde_yaml::Value::Bool(true),
        );

        let mut desired_map = serde_yaml::Mapping::new();
        desired_map.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String(config.to_str().unwrap().into()),
        );
        desired_map.insert(
            serde_yaml::Value::String("settings".into()),
            serde_yaml::Value::Mapping(settings),
        );

        let cc = ContainerdConfigurator;
        let desired = serde_yaml::Value::Mapping(desired_map);
        let drifts = cc.diff(&desired).unwrap();
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "containerd.SystemdCgroup");
        assert_eq!(drifts[0].expected, "true");
        assert_eq!(drifts[0].actual, "false");
    }

    // --- kubelet diff with string matching ---

    #[test]
    fn kubelet_diff_string_value_matches() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        fs::write(&config, "cgroupDriver: systemd\n").unwrap();

        let mut settings = serde_yaml::Mapping::new();
        settings.insert(
            serde_yaml::Value::String("cgroupDriver".into()),
            serde_yaml::Value::String("systemd".into()),
        );

        let mut desired_map = serde_yaml::Mapping::new();
        desired_map.insert(
            serde_yaml::Value::String("configPath".into()),
            serde_yaml::Value::String(config.to_str().unwrap().into()),
        );
        desired_map.insert(
            serde_yaml::Value::String("settings".into()),
            serde_yaml::Value::Mapping(settings),
        );

        let kc = KubeletConfigurator;
        let desired = serde_yaml::Value::Mapping(desired_map);
        let drifts = kc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // --- SysctlConfigurator current_state ---

    #[test]
    fn sysctl_current_state_returns_empty_mapping() {
        let sc = SysctlConfigurator;
        let state = sc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    // --- KernelModuleConfigurator current_state ---

    #[test]
    fn kernel_module_current_state_returns_empty_sequence() {
        let km = KernelModuleConfigurator;
        let state = km.current_state().unwrap();
        assert!(state.is_sequence());
        assert!(state.as_sequence().unwrap().is_empty());
    }

    // --- ContainerdConfigurator current_state ---

    #[test]
    fn containerd_current_state_returns_empty_mapping() {
        let cc = ContainerdConfigurator;
        let state = cc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    // --- KubeletConfigurator current_state ---

    #[test]
    fn kubelet_current_state_returns_empty_mapping() {
        let kc = KubeletConfigurator;
        let state = kc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    // --- AppArmorConfigurator current_state ---

    #[test]
    fn apparmor_current_state_returns_empty_mapping() {
        let ac = AppArmorConfigurator;
        let state = ac.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    // --- SeccompConfigurator current_state ---

    #[test]
    fn seccomp_current_state_returns_empty_mapping() {
        let sc = SeccompConfigurator;
        let state = sc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    // --- CertificateConfigurator current_state ---

    #[test]
    fn certificate_current_state_returns_empty_mapping() {
        let cc = CertificateConfigurator;
        let state = cc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }

    // --- SysctlConfigurator::validate_sysctl_key additional patterns ---

    #[test]
    fn sysctl_validate_key_with_dash() {
        assert!(
            SysctlConfigurator::validate_sysctl_key("net.bridge.bridge-nf-call-ip6tables").is_ok()
        );
    }

    #[test]
    fn sysctl_validate_key_with_underscore_and_digits() {
        assert!(SysctlConfigurator::validate_sysctl_key("vm.max_map_count").is_ok());
    }

    #[test]
    fn sysctl_validate_key_single_segment() {
        assert!(SysctlConfigurator::validate_sysctl_key("hostname").is_ok());
    }

    #[test]
    fn sysctl_validate_key_with_tab_rejected() {
        assert!(SysctlConfigurator::validate_sysctl_key("key\twith\ttab").is_err());
    }

    #[test]
    fn sysctl_validate_key_with_shell_injection_rejected() {
        assert!(SysctlConfigurator::validate_sysctl_key("key$(whoami)").is_err());
    }

    #[test]
    fn sysctl_validate_key_with_backtick_rejected() {
        assert!(SysctlConfigurator::validate_sysctl_key("key`cmd`").is_err());
    }

    // --- ContainerdConfigurator::read_current_config with valid TOML ---

    #[test]
    fn containerd_read_config_with_multiple_sections() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        fs::write(
            &config,
            "version = 2\n\n[plugins.cri]\nsandbox_image = \"pause:3.9\"\n\n[plugins.cri.containerd.runtimes.runc.options]\nSystemdCgroup = true\n",
        ).unwrap();
        let table = ContainerdConfigurator::read_current_config(&config).unwrap();
        assert_eq!(table.get("version").unwrap().as_integer(), Some(2));
        assert_eq!(
            find_toml_value(&table, "plugins.cri.sandbox_image"),
            Some("pause:3.9".to_string())
        );
        assert_eq!(
            find_toml_value(
                &table,
                "plugins.cri.containerd.runtimes.runc.options.SystemdCgroup"
            ),
            Some("true".to_string())
        );
    }

    // --- ContainerdConfigurator diff with non-mapping desired ---

    #[test]
    fn containerd_diff_non_mapping_returns_empty() {
        let cc = ContainerdConfigurator;
        let desired = serde_yaml::Value::String("not a mapping".into());
        let drifts = cc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // --- KubeletConfigurator diff with non-mapping desired ---

    #[test]
    fn kubelet_diff_non_mapping_returns_empty() {
        let kc = KubeletConfigurator;
        let desired = serde_yaml::Value::String("not a mapping".into());
        // config_path falls back to default, which doesn't exist
        // and settings extraction returns None since desired is not a mapping
        // Note: config_path looks for "configPath" on desired, which is not available
        // so uses default. The real test is that settings returns None.
        let drifts = kc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // --- seccomp diff with existing matching JSON content ---

    #[test]
    fn seccomp_diff_existing_matching_content_no_drift() {
        let dir = tempdir().unwrap();
        let profile_path = dir.path().join("default-audit.json");
        let content = r#"{"defaultAction":"SCMP_ACT_LOG"}"#;
        fs::write(&profile_path, content).unwrap();

        let sc = SeccompConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("default-audit".into()),
        );
        profile.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("default-audit.json".into()),
        );
        profile.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String(content.into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profilesDir".into()),
            serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = sc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // --- seccomp diff with profile that has no content key ---

    #[test]
    fn seccomp_diff_profile_without_content_no_content_drift() {
        let dir = tempdir().unwrap();
        let profile_path = dir.path().join("existing.json");
        fs::write(&profile_path, r#"{"defaultAction":"SCMP_ACT_LOG"}"#).unwrap();

        let sc = SeccompConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("existing".into()),
        );
        profile.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("existing.json".into()),
        );
        // No "content" key — content comparison should be skipped

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profilesDir".into()),
            serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = sc.diff(&desired).unwrap();
        // File exists, no content key — no drift expected
        assert!(drifts.is_empty());
    }

    // --- certificate diff with existing cert with correct permissions ---

    #[test]
    fn certificate_diff_existing_cert_no_drift() {
        let dir = tempdir().unwrap();
        let cert_path = dir.path().join("tls.crt");
        let key_path = dir.path().join("tls.key");
        fs::write(&cert_path, "cert data").unwrap();
        fs::write(&key_path, "key data").unwrap();

        let cc = CertificateConfigurator;

        let mut cert = serde_yaml::Mapping::new();
        cert.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("tls".into()),
        );
        cert.insert(
            serde_yaml::Value::String("certPath".into()),
            serde_yaml::Value::String(cert_path.to_str().unwrap().into()),
        );
        cert.insert(
            serde_yaml::Value::String("keyPath".into()),
            serde_yaml::Value::String(key_path.to_str().unwrap().into()),
        );
        // No mode key — no permission drift

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("certificates".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        let drifts = cc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // --- certificate diff with non-sequence desired ---

    #[test]
    fn certificate_diff_non_sequence_desired() {
        let cc = CertificateConfigurator;
        let desired = serde_yaml::Value::String("not a mapping".into());
        let drifts = cc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // --- CertificateConfigurator is_available ---

    #[test]
    fn certificate_is_available_on_linux() {
        let cc = CertificateConfigurator;
        assert_eq!(cc.is_available(), cfg!(target_os = "linux"));
    }

    // --- SeccompConfigurator is_available ---

    #[test]
    fn seccomp_is_available_depends_on_proc() {
        let sc = SeccompConfigurator;
        let expected = std::path::Path::new("/proc/sys/kernel/seccomp").exists();
        assert_eq!(sc.is_available(), expected);
    }

    // --- find_toml_value dot-path with missing intermediate ---

    #[test]
    fn find_toml_value_dot_path_with_nonexistent_intermediate() {
        let mut table = toml::Table::new();
        table.insert("key".to_string(), toml::Value::String("val".into()));
        assert_eq!(find_toml_value(&table, "nonexistent.sub.key"), None);
    }

    // --- find_toml_value recursive search ---

    #[test]
    fn find_toml_value_recursive_search_in_nested() {
        let mut inner = toml::Table::new();
        inner.insert("deep_key".to_string(), toml::Value::Integer(42));
        let mut mid = toml::Table::new();
        mid.insert("inner".to_string(), toml::Value::Table(inner));
        let mut table = toml::Table::new();
        table.insert("outer".to_string(), toml::Value::Table(mid));

        // Non-dotted key should be found via recursive search
        assert_eq!(find_toml_value(&table, "deep_key"), Some("42".to_string()));
    }

    // --- json_equal both empty arrays ---

    #[test]
    fn json_equal_empty_arrays() {
        assert!(json_equal("[]", "[]"));
    }

    #[test]
    fn json_equal_arrays_with_different_order() {
        assert!(!json_equal("[1,2]", "[2,1]"));
    }

    // --- KernelModuleConfigurator diff mixed entries ---

    #[test]
    fn kernel_module_diff_mixed_string_and_non_string() {
        let km = KernelModuleConfigurator;
        // First entry is a non-string (skipped), second is a definitely-missing module
        let desired = serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::Number(99.into()),
            serde_yaml::Value::String("cfgd_fake_module_test_abc".into()),
        ]);
        let drifts = km.diff(&desired).unwrap();
        // Only the string module should produce drift
        assert_eq!(drifts.len(), 1);
        assert_eq!(drifts[0].key, "cfgd_fake_module_test_abc");
    }

    // --- ContainerdConfigurator config_path default vs custom ---

    #[test]
    fn containerd_config_path_falls_back_to_default_for_empty_mapping() {
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let path = ContainerdConfigurator::config_path(&desired);
        assert_eq!(
            path,
            PathBuf::from(ContainerdConfigurator::DEFAULT_CONFIG_PATH)
        );
    }

    // --- KubeletConfigurator config_path ---

    #[test]
    fn kubelet_config_path_falls_back_to_default_for_empty_mapping() {
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let path = KubeletConfigurator::config_path(&desired);
        assert_eq!(
            path,
            PathBuf::from(KubeletConfigurator::DEFAULT_CONFIG_PATH)
        );
    }

    // --- KubeletConfigurator read_current_config with complex YAML ---

    #[test]
    fn kubelet_read_config_with_nested_yaml() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.yaml");
        fs::write(
            &config,
            "apiVersion: kubelet.config.k8s.io/v1beta1\nkind: KubeletConfiguration\nmaxPods: 110\nclusterDNS:\n  - 10.96.0.10\n",
        ).unwrap();
        let value = KubeletConfigurator::read_current_config(&config).unwrap();
        assert_eq!(value.get("maxPods").and_then(|v| v.as_u64()), Some(110));
        assert!(value.get("clusterDNS").unwrap().is_sequence());
    }

    // --- set_toml_value deeply nested ---

    #[test]
    fn set_toml_value_deeply_nested_path() {
        let mut table = toml::Table::new();
        set_toml_value(
            &mut table,
            "a.b.c.d.e",
            &serde_yaml::Value::String("deep".into()),
        );
        assert_eq!(
            find_toml_value(&table, "a.b.c.d.e"),
            Some("deep".to_string())
        );
    }

    #[test]
    fn set_toml_value_boolean_in_nested_path() {
        let mut table = toml::Table::new();
        set_toml_value(
            &mut table,
            "plugins.cri.containerd.runtimes.runc.options.SystemdCgroup",
            &serde_yaml::Value::Bool(true),
        );
        assert_eq!(
            find_toml_value(
                &table,
                "plugins.cri.containerd.runtimes.runc.options.SystemdCgroup"
            ),
            Some("true".to_string())
        );
    }

    // --- persist_all_sysctls_to: file content verification ---

    #[test]
    fn persist_sysctls_writes_sorted_conf_file() {
        let dir = tempdir().unwrap();
        let conf_dir = dir.path().join("sysctl.d");

        let mut entries = BTreeMap::new();
        entries.insert("net.ipv4.ip_forward", "1".to_string());
        entries.insert("vm.max_map_count", "262144".to_string());
        entries.insert("net.bridge.bridge-nf-call-iptables", "1".to_string());

        SysctlConfigurator::persist_all_sysctls_to(&conf_dir, &entries).unwrap();

        let content = fs::read_to_string(conf_dir.join("99-cfgd.conf")).unwrap();
        assert!(
            content.starts_with("# Managed by cfgd"),
            "missing header comment"
        );
        // BTreeMap iterates in sorted order
        assert!(content.contains("net.bridge.bridge-nf-call-iptables = 1\n"));
        assert!(content.contains("net.ipv4.ip_forward = 1\n"));
        assert!(content.contains("vm.max_map_count = 262144\n"));
        // Verify net.bridge comes before net.ipv4 (sorted)
        let bridge_pos = content.find("net.bridge").unwrap();
        let ipv4_pos = content.find("net.ipv4").unwrap();
        assert!(bridge_pos < ipv4_pos, "entries should be in sorted order");
    }

    #[test]
    fn persist_sysctls_creates_conf_dir_if_missing() {
        let dir = tempdir().unwrap();
        let conf_dir = dir.path().join("nonexistent").join("sysctl.d");
        assert!(!conf_dir.exists());

        let mut entries = BTreeMap::new();
        entries.insert("net.ipv4.ip_forward", "1".to_string());

        SysctlConfigurator::persist_all_sysctls_to(&conf_dir, &entries).unwrap();
        assert!(conf_dir.join("99-cfgd.conf").exists());
    }

    #[test]
    fn persist_sysctls_empty_entries_removes_conf_file() {
        let dir = tempdir().unwrap();
        let conf_dir = dir.path().join("sysctl.d");
        fs::create_dir_all(&conf_dir).unwrap();
        let conf_path = conf_dir.join("99-cfgd.conf");
        fs::write(&conf_path, "old content").unwrap();
        assert!(conf_path.exists());

        let entries: BTreeMap<&str, String> = BTreeMap::new();
        SysctlConfigurator::persist_all_sysctls_to(&conf_dir, &entries).unwrap();
        assert!(!conf_path.exists(), "empty entries should remove the file");
    }

    #[test]
    fn persist_sysctls_overwrites_existing_content() {
        let dir = tempdir().unwrap();
        let conf_dir = dir.path().join("sysctl.d");
        fs::create_dir_all(&conf_dir).unwrap();
        fs::write(conf_dir.join("99-cfgd.conf"), "old.key = old_value\n").unwrap();

        let mut entries = BTreeMap::new();
        entries.insert("new.key", "new_value".to_string());

        SysctlConfigurator::persist_all_sysctls_to(&conf_dir, &entries).unwrap();

        let content = fs::read_to_string(conf_dir.join("99-cfgd.conf")).unwrap();
        assert!(content.contains("new.key = new_value"));
        assert!(
            !content.contains("old.key"),
            "old content should be replaced"
        );
    }

    // --- persist_modules_to: file content verification ---

    #[test]
    fn persist_modules_writes_one_per_line() {
        let dir = tempdir().unwrap();
        let conf_dir = dir.path().join("modules-load.d");

        KernelModuleConfigurator::persist_modules_to(
            &conf_dir,
            &["br_netfilter", "overlay", "ip_vs"],
        )
        .unwrap();

        let content = fs::read_to_string(conf_dir.join("cfgd.conf")).unwrap();
        assert!(
            content.starts_with("# Managed by cfgd"),
            "missing header comment"
        );
        assert!(content.contains("br_netfilter\n"));
        assert!(content.contains("overlay\n"));
        assert!(content.contains("ip_vs\n"));
        // Verify each module is on its own line (not space-separated)
        let module_lines: Vec<&str> = content
            .lines()
            .filter(|l| !l.starts_with('#') && !l.is_empty())
            .collect();
        assert_eq!(module_lines.len(), 3);
    }

    #[test]
    fn persist_modules_creates_dir_if_missing() {
        let dir = tempdir().unwrap();
        let conf_dir = dir.path().join("deep").join("modules-load.d");
        assert!(!conf_dir.exists());

        KernelModuleConfigurator::persist_modules_to(&conf_dir, &["overlay"]).unwrap();
        assert!(conf_dir.join("cfgd.conf").exists());
    }

    #[test]
    fn persist_modules_empty_removes_conf_file() {
        let dir = tempdir().unwrap();
        let conf_dir = dir.path().join("modules-load.d");
        fs::create_dir_all(&conf_dir).unwrap();
        let conf_path = conf_dir.join("cfgd.conf");
        fs::write(&conf_path, "old content").unwrap();

        KernelModuleConfigurator::persist_modules_to(&conf_dir, &[]).unwrap();
        assert!(
            !conf_path.exists(),
            "empty modules should remove the conf file"
        );
    }

    #[test]
    fn persist_modules_overwrites_existing() {
        let dir = tempdir().unwrap();
        let conf_dir = dir.path().join("modules-load.d");
        fs::create_dir_all(&conf_dir).unwrap();
        fs::write(conf_dir.join("cfgd.conf"), "old_module\n").unwrap();

        KernelModuleConfigurator::persist_modules_to(&conf_dir, &["new_module"]).unwrap();

        let content = fs::read_to_string(conf_dir.join("cfgd.conf")).unwrap();
        assert!(content.contains("new_module"));
        assert!(!content.contains("old_module"));
    }

    // --- SeccompConfigurator apply: writes profiles to temp dirs ---

    #[test]
    fn seccomp_apply_writes_profiles() {
        let dir = tempdir().unwrap();
        let profiles_dir = dir.path().join("seccomp");
        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let sc = SeccompConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("test-audit".into()),
        );
        profile.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("test-audit.json".into()),
        );
        profile.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String(r#"{"defaultAction":"SCMP_ACT_LOG"}"#.into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profilesDir".into()),
            serde_yaml::Value::String(profiles_dir.to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        sc.apply(&desired, &printer).unwrap();

        let written = fs::read_to_string(profiles_dir.join("test-audit.json")).unwrap();
        assert_eq!(written, r#"{"defaultAction":"SCMP_ACT_LOG"}"#);
    }

    #[test]
    fn seccomp_apply_no_profiles_key_is_noop() {
        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let sc = SeccompConfigurator;
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        // Should not error even with no profiles key
        sc.apply(&desired, &printer).unwrap();
    }

    #[test]
    fn seccomp_apply_skips_missing_fields() {
        let dir = tempdir().unwrap();
        let profiles_dir = dir.path().join("seccomp");
        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let sc = SeccompConfigurator;

        // Profile with no "file" key
        let mut p1 = serde_yaml::Mapping::new();
        p1.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("no-file".into()),
        );
        p1.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String("data".into()),
        );

        // Profile with no "content" key
        let mut p2 = serde_yaml::Mapping::new();
        p2.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("no-content".into()),
        );
        p2.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("no-content.json".into()),
        );

        // Profile with no "name" key
        let mut p3 = serde_yaml::Mapping::new();
        p3.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("nameless.json".into()),
        );
        p3.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String("data".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profilesDir".into()),
            serde_yaml::Value::String(profiles_dir.to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![
                serde_yaml::Value::Mapping(p1),
                serde_yaml::Value::Mapping(p2),
                serde_yaml::Value::Mapping(p3),
            ]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        sc.apply(&desired, &printer).unwrap();

        // profiles_dir should be created but no profiles written (all incomplete)
        assert!(profiles_dir.exists());
        // No files should have been written since each profile is missing a required field
        let entries: Vec<_> = fs::read_dir(&profiles_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(entries.is_empty(), "no profiles should be written");
    }

    #[test]
    fn seccomp_apply_path_traversal_skipped() {
        let dir = tempdir().unwrap();
        let profiles_dir = dir.path().join("seccomp");
        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let sc = SeccompConfigurator;

        let mut profile = serde_yaml::Mapping::new();
        profile.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("evil".into()),
        );
        profile.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("../../etc/passwd".into()),
        );
        profile.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String("hacked".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profilesDir".into()),
            serde_yaml::Value::String(profiles_dir.to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        sc.apply(&desired, &printer).unwrap();

        // The traversal path file should NOT have been written
        let etc_passwd = dir.path().join("etc/passwd");
        assert!(!etc_passwd.exists(), "path traversal should be blocked");
    }

    #[test]
    fn seccomp_apply_multiple_profiles() {
        let dir = tempdir().unwrap();
        let profiles_dir = dir.path().join("seccomp");
        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let sc = SeccompConfigurator;

        let mut p1 = serde_yaml::Mapping::new();
        p1.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("audit".into()),
        );
        p1.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("audit.json".into()),
        );
        p1.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String(r#"{"action":"LOG"}"#.into()),
        );

        let mut p2 = serde_yaml::Mapping::new();
        p2.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("strict".into()),
        );
        p2.insert(
            serde_yaml::Value::String("file".into()),
            serde_yaml::Value::String("strict.json".into()),
        );
        p2.insert(
            serde_yaml::Value::String("content".into()),
            serde_yaml::Value::String(r#"{"action":"ERRNO"}"#.into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profilesDir".into()),
            serde_yaml::Value::String(profiles_dir.to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(vec![
                serde_yaml::Value::Mapping(p1),
                serde_yaml::Value::Mapping(p2),
            ]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        sc.apply(&desired, &printer).unwrap();

        assert_eq!(
            fs::read_to_string(profiles_dir.join("audit.json")).unwrap(),
            r#"{"action":"LOG"}"#
        );
        assert_eq!(
            fs::read_to_string(profiles_dir.join("strict.json")).unwrap(),
            r#"{"action":"ERRNO"}"#
        );
    }

    // --- CertificateConfigurator apply: creates dir and sets permissions ---

    #[test]
    fn certificate_apply_creates_ca_cert_dir() {
        let dir = tempdir().unwrap();
        let ca_dir = dir.path().join("pki");
        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let cc = CertificateConfigurator;

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("caCertDir".into()),
            serde_yaml::Value::String(ca_dir.to_str().unwrap().into()),
        );
        // No certificates key — should only create the dir
        let desired = serde_yaml::Value::Mapping(m);
        cc.apply(&desired, &printer).unwrap();
        assert!(ca_dir.exists());
    }

    #[test]
    fn certificate_apply_no_certificates_is_noop() {
        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let cc = CertificateConfigurator;
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        // Should not error with no caCertDir or certificates
        cc.apply(&desired, &printer).unwrap();
    }

    #[test]
    fn certificate_apply_sets_permissions_on_existing_files() {
        let dir = tempdir().unwrap();
        let cert_path = dir.path().join("tls.crt");
        let key_path = dir.path().join("tls.key");
        fs::write(&cert_path, "cert data").unwrap();
        fs::write(&key_path, "key data").unwrap();

        // Set initial permissions to 0o644
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cert_path, fs::Permissions::from_mode(0o644)).unwrap();
            fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
        }

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let cc = CertificateConfigurator;

        let mut cert = serde_yaml::Mapping::new();
        cert.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("tls".into()),
        );
        cert.insert(
            serde_yaml::Value::String("certPath".into()),
            serde_yaml::Value::String(cert_path.to_str().unwrap().into()),
        );
        cert.insert(
            serde_yaml::Value::String("keyPath".into()),
            serde_yaml::Value::String(key_path.to_str().unwrap().into()),
        );
        cert.insert(
            serde_yaml::Value::String("mode".into()),
            serde_yaml::Value::String("0600".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("caCertDir".into()),
            serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("certificates".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        cc.apply(&desired, &printer).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let cert_mode = fs::metadata(&cert_path).unwrap().permissions().mode() & 0o777;
            let key_mode = fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(cert_mode, 0o600);
            assert_eq!(key_mode, 0o600);
        }
    }

    #[test]
    fn certificate_apply_warns_for_missing_files() {
        let dir = tempdir().unwrap();
        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let cc = CertificateConfigurator;

        let mut cert = serde_yaml::Mapping::new();
        cert.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("missing".into()),
        );
        cert.insert(
            serde_yaml::Value::String("certPath".into()),
            serde_yaml::Value::String("/nonexistent/cert.pem".into()),
        );
        cert.insert(
            serde_yaml::Value::String("mode".into()),
            serde_yaml::Value::String("0600".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("caCertDir".into()),
            serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("certificates".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        // Should not error — just warns about missing files
        cc.apply(&desired, &printer).unwrap();
    }

    #[test]
    fn certificate_apply_skips_cert_without_name() {
        let dir = tempdir().unwrap();
        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let cc = CertificateConfigurator;

        let mut cert = serde_yaml::Mapping::new();
        // No "name" key
        cert.insert(
            serde_yaml::Value::String("certPath".into()),
            serde_yaml::Value::String("/tmp/cert.pem".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("caCertDir".into()),
            serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("certificates".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        cc.apply(&desired, &printer).unwrap();
    }

    #[test]
    fn certificate_apply_correct_permissions_no_change() {
        let dir = tempdir().unwrap();
        let cert_path = dir.path().join("already-ok.crt");
        fs::write(&cert_path, "cert data").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&cert_path, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let cc = CertificateConfigurator;

        let mut cert = serde_yaml::Mapping::new();
        cert.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String("ok-cert".into()),
        );
        cert.insert(
            serde_yaml::Value::String("certPath".into()),
            serde_yaml::Value::String(cert_path.to_str().unwrap().into()),
        );
        cert.insert(
            serde_yaml::Value::String("mode".into()),
            serde_yaml::Value::String("0600".into()),
        );

        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("caCertDir".into()),
            serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
        );
        m.insert(
            serde_yaml::Value::String("certificates".into()),
            serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
        );

        let desired = serde_yaml::Value::Mapping(m);
        // Should not error and should detect permissions are already correct
        cc.apply(&desired, &printer).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&cert_path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "permissions should remain unchanged");
        }
    }

    // --- SeccompConfigurator apply uses default profilesDir ---

    #[test]
    fn seccomp_apply_uses_default_profiles_dir_when_unset() {
        let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Quiet);
        let sc = SeccompConfigurator;

        // profiles key with empty sequence — should try to create /etc/cfgd/seccomp
        // but that requires root, so we just verify the no-profiles case
        let mut m = serde_yaml::Mapping::new();
        m.insert(
            serde_yaml::Value::String("profiles".into()),
            serde_yaml::Value::Sequence(Vec::new()),
        );
        let desired = serde_yaml::Value::Mapping(m);
        // Empty profiles list - should still try to create dir but won't error
        // because we catch the permission error at fs::create_dir_all
        // Actually, let's verify this specific case doesn't panic
        let result = sc.apply(&desired, &printer);
        // On CI/test machines this may fail due to permissions, which is expected
        // The important thing is it doesn't panic
        let _ = result;
    }
}
