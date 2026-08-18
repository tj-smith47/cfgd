use cfgd_core::errors::{CfgdError, Result};
use cfgd_core::output::Role;
use cfgd_core::providers::{SystemConfigurator, SystemContext, SystemDrift};

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

/// Read every key at the configured location in one spawn.
///
/// `git config --list -z` emits `key\nvalue\0` records (and a bare `key\0` for
/// a valueless boolean), so a value carrying newlines — an alias body — is
/// unambiguous in a way `--list`'s line format is not. It reads the same
/// location `apply` writes, includes and all, and a repeated key keeps its LAST
/// value, which is what `git config --get` answers with.
fn git_config_snapshot() -> std::collections::HashMap<String, String> {
    let loc = git_location_args();
    let mut cmd = cfgd_core::git_cmd_local();
    cmd.arg("config").args(&loc).args(["--list", "-z"]);
    let output = cfgd_core::command_output_with_timeout(&mut cmd, cfgd_core::COMMAND_TIMEOUT)
        .ok()
        .filter(|o| o.status.success());
    match output {
        Some(o) => parse_config_list(&String::from_utf8_lossy(&o.stdout)),
        None => std::collections::HashMap::new(),
    }
}

fn parse_config_list(dump: &str) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for record in dump.split('\0').filter(|r| !r.is_empty()) {
        let (key, value) = match record.split_once('\n') {
            Some((k, v)) => (k, v),
            None => (record, ""),
        };
        map.insert(canonical_git_key(key), value.trim().to_string());
    }
    map
}

/// Fold a git config key to the spelling `--list` prints.
///
/// git matches the section and the variable case-INsensitively and a subsection
/// case-SENSITIVELY (`remote.Origin.url` and `remote.origin.url` are two keys),
/// so only the first and last segments are lowered and everything between is
/// left exactly as written. Folding the whole key would answer a declared
/// `remote.Origin.url` with a different remote's URL.
fn canonical_git_key(key: &str) -> String {
    let segments: Vec<&str> = key.split('.').collect();
    match segments.len() {
        0 | 1 => key.to_lowercase(),
        2 => key.to_lowercase(),
        _ => {
            let first = segments[0].to_lowercase();
            let last = segments[segments.len() - 1].to_lowercase();
            let middle = segments[1..segments.len() - 1].join(".");
            format!("{first}.{middle}.{last}")
        }
    }
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

        let flattened = flatten_git_keys(mapping);
        if flattened.is_empty() {
            // Nothing declared, so the listing's answer would be discarded.
            return Ok(Vec::new());
        }

        let mut drifts = Vec::new();
        // One `git config --list` for every declared key, instead of one
        // `git config --get` per key.
        let snapshot = git_config_snapshot();

        for (key, desired_val) in flattened {
            // A non-scalar leaf (Sequence/Null) is not git-storable; apply()
            // warns on it, so diff() must agree by ignoring it rather than
            // reporting phantom drift against a Debug-encoded value.
            if !is_git_scalar(desired_val) {
                continue;
            }

            let desired_str = value_to_git_string(desired_val);
            let actual_str = snapshot
                .get(&canonical_git_key(&key))
                .cloned()
                .unwrap_or_default();

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

    fn apply(&self, desired: &serde_yaml::Value, cx: &SystemContext<'_>) -> Result<()> {
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
                cx.report(
                    Role::Warn,
                    format!("git config {}: non-scalar value ignored", key),
                );
                continue;
            }

            let desired_str = value_to_git_string(desired_val);

            cx.report(
                Role::Info,
                format!("git config --global {} {}", key, desired_str),
            );

            let mut cmd = cfgd_core::git_cmd_local();
            cmd.arg("config")
                .args(&loc)
                .args([key.as_str(), &desired_str]);
            let output =
                cfgd_core::command_output_with_timeout(&mut cmd, cfgd_core::COMMAND_TIMEOUT)
                    .map_err(CfgdError::Io)?;

            if !output.status.success() {
                cx.report(
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
        let mut cmd = cfgd_core::git_cmd_local();
        cmd.args(["config", "--file"]).arg(file).args([key, value]);
        let output = cfgd_core::command_output_with_timeout(&mut cmd, cfgd_core::COMMAND_TIMEOUT)
            .expect("git config set failed");
        assert!(output.status.success(), "git config set returned non-zero");
    }

    /// Read a key from a specific git config file (used for apply assertions).
    fn git_config_get_file(file: &std::path::Path, key: &str) -> Option<String> {
        let mut cmd = cfgd_core::git_cmd_local();
        cmd.args(["config", "--file"])
            .arg(file)
            .args(["--get", key]);
        cfgd_core::command_output_with_timeout(&mut cmd, cfgd_core::COMMAND_TIMEOUT)
            .ok()
            .filter(|o| o.status.success())
            .map(|o| cfgd_core::stdout_lossy_trimmed(&o))
    }

    // ---------------------------------------------------------------------------
    // Pure unit tests — no filesystem interaction
    // ---------------------------------------------------------------------------

    /// Verbatim `git config --file <f> --list -z` output (git 2.51) for a file
    /// carrying a subsection, a repeated key, a multi-line alias body and a
    /// valueless boolean. Every expectation below is the same file's
    /// `git config --get <key>` output.
    const REAL_LIST_Z: &str = "user.name\nJane Doe\0\
        push.autosetupremote\ntrue\0\
        remote.Origin.url\nhttps://a/b\0\
        core.gitproxy\none\0\
        core.gitproxy\ntwo\0\
        alias.multi\n!f() {\n echo hi\n}; f\0\
        core.bare\0";

    #[test]
    fn snapshot_answers_exactly_what_git_config_get_answers() {
        let snapshot = parse_config_list(REAL_LIST_Z);
        for (key, expected) in [
            ("user.name", "Jane Doe"),
            ("push.autosetupremote", "true"),
            ("remote.Origin.url", "https://a/b"),
            // `--get` of a repeated key answers with the LAST value.
            ("core.gitproxy", "two"),
            // A multi-line value: `-z` keeps it in one record, so the newlines
            // inside the alias body are the value's own.
            ("alias.multi", "!f() {\n echo hi\n}; f"),
            // A valueless boolean prints an empty line from `--get`.
            ("core.bare", ""),
        ] {
            assert_eq!(
                snapshot.get(key).map(String::as_str),
                Some(expected),
                "listing and `--get` disagree about {key}"
            );
        }
    }

    #[test]
    fn a_declared_key_folds_to_the_spelling_the_listing_prints() {
        let snapshot = parse_config_list(REAL_LIST_Z);
        // git matches section and variable case-insensitively, so the declared
        // spelling has to reach the listing's lowercased one.
        assert_eq!(
            snapshot
                .get(&canonical_git_key("push.autoSetupRemote"))
                .map(String::as_str),
            Some("true")
        );
        // A subsection is case-SENSITIVE: two spellings are two keys.
        assert_eq!(
            snapshot
                .get(&canonical_git_key("remote.Origin.URL"))
                .map(String::as_str),
            Some("https://a/b")
        );
        assert_eq!(snapshot.get(&canonical_git_key("remote.origin.url")), None);
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn git_diff_lists_the_config_once_however_many_keys_it_declares() {
        let _guard = ENV_MUTEX.lock().unwrap();
        // The shim answers nothing, so every declared key drifts against the
        // same empty string a missing key produced before — what the test is
        // about is how many times git ran.
        let (_bin, _path, log) =
            cfgd_core::test_helpers::install_named_path_shim_logged("git", 0, "", "");
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            "user.name: Jane Doe\nuser.email: jane@work.com\npush:\n  autoSetupRemote: true\n",
        )
        .unwrap();

        let drifts = GitConfigurator.diff(&yaml).unwrap();

        // Reading the WHOLE log is safe here, unlike the env-seam shims: this
        // one is a PATH shim, and `install_named_path_shim_logged` holds the
        // exclusive `path_env_mutation_guard` for its lifetime, so no other
        // test can spawn through it while this assertion is being set up.
        assert_eq!(
            log.argv_log().lines().collect::<Vec<_>>(),
            vec!["config --global --list -z"],
            "one listing answers every declared key"
        );
        assert_eq!(drifts.len(), 3, "unexpected drifts: {drifts:?}");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn git_diff_of_an_empty_mapping_lists_nothing() {
        let _guard = ENV_MUTEX.lock().unwrap();
        let (_bin, _path, log) =
            cfgd_core::test_helpers::install_named_path_shim_logged("git", 0, "", "");
        let yaml: serde_yaml::Value = serde_yaml::from_str("push: {}\n").unwrap();

        let drifts = GitConfigurator.diff(&yaml).unwrap();

        assert!(drifts.is_empty());
        assert_eq!(
            log.argv_log(),
            "",
            "a profile declaring no git keys spawns nothing"
        );
    }

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
            GitConfigurator
                .apply(
                    &desired,
                    &cfgd_core::providers::SystemContext::new(&printer),
                )
                .unwrap();

            let actual = git_config_get_file(config_file, "user.email");
            assert_eq!(actual.as_deref(), Some("jane@work.com"));
        });
    }

    #[test]
    fn test_apply_handles_bool_value() {
        with_temp_global_config(|config_file| {
            let desired: serde_yaml::Value = serde_yaml::from_str("commit.gpgSign: true").unwrap();
            let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
            GitConfigurator
                .apply(
                    &desired,
                    &cfgd_core::providers::SystemContext::new(&printer),
                )
                .unwrap();

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
            GitConfigurator
                .apply(&nested, &cfgd_core::providers::SystemContext::new(&printer))
                .unwrap();

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
            GitConfigurator
                .apply(
                    &desired,
                    &cfgd_core::providers::SystemContext::new(&printer),
                )
                .unwrap();

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
            GitConfigurator
                .apply(
                    &desired,
                    &cfgd_core::providers::SystemContext::new(&printer),
                )
                .unwrap();

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
            GitConfigurator
                .apply(
                    &desired,
                    &cfgd_core::providers::SystemContext::new(&printer),
                )
                .unwrap();

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
            GitConfigurator
                .apply(
                    &desired,
                    &cfgd_core::providers::SystemContext::new(&printer),
                )
                .unwrap();

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
