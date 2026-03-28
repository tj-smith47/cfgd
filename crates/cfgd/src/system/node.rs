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
        let conf_dir = Path::new("/etc/sysctl.d");
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
        let conf_dir = Path::new("/etc/modules-load.d");
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
                let _ = cfgd_core::atomic_write(&config_path, &state.content);
                let _ = Self::restart_containerd();
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
                let _ = cfgd_core::atomic_write(&config_path, &state.content);
                let _ = Self::restart_kubelet();
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

fn find_toml_value(table: &toml::Table, key: &str) -> Option<String> {
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

fn toml_value_to_string(value: &toml::Value) -> String {
    match value {
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Integer(n) => n.to_string(),
        toml::Value::Float(n) => n.to_string(),
        toml::Value::String(s) => s.clone(),
        _ => format!("{}", value),
    }
}

fn set_toml_value(table: &mut toml::Table, key: &str, value: &serde_yaml::Value) {
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

fn yaml_to_toml_value(value: &serde_yaml::Value) -> toml::Value {
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

    #[test]
    fn sysctl_configurator_name() {
        let sc = SysctlConfigurator;
        assert_eq!(sc.name(), "sysctl");
    }

    #[test]
    fn kernel_module_configurator_name() {
        let km = KernelModuleConfigurator;
        assert_eq!(km.name(), "kernelModules");
    }

    #[test]
    fn containerd_configurator_name() {
        let cc = ContainerdConfigurator;
        assert_eq!(cc.name(), "containerd");
    }

    #[test]
    fn kubelet_configurator_name() {
        let kc = KubeletConfigurator;
        assert_eq!(kc.name(), "kubelet");
    }

    #[test]
    fn apparmor_configurator_name() {
        let ac = AppArmorConfigurator;
        assert_eq!(ac.name(), "apparmor");
    }

    #[test]
    fn seccomp_configurator_name() {
        let sc = SeccompConfigurator;
        assert_eq!(sc.name(), "seccomp");
    }

    #[test]
    fn certificate_configurator_name() {
        let cc = CertificateConfigurator;
        assert_eq!(cc.name(), "certificates");
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
    fn sysctl_diff_with_empty_mapping() {
        let sc = SysctlConfigurator;
        let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        let drifts = sc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn sysctl_diff_with_non_mapping() {
        let sc = SysctlConfigurator;
        let desired = serde_yaml::Value::String("invalid".into());
        let drifts = sc.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kernel_module_diff_with_empty_sequence() {
        let km = KernelModuleConfigurator;
        let desired = serde_yaml::Value::Sequence(Vec::new());
        let drifts = km.diff(&desired).unwrap();
        assert!(drifts.is_empty());
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
}
