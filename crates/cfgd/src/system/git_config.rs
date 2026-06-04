use cfgd_core::errors::{CfgdError, Result};
use cfgd_core::output::{Printer, Role};
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

/// GitConfigurator — manages `git config --global` settings declaratively.
///
/// Keys may be written either as flat dotted strings that map 1:1 to git config
/// keys, or as nested YAML mappings that cfgd flattens to the same dotted form.
/// Both styles may be mixed freely in the same `git:` block:
///
/// ```yaml
/// system:
///   git:
///     # flat dotted form
///     user.name: "Jane Doe"
///     user.email: jane@work.com
///     init.defaultBranch: main
///     # nested form — flattened to push.autoSetupRemote / push.default
///     push:
///       autoSetupRemote: true
///       default: simple
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
    cfgd_core::git_cmd_local()
        .arg("config")
        .args(&loc)
        .args(["--get", key])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| cfgd_core::stdout_lossy_trimmed(&o))
}

/// Convert a YAML scalar to the string git config expects.
///
/// - `Bool`   → `"true"` / `"false"` (git's canonical form)
/// - `Number` → decimal string
/// - `String` → as-is
///
/// Both call sites (`diff` and `apply`) gate on `is_git_scalar` before calling
/// this, so a non-scalar value (nested mapping, sequence, null) never reaches
/// here — mappings are flattened and sequence/null leaves are skipped with a
/// warning by the caller. The `_` arm is therefore an unreachable defensive
/// default, not a path real input takes.
fn value_to_git_string(value: &serde_yaml::Value) -> String {
    match value {
        serde_yaml::Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::String(s) => s.clone(),
        _ => format!("{value:?}"),
    }
}

/// Recursively flatten a desired git mapping into `(dotted_key, leaf_value)`
/// pairs. Nested mappings join their keys with `.` (`push: { default: simple }`
/// → `push.default`); flat dotted keys (`push.default`) pass through unchanged,
/// so the two forms — and any mix of them — collapse to the same flat list.
///
/// Mappings are always recursed, so every yielded leaf is a non-mapping value.
/// Scalars (`Bool` / `Number` / `String`) are git-storable; a `Sequence` or
/// `Null` leaf is NOT and is the caller's responsibility to reject rather than
/// emit a `Debug` string.
fn flatten_git_keys(mapping: &serde_yaml::Mapping) -> Vec<(String, &serde_yaml::Value)> {
    let mut out = Vec::new();
    collect_git_keys(mapping, None, &mut out);
    out
}

fn collect_git_keys<'a>(
    mapping: &'a serde_yaml::Mapping,
    prefix: Option<&str>,
    out: &mut Vec<(String, &'a serde_yaml::Value)>,
) {
    for (key, value) in mapping {
        let key_str = match key.as_str() {
            Some(k) => k,
            None => continue,
        };
        let dotted = match prefix {
            Some(p) => format!("{p}.{key_str}"),
            None => key_str.to_string(),
        };
        match value {
            serde_yaml::Value::Mapping(child) => collect_git_keys(child, Some(&dotted), out),
            leaf => out.push((dotted, leaf)),
        }
    }
}

/// Whether a flattened leaf is a scalar git can store (`Bool` / `Number` /
/// `String`). A `Sequence` or `Null` is not and must be skipped rather than
/// Debug-encoded.
fn is_git_scalar(value: &serde_yaml::Value) -> bool {
    matches!(
        value,
        serde_yaml::Value::Bool(_) | serde_yaml::Value::Number(_) | serde_yaml::Value::String(_)
    )
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

        for (key, desired_val) in flatten_git_keys(mapping) {
            // A non-scalar leaf (Sequence/Null) is not git-storable; apply()
            // warns on it, so diff() must agree by ignoring it rather than
            // reporting phantom drift against a Debug-encoded value.
            if !is_git_scalar(desired_val) {
                continue;
            }

            let desired_str = value_to_git_string(desired_val);
            let actual_str = git_config_get(&key).unwrap_or_default();

            if actual_str != desired_str {
                drifts.push(SystemDrift {
                    key,
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

        for (key, desired_val) in flatten_git_keys(mapping) {
            // A non-scalar leaf (Sequence/Null) has no git-storable form; skip
            // it with a warning naming the key rather than writing a Debug
            // string git would store verbatim.
            if !is_git_scalar(desired_val) {
                printer.status_simple(
                    Role::Warn,
                    format!("git config {}: non-scalar value ignored", key),
                );
                continue;
            }

            let desired_str = value_to_git_string(desired_val);

            printer.status_simple(
                Role::Info,
                format!("git config --global {} {}", key, desired_str),
            );

            let output = cfgd_core::git_cmd_local()
                .arg("config")
                .args(&loc)
                .args([key.as_str(), &desired_str])
                .output()
                .map_err(CfgdError::Io)?;

            if !output.status.success() {
                printer.status_simple(
                    Role::Warn,
                    format!(
                        "git config --global {} failed: {}",
                        key,
                        cfgd_core::stderr_lossy_trimmed(&output)
                    ),
                );
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
        let status = cfgd_core::git_cmd_local()
            .args(["config", "--file"])
            .arg(file)
            .args([key, value])
            .status()
            .expect("git config set failed");
        assert!(status.success(), "git config set returned non-zero");
    }

    /// Read a key from a specific git config file (used for apply assertions).
    fn git_config_get_file(file: &std::path::Path, key: &str) -> Option<String> {
        cfgd_core::git_cmd_local()
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
    fn value_to_git_string_conversions() {
        let cases: &[(serde_yaml::Value, &str)] = &[
            (serde_yaml::Value::Bool(true), "true"),
            (serde_yaml::Value::Bool(false), "false"),
            (serde_yaml::from_str("42").unwrap(), "42"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                value_to_git_string(input),
                *expected,
                "failed for {:?}",
                input
            );
        }
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
            let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
            GitConfigurator.apply(&desired, &printer).unwrap();

            let actual = git_config_get_file(config_file, "user.email");
            assert_eq!(actual.as_deref(), Some("jane@work.com"));
        });
    }

    #[test]
    fn test_apply_handles_bool_value() {
        with_temp_global_config(|config_file| {
            let desired: serde_yaml::Value = serde_yaml::from_str("commit.gpgSign: true").unwrap();
            let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
            GitConfigurator.apply(&desired, &printer).unwrap();

            let actual = git_config_get_file(config_file, "commit.gpgSign");
            assert_eq!(actual.as_deref(), Some("true"));
        });
    }

    // ---------------------------------------------------------------------------
    // Nested-key flattening (PART 1)
    // ---------------------------------------------------------------------------

    #[test]
    fn flatten_git_keys_handles_flat_nested_and_deep() {
        let desired: serde_yaml::Value = serde_yaml::from_str(
            "user.name: Jane\npush:\n  autoSetupRemote: true\n  default: simple\na:\n  b:\n    c: x\n",
        )
        .unwrap();
        let mapping = desired.as_mapping().unwrap();
        let mut flat = flatten_git_keys(mapping);
        flat.sort_by(|a, b| a.0.cmp(&b.0));

        let keys: Vec<&str> = flat.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(
            keys,
            ["a.b.c", "push.autoSetupRemote", "push.default", "user.name"]
        );
        // Every yielded leaf is a non-mapping scalar.
        for (_, v) in &flat {
            assert!(is_git_scalar(v), "leaf should be a git scalar: {v:?}");
        }
    }

    #[test]
    fn test_nested_form_diffs_identically_to_flat() {
        with_temp_global_config(|_config_file| {
            let nested: serde_yaml::Value =
                serde_yaml::from_str("push:\n  autoSetupRemote: true\n  default: simple\n")
                    .unwrap();
            let flat: serde_yaml::Value =
                serde_yaml::from_str("push.autoSetupRemote: true\npush.default: simple\n").unwrap();

            let mut nested_drift = GitConfigurator.diff(&nested).unwrap();
            let mut flat_drift = GitConfigurator.diff(&flat).unwrap();
            nested_drift.sort_by(|a, b| a.key.cmp(&b.key));
            flat_drift.sort_by(|a, b| a.key.cmp(&b.key));

            assert_eq!(nested_drift.len(), 2);
            assert_eq!(
                nested_drift.iter().map(|d| &d.key).collect::<Vec<_>>(),
                flat_drift.iter().map(|d| &d.key).collect::<Vec<_>>()
            );
            assert_eq!(nested_drift[0].key, "push.autoSetupRemote");
            assert_eq!(nested_drift[0].expected, "true");
            assert_eq!(nested_drift[1].key, "push.default");
            assert_eq!(nested_drift[1].expected, "simple");
        });
    }

    #[test]
    fn test_nested_form_applies_identically_to_flat() {
        with_temp_global_config(|config_file| {
            let nested: serde_yaml::Value =
                serde_yaml::from_str("push:\n  autoSetupRemote: true\n  default: simple\n")
                    .unwrap();
            let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
            GitConfigurator.apply(&nested, &printer).unwrap();

            assert_eq!(
                git_config_get_file(config_file, "push.autoSetupRemote").as_deref(),
                Some("true")
            );
            assert_eq!(
                git_config_get_file(config_file, "push.default").as_deref(),
                Some("simple")
            );
        });
    }

    #[test]
    fn test_mixed_flat_and_nested_combine() {
        with_temp_global_config(|config_file| {
            let desired: serde_yaml::Value =
                serde_yaml::from_str("user.name: Jane Doe\npush:\n  autoSetupRemote: true\n")
                    .unwrap();
            let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
            GitConfigurator.apply(&desired, &printer).unwrap();

            assert_eq!(
                git_config_get_file(config_file, "user.name").as_deref(),
                Some("Jane Doe")
            );
            assert_eq!(
                git_config_get_file(config_file, "push.autoSetupRemote").as_deref(),
                Some("true")
            );
        });
    }

    #[test]
    fn test_deeply_nested_flattens_to_dotted() {
        with_temp_global_config(|config_file| {
            let desired: serde_yaml::Value = serde_yaml::from_str("a:\n  b:\n    c: x\n").unwrap();
            let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
            GitConfigurator.apply(&desired, &printer).unwrap();

            assert_eq!(
                git_config_get_file(config_file, "a.b.c").as_deref(),
                Some("x")
            );
        });
    }

    #[test]
    fn test_sequence_leaf_is_skipped_with_warning_not_debug_encoded() {
        with_temp_global_config(|config_file| {
            // A sequence-valued leaf is not git-storable; apply must skip it with
            // a warning naming the key, never write a Debug string.
            let desired: serde_yaml::Value =
                serde_yaml::from_str("custom:\n  list:\n    - a\n    - b\n").unwrap();
            let (printer, doc) = cfgd_core::output::Printer::for_test_doc();
            GitConfigurator.apply(&desired, &printer).unwrap();

            // Nothing was written for the non-scalar key.
            assert_eq!(git_config_get_file(config_file, "custom.list"), None);

            let out = cfgd_core::output::strip_ansi(&doc.human());
            assert!(
                out.contains("custom.list") && out.contains("non-scalar"),
                "expected a warning naming the skipped key, got: {out}"
            );
            assert!(
                !out.contains("Sequence"),
                "must not Debug-encode the sequence value, got: {out}"
            );
        });
    }

    #[test]
    fn test_sequence_leaf_skipped_in_diff() {
        with_temp_global_config(|_config_file| {
            let desired: serde_yaml::Value =
                serde_yaml::from_str("custom:\n  list:\n    - a\n    - b\n").unwrap();
            let drifts = GitConfigurator.diff(&desired).unwrap();
            assert!(
                drifts.is_empty(),
                "non-scalar leaf must not produce drift, got {} entries",
                drifts.len()
            );
        });
    }

    #[test]
    fn test_empty_nested_map_is_a_silent_noop() {
        with_temp_global_config(|config_file| {
            // An empty nested map yields no leaves: it must produce zero drift,
            // write nothing, and emit no warning (it is not a non-scalar leaf —
            // it is recursed into and simply contributes nothing).
            let desired: serde_yaml::Value = serde_yaml::from_str("push: {}\n").unwrap();

            let drifts = GitConfigurator.diff(&desired).unwrap();
            assert!(
                drifts.is_empty(),
                "empty nested map must not produce drift, got {} entries",
                drifts.len()
            );

            let (printer, doc) = cfgd_core::output::Printer::for_test_doc();
            GitConfigurator.apply(&desired, &printer).unwrap();

            // Nothing written under the empty section.
            assert_eq!(git_config_get_file(config_file, "push.default"), None);

            let out = cfgd_core::output::strip_ansi(&doc.human());
            assert!(
                !out.contains("non-scalar") && !out.contains("git config"),
                "empty nested map must be a silent no-op, got: {out}"
            );
        });
    }
}
