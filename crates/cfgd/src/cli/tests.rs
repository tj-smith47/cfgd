use super::*;
use std::sync::{Arc, Mutex};

const TEST_CONFIG_YAML: &str =
    "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n";

const DEFAULT_PROFILE_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  env:
    - name: editor
      value: vim
  packages:
    cargo:
      - bat
"#;

const WORK_PROFILE_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - default
  env:
    - name: editor
      value: code
  packages:
    cargo:
      - exa
"#;

const SIMPLE_MODULE_YAML: &str = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages: []\n";

const RICH_CONFIG_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: my-machine
spec:
  profile: default
  sources:
    - name: team-config
      origin:
        url: https://github.com/team/config
        branch: main
        type: Git
      subscription:
        priority: 100
  daemon:
    enabled: true
    reconcile:
      interval: 5m
      onChange: true
      autoApply: false
    sync:
      interval: 10m
  secrets:
    backend: age
"#;

// -----------------------------------------------------------------------
// CliTestHarness — builder for isolated CLI test environments
// -----------------------------------------------------------------------

struct CliTestHarnessBuilder {
    config_yaml: String,
    profiles: Vec<(String, String)>,
    modules: Vec<(String, String)>,
    output_format: cfgd_core::output::OutputFormat,
}

impl CliTestHarnessBuilder {
    fn new() -> Self {
        Self {
            config_yaml: TEST_CONFIG_YAML.to_string(),
            profiles: vec![
                ("default.yaml".into(), DEFAULT_PROFILE_YAML.into()),
                ("work.yaml".into(), WORK_PROFILE_YAML.into()),
            ],
            modules: Vec::new(),
            output_format: cfgd_core::output::OutputFormat::Table,
        }
    }

    fn config(mut self, yaml: &str) -> Self {
        self.config_yaml = yaml.to_string();
        self
    }

    fn rich_config(self) -> Self {
        self.config(RICH_CONFIG_YAML)
    }

    fn profile(mut self, name: &str, content: &str) -> Self {
        self.profiles
            .push((format!("{name}.yaml"), content.to_string()));
        self
    }

    fn module(mut self, name: &str, content: &str) -> Self {
        self.modules.push((name.to_string(), content.to_string()));
        self
    }

    fn json(mut self) -> Self {
        self.output_format = cfgd_core::output::OutputFormat::Json;
        self
    }

    fn build(self) -> CliTestHarness {
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let cache_dir = tempfile::tempdir().unwrap();

        std::fs::write(config_dir.path().join("cfgd.yaml"), &self.config_yaml).unwrap();

        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        for (name, content) in &self.profiles {
            std::fs::write(profiles_dir.join(name), content).unwrap();
        }

        let modules_dir = config_dir.path().join("modules");
        std::fs::create_dir_all(&modules_dir).unwrap();
        for (name, content) in &self.modules {
            let mod_dir = modules_dir.join(name);
            std::fs::create_dir_all(mod_dir.join("files")).unwrap();
            std::fs::write(mod_dir.join("module.yaml"), content).unwrap();
        }

        // For human formats, use Normal verbosity so tests can assert on
        // rendered output (Quiet would suppress headings/sections). Structured
        // formats route through `for_test_with_format`, which auto-quiets.
        let (printer, buf) = if self.output_format == cfgd_core::output::OutputFormat::Table {
            cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal)
        } else {
            cfgd_core::output::Printer::for_test_with_format(self.output_format.clone())
        };

        CliTestHarness {
            config_dir,
            state_dir,
            cache_dir,
            printer,
            buf,
            output_format: self.output_format,
        }
    }
}

struct CliTestHarness {
    config_dir: tempfile::TempDir,
    state_dir: tempfile::TempDir,
    cache_dir: tempfile::TempDir,
    printer: cfgd_core::output::Printer,
    buf: Arc<Mutex<String>>,
    output_format: cfgd_core::output::OutputFormat,
}

impl CliTestHarness {
    fn builder() -> CliTestHarnessBuilder {
        CliTestHarnessBuilder::new()
    }

    fn cli(&self) -> Cli {
        Cli {
            config: self.config_dir.path().join("cfgd.yaml"),
            config_explicit: false,
            profile: None,
            no_color: true,
            verbose: 0,
            quiet: true,
            output: OutputFormatArg(self.output_format.clone()),
            list_envelope: false,
            jsonpath: None,
            state_dir: Some(self.state_dir.path().to_path_buf()),
            config_dir: None,
            cache_dir: Some(self.cache_dir.path().to_path_buf()),
            runtime_dir: None,
            scope_arg: crate::cli::ScopeArg::User,
            command: Some(Command::Status {
                module: None,
                exit_code: false,
            }),
        }
    }

    fn cli_with_command(&self, command: Command) -> Cli {
        Cli {
            command: Some(command),
            ..self.cli()
        }
    }

    fn printer(&self) -> &cfgd_core::output::Printer {
        &self.printer
    }

    fn config_path(&self) -> &Path {
        self.config_dir.path()
    }

    fn state_path(&self) -> &Path {
        self.state_dir.path()
    }

    fn output(&self) -> String {
        self.printer.flush();
        self.buf.lock().unwrap().clone()
    }

    fn json_output(&self) -> serde_json::Value {
        extract_json(&self.output())
    }

    fn assert_output_contains(&self, expected: &str) {
        let output = self.output();
        assert!(
            output.contains(expected),
            "expected output to contain '{expected}', got:\n{output}"
        );
    }

    fn assert_header(&self, header: &str) {
        self.assert_output_contains(header);
    }
}

// -----------------------------------------------------------------------
// Free-standing assertion helpers
// -----------------------------------------------------------------------

fn assert_json_has_fields(json: &serde_json::Value, fields: &[&str]) {
    for field in fields {
        assert!(
            json.get(*field).is_some(),
            "JSON missing required field '{field}', got: {json}"
        );
    }
}

fn assert_json_field_type(json: &serde_json::Value, field: &str, type_name: &str) {
    let val = json
        .get(field)
        .unwrap_or_else(|| panic!("JSON missing field '{field}', got: {json}"));
    let matches = match type_name {
        "string" => val.is_string(),
        "number" | "u64" | "i64" | "f64" => val.is_number(),
        "bool" | "boolean" => val.is_boolean(),
        "array" => val.is_array(),
        "object" => val.is_object(),
        "null" => val.is_null(),
        _ => panic!("unknown type check '{type_name}'"),
    };
    assert!(matches, "expected '{field}' to be {type_name}, got: {val}");
}

fn assert_error_contains(result: &anyhow::Result<()>, expected: &str) {
    match result {
        Ok(_) => panic!("expected error containing '{expected}', but got Ok"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains(expected),
                "expected error to contain '{expected}', got: {msg}"
            );
        }
    }
}

fn create_test_config_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();

    // Create profiles directory with a test profile
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();

    std::fs::write(
        profiles_dir.join("default.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  env:
    - name: editor
      value: vim
  packages:
    cargo:
      - bat
"#,
    )
    .unwrap();

    std::fs::write(
        profiles_dir.join("work.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: work
spec:
  inherits:
    - default
  env:
    - name: editor
      value: code
  packages:
    cargo:
      - exa
"#,
    )
    .unwrap();

    dir
}

#[test]
fn cli_has_output_flag() {
    use clap::CommandFactory;
    let cmd = Cli::command();
    assert!(
        cmd.get_arguments().any(|a| a.get_id() == "output"),
        "should have global --output flag"
    );
}

#[test]
fn cli_has_jsonpath_flag() {
    use clap::CommandFactory;
    let cmd = Cli::command();
    assert!(
        cmd.get_arguments().any(|a| a.get_id() == "jsonpath"),
        "should have global --jsonpath flag"
    );
}

#[test]
fn cli_output_flag_has_short_alias() {
    use clap::CommandFactory;
    let cmd = Cli::command();
    let output_arg = cmd
        .get_arguments()
        .find(|a| a.get_id() == "output")
        .unwrap();
    assert_eq!(
        output_arg.get_short(),
        Some('o'),
        "--output should have -o short alias"
    );
}

#[test]
fn cli_init_has_apply_flag() {
    use clap::CommandFactory;
    let cmd = Cli::command();
    let init_cmd = cmd
        .get_subcommands()
        .find(|c| c.get_name() == "init")
        .unwrap();
    assert!(
        init_cmd.get_arguments().any(|a| a.get_id() == "apply"),
        "init should have --apply flag"
    );
    assert!(
        init_cmd
            .get_arguments()
            .any(|a| a.get_id() == "install_daemon"),
        "init should have --install-daemon flag"
    );
}

#[test]
fn cli_daemon_has_subcommands() {
    use clap::CommandFactory;
    let cmd = Cli::command();
    let daemon_cmd = cmd
        .get_subcommands()
        .find(|c| c.get_name() == "daemon")
        .unwrap();
    let subcommands: Vec<&str> = daemon_cmd.get_subcommands().map(|c| c.get_name()).collect();
    assert!(
        subcommands.contains(&"run"),
        "daemon should have run subcommand"
    );
    assert!(
        subcommands.contains(&"install"),
        "daemon should have install subcommand"
    );
    assert!(
        subcommands.contains(&"uninstall"),
        "daemon should have uninstall subcommand"
    );
    assert!(
        subcommands.contains(&"status"),
        "daemon should have status subcommand"
    );
}

#[test]
fn cli_has_source_subcommand() {
    use clap::CommandFactory;
    let cmd = Cli::command();
    let source_cmd = cmd.get_subcommands().find(|c| c.get_name() == "source");
    assert!(source_cmd.is_some(), "source subcommand should exist");

    let source_cmd = source_cmd.unwrap();
    let subcommands: Vec<&str> = source_cmd.get_subcommands().map(|c| c.get_name()).collect();
    assert!(subcommands.contains(&"add"));
    assert!(subcommands.contains(&"list"));
    assert!(subcommands.contains(&"show"));
    assert!(subcommands.contains(&"remove"));
    assert!(subcommands.contains(&"update"));
    assert!(subcommands.contains(&"override"));
    assert!(subcommands.contains(&"priority"));
    assert!(subcommands.contains(&"replace"));
}

#[test]
fn cli_has_alias_subcommand() {
    use clap::CommandFactory;
    let cmd = Cli::command();
    let alias_cmd = cmd.get_subcommands().find(|c| c.get_name() == "alias");
    assert!(alias_cmd.is_some(), "alias subcommand should exist");

    let alias_cmd = alias_cmd.unwrap();
    let subnames: Vec<&str> = alias_cmd.get_subcommands().map(|c| c.get_name()).collect();
    assert!(subnames.contains(&"set"), "missing 'set': {subnames:?}");
    assert!(
        subnames.contains(&"delete"),
        "missing 'delete': {subnames:?}"
    );
    assert!(subnames.contains(&"list"), "missing 'list': {subnames:?}");
    assert!(subnames.contains(&"show"), "missing 'show': {subnames:?}");

    // Pin the clap-level aliases on the canonical subcommand definitions —
    // each alias is part of the contractual CLI surface (mc-style ergonomic
    // entry points), not just sugar.
    let find_sub = |name: &str| {
        alias_cmd
            .get_subcommands()
            .find(|s| s.get_name() == name)
            .unwrap_or_else(|| panic!("alias '{name}' subcommand missing"))
    };
    let set_aliases: Vec<&str> = find_sub("set").get_all_aliases().collect();
    assert!(
        set_aliases.contains(&"add"),
        "'set' missing 'add' alias: {set_aliases:?}"
    );
    let delete_aliases: Vec<&str> = find_sub("delete").get_all_aliases().collect();
    assert!(
        delete_aliases.contains(&"rm"),
        "'delete' missing 'rm' alias: {delete_aliases:?}"
    );
    let list_aliases: Vec<&str> = find_sub("list").get_all_aliases().collect();
    assert!(
        list_aliases.contains(&"ls"),
        "'list' missing 'ls' alias: {list_aliases:?}"
    );

    // Sanity-check both canonical and alias entry points actually parse —
    // catches regressions where the enum variant exists but clap routing
    // breaks (e.g. duplicate alias collision).
    assert!(Cli::try_parse_from(["cfgd", "alias", "set", "n", "v"]).is_ok());
    assert!(Cli::try_parse_from(["cfgd", "alias", "add", "n", "v"]).is_ok());
    assert!(Cli::try_parse_from(["cfgd", "alias", "delete", "n"]).is_ok());
    assert!(Cli::try_parse_from(["cfgd", "alias", "rm", "n"]).is_ok());
    assert!(Cli::try_parse_from(["cfgd", "alias", "list"]).is_ok());
    assert!(Cli::try_parse_from(["cfgd", "alias", "ls"]).is_ok());
    assert!(Cli::try_parse_from(["cfgd", "alias", "show", "n"]).is_ok());
}

#[test]
fn infer_source_name_from_ssh_url() {
    assert_eq!(
        super::infer_source_name("git@github.com:acme-corp/dev-config.git"),
        "acme-corp-dev-config"
    );
}

#[test]
fn infer_source_name_from_https_url() {
    assert_eq!(
        super::infer_source_name("https://github.com/acme/config.git"),
        "config"
    );
}

#[test]
fn count_policy_items_empty() {
    let items = cfgd_core::config::PolicyItems::default();
    assert_eq!(super::count_policy_items(&items), 0);
}

// --- resolve_non_interactive_profile ---

#[test]
fn resolve_non_interactive_profile_explicit_wins_over_everything() {
    // Explicit --profile beats auto-detect, sole option, and prompt.
    let out = super::resolve_non_interactive_profile(
        Some("explicit-pick"),
        Some("auto-pick"),
        &["only-one".to_string()],
    );
    assert_eq!(out.as_deref(), Some("explicit-pick"));
}

#[test]
fn resolve_non_interactive_profile_auto_detected_wins_over_sole_option() {
    // Auto-detected platform profile beats sole-option fallback.
    let out =
        super::resolve_non_interactive_profile(None, Some("ubuntu"), &["only-one".to_string()]);
    assert_eq!(out.as_deref(), Some("ubuntu"));
}

#[test]
fn resolve_non_interactive_profile_sole_option_wins_when_no_higher_signal() {
    // No explicit, no auto-detect, exactly one provided → it gets picked.
    let out = super::resolve_non_interactive_profile(None, None, &["dev".to_string()]);
    assert_eq!(out.as_deref(), Some("dev"));
}

#[test]
fn resolve_non_interactive_profile_returns_none_when_multiple_options() {
    // Multiple provided + no override → caller must prompt.
    let out = super::resolve_non_interactive_profile(
        None,
        None,
        &["dev".to_string(), "prod".to_string(), "ci".to_string()],
    );
    assert!(
        out.is_none(),
        "multi-option case must return None so the caller prompts"
    );
}

#[test]
fn resolve_non_interactive_profile_returns_none_when_no_options_and_no_override() {
    // Nothing to pick from and no override — caller treats as "no profile".
    let out = super::resolve_non_interactive_profile(None, None, &[]);
    assert!(out.is_none());
}

#[test]
fn resolve_non_interactive_profile_explicit_wins_even_over_empty_provided() {
    // Source manifest may declare no profiles, but the user can still
    // pin one via --profile (e.g. for a custom subscription).
    let out = super::resolve_non_interactive_profile(Some("custom"), None, &[]);
    assert_eq!(out.as_deref(), Some("custom"));
}

// --- parse_priority_input ---

#[test]
fn parse_priority_input_accepts_valid_u32() {
    assert_eq!(super::parse_priority_input("42").unwrap(), 42);
    assert_eq!(super::parse_priority_input("0").unwrap(), 0);
    assert_eq!(super::parse_priority_input("1000").unwrap(), 1000);
    assert_eq!(
        super::parse_priority_input(&cfgd_core::config::MAX_SOURCE_PRIORITY.to_string()).unwrap(),
        cfgd_core::config::MAX_SOURCE_PRIORITY,
        "must accept a value at the priority ceiling"
    );
}

#[test]
fn parse_priority_input_rejects_non_numeric_with_canonical_error() {
    let err = super::parse_priority_input("five").unwrap_err().to_string();
    assert!(
        err.contains("invalid priority: 'five'") && err.contains("must be a number"),
        "error must use the canonical CLI wording, got: {err}"
    );
}

#[test]
fn parse_priority_input_rejects_negative_numbers() {
    // u32 has no negative range — pin that contract.
    let err = super::parse_priority_input("-1").unwrap_err().to_string();
    assert!(err.contains("invalid priority: '-1'"));
}

#[test]
fn parse_priority_input_rejects_overflow() {
    // u32::MAX + 1; surfaces as the same canonical error.
    let err = super::parse_priority_input("4294967296")
        .unwrap_err()
        .to_string();
    assert!(err.contains("invalid priority: '4294967296'"));
}

#[test]
fn parse_priority_input_rejects_empty_string() {
    let err = super::parse_priority_input("").unwrap_err().to_string();
    assert!(err.contains("invalid priority: ''"));
}

#[test]
fn parse_priority_input_rejects_whitespace_only() {
    // u32::from_str does not trim; whitespace must error rather than
    // silently succeed at zero.
    let err = super::parse_priority_input("  ").unwrap_err().to_string();
    assert!(err.contains("invalid priority: '  '"));
}

#[test]
fn default_noninteractive_priority_is_midpoint() {
    // Pin the constant so a future "let's bump the default" change is
    // an explicit choice rather than a silent drift.
    assert_eq!(super::DEFAULT_NONINTERACTIVE_PRIORITY, 500);
}

// --- display_source_manifest ---

fn manifest_yaml(extra_spec: &str) -> cfgd_core::config::ConfigSourceDocument {
    let yaml = format!(
        r#"apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: acme-platform
  version: "1.4.0"
  description: "Acme platform baseline"
spec:
{extra_spec}
"#
    );
    serde_yaml::from_str(&yaml).expect("manifest fixture must parse")
}

#[test]
fn display_source_manifest_returns_provided_profiles_in_listed_order() {
    let manifest = manifest_yaml("  provides:\n    profiles: [dev, prod, ci]\n");
    let (printer, _buf) = cfgd_core::output::Printer::for_test();
    let profiles = super::display_source_manifest(&printer, &manifest);
    assert_eq!(profiles, vec!["dev", "prod", "ci"]);
}

#[test]
fn display_source_manifest_emits_metadata_header_kv_lines() {
    let manifest = manifest_yaml("  provides: {}\n");
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    super::display_source_manifest(&printer, &manifest);
    drop(printer);
    let out = buf.lock().unwrap().clone();
    assert!(out.contains("Source Manifest"), "header missing: {out}");
    assert!(
        out.contains("Name") && out.contains("acme-platform"),
        "Name kv missing: {out}"
    );
    assert!(
        out.contains("Version") && out.contains("1.4.0"),
        "Version kv missing: {out}"
    );
    assert!(
        out.contains("Description") && out.contains("Acme platform baseline"),
        "Description kv missing: {out}"
    );
}

#[test]
fn display_source_manifest_omits_profiles_kv_when_empty() {
    // When the manifest provides no profiles, the "Profiles:" key/value
    // line is suppressed entirely (rather than printing an empty value).
    let manifest = manifest_yaml("  provides: {}\n");
    let (printer, buf) = cfgd_core::output::Printer::for_test();
    let profiles = super::display_source_manifest(&printer, &manifest);
    assert!(profiles.is_empty());
    drop(printer);
    let out = buf.lock().unwrap().clone();
    assert!(
        !out.contains("Profiles"),
        "Profiles label must be suppressed when none provided, got: {out}"
    );
}

#[test]
fn display_source_manifest_summarizes_required_recommended_locked_counts() {
    // Each tier with a non-zero count emits a labeled line.
    let manifest = manifest_yaml(
        r#"  provides: {}
  policy:
    required:
      env:
        - name: REQUIRED_VAR
          value: required-value
    recommended:
      env:
        - name: REC_ONE
          value: r1
        - name: REC_TWO
          value: r2
    locked:
      env:
        - name: LOCKED_VAR
          value: locked-value
"#,
    );
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    super::display_source_manifest(&printer, &manifest);
    drop(printer);
    let out = buf.lock().unwrap().clone();
    assert!(out.contains("Policy"), "Policy header missing: {out}");
    assert!(
        out.contains("1 locked item(s)") && out.contains("cannot override"),
        "locked tier line missing: {out}"
    );
    assert!(
        out.contains("1 required item(s)") && out.contains("team requirement"),
        "required tier line missing: {out}"
    );
    assert!(
        out.contains("2 recommended item(s)"),
        "recommended count line missing: {out}"
    );
}

#[test]
fn display_source_manifest_omits_zero_count_tiers() {
    // When a tier has zero items its line must NOT appear.
    let manifest = manifest_yaml("  provides: {}\n");
    let (printer, buf) = cfgd_core::output::Printer::for_test();
    super::display_source_manifest(&printer, &manifest);
    drop(printer);
    let out = buf.lock().unwrap().clone();
    assert!(
        !out.contains("required item(s)") && !out.contains("recommended item(s)"),
        "zero-count tiers must be suppressed, got: {out}"
    );
}

#[test]
fn display_source_manifest_constraints_render_each_blocked_axis() {
    let manifest = manifest_yaml(
        r#"  provides: {}
  policy:
    constraints:
      noScripts: true
      noSecretsRead: true
      allowedTargetPaths: ["/etc/cfgd", "/var/lib/cfgd"]
"#,
    );
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    super::display_source_manifest(&printer, &manifest);
    drop(printer);
    let out = buf.lock().unwrap().clone();
    assert!(
        out.contains("Scripts: blocked"),
        "no-scripts line missing: {out}"
    );
    assert!(
        out.contains("Secret access: blocked"),
        "no-secrets line missing: {out}"
    );
    assert!(
        out.contains("Allowed paths") && out.contains("/etc/cfgd, /var/lib/cfgd"),
        "allowed-paths line must be comma-joined, got: {out}"
    );
}

#[test]
fn display_source_manifest_constraints_omitted_when_unrestricted() {
    // noScripts and noSecretsRead default to true via default_true; turn
    // them off to verify the suppression branches.
    let manifest = manifest_yaml(
        r#"  provides: {}
  policy:
    constraints:
      noScripts: false
      noSecretsRead: false
      allowedTargetPaths: []
"#,
    );
    let (printer, buf) = cfgd_core::output::Printer::for_test();
    super::display_source_manifest(&printer, &manifest);
    drop(printer);
    let out = buf.lock().unwrap().clone();
    assert!(
        !out.contains("Scripts: blocked")
            && !out.contains("Secret access: blocked")
            && !out.contains("Allowed paths"),
        "no constraint lines should appear when all unrestricted, got: {out}"
    );
}

#[test]
fn display_source_manifest_omits_optional_metadata_kv_when_absent() {
    // Manifest with only the required `name` field — no version, no
    // description. The Name kv must still appear; the other two suppressed.
    let manifest: cfgd_core::config::ConfigSourceDocument = serde_yaml::from_str(
        r#"apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: minimal
spec:
  provides: {}
"#,
    )
    .unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    super::display_source_manifest(&printer, &manifest);
    drop(printer);
    let out = buf.lock().unwrap().clone();
    assert!(out.contains("Name") && out.contains("minimal"));
    assert!(
        !out.contains("Version"),
        "Version kv must be suppressed when None: {out}"
    );
    assert!(
        !out.contains("Description"),
        "Description kv must be suppressed when None: {out}"
    );
}

// --- build_subscription_preview_input ---

fn manifest_policy_with_constraints() -> cfgd_core::config::ConfigSourcePolicy {
    let manifest: cfgd_core::config::ConfigSourceDocument = serde_yaml::from_str(
        r#"apiVersion: cfgd.io/v1alpha1
kind: ConfigSource
metadata:
  name: acme
spec:
  provides: {}
  policy:
    constraints:
      noScripts: true
      noSecretsRead: false
      allowedTargetPaths: ["/etc/cfgd"]
"#,
    )
    .expect("manifest fixture must parse");
    manifest.spec.policy
}

#[test]
fn build_subscription_preview_input_threads_priority_through() {
    let policy = manifest_policy_with_constraints();
    let input = super::build_subscription_preview_input("acme", 750, &policy, false, &[], vec![]);
    assert_eq!(input.priority, 750);
    assert_eq!(input.source_name, "acme");
}

#[test]
fn build_subscription_preview_input_clones_policy_and_constraints() {
    // The composition engine reads `input.policy` for tier classification
    // and `input.constraints` for path/script/secrets checks. Pin that
    // both come from the manifest's policy, not silently zeroed.
    let policy = manifest_policy_with_constraints();
    let input = super::build_subscription_preview_input("acme", 100, &policy, false, &[], vec![]);
    assert!(
        input.constraints.no_scripts,
        "noScripts must propagate from manifest"
    );
    assert!(
        !input.constraints.no_secrets_read,
        "noSecretsRead=false must NOT be silently flipped to true"
    );
    assert_eq!(
        input.constraints.allowed_target_paths,
        vec!["/etc/cfgd".to_string()]
    );
}

#[test]
fn build_subscription_preview_input_propagates_subscription_flags() {
    let policy = manifest_policy_with_constraints();
    let opt_in = vec!["editor".to_string(), "shell-aliases".to_string()];
    let input =
        super::build_subscription_preview_input("acme", 100, &policy, true, &opt_in, vec![]);
    assert!(input.subscription.accept_recommended);
    assert_eq!(input.subscription.opt_in, opt_in);
}

#[test]
fn build_subscription_preview_input_defaults_overrides_and_reject_to_null() {
    // The cfgd source add preview never carries user overrides/reject —
    // those are only meaningful after subscription. Pin that the helper
    // emits Null so the engine's default-tier classification kicks in.
    let policy = manifest_policy_with_constraints();
    let input = super::build_subscription_preview_input("acme", 100, &policy, false, &[], vec![]);
    assert!(matches!(
        input.subscription.overrides,
        serde_yaml::Value::Null
    ));
    assert!(matches!(input.subscription.reject, serde_yaml::Value::Null));
}

#[test]
fn build_subscription_preview_input_preserves_layer_ordering() {
    // Layers are applied in the order resolve_profile returned them
    // (lowest priority → highest, parents before children). The helper
    // must move the Vec into the input without reordering or dropping.
    let policy = manifest_policy_with_constraints();
    let layers = vec![
        cfgd_core::config::ProfileLayer {
            source: "local".to_string(),
            profile_name: "base".to_string(),
            priority: 0,
            policy: cfgd_core::config::LayerPolicy::Local,
            spec: cfgd_core::config::ProfileSpec::default(),
        },
        cfgd_core::config::ProfileLayer {
            source: "local".to_string(),
            profile_name: "overlay".to_string(),
            priority: 10,
            policy: cfgd_core::config::LayerPolicy::Local,
            spec: cfgd_core::config::ProfileSpec::default(),
        },
    ];
    let input =
        super::build_subscription_preview_input("acme", 100, &policy, false, &[], layers.clone());
    assert_eq!(input.layers.len(), 2);
    assert_eq!(input.layers[0].profile_name, "base");
    assert_eq!(input.layers[1].profile_name, "overlay");
}

#[test]
fn build_subscription_preview_input_empty_opt_in_is_empty_vec_not_dropped() {
    // SubscriptionConfig::opt_in is `Vec<String>` (no Option). An empty
    // input must produce an empty Vec — not panic, not skip the field.
    let policy = manifest_policy_with_constraints();
    let input = super::build_subscription_preview_input("acme", 100, &policy, false, &[], vec![]);
    assert!(input.subscription.opt_in.is_empty());
}

// --- format_conflict_preview_lines ---

fn conflict(
    resource: &str,
    kind: cfgd_core::composition::ResolutionType,
    source: &str,
    details: &str,
) -> cfgd_core::composition::ConflictResolution {
    cfgd_core::composition::ConflictResolution {
        resource_id: resource.to_string(),
        resolution_type: kind,
        winning_source: source.to_string(),
        details: details.to_string(),
    }
}

#[test]
fn format_conflict_preview_lines_empty_input_returns_empty_vec() {
    // The caller relies on `is_empty()` to take the "No conflicts" branch.
    assert!(super::format_conflict_preview_lines(&[]).is_empty());
}

#[test]
fn format_conflict_preview_lines_emits_canonical_shape() {
    let conflicts = vec![conflict(
        "package:apt:curl",
        cfgd_core::composition::ResolutionType::Locked,
        "acme-baseline",
        "policy locks installation",
    )];
    let lines = super::format_conflict_preview_lines(&conflicts);
    assert_eq!(lines.len(), 1);
    assert_eq!(
        lines[0],
        "  LOCKED package:apt:curl <- acme-baseline (policy locks installation)"
    );
}

#[test]
fn format_conflict_preview_lines_renders_each_resolution_type_label() {
    // All five ResolutionType variants must produce their canonical UPPER
    // label — pin so a future rename of `Override` → `Overridden` is
    // intentional, not silent.
    let conflicts = vec![
        conflict(
            "a",
            cfgd_core::composition::ResolutionType::Locked,
            "src",
            "d",
        ),
        conflict(
            "b",
            cfgd_core::composition::ResolutionType::Required,
            "src",
            "d",
        ),
        conflict(
            "c",
            cfgd_core::composition::ResolutionType::Override,
            "src",
            "d",
        ),
        conflict(
            "d",
            cfgd_core::composition::ResolutionType::Rejected,
            "src",
            "d",
        ),
        conflict(
            "e",
            cfgd_core::composition::ResolutionType::Default,
            "src",
            "d",
        ),
    ];
    let lines = super::format_conflict_preview_lines(&conflicts);
    assert_eq!(lines.len(), 5);
    assert!(lines[0].contains("LOCKED"));
    assert!(lines[1].contains("REQUIRED"));
    assert!(lines[2].contains("OVERRIDE"));
    assert!(lines[3].contains("REJECTED"));
    assert!(lines[4].contains("DEFAULT"));
}

#[test]
fn format_conflict_preview_lines_preserves_input_order() {
    // The composition engine returns conflicts in a deterministic order;
    // the formatter must not reorder them — consumers reading the output
    // top-to-bottom assume the engine's grouping (e.g. all package
    // conflicts together).
    let conflicts = vec![
        conflict(
            "z",
            cfgd_core::composition::ResolutionType::Default,
            "src",
            "z-details",
        ),
        conflict(
            "a",
            cfgd_core::composition::ResolutionType::Default,
            "src",
            "a-details",
        ),
    ];
    let lines = super::format_conflict_preview_lines(&conflicts);
    assert!(
        lines[0].contains(" z "),
        "first line must be `z`: {:?}",
        lines
    );
    assert!(
        lines[1].contains(" a "),
        "second line must be `a`: {:?}",
        lines
    );
}

#[test]
fn format_conflict_preview_lines_uses_two_space_indent() {
    // The output is indented under the "Conflicts with Current Config"
    // subheader so the eye groups them. Two spaces is the project-wide
    // indent convention.
    let conflicts = vec![conflict(
        "a",
        cfgd_core::composition::ResolutionType::Default,
        "s",
        "d",
    )];
    let lines = super::format_conflict_preview_lines(&conflicts);
    assert!(
        lines[0].starts_with("  "),
        "must start with two-space indent: {:?}",
        lines[0]
    );
    assert!(
        !lines[0].starts_with("   "),
        "must not be three-space indent: {:?}",
        lines[0]
    );
}

#[test]
fn add_and_remove_source_in_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#,
    )
    .unwrap();

    let source = cfgd_core::sources::SourceManager::build_source_spec(
        "acme",
        "git@github.com:acme/config.git",
        Some("backend"),
    );
    super::add_source_to_config(&config_path, &source).unwrap();

    let cfg = cfgd_core::config::load_config(&config_path).unwrap();
    assert_eq!(cfg.spec.sources.len(), 1);
    assert_eq!(cfg.spec.sources[0].name, "acme");

    super::remove_source_from_config(&config_path, "acme").unwrap();
    let cfg = cfgd_core::config::load_config(&config_path).unwrap();
    assert!(cfg.spec.sources.is_empty());
}

#[test]
fn set_nested_yaml_value_creates_path() {
    let mut root = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    super::set_nested_yaml_value(
        &mut root,
        "env.EDITOR",
        &serde_yaml::Value::String("nvim".into()),
    )
    .unwrap();

    let editor = root
        .get("env")
        .and_then(|v| v.get("EDITOR"))
        .and_then(|v| v.as_str());
    assert_eq!(editor, Some("nvim"));
}

// --- Module CRUD tests ---

fn test_cli(dir: &Path) -> Cli {
    test_cli_with_state(dir, None)
}

fn test_cli_with_state(dir: &Path, state_dir: Option<PathBuf>) -> Cli {
    // Default the cache root to the state dir so source/module caches stay inside
    // the test's tempdir rather than resolving to the real `~/.cache/cfgd`.
    let cache_dir = state_dir.clone();
    Cli {
        config: dir.join("cfgd.yaml"),
        config_explicit: false,
        profile: None,
        no_color: true,
        verbose: 0,
        quiet: true,
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Table),
        list_envelope: false,
        jsonpath: None,
        state_dir,
        config_dir: None,
        cache_dir,
        runtime_dir: None,
        scope_arg: crate::cli::ScopeArg::User,
        command: Some(Command::Status {
            module: None,
            exit_code: false,
        }),
    }
}

use cfgd_core::test_helpers::test_printer;

/// Capturing Printer at `Normal` verbosity for tests that need to inspect
/// headings, sections, or other output that requires non-quiet verbosity.
fn test_printer_capture() -> (cfgd_core::output::Printer, Arc<Mutex<String>>) {
    cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal)
}

/// Extract JSON object or array from captured output that may contain
/// preamble text (e.g. key_value lines from load_config_and_profile).
fn extract_json(output: &str) -> serde_json::Value {
    // Find first '{' or '['
    let start = output
        .find('{')
        .or_else(|| output.find('['))
        .unwrap_or_else(|| panic!("no JSON found in output: {output}"));
    let json_str = output[start..].trim();
    serde_json::from_str(json_str)
        .unwrap_or_else(|e| panic!("invalid JSON at offset {start}: {e}, got: {output}"))
}

fn test_profile_create_args(name: &str) -> ProfileCreateArgs {
    ProfileCreateArgs {
        name: name.to_string(),
        inherits: vec![],
        modules: vec![],
        packages: vec![],
        env: vec![],
        aliases: vec![],
        system: vec![],
        files: vec![],
        private: false,
        secrets: vec![],
        pre_apply: vec![],
        post_apply: vec![],
        pre_reconcile: vec![],
        post_reconcile: vec![],
        on_change: vec![],
        on_drift: vec![],
    }
}

fn empty_profile_update_args() -> ProfileUpdateArgs {
    ProfileUpdateArgs {
        name: None,
        inherits: vec![],
        modules: vec![],
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        system: vec![],
        secrets: vec![],
        pre_apply: vec![],
        post_apply: vec![],
        pre_reconcile: vec![],
        post_reconcile: vec![],
        on_change: vec![],
        on_drift: vec![],
        private: false,
        yes: false,
        allow_unsigned: false,
    }
}

fn create_module_in_dir(dir: &Path, name: &str, content: &str) {
    let mod_dir = dir.join("modules").join(name);
    std::fs::create_dir_all(mod_dir.join("files")).unwrap();
    std::fs::write(mod_dir.join("module.yaml"), content).unwrap();
}

fn empty_module_update_args(name: &str) -> ModuleUpdateArgs {
    ModuleUpdateArgs {
        name: name.to_string(),
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        depends: vec![],
        post_apply: vec![],
        private: false,
        description: None,
        sets: vec![],
    }
}

fn test_module_create_args(name: &str) -> ModuleCreateArgs {
    ModuleCreateArgs {
        name: name.to_string(),
        description: None,
        depends: vec![],
        packages: vec![],
        files: vec![],
        env: vec![],
        aliases: vec![],
        private: false,
        post_apply: vec![],
        sets: vec![],
        apply: false,
        yes: false,
    }
}

#[test]
fn module_create_with_flags_produces_valid_yaml() {
    let dir = tempfile::tempdir().unwrap();
    let module_dir = dir.path().join("modules").join("test-mod");
    let module_yaml = module_dir.join("module.yaml");

    // Create a test file to import
    let test_file = dir.path().join("testfile.txt");
    std::fs::write(&test_file, "content").unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleCreateArgs {
        description: Some("A test module".to_string()),
        depends: vec!["base".to_string()],
        packages: vec!["curl".to_string(), "vim".to_string()],
        files: vec![test_file.display().to_string()],
        post_apply: vec!["echo done".to_string()],
        sets: vec![
            "package.curl.minVersion=7.0".to_string(),
            "package.curl.prefer=brew,apt".to_string(),
            "package.vim.alias.snap=nvim".to_string(),
        ],
        ..test_module_create_args("test-mod")
    };
    module::cmd_module_create(&cli, &printer, &args).unwrap();

    assert!(module_yaml.exists());

    let contents = std::fs::read_to_string(&module_yaml).unwrap();
    let doc = config::parse_module(&contents).unwrap();

    assert_eq!(doc.metadata.name, "test-mod");
    assert_eq!(doc.metadata.description, Some("A test module".to_string()));
    assert_eq!(doc.spec.depends, vec!["base"]);
    assert_eq!(doc.spec.packages.len(), 2);
    assert_eq!(doc.spec.packages[0].name, "curl");
    assert_eq!(doc.spec.packages[0].min_version, Some("7.0".to_string()));
    assert_eq!(doc.spec.packages[0].prefer, vec!["brew", "apt"]);
    assert_eq!(doc.spec.packages[1].name, "vim");
    assert_eq!(
        doc.spec.packages[1].aliases.get("snap"),
        Some(&"nvim".to_string())
    );
    assert_eq!(doc.spec.files.len(), 1);
    assert!(doc.spec.files[0].source.contains("testfile.txt"));
    assert!(
        doc.spec
            .scripts
            .as_ref()
            .unwrap()
            .post_apply
            .contains(&config::ScriptEntry::Simple("echo done".to_string()))
    );
    assert!(module_dir.join("files").join("testfile.txt").exists());
}

#[test]
fn module_create_refuses_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "existing",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: existing\nspec: {}\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleCreateArgs {
        description: Some("dup".to_string()),
        ..test_module_create_args("existing")
    };
    let result = module::cmd_module_create(&cli, &printer, &args);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("already exists"));
}

#[test]
fn module_update_add_and_remove_packages() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "test-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n    - name: vim\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleUpdateArgs {
        packages: vec!["ripgrep".to_string(), "-vim".to_string()],
        ..empty_module_update_args("test-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "test-mod").unwrap();
    let names: Vec<&str> = doc.spec.packages.iter().map(|p| p.name.as_str()).collect();
    assert!(names.contains(&"curl"));
    assert!(names.contains(&"ripgrep"));
    assert!(!names.contains(&"vim"));
}

#[test]
fn module_update_set_overrides() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "test-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: neovim\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleUpdateArgs {
        sets: vec![
            "package.neovim.minVersion=0.9".to_string(),
            "package.neovim.prefer=brew,snap,apt".to_string(),
            "package.neovim.alias.snap=nvim".to_string(),
        ],
        ..empty_module_update_args("test-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "test-mod").unwrap();
    let pkg = &doc.spec.packages[0];
    assert_eq!(pkg.min_version, Some("0.9".to_string()));
    assert_eq!(pkg.prefer, vec!["brew", "snap", "apt"]);
    assert_eq!(pkg.aliases.get("snap"), Some(&"nvim".to_string()));
}

#[test]
fn module_delete_refuses_when_referenced() {
    let dir = create_test_config_dir();
    create_module_in_dir(
        dir.path(),
        "used-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: used-mod\nspec: {}\n",
    );

    // Update profile to reference the module
    let profile_path = dir.path().join("profiles").join("default.yaml");
    let mut doc = config::load_profile(&profile_path).unwrap();
    doc.spec.modules.push("used-mod".to_string());
    let yaml = serde_yaml::to_string(&doc).unwrap();
    std::fs::write(&profile_path, &yaml).unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = module::cmd_module_delete(&cli, &printer, "used-mod", true, false, false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("referenced by"));
}

#[test]
fn module_delete_succeeds_when_unreferenced() {
    let dir = create_test_config_dir();
    create_module_in_dir(
        dir.path(),
        "orphan-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: orphan-mod\nspec: {}\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    module::cmd_module_delete(&cli, &printer, "orphan-mod", true, false, false).unwrap();
    assert!(!dir.path().join("modules").join("orphan-mod").exists());
}

#[test]
fn module_delete_purge_removes_target_files() {
    let dir = create_test_config_dir();

    // Create a target file outside the module directory
    let target_dir = dir.path().join("targets");
    std::fs::create_dir_all(&target_dir).unwrap();
    let target_file = target_dir.join("deployed.conf");
    std::fs::write(&target_file, "deployed content").unwrap();
    assert!(target_file.exists());

    // Create a module with a file entry pointing at the target
    let module_yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: purge-mod\nspec:\n  files:\n    - source: files/deployed.conf\n      target: {}\n",
        target_file.display()
    );
    create_module_in_dir(dir.path(), "purge-mod", &module_yaml);
    // Write a source file in the module
    std::fs::write(
        dir.path()
            .join("modules")
            .join("purge-mod")
            .join("files")
            .join("deployed.conf"),
        "source content",
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    module::cmd_module_delete(&cli, &printer, "purge-mod", true, true, false).unwrap();
    assert!(!dir.path().join("modules").join("purge-mod").exists());
    assert!(!target_file.exists(), "purge should remove target file");
}

#[test]
fn module_delete_no_purge_preserves_target_files() {
    let dir = create_test_config_dir();

    // Create a target file (not a symlink into the module)
    let target_dir = dir.path().join("targets");
    std::fs::create_dir_all(&target_dir).unwrap();
    let target_file = target_dir.join("regular.conf");
    std::fs::write(&target_file, "user content").unwrap();

    let module_yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: keep-mod\nspec:\n  files:\n    - source: files/regular.conf\n      target: {}\n",
        target_file.display()
    );
    create_module_in_dir(dir.path(), "keep-mod", &module_yaml);

    let cli = test_cli(dir.path());
    let printer = test_printer();

    module::cmd_module_delete(&cli, &printer, "keep-mod", true, false, false).unwrap();
    assert!(!dir.path().join("modules").join("keep-mod").exists());
    assert!(
        target_file.exists(),
        "without purge, non-symlinked target files are preserved"
    );
}

#[test]
fn apply_module_sets_rejects_invalid_format() {
    let mut doc = config::ModuleDocument {
        api_version: cfgd_core::API_VERSION.to_string(),
        kind: "Module".to_string(),
        metadata: config::ModuleMetadata {
            name: "test".to_string(),
            description: None,
        },
        spec: config::ModuleSpec::default(),
    };

    // No = sign
    assert!(module::apply_module_sets(&["bad-format".to_string()], &mut doc).is_err());
    // Invalid path prefix
    assert!(module::apply_module_sets(&["foo.bar=baz".to_string()], &mut doc).is_err());
    // Package not found
    assert!(
        module::apply_module_sets(&["package.missing.minVersion=1.0".to_string()], &mut doc)
            .is_err()
    );
    // Empty package name
    assert!(module::apply_module_sets(&["package..minVersion=1.0".to_string()], &mut doc).is_err());
    // Empty field name
    assert!(module::apply_module_sets(&["package.curl.=1.0".to_string()], &mut doc).is_err());
}

#[test]
fn module_update_idempotent_add() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "test-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleUpdateArgs {
        packages: vec!["curl".to_string()],
        ..empty_module_update_args("test-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "test-mod").unwrap();
    assert_eq!(doc.spec.packages.len(), 1);
}

// --- Profile CRUD tests ---

#[test]
fn profile_create_with_flags() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ProfileCreateArgs {
        inherits: vec!["default".to_string()],
        modules: vec!["nvim".to_string()],
        packages: vec!["brew:curl".to_string(), "cargo:bat".to_string()],
        env: vec!["EDITOR=nvim".to_string()],
        system: vec!["shell=/bin/zsh".to_string()],
        ..test_profile_create_args("new-profile")
    };
    profile::cmd_profile_create(&cli, &printer, &args).unwrap();

    let profile_path = dir
        .path()
        .join("profiles")
        .join("new-profile")
        .join("profile.yaml");
    assert!(profile_path.exists());

    let doc = config::load_profile(&profile_path).unwrap();
    assert_eq!(doc.metadata.name, "new-profile");
    assert_eq!(doc.spec.inherits, vec!["default"]);
    assert_eq!(doc.spec.modules, vec!["nvim"]);
    assert!(doc.spec.env.iter().any(|e| e.name == "EDITOR"));
    assert!(doc.spec.system.contains_key("shell"));
}

#[test]
fn profile_create_refuses_duplicate() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = test_profile_create_args("default");
    let result = profile::cmd_profile_create(&cli, &printer, &args);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("already exists"));
}

#[test]
fn profile_create_refuses_missing_parent() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ProfileCreateArgs {
        inherits: vec!["nonexistent".to_string()],
        ..test_profile_create_args("child")
    };
    let result = profile::cmd_profile_create(&cli, &printer, &args);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn profile_update_add_and_remove() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ProfileUpdateArgs {
        modules: vec!["nvim".to_string()],
        packages: vec!["brew:jq".to_string()],
        env: vec!["EDITOR=nvim".to_string()],
        system: vec!["shell=/bin/zsh".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let profile_path = dir.path().join("profiles").join("default.yaml");
    let doc = config::load_profile(&profile_path).unwrap();
    assert!(doc.spec.modules.contains(&"nvim".to_string()));
    assert!(doc.spec.env.iter().any(|e| e.name == "EDITOR"));
    assert!(doc.spec.system.contains_key("shell"));
}

#[test]
fn profile_delete_refuses_active() {
    let dir = create_test_config_dir();
    std::fs::write(
            dir.path().join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n",
        )
        .unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = profile::cmd_profile_delete(&cli, &printer, "default", true, false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("active profile"));
}

#[test]
fn profile_delete_refuses_when_inherited() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = profile::cmd_profile_delete(&cli, &printer, "default", true, false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("inherited by"));
}

#[test]
fn profile_delete_succeeds() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    profile::cmd_profile_delete(&cli, &printer, "work", true, false).unwrap();
    assert!(!dir.path().join("profiles").join("work.yaml").exists());
}

#[test]
fn profiles_inheriting_finds_children() {
    let dir = create_test_config_dir();
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let result =
        profile::profiles_inheriting(&dir.path().join("profiles"), "default", &printer).unwrap();
    assert_eq!(result, vec!["work"]);

    let result =
        profile::profiles_inheriting(&dir.path().join("profiles"), "work", &printer).unwrap();
    assert!(result.is_empty());
}

/// Write both manifest forms for `name` so the profile is ambiguous on disk.
fn make_ambiguous_profile(profiles_dir: &Path, name: &str, inherits: Option<&str>) {
    let inherits_block = match inherits {
        Some(parent) => format!("  inherits:\n    - {}\n", parent),
        None => String::new(),
    };
    let yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: {name}\nspec:\n{inherits_block}  packages: {{}}\n",
    );
    let bundle = profiles_dir.join(name);
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(bundle.join("profile.yaml"), &yaml).unwrap();
    std::fs::write(profiles_dir.join(format!("{name}.yaml")), &yaml).unwrap();
}

#[test]
fn profiles_inheriting_tolerates_unrelated_ambiguous_profile() {
    let dir = create_test_config_dir();
    let pdir = dir.path().join("profiles");
    make_ambiguous_profile(&pdir, "amb", None);

    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let result = profile::profiles_inheriting(&pdir, "default", &printer).unwrap();
    assert_eq!(
        result,
        vec!["work"],
        "unrelated ambiguity must not error or hide the real inheritor"
    );
}

#[test]
fn profiles_inheriting_detects_inheritor_hidden_in_ambiguous_form() {
    let dir = create_test_config_dir();
    let pdir = dir.path().join("profiles");
    make_ambiguous_profile(&pdir, "hidden", Some("work"));

    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let result = profile::profiles_inheriting(&pdir, "work", &printer).unwrap();
    assert_eq!(
        result,
        vec!["hidden"],
        "an ambiguous profile that inherits the target must still count as an inheritor"
    );
}

#[test]
fn profiles_inheriting_warns_on_unparseable_manifest_and_keeps_real_inheritors() {
    let dir = create_test_config_dir();
    let pdir = dir.path().join("profiles");
    std::fs::write(pdir.join("broken.yaml"), "this: [is, not, a, profile\n").unwrap();

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let result = profile::profiles_inheriting(&pdir, "default", &printer).unwrap();
    assert_eq!(
        result,
        vec!["work"],
        "unparseable manifest must not hide the parseable inheritor"
    );
    let out = buf.lock().unwrap();
    assert!(
        out.contains("Skipping profile") && out.contains("broken.yaml"),
        "unparseable manifest must be surfaced as a warn naming its path; got: {out:?}"
    );
}

#[test]
fn profile_delete_still_refuses_when_parseable_inheritor_exists_beside_broken_manifest() {
    let dir = create_test_config_dir();
    let pdir = dir.path().join("profiles");
    std::fs::write(pdir.join("broken.yaml"), "this: [is, not, a, profile\n").unwrap();
    let cli = test_cli(dir.path());

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let err = profile::cmd_profile_delete(&cli, &printer, "default", true, false)
        .expect_err("delete of an inherited profile must refuse");
    assert!(
        err.to_string().contains("inherited by: work"),
        "refusal must name the parseable inheritor; got: {err}"
    );
    assert!(
        dir.path().join("profiles").join("default.yaml").exists(),
        "refused delete must leave the profile on disk"
    );
    let out = buf.lock().unwrap();
    assert!(
        out.contains("Skipping profile") && out.contains("broken.yaml"),
        "delete guard scan must surface the unparseable manifest; got: {out:?}"
    );
}

#[test]
fn profile_create_succeeds_despite_unrelated_ambiguous_profile() {
    let dir = create_test_config_dir();
    let pdir = dir.path().join("profiles");
    make_ambiguous_profile(&pdir, "amb", None);
    let cli = test_cli(dir.path());
    let (printer, buf) = test_printer_capture();

    // A non-empty spec flag keeps creation non-interactive (no TTY in tests).
    let args = ProfileCreateArgs {
        packages: vec!["cargo:bat".to_string()],
        ..test_profile_create_args("fresh")
    };
    profile::cmd_profile_create(&cli, &printer, &args).unwrap();

    assert!(pdir.join("fresh").join("profile.yaml").exists());
    let out = buf.lock().unwrap();
    assert!(
        out.contains("Created profile 'fresh'"),
        "success Doc must still render; got: {out:?}"
    );
    assert!(
        out.contains("Skipping profile 'amb'"),
        "ambiguous profile must surface as a warn, not an error; got: {out:?}"
    );
}

#[test]
fn profile_delete_succeeds_despite_unrelated_ambiguous_profile() {
    let dir = create_test_config_dir();
    let pdir = dir.path().join("profiles");
    make_ambiguous_profile(&pdir, "amb", None);
    let cli = test_cli(dir.path());
    let (printer, buf) = test_printer_capture();

    profile::cmd_profile_delete(&cli, &printer, "work", true, false).unwrap();

    assert!(!pdir.join("work.yaml").exists());
    let out = buf.lock().unwrap();
    assert!(
        out.contains("Deleted profile 'work'"),
        "success Doc must still render; got: {out:?}"
    );
    assert!(
        out.contains("Skipping profile 'amb'"),
        "ambiguous profile must surface as a warn, not an error; got: {out:?}"
    );
}

#[test]
fn parse_manager_package_valid() {
    let (mgr, pkg) = profile::parse_manager_package("brew:curl").unwrap();
    assert_eq!(mgr, "brew");
    assert_eq!(pkg, "curl");
}

#[test]
fn parse_manager_package_invalid() {
    assert!(profile::parse_manager_package("no-colon").is_err());
    assert!(profile::parse_manager_package(":curl").is_err());
    assert!(profile::parse_manager_package("brew:").is_err());
    assert!(profile::parse_manager_package(":").is_err());
}

#[test]
fn parse_package_flag_with_known_manager() {
    let known = &["brew", "apt", "cargo"];
    let (mgr, pkg) = parse_package_flag("brew:curl", known);
    assert_eq!(mgr, Some("brew".to_string()));
    assert_eq!(pkg, "curl");
}

#[test]
fn parse_package_flag_bare_name() {
    let known = &["brew", "apt", "cargo"];
    let (mgr, pkg) = parse_package_flag("ripgrep", known);
    assert_eq!(mgr, None);
    assert_eq!(pkg, "ripgrep");
}

#[test]
fn parse_package_flag_unknown_prefix_treated_as_bare() {
    let known = &["brew", "apt", "cargo"];
    // "python3:amd64" — "python3" is not a known manager
    let (mgr, pkg) = parse_package_flag("python3:amd64", known);
    assert_eq!(mgr, None);
    assert_eq!(pkg, "python3:amd64");
}

#[test]
fn parse_package_flag_empty_parts() {
    let known = &["brew"];
    // ":curl" — empty prefix, not a known manager
    let (mgr, pkg) = parse_package_flag(":curl", known);
    assert_eq!(mgr, None);
    assert_eq!(pkg, ":curl");

    // "brew:" — empty suffix
    let (mgr, pkg) = parse_package_flag("brew:", known);
    assert_eq!(mgr, None);
    assert_eq!(pkg, "brew:");
}

#[test]
fn parse_secret_spec_valid() {
    let spec = profile::parse_secret_spec("secrets/key.enc:~/.config/app/key").unwrap();
    assert_eq!(spec.source, "secrets/key.enc");
    assert_eq!(spec.target, Some(PathBuf::from("~/.config/app/key")));
    assert!(spec.template.is_none());
    assert!(spec.backend.is_none());
    assert!(spec.envs.is_none());
}

#[test]
fn parse_secret_spec_provider_url() {
    // Provider URLs with :// must not be split on the scheme colon
    let spec = profile::parse_secret_spec("op://vault/item:~/.config/key").unwrap();
    assert_eq!(spec.source, "op://vault/item");
    assert_eq!(spec.target, Some(PathBuf::from("~/.config/key")));
}

#[test]
fn parse_secret_spec_absolute_target() {
    let spec = profile::parse_secret_spec("secrets/db.enc:/etc/app/db.conf").unwrap();
    assert_eq!(spec.source, "secrets/db.enc");
    assert_eq!(spec.target, Some(PathBuf::from("/etc/app/db.conf")));
}

#[test]
fn parse_secret_spec_invalid() {
    assert!(profile::parse_secret_spec("no-colon").is_err());
    assert!(profile::parse_secret_spec(":target").is_err());
    assert!(profile::parse_secret_spec("source:").is_err());
}

#[test]
fn profile_update_inherits() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Add inherits
    let args = ProfileUpdateArgs {
        inherits: vec!["default".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "work", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("work.yaml")).unwrap();
    assert!(doc.spec.inherits.contains(&"default".to_string()));

    // Remove inherits
    let args = ProfileUpdateArgs {
        inherits: vec!["-default".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "work", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("work.yaml")).unwrap();
    assert!(!doc.spec.inherits.contains(&"default".to_string()));
}

#[test]
fn profile_update_secrets() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Add secret
    let args = ProfileUpdateArgs {
        secrets: vec!["secrets/key.enc:~/.config/app/key".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert_eq!(doc.spec.secrets.len(), 1);
    assert_eq!(doc.spec.secrets[0].source, "secrets/key.enc");

    // Remove secret
    let args = ProfileUpdateArgs {
        secrets: vec!["-~/.config/app/key".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(doc.spec.secrets.is_empty());
}

#[test]
fn profile_update_scripts() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Add pre-apply, post-apply, pre-reconcile, post-reconcile, on-change
    let args = ProfileUpdateArgs {
        pre_apply: vec!["scripts/pre.sh".to_string()],
        post_apply: vec!["scripts/post.sh".to_string()],
        pre_reconcile: vec!["scripts/pre-rec.sh".to_string()],
        post_reconcile: vec!["scripts/post-rec.sh".to_string()],
        on_change: vec!["scripts/on-change.sh".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let scripts = doc.spec.scripts.as_ref().unwrap();
    assert_eq!(
        scripts.pre_apply,
        vec![config::ScriptEntry::Simple("scripts/pre.sh".to_string())]
    );
    assert_eq!(
        scripts.post_apply,
        vec![config::ScriptEntry::Simple("scripts/post.sh".to_string())]
    );
    assert_eq!(
        scripts.pre_reconcile,
        vec![config::ScriptEntry::Simple(
            "scripts/pre-rec.sh".to_string()
        )]
    );
    assert_eq!(
        scripts.post_reconcile,
        vec![config::ScriptEntry::Simple(
            "scripts/post-rec.sh".to_string()
        )]
    );
    assert_eq!(
        scripts.on_change,
        vec![config::ScriptEntry::Simple(
            "scripts/on-change.sh".to_string()
        )]
    );

    // Remove all scripts
    let args = ProfileUpdateArgs {
        pre_apply: vec!["-scripts/pre.sh".to_string()],
        post_apply: vec!["-scripts/post.sh".to_string()],
        pre_reconcile: vec!["-scripts/pre-rec.sh".to_string()],
        post_reconcile: vec!["-scripts/post-rec.sh".to_string()],
        on_change: vec!["-scripts/on-change.sh".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let scripts = doc.spec.scripts.as_ref().unwrap();
    assert!(scripts.pre_apply.is_empty());
    assert!(scripts.post_apply.is_empty());
    assert!(scripts.pre_reconcile.is_empty());
    assert!(scripts.post_reconcile.is_empty());
    assert!(scripts.on_change.is_empty());
}

#[test]
fn profiles_using_module_finds_references() {
    let dir = create_test_config_dir();

    // Add module ref to default profile
    let profile_path = dir.path().join("profiles").join("default.yaml");
    let mut doc = config::load_profile(&profile_path).unwrap();
    doc.spec.modules.push("my-mod".to_string());
    std::fs::write(&profile_path, serde_yaml::to_string(&doc).unwrap()).unwrap();

    let result = module::profiles_using_module(&dir.path().join("profiles"), "my-mod").unwrap();
    assert_eq!(result, vec!["default"]);

    let result =
        module::profiles_using_module(&dir.path().join("profiles"), "nonexistent").unwrap();
    assert!(result.is_empty());
}

// --- Config CRUD tests ---

#[test]
fn config_show_displays_config() {
    let dir = create_test_config_dir();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test-config
spec:
  profile: default
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();
    let result = config_cmd::cmd_config_show(&cli, &printer);
    assert!(result.is_ok(), "config show failed: {:?}", result.err());
    drop(printer);

    let output = cap.human();
    assert!(
        output.contains("Configuration"),
        "output should contain header 'Configuration', got: {output}"
    );
    assert!(
        output.contains("default"),
        "output should show profile name 'default', got: {output}"
    );
}

#[test]
fn config_show_fails_without_config() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let result = config_cmd::cmd_config_show(&cli, &test_printer());
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("config file not found"),
        "expected typed no-config error, got: {msg}"
    );
}

// --- Source CRUD tests ---

#[test]
fn source_create_scaffolds_manifest() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = source::cmd_source_create(
        &cli,
        &printer,
        Some("my-source"),
        Some("Test"),
        Some("1.0.0"),
    );
    assert!(
        result.is_ok(),
        "source create should scaffold manifest successfully: {:?}",
        result.err()
    );

    let source_path = dir.path().join("cfgd-source.yaml");
    assert!(source_path.exists());

    let contents = std::fs::read_to_string(&source_path).unwrap();
    assert_eq!(
        contents.lines().next().unwrap(),
        cfgd_core::config::schema_modeline(
            cfgd_core::config::SchemaDocKind::ConfigSource,
            env!("CARGO_PKG_VERSION")
        )
        .trim_end(),
        "scaffolded cfgd-source.yaml must start with the schema modeline"
    );
    // Modeline is a YAML comment: the manifest must still parse.
    let parsed: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
    assert_eq!(parsed["kind"], serde_yaml::Value::from("ConfigSource"));
    assert!(contents.contains("my-source"));
    assert!(contents.contains("Test"));
    assert!(contents.contains("1.0.0"));
    // Should include profiles found in the directory
    assert!(contents.contains("default"));
    assert!(contents.contains("work"));
}

#[test]
fn source_create_refuses_duplicate() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
    std::fs::write(dir.path().join("cfgd-source.yaml"), "existing").unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();
    let result = source::cmd_source_create(&cli, &printer, Some("x"), Some("x"), Some("1.0"));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("already exists"));
}

#[test]
fn source_create_interactive_mode_prompts_for_name_and_description() {
    // All three flags (name/description/version) are None → is_interactive
    // is true → cmd_source_create.rs:30-31 + 41-42 prompt branches fire.
    // Queue Text answers via Printer::for_test_with_prompt_responses.
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let (printer, _cap) = cfgd_core::output::Printer::for_test_doc_with_prompt_responses(vec![
        cfgd_core::output::PromptAnswer::Text("interactive-source".to_string()),
        cfgd_core::output::PromptAnswer::Text("Interactive description".to_string()),
    ]);

    source::cmd_source_create(&cli, &printer, None, None, None)
        .expect("interactive create should succeed");

    let contents = std::fs::read_to_string(dir.path().join("cfgd-source.yaml")).unwrap();
    assert!(
        contents.contains("interactive-source"),
        "name from prompt must land in manifest: {contents}"
    );
    assert!(
        contents.contains("Interactive description"),
        "description from prompt must land in manifest: {contents}"
    );
    // Default version when version flag is None and not interactive-prompted
    // for (cmd_source_create only prompts for name + description).
    assert!(
        contents.contains("0.1.0"),
        "default version 0.1.0 must be applied: {contents}"
    );
}

#[cfg(unix)]
use cfgd_core::test_helpers::EditorGuard;

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn source_edit_with_valid_manifest_reports_valid_and_returns_ok() {
    // EDITOR=/bin/true → open_in_editor exits 0 without touching the
    // file, so the post-edit validation reads the same valid manifest we
    // wrote and lands in the "Source manifest is valid" success arm.
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();
    std::fs::write(
        dir.path().join("cfgd-source.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: edit-mod\nspec:\n  provides:\n    profiles:\n      - default\n",
    )
    .unwrap();

    let _editor = EditorGuard::set("/usr/bin/true");
    source::cmd_source_edit(&cli, &printer).expect("valid manifest + no-op editor → Ok");

    drop(printer);
    let out = cap.human();
    assert!(
        out.contains("Source manifest is valid"),
        "happy-path validation arm should announce validity: {out}"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn source_edit_with_invalid_manifest_and_prompt_declined_breaks_with_warning() {
    // Mirrors the profile/edit and config_cmd patterns: pre-stage an
    // invalid manifest, route through the no-op editor, queue
    // Confirm(false) so the prompt at source/edit.rs:25 takes the
    // "Saved with validation errors" branch.
    let dir = create_test_config_dir();
    std::fs::write(
        dir.path().join("cfgd-source.yaml"),
        "not a ConfigSource document",
    )
    .unwrap();
    let cli = test_cli(dir.path());
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc_with_prompt_responses(vec![
        cfgd_core::output::PromptAnswer::Confirm(false),
    ]);

    let _editor = EditorGuard::set("/usr/bin/true");
    source::cmd_source_edit(&cli, &printer).expect("save-with-errors must return Ok");

    drop(printer);
    let out = cap.human();
    assert!(
        out.contains("Saved with validation errors"),
        "prompt-decline branch must warn: {out}"
    );
}

#[test]
fn source_edit_fails_without_manifest() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();
    let result = source::cmd_source_edit(&cli, &printer);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("No cfgd-source.yaml")
    );
}

// --- Workflow tests ---

#[test]
fn generate_workflow_yaml_contains_all_resources() {
    let modules = vec!["neovim".to_string(), "zsh".to_string()];
    let profiles = vec!["default".to_string(), "work".to_string()];

    let yaml = workflow::generate_release_workflow_yaml(&modules, &profiles, "master").unwrap();

    // Header
    assert!(yaml.contains("name: cfgd Release"));
    assert!(yaml.contains("on:"));

    // Module paths
    assert!(yaml.contains("modules/neovim/**"));
    assert!(yaml.contains("modules/zsh/**"));

    // Profile paths
    assert!(yaml.contains("profiles/default.yaml"));
    assert!(yaml.contains("profiles/work.yaml"));

    // Jobs
    assert!(yaml.contains("detect-changes:"));
    assert!(yaml.contains("tag-modules:"));
    assert!(yaml.contains("tag-profiles:"));

    // Module outputs
    assert!(yaml.contains("module_neovim"));
    assert!(yaml.contains("module_zsh"));

    // Profile outputs
    assert!(yaml.contains("profile_default"));
    assert!(yaml.contains("profile_work"));
}

#[test]
fn generate_workflow_yaml_modules_only() {
    let modules = vec!["vim".to_string()];
    let profiles: Vec<String> = vec![];

    let yaml = workflow::generate_release_workflow_yaml(&modules, &profiles, "master").unwrap();

    assert!(yaml.contains("tag-modules:"));
    assert!(!yaml.contains("tag-profiles:"));
}

#[test]
fn generate_workflow_yaml_profiles_only() {
    let modules: Vec<String> = vec![];
    let profiles = vec!["default".to_string()];

    let yaml = workflow::generate_release_workflow_yaml(&modules, &profiles, "master").unwrap();

    assert!(!yaml.contains("tag-modules:"));
    assert!(yaml.contains("tag-profiles:"));
}

#[test]
fn workflow_generate_creates_file() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = workflow::cmd_workflow_generate(&cli, &printer, false);
    assert!(
        result.is_ok(),
        "workflow generate should create the workflow file: {:?}",
        result.err()
    );

    let workflow_path = dir
        .path()
        .join(".github")
        .join("workflows")
        .join("cfgd-release.yml");
    assert!(workflow_path.exists());

    let contents = std::fs::read_to_string(&workflow_path).unwrap();
    assert!(contents.contains("cfgd Release"));
    assert!(contents.contains("default"));
}

#[test]
fn workflow_generate_empty_repo() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) = test_printer_capture();

    // No profiles or modules — should warn and return Ok
    let result = workflow::cmd_workflow_generate(&cli, &printer, false);
    assert!(
        result.is_ok(),
        "workflow generate should return Ok with no modules/profiles (warn+skip): {:?}",
        result.err()
    );

    let workflow_path = dir
        .path()
        .join(".github")
        .join("workflows")
        .join("cfgd-release.yml");
    assert!(
        !workflow_path.exists(),
        "no workflow file should be created for empty repo"
    );

    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("No profiles") || output.contains("nothing to generate"),
        "should warn about no profiles/modules, got: {output}"
    );
}

#[test]
fn generate_workflow_yaml_hyphens_in_names() {
    let modules = vec!["my-module".to_string()];
    let profiles = vec!["my-profile".to_string()];

    let yaml = workflow::generate_release_workflow_yaml(&modules, &profiles, "master").unwrap();

    // Hyphens should be converted to underscores in output names
    assert!(yaml.contains("module_my_module"));
    assert!(yaml.contains("profile_my_profile"));
}

#[test]
fn test_validate_resource_name_valid() {
    // Each call must succeed (unwrap ensures no silent failures)
    validate_resource_name("my-module", "Module").unwrap();
    validate_resource_name("my_module", "Module").unwrap();
    validate_resource_name("Module123", "Module").unwrap();
    validate_resource_name("a", "Module").unwrap();
    validate_resource_name("foo.bar", "Module").unwrap();
}

#[test]
fn test_validate_resource_name_invalid() {
    assert!(validate_resource_name("", "Module").is_err());
    assert!(validate_resource_name("../etc", "Module").is_err());
    assert!(validate_resource_name(".hidden", "Module").is_err());
    assert!(validate_resource_name("-leading", "Module").is_err());
    assert!(validate_resource_name("foo/bar", "Module").is_err());
    assert!(validate_resource_name("foo bar", "Module").is_err());
    assert!(validate_resource_name("a".repeat(129).as_str(), "Module").is_err());
}

#[test]
fn workflow_generate_force_overwrites() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    // First generate
    workflow::cmd_workflow_generate(&cli, &printer, false).unwrap();
    let path = dir.path().join(".github/workflows/cfgd-release.yml");
    assert!(path.exists());

    // Write something different to the file
    std::fs::write(&path, "old content").unwrap();

    // Force overwrite
    workflow::cmd_workflow_generate(&cli, &printer, true).unwrap();
    let contents = std::fs::read_to_string(&path).unwrap();
    assert!(contents.contains("cfgd Release"));
    assert!(!contents.contains("old content"));
}

#[test]
fn source_create_with_modules() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    // Create a module
    create_module_in_dir(
        dir.path(),
        "neovim",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: neovim\nspec:\n  packages: []\n  files: []\n  depends: []\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = source::cmd_source_create(
        &cli,
        &printer,
        Some("test-source"),
        Some("Test"),
        Some("1.0.0"),
    );
    assert!(
        result.is_ok(),
        "source create should succeed with modules present: {:?}",
        result.err()
    );

    let source_path = dir.path().join("cfgd-source.yaml");
    assert!(source_path.exists());

    let contents = std::fs::read_to_string(&source_path).unwrap();
    // Should contain both the profile and the module
    assert!(contents.contains("default"));
    assert!(contents.contains("neovim"));
}

#[test]
fn source_create_output_is_parseable() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    source::cmd_source_create(
        &cli,
        &printer,
        Some("my-source"),
        Some("desc"),
        Some("0.1.0"),
    )
    .unwrap();

    let contents = std::fs::read_to_string(dir.path().join("cfgd-source.yaml")).unwrap();
    let result = config::parse_config_source(&contents);
    assert!(
        result.is_ok(),
        "Generated source YAML should be parseable: {:?}",
        result.err()
    );

    let doc = result.unwrap();
    assert_eq!(doc.metadata.name, "my-source");
    assert_eq!(doc.metadata.version, Some("0.1.0".to_string()));
}

#[test]
fn config_show_with_all_sections() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  origin:
    - url: https://github.com/test/config
      branch: main
      type: Git
  sources:
    - name: team-config
      origin:
        url: https://github.com/test/team
        branch: main
        type: Git
      subscription:
        priority: 100
  modules:
    registries:
      - name: community
        url: https://github.com/cfgd/modules
    security:
      requireSignatures: true
  daemon:
    enabled: true
    reconcile:
      interval: 5m
      onChange: true
      autoApply: false
    sync:
      interval: 30m
  secrets:
    backend: sops-age
  theme:
    name: ocean
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    let result = config_cmd::cmd_config_show(&cli, &printer);
    assert!(result.is_ok(), "config show failed: {:?}", result.err());
    drop(printer);

    let output = cap.human();
    assert!(output.contains("Configuration"), "missing header");
    assert!(output.contains("Origins"), "missing Origins section");
    assert!(output.contains("Sources"), "missing Sources section");
    assert!(output.contains("team-config"), "missing source name");
    assert!(
        output.contains("Module Registries"),
        "missing Module Registries section"
    );
    assert!(output.contains("community"), "missing registry name");
    assert!(output.contains("Daemon"), "missing Daemon section");
    assert!(
        output.contains("Enabled") && output.contains("yes"),
        "daemon enabled key/value missing"
    );
    assert!(output.contains("Secrets"), "missing Secrets section");
    assert!(output.contains("sops-age"), "missing secrets backend");
    assert!(output.contains("Theme"), "missing Theme section");
    assert!(output.contains("ocean"), "missing theme name");
}

// --- Alias expansion tests ---

#[test]
fn expand_aliases_no_builtins() {
    // Aliases come from cfgd.yaml only — no hardcoded builtins.
    // Without a config, "add" and "remove" pass through unchanged.
    let args = vec!["cfgd".into(), "add".into(), "~/.zshrc".into()];
    let expanded = expand_aliases(args.clone());
    assert_eq!(expanded, args);

    let args = vec!["cfgd".into(), "remove".into(), "~/.zshrc".into()];
    let expanded = expand_aliases(args.clone());
    assert_eq!(expanded, args);
}

#[test]
fn expand_aliases_no_match_passthrough() {
    let args = vec!["cfgd".into(), "apply".into(), "--dry-run".into()];
    let expanded = expand_aliases(args.clone());
    assert_eq!(expanded, args);
}

#[test]
fn expand_aliases_skips_global_flags() {
    // Without config-defined aliases, "add" passes through even with global flags
    let args = vec![
        "cfgd".into(),
        "--verbose".into(),
        "add".into(),
        "~/.zshrc".into(),
    ];
    let expanded = expand_aliases(args.clone());
    assert_eq!(expanded, args);
}

#[test]
fn expand_aliases_with_config_flag() {
    // With nonexistent config, no aliases are loaded — passthrough
    let args = vec![
        "cfgd".into(),
        "--config".into(),
        "/tmp/nonexistent.yaml".into(),
        "add".into(),
        "~/.zshrc".into(),
    ];
    let expanded = expand_aliases(args.clone());
    assert_eq!(expanded, args);
}

#[test]
fn expand_aliases_empty_args() {
    let args = vec!["cfgd".into()];
    let expanded = expand_aliases(args.clone());
    assert_eq!(expanded, args);
}

// --- find_subcommand_index ---

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn find_subcommand_index_returns_none_for_argv0_only() {
    assert_eq!(super::find_subcommand_index(&s(&["cfgd"])), None);
}

#[test]
fn find_subcommand_index_returns_none_for_only_flags() {
    // No positional → None (alias expansion bails out, clap takes over).
    assert_eq!(
        super::find_subcommand_index(&s(&["cfgd", "--verbose", "--no-color"])),
        None
    );
}

#[test]
fn find_subcommand_index_locates_first_positional() {
    let args = s(&["cfgd", "apply"]);
    assert_eq!(super::find_subcommand_index(&args), Some(1));
}

#[test]
fn find_subcommand_index_skips_boolean_global_flags() {
    let args = s(&["cfgd", "--verbose", "-v", "-q", "--no-color", "apply"]);
    assert_eq!(super::find_subcommand_index(&args), Some(5));
}

#[test]
fn find_subcommand_index_skips_value_for_separate_form_config() {
    // --config <path> is two args; the subcommand is at idx 3, not 2.
    let args = s(&["cfgd", "--config", "/etc/cfgd.yaml", "apply"]);
    assert_eq!(super::find_subcommand_index(&args), Some(3));
}

#[test]
fn find_subcommand_index_skips_value_for_separate_form_profile() {
    let args = s(&["cfgd", "--profile", "developer", "apply"]);
    assert_eq!(super::find_subcommand_index(&args), Some(3));
}

#[test]
fn find_subcommand_index_does_not_skip_for_inline_form_config() {
    // --config=<value> is a single arg; the subcommand is right after.
    let args = s(&["cfgd", "--config=/etc/cfgd.yaml", "apply"]);
    assert_eq!(super::find_subcommand_index(&args), Some(2));
}

#[test]
fn find_subcommand_index_does_not_skip_for_inline_form_profile() {
    let args = s(&["cfgd", "--profile=dev", "apply"]);
    assert_eq!(super::find_subcommand_index(&args), Some(2));
}

#[test]
fn find_subcommand_index_handles_mixed_global_flag_forms() {
    let args = s(&[
        "cfgd",
        "-v",
        "--config",
        "/path",
        "--profile=dev",
        "--no-color",
        "module",
    ]);
    assert_eq!(super::find_subcommand_index(&args), Some(6));
}

#[test]
fn find_subcommand_index_stops_at_double_dash() {
    // POSIX `--` ends the options section; nothing past it is a subcommand
    // candidate (it's all positional args for the *parent*, not a new
    // subcommand). Pin so a future "scan past --" change is intentional.
    let args = s(&["cfgd", "--verbose", "--", "apply"]);
    assert_eq!(super::find_subcommand_index(&args), None);
}

#[test]
fn find_subcommand_index_does_not_misread_value_starting_with_dash() {
    // After --config the next slot is *the value*, even if it looks like
    // a flag (e.g. someone passes `--config -my-config.yaml`). The scanner
    // must skip it unconditionally so the real subcommand stays visible.
    let args = s(&["cfgd", "--config", "-/weird/path", "apply"]);
    assert_eq!(super::find_subcommand_index(&args), Some(3));
}

#[test]
fn find_subcommand_index_returns_first_positional_when_subcommand_at_position_one() {
    // Common case: no global flags, subcommand at idx 1.
    let args = s(&["cfgd", "init", "--apply"]);
    assert_eq!(super::find_subcommand_index(&args), Some(1));
}

#[test]
fn find_subcommand_index_skips_value_taking_global_flags() {
    // Table covers every value-taking global flag on `Cli` (--config,
    // --profile, --output/-o, --jsonpath, --state-dir) in both space and
    // inline-`=` forms, plus the boolean fall-through and the POSIX `--`
    // sentinel. Each row carries a label so a failure pinpoints the case.
    let cases: &[(&[&str], Option<usize>, &str)] = &[
        (&["cfgd", "apply"], Some(1), "baseline"),
        (
            &["cfgd", "--config", "foo.yaml", "apply"],
            Some(3),
            "--config space",
        ),
        (
            &["cfgd", "--config=foo.yaml", "apply"],
            Some(2),
            "--config inline",
        ),
        (
            &["cfgd", "--state-dir", "/tmp/x", "add", "f.txt"],
            Some(3),
            "--state-dir space",
        ),
        (
            &["cfgd", "--state-dir=/tmp/x", "add", "f.txt"],
            Some(2),
            "--state-dir inline",
        ),
        (&["cfgd", "-o", "json", "status"], Some(3), "-o space"),
        (&["cfgd", "-o=json", "status"], Some(2), "-o inline"),
        (
            &["cfgd", "--output", "json", "status"],
            Some(3),
            "--output space",
        ),
        (
            &["cfgd", "--output=json", "status"],
            Some(2),
            "--output inline",
        ),
        (
            &["cfgd", "--jsonpath", "{.name}", "status"],
            Some(3),
            "--jsonpath space",
        ),
        (
            &["cfgd", "--jsonpath={.name}", "status"],
            Some(2),
            "--jsonpath inline",
        ),
        (
            &["cfgd", "--profile", "dev", "apply"],
            Some(3),
            "--profile space",
        ),
        (
            &["cfgd", "--profile=dev", "apply"],
            Some(2),
            "--profile inline",
        ),
        (
            &["cfgd", "--verbose", "apply"],
            Some(2),
            "--verbose boolean",
        ),
        (&["cfgd", "-v", "apply"], Some(2), "-v boolean"),
        (&["cfgd", "-q", "apply"], Some(2), "-q boolean"),
        (&["cfgd", "--quiet", "apply"], Some(2), "--quiet boolean"),
        (
            &["cfgd", "--no-color", "apply"],
            Some(2),
            "--no-color boolean",
        ),
        (
            &[
                "cfgd",
                "--state-dir",
                "/x",
                "--verbose",
                "--output",
                "json",
                "status",
            ],
            Some(6),
            "mixed value + boolean globals",
        ),
        (&["cfgd", "--", "literal"], None, "POSIX -- sentinel"),
        (&["cfgd"], None, "no args"),
        (
            &["cfgd", "--config", "foo.yaml"],
            None,
            "only flags + value, no positional",
        ),
    ];
    for (argv, expected, label) in cases {
        let args: Vec<String> = argv.iter().map(|x| x.to_string()).collect();
        assert_eq!(
            super::find_subcommand_index(&args),
            *expected,
            "case: {label}",
        );
    }
}

#[test]
fn resolve_profile_name_explicit_takes_precedence() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let result = resolve_profile_name(&cli, &test_printer(), Some("my-profile"));
    assert_eq!(result.unwrap(), "my-profile");
}

#[test]
fn resolve_profile_name_defaults_to_active() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
    let cli = test_cli(dir.path());
    let result = resolve_profile_name(&cli, &test_printer(), None);
    assert_eq!(result.unwrap(), "default");
}

#[test]
fn parse_file_spec_plain_path() {
    let (source, target) = super::parse_file_spec("~/.zshrc").unwrap();
    assert_eq!(source, target);
}

#[test]
fn parse_file_spec_source_target() {
    let (source, target) = super::parse_file_spec("./my-config:~/.config/app/config").unwrap();
    assert_eq!(source, std::path::PathBuf::from("./my-config"));
    assert!(target.to_string_lossy().contains(".config/app/config"));
}

#[test]
fn parse_file_spec_empty_source_errors() {
    assert!(super::parse_file_spec(":~/.zshrc").is_err());
}

#[test]
fn parse_file_spec_empty_target_errors() {
    assert!(super::parse_file_spec("~/.zshrc:").is_err());
}

#[test]
fn is_unmanaged_file_nonexistent() {
    let dir = tempfile::tempdir().unwrap();
    let state = StateStore::open_in_memory().unwrap();
    let target = dir.path().join("does-not-exist");
    assert!(!is_unmanaged_file(&target, dir.path(), &state));
}

#[test]
fn is_unmanaged_file_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let state = StateStore::open_in_memory().unwrap();
    let target = dir.path().join("existing-file");
    std::fs::write(&target, "content").unwrap();
    assert!(is_unmanaged_file(&target, dir.path(), &state));
}

#[test]
#[cfg(unix)]
fn is_unmanaged_file_cfgd_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let state = StateStore::open_in_memory().unwrap();
    let source = dir.path().join("source-file");
    std::fs::write(&source, "content").unwrap();
    let target = dir.path().join("subdir").join("symlink");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&source, &target).unwrap();
    // Symlink points into config_dir, so it's managed
    assert!(!is_unmanaged_file(&target, dir.path(), &state));
}

#[test]
fn is_unmanaged_file_tracked_in_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = StateStore::open_in_memory().unwrap();
    let target = dir.path().join("tracked-file");
    std::fs::write(&target, "content").unwrap();
    let target_str = target.display().to_string();
    state
        .upsert_managed_resource("file", &target_str, "local", None, None)
        .unwrap();
    assert!(!is_unmanaged_file(&target, dir.path(), &state));
}

// --- config get/set/unset helpers ---

fn make_test_config(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("cfgd.yaml");
    std::fs::write(
        &path,
        "apiVersion: cfgd.io/v1alpha1\n\
             kind: Config\n\
             metadata:\n\
             \x20 name: test\n\
             spec:\n\
             \x20 profile: work\n\
             \x20 fileStrategy: Symlink\n\
             \x20 theme:\n\
             \x20\x20\x20 name: dracula\n\
             \x20 daemon:\n\
             \x20\x20\x20 enabled: true\n\
             \x20\x20\x20 reconcile:\n\
             \x20\x20\x20\x20\x20 interval: 5m\n\
             \x20\x20\x20\x20\x20 onChange: false\n\
             \x20 aliases:\n\
             \x20\x20\x20 add: 'profile update --file'\n\
             \x20\x20\x20 deploy: 'apply --yes'\n",
    )
    .unwrap();
    path
}

#[test]
fn config_get_scalar() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_test_config(dir.path());
    let contents = std::fs::read_to_string(&path).unwrap();
    let raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
    let spec = raw.get("spec").unwrap();

    let val = config_cmd::walk_yaml_path(spec, "profile").unwrap();
    assert_eq!(val.as_str().unwrap(), "work");
}

#[test]
fn config_get_nested() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_test_config(dir.path());
    let contents = std::fs::read_to_string(&path).unwrap();
    let raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
    let spec = raw.get("spec").unwrap();

    let val = config_cmd::walk_yaml_path(spec, "daemon.reconcile.interval").unwrap();
    assert_eq!(val.as_str().unwrap(), "5m");
}

#[test]
fn config_get_boolean() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_test_config(dir.path());
    let contents = std::fs::read_to_string(&path).unwrap();
    let raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
    let spec = raw.get("spec").unwrap();

    let val = config_cmd::walk_yaml_path(spec, "daemon.enabled").unwrap();
    assert!(val.as_bool().unwrap());
}

#[test]
fn config_get_complex_returns_mapping() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_test_config(dir.path());
    let contents = std::fs::read_to_string(&path).unwrap();
    let raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
    let spec = raw.get("spec").unwrap();

    let val = config_cmd::walk_yaml_path(spec, "daemon").unwrap();
    assert!(val.is_mapping());
}

#[test]
fn config_get_missing_key_errors() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_test_config(dir.path());
    let contents = std::fs::read_to_string(&path).unwrap();
    let raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
    let spec = raw.get("spec").unwrap();

    let result = config_cmd::walk_yaml_path(spec, "nonexistent");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn config_get_alias() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_test_config(dir.path());
    let contents = std::fs::read_to_string(&path).unwrap();
    let raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
    let spec = raw.get("spec").unwrap();

    let val = config_cmd::walk_yaml_path(spec, "aliases.deploy").unwrap();
    assert_eq!(val.as_str().unwrap(), "apply --yes");
}

#[test]
fn config_set_scalar() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_test_config(dir.path());
    let contents = std::fs::read_to_string(&path).unwrap();
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
    let spec = raw.get_mut("spec").unwrap();

    let (parent, key) = config_cmd::walk_yaml_path_mut(spec, "profile").unwrap();
    parent.insert(
        serde_yaml::Value::String(key),
        config_cmd::parse_yaml_value("personal"),
    );

    let spec = raw.get("spec").unwrap();
    let val = config_cmd::walk_yaml_path(spec, "profile").unwrap();
    assert_eq!(val.as_str().unwrap(), "personal");
}

#[test]
fn config_set_creates_intermediates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cfgd.yaml");
    std::fs::write(
        &path,
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: base
"#,
    )
    .unwrap();
    let contents = std::fs::read_to_string(&path).unwrap();
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
    let spec = raw.get_mut("spec").unwrap();

    let (parent, key) = config_cmd::walk_yaml_path_mut(spec, "daemon.reconcile.interval").unwrap();
    parent.insert(
        serde_yaml::Value::String(key),
        config_cmd::parse_yaml_value("10m"),
    );

    let spec = raw.get("spec").unwrap();
    let val = config_cmd::walk_yaml_path(spec, "daemon.reconcile.interval").unwrap();
    assert_eq!(val.as_str().unwrap(), "10m");
}

#[test]
fn config_set_boolean_value() {
    let val = config_cmd::parse_yaml_value("true");
    assert_eq!(val, serde_yaml::Value::Bool(true));
    let val = config_cmd::parse_yaml_value("false");
    assert_eq!(val, serde_yaml::Value::Bool(false));
}

#[test]
fn config_set_number_value() {
    let val = config_cmd::parse_yaml_value("42");
    assert!(val.is_number());
    assert_eq!(val.as_i64().unwrap(), 42);
}

#[test]
fn config_set_string_value() {
    let val = config_cmd::parse_yaml_value("hello world");
    assert_eq!(val.as_str().unwrap(), "hello world");
}

#[test]
fn config_unset_removes_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_test_config(dir.path());
    let contents = std::fs::read_to_string(&path).unwrap();
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
    let spec = raw.get_mut("spec").unwrap();

    // Verify theme exists before removal
    assert!(
        config_cmd::walk_yaml_path(spec, "theme").is_ok(),
        "theme key should exist before unset"
    );

    let (parent, key) = config_cmd::walk_yaml_path_mut(spec, "theme").unwrap();
    let yaml_key = serde_yaml::Value::String(key);
    let removed = parent.remove(&yaml_key);
    assert!(removed.is_some(), "remove should return the removed value");

    // Verify theme is actually gone
    let spec = raw.get("spec").unwrap();
    assert!(
        config_cmd::walk_yaml_path(spec, "theme").is_err(),
        "theme key should not exist after unset"
    );
}

#[test]
fn config_unset_nested_alias() {
    let dir = tempfile::tempdir().unwrap();
    let path = make_test_config(dir.path());
    let contents = std::fs::read_to_string(&path).unwrap();
    let mut raw: serde_yaml::Value = serde_yaml::from_str(&contents).unwrap();
    let spec = raw.get_mut("spec").unwrap();

    // Verify deploy alias exists before removal
    assert!(
        config_cmd::walk_yaml_path(spec, "aliases.deploy").is_ok(),
        "deploy alias should exist before unset"
    );

    let (parent, key) = config_cmd::walk_yaml_path_mut(spec, "aliases.deploy").unwrap();
    let yaml_key = serde_yaml::Value::String(key);
    let removed = parent.remove(&yaml_key);
    assert!(removed.is_some(), "remove should return the removed value");

    // "add" alias should still exist, "deploy" should be gone
    let spec = raw.get("spec").unwrap();
    config_cmd::walk_yaml_path(spec, "aliases.add").unwrap();
    assert!(
        config_cmd::walk_yaml_path(spec, "aliases.deploy").is_err(),
        "deploy alias should not exist after unset"
    );
}

#[test]
fn theme_string_shorthand_deserializes() {
    let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  theme: dracula
"#;
    let cfg = config::parse_config(yaml, std::path::Path::new("cfgd.yaml")).unwrap();
    let theme = cfg.spec.theme.unwrap();
    assert_eq!(theme.name, "dracula");
    assert!(theme.overrides.is_empty());
}

#[test]
fn theme_struct_form_deserializes() {
    let yaml = "apiVersion: cfgd.io/v1alpha1\n\
                     kind: Config\n\
                     metadata:\n\
                     \x20 name: test\n\
                     spec:\n\
                     \x20 theme:\n\
                     \x20\x20\x20 name: dracula\n\
                     \x20\x20\x20 overrides:\n\
                     \x20\x20\x20\x20\x20 success: '#50fa7b'\n";
    let cfg = config::parse_config(yaml, std::path::Path::new("cfgd.yaml")).unwrap();
    let theme = cfg.spec.theme.unwrap();
    assert_eq!(theme.name, "dracula");
    assert_eq!(theme.overrides.success.as_deref(), Some("#50fa7b"));
}

// --- parse_file_spec ---

#[test]
fn parse_file_spec_with_colon() {
    let (src, tgt) = super::parse_file_spec("/tmp/a:/tmp/b").unwrap();
    assert_eq!(src, PathBuf::from("/tmp/a"));
    assert_eq!(tgt, PathBuf::from("/tmp/b"));
}

#[test]
fn parse_file_spec_no_colon() {
    let (src, tgt) = super::parse_file_spec("/tmp/a").unwrap();
    assert_eq!(src, tgt);
}

#[test]
fn parse_file_spec_empty_source() {
    assert!(super::parse_file_spec(":/tmp/b").is_err());
}

#[test]
fn parse_file_spec_empty_target() {
    assert!(super::parse_file_spec("/tmp/a:").is_err());
}

#[test]
fn parse_file_spec_windows_drive_letter() {
    // C:\Users\foo should NOT be split on the drive-letter colon
    let (src, tgt) = super::parse_file_spec(r"C:\Users\foo").unwrap();
    assert_eq!(src, PathBuf::from(r"C:\Users\foo"));
    assert_eq!(tgt, PathBuf::from(r"C:\Users\foo"));
}

#[test]
fn parse_file_spec_windows_source_target() {
    // source:target where both have drive letters
    let (src, tgt) = super::parse_file_spec(r"/home/a:C:\Users\b").unwrap();
    assert_eq!(src, PathBuf::from("/home/a"));
    assert_eq!(tgt, PathBuf::from(r"C:\Users\b"));
}

// --- add_to_gitignore ---

#[test]
fn add_to_gitignore_creates_file() {
    let dir = tempfile::tempdir().unwrap();
    super::add_to_gitignore(dir.path(), "secrets/key.enc").unwrap();
    let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(content.contains("secrets/key.enc"));
}

#[test]
fn add_to_gitignore_no_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    super::add_to_gitignore(dir.path(), "secrets/key.enc").unwrap();
    super::add_to_gitignore(dir.path(), "secrets/key.enc").unwrap();
    let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    let count = content.matches("secrets/key.enc").count();
    assert_eq!(count, 1);
}

// --- config_cmd::parse_yaml_value ---

#[test]
fn parse_yaml_value_bool_true() {
    assert_eq!(
        super::config_cmd::parse_yaml_value("true"),
        serde_yaml::Value::Bool(true)
    );
}

#[test]
fn parse_yaml_value_bool_false() {
    assert_eq!(
        super::config_cmd::parse_yaml_value("false"),
        serde_yaml::Value::Bool(false)
    );
}

#[test]
fn parse_yaml_value_null() {
    assert_eq!(
        super::config_cmd::parse_yaml_value("null"),
        serde_yaml::Value::Null
    );
    assert_eq!(
        super::config_cmd::parse_yaml_value("~"),
        serde_yaml::Value::Null
    );
}

#[test]
fn parse_yaml_value_integer() {
    assert_eq!(
        super::config_cmd::parse_yaml_value("42"),
        serde_yaml::Value::Number(42.into())
    );
}

#[test]
fn parse_yaml_value_string() {
    assert_eq!(
        super::config_cmd::parse_yaml_value("hello"),
        serde_yaml::Value::String("hello".into())
    );
}

// --- config_cmd::walk_yaml_path ---

#[test]
fn walk_yaml_path_root() {
    let value = serde_yaml::Value::String("hi".into());
    let result = super::config_cmd::walk_yaml_path(&value, ".").unwrap();
    assert_eq!(result, &value);
}

#[test]
fn walk_yaml_path_nested() {
    let yaml = "a:\n  b: 42\n";
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    let result = super::config_cmd::walk_yaml_path(&value, "a.b").unwrap();
    assert_eq!(result.as_i64(), Some(42));
}

#[test]
fn walk_yaml_path_missing_key() {
    let yaml = "a:\n  b: 42\n";
    let value: serde_yaml::Value = serde_yaml::from_str(yaml).unwrap();
    assert!(super::config_cmd::walk_yaml_path(&value, "a.c").is_err());
}

#[test]
fn walk_yaml_path_empty_segment() {
    let value = serde_yaml::Value::Null;
    assert!(super::config_cmd::walk_yaml_path(&value, "a..b").is_err());
}

// --- config_cmd::walk_yaml_path_mut ---

#[test]
fn walk_yaml_path_mut_creates_intermediate() {
    let mut value = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    let (parent, leaf) = super::config_cmd::walk_yaml_path_mut(&mut value, "a.b.c").unwrap();
    assert_eq!(leaf, "c");
    parent.insert(
        serde_yaml::Value::String("c".into()),
        serde_yaml::Value::String("val".into()),
    );
    let result = super::config_cmd::walk_yaml_path(&value, "a.b.c").unwrap();
    assert_eq!(result.as_str(), Some("val"));
}

// --- scan_profile_names / scan_module_names ---

#[test]
fn scan_profile_names_from_dir() {
    let dir = create_test_config_dir();
    let profiles_dir = dir.path().join("profiles");
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let names = super::scan_profile_names(&profiles_dir, &printer).unwrap();
    assert!(names.contains(&"default".to_string()));
    assert!(names.contains(&"work".to_string()));
}

#[test]
fn scan_profile_names_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let names = super::scan_profile_names(dir.path(), &printer).unwrap();
    assert_eq!(
        names,
        Vec::<String>::new(),
        "empty dir should yield empty profile list"
    );
}

#[test]
fn scan_profile_names_warns_and_skips_malformed() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("alpha.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: alpha\nspec:\n  packages: {}\n",
    )
    .unwrap();
    std::fs::write(
        profiles_dir.join("beta.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: beta\nspec:\n  packages: {}\n",
    )
    .unwrap();
    // Missing required apiVersion/kind/metadata so load_profile errors.
    std::fs::write(
        profiles_dir.join("bad.yaml"),
        "this: [is, not, a, profile\n",
    )
    .unwrap();

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let names = super::scan_profile_names(&profiles_dir, &printer).unwrap();

    assert_eq!(
        names,
        vec!["alpha".to_string(), "beta".to_string()],
        "good profiles scanned, malformed one absent"
    );

    let out = buf.lock().unwrap();
    assert!(
        out.contains("bad.yaml"),
        "warning must name the malformed profile path; got: {out:?}"
    );
    assert!(
        out.contains("Skipping profile"),
        "warning must use the 'Skipping profile' shape; got: {out:?}"
    );
}

#[test]
fn scan_profile_names_returns_stem_and_warns_on_divergent_metadata_name() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("work.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: other\nspec:\n  packages: {}\n",
    )
    .unwrap();

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let names = super::scan_profile_names(&profiles_dir, &printer).unwrap();

    assert_eq!(
        names,
        vec!["work".to_string()],
        "scan must return the resolvable filename stem, not the divergent metadata.name"
    );
    let out = buf.lock().unwrap();
    assert!(
        out.contains("metadata.name 'other'") && out.contains("using 'work'"),
        "divergence warn must name both the metadata.name and the stem in use; got: {out:?}"
    );
    assert!(
        out.contains("work.yaml"),
        "divergence warn must name the offending file; got: {out:?}"
    );
}

#[test]
fn scan_profile_names_warns_and_skips_ambiguous() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("alpha.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: alpha\nspec:\n  packages: {}\n",
    )
    .unwrap();
    make_ambiguous_profile(&profiles_dir, "amb", None);

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let names = super::scan_profile_names(&profiles_dir, &printer).unwrap();

    assert_eq!(
        names,
        vec!["alpha".to_string()],
        "ambiguous profile skipped, scan continues"
    );
    let out = buf.lock().unwrap();
    assert!(
        out.contains("Skipping profile 'amb'"),
        "warning must name the ambiguous profile; got: {out:?}"
    );
}

#[test]
fn scan_module_names_from_dir() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "test-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages: []\n",
    );
    let modules_dir = dir.path().join("modules");
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let names = super::scan_module_names(&modules_dir, &printer).unwrap();
    assert_eq!(names, vec!["test-mod"]);
}

#[test]
fn scan_module_names_nonexistent() {
    let dir = tempfile::tempdir().unwrap();
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let names = super::scan_module_names(&dir.path().join("nope"), &printer).unwrap();
    assert_eq!(
        names,
        Vec::<String>::new(),
        "nonexistent dir should yield empty module list"
    );
}

// --- copy_files_to_dir ---

#[test]
fn copy_files_to_dir_copies_and_symlinks() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("myfile.txt");
    std::fs::write(&source, "content").unwrap();
    let repo_dir = dir.path().join("repo");

    let results = super::copy_files_to_dir(&[source.display().to_string()], &repo_dir).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "myfile.txt");
    // File should be in repo
    assert!(repo_dir.join("myfile.txt").exists());
    // Original should now be a symlink
    assert!(source.symlink_metadata().unwrap().file_type().is_symlink());
}

#[test]
fn copy_files_to_dir_nonexistent_source_errors() {
    let dir = tempfile::tempdir().unwrap();
    let result = super::copy_files_to_dir(&["/nonexistent-12345/file".into()], dir.path());
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("File not found"),
        "expected 'File not found' error, got: {msg}"
    );
}

// --- workflow::generate_release_workflow_yaml ---

#[test]
fn generate_release_workflow_empty() {
    let yaml = super::workflow::generate_release_workflow_yaml(&[], &[], "master").unwrap();
    assert!(yaml.contains("placeholder:"));
    assert!(yaml.contains("No modules or profiles to tag yet"));
}

#[test]
fn generate_release_workflow_with_modules() {
    let yaml =
        super::workflow::generate_release_workflow_yaml(&["shell-tools".into()], &[], "master")
            .unwrap();
    assert!(yaml.contains("modules/shell-tools/**"));
    assert!(yaml.contains("tag-modules:"));
    assert!(!yaml.contains("placeholder:"));
}

#[test]
fn generate_release_workflow_with_profiles() {
    let yaml =
        super::workflow::generate_release_workflow_yaml(&[], &["work".into()], "master").unwrap();
    assert!(yaml.contains("profiles/work.yaml"));
    assert!(yaml.contains("tag-profiles:"));
}

#[test]
fn generate_release_workflow_detect_grep_covers_flat_and_bundle_forms() {
    let yaml =
        super::workflow::generate_release_workflow_yaml(&[], &["work".into()], "master").unwrap();
    // BRE alternation matches BOTH the anchored flat manifest
    // (profiles/work.yaml|yml, exactly) and the bundle directory
    // (profiles/work/...) while rejecting sibling prefixes
    // (profiles/work.app.yaml, profiles/work-extra/...).
    assert!(
        yaml.contains("grep -q '^profiles/work\\(\\.\\(yaml\\|yml\\)$\\|/\\)'"),
        "detect step must grep both manifest forms with anchored flat form, got:\n{yaml}"
    );
}

// GNU-grep-only: the emitted pattern uses GNU BRE extensions (`\|`, `$` in a
// group) and the generated workflow pins ubuntu-latest, so behavior is pinned
// against the real grep it will run under. BSD/macOS grep would not match.
#[cfg(target_os = "linux")]
#[test]
fn generate_release_workflow_detect_grep_behavior_against_real_grep() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let fixtures = "profiles/work.yaml\n\
                    profiles/work.yml\n\
                    profiles/work/profile.yaml\n\
                    profiles/work/files/x.sh\n\
                    profiles/work.app.yaml\n\
                    profiles/work.app/profile.yaml\n\
                    profiles/work-extra/profile.yaml\n\
                    profiles/workother.yaml\n\
                    profiles/work.yaml.bak\n";
    let run_grep = |pattern: &str| -> Vec<String> {
        let mut child = Command::new("grep")
            .arg(pattern)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn grep");
        child
            .stdin
            .take()
            .expect("grep stdin")
            .write_all(fixtures.as_bytes())
            .expect("write fixtures");
        let out = child.wait_with_output().expect("grep output");
        String::from_utf8(out.stdout)
            .expect("utf8 grep output")
            .lines()
            .map(str::to_string)
            .collect()
    };
    let extract_pattern = |yaml: &str, name: &str| -> String {
        let needle = format!("grep -q '^profiles/{}", name.replace('.', "\\."));
        let line = yaml
            .lines()
            .find(|l| l.contains(&needle))
            .unwrap_or_else(|| panic!("no detect grep for {name} in:\n{yaml}"));
        let start = line.find("grep -q '").expect("grep prefix") + "grep -q '".len();
        let end = line.rfind('\'').expect("closing quote");
        line[start..end].to_string()
    };

    let yaml = super::workflow::generate_release_workflow_yaml(
        &[],
        &["work".into(), "work.app".into()],
        "master",
    )
    .unwrap();

    assert_eq!(
        run_grep(&extract_pattern(&yaml, "work")),
        vec![
            "profiles/work.yaml",
            "profiles/work.yml",
            "profiles/work/profile.yaml",
            "profiles/work/files/x.sh",
        ],
        "work pattern must match exactly its own flat manifests and bundle subtree"
    );
    assert_eq!(
        run_grep(&extract_pattern(&yaml, "work.app")),
        vec!["profiles/work.app.yaml", "profiles/work.app/profile.yaml"],
        "work.app pattern must match its own forms without bleeding into work.*"
    );
}

#[test]
fn generate_release_workflow_escapes_dotted_profile_name() {
    let yaml = super::workflow::generate_release_workflow_yaml(&[], &["web.app".into()], "master")
        .unwrap();
    assert!(
        yaml.contains("grep -q '^profiles/web\\.app\\(\\.\\(yaml\\|yml\\)$\\|/\\)'"),
        "dot in profile name must be escaped in the detect grep, got:\n{yaml}"
    );
    // Output keys fold `.` to `_` — a literal dot would parse as a property
    // accessor inside ${{ ... }} expressions.
    assert!(yaml.contains("profile_web_app:"));
    assert!(!yaml.contains("profile_web.app"));
}

#[test]
fn generate_release_workflow_escapes_dotted_module_name() {
    let yaml =
        super::workflow::generate_release_workflow_yaml(&["my.mod".into()], &[], "master").unwrap();
    assert!(
        yaml.contains("grep -q '^modules/my\\.mod/'"),
        "dot in module name must be escaped in the detect grep, got:\n{yaml}"
    );
    assert!(yaml.contains("module_my_mod:"));
    assert!(!yaml.contains("module_my.mod"));
}

#[test]
fn generate_release_workflow_fails_on_output_key_collision() {
    // `web.app` and `web-app` both fold to `profile_web_app` — duplicate YAML
    // mapping keys that GitHub rejects at load, so generation must fail
    // naming both sources.
    let err = super::workflow::generate_release_workflow_yaml(
        &[],
        &["web.app".into(), "web-app".into()],
        "master",
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("profile_web_app")
            && msg.contains("profile 'web.app'")
            && msg.contains("profile 'web-app'"),
        "collision error must name the folded key and both sources, got: {msg}"
    );
}

#[test]
fn generate_release_workflow_module_key_collision_fails() {
    let err = super::workflow::generate_release_workflow_yaml(
        &["my.mod".into(), "my_mod".into()],
        &[],
        "master",
    )
    .unwrap_err();
    assert!(err.to_string().contains("module_my_mod"), "got: {err}");
}

#[test]
fn generate_release_workflow_module_and_profile_same_name_no_collision() {
    // Kind prefixes keep the key spaces disjoint: module `web` and profile
    // `web` coexist.
    let yaml =
        super::workflow::generate_release_workflow_yaml(&["web".into()], &["web".into()], "master")
            .unwrap();
    assert!(yaml.contains("module_web:") && yaml.contains("profile_web:"));
}

#[test]
fn scan_profile_names_skips_invalid_stem() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("work.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: work\nspec:\n  packages: {}\n",
    )
    .unwrap();
    // Hand-dropped file whose stem would inject a quote into the generated
    // workflow's single-quoted grep pattern.
    std::fs::write(
        profiles_dir.join("it's.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: its\nspec:\n  packages: {}\n",
    )
    .unwrap();

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let names = super::scan_profile_names(&profiles_dir, &printer).unwrap();

    assert_eq!(
        names,
        vec!["work".to_string()],
        "invalid stem must be skipped"
    );
    let out = buf.lock().unwrap();
    assert!(
        out.contains("Skipping profile") && out.contains("invalid characters"),
        "invalid stem must warn with the Skipping-profile shape; got: {out:?}"
    );
}

#[test]
fn scan_module_names_skips_invalid_stem() {
    let dir = tempfile::tempdir().unwrap();
    let modules_dir = dir.path().join("modules");
    let bad = modules_dir.join("it's");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("module.yaml"), "spec: {}\n").unwrap();
    let good = modules_dir.join("git");
    std::fs::create_dir_all(&good).unwrap();
    std::fs::write(good.join("module.yaml"), "spec: {}\n").unwrap();

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let names = super::scan_module_names(&modules_dir, &printer).unwrap();

    assert_eq!(
        names,
        vec!["git".to_string()],
        "invalid stem must be skipped"
    );
    let out = buf.lock().unwrap();
    assert!(
        out.contains("Skipping module") && out.contains("invalid characters"),
        "invalid stem must warn with the Skipping-module shape; got: {out:?}"
    );
}

#[test]
fn generate_release_workflow_both() {
    let yaml = super::workflow::generate_release_workflow_yaml(
        &["git-tools".into()],
        &["personal".into()],
        "master",
    )
    .unwrap();
    assert!(yaml.contains("tag-modules:"));
    assert!(yaml.contains("tag-profiles:"));
    assert!(yaml.contains("detect-changes:"));
}

// --- config_cmd::cmd_config_get / config_cmd::cmd_config_set / config_cmd::cmd_config_unset ---

#[test]
fn config_get_reads_value() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");
    std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

    let cli = Cli {
        config: config_path.clone(),
        config_explicit: false,
        ..test_cli(dir.path())
    };
    let printer = test_printer();

    let result = super::config_cmd::cmd_config_get(&cli, &printer, "profile");
    assert!(
        result.is_ok(),
        "config get should read profile value without error: {:?}",
        result.err()
    );

    // Verify the config file actually contains the expected profile
    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("profile: default"));
}

#[test]
fn cmd_config_get_missing_key_errors() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");
    std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

    let cli = Cli {
        config: config_path,
        config_explicit: false,
        ..test_cli(dir.path())
    };
    let printer = test_printer();

    assert!(super::config_cmd::cmd_config_get(&cli, &printer, "nonexistent").is_err());
}

#[test]
fn config_set_and_get_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");
    std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

    let cli = Cli {
        config: config_path.clone(),
        config_explicit: false,
        ..test_cli(dir.path())
    };
    let printer = test_printer();

    super::config_cmd::cmd_config_set(&cli, &printer, "profile", "work").unwrap();

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("work"));
}

#[test]
fn cmd_config_unset_removes_key() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");
    std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

    let cli = Cli {
        config: config_path.clone(),
        config_explicit: false,
        ..test_cli(dir.path())
    };
    let printer = test_printer();

    let result = super::config_cmd::cmd_config_unset(&cli, &printer, "profile");
    assert!(
        result.is_ok(),
        "config unset should remove the key successfully: {:?}",
        result.err()
    );

    // Verify the key was actually removed from the config file
    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !contents.contains("profile:"),
        "profile key should be removed from config"
    );
}

#[test]
fn cmd_config_unset_missing_key_errors() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");
    std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

    let cli = Cli {
        config: config_path,
        config_explicit: false,
        ..test_cli(dir.path())
    };
    let printer = test_printer();

    assert!(super::config_cmd::cmd_config_unset(&cli, &printer, "nope").is_err());
}

// --- config_cmd::cmd_config_show ---

#[test]
fn config_show_succeeds_with_valid_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");
    std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

    let cli = Cli {
        config: config_path,
        config_explicit: false,
        ..test_cli(dir.path())
    };
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    assert!(
        super::config_cmd::cmd_config_show(&cli, &printer).is_ok(),
        "config show should succeed when cfgd.yaml exists and is valid"
    );
    drop(printer);

    let output = cap.human();
    assert!(
        output.contains("Configuration"),
        "output should contain header, got: {output}"
    );
    assert!(
        output.contains("default"),
        "output should show profile, got: {output}"
    );
}

#[test]
fn config_show_errors_without_config() {
    let dir = tempfile::tempdir().unwrap();
    let cli = Cli {
        config: dir.path().join("nonexistent.yaml"),
        config_explicit: false,
        ..test_cli(dir.path())
    };

    assert!(super::config_cmd::cmd_config_show(&cli, &test_printer()).is_err());
}

// --- secret_backend_from_config ---

#[test]
fn secret_backend_defaults_to_sops() {
    let (backend, _) = super::secret_backend_from_config(None);
    assert_eq!(backend, "sops");
}

// --- expand_aliases ---

#[test]
fn expand_aliases_passthrough() {
    let args = vec!["cfgd".into(), "status".into()];
    let result = super::expand_aliases(args.clone());
    assert_eq!(result, args);
}

#[test]
fn expand_aliases_no_alias_passthrough() {
    // With empty builtin_aliases, no expansion happens
    let args = vec!["cfgd".into(), "apply".into(), "--dry-run".into()];
    let result = super::expand_aliases(args.clone());
    assert_eq!(result, args);
}

#[test]
fn expand_aliases_expands_user_defined_alias_from_config_file() {
    // The actual expansion hot path (mod.rs:158-162) — every other expand_aliases
    // test exercises a passthrough branch. A user-defined alias loaded from
    // --config <yaml> should replace the alias token with its expansion tokens,
    // and surrounding args (globals before, trailing args after) must survive
    // verbatim so the user's argv contract isn't silently rearranged.
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("cfgd.yaml");
    std::fs::write(
        &cfg_path,
        "\
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  aliases:
    add: \"profile update --file\"
",
    )
    .unwrap();

    let args = vec![
        "cfgd".into(),
        "--config".into(),
        cfg_path.to_string_lossy().into_owned(),
        "add".into(),
        "~/.zshrc".into(),
    ];
    let expanded = super::expand_aliases(args);

    assert_eq!(
        expanded,
        vec![
            "cfgd".to_string(),
            "--config".to_string(),
            cfg_path.to_string_lossy().into_owned(),
            "profile".to_string(),
            "update".to_string(),
            "--file".to_string(),
            "~/.zshrc".to_string(),
        ]
    );
}

#[test]
fn expand_aliases_unknown_alias_with_loaded_config_passes_through() {
    // Config IS loaded (so the config-load branch runs at mod.rs:137-146) but
    // the candidate token isn't in any alias — the function should still pass
    // through verbatim rather than partial-mangle the args.
    let dir = tempfile::tempdir().unwrap();
    let cfg_path = dir.path().join("cfgd.yaml");
    std::fs::write(
        &cfg_path,
        "\
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  aliases:
    add: \"profile update --file\"
",
    )
    .unwrap();

    let args = vec![
        "cfgd".into(),
        "--config".into(),
        cfg_path.to_string_lossy().into_owned(),
        "apply".into(),
        "--dry-run".into(),
    ];
    let expanded = super::expand_aliases(args.clone());
    assert_eq!(expanded, args);
}

// --- extract_config_path ---

#[test]
fn extract_config_path_explicit() {
    let args = vec![
        "cfgd".into(),
        "--config".into(),
        "/tmp/my.yaml".into(),
        "status".into(),
    ];
    assert_eq!(
        super::extract_config_path(&args),
        Some(PathBuf::from("/tmp/my.yaml"))
    );
}

#[test]
fn extract_config_path_equals() {
    let args = vec!["cfgd".into(), "--config=/tmp/my.yaml".into()];
    assert_eq!(
        super::extract_config_path(&args),
        Some(PathBuf::from("/tmp/my.yaml"))
    );
}

// --- resolve_profile_name ---

#[test]
fn resolve_profile_name_explicit_from_name() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let result = super::resolve_profile_name(&cli, &test_printer(), Some("work")).unwrap();
    assert_eq!(result, "work");
}

// --- parse_package_flag ---

#[test]
fn parse_package_flag_known_manager_splits() {
    let known = &["brew", "apt", "cargo"];
    let (mgr, pkg) = super::parse_package_flag("brew:ripgrep", known);
    assert_eq!(mgr, Some("brew".to_string()));
    assert_eq!(pkg, "ripgrep");
}

#[test]
fn parse_package_flag_unknown_manager_passthrough() {
    let known = &["brew", "apt"];
    let (mgr, pkg) = super::parse_package_flag("unknown:ripgrep", known);
    assert!(mgr.is_none());
    assert_eq!(pkg, "unknown:ripgrep");
}

#[test]
fn parse_package_flag_bare_name_passthrough() {
    let known = &["brew"];
    let (mgr, pkg) = super::parse_package_flag("ripgrep", known);
    assert!(mgr.is_none());
    assert_eq!(pkg, "ripgrep");
}

// --- builtin_aliases ---

#[test]
fn builtin_aliases_returns_map() {
    let aliases = super::builtin_aliases();
    assert_eq!(
        aliases.len(),
        0,
        "builtin_aliases should return an empty map (no built-in aliases yet)"
    );
}

// --- cmd_doctor basic ---

#[test]
fn cmd_doctor_with_valid_config() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let result = super::doctor::run_doctor(&cli, &printer);
    assert!(result.is_ok(), "doctor failed: {:?}", result.err());
    printer.flush();

    let output = buf.lock().unwrap();
    assert!(output.contains("Doctor"), "missing Doctor header");
    assert!(output.contains("Config file"), "missing config file status");
    assert!(
        output.contains("Package Managers"),
        "missing Package Managers section"
    );
}

#[test]
fn cmd_doctor_without_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("nonexistent.yaml");

    let cli = Cli {
        config: config_path,
        config_explicit: false,
        ..test_cli(dir.path())
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let result = super::doctor::run_doctor(&cli, &printer);
    // Missing at the DEFAULT path is the fresh-machine state: the verdict
    // must pass (exit 0) — pinned by S18.4-doctor in acceptance.
    assert!(
        result.as_ref().is_ok_and(|passed| *passed),
        "fresh-machine doctor verdict should pass, got: {result:?}"
    );
    printer.flush();

    let output = buf.lock().unwrap();
    assert!(output.contains("Doctor"), "missing Doctor header");
    assert!(
        output.contains("not found"),
        "should report config not found, got: {output}"
    );
    assert!(
        output.contains("run 'cfgd init' to create one"),
        "fresh-machine Warn should carry the init hint, got: {output}"
    );
}

#[test]
fn cmd_doctor_missing_config_at_explicit_path_fails_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("typo.yaml");

    let cli = Cli {
        config: config_path.clone(),
        config_explicit: true,
        ..test_cli(dir.path())
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let passed = super::doctor::run_doctor(&cli, &printer).unwrap();
    assert!(
        !passed,
        "missing config at an explicit --config path must fail the verdict"
    );
    printer.flush();

    let output = buf.lock().unwrap();
    assert!(
        output.contains(&format!(
            "Config file: {} — not found",
            config_path.display()
        )),
        "Fail line should name the explicit path, got: {output}"
    );
    assert!(
        output.contains("Some checks failed"),
        "verdict line should be the failure form, got: {output}"
    );
}

#[test]
fn cmd_doctor_json_missing_config_shape_is_unchanged() {
    // The typed DoctorConfigState must NOT leak into `-o json`: the config
    // object keeps its frozen five-key shape and the `error` string stays
    // "not found" for both the default-path and explicit-path cases.
    let expected_keys = ["error", "name", "path", "profile", "valid"];
    let formats = [
        cfgd_core::output::OutputFormat::Json,
        cfgd_core::output::OutputFormat::Yaml,
    ];
    for (format, explicit) in formats
        .into_iter()
        .flat_map(|f| [(f.clone(), false), (f, true)])
    {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli {
            config: dir.path().join("typo.yaml"),
            config_explicit: explicit,
            output: OutputFormatArg(format.clone()),
            ..test_cli(dir.path())
        };
        let (printer, buf) = cfgd_core::output::Printer::for_test_with_format(format.clone());
        super::doctor::run_doctor(&cli, &printer).unwrap();
        printer.flush();

        let output = buf.lock().unwrap();
        let parsed: serde_json::Value = match format {
            cfgd_core::output::OutputFormat::Json => extract_json(&output),
            _ => serde_yaml::from_str(output.trim()).unwrap(),
        };
        let config = parsed["config"].as_object().unwrap();
        let mut keys: Vec<&str> = config.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys, expected_keys,
            "config keys drifted (format={format:?}, explicit={explicit})"
        );
        assert_eq!(
            config["error"], "not found",
            "error string drifted (format={format:?}, explicit={explicit})"
        );
        assert_eq!(config["valid"], false);
    }
}

// --- Command handler tests (require state store) ---
//
// These test full command handlers that depend on the state store.
// Each test passes the state dir through the Cli struct (no env vars, no serial needed).

/// Set up a full test environment: config dir + state dir.
/// Returns (config_dir_tempdir, state_dir_tempdir).
/// Use `test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))` to get a Cli
/// that uses the state dir directly (no env vars, no cross-thread races).
fn setup_test_env() -> (tempfile::TempDir, tempfile::TempDir) {
    let config_dir = create_test_config_dir();
    std::fs::write(config_dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    // Create modules dir
    std::fs::create_dir_all(config_dir.path().join("modules")).unwrap();

    let state_dir = tempfile::tempdir().unwrap();
    (config_dir, state_dir)
}

#[test]
fn cmd_status_with_empty_state() {
    let h = CliTestHarness::builder().build();
    super::status::cmd_status(&h.cli(), h.printer(), None, false).unwrap();
    h.assert_header("Status");
    h.assert_output_contains("No applies recorded yet");
}

#[test]
fn cmd_status_module_not_found() {
    let h = CliTestHarness::builder().build();
    super::status::cmd_status(&h.cli(), h.printer(), Some("nonexistent"), false).unwrap();
    h.assert_output_contains("nonexistent");
}

#[test]
fn cmd_status_module_found() {
    let h = CliTestHarness::builder()
        .module("test-mod", SIMPLE_MODULE_YAML)
        .build();
    super::status::cmd_status(&h.cli(), h.printer(), Some("test-mod"), false).unwrap();
    h.assert_output_contains("test-mod");
}

#[test]
fn cmd_verify_module() {
    let h = CliTestHarness::builder()
        .module("test-mod", SIMPLE_MODULE_YAML)
        .build();
    super::verify::cmd_verify(&h.cli(), h.printer(), Some("test-mod"), false).unwrap();
    h.assert_header("Verify");
    // An empty module (no packages, no files) has nothing to verify. The former
    // blanket "module healthy" row was removed (it contradicted folded-in
    // file-drift rows), so the honest verdict is "no managed resources".
    let output = h.output();
    assert!(
        output.contains("No managed resources to verify"),
        "empty module must report nothing to verify, got: {output}"
    );
}

#[test]
fn cmd_log_with_empty_state() {
    let h = CliTestHarness::builder().build();
    super::log::cmd_log(h.printer(), 10, None, Some(h.state_path())).unwrap();
    h.assert_header("Apply History");
    h.assert_output_contains("No applies recorded yet");
}

#[test]
fn cmd_apply_dry_run_empty_profile() {
    let h = CliTestHarness::builder().build();
    let args = ApplyArgs {
        from: None,
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    super::apply::cmd_apply(&h.cli(), h.printer(), &args).unwrap();
    h.assert_header("Plan");
    let output = h.output();
    assert!(
        output.contains("Nothing to do") || output.contains("action(s) planned"),
        "should indicate plan result, got: {output}"
    );

    // Dry-run should NOT create any state store records
    let state = StateStore::open(&h.state_path().join("state.db")).unwrap();
    assert!(
        state.last_apply().unwrap().is_none(),
        "dry-run should not create apply records in state store"
    );
}

#[test]
fn cmd_apply_from_flag_parses() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();
    let args = ApplyArgs {
        from: Some("https://github.com/example/config.git".to_string()),
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: Some("dev-tools".to_string()),
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    // cmd_apply should attempt to resolve the --from URL.
    // The URL is unreachable so this will fail — but it must fail because
    // the URL was attempted (resolve_from path), not because --from was ignored.
    let result = super::apply::cmd_apply(&cli, &printer, &args);
    // Either succeeds (local config found) or fails on the URL — both prove --from was wired up
    if let Err(ref e) = result {
        let msg = e.to_string();
        // The error should relate to git/clone/network, NOT to missing config or parse errors
        assert!(
            !msg.contains("not found") || msg.contains("clone"),
            "--from error should be about git clone, not missing config: {msg}"
        );
    }
}

// Unix-only: the literal-`~` clean-failure contract is a HOME semantic. On
// Windows the config and state dirs resolve from the %APPDATA%/%LOCALAPPDATA%
// known folders via `directories::BaseDirs`, independent of HOME, so unsetting
// HOME does not strand resolution and apply does not error — a different (and
// correct) Windows contract.
#[cfg(unix)]
#[test]
#[serial_test::serial]
fn run_apply_home_unset_errors_and_creates_no_state() {
    use cfgd_core::test_helpers::EnvVarGuard;

    // Resolve home through neither a thread-local override nor HOME: config
    // discovery must then surface a clean error and no state.db may be created.
    let _home = EnvVarGuard::unset("HOME");
    let _xdg_cfg = EnvVarGuard::unset("XDG_CONFIG_HOME");
    let _xdg_data = EnvVarGuard::unset("XDG_DATA_HOME");
    let _state_env = EnvVarGuard::unset("CFGD_STATE_DIR");

    // The default config path keeps a literal `~` once home cannot be resolved.
    let config = super::default_config_file();
    assert!(
        config.starts_with("~"),
        "with HOME unset the default config path stays literal `~`, got: {}",
        config.display()
    );

    let cli = Cli {
        config: config.clone(),
        config_explicit: false,
        profile: None,
        no_color: true,
        verbose: 0,
        quiet: true,
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Table),
        list_envelope: false,
        jsonpath: None,
        state_dir: None,
        config_dir: None,
        cache_dir: None,
        runtime_dir: None,
        scope_arg: crate::cli::ScopeArg::User,
        command: Some(Command::Status {
            module: None,
            exit_code: false,
        }),
    };
    let printer = test_printer();
    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    let err = super::apply::run_apply(&cli, &printer, &args)
        .expect_err("apply with no resolvable home must error before any side-effect");
    let msg = err.to_string();
    assert!(
        msg.contains("HOME") || msg.contains("config"),
        "error must name the missing config / unresolved HOME, got: {msg}"
    );
    // No actionable advice can come from a literal `~` — it must not leak.
    assert!(
        !msg.trim_end().ends_with('~'),
        "error must not end on a bare, unactionable `~`: {msg}"
    );

    // State resolution shares the home policy, so the default state dir is
    // unresolvable and no orphan state.db is created.
    assert!(
        cfgd_core::state::default_state_dir().is_err(),
        "state dir must be unresolvable when home cannot be resolved"
    );
}

#[test]
fn cmd_apply_dry_run_with_phase_filter() {
    let h = CliTestHarness::builder().build();
    let args = ApplyArgs {
        from: None,
        dry_run: true,
        phase: Some(ApplyPhase::Packages),
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&h.cli(), h.printer(), &args).unwrap();
    h.assert_header("Plan");
    let output = h.output();
    assert!(
        output.contains("Nothing to do") || output.contains("Packages"),
        "should mention nothing-to-do or the filtered phase, got: {output}"
    );
}

// cmd_apply_dry_run_invalid_phase test removed — ApplyPhase is a clap
// ValueEnum, so invalid phase names are rejected at parse time and
// can no longer reach cmd_apply at runtime.

#[test]
fn cmd_apply_dry_run_with_skip() {
    let h = CliTestHarness::builder().build();
    let args = ApplyArgs {
        from: None,
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec!["packages".to_string()],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&h.cli(), h.printer(), &args).unwrap();
    h.assert_header("Plan");
    let output = h.output();
    assert!(
        output.contains("Nothing to do") || output.contains("Phase:"),
        "should mention plan or nothing to do, got: {output}"
    );
}

#[test]
fn cmd_apply_dry_run_with_only() {
    let h = CliTestHarness::builder().build();
    let args = ApplyArgs {
        from: None,
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec!["files".to_string()],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&h.cli(), h.printer(), &args).unwrap();
    h.assert_header("Plan");
    let output = h.output();
    assert!(
        output.contains("Nothing to do") || output.contains("Phase:"),
        "should mention plan or nothing to do, got: {output}"
    );
}

#[test]
fn cmd_apply_real_with_empty_profile() {
    let h = CliTestHarness::builder()
            .config("apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: empty\n")
            .profile("empty", "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: empty\nspec:\n  inherits: []\n  modules: []\n")
            .build();
    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&h.cli(), h.printer(), &args).unwrap();
    h.assert_header("Apply");
    h.assert_output_contains("Nothing to do");

    let state = StateStore::open(&h.state_path().join("state.db")).unwrap();
    assert!(
        state.last_apply().unwrap().is_none(),
        "empty profile apply should not create apply records (nothing to do)"
    );
}

#[test]
fn cmd_status_after_apply() {
    // The harness doesn't support buffer clearing, so use raw setup for multi-step tests
    let (config_dir, state_dir) = setup_test_env();
    let empty_profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: empty\nspec:\n  inherits: []\n  modules: []\n";
    std::fs::write(
        config_dir.path().join("profiles").join("empty.yaml"),
        empty_profile,
    )
    .unwrap();
    std::fs::write(config_dir.path().join("cfgd.yaml"), "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: empty\n").unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&cli, &printer, &args).unwrap();

    super::status::cmd_status(&cli, &printer, None, false).unwrap();
    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Status"),
        "should contain Status heading, got: {output}"
    );
}

#[test]
fn cmd_log_after_apply() {
    let (config_dir, state_dir) = setup_test_env();
    let empty_profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: empty\nspec:\n  inherits: []\n  modules: []\n";
    std::fs::write(
        config_dir.path().join("profiles").join("empty.yaml"),
        empty_profile,
    )
    .unwrap();
    std::fs::write(config_dir.path().join("cfgd.yaml"), "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: empty\n").unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&cli, &printer, &args).unwrap();

    let (log_printer, log_buf) = test_printer_capture();
    super::log::cmd_log(&log_printer, 10, None, Some(state_dir.path())).unwrap();
    drop(log_printer);
    let output = log_buf.lock().unwrap();
    assert!(
        output.contains("Apply History"),
        "should contain Apply History header, got: {output}"
    );
    assert!(
        output.contains("No applies recorded yet"),
        "empty profile creates no history records, got: {output}"
    );
}

#[test]
fn cmd_verify_empty_profile() {
    let h = CliTestHarness::builder().build();
    super::verify::cmd_verify(&h.cli(), h.printer(), None, false).unwrap();
    h.assert_header("Verify");
}

#[test]
fn cmd_diff_empty_profile() {
    let h = CliTestHarness::builder().build();
    super::diff::cmd_diff(&h.cli(), h.printer(), None, false).unwrap();
    h.assert_header("Diff");
}

#[test]
fn cmd_apply_dry_run_with_files() {
    let (config_dir, state_dir) = setup_test_env();

    // Create a source file
    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("test.txt"), "hello world").unwrap();

    let target = config_dir.path().join("output").join("test.txt");

    // Profile with a file
    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: withfile\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/test.txt\n        target: {}\n",
        target.display()
    );
    std::fs::write(
        config_dir.path().join("profiles").join("withfile.yaml"),
        &profile,
    )
    .unwrap();
    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: withfile\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = ApplyArgs {
        from: None,
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    let result = super::apply::cmd_apply(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "dry-run apply with files should succeed: {:?}",
        result.err()
    );
    // File should NOT be created (dry-run)
    assert!(!target.exists());

    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Plan"),
        "should contain Plan header, got: {output}"
    );
    assert!(
        output.contains("create") || output.contains("test.txt"),
        "should show file plan action, got: {output}"
    );
}

#[test]
fn cmd_apply_creates_file() {
    let (config_dir, state_dir) = setup_test_env();

    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("test.txt"), "applied content").unwrap();

    let target = config_dir.path().join("output").join("test.txt");

    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: withfile\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/test.txt\n        target: {}\n        strategy: Copy\n",
        target.display()
    );
    std::fs::write(
        config_dir.path().join("profiles").join("withfile.yaml"),
        &profile,
    )
    .unwrap();
    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: withfile\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();
    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    let result = super::apply::cmd_apply(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "apply should succeed and create the target file: {:?}",
        result.err()
    );
    // File SHOULD be created
    assert!(target.exists());
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "applied content");
}

#[test]
fn cmd_apply_idempotent() {
    let (config_dir, state_dir) = setup_test_env();

    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("test.txt"), "content").unwrap();

    let target = config_dir.path().join("output").join("test.txt");

    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: withfile\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/test.txt\n        target: {}\n        strategy: Copy\n",
        target.display()
    );
    std::fs::write(
        config_dir.path().join("profiles").join("withfile.yaml"),
        &profile,
    )
    .unwrap();
    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: withfile\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    // First apply
    super::apply::cmd_apply(&cli, &printer, &args).unwrap();
    assert!(target.exists());

    // Clear buffers before second apply
    buf.lock().unwrap().clear();

    // Second apply — should succeed with nothing to do
    let result = super::apply::cmd_apply(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "second apply (idempotent) should succeed with nothing to do: {:?}",
        result.err()
    );

    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Nothing to do"),
        "second apply should say nothing to do, got: {output}"
    );
}

#[test]
fn cmd_diff_with_files() {
    let (config_dir, state_dir) = setup_test_env();

    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("test.txt"), "desired content").unwrap();

    let target_dir = config_dir.path().join("output");
    std::fs::create_dir_all(&target_dir).unwrap();
    let target = target_dir.join("test.txt");
    std::fs::write(&target, "current content").unwrap();

    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: withfile\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/test.txt\n        target: {}\n        strategy: Copy\n",
        target.display()
    );
    std::fs::write(
        config_dir.path().join("profiles").join("withfile.yaml"),
        &profile,
    )
    .unwrap();
    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: withfile\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();

    let result = super::diff::cmd_diff(&cli, &printer, None, false);
    assert!(result.is_ok(), "diff failed: {:?}", result.err());

    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(output.contains("Diff"), "missing Diff header");
    assert!(
        output.contains("-current content") || output.contains("+desired content"),
        "output should contain diff lines showing content change, got: {output}"
    );
}

#[test]
fn cmd_status_structured_output() {
    let h = CliTestHarness::builder().json().build();
    super::status::cmd_status(&h.cli(), h.printer(), None, false).unwrap();
    let parsed = h.json_output();
    assert!(
        parsed.get("lastApply").is_some() || parsed.get("modules").is_some(),
        "JSON should contain status fields, got: {parsed}"
    );
}

#[test]
fn cmd_log_structured_output() {
    let h = CliTestHarness::builder().json().build();
    super::log::cmd_log(h.printer(), 5, None, Some(h.state_path())).unwrap();
    let parsed = h.json_output();
    assert_eq!(
        parsed,
        serde_json::json!({"entries": []}),
        "fresh state should produce exactly {{entries: []}}"
    );
}

#[test]
fn execute_with_no_subcommand_prints_help_and_returns_ok() {
    // Pinned contract — winget / chocolatey validators smoke-test the
    // installed binary with no arguments and treat any non-zero exit code as
    // failure. `cfgd` with no subcommand MUST exit 0 and emit a help banner.
    // See the comment at execute()'s top in cli/mod.rs.
    let h = CliTestHarness::builder().build();
    let cli = Cli {
        config: h.config_path().join("cfgd.yaml"),
        config_explicit: false,
        profile: None,
        no_color: true,
        verbose: 0,
        quiet: false,
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Table),
        list_envelope: false,
        jsonpath: None,
        state_dir: Some(h.state_path().to_path_buf()),
        config_dir: None,
        cache_dir: None,
        runtime_dir: None,
        scope_arg: crate::cli::ScopeArg::User,
        command: None,
    };
    // The contract: exit 0 (Ok). winget/chocolatey treat any non-zero exit
    // from `<bin>` (no args) as a failed install. Clap's `print_help()`
    // writes directly to stdout (not through Printer), so we don't assert
    // on captured output here — exit-code 0 is the part of the contract that
    // moves the needle if it regresses.
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("no-subcommand must return Ok(())");
}

#[test]
fn execute_status_command() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Status {
        module: None,
        exit_code: false,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    h.assert_header("Status");
}

#[test]
fn execute_log_command() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Log {
        limit: 10,
        show_output: None,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    let output = h.output();
    assert!(
        output.contains("Apply History") || output.contains("No applies"),
        "execute Log should show history, got: {output}"
    );
}

#[test]
fn execute_verify_command() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Verify {
        module: None,
        exit_code: false,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    h.assert_header("Verify");
}

#[test]
fn execute_diff_command() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Diff {
        module: None,
        exit_code: false,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    h.assert_header("Diff");
}

#[test]
fn execute_doctor_command() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Doctor);
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    h.assert_header("Doctor");
}

#[test]
fn execute_profile_list() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Profile {
        command: ProfileCommand::List,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    h.assert_output_contains("default");
}

#[test]
fn execute_profile_show() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Profile {
        command: ProfileCommand::Show { name: None },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    h.assert_output_contains("default");
}

#[test]
fn execute_config_show() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Config {
        command: ConfigCommand::Show,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    h.assert_header("Configuration");
}

#[test]
fn execute_config_get() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Config {
        command: ConfigCommand::Get {
            key: "profile".to_string(),
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    h.assert_output_contains("default");
}

#[test]
fn execute_config_set() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Config {
        command: ConfigCommand::Set {
            key: "profile".to_string(),
            value: "work".to_string(),
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();

    let cfg = config::load_config(&h.config_path().join("cfgd.yaml")).unwrap();
    assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
}

#[test]
fn execute_apply_dry_run() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Apply(ApplyArgs {
        from: None,
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    }));
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    let output = h.output();
    assert!(
        output.contains("Plan") || output.contains("Nothing"),
        "execute Apply dry-run should show plan or nothing-to-do, got: {output}"
    );
}

#[test]
fn execute_completions_bash() {
    let dir = tempfile::tempdir().unwrap();
    let cli = Cli {
        command: Some(Command::Completion {
            shell: clap_complete::Shell::Bash,
        }),
        ..test_cli(dir.path())
    };
    let printer = test_printer();
    // Completions write directly to stdout via clap_complete, not through Printer.
    // We verify execution succeeds; output content is clap_complete's responsibility.
    let result = super::execute(&cli, &printer, &super::paths::DirSources::all_default());
    assert!(
        result.is_ok(),
        "bash completions failed: {:?}",
        result.err()
    );
}

#[test]
fn execute_completions_zsh() {
    let dir = tempfile::tempdir().unwrap();
    let cli = Cli {
        command: Some(Command::Completion {
            shell: clap_complete::Shell::Zsh,
        }),
        ..test_cli(dir.path())
    };
    let printer = test_printer();
    let result = super::execute(&cli, &printer, &super::paths::DirSources::all_default());
    assert!(result.is_ok(), "zsh completions failed: {:?}", result.err());
}

#[test]
fn execute_completions_fish() {
    let dir = tempfile::tempdir().unwrap();
    let cli = Cli {
        command: Some(Command::Completion {
            shell: clap_complete::Shell::Fish,
        }),
        ..test_cli(dir.path())
    };
    let printer = test_printer();
    let result = super::execute(&cli, &printer, &super::paths::DirSources::all_default());
    assert!(
        result.is_ok(),
        "fish completions failed: {:?}",
        result.err()
    );
}

#[test]
fn execute_explain_command() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Explain {
        resource: Some("config".to_string()),
        recursive: false,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    let output = h.output();
    assert!(
        output.contains("Config") || output.contains("cfgd.yaml"),
        "explain config should describe config resource, got: {output}"
    );
}

#[test]
fn execute_explain_profile() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Explain {
        resource: Some("profile".to_string()),
        recursive: false,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    let output = h.output();
    assert!(
        output.contains("Profile") || output.contains("profile"),
        "explain profile should describe profile resource, got: {output}"
    );
}

#[test]
fn execute_explain_module() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Explain {
        resource: Some("module".to_string()),
        recursive: false,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    let output = h.output();
    assert!(
        output.contains("Module") || output.contains("module"),
        "explain module should describe module resource, got: {output}"
    );
}

#[test]
fn execute_explain_no_resource_json_format_writes_structured_array() {
    // Structured emit routes the index Doc's with_data payload (Vec<ExplainOutput>)
    // through Printer::emit → emit_structured, producing a top-level JSON array.
    let h = CliTestHarness::builder().json().build();
    let cli = h.cli_with_command(Command::Explain {
        resource: None,
        recursive: false,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    let output = h.output();
    assert!(
        output.trim().starts_with('[') && output.contains("\"kind\""),
        "should emit JSON array of schemas: {output}"
    );
}

#[test]
fn execute_explain_resource_json_format_writes_structured_object() {
    // Structured emit routes the schema Doc's with_data payload (ExplainOutput)
    // through Printer::emit → emit_structured, producing a top-level JSON object.
    let h = CliTestHarness::builder().json().build();
    let cli = h.cli_with_command(Command::Explain {
        resource: Some("module".to_string()),
        recursive: false,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    let output = h.output();
    assert!(
        output.trim().starts_with('{') && output.contains("\"kind\""),
        "should emit JSON object: {output}"
    );
}

#[test]
fn execute_explain_no_resource() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Explain {
        resource: None,
        recursive: false,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    let output = h.output();
    assert!(
        output.contains("Available resource types")
            || output.contains("NAME")
            || output.contains("config"),
        "explain (all resources) should list available resource types, got: {output}"
    );
}

#[test]
fn cmd_apply_with_module_filter() {
    let (config_dir, state_dir) = setup_test_env();

    // Create a module
    create_module_in_dir(
        config_dir.path(),
        "test-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n",
    );

    // Profile referencing the module
    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - test-mod\n";
    std::fs::write(
        config_dir.path().join("profiles").join("default.yaml"),
        profile,
    )
    .unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = ApplyArgs {
        from: None,
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: Some("test-mod".to_string()),
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    let result = super::apply::cmd_apply(&cli, &printer, &args);
    assert!(result.is_ok(), "apply failed: {:?}", result.err());

    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Plan") || output.contains("test-mod") || output.contains("Nothing"),
        "apply with module filter should reference module or show plan, got: {output}"
    );
}

#[test]
fn cmd_apply_with_env_vars() {
    let (config_dir, state_dir) = setup_test_env();

    // Profile with env vars
    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n    - name: PAGER\n      value: less\n  modules: []\n";
    std::fs::write(
        config_dir.path().join("profiles").join("default.yaml"),
        profile,
    )
    .unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    let result = super::apply::cmd_apply(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "apply should succeed when profile contains env vars: {:?}",
        result.err()
    );

    printer.flush();
    {
        let output = buf.lock().unwrap().clone();
        assert!(
            output.contains("Apply"),
            "should contain Apply header, got: {output}"
        );
        assert!(
            output.contains("Plan preview") || output.contains("Nothing to do"),
            "should mention plan preview or nothing to do, got: {output}"
        );
    }

    // Verify the profile was loaded with env vars by loading config+profile
    let (_, _, resolved) = super::load_config_and_profile(&cli).unwrap();
    assert!(
        resolved.merged.env.iter().any(|e| e.name == "EDITOR"),
        "resolved profile should contain EDITOR env var"
    );
    assert!(
        resolved.merged.env.iter().any(|e| e.name == "PAGER"),
        "resolved profile should contain PAGER env var"
    );
}

#[test]
fn cmd_status_with_modules() {
    let (config_dir, state_dir) = setup_test_env();

    create_module_in_dir(
        config_dir.path(),
        "test-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test-mod\nspec:\n  packages:\n    - name: curl\n",
    );

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - test-mod\n";
    std::fs::write(
        config_dir.path().join("profiles").join("default.yaml"),
        profile,
    )
    .unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    assert!(
        super::status::cmd_status(&cli, &printer, None, false).is_ok(),
        "status should succeed when profile references modules"
    );

    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(output.contains("Status"), "missing Status heading");
    assert!(
        output.contains("test-mod"),
        "output should list module test-mod, got: {output}"
    );
}

#[test]
fn cmd_status_with_drift_events() {
    let (config_dir, state_dir) = setup_test_env();

    let empty_profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: empty\nspec:\n  inherits: []\n  modules: []\n";
    std::fs::write(
        config_dir.path().join("profiles").join("empty.yaml"),
        empty_profile,
    )
    .unwrap();
    let empty_config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: empty\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), empty_config).unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&cli, &printer, &args).unwrap();

    // Record a drift event
    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    state
        .record_drift(
            "package",
            "curl",
            Some("installed"),
            Some("missing"),
            "local",
        )
        .unwrap();

    // Clear buffer before status call

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    super::status::cmd_status(&cli, &printer, None, false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("curl"),
        "drift output should mention resource 'curl', got: {output}"
    );
}

// --- Source command tests ---

#[test]
fn cmd_source_list_no_sources() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    assert!(
        super::source::cmd_source_list(&cli, &printer).is_ok(),
        "source list should succeed when no sources are configured"
    );

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Sources") || output.contains("No sources"),
        "output should show Sources header or no-sources message, got: {output}"
    );
}

#[test]
fn cmd_source_list_no_config() {
    let (_config_dir, state_dir) = setup_test_env();

    let dir = tempfile::tempdir().unwrap();
    let cli = Cli {
        config: dir.path().join("nonexistent.yaml"),
        config_explicit: false,
        ..test_cli_with_state(dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    assert!(
        super::source::cmd_source_list(&cli, &printer).is_ok(),
        "source list should succeed even without cfgd.yaml"
    );

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Sources") || output.contains("No sources"),
        "output should show sources info even without config, got: {output}"
    );
}

// --- Decide command tests ---

#[test]
fn cmd_decide_accept_all_empty() {
    let (_config_dir, state_dir) = setup_test_env();

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let result = super::decide::cmd_decide(
        &printer,
        super::DecideAction::Accept,
        None,
        None,
        true,
        Some(state_dir.path()),
    );
    assert!(result.is_ok(), "decide failed: {:?}", result.err());
    drop(printer);

    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    assert!(state.pending_decisions().unwrap().is_empty());

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No pending") || output.contains("0 decision"),
        "should report no pending decisions, got: {output}"
    );
}

#[test]
fn cmd_decide_reject_all_empty() {
    let (_config_dir, state_dir) = setup_test_env();

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let result = super::decide::cmd_decide(
        &printer,
        super::DecideAction::Reject,
        None,
        None,
        true,
        Some(state_dir.path()),
    );
    assert!(result.is_ok(), "decide failed: {:?}", result.err());
    drop(printer);

    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    assert!(state.pending_decisions().unwrap().is_empty());

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No pending") || output.contains("0 decision"),
        "should report no pending decisions, got: {output}"
    );
}

// cmd_decide_invalid_action test removed — invalid actions are now
// rejected by clap at parse time via the DecideAction ValueEnum, so
// there is no runtime code path for "Unknown action" to exercise.

#[test]
fn cmd_decide_accept_specific_resource() {
    let (_config_dir, state_dir) = setup_test_env();

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let result = super::decide::cmd_decide(
        &printer,
        super::DecideAction::Accept,
        Some("packages.brew.curl"),
        None,
        false,
        Some(state_dir.path()),
    );
    assert!(result.is_ok(), "decide failed: {:?}", result.err());
    drop(printer);

    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    let pending = state.pending_decisions().unwrap();
    assert_eq!(pending.len(), 0, "no decisions should remain pending");

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No pending decision")
            || output.contains("ACCEPTED")
            || output.contains("reconcile"),
        "decide accept should mention acceptance or no-pending state, got: {output}"
    );
}

#[test]
fn cmd_decide_reject_by_source() {
    let (_config_dir, state_dir) = setup_test_env();

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let result = super::decide::cmd_decide(
        &printer,
        super::DecideAction::Reject,
        None,
        Some("acme"),
        false,
        Some(state_dir.path()),
    );
    assert!(
        result.is_ok(),
        "decide reject-by-source should succeed (no-op when nothing pending for source): {:?}",
        result.err()
    );
    drop(printer);

    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    let pending = state.pending_decisions().unwrap();
    assert_eq!(
        pending.len(),
        0,
        "no decisions should remain pending after reject"
    );

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No pending decisions")
            || output.contains("REJECTED")
            || output.contains("acme"),
        "decide reject should mention rejection or source name, got: {output}"
    );
}

// --- Profile commands via execute ---

// profile create/delete tested via existing module_create tests above
#[test]
fn execute_profile_switch() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        command: Some(Command::Profile {
            command: ProfileCommand::Switch {
                name: "work".to_string(),
            },
        }),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };

    assert!(
        super::execute(
            &cli,
            &test_printer(),
            &super::paths::DirSources::all_default()
        )
        .is_ok(),
        "execute should dispatch Profile Switch command successfully"
    );

    // Verify config updated
    let cfg = config::load_config(&config_dir.path().join("cfgd.yaml")).unwrap();
    assert_eq!(cfg.spec.profile.as_deref(), Some("work"));
}

// --- Module commands via execute ---

#[test]
fn execute_module_list() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        command: Some(Command::Module {
            command: ModuleCommand::List,
        }),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    super::execute(&cli, &printer, &super::paths::DirSources::all_default()).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Modules") || output.contains("No modules"),
        "module list should show modules header, got: {output}"
    );
}

#[test]
fn execute_workflow_generate() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        command: Some(Command::Workflow {
            command: WorkflowCommand::Generate { force: false },
        }),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) = test_printer_capture();

    super::execute(&cli, &printer, &super::paths::DirSources::all_default()).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("workflow")
            || output.contains("Workflow")
            || output.contains("Generated")
            || output.contains("No profiles"),
        "workflow generate should mention workflow or generation result, got: {output}"
    );
}

// --- Sync/Pull without sources (no-op) ---

#[test]
fn cmd_sync_no_sources() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();

    super::sync::cmd_sync(&cli, &printer).unwrap();

    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("No sources") || output.contains("Sync"),
        "sync with no sources should report no-sources or show header, got: {output}"
    );
}

#[test]
fn cmd_pull_no_sources() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();

    super::pull::cmd_pull(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No sources") || output.contains("Pull") || output.contains("no origin"),
        "pull with no sources should report no-sources, got: {output}"
    );
}

// --- Apply with all phases ---

#[test]
fn cmd_apply_dry_run_each_phase() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let all_phases = [
        ApplyPhase::PreScripts,
        ApplyPhase::Env,
        ApplyPhase::Modules,
        ApplyPhase::Packages,
        ApplyPhase::System,
        ApplyPhase::Files,
        ApplyPhase::Secrets,
        ApplyPhase::PostScripts,
    ];
    for phase in all_phases {
        let args = ApplyArgs {
            from: None,
            dry_run: true,
            phase: Some(phase),
            yes: true,
            skip: vec![],
            only: vec![],
            module: None,
            skip_scripts: false,
            context: "apply".to_string(),
            shell: None,
        };
        let result = super::apply::cmd_apply(&cli, &printer, &args);
        assert!(
            result.is_ok(),
            "dry-run failed for phase: {}",
            phase.as_str()
        );
    }
    // Verify all 8 phase names are accepted (no unknown-phase errors)
    assert_eq!(all_phases.len(), 8);
}

// --- Verify after real apply ---

#[test]
fn cmd_verify_after_apply_with_env() {
    let (config_dir, state_dir) = setup_test_env();

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n  modules: []\n";
    std::fs::write(
        config_dir.path().join("profiles").join("default.yaml"),
        profile,
    )
    .unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&cli, &printer, &args).unwrap();

    let (verify_printer, verify_buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    super::verify::cmd_verify(&cli, &verify_printer, None, false).unwrap();
    verify_printer.flush();

    let output = verify_buf.lock().unwrap();
    assert!(
        output.contains("Verify"),
        "verify after apply should show Verify header, got: {output}"
    );
}

#[test]
fn output_format_arg_parse_basic() {
    use super::OutputFormatArg;
    let table: OutputFormatArg = "table".parse().unwrap();
    assert_eq!(table.0, cfgd_core::output::OutputFormat::Table);
    let wide: OutputFormatArg = "wide".parse().unwrap();
    assert_eq!(wide.0, cfgd_core::output::OutputFormat::Wide);
    let json: OutputFormatArg = "json".parse().unwrap();
    assert_eq!(json.0, cfgd_core::output::OutputFormat::Json);
    let yaml: OutputFormatArg = "yaml".parse().unwrap();
    assert_eq!(yaml.0, cfgd_core::output::OutputFormat::Yaml);
    let name: OutputFormatArg = "name".parse().unwrap();
    assert_eq!(name.0, cfgd_core::output::OutputFormat::Name);
}

#[test]
fn output_format_arg_parse_data_carrying() {
    use super::OutputFormatArg;
    let jp: OutputFormatArg = "jsonpath=.items[*].name".parse().unwrap();
    assert_eq!(
        jp.0,
        cfgd_core::output::OutputFormat::Jsonpath(".items[*].name".to_string())
    );
    let tmpl: OutputFormatArg = "template={{ name }}".parse().unwrap();
    assert_eq!(
        tmpl.0,
        cfgd_core::output::OutputFormat::Template("{{ name }}".to_string())
    );
    let tf: OutputFormatArg = "template-file=/tmp/report.tera".parse().unwrap();
    assert_eq!(
        tf.0,
        cfgd_core::output::OutputFormat::TemplateFile(std::path::PathBuf::from("/tmp/report.tera"))
    );
}

#[test]
fn output_format_arg_parse_error() {
    use super::OutputFormatArg;
    let result: Result<OutputFormatArg, _> = "invalid".parse();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("unknown output format"));
}

#[test]
fn output_format_arg_rejects_malformed_jsonpath() {
    use super::OutputFormatArg;
    // `jsonpath={.items[` must be rejected at parse time so clap surfaces a
    // usage error rather than letting the walker run (it once panicked here).
    let result: Result<OutputFormatArg, _> = "jsonpath={.items[".parse();
    assert!(result.is_err(), "malformed jsonpath must error, not Ok");
    let err = result.unwrap_err();
    assert!(
        err.contains("jsonpath"),
        "error should name jsonpath, got: {err:?}"
    );
}

#[test]
fn output_format_arg_accepts_wellformed_jsonpath() {
    use super::OutputFormatArg;
    for expr in [
        "jsonpath={[0].name}",
        "jsonpath={.items[*].name}",
        "jsonpath={.drift}",
    ] {
        assert!(
            expr.parse::<OutputFormatArg>().is_ok(),
            "expected {expr:?} to parse"
        );
    }
}

#[test]
fn output_format_arg_rejects_malformed_template() {
    use super::OutputFormatArg;
    let result: Result<OutputFormatArg, _> = "template={{range}".parse();
    assert!(result.is_err(), "malformed template must error, not Ok");
    let err = result.unwrap_err();
    assert!(
        err.contains("template"),
        "error should name template, got: {err:?}"
    );
}

#[test]
fn output_format_arg_accepts_wellformed_template() {
    use super::OutputFormatArg;
    assert!("template={{ name }}".parse::<OutputFormatArg>().is_ok());
}

// --- cmd_plan tests ---

#[test]
fn cmd_plan_empty_profile() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    };

    super::plan::cmd_plan(&cli, &printer, &args).unwrap();
    printer.flush();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Plan") || output.contains("Phase"),
        "plan should show Plan header or phase info, got: {output}"
    );
}

#[test]
fn cmd_plan_reconcile_context() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "reconcile".to_string(),
    };

    super::plan::cmd_plan(&cli, &printer, &args).unwrap();
    printer.flush();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Plan") || output.contains("Phase"),
        "plan with reconcile context should show plan info, got: {output}"
    );
}

#[test]
fn cmd_plan_invalid_context() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "bogus".to_string(),
    };

    let result = super::plan::cmd_plan(&cli, &printer, &args);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("Unknown context"));
}

#[test]
fn cmd_plan_with_phase_filter() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = PlanArgs {
        from: None,
        phase: Some(ApplyPhase::Packages),
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    };

    super::plan::cmd_plan(&cli, &printer, &args).unwrap();
    printer.flush();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Plan") || output.contains("Packages"),
        "plan with phase filter should show plan, got: {output}"
    );
}

// cmd_plan_invalid_phase test removed — ApplyPhase is a clap
// ValueEnum, so invalid phase names are rejected at parse time and
// can no longer reach cmd_plan at runtime.

#[test]
fn cmd_plan_with_skip_filter() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec!["packages".to_string()],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    };

    super::plan::cmd_plan(&cli, &printer, &args).unwrap();
    printer.flush();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Plan") || output.contains("Phase"),
        "plan with skip filter should show plan, got: {output}"
    );
}

#[test]
fn cmd_plan_with_only_filter() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec!["files".to_string()],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    };

    super::plan::cmd_plan(&cli, &printer, &args).unwrap();
    printer.flush();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Plan") || output.contains("Phase"),
        "plan with only filter should show plan, got: {output}"
    );
}

#[test]
fn cmd_plan_with_skip_scripts() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: true,
        context: "apply".to_string(),
    };

    super::plan::cmd_plan(&cli, &printer, &args).unwrap();
    printer.flush();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Plan") || output.contains("Phase"),
        "plan with skip-scripts should show plan, got: {output}"
    );
}

#[test]
fn cmd_plan_with_module_filter() {
    let (config_dir, state_dir) = setup_test_env();

    create_module_in_dir(
        config_dir.path(),
        "plan-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: plan-mod\nspec:\n  packages: []\n",
    );

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: Some("plan-mod".to_string()),
        skip_scripts: false,
        context: "apply".to_string(),
    };

    super::plan::cmd_plan(&cli, &printer, &args).unwrap();
    printer.flush();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Plan") || output.contains("plan-mod"),
        "plan with module filter should reference module, got: {output}"
    );
}

// --- cmd_rollback tests ---

#[test]
fn cmd_rollback_invalid_id_empty_state() {
    let state_dir = tempfile::tempdir().unwrap();
    let printer = test_printer();

    let result = super::rollback::cmd_rollback(&printer, 9999, true, Some(state_dir.path()));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no apply found"));
}

#[test]
fn cmd_rollback_after_file_apply() {
    let (config_dir, state_dir) = setup_test_env();

    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("rollback-test.txt"), "rollback content").unwrap();

    let target = config_dir.path().join("output").join("rollback-test.txt");

    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: withfile\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/rollback-test.txt\n        target: {}\n        strategy: Copy\n",
        target.display()
    );
    std::fs::write(
        config_dir.path().join("profiles").join("withfile.yaml"),
        &profile,
    )
    .unwrap();
    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: withfile\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    // Apply to create the file
    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&cli, &printer, &args).unwrap();
    assert!(target.exists());

    // Get the apply ID from history
    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    let history = state.history(1).unwrap();
    assert!(
        !history.is_empty(),
        "apply should have created a history entry"
    );
    let apply_id = history[0].id;

    let (printer, buf) = test_printer_capture();
    let result = super::rollback::cmd_rollback(&printer, apply_id, true, Some(state_dir.path()));
    assert!(
        result.is_ok(),
        "rollback should succeed for valid apply ID: {:?}",
        result.err()
    );

    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Rollback") || output.contains("restore"),
        "rollback output should mention rollback or restoration, got: {output}"
    );
}

/// Stage a config + profile + applied file under tempdirs and return
/// (config_dir, state_dir, target file path, apply_id). Shared by the two
/// `cmd_rollback` prompt-driven tests below — extracted so the post-apply
/// setup churn doesn't get copy-pasted three times.
fn apply_one_file_and_record(
    name: &str,
) -> (
    tempfile::TempDir,
    tempfile::TempDir,
    std::path::PathBuf,
    i64,
) {
    let (config_dir, state_dir) = setup_test_env();
    let files_dir = config_dir.path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join(format!("{name}.txt")), "rollback content").unwrap();
    let target = config_dir.path().join("output").join(format!("{name}.txt"));

    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: withfile\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - source: files/{name}.txt\n        target: {}\n        strategy: Copy\n",
        target.display()
    );
    std::fs::write(
        config_dir.path().join("profiles").join("withfile.yaml"),
        &profile,
    )
    .unwrap();
    let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: withfile\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&cli, &printer, &args).unwrap();

    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    let history = state.history(1).unwrap();
    let apply_id = history[0].id;
    (config_dir, state_dir, target, apply_id)
}

#[test]
fn cmd_rollback_without_yes_and_prompt_confirmed_proceeds() {
    // yes=false + Confirm(true) drives the prompt-true branch — the
    // reconciler.rollback_apply call fires and the success message follows.
    let (_cd, state_dir, _target, apply_id) = apply_one_file_and_record("rb-yes-prompt");
    let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(true)],
        cfgd_core::output::Verbosity::Normal,
    );

    let result = super::rollback::cmd_rollback(&printer, apply_id, false, Some(state_dir.path()));
    assert!(
        result.is_ok(),
        "prompt-confirmed rollback must succeed: {:?}",
        result.err()
    );
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Rollback") || output.contains("restore"),
        "should produce rollback output: {output}"
    );
    assert!(
        !output.contains("Aborted"),
        "Aborted must not fire when prompt is true: {output}"
    );
}

#[test]
fn cmd_rollback_without_yes_and_prompt_declined_aborts() {
    // yes=false + Confirm(false) takes the early-return arm — "Aborted"
    // fires and reconciler.rollback_apply is never called.
    let (_cd, state_dir, _target, apply_id) = apply_one_file_and_record("rb-no-prompt");
    let (printer, buf) = cfgd_core::output::Printer::for_test_with_prompt_responses_at(
        vec![cfgd_core::output::PromptAnswer::Confirm(false)],
        cfgd_core::output::Verbosity::Normal,
    );

    let result = super::rollback::cmd_rollback(&printer, apply_id, false, Some(state_dir.path()));
    assert!(result.is_ok(), "prompt-declined rollback must return Ok");
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Aborted"),
        "should print Aborted when prompt is false: {output}"
    );
    assert!(
        !output.contains("Rollback complete"),
        "rollback body must NOT run when prompt is false: {output}"
    );
}

// --- cmd_compliance tests ---

#[test]
fn cmd_compliance_snapshot_basic() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    let result = super::compliance::cmd_compliance_snapshot(&cli, &printer);
    assert!(
        result.is_ok(),
        "compliance snapshot should succeed: {:?}",
        result.err()
    );

    // Verify snapshot was recorded in state store
    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    let entries = state.compliance_history(None, 10).unwrap();
    assert!(
        !entries.is_empty(),
        "compliance snapshot should create a history entry in state store"
    );

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Compliance") || output.contains("Snapshot"),
        "compliance snapshot should mention compliance or snapshot, got: {output}"
    );
}

#[test]
fn cmd_compliance_export_basic() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    let result = super::compliance::cmd_compliance_export(&cli, &printer);
    assert!(
        result.is_ok(),
        "compliance export failed: {:?}",
        result.err()
    );

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Compliance") || output.contains("compliance") || !output.is_empty(),
        "compliance export should produce output, got: {output}"
    );
}

#[test]
fn cmd_compliance_history_empty() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    let result = super::compliance::cmd_compliance_history(&cli, &printer, None);
    assert!(
        result.is_ok(),
        "compliance history should succeed with no snapshots: {:?}",
        result.err()
    );

    // Verify state store has no compliance entries
    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    let entries = state.compliance_history(None, 10).unwrap();
    assert_eq!(
        entries.len(),
        0,
        "compliance history should be empty when no snapshots have been taken"
    );

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("No compliance snapshots")
            || output.contains("History")
            || output.contains("Compliance"),
        "compliance history should mention no snapshots or show header, got: {output}"
    );
}

#[test]
fn cmd_compliance_history_with_since() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    let result = super::compliance::cmd_compliance_history(&cli, &printer, Some("7d"));
    assert!(
        result.is_ok(),
        "compliance history should succeed with --since 7d time filter: {:?}",
        result.err()
    );

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Compliance") || output.contains("History") || output.contains("No"),
        "compliance history with --since should produce output, got: {output}"
    );
}

#[test]
fn cmd_compliance_history_invalid_since() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result =
        super::compliance::cmd_compliance_history(&cli, &printer, Some("invalid-duration"));
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("invalid --since value"),
        "expected 'invalid --since value' error, got: {msg}"
    );
}

#[test]
fn cmd_compliance_diff_missing_snapshots() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::compliance::cmd_compliance_diff(&cli, &printer, 1, 2);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn cmd_compliance_diff_after_two_snapshots() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    // Create two snapshots
    super::compliance::cmd_compliance_snapshot(&cli, &printer).unwrap();
    super::compliance::cmd_compliance_snapshot(&cli, &printer).unwrap();

    // Get snapshot IDs from history — must have exactly 2
    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    let entries = state.compliance_history(None, 10).unwrap();
    assert_eq!(
        entries.len(),
        2,
        "two snapshots should create two history entries"
    );
    let result =
        super::compliance::cmd_compliance_diff(&cli, &printer, entries[1].id, entries[0].id);
    assert!(
        result.is_ok(),
        "compliance diff should succeed when comparing two valid snapshots: {:?}",
        result.err()
    );
}

#[test]
fn cmd_compliance_history_after_snapshot() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));

    // Take a snapshot first (separate printer so its output doesn't pollute the
    // history-capture assertions).
    let snap_printer = test_printer();
    super::compliance::cmd_compliance_snapshot(&cli, &snap_printer).unwrap();
    drop(snap_printer);

    // History should show at least one entry
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();
    let result = super::compliance::cmd_compliance_history(&cli, &printer, None);
    assert!(
        result.is_ok(),
        "compliance history should succeed after a snapshot was taken: {:?}",
        result.err()
    );

    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    let entries = state.compliance_history(None, 10).unwrap();
    assert_eq!(
        entries.len(),
        1,
        "should have exactly 1 compliance history entry after one snapshot"
    );

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Compliance") || output.contains("History"),
        "compliance history should display history header, got: {output}"
    );
}

// --- helpers: managers_map / module_state_map / default_device_id ---

#[test]
fn managers_map_round_trips_registry_managers_by_name() {
    // Build a registry, then check every name reachable via managers_map
    // matches a manager in the original registry.
    let registry = super::build_registry();
    let map = super::managers_map(&registry);
    assert!(
        !map.is_empty(),
        "registry must produce at least one manager"
    );
    for m in &registry.package_managers {
        assert!(
            map.contains_key(m.name()),
            "managers_map missing entry for {}",
            m.name()
        );
        // Trait-object identity via name is the contract — every value must
        // self-report the same name as the key.
        assert_eq!(map[m.name()].name(), m.name());
    }
}

#[test]
fn module_state_map_returns_empty_map_when_state_has_no_modules() {
    // Empty store → empty map. Pure read-only contract; the function falls
    // back to Vec::new() on any state-store Err and returns an empty map.
    let state = cfgd_core::test_helpers::test_state();
    let map = super::module_state_map(&state);
    assert!(
        map.is_empty(),
        "fresh state store should yield an empty module state map: {map:?}"
    );
}

#[test]
fn default_device_id_returns_the_hostname_string() {
    // The function is a thin wrapper around cfgd_core::hostname_string —
    // pin the contract that they return the same string verbatim.
    let id = super::default_device_id();
    assert_eq!(id, cfgd_core::hostname_string());
    assert!(!id.is_empty(), "device id must not be empty");
}

// --- empty_resolved_profile tests ---

#[test]
fn empty_resolved_profile_contains_module_name() {
    let resolved = super::empty_resolved_profile("my-module");
    assert_eq!(resolved.merged.modules, vec!["my-module".to_string()]);
    assert!(resolved.layers.is_empty());
    assert!(resolved.merged.packages.brew.is_none());
    assert!(resolved.merged.env.is_empty());
    assert!(resolved.merged.secrets.is_empty());
}

// --- cmd_log with show_output ---

#[test]
fn cmd_log_show_output_nonexistent_apply() {
    let state_dir = tempfile::tempdir().unwrap();
    let printer = test_printer();

    // show_output for a nonexistent apply ID should fail
    let result = super::log::cmd_log(&printer, 10, Some(9999), Some(state_dir.path()));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no apply found"));
}

// --- cmd_apply with skip_scripts ---

#[test]
fn cmd_apply_dry_run_with_skip_scripts() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = ApplyArgs {
        from: None,
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: true,
        context: "apply".to_string(),
        shell: None,
    };

    let result = super::apply::cmd_apply(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "dry-run apply should succeed with --skip-scripts flag: {:?}",
        result.err()
    );

    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Apply")
            || output.contains("Plan")
            || output.contains("Nothing")
            || output.contains("dry"),
        "dry-run apply with skip-scripts should produce output, got: {output}"
    );
}

// --- execute dispatch tests for new commands ---

#[test]
fn execute_plan_command() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        command: Some(Command::Plan(PlanArgs {
            from: None,
            phase: None,
            skip: vec![],
            only: vec![],
            module: None,
            skip_scripts: false,
            context: "apply".to_string(),
        })),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) = test_printer_capture();

    super::execute(&cli, &printer, &super::paths::DirSources::all_default()).unwrap();
    printer.flush();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Plan") || output.contains("Phase"),
        "execute Plan should show plan info, got: {output}"
    );
}

#[test]
fn execute_compliance_snapshot() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        command: Some(Command::Compliance { command: None }),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    super::execute(&cli, &printer, &super::paths::DirSources::all_default()).unwrap();
    printer.flush();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Compliance") || output.contains("snapshot"),
        "execute Compliance should show compliance info, got: {output}"
    );
}

#[test]
fn execute_compliance_export() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        command: Some(Command::Compliance {
            command: Some(ComplianceCommand::Export),
        }),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    super::execute(&cli, &printer, &super::paths::DirSources::all_default()).unwrap();
    printer.flush();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Compliance")
            || output.contains("compliance")
            || output.contains("Snapshot"),
        "compliance export should contain compliance data, got: {output}"
    );
}

#[test]
fn execute_compliance_history() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        command: Some(Command::Compliance {
            command: Some(ComplianceCommand::History { since: None }),
        }),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    super::execute(&cli, &printer, &super::paths::DirSources::all_default()).unwrap();
    printer.flush();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Compliance") || output.contains("History") || output.contains("No"),
        "compliance history should show info, got: {output}"
    );
}

#[test]
fn execute_rollback_invalid() {
    let state_dir = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();

    let cli = Cli {
        command: Some(Command::Rollback {
            apply_id: 9999,
            yes: true,
        }),
        ..test_cli_with_state(dir.path(), Some(state_dir.path().to_path_buf()))
    };

    let result = super::execute(
        &cli,
        &test_printer(),
        &super::paths::DirSources::all_default(),
    );
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("no apply found with ID 9999"),
        "expected 'no apply found with ID 9999' error, got: {msg}"
    );
}

// --- secret_backend_from_config with config ---

#[test]
fn secret_backend_from_config_with_backend() {
    let yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  secrets:
    backend: sops-age
"#;
    let cfg = config::parse_config(yaml, std::path::Path::new("cfgd.yaml")).unwrap();
    let (backend, _) = super::secret_backend_from_config(Some(&cfg));
    assert_eq!(backend, "sops-age");
}

// --- known_manager_names ---

#[test]
fn known_manager_names_is_not_empty() {
    let names = super::known_manager_names();
    assert!(!names.is_empty());
    // Should at least contain "cargo" which is always available in Rust projects
    assert!(names.contains(&"cargo".to_string()));
}

// --- Structured output mode tests ---

#[test]
fn cmd_plan_structured_json() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    };

    super::plan::cmd_plan(&cli, &printer, &args).unwrap();
    printer.flush();

    let output = buf.lock().unwrap();
    let parsed = extract_json(&output);
    assert!(
        parsed.get("context").is_some(),
        "plan JSON should have 'context'"
    );
    assert_eq!(parsed["context"], "apply", "plan context should be 'apply'");
    assert!(
        parsed.get("phases").is_some(),
        "plan JSON should have 'phases'"
    );
    assert!(parsed["phases"].is_array(), "phases should be an array");
    assert!(
        parsed.get("totalActions").is_some(),
        "plan JSON should have 'totalActions'"
    );
    assert!(
        parsed["totalActions"].is_u64(),
        "totalActions should be a numeric value"
    );
}

#[test]
fn cmd_verify_structured_json() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    super::verify::cmd_verify(&cli, &printer, None, false).unwrap();
    printer.flush();

    let output = buf.lock().unwrap();
    let parsed = extract_json(&output);
    assert!(
        parsed.get("results").is_some(),
        "verify JSON should have 'results'"
    );
    assert!(
        parsed.get("passCount").is_some(),
        "verify JSON should have 'passCount'"
    );
    assert!(
        parsed.get("failCount").is_some(),
        "verify JSON should have 'failCount'"
    );
    let results = parsed["results"].as_array().unwrap();
    let pass_count = parsed["passCount"].as_u64().unwrap();
    let fail_count = parsed["failCount"].as_u64().unwrap();
    assert_eq!(
        pass_count + fail_count,
        results.len() as u64,
        "passCount + failCount should equal results array length"
    );
}

#[test]
fn cmd_doctor_structured_json() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    super::doctor::run_doctor(&cli, &printer).unwrap();
    printer.flush();

    let output = buf.lock().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&output)
        .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {output}"));
    assert!(
        parsed.get("config").is_some(),
        "doctor JSON should have 'config' field, got: {parsed}"
    );
    assert_eq!(
        parsed["config"]["valid"], true,
        "config should be valid in doctor output"
    );
    assert!(
        parsed.get("git").is_some(),
        "doctor JSON should have 'git' field"
    );
    assert!(
        parsed.get("packageManagers").is_some(),
        "doctor JSON should have 'packageManagers' field"
    );
    assert!(
        parsed["packageManagers"].is_array(),
        "packageManagers should be an array"
    );
}

#[test]
fn cmd_compliance_snapshot_structured_json() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    super::compliance::cmd_compliance_snapshot(&cli, &printer).unwrap();

    drop(printer);
    let output = buf.lock().unwrap();
    let parsed = extract_json(&output);
    assert!(
        parsed.get("snapshot").is_some(),
        "compliance snapshot JSON should have 'snapshot' field, got: {parsed}"
    );
    let snapshot = &parsed["snapshot"];
    assert!(
        snapshot["timestamp"].is_string(),
        "snapshot.timestamp should be a string"
    );
    assert_eq!(
        snapshot["profile"], "default",
        "snapshot.profile should be 'default'"
    );
    assert!(
        snapshot["checks"].is_array(),
        "snapshot.checks should be an array"
    );
    assert!(
        snapshot["summary"].is_object(),
        "snapshot.summary should be an object"
    );
}

#[test]
fn cmd_compliance_history_structured_json() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    super::compliance::cmd_compliance_history(&cli, &printer, None).unwrap();

    drop(printer);
    let output = buf.lock().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(output.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {output}"));
    assert_eq!(
        parsed,
        serde_json::json!({"entries": []}),
        "fresh state should produce exactly {{entries: []}}"
    );
}

// --- cmd_diff with module filter ---

#[test]
fn cmd_diff_with_module_filter() {
    let (config_dir, state_dir) = setup_test_env();

    create_module_in_dir(
        config_dir.path(),
        "diff-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: diff-mod\nspec:\n  packages: []\n",
    );

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();

    let result = super::diff::cmd_diff(&cli, &printer, Some("diff-mod"), false);
    assert!(
        result.is_ok(),
        "diff should succeed when filtering to a specific module: {:?}",
        result.err()
    );

    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Diff") || output.contains("diff-mod"),
        "diff with module filter should mention the module, got: {output}"
    );
}

// --- cmd_verify with module filter on nonexistent module ---

#[test]
fn cmd_verify_module_not_found() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));

    // Nonexistent module should succeed gracefully (empty results, exit 0)
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let result = super::verify::cmd_verify(&cli, &printer, Some("nonexistent"), false);
    assert!(
        result.is_ok(),
        "verify should handle nonexistent module gracefully: {:?}",
        result.err()
    );
    printer.flush();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Verify") || output.contains("No managed"),
        "verify for nonexistent module should mention verify or no managed resources, got: {output}"
    );
}

// --- cmd_plan with module that has dependencies ---

#[test]
fn cmd_plan_module_with_packages() {
    let (config_dir, state_dir) = setup_test_env();

    create_module_in_dir(
        config_dir.path(),
        "pkg-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: pkg-mod\nspec:\n  packages:\n    - name: curl\n    - name: wget\n",
    );

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - pkg-mod\n";
    std::fs::write(
        config_dir.path().join("profiles").join("default.yaml"),
        profile,
    )
    .unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    };

    let result = super::plan::cmd_plan(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "plan should succeed when module contains packages: {:?}",
        result.err()
    );
    printer.flush();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Plan")
            || output.contains("Phase")
            || output.contains("Package")
            || output.contains("curl"),
        "plan output should contain plan info or package actions, got: {output}"
    );
}

// --- open_state_store ---

#[test]
fn open_state_store_creates_dir() {
    let dir = tempfile::tempdir().unwrap();
    let subdir = dir.path().join("nested").join("state");
    let result = super::open_state_store(Some(&subdir));
    assert!(
        result.is_ok(),
        "open_state_store should create nested directories: {:?}",
        result.err()
    );
    assert!(subdir.exists(), "nested state directory should be created");
    assert!(
        subdir.join("state.db").exists(),
        "DB file should exist in the created directory"
    );
}

#[test]
#[serial_test::serial(default_state_store)]
fn open_state_store_default() {
    // Verify the default path variant does not panic and creates a DB.
    // Serialized against other tests that touch the default DB path so
    // parallel SQLite access doesn't trigger 'database is locked'.
    let result = super::open_state_store(None);
    assert!(
        result.is_ok(),
        "open_state_store with default path should not panic: {:?}",
        result.err()
    );
    // Verify we can actually use the store
    let state = result.unwrap();
    assert!(
        state.history(1).is_ok(),
        "state store should be functional after opening"
    );
}

// --- build_registry ---

#[test]
fn build_registry_has_package_managers() {
    let registry = super::build_registry();
    assert_eq!(
        registry.package_managers.len(),
        20,
        "registry should have all 20 package managers"
    );
    let names: Vec<&str> = registry.package_managers.iter().map(|m| m.name()).collect();
    assert!(names.contains(&"brew"), "should include brew");
    assert!(names.contains(&"cargo"), "should include cargo");
    assert!(names.contains(&"apt"), "should include apt");
    assert!(names.contains(&"npm"), "should include npm");
}

#[test]
fn build_registry_has_system_configurators() {
    let registry = super::build_registry();
    // On Linux we get: shell, systemd, gsettings, kdeConfig, xfconf, environment, sshKeys,
    // plus conditionally gpg and git (both available in CI/dev). At minimum we should have
    // the unconditional ones.
    assert!(
        registry.system_configurators.len() >= 6,
        "registry should have at least 6 system configurators on Linux, got: {}",
        registry.system_configurators.len()
    );
    let names: Vec<&str> = registry
        .system_configurators
        .iter()
        .map(|c| c.name())
        .collect();
    assert!(
        names.contains(&"shell"),
        "should include shell configurator"
    );
    assert!(
        names.contains(&"sshKeys"),
        "should include sshKeys configurator"
    );
    assert!(
        names.contains(&"environment"),
        "should include environment configurator"
    );
}

#[test]
fn build_registry_has_secret_backend() {
    let registry = super::build_registry();
    assert!(
        registry.secret_backend.is_some(),
        "registry should have a secret backend"
    );
    assert_eq!(
        registry.secret_backend.as_ref().unwrap().name(),
        "sops",
        "default secret backend should be 'sops'"
    );
}

// --- config_dir helper ---

#[test]
fn config_dir_derives_from_cli_config() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let result = super::config_dir(&cli);
    assert_eq!(result, dir.path().to_path_buf());
}

// --- profiles_dir helper ---

#[test]
fn profiles_dir_derives_from_cli() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let result = super::profiles_dir(&cli);
    assert_eq!(result, dir.path().join("profiles"));
}

// =========================================================================
// Module handler tests
// =========================================================================

// --- cmd_module_list ---

#[test]
fn module_list_empty_config_dir() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("modules")).unwrap();
    let state_dir = dir.path().join("state");
    let cli = test_cli_with_state(dir.path(), Some(state_dir));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    module::cmd_module_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Modules") || output.contains("No modules"),
        "module list with empty dir should show header or no-modules, got: {output}"
    );
}

#[test]
fn module_list_with_modules() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "vim",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: vim\nspec:\n  packages:\n    - name: vim\n",
    );
    create_module_in_dir(
        dir.path(),
        "git",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: git\nspec:\n  packages:\n    - name: git\n    - name: git-lfs\n",
    );

    let state_dir = dir.path().join("state");
    let cli = test_cli_with_state(dir.path(), Some(state_dir));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    module::cmd_module_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(output.contains("vim"), "module list should contain 'vim'");
    assert!(output.contains("git"), "module list should contain 'git'");
}

#[test]
fn module_list_no_modules_dir() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = dir.path().join("state");
    let cli = test_cli_with_state(dir.path(), Some(state_dir));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    module::cmd_module_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No modules") || output.contains("Modules"),
        "module list without modules dir should show header or no-modules, got: {output}"
    );
}

#[test]
fn module_list_with_config_and_profile() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
    create_module_in_dir(
        dir.path(),
        "bat",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: bat\nspec:\n  packages:\n    - name: bat\n",
    );
    let profile_path = dir.path().join("profiles").join("default.yaml");
    let mut doc = config::load_profile(&profile_path).unwrap();
    doc.spec.modules.push("bat".to_string());
    let yaml = serde_yaml::to_string(&doc).unwrap();
    std::fs::write(&profile_path, &yaml).unwrap();

    let state_dir = dir.path().join("state");
    let cli = test_cli_with_state(dir.path(), Some(state_dir));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    module::cmd_module_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(output.contains("bat"), "module list should contain 'bat'");
}

// --- cmd_module_show ---

#[test]
fn module_show_not_found() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("modules")).unwrap();
    let state_dir = dir.path().join("state");
    let cli = test_cli_with_state(dir.path(), Some(state_dir));
    let printer = test_printer();

    let result = module::cmd_module_show(&cli, &printer, "nonexistent", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn module_show_with_packages_and_files() {
    let dir = tempfile::tempdir().unwrap();
    let mod_dir = dir.path().join("modules").join("dev-tools");
    std::fs::create_dir_all(mod_dir.join("files")).unwrap();
    std::fs::write(mod_dir.join("files").join("config.toml"), "content").unwrap();
    std::fs::write(
        mod_dir.join("module.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: dev-tools
  description: Development tools
spec:
  depends:
    - base
  packages:
    - name: ripgrep
    - name: fd
      prefer:
        - cargo
  files:
    - source: files/config.toml
      target: ~/.config/tool/config.toml
  env:
    - name: EDITOR
      value: nvim
  aliases:
    - name: ll
      command: ls -la
"#,
    )
    .unwrap();

    // Verify module was parsed correctly before testing show
    let (doc, _) = module::load_module_document(dir.path(), "dev-tools").unwrap();
    assert_eq!(doc.metadata.name, "dev-tools");
    assert_eq!(
        doc.metadata.description,
        Some("Development tools".to_string())
    );
    assert_eq!(doc.spec.depends, vec!["base"]);
    assert_eq!(doc.spec.packages.len(), 2);
    assert_eq!(doc.spec.packages[0].name, "ripgrep");
    assert_eq!(doc.spec.packages[1].name, "fd");
    assert_eq!(doc.spec.files.len(), 1);
    assert_eq!(doc.spec.env.len(), 1);
    assert_eq!(doc.spec.env[0].name, "EDITOR");
    assert_eq!(doc.spec.aliases.len(), 1);
    assert_eq!(doc.spec.aliases[0].name, "ll");

    let state_dir = dir.path().join("state");
    let cli = test_cli_with_state(dir.path(), Some(state_dir));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    module::cmd_module_show(&cli, &printer, "dev-tools", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("dev-tools"),
        "show should contain module name"
    );
    assert!(
        output.contains("ripgrep"),
        "show should list package ripgrep"
    );
    assert!(
        output.contains("config.toml"),
        "show should list file config.toml"
    );
    assert!(output.contains("EDITOR"), "show should list env var EDITOR");
}

#[test]
fn module_show_with_values_flag() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "secrets-mod",
        r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: secrets-mod
spec:
  env:
    - name: API_KEY
      value: super-secret-token-123
"#,
    );

    let state_dir = dir.path().join("state");
    let cli = test_cli_with_state(dir.path(), Some(state_dir));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    module::cmd_module_show(&cli, &printer, "secrets-mod", false).unwrap();
    {
        let output = buf.lock().unwrap();
        assert!(output.contains("API_KEY"), "show should list env var name");
    }

    // With show_values=true
    buf.lock().unwrap().clear();
    module::cmd_module_show(&cli, &printer, "secrets-mod", true).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("API_KEY"),
        "show with values should list env"
    );
}

#[test]
fn module_show_suggests_available_modules() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "vim",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: vim\nspec: {}\n",
    );
    let state_dir = dir.path().join("state");
    let cli = test_cli_with_state(dir.path(), Some(state_dir));
    let printer = test_printer();

    let result = module::cmd_module_show(&cli, &printer, "emacs", false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn module_show_with_scripts() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "scripted",
        r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: scripted
spec:
  packages:
    - name: curl
  scripts:
    postApply:
      - echo "done"
"#,
    );

    let state_dir = dir.path().join("state");
    let cli = test_cli_with_state(dir.path(), Some(state_dir));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    module::cmd_module_show(&cli, &printer, "scripted", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("scripted"),
        "show should contain module name"
    );
    assert!(output.contains("curl"), "show should list package curl");
    assert!(
        output.contains("Scripts") || output.contains("postApply"),
        "show should mention scripts, got: {output}"
    );
}

// --- cmd_module_create ---

#[test]
fn module_create_minimal() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleCreateArgs {
        description: Some("Minimal module".to_string()),
        ..test_module_create_args("minimal")
    };
    module::cmd_module_create(&cli, &printer, &args).unwrap();

    let module_yaml = dir
        .path()
        .join("modules")
        .join("minimal")
        .join("module.yaml");
    assert!(module_yaml.exists());
    let (doc, _) = module::load_module_document(dir.path(), "minimal").unwrap();
    assert_eq!(doc.metadata.name, "minimal");
    assert_eq!(doc.metadata.description, Some("Minimal module".to_string()));
    assert!(doc.spec.packages.is_empty());
    assert!(doc.spec.files.is_empty());
    assert!(doc.spec.depends.is_empty());
}

#[test]
fn module_create_with_env_and_aliases() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleCreateArgs {
        description: Some("Env module".to_string()),
        env: vec!["EDITOR=nvim".to_string(), "PAGER=less".to_string()],
        aliases: vec!["ll=ls -la".to_string(), "gs=git status".to_string()],
        ..test_module_create_args("env-mod")
    };
    module::cmd_module_create(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "env-mod").unwrap();
    assert_eq!(doc.spec.env.len(), 2);
    assert_eq!(doc.spec.env[0].name, "EDITOR");
    assert_eq!(doc.spec.env[0].value, "nvim");
    assert_eq!(doc.spec.env[1].name, "PAGER");
    assert_eq!(doc.spec.env[1].value, "less");
    assert_eq!(doc.spec.aliases.len(), 2);
    assert_eq!(doc.spec.aliases[0].name, "ll");
    assert_eq!(doc.spec.aliases[0].command, "ls -la");
}

#[test]
fn module_create_with_depends() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleCreateArgs {
        depends: vec!["base".to_string(), "core".to_string()],
        ..test_module_create_args("dep-mod")
    };
    module::cmd_module_create(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "dep-mod").unwrap();
    assert_eq!(doc.spec.depends, vec!["base", "core"]);
}

#[test]
fn module_create_with_post_apply_normalizes_escapes() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleCreateArgs {
        post_apply: vec!["echo \\!done".to_string()],
        ..test_module_create_args("script-mod")
    };
    module::cmd_module_create(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "script-mod").unwrap();
    let scripts = doc.spec.scripts.unwrap();
    assert_eq!(scripts.post_apply.len(), 1);
    // \! should be normalized to !
    assert_eq!(scripts.post_apply[0].run_str(), "echo !done");
}

#[test]
fn module_create_rejects_invalid_name() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = test_module_create_args(".bad-name");
    let result = module::cmd_module_create(&cli, &printer, &args);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("cannot start with '.' or '-'"),
        "expected name validation error, got: {msg}"
    );
}

#[test]
fn module_create_with_duplicate_file_basenames_fails() {
    let dir = tempfile::tempdir().unwrap();
    // Create two files with same basename in different directories
    let dir_a = dir.path().join("dir_a");
    let dir_b = dir.path().join("dir_b");
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();
    std::fs::write(dir_a.join("config.toml"), "a").unwrap();
    std::fs::write(dir_b.join("config.toml"), "b").unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleCreateArgs {
        files: vec![
            format!("{}:~/.config/a", dir_a.join("config.toml").display()),
            format!("{}:~/.config/b", dir_b.join("config.toml").display()),
        ],
        ..test_module_create_args("dup-files")
    };
    let result = module::cmd_module_create(&cli, &printer, &args);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Duplicate file basename")
    );
}

// --- cmd_module_update_local ---

#[test]
fn module_update_add_and_remove_env() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "env-mod",
        r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: env-mod
spec:
  env:
    - name: EDITOR
      value: vim
    - name: PAGER
      value: less
"#,
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleUpdateArgs {
        env: vec!["TERM=xterm".to_string(), "-PAGER".to_string()],
        ..empty_module_update_args("env-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "env-mod").unwrap();
    let names: Vec<&str> = doc.spec.env.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"EDITOR"));
    assert!(names.contains(&"TERM"));
    assert!(!names.contains(&"PAGER"));
}

#[test]
fn module_update_add_and_remove_aliases() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "alias-mod",
        r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: alias-mod
spec:
  aliases:
    - name: ll
      command: ls -la
    - name: gs
      command: git status
"#,
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleUpdateArgs {
        aliases: vec!["gd=git diff".to_string(), "-gs".to_string()],
        ..empty_module_update_args("alias-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "alias-mod").unwrap();
    let names: Vec<&str> = doc.spec.aliases.iter().map(|a| a.name.as_str()).collect();
    assert!(names.contains(&"ll"));
    assert!(names.contains(&"gd"));
    assert!(!names.contains(&"gs"));
}

#[test]
fn module_update_add_and_remove_depends() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "dep-mod",
        r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: dep-mod
spec:
  depends:
    - base
    - core
"#,
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleUpdateArgs {
        depends: vec!["tools".to_string(), "-core".to_string()],
        ..empty_module_update_args("dep-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "dep-mod").unwrap();
    assert!(doc.spec.depends.contains(&"base".to_string()));
    assert!(doc.spec.depends.contains(&"tools".to_string()));
    assert!(!doc.spec.depends.contains(&"core".to_string()));
}

#[test]
fn module_update_add_and_remove_post_apply() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "script-mod",
        r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: script-mod
spec:
  scripts:
    postApply:
      - echo setup
      - echo cleanup
"#,
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleUpdateArgs {
        post_apply: vec!["echo new-step".to_string(), "-echo cleanup".to_string()],
        ..empty_module_update_args("script-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "script-mod").unwrap();
    let scripts = doc.spec.scripts.unwrap();
    let script_strs: Vec<&str> = scripts.post_apply.iter().map(|s| s.run_str()).collect();
    assert!(script_strs.contains(&"echo setup"));
    assert!(script_strs.contains(&"echo new-step"));
    assert!(!script_strs.contains(&"echo cleanup"));
}

#[test]
fn module_update_description() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "desc-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: desc-mod\nspec: {}\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleUpdateArgs {
        description: Some("New description".to_string()),
        ..empty_module_update_args("desc-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "desc-mod").unwrap();
    assert_eq!(
        doc.metadata.description,
        Some("New description".to_string())
    );
}

#[test]
fn module_update_clear_description() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "desc-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: desc-mod\n  description: Old desc\nspec: {}\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleUpdateArgs {
        description: Some(String::new()),
        ..empty_module_update_args("desc-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "desc-mod").unwrap();
    assert_eq!(doc.metadata.description, None);
}

#[test]
fn module_update_no_changes() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "noop-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: noop-mod\nspec: {}\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    // No flags at all — should print "no changes" and succeed
    let args = empty_module_update_args("noop-mod");
    let result = module::cmd_module_update_local(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "module update with no flags should succeed (no-op): {:?}",
        result.err()
    );

    // Verify module YAML is unchanged
    let (doc, _) = module::load_module_document(dir.path(), "noop-mod").unwrap();
    assert_eq!(doc.metadata.name, "noop-mod");
    assert!(doc.spec.packages.is_empty());
}

#[test]
fn module_update_nonexistent_module_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = empty_module_update_args("nonexistent");
    let result = module::cmd_module_update_local(&cli, &printer, &args);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Module 'nonexistent' not found"),
        "expected module not found error, got: {msg}"
    );
}

#[test]
fn module_update_add_files() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "file-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: file-mod\nspec: {}\n",
    );

    // Create a file to import
    let source_file = dir.path().join("my-config.toml");
    std::fs::write(&source_file, "key = \"value\"").unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleUpdateArgs {
        files: vec![format!(
            "{}:~/.config/app/config.toml",
            source_file.display()
        )],
        ..empty_module_update_args("file-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "file-mod").unwrap();
    assert_eq!(doc.spec.files.len(), 1);
    assert!(doc.spec.files[0].source.contains("my-config.toml"));
}

#[test]
fn module_update_remove_files() {
    let dir = tempfile::tempdir().unwrap();
    let target_path = "/tmp/cfgd-test-file-target";
    let module_yaml = format!(
        r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: rm-file-mod
spec:
  files:
    - source: files/config.toml
      target: {}
"#,
        target_path
    );
    create_module_in_dir(dir.path(), "rm-file-mod", &module_yaml);
    // Create the source file in the module
    std::fs::write(
        dir.path()
            .join("modules")
            .join("rm-file-mod")
            .join("files")
            .join("config.toml"),
        "content",
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleUpdateArgs {
        files: vec![format!("-{}", target_path)],
        ..empty_module_update_args("rm-file-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "rm-file-mod").unwrap();
    assert_eq!(
        doc.spec.files.len(),
        0,
        "module spec should have no files after removal"
    );
    // Source file should also be removed
    assert!(
        !dir.path()
            .join("modules")
            .join("rm-file-mod")
            .join("files")
            .join("config.toml")
            .exists()
    );
}

#[test]
fn module_update_remove_nonexistent_warns() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "warn-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: warn-mod\nspec:\n  packages:\n    - name: curl\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Try removing items that don't exist — should still succeed (just warns)
    let args = ModuleUpdateArgs {
        packages: vec!["-nonexistent".to_string()],
        env: vec!["-MISSING".to_string()],
        aliases: vec!["-gone".to_string()],
        depends: vec!["-nodep".to_string()],
        ..empty_module_update_args("warn-mod")
    };
    let result = module::cmd_module_update_local(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "module update should succeed when removing nonexistent items (warns only): {:?}",
        result.err()
    );

    // Verify original package is still present (removals of non-existent items are no-ops)
    let (doc, _) = module::load_module_document(dir.path(), "warn-mod").unwrap();
    let names: Vec<&str> = doc.spec.packages.iter().map(|p| p.name.as_str()).collect();
    assert!(
        names.contains(&"curl"),
        "original package should still be present"
    );
}

// --- cmd_module_delete ---

#[test]
fn module_delete_nonexistent() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = module::cmd_module_delete(&cli, &printer, "nonexistent", true, false, false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn module_delete_cleans_lockfile() {
    let dir = create_test_config_dir();
    create_module_in_dir(
        dir.path(),
        "remote-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: remote-mod\nspec: {}\n",
    );

    // Create a lockfile with an entry for this module
    let lockfile_content = r#"modules:
  - name: remote-mod
    url: https://github.com/example/mod.git@v1.0
    pinnedRef: v1.0
    commit: abc123
    integrity: sha256-fake
"#;
    std::fs::write(dir.path().join("modules.lock"), lockfile_content).unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    module::cmd_module_delete(&cli, &printer, "remote-mod", true, false, false).unwrap();

    // Module directory should be gone
    assert!(
        !dir.path().join("modules").join("remote-mod").exists(),
        "module directory should be removed after delete"
    );

    // Lockfile should no longer contain the module
    let lockfile = cfgd_core::modules::load_lockfile(dir.path()).unwrap();
    assert_eq!(
        lockfile.modules.len(),
        0,
        "lockfile should have no module entries after delete"
    );
}

#[test]
#[cfg(unix)]
fn module_delete_restores_symlinked_files() {
    let dir = create_test_config_dir();

    // Create a target that's a symlink into the module dir
    let target_dir = dir.path().join("targets");
    std::fs::create_dir_all(&target_dir).unwrap();

    let module_dir = dir.path().join("modules").join("link-mod");
    let files_dir = module_dir.join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("config.txt"), "module content").unwrap();

    let target_file = target_dir.join("config.txt");
    // Create symlink from target -> module file
    std::os::unix::fs::symlink(files_dir.join("config.txt"), &target_file).unwrap();

    let module_yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: link-mod\nspec:\n  files:\n    - source: files/config.txt\n      target: {}\n",
        target_file.display()
    );
    std::fs::write(module_dir.join("module.yaml"), &module_yaml).unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    module::cmd_module_delete(&cli, &printer, "link-mod", true, false, false).unwrap();

    // Module dir gone
    assert!(!module_dir.exists());
    // Target should have been restored as a regular file
    assert!(target_file.exists());
    assert!(!target_file.is_symlink());
    assert_eq!(
        std::fs::read_to_string(&target_file).unwrap(),
        "module content"
    );
}

// --- cmd_module_export ---

#[test]
fn module_export_devcontainer_via_wrapper() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "export-mod",
        r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: export-mod
spec:
  packages:
    - name: jq
"#,
    );

    let output = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = module::cmd_module_export(
        &cli,
        &printer,
        "export-mod",
        &ExportFormat::Devcontainer,
        Some(output.path().to_str().unwrap()),
    );
    assert!(
        result.is_ok(),
        "module export devcontainer should succeed: {:?}",
        result.err()
    );

    let install_sh = output.path().join("export-mod").join("install.sh");
    assert!(install_sh.exists(), "install.sh should be created");
    let install_content = std::fs::read_to_string(&install_sh).unwrap();
    assert!(
        !install_content.is_empty(),
        "install.sh should have content"
    );

    let feature_json = output
        .path()
        .join("export-mod")
        .join("devcontainer-feature.json");
    assert!(
        feature_json.exists(),
        "devcontainer-feature.json should be created"
    );
    let feature_content = std::fs::read_to_string(&feature_json).unwrap();
    assert!(
        feature_content.contains("export-mod"),
        "devcontainer-feature.json should reference the module name"
    );
}

#[test]
fn module_export_nonexistent_module() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("modules")).unwrap();
    let cli = test_cli(dir.path());

    let printer = test_printer();
    let result = module::cmd_module_export(
        &cli,
        &printer,
        "nonexistent",
        &ExportFormat::Devcontainer,
        None,
    );
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Module 'nonexistent' not found"),
        "expected module not found error, got: {msg}"
    );
}

#[test]
fn module_export_devcontainer_with_env_and_depends() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "full-mod",
        r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: full-mod
  description: A full module
spec:
  depends:
    - base
  packages:
    - name: curl
    - name: custom-tool
      script: curl -sL https://example.com/install.sh | sh
  env:
    - name: EDITOR
      value: vim
  scripts:
    postApply:
      - echo setup complete
"#,
    );

    let output = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    module::cmd_module_export(
        &cli,
        &printer,
        "full-mod",
        &ExportFormat::Devcontainer,
        Some(output.path().to_str().unwrap()),
    )
    .unwrap();

    let install =
        std::fs::read_to_string(output.path().join("full-mod").join("install.sh")).unwrap();
    assert!(install.contains("curl"));
    assert!(install.contains("EDITOR"));
    assert!(install.contains("setup complete"));
    assert!(install.contains("curl -sL https://example.com/install.sh | sh"));

    let feature_json = std::fs::read_to_string(
        output
            .path()
            .join("full-mod")
            .join("devcontainer-feature.json"),
    )
    .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&feature_json).unwrap();
    assert_eq!(parsed["description"], "A full module");
    let installs_after = parsed["installsAfter"].as_array().unwrap();
    assert_eq!(installs_after.len(), 1);
    assert!(installs_after[0].as_str().unwrap().contains("base"));
}

// --- cmd_module_registry_list ---

#[test]
fn module_registry_list_no_config() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let (printer, buf) = test_printer_capture();

    module::cmd_module_registry_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No registries") || output.contains("Registries"),
        "registry list without config should show message, got: {output}"
    );
}

#[test]
fn module_registry_list_empty_registries() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) = test_printer_capture();

    module::cmd_module_registry_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Registries") || output.contains("No registries"),
        "registry list with none should report no registries, got: {output}"
    );
}

#[test]
fn module_registry_list_with_registries() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  modules:
    registries:
      - name: community
        url: https://github.com/cfgd-modules/community.git
      - name: private
        url: git@github.com:my-org/modules.git
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) = test_printer_capture();

    module::cmd_module_registry_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("community"),
        "registry list should contain 'community'"
    );
    assert!(
        output.contains("private"),
        "registry list should contain 'private'"
    );
}

// --- cmd_module_registry_add ---

#[test]
fn module_registry_add_no_config() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = module::cmd_module_registry_add(
        &cli,
        &printer,
        "https://github.com/example/modules.git",
        None,
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("config file not found")
    );
}

#[test]
fn module_registry_add_success() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    module::cmd_module_registry_add(
        &cli,
        &printer,
        "https://github.com/cfgd-community/modules.git",
        Some("community"),
    )
    .unwrap();

    let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
    let registries = cfg.spec.modules.unwrap().registries;
    assert_eq!(registries.len(), 1);
    assert_eq!(registries[0].name, "community");
    assert_eq!(
        registries[0].url,
        "https://github.com/cfgd-community/modules.git"
    );
}

#[test]
fn module_registry_add_duplicate() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  modules:
    registries:
      - name: community
        url: https://github.com/cfgd-community/modules.git
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Adding the same registry again should succeed (idempotent) but not duplicate
    let result = module::cmd_module_registry_add(
        &cli,
        &printer,
        "https://github.com/cfgd-community/modules.git",
        Some("community"),
    );
    assert!(
        result.is_ok(),
        "adding duplicate registry should succeed (idempotent): {:?}",
        result.err()
    );

    let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
    let registries = cfg.spec.modules.unwrap().registries;
    assert_eq!(registries.len(), 1);
}

#[test]
fn module_registry_add_second_registry() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  modules:
    registries:
      - name: community
        url: https://github.com/cfgd-community/modules.git
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    module::cmd_module_registry_add(
        &cli,
        &printer,
        "git@github.com:my-org/private-modules.git",
        Some("private"),
    )
    .unwrap();

    let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
    let registries = cfg.spec.modules.unwrap().registries;
    assert_eq!(registries.len(), 2);
    assert_eq!(registries[1].name, "private");
}

// --- cmd_module_registry_remove ---

#[test]
fn module_registry_remove_no_config() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = module::cmd_module_registry_remove(&cli, &printer, "community", false);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("config file not found"),
        "expected typed no-config error, got: {msg}"
    );
}

#[test]
fn module_registry_remove_success() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  modules:
    registries:
      - name: community
        url: https://github.com/cfgd-community/modules.git
      - name: private
        url: git@github.com:my-org/modules.git
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    module::cmd_module_registry_remove(&cli, &printer, "community", false).unwrap();

    let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
    let registries = cfg.spec.modules.unwrap().registries;
    assert_eq!(registries.len(), 1);
    assert_eq!(registries[0].name, "private");
}

#[test]
fn module_registry_remove_with_registries_but_missing_name_errs_not_found() {
    // A config that DOES have registries but none match the requested name is a
    // strict not-found error (exit 6) — not an idempotent no-op.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  modules:
    registries:
      - name: community
        url: https://github.com/cfgd-community/modules.git
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, _buf) = test_printer_capture();

    let err = module::cmd_module_registry_remove(&cli, &printer, "nonexistent", false).unwrap_err();
    drop(printer);

    let meta = err
        .downcast_ref::<crate::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "registry_not_found");
    assert_eq!(
        crate::cli::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound,
    );

    // Config must remain unchanged — the existing registry is still there.
    let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
    let registries = cfg.spec.modules.unwrap().registries;
    assert_eq!(registries.len(), 1, "no entry was removed");
    assert_eq!(registries[0].name, "community");
}

#[test]
fn module_registry_remove_not_found() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let (printer, _buf) = test_printer_capture();

    let err = module::cmd_module_registry_remove(&cli, &printer, "nonexistent", false).unwrap_err();
    drop(printer);

    let meta = err
        .downcast_ref::<crate::cli::CliErrorMeta>()
        .expect("handler returns CliErrorMeta");
    assert_eq!(meta.error_kind, "registry_not_found");
    assert_eq!(
        crate::cli::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound,
    );
}

#[test]
fn module_registry_remove_warns_on_profile_references() {
    let dir = tempfile::tempdir().unwrap();
    // Config with a registry
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  modules:
    registries:
      - name: community
        url: https://github.com/cfgd-community/modules.git
"#,
    )
    .unwrap();
    // Profile that references community/vim
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  modules:
    - community/vim
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, buf) = test_printer_capture();

    module::cmd_module_registry_remove(&cli, &printer, "community", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("community"),
        "output should mention removed registry name, got: {output}"
    );
    // Verify registry was actually removed from config
    let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
    let registries = cfg.spec.modules.map(|m| m.registries).unwrap_or_default();
    assert!(
        registries.is_empty(),
        "registry should be removed from config"
    );
}

// --- cmd_module_registry_rename ---

#[test]
fn module_registry_rename_success() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  modules:
    registries:
      - name: old-name
        url: https://github.com/example/modules.git
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    module::cmd_module_registry_rename(&cli, &printer, "old-name", "new-name").unwrap();

    let cfg = config::load_config(&dir.path().join("cfgd.yaml")).unwrap();
    let registries = cfg.spec.modules.unwrap().registries;
    assert_eq!(registries[0].name, "new-name");
}

#[test]
fn module_registry_rename_cascades_to_profiles() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  modules:
    registries:
      - name: old-reg
        url: https://github.com/example/modules.git
"#,
    )
    .unwrap();

    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(
        profiles_dir.join("default.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  modules:
    - old-reg/vim
    - old-reg/git
    - local-mod
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    module::cmd_module_registry_rename(&cli, &printer, "old-reg", "new-reg").unwrap();

    // Profile should be updated
    let profile = config::load_profile(&profiles_dir.join("default.yaml")).unwrap();
    assert!(profile.spec.modules.contains(&"new-reg/vim".to_string()));
    assert!(profile.spec.modules.contains(&"new-reg/git".to_string()));
    assert!(profile.spec.modules.contains(&"local-mod".to_string()));
    // Old references should be gone
    assert!(
        !profile
            .spec
            .modules
            .iter()
            .any(|m| m.starts_with("old-reg/"))
    );
}

#[test]
fn module_registry_rename_not_found() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = module::cmd_module_registry_rename(&cli, &printer, "nonexistent", "new-name");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn module_registry_rename_duplicate_target() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  modules:
    registries:
      - name: alpha
        url: https://github.com/alpha/modules.git
      - name: beta
        url: https://github.com/beta/modules.git
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = module::cmd_module_registry_rename(&cli, &printer, "alpha", "beta");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("already exists"));
}

#[test]
fn module_registry_rename_no_config() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = module::cmd_module_registry_rename(&cli, &printer, "old", "new");
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("config file not found"),
        "expected typed no-config error, got: {msg}"
    );
}

// --- cmd_module_keys_list ---

#[test]
fn module_keys_list_no_keys() {
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    module::cmd_module_keys_list(&printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Keys") || output.contains("No") || output.contains("cosign"),
        "keys list should show key info or no-keys, got: {output}"
    );
}

#[test]
fn module_keys_list_with_pub_key() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("cosign.pub"), "fake pub key").unwrap();

    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    module::cmd_module_keys_list(&printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Keys") || output.contains("cosign"),
        "keys list should show key info, got: {output}"
    );
}

// --- cmd_module_create with manager prefix ---

#[test]
fn module_create_with_manager_prefix_packages() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ModuleCreateArgs {
        packages: vec!["brew:ripgrep".to_string(), "cargo:bat".to_string()],
        ..test_module_create_args("mgr-mod")
    };
    module::cmd_module_create(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "mgr-mod").unwrap();
    assert_eq!(doc.spec.packages.len(), 2);
    assert_eq!(doc.spec.packages[0].name, "ripgrep");
    assert_eq!(doc.spec.packages[0].prefer, vec!["brew"]);
    assert_eq!(doc.spec.packages[1].name, "bat");
    assert_eq!(doc.spec.packages[1].prefer, vec!["cargo"]);
}

// --- structured output mode ---

#[test]
fn module_list_structured_output_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("modules")).unwrap();
    let state_dir = dir.path().join("state");
    let cli = test_cli_with_state(dir.path(), Some(state_dir));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    module::cmd_module_list(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&output)
        .unwrap_or_else(|e| panic!("invalid JSON output: {e}, got: {output}"));
    assert!(parsed.is_array(), "JSON should be an array");
}

#[test]
fn module_show_structured_output() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "json-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: json-mod\nspec:\n  packages:\n    - name: curl\n",
    );
    let state_dir = dir.path().join("state");
    let cli = test_cli_with_state(dir.path(), Some(state_dir));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    module::cmd_module_show(&cli, &printer, "json-mod", false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&output)
        .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {output}"));
    assert_eq!(parsed["name"], "json-mod", "JSON should have module name");
}

#[test]
fn module_registry_list_structured_output() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  modules:
    registries:
      - name: community
        url: https://github.com/cfgd-modules/community.git
"#,
    )
    .unwrap();

    let cli = test_cli(dir.path());
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    module::cmd_module_registry_list(&cli, &printer).unwrap();
    drop(printer);

    let parsed = cap.json().expect("doc captured json");
    assert!(parsed.is_array(), "JSON should be an array of registries");
    assert_eq!(parsed[0]["name"], "community");
}

// ===================================================================
// Additional coverage tests — untested command handlers & helpers
// ===================================================================

// --- load_config_and_profile ---

#[test]
fn load_config_and_profile_default_profile() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());

    let result = super::load_config_and_profile(&cli);
    assert!(
        result.is_ok(),
        "loading config and default profile should succeed: {:?}",
        result.err()
    );
    let (cfg, _, resolved) = result.unwrap();
    assert_eq!(cfg.spec.profile.as_deref(), Some("default"));
    // The resolved profile should contain the env var from default profile
    assert!(resolved.merged.env.iter().any(|e| e.name == "editor"));
}

#[test]
fn load_config_and_profile_with_override() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let mut cli = test_cli(dir.path());
    cli.profile = Some("work".to_string());

    let result = super::load_config_and_profile(&cli);
    assert!(
        result.is_ok(),
        "loading config with profile override should succeed: {:?}",
        result.err()
    );
    let (_cfg, _, resolved) = result.unwrap();
    // Work profile overrides editor to 'code'
    let editor = resolved.merged.env.iter().find(|e| e.name == "editor");
    assert!(editor.is_some());
    assert_eq!(editor.unwrap().value, "code");
}

#[test]
fn load_config_and_profile_missing_config_errors() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());

    let result = super::load_config_and_profile(&cli);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("config file not found"),
        "expected 'config file not found' error, got: {msg}"
    );
}

#[test]
fn load_config_and_profile_missing_profile_errors() {
    let dir = create_test_config_dir();
    std::fs::write(
            dir.path().join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: nonexistent\n",
        )
        .unwrap();

    let cli = test_cli(dir.path());

    let result = super::load_config_and_profile(&cli);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("profile not found: nonexistent"),
        "expected 'profile not found: nonexistent' error, got: {msg}"
    );
}

#[test]
fn load_config_and_profile_active_profile_delivered_by_source_emits_wrap_hint() {
    // The active profile names a profile that exists ONLY in a subscribed
    // source's cached profiles/. The smart error must (a) carry the wrap hint
    // naming the providing source, and (b) still downcast to exit code 6.
    let (config_dir, state_dir) = setup_test_env();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: acme-backend\n  sources:\n    - name: acme-corp\n      origin:\n        type: Git\n        url: git@github.com:acme-corp/dev-config.git\n        branch: master\n",
    )
    .unwrap();

    // Seed the source cache with a profile the local config doesn't have.
    let src_profiles = state_dir
        .path()
        .join("sources")
        .join("acme-corp")
        .join("profiles");
    std::fs::create_dir_all(&src_profiles).unwrap();
    std::fs::write(
        src_profiles.join("acme-backend.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: acme-backend\nspec: {}\n",
    )
    .unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));

    let err = super::load_config_and_profile(&cli).unwrap_err();

    // Exit code survives the metadata wrap (typed ProfileNotFound → exit 6).
    assert_eq!(
        super::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound,
        "source-delivered profile must still resolve exit 6"
    );

    let meta = err
        .downcast_ref::<super::CliErrorMeta>()
        .expect("smart error carries CliErrorMeta");
    let joined = meta.hints.join("\n");
    assert!(
        meta.hints.iter().any(|h| h.contains("acme-corp")),
        "wrap hint must name the providing source, got: {joined}"
    );
    // The YAML wrap lives in the code block, not the hints.
    assert!(
        meta.code_block.iter().any(|l| l.contains("subscription:")),
        "code block must show the subscription wrap, got: {:?}",
        meta.code_block
    );
    assert!(
        meta.code_block
            .iter()
            .any(|l| l.contains("profile: acme-backend")),
        "code block must wrap the source profile, got: {:?}",
        meta.code_block
    );
    // Every carried line (hint OR code block) must be newline-free so the
    // write_line debug_assert holds when replayed.
    assert!(
        meta.hints
            .iter()
            .chain(meta.code_block.iter())
            .all(|l| !l.contains('\n')),
        "each carried line must be newline-free, got hints={joined} block={:?}",
        meta.code_block
    );
    // Structured consumers see the providing source(s) and profile name.
    assert_eq!(meta.extras["profile"], "acme-backend");
    assert_eq!(meta.extras["sources"], serde_json::json!(["acme-corp"]));

    // Regression: the RENDERED human output must show the YAML block tight —
    // no `→` glyph on the YAML rows and no blank line between them — so it stays
    // copy-pasteable (the original BLOCKER: each row replayed as a spaced hint).
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    super::error::render_cli_error(&printer, &err);
    printer.flush();
    let out = cfgd_core::output::strip_ansi(&buf.lock().unwrap());
    let block = "spec:\n  sources:\n    - name: acme-corp\n      subscription:\n        profile: acme-backend\n";
    assert!(
        out.contains(block),
        "YAML block must render contiguous (no `→`, no inter-row blanks), got:\n{out}"
    );
}

#[test]
fn load_config_and_profile_explicit_profile_delivered_by_source_emits_wrap_hint() {
    // Same remedy applies when the name comes from --profile, not active_profile.
    let (config_dir, state_dir) = setup_test_env();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: team-base\n      origin:\n        type: Git\n        url: git@github.com:team/base.git\n        branch: master\n",
    )
    .unwrap();

    let src_profiles = state_dir
        .path()
        .join("sources")
        .join("team-base")
        .join("profiles");
    std::fs::create_dir_all(&src_profiles).unwrap();
    std::fs::write(
        src_profiles.join("hardened.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: hardened\nspec: {}\n",
    )
    .unwrap();

    let cli = Cli {
        profile: Some("hardened".to_string()),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };

    let err = super::load_config_and_profile(&cli).unwrap_err();

    assert_eq!(
        super::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound
    );
    let meta = err
        .downcast_ref::<super::CliErrorMeta>()
        .expect("smart error carries CliErrorMeta");
    assert!(
        meta.hints.iter().any(|h| h.contains("team-base")),
        "wrap hint must name the providing source, got: {:?}",
        meta.hints
    );
    assert_eq!(meta.extras["profile"], "hardened");
}

#[test]
fn load_config_and_profile_plain_typo_returns_bare_not_found() {
    // No source provides the name → the original bare ProfileNotFound, no hint,
    // exit 6. The smart-error path must not decorate an honest typo.
    let (config_dir, state_dir) = setup_test_env();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: typo\n  sources:\n    - name: acme-corp\n      origin:\n        type: Git\n        url: git@github.com:acme-corp/dev-config.git\n        branch: master\n",
    )
    .unwrap();

    // Source cache exists but provides a DIFFERENT profile.
    let src_profiles = state_dir
        .path()
        .join("sources")
        .join("acme-corp")
        .join("profiles");
    std::fs::create_dir_all(&src_profiles).unwrap();
    std::fs::write(
        src_profiles.join("acme-backend.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: acme-backend\nspec: {}\n",
    )
    .unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));

    let err = super::load_config_and_profile(&cli).unwrap_err();

    assert_eq!(
        super::exit_code_for_anyhow(&err),
        cfgd_core::exit::ExitCode::NotFound,
        "plain typo still exit 6"
    );
    assert!(
        err.downcast_ref::<super::CliErrorMeta>().is_none(),
        "a plain typo must NOT be decorated with a wrap hint"
    );
    assert!(
        err.to_string().contains("profile not found: typo"),
        "bare ProfileNotFound preserved, got: {}",
        err
    );
}

// --- add_to_gitignore edge cases ---

#[test]
fn add_to_gitignore_appends_to_existing() {
    let dir = tempfile::tempdir().unwrap();
    // Pre-populate .gitignore with existing content (no trailing newline)
    std::fs::write(dir.path().join(".gitignore"), "*.log").unwrap();

    super::add_to_gitignore(dir.path(), "secrets/").unwrap();

    let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    assert!(content.contains("*.log"));
    assert!(content.contains("secrets/"));
    // Ensure a newline was added between old and new content
    assert!(content.contains("*.log\n"));
}

#[test]
fn add_to_gitignore_preserves_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    // Pre-populate with trailing newline
    std::fs::write(dir.path().join(".gitignore"), "*.log\n").unwrap();

    super::add_to_gitignore(dir.path(), "secrets/").unwrap();

    let content = std::fs::read_to_string(dir.path().join(".gitignore")).unwrap();
    // Should not double-newline
    assert!(!content.contains("\n\n"));
    assert!(content.contains("secrets/\n"));
}

// --- copy_files_to_dir edge cases ---

#[test]
#[cfg(unix)]
fn copy_files_to_dir_with_source_target_spec() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("my-config.txt");
    std::fs::write(&source, "config data").unwrap();
    let repo_dir = dir.path().join("repo");

    let target = dir.path().join("deploy").join("app.conf");
    let spec = format!("{}:{}", source.display(), target.display());
    let results = super::copy_files_to_dir(&[spec], &repo_dir).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "my-config.txt");
    assert_eq!(results[0].1, target);
    // File should be in repo
    assert!(repo_dir.join("my-config.txt").exists());
    // Original should now be a symlink
    assert!(source.symlink_metadata().unwrap().file_type().is_symlink());
}

#[test]
#[cfg(unix)]
fn copy_files_to_dir_directory_source() {
    let dir = tempfile::tempdir().unwrap();
    let source_dir = dir.path().join("dotfiles");
    std::fs::create_dir_all(&source_dir).unwrap();
    std::fs::write(source_dir.join("file1.txt"), "content1").unwrap();
    std::fs::write(source_dir.join("file2.txt"), "content2").unwrap();

    let repo_dir = dir.path().join("repo");
    let results = super::copy_files_to_dir(&[source_dir.display().to_string()], &repo_dir).unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "dotfiles");
    // Directory should be recursively copied
    assert!(repo_dir.join("dotfiles").join("file1.txt").exists());
    assert!(repo_dir.join("dotfiles").join("file2.txt").exists());
    // Source should be a symlink
    assert!(
        source_dir
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn copy_files_to_dir_rejects_system_directories() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");

    // /etc/passwd exists on Linux, and it should be rejected
    if std::path::Path::new("/etc/passwd").exists() {
        let result = super::copy_files_to_dir(&["/etc/passwd".into()], &repo_dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("system directory"));
    }
}

// --- action_type_str ---

#[test]
fn action_type_str_file_variants() {
    use cfgd_core::reconciler::Action;

    assert_eq!(
        super::action_type_str(&Action::File(FileAction::Create {
            source: "/a".into(),
            target: "/b".into(),
            origin: "local".into(),
            strategy: cfgd_core::config::FileStrategy::default(),
            source_hash: None,
        })),
        "create"
    );

    assert_eq!(
        super::action_type_str(&Action::File(FileAction::Update {
            source: "/a".into(),
            target: "/b".into(),
            diff: String::new(),
            origin: "local".into(),
            strategy: cfgd_core::config::FileStrategy::default(),
            source_hash: None,
        })),
        "update"
    );

    assert_eq!(
        super::action_type_str(&Action::File(FileAction::Delete {
            target: "/b".into(),
            origin: "local".into(),
        })),
        "delete"
    );

    assert_eq!(
        super::action_type_str(&Action::File(FileAction::SetPermissions {
            target: "/b".into(),
            mode: 0o644,
            origin: "local".into(),
        })),
        "chmod"
    );

    assert_eq!(
        super::action_type_str(&Action::File(FileAction::Skip {
            target: "/b".into(),
            reason: "test".into(),
            origin: "local".into(),
        })),
        "skip"
    );
}

#[test]
fn action_type_str_package_variants() {
    use cfgd_core::reconciler::Action;

    assert_eq!(
        super::action_type_str(&Action::Package(PackageAction::Install {
            manager: "brew".into(),
            packages: vec!["curl".into()],
            origin: "local".into(),
        })),
        "install"
    );

    assert_eq!(
        super::action_type_str(&Action::Package(PackageAction::Uninstall {
            manager: "brew".into(),
            packages: vec!["curl".into()],
            origin: "local".into(),
        })),
        "uninstall"
    );

    assert_eq!(
        super::action_type_str(&Action::Package(PackageAction::Bootstrap {
            manager: "brew".into(),
            method: "curl".into(),
            origin: "local".into(),
        })),
        "bootstrap"
    );

    assert_eq!(
        super::action_type_str(&Action::Package(PackageAction::Skip {
            manager: "brew".into(),
            reason: "test".into(),
            origin: "local".into(),
        })),
        "skip"
    );
}

#[test]
fn action_type_str_secret_variants() {
    use cfgd_core::reconciler::Action;

    assert_eq!(
        super::action_type_str(&Action::Secret(SecretAction::Decrypt {
            source: "a.enc".into(),
            target: "/b".into(),
            backend: "sops-age".into(),
            origin: "local".into(),
        })),
        "decrypt"
    );

    assert_eq!(
        super::action_type_str(&Action::Secret(SecretAction::Resolve {
            provider: "onepassword".into(),
            reference: "op://vault/item".into(),
            target: "/b".into(),
            origin: "local".into(),
        })),
        "resolve"
    );

    assert_eq!(
        super::action_type_str(&Action::Secret(SecretAction::ResolveEnv {
            provider: "vault".into(),
            reference: "secret/data/app".into(),
            envs: vec!["TOKEN".into(), "API_KEY".into()],
            origin: "local".into(),
        })),
        "resolve-env"
    );

    assert_eq!(
        super::action_type_str(&Action::Secret(SecretAction::Skip {
            source: "a".into(),
            reason: "test".into(),
            origin: "local".into(),
        })),
        "skip"
    );
}

#[test]
fn action_type_str_env_variants() {
    use cfgd_core::reconciler::{Action, EnvAction};

    assert_eq!(
        super::action_type_str(&Action::Env(EnvAction::WriteEnvFile {
            path: "/tmp/env".into(),
            content: String::new(),
        })),
        "write"
    );

    assert_eq!(
        super::action_type_str(&Action::Env(EnvAction::InjectSourceLine {
            rc_path: "/tmp/rc".into(),
            line: "source /tmp/env".into(),
        })),
        "inject"
    );
}

#[test]
fn action_type_str_system_variants() {
    use cfgd_core::reconciler::{Action, SystemAction};

    assert_eq!(
        super::action_type_str(&Action::System(SystemAction::SetValue {
            configurator: "shell".into(),
            key: "/bin/zsh".into(),
            desired: "/bin/zsh".into(),
            current: "/bin/bash".into(),
            origin: "local".into(),
        })),
        "set"
    );

    assert_eq!(
        super::action_type_str(&Action::System(SystemAction::Skip {
            configurator: "shell".into(),
            reason: "test".into(),
            origin: "local".into(),
            unknown: false,
        })),
        "skip"
    );
}

#[test]
fn action_type_str_module_variants() {
    use cfgd_core::reconciler::{Action, ModuleAction, ModuleActionKind};

    assert_eq!(
        super::action_type_str(&Action::Module(ModuleAction {
            module_name: "m".into(),
            kind: ModuleActionKind::InstallPackages { resolved: vec![] },
            origin: None,
        })),
        "install"
    );

    assert_eq!(
        super::action_type_str(&Action::Module(ModuleAction {
            module_name: "m".into(),
            kind: ModuleActionKind::DeployFiles { files: vec![] },
            origin: None,
        })),
        "deploy"
    );

    assert_eq!(
        super::action_type_str(&Action::Module(ModuleAction {
            module_name: "m".into(),
            kind: ModuleActionKind::RunScript {
                script: cfgd_core::config::ScriptEntry::Simple("echo hi".into()),
                phase: cfgd_core::reconciler::ScriptPhase::PostApply,
            },
            origin: None,
        })),
        "run"
    );

    assert_eq!(
        super::action_type_str(&Action::Module(ModuleAction {
            module_name: "m".into(),
            kind: ModuleActionKind::Skip {
                reason: "test".into()
            },
            origin: None,
        })),
        "skip"
    );
}

#[test]
fn action_type_str_script() {
    use cfgd_core::reconciler::{Action, ScriptAction};

    assert_eq!(
        super::action_type_str(&Action::Script(ScriptAction::Run {
            entry: cfgd_core::config::ScriptEntry::Simple("echo done".into()),
            phase: cfgd_core::reconciler::ScriptPhase::PostApply,
            origin: "local".into(),
        })),
        "run"
    );
}

// --- build_plan_output ---

#[test]
fn build_plan_output_empty_plan() {
    let plan = reconciler::Plan {
        phases: vec![],
        warnings: vec![],
    };
    let output = super::build_plan_output(&plan, "apply", None);
    assert_eq!(output.context, "apply");
    assert_eq!(output.total_actions, 0);
    assert!(output.phases.is_empty());
}

#[test]
fn build_plan_output_with_actions() {
    let plan = reconciler::Plan {
        phases: vec![reconciler::Phase {
            name: reconciler::PhaseName::Packages,
            actions: vec![reconciler::Action::Package(PackageAction::Install {
                manager: "brew".into(),
                packages: vec!["curl".into()],
                origin: "local".into(),
            })],
        }],
        warnings: vec!["something".into()],
    };
    let output = super::build_plan_output(&plan, "reconcile", None);
    assert_eq!(output.context, "reconcile");
    assert_eq!(output.total_actions, 1);
    assert_eq!(output.phases.len(), 1);
    assert_eq!(output.warnings, vec!["something".to_string()]);
}

#[test]
fn build_plan_output_with_phase_filter() {
    let plan = reconciler::Plan {
        phases: vec![
            reconciler::Phase {
                name: reconciler::PhaseName::Packages,
                actions: vec![reconciler::Action::Package(PackageAction::Install {
                    manager: "brew".into(),
                    packages: vec!["curl".into()],
                    origin: "local".into(),
                })],
            },
            reconciler::Phase {
                name: reconciler::PhaseName::Files,
                actions: vec![reconciler::Action::File(FileAction::Create {
                    source: "/a".into(),
                    target: "/b".into(),
                    origin: "local".into(),
                    strategy: cfgd_core::config::FileStrategy::default(),
                    source_hash: None,
                })],
            },
        ],
        warnings: vec![],
    };
    // Filter to only Files phase
    let output = super::build_plan_output(&plan, "apply", Some(&reconciler::PhaseName::Files));
    assert_eq!(output.total_actions, 1);
    assert_eq!(output.phases.len(), 1);
    assert_eq!(output.phases[0].phase, "Files");
}

// --- strip_scripts_from_plan ---

#[test]
fn strip_scripts_removes_script_phases() {
    use cfgd_core::reconciler::{Phase, PhaseName, Plan, ScriptAction};

    let mut plan = Plan {
        phases: vec![
            Phase {
                name: PhaseName::PreScripts,
                actions: vec![reconciler::Action::Script(ScriptAction::Run {
                    entry: cfgd_core::config::ScriptEntry::Simple("echo pre".into()),
                    phase: cfgd_core::reconciler::ScriptPhase::PreApply,
                    origin: "local".into(),
                })],
            },
            Phase {
                name: PhaseName::Packages,
                actions: vec![reconciler::Action::Package(PackageAction::Install {
                    manager: "brew".into(),
                    packages: vec!["curl".into()],
                    origin: "local".into(),
                })],
            },
            Phase {
                name: PhaseName::PostScripts,
                actions: vec![reconciler::Action::Script(ScriptAction::Run {
                    entry: cfgd_core::config::ScriptEntry::Simple("echo post".into()),
                    phase: cfgd_core::reconciler::ScriptPhase::PostApply,
                    origin: "local".into(),
                })],
            },
        ],
        warnings: vec![],
    };

    super::strip_scripts_from_plan(&mut plan);

    // Pre and post script phases should be removed
    assert_eq!(plan.phases.len(), 1);
    assert_eq!(plan.phases[0].name, PhaseName::Packages);
}

#[test]
fn strip_scripts_removes_module_run_script_actions() {
    use cfgd_core::reconciler::{ModuleAction, ModuleActionKind, Phase, PhaseName, Plan};

    let mut plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Modules,
            actions: vec![
                reconciler::Action::Module(ModuleAction {
                    module_name: "m".into(),
                    kind: ModuleActionKind::InstallPackages { resolved: vec![] },
                    origin: None,
                }),
                reconciler::Action::Module(ModuleAction {
                    module_name: "m".into(),
                    kind: ModuleActionKind::RunScript {
                        script: cfgd_core::config::ScriptEntry::Simple("echo hello".into()),
                        phase: cfgd_core::reconciler::ScriptPhase::PostApply,
                    },
                    origin: None,
                }),
                reconciler::Action::Module(ModuleAction {
                    module_name: "m".into(),
                    kind: ModuleActionKind::DeployFiles { files: vec![] },
                    origin: None,
                }),
            ],
        }],
        warnings: vec![],
    };

    super::strip_scripts_from_plan(&mut plan);

    // RunScript should be removed, other actions kept
    assert_eq!(plan.phases[0].actions.len(), 2);
}

// --- filter_plan edge cases ---

#[test]
fn filter_plan_skip_file_by_target() {
    use cfgd_core::reconciler::{Action, Phase, PhaseName, Plan};

    let mut plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Files,
            actions: vec![
                Action::File(FileAction::Create {
                    source: "/tmp/a".into(),
                    target: "/etc/foo".into(),
                    origin: "local".into(),
                    strategy: cfgd_core::config::FileStrategy::default(),
                    source_hash: None,
                }),
                Action::File(FileAction::Create {
                    source: "/tmp/b".into(),
                    target: "/etc/bar".into(),
                    origin: "local".into(),
                    strategy: cfgd_core::config::FileStrategy::default(),
                    source_hash: None,
                }),
            ],
        }],
        warnings: vec![],
    };

    super::filter_plan(&mut plan, &["files:/etc/foo".into()], &[]);
    assert_eq!(plan.phases[0].actions.len(), 1);
}

#[test]
fn filter_plan_empty_skip_and_only_noop() {
    use cfgd_core::reconciler::{Action, Phase, PhaseName, Plan};

    let mut plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Packages,
            actions: vec![Action::Package(PackageAction::Install {
                manager: "brew".into(),
                packages: vec!["curl".into()],
                origin: "local".into(),
            })],
        }],
        warnings: vec![],
    };

    super::filter_plan(&mut plan, &[], &[]);
    assert_eq!(plan.phases[0].actions.len(), 1);
}

#[test]
fn filter_plan_skip_uninstall_packages() {
    use cfgd_core::reconciler::{Action, Phase, PhaseName, Plan};

    let mut plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Packages,
            actions: vec![Action::Package(PackageAction::Uninstall {
                manager: "brew".into(),
                packages: vec!["old-tool".into(), "keep-me".into()],
                origin: "local".into(),
            })],
        }],
        warnings: vec![],
    };

    super::filter_plan(&mut plan, &["packages.brew.old-tool".into()], &[]);

    match &plan.phases[0].actions[0] {
        reconciler::Action::Package(PackageAction::Uninstall { packages, .. }) => {
            assert_eq!(packages, &["keep-me".to_string()]);
        }
        _ => panic!("expected Uninstall action"),
    }
}

#[test]
fn filter_plan_only_with_uninstall() {
    use cfgd_core::reconciler::{Action, Phase, PhaseName, Plan};

    let mut plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Packages,
            actions: vec![Action::Package(PackageAction::Uninstall {
                manager: "apt".into(),
                packages: vec!["vim".into(), "nano".into()],
                origin: "local".into(),
            })],
        }],
        warnings: vec![],
    };

    super::filter_plan(&mut plan, &[], &["packages.apt.vim".into()]);

    match &plan.phases[0].actions[0] {
        reconciler::Action::Package(PackageAction::Uninstall { packages, .. }) => {
            assert_eq!(packages, &["vim".to_string()]);
        }
        _ => panic!("expected Uninstall action"),
    }
}

// --- config_cmd::cmd_config_set and config_cmd::cmd_config_unset via full commands ---

#[test]
fn cmd_config_set_creates_nested_key() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");
    std::fs::write(&config_path, TEST_CONFIG_YAML).unwrap();

    let cli = Cli {
        config: config_path.clone(),
        config_explicit: false,
        ..test_cli(dir.path())
    };
    let printer = test_printer();

    // Set a nested key
    super::config_cmd::cmd_config_set(&cli, &printer, "daemon.enabled", "true").unwrap();

    let contents = std::fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("daemon"));
    assert!(contents.contains("enabled"));
}

#[test]
fn cmd_config_set_no_config_errors() {
    let dir = tempfile::tempdir().unwrap();
    let cli = Cli {
        config: dir.path().join("nonexistent.yaml"),
        config_explicit: false,
        ..test_cli(dir.path())
    };
    let printer = test_printer();

    let result = super::config_cmd::cmd_config_set(&cli, &printer, "profile", "work");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("config file not found")
    );
}

#[test]
fn cmd_config_unset_no_config_errors() {
    let dir = tempfile::tempdir().unwrap();
    let cli = Cli {
        config: dir.path().join("nonexistent.yaml"),
        config_explicit: false,
        ..test_cli(dir.path())
    };
    let printer = test_printer();

    let result = super::config_cmd::cmd_config_unset(&cli, &printer, "profile");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("config file not found")
    );
}

// --- source::cmd_source_list with sources configured ---

#[test]
fn cmd_source_list_with_sources_configured() {
    let (config_dir, state_dir) = setup_test_env();

    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: team-config
      origin:
        url: https://github.com/team/config
        branch: main
        type: Git
      subscription:
        priority: 100
"#;
    std::fs::write(config_dir.path().join("cfgd.yaml"), config_with_source).unwrap();

    // Verify config loaded with the source
    let cfg = config::load_config(&config_dir.path().join("cfgd.yaml")).unwrap();
    assert_eq!(cfg.spec.sources.len(), 1);
    assert_eq!(cfg.spec.sources[0].name, "team-config");

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::source::cmd_source_list(&cli, &printer);
    assert!(
        result.is_ok(),
        "source list should succeed when sources are configured in cfgd.yaml: {:?}",
        result.err()
    );
}

#[test]
fn cmd_source_list_structured_output() {
    let (config_dir, state_dir) = setup_test_env();

    // Write config with a source so we can verify the source name appears in output
    let config_with_source = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: team-config\n      origin:\n        url: https://github.com/team/config\n        branch: main\n        type: Git\n      subscription:\n        priority: 100\n";
    std::fs::write(config_dir.path().join("cfgd.yaml"), config_with_source).unwrap();

    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    super::source::cmd_source_list(&cli, &printer).unwrap();

    drop(printer);
    let parsed = cap
        .json()
        .expect("source list should emit a Doc with payload");
    let arr = parsed
        .as_array()
        .expect("source list JSON should be an array");
    assert_eq!(arr.len(), 1, "should have exactly one source");
    assert_eq!(
        arr[0]["name"], "team-config",
        "source name should be 'team-config'"
    );
}

// --- source::cmd_source_show ---

#[test]
fn cmd_source_show_not_found() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));

    let result = super::source::cmd_source_show(&cli, &test_printer(), "nonexistent");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn cmd_source_show_with_source() {
    let (config_dir, state_dir) = setup_test_env();

    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: team-config
      origin:
        url: https://github.com/team/config
        branch: main
        type: Git
      subscription:
        priority: 100
"#;
    std::fs::write(config_dir.path().join("cfgd.yaml"), config_with_source).unwrap();

    // Verify the source exists in config
    let cfg = config::load_config(&config_dir.path().join("cfgd.yaml")).unwrap();
    assert!(
        cfg.spec.sources.iter().any(|s| s.name == "team-config"),
        "precondition: config should contain team-config source"
    );

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    super::source::cmd_source_show(&cli, &printer, "team-config").unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("team-config"),
        "source show should display source name, got: {output}"
    );
}

// --- source::cmd_source_remove ---

#[test]
fn cmd_source_remove_not_found() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result =
        super::source::cmd_source_remove(&cli, &printer, "nonexistent", true, false, false);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn cmd_source_remove_keep_all_and_remove_all_conflict() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::source::cmd_source_remove(&cli, &printer, "anything", true, true, false);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("cannot use --keep-all and --remove-all together")
    );
}

// --- source::cmd_source_override ---

#[test]
fn cmd_source_override_source_not_found() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::source::cmd_source_override(
        &cli,
        &printer,
        "nonexistent",
        super::SourceOverrideAction::Reject,
        "env.FOO",
        None,
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

// cmd_source_override_invalid_action test removed — SourceOverrideAction
// is a clap ValueEnum so invalid strings fail at parse time.

#[test]
fn cmd_source_override_set_requires_value() {
    let (config_dir, state_dir) = setup_test_env();

    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: team
      origin:
        url: https://github.com/team/config
        branch: main
        type: Git
      subscription:
        priority: 100
"#;
    std::fs::write(config_dir.path().join("cfgd.yaml"), config_with_source).unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::source::cmd_source_override(
        &cli,
        &printer,
        "team",
        super::SourceOverrideAction::Set,
        "env.FOO",
        None,
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("requires a value"));
}

// --- source::cmd_source_priority ---

#[test]
fn cmd_source_priority_not_found() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::source::cmd_source_priority(&cli, &printer, "nonexistent", None);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn cmd_source_priority_view() {
    let (config_dir, state_dir) = setup_test_env();

    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: team
      origin:
        url: https://github.com/team/config
        branch: main
        type: Git
      subscription:
        priority: 200
"#;
    std::fs::write(config_dir.path().join("cfgd.yaml"), config_with_source).unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    super::source::cmd_source_priority(&cli, &printer, "team", None).unwrap();

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("team") || output.contains("200"),
        "source priority should show source name or priority, got: {output}"
    );
}

#[test]
fn cmd_source_priority_update() {
    let (config_dir, state_dir) = setup_test_env();

    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: team
      origin:
        url: https://github.com/team/config
        branch: main
        type: Git
      subscription:
        priority: 200
"#;
    std::fs::write(config_dir.path().join("cfgd.yaml"), config_with_source).unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::source::cmd_source_priority(&cli, &printer, "team", Some(500));
    assert!(
        result.is_ok(),
        "source priority update should succeed: {:?}",
        result.err()
    );

    // Verify the config was updated
    let cfg = config::load_config(&config_dir.path().join("cfgd.yaml")).unwrap();
    let source = cfg.spec.sources.iter().find(|s| s.name == "team").unwrap();
    assert_eq!(source.subscription.priority, 500);
}

// --- cmd_decide edge cases ---

#[test]
fn cmd_decide_no_args_shows_pending() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    super::decide::cmd_decide(
        &printer,
        super::DecideAction::Accept,
        None,
        None,
        false,
        Some(state_dir.path()),
    )
    .unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No pending") || output.contains("Pending"),
        "decide should show pending decisions info, got: {output}"
    );
}

#[test]
fn cmd_decide_with_pending_decision() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    state
        .upsert_pending_decision(
            "team-config",
            "packages.brew.curl",
            "recommended",
            "install",
            "Install curl via brew",
        )
        .unwrap();

    let result = super::decide::cmd_decide(
        &printer,
        super::DecideAction::Accept,
        Some("packages.brew.curl"),
        None,
        false,
        Some(state_dir.path()),
    );
    assert!(
        result.is_ok(),
        "decide accept should succeed for a pending decision: {:?}",
        result.err()
    );
    drop(printer);

    let pending = state.pending_decisions().unwrap();
    assert!(
        pending.is_empty(),
        "accepted decision should no longer be pending"
    );

    let output = buf.lock().unwrap();
    assert!(
        output.contains("curl") || output.contains("Accepted"),
        "output should mention the accepted resource, got: {output}"
    );
}

#[test]
fn cmd_decide_accept_all_with_pending() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    state
        .upsert_pending_decision(
            "team",
            "packages.brew.curl",
            "recommended",
            "install",
            "Install curl via brew",
        )
        .unwrap();
    state
        .upsert_pending_decision("team", "env.EDITOR", "recommended", "set", "Set EDITOR")
        .unwrap();

    let result = super::decide::cmd_decide(
        &printer,
        super::DecideAction::Accept,
        None,
        None,
        true,
        Some(state_dir.path()),
    );
    assert!(
        result.is_ok(),
        "decide accept-all should succeed and resolve all pending decisions: {:?}",
        result.err()
    );
    drop(printer);

    let pending = state.pending_decisions().unwrap();
    assert!(pending.is_empty());

    let output = buf.lock().unwrap();
    assert!(
        output.contains("ACCEPTED") || output.contains("2 item"),
        "accept-all should mention accepted decisions, got: {output}"
    );
}

#[test]
fn cmd_decide_reject_by_source_with_pending() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let state = super::open_state_store(Some(state_dir.path())).unwrap();
    state
        .upsert_pending_decision(
            "team",
            "packages.brew.curl",
            "recommended",
            "install",
            "Install curl via brew",
        )
        .unwrap();
    state
        .upsert_pending_decision("other", "env.EDITOR", "recommended", "set", "Set EDITOR")
        .unwrap();

    let result = super::decide::cmd_decide(
        &printer,
        super::DecideAction::Reject,
        None,
        Some("team"),
        false,
        Some(state_dir.path()),
    );
    assert!(
        result.is_ok(),
        "decide reject-by-source should succeed and only reject matching source: {:?}",
        result.err()
    );
    drop(printer);

    // Only the "other" source's decision should remain pending
    let pending = state.pending_decisions().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source, "other");
}

// --- workflow::cmd_workflow_generate ---

#[test]
#[cfg(unix)] // prompt_confirm hangs on Windows CI (no /dev/null stdin fallback)
fn cmd_workflow_generate_no_overwrite_without_force() {
    let dir = create_test_config_dir();
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    // First generate
    super::workflow::cmd_workflow_generate(&cli, &printer, false).unwrap();
    let path = dir.path().join(".github/workflows/cfgd-release.yml");
    assert!(path.exists());

    // Write custom content
    std::fs::write(&path, "custom content").unwrap();

    // Generate without force — should NOT overwrite. The non-force path
    // prompts via inquire; with no response queued the prompt returns Err
    // which `unwrap_or(false)` maps to "do not overwrite".
    super::workflow::cmd_workflow_generate(&cli, &printer, false).unwrap();

    let contents = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        contents, "custom content",
        "should not overwrite without --force"
    );
}

// --- cmd_log_show_output ---

#[test]
fn cmd_log_show_output_nonexistent_apply_via_dispatch() {
    let state_dir = tempfile::tempdir().unwrap();
    let printer = test_printer();

    // Nonexistent apply ID should fail (routes through cmd_log → cmd_log_show_output)
    let result = super::log::cmd_log(&printer, 10, Some(9999), Some(state_dir.path()));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no apply found"));
}

#[test]
fn cmd_compliance_history_structured() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    super::compliance::cmd_compliance_history(&cli, &printer, None).unwrap();

    drop(printer);
    let output = buf.lock().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(output.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {output}"));
    assert_eq!(
        parsed,
        serde_json::json!({"entries": []}),
        "fresh state should produce exactly {{entries: []}}"
    );
}

// --- cmd_compliance_diff ---

#[test]
fn cmd_compliance_diff_missing_snapshot() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::compliance::cmd_compliance_diff(&cli, &printer, 1, 2);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

// --- cmd_apply module-only mode (no profile configured) ---

#[test]
fn cmd_apply_module_only_no_profile() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    // Minimal config with no profile
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec: {}\n",
    )
    .unwrap();

    // Create a module
    create_module_in_dir(
        dir.path(),
        "standalone-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: standalone-mod\nspec:\n  packages: []\n",
    );

    let cli = test_cli_with_state(dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = ApplyArgs {
        from: None,
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: Some("standalone-mod".to_string()),
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    let result = super::apply::cmd_apply(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "dry-run apply should succeed with --module flag and no profile configured: {:?}",
        result.err()
    );

    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("standalone-mod") || output.contains("Apply") || output.contains("Nothing"),
        "apply with module-only should mention the module, got: {output}"
    );
}

// --- cmd_plan module-only mode (no profile) ---

#[test]
fn cmd_plan_module_only_no_profile() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec: {}\n",
    )
    .unwrap();

    create_module_in_dir(
        dir.path(),
        "solo",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: solo\nspec:\n  packages: []\n",
    );

    let cli = test_cli_with_state(dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: Some("solo".to_string()),
        skip_scripts: false,
        context: "apply".to_string(),
    };

    let result = super::plan::cmd_plan(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "plan should succeed with --module flag and no profile configured: {:?}",
        result.err()
    );
    printer.flush();

    let output = buf.lock().unwrap();
    assert!(
        output.contains("Plan") || output.contains("solo") || output.contains("Nothing"),
        "plan with module-only should mention the module or plan, got: {output}"
    );
}

// --- empty_resolved_profile ---

#[test]
fn empty_resolved_profile_has_module() {
    let resolved = super::empty_resolved_profile("my-mod");
    assert_eq!(resolved.merged.modules, vec!["my-mod".to_string()]);
    assert!(resolved.merged.env.is_empty());
    assert!(resolved.layers.is_empty());
}

// --- known_manager_names ---

#[test]
fn known_manager_names_not_empty() {
    let names = super::known_manager_names();
    assert!(!names.is_empty());
    // Should contain at least "cargo" since it's always available
    assert!(
        names.contains(&"cargo".to_string()),
        "should contain 'cargo' manager"
    );
}

// --- secret_backend_from_config with config ---

#[test]
fn secret_backend_from_config_with_custom_backend() {
    let cfg = config::parse_config(
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  secrets:
    backend: sops-age
    sops:
      ageKey: /tmp/test-key.txt
"#,
        std::path::Path::new("cfgd.yaml"),
    )
    .unwrap();

    let (backend, key) = super::secret_backend_from_config(Some(&cfg));
    assert_eq!(backend, "sops-age");
    assert_eq!(key, Some(PathBuf::from("/tmp/test-key.txt")));
}

#[test]
fn secret_backend_from_config_no_secrets_section() {
    let cfg = config::parse_config(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: default\n",
            std::path::Path::new("cfgd.yaml"),
        )
        .unwrap();

    let (backend, key) = super::secret_backend_from_config(Some(&cfg));
    assert_eq!(backend, "sops");
    assert!(key.is_none());
}

// --- build_registry_with_config ---

#[test]
fn build_registry_with_config_applies_file_strategy() {
    let cfg = config::parse_config(
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  fileStrategy: Copy
"#,
        std::path::Path::new("cfgd.yaml"),
    )
    .unwrap();

    let registry = super::build_registry_with_config(Some(&cfg));
    assert_eq!(
        registry.default_file_strategy,
        cfgd_core::config::FileStrategy::Copy
    );
}

// --- execute dispatch for more commands ---

#[test]
fn execute_explain_recursive() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Explain {
        resource: Some("config".to_string()),
        recursive: true,
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    let output = h.output();
    assert!(
        output.contains("Config") || output.contains("config") || output.contains("spec"),
        "explain recursive for config should describe config resource, got: {output}"
    );
}

#[test]
fn execute_compliance_command() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        command: Some(Command::Compliance { command: None }),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    super::execute(&cli, &printer, &super::paths::DirSources::all_default()).unwrap();
    printer.flush();
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Compliance") || output.contains("snapshot"),
        "compliance dispatch should produce output, got: {output}"
    );
}

#[test]
fn execute_source_list() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        command: Some(Command::Source {
            command: SourceCommand::List,
        }),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    super::execute(&cli, &printer, &super::paths::DirSources::all_default()).unwrap();
    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Sources") || output.contains("No sources"),
        "source list should produce output, got: {output}"
    );
}

#[test]
fn execute_decide_accept_all() {
    let (_config_dir, state_dir) = setup_test_env();

    let dir = tempfile::tempdir().unwrap();
    let cli = Cli {
        command: Some(Command::Decide {
            action: super::DecideAction::Accept,
            resource: None,
            source: None,
            all: true,
        }),
        state_dir: Some(state_dir.path().to_path_buf()),
        config_dir: None,
        cache_dir: None,
        runtime_dir: None,
        scope_arg: crate::cli::ScopeArg::User,
        ..test_cli(dir.path())
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    super::execute(&cli, &printer, &super::paths::DirSources::all_default()).unwrap();
    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("No pending")
            || output.contains("0 decision")
            || output.contains("ACCEPTED"),
        "decide dispatch should produce output, got: {output}"
    );
}

#[test]
fn execute_sync_command() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        command: Some(Command::Sync),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) = test_printer_capture();

    super::execute(&cli, &printer, &super::paths::DirSources::all_default()).unwrap();
    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Sync") || output.contains("No sources"),
        "sync dispatch should produce output, got: {output}"
    );
}

#[test]
fn execute_pull_command() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        command: Some(Command::Pull),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) = test_printer_capture();

    super::execute(&cli, &printer, &super::paths::DirSources::all_default()).unwrap();
    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Pull") || output.contains("No sources") || output.contains("no origin"),
        "pull dispatch should produce output, got: {output}"
    );
}

// --- cmd_apply with aliases ---

#[test]
fn cmd_apply_with_aliases() {
    let (config_dir, state_dir) = setup_test_env();

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  aliases:\n    - name: ll\n      command: ls -la\n  modules: []\n";
    std::fs::write(
        config_dir.path().join("profiles").join("default.yaml"),
        profile,
    )
    .unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = ApplyArgs {
        from: None,
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    let result = super::apply::cmd_apply(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "dry-run apply should succeed when profile contains shell aliases: {:?}",
        result.err()
    );

    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Plan"),
        "should contain Plan header, got: {output}"
    );
    assert!(
        output.contains("Phase:") || output.contains("Nothing to do"),
        "should mention plan phases or nothing to do, got: {output}"
    );
}

// --- cmd_status structured output for module ---

#[test]
fn cmd_status_module_structured_output() {
    let (config_dir, state_dir) = setup_test_env();

    let mod_dir = config_dir.path().join("modules").join("json-mod");
    std::fs::create_dir_all(&mod_dir).unwrap();
    std::fs::write(
            mod_dir.join("module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: json-mod\nspec:\n  packages: []\n",
        )
        .unwrap();

    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    super::status::cmd_status(&cli, &printer, Some("json-mod"), false).unwrap();
    drop(printer);

    let output = buf.lock().unwrap().clone();
    let parsed = extract_json(&output);
    assert_eq!(parsed["name"], "json-mod", "should contain module name");
    assert_eq!(
        parsed["status"], "not applied",
        "fresh module should be 'not applied'"
    );
    assert_eq!(parsed["packages"], 0, "empty module should have 0 packages");
}

// --- cmd_verify structured output ---

#[test]
fn cmd_verify_structured_output() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    super::verify::cmd_verify(&cli, &printer, None, false).unwrap();
    printer.flush();

    let output = buf.lock().unwrap();
    let parsed = extract_json(&output);
    assert!(
        parsed.get("results").is_some(),
        "verify JSON should have 'results'"
    );
    assert!(
        parsed.get("passCount").is_some(),
        "verify JSON should have 'passCount'"
    );
    assert!(
        parsed.get("failCount").is_some(),
        "verify JSON should have 'failCount'"
    );
    let total = parsed["passCount"].as_u64().unwrap() + parsed["failCount"].as_u64().unwrap();
    assert_eq!(
        total,
        parsed["results"].as_array().unwrap().len() as u64,
        "pass + fail should equal total results"
    );
}

// --- cmd_plan structured output ---

#[test]
fn cmd_plan_structured_output() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    };

    super::plan::cmd_plan(&cli, &printer, &args).unwrap();
    printer.flush();

    let output = buf.lock().unwrap();
    let parsed = extract_json(&output);
    assert!(
        parsed.get("context").is_some(),
        "plan JSON should have 'context'"
    );
    assert_eq!(parsed["context"], "apply", "plan context should be 'apply'");
    assert!(
        parsed.get("totalActions").is_some(),
        "plan JSON should have 'totalActions'"
    );
    assert!(
        parsed["totalActions"].is_u64(),
        "totalActions should be a numeric value"
    );
}

// --- cmd_log with show_output ---

#[test]
fn cmd_log_show_output_for_nonexistent_apply() {
    let state_dir = tempfile::tempdir().unwrap();
    let printer = test_printer();

    // Nonexistent apply ID should fail
    let result = super::log::cmd_log(&printer, 10, Some(999), Some(state_dir.path()));
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no apply found"));
}

// --- validate_resource_name edge cases ---

#[test]
fn validate_resource_name_max_length() {
    // 128 chars should be valid
    let name = "a".repeat(128);
    assert!(super::validate_resource_name(&name, "Module").is_ok());

    // 129 chars should be invalid
    let name = "a".repeat(129);
    assert!(super::validate_resource_name(&name, "Module").is_err());
}

#[test]
fn validate_resource_name_underscores_and_dots() {
    // Unwrap to ensure success; not just is_ok()
    super::validate_resource_name("my_module.v2", "Module").unwrap();
}

// --- infer_source_name edge cases ---

#[test]
fn infer_source_name_plain_url() {
    let name = super::infer_source_name("https://example.com/config");
    assert_eq!(name, "config");
}

#[test]
fn infer_source_name_with_git_suffix() {
    let name = super::infer_source_name("https://github.com/org/repo.git");
    assert_eq!(name, "repo");
}

// --- OutputFormatArg From impl ---

#[test]
fn output_format_arg_into_os_str() {
    use super::OutputFormatArg;
    let arg = OutputFormatArg(cfgd_core::output::OutputFormat::Json);
    let os_str: clap::builder::OsStr = arg.into();
    assert_eq!(os_str, "table");
}

// --- set_nested_yaml_value ---

#[test]
fn set_nested_yaml_value_overwrites_existing() {
    let mut root: serde_yaml::Value = serde_yaml::from_str("a:\n  b: old\n").unwrap();
    super::set_nested_yaml_value(&mut root, "a.b", &serde_yaml::Value::String("new".into()))
        .unwrap();

    let val = root
        .get("a")
        .and_then(|v| v.get("b"))
        .and_then(|v| v.as_str());
    assert_eq!(val, Some("new"));
}

#[test]
fn set_nested_yaml_value_creates_deep_path() {
    let mut root = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    super::set_nested_yaml_value(
        &mut root,
        "a.b.c.d",
        &serde_yaml::Value::String("deep".into()),
    )
    .unwrap();

    let val = root
        .get("a")
        .and_then(|v| v.get("b"))
        .and_then(|v| v.get("c"))
        .and_then(|v| v.get("d"))
        .and_then(|v| v.as_str());
    assert_eq!(val, Some("deep"));
}

// --- Profile update with files ---

#[test]
fn profile_update_add_and_remove_files() {
    let dir = create_test_config_dir();

    // Create a test file to import
    let test_file = dir.path().join("testfile.conf");
    std::fs::write(&test_file, "test content").unwrap();

    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Add file to profile
    let args = ProfileUpdateArgs {
        files: vec![format!(
            "{}:{}",
            test_file.display(),
            dir.path().join("deploy").join("testfile.conf").display()
        )],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let managed = &doc.spec.files.as_ref().unwrap().managed;
    assert_eq!(
        managed.len(),
        1,
        "profile should have exactly 1 managed file after add"
    );
    assert!(
        managed[0].source.contains("testfile.conf"),
        "managed file source should reference testfile.conf"
    );

    // Remove file from profile
    let target_path = dir.path().join("deploy").join("testfile.conf");
    let args = ProfileUpdateArgs {
        files: vec![format!("-{}", target_path.display())],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let file_count = doc
        .spec
        .files
        .as_ref()
        .map(|f| f.managed.len())
        .unwrap_or(0);
    assert_eq!(
        file_count, 0,
        "profile should have no managed files after removal"
    );
}

// --- Profile update env add/remove ---

#[test]
fn profile_update_env_add_and_remove() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Add env var
    let args = ProfileUpdateArgs {
        env: vec!["CUSTOM_VAR=hello".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(doc.spec.env.iter().any(|e| e.name == "CUSTOM_VAR"));

    // Remove env var
    let args = ProfileUpdateArgs {
        env: vec!["-CUSTOM_VAR".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(!doc.spec.env.iter().any(|e| e.name == "CUSTOM_VAR"));
}

// --- Profile update alias add/remove ---

#[test]
fn profile_update_alias_add_and_remove() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Add alias
    let args = ProfileUpdateArgs {
        aliases: vec!["ll=ls -la".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(doc.spec.aliases.iter().any(|a| a.name == "ll"));

    // Remove alias
    let args = ProfileUpdateArgs {
        aliases: vec!["-ll".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(!doc.spec.aliases.iter().any(|a| a.name == "ll"));
}

// --- Profile update modules add/remove ---

#[test]
fn profile_update_modules_add_and_remove() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Add module
    let args = ProfileUpdateArgs {
        modules: vec!["neovim".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(doc.spec.modules.contains(&"neovim".to_string()));

    // Remove module
    let args = ProfileUpdateArgs {
        modules: vec!["-neovim".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    assert!(!doc.spec.modules.contains(&"neovim".to_string()));
}

// --- Profile update packages add/remove ---

#[test]
fn profile_update_packages_add_and_remove() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Add package
    let args = ProfileUpdateArgs {
        packages: vec!["brew:jq".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let brew = doc.spec.packages.as_ref().unwrap().brew.as_ref().unwrap();
    assert!(brew.formulae.contains(&"jq".to_string()));

    // Remove package
    let args = ProfileUpdateArgs {
        packages: vec!["-brew:jq".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let brew = doc.spec.packages.as_ref().unwrap().brew.as_ref().unwrap();
    assert!(!brew.formulae.contains(&"jq".to_string()));
}

// --- Profile create with aliases, secrets, scripts ---

#[test]
fn profile_create_with_aliases() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ProfileCreateArgs {
        aliases: vec!["ll=ls -la".to_string(), "gs=git status".to_string()],
        ..test_profile_create_args("alias-profile")
    };
    profile::cmd_profile_create(&cli, &printer, &args).unwrap();

    let doc = config::load_profile(
        &dir.path()
            .join("profiles")
            .join("alias-profile")
            .join("profile.yaml"),
    )
    .unwrap();
    assert_eq!(doc.spec.aliases.len(), 2);
    assert!(doc.spec.aliases.iter().any(|a| a.name == "ll"));
    assert!(doc.spec.aliases.iter().any(|a| a.name == "gs"));
}

#[test]
fn profile_create_with_secrets() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ProfileCreateArgs {
        secrets: vec!["secrets/api.enc:~/.config/app/key".to_string()],
        ..test_profile_create_args("secret-profile")
    };
    profile::cmd_profile_create(&cli, &printer, &args).unwrap();

    let doc = config::load_profile(
        &dir.path()
            .join("profiles")
            .join("secret-profile")
            .join("profile.yaml"),
    )
    .unwrap();
    assert_eq!(doc.spec.secrets.len(), 1);
    assert_eq!(doc.spec.secrets[0].source, "secrets/api.enc");
}

#[test]
fn profile_create_with_scripts() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ProfileCreateArgs {
        pre_apply: vec!["scripts/pre.sh".to_string()],
        post_apply: vec!["scripts/post.sh".to_string()],
        on_change: vec!["scripts/change.sh".to_string()],
        on_drift: vec!["scripts/drift.sh".to_string()],
        ..test_profile_create_args("script-profile")
    };
    profile::cmd_profile_create(&cli, &printer, &args).unwrap();

    let doc = config::load_profile(
        &dir.path()
            .join("profiles")
            .join("script-profile")
            .join("profile.yaml"),
    )
    .unwrap();
    let scripts = doc.spec.scripts.as_ref().unwrap();
    assert_eq!(scripts.pre_apply.len(), 1);
    assert_eq!(scripts.post_apply.len(), 1);
    assert_eq!(scripts.on_change.len(), 1);
    assert_eq!(scripts.on_drift.len(), 1);
}

// --- Profile update on_drift scripts ---

#[test]
fn profile_update_on_drift_scripts() {
    let dir = create_test_config_dir();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let args = ProfileUpdateArgs {
        on_drift: vec!["scripts/drift.sh".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let scripts = doc.spec.scripts.as_ref().unwrap();
    assert_eq!(
        scripts.on_drift,
        vec![config::ScriptEntry::Simple("scripts/drift.sh".to_string())]
    );

    // Remove
    let args = ProfileUpdateArgs {
        on_drift: vec!["-scripts/drift.sh".to_string()],
        ..empty_profile_update_args()
    };
    profile::cmd_profile_update(&cli, &printer, "default", &args).unwrap();

    let doc = config::load_profile(&dir.path().join("profiles").join("default.yaml")).unwrap();
    let scripts = doc.spec.scripts.as_ref().unwrap();
    assert!(scripts.on_drift.is_empty());
}

// --- Module update add/remove env ---

#[test]
fn module_update_env_add_and_remove() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "env-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: env-mod\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Add env
    let args = ModuleUpdateArgs {
        env: vec!["MY_VAR=hello".to_string()],
        ..empty_module_update_args("env-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "env-mod").unwrap();
    assert!(doc.spec.env.iter().any(|e| e.name == "MY_VAR"));

    // Remove env
    let args = ModuleUpdateArgs {
        env: vec!["-MY_VAR".to_string()],
        ..empty_module_update_args("env-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "env-mod").unwrap();
    assert!(!doc.spec.env.iter().any(|e| e.name == "MY_VAR"));
}

// --- Module update add/remove aliases ---

#[test]
fn module_update_alias_add_and_remove() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "alias-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: alias-mod\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Add alias
    let args = ModuleUpdateArgs {
        aliases: vec!["ll=ls -la".to_string()],
        ..empty_module_update_args("alias-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "alias-mod").unwrap();
    assert!(doc.spec.aliases.iter().any(|a| a.name == "ll"));

    // Remove alias
    let args = ModuleUpdateArgs {
        aliases: vec!["-ll".to_string()],
        ..empty_module_update_args("alias-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "alias-mod").unwrap();
    assert!(!doc.spec.aliases.iter().any(|a| a.name == "ll"));
}

// --- Module update add/remove depends ---

#[test]
fn module_update_depends_add_and_remove() {
    let dir = tempfile::tempdir().unwrap();
    create_module_in_dir(
        dir.path(),
        "dep-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: dep-mod\nspec:\n  packages: []\n",
    );

    let cli = test_cli(dir.path());
    let printer = test_printer();

    // Add dependency
    let args = ModuleUpdateArgs {
        depends: vec!["base".to_string()],
        ..empty_module_update_args("dep-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "dep-mod").unwrap();
    assert!(doc.spec.depends.contains(&"base".to_string()));

    // Remove dependency
    let args = ModuleUpdateArgs {
        depends: vec!["-base".to_string()],
        ..empty_module_update_args("dep-mod")
    };
    module::cmd_module_update_local(&cli, &printer, &args).unwrap();

    let (doc, _) = module::load_module_document(dir.path(), "dep-mod").unwrap();
    assert!(!doc.spec.depends.contains(&"base".to_string()));
}

// --- source_cache_dir ---

#[test]
fn source_cache_dir_with_state_dir() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let result = super::source_cache_dir(&cli);
    assert!(
        result.is_ok(),
        "source_cache_dir should return a valid path: {:?}",
        result.err()
    );
    let path = result.unwrap();
    assert!(path.to_string_lossy().contains("sources"));
}

// --- cmd_apply with both skip and only ---

#[test]
fn cmd_apply_dry_run_with_skip_and_only() {
    let (config_dir, state_dir) = setup_test_env();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = ApplyArgs {
        from: None,
        dry_run: true,
        phase: None,
        yes: true,
        skip: vec!["packages.cargo".to_string()],
        only: vec!["packages".to_string()],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    let result = super::apply::cmd_apply(&cli, &printer, &args);
    assert!(
        result.is_ok(),
        "dry-run apply should succeed with both --skip and --only filters: {:?}",
        result.err()
    );

    // Dry-run should NOT create any state store records
    let state = StateStore::open(&state_dir.path().join("state.db")).unwrap();
    assert!(
        state.last_apply().unwrap().is_none(),
        "dry-run with skip+only filters should not create apply records"
    );

    drop(printer);
    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Apply")
            || output.contains("Plan")
            || output.contains("Nothing")
            || output.contains("dry"),
        "dry-run apply with skip+only should produce output, got: {output}"
    );
}

// --- cmd_plan with module filter and structured output ---

#[test]
fn cmd_plan_module_structured_output() {
    let (config_dir, state_dir) = setup_test_env();

    create_module_in_dir(
        config_dir.path(),
        "struct-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: struct-mod\nspec:\n  packages: []\n",
    );

    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: Some("struct-mod".to_string()),
        skip_scripts: false,
        context: "apply".to_string(),
    };

    super::plan::cmd_plan(&cli, &printer, &args).unwrap();
    printer.flush();

    let output = buf.lock().unwrap();
    let parsed = extract_json(&output);
    assert!(
        parsed.get("context").is_some(),
        "plan JSON should have 'context'"
    );
    assert_eq!(parsed["context"], "apply", "plan context should be 'apply'");
    assert!(
        parsed.get("phases").is_some(),
        "plan JSON should have 'phases'"
    );
    assert!(parsed["phases"].is_array(), "phases should be an array");
}

fn setup_rich_test_env() -> (tempfile::TempDir, tempfile::TempDir) {
    let (config_dir, state_dir) = setup_test_env();
    std::fs::write(config_dir.path().join("cfgd.yaml"), RICH_CONFIG_YAML).unwrap();
    (config_dir, state_dir)
}

// --- config_cmd::cmd_config_show ---

#[test]
fn cmd_config_show_with_rich_config() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    super::config_cmd::cmd_config_show(&cli, &printer).unwrap();
    drop(printer);

    let output = cap.human();
    assert!(
        output.contains("Configuration"),
        "missing Configuration header"
    );
    assert!(output.contains("team-config"), "missing source name");
    assert!(output.contains("Daemon"), "missing Daemon section");
}

#[test]
fn cmd_config_show_structured_json() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    super::config_cmd::cmd_config_show(&cli, &printer).unwrap();
    drop(printer);

    let output = buf.lock().unwrap().clone();
    let parsed: serde_json::Value = serde_json::from_str(&output)
        .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {output}"));
    assert_eq!(
        parsed["metadata"]["name"], "my-machine",
        "config show JSON should include metadata.name"
    );
    assert!(
        parsed.get("spec").is_some(),
        "config show JSON should include spec"
    );
}

#[test]
fn cmd_config_show_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());

    let result = super::config_cmd::cmd_config_show(&cli, &test_printer());
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("config file not found")
    );
}

// --- config_cmd::cmd_config_get ---

#[test]
fn cmd_config_get_reads_profile() {
    let (config_dir, state_dir) = setup_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    super::config_cmd::cmd_config_get(&cli, &printer, "profile").unwrap();
    drop(printer);

    let output = cap.human();
    assert!(
        output.contains("default"),
        "config get profile should return 'default', got: {output}"
    );
}

#[test]
fn cmd_config_get_nested_key() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    super::config_cmd::cmd_config_get(&cli, &printer, "daemon.enabled").unwrap();
    drop(printer);

    let output = cap.human();
    assert!(
        output.contains("true"),
        "config get daemon.enabled should return 'true', got: {output}"
    );
}

#[test]
fn cmd_config_get_structured_json() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, cap) =
        cfgd_core::output::Printer::for_test_doc_with_format(cfgd_core::output::OutputFormat::Json);

    super::config_cmd::cmd_config_get(&cli, &printer, "profile").unwrap();
    drop(printer);

    let parsed = cap.json().expect("doc captured json");
    assert_eq!(parsed["key"], "profile");
    assert_eq!(
        parsed["value"], "default",
        "profile value should be 'default'"
    );
}

#[test]
fn cmd_config_get_missing_key_fails() {
    let (config_dir, state_dir) = setup_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::config_cmd::cmd_config_get(&cli, &printer, "nonexistent.path");
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("not found in config"),
        "expected 'not found in config' error, got: {msg}"
    );
}

#[test]
fn cmd_config_get_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = super::config_cmd::cmd_config_get(&cli, &printer, "profile");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("config file not found")
    );
}

// --- config_cmd::cmd_config_set ---

#[test]
fn cmd_config_set_updates_value() {
    let (config_dir, state_dir) = setup_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::config_cmd::cmd_config_set(&cli, &printer, "profile", "work");
    assert!(
        result.is_ok(),
        "config set should succeed: {:?}",
        result.err()
    );

    let cfg = config::load_config(&config_dir.path().join("cfgd.yaml")).unwrap();
    assert_eq!(cfg.spec.profile, Some("work".to_string()));
}

#[test]
fn cmd_config_set_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = super::config_cmd::cmd_config_set(&cli, &printer, "profile", "work");
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("config file not found"),
        "expected typed no-config error, got: {msg}"
    );
}

// --- config_cmd::cmd_config_unset ---

#[test]
fn cmd_config_unset_missing_key_fails() {
    let (config_dir, state_dir) = setup_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::config_cmd::cmd_config_unset(&cli, &printer, "nonexistent");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

#[test]
fn cmd_config_unset_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = super::config_cmd::cmd_config_unset(&cli, &printer, "profile");
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("config file not found"),
        "expected typed no-config error, got: {msg}"
    );
}

// --- cmd_doctor ---

#[test]
fn cmd_doctor_without_config_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
    let cli = test_cli(dir.path());
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    super::doctor::run_doctor(&cli, &printer).unwrap();
    printer.flush();

    let output = buf.lock().unwrap();
    assert!(output.contains("Doctor"), "missing Doctor header");
}

#[test]
fn cmd_doctor_with_rich_config() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    super::doctor::run_doctor(&cli, &printer).unwrap();
    printer.flush();

    let output = buf.lock().unwrap();
    assert!(output.contains("Doctor"), "missing Doctor header");
    assert!(
        output.contains("Package Managers"),
        "missing Package Managers section"
    );
}

// --- load_config_and_profile ---

#[test]
fn load_config_and_profile_returns_correct_config() {
    let (config_dir, state_dir) = setup_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));

    let (cfg, _, resolved) = super::load_config_and_profile(&cli).unwrap();
    assert_eq!(cfg.metadata.name, "t");
    assert!(
        !resolved.merged.env.is_empty(),
        "default profile should have env vars"
    );
}

#[test]
fn load_config_and_profile_missing_profile_fails() {
    let (config_dir, state_dir) = setup_test_env();
    let cli = Cli {
        profile: Some("nonexistent".to_string()),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };

    let result = super::load_config_and_profile(&cli);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("profile not found: nonexistent"),
        "expected 'profile not found: nonexistent' error, got: {msg}"
    );
}

// --- expand_aliases ---

#[test]
fn expand_aliases_config_flag_inline_value() {
    let args = vec![
        "cfgd".into(),
        "--config=/tmp/cfgd.yaml".into(),
        "status".into(),
    ];
    let result = super::expand_aliases(args.clone());
    assert_eq!(result, args);
}

// --- cmd_plan ---

// cmd_plan_invalid_phase_name_fails test removed — ApplyPhase is a
// clap ValueEnum, so invalid phase names are rejected at parse time.

#[test]
fn cmd_plan_invalid_context_fails() {
    let h = CliTestHarness::builder().build();
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "invalid".to_string(),
    };
    let result = super::plan::cmd_plan(&h.cli(), h.printer(), &args);
    assert_error_contains(&result, "Unknown context");
}

#[test]
fn cmd_plan_with_skip_filters_actions() {
    let h = CliTestHarness::builder().build();
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec!["packages".to_string()],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    };
    super::plan::cmd_plan(&h.cli(), h.printer(), &args).unwrap();
    let output = h.output();
    assert!(
        output.contains("Plan") || output.contains("Phase"),
        "plan with skip should show plan, got: {output}"
    );
}

// --- cmd_diff ---

#[test]
fn cmd_diff_module_not_found_succeeds() {
    let h = CliTestHarness::builder().build();
    super::diff::cmd_diff(&h.cli(), h.printer(), Some("nonexistent"), false).unwrap();
    let output = h.output();
    assert!(
        output.contains("not found") || output.contains("Diff"),
        "diff nonexistent module should show not-found or diff header, got: {output}"
    );
}

#[test]
fn cmd_diff_with_module() {
    let h = CliTestHarness::builder()
            .module("diff-mod", "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: diff-mod\nspec:\n  packages: []\n")
            .build();
    super::diff::cmd_diff(&h.cli(), h.printer(), Some("diff-mod"), false).unwrap();
    let output = h.output();
    assert!(
        output.contains("Diff") || output.contains("diff-mod"),
        "diff with module should show diff header or module name, got: {output}"
    );
}

// --- cmd_verify ---

#[test]
fn cmd_verify_with_module_filter() {
    let h = CliTestHarness::builder()
            .module("verify-mod", "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: verify-mod\nspec:\n  packages: []\n")
            .build();
    super::verify::cmd_verify(&h.cli(), h.printer(), Some("verify-mod"), false).unwrap();
    let output = h.output();
    assert!(
        output.contains("Verify") || output.contains("verify-mod"),
        "verify with module should show header, got: {output}"
    );
}

// --- cmd_log ---

#[test]
fn cmd_log_empty_state_succeeds() {
    let h = CliTestHarness::builder().build();
    super::log::cmd_log(h.printer(), 10, None, Some(h.state_path())).unwrap();
    let output = h.output();
    assert!(
        output.contains("Apply History") || output.contains("No applies"),
        "log empty state should show history header, got: {output}"
    );
}

#[test]
fn cmd_log_structured_json_output() {
    let h = CliTestHarness::builder().json().build();
    super::log::cmd_log(h.printer(), 10, None, Some(h.state_path())).unwrap();
    let parsed = h.json_output();
    assert_json_has_fields(&parsed, &["entries"]);
    assert_eq!(
        parsed["entries"],
        serde_json::json!([]),
        "fresh state should have no log entries"
    );
}

// --- cmd_apply ---

#[test]
fn cmd_apply_real_records_state() {
    // A real (non-dry-run) apply must record state. The action is a hermetic
    // file copy whose source and target both live inside the harness's config
    // dir, so the full apply pipeline runs and records state without any
    // network access or host mutation.
    //
    // The shared DEFAULT_PROFILE_YAML declares `cargo: [bat]`; a real apply of
    // it runs `cargo install bat` over the network — flaky in CI (a crates.io
    // HTTP2 blip once made this panic) and irrelevant to what this verifies.
    let h = CliTestHarness::builder().build();

    let files_dir = h.config_path().join("files");
    std::fs::create_dir_all(&files_dir).unwrap();
    std::fs::write(files_dir.join("seed.txt"), "applied content").unwrap();
    let target = h.config_path().join("output").join("seed.txt");
    let default_profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  files:\n    managed:\n      - source: files/seed.txt\n        target: {}\n        strategy: Copy\n",
        target.display()
    );
    std::fs::write(
        h.config_path().join("profiles").join("default.yaml"),
        &default_profile,
    )
    .unwrap();

    let args = ApplyArgs {
        dry_run: false,
        yes: true,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        from: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&h.cli(), h.printer(), &args).unwrap();

    // The hermetic file action actually ran.
    assert!(target.exists(), "managed file should have been created");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "applied content");

    let state = StateStore::open(&h.state_path().join("state.db")).unwrap();
    let last = state.last_apply().unwrap();
    assert!(last.is_some(), "should have recorded an apply in state");
    let record = last.unwrap();
    assert_eq!(record.profile, "default");
    assert!(
        matches!(record.status, cfgd_core::state::ApplyStatus::Success),
        "apply record status should be Success, got: {:?}",
        record.status
    );
}

// cmd_apply_invalid_phase_fails test removed — ApplyPhase is a clap
// ValueEnum, so invalid phase names are rejected at parse time.

#[test]
fn cmd_apply_with_skip_and_only() {
    let h = CliTestHarness::builder().build();
    let args = ApplyArgs {
        dry_run: true,
        yes: true,
        phase: None,
        skip: vec!["packages".to_string()],
        only: vec![],
        module: None,
        from: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&h.cli(), h.printer(), &args).unwrap();
    let output = h.output();
    assert!(
        output.contains("Apply")
            || output.contains("Plan")
            || output.contains("Nothing")
            || output.contains("dry"),
        "apply dry-run with skip should produce output, got: {output}"
    );
}

#[test]
fn cmd_apply_skip_scripts_flag() {
    let h = CliTestHarness::builder().build();
    let args = ApplyArgs {
        dry_run: true,
        yes: true,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        from: None,
        skip_scripts: true,
        context: "apply".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&h.cli(), h.printer(), &args).unwrap();
    let output = h.output();
    assert!(
        output.contains("Apply")
            || output.contains("Plan")
            || output.contains("Nothing")
            || output.contains("dry"),
        "apply dry-run with skip-scripts should produce output, got: {output}"
    );
}

#[test]
fn cmd_apply_invalid_context_fails() {
    let h = CliTestHarness::builder().build();
    let args = ApplyArgs {
        dry_run: true,
        yes: true,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        from: None,
        skip_scripts: false,
        context: "bogus".to_string(),
        shell: None,
    };
    let result = super::apply::cmd_apply(&h.cli(), h.printer(), &args);
    let err = result
        .expect_err("apply with unknown context must fail before any reconcile work runs")
        .to_string();
    assert!(
        err.contains("Unknown context") && err.contains("'bogus'"),
        "error must name the rejected context value, got: {err}"
    );
    assert!(
        err.contains("apply") && err.contains("reconcile"),
        "error must list both valid context values, got: {err}"
    );
}

// `--shell bash` on `cfgd apply` parses to `Some(ApplyShell::Bash)` and lowers
// to `Some(ScriptShell::Bash)`. Guards the debugging-override flag from quiet
// regressions (e.g. a future rename of the variant or a missing value_enum).
#[test]
fn apply_shell_flag_parses() {
    use super::{ApplyShell, Command, apply_shell_to_script_shell};
    let cli = Cli::try_parse_from(["cfgd", "apply", "--shell", "bash", "--yes", "--dry-run"])
        .expect("--shell bash must parse");
    match cli.command {
        Some(Command::Apply(args)) => {
            let s = args.shell.expect("--shell must be Some after parse");
            assert!(matches!(s, ApplyShell::Bash));
            assert!(matches!(
                apply_shell_to_script_shell(s),
                cfgd_core::config::ScriptShell::Bash
            ));
        }
        other => panic!(
            "expected Command::Apply, got subcommand present: {}",
            other.is_some()
        ),
    }
}

// Absent `--shell`, `args.shell` is `None`, so no override is applied and
// per-entry `shell:` fields are honoured.
#[test]
fn apply_shell_flag_default_is_none() {
    use super::Command;
    let cli =
        Cli::try_parse_from(["cfgd", "apply", "--yes", "--dry-run"]).expect("apply must parse");
    match cli.command {
        Some(Command::Apply(args)) => assert!(args.shell.is_none()),
        _ => panic!("expected Command::Apply"),
    }
}

// Reject unknown interpreter values at parse time (clap value_enum).
#[test]
fn apply_shell_flag_rejects_unknown_value() {
    let result = Cli::try_parse_from(["cfgd", "apply", "--shell", "fish", "--yes", "--dry-run"]);
    assert!(result.is_err(), "fish is not a supported interpreter");
}

#[test]
fn cmd_apply_reconcile_context_threads_through() {
    let h = CliTestHarness::builder().build();
    let args = ApplyArgs {
        dry_run: true,
        yes: true,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        from: None,
        skip_scripts: false,
        context: "reconcile".to_string(),
        shell: None,
    };
    super::apply::cmd_apply(&h.cli(), h.printer(), &args).unwrap();
    h.assert_header("Plan");
    let output = h.output();
    assert!(
        output.contains("Nothing to do") || output.contains("action(s) planned"),
        "apply --context reconcile dry-run should still produce a plan, got: {output}"
    );
}

#[test]
fn cmd_apply_phase_post_scripts_catches_module_post_scripts() {
    // Bug regression guard: --phase post-scripts must also re-attempt
    // module-level postApply scripts that live in PhaseName::Modules.
    let (config_dir, state_dir) = setup_test_env();
    let marker = config_dir.path().join("post_script_marker");

    create_module_in_dir(
        config_dir.path(),
        "nvim",
        &format!(
            r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec:
  packages: []
  scripts:
    postApply:
      - touch {}
"#,
            marker.display()
        ),
    );

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - nvim\n";
    std::fs::write(
        config_dir.path().join("profiles").join("default.yaml"),
        profile,
    )
    .unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: Some(ApplyPhase::PostScripts),
        yes: true,
        skip: vec![],
        only: vec![],
        module: Some("nvim".to_string()),
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    super::apply::cmd_apply(&cli, &printer, &args).unwrap();
    printer.flush();
    let output = buf.lock().unwrap().clone();

    assert!(
        !output.contains(MSG_NOTHING_TO_DO),
        "--phase post-scripts on a module with a postApply script must NOT print MSG_NOTHING_TO_DO, got:\n{output}"
    );
    assert!(
        marker.exists(),
        "module postApply script should have executed under --phase post-scripts; marker missing. output:\n{output}"
    );
}

#[test]
fn report_no_in_scope_actions_classifies_outcomes() {
    // No scoping filter active → genuinely "up to date".
    {
        let (printer, buf) = test_printer_capture();
        let scope = ScopeReport {
            filter_active: false,
            unfiltered_total: 0,
            phases_with_work: vec![],
            module_miss: None,
        };
        report_no_in_scope_actions(&printer, &scope, None);
        printer.flush();
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains(MSG_NOTHING_TO_DO),
            "no filter → up-to-date, got:\n{out}"
        );
    }

    // Filter active AND the unfiltered plan had pending work → honest warning,
    // never "up to date"; the files→modules hint fires for --phase files.
    {
        let (printer, buf) = test_printer_capture();
        let scope = ScopeReport {
            filter_active: true,
            unfiltered_total: 3,
            phases_with_work: vec!["Modules".to_string()],
            module_miss: None,
        };
        report_no_in_scope_actions(&printer, &scope, Some(&PhaseName::Files));
        printer.flush();
        let out = buf.lock().unwrap().clone();
        assert!(
            !out.contains(MSG_NOTHING_TO_DO),
            "filter-excluded-all must not claim up-to-date, got:\n{out}"
        );
        assert!(
            out.contains("No actions in scope"),
            "expected warning, got:\n{out}"
        );
        assert!(
            out.contains("module-sourced files apply in the 'modules' phase"),
            "expected files→modules hint, got:\n{out}"
        );
    }

    // Filter active but the plan was empty anyway → genuinely "up to date".
    {
        let (printer, buf) = test_printer_capture();
        let scope = ScopeReport {
            filter_active: true,
            unfiltered_total: 0,
            phases_with_work: vec![],
            module_miss: None,
        };
        report_no_in_scope_actions(&printer, &scope, Some(&PhaseName::Files));
        printer.flush();
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains(MSG_NOTHING_TO_DO),
            "filter active but no pending work → up-to-date, got:\n{out}"
        );
    }

    // --module that resolved to nothing → module-specific warning.
    {
        let (printer, buf) = test_printer_capture();
        let scope = ScopeReport {
            filter_active: true,
            unfiltered_total: 0,
            phases_with_work: vec![],
            module_miss: Some("nvm".to_string()),
        };
        report_no_in_scope_actions(&printer, &scope, None);
        printer.flush();
        let out = buf.lock().unwrap().clone();
        assert!(
            out.contains("Module 'nvm' matched no actions"),
            "expected module-miss warning, got:\n{out}"
        );
        assert!(
            !out.contains(MSG_NOTHING_TO_DO),
            "module miss must not claim up-to-date, got:\n{out}"
        );
    }
}

#[test]
fn apply_phase_files_warns_when_files_are_module_sourced() {
    // Bug guard: `cfgd apply --phase files` for a config whose files come from a
    // module (Modules phase) used to print "everything is up to date" while
    // deploying nothing — a silent no-op. It must instead warn that the active
    // filter excluded pending work, and must not deploy the module's files.
    let (config_dir, state_dir) = setup_test_env();
    let target = config_dir.path().join("deployed-by-module.txt");

    create_module_in_dir(
        config_dir.path(),
        "filekit",
        &format!(
            r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: filekit
spec:
  files:
    - source: files/hello.txt
      target: {}
      strategy: Copy
"#,
            target.display()
        ),
    );
    std::fs::write(
        config_dir
            .path()
            .join("modules")
            .join("filekit")
            .join("files")
            .join("hello.txt"),
        "hello from module\n",
    )
    .unwrap();

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules:\n    - filekit\n";
    std::fs::write(
        config_dir.path().join("profiles").join("default.yaml"),
        profile,
    )
    .unwrap();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();
    let args = ApplyArgs {
        from: None,
        dry_run: false,
        phase: Some(ApplyPhase::Files),
        yes: true,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
        shell: None,
    };

    super::apply::cmd_apply(&cli, &printer, &args).unwrap();
    printer.flush();
    let output = buf.lock().unwrap().clone();

    assert!(
        !output.contains(MSG_NOTHING_TO_DO),
        "--phase files with module-sourced files must NOT claim up-to-date, got:\n{output}"
    );
    assert!(
        output.contains("No actions in scope"),
        "expected the filter-excluded-all warning, got:\n{output}"
    );
    assert!(
        output.contains("module-sourced files apply in the 'modules' phase"),
        "expected the files→modules hint, got:\n{output}"
    );
    assert!(
        !target.exists(),
        "--phase files must not deploy module files; target unexpectedly created. output:\n{output}"
    );
}

// --- cmd_compliance ---

#[test]
fn cmd_compliance_history_invalid_since_fails() {
    let h = CliTestHarness::builder().build();
    let result = super::compliance::cmd_compliance_history(&h.cli(), h.printer(), Some("invalid"));
    assert_error_contains(&result, "invalid --since value");
}

#[test]
fn cmd_compliance_diff_missing_snapshots_fails() {
    let h = CliTestHarness::builder().build();
    let result = super::compliance::cmd_compliance_diff(&h.cli(), h.printer(), 1, 2);
    assert_error_contains(&result, "not found");
}

// --- cmd_decide ---

// cmd_decide_invalid_action_fails test removed — DecideAction is a
// clap ValueEnum, so "invalid" is rejected at parse time and can no
// longer reach cmd_decide at runtime.

// --- source::cmd_source_remove ---

#[test]
fn cmd_source_remove_existing_removes_from_config() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let cfg = config::load_config(&config_dir.path().join("cfgd.yaml")).unwrap();
    assert_eq!(cfg.spec.sources.len(), 1);

    let result =
        super::source::cmd_source_remove(&cli, &printer, "team-config", false, true, false);
    assert!(
        result.is_ok(),
        "source remove should succeed: {:?}",
        result.err()
    );

    let cfg = config::load_config(&config_dir.path().join("cfgd.yaml")).unwrap();
    assert!(cfg.spec.sources.is_empty());
}

#[test]
fn cmd_source_remove_with_keep_all_transfers_resources_to_local_management() {
    // Pre-seed managed_resources with one row claimed by `team-config`. Then
    // call cmd_source_remove(name, keep_all=true, remove_all=false) — the
    // keep_all-with-resources arm (lines 59-69 in source/remove.rs) should
    // re-upsert each row with source="local" before removing the source.
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let store = cfgd_core::state::StateStore::open(&state_dir.path().join("state.db")).unwrap();
    store
        .upsert_managed_resource(
            "file",
            "/etc/managed-by-team-config",
            "team-config",
            Some("h1"),
            None,
        )
        .unwrap();
    drop(store);

    super::source::cmd_source_remove(&cli, &printer, "team-config", true, false, false)
        .expect("source remove --keep-all should succeed");

    // Source dropped from cfgd.yaml.
    let cfg = config::load_config(&config_dir.path().join("cfgd.yaml")).unwrap();
    assert!(
        cfg.spec.sources.is_empty(),
        "source should be removed from cfgd.yaml regardless of keep_all"
    );

    // Resource row now claims source="local" instead of "team-config".
    let store2 = cfgd_core::state::StateStore::open(&state_dir.path().join("state.db")).unwrap();
    let leftovers = store2.managed_resources_by_source("team-config").unwrap();
    assert!(
        leftovers.is_empty(),
        "no resources should still be attributed to the removed source"
    );
    let local_rows = store2.managed_resources_by_source("local").unwrap();
    assert!(
        local_rows
            .iter()
            .any(|r| r.resource_id == "/etc/managed-by-team-config"),
        "the transferred resource should now show under source=local: {local_rows:?}"
    );
}

#[test]
fn cmd_source_remove_nonexistent_fails() {
    let (config_dir, state_dir) = setup_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result =
        super::source::cmd_source_remove(&cli, &printer, "nonexistent", false, true, false);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Source 'nonexistent' not found"),
        "expected 'Source not found' error, got: {msg}"
    );
}

#[test]
fn cmd_source_remove_deletes_cached_clone() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    // Seed a cached clone for the source, mirroring what `source add` leaves at
    // `<cache_dir>/sources/<name>`.
    let cached_dir = state_dir.path().join("sources").join("team-config");
    std::fs::create_dir_all(&cached_dir).unwrap();
    std::fs::write(cached_dir.join("marker"), b"cached").unwrap();
    assert!(cached_dir.exists());

    super::source::cmd_source_remove(&cli, &printer, "team-config", false, true, false)
        .expect("source remove should succeed");

    assert!(
        !cached_dir.exists(),
        "cached clone dir must be deleted on source remove, still present at {}",
        cached_dir.display()
    );
}

#[test]
fn cmd_source_replace_clears_stale_cache() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    // Seed a stale cached clone for the existing source.
    let cached_dir = state_dir.path().join("sources").join("team-config");
    std::fs::create_dir_all(&cached_dir).unwrap();
    std::fs::write(cached_dir.join("STALE"), b"old contents").unwrap();

    // Replace fails at the add step (unreachable URL is rejected before clone),
    // but the remove step must still have cleared the stale cache so a later
    // successful add cannot inherit the previous source's contents.
    let _ = super::source::cmd_source_replace(
        &cli,
        &printer,
        "team-config",
        "file:///nonexistent/new-config.git",
    );

    assert!(
        !cached_dir.join("STALE").exists(),
        "replace must clear the old source's stale cache; STALE marker survived"
    );
}

// --- source::cmd_source_override ---

#[test]
fn cmd_source_override_reject_succeeds() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    super::source::cmd_source_override(
        &cli,
        &printer,
        "team-config",
        super::SourceOverrideAction::Reject,
        "packages.brew",
        None,
    )
    .unwrap();

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Rejected") && output.contains("packages.brew"),
        "should confirm rejection of packages.brew, got: {output}"
    );
}

#[test]
fn cmd_source_override_set_succeeds() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    super::source::cmd_source_override(
        &cli,
        &printer,
        "team-config",
        super::SourceOverrideAction::Set,
        "packages.brew.ripgrep",
        Some("true"),
    )
    .unwrap();

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("Override set") && output.contains("packages.brew.ripgrep"),
        "should confirm override set for packages.brew.ripgrep, got: {output}"
    );
}

#[test]
fn cmd_source_override_set_stores_typed_list() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();

    super::source::cmd_source_override(
        &cli,
        &printer,
        "team-config",
        super::SourceOverrideAction::Set,
        "packages.npm.global",
        Some("[prettier]"),
    )
    .unwrap();
    drop(printer);

    let written = std::fs::read_to_string(config_dir.path().join("cfgd.yaml")).unwrap();
    let cfg: serde_yaml::Value = serde_yaml::from_str(&written).unwrap();
    let global = cfg["spec"]["sources"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|s| s["name"] == serde_yaml::Value::String("team-config".into()))
        .unwrap()["subscription"]["overrides"]["packages"]["npm"]["global"]
        .clone();

    assert!(
        global.is_sequence(),
        "global should be a YAML list, not a string, got: {global:?}"
    );
    assert_eq!(
        global.as_sequence().unwrap(),
        &vec![serde_yaml::Value::String("prettier".into())],
        "list override should store [prettier], got: {global:?}"
    );
}

#[test]
fn cmd_source_override_set_stores_plain_string() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();

    super::source::cmd_source_override(
        &cli,
        &printer,
        "team-config",
        super::SourceOverrideAction::Set,
        "env.EDITOR",
        Some("nvim"),
    )
    .unwrap();
    drop(printer);

    let written = std::fs::read_to_string(config_dir.path().join("cfgd.yaml")).unwrap();
    let cfg: serde_yaml::Value = serde_yaml::from_str(&written).unwrap();
    let editor = cfg["spec"]["sources"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|s| s["name"] == serde_yaml::Value::String("team-config".into()))
        .unwrap()["subscription"]["overrides"]["env"]["EDITOR"]
        .clone();

    assert_eq!(
        editor,
        serde_yaml::Value::String("nvim".into()),
        "string override should store the plain value, got: {editor:?}"
    );
}

#[test]
fn cmd_source_override_set_env_value_always_string() {
    // env values are always strings; a `true`/`8080`-looking value must NOT be
    // YAML-parsed into a bool/number (which would fail EnvVar deserialization at
    // compose time).
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();

    super::source::cmd_source_override(
        &cli,
        &printer,
        "team-config",
        super::SourceOverrideAction::Set,
        "env.DEBUG",
        Some("true"),
    )
    .unwrap();
    drop(printer);

    let written = std::fs::read_to_string(config_dir.path().join("cfgd.yaml")).unwrap();
    let cfg: serde_yaml::Value = serde_yaml::from_str(&written).unwrap();
    let debug = cfg["spec"]["sources"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|s| s["name"] == serde_yaml::Value::String("team-config".into()))
        .unwrap()["subscription"]["overrides"]["env"]["DEBUG"]
        .clone();

    assert_eq!(
        debug,
        serde_yaml::Value::String("true".into()),
        "env value 'true' must be stored as the STRING \"true\", not a bool, got: {debug:?}"
    );
}

#[test]
fn cmd_source_override_set_env_scope_snake_normalizes() {
    // The override path's leading field is a ProfileSpec field (camelCase wire
    // name). A user typing the snake_case `env_scope` must land as `envScope`,
    // otherwise deny_unknown_fields rejects it cryptically at compose time.
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, _cap) = cfgd_core::output::Printer::for_test_doc();

    super::source::cmd_source_override(
        &cli,
        &printer,
        "team-config",
        super::SourceOverrideAction::Set,
        "env_scope",
        Some("All"),
    )
    .unwrap();
    drop(printer);

    let written = std::fs::read_to_string(config_dir.path().join("cfgd.yaml")).unwrap();
    let cfg: serde_yaml::Value = serde_yaml::from_str(&written).unwrap();
    let overrides = cfg["spec"]["sources"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|s| s["name"] == serde_yaml::Value::String("team-config".into()))
        .unwrap()["subscription"]["overrides"]
        .clone();

    assert_eq!(
        overrides["envScope"],
        serde_yaml::Value::String("All".into()),
        "snake_case env_scope must be stored under the camelCase key envScope, got: {overrides:?}"
    );
    assert!(
        overrides.get("env_scope").is_none(),
        "the snake_case key must not be persisted verbatim, got: {overrides:?}"
    );
}

// cmd_source_override_invalid_action_fails test removed —
// SourceOverrideAction is a clap ValueEnum so invalid strings fail at
// parse time.

#[test]
fn cmd_source_override_nonexistent_source_fails() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::source::cmd_source_override(
        &cli,
        &printer,
        "nonexistent",
        super::SourceOverrideAction::Reject,
        "packages.brew",
        None,
    );
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Source 'nonexistent' not found"),
        "expected 'Source not found' error, got: {msg}"
    );
}

// --- source::cmd_source_priority ---

#[test]
fn cmd_source_priority_nonexistent_fails() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::source::cmd_source_priority(&cli, &printer, "nonexistent", None);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("source 'nonexistent' not found"),
        "expected 'source not found' error, got: {msg}"
    );
}

#[test]
fn cmd_source_priority_updates_config() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = super::source::cmd_source_priority(&cli, &printer, "team-config", Some(200));
    assert!(result.is_ok(), "source priority update: {:?}", result.err());

    let cfg = config::load_config(&config_dir.path().join("cfgd.yaml")).unwrap();
    assert_eq!(cfg.spec.sources[0].subscription.priority, 200);
}

// --- source::cmd_source_show ---

#[test]
fn cmd_source_show_exists() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let result = super::source::cmd_source_show(&cli, &printer, "team-config");
    assert!(
        result.is_ok(),
        "source show should succeed: {:?}",
        result.err()
    );
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("team-config") || output.contains("Source"),
        "source show should display source info, got: {output}"
    );
}

#[test]
fn cmd_source_show_structured_json() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_with_format(cfgd_core::output::OutputFormat::Json);

    super::source::cmd_source_show(&cli, &printer, "team-config").unwrap();
    drop(printer);

    let output = buf.lock().unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&output)
        .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {output}"));
    assert_eq!(parsed["name"], "team-config");
    assert_eq!(parsed["url"], "https://github.com/team/config");
    assert_eq!(parsed["priority"], 100);
}

// --- source::cmd_source_create ---

#[test]
fn cmd_source_create_initializes_manifest() {
    let (config_dir, state_dir) = setup_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    // source create writes manifest in the config directory itself
    let result = super::source::cmd_source_create(
        &cli,
        &printer,
        Some("my-source"),
        Some("A test source"),
        Some("1.0.0"),
    );
    assert!(
        result.is_ok(),
        "source create should succeed: {:?}",
        result.err()
    );

    // Verify the manifest file was created with expected content
    let manifest_path = config_dir.path().join("cfgd-source.yaml");
    assert!(manifest_path.exists(), "cfgd-source.yaml should be created");
    let content = std::fs::read_to_string(&manifest_path).unwrap();
    assert!(
        content.contains("my-source"),
        "manifest should contain source name, got: {content}"
    );
}

// --- source::cmd_source_list ---

#[test]
fn cmd_source_list_with_sources_shows_entries() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    let result = super::source::cmd_source_list(&cli, &printer);
    assert!(
        result.is_ok(),
        "source list with sources: {:?}",
        result.err()
    );

    drop(printer);
    let output = cap.human();
    assert!(
        output.contains("team-config") || output.contains("Config Sources"),
        "source list should show source entries, got: {output}"
    );
}

#[test]
fn cmd_source_list_structured_json() {
    let (config_dir, state_dir) = setup_rich_test_env();
    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    super::source::cmd_source_list(&cli, &printer).unwrap();

    drop(printer);
    let parsed = cap
        .json()
        .expect("source list should emit a Doc with payload");
    let arr = parsed
        .as_array()
        .expect("source list JSON should be an array");
    assert_eq!(arr.len(), 1, "rich config has one source");
    assert_eq!(arr[0]["name"], "team-config");
    assert_eq!(arr[0]["url"], "https://github.com/team/config");
}

// --- workflow::cmd_workflow_generate ---

#[test]
fn cmd_workflow_generate_with_git_repo() {
    let (config_dir, state_dir) = setup_test_env();
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(config_dir.path())
        .output()
        .ok();

    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();

    let result = super::workflow::cmd_workflow_generate(&cli, &printer, true);
    assert!(
        result.is_ok(),
        "workflow generate with git repo should succeed: {:?}",
        result.err()
    );

    let workflow_dir = config_dir.path().join(".github").join("workflows");
    assert!(
        workflow_dir.exists(),
        "workflow directory should be created"
    );

    drop(printer);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Generated") || output.contains("workflow"),
        "workflow generate should mention generated workflow, got: {output}"
    );
}

// --- cmd_rollback ---

#[test]
fn cmd_rollback_missing_apply_id_fails() {
    let state_dir = tempfile::tempdir().unwrap();
    let printer = test_printer();

    let result = super::rollback::cmd_rollback(&printer, 99, true, Some(state_dir.path()));
    assert!(result.is_err(), "rollback with no history should fail");
}

// --- open_state_store ---

#[test]
fn open_state_store_creates_db_file() {
    let state_dir = tempfile::tempdir().unwrap();

    let result = super::open_state_store(Some(state_dir.path()));
    assert!(
        result.is_ok(),
        "open_state_store should succeed: {:?}",
        result.err()
    );

    let db_path = state_dir.path().join("state.db");
    assert!(db_path.exists(), "state.db file should be created");
    assert!(
        std::fs::metadata(&db_path).unwrap().len() > 0,
        "state.db should not be empty"
    );
}

/// Regression: an explicit `--state-dir`/`CFGD_STATE_DIR` pointed at the
/// platform default dir must open the SAME db file the default path would,
/// not a sibling. Both resolve `state.db`, and exactly one `*.db` is created.
#[test]
#[serial_test::serial(default_state_store)]
fn open_state_store_override_matches_default_filename() {
    use cfgd_core::test_helpers::EnvVarGuard;

    let dir = tempfile::tempdir().unwrap();
    let _state_env = EnvVarGuard::set("CFGD_STATE_DIR", dir.path().to_str().unwrap());

    // The default path honors CFGD_STATE_DIR; the override path is handed the
    // same dir. They must land on identical basenames.
    let from_override = super::open_state_store(Some(dir.path()));
    assert!(
        from_override.is_ok(),
        "override open failed: {:?}",
        from_override.err()
    );
    drop(from_override);
    let from_default = super::open_state_store(None);
    assert!(
        from_default.is_ok(),
        "default open failed: {:?}",
        from_default.err()
    );

    assert!(
        dir.path().join("state.db").exists(),
        "both paths resolve state.db"
    );
    assert!(
        !dir.path().join("cfgd.db").exists(),
        "no divergent cfgd.db sibling should be created"
    );
    let dbs: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "db"))
        .collect();
    assert_eq!(
        dbs.len(),
        1,
        "exactly one *.db file should be created, found: {dbs:?}"
    );
}

// --- module commands ---

#[test]
fn cmd_module_search_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result = module::cmd_module_search(&cli, &printer, "test");
    let err = result.unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<cfgd_core::errors::CfgdError>(),
            Some(cfgd_core::errors::CfgdError::Config(
                cfgd_core::errors::ConfigError::NotFound { .. }
            ))
        ),
        "expected typed ConfigError::NotFound, got: {err}"
    );
    assert!(
        err.to_string().contains("config file not found"),
        "expected typed no-config error, got: {err}"
    );
}

#[test]
fn cmd_module_search_no_registries() {
    let (config_dir, state_dir) = setup_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let (printer, buf) = test_printer_capture();

    // Config exists but has no module registries
    let result = module::cmd_module_search(&cli, &printer, "test");
    assert!(
        result.is_ok(),
        "search with no registries should succeed: {:?}",
        result.err()
    );
    drop(printer);

    let output = buf.lock().unwrap();
    assert!(
        output.contains("No module registries") || output.contains("Search"),
        "search with no registries should say no registries, got: {output}"
    );
}

#[test]
fn cmd_module_search_no_registries_structured() {
    let (config_dir, state_dir) = setup_test_env();
    let cli = Cli {
        output: OutputFormatArg(cfgd_core::output::OutputFormat::Json),
        ..test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()))
    };
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();

    module::cmd_module_search(&cli, &printer, "test").unwrap();
    drop(printer);

    let parsed = cap.json().expect("doc captured json");
    let arr = parsed.as_array().expect("search JSON should be an array");
    assert_eq!(arr.len(), 0, "no registries should yield zero results");
}

#[test]
fn cmd_module_build_no_module_yaml_fails() {
    let dir = tempfile::tempdir().unwrap();
    let printer = test_printer();

    let result = module::cmd_module_build(
        &printer,
        &dir.path().display().to_string(),
        None,
        None,
        None,
        false,
        None,
    );
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("does not contain a module.yaml")
    );
}

#[test]
#[serial_test::serial]
fn cmd_module_keys_generate_no_cosign_fails() {
    // Parallel CosignTestShim tests set CFGD_COSIGN_BIN; force require_cosign
    // through the PATH-only branch, and empty PATH so the missing-tool error
    // fires whether or not the host has cosign. Spawn-exclusion guard first
    // so it drops last, bracketing the empty-PATH window.
    let _spawn_excl = cfgd_core::test_helpers::path_env_mutation_guard();
    let _g = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_COSIGN_BIN");
    let _path = cfgd_core::test_helpers::EnvVarGuard::set("PATH", "");
    let printer = test_printer();

    let result = module::cmd_module_keys_generate(&printer, None);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("cosign not found"));
}

#[test]
#[serial_test::serial]
fn cmd_module_keys_rotate_no_cosign_fails() {
    let _spawn_excl = cfgd_core::test_helpers::path_env_mutation_guard();
    let _g = cfgd_core::test_helpers::EnvVarGuard::unset("CFGD_COSIGN_BIN");
    let _path = cfgd_core::test_helpers::EnvVarGuard::set("PATH", "");
    let printer = test_printer();

    let result = module::cmd_module_keys_rotate(&printer, None, &[]);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("cosign not found"));
}

#[test]
fn cmd_module_add_from_registry_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();

    let result =
        module::cmd_module_add_from_registry(&cli, &printer, "myregistry/mymod", false, false);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("config file not found"),
        "expected typed no-config error, got: {msg}"
    );
}

#[test]
fn cmd_module_add_from_registry_invalid_ref_fails() {
    let (config_dir, state_dir) = setup_test_env();
    let cli = test_cli_with_state(config_dir.path(), Some(state_dir.path().to_path_buf()));
    let printer = test_printer();

    let result = module::cmd_module_add_from_registry(&cli, &printer, "no-slash", false, false);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Invalid registry reference")
    );
}

#[test]
fn cmd_module_add_from_registry_not_configured_fails() {
    let h = CliTestHarness::builder().build();
    let result = module::cmd_module_add_from_registry(
        &h.cli(),
        h.printer(),
        "myregistry/mymod@v1.0",
        false,
        false,
    );
    assert_error_contains(&result, "not configured");
}

// -----------------------------------------------------------------------
// New coverage: cmd_status_module
// -----------------------------------------------------------------------

#[test]
fn cmd_status_module_not_found_output() {
    let h = CliTestHarness::builder().build();
    super::status::cmd_status_module(&h.cli(), h.printer(), "nonexistent").unwrap();
    h.assert_output_contains("nonexistent");
    h.assert_output_contains("not found");
}

#[test]
fn cmd_status_module_not_found_json() {
    let h = CliTestHarness::builder().json().build();
    super::status::cmd_status_module(&h.cli(), h.printer(), "ghost-mod").unwrap();
    let parsed = h.json_output();
    assert_eq!(parsed["name"], "ghost-mod");
    assert_eq!(parsed["status"], "not found");
    assert_eq!(parsed["packages"], 0);
    assert_eq!(parsed["files"], 0);
}

#[test]
fn cmd_status_module_found_output() {
    let h = CliTestHarness::builder()
            .module("my-mod", "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: my-mod\nspec:\n  packages:\n    - name: ripgrep\n  files: []\n")
            .build();
    super::status::cmd_status_module(&h.cli(), h.printer(), "my-mod").unwrap();
    h.assert_output_contains("my-mod");
    // Status shows package count, not individual package names
    h.assert_output_contains("1");
}

#[test]
fn cmd_status_module_found_json() {
    let h = CliTestHarness::builder()
            .json()
            .module("my-mod", "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: my-mod\nspec:\n  packages:\n    - name: ripgrep\n  files: []\n  depends:\n    - base\n")
            .build();
    super::status::cmd_status_module(&h.cli(), h.printer(), "my-mod").unwrap();
    let parsed = h.json_output();
    assert_eq!(parsed["name"], "my-mod");
    assert_eq!(parsed["packages"], 1);
    assert_eq!(parsed["files"], 0);
    assert_eq!(parsed["status"], "not applied");
    assert_json_has_fields(
        &parsed,
        &[
            "name",
            "packages",
            "files",
            "depends",
            "status",
            "lastApplied",
        ],
    );
}

// -----------------------------------------------------------------------
// New coverage: source::cmd_source_add error paths
// -----------------------------------------------------------------------

#[test]
fn cmd_source_add_duplicate_fails() {
    let h = CliTestHarness::builder().rich_config().build();
    let args = SourceAddArgs {
        url: "https://github.com/team/config".to_string(),
        name: Some("team-config".to_string()),
        branch: None,
        profile: None,
        accept_recommended: false,
        priority: None,
        opt_in: vec![],
        sync_interval: None,
        auto_apply: false,
        pin_version: None,
        yes: true,
    };
    let result = super::source::cmd_source_add(&h.cli(), h.printer(), &args);
    assert_error_contains(&result, "already exists");
}

// -----------------------------------------------------------------------
// New coverage: cmd_pull / cmd_sync output
// -----------------------------------------------------------------------

#[test]
fn cmd_pull_non_git_dir_shows_warning() {
    let h = CliTestHarness::builder().build();
    // config dir is not a git repo, so git_pull_sync will fail gracefully
    super::pull::cmd_pull(&h.cli(), h.printer()).unwrap();
    let output = h.output();
    assert!(
        output.contains("Pull"),
        "pull output should contain Pull heading, got: {output}"
    );
    assert!(
        output.contains("Pull failed") || output.contains("up to date"),
        "pull in non-git dir should warn or show up-to-date, got: {output}"
    );
}

#[test]
fn cmd_sync_non_git_dir_shows_output() {
    let h = CliTestHarness::builder().build();
    super::sync::cmd_sync(&h.cli(), h.printer()).unwrap();
    h.assert_header("Sync");
}

// -----------------------------------------------------------------------
// New coverage: config_cmd::cmd_config_edit error path
// -----------------------------------------------------------------------

#[test]
fn cmd_config_edit_no_config_fails() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let printer = test_printer();
    let result = super::config_cmd::cmd_config_edit(&cli, &printer);
    assert!(result.is_err());
    assert_error_contains(&result, "config file not found");
}

#[test]
#[cfg(unix)]
#[serial_test::serial]
fn cmd_config_edit_with_invalid_config_and_prompt_declined_breaks_with_warning() {
    // Drive cmd_config_edit's validate loop into the prompt-decline branch.
    // EDITOR=/bin/true leaves the invalid config in place; load_config Errs;
    // the prompt fires; queue's Confirm(false) breaks the loop with the
    // "Saved with validation errors" Doc.
    let dir = tempfile::tempdir().unwrap();
    // Write invalid YAML AT cli.config so the loop's first iteration
    // takes the Err arm.
    std::fs::write(dir.path().join("cfgd.yaml"), "not a Config document").unwrap();

    let cli = test_cli(dir.path());
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc_with_prompt_responses(vec![
        cfgd_core::output::PromptAnswer::Confirm(false),
    ]);

    let _editor = cfgd_core::test_helpers::EditorGuard::set("/usr/bin/true");
    super::config_cmd::cmd_config_edit(&cli, &printer).expect("edit must Ok on save-with-errors");
    drop(printer);

    let output = cap.human();
    assert!(
        output.contains("Saved with validation errors"),
        "should warn: {output}"
    );
}

// -----------------------------------------------------------------------
// New coverage: source::cmd_source_update error paths
// -----------------------------------------------------------------------

#[test]
fn cmd_source_update_no_sources_succeeds() {
    let h = CliTestHarness::builder().build();
    super::source::cmd_source_update(&h.cli(), h.printer(), None).unwrap();
    h.assert_header("Update Sources");
    h.assert_output_contains("No sources configured");
}

#[test]
fn cmd_source_update_named_not_found_fails() {
    // Need a config with sources so it doesn't take the "no sources" early return
    let h = CliTestHarness::builder().rich_config().build();
    let result = super::source::cmd_source_update(&h.cli(), h.printer(), Some("nonexistent"));
    assert_error_contains(&result, "not found");
}

// -----------------------------------------------------------------------
// New coverage: source::cmd_source_replace error path
// -----------------------------------------------------------------------

#[test]
fn cmd_source_replace_nonexistent_fails() {
    let h = CliTestHarness::builder().build();
    let result = super::source::cmd_source_replace(
        &h.cli(),
        h.printer(),
        "nonexistent",
        "https://github.com/new/config.git",
    );
    assert_error_contains(&result, "not found");
}

// -----------------------------------------------------------------------
// New coverage: cmd_compliance_snapshot / cmd_compliance_export
// -----------------------------------------------------------------------

#[test]
fn cmd_compliance_snapshot_empty_state() {
    let h = CliTestHarness::builder().build();
    super::compliance::cmd_compliance_snapshot(&h.cli(), h.printer()).unwrap();
    let output = h.output();
    assert!(
        output.contains("Compliance") || output.contains("Snapshot"),
        "compliance snapshot should produce compliance-related output, got: {output}"
    );
}

#[test]
fn cmd_compliance_export_empty_state() {
    let h = CliTestHarness::builder().build();
    super::compliance::cmd_compliance_export(&h.cli(), h.printer()).unwrap();
    // export writes to a file and prints success message
    let output = h.output();
    assert!(
        output.contains("snapshot") || output.contains("Compliance"),
        "compliance export should mention snapshot, got: {output}"
    );
}

// -----------------------------------------------------------------------
// New coverage: cmd_compliance structured output
// -----------------------------------------------------------------------

#[test]
fn cmd_compliance_snapshot_json() {
    let h = CliTestHarness::builder().json().build();
    super::compliance::cmd_compliance_snapshot(&h.cli(), h.printer()).unwrap();
    let parsed = h.json_output();
    // Compliance snapshot JSON wraps a snapshot object
    assert_json_has_fields(&parsed, &["snapshot"]);
}

#[test]
fn cmd_compliance_history_json() {
    let h = CliTestHarness::builder().json().build();
    super::compliance::cmd_compliance_history(&h.cli(), h.printer(), None).unwrap();
    let output = h.output();
    // History output may be an array or object — just verify it's valid JSON
    let _parsed: serde_json::Value = serde_json::from_str(output.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {output}"));
}

// -----------------------------------------------------------------------
// New coverage: execute dispatch for remaining commands
// -----------------------------------------------------------------------

// execute dispatch tests for plan/sync/pull/compliance already exist above

// -----------------------------------------------------------------------
// JSON schema tests for --output json commands
// -----------------------------------------------------------------------

#[test]
fn json_schema_status() {
    let h = CliTestHarness::builder().json().build();
    super::status::cmd_status(&h.cli(), h.printer(), None, false).unwrap();
    let parsed = h.json_output();
    assert_json_has_fields(
        &parsed,
        &[
            "lastApply",
            "drift",
            "modules",
            "sources",
            "pendingDecisions",
        ],
    );
    assert_json_field_type(&parsed, "modules", "array");
    assert_json_field_type(&parsed, "sources", "array");
    assert_json_field_type(&parsed, "pendingDecisions", "array");
}

#[test]
fn json_schema_status_module() {
    // Module dir name must match metadata.name in module.yaml
    let h = CliTestHarness::builder()
        .json()
        .module("test-mod", SIMPLE_MODULE_YAML)
        .build();
    super::status::cmd_status_module(&h.cli(), h.printer(), "test-mod").unwrap();
    let parsed = h.json_output();
    assert_json_has_fields(
        &parsed,
        &[
            "name",
            "packages",
            "files",
            "depends",
            "status",
            "lastApplied",
        ],
    );
    assert_json_field_type(&parsed, "name", "string");
    assert_json_field_type(&parsed, "packages", "number");
    assert_json_field_type(&parsed, "files", "number");
    assert_json_field_type(&parsed, "depends", "array");
    assert_json_field_type(&parsed, "status", "string");
}

#[test]
fn json_schema_log() {
    let h = CliTestHarness::builder().json().build();
    super::log::cmd_log(h.printer(), 10, None, Some(h.state_path())).unwrap();
    let parsed = h.json_output();
    assert_json_has_fields(&parsed, &["entries"]);
    assert_json_field_type(&parsed, "entries", "array");
}

#[test]
fn json_schema_plan() {
    let h = CliTestHarness::builder().json().build();
    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    };
    super::plan::cmd_plan(&h.cli(), h.printer(), &args).unwrap();
    let parsed = h.json_output();
    assert_json_has_fields(&parsed, &["context", "phases", "totalActions"]);
    assert_json_field_type(&parsed, "context", "string");
    assert_json_field_type(&parsed, "phases", "array");
    assert_json_field_type(&parsed, "totalActions", "number");
}

#[test]
fn json_schema_config_show() {
    let h = CliTestHarness::builder().json().rich_config().build();
    super::config_cmd::cmd_config_show(&h.cli(), h.printer()).unwrap();
    let parsed = h.json_output();
    assert_json_has_fields(&parsed, &["metadata", "spec"]);
    assert_json_field_type(&parsed, "metadata", "object");
    assert_json_field_type(&parsed, "spec", "object");
}

#[test]
fn json_schema_doctor() {
    let h = CliTestHarness::builder().json().build();
    super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();
    let parsed = h.json_output();
    assert_json_has_fields(
        &parsed,
        &["config", "packageManagers", "systemConfigurators"],
    );
    assert_json_field_type(&parsed, "config", "object");
    assert_json_field_type(&parsed, "packageManagers", "array");
    assert_json_field_type(&parsed, "systemConfigurators", "array");
}

#[test]
fn json_schema_verify() {
    let h = CliTestHarness::builder().json().build();
    super::verify::cmd_verify(&h.cli(), h.printer(), None, false).unwrap();
    let parsed = h.json_output();
    assert_json_has_fields(&parsed, &["passCount", "failCount", "results"]);
    assert_json_field_type(&parsed, "passCount", "number");
    assert_json_field_type(&parsed, "failCount", "number");
    assert_json_field_type(&parsed, "results", "array");
}

#[test]
fn json_schema_source_list() {
    let h = CliTestHarness::builder().json().rich_config().build();
    super::source::cmd_source_list(&h.cli(), h.printer()).unwrap();
    let output = h.output();
    // Source list writes a JSON array; output may have trailing newlines from capture()
    // Use serde_json::from_str on trimmed output, finding the JSON portion
    let start = output
        .find('[')
        .unwrap_or_else(|| panic!("no JSON array in output: {output}"));
    // Find the matching closing bracket
    let json_substr = &output[start..];
    let end = json_substr.rfind(']').expect("no closing ] in JSON") + 1;
    let parsed: serde_json::Value = serde_json::from_str(&json_substr[..end])
        .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {json_substr}"));
    assert!(parsed.is_array(), "source list JSON should be an array");
    let arr = parsed.as_array().unwrap();
    assert_eq!(arr.len(), 1, "rich config has 1 source");
    assert_json_has_fields(&arr[0], &["name", "url", "priority", "status"]);
}

#[test]
fn json_schema_source_show() {
    let h = CliTestHarness::builder().json().rich_config().build();
    super::source::cmd_source_show(&h.cli(), h.printer(), "team-config").unwrap();
    let parsed = h.json_output();
    assert_json_has_fields(&parsed, &["name", "url"]);
}

// -----------------------------------------------------------------------
// cmd_secret_init — creates age key and .sops.yaml
// -----------------------------------------------------------------------

#[test]
fn secret_init_prints_header_and_key_path() {
    // cmd_secret_init calls secrets::init_age_key which shells out to age-keygen.
    // If age-keygen is not installed, the error message should mention it.
    let h = CliTestHarness::builder().build();
    let result = super::secret::cmd_secret_init(&h.cli(), h.printer());

    match result {
        Ok(()) => {
            // age-keygen was available: verify output mentions key path and completion
            let output = h.output();
            assert!(
                output.contains("Secrets Initialized") || output.contains("already initialized"),
                "expected secrets section header, got: {output}"
            );
            assert!(
                output.contains("Age key") || output.contains("age-key"),
                "expected key path in output, got: {output}"
            );
            assert!(
                output.contains("Secrets setup complete") || output.contains("already initialized"),
                "expected completion message, got: {output}"
            );
        }
        Err(e) => {
            // age-keygen not installed: error should mention it
            let msg = e.to_string();
            assert!(
                msg.contains("age-keygen"),
                "expected error mentioning age-keygen, got: {msg}"
            );
        }
    }
}

// -----------------------------------------------------------------------
// resolve_secret_backend / get_secret_backend — error paths
// -----------------------------------------------------------------------

#[test]
fn resolve_secret_backend_file_not_found() {
    let h = CliTestHarness::builder().rich_config().build();
    let nonexistent = h.config_path().join("does-not-exist.yaml");
    let result = super::resolve_secret_backend(&h.cli(), &nonexistent);
    assert_error_contains(&result.map(|_| ()), "File not found");
}

#[test]
fn get_secret_backend_file_not_found() {
    let h = CliTestHarness::builder().rich_config().build();
    let nonexistent = h.config_path().join("nonexistent-secret.yaml");
    let result = super::get_secret_backend(&h.cli(), &nonexistent);
    assert_error_contains(&result.map(|_| ()), "File not found");
}

#[test]
fn resolve_secret_backend_no_config_file_errors() {
    // When no config file exists, resolve_secret_backend should fail because
    // load_config can't find the config file.
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli(dir.path());
    let nonexistent = dir.path().join("secret.enc.yaml");
    let result = super::resolve_secret_backend(&cli, &nonexistent);
    // Config file missing: load_config will fail
    match result {
        Ok(_) => panic!("expected error when config file is missing"),
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("config file not found") || msg.contains("File not found"),
                "expected config or file error, got: {msg}"
            );
        }
    }
}

#[test]
fn cmd_secret_encrypt_file_not_found() {
    let h = CliTestHarness::builder().rich_config().build();
    let nonexistent = h.config_path().join("missing.enc.yaml");
    let result = super::secret::cmd_secret_encrypt(&h.cli(), h.printer(), &nonexistent);
    assert_error_contains(&result, "File not found");
}

#[test]
fn cmd_secret_decrypt_file_not_found() {
    let h = CliTestHarness::builder().rich_config().build();
    let nonexistent = h.config_path().join("missing.enc.yaml");
    let result = super::secret::cmd_secret_decrypt(&h.cli(), h.printer(), &nonexistent);
    assert_error_contains(&result, "File not found");
}

// -----------------------------------------------------------------------
// cmd_daemon_status — no daemon running
// -----------------------------------------------------------------------

#[test]
fn daemon_status_no_daemon_running_human_output() {
    let h = CliTestHarness::builder().build();
    super::daemon::cmd_daemon_status(&h.cli(), h.printer()).unwrap();
    let output = h.output();
    assert!(
        output.contains("Daemon Status"),
        "expected 'Daemon Status' header, got: {output}"
    );
    assert!(
        output.contains("not running"),
        "expected 'not running' message, got: {output}"
    );
    assert!(
        output.contains("cfgd daemon"),
        "expected start hint, got: {output}"
    );
    assert!(
        output.contains("cfgd daemon install"),
        "expected install hint, got: {output}"
    );
}

#[test]
fn daemon_status_no_daemon_running_json_output() {
    let h = CliTestHarness::builder().json().build();
    super::daemon::cmd_daemon_status(&h.cli(), h.printer()).unwrap();
    let parsed = h.json_output();
    assert_json_has_fields(
        &parsed,
        &["running", "pid", "uptimeSecs", "driftCount", "sources"],
    );
    assert_json_field_type(&parsed, "running", "bool");
    assert!(
        !parsed.get("running").unwrap().as_bool().unwrap(),
        "running should be false when no daemon is present"
    );
    assert_eq!(
        parsed.get("pid").unwrap().as_u64().unwrap(),
        0,
        "pid should be 0 when no daemon is present"
    );
}

// -----------------------------------------------------------------------
// render_daemon_status — direct, no IPC bind required
// -----------------------------------------------------------------------

fn sample_daemon_status(
    pid: u32,
    uptime_secs: u64,
    drift_count: u32,
    sources: Vec<cfgd_core::daemon::SourceStatus>,
    update_available: Option<String>,
) -> cfgd_core::daemon::DaemonStatusResponse {
    cfgd_core::daemon::DaemonStatusResponse {
        running: true,
        pid,
        uptime_secs,
        last_reconcile: Some("2026-05-12T10:00:00Z".to_string()),
        last_sync: Some("2026-05-12T09:55:00Z".to_string()),
        drift_count,
        sources,
        update_available,
        module_reconcile: vec![],
    }
}

fn sample_source(
    name: &str,
    status: &str,
    drift: u32,
    last_sync: Option<&str>,
) -> cfgd_core::daemon::SourceStatus {
    cfgd_core::daemon::SourceStatus {
        name: name.to_string(),
        status: status.to_string(),
        drift_count: drift,
        last_sync: last_sync.map(|s| s.to_string()),
        last_reconcile: None,
    }
}

#[test]
fn render_daemon_status_human_running_with_sources_and_update() {
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();
    let status = sample_daemon_status(
        4242,
        3600,
        7,
        vec![
            sample_source("local", "active", 0, None),
            sample_source("team", "syncing", 7, Some("2026-05-12T09:00:00Z")),
        ],
        Some("9.9.9".to_string()),
    );
    printer.emit(super::daemon::build_daemon_status_doc(Some(&status)));
    drop(printer);
    let output = cap.human();
    assert!(output.contains("Daemon is running"), "got: {output}");
    assert!(output.contains("4242"), "PID missing: {output}");
    assert!(output.contains("3600s"), "uptime missing: {output}");
    assert!(
        output.contains("Last reconcile"),
        "last_reconcile row missing: {output}"
    );
    assert!(
        output.contains("Last sync"),
        "last_sync row missing: {output}"
    );
    assert!(
        output.contains("Update available: 9.9.9"),
        "update-available banner missing: {output}"
    );
    assert!(
        output.contains("Sources"),
        "sources subheader missing: {output}"
    );
    assert!(output.contains("team"), "source name missing: {output}");
}

#[test]
fn render_daemon_status_human_running_without_last_timestamps_skips_rows() {
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();
    let status = cfgd_core::daemon::DaemonStatusResponse {
        running: true,
        pid: 1,
        uptime_secs: 1,
        last_reconcile: None,
        last_sync: None,
        drift_count: 0,
        sources: vec![],
        update_available: None,
        module_reconcile: vec![],
    };
    printer.emit(super::daemon::build_daemon_status_doc(Some(&status)));
    drop(printer);
    let output = cap.human();
    assert!(output.contains("Daemon is running"));
    // When last_reconcile / last_sync are None the rows are not printed
    assert!(
        !output.contains("Last reconcile"),
        "Last reconcile row should be skipped: {output}"
    );
    assert!(
        !output.contains("Last sync"),
        "Last sync row should be skipped: {output}"
    );
    assert!(
        !output.contains("Update available"),
        "Update available row should be skipped: {output}"
    );
}

#[test]
fn render_daemon_status_json_emits_some_status_shape() {
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();
    let status = sample_daemon_status(99, 60, 1, vec![sample_source("s1", "ok", 0, None)], None);
    printer.emit(super::daemon::build_daemon_status_doc(Some(&status)));
    drop(printer);
    let parsed = cap.json().expect("doc captured json");
    assert_eq!(parsed.get("pid").unwrap().as_u64().unwrap(), 99);
    assert_eq!(parsed.get("uptimeSecs").unwrap().as_u64().unwrap(), 60);
    assert_eq!(parsed.get("driftCount").unwrap().as_u64().unwrap(), 1);
    assert!(parsed.get("running").unwrap().as_bool().unwrap());
}

#[test]
fn render_daemon_status_json_emits_placeholder_when_none() {
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();
    printer.emit(super::daemon::build_daemon_status_doc(None));
    drop(printer);
    let parsed = cap.json().expect("doc captured json");
    assert_eq!(parsed.get("pid").unwrap().as_u64().unwrap(), 0);
    assert!(!parsed.get("running").unwrap().as_bool().unwrap());
    assert_eq!(parsed.get("uptimeSecs").unwrap().as_u64().unwrap(), 0);
}

// -----------------------------------------------------------------------
// cmd_daemon_uninstall — output and completion
// -----------------------------------------------------------------------

#[test]
fn daemon_uninstall_prints_platform_info_and_succeeds() {
    // Isolate HOME so the best-effort service stop is skipped (the thread-local
    // override short-circuits the real systemctl/launchctl shell-out) and the
    // file-removal path runs against the temp dir, never the runner's session.
    let tmp_home = tempfile::tempdir().unwrap();
    let _home = cfgd_core::with_test_home_guard(tmp_home.path());
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();
    // On Linux (CI/test env), uninstall_service just removes the unit file
    // if present; in a clean test env there is nothing to remove, so it succeeds.
    let cli = Cli::try_parse_from(["cfgd"]).unwrap();
    let result = super::daemon::cmd_daemon_uninstall(&cli, &printer);
    drop(printer);
    let output = cap.human();

    assert!(
        output.contains("Uninstall Daemon Service"),
        "expected header, got: {output}"
    );

    // On Linux, the function prints the systemctl stop message
    #[cfg(target_os = "linux")]
    assert!(
        output.contains("systemctl --user disable --now cfgd.service"),
        "expected systemctl uninstall message on Linux, got: {output}"
    );

    // The call should succeed (no unit file to remove in test env)
    assert!(result.is_ok(), "expected success, got: {:?}", result.err());

    assert!(
        output.contains("Daemon service removed"),
        "expected completion message, got: {output}"
    );
}

// -----------------------------------------------------------------------
// cmd_checkin — error when config has no profile
// -----------------------------------------------------------------------

#[test]
fn checkin_fails_when_no_profile_configured() {
    let no_profile_config =
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec: {}\n";
    let h = CliTestHarness::builder().config(no_profile_config).build();
    let printer = test_printer();
    let result =
        super::checkin::cmd_checkin(&h.cli(), &printer, "http://localhost:8080", None, None);
    assert_error_contains(&result, "no profile configured");
}

#[test]
fn checkin_fails_when_config_file_missing() {
    let dir = tempfile::tempdir().unwrap();
    // Don't write any config file
    let cli = test_cli(dir.path());
    let printer = test_printer();
    let result = super::checkin::cmd_checkin(&cli, &printer, "http://localhost:8080", None, None);
    assert_error_contains(&result, "config file not found");
}

#[test]
fn checkin_fails_when_profile_does_not_exist() {
    let bad_profile_config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: nonexistent\n";
    let h = CliTestHarness::builder().config(bad_profile_config).build();
    let printer = test_printer();
    let result =
        super::checkin::cmd_checkin(&h.cli(), &printer, "http://localhost:8080", None, None);
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("profile not found: nonexistent"),
        "expected 'profile not found' error, got: {msg}"
    );
}

// -----------------------------------------------------------------------
// add_source_to_config tests
// -----------------------------------------------------------------------

#[test]
fn add_source_to_config_appends_to_existing_sources() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
  sources:
    - name: existing
      origin:
        type: Git
        url: https://example.com/existing
        branch: main
"#,
    )
    .unwrap();

    let source = config::SourceSpec {
        name: "new-source".into(),
        origin: config::OriginSpec {
            origin_type: config::OriginType::Git,
            url: "https://example.com/new".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: config::SshHostKeyPolicy::AcceptNew,
        },
        subscription: config::SubscriptionSpec::default(),
        sync: config::SourceSyncSpec::default(),
    };

    let result = super::add_source_to_config(&config_path, &source);
    assert!(
        result.is_ok(),
        "add_source_to_config failed: {:?}",
        result.err()
    );

    let written = std::fs::read_to_string(&config_path).unwrap();
    let parsed: serde_yaml::Value = serde_yaml::from_str(&written).unwrap();
    let sources = parsed["spec"]["sources"].as_sequence().unwrap();
    assert_eq!(
        sources.len(),
        2,
        "expected 2 sources after append, got {}",
        sources.len()
    );
    assert_eq!(sources[0]["name"].as_str().unwrap(), "existing");
    assert_eq!(sources[1]["name"].as_str().unwrap(), "new-source");
    assert_eq!(
        sources[1]["origin"]["url"].as_str().unwrap(),
        "https://example.com/new"
    );
}

#[test]
fn add_source_to_config_errors_on_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("nonexistent.yaml");
    let source = config::SourceSpec {
        name: "test".into(),
        origin: config::OriginSpec {
            origin_type: config::OriginType::Git,
            url: "https://example.com".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: config::SshHostKeyPolicy::AcceptNew,
        },
        subscription: config::SubscriptionSpec::default(),
        sync: config::SourceSyncSpec::default(),
    };

    let result = super::add_source_to_config(&config_path, &source);
    assert!(result.is_err(), "expected error for missing file");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("Config file not found"),
        "expected 'Config file not found', got: {msg}"
    );
}

#[test]
fn add_source_to_config_creates_sources_array_when_absent() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: test
spec:
  profile: default
"#,
    )
    .unwrap();

    let source = config::SourceSpec {
        name: "first-source".into(),
        origin: config::OriginSpec {
            origin_type: config::OriginType::Git,
            url: "https://example.com/repo".into(),
            branch: "master".into(),
            auth: None,
            ssh_strict_host_key_checking: config::SshHostKeyPolicy::AcceptNew,
        },
        subscription: config::SubscriptionSpec::default(),
        sync: config::SourceSyncSpec::default(),
    };

    let result = super::add_source_to_config(&config_path, &source);
    assert!(
        result.is_ok(),
        "add_source_to_config failed: {:?}",
        result.err()
    );

    let written = std::fs::read_to_string(&config_path).unwrap();
    let parsed: serde_yaml::Value = serde_yaml::from_str(&written).unwrap();
    let sources = parsed["spec"]["sources"].as_sequence().unwrap();
    assert_eq!(sources.len(), 1, "expected 1 source, got {}", sources.len());
    assert_eq!(sources[0]["name"].as_str().unwrap(), "first-source");
    assert_eq!(
        sources[0]["origin"]["url"].as_str().unwrap(),
        "https://example.com/repo"
    );
}

#[test]
fn add_source_to_config_errors_when_spec_missing() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("cfgd.yaml");
    std::fs::write(
        &config_path,
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\n",
    )
    .unwrap();

    let source = config::SourceSpec {
        name: "src".into(),
        origin: config::OriginSpec {
            origin_type: config::OriginType::Git,
            url: "https://example.com".into(),
            branch: "main".into(),
            auth: None,
            ssh_strict_host_key_checking: config::SshHostKeyPolicy::AcceptNew,
        },
        subscription: config::SubscriptionSpec::default(),
        sync: config::SourceSyncSpec::default(),
    };

    let result = super::add_source_to_config(&config_path, &source);
    assert!(result.is_err(), "expected error when spec is missing");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("config missing 'spec'"),
        "expected 'config missing spec', got: {msg}"
    );
}

// -----------------------------------------------------------------------
// WorkstationDaemonHooks tests
// -----------------------------------------------------------------------

#[test]
fn workstation_daemon_hooks_build_registry_returns_populated_registry() {
    use cfgd_core::daemon::DaemonHooks;
    let hooks = super::WorkstationDaemonHooks;
    let cfg = cfgd_core::config::CfgdConfig {
        api_version: cfgd_core::API_VERSION.into(),
        kind: "Config".into(),
        metadata: cfgd_core::config::ConfigMetadata {
            name: "test".into(),
        },
        spec: cfgd_core::config::ConfigSpec::default(),
    };
    let registry = hooks.build_registry(&cfg);
    assert!(
        !registry.package_managers.is_empty(),
        "build_registry should return a registry with at least one package manager"
    );
    assert!(
        !registry.system_configurators.is_empty(),
        "build_registry should return a registry with at least one system configurator"
    );
}

#[test]
fn workstation_daemon_hooks_expand_tilde() {
    use cfgd_core::daemon::DaemonHooks;
    let hooks = super::WorkstationDaemonHooks;
    let expanded = hooks.expand_tilde(std::path::Path::new("~/test/file"));
    // Should not start with ~ after expansion
    assert!(
        !expanded.to_string_lossy().starts_with('~'),
        "expand_tilde should expand ~ to home directory, got: {}",
        expanded.display()
    );
    // Should end with the path suffix
    assert!(
        expanded.to_string_lossy().ends_with("test/file"),
        "expand_tilde should preserve path suffix, got: {}",
        expanded.display()
    );
}

#[test]
fn workstation_daemon_hooks_expand_tilde_absolute_unchanged() {
    use cfgd_core::daemon::DaemonHooks;
    let hooks = super::WorkstationDaemonHooks;
    let abs = std::path::Path::new("/absolute/path");
    let expanded = hooks.expand_tilde(abs);
    assert_eq!(
        expanded, abs,
        "expand_tilde should not modify absolute paths"
    );
}

#[test]
fn workstation_daemon_hooks_plan_files_empty_profile() {
    use cfgd_core::daemon::DaemonHooks;
    let hooks = super::WorkstationDaemonHooks;
    let dir = tempfile::tempdir().unwrap();
    // Write a minimal config so load_config succeeds
    std::fs::write(dir.path().join("cfgd.yaml"), TEST_CONFIG_YAML).unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("default.yaml"), DEFAULT_PROFILE_YAML).unwrap();

    let resolved = cfgd_core::config::ResolvedProfile {
        layers: vec![],
        merged: cfgd_core::config::MergedProfile::default(),
    };
    let result = hooks.plan_files(dir.path(), &resolved);
    assert!(
        result.is_ok(),
        "plan_files with empty profile should succeed: {:?}",
        result.err()
    );
    let actions = result.unwrap();
    assert!(
        actions.is_empty(),
        "plan_files with empty profile should produce no actions, got {}",
        actions.len()
    );
}

// -----------------------------------------------------------------------
// is_unmanaged_file — module-cache symlink branch
// -----------------------------------------------------------------------

#[test]
#[cfg(unix)]
#[serial_test::serial]
fn is_unmanaged_file_module_cache_symlink_under_test_home() {
    // is_unmanaged_file second early-return: a symlink pointing into
    // ~/.cache/cfgd/modules/ is module-managed (NOT unmanaged) even though it
    // does not start with config_dir. Honors the test-home thread-local via
    // expand_tilde, so we can build the cache-dir path under a tempdir.
    let dir = tempfile::tempdir().unwrap();
    let _guard = cfgd_core::with_test_home_guard(dir.path());
    let state = StateStore::open_in_memory().unwrap();

    // Build a real source under the redirected ~/.cache/cfgd/modules/<mod>/
    let module_root = dir.path().join(".cache/cfgd/modules/example-mod");
    std::fs::create_dir_all(&module_root).unwrap();
    let module_payload = module_root.join("rc-fragment");
    std::fs::write(&module_payload, "# from module\n").unwrap();

    // Symlink lives outside config_dir so the first early-return (link_target
    // starts with config_dir) does NOT fire; the module-cache check must.
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let unrelated_dir = dir.path().join("home");
    std::fs::create_dir_all(&unrelated_dir).unwrap();
    let target = unrelated_dir.join(".module-link");
    std::os::unix::fs::symlink(&module_payload, &target).unwrap();

    assert!(
        !is_unmanaged_file(&target, &config_dir, &state),
        "symlink into ~/.cache/cfgd/modules must be treated as cfgd-managed",
    );
}

// -----------------------------------------------------------------------
// validate_resource_name — all validation branches
// -----------------------------------------------------------------------

#[test]
fn validate_resource_name_valid_names() {
    assert!(super::validate_resource_name("my-module", "Module").is_ok());
    assert!(super::validate_resource_name("mod_v2", "Module").is_ok());
    assert!(super::validate_resource_name("a.b.c", "Module").is_ok());
    assert!(super::validate_resource_name("X", "Module").is_ok());
    assert!(super::validate_resource_name("a123", "Module").is_ok());
}

#[test]
fn validate_resource_name_empty_fails() {
    let result = super::validate_resource_name("", "Module");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("cannot be empty"),
        "should mention empty name"
    );
}

#[test]
fn validate_resource_name_too_long_fails() {
    let name = "a".repeat(129);
    let result = super::validate_resource_name(&name, "Module");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("too long"),
        "should mention too long"
    );
}

#[test]
fn validate_resource_name_leading_dot_fails() {
    let result = super::validate_resource_name(".hidden", "Profile");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("cannot start with"),
        "should mention leading character restriction"
    );
}

#[test]
fn validate_resource_name_leading_hyphen_fails() {
    let result = super::validate_resource_name("-bad", "Profile");
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("cannot start with"),
        "should mention leading character restriction"
    );
}

#[test]
fn validate_resource_name_invalid_chars_fails() {
    let result = super::validate_resource_name("my module!", "Module");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("invalid characters"),
        "should mention invalid characters, got: {msg}"
    );
}

// -----------------------------------------------------------------------
// scan_profile_names and scan_module_names
// -----------------------------------------------------------------------

#[test]
fn scan_profile_names_finds_all_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();

    std::fs::write(profiles_dir.join("default.yaml"), DEFAULT_PROFILE_YAML).unwrap();
    std::fs::write(profiles_dir.join("work.yaml"), WORK_PROFILE_YAML).unwrap();
    // Non-yaml file should be ignored
    std::fs::write(profiles_dir.join("readme.txt"), "not a profile").unwrap();

    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let names = super::scan_profile_names(&profiles_dir, &printer).unwrap();
    assert!(
        names.contains(&"default".to_string()),
        "should find default profile, got: {:?}",
        names
    );
    assert!(
        names.contains(&"work".to_string()),
        "should find work profile, got: {:?}",
        names
    );
    assert!(
        !names.contains(&"readme".to_string()),
        "should not include non-yaml files"
    );
}

#[test]
fn scan_profile_names_nonexistent_dir_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let profiles_dir = dir.path().join("no-such-dir");
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let names = super::scan_profile_names(&profiles_dir, &printer).unwrap();
    assert!(
        names.is_empty(),
        "nonexistent profiles dir should return empty list"
    );
}

#[test]
fn scan_module_names_finds_modules() {
    let dir = tempfile::tempdir().unwrap();
    let modules_dir = dir.path().join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();

    // Create two valid modules
    create_module_in_dir(dir.path(), "alpha-mod", SIMPLE_MODULE_YAML);
    create_module_in_dir(dir.path(), "beta-mod", SIMPLE_MODULE_YAML);

    // Create a dir without module.yaml (should be ignored)
    std::fs::create_dir_all(modules_dir.join("not-a-module")).unwrap();

    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    let names = super::scan_module_names(&modules_dir, &printer).unwrap();
    assert_eq!(
        names.len(),
        2,
        "should find exactly 2 modules, got: {:?}",
        names
    );
    assert_eq!(names[0], "alpha-mod", "should be sorted alphabetically");
    assert_eq!(names[1], "beta-mod", "should be sorted alphabetically");
}

#[test]
fn scan_module_names_nonexistent_dir_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let modules_dir = dir.path().join("no-modules");
    let (printer, _buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let names = super::scan_module_names(&modules_dir, &printer).unwrap();
    assert!(
        names.is_empty(),
        "nonexistent modules dir should return empty list"
    );
}

// -----------------------------------------------------------------------
// config_cmd::walk_yaml_path — exercises all branches including error paths
// -----------------------------------------------------------------------

#[test]
fn walk_yaml_path_root_dot_returns_whole_value() {
    let val: serde_yaml::Value = serde_yaml::from_str("foo: bar\nbaz: 42").unwrap();
    let result = super::config_cmd::walk_yaml_path(&val, ".").unwrap();
    assert!(result.is_mapping(), "root should be a mapping");
}

#[test]
fn walk_yaml_path_nested_key() {
    let val: serde_yaml::Value = serde_yaml::from_str("a:\n  b:\n    c: hello").unwrap();
    let result = super::config_cmd::walk_yaml_path(&val, "a.b.c").unwrap();
    assert_eq!(
        result.as_str().unwrap(),
        "hello",
        "should reach nested value"
    );
}

#[test]
fn walk_yaml_path_missing_key_errors() {
    let val: serde_yaml::Value = serde_yaml::from_str("a:\n  b: 1").unwrap();
    let result = super::config_cmd::walk_yaml_path(&val, "a.z");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not found"),
        "should report key not found, got: {msg}"
    );
}

#[test]
fn walk_yaml_path_empty_segment_errors() {
    let val: serde_yaml::Value = serde_yaml::from_str("a: 1").unwrap();
    let result = super::config_cmd::walk_yaml_path(&val, "a..b");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("empty segment"),
        "should report empty segment"
    );
}

#[test]
fn walk_yaml_path_traverse_into_scalar_errors() {
    let val: serde_yaml::Value = serde_yaml::from_str("a: 1").unwrap();
    let result = super::config_cmd::walk_yaml_path(&val, "a.b");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("not a mapping"),
        "should report not a mapping, got: {msg}"
    );
}

// -----------------------------------------------------------------------
// config_cmd::walk_yaml_path_mut — creates intermediate mappings
// -----------------------------------------------------------------------

#[test]
fn walk_yaml_path_mut_creates_intermediate_maps() {
    let mut val: serde_yaml::Value = serde_yaml::from_str("root: {}").unwrap();
    let (parent, leaf) =
        super::config_cmd::walk_yaml_path_mut(&mut val, "root.new.nested.key").unwrap();
    assert_eq!(leaf, "key");
    // Insert a value to verify parent is the right map
    parent.insert(
        serde_yaml::Value::String(leaf),
        serde_yaml::Value::String("value".into()),
    );
    let result = super::config_cmd::walk_yaml_path(&val, "root.new.nested.key").unwrap();
    assert_eq!(result.as_str().unwrap(), "value");
}

#[test]
fn walk_yaml_path_mut_empty_path_errors() {
    let mut val: serde_yaml::Value = serde_yaml::from_str("a: 1").unwrap();
    let result = super::config_cmd::walk_yaml_path_mut(&mut val, "");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("empty segment"),
        "should reject empty path"
    );
}

// -----------------------------------------------------------------------
// config_cmd::parse_yaml_value — type inference branches
// -----------------------------------------------------------------------

#[test]
fn parse_yaml_value_all_types() {
    assert_eq!(
        super::config_cmd::parse_yaml_value("true"),
        serde_yaml::Value::Bool(true)
    );
    assert_eq!(
        super::config_cmd::parse_yaml_value("false"),
        serde_yaml::Value::Bool(false)
    );
    assert_eq!(
        super::config_cmd::parse_yaml_value("null"),
        serde_yaml::Value::Null
    );
    assert_eq!(
        super::config_cmd::parse_yaml_value("~"),
        serde_yaml::Value::Null
    );
    assert_eq!(
        super::config_cmd::parse_yaml_value("42"),
        serde_yaml::Value::Number(42i64.into())
    );
    assert_eq!(
        super::config_cmd::parse_yaml_value("hello"),
        serde_yaml::Value::String("hello".into())
    );
    // Float
    let float_val = super::config_cmd::parse_yaml_value("3.14");
    assert!(float_val.is_number(), "3.14 should parse as number");
}

// -----------------------------------------------------------------------
// filter_plan — skip and only filters on package and non-package actions
// -----------------------------------------------------------------------

#[test]
fn filter_plan_skip_removes_matching_packages() {
    use cfgd_core::reconciler::{Action, Phase, Plan};

    let mut plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Packages,
            actions: vec![
                Action::Package(PackageAction::Install {
                    manager: "brew".into(),
                    packages: vec!["ripgrep".into(), "fd".into(), "bat".into()],
                    origin: "profile".into(),
                }),
                Action::Package(PackageAction::Install {
                    manager: "cargo".into(),
                    packages: vec!["tokei".into()],
                    origin: "profile".into(),
                }),
            ],
        }],
        warnings: vec![],
    };

    super::filter_plan(&mut plan, &["packages.brew.fd".to_string()], &[]);

    // brew install should remain but without fd
    let brew_action = &plan.phases[0].actions[0];
    match brew_action {
        Action::Package(PackageAction::Install { packages, .. }) => {
            assert!(
                !packages.contains(&"fd".to_string()),
                "fd should be filtered out"
            );
            assert!(
                packages.contains(&"ripgrep".to_string()),
                "ripgrep should remain"
            );
            assert!(packages.contains(&"bat".to_string()), "bat should remain");
        }
        other => panic!("expected Install action, got: {:?}", other),
    }
    // cargo should be untouched
    assert_eq!(
        plan.phases[0].actions.len(),
        2,
        "cargo action should remain"
    );
}

#[test]
fn filter_plan_only_keeps_matching_phase() {
    use cfgd_core::reconciler::{Action, Phase, Plan};

    let mut plan = Plan {
        phases: vec![
            Phase {
                name: PhaseName::Packages,
                actions: vec![Action::Package(PackageAction::Install {
                    manager: "brew".into(),
                    packages: vec!["git".into()],
                    origin: "profile".into(),
                })],
            },
            Phase {
                name: PhaseName::Files,
                actions: vec![Action::File(FileAction::Create {
                    source: PathBuf::from("/src"),
                    target: PathBuf::from("/dst"),
                    origin: "profile".into(),
                    strategy: config::FileStrategy::Copy,
                    source_hash: None,
                })],
            },
        ],
        warnings: vec![],
    };

    super::filter_plan(&mut plan, &[], &["packages".to_string()]);

    // Packages phase should keep its action
    assert_eq!(
        plan.phases[0].actions.len(),
        1,
        "packages phase should retain action"
    );
    // Files phase should have its action removed by only filter
    assert_eq!(
        plan.phases[1].actions.len(),
        0,
        "files phase should be empty after only=packages"
    );
}

#[test]
fn filter_plan_skip_uninstall_packages_env() {
    use cfgd_core::reconciler::{Action, Phase, Plan};

    let mut plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Packages,
            actions: vec![Action::Package(PackageAction::Uninstall {
                manager: "npm".into(),
                packages: vec!["left-pad".into(), "is-odd".into()],
                origin: "profile".into(),
            })],
        }],
        warnings: vec![],
    };

    super::filter_plan(&mut plan, &["packages.npm.left-pad".to_string()], &[]);

    match &plan.phases[0].actions[0] {
        Action::Package(PackageAction::Uninstall { packages, .. }) => {
            assert_eq!(packages, &vec!["is-odd".to_string()]);
        }
        other => panic!("expected Uninstall, got: {:?}", other),
    }
}

#[test]
fn filter_plan_empty_filters_is_noop() {
    use cfgd_core::reconciler::{Action, Phase, Plan};

    let mut plan = Plan {
        phases: vec![Phase {
            name: PhaseName::Packages,
            actions: vec![Action::Package(PackageAction::Install {
                manager: "apt".into(),
                packages: vec!["vim".into()],
                origin: "profile".into(),
            })],
        }],
        warnings: vec![],
    };

    super::filter_plan(&mut plan, &[], &[]);

    assert_eq!(
        plan.phases[0].actions.len(),
        1,
        "empty filters should not change anything"
    );
}

// -----------------------------------------------------------------------
// strip_scripts_from_plan — removes script phases and module script actions
// -----------------------------------------------------------------------

#[test]
fn strip_scripts_from_plan_removes_script_phases() {
    use cfgd_core::reconciler::{Action, Phase, Plan};

    let mut plan = Plan {
        phases: vec![
            Phase {
                name: PhaseName::PreScripts,
                actions: vec![],
            },
            Phase {
                name: PhaseName::Packages,
                actions: vec![Action::Package(PackageAction::Install {
                    manager: "brew".into(),
                    packages: vec!["git".into()],
                    origin: "profile".into(),
                })],
            },
            Phase {
                name: PhaseName::PostScripts,
                actions: vec![],
            },
        ],
        warnings: vec![],
    };

    super::strip_scripts_from_plan(&mut plan);

    assert_eq!(
        plan.phases.len(),
        1,
        "should only have packages phase remaining"
    );
    assert_eq!(
        plan.phases[0].name,
        PhaseName::Packages,
        "remaining phase should be Packages"
    );
}

// -----------------------------------------------------------------------
// action_path — all action type variants
// -----------------------------------------------------------------------

#[test]
fn action_path_package_install() {
    let action = reconciler::Action::Package(PackageAction::Install {
        manager: "brew".into(),
        packages: vec!["git".into()],
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::Packages, &action);
    assert_eq!(path, "packages.brew");
}

#[test]
fn action_path_file_create() {
    let action = reconciler::Action::File(FileAction::Create {
        source: PathBuf::from("/src/bashrc"),
        target: PathBuf::from("/home/user/.bashrc"),
        origin: "profile".into(),
        strategy: config::FileStrategy::Copy,
        source_hash: None,
    });
    let path = super::action_path(&PhaseName::Files, &action);
    assert_eq!(path, "files:/home/user/.bashrc");
}

#[test]
fn action_path_module() {
    let action = reconciler::Action::Module(reconciler::ModuleAction {
        module_name: "dev-tools".into(),
        kind: reconciler::ModuleActionKind::InstallPackages { resolved: vec![] },
        origin: None,
    });
    let path = super::action_path(&PhaseName::Modules, &action);
    assert_eq!(path, "modules.dev-tools");
}

#[test]
fn action_path_env_write() {
    let action = reconciler::Action::Env(reconciler::EnvAction::WriteEnvFile {
        path: PathBuf::from("/home/user/.config/cfgd/env.sh"),
        content: String::new(),
    });
    let path = super::action_path(&PhaseName::Env, &action);
    assert_eq!(path, "env:/home/user/.config/cfgd/env.sh");
}

// -----------------------------------------------------------------------
// cmd_plan with module containing packages, env, and files
// Exercises: module loading, plan generation, display_plan_preview
// -----------------------------------------------------------------------

#[test]
fn cmd_plan_rich_module_with_packages_env_and_files() {
    let rich_module = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: dev-tools
spec:
  packages:
    - name: ripgrep
      prefer: [cargo]
    - name: fd-find
      prefer: [cargo]
    - name: bat
  env:
    - name: EDITOR
      value: nvim
    - name: PAGER
      value: less
  files:
    - source: gitconfig
      target: /tmp/cfgd-test-gitconfig
"#;
    let profile_with_module = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  env:
    - name: SHELL
      value: /bin/zsh
  packages:
    cargo:
      - tokei
  modules:
    - dev-tools
"#;

    let h = CliTestHarness::builder()
        .profile("default", profile_with_module)
        .module("dev-tools", rich_module)
        .build();

    // Create the module file referenced by the module
    let module_files_dir = h
        .config_path()
        .join("modules")
        .join("dev-tools")
        .join("files");
    std::fs::write(
        module_files_dir.join("gitconfig"),
        "[user]\n  name = Test\n",
    )
    .unwrap();

    let cli = h.cli_with_command(Command::Plan(PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    }));
    let result = super::plan::cmd_plan(
        &cli,
        h.printer(),
        &PlanArgs {
            from: None,
            phase: None,
            skip: vec![],
            only: vec![],
            module: None,
            skip_scripts: false,
            context: "apply".to_string(),
        },
    );
    assert!(
        result.is_ok(),
        "cmd_plan with rich module should succeed: {:?}",
        result.err()
    );

    let output = h.output();
    assert!(
        output.contains("Plan"),
        "should show Plan header, got: {output}"
    );
    // Should mention actions or nothing-to-do
    assert!(
        output.contains("action(s) planned")
            || output.contains("Nothing to do")
            || output.contains("Phase"),
        "should show plan summary, got: {output}"
    );
}

// -----------------------------------------------------------------------
// cmd_plan with --module filter (module-only mode)
// Exercises: module-only path, empty_resolved_profile, module resolution
// -----------------------------------------------------------------------

#[test]
fn cmd_plan_module_only_mode() {
    let module_yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: standalone
spec:
  packages:
    - name: jq
    - name: yq
"#;
    let h = CliTestHarness::builder()
        .module("standalone", module_yaml)
        .build();

    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: Some("standalone".to_string()),
        skip_scripts: false,
        context: "apply".to_string(),
    };
    let result = super::plan::cmd_plan(&h.cli(), h.printer(), &args);
    assert!(
        result.is_ok(),
        "module-only plan should succeed: {:?}",
        result.err()
    );

    let output = h.output();
    assert!(
        output.contains("Plan"),
        "should show Plan header, got: {output}"
    );
}

// -----------------------------------------------------------------------
// cmd_plan JSON structured output with module
// Exercises: build_plan_output, structured serialization
// -----------------------------------------------------------------------

#[test]
// Reads PATH through command_path for manager detection; must hold the same
// global lock as the #[serial] PATH mutators (cli/paths.rs, cli/kubectl.rs,
// cli/init/tests.rs) or plain `cargo test` (shared-process) races them.
#[serial_test::serial]
fn cmd_plan_json_output_with_module() {
    let module_yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: json-mod
spec:
  packages:
    - name: curl
    - name: wget
"#;
    let profile_yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  modules:
    - json-mod
  packages:
    cargo:
      - bat
"#;
    let h = CliTestHarness::builder()
        .json()
        .profile("default", profile_yaml)
        .module("json-mod", module_yaml)
        .build();

    let args = PlanArgs {
        from: None,
        phase: None,
        skip: vec![],
        only: vec![],
        module: None,
        skip_scripts: false,
        context: "apply".to_string(),
    };
    let result = super::plan::cmd_plan(&h.cli(), h.printer(), &args);
    assert!(
        result.is_ok(),
        "JSON plan should succeed: {:?}",
        result.err()
    );

    let json = h.json_output();
    assert_json_has_fields(&json, &["context", "totalActions", "phases"]);
    assert_json_field_type(&json, "totalActions", "number");
    assert_json_field_type(&json, "phases", "array");
}

// -----------------------------------------------------------------------
// cmd_status with module configured — exercises module status display
// -----------------------------------------------------------------------

#[test]
fn cmd_status_with_module_displays_module_info() {
    let module_yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: status-mod
spec:
  packages:
    - name: git
    - name: curl
  files:
    - source: config.txt
      target: /tmp/cfgd-test-config.txt
  depends:
    - base-mod
"#;
    let profile_yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  modules:
    - status-mod
"#;
    let h = CliTestHarness::builder()
        .profile("default", profile_yaml)
        .module("status-mod", module_yaml)
        .build();

    // Also create the base-mod so dependency display works
    create_module_in_dir(
        h.config_path(),
        "base-mod",
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: base-mod\nspec:\n  packages: []\n",
    );

    let result = super::status::cmd_status_module(&h.cli(), h.printer(), "status-mod");
    assert!(
        result.is_ok(),
        "cmd_status_module should succeed: {:?}",
        result.err()
    );

    let output = h.output();
    assert!(
        output.contains("Status: status-mod"),
        "should show module name in header, got: {output}"
    );
    assert!(
        output.contains("Packages") && output.contains("2"),
        "should show package count, got: {output}"
    );
    assert!(
        output.contains("Files") && output.contains("1"),
        "should show file count, got: {output}"
    );
    assert!(
        output.contains("Dependencies") && output.contains("base-mod"),
        "should show dependencies, got: {output}"
    );
    assert!(
        output.contains("not applied"),
        "should show 'not applied' status, got: {output}"
    );
}

#[test]
fn cmd_status_module_json_output_found() {
    let module_yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: json-status-mod
spec:
  packages:
    - name: vim
  files:
    - source: vimrc
      target: /tmp/cfgd-test-vimrc
  depends:
    - core
"#;
    let h = CliTestHarness::builder()
        .json()
        .module("json-status-mod", module_yaml)
        .build();

    let result = super::status::cmd_status_module(&h.cli(), h.printer(), "json-status-mod");
    assert!(
        result.is_ok(),
        "JSON module status should succeed: {:?}",
        result.err()
    );

    let json = h.json_output();
    assert_json_has_fields(&json, &["name", "packages", "files", "depends", "status"]);
    assert_eq!(json["name"].as_str().unwrap(), "json-status-mod");
    assert_eq!(json["packages"].as_u64().unwrap(), 1);
    assert_eq!(json["files"].as_u64().unwrap(), 1);
    assert_eq!(json["status"].as_str().unwrap(), "not applied");
    let depends = json["depends"].as_array().unwrap();
    assert_eq!(depends.len(), 1);
    assert_eq!(depends[0].as_str().unwrap(), "core");
}

#[test]
fn cmd_status_module_json_output_not_found() {
    let h = CliTestHarness::builder().json().build();

    let result = super::status::cmd_status_module(&h.cli(), h.printer(), "nonexistent-mod");
    assert!(result.is_ok(), "missing module JSON status should succeed");

    let json = h.json_output();
    assert_eq!(json["name"].as_str().unwrap(), "nonexistent-mod");
    assert_eq!(json["status"].as_str().unwrap(), "not found");
    assert_eq!(json["packages"].as_u64().unwrap(), 0);
}

// -----------------------------------------------------------------------
// config_cmd::cmd_config_show — exercises all branches (origins, sources, daemon, etc.)
// -----------------------------------------------------------------------

#[test]
fn cmd_config_show_with_rich_config_full() {
    let h = CliTestHarness::builder().rich_config().build();

    let result = super::config_cmd::cmd_config_show(&h.cli(), h.printer());
    assert!(
        result.is_ok(),
        "config show should succeed: {:?}",
        result.err()
    );

    let output = h.output();
    assert!(
        output.contains("Configuration"),
        "should show Configuration header, got: {output}"
    );
    assert!(
        output.contains("Profile") && output.contains("default"),
        "should show profile name, got: {output}"
    );
    // Should show sources section
    assert!(
        output.contains("Sources") && output.contains("team-config"),
        "should show source names, got: {output}"
    );
    // Should show daemon section
    assert!(
        output.contains("Daemon") && output.contains("yes"),
        "should show daemon enabled, got: {output}"
    );
    assert!(
        output.contains("Reconcile") && output.contains("Interval") && output.contains("5m"),
        "should show reconcile interval, got: {output}"
    );
    // Should show secrets
    assert!(
        output.contains("Secrets") && output.contains("age"),
        "should show secrets backend, got: {output}"
    );
}

#[test]
fn cmd_config_show_missing_file_errors() {
    let dir = tempfile::tempdir().unwrap();
    let cli = test_cli_with_state(dir.path(), None);
    let result = super::config_cmd::cmd_config_show(&cli, &test_printer());
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("config file not found"),
        "should report missing config"
    );
}

// -----------------------------------------------------------------------
// config_cmd::cmd_config_get — exercises config_cmd::walk_yaml_path through config
// -----------------------------------------------------------------------

#[test]
fn cmd_config_get_string_value() {
    let h = CliTestHarness::builder().build();
    let result = super::config_cmd::cmd_config_get(&h.cli(), h.printer(), "profile");
    assert!(
        result.is_ok(),
        "config get profile should succeed: {:?}",
        result.err()
    );
    let output = h.output();
    assert!(
        output.contains("default"),
        "should output 'default' profile, got: {output}"
    );
}

#[test]
fn cmd_config_get_missing_key_errors_no_config() {
    let h = CliTestHarness::builder().build();
    let result = super::config_cmd::cmd_config_get(&h.cli(), h.printer(), "nonexistent.key");
    assert!(result.is_err(), "missing key should error");
    assert!(
        result.unwrap_err().to_string().contains("not found"),
        "should report key not found"
    );
}

#[test]
fn cmd_config_get_json_output() {
    let h = CliTestHarness::builder().json().build();
    let result = super::config_cmd::cmd_config_get(&h.cli(), h.printer(), "profile");
    assert!(result.is_ok(), "JSON config get should succeed");
    let output = h.output();
    // JSON output should contain the value
    assert!(
        output.contains("default"),
        "JSON should contain profile value, got: {output}"
    );
}

// -----------------------------------------------------------------------
// build_registry_with_config_and_packages — with custom packages spec
// -----------------------------------------------------------------------

#[test]
fn build_registry_with_config_populates_secret_backend() {
    let cfg = config::CfgdConfig {
        api_version: cfgd_core::API_VERSION.into(),
        kind: "Config".into(),
        metadata: config::ConfigMetadata {
            name: "test".into(),
        },
        spec: config::ConfigSpec {
            secrets: Some(config::SecretsConfig {
                backend: "age".into(),
                sops: None,
                integrations: vec![],
            }),
            ..config::ConfigSpec::default()
        },
    };
    let registry = super::build_registry_with_config_and_packages(Some(&cfg), None);
    assert!(
        registry.secret_backend.is_some(),
        "should have a secret backend configured"
    );
    assert!(
        !registry.package_managers.is_empty(),
        "should have package managers"
    );
    assert!(
        !registry.system_configurators.is_empty(),
        "should have system configurators"
    );
}

#[test]
fn build_registry_with_no_config_uses_defaults() {
    let registry = super::build_registry_with_config_and_packages(None, None);
    assert!(
        !registry.package_managers.is_empty(),
        "should have default package managers even without config"
    );
    assert!(
        registry.secret_backend.is_some(),
        "should have default secret backend"
    );
}

// -----------------------------------------------------------------------
// cmd_diff empty profile — exercises file, package, and system diff display
// -----------------------------------------------------------------------

#[test]
fn cmd_diff_full_profile_shows_all_sections() {
    let h = CliTestHarness::builder().build();
    let result = super::diff::cmd_diff(&h.cli(), h.printer(), None, false);
    assert!(
        result.is_ok(),
        "diff with default profile should succeed: {:?}",
        result.err()
    );

    let output = h.output();
    assert!(
        output.contains("Diff"),
        "should show Diff header, got: {output}"
    );
    assert!(
        output.contains("Files"),
        "should show Files section, got: {output}"
    );
    assert!(
        output.contains("Packages"),
        "should show Packages section, got: {output}"
    );
    assert!(
        output.contains("System"),
        "should show System section, got: {output}"
    );
}

// -----------------------------------------------------------------------
// cmd_diff with module filter — module-only diff path
// -----------------------------------------------------------------------

#[test]
fn cmd_diff_module_not_found_shows_info() {
    let h = CliTestHarness::builder().build();
    let result = super::diff::cmd_diff(&h.cli(), h.printer(), Some("nonexistent-mod"), false);
    assert!(
        result.is_ok(),
        "diff with missing module should succeed gracefully"
    );
    let output = h.output();
    assert!(
        output.contains("not found"),
        "should indicate module not found, got: {output}"
    );
}

#[test]
fn cmd_diff_module_with_files_shows_file_and_package_sections() {
    // Target lands in an isolated temp dir (never a shared path); it is left
    // absent so the renderer exercises the missing-target branch.
    let target_dir = tempfile::tempdir().unwrap();
    let target = target_dir.path().join("cfgd-diff-test-target");
    let module_yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: diff-mod\nspec:\n  packages:\n    - name: curl\n  files:\n    - source: my-config\n      target: {}\n",
        target.display()
    );
    let h = CliTestHarness::builder()
        .module("diff-mod", &module_yaml)
        .build();

    // Module file `source: my-config` resolves relative to the module dir, so
    // the source must live at `<module_dir>/my-config`.
    let module_dir = h.config_path().join("modules").join("diff-mod");
    std::fs::write(module_dir.join("my-config"), "new config content\n").unwrap();

    let result = super::diff::cmd_diff(&h.cli(), h.printer(), Some("diff-mod"), false);
    assert!(
        result.is_ok(),
        "module diff should succeed: {:?}",
        result.err()
    );

    let output = h.output();
    assert!(
        output.contains("Module") && output.contains("diff-mod"),
        "should show module name, got: {output}"
    );
    assert!(
        output.contains("Files"),
        "should show Files line, got: {output}"
    );
    assert!(
        output.contains("Packages"),
        "should show Packages section, got: {output}"
    );
    // Target is absent: the shared renderer shows the would-be-created content
    // ("(new file)" + the source bytes), identical to profile-file rendering.
    assert!(
        output.contains("(new file)") && output.contains("new config content"),
        "should render new-file content for the missing target, got: {output}"
    );
}

// -----------------------------------------------------------------------
// cmd_status full profile with sources — exercises source display
// -----------------------------------------------------------------------

#[test]
fn cmd_status_with_sources_shows_source_section() {
    let h = CliTestHarness::builder().rich_config().build();
    let result = super::status::cmd_status(&h.cli(), h.printer(), None, false);
    assert!(
        result.is_ok(),
        "status with sources should succeed: {:?}",
        result.err()
    );

    let output = h.output();
    assert!(
        output.contains("Status"),
        "should show Status header, got: {output}"
    );
    assert!(
        output.contains("Config Sources"),
        "should show Config Sources section, got: {output}"
    );
    assert!(
        output.contains("team-config"),
        "should show source name, got: {output}"
    );
    assert!(
        output.contains("not yet fetched"),
        "unfetched source should show 'not yet fetched', got: {output}"
    );
}

// -----------------------------------------------------------------------
// action_path — remaining variants not covered above
// -----------------------------------------------------------------------

#[test]
fn action_path_file_update() {
    let action = reconciler::Action::File(FileAction::Update {
        source: PathBuf::from("/src/bashrc"),
        target: PathBuf::from("/home/user/.bashrc"),
        diff: "diff contents".into(),
        origin: "profile".into(),
        strategy: config::FileStrategy::Copy,
        source_hash: None,
    });
    let path = super::action_path(&PhaseName::Files, &action);
    assert_eq!(path, "files:/home/user/.bashrc");
}

#[test]
fn action_path_file_delete() {
    let action = reconciler::Action::File(FileAction::Delete {
        target: PathBuf::from("/home/user/.obsolete"),
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::Files, &action);
    assert_eq!(path, "files:/home/user/.obsolete");
}

#[test]
fn action_path_file_permissions() {
    let action = reconciler::Action::File(FileAction::SetPermissions {
        target: PathBuf::from("/home/user/.ssh/config"),
        mode: 0o600,
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::Files, &action);
    assert_eq!(path, "files:/home/user/.ssh/config");
}

#[test]
fn action_path_file_skip() {
    let action = reconciler::Action::File(FileAction::Skip {
        target: PathBuf::from("/home/user/.gitconfig"),
        reason: "up to date".into(),
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::Files, &action);
    assert_eq!(path, "files:/home/user/.gitconfig");
}

#[test]
fn action_path_package_uninstall() {
    let action = reconciler::Action::Package(PackageAction::Uninstall {
        manager: "npm".into(),
        packages: vec!["left-pad".into()],
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::Packages, &action);
    assert_eq!(path, "packages.npm");
}

#[test]
fn action_path_package_bootstrap() {
    let action = reconciler::Action::Package(PackageAction::Bootstrap {
        manager: "brew".into(),
        method: "curl install".into(),
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::Packages, &action);
    assert_eq!(path, "packages.brew");
}

#[test]
fn action_path_package_skip() {
    let action = reconciler::Action::Package(PackageAction::Skip {
        manager: "cargo".into(),
        reason: "already installed".into(),
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::Packages, &action);
    assert_eq!(path, "packages.cargo");
}

#[test]
fn action_path_script_run() {
    let entry = config::ScriptEntry::Simple("scripts/setup.sh".into());
    let action = reconciler::Action::Script(reconciler::ScriptAction::Run {
        entry,
        phase: reconciler::ScriptPhase::PreApply,
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::PreScripts, &action);
    assert_eq!(path, "pre-scripts:scripts/setup.sh");
}

#[test]
fn action_path_script_run_full_entry() {
    let entry = config::ScriptEntry::Full {
        workdir: None,
        run: "echo hello".into(),
        timeout: None,
        idle_timeout: None,
        continue_on_error: None,
        shell: config::ScriptShell::Auto,
        only_if: None,
        unless: None,
        creates: None,
        interactive: false,
    };
    let action = reconciler::Action::Script(reconciler::ScriptAction::Run {
        entry,
        phase: reconciler::ScriptPhase::PostApply,
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::PostScripts, &action);
    assert_eq!(path, "post-scripts:echo hello");
}

#[test]
fn action_path_system_set_value() {
    let action = reconciler::Action::System(reconciler::SystemAction::SetValue {
        configurator: "sysctl".into(),
        key: "vm.swappiness".into(),
        desired: "10".into(),
        current: "60".into(),
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::System, &action);
    assert_eq!(path, "system.sysctl.vm.swappiness");
}

#[test]
fn action_path_system_skip() {
    let action = reconciler::Action::System(reconciler::SystemAction::Skip {
        configurator: "macosDefaults".into(),
        reason: "not on macOS".into(),
        origin: "profile".into(),
        unknown: false,
    });
    let path = super::action_path(&PhaseName::System, &action);
    assert_eq!(path, "system.macosDefaults");
}

#[test]
fn action_path_env_inject_source_line() {
    let action = reconciler::Action::Env(reconciler::EnvAction::InjectSourceLine {
        rc_path: PathBuf::from("/home/user/.zshrc"),
        line: "source ~/.cfgd.env".into(),
    });
    let path = super::action_path(&PhaseName::Env, &action);
    assert_eq!(path, "env:/home/user/.zshrc");
}

#[test]
fn action_path_secret_decrypt() {
    let action = reconciler::Action::Secret(SecretAction::Decrypt {
        source: PathBuf::from("/repo/secrets/api.enc"),
        target: PathBuf::from("/home/user/.config/api-key"),
        backend: "sops".into(),
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::Secrets, &action);
    assert_eq!(path, "secrets:/home/user/.config/api-key");
}

#[test]
fn action_path_secret_resolve() {
    let action = reconciler::Action::Secret(SecretAction::Resolve {
        provider: "1password".into(),
        reference: "op://vault/item/field".into(),
        target: PathBuf::from("/home/user/.token"),
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::Secrets, &action);
    assert_eq!(path, "secrets.1password.op://vault/item/field");
}

#[test]
fn action_path_secret_resolve_env() {
    let action = reconciler::Action::Secret(SecretAction::ResolveEnv {
        provider: "vault".into(),
        reference: "secret/data/app".into(),
        envs: vec!["API_KEY".into(), "DB_PASS".into()],
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::Secrets, &action);
    assert_eq!(path, "secrets.vault.secret/data/app:[API_KEY,DB_PASS]");
}

#[test]
fn action_path_secret_skip() {
    let action = reconciler::Action::Secret(SecretAction::Skip {
        source: "old-secret".into(),
        reason: "not needed".into(),
        origin: "profile".into(),
    });
    let path = super::action_path(&PhaseName::Secrets, &action);
    assert_eq!(path, "secrets.old-secret");
}

// -----------------------------------------------------------------------
// workflow::generate_release_workflow_yaml — deeper content verification
// -----------------------------------------------------------------------

#[test]
fn generate_release_workflow_multiple_modules() {
    let yaml = super::workflow::generate_release_workflow_yaml(
        &["shell-tools".into(), "git-config".into()],
        &[],
        "master",
    )
    .unwrap();
    // Both module paths should appear
    assert!(yaml.contains("modules/shell-tools/**"));
    assert!(yaml.contains("modules/git-config/**"));
    // Both should have matrix entries in detect-changes outputs
    assert!(yaml.contains("module_shell_tools"));
    assert!(yaml.contains("module_git_config"));
    // Should have tag-modules job but not tag-profiles
    assert!(yaml.contains("tag-modules:"));
    assert!(!yaml.contains("tag-profiles:"));
}

#[test]
fn generate_release_workflow_hyphenated_names_become_underscored() {
    let yaml = super::workflow::generate_release_workflow_yaml(
        &["my-cool-tools".into()],
        &["work-laptop".into()],
        "master",
    )
    .unwrap();
    // Hyphens in names become underscores in output variable names
    assert!(yaml.contains("module_my_cool_tools"));
    assert!(yaml.contains("profile_work_laptop"));
}

#[test]
fn generate_release_workflow_empty_has_placeholder_job() {
    let yaml = super::workflow::generate_release_workflow_yaml(&[], &[], "master").unwrap();
    // Should have commented-out paths section
    assert!(yaml.contains("# paths:"));
    // Should have placeholder job
    assert!(yaml.contains("placeholder:"));
    assert!(yaml.contains("No modules or profiles to tag yet"));
    // Should NOT have detect-changes or tag jobs
    assert!(!yaml.contains("detect-changes:"));
    assert!(!yaml.contains("tag-modules:"));
    assert!(!yaml.contains("tag-profiles:"));
}

#[test]
fn generate_release_workflow_profiles_only() {
    let yaml = super::workflow::generate_release_workflow_yaml(
        &[],
        &["personal".into(), "server".into()],
        "master",
    )
    .unwrap();
    assert!(yaml.contains("profiles/personal.yaml"));
    assert!(yaml.contains("profiles/personal.yml"));
    assert!(yaml.contains("profiles/server.yaml"));
    assert!(yaml.contains("profiles/server.yml"));
    assert!(yaml.contains("tag-profiles:"));
    assert!(!yaml.contains("tag-modules:"));
    assert!(yaml.contains("detect-changes:"));
}

// -----------------------------------------------------------------------
// set_nested_yaml_value — top-level key
// -----------------------------------------------------------------------

#[test]
fn set_nested_yaml_value_top_level_key() {
    let mut root = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    super::set_nested_yaml_value(&mut root, "name", &serde_yaml::Value::String("test".into()))
        .unwrap();

    let val = root.get("name").and_then(|v| v.as_str());
    assert_eq!(val, Some("test"));
}

#[test]
fn set_nested_yaml_value_three_level_path() {
    let mut root = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    super::set_nested_yaml_value(
        &mut root,
        "a.b.c",
        &serde_yaml::Value::String("value".into()),
    )
    .unwrap();

    let val = root
        .get("a")
        .and_then(|v| v.get("b"))
        .and_then(|v| v.get("c"))
        .and_then(|v| v.as_str());
    assert_eq!(val, Some("value"));
    // Intermediate nodes should be mappings
    assert!(root.get("a").unwrap().is_mapping());
    assert!(root.get("a").unwrap().get("b").unwrap().is_mapping());
}

#[test]
fn set_nested_yaml_value_preserves_siblings() {
    let mut root: serde_yaml::Value = serde_yaml::from_str("a:\n  existing: kept\n").unwrap();
    super::set_nested_yaml_value(
        &mut root,
        "a.new_key",
        &serde_yaml::Value::String("added".into()),
    )
    .unwrap();

    // Existing sibling should be preserved
    let existing = root
        .get("a")
        .and_then(|v| v.get("existing"))
        .and_then(|v| v.as_str());
    assert_eq!(existing, Some("kept"));
    // New key should be present
    let new_key = root
        .get("a")
        .and_then(|v| v.get("new_key"))
        .and_then(|v| v.as_str());
    assert_eq!(new_key, Some("added"));
}

#[test]
fn set_nested_yaml_value_numeric_value() {
    let mut root = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    super::set_nested_yaml_value(
        &mut root,
        "spec.replicas",
        &serde_yaml::Value::Number(serde_yaml::Number::from(3)),
    )
    .unwrap();

    let val = root
        .get("spec")
        .and_then(|v| v.get("replicas"))
        .and_then(|v| v.as_u64());
    assert_eq!(val, Some(3));
}

// -----------------------------------------------------------------------
// pattern_matches — additional cases
// -----------------------------------------------------------------------

#[test]
fn pattern_matches_no_match() {
    assert!(!super::pattern_matches("files", "packages.brew.ripgrep"));
}

#[test]
fn pattern_matches_longer_pattern_than_path() {
    assert!(!super::pattern_matches(
        "packages.brew.ripgrep.extra",
        "packages.brew.ripgrep"
    ));
}

#[test]
fn pattern_matches_secrets_colon() {
    assert!(super::pattern_matches(
        "secrets",
        "secrets:/home/user/.token"
    ));
}

#[test]
fn pattern_matches_env_colon() {
    assert!(super::pattern_matches("env", "env:/home/user/.zshrc"));
}

#[test]
fn pattern_matches_exact_colon_path() {
    assert!(super::pattern_matches(
        "files:/home/user/.bashrc",
        "files:/home/user/.bashrc"
    ));
}

#[test]
fn pattern_matches_empty_pattern() {
    // Empty pattern should not match non-empty paths
    assert!(!super::pattern_matches("", "packages.brew"));
}

// -----------------------------------------------------------------------
// copy_files_to_dir — additional forbidden prefix tests
// -----------------------------------------------------------------------

#[test]
fn copy_files_to_dir_rejects_usr_directory() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");
    if std::path::Path::new("/usr/bin/env").exists() {
        let result = super::copy_files_to_dir(&["/usr/bin/env".into()], &repo_dir);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("system directory"),
            "expected 'system directory' error, got: {msg}"
        );
    }
}

#[test]
fn copy_files_to_dir_rejects_bin_directory() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");
    if std::path::Path::new("/bin/sh").exists() {
        let result = super::copy_files_to_dir(&["/bin/sh".into()], &repo_dir);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("system directory"),
            "expected 'system directory' error, got: {msg}"
        );
    }
}

#[test]
fn copy_files_to_dir_rejects_var_directory() {
    let dir = tempfile::tempdir().unwrap();
    let repo_dir = dir.path().join("repo");
    if std::path::Path::new("/var/log/syslog").exists() {
        let result = super::copy_files_to_dir(&["/var/log/syslog".into()], &repo_dir);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("system directory"),
            "expected 'system directory' error, got: {msg}"
        );
    }
}

#[test]
fn copy_files_to_dir_allows_home_directory() {
    let dir = tempfile::tempdir().unwrap();
    // Create a file in a temp directory (not a system directory)
    let source = dir.path().join("safe-file.txt");
    std::fs::write(&source, "safe content").unwrap();
    let repo_dir = dir.path().join("repo");

    let results = super::copy_files_to_dir(&[source.display().to_string()], &repo_dir).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, "safe-file.txt");
    assert!(repo_dir.join("safe-file.txt").exists());
    assert_eq!(
        std::fs::read_to_string(repo_dir.join("safe-file.txt")).unwrap(),
        "safe content"
    );
}

// -----------------------------------------------------------------------
// source::cmd_source_list — table columns with state info
// -----------------------------------------------------------------------

#[test]
fn cmd_source_list_table_shows_status_and_priority() {
    let h = CliTestHarness::builder().rich_config().build();
    // Populate state with source info so the table columns have values
    let state = super::open_state_store(Some(h.state_path())).unwrap();
    state
        .upsert_config_source(
            "team-config",
            "https://github.com/team/config",
            "main",
            Some("abc123"),
            Some("1.2.0"),
            None,
        )
        .unwrap();

    super::source::cmd_source_list(&h.cli(), h.printer()).unwrap();

    let output = h.output();
    // Table should include the source name, URL, priority, version, and status
    assert!(
        output.contains("team-config"),
        "table should show source name, got: {output}"
    );
    assert!(
        output.contains("100"),
        "table should show priority 100, got: {output}"
    );
}

#[test]
fn cmd_source_list_structured_json_includes_state_info() {
    let h = CliTestHarness::builder().rich_config().json().build();
    let state = super::open_state_store(Some(h.state_path())).unwrap();
    state
        .upsert_config_source(
            "team-config",
            "https://github.com/team/config",
            "main",
            Some("abc123def"),
            Some("2.0.0"),
            None,
        )
        .unwrap();

    super::source::cmd_source_list(&h.cli(), h.printer()).unwrap();

    let output = h.output();
    let parsed: serde_json::Value = serde_json::from_str(output.trim())
        .unwrap_or_else(|e| panic!("invalid JSON: {e}, got: {output}"));
    let arr = parsed.as_array().expect("should be an array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "team-config");
    assert_eq!(arr[0]["version"], "2.0.0");
    assert!(
        arr[0]["lastFetched"].is_string(),
        "lastFetched should be populated after upsert"
    );
}

// -----------------------------------------------------------------------
// source::cmd_source_show — verify key fields displayed with state + resources
// -----------------------------------------------------------------------

#[test]
fn cmd_source_show_displays_all_key_fields() {
    let h = CliTestHarness::builder().rich_config().build();

    super::source::cmd_source_show(&h.cli(), h.printer(), "team-config").unwrap();

    let output = h.output();
    assert!(
        output.contains("URL"),
        "should display URL label, got: {output}"
    );
    assert!(
        output.contains("https://github.com/team/config"),
        "should display URL value, got: {output}"
    );
    assert!(
        output.contains("Branch"),
        "should display Branch label, got: {output}"
    );
    assert!(
        output.contains("main"),
        "should display branch value, got: {output}"
    );
    assert!(
        output.contains("Priority"),
        "should display Priority label, got: {output}"
    );
    assert!(
        output.contains("100"),
        "should display priority value, got: {output}"
    );
    assert!(
        output.contains("Sync Interval"),
        "should display Sync Interval label, got: {output}"
    );
    assert!(
        output.contains("Auto Apply"),
        "should display Auto Apply label, got: {output}"
    );
}

#[test]
fn cmd_source_show_with_state_shows_status_section() {
    let h = CliTestHarness::builder().rich_config().build();
    let state = super::open_state_store(Some(h.state_path())).unwrap();
    state
        .upsert_config_source(
            "team-config",
            "https://github.com/team/config",
            "main",
            Some("deadbeef1234"),
            Some("3.1.0"),
            None,
        )
        .unwrap();

    super::source::cmd_source_show(&h.cli(), h.printer(), "team-config").unwrap();

    let output = h.output();
    assert!(
        output.contains("State"),
        "should display State section, got: {output}"
    );
    assert!(
        output.contains("Status"),
        "should display Status within State section, got: {output}"
    );
    assert!(
        output.contains("Last Fetched"),
        "should display Last Fetched, got: {output}"
    );
    // Last Commit should be truncated to 12 chars
    assert!(
        output.contains("deadbeef1234"),
        "should display truncated commit hash, got: {output}"
    );
    assert!(
        output.contains("3.1.0"),
        "should display version, got: {output}"
    );
}

#[test]
fn cmd_source_show_with_managed_resources_shows_table() {
    let h = CliTestHarness::builder().rich_config().build();
    let state = super::open_state_store(Some(h.state_path())).unwrap();
    state
        .upsert_managed_resource("package", "brew/curl", "team-config", None, None)
        .unwrap();
    state
        .upsert_managed_resource("file", "~/.bashrc", "team-config", None, None)
        .unwrap();

    super::source::cmd_source_show(&h.cli(), h.printer(), "team-config").unwrap();

    let output = h.output();
    assert!(
        output.contains("Managed Resources"),
        "should display Managed Resources section, got: {output}"
    );
    assert!(
        output.contains("brew/curl"),
        "should list brew/curl resource, got: {output}"
    );
    assert!(
        output.contains("~/.bashrc"),
        "should list ~/.bashrc resource, got: {output}"
    );
}

#[test]
fn cmd_source_show_json_includes_managed_resources() {
    let h = CliTestHarness::builder().rich_config().json().build();
    let state = super::open_state_store(Some(h.state_path())).unwrap();
    state
        .upsert_managed_resource("env", "EDITOR", "team-config", None, None)
        .unwrap();

    super::source::cmd_source_show(&h.cli(), h.printer(), "team-config").unwrap();

    let parsed = h.json_output();
    assert_eq!(parsed["name"], "team-config");
    let resources = parsed["managedResources"]
        .as_array()
        .expect("should be array");
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["resourceType"], "env");
    assert_eq!(resources[0]["resourceId"], "EDITOR");
}

// -----------------------------------------------------------------------
// source::cmd_source_remove — keep_all reassigns resources to local
// -----------------------------------------------------------------------

#[test]
fn cmd_source_remove_keep_all_reassigns_resources_to_local() {
    let h = CliTestHarness::builder().rich_config().build();
    let state = super::open_state_store(Some(h.state_path())).unwrap();
    // Pre-populate managed resources owned by team-config
    state
        .upsert_managed_resource("package", "brew/curl", "team-config", Some("hash1"), None)
        .unwrap();
    state
        .upsert_managed_resource("env", "EDITOR", "team-config", Some("hash2"), None)
        .unwrap();

    let result =
        super::source::cmd_source_remove(&h.cli(), h.printer(), "team-config", true, false, false);
    assert!(result.is_ok(), "remove with keep_all: {:?}", result.err());

    // Source should be gone from config
    let cfg = config::load_config(&h.config_path().join("cfgd.yaml")).unwrap();
    assert!(
        cfg.spec.sources.is_empty(),
        "source should be removed from config"
    );

    // Resources should now be owned by "local"
    let resources = state.managed_resources_by_source("local").unwrap();
    assert_eq!(
        resources.len(),
        2,
        "both resources should be reassigned to local"
    );
    let resource_ids: Vec<&str> = resources.iter().map(|r| r.resource_id.as_str()).collect();
    assert!(resource_ids.contains(&"brew/curl"));
    assert!(resource_ids.contains(&"EDITOR"));

    // team-config should have no resources left
    let team_resources = state.managed_resources_by_source("team-config").unwrap();
    assert!(team_resources.is_empty());
}

#[test]
fn cmd_source_remove_remove_all_does_not_reassign() {
    let h = CliTestHarness::builder().rich_config().build();
    let state = super::open_state_store(Some(h.state_path())).unwrap();
    state
        .upsert_managed_resource("package", "brew/curl", "team-config", None, None)
        .unwrap();

    let result =
        super::source::cmd_source_remove(&h.cli(), h.printer(), "team-config", false, true, false);
    assert!(result.is_ok(), "remove with remove_all: {:?}", result.err());

    // Source should be gone from config
    let cfg = config::load_config(&h.config_path().join("cfgd.yaml")).unwrap();
    assert!(cfg.spec.sources.is_empty());

    // Resources should NOT be reassigned to local (they stay with team-config
    // but the source state record is deleted, so they're effectively orphaned)
    let local_resources = state.managed_resources_by_source("local").unwrap();
    assert!(
        local_resources.is_empty(),
        "remove_all should not reassign resources to local"
    );

    h.assert_output_contains("removed");
}

#[test]
fn cmd_source_remove_prints_success_message() {
    let h = CliTestHarness::builder().rich_config().build();

    super::source::cmd_source_remove(&h.cli(), h.printer(), "team-config", false, true, false)
        .unwrap();

    h.assert_output_contains("Source 'team-config' removed");
}

// -----------------------------------------------------------------------
// cmd_compliance_diff — actual differences between snapshots
// -----------------------------------------------------------------------

#[test]
fn cmd_compliance_diff_identical_snapshots_reports_no_differences() {
    let h = CliTestHarness::builder().build();

    // Create two identical snapshots
    super::compliance::cmd_compliance_snapshot(&h.cli(), h.printer()).unwrap();
    super::compliance::cmd_compliance_snapshot(&h.cli(), h.printer()).unwrap();

    let state = super::open_state_store(Some(h.state_path())).unwrap();
    let entries = state.compliance_history(None, 10).unwrap();
    assert_eq!(entries.len(), 2);

    // Clear output before diff
    h.buf.lock().unwrap().clear();

    super::compliance::cmd_compliance_diff(&h.cli(), h.printer(), entries[1].id, entries[0].id)
        .unwrap();

    h.assert_output_contains("No differences");
}

#[test]
fn cmd_compliance_diff_with_changes_shows_added_and_removed() {
    let h = CliTestHarness::builder().build();
    let state = super::open_state_store(Some(h.state_path())).unwrap();

    // Create first snapshot with one check
    let snap1 = cfgd_core::compliance::ComplianceSnapshot {
        timestamp: "2026-01-01T00:00:00Z".into(),
        machine: cfgd_core::compliance::MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec![],
        checks: vec![cfgd_core::compliance::ComplianceCheck {
            category: "packages".into(),
            target: Some("brew/curl".into()),
            status: cfgd_core::compliance::ComplianceStatus::Compliant,
            ..Default::default()
        }],
        summary: cfgd_core::compliance::ComplianceSummary {
            compliant: 1,
            warning: 0,
            violation: 0,
        },
    };
    state.store_compliance_snapshot(&snap1, "hash1").unwrap();

    // Create second snapshot with a different check (curl removed, git added)
    let snap2 = cfgd_core::compliance::ComplianceSnapshot {
        timestamp: "2026-01-02T00:00:00Z".into(),
        machine: cfgd_core::compliance::MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec![],
        checks: vec![cfgd_core::compliance::ComplianceCheck {
            category: "packages".into(),
            target: Some("brew/git".into()),
            status: cfgd_core::compliance::ComplianceStatus::Warning,
            ..Default::default()
        }],
        summary: cfgd_core::compliance::ComplianceSummary {
            compliant: 0,
            warning: 1,
            violation: 0,
        },
    };
    state.store_compliance_snapshot(&snap2, "hash2").unwrap();

    let entries = state.compliance_history(None, 10).unwrap();
    assert_eq!(entries.len(), 2);
    // entries are DESC order, so entries[0] is snap2 (id=2), entries[1] is snap1 (id=1)
    let id1 = entries[1].id;
    let id2 = entries[0].id;

    super::compliance::cmd_compliance_diff(&h.cli(), h.printer(), id1, id2).unwrap();

    let output = h.output();
    assert!(
        output.contains("Added"),
        "should show Added section for brew/git, got: {output}"
    );
    assert!(
        output.contains("Removed"),
        "should show Removed section for brew/curl, got: {output}"
    );
    assert!(
        output.contains("brew/git"),
        "should mention added check brew/git, got: {output}"
    );
    assert!(
        output.contains("brew/curl"),
        "should mention removed check brew/curl, got: {output}"
    );
}

#[test]
fn cmd_compliance_diff_with_status_change_shows_changed() {
    let h = CliTestHarness::builder().build();
    let state = super::open_state_store(Some(h.state_path())).unwrap();

    let snap1 = cfgd_core::compliance::ComplianceSnapshot {
        timestamp: "2026-01-01T00:00:00Z".into(),
        machine: cfgd_core::compliance::MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec![],
        checks: vec![cfgd_core::compliance::ComplianceCheck {
            category: "packages".into(),
            target: Some("brew/curl".into()),
            status: cfgd_core::compliance::ComplianceStatus::Compliant,
            detail: None,
            ..Default::default()
        }],
        summary: cfgd_core::compliance::ComplianceSummary {
            compliant: 1,
            warning: 0,
            violation: 0,
        },
    };
    state.store_compliance_snapshot(&snap1, "hash1").unwrap();

    // Same check but status changed from Compliant to Violation
    let snap2 = cfgd_core::compliance::ComplianceSnapshot {
        timestamp: "2026-01-02T00:00:00Z".into(),
        machine: cfgd_core::compliance::MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec![],
        checks: vec![cfgd_core::compliance::ComplianceCheck {
            category: "packages".into(),
            target: Some("brew/curl".into()),
            status: cfgd_core::compliance::ComplianceStatus::Violation,
            detail: Some("package not installed".into()),
            ..Default::default()
        }],
        summary: cfgd_core::compliance::ComplianceSummary {
            compliant: 0,
            warning: 0,
            violation: 1,
        },
    };
    state.store_compliance_snapshot(&snap2, "hash2").unwrap();

    let entries = state.compliance_history(None, 10).unwrap();
    let id1 = entries[1].id;
    let id2 = entries[0].id;

    super::compliance::cmd_compliance_diff(&h.cli(), h.printer(), id1, id2).unwrap();

    let output = h.output();
    assert!(
        output.contains("Changed"),
        "should show Changed section, got: {output}"
    );
    assert!(
        output.contains("Compliant") && output.contains("Violation"),
        "should show status transition, got: {output}"
    );
    assert!(
        output.contains("package not installed"),
        "should show detail for changed check, got: {output}"
    );
}

#[test]
fn cmd_compliance_diff_structured_json_with_changes() {
    let h = CliTestHarness::builder().json().build();
    let state = super::open_state_store(Some(h.state_path())).unwrap();

    let snap1 = cfgd_core::compliance::ComplianceSnapshot {
        timestamp: "2026-01-01T00:00:00Z".into(),
        machine: cfgd_core::compliance::MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec![],
        checks: vec![cfgd_core::compliance::ComplianceCheck {
            category: "env".into(),
            target: Some("EDITOR".into()),
            status: cfgd_core::compliance::ComplianceStatus::Compliant,
            ..Default::default()
        }],
        summary: cfgd_core::compliance::ComplianceSummary {
            compliant: 1,
            warning: 0,
            violation: 0,
        },
    };
    state.store_compliance_snapshot(&snap1, "hash1").unwrap();

    let snap2 = cfgd_core::compliance::ComplianceSnapshot {
        timestamp: "2026-01-02T00:00:00Z".into(),
        machine: cfgd_core::compliance::MachineInfo {
            hostname: "test".into(),
            os: "linux".into(),
            arch: "x86_64".into(),
        },
        profile: "default".into(),
        sources: vec![],
        checks: vec![
            cfgd_core::compliance::ComplianceCheck {
                category: "env".into(),
                target: Some("EDITOR".into()),
                status: cfgd_core::compliance::ComplianceStatus::Warning,
                ..Default::default()
            },
            cfgd_core::compliance::ComplianceCheck {
                category: "packages".into(),
                target: Some("brew/jq".into()),
                status: cfgd_core::compliance::ComplianceStatus::Compliant,
                ..Default::default()
            },
        ],
        summary: cfgd_core::compliance::ComplianceSummary {
            compliant: 1,
            warning: 1,
            violation: 0,
        },
    };
    state.store_compliance_snapshot(&snap2, "hash2").unwrap();

    let entries = state.compliance_history(None, 10).unwrap();
    let id1 = entries[1].id;
    let id2 = entries[0].id;

    super::compliance::cmd_compliance_diff(&h.cli(), h.printer(), id1, id2).unwrap();

    let parsed = h.json_output();
    assert_eq!(parsed["id1"], id1);
    assert_eq!(parsed["id2"], id2);
    let added = parsed["added"].as_array().expect("added should be array");
    assert_eq!(added.len(), 1, "one check was added (brew/jq)");
    assert_eq!(added[0]["target"], "brew/jq");
    let changed = parsed["changed"]
        .as_array()
        .expect("changed should be array");
    assert_eq!(changed.len(), 1, "one check changed status (EDITOR)");
    assert_eq!(changed[0]["oldStatus"], "Compliant");
    assert_eq!(changed[0]["newStatus"], "Warning");
    let removed = parsed["removed"]
        .as_array()
        .expect("removed should be array");
    assert!(removed.is_empty(), "nothing was removed");
}

// -----------------------------------------------------------------------
// cmd_compliance_history — with snapshots populated
// -----------------------------------------------------------------------

#[test]
fn cmd_compliance_history_with_entries_shows_table() {
    let h = CliTestHarness::builder().build();

    // Create a snapshot to populate history
    super::compliance::cmd_compliance_snapshot(&h.cli(), h.printer()).unwrap();

    h.buf.lock().unwrap().clear();

    super::compliance::cmd_compliance_history(&h.cli(), h.printer(), None).unwrap();

    let output = h.output();
    assert!(
        output.contains("Compliance History"),
        "should show Compliance History header, got: {output}"
    );
    // Table should have columns
    assert!(
        output.contains("Compliant"),
        "should have Compliant column, got: {output}"
    );
    assert!(
        output.contains("Warning"),
        "should have Warning column, got: {output}"
    );
    assert!(
        output.contains("Violation"),
        "should have Violation column, got: {output}"
    );
}

// -----------------------------------------------------------------------
// cmd_doctor — invalid config, JSON fields, modules section
// -----------------------------------------------------------------------

#[test]
fn cmd_doctor_with_invalid_config_shows_error_but_succeeds() {
    let h = CliTestHarness::builder()
        .config("this is not valid yaml: [[[")
        .build();

    let result = super::doctor::run_doctor(&h.cli(), h.printer());
    assert!(
        result.is_ok(),
        "doctor should succeed even with invalid config"
    );

    let output = h.output();
    assert!(
        output.contains("Doctor"),
        "should show Doctor header, got: {output}"
    );
    // The config check should report invalid
    assert!(
        output.contains("not found") || output.contains("Config file"),
        "should mention config file status, got: {output}"
    );
}

#[test]
fn cmd_doctor_json_has_all_top_level_fields() {
    let h = CliTestHarness::builder().json().build();

    super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();

    let parsed = h.json_output();
    assert_json_has_fields(
        &parsed,
        &[
            "config",
            "git",
            "secrets",
            "packageManagers",
            "modules",
            "systemConfigurators",
        ],
    );
    assert_json_field_type(&parsed, "config", "object");
    assert_json_field_type(&parsed, "git", "boolean");
    assert_json_field_type(&parsed, "secrets", "object");
    assert_json_field_type(&parsed, "packageManagers", "array");
    assert_json_field_type(&parsed, "modules", "array");
    assert_json_field_type(&parsed, "systemConfigurators", "array");
}

#[test]
fn cmd_doctor_json_config_section_has_expected_fields() {
    let h = CliTestHarness::builder().json().build();

    super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();

    let parsed = h.json_output();
    let config = &parsed["config"];
    assert_json_has_fields(config, &["valid", "path"]);
    assert_eq!(config["valid"], true);
    assert!(
        config["name"].is_string(),
        "name should be present for valid config"
    );
}

#[test]
fn cmd_doctor_with_module_in_profile() {
    let profile_with_module = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  modules:
    - test-mod
  packages:
    cargo:
      - bat
"#;
    let h = CliTestHarness::builder()
        .profile("default", profile_with_module)
        .module("test-mod", SIMPLE_MODULE_YAML)
        .build();

    super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();

    let output = h.output();
    assert!(
        output.contains("Modules"),
        "should show Modules section when modules declared, got: {output}"
    );
    assert!(
        output.contains("test-mod"),
        "should list the test-mod module, got: {output}"
    );
}

#[test]
fn cmd_doctor_with_missing_module_reports_not_found() {
    let profile_with_missing_module = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  modules:
    - nonexistent-mod
  packages:
    cargo:
      - bat
"#;
    let h = CliTestHarness::builder()
        .profile("default", profile_with_missing_module)
        .build();

    super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();

    let output = h.output();
    assert!(
        output.contains("nonexistent-mod"),
        "should mention the missing module, got: {output}"
    );
    assert!(
        output.contains("not found"),
        "should report module not found, got: {output}"
    );
}

/// Profile that declares *every* package-manager category in `PackagesSpec`:
/// the struct-wrapper managers (brew formulae/taps/casks, apt, cargo, npm
/// global, snap, flatpak) PLUS the simple-list managers exposed by
/// `non_empty_simple_lists` (pipx, dnf, apk, pacman, zypper, yum, pkg, nix,
/// go, winget, chocolatey, scoop). Used to drive doctor's per-manager
/// declared-detection arms (doctor.rs lines 80-115).
const ALL_MANAGERS_PROFILE_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  packages:
    brew:
      taps: ["homebrew/cask"]
      formulae: ["ripgrep"]
      casks: ["alacritty"]
    apt:
      packages: ["curl"]
    cargo:
      packages: ["bat"]
    npm:
      global: ["typescript"]
    snap:
      packages: ["code"]
    flatpak:
      packages: ["org.gimp.GIMP"]
    pipx: ["black"]
    dnf: ["htop"]
    apk: ["busybox"]
    pacman: ["fish"]
    zypper: ["jq"]
    yum: ["ncdu"]
    pkg: ["tmux"]
    nix: ["nix-tree"]
    go: ["github.com/charmbracelet/glow"]
    winget: ["Microsoft.PowerToys"]
    chocolatey: ["7zip"]
    scoop: ["nvm"]
"#;

#[test]
fn cmd_doctor_declares_every_supported_package_manager() {
    // Drives every arm of doctor.rs's declared_managers detection, including
    // the simple-list managers iterated via PackagesSpec::non_empty_simple_lists.
    let h = CliTestHarness::builder()
        .profile("default", ALL_MANAGERS_PROFILE_YAML)
        .json()
        .build();
    super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();

    let parsed = h.json_output();
    let managers = parsed["packageManagers"]
        .as_array()
        .expect("packageManagers should be array");

    // Per-manager: each declared name must appear with declared=true.
    let names_declared: Vec<&str> = managers
        .iter()
        .filter(|m| m["declared"] == true)
        .filter_map(|m| m["name"].as_str())
        .collect();

    for expected in &[
        "brew",
        "apt",
        "cargo",
        "npm",
        "snap",
        "flatpak",
        "pipx",
        "dnf",
        "apk",
        "pacman",
        "zypper",
        "yum",
        "pkg",
        "nix",
        "go",
        "winget",
        "chocolatey",
        "scoop",
    ] {
        assert!(
            names_declared.iter().any(|n| n == expected),
            "manager '{expected}' should be declared but isn't in: {names_declared:?}"
        );
    }
    // brew-tap / brew-cask are deduplicated under "brew" — they must NOT
    // appear as separate entries in the output.
    let raw_names: Vec<&str> = managers.iter().filter_map(|m| m["name"].as_str()).collect();
    assert!(
        !raw_names.contains(&"brew-tap"),
        "brew-tap should be deduplicated under brew, got: {raw_names:?}"
    );
    assert!(
        !raw_names.contains(&"brew-cask"),
        "brew-cask should be deduplicated under brew, got: {raw_names:?}"
    );
}

#[test]
fn cmd_doctor_shows_config_sources_section_when_sources_declared() {
    // RICH_CONFIG_YAML carries `spec.sources` declaring a single Git source
    // pointing at a remote URL that's NEVER been cached by this test harness
    // — so the "Config Sources" section should render with the "not cached"
    // warning arm (doctor.rs lines 415-439).
    let h = CliTestHarness::builder().rich_config().build();
    super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();

    let output = h.output();
    assert!(
        output.contains("Config Sources"),
        "should render Config Sources subheader: {output}"
    );
    assert!(
        output.contains("team-config"),
        "should name the source declared in cfgd.yaml: {output}"
    );
    assert!(
        output.contains("not cached") && output.contains("cfgd source update"),
        "should point uncached source at the source-update remediation: {output}"
    );
}

#[test]
fn cmd_doctor_json_with_missing_module_shows_error() {
    let profile_with_missing_module = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  modules:
    - ghost-mod
  packages:
    cargo:
      - bat
"#;
    let h = CliTestHarness::builder()
        .profile("default", profile_with_missing_module)
        .json()
        .build();

    super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();

    let parsed = h.json_output();
    let modules = parsed["modules"]
        .as_array()
        .expect("modules should be array");
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0]["name"], "ghost-mod");
    assert_eq!(modules[0]["valid"], false);
    assert!(
        modules[0]["error"].is_string(),
        "error should be set for missing module"
    );
}

fn base_doctor_output() -> super::output_types::DoctorOutput {
    super::output_types::DoctorOutput {
        config: super::output_types::DoctorConfigCheck {
            valid: true,
            path: "/etc/cfgd.yaml".into(),
            name: Some("mybox".into()),
            profile: Some("default".into()),
            error: None,
            state: super::output_types::DoctorConfigState::Valid,
        },
        git: true,
        secrets: super::output_types::DoctorSecretsCheck {
            sops_available: true,
            sops_version: Some("3.8.0".into()),
            age_key_exists: true,
            age_key_path: Some("/home/user/.config/cfgd/keys/age.key".into()),
            sops_config_exists: true,
            sops_config_path: Some("/etc/.sops.yaml".into()),
            providers: vec![],
        },
        package_managers: vec![],
        modules: vec![],
        system_configurators: vec![],
        profiles: vec![],
    }
}

fn emit_doc(
    output: &super::output_types::DoctorOutput,
    extras: &super::doctor::DoctorExtras,
) -> String {
    let (printer, cap) = cfgd_core::output::Printer::for_test_doc();
    let doc = super::doctor::build_doctor_doc(output, extras);
    printer.emit(doc);
    drop(printer);
    cap.human()
}

#[test]
fn build_doctor_doc_config_invalid_parse_error_emits_fail() {
    let mut output = base_doctor_output();
    output.config.valid = false;
    output.config.error = Some("unexpected key 'x'".into());
    output.config.state = super::output_types::DoctorConfigState::Invalid;
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("Config file") && text.contains("unexpected key"),
        "should show parse error, got: {text}"
    );
    assert!(
        text.contains("Some checks failed"),
        "all_passed should be false, got: {text}"
    );
}

#[test]
fn build_doctor_doc_config_invalid_no_error_field_emits_fail() {
    let mut output = base_doctor_output();
    output.config.valid = false;
    output.config.error = None;
    output.config.state = super::output_types::DoctorConfigState::Invalid;
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("Config file: /etc/cfgd.yaml — invalid"),
        "should show the invalid fallback, got: {text}"
    );
}

#[test]
fn build_doctor_doc_git_missing_emits_fail_status() {
    let mut output = base_doctor_output();
    output.git = false;
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("git: not found"),
        "should mention git missing, got: {text}"
    );
    assert!(
        text.contains("Some checks failed"),
        "all_passed should be false when git missing, got: {text}"
    );
}

#[test]
fn build_doctor_doc_sops_missing_emits_warn() {
    let mut output = base_doctor_output();
    output.secrets.sops_available = false;
    output.secrets.sops_version = None;
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("sops: not found"),
        "should warn about missing sops, got: {text}"
    );
}

#[test]
fn build_doctor_doc_age_key_missing_with_path_emits_warn() {
    let mut output = base_doctor_output();
    output.secrets.age_key_exists = false;
    output.secrets.age_key_path = Some("/home/user/.config/cfgd/keys/age.key".into());
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("age key: not found at") && text.contains("cfgd init"),
        "should warn about missing age key and suggest cfgd init, got: {text}"
    );
}

#[test]
fn build_doctor_doc_sops_config_present_no_path_emits_ok() {
    let mut output = base_doctor_output();
    output.secrets.sops_config_exists = true;
    output.secrets.sops_config_path = None;
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains(".sops.yaml: present"),
        "should show '.sops.yaml: present' when path is None, got: {text}"
    );
}

#[test]
fn build_doctor_doc_sops_config_missing_emits_warn() {
    let mut output = base_doctor_output();
    output.secrets.sops_config_exists = false;
    output.secrets.sops_config_path = None;
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains(".sops.yaml: not found"),
        "should warn about missing .sops.yaml, got: {text}"
    );
}

#[test]
fn build_doctor_doc_provider_unavailable_emits_info() {
    let mut output = base_doctor_output();
    output.secrets.providers = vec![super::output_types::DoctorProviderCheck {
        name: "1password".into(),
        available: false,
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("1password") && text.contains("not installed"),
        "should show unavailable provider as info, got: {text}"
    );
}

#[test]
fn build_doctor_doc_manager_declared_unavailable_can_bootstrap_emits_warn() {
    let mut output = base_doctor_output();
    output.package_managers = vec![super::output_types::DoctorManagerCheck {
        name: "brew".into(),
        available: false,
        declared: true,
        can_bootstrap: true,
        bootstrap_method: Some("curl".into()),
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("brew") && text.contains("not found"),
        "should show brew not found, got: {text}"
    );
    assert!(
        text.contains("auto-bootstrap") && text.contains("curl"),
        "should mention auto-bootstrap method, got: {text}"
    );
}

#[test]
fn build_doctor_doc_manager_declared_unavailable_no_bootstrap_emits_fail() {
    let mut output = base_doctor_output();
    output.package_managers = vec![super::output_types::DoctorManagerCheck {
        name: "apt".into(),
        available: false,
        declared: true,
        can_bootstrap: false,
        bootstrap_method: None,
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("apt") && text.contains("not found") && text.contains("declared in config"),
        "should show apt declared but not available fail, got: {text}"
    );
    assert!(
        text.contains("Some checks failed"),
        "all_passed should be false when declared manager unavailable, got: {text}"
    );
}

#[test]
fn build_doctor_doc_manager_undeclared_unavailable_emits_nothing_for_that_entry() {
    let mut output = base_doctor_output();
    output.package_managers = vec![super::output_types::DoctorManagerCheck {
        name: "winget".into(),
        available: false,
        declared: false,
        can_bootstrap: false,
        bootstrap_method: None,
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        !text.contains("winget"),
        "undeclared+unavailable manager should not appear in output, got: {text}"
    );
}

#[test]
fn build_doctor_doc_module_invalid_emits_fail_with_detail() {
    let mut output = base_doctor_output();
    output.modules = vec![super::output_types::DoctorModuleCheck {
        name: "broken-mod".into(),
        valid: false,
        error: Some("YAML parse error".into()),
        packages: vec![],
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("broken-mod") && text.contains("YAML parse error"),
        "should show module error detail, got: {text}"
    );
    assert!(
        text.contains("Some checks failed"),
        "all_passed should be false for invalid module, got: {text}"
    );
}

#[test]
fn build_doctor_doc_module_valid_no_packages_emits_ok() {
    let mut output = base_doctor_output();
    output.modules = vec![super::output_types::DoctorModuleCheck {
        name: "empty-mod".into(),
        valid: true,
        error: None,
        packages: vec![],
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("empty-mod"),
        "should list module with no packages, got: {text}"
    );
}

#[test]
fn build_doctor_doc_module_package_with_error_emits_fail() {
    let mut output = base_doctor_output();
    output.modules = vec![super::output_types::DoctorModuleCheck {
        name: "mod-a".into(),
        valid: true,
        error: None,
        packages: vec![super::output_types::DoctorModulePackageCheck {
            name: "ripgrep".into(),
            resolved_name: "ripgrep".into(),
            manager: "cargo".into(),
            installed: false,
            version: None,
            skip_reason: None,
            error: Some("resolver error".into()),
        }],
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("ripgrep") && text.contains("resolver error"),
        "should show package error detail, got: {text}"
    );
}

#[test]
fn build_doctor_doc_module_package_skipped_emits_info() {
    let mut output = base_doctor_output();
    output.modules = vec![super::output_types::DoctorModuleCheck {
        name: "mod-b".into(),
        valid: true,
        error: None,
        packages: vec![super::output_types::DoctorModulePackageCheck {
            name: "brew-only".into(),
            resolved_name: "brew-only".into(),
            manager: String::new(),
            installed: false,
            version: None,
            skip_reason: Some("platform".into()),
            error: None,
        }],
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("brew-only") && text.contains("skipped") && text.contains("platform"),
        "should show platform-skipped package as info, got: {text}"
    );
}

#[test]
fn build_doctor_doc_module_package_not_installed_emits_fail() {
    let mut output = base_doctor_output();
    output.modules = vec![super::output_types::DoctorModuleCheck {
        name: "mod-c".into(),
        valid: true,
        error: None,
        packages: vec![super::output_types::DoctorModulePackageCheck {
            name: "fd".into(),
            resolved_name: "fd-find".into(),
            manager: "apt".into(),
            installed: false,
            version: None,
            skip_reason: None,
            error: None,
        }],
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("fd") && text.contains("not installed"),
        "should show not-installed package as fail, got: {text}"
    );
    assert!(
        text.contains("Some checks failed"),
        "all_passed should be false for uninstalled package, got: {text}"
    );
}

#[test]
fn build_doctor_doc_module_package_installed_with_version_emits_ok() {
    let mut output = base_doctor_output();
    output.modules = vec![super::output_types::DoctorModuleCheck {
        name: "mod-d".into(),
        valid: true,
        error: None,
        packages: vec![super::output_types::DoctorModulePackageCheck {
            name: "bat".into(),
            resolved_name: "bat".into(),
            manager: "cargo".into(),
            installed: true,
            version: Some("0.24.0".into()),
            skip_reason: None,
            error: None,
        }],
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("bat") && text.contains("0.24.0") && text.contains("cargo"),
        "should show installed package with version and manager, got: {text}"
    );
    assert!(
        text.contains("All checks passed"),
        "all_passed should be true when package is installed, got: {text}"
    );
}

#[test]
fn build_doctor_doc_system_state_store_inaccessible_emits_warn() {
    let output = base_doctor_output();
    let extras = super::doctor::DoctorExtras {
        state_store: Some(super::doctor::DoctorStateStore {
            accessible: false,
            message: Some("database locked".into()),
        }),
        profiles_dir: None,
        config_sources: vec![],
    };
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("State store: unavailable") && text.contains("database locked"),
        "should warn about inaccessible state store with message, got: {text}"
    );
}

#[test]
fn build_doctor_doc_system_profiles_dir_missing_emits_warn() {
    let output = base_doctor_output();
    let extras = super::doctor::DoctorExtras {
        state_store: None,
        profiles_dir: Some(super::doctor::DoctorProfilesDir {
            path: "/etc/cfgd/profiles".into(),
            exists: false,
            profile_count: 0,
            error: None,
        }),
        config_sources: vec![],
    };
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("Profiles directory not found") && text.contains("/etc/cfgd/profiles"),
        "should warn about missing profiles directory, got: {text}"
    );
}

#[test]
fn build_doctor_doc_source_cached_emits_ok() {
    let output = base_doctor_output();
    let extras = super::doctor::DoctorExtras {
        state_store: None,
        profiles_dir: None,
        config_sources: vec![super::doctor::DoctorConfigSource {
            name: "team-config".into(),
            cached_path: Some("/home/user/.cache/cfgd/sources/team-config".into()),
        }],
    };
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("team-config") && text.contains("cached at"),
        "should show cached source path, got: {text}"
    );
}

#[test]
fn build_doctor_doc_all_passed_true_when_everything_ok() {
    let output = base_doctor_output();
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("All checks passed"),
        "should show all-passed when output is clean, got: {text}"
    );
}

#[test]
fn build_doctor_doc_all_passed_false_when_config_invalid() {
    let mut output = base_doctor_output();
    output.config.valid = false;
    output.config.error = Some("yaml parse error: unexpected token".into());
    output.config.state = super::output_types::DoctorConfigState::Invalid;
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("Some checks failed"),
        "a present-but-unparseable config must fail the verdict, got: {text}"
    );
}

#[test]
fn build_doctor_doc_missing_config_does_not_fail_verdict() {
    // A missing config is a fresh-machine Warn, not a failure — the verdict
    // (and thus the process exit) must stay green.
    let mut output = base_doctor_output();
    output.config.valid = false;
    output.config.error = Some("not found".into());
    output.config.state = super::output_types::DoctorConfigState::MissingAtDefault;
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("All checks passed"),
        "missing config must not fail the doctor verdict, got: {text}"
    );
}

#[test]
fn build_doctor_doc_missing_config_at_explicit_path_fails_verdict() {
    // The same "not found" payload flips the verdict when the path was
    // user-supplied: doctor must stop `doctor && apply` on a --config typo.
    let mut output = base_doctor_output();
    output.config.valid = false;
    output.config.error = Some("not found".into());
    output.config.state = super::output_types::DoctorConfigState::MissingAtExplicit;
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("Some checks failed"),
        "missing config at an explicit path must fail the verdict, got: {text}"
    );
    assert!(
        text.contains("Config file: /etc/cfgd.yaml — not found"),
        "Fail line should name the path, got: {text}"
    );
}

#[test]
fn build_doctor_doc_provider_available_emits_ok() {
    let mut output = base_doctor_output();
    output.secrets.providers = vec![super::output_types::DoctorProviderCheck {
        name: "bitwarden".into(),
        available: true,
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("bitwarden") && text.contains("available"),
        "should show available provider as ok, got: {text}"
    );
}

#[test]
fn build_doctor_doc_manager_can_bootstrap_no_method_emits_generic_hint() {
    let mut output = base_doctor_output();
    output.package_managers = vec![super::output_types::DoctorManagerCheck {
        name: "nix".into(),
        available: false,
        declared: true,
        can_bootstrap: true,
        bootstrap_method: None,
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("nix") && text.contains("auto-bootstrap"),
        "should show generic auto-bootstrap hint when method is None, got: {text}"
    );
}

const MODULE_WITH_PACKAGES_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: tools-mod
spec:
  packages:
    - name: bat
      prefer: [cargo]
    - name: fd
      platforms: [macos]
"#;

#[test]
fn cmd_doctor_with_module_with_packages_exercises_resolution_loop() {
    let profile_yaml = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  modules:
    - tools-mod
"#;
    let h = CliTestHarness::builder()
        .profile("default", profile_yaml)
        .module("tools-mod", MODULE_WITH_PACKAGES_YAML)
        .build();

    super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();

    let output = h.output();
    assert!(
        output.contains("tools-mod"),
        "should list tools-mod in Modules section, got: {output}"
    );
}

const CUSTOM_PKG_CONFIG_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: custom-box
spec:
  profile: default
"#;

const CUSTOM_PKG_PROFILE_YAML: &str = r#"apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: default
spec:
  packages:
    custom:
      - name: my-tool
        packages:
          - my-pkg
"#;

#[test]
fn cmd_doctor_with_custom_package_manager_declared_exercises_custom_branch() {
    let h = CliTestHarness::builder()
        .config(CUSTOM_PKG_CONFIG_YAML)
        .profile("default", CUSTOM_PKG_PROFILE_YAML)
        .build();

    super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();

    let output = h.output();
    assert!(
        output.contains("Doctor"),
        "should show Doctor header with custom package manager, got: {output}"
    );
}

// -----------------------------------------------------------------------
// cmd_decide — exercise decision resolution logic with state verification
// -----------------------------------------------------------------------

#[test]
fn cmd_decide_no_args_no_pending_shows_info() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    // With no resource, no source, and all=false, should show pending list
    super::decide::cmd_decide(
        &printer,
        super::DecideAction::Accept,
        None,
        None,
        false,
        Some(state_dir.path()),
    )
    .unwrap();
    drop(printer);

    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("No pending decisions"),
        "should report no pending decisions, got: {output}"
    );
}

#[test]
fn cmd_decide_no_args_with_pending_shows_list() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let state = super::open_state_store(Some(state_dir.path())).unwrap();

    state
        .upsert_pending_decision("alpha", "pkg/git", "required", "install", "Install git")
        .unwrap();
    state
        .upsert_pending_decision("beta", "env/EDITOR", "recommended", "set", "Set EDITOR")
        .unwrap();

    // No resource/source/all — should display pending decisions
    super::decide::cmd_decide(
        &printer,
        super::DecideAction::Accept,
        None,
        None,
        false,
        Some(state_dir.path()),
    )
    .unwrap();
    drop(printer);

    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("Pending Decisions"),
        "should show Pending Decisions header, got: {output}"
    );
    assert!(
        output.contains("alpha"),
        "should list alpha source, got: {output}"
    );
    assert!(
        output.contains("beta"),
        "should list beta source, got: {output}"
    );
    assert!(
        output.contains("pkg/git"),
        "should list pkg/git resource, got: {output}"
    );
    assert!(
        output.contains("env/EDITOR"),
        "should list env/EDITOR resource, got: {output}"
    );
    // Usage hint
    assert!(
        output.contains("cfgd decide accept"),
        "should show usage hint, got: {output}"
    );

    // Decisions should still be pending (not resolved by just viewing)
    let pending = state.pending_decisions().unwrap();
    assert_eq!(pending.len(), 2, "viewing should not resolve decisions");
}

#[test]
fn cmd_decide_reject_specific_resource_verifies_resolution() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let state = super::open_state_store(Some(state_dir.path())).unwrap();

    state
        .upsert_pending_decision(
            "team",
            "packages.brew.jq",
            "recommended",
            "install",
            "Install jq",
        )
        .unwrap();

    super::decide::cmd_decide(
        &printer,
        super::DecideAction::Reject,
        Some("packages.brew.jq"),
        None,
        false,
        Some(state_dir.path()),
    )
    .unwrap();
    drop(printer);

    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("REJECTED"),
        "should confirm rejection, got: {output}"
    );
    assert!(
        output.contains("packages.brew.jq"),
        "should mention resource name, got: {output}"
    );
    assert!(
        output.contains("not be applied"),
        "rejected resource should mention 'not be applied', got: {output}"
    );

    let pending = state.pending_decisions().unwrap();
    assert!(pending.is_empty(), "decision should be resolved");
}

#[test]
fn cmd_decide_accept_specific_resource_verifies_messaging() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let state = super::open_state_store(Some(state_dir.path())).unwrap();

    state
        .upsert_pending_decision("team", "file/bashrc", "required", "create", "Create bashrc")
        .unwrap();

    super::decide::cmd_decide(
        &printer,
        super::DecideAction::Accept,
        Some("file/bashrc"),
        None,
        false,
        Some(state_dir.path()),
    )
    .unwrap();
    drop(printer);

    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("ACCEPTED"),
        "should confirm acceptance, got: {output}"
    );
    assert!(
        output.contains("file/bashrc"),
        "should mention resource name, got: {output}"
    );
    assert!(
        output.contains("be applied"),
        "accepted resource should mention 'be applied', got: {output}"
    );
}

#[test]
fn cmd_decide_accept_nonexistent_resource_warns() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    super::decide::cmd_decide(
        &printer,
        super::DecideAction::Accept,
        Some("no.such.resource"),
        None,
        false,
        Some(state_dir.path()),
    )
    .unwrap();
    drop(printer);

    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("No pending decision found"),
        "should warn about nonexistent resource, got: {output}"
    );
    assert!(
        output.contains("no.such.resource"),
        "should mention the resource name, got: {output}"
    );
}

#[test]
fn cmd_decide_accept_all_reports_count() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let state = super::open_state_store(Some(state_dir.path())).unwrap();

    for i in 0..3 {
        state
            .upsert_pending_decision(
                "src",
                &format!("pkg/{i}"),
                "recommended",
                "install",
                &format!("Install pkg {i}"),
            )
            .unwrap();
    }

    super::decide::cmd_decide(
        &printer,
        super::DecideAction::Accept,
        None,
        None,
        true,
        Some(state_dir.path()),
    )
    .unwrap();
    drop(printer);

    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("ACCEPTED"),
        "should confirm acceptance, got: {output}"
    );
    assert!(
        output.contains("3 items"),
        "should report count of 3 items, got: {output}"
    );
    assert!(
        output.contains("next reconcile"),
        "should mention next reconcile, got: {output}"
    );

    let pending = state.pending_decisions().unwrap();
    assert!(pending.is_empty(), "all decisions should be resolved");
}

#[test]
fn cmd_decide_reject_by_source_preserves_other_sources() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let state = super::open_state_store(Some(state_dir.path())).unwrap();

    state
        .upsert_pending_decision("alpha", "pkg/a", "recommended", "install", "A")
        .unwrap();
    state
        .upsert_pending_decision("alpha", "pkg/b", "recommended", "install", "B")
        .unwrap();
    state
        .upsert_pending_decision("beta", "env/X", "required", "set", "X")
        .unwrap();

    super::decide::cmd_decide(
        &printer,
        super::DecideAction::Reject,
        None,
        Some("alpha"),
        false,
        Some(state_dir.path()),
    )
    .unwrap();
    drop(printer);

    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("REJECTED"),
        "should confirm rejection, got: {output}"
    );
    assert!(
        output.contains("2 items"),
        "should report 2 items rejected from alpha, got: {output}"
    );
    assert!(
        output.contains("alpha"),
        "should mention source name, got: {output}"
    );

    // Only beta's decision should remain
    let pending = state.pending_decisions().unwrap();
    assert_eq!(pending.len(), 1, "only beta's decision should remain");
    assert_eq!(pending[0].source, "beta");
    assert_eq!(pending[0].resource, "env/X");
}

#[test]
fn cmd_decide_reject_by_source_with_no_matching_decisions() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let state = super::open_state_store(Some(state_dir.path())).unwrap();

    state
        .upsert_pending_decision("alpha", "pkg/a", "recommended", "install", "A")
        .unwrap();

    super::decide::cmd_decide(
        &printer,
        super::DecideAction::Reject,
        None,
        Some("nonexistent-source"),
        false,
        Some(state_dir.path()),
    )
    .unwrap();
    drop(printer);

    let output = buf.lock().unwrap().clone();
    assert!(
        output.contains("No pending decisions for source"),
        "should report no decisions for this source, got: {output}"
    );

    // Alpha's decision should be untouched
    let pending = state.pending_decisions().unwrap();
    assert_eq!(pending.len(), 1);
}

#[test]
fn cmd_decide_accept_single_item_singular_message() {
    let state_dir = tempfile::tempdir().unwrap();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let state = super::open_state_store(Some(state_dir.path())).unwrap();

    state
        .upsert_pending_decision("src", "pkg/only", "recommended", "install", "Only pkg")
        .unwrap();

    super::decide::cmd_decide(
        &printer,
        super::DecideAction::Accept,
        None,
        None,
        true,
        Some(state_dir.path()),
    )
    .unwrap();
    drop(printer);

    let output = buf.lock().unwrap().clone();
    // When exactly 1 item, the message should use singular "item" not "items"
    assert!(
        output.contains("1 item"),
        "should report singular '1 item', got: {output}"
    );
    assert!(
        !output.contains("1 items"),
        "should NOT use plural '1 items', got: {output}"
    );
}

// -----------------------------------------------------------------------
// Coverage: source::cmd_source_update error display path
//
// The all-sources-fail path calls `cfgd_core::exit::ExitCode::Error.exit()`
// (process::exit), so it cannot be exercised in-process — terminating the
// test binary would abort the whole run. The exit code + per-source failure
// output are covered by the subprocess tests `source_update_*_exits_1` in
// `tests/cli_integration.rs` instead.
// -----------------------------------------------------------------------

// -----------------------------------------------------------------------
// Coverage: source::cmd_source_replace — replace removes old and adds new
// -----------------------------------------------------------------------

#[test]
fn cmd_source_replace_existing_source() {
    // Set up a config with a source, then replace it.
    // The replacement will fail at the "add" step (can't clone the new URL),
    // but the remove step will succeed, exercising the replace flow.
    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: old-source
      origin:
        url: https://github.com/old/config
        branch: main
        type: Git
      subscription:
        priority: 400
"#;
    let h = CliTestHarness::builder().config(config_with_source).build();
    // Replace will display the header and remove old source, then fail on clone
    let result = super::source::cmd_source_replace(
        &h.cli(),
        h.printer(),
        "old-source",
        "file:///nonexistent/new-config.git",
    );
    // The header should be printed regardless of success/failure
    h.assert_output_contains("Replace Source: old-source");
    h.assert_output_contains("Remove Source: old-source");
    // The add step will fail (can't clone), so the overall result should be Err
    assert!(
        result.is_err(),
        "replace should fail when new source URL is unreachable"
    );
    // Verify old source was removed from config
    let cfg = config::load_config(&h.config_path().join("cfgd.yaml")).unwrap();
    assert!(
        !cfg.spec.sources.iter().any(|s| s.name == "old-source"),
        "old source should have been removed from config"
    );
}

// -----------------------------------------------------------------------
// Coverage: source::cmd_source_priority — view displays key_value fields
// -----------------------------------------------------------------------

#[test]
fn cmd_source_priority_view_displays_fields() {
    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: team-src
      origin:
        url: https://github.com/team/config
        branch: main
        type: Git
      subscription:
        priority: 750
"#;
    let h = CliTestHarness::builder().config(config_with_source).build();
    super::source::cmd_source_priority(&h.cli(), h.printer(), "team-src", None).unwrap();
    // View mode should display source name and priority value
    h.assert_output_contains("team-src");
    h.assert_output_contains("750");
    h.assert_output_contains("Local config priority is 1000");
}

#[test]
fn cmd_source_priority_update_displays_change() {
    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: team-src
      origin:
        url: https://github.com/team/config
        branch: main
        type: Git
      subscription:
        priority: 750
"#;
    let h = CliTestHarness::builder().config(config_with_source).build();
    super::source::cmd_source_priority(&h.cli(), h.printer(), "team-src", Some(100)).unwrap();
    // Should display the old->new priority change
    h.assert_output_contains("priority updated: 750 -> 100");
    // Verify the file was actually updated
    let cfg = config::load_config(&h.config_path().join("cfgd.yaml")).unwrap();
    let source = cfg
        .spec
        .sources
        .iter()
        .find(|s| s.name == "team-src")
        .unwrap();
    assert_eq!(source.subscription.priority, 100);
}

// -----------------------------------------------------------------------
// Coverage: cmd_checkin — server unreachable exercises config loading,
// hash computation, and server client construction paths
// -----------------------------------------------------------------------

#[test]
fn cmd_checkin_server_unreachable() {
    let h = CliTestHarness::builder().build();
    let printer = test_printer();
    let result = super::checkin::cmd_checkin(
        &h.cli(),
        &printer,
        "http://127.0.0.1:19999",
        Some("test-api-key"),
        Some("test-device-42"),
    );
    // The call should fail because the server is unreachable
    assert!(
        result.is_err(),
        "checkin should fail with unreachable server"
    );
    let err = result.unwrap_err();
    let chain = format!("{err:#}");
    assert!(
        chain.contains("checkin to gateway failed")
            || chain.contains("Connection refused")
            || chain.contains("connection"),
        "error chain should mention connection failure, got: {chain}"
    );
}

#[test]
fn cmd_checkin_with_compliance_config_server_unreachable() {
    // Config with compliance enabled — exercises the compliance snapshot
    // collection branch before the checkin HTTP call fails.
    let config_with_compliance = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  compliance:
    enabled: true
    scope:
      packages: true
      files: true
"#;
    let h = CliTestHarness::builder()
        .config(config_with_compliance)
        .build();
    let result =
        super::checkin::cmd_checkin(&h.cli(), h.printer(), "http://127.0.0.1:19999", None, None);
    assert!(
        result.is_err(),
        "checkin should fail with unreachable server"
    );
    // The compliance snapshot collection path was exercised before the failure
    let output = h.output();
    assert!(
        output.contains("Compliance:") || output.contains("Checking in"),
        "should show compliance or checkin info before failing, got: {output}"
    );
}

// -----------------------------------------------------------------------
// Coverage: cmd_compliance_export — snapshot stored then exported to file
// -----------------------------------------------------------------------

#[test]
fn cmd_compliance_export_writes_file_and_displays_path() {
    let h = CliTestHarness::builder().build();
    super::compliance::cmd_compliance_export(&h.cli(), h.printer()).unwrap();
    let output = h.output();
    // export writes a file and prints the path in a success message
    assert!(
        output.contains("Compliance snapshot written to"),
        "should confirm file was written, got: {output}"
    );
    // The output should also include the export heading
    assert!(
        output.contains("Compliance Export"),
        "should display the compliance export heading, got: {output}"
    );
}

#[test]
fn cmd_compliance_export_json_returns_snapshot_object() {
    let h = CliTestHarness::builder().json().build();
    super::compliance::cmd_compliance_export(&h.cli(), h.printer()).unwrap();
    let parsed = h.json_output();
    // Structured output should contain the snapshot wrapper
    assert_json_has_fields(&parsed, &["snapshot"]);
    let snapshot = &parsed["snapshot"];
    assert_json_has_fields(snapshot, &["timestamp", "profile", "summary"]);
}

// -----------------------------------------------------------------------
// Coverage: cmd_sync — displays pull result and source sync header
// -----------------------------------------------------------------------

#[test]
fn cmd_sync_non_git_shows_pull_warning_and_sync_header() {
    // A tempdir is not a git repo, so git_pull_sync will fail with a
    // warning. The test verifies both the header and the pull-failure warning path.
    let h = CliTestHarness::builder().build();
    super::sync::cmd_sync(&h.cli(), h.printer()).unwrap();
    h.assert_header("Sync");
    let output = h.output();
    // Spinner section appears with the pulling message; final state is "Pull
    // failed" on a non-git dir.
    assert!(
        output.contains("Local repo"),
        "missing 'Local repo' section: {output}"
    );
    assert!(
        output.contains("Pull failed"),
        "missing pull failure status: {output}"
    );
}

#[test]
fn cmd_sync_with_sources_shows_source_section() {
    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: team-config
      origin:
        url: file:///nonexistent/repo.git
        branch: main
        type: Git
      subscription:
        priority: 100
"#;
    let h = CliTestHarness::builder().config(config_with_source).build();
    super::sync::cmd_sync(&h.cli(), h.printer()).unwrap();
    h.assert_header("Sync");
    // When sources are configured, the Sources subheader should appear
    h.assert_output_contains("Sources");
    // The source sync will fail because the URL is non-existent — spinner
    // finishes with finish_fail("Failed to sync ...").
    h.assert_output_contains("Failed to sync 'team-config'");
}

// -----------------------------------------------------------------------
// Coverage: Command dispatch match arms via execute()
// -----------------------------------------------------------------------

#[test]
fn execute_dispatch_checkin() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Checkin {
        server_url: "http://127.0.0.1:19999".to_string(),
        api_key: None,
        device_id: Some("test-device".to_string()),
    });
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    // Checkin fails because server is unreachable, but dispatch arm was exercised
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("checkin") || err_msg.contains("Connection"),
        "dispatch should route to cmd_checkin, got error: {err_msg}"
    );
}

#[test]
fn execute_dispatch_source_update() {
    // No sources: proves execute() routes Source/Update to the update handler.
    // A failing source would `process::exit(1)` and abort the test binary; that
    // failure-exit wiring is covered by the subprocess test
    // `source_update_all_failed_exits_1` in tests/cli_integration.rs.
    let config_no_sources = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
"#;
    let h = CliTestHarness::builder().config(config_no_sources).build();
    let cli = h.cli_with_command(Command::Source {
        command: SourceCommand::Update { name: None },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    h.assert_header("Update Sources");
    h.assert_output_contains("No sources configured");
}

#[test]
fn execute_dispatch_source_priority() {
    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: my-source
      origin:
        url: https://github.com/org/config
        branch: main
        type: Git
      subscription:
        priority: 500
"#;
    let h = CliTestHarness::builder().config(config_with_source).build();
    let cli = h.cli_with_command(Command::Source {
        command: SourceCommand::Priority {
            name: "my-source".to_string(),
            value: None,
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    h.assert_output_contains("my-source");
    h.assert_output_contains("500");
}

#[test]
fn execute_dispatch_source_replace() {
    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: replaceable
      origin:
        url: https://github.com/old/config
        branch: main
        type: Git
      subscription:
        priority: 600
"#;
    let h = CliTestHarness::builder().config(config_with_source).build();
    let cli = h.cli_with_command(Command::Source {
        command: SourceCommand::Replace {
            old_name: "replaceable".to_string(),
            new_url: "file:///nonexistent/new.git".to_string(),
        },
    });
    // Dispatches through execute -> source::cmd_source_replace
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    // Replace will fail on the add step, but dispatch arm is exercised
    assert!(result.is_err());
    h.assert_output_contains("Replace Source: replaceable");
}

#[test]
fn execute_dispatch_compliance_export() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Compliance {
        command: Some(ComplianceCommand::Export),
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default()).unwrap();
    h.assert_output_contains("Compliance snapshot written to");
}

// ============================================================================
// count_policy_items — per-package-kind and per-resource-kind counting
// ============================================================================

#[test]
fn count_policy_items_counts_brew_formulae_casks_and_taps_each_as_one() {
    let items = cfgd_core::config::PolicyItems {
        packages: Some(cfgd_core::config::PackagesSpec {
            brew: Some(cfgd_core::config::BrewSpec {
                taps: vec!["org/tap".to_string()],
                formulae: vec!["ripgrep".to_string(), "fd".to_string()],
                casks: vec!["firefox".to_string()],
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    // 1 tap + 2 formulae + 1 cask = 4
    assert_eq!(super::count_policy_items(&items), 4);
}

#[test]
fn count_policy_items_counts_apt_and_cargo_packages() {
    let items = cfgd_core::config::PolicyItems {
        packages: Some(cfgd_core::config::PackagesSpec {
            apt: Some(cfgd_core::config::AptSpec {
                packages: vec!["curl".to_string(), "git".to_string(), "vim".to_string()],
                ..Default::default()
            }),
            cargo: Some(cfgd_core::config::CargoSpec {
                packages: vec!["bat".to_string(), "ripgrep".to_string()],
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(super::count_policy_items(&items), 5);
}

#[test]
fn count_policy_items_counts_pipx_dnf_and_npm_global() {
    let items = cfgd_core::config::PolicyItems {
        packages: Some(cfgd_core::config::PackagesSpec {
            pipx: vec!["black".to_string()],
            dnf: vec!["wireshark".to_string(), "tcpdump".to_string()],
            npm: Some(cfgd_core::config::NpmSpec {
                global: vec!["typescript".to_string()],
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    // 1 + 2 + 1 = 4
    assert_eq!(super::count_policy_items(&items), 4);
}

#[test]
fn count_policy_items_counts_files_env_and_system_independently() {
    use cfgd_core::config::{EnvVar, ManagedFileSpec};
    let mut system = std::collections::HashMap::new();
    system.insert(
        "shell".to_string(),
        serde_yaml::Value::String("bash".to_string()),
    );
    system.insert(
        "systemd".to_string(),
        serde_yaml::Value::String("running".to_string()),
    );
    let items = cfgd_core::config::PolicyItems {
        files: vec![
            ManagedFileSpec {
                source: "src/foo".to_string(),
                target: std::path::PathBuf::from("/etc/foo"),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            },
            ManagedFileSpec {
                source: "src/bar".to_string(),
                target: std::path::PathBuf::from("/etc/bar"),
                strategy: None,
                private: false,
                origin: None,
                encryption: None,
                permissions: None,
            },
        ],
        env: vec![EnvVar {
            name: "FOO".to_string(),
            value: "bar".to_string(),
        }],
        system,
        ..Default::default()
    };
    // 2 files + 1 env + 2 system = 5
    assert_eq!(super::count_policy_items(&items), 5);
}

#[test]
fn count_policy_items_sums_packages_files_env_and_system() {
    // End-to-end mixed bag: every contributing field set at once. Pin the
    // additive contract: no field silently swallows another.
    let mut system = std::collections::HashMap::new();
    system.insert(
        "shell".to_string(),
        serde_yaml::Value::String("bash".to_string()),
    );
    let items = cfgd_core::config::PolicyItems {
        packages: Some(cfgd_core::config::PackagesSpec {
            brew: Some(cfgd_core::config::BrewSpec {
                formulae: vec!["ripgrep".to_string()],
                ..Default::default()
            }),
            pipx: vec!["black".to_string()],
            ..Default::default()
        }),
        files: vec![cfgd_core::config::ManagedFileSpec {
            source: "src/foo".to_string(),
            target: std::path::PathBuf::from("/etc/foo"),
            strategy: None,
            private: false,
            origin: None,
            encryption: None,
            permissions: None,
        }],
        env: vec![cfgd_core::config::EnvVar {
            name: "X".to_string(),
            value: "1".to_string(),
        }],
        system,
        ..Default::default()
    };
    // 1 brew + 1 pipx + 1 file + 1 env + 1 system = 5
    assert_eq!(super::count_policy_items(&items), 5);
}

#[test]
fn count_policy_items_packages_none_does_not_panic() {
    // policy.packages is Option<_>; when None, the helper must not panic
    // and must still count items.{files,env,system}.
    let items = cfgd_core::config::PolicyItems {
        env: vec![cfgd_core::config::EnvVar {
            name: "X".to_string(),
            value: "1".to_string(),
        }],
        ..Default::default()
    };
    assert_eq!(super::count_policy_items(&items), 1);
}

// ===========================================================================
// cmd_source_add end-to-end against a local bare repo (file:// fixture).
//
// CFGD_ALLOW_LOCAL_SOURCES=1 flips off the file:// safety check, so the test
// can stand up an `init_bare` upstream containing a real `cfgd-source.yaml`,
// drive cmd_source_add against it, and verify the cfgd.yaml is mutated +
// state store updated. This walks the orchestration body in
// `cli/source/add.rs` which previously only had error-path coverage.
// ===========================================================================

mod cmd_source_add_local {
    use super::*;
    use cfgd_core::test_helpers::with_test_env_var;
    use serial_test::serial;

    /// Build a bare upstream that contains a single-profile `cfgd-source.yaml`.
    /// Returns the bare repo path.
    fn make_bare_with_manifest(
        scratch: &tempfile::TempDir,
        name: &str,
        version: Option<&str>,
    ) -> std::path::PathBuf {
        let bare = scratch.path().join(format!("{name}-bare.git"));
        let _ = git2::Repository::init_bare(&bare).unwrap();
        let src = scratch.path().join(format!("{name}-src"));
        let src_repo = git2::Repository::init(&src).unwrap();
        let mut manifest = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: {name}\n"
        );
        if let Some(v) = version {
            manifest.push_str(&format!("  version: {v}\n"));
        }
        manifest.push_str("spec:\n  provides:\n    profiles:\n      - default\n");
        std::fs::write(src.join("cfgd-source.yaml"), &manifest).unwrap();
        // Profile dir with default.yaml so source_profiles_dir(name)/default.yaml resolves.
        std::fs::create_dir_all(src.join("profiles")).unwrap();
        std::fs::write(
            src.join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        let mut index = src_repo.index().unwrap();
        index
            .add_path(std::path::Path::new("cfgd-source.yaml"))
            .unwrap();
        index
            .add_path(std::path::Path::new("profiles/default.yaml"))
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let url = cfgd_core::test_helpers::file_url(&bare);
        let mut remote = src_repo.remote("origin", &url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
            .unwrap();
        bare
    }

    /// Like [`make_bare_with_manifest`] but tags the single commit with `tag`
    /// (and pushes the tag) so a `pinVersion` range can resolve against it.
    fn make_bare_with_tag(
        scratch: &tempfile::TempDir,
        name: &str,
        tag: &str,
    ) -> std::path::PathBuf {
        let bare = scratch.path().join(format!("{name}-bare.git"));
        git2::Repository::init_bare(&bare).unwrap();
        let src = scratch.path().join(format!("{name}-src"));
        let src_repo = git2::Repository::init(&src).unwrap();
        let manifest = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: {name}\nspec:\n  provides:\n    profiles:\n      - default\n"
        );
        std::fs::write(src.join("cfgd-source.yaml"), &manifest).unwrap();
        std::fs::create_dir_all(src.join("profiles")).unwrap();
        std::fs::write(
            src.join("profiles").join("default.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n",
        )
        .unwrap();
        let mut index = src_repo.index().unwrap();
        index
            .add_path(std::path::Path::new("cfgd-source.yaml"))
            .unwrap();
        index
            .add_path(std::path::Path::new("profiles/default.yaml"))
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let oid = src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let obj = src_repo.find_object(oid, None).unwrap();
        src_repo.tag_lightweight(tag, &obj, false).unwrap();
        let url = cfgd_core::test_helpers::file_url(&bare);
        let mut remote = src_repo.remote("origin", &url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(
                &[
                    &format!("refs/heads/{branch}:refs/heads/{branch}"),
                    &format!("refs/tags/{tag}:refs/tags/{tag}"),
                ],
                None,
            )
            .unwrap();
        bare
    }

    fn empty_source_args(url: String) -> SourceAddArgs {
        SourceAddArgs {
            url,
            name: None,
            branch: None,
            profile: Some("default".to_string()),
            accept_recommended: false,
            priority: Some(500),
            opt_in: vec![],
            sync_interval: None,
            auto_apply: false,
            pin_version: None,
            yes: true,
        }
    }

    #[test]
    #[serial]
    fn cmd_source_add_against_local_bare_repo_writes_config() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&scratch, "local-team", None);
            let h = CliTestHarness::builder().build();
            let url = cfgd_core::test_helpers::file_url(&bare);
            let args = SourceAddArgs {
                name: Some("local-team".to_string()),
                ..empty_source_args(url.clone())
            };
            let result = super::source::cmd_source_add(&h.cli(), h.printer(), &args);
            assert!(result.is_ok(), "cmd_source_add should succeed: {result:?}");
            // Source row added to cfgd.yaml.
            let cfg_after = std::fs::read_to_string(h.config_path().join("cfgd.yaml")).unwrap();
            assert!(
                cfg_after.contains("local-team"),
                "expected 'local-team' in cfgd.yaml: {cfg_after}"
            );
            assert!(
                cfg_after.contains(&url),
                "expected file:// URL in cfgd.yaml"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_source_add_pin_version_persists_to_config() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            // Pin resolves against git tags now, so the bare must carry a tag.
            let bare = make_bare_with_tag(&scratch, "pinned-src", "v1.2.3");
            let h = CliTestHarness::builder().build();
            let url = cfgd_core::test_helpers::file_url(&bare);
            let args = SourceAddArgs {
                name: Some("pinned-src".to_string()),
                pin_version: Some("~1".to_string()),
                ..empty_source_args(url)
            };
            let result = super::source::cmd_source_add(&h.cli(), h.printer(), &args);
            assert!(result.is_ok(), "cmd_source_add should succeed: {result:?}");
            let cfg_after = std::fs::read_to_string(h.config_path().join("cfgd.yaml")).unwrap();
            assert!(
                cfg_after.contains("pinVersion") || cfg_after.contains("~1"),
                "expected pinVersion field in cfgd.yaml: {cfg_after}"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_source_add_rejects_branch_and_pin_together() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            let bare = make_bare_with_tag(&scratch, "conflict-src", "v1.0.0");
            let h = CliTestHarness::builder().build();
            let url = cfgd_core::test_helpers::file_url(&bare);
            let args = SourceAddArgs {
                name: Some("conflict-src".to_string()),
                branch: Some("main".to_string()),
                pin_version: Some("~1".to_string()),
                ..empty_source_args(url)
            };
            let err = super::source::cmd_source_add(&h.cli(), h.printer(), &args)
                .expect_err("branch + pin should be rejected");
            let msg = err.to_string();
            assert!(
                msg.contains("mutually exclusive") || msg.contains("branch_pin_conflict"),
                "expected branch/pin conflict error, got: {msg}"
            );
            // Must reject before writing config.
            assert!(
                !h.config_path().join("cfgd.yaml").exists()
                    || !std::fs::read_to_string(h.config_path().join("cfgd.yaml"))
                        .unwrap()
                        .contains("conflict-src"),
                "conflicting add must not persist the source"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_source_add_rejects_dash_leading_pin_version() {
        // Argument-injection guard at the CLI boundary: a `-`-leading pin is
        // rejected before any clone, with a clear error.
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            let bare = make_bare_with_tag(&scratch, "dash-pin-src", "v1.0.0");
            let h = CliTestHarness::builder().build();
            let url = cfgd_core::test_helpers::file_url(&bare);
            let args = SourceAddArgs {
                name: Some("dash-pin-src".to_string()),
                pin_version: Some("-x".to_string()),
                ..empty_source_args(url)
            };
            let err = super::source::cmd_source_add(&h.cli(), h.printer(), &args)
                .expect_err("dash-leading pin should be rejected");
            let msg = err.to_string();
            assert!(
                msg.contains("invalid_pin_version") || msg.contains("must not start with '-'"),
                "expected invalid_pin_version error, got: {msg}"
            );
            assert!(
                !h.config_path().join("cfgd.yaml").exists()
                    || !std::fs::read_to_string(h.config_path().join("cfgd.yaml"))
                        .unwrap()
                        .contains("dash-pin-src"),
                "rejected add must not persist the source"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_source_add_records_opt_in_sync_interval_and_auto_apply_in_config() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&scratch, "opt-in-src", None);
            let h = CliTestHarness::builder().build();
            let url = cfgd_core::test_helpers::file_url(&bare);
            let args = SourceAddArgs {
                name: Some("opt-in-src".to_string()),
                opt_in: vec!["app/featureA".to_string(), "app/featureB".to_string()],
                sync_interval: Some("15m".to_string()),
                auto_apply: true,
                accept_recommended: true,
                ..empty_source_args(url)
            };
            let result = super::source::cmd_source_add(&h.cli(), h.printer(), &args);
            assert!(result.is_ok(), "cmd_source_add should succeed: {result:?}");
            let cfg_after = std::fs::read_to_string(h.config_path().join("cfgd.yaml")).unwrap();
            assert!(
                cfg_after.contains("app/featureA"),
                "opt-in items should land in cfgd.yaml: {cfg_after}"
            );
            assert!(
                cfg_after.contains("15m"),
                "sync interval should land in cfgd.yaml: {cfg_after}"
            );
            assert!(
                cfg_after.contains("autoApply: true") || cfg_after.contains("autoApply: yes"),
                "auto_apply should serialise as true in cfgd.yaml: {cfg_after}"
            );
            assert!(
                cfg_after.contains("acceptRecommended: true"),
                "accept_recommended should serialise as true: {cfg_after}"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_source_add_duplicate_name_via_local_bare_fails() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&scratch, "dup-name", None);
            let h = CliTestHarness::builder().build();
            let url = cfgd_core::test_helpers::file_url(&bare);
            let args = SourceAddArgs {
                name: Some("dup-name".to_string()),
                ..empty_source_args(url.clone())
            };
            // First add succeeds.
            let r1 = super::source::cmd_source_add(&h.cli(), h.printer(), &args);
            assert!(r1.is_ok(), "first add should succeed: {r1:?}");
            // Second add against the same name fails with the "already exists" message.
            let args2 = SourceAddArgs {
                name: Some("dup-name".to_string()),
                ..empty_source_args(url)
            };
            let r2 = super::source::cmd_source_add(&h.cli(), h.printer(), &args2);
            let err = r2.expect_err("duplicate source name should fail");
            assert!(
                err.to_string().to_lowercase().contains("already exists"),
                "expected 'already exists' in error, got: {err}"
            );
        });
    }

    /// Like `make_bare_with_manifest` but also writes a non-empty
    /// `platform_profiles` mapping into the source manifest. Used to drive the
    /// auto-detect arm of cmd_source_add, which fires when caller passes no
    /// `--profile` and the manifest declares `platformProfiles`.
    fn make_bare_with_platform_profiles(
        scratch: &tempfile::TempDir,
        name: &str,
        profile_files: &[&str],
        platform_keys: &[(&str, &str)],
    ) -> std::path::PathBuf {
        let bare = scratch.path().join(format!("{name}-bare.git"));
        let _ = git2::Repository::init_bare(&bare).unwrap();
        let src = scratch.path().join(format!("{name}-src"));
        let src_repo = git2::Repository::init(&src).unwrap();
        let mut manifest = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: {name}\nspec:\n  provides:\n    profiles:\n"
        );
        for p in profile_files {
            manifest.push_str(&format!("      - {p}\n"));
        }
        if !platform_keys.is_empty() {
            manifest.push_str("    platformProfiles:\n");
            for (k, v) in platform_keys {
                manifest.push_str(&format!("      {k}: {v}\n"));
            }
        }
        std::fs::write(src.join("cfgd-source.yaml"), &manifest).unwrap();
        std::fs::create_dir_all(src.join("profiles")).unwrap();
        for p in profile_files {
            std::fs::write(
                src.join("profiles").join(format!("{p}.yaml")),
                format!(
                    "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: {p}\nspec: {{}}\n"
                ),
            )
            .unwrap();
        }
        let mut index = src_repo.index().unwrap();
        index
            .add_path(std::path::Path::new("cfgd-source.yaml"))
            .unwrap();
        for p in profile_files {
            let rel = format!("profiles/{p}.yaml");
            index.add_path(std::path::Path::new(&rel)).unwrap();
        }
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = src_repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        src_repo
            .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
            .unwrap();
        drop(tree);
        let url = cfgd_core::test_helpers::file_url(&bare);
        let mut remote = src_repo.remote("origin", &url).unwrap();
        let branch = src_repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        remote
            .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
            .unwrap();
        bare
    }

    #[test]
    #[serial]
    fn cmd_source_add_auto_selects_platform_profile_when_no_profile_flag() {
        // Manifest declares `platformProfiles: {linux: linux-default, macos: macos-default}`.
        // On the Linux CI host detect_platform().os == "linux", so the auto-detect
        // branch picks "linux-default", emits the `Auto-selected profile` success
        // line, and persists that as the subscription's profile.
        // Skip on non-Linux to keep platform expectations honest.
        if std::env::consts::OS != "linux" {
            return;
        }
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            let bare = make_bare_with_platform_profiles(
                &scratch,
                "platform-src",
                &["linux-default", "macos-default"],
                &[("linux", "linux-default"), ("macos", "macos-default")],
            );
            let h = CliTestHarness::builder().build();
            let url = cfgd_core::test_helpers::file_url(&bare);
            let args = SourceAddArgs {
                name: Some("platform-src".to_string()),
                profile: None, // <- trigger auto-detect branch
                ..empty_source_args(url)
            };
            let result = super::source::cmd_source_add(&h.cli(), h.printer(), &args);
            assert!(result.is_ok(), "cmd_source_add should succeed: {result:?}");
            let cfg_after = std::fs::read_to_string(h.config_path().join("cfgd.yaml")).unwrap();
            assert!(
                cfg_after.contains("linux-default"),
                "auto-detected profile should land in cfgd.yaml: {cfg_after}"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_source_add_with_empty_provided_profiles_bails_at_source_load() {
        // Manifest declares no profiles and no modules. SourceManager::load_source
        // rejects the source before we ever reach the profile-selection arms
        // of cmd_source_add — encoding the contract that a subscribable
        // source must expose at least one profile or at least one module.
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            let bare = scratch.path().join("empty-bare.git");
            let _ = git2::Repository::init_bare(&bare).unwrap();
            let src = scratch.path().join("empty-src");
            let src_repo = git2::Repository::init(&src).unwrap();
            std::fs::write(
                src.join("cfgd-source.yaml"),
                "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: empty-provider\nspec:\n  provides:\n    profiles: []\n",
            )
            .unwrap();
            let mut index = src_repo.index().unwrap();
            index
                .add_path(std::path::Path::new("cfgd-source.yaml"))
                .unwrap();
            index.write().unwrap();
            let tree_id = index.write_tree().unwrap();
            let tree = src_repo.find_tree(tree_id).unwrap();
            let sig = git2::Signature::now("t", "t@example.com").unwrap();
            src_repo
                .commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[])
                .unwrap();
            drop(tree);
            let url = cfgd_core::test_helpers::file_url(&bare);
            let mut remote = src_repo.remote("origin", &url).unwrap();
            let branch = src_repo
                .head()
                .unwrap()
                .shorthand()
                .unwrap_or("master")
                .to_string();
            remote
                .push(&[&format!("refs/heads/{branch}:refs/heads/{branch}")], None)
                .unwrap();

            let h = CliTestHarness::builder().build();
            let args = SourceAddArgs {
                name: Some("empty-provider".to_string()),
                profile: None,
                ..empty_source_args(url)
            };
            let result = super::source::cmd_source_add(&h.cli(), h.printer(), &args);
            let err = result.expect_err("empty provides must fail in source load");
            assert!(
                err.to_string()
                    .to_lowercase()
                    .contains("provides neither profiles nor modules"),
                "expected 'provides neither profiles nor modules' in error, got: {err}"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_source_add_with_branch_override_respects_branch_flag() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&scratch, "branched", None);
            let h = CliTestHarness::builder().build();
            let url = cfgd_core::test_helpers::file_url(&bare);
            // Determine the actual default branch name from the bare repo.
            let actual_branch = {
                let repo = git2::Repository::open(&bare).unwrap();
                let refs = repo.references().unwrap();
                let mut name = String::from("master");
                for r in refs.flatten() {
                    if let Ok(n) = r.name()
                        && let Some(stripped) = n.strip_prefix("refs/heads/")
                    {
                        name = stripped.to_string();
                        break;
                    }
                }
                name
            };
            let args = SourceAddArgs {
                name: Some("branched".to_string()),
                branch: Some(actual_branch.clone()),
                ..empty_source_args(url)
            };
            let result = super::source::cmd_source_add(&h.cli(), h.printer(), &args);
            assert!(result.is_ok(), "cmd_source_add should succeed: {result:?}");
            let cfg_after = std::fs::read_to_string(h.config_path().join("cfgd.yaml")).unwrap();
            assert!(
                cfg_after.contains(&actual_branch),
                "expected branch '{actual_branch}' in cfgd.yaml: {cfg_after}"
            );
        });
    }

    // ─── cmd_source_update end-to-end against the local bare fixture ─────────
    //
    // cmd_source_add seeds cfgd.yaml + a clone under <state>/sources/<name>.
    // cmd_source_update then walks the happy-path arm that has previously
    // only had error-path coverage (no-sources, name-not-found, load-failure):
    // refresh the source manifest, upsert the state-store row, and emit
    // "Updated source 'X'". The "load failure" arm is exercised by pointing
    // at a non-existent file:// URL.

    #[test]
    #[serial]
    fn cmd_source_update_all_walks_happy_path_and_records_success() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&scratch, "upd-src", None);
            let h = CliTestHarness::builder().build();
            let url = cfgd_core::test_helpers::file_url(&bare);
            let add_args = SourceAddArgs {
                name: Some("upd-src".to_string()),
                ..empty_source_args(url)
            };
            super::source::cmd_source_add(&h.cli(), h.printer(), &add_args)
                .expect("cmd_source_add precondition should succeed");

            // No name → updates every source. Drives the
            // `mgr.get(...).is_some()` happy path + upsert_config_source +
            // "Updated source" success line.
            super::source::cmd_source_update(&h.cli(), h.printer(), None)
                .expect("cmd_source_update should succeed against the staged source");

            h.assert_output_contains("Updated source 'upd-src'");

            // The state store should now have a row recording the source.
            let store =
                cfgd_core::state::StateStore::open(&h.state_path().join("state.db")).unwrap();
            let sources = store.config_sources().unwrap();
            assert!(
                sources.iter().any(|s| s.name == "upd-src"),
                "config_sources should record the source row: {:?}",
                sources.iter().map(|s| &s.name).collect::<Vec<_>>()
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_source_update_named_walks_happy_path_for_single_source_only() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            let bare_a = make_bare_with_manifest(&scratch, "src-a", None);
            let bare_b = make_bare_with_manifest(&scratch, "src-b", None);
            let h = CliTestHarness::builder().build();
            let url_a = cfgd_core::test_helpers::file_url(&bare_a);
            let url_b = cfgd_core::test_helpers::file_url(&bare_b);
            super::source::cmd_source_add(
                &h.cli(),
                h.printer(),
                &SourceAddArgs {
                    name: Some("src-a".to_string()),
                    ..empty_source_args(url_a)
                },
            )
            .unwrap();
            super::source::cmd_source_add(
                &h.cli(),
                h.printer(),
                &SourceAddArgs {
                    name: Some("src-b".to_string()),
                    ..empty_source_args(url_b)
                },
            )
            .unwrap();

            // Snapshot the buffer length so we only inspect output from
            // cmd_source_update — cmd_source_add ran twice above and its
            // success messages would otherwise satisfy the assertions.
            let baseline_len = h.output().len();

            // Update only src-b — the name-filter arm should pick exactly one
            // source. The post-update slice must contain src-b AND must NOT
            // mention src-a; without the second assertion the test would
            // pass even if the name filter was wired to update everything.
            super::source::cmd_source_update(&h.cli(), h.printer(), Some("src-b"))
                .expect("named update should succeed");

            let full = h.output();
            let update_out = &full[baseline_len..];
            assert!(
                update_out.contains("Updated source 'src-b'"),
                "named update should report src-b: {update_out}"
            );
            assert!(
                !update_out.contains("Updated source 'src-a'"),
                "named update must NOT touch src-a — the filter arm is broken if it does: {update_out}"
            );
        });
    }

    /// Clone `bare` into a fresh workdir, replace its cfgd-source.yaml with
    /// `new_manifest_yaml`, commit + push back to the bare. Used by the
    /// permission-change update tests to publish a v2 manifest with
    /// expanded policy.
    fn push_replacement_manifest(
        scratch: &tempfile::TempDir,
        bare: &std::path::Path,
        new_manifest_yaml: &str,
    ) {
        let clone_dir = scratch.path().join(format!(
            "replace-clone-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let url = cfgd_core::test_helpers::file_url(bare);
        let repo = git2::Repository::clone(&url, &clone_dir).unwrap();
        std::fs::write(clone_dir.join("cfgd-source.yaml"), new_manifest_yaml).unwrap();
        let mut index = repo.index().unwrap();
        index
            .add_path(std::path::Path::new("cfgd-source.yaml"))
            .unwrap();
        index.write().unwrap();
        let tree_id = index.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@example.com").unwrap();
        let parent = repo.head().unwrap().peel_to_commit().unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "v2 manifest", &tree, &[&parent])
            .unwrap();
        drop(tree);
        let branch = repo
            .head()
            .unwrap()
            .shorthand()
            .unwrap_or("master")
            .to_string();
        let mut remote = repo.find_remote("origin").unwrap();
        remote
            .push(
                &[&format!("+refs/heads/{branch}:refs/heads/{branch}")],
                None,
            )
            .unwrap();
    }

    #[test]
    #[serial]
    fn cmd_source_show_displays_cached_manifest_and_policy_summary() {
        // After cmd_source_add against a local bare publishes a v2 manifest
        // with a populated policy (1 required + 1 recommended item), drive
        // cmd_source_show. With a successfully-cached manifest, show.rs
        // enters its manifest-display block: Name + Description + the
        // Policy Summary subheader with per-tier item listings.
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&scratch, "shown-src", Some("2.0.0"));
            let h = CliTestHarness::builder().build();
            let url = cfgd_core::test_helpers::file_url(&bare);
            super::source::cmd_source_add(
                &h.cli(),
                h.printer(),
                &SourceAddArgs {
                    name: Some("shown-src".to_string()),
                    accept_recommended: true,
                    ..empty_source_args(url)
                },
            )
            .expect("cmd_source_add precondition should succeed");

            // Republish with a v2 manifest carrying a populated policy. This
            // is what cmd_source_show will load into its display block.
            let v2 = "apiVersion: cfgd.io/v1alpha1\n\
                      kind: ConfigSource\n\
                      metadata:\n  name: shown-src\n  version: 2.0.0\n  \
                      description: Team-shared config\n\
                      spec:\n  provides:\n    profiles:\n      - default\n  \
                      policy:\n    required:\n      packages:\n        \
                      pipx:\n          - ripgrep\n    recommended:\n      \
                      packages:\n        pipx:\n          - fd-find\n";
            push_replacement_manifest(&scratch, &bare, v2);

            // Pull v2 into the cache. Permission expansion warning fires
            // (required count 0 → 1) but prompt_confirm in test mode returns
            // Err → continue. The cache nevertheless got the v2 manifest
            // written by SourceManager::load_source BEFORE the permission
            // check ran, so cmd_source_show can render its policy section.
            super::source::cmd_source_update(&h.cli(), h.printer(), Some("shown-src"))
                .expect("cmd_source_update");

            let baseline_len = h.output().len();
            super::source::cmd_source_show(&h.cli(), h.printer(), "shown-src")
                .expect("cmd_source_show");
            let full = h.output();
            let show_out = &full[baseline_len..];

            assert!(
                show_out.contains("Source: shown-src"),
                "expected header, got: {show_out}"
            );
            assert!(
                show_out.contains("Manifest"),
                "expected Manifest subheader, got: {show_out}"
            );
            assert!(
                show_out.contains("Team-shared config"),
                "expected manifest description, got: {show_out}"
            );
            assert!(
                show_out.contains("Policy Summary"),
                "expected Policy Summary subheader, got: {show_out}"
            );
            assert!(
                show_out.contains("Required") && show_out.contains("Recommended"),
                "expected per-tier labels, got: {show_out}"
            );
            assert!(
                show_out.contains("pipx: ripgrep"),
                "expected required pipx item rendered, got: {show_out}"
            );
            assert!(
                show_out.contains("pipx: fd-find"),
                "expected recommended pipx item rendered, got: {show_out}"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_source_update_detects_permission_change_and_skips_on_prompt_cancel() {
        // cmd_source_add stages the initial manifest (no policy). A second
        // commit is pushed to the bare that expands the policy
        // (required.modules: [m1, m2] — 2 items). cmd_source_update fetches
        // the v2 manifest; detect_permission_changes returns "Required
        // items increased from 0 to 2"; the warning fires; prompt_confirm
        // in test mode returns Err → the Err(_) arm prints
        // "Skipped source 'X' (prompt cancelled)" and continue's out of the
        // loop. Pins the prompt-cancel branch (lines 72-77 in source/update.rs).
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            let scratch = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&scratch, "perm-src", None);
            let h = CliTestHarness::builder().build();
            let url = cfgd_core::test_helpers::file_url(&bare);
            super::source::cmd_source_add(
                &h.cli(),
                h.printer(),
                &SourceAddArgs {
                    name: Some("perm-src".to_string()),
                    ..empty_source_args(url)
                },
            )
            .expect("cmd_source_add precondition should succeed");

            // Publish a v2 manifest with EXPANDED policy. required.modules
            // grew from 0 to 2 — detect_permission_changes will flag this.
            let v2 = "apiVersion: cfgd.io/v1alpha1\n\
                      kind: ConfigSource\n\
                      metadata:\n  name: perm-src\n\
                      spec:\n  provides:\n    profiles:\n      - default\n  \
                      policy:\n    required:\n      modules:\n        - mod-a\n        - mod-b\n";
            push_replacement_manifest(&scratch, &bare, v2);

            let baseline_len = h.output().len();
            super::source::cmd_source_update(&h.cli(), h.printer(), Some("perm-src"))
                .expect("cmd_source_update should not bubble up the cancelled prompt");

            let full = h.output();
            let update_out = &full[baseline_len..];
            assert!(
                update_out.contains("update changes permissions"),
                "expected permission-change warning, got: {update_out}"
            );
            assert!(
                update_out.contains("Required items increased from 0 to 2"),
                "expected required-items expansion message, got: {update_out}"
            );
            assert!(
                update_out.contains("Skipped source 'perm-src' (prompt cancelled)"),
                "expected prompt-cancelled skip line, got: {update_out}"
            );
            // The upsert_config_source success line MUST NOT appear — the
            // continue must short-circuit before the state-store write.
            assert!(
                !update_out.contains("Updated source 'perm-src'"),
                "perm-src should NOT have been marked updated when prompt is cancelled: {update_out}"
            );
        });
    }

    #[test]
    #[serial]
    fn cmd_source_update_records_error_status_when_upstream_unreachable() {
        with_test_env_var("CFGD_ALLOW_LOCAL_SOURCES", Some("1"), || {
            // Stage a real source so cmd_source_add succeeds — then bulldoze
            // the bare upstream so the *next* fetch fails. cmd_source_update
            // should surface the failure via the "Failed to update source"
            // error line + flip the state-store status to 'error'.
            let scratch = tempfile::tempdir().unwrap();
            let bare = make_bare_with_manifest(&scratch, "doomed-src", None);
            let h = CliTestHarness::builder().build();
            let url = cfgd_core::test_helpers::file_url(&bare);
            super::source::cmd_source_add(
                &h.cli(),
                h.printer(),
                &SourceAddArgs {
                    name: Some("doomed-src".to_string()),
                    ..empty_source_args(url)
                },
            )
            .unwrap();

            // Bulldoze the upstream + the local cache so the update can't
            // satisfy the fetch from either side.
            std::fs::remove_dir_all(&bare).unwrap();
            let cache_dir = h.state_path().join("sources").join("doomed-src");
            if cache_dir.exists() {
                std::fs::remove_dir_all(&cache_dir).unwrap();
            }

            // Call the non-exiting core directly: `cmd_source_update` would
            // `process::exit(1)` on this failure and abort the test binary.
            let error_count =
                super::source::run_source_update(&h.cli(), h.printer(), Some("doomed-src"))
                    .expect("run_source_update should not bubble up a fetch failure");
            assert_eq!(
                error_count, 1,
                "the single doomed source should count as 1 failure"
            );

            h.assert_output_contains("Failed to update source 'doomed-src'");

            // The status update arm should have flipped the row to 'error'.
            let store =
                cfgd_core::state::StateStore::open(&h.state_path().join("state.db")).unwrap();
            let sources = store.config_sources().unwrap();
            let row = sources
                .iter()
                .find(|s| s.name == "doomed-src")
                .expect("doomed-src should still be in the state store");
            assert_eq!(
                row.status, "error",
                "state store should record the error status, got: {}",
                row.status
            );
        });
    }
}

// ---------------------------------------------------------------------------
// ApplyPhase mapping helpers — pure pinning tests
//
// The `cmd_apply_dry_run_each_phase` test exercises both `as_str` and
// `apply_phase_to_phase_name` via Option::map / format-args, but those call
// paths are only evaluated when assertions FAIL (format-arg path) or get
// inlined into the caller (Option::map path), so neither shows up in
// per-function coverage. These direct tests pin the public contract:
// callers (plan_ops.rs prefix, apply.rs phase filter) depend on these
// exact mappings.
// ---------------------------------------------------------------------------

#[test]
fn apply_phase_as_str_round_trips_every_variant_to_its_kebab_label() {
    let cases = [
        (super::ApplyPhase::PreScripts, "pre-scripts"),
        (super::ApplyPhase::Env, "env"),
        (super::ApplyPhase::Modules, "modules"),
        (super::ApplyPhase::Packages, "packages"),
        (super::ApplyPhase::System, "system"),
        (super::ApplyPhase::Files, "files"),
        (super::ApplyPhase::Secrets, "secrets"),
        (super::ApplyPhase::PostScripts, "post-scripts"),
    ];
    for (phase, label) in cases {
        assert_eq!(phase.as_str(), label);
    }
}

#[test]
fn apply_phase_to_phase_name_maps_every_variant_to_matching_reconciler_phase() {
    use cfgd_core::reconciler::PhaseName;
    let cases = [
        (super::ApplyPhase::PreScripts, PhaseName::PreScripts),
        (super::ApplyPhase::Env, PhaseName::Env),
        (super::ApplyPhase::Modules, PhaseName::Modules),
        (super::ApplyPhase::Packages, PhaseName::Packages),
        (super::ApplyPhase::System, PhaseName::System),
        (super::ApplyPhase::Files, PhaseName::Files),
        (super::ApplyPhase::Secrets, PhaseName::Secrets),
        (super::ApplyPhase::PostScripts, PhaseName::PostScripts),
    ];
    for (input, expected) in cases {
        assert_eq!(super::apply_phase_to_phase_name(input), expected);
    }
}

#[test]
fn decide_action_resolution_pins_accepted_and_rejected_strings() {
    // DecideAction::resolution() is the persisted state-store enum string
    // for source-decisions. Renaming either label without a migration would
    // orphan historical decision rows — pin both here.
    assert_eq!(super::DecideAction::Accept.resolution(), "accepted");
    assert_eq!(super::DecideAction::Reject.resolution(), "rejected");
}

#[test]
fn expand_aliases_flags_only_returns_args_unchanged() {
    // When find_subcommand_index returns None (no positional found), expand_aliases
    // returns args unchanged — exercises the None branch at line 129.
    let args = vec![
        "cfgd".to_string(),
        "--verbose".to_string(),
        "--no-color".to_string(),
    ];
    let result = super::expand_aliases(args.clone());
    assert_eq!(result, args);
}

#[test]
fn expand_aliases_double_dash_separator_returns_args_unchanged() {
    // Double-dash stops scanning → find_subcommand_index returns None.
    let args = vec!["cfgd".to_string(), "--".to_string(), "apply".to_string()];
    let result = super::expand_aliases(args.clone());
    assert_eq!(result, args);
}

#[test]
fn execute_profile_create_dispatch() {
    let h = CliTestHarness::builder().build();
    // Provide a post_apply script so interactive mode is not triggered.
    let cli = h.cli_with_command(Command::Profile {
        command: ProfileCommand::Create(Box::new(ProfileCreateArgs {
            post_apply: vec!["echo done".to_string()],
            ..test_profile_create_args("newprof")
        })),
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Profile Create dispatch must succeed");
    let profiles_dir = h.config_path().join("profiles");
    assert!(
        profiles_dir.join("newprof").join("profile.yaml").exists(),
        "new profile file should have been created"
    );
}

#[test]
fn execute_profile_update_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Profile {
        command: ProfileCommand::Update(Box::new(empty_profile_update_args())),
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Profile Update dispatch must succeed");
}

#[test]
fn execute_profile_delete_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Profile {
        command: ProfileCommand::Delete {
            name: "work".to_string(),
            yes: true,
            ignore_not_found: false,
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Profile Delete dispatch must succeed");
}

#[test]
fn execute_module_show_dispatch() {
    let h = CliTestHarness::builder()
        .module("test-mod", SIMPLE_MODULE_YAML)
        .build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Show {
            name: "test-mod".to_string(),
            show_values: false,
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Module Show dispatch must succeed");
    h.assert_output_contains("test-mod");
}

#[test]
fn execute_module_create_dispatch() {
    let h = CliTestHarness::builder().build();
    // Provide a post_apply script so interactive mode is not triggered.
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Create(Box::new(ModuleCreateArgs {
            post_apply: vec!["echo done".to_string()],
            ..test_module_create_args("new-mod")
        })),
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Module Create dispatch must succeed");
    assert!(
        h.config_path()
            .join("modules")
            .join("new-mod")
            .join("module.yaml")
            .exists(),
        "module.yaml should have been created"
    );
}

#[test]
fn execute_module_update_dispatch() {
    let h = CliTestHarness::builder()
        .module("test-mod", SIMPLE_MODULE_YAML)
        .build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Update(Box::new(empty_module_update_args("test-mod"))),
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Module Update dispatch must succeed");
}

#[test]
fn execute_module_delete_dispatch() {
    let h = CliTestHarness::builder()
        .module("test-mod", SIMPLE_MODULE_YAML)
        .build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Delete {
            name: "test-mod".to_string(),
            yes: true,
            purge: false,
            ignore_not_found: false,
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Module Delete dispatch must succeed");
}

#[test]
fn execute_module_search_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Search {
            query: "networking".to_string(),
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Module Search dispatch must succeed");
}

#[test]
fn execute_module_registry_list_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Registry {
            command: ModuleRegistryCommand::List,
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Module Registry List dispatch must succeed");
}

#[test]
fn execute_module_registry_add_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Registry {
            command: ModuleRegistryCommand::Add {
                url: "https://github.com/example/modules".to_string(),
                name: Some("example".to_string()),
            },
        },
    });
    // Add may fail (network) but dispatch arm is exercised.
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    let _ = result;
}

#[test]
fn execute_module_registry_remove_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Registry {
            command: ModuleRegistryCommand::Remove {
                name: "nonexistent".to_string(),
                ignore_not_found: false,
            },
        },
    });
    // Removing a nonexistent registry is now a strict not-found error (exit 6),
    // uniform with every other named-resource miss — not an idempotent no-op.
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    assert!(result.is_err(), "removing nonexistent registry should fail");
}

#[test]
fn execute_module_registry_rename_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Registry {
            command: ModuleRegistryCommand::Rename {
                name: "nonexistent".to_string(),
                new_name: "new-name".to_string(),
            },
        },
    });
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    assert!(result.is_err(), "renaming nonexistent registry should fail");
}

#[test]
fn execute_module_export_dispatch() {
    let h = CliTestHarness::builder()
        .module("test-mod", SIMPLE_MODULE_YAML)
        .build();
    let out = tempfile::tempdir().expect("tempdir");
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Export {
            name: "test-mod".to_string(),
            as_format: ExportFormat::Devcontainer,
            dir: Some(out.path().to_string_lossy().into_owned()),
        },
    });
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    let _ = result;
}

#[test]
fn execute_module_push_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Push {
            dir: "/nonexistent/module".to_string(),
            artifact: "ghcr.io/example/module:v0.0.0".to_string(),
            platform: None,
            apply: false,
            sign: false,
            key: None,
            attest: false,
        },
    });
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    // Fails because dir doesn't contain module.yaml, but dispatch arm was reached.
    assert!(
        result.is_err(),
        "push of nonexistent module dir should fail"
    );
}

#[test]
fn execute_module_pull_dispatch() {
    let h = CliTestHarness::builder().build();
    let out_dir = tempfile::tempdir().unwrap();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Pull {
            artifact_ref: "ghcr.io/example/module:v0.0.0".to_string(),
            dir: out_dir.path().display().to_string(),
            require_signature: false,
            verify_attestation: false,
            key: None,
            certificate_identity: None,
            certificate_oidc_issuer: None,
        },
    });
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    // Fails on network/registry, but dispatch arm was reached.
    assert!(result.is_err(), "pull of unreachable artifact should fail");
}

#[test]
fn execute_module_build_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Build {
            dir: "/nonexistent/module".to_string(),
            target: None,
            base_image: None,
            artifact: None,
            sign: false,
            key: None,
        },
    });
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    // Fails because dir doesn't contain module.yaml, but dispatch arm was reached.
    assert!(result.is_err(), "build of nonexistent dir should fail");
    let msg = result.unwrap_err().to_string();
    assert!(
        msg.contains("module.yaml") || msg.contains("nonexistent"),
        "error should describe the missing module, got: {msg}"
    );
}

#[test]
fn execute_module_keys_list_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Keys {
            command: ModuleKeysCommand::List,
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Module Keys List dispatch must succeed");
}

#[test]
fn execute_module_keys_generate_dispatch() {
    let out_dir = tempfile::tempdir().unwrap();
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Keys {
            command: ModuleKeysCommand::Generate {
                dir: Some(out_dir.path().display().to_string()),
            },
        },
    });
    // Fails when cosign is absent; still exercises the dispatch arm.
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    let _ = result;
}

#[test]
fn execute_module_keys_rotate_dispatch() {
    let out_dir = tempfile::tempdir().unwrap();
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Keys {
            command: ModuleKeysCommand::Rotate {
                dir: Some(out_dir.path().display().to_string()),
                artifacts: vec![],
            },
        },
    });
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    let _ = result;
}

#[test]
fn execute_module_upgrade_dispatch() {
    let h = CliTestHarness::builder()
        .module("test-mod", SIMPLE_MODULE_YAML)
        .build();
    let cli = h.cli_with_command(Command::Module {
        command: ModuleCommand::Upgrade {
            name: "test-mod".to_string(),
            ref_: None,
            yes: true,
            allow_unsigned: false,
        },
    });
    // Upgrade of a local (non-locked-remote) module fails, but dispatch arm reached.
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    let _ = result;
}

#[test]
fn execute_config_unset_dispatch() {
    // Set a key first so unset has something to remove.
    let h = CliTestHarness::builder().build();
    let set_cli = h.cli_with_command(Command::Config {
        command: ConfigCommand::Set {
            key: "theme".to_string(),
            value: "dracula".to_string(),
        },
    });
    super::execute(
        &set_cli,
        h.printer(),
        &super::paths::DirSources::all_default(),
    )
    .expect("Config Set must succeed");

    let unset_cli = h.cli_with_command(Command::Config {
        command: ConfigCommand::Unset {
            key: "theme".to_string(),
        },
    });
    super::execute(
        &unset_cli,
        h.printer(),
        &super::paths::DirSources::all_default(),
    )
    .expect("Config Unset dispatch must succeed");
}

#[test]
fn execute_source_show_dispatch() {
    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: my-src
      origin:
        url: https://github.com/example/config
        branch: main
        type: Git
      subscription:
        priority: 500
"#;
    let h = CliTestHarness::builder().config(config_with_source).build();
    let cli = h.cli_with_command(Command::Source {
        command: SourceCommand::Show {
            name: "my-src".to_string(),
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Source Show dispatch must succeed");
    h.assert_output_contains("my-src");
}

#[test]
fn execute_source_remove_dispatch() {
    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: removable
      origin:
        url: https://github.com/example/config
        branch: main
        type: Git
      subscription:
        priority: 500
"#;
    let h = CliTestHarness::builder().config(config_with_source).build();
    let cli = h.cli_with_command(Command::Source {
        command: SourceCommand::Remove {
            name: "removable".to_string(),
            keep_all: true,
            remove_all: false,
            yes: true,
            ignore_not_found: false,
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Source Remove dispatch must succeed");
}

#[test]
fn execute_source_override_dispatch() {
    let config_with_source = r#"apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: t
spec:
  profile: default
  sources:
    - name: my-src
      origin:
        url: https://github.com/example/config
        branch: main
        type: Git
      subscription:
        priority: 500
"#;
    let h = CliTestHarness::builder().config(config_with_source).build();
    let cli = h.cli_with_command(Command::Source {
        command: SourceCommand::Override {
            source: "my-src".to_string(),
            action: SourceOverrideAction::Reject,
            path: "env.EDITOR".to_string(),
            value: None,
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Source Override dispatch must succeed");
}

#[test]
fn execute_source_create_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Source {
        command: SourceCommand::Create {
            name: Some("my-source".to_string()),
            description: Some("test source".to_string()),
            version: Some("1.0.0".to_string()),
        },
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Source Create dispatch must succeed");
}

#[test]
fn execute_daemon_status_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Daemon {
        command: Some(DaemonCommand::Status),
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Daemon Status dispatch must succeed");
}

#[test]
fn execute_compliance_history_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Compliance {
        command: Some(ComplianceCommand::History { since: None }),
    });
    super::execute(&cli, h.printer(), &super::paths::DirSources::all_default())
        .expect("Compliance History dispatch must succeed");
}

#[test]
fn execute_compliance_diff_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Compliance {
        command: Some(ComplianceCommand::Diff {
            base_id: 1,
            target_id: 2,
        }),
    });
    // Fails when snapshots 1 and 2 don't exist, but dispatch arm is exercised.
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    let _ = result;
}

#[test]
fn execute_enroll_dispatch() {
    let h = CliTestHarness::builder().build();
    let cli = h.cli_with_command(Command::Enroll {
        server_url: "http://127.0.0.1:19999".to_string(),
        token: Some("test-token".to_string()),
        ssh_key: None,
        gpg_key: None,
        username: None,
    });
    // Fails because server is unreachable, but dispatch arm is exercised.
    let result = super::execute(&cli, h.printer(), &super::paths::DirSources::all_default());
    assert!(
        result.is_err(),
        "enroll with unreachable server should fail"
    );
}

// -----------------------------------------------------------------------
// doctor: profile layout checks
// -----------------------------------------------------------------------

#[test]
fn cmd_doctor_json_flags_legacy_profiles() {
    // The default harness writes flat legacy manifests (default.yaml, work.yaml).
    let h = CliTestHarness::builder().json().build();
    super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();

    let parsed = h.json_output();
    let profiles = parsed["profiles"]
        .as_array()
        .expect("profiles should be array");
    assert_eq!(profiles.len(), 2);
    for p in profiles {
        assert_eq!(p["legacy"], true, "flat manifests are legacy: {p}");
        let path = p["path"].as_str().expect("path should be set");
        assert!(!path.contains('\\'), "payload paths must be posix: {path}");
        assert!(
            path.ends_with(&format!("{}.yaml", p["name"].as_str().unwrap())),
            "path should be the flat manifest: {path}"
        );
        assert!(p["error"].is_null());
    }
}

#[test]
fn cmd_doctor_json_canonical_profiles_not_legacy() {
    let h = CliTestHarness::builder().json().build();
    // convert the harness's flat manifests to canonical bundles
    let pdir = h.config_path().join("profiles");
    for name in ["default", "work"] {
        let bundle = pdir.join(name);
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::rename(
            pdir.join(format!("{name}.yaml")),
            bundle.join("profile.yaml"),
        )
        .unwrap();
    }
    super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();

    let parsed = h.json_output();
    let profiles = parsed["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 2);
    for p in profiles {
        assert_eq!(p["legacy"], false, "canonical bundles are not legacy: {p}");
        assert!(
            p["path"]
                .as_str()
                .unwrap()
                .ends_with(&format!("{}/profile.yaml", p["name"].as_str().unwrap()))
        );
    }
}

#[test]
fn run_doctor_returns_false_verdict_on_ambiguous_profile() {
    // Both a flat manifest and its canonical bundle for the same name: a
    // hard-broken config the wrapper maps to a non-zero exit.
    let h = CliTestHarness::builder().build();
    let pdir = h.config_path().join("profiles");
    let bundle = pdir.join("work");
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::copy(pdir.join("work.yaml"), bundle.join("profile.yaml")).unwrap();

    let passed = super::doctor::run_doctor(&h.cli(), h.printer()).unwrap();
    assert!(
        !passed,
        "an ambiguous profile must fail the doctor verdict (drives the non-zero exit)"
    );
}

#[test]
fn build_doctor_doc_legacy_profile_warns_with_migrate_hint() {
    let mut output = base_doctor_output();
    output.profiles = vec![super::output_types::DoctorProfileLayoutCheck {
        name: "work".into(),
        legacy: true,
        path: Some("/etc/cfgd/profiles/work.yaml".into()),
        error: None,
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains(
            "profile 'work' uses the legacy flat layout — run 'cfgd profile migrate work'"
        ),
        "should warn with the migrate remediation, got: {text}"
    );
    assert!(
        text.contains("All checks passed"),
        "legacy layout is supported — a WARN must not fail doctor, got: {text}"
    );
}

#[test]
fn build_doctor_doc_profiles_all_canonical_ok() {
    let mut output = base_doctor_output();
    output.profiles = vec![super::output_types::DoctorProfileLayoutCheck {
        name: "work".into(),
        legacy: false,
        path: Some("/etc/cfgd/profiles/work/profile.yaml".into()),
        error: None,
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("All profiles use the canonical bundle layout"),
        "should report OK when all canonical, got: {text}"
    );
}

#[test]
fn build_doctor_doc_ambiguous_profile_fails() {
    let mut output = base_doctor_output();
    output.profiles = vec![super::output_types::DoctorProfileLayoutCheck {
        name: "work".into(),
        legacy: true,
        path: None,
        error: Some("ambiguous profile 'work': multiple forms exist".into()),
    }];
    let extras = super::doctor::DoctorExtras::default();
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("ambiguous profile 'work'"),
        "should surface the ambiguity message, got: {text}"
    );
    assert!(
        text.contains("Some checks failed"),
        "an ambiguous profile is hard-broken (every load errors) — it must fail doctor, got: {text}"
    );
}

#[test]
fn build_doctor_doc_unscannable_profiles_dir_fails() {
    let mut output = base_doctor_output();
    output.profiles = vec![super::output_types::DoctorProfileLayoutCheck {
        name: "/etc/cfgd/profiles".into(),
        legacy: false,
        path: None,
        error: Some("failed to read /etc/cfgd/profiles: permission denied".into()),
    }];
    let extras = super::doctor::DoctorExtras {
        state_store: None,
        profiles_dir: Some(super::doctor::DoctorProfilesDir {
            path: "/etc/cfgd/profiles".into(),
            exists: true,
            profile_count: 0,
            error: Some("failed to read /etc/cfgd/profiles: permission denied".into()),
        }),
        config_sources: vec![],
    };
    let text = emit_doc(&output, &extras);
    assert!(
        text.contains("Profiles directory: /etc/cfgd/profiles — failed to read"),
        "System line must report the unreadable dir, not a bogus count, got: {text}"
    );
    assert!(
        text.contains("Some checks failed"),
        "an unscannable profiles dir must fail doctor, got: {text}"
    );
}
