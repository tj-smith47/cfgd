use std::process::Command;

use cfgd_core::errors::{CfgdError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

/// GitConfigurator — manages `git config --global` settings declaratively.
///
/// Config keys are flat dotted strings that map 1:1 to git config keys:
///
/// ```yaml
/// system:
///   git:
///     user.name: "Jane Doe"
///     user.email: jane@work.com
///     commit.gpgSign: true
///     init.defaultBranch: main
/// ```
///
/// Only keys declared by cfgd are managed; existing keys not in the desired
/// mapping are left untouched.
pub struct GitConfigurator;

/// Determine the git config location arguments.
///
/// If `GIT_CONFIG_GLOBAL` is set (used by tests to point at a temp file),
/// return `["--file", "<path>"]`.  Otherwise return `["--global"]`.
fn git_location_args() -> Vec<String> {
    if let Ok(path) = std::env::var("GIT_CONFIG_GLOBAL") {
        vec!["--file".to_string(), path]
    } else {
        vec!["--global".to_string()]
    }
}

/// Read a single git config key.
///
/// Returns `Some(value)` if the key exists and git exits 0, `None` otherwise.
fn git_config_get(key: &str) -> Option<String> {
    let loc = git_location_args();
    Command::new("git")
        .arg("config")
        .args(&loc)
        .args(["--get", key])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| cfgd_core::stdout_lossy_trimmed(&o))
}

/// Convert a YAML value to the string git config expects.
///
/// - `Bool`   → `"true"` / `"false"` (git's canonical form)
/// - `Number` → decimal string
/// - `String` → as-is
/// - anything else → `Debug` representation
fn value_to_git_string(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::String(s) => s.clone(),
        _ => format!("{value:?}"),
    }
}

impl SystemConfigurator for GitConfigurator {
    fn name(&self) -> &str {
        "git"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("git")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        // We report an empty mapping; the reconciler uses diff() for drift detection.
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        let mut drifts = Vec::new();

        for (key, desired_val) in mapping {
            let key_str = match key.as_str() {
                Some(k) => k,
                None => continue,
            };

            let desired_str = value_to_git_string(desired_val);
            let actual_str = git_config_get(key_str).unwrap_or_default();

            if actual_str != desired_str {
                drifts.push(SystemDrift {
                    key: key_str.to_string(),
                    expected: desired_str,
                    actual: actual_str,
                });
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let mapping = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(()),
        };

        let loc = git_location_args();

        for (key, desired_val) in mapping {
            let key_str = match key.as_str() {
                Some(k) => k,
                None => continue,
            };

            let desired_str = value_to_git_string(desired_val);

            printer.info(&format!("git config --global {} {}", key_str, desired_str));

            let output = Command::new("git")
                .arg("config")
                .args(&loc)
                .args([key_str, &desired_str])
                .output()
                .map_err(CfgdError::Io)?;

            if !output.status.success() {
                printer.warning(&format!(
                    "git config --global {} failed: {}",
                    key_str,
                    cfgd_core::stderr_lossy_trimmed(&output)
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------------------------------------------------------------------------
    // Test isolation
    //
    // All tests that touch git config point `GIT_CONFIG_GLOBAL` at a temp file.
    // Because tests run in parallel and env var mutation is unsafe, we use a
    // std::sync::Mutex to serialise the tests that need to mutate the env var.
    // ---------------------------------------------------------------------------

    static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with `GIT_CONFIG_GLOBAL` pointing at a fresh temp file.
    /// Serialised via `ENV_MUTEX` to prevent races between parallel tests.
    fn with_temp_global_config<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = ENV_MUTEX.lock().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let config_file = dir.path().join(".gitconfig");
        // Create an empty file so git treats it as a valid config.
        std::fs::write(&config_file, "").unwrap();

        // SAFETY: serialised by ENV_MUTEX; no other thread accesses this var.
        unsafe { std::env::set_var("GIT_CONFIG_GLOBAL", &config_file) };
        f(&config_file);
        // SAFETY: same rationale.
        unsafe { std::env::remove_var("GIT_CONFIG_GLOBAL") };
    }

    /// Set a key directly in a git config file (used for test setup).
    fn git_config_set_file(file: &std::path::Path, key: &str, value: &str) {
        let status = Command::new("git")
            .args(["config", "--file"])
            .arg(file)
            .args([key, value])
            .status()
            .expect("git config set failed");
        assert!(status.success(), "git config set returned non-zero");
    }

    /// Read a key from a specific git config file (used for apply assertions).
    fn git_config_get_file(file: &std::path::Path, key: &str) -> Option<String> {
        Command::new("git")
            .args(["config", "--file"])
            .arg(file)
            .args(["--get", key])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| cfgd_core::stdout_lossy_trimmed(&o))
    }

    // ---------------------------------------------------------------------------
    // Pure unit tests — no filesystem interaction
    // ---------------------------------------------------------------------------

    #[test]
    fn test_name() {
        assert_eq!(GitConfigurator.name(), "git");
    }

    #[test]
    fn test_is_available() {
        let expected = cfgd_core::command_available("git");
        assert_eq!(GitConfigurator.is_available(), expected);
    }

    #[test]
    fn test_value_to_git_string_bool_true() {
        let v = serde_yaml::Value::Bool(true);
        assert_eq!(value_to_git_string(&v), "true");
    }

    #[test]
    fn test_value_to_git_string_bool_false() {
        let v = serde_yaml::Value::Bool(false);
        assert_eq!(value_to_git_string(&v), "false");
    }

    #[test]
    fn test_value_to_git_string_number() {
        let v: serde_yaml::Value = serde_yaml::from_str("42").unwrap();
        assert_eq!(value_to_git_string(&v), "42");
    }

    #[test]
    fn test_value_to_git_string_string() {
        let v = serde_yaml::Value::String("Jane Doe".to_string());
        assert_eq!(value_to_git_string(&v), "Jane Doe");
    }

    #[test]
    fn test_diff_returns_empty_for_non_mapping() {
        let desired = serde_yaml::Value::Null;
        let drifts = GitConfigurator.diff(&desired).unwrap();
        assert!(drifts.is_empty());
    }

    // ---------------------------------------------------------------------------
    // Integration tests — isolated via GIT_CONFIG_GLOBAL + ENV_MUTEX
    // ---------------------------------------------------------------------------

    #[test]
    fn test_diff_detects_missing_key() {
        with_temp_global_config(|_config_file| {
            let desired: serde_yaml::Value = serde_yaml::from_str("user.name: Jane Doe").unwrap();
            let drifts = GitConfigurator.diff(&desired).unwrap();
            assert_eq!(drifts.len(), 1);
            assert_eq!(drifts[0].key, "user.name");
            assert_eq!(drifts[0].expected, "Jane Doe");
            assert_eq!(drifts[0].actual, "");
        });
    }

    #[test]
    fn test_diff_detects_wrong_value() {
        with_temp_global_config(|config_file| {
            git_config_set_file(config_file, "user.name", "Wrong Name");

            let desired: serde_yaml::Value = serde_yaml::from_str("user.name: Jane Doe").unwrap();
            let drifts = GitConfigurator.diff(&desired).unwrap();
            assert_eq!(drifts.len(), 1);
            assert_eq!(drifts[0].key, "user.name");
            assert_eq!(drifts[0].expected, "Jane Doe");
            assert_eq!(drifts[0].actual, "Wrong Name");
        });
    }

    #[test]
    fn test_diff_empty_when_value_matches() {
        with_temp_global_config(|config_file| {
            git_config_set_file(config_file, "user.name", "Jane Doe");

            let desired: serde_yaml::Value = serde_yaml::from_str("user.name: Jane Doe").unwrap();
            let drifts = GitConfigurator.diff(&desired).unwrap();
            assert!(drifts.is_empty());
        });
    }

    #[test]
    fn test_apply_sets_key() {
        with_temp_global_config(|config_file| {
            let desired: serde_yaml::Value =
                serde_yaml::from_str("user.email: jane@work.com").unwrap();
            let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Normal);
            GitConfigurator.apply(&desired, &printer).unwrap();

            let actual = git_config_get_file(config_file, "user.email");
            assert_eq!(actual.as_deref(), Some("jane@work.com"));
        });
    }

    #[test]
    fn test_apply_handles_bool_value() {
        with_temp_global_config(|config_file| {
            let desired: serde_yaml::Value = serde_yaml::from_str("commit.gpgSign: true").unwrap();
            let printer = cfgd_core::output::Printer::new(cfgd_core::output::Verbosity::Normal);
            GitConfigurator.apply(&desired, &printer).unwrap();

            let actual = git_config_get_file(config_file, "commit.gpgSign");
            assert_eq!(actual.as_deref(), Some("true"));
        });
    }
}
