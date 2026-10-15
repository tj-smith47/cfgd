//! End-to-end proof of the central CLI error sink (`render_cli_error`).
//!
//! These tests drive the REAL `cfgd` binary on failing commands — the only path
//! that traverses `main`'s dispatch and `render_cli_error`. Unit tests call `cmd_*`
//! handlers directly and never reach the sink, which is exactly why the systemic
//! double-print / json-stray-line / json-silent bug survived "completed" + reviews.
//! The invariants pinned here:
//!   - human mode: exactly ONE `✗` failure line on stderr (never two), with the
//!     error text;
//!   - structured mode (`-o json`): stdout is exactly ONE JSON object (parsing the
//!     whole stdout as a single value fails if a second was concatenated), NEVER
//!     silent on failure, and no `✗` human line leaks onto stdout;
//!   - the structured payload carries the expected `error` kind.

mod cfgd_binary;
use cfgd_binary::cfgd_bin;

/// Minimal valid config dir (so a command reaches its own not-found logic rather
/// than failing earlier on missing config).
fn create_valid_config(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("profiles")).unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: base\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("profiles/base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
    )
    .unwrap();
}

fn run(args: &[&str]) -> (String, String, Option<i32>) {
    let out = cfgd_bin().unwrap().args(args).output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

/// stdout in structured mode must be exactly ONE JSON value — concatenated objects
/// (the duplication signature) make `from_str` fail on trailing input.
fn parse_single_json(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!(
            "structured failure stdout must be exactly one JSON value (got error {e}): {stdout:?}"
        )
    })
}

#[test]
fn module_show_not_found_human_emits_exactly_one_fail_line() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let cfg = dir.path().join("cfgd.yaml");

    let (stdout, stderr, code) = run(&[
        "module",
        "show",
        "does-not-exist",
        "--config",
        cfg.to_str().unwrap(),
    ]);

    assert_ne!(code, Some(0), "missing module must fail");
    assert_eq!(
        stderr.matches('✗').count(),
        1,
        "exactly one ✗ failure line on stderr — stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("not found"),
        "failure text must say not found — stderr: {stderr:?}"
    );
    assert!(
        !stdout.contains('✗'),
        "no failure line on stdout in human mode — stdout: {stdout:?}"
    );
}

#[test]
fn module_show_not_found_json_emits_one_payload_never_silent() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let cfg = dir.path().join("cfgd.yaml");

    let (stdout, _stderr, code) = run(&[
        "module",
        "show",
        "does-not-exist",
        "--config",
        cfg.to_str().unwrap(),
        "-o",
        "json",
    ]);

    assert_ne!(code, Some(0), "missing module must fail");
    assert!(
        !stdout.trim().is_empty(),
        "structured failure must NOT be silent on stdout"
    );
    assert!(
        !stdout.contains('✗'),
        "no human ✗ line beside the json payload — stdout: {stdout:?}"
    );
    let v = parse_single_json(&stdout);
    assert_eq!(v["error"], "not_found", "payload error kind — got {v}");
    assert_eq!(v["name"], "does-not-exist");
}

#[test]
fn missing_config_human_emits_one_fail_line_and_init_hint() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("nope").join("cfgd.yaml");

    let (stdout, stderr, code) = run(&["status", "--config", nonexistent.to_str().unwrap()]);

    // Exit 3 = NoConfig: cli_error_ctx's exit code survives to the real process
    // exit, end to end, beyond the unit level.
    assert_eq!(code, Some(3), "missing config must exit NoConfig(3)");
    assert_eq!(
        stderr.matches('✗').count(),
        1,
        "exactly one ✗ failure line on stderr — stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("cfgd init"),
        "missing-config failure must surface the `cfgd init` remediation hint — stderr: {stderr:?}"
    );
    assert!(
        !stdout.contains('✗'),
        "stdout clean in human mode: {stdout:?}"
    );
}

#[test]
fn missing_config_json_emits_one_payload_never_silent() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("nope").join("cfgd.yaml");

    let (stdout, _stderr, code) = run(&[
        "status",
        "--config",
        nonexistent.to_str().unwrap(),
        "-o",
        "json",
    ]);

    assert_eq!(code, Some(3), "missing config must exit NoConfig(3)");
    assert!(
        !stdout.trim().is_empty(),
        "structured failure must NOT be silent on stdout"
    );
    let v = parse_single_json(&stdout);
    assert!(
        v.get("error").is_some(),
        "structured failure payload must carry an `error` key — got {v}"
    );
}

/// The same class decision, over the real binary's dispatch: a refusal's
/// remediation survives `CFGD_USAGE_HINTS=false`.
///
/// `usageHints` decides tutorial pointers; the one statement of what would let
/// a refused command run always renders. The child's stderr is read raw — a piped
/// child resolves `ColorChoice::Auto` to no colour, so there is nothing to
/// strip — and the fixture declares a module, because the not-found hint names
/// the modules that DO exist and a config declaring none carries no hint to
/// suppress.
#[test]
fn a_refusal_names_its_fix_end_to_end_with_usage_hints_off() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let module_dir = dir.path().join("modules").join("git");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(
        module_dir.join("module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: git\nspec: {}\n",
    )
    .unwrap();

    let out = cfgd_bin()
        .unwrap()
        .env(cfgd_core::CFGD_USAGE_HINTS_ENV, "false")
        .args(["module", "show", "nope", "--config"])
        .arg(dir.path().join("cfgd.yaml"))
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(6),
        "module show must refuse with NotFound(6), or the hint below is a tutorial `usageHints` may take away — stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("→ "),
        "the refusal's remediation survives the gate: {stderr:?}"
    );
    assert!(
        stderr.contains("Available modules: git"),
        "and it still names the way out: {stderr:?}"
    );
}

/// The `-o json` payload a refusal leaves on stdout, run against a valid
/// config so each command reaches its own refusal past the config read.
fn json_refusal(dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    create_valid_config(dir);
    json_refusal_against(&dir.join("cfgd.yaml"), args)
}

/// A refused flag value names the flag, repeats the value and, where the
/// accepted words are a closed list, carries that list.
#[test]
fn an_unknown_context_names_its_flag_value_and_the_accepted_words_in_json() {
    let dir = tempfile::tempdir().unwrap();
    let v = json_refusal(dir.path(), &["apply", "--context", "bogus"]);
    assert_eq!(
        v,
        serde_json::json!({
            "error": "invalid_argument",
            "name": "--context",
            "flag": "--context",
            "value": "bogus",
            "valid": ["apply", "reconcile"],
        })
    );
}

/// An unknown `bootstrap.` selector carries the selectors that would have
/// matched, cfgd's own groups and the managers alike.
#[test]
fn an_unknown_bootstrap_selector_names_the_selectors_it_accepts_in_json() {
    let dir = tempfile::tempdir().unwrap();
    let v = json_refusal(dir.path(), &["plan", "--phase", "bootstrap.nope"]);
    assert_eq!(v["error"], "invalid_argument", "{v}");
    assert_eq!(v["flag"], "--phase", "{v}");
    assert_eq!(v["value"], "bootstrap.nope", "{v}");
    let valid: Vec<&str> = v["valid"]
        .as_array()
        .unwrap_or_else(|| panic!("`valid` is a list: {v}"))
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        valid.contains(&"managers") && valid.contains(&"apt"),
        "`valid` lists cfgd's groups and the managers: {v}"
    );
}

/// `--with-profile` with no `--module` is a flag missing its partner.
#[test]
fn with_profile_without_a_module_is_a_missing_argument_in_json() {
    let dir = tempfile::tempdir().unwrap();
    let v = json_refusal(dir.path(), &["plan", "--with-profile"]);
    assert_eq!(
        v,
        serde_json::json!({
            "error": "missing_argument",
            "name": "--with-profile",
            "flag": "--with-profile",
            "requires": "--module",
        })
    );
}

/// A secret verb handed a file that is not there names the path it looked at.
#[test]
fn a_secret_verb_on_a_missing_file_is_not_found_with_its_path_in_json() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("absent.enc");
    let v = json_refusal(
        dir.path(),
        &["secret", "encrypt", missing.to_str().unwrap()],
    );
    let path = cfgd_core::to_posix_string(&missing);
    assert_eq!(
        v,
        serde_json::json!({ "error": "not_found", "name": &path, "path": &path })
    );
}

/// An unknown `explain` field path names the resource it was read against and
/// the fields available where the path stopped resolving.
#[test]
fn an_unknown_explain_field_path_names_the_fields_where_it_stopped_in_json() {
    let dir = tempfile::tempdir().unwrap();
    let v = json_refusal(dir.path(), &["explain", "profile.spec.packages.nope"]);
    assert_eq!(v["error"], "not_found", "{v}");
    assert_eq!(v["resource"], "profile", "{v}");
    let available: Vec<&str> = v["available"]
        .as_array()
        .unwrap_or_else(|| panic!("`available` is a list: {v}"))
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        available.contains(&"brew") && !available.contains(&"packages"),
        "`available` lists the children of `spec.packages`, where resolution stopped: {v}"
    );
}

/// A retired `status` switch is a refused flag like any other: it names the
/// switch, carries the value clap read for it and the command that replaces it.
#[test]
fn a_retired_status_switch_names_its_flag_value_and_replacement_in_json() {
    let dir = tempfile::tempdir().unwrap();
    let v = json_refusal(dir.path(), &["status", "--show-all"]);
    assert_eq!(
        v,
        serde_json::json!({
            "error": "invalid_argument",
            "name": "--show-all",
            "flag": "--show-all",
            "value": "true",
            "replacement": "cfgd status -o wide",
        })
    );
}

/// A resource name refused at creation names the argument, repeats the name
/// and says which kind of resource it was for.
#[test]
fn a_refused_resource_name_names_its_argument_and_value_in_json() {
    let dir = tempfile::tempdir().unwrap();
    let v = json_refusal(dir.path(), &["module", "create", "my mod"]);
    assert_eq!(
        v,
        serde_json::json!({
            "error": "invalid_argument",
            "name": "my mod",
            "flag": "<NAME>",
            "value": "my mod",
            "resource": "module",
        })
    );
}

/// A config document whose `spec` is `spec_tail`, beside the default profile.
fn write_config_with_spec(dir: &std::path::Path, spec_tail: &str) -> std::path::PathBuf {
    create_valid_config(dir);
    let config = dir.join("cfgd.yaml");
    std::fs::write(
        &config,
        format!("apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\n{spec_tail}"),
    )
    .unwrap();
    config
}

/// A bare `spec:` is a spec with nothing in it yet, so a verb writing under it
/// creates the sections it writes to, as `config set` does.
#[test]
fn module_registry_add_writes_under_a_bare_spec() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config_with_spec(dir.path(), "spec:\n");
    let out = cfgd_bin()
        .unwrap()
        .args([
            "module",
            "registry",
            "add",
            "https://github.com/example/mods.git",
        ])
        .arg("--config")
        .arg(&config)
        .args(["-o", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    let v = parse_single_json(&stdout);
    assert_eq!(v["name"], "example", "{v}");

    let written: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    assert_eq!(
        written["spec"]["modules"]["registries"][0]["url"],
        "https://github.com/example/mods.git"
    );
}

/// A `spec` holding something other than a mapping is refused as a document
/// contradicting its schema, and the refusal says what the document holds.
#[test]
fn a_spec_that_is_not_a_mapping_is_refused_naming_what_it_holds() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config_with_spec(dir.path(), "spec: 3\n");
    let before = std::fs::read_to_string(&config).unwrap();
    let args = [
        "module",
        "registry",
        "add",
        "https://github.com/example/mods.git",
    ];

    let v = json_refusal_against(&config, &args);
    let path = cfgd_core::to_posix_string(&config);
    assert_eq!(
        v,
        serde_json::json!({ "error": "parse_failed", "name": &path, "path": &path })
    );

    let (_, stderr, code) = run(&[&args[..], &["--config", config.to_str().unwrap()]].concat());
    assert_eq!(code, Some(1), "{stderr}");
    assert!(
        stderr.contains("'spec' holds a scalar where a mapping belongs"),
        "the refusal names what the document holds: {stderr}"
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
}

/// A config holding one source, `s`, whose entry ends on `subscription`.
fn write_config_with_source(dir: &std::path::Path, subscription: &str) -> std::path::PathBuf {
    write_config_with_spec(
        dir,
        &format!(
            "spec:\n  sources:\n  - name: s\n    origin:\n      type: Git\n      url: https://example.com/x.git\n{subscription}"
        ),
    )
}

/// `cfgd source priority` against `config`, as its `-o json` payload and exit.
fn source_priority_json(
    config: &std::path::Path,
    args: &[&str],
) -> (serde_json::Value, Option<i32>) {
    let out = cfgd_bin()
        .unwrap()
        .args(["source", "priority"])
        .args(args)
        .arg("--config")
        .arg(config)
        .args(["-o", "json"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    (parse_single_json(&stdout), out.status.code())
}

/// A source whose `subscription` is absent, bare or `null` holds the default
/// block, so a new priority is written into it, and the file it leaves loads
/// and reports that priority. A bare block used to be rewritten as `null` with
/// the priority dropped, and every later command refused the file.
#[test]
fn source_priority_writes_into_an_absent_or_bare_subscription_and_the_file_still_loads() {
    for (case, block) in [
        ("absent", ""),
        ("bare", "    subscription:\n"),
        ("null", "    subscription: null\n"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let config = write_config_with_source(dir.path(), block);

        let (v, code) = source_priority_json(&config, &["s", "7"]);
        assert_eq!(code, Some(0), "{case}: {v}");
        assert_eq!(
            v,
            serde_json::json!({ "name": "s", "priority": 7, "previousPriority": 500 }),
            "{case}"
        );

        let written: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            written["spec"]["sources"][0]["subscription"]["priority"], 7,
            "{case}: the file records the priority: {written:?}"
        );

        let (v, code) = source_priority_json(&config, &["s"]);
        assert_eq!(code, Some(0), "{case}: the written file loads: {v}");
        assert_eq!(
            v,
            serde_json::json!({ "name": "s", "priority": 7 }),
            "{case}"
        );
    }
}

/// A source whose `sync` block is written `null` loads with the default block,
/// as a bare `sync:` beside it does, so `source list` reports the source.
#[test]
fn a_source_whose_sync_block_is_null_loads_and_lists() {
    for (case, block) in [("bare", "    sync:\n"), ("null", "    sync: null\n")] {
        let dir = tempfile::tempdir().unwrap();
        let config = write_config_with_source(dir.path(), block);
        let out = cfgd_bin()
            .unwrap()
            .args(["source", "list", "--config"])
            .arg(&config)
            .args(["-o", "json"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{case}: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v = parse_single_json(&stdout);
        assert_eq!(v[0]["name"], "s", "{case}: {v}");
        assert_eq!(v[0]["priority"], 500, "{case}: {v}");
    }
}

/// A config whose `spec.aliases` is written bare or `null` loads, so a
/// command reading it runs.
#[test]
fn a_config_whose_aliases_are_null_loads_and_lists() {
    for (case, block) in [("bare", "  aliases:\n"), ("null", "  aliases: null\n")] {
        let dir = tempfile::tempdir().unwrap();
        let config = write_config_with_source(dir.path(), block);
        let out = cfgd_bin()
            .unwrap()
            .args(["source", "list", "--config"])
            .arg(&config)
            .args(["-o", "json"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{case}: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            parse_single_json(&stdout)[0]["name"],
            "s",
            "{case}: {stdout}"
        );
    }
}

/// An out-of-range priority given to `source priority` is refused as the
/// `[VALUE]` positional its `--help` prints, the argument the invocation
/// actually carried.
#[test]
fn an_out_of_range_source_priority_names_its_positional_in_json() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config_with_source(dir.path(), "");
    let before = std::fs::read_to_string(&config).unwrap();

    let (v, code) = source_priority_json(&config, &["s", "4294967295"]);
    assert_eq!(code, Some(1), "{v}");
    assert_eq!(
        v,
        serde_json::json!({
            "error": "invalid_argument",
            "name": "[VALUE]",
            "flag": "[VALUE]",
            "value": "4294967295",
        })
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), before);
}

/// The `-o json` payload a refusal leaves on stdout, run against `config` as
/// it stands.
fn json_refusal_against(config: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let out = cfgd_bin()
        .unwrap()
        .args(args)
        .arg("--config")
        .arg(config)
        .args(["-o", "json"])
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0), "{args:?} must refuse");
    parse_single_json(&String::from_utf8_lossy(&out.stdout))
}

/// A secret verb whose backend cannot run names the file it was asked about
/// and why the backend cannot run.
#[test]
fn a_secret_verb_whose_backend_is_not_installed_is_backend_unavailable_in_json() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config_with_spec(dir.path(), "spec:\n  secrets:\n    backend: sops\n");
    let target = dir.path().join("present.yaml");
    std::fs::write(&target, "k: v\n").unwrap();
    // A PATH holding no executables, handed to the child alone, so sops is
    // missing whatever the host has installed.
    let no_tools = tempfile::tempdir().unwrap();

    let out = cfgd_bin()
        .unwrap()
        .env("PATH", no_tools.path())
        .args(["secret", "encrypt"])
        .arg(&target)
        .arg("--config")
        .arg(&config)
        .args(["-o", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let path = cfgd_core::to_posix_string(&target);
    assert_eq!(
        parse_single_json(&String::from_utf8_lossy(&out.stdout)),
        serde_json::json!({
            "error": "backend_unavailable",
            "name": &path,
            "path": &path,
            "detail": "sops: not installed",
        })
    );
}

/// A config that cannot be parsed reaches `-o json` as the `config` domain,
/// ahead of any question about the file or the backend.
#[test]
fn a_secret_verb_over_an_unparseable_config_is_the_config_domain_in_json() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config_with_spec(dir.path(), "spec: [\n");
    let target = dir.path().join("present.yaml");
    std::fs::write(&target, "k: v\n").unwrap();

    let v = json_refusal_against(&config, &["secret", "encrypt", target.to_str().unwrap()]);
    assert_eq!(v["error"], "config", "{v}");
}

/// `cfgd` with no home directory to resolve, so a `~` in the config path
/// stays a literal `~`.
fn run_homeless(args: &[&str]) -> (String, String, Option<i32>) {
    let out = cfgd_bin()
        .unwrap()
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .env_remove("XDG_CONFIG_HOME")
        .args(args)
        .output()
        .unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code(),
    )
}

fn home_unresolved() -> String {
    format!(
        "cannot resolve home directory ({} unset) to locate config at",
        cfgd_core::HOME_ENV_VARS
    )
}

/// A `--config` under `~` with no home set is refused as an unset home,
/// naming the path as written, under table and json output alike.
#[test]
fn a_tilde_config_with_no_home_reports_the_unset_home() {
    let (_, stderr, code) = run_homeless(&["status", "--config", "~/cfgd.yaml"]);
    assert_eq!(
        code,
        Some(3),
        "an unresolvable home exits NoConfig(3): {stderr}"
    );
    assert!(
        stderr.contains(&format!("{} ~/cfgd.yaml", home_unresolved())),
        "stderr: {stderr:?}"
    );

    let (stdout, _, code) = run_homeless(&["status", "--config", "~/cfgd.yaml", "-o", "json"]);
    assert_eq!(
        code,
        Some(3),
        "an unresolvable home exits NoConfig(3): {stdout}"
    );
    let v = parse_single_json(&stdout);
    assert_eq!(v["error"], "config", "payload: {v}");
    assert!(
        v["message"]
            .as_str()
            .is_some_and(|m| m.contains(&format!("{} ~/cfgd.yaml", home_unresolved()))),
        "payload: {v}"
    );
}

/// The verbs that check for the config file before reading it (one caller
/// of each family besides the `config` verbs) report a `~` path with no home
/// set the way `status` does: the unset home, under table
/// and json output alike.
#[test]
fn every_config_verb_reports_a_tilde_config_with_no_home_as_the_unset_home() {
    let verbs: [&[&str]; 8] = [
        &["config", "show"],
        &["config", "get", "profile"],
        &["config", "set", "profile", "work"],
        &["config", "unset", "theme.name"],
        &["config", "edit"],
        &["config", "migrate"],
        &["profile", "switch", "work"],
        &["module", "registry", "remove", "community"],
    ];
    for verb in verbs {
        let mut args = verb.to_vec();
        args.extend(["--config", "~/cfgd.yaml"]);
        let (_, stderr, code) = run_homeless(&args);
        assert_eq!(code, Some(3), "{verb:?}: exits NoConfig(3): {stderr}");
        assert!(
            stderr.contains(&format!("{} ~/cfgd.yaml", home_unresolved())),
            "{verb:?}: {stderr:?}"
        );

        args.extend(["-o", "json"]);
        let (stdout, _, code) = run_homeless(&args);
        assert_eq!(code, Some(3), "{verb:?}: exits NoConfig(3): {stdout}");
        let v = parse_single_json(&stdout);
        assert_eq!(v["error"], "config", "{verb:?}: {v}");
        assert!(
            v["message"]
                .as_str()
                .is_some_and(|m| m.contains(&format!("{} ~/cfgd.yaml", home_unresolved()))),
            "{verb:?}: {v}"
        );
    }
}

/// `doctor` reports a `~` config path with no home set as the unset home on
/// its config-file row, and never as a path that does not exist.
#[test]
fn doctor_reports_a_tilde_config_with_no_home_as_the_unset_home() {
    let (_, stderr, _) = run_homeless(&["doctor", "--config", "~/cfgd.yaml"]);
    let row = stderr
        .lines()
        .find(|line| line.contains("Config file"))
        .unwrap_or_else(|| panic!("no config-file row: {stderr}"));
    assert!(
        row.contains(&format!("{} ~/cfgd.yaml", home_unresolved())),
        "{row:?}"
    );

    let (stdout, _, _) = run_homeless(&["doctor", "--config", "~/cfgd.yaml", "-o", "json"]);
    let v = parse_single_json(&stdout);
    assert_eq!(v["config"]["valid"], false, "{v}");
    assert!(
        v["config"]["error"]
            .as_str()
            .is_some_and(|e| e.contains(&format!("{} ~/cfgd.yaml", home_unresolved()))),
        "{v}"
    );
}

/// `doctor` with no home set and no `--config` fails its config check on the
/// unset home and exits 1: every verb reading the config fails there, so a
/// passing verdict would let `cfgd doctor && cfgd apply` reach a broken apply.
/// An `XDG_CONFIG_HOME` or systemd's `CONFIGURATION_DIRECTORY` places the
/// default path without a home, so there the missing config is the
/// fresh-machine warning and the verdict passes. Those two arms pass only
/// because `cfgd_bin()` points `CFGD_STATE_DIR` and `CFGD_CACHE_DIR` at the
/// test's own directory: the verdict also needs the state store and the cache,
/// which with no home resolve nowhere else
/// (`doctor_and_apply_both_refuse_a_state_store_with_no_home`). Every OS takes
/// its default config root from the same home, Windows included.
#[test]
fn doctor_with_no_home_fails_the_default_config_check_on_the_unset_home() {
    let config_row = |stderr: &str| {
        stderr
            .lines()
            .find(|line| line.contains("Config file"))
            .unwrap_or_else(|| panic!("no config-file row: {stderr}"))
            .to_string()
    };
    let (_, stderr, code) = run_homeless(&["doctor"]);
    let row = config_row(&stderr);
    assert!(row.contains(&home_unresolved()), "{row:?}");
    assert_eq!(code, Some(1), "{stderr}");

    let xdg = tempfile::tempdir().unwrap();
    let out = cfgd_bin()
        .unwrap()
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .env("XDG_CONFIG_HOME", xdg.path())
        .arg("doctor")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let row = config_row(&stderr);
    assert!(row.contains("not found; run `cfgd init`"), "{row:?}");
    assert!(!row.contains(&home_unresolved()), "{row:?}");
    assert_eq!(out.status.code(), Some(0), "{stderr}");

    let systemd = tempfile::tempdir().unwrap();
    let out = cfgd_bin()
        .unwrap()
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .env_remove("XDG_CONFIG_HOME")
        .env("CONFIGURATION_DIRECTORY", systemd.path())
        .arg("doctor")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let row = config_row(&stderr);
    assert!(row.contains("not found; run `cfgd init`"), "{row:?}");
    assert!(!row.contains(&home_unresolved()), "{row:?}");
    assert_eq!(out.status.code(), Some(0), "{stderr}");
}

/// A machine with no home, no `CFGD_STATE_DIR` and no `XDG_STATE_HOME`
/// has nowhere to keep its state store, so `apply` refuses before it does
/// anything. `doctor` opens the same store, so it fails its verdict there too,
/// on a Fail row naming why and on `stateStore` in its `-o json` payload: a
/// passing doctor would let `cfgd doctor && cfgd apply` reach that refusal.
/// The config itself resolves through `XDG_CONFIG_HOME`, so the store is the
/// only thing wrong.
#[test]
fn doctor_and_apply_both_refuse_a_state_store_with_no_home() {
    let xdg = tempfile::tempdir().unwrap();
    create_valid_config(&xdg.path().join("cfgd"));
    let homeless = |args: &[&str]| {
        cfgd_bin()
            .unwrap()
            .env_remove("HOME")
            .env_remove("USERPROFILE")
            .env_remove(cfgd_core::CFGD_STATE_DIR_ENV)
            .env_remove("XDG_STATE_HOME")
            .env("XDG_CONFIG_HOME", xdg.path())
            .args(args)
            .output()
            .unwrap()
    };

    let out = homeless(&["doctor"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let row = stderr
        .lines()
        .find(|line| line.contains("State store"))
        .unwrap_or_else(|| panic!("no state-store row: {stderr}"));
    assert!(
        row.contains("✗ State store: unavailable") && row.contains("no home directory found"),
        "{row:?}"
    );
    assert_eq!(out.status.code(), Some(1), "{stderr}");

    let out = homeless(&["doctor", "-o", "json"]);
    let v = parse_single_json(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(v["stateStore"]["accessible"], false, "{v}");
    assert!(
        v["stateStore"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("no home directory found")),
        "{v}"
    );
    assert_eq!(out.status.code(), Some(1), "{v}");

    let out = homeless(&["apply", "--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no home directory found"), "{stderr}");
    assert_eq!(out.status.code(), Some(1), "{stderr}");
}

/// With no home, an absolute `XDG_STATE_HOME` places the state store and an
/// absolute `XDG_CACHE_HOME` the cache, the way an absolute `XDG_CONFIG_HOME`
/// places the config, so `doctor` passes without creating the store, `apply`
/// runs and creates it at `$XDG_STATE_HOME/cfgd/state.db`, and a second
/// `doctor` opens that store. `CFGD_CACHE_DIR` is removed because it outranks
/// `XDG_CACHE_HOME` and `cfgd_bin()` sets it.
#[test]
fn doctor_and_apply_both_place_the_state_store_under_xdg_state_home_with_no_home() {
    let xdg = tempfile::tempdir().unwrap();
    let config_home = xdg.path().join("config");
    let state_home = xdg.path().join("state");
    let cache_home = xdg.path().join("cache");
    create_valid_config(&config_home.join("cfgd"));
    let homeless = |args: &[&str]| {
        cfgd_bin()
            .unwrap()
            .env_remove("HOME")
            .env_remove("USERPROFILE")
            .env_remove(cfgd_core::CFGD_STATE_DIR_ENV)
            .env_remove(cfgd_core::CFGD_CACHE_DIR_ENV)
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_STATE_HOME", &state_home)
            .env("XDG_CACHE_HOME", &cache_home)
            .args(args)
            .output()
            .unwrap()
    };

    let db = state_home.join("cfgd").join("state.db");
    let out = homeless(&["doctor"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("✓ State store: not created yet"),
        "{stderr}"
    );
    assert!(stderr.contains("✓ Cache directory: accessible"), "{stderr}");
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(!db.exists(), "doctor created {}", db.display());

    let out = homeless(&["apply", "--yes"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(db.is_file(), "no state store at {}: {stderr}", db.display());

    let out = homeless(&["doctor"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("✓ State store: accessible"), "{stderr}");
    assert_eq!(out.status.code(), Some(0), "{stderr}");
}

/// With no home and no `XDG_CONFIG_HOME` there is no default config directory,
/// so a `--config` naming an absolute file is not the machine's own config and
/// nothing about it needs the home. `apply` and `plan` of an empty profile both
/// run, and `doctor` gives the same verdict, with the state store and cache
/// placed by `XDG_STATE_HOME` and `XDG_CACHE_HOME`.
#[test]
fn an_explicit_config_with_no_home_and_no_default_config_dir_runs() {
    let xdg = tempfile::tempdir().unwrap();
    let config_dir = xdg.path().join("elsewhere");
    create_valid_config(&config_dir);
    let config = config_dir.join("cfgd.yaml");
    let homeless = |args: &[&str]| {
        cfgd_bin()
            .unwrap()
            .env_remove("HOME")
            .env_remove("USERPROFILE")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove(cfgd_core::CFGD_STATE_DIR_ENV)
            .env_remove(cfgd_core::CFGD_CACHE_DIR_ENV)
            .env("XDG_STATE_HOME", xdg.path().join("state"))
            .env("XDG_CACHE_HOME", xdg.path().join("cache"))
            .arg("--config")
            .arg(&config)
            .args(args)
            .output()
            .unwrap()
    };
    for verb in [&["doctor"][..], &["plan"][..], &["apply", "--yes"][..]] {
        let out = homeless(verb);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{verb:?}: {stderr}");
    }
}

/// With no home, an absolute `XDG_STATE_HOME` and no `XDG_CACHE_HOME`, the
/// state store has a place and the module cache has none. `doctor` fails its
/// verdict on the Cache row without creating the store, and `apply` and `plan`
/// refuse before opening it, so no run leaves a `state.db` behind for a later
/// one to find.
#[test]
fn doctor_apply_and_plan_refuse_a_cache_with_no_home_before_creating_state() {
    let xdg = tempfile::tempdir().unwrap();
    let config_home = xdg.path().join("config");
    let state_home = xdg.path().join("state");
    create_valid_config(&config_home.join("cfgd"));
    let homeless = |args: &[&str]| {
        cfgd_bin()
            .unwrap()
            .env_remove("HOME")
            .env_remove("USERPROFILE")
            .env_remove(cfgd_core::CFGD_STATE_DIR_ENV)
            .env_remove(cfgd_core::CFGD_CACHE_DIR_ENV)
            .env_remove("XDG_CACHE_HOME")
            .env("XDG_CONFIG_HOME", &config_home)
            .env("XDG_STATE_HOME", &state_home)
            .args(args)
            .output()
            .unwrap()
    };
    let refusal = "cannot locate the per-user cache directory: no home directory found";

    let db = state_home.join("cfgd").join("state.db");
    let out = homeless(&["doctor"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let row = stderr
        .lines()
        .find(|line| line.contains("Cache directory"))
        .unwrap_or_else(|| panic!("no cache row: {stderr}"));
    assert!(row.contains("✗ Cache directory: unavailable"), "{row:?}");
    assert!(stderr.contains(refusal), "{stderr}");
    assert_eq!(out.status.code(), Some(1), "{stderr}");

    let out = homeless(&["doctor", "-o", "json"]);
    let v = parse_single_json(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(v["cacheDir"]["accessible"], false, "{v}");
    assert!(
        v["cacheDir"]["message"]
            .as_str()
            .is_some_and(|m| m.contains(refusal)),
        "{v}"
    );
    assert_eq!(out.status.code(), Some(1), "{v}");
    assert!(
        !db.exists(),
        "doctor left a state store at {}",
        db.display()
    );

    for verb in [&["apply", "--yes"][..], &["plan"][..]] {
        let out = homeless(verb);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_ne!(out.status.code(), Some(0), "{verb:?}: {stderr}");
        assert!(stderr.contains(refusal), "{verb:?}: {stderr}");
        assert!(
            !db.exists(),
            "{verb:?}: the refusal left a state store at {}",
            db.display()
        );
    }
}

/// `doctor` is a diagnostic: on a machine no `apply` has run on, the state
/// store row passes as not created yet (the first `apply` creates it), and the
/// run leaves no `state.db` behind, under table and json output alike.
#[test]
fn doctor_on_a_fresh_home_reports_the_store_not_created_and_creates_none() {
    let root = tempfile::tempdir().unwrap();
    let state_dir = root.path().join("state");
    let doctor = |args: &[&str]| {
        cfgd_bin()
            .unwrap()
            .env(cfgd_core::CFGD_STATE_DIR_ENV, &state_dir)
            .args(args)
            .output()
            .unwrap()
    };
    let db = state_dir.join("state.db");

    let out = doctor(&["doctor"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let row = stderr
        .lines()
        .find(|line| line.contains("State store"))
        .unwrap_or_else(|| panic!("no state store row: {stderr}"));
    assert!(row.contains("✓ State store: not created yet"), "{row:?}");
    assert!(row.contains("`cfgd apply` creates it"), "{row:?}");
    assert!(!db.exists(), "doctor created {}: {stderr}", db.display());

    let out = doctor(&["doctor", "-o", "json"]);
    let v = parse_single_json(&String::from_utf8_lossy(&out.stdout));
    assert_eq!(v["stateStore"]["accessible"], true, "{v}");
    assert_eq!(v["stateStore"]["created"], false, "{v}");
    assert!(!db.exists(), "doctor -o json created {}", db.display());
}

/// Every file under the directory `cfgd_bin()` roots the command's home, XDG
/// and `CFGD_*` directories in, relative to it.
fn files_under_isolated_root(
    cmd: &std::process::Command,
) -> std::collections::BTreeSet<std::path::PathBuf> {
    let root = home_of(cmd).parent().unwrap().to_path_buf();
    let mut found = std::collections::BTreeSet::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()));
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                found.insert(path.strip_prefix(&root).unwrap().to_path_buf());
            }
        }
    }
    found
}

/// `doctor` on a home no `cfgd` has run under writes no file anywhere under the
/// home, XDG or `CFGD_*` directories the run resolves: no state store, no
/// update check timestamp and no cache entry. The update check is left on,
/// with every opt-out variable the host may export removed, and pointed at a
/// port nothing listens on, since a failed check records its timestamp too.
#[test]
fn doctor_on_a_fresh_home_writes_no_file_anywhere() {
    let probe = cfgd_bin().unwrap();
    create_valid_config(&home_of(&probe).join(".config").join("cfgd"));
    let root = home_of(&probe).parent().unwrap().to_path_buf();
    let cases = [&["doctor"][..], &["doctor", "-o", "json"][..]];
    // Each case gets a state directory of its own, since every spawn in one
    // test shares the isolated one. They sit under the walked root, so a
    // store either case creates still shows.
    for case in 0..cases.len() {
        std::fs::create_dir_all(root.join(format!("state-{case}"))).unwrap();
    }
    let before = files_under_isolated_root(&probe);

    for (case, args) in cases.into_iter().enumerate() {
        let mut cmd = cfgd_bin().unwrap();
        cmd.env(
            cfgd_core::CFGD_STATE_DIR_ENV,
            root.join(format!("state-{case}")),
        );
        for var in cfgd_core::upgrade::OPTOUT_VARS {
            cmd.env_remove(var);
        }
        cmd.env(cfgd_core::CFGD_GITHUB_API_BASE_ENV, "http://127.0.0.1:1");
        let out = cmd.args(args).output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        let after = files_under_isolated_root(&cmd);
        let written: Vec<_> = after.difference(&before).collect();
        assert!(written.is_empty(), "{args:?} wrote {written:?}: {stderr}");
    }
}

/// A store whose `-wal` and `-shm` sidecars are gone (a clean close removes
/// both) is opened by `doctor` without bringing either back.
#[test]
fn doctor_opens_a_closed_wal_store_without_recreating_its_sidecars() {
    let mut apply = cfgd_bin().unwrap();
    create_valid_config(&home_of(&apply).join(".config").join("cfgd"));
    let out = apply.args(["apply", "--yes"]).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let state_dir = env_path(&apply, cfgd_core::CFGD_STATE_DIR_ENV);
    let db = state_dir.join("state.db");
    assert!(db.is_file(), "apply created no store at {}", db.display());
    let sidecars = [
        state_dir.join("state.db-wal"),
        state_dir.join("state.db-shm"),
    ];
    for sidecar in &sidecars {
        let _ = std::fs::remove_file(sidecar);
    }

    let out = cfgd_bin().unwrap().arg("doctor").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("✓ State store: accessible"), "{stderr}");
    for sidecar in &sidecars {
        assert!(
            !sidecar.exists(),
            "doctor recreated {}: {stderr}",
            sidecar.display()
        );
    }
}

/// The state directory's three store files and its mtime, which moves with
/// any file created or removed in it.
type StoreSnapshot = ([Option<Vec<u8>>; 3], std::time::SystemTime);

fn store_snapshot(state_dir: &std::path::Path) -> StoreSnapshot {
    (
        ["state.db", "state.db-wal", "state.db-shm"].map(|f| read_store_file(&state_dir.join(f))),
        std::fs::metadata(state_dir).unwrap().modified().unwrap(),
    )
}

/// The bytes of the store file at `path`, or `None` when there is none.
///
/// On Unix a child process reads it. Closing a descriptor drops every POSIX
/// lock its process holds on the file, so a read here would drop the lock a
/// store held open by this process keeps on its `-shm`, and the next
/// connection, finding no other holder, would rebuild that file.
fn read_store_file(path: &std::path::Path) -> Option<Vec<u8>> {
    if !path.exists() {
        return None;
    }
    #[cfg(unix)]
    {
        let _path = cfgd_core::test_helpers::path_env_read_guard();
        let out = std::process::Command::new("cat")
            .arg(path)
            .output()
            .unwrap();
        assert!(out.status.success(), "cat {}", path.display());
        Some(out.stdout)
    }
    #[cfg(not(unix))]
    {
        Some(std::fs::read(path).unwrap())
    }
}

/// `doctor` against a config behind the schema reports the fields it lacks
/// and writes nothing, under every policy a run can bring: `--yes`,
/// `CFGD_YES`, `--migration-policy update`, and a prompt with no terminal to
/// ask on. The config keeps its bytes; no store is created; an existing store
/// an older cfgd left keeps its bytes, its sidecars and its schema version,
/// open or closed.
#[test]
fn doctor_against_a_behind_config_writes_neither_the_config_nor_the_store() {
    let runs: [(&str, &[&str], bool); 4] = [
        ("--yes", &["--yes", "doctor"], false),
        (cfgd_core::CFGD_YES_ENV, &["doctor"], true),
        ("update", &["--migration-policy", "update", "doctor"], false),
        ("prompt", &["doctor"], false),
    ];
    for store in ["none", "closed", "open"] {
        for (name, args, yes_env) in runs {
            let case = format!("{name} over store {store}");
            let mut cmd = cfgd_bin().unwrap();
            let config_dir = home_of(&cmd).join(".config").join("cfgd");
            create_valid_config(&config_dir);
            let config = config_dir.join("cfgd.yaml");
            let document = std::fs::read(&config).unwrap();
            // Each case gets a state directory of its own, since every spawn
            // in one test shares the isolated one.
            let state = tempfile::tempdir().unwrap();
            let db = state.path().join("state.db");
            let held = (store != "none").then(|| schema_27_store(&db));
            let held = held.filter(|_| store == "open");
            let before = store_snapshot(state.path());

            cmd.env(cfgd_core::CFGD_STATE_DIR_ENV, state.path())
                .arg("--config")
                .arg(&config)
                .args(args);
            if yes_env {
                cmd.env(cfgd_core::CFGD_YES_ENV, "1");
            }
            let out = cmd.output().unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains("This config does not declare 2 fields this build reads"),
                "{case}: {stderr}"
            );
            assert_eq!(
                std::fs::read(&config).unwrap(),
                document,
                "{case}: {stderr}"
            );
            let after = store_snapshot(state.path());
            // The files alone: doctor probes the directory for write access,
            // which moves its mtime and leaves nothing behind.
            assert!(
                before.0 == after.0,
                "{case}: doctor changed the store: {stderr}"
            );
            drop(held);
            if store != "none" {
                assert_eq!(schema_version_of(&db), 27, "{case}");
            }
        }
    }
}

/// A store copied with its `-wal` file and without its `-shm` file cannot be
/// read without creating the `-shm`. With no home, `apply` refuses naming the
/// store, `doctor` reports it unavailable, and neither creates the file.
#[test]
fn a_wal_store_missing_its_shm_is_refused_with_no_home_and_left_without_one() {
    for args in [&["apply", "--yes"][..], &["doctor"]] {
        let live = tempfile::tempdir().unwrap();
        let writer = cfgd_core::state::StateStore::open(&live.path().join("state.db")).unwrap();
        writer
            .record_bootstrapped_path_dirs("brew", &["/opt/brew/bin".to_string()])
            .unwrap();
        // Each case gets a state directory of its own, since every spawn in
        // one test shares the isolated one.
        let state = tempfile::tempdir().unwrap();
        for name in ["state.db", "state.db-wal"] {
            // Read and written, since a copy of a file another handle holds
            // open for writing is a sharing violation on Windows.
            let bytes = std::fs::read(live.path().join(name)).unwrap();
            std::fs::write(state.path().join(name), bytes).unwrap();
        }
        drop(writer);
        let before = store_snapshot(state.path());

        let mut cmd = cfgd_bin().unwrap();
        let config_dir = home_of(&cmd).join(".config").join("cfgd");
        create_valid_config(&config_dir);
        let out = cmd
            .env_remove("HOME")
            .env_remove("USERPROFILE")
            .env(cfgd_core::CFGD_STATE_DIR_ENV, state.path())
            .arg("--config")
            .arg(config_dir.join("cfgd.yaml"))
            .args(args)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_ne!(out.status.code(), Some(0), "{args:?}: {stderr}");
        if args[0] == "apply" {
            assert!(
                stderr.contains("state.db without writing to it"),
                "{args:?}: {stderr}"
            );
        } else {
            assert!(
                stderr.contains("State store: unavailable"),
                "{args:?}: {stderr}"
            );
        }
        let after = store_snapshot(state.path());
        assert!(before.0 == after.0, "{args:?}: the run changed the store");
        // `doctor` probes the directory for write access, which moves its
        // mtime and leaves nothing behind, so only `apply` is held to it.
        if args[0] == "apply" {
            assert_eq!(
                before.1, after.1,
                "{args:?}: the run created a file: {stderr}"
            );
        }
    }
}

/// Run `cmd` on a pseudo-terminal, which is its controlling terminal and its
/// stdin, stdout and stderr, typing `answer` once `prompt` appears in what
/// it writes. Returns the exit code and everything it wrote.
#[cfg(unix)]
fn run_on_a_terminal(
    mut cmd: std::process::Command,
    prompt: &str,
    answer: &[u8],
) -> (Option<i32>, String) {
    use std::io::{Read, Write};
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::os::unix::process::CommandExt;
    use std::sync::{Arc, Mutex};

    let (mut master, mut slave) = (0, 0);
    // SAFETY: openpty writes two new descriptors into the integers it is
    // handed; the null pointers ask for no name and the default settings.
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            // `*mut` coerces to the `*const` Linux declares and is what macOS
            // declares, so one spelling compiles on both.
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(opened, 0, "openpty: {}", std::io::Error::last_os_error());
    // A child another test spawns meanwhile would otherwise inherit the
    // terminal end and hold it open, and the reader below would wait for that
    // child too.
    for fd in [master, slave] {
        // SAFETY: fcntl on a descriptor this function owns.
        unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    // SAFETY: both descriptors were just opened and nothing else owns them.
    let (mut master, slave) = unsafe {
        (
            std::fs::File::from_raw_fd(master),
            OwnedFd::from_raw_fd(slave),
        )
    };
    cmd.stdin(slave.try_clone().unwrap())
        .stdout(slave.try_clone().unwrap())
        .stderr(slave);
    // SAFETY: setsid and ioctl are async-signal-safe, and the closure touches
    // nothing but the child's own session and descriptor 0.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();
    // The parent's copies of the terminal end close here, so the reader sees
    // the end of output once the child exits.
    drop(cmd);

    let written = Arc::new(Mutex::new(Vec::new()));
    let reader = {
        let written = Arc::clone(&written);
        let mut master = master.try_clone().unwrap();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n @ 1..) = master.read(&mut chunk) {
                written.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        })
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let text = String::from_utf8_lossy(&written.lock().unwrap()).into_owned();
        if text.contains(prompt) {
            // The prompt is drawn as it starts reading keys; the pause lets
            // it finish that before the answer arrives.
            std::thread::sleep(std::time::Duration::from_millis(200));
            master.write_all(answer).unwrap();
            break;
        }
        if std::time::Instant::now() > deadline || child.try_wait().unwrap().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let status = child.wait().unwrap();
    reader.join().unwrap();
    let text = String::from_utf8_lossy(&written.lock().unwrap()).into_owned();
    (status.code(), text)
}

/// With no home, a run whose desired state writes under `~/` refuses. Asked
/// the migration prompt on a terminal first and answered no, it still
/// records nothing: no store is created, and an existing one keeps its
/// bytes, its sidecars and its schema version, because a run refused for
/// want of the home drops the answer it was given.
#[cfg(unix)]
#[test]
fn a_homeless_run_answering_the_migration_prompt_and_refused_records_nothing() {
    for store in ["none", "older closed", "current open"] {
        let mut cmd = cfgd_bin().unwrap();
        let config_dir = home_of(&cmd).join(".config").join("cfgd");
        create_valid_config(&config_dir);
        std::fs::write(
            config_dir.join("profiles/base.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();
        let config = config_dir.join("cfgd.yaml");
        let document = std::fs::read(&config).unwrap();
        // Each case gets a state directory of its own, since every spawn in
        // one test shares the isolated one.
        let state = tempfile::tempdir().unwrap();
        let db = state.path().join("state.db");
        let held = match store {
            "older closed" => {
                drop(schema_27_store(&db));
                None
            }
            "current open" => Some(cfgd_core::state::StateStore::open(&db).unwrap()),
            _ => None,
        };
        let version = (store != "none").then(|| schema_version_of(&db));
        let before = store_snapshot(state.path());

        cmd.env_remove("HOME")
            .env_remove("USERPROFILE")
            .env(cfgd_core::CFGD_STATE_DIR_ENV, state.path())
            .arg("--config")
            .arg(&config)
            .arg("apply");
        let (code, text) = run_on_a_terminal(cmd, MIGRATION_PROMPT, b"n\r");
        assert!(text.contains(MIGRATION_PROMPT), "{store}: {text}");
        assert_eq!(code, Some(1), "{store}: {text}");
        assert!(
            text.contains("cannot write the managed env file"),
            "{store}: {text}"
        );
        assert_eq!(std::fs::read(&config).unwrap(), document, "{store}: {text}");
        let after = store_snapshot(state.path());
        assert!(
            before.0 == after.0,
            "{store}: the refused run changed the store"
        );
        assert_eq!(before.1, after.1, "{store}: the refused run created a file");
        drop(held);
        if store == "none" {
            assert!(!db.exists(), "{store}: the refused run left a store");
        } else {
            assert_eq!(Some(schema_version_of(&db)), version, "{store}");
        }
    }
}

/// The question the migration gate asks of a config behind by the two fields
/// [`create_valid_config`] leaves out.
#[cfg(unix)]
const MIGRATION_PROMPT: &str = "Add 2 fields this build reads";

/// Run `args` with no home against `config` and the state root `state` on a
/// terminal, answering no if the migration prompt is asked.
#[cfg(unix)]
fn run_homeless_on_a_terminal(
    config: &std::path::Path,
    state: &std::path::Path,
    args: &[&str],
) -> (Option<i32>, String) {
    let mut cmd = cfgd_bin().unwrap();
    cmd.env_remove("HOME")
        .env_remove("USERPROFILE")
        .env(cfgd_core::CFGD_STATE_DIR_ENV, state)
        .arg("--config")
        .arg(config)
        .args(args);
    run_on_a_terminal(cmd, MIGRATION_PROMPT, b"n\r")
}

/// With no home, a verb that succeeds without the run's own store handle
/// (`log` opens the store itself) records the answer it was given: the next
/// run is not asked again.
#[cfg(unix)]
#[test]
fn a_homeless_run_that_succeeds_records_its_migration_answer() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let config = dir.path().join("cfgd.yaml");
    let state = tempfile::tempdir().unwrap();

    let (code, text) = run_homeless_on_a_terminal(&config, state.path(), &["log"]);
    assert!(text.contains(MIGRATION_PROMPT), "{text}");
    assert_eq!(code, Some(0), "{text}");

    let (code, text) = run_homeless_on_a_terminal(&config, state.path(), &["log"]);
    assert_eq!(code, Some(0), "{text}");
    assert!(
        !text.contains(MIGRATION_PROMPT),
        "the recorded answer was not read: {text}"
    );
}

/// With no home, a verb refused for want of it records nothing: no store is
/// created, and the next run is asked again. `status`, `diff` and `verify`
/// all need the home for their env check, whatever the profile declares.
#[cfg(unix)]
#[test]
fn a_homeless_run_refused_for_the_home_records_no_migration_answer() {
    for verb in ["status", "diff", "verify"] {
        let dir = tempfile::tempdir().unwrap();
        create_valid_config(dir.path());
        let config = dir.path().join("cfgd.yaml");
        let state = tempfile::tempdir().unwrap();

        let (code, text) = run_homeless_on_a_terminal(&config, state.path(), &[verb]);
        assert!(text.contains(MIGRATION_PROMPT), "{verb}: {text}");
        assert_eq!(code, Some(1), "{verb}: {text}");
        assert!(text.contains("no home directory found"), "{verb}: {text}");
        assert!(
            !state.path().join("state.db").exists(),
            "{verb}: the refused run left a store: {text}"
        );

        let (code, text) = run_homeless_on_a_terminal(&config, state.path(), &["log"]);
        assert_eq!(code, Some(0), "{verb}: {text}");
        assert!(
            text.contains(MIGRATION_PROMPT),
            "{verb}: the refused run's answer was recorded: {text}"
        );
    }
}

/// With no home, an `apply` that fails for a reason other than the home (a
/// profile that does not exist, refused before the store opens and with no
/// source conflict to record) still records the answer it was given.
#[cfg(unix)]
#[test]
fn a_homeless_apply_failing_for_another_reason_records_its_migration_answer() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    std::fs::remove_file(dir.path().join("profiles/base.yaml")).unwrap();
    let config = dir.path().join("cfgd.yaml");
    let state = tempfile::tempdir().unwrap();

    let (code, text) = run_homeless_on_a_terminal(&config, state.path(), &["apply"]);
    assert!(text.contains(MIGRATION_PROMPT), "{text}");
    assert_eq!(code, Some(6), "exits NotFound(6): {text}");
    assert!(text.contains("profile not found: base"), "{text}");

    let (code, text) = run_homeless_on_a_terminal(&config, state.path(), &["log"]);
    assert_eq!(code, Some(0), "{text}");
    assert!(
        !text.contains(MIGRATION_PROMPT),
        "the recorded answer was not read: {text}"
    );
}

/// With no home, `cfgd init` over a directory already holding a config asks
/// the migration question of that config and records the answer: the next
/// run over it is not asked again.
#[cfg(unix)]
#[test]
fn a_homeless_init_over_an_existing_config_records_its_migration_answer() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let config = dir.path().join("cfgd.yaml");
    let state = tempfile::tempdir().unwrap();
    let target = dir.path().to_str().unwrap();

    let (code, text) = run_homeless_on_a_terminal(&config, state.path(), &["init", target]);
    assert!(text.contains("Already initialized"), "{text}");
    assert!(text.contains(MIGRATION_PROMPT), "{text}");
    assert_eq!(code, Some(0), "{text}");

    let (code, text) = run_homeless_on_a_terminal(&config, state.path(), &["log"]);
    assert_eq!(code, Some(0), "{text}");
    assert!(
        !text.contains(MIGRATION_PROMPT),
        "the recorded answer was not read: {text}"
    );
}

/// With no home, a verb that reports its own failure and exits nonzero (a
/// `pull` whose origin does not exist) still records the answer it was given:
/// the exit passes through the one recording before the process ends.
#[cfg(unix)]
#[test]
fn a_homeless_pull_that_fails_records_its_migration_answer() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    for args in [
        &["init", "-q"][..],
        &["remote", "add", "origin", "/nonexistent/repo.git"],
    ] {
        let status = cfgd_core::git_cmd_local()
            .current_dir(dir.path())
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
    let config = dir.path().join("cfgd.yaml");
    let state = tempfile::tempdir().unwrap();

    let (code, text) = run_homeless_on_a_terminal(&config, state.path(), &["pull"]);
    assert!(text.contains(MIGRATION_PROMPT), "{text}");
    assert_eq!(code, Some(1), "{text}");

    let (code, text) = run_homeless_on_a_terminal(&config, state.path(), &["log"]);
    assert_eq!(code, Some(0), "{text}");
    assert!(
        !text.contains(MIGRATION_PROMPT),
        "the recorded answer was not read: {text}"
    );
}

/// With no home, and the config, state store and cache all placed by `XDG_*`,
/// a desired state that writes under `~/` (an env var, a `~/` file target, a
/// module's env var or file) has nowhere to put it. `doctor` fails its Home
/// row and verdict, and `apply` refuses naming what needs the home before it
/// opens the state store, writing nothing under the working directory (where a
/// literal `~` would otherwise land), no `state.db` and no cache.
#[test]
fn doctor_and_apply_refuse_home_work_with_no_home_and_write_nothing_under_the_cwd() {
    let cases = [
        (
            "profile file",
            "spec:\n  files:\n    managed:\n      - source: files/gitconfig\n        target: ~/.gitconfig\n",
            "for the file target",
        ),
        (
            "profile env",
            "spec:\n  env:\n    - name: EDITOR\n      value: vim\n",
            "cannot write the managed env file",
        ),
        (
            "module env",
            "spec:\n  modules: [tools]\n",
            "cannot write the managed env file",
        ),
        (
            "module file",
            "spec:\n  modules: [dots]\n",
            "for the file target",
        ),
    ];
    for (case, profile_spec, refusal) in cases {
        let xdg = tempfile::tempdir().unwrap();
        let cwd = tempfile::tempdir().unwrap();
        let config_dir = xdg.path().join("config").join("cfgd");
        create_valid_config(&config_dir);
        std::fs::write(
            config_dir.join("profiles/base.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\n{profile_spec}"
            ),
        )
        .unwrap();
        std::fs::create_dir_all(config_dir.join("files")).unwrap();
        std::fs::write(config_dir.join("files/gitconfig"), "[user]\n").unwrap();
        std::fs::create_dir_all(config_dir.join("modules/tools")).unwrap();
        std::fs::write(
            config_dir.join("modules/tools/module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: tools\nspec:\n  env:\n    - name: EDITOR\n      value: vim\n",
        )
        .unwrap();
        std::fs::create_dir_all(config_dir.join("modules/dots")).unwrap();
        std::fs::write(config_dir.join("modules/dots/init.lua"), "-- init\n").unwrap();
        std::fs::write(
            config_dir.join("modules/dots/module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: dots\nspec:\n  files:\n    - source: init.lua\n      target: ~/.config/nvim/init.lua\n",
        )
        .unwrap();
        let homeless = |args: &[&str]| {
            cfgd_bin()
                .unwrap()
                .current_dir(cwd.path())
                .env_remove("HOME")
                .env_remove("USERPROFILE")
                .env_remove(cfgd_core::CFGD_STATE_DIR_ENV)
                .env_remove(cfgd_core::CFGD_CACHE_DIR_ENV)
                .env("XDG_CONFIG_HOME", xdg.path().join("config"))
                .env("XDG_STATE_HOME", xdg.path().join("state"))
                .env("XDG_CACHE_HOME", xdg.path().join("cache"))
                .args(args)
                .output()
                .unwrap()
        };

        let out = homeless(&["apply", "--yes"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let created: Vec<_> = std::fs::read_dir(cwd.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(
            created.is_empty(),
            "{case}: apply created {created:?} under the working directory: {stderr}"
        );
        assert_ne!(out.status.code(), Some(0), "{case}: {stderr}");
        assert!(stderr.contains(refusal), "{case}: {stderr}");
        let db = xdg.path().join("state").join("cfgd").join("state.db");
        assert!(
            !db.exists(),
            "{case}: the refusal left a state store at {}",
            db.display()
        );
        let cache = xdg.path().join("cache");
        assert!(
            !cache.exists(),
            "{case}: the refusal wrote under {}",
            cache.display()
        );

        let out = homeless(&["doctor"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let row = stderr
            .lines()
            .find(|line| line.contains("Home directory"))
            .unwrap_or_else(|| panic!("{case}: no home row: {stderr}"));
        assert!(row.contains("✗ Home directory: unset"), "{case}: {row:?}");
        assert!(stderr.contains(refusal), "{case}: {stderr}");
        assert_eq!(out.status.code(), Some(1), "{case}: {stderr}");

        let out = homeless(&["doctor", "-o", "json"]);
        let v = parse_single_json(&String::from_utf8_lossy(&out.stdout));
        assert_eq!(v["home"]["resolved"], false, "{case}: {v}");
        assert!(
            v["home"]["refusal"]
                .as_str()
                .is_some_and(|m| m.contains(refusal)),
            "{case}: {v}"
        );
    }
}

/// A source's profile declaring an env var is home work the local config never
/// spells. With the source already in the cache, `doctor` composes it the way
/// `apply` does, so with no home its Home row and verdict fail naming the env
/// file, and `apply` refuses the same way.
#[test]
fn doctor_refuses_home_work_a_cached_source_delivers_with_no_home() {
    let (_workspace, config_dir, _state) = cfgd_test_fixtures::local_source_setup("", |_| {
        (
            "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: acme\n  version: \"1.0.0\"\nspec:\n  provides:\n    profiles:\n      - default\n".to_string(),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  env:\n    - name: ACME_EDITOR\n      value: vim\n".to_string(),
        )
    });
    let config = config_dir.path().join("cfgd.yaml");
    let cmd = |homeless: bool| {
        let mut cmd = cfgd_bin().unwrap();
        if homeless {
            cmd.env_remove("HOME").env_remove("USERPROFILE");
        }
        cmd.env(cfgd_core::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1")
            .arg("--config")
            .arg(&config);
        cmd
    };
    let out = cmd(false).arg("sync").output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let refusal = "cannot write the managed env file";
    let out = cmd(true).arg("doctor").output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let row = stderr
        .lines()
        .find(|line| line.contains("Home directory"))
        .unwrap_or_else(|| panic!("no home row: {stderr}"));
    assert!(row.contains("✗ Home directory: unset"), "{row:?}");
    assert!(stderr.contains(refusal), "{stderr}");
    assert_eq!(out.status.code(), Some(1), "{stderr}");

    let out = cmd(true).args(["apply", "--yes"]).output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_ne!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(refusal), "{stderr}");
}

/// A source the cache has never held is fetched by `apply` and `plan`
/// themselves, so the `~/` file its profile declares is known only once the
/// fetch is done. With no home, each resolves first and judges that file
/// before it opens the state store: the run refuses and no `state.db` is left
/// behind.
#[test]
fn a_homeless_run_refuses_home_work_a_source_it_fetches_delivers_and_writes_no_store() {
    for verb in [&["apply", "--yes"][..], &["plan"]] {
        let (_workspace, config_dir, state) = cfgd_test_fixtures::local_source_setup("", |_| {
            (
                "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: acme\n  version: \"1.0.0\"\nspec:\n  provides:\n    profiles:\n      - default\n".to_string(),
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  files:\n    managed:\n      - source: acmerc\n        target: ~/.acmerc\n".to_string(),
            )
        });
        let mut cmd = cfgd_bin().unwrap();
        // Each arm gets a state and a cache directory of its own, since every
        // spawn in one test shares the isolated ones: the plan arm has to
        // fetch a source no earlier arm left in the cache.
        cmd.env_remove("HOME")
            .env_remove("USERPROFILE")
            .env(cfgd_core::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1")
            .env(cfgd_core::CFGD_STATE_DIR_ENV, state.path().join("state"))
            .env(cfgd_core::CFGD_CACHE_DIR_ENV, state.path().join("cache"))
            .arg("--config")
            .arg(config_dir.path().join("cfgd.yaml"))
            .args(verb)
            .args(["-o", "json"]);
        let state_dir = env_path(&cmd, cfgd_core::CFGD_STATE_DIR_ENV);
        let cache_dir = env_path(&cmd, cfgd_core::CFGD_CACHE_DIR_ENV);
        assert!(!cache_dir.exists(), "{verb:?}: the cache is not fresh");
        let out = cmd.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{verb:?}: {stderr}");
        let v = parse_single_json(&String::from_utf8_lossy(&out.stdout));
        assert_eq!(v["error"], "state", "{verb:?}: {v}");
        assert!(
            v["message"]
                .as_str()
                .is_some_and(|m| m.contains("~/.acmerc")),
            "{verb:?}: {v}"
        );
        let db = state_dir.join("state.db");
        assert!(!db.exists(), "{verb:?} left {}", db.display());
        let sources = cache_dir.join("sources");
        let mut cached = Vec::new();
        collect_files(&cache_dir, &mut cached);
        assert!(
            cached.contains(&sources.join("acme").join("cfgd-source.yaml")),
            "{verb:?} fetched no checkout: {cached:?}"
        );
        let stray: Vec<_> = cached.iter().filter(|f| !f.starts_with(&sources)).collect();
        assert!(
            stray.is_empty(),
            "{verb:?} wrote beside its fetch: {stray:?}"
        );
    }
}

/// Every file under `dir`, recursively; a directory that cannot be read
/// fails the test naming it.
fn collect_files(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display()));
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files(&path, found);
        } else {
            found.push(path);
        }
    }
}

/// A homeless run that fails for a reason other than the missing home, here a
/// module the profile names and nothing provides, still records the source
/// conflict its composition found, the way a run with a home records it while
/// it composes. One the resolution refuses for want of a home, here a module's
/// `~/` file target, writes no store.
#[test]
fn a_homeless_run_that_fails_after_composing_records_the_source_conflict() {
    for (module, failure, records) in [
        ("nowhere", "module not found: nowhere", true),
        ("dots", "for the file target", false),
    ] {
        let (_workspace, config_dir, state) = cfgd_test_fixtures::local_source_setup(
            "",
            |workspace| {
                let target = cfgd_core::to_posix_string(workspace.join("out").join("acmerc"));
                (
                    format!(
                        "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: acme\n  version: \"1.0.0\"\nspec:\n  provides:\n    profiles:\n      - default\n  policy:\n    locked:\n      files:\n        - source: acmerc\n          target: {target}\n"
                    ),
                    format!(
                        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules: [{module}]\n"
                    ),
                )
            },
        );
        let config_dir = config_dir.path();
        std::fs::write(config_dir.join("acmerc"), "level = high\n").unwrap();
        std::fs::create_dir_all(config_dir.join("modules/dots")).unwrap();
        std::fs::write(config_dir.join("modules/dots/init.lua"), "-- init\n").unwrap();
        std::fs::write(
            config_dir.join("modules/dots/module.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: dots\nspec:\n  files:\n    - source: init.lua\n      target: ~/.config/nvim/init.lua\n",
        )
        .unwrap();
        let mut plan = cfgd_bin().unwrap();
        plan.env_remove("HOME")
            .env_remove("USERPROFILE")
            .env(cfgd_core::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1")
            // Each case gets a state directory of its own, since every spawn in
            // one test shares the isolated one.
            .env(cfgd_core::CFGD_STATE_DIR_ENV, state.path())
            .arg("--config")
            .arg(config_dir.join("cfgd.yaml"))
            .arg("plan");
        let db = env_path(&plan, cfgd_core::CFGD_STATE_DIR_ENV).join("state.db");
        let out = plan.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_ne!(out.status.code(), Some(0), "{module}: {stderr}");
        assert!(stderr.contains(failure), "{module}: {stderr}");
        if records {
            let store = cfgd_core::state::StateStore::open_read_only(&db)
                .unwrap_or_else(|e| panic!("no store recorded the conflict ({e}): {stderr}"));
            assert_eq!(store.source_conflict_count().unwrap(), 1, "{stderr}");
        } else {
            assert!(
                !db.exists(),
                "{module}: the refusal left {}: {stderr}",
                db.display()
            );
        }
    }
}

/// A state store as cfgd 0.11 left it at `db`: schema version 27, before
/// the migration-answer table (migration 28), the tracking rows' `kind` and
/// `manager` columns (29) and the store's identity (30), with one manager
/// `PATH` entry recorded. Built from a current store by undoing those three
/// migrations through rusqlite, so a writable open replays them and moves
/// the version. The connection comes back open, holding the `-wal` and
/// `-shm` sidecars until the caller drops it.
fn schema_27_store(db: &std::path::Path) -> rusqlite::Connection {
    drop(cfgd_core::state::StateStore::open(db).unwrap());
    let conn = rusqlite::Connection::open(db).unwrap();
    conn.execute_batch(
        "DROP TABLE config_migrations;
         DROP TABLE store_identity;
         ALTER TABLE managed_resources DROP COLUMN kind;
         ALTER TABLE managed_resources DROP COLUMN manager;
         UPDATE schema_version SET version = 27;
         INSERT INTO bootstrapped_managers (manager, path_dirs, bootstrapped_at)
         VALUES ('brew', '[\"/opt/brew/bin\"]', '2026-01-01T00:00:00Z');",
    )
    .unwrap();
    // A read after the last write leaves a reader's mark in the `-shm` at the
    // log's end, where the next reader looks for one, so that reader finds it
    // and writes no mark of its own.
    conn.query_row("SELECT count(*) FROM schema_version", [], |_| Ok(()))
        .unwrap();
    conn
}

/// The `schema_version` row of the store at `db`, read without a cfgd open.
fn schema_version_of(db: &std::path::Path) -> i64 {
    rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap()
        .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
        .unwrap()
}

/// A store an older cfgd left behind records a manager `PATH` entry, which
/// lives in the managed env file. With no home, `apply` and `plan` read that
/// record without opening the store for writing and refuse naming the env
/// file, leaving the store's bytes, its `-wal` and `-shm` sidecars and its
/// schema version as they were, whether those sidecars are open or closed.
/// The store is behind the schema, so a writable open would migrate it.
#[test]
fn a_homeless_run_over_a_recorded_path_entry_refuses_and_leaves_the_store_as_it_was() {
    for sidecars_open in [false, true] {
        for verb in [&["apply", "--yes"][..], &["plan"]] {
            let case = format!("{verb:?} with sidecars open: {sidecars_open}");
            let mut cmd = cfgd_bin().unwrap();
            let config_dir = home_of(&cmd).join(".config").join("cfgd");
            create_valid_config(&config_dir);
            // Each case gets a state directory of its own, since every spawn
            // in one test shares the isolated one.
            let state = tempfile::tempdir().unwrap();
            let state_dir = state.path().to_path_buf();
            cmd.env(cfgd_core::CFGD_STATE_DIR_ENV, &state_dir);
            let store = schema_27_store(&state_dir.join("state.db"));
            let held = sidecars_open.then_some(store);
            let files = ["state.db", "state.db-wal", "state.db-shm"];
            // The directory's mtime moves with any file created or removed in it,
            // so a `-wal` a writable open makes and a clean close removes again
            // still shows.
            let snapshot = || {
                (
                    files.map(|f| read_store_file(&state_dir.join(f))),
                    std::fs::metadata(&state_dir).unwrap().modified().unwrap(),
                )
            };
            let before = snapshot();
            assert_eq!(
                before.0[1].is_some(),
                sidecars_open,
                "{case}: the fixture's -wal"
            );

            cmd.env_remove("HOME")
                .env_remove("USERPROFILE")
                .arg("--config")
                .arg(config_dir.join("cfgd.yaml"))
                .args(verb);
            let out = cmd.output().unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(1), "{case}: {stderr}");
            assert!(
                stderr.contains("cannot write the managed env file"),
                "{case}: {stderr}"
            );
            let after = snapshot();
            for ((file, before), after) in files.iter().zip(&before.0).zip(&after.0) {
                assert!(before == after, "{case}: the refused run changed {file}");
            }
            assert_eq!(
                before.1, after.1,
                "{case}: the refused run created or removed a file beside the store"
            );
            drop(held);
            assert_eq!(
                schema_version_of(&state_dir.join("state.db")),
                27,
                "{case}: the refused run migrated the store"
            );
        }
    }
}

/// With no home, `plan` composes a source it fetches before its state store
/// opens; the conflict that composition finds is recorded once the store opens,
/// the way a run with a home records it.
#[test]
fn a_homeless_plan_records_the_source_conflict_it_composed_before_its_store_opened() {
    let (_workspace, config_dir, _state) = cfgd_test_fixtures::local_source_setup(
        "",
        |workspace| {
            let target = cfgd_core::to_posix_string(workspace.join("out").join("acmerc"));
            (
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: acme\n  version: \"1.0.0\"\nspec:\n  provides:\n    profiles:\n      - default\n  policy:\n    locked:\n      files:\n        - source: acmerc\n          target: {target}\n"
            ),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec: {}\n".to_string(),
        )
        },
    );
    std::fs::write(config_dir.path().join("acmerc"), "level = high\n").unwrap();
    let mut plan = cfgd_bin().unwrap();
    plan.env_remove("HOME")
        .env_remove("USERPROFILE")
        .env(cfgd_core::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1")
        .arg("--config")
        .arg(config_dir.path().join("cfgd.yaml"))
        .arg("plan");
    let state_dir = env_path(&plan, cfgd_core::CFGD_STATE_DIR_ENV);
    let out = plan.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("Source Conflicts"), "{stderr}");
    let store = cfgd_core::state::StateStore::open_read_only(&state_dir.join("state.db")).unwrap();
    assert_eq!(store.source_conflict_count().unwrap(), 1, "{stderr}");
}

/// `apply --plan` with no home reads the saved plan's store without opening it
/// for writing, and leaves the store's files as they were in every case:
///
/// - over the store the plan was derived against, which records a manager
///   `PATH` entry, the replay refuses naming the managed env file;
/// - with no store on disk there is nothing to check the plan against, and the
///   replay refuses for the missing home without creating one;
/// - over a store an older cfgd wrote, which carries no identity, the plan is
///   stale, as it is for a run with a home once the migration mints a new one.
#[test]
fn a_homeless_replay_reads_its_store_without_opening_it_for_writing() {
    for case in ["derived", "no store", "older store"] {
        let mut with_home = cfgd_bin().unwrap();
        let config = home_of(&with_home).join(".config").join("cfgd");
        create_valid_config(&config);
        let config = config.join("cfgd.yaml");
        // Each case gets a state directory of its own, since every spawn in
        // one test shares the isolated one.
        let state = tempfile::tempdir().unwrap();
        let state_dir = state.path().join("state");
        std::fs::create_dir_all(&state_dir).unwrap();
        with_home.env(cfgd_core::CFGD_STATE_DIR_ENV, &state_dir);
        cfgd_core::state::StateStore::open(&state_dir.join("state.db"))
            .unwrap()
            .record_bootstrapped_path_dirs("brew", &["/opt/brew/bin".to_string()])
            .unwrap();
        let out = with_home
            .arg("--config")
            .arg(&config)
            .args(["plan", "-o", "json"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{case}: {stderr}");
        let plan_file = state.path().join("plan.json");
        std::fs::write(&plan_file, &out.stdout).unwrap();
        let files = ["state.db", "state.db-wal", "state.db-shm"];
        if case != "derived" {
            for file in files {
                let _ = std::fs::remove_file(state_dir.join(file));
            }
        }
        if case == "older store" {
            drop(schema_27_store(&state_dir.join("state.db")));
        }
        // The directory's mtime moves with any file created or removed in it.
        let snapshot = || {
            (
                files.map(|f| read_store_file(&state_dir.join(f))),
                std::fs::metadata(&state_dir).unwrap().modified().unwrap(),
            )
        };
        let before = snapshot();

        let replay = |format: &str| {
            cfgd_bin()
                .unwrap()
                .env_remove("HOME")
                .env_remove("USERPROFILE")
                .env(cfgd_core::CFGD_STATE_DIR_ENV, &state_dir)
                .arg("--config")
                .arg(&config)
                .args(["apply", "--yes", "-o", format, "--plan"])
                .arg(&plan_file)
                .output()
                .unwrap()
        };
        let out = replay("json");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let v = parse_single_json(&String::from_utf8_lossy(&out.stdout));
        assert_ne!(out.status.code(), Some(0), "{case}: {stderr}");
        // A refusal of the plan file carries its sentence in the human
        // rendering alone.
        let human = replay("table");
        let human = String::from_utf8_lossy(&human.stderr);
        match case {
            "derived" => assert!(
                v["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("cannot write the managed env file")),
                "{v}"
            ),
            "no store" => {
                assert_eq!(v["error"], "home_unresolved", "{v}");
                assert_eq!(v["name"], "plan", "{v}");
                assert!(v["file"].is_string(), "{v}");
                assert!(
                    human.contains("there is no state store to check it against")
                        && human.contains("no home directory found"),
                    "{human}"
                );
            }
            _ => {
                assert_eq!(v["error"], "stale", "{v}");
                assert!(v["storeId"].is_null(), "{v}");
                assert!(v["recordedStoreId"].is_string(), "{v}");
                assert!(human.contains("written by an older cfgd"), "{human}");
            }
        }
        let after = snapshot();
        for ((file, before), after) in files.iter().zip(&before.0).zip(&after.0) {
            assert!(before == after, "{case}: the replay changed {file}");
        }
        assert_eq!(
            before.1, after.1,
            "{case}: the replay created or removed a file beside the store"
        );
        if case == "older store" {
            assert_eq!(schema_version_of(&state_dir.join("state.db")), 27, "{case}");
        }
    }
}

/// With no home an age key declared under `~/` has nowhere to resolve. A
/// profile decrypting a secret needs that key, so `doctor` fails the age key row
/// and its verdict and `apply` refuses before opening the state store; with no
/// secret declared the same row only warns and the run passes.
#[test]
fn a_homeless_run_names_the_age_key_it_cannot_locate() {
    for (case, profile_spec) in [
        (
            "decrypting",
            "spec:\n  secrets:\n    - source: secrets/token.enc\n      target: TARGET\n",
        ),
        ("no secrets", "spec: {}\n"),
    ] {
        let decrypts = case == "decrypting";
        let xdg = tempfile::tempdir().unwrap();
        let config_dir = xdg.path().join("config").join("cfgd");
        create_valid_config(&config_dir);
        std::fs::write(
            config_dir.join("cfgd.yaml"),
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: base\n  secrets:\n    sops:\n      ageKey: ~/.age/key.txt\n",
        )
        .unwrap();
        let target = cfgd_core::to_posix_string(xdg.path().join("token"));
        std::fs::write(
            config_dir.join("profiles/base.yaml"),
            format!(
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\n{}",
                profile_spec.replace("TARGET", &target)
            ),
        )
        .unwrap();
        std::fs::create_dir_all(config_dir.join("secrets")).unwrap();
        std::fs::write(config_dir.join("secrets/token.enc"), "sops: {}\n").unwrap();
        let homeless = |args: &[&str]| {
            cfgd_bin()
                .unwrap()
                .env_remove("HOME")
                .env_remove("USERPROFILE")
                .env_remove(cfgd_core::CFGD_STATE_DIR_ENV)
                .env_remove(cfgd_core::CFGD_CACHE_DIR_ENV)
                .env("XDG_CONFIG_HOME", xdg.path().join("config"))
                .env("XDG_STATE_HOME", xdg.path().join("state"))
                .env("XDG_CACHE_HOME", xdg.path().join("cache"))
                .args(args)
                .output()
                .unwrap()
        };
        let refusal = "for the age key: no home directory found";

        let out = homeless(&["doctor"]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let row = stderr
            .lines()
            .find(|line| line.contains("age key"))
            .unwrap_or_else(|| panic!("{case}: no age key row: {stderr}"));
        let glyph = if decrypts {
            "✗ age key"
        } else {
            "⚠ age key"
        };
        assert!(row.contains(glyph), "{case}: {row:?}");
        assert!(stderr.contains(refusal), "{case}: {stderr}");
        assert_eq!(
            out.status.code(),
            Some(if decrypts { 1 } else { 0 }),
            "{case}: {stderr}"
        );

        let out = homeless(&["doctor", "-o", "json"]);
        let v = parse_single_json(&String::from_utf8_lossy(&out.stdout));
        assert!(
            v["secrets"]["ageKeyError"]
                .as_str()
                .is_some_and(|m| m.contains(refusal)),
            "{case}: {v}"
        );

        if decrypts {
            let out = homeless(&["apply", "--yes"]);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_ne!(out.status.code(), Some(0), "{case}: {stderr}");
            assert!(stderr.contains(refusal), "{case}: {stderr}");
            let db = xdg.path().join("state").join("cfgd").join("state.db");
            assert!(!db.exists(), "{case}: the refusal left {}", db.display());
        }
    }
}

/// A directory override under `~` with no home set fails the run naming the
/// unset home, from a flag and from its environment variable alike, and
/// creates nothing: used as written it would name `./~/s` under the working
/// directory.
#[test]
fn a_tilde_state_dir_with_no_home_fails_naming_the_unset_home() {
    let refusal = format!(
        "cannot expand ~/s for the state directory: no home directory found ({} unset)",
        cfgd_core::HOME_ENV_VARS
    );
    let dir = tempfile::tempdir().unwrap();
    let config = write_config_with_spec(dir.path(), "spec: {}\n");
    let cwd = tempfile::tempdir().unwrap();
    let homeless = || {
        let mut cmd = cfgd_bin().unwrap();
        cmd.current_dir(cwd.path())
            .env_remove("HOME")
            .env_remove("USERPROFILE")
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("XDG_STATE_HOME")
            .env_remove(cfgd_core::CFGD_STATE_DIR_ENV)
            .arg("--config")
            .arg(&config);
        cmd
    };
    let mut by_flag = homeless();
    by_flag.args(["--state-dir", "~/s", "log"]);
    let mut by_env = homeless();
    by_env.env(cfgd_core::CFGD_STATE_DIR_ENV, "~/s").arg("log");
    for (spelling, mut cmd) in [
        ("--state-dir", by_flag),
        (cfgd_core::CFGD_STATE_DIR_ENV, by_env),
    ] {
        let out = cmd.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{spelling}: {stderr}");
        assert!(stderr.contains(&refusal), "{spelling}: {stderr:?}");
        assert!(
            !cwd.path().join("~").exists(),
            "{spelling}: a `~` directory was created under the working directory"
        );
    }
}

/// The default config location with no home set. Every OS, Windows included,
/// derives it from the one home, so the run reports the unset home.
#[test]
fn the_default_config_with_no_home_names_what_is_missing() {
    let expected = home_unresolved();
    for format in ["table", "json"] {
        let (stdout, stderr, code) = run_homeless(&["status", "-o", format]);
        assert_eq!(code, Some(3), "{format}: exits NoConfig(3): {stderr}");
        let reported = if format == "json" {
            let v = parse_single_json(&stdout);
            v["message"].as_str().unwrap_or_default().to_string()
        } else {
            stderr
        };
        assert!(reported.contains(&expected), "{format}: {reported:?}");
        assert!(
            !reported.contains("/~/") && !reported.contains("\\~\\"),
            "{format}: a `~` joined under another directory: {reported:?}"
        );
    }
}

/// The home directory a command built by `cfgd_bin()` hands the binary.
fn home_of(cmd: &std::process::Command) -> std::path::PathBuf {
    env_path(cmd, "HOME")
}

/// The directory `cmd` sets `var` to, which the constructor always sets.
fn env_path(cmd: &std::process::Command, var: &str) -> std::path::PathBuf {
    cmd.get_envs()
        .find_map(|(name, value)| (name == var).then_some(value).flatten())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| panic!("the constructor sets {var}"))
}

/// A `--config` under `~` with a home set names the file under that home, so
/// the refusal points at it and never claims the home is unset.
#[test]
fn a_tilde_config_with_a_home_set_reports_the_missing_file() {
    for format in ["table", "json"] {
        let mut cmd = cfgd_bin().unwrap();
        let expected = format!(
            "config error: config file not found: {}",
            cfgd_core::absolutize_path(&home_of(&cmd).join("cfgd.yaml")).display()
        );
        let out = cmd
            .args(["status", "--config", "~/cfgd.yaml", "-o", format])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(3), "{format}: {stderr}");
        let reported = if format == "json" {
            let v = parse_single_json(&String::from_utf8_lossy(&out.stdout));
            v["message"].as_str().unwrap_or_default().to_string()
        } else {
            stderr.lines().next().unwrap_or_default().to_string()
        };
        assert!(reported.ends_with(&expected), "{format}: {reported:?}");
        assert!(
            !reported.contains(&home_unresolved()),
            "{format}: {reported:?}"
        );
    }
}

/// A config under the home directory loads through a `~` spelling of its
/// path, given as `--config` or as `CFGD_CONFIG`, as an environment file
/// passes it with no shell to expand it.
#[test]
fn a_tilde_config_under_the_home_loads_from_the_flag_and_the_env() {
    let probe = cfgd_bin().unwrap();
    write_config_with_spec(&home_of(&probe), "spec:\n  profile: tilde-home\n");
    let mut by_flag = cfgd_bin().unwrap();
    by_flag.args(["config", "get", "profile", "--config", "~/cfgd.yaml"]);
    let mut by_env = cfgd_bin().unwrap();
    by_env
        .env(cfgd_core::CFGD_CONFIG_ENV, "~/cfgd.yaml")
        .args(["config", "get", "profile"]);
    for (spelling, mut cmd) in [("--config", by_flag), (cfgd_core::CFGD_CONFIG_ENV, by_env)] {
        let out = cmd.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{spelling}: {stderr}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "tilde-home",
            "{spelling}: {stderr}"
        );
    }
}

/// Every directory flag, its `CFGD_*` env form and `CFGD_DAEMON_IPC_PATH`
/// take a leading `~` as the home directory: an environment file or a quoted
/// argument passes it with no shell to expand it. `cfgd paths` reports what
/// the run resolved.
#[test]
fn every_directory_flag_and_env_expands_a_leading_tilde() {
    const CASES: [(Option<&str>, &str, &str, &str); 5] = [
        (
            Some("--config-dir"),
            cfgd_core::CFGD_CONFIG_DIR_ENV,
            "config",
            "dir",
        ),
        (
            Some("--state-dir"),
            cfgd_core::CFGD_STATE_DIR_ENV,
            "state",
            "dir",
        ),
        (
            Some("--cache-dir"),
            cfgd_core::CFGD_CACHE_DIR_ENV,
            "cache",
            "dir",
        ),
        (
            Some("--runtime-dir"),
            cfgd_core::CFGD_RUNTIME_DIR_ENV,
            "runtime",
            "dir",
        ),
        (
            None,
            cfgd_core::CFGD_DAEMON_IPC_PATH_ENV,
            "runtime",
            "socket",
        ),
    ];
    for (flag, env, section, field) in CASES {
        for spelling in flag.into_iter().chain([env]) {
            let mut cmd = cfgd_bin().unwrap();
            let expected = cfgd_core::to_posix_string(home_of(&cmd).join("tilde-dir"));
            if spelling == env {
                cmd.env(env, "~/tilde-dir");
            } else {
                cmd.args([spelling, "~/tilde-dir"]);
            }
            let out = cmd.args(["paths", "-o", "json"]).output().unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{spelling}: {stderr}");
            let v = parse_single_json(&String::from_utf8_lossy(&out.stdout));
            assert_eq!(
                v[section][field].as_str(),
                Some(expected.as_str()),
                "{spelling}: {v}"
            );
        }
    }
}

/// The verbs the homeless store population leaves out, each with the reason
/// the fixture cannot drive it. A row named here with no reason, or a name no
/// longer in the clap tree, fails the pin.
const HOMELESS_STORE_SKIPS: &[(&str, &str)] = &[
    (
        "daemon",
        "runs the daemon until signalled, so no exit is observed",
    ),
    ("daemon run", "runs until signalled, so no exit is observed"),
    (
        "daemon install",
        "installs a service on the host running the tests",
    ),
    (
        "daemon uninstall",
        "removes a service from the host running the tests",
    ),
    ("checkin", "needs a running device gateway"),
    ("enroll", "needs a running device gateway"),
    (
        "upgrade",
        "asks GitHub for the latest release over the network",
    ),
    ("module push", "pushes to an OCI registry over the network"),
    ("module pull", "pulls from an OCI registry over the network"),
    ("generate module", "asks an AI provider over the network"),
    ("generate profile", "asks an AI provider over the network"),
];

/// The arguments a row runs with, past its path: a value for every argument
/// clap requires, naming what the fixture declares where a verb takes a name.
const HOMELESS_STORE_ARGS: &[(&str, &[&str])] = &[
    ("secret encrypt", &["secret.yaml"]),
    ("secret decrypt", &["secret.yaml"]),
    ("secret edit", &["secret.yaml"]),
    ("profile switch", &["default"]),
    ("profile create", &["extra"]),
    ("profile edit", &["default"]),
    ("profile delete", &["default"]),
    ("profile validate", &["profiles/default.yaml"]),
    ("module show", &["tools"]),
    (
        "module create",
        &["extra", "--env", "A=b", "--alias", "ll=ls"],
    ),
    ("module update", &["tools"]),
    ("module edit", &["tools"]),
    ("module delete", &["tools"]),
    ("module upgrade", &["tools"]),
    ("module search", &["tools"]),
    (
        "module registry add",
        &["https://example.invalid/registry.git"],
    ),
    ("module registry remove", &["registry"]),
    ("module registry rename", &["registry", "renamed"]),
    ("module export", &["--as", "devcontainer", "tools"]),
    ("module build", &["modules/tools"]),
    ("module validate", &["modules/tools/module.yaml"]),
    ("source add", &["https://example.invalid/source.git"]),
    ("source show", &["acme"]),
    ("source remove", &["acme"]),
    ("source override", &["acme", "reject", "packages.brew"]),
    ("source priority", &["acme", "100"]),
    (
        "source replace",
        &["acme", "https://example.invalid/source.git"],
    ),
    ("source validate", &["."]),
    ("backup run", &["notes"]),
    ("backup restore", &["notes"]),
    ("backup gc", &["notes"]),
    ("backup rollback", &["notes"]),
    ("machineconfig validate", &["cfgd.yaml"]),
    ("configpolicy validate", &["cfgd.yaml"]),
    ("clusterconfigpolicy validate", &["cfgd.yaml"]),
    ("skill install", &["claude"]),
    ("skill remove", &["claude"]),
    ("config get", &["profile"]),
    ("config set", &["profile", "default"]),
    ("config unset", &["theme.name"]),
    ("alias set", &["hi", "status"]),
    ("alias show", &["hi"]),
    ("alias delete", &["hi"]),
    ("completion", &["bash"]),
    ("rollback", &["1"]),
    ("compliance diff", &["1", "2"]),
    ("image pack", &["modules/tools", "artifact.tar"]),
    ("decide", &["accept", "x"]),
];

/// Every command path clap can run: each leaf, and each command with
/// subcommands that runs bare too (`cfgd compliance`).
fn runnable_paths(cmd: &clap::Command, path: &[String], out: &mut Vec<Vec<String>>) {
    let subs: Vec<&clap::Command> = cmd
        .get_subcommands()
        .filter(|sub| sub.get_name() != "help")
        .collect();
    if !path.is_empty() && (subs.is_empty() || !cmd.is_subcommand_required_set()) {
        out.push(path.to_vec());
    }
    for sub in subs {
        let mut next = path.to_vec();
        next.push(sub.get_name().to_string());
        runnable_paths(sub, &next, out);
    }
}

/// The command at `path` under `root`.
fn command_at<'c>(root: &'c clap::Command, path: &[String]) -> &'c clap::Command {
    path.iter().fold(root, |cmd, name| {
        cmd.find_subcommand(name)
            .unwrap_or_else(|| panic!("no subcommand {name} in {path:?}"))
    })
}

/// A config that declares `~/` work in every slot a verb reads: in the
/// profile a file target, an env var, an alias and a secret target; a backup
/// destination and a backup source; a module with an env var and an alias,
/// and one with a file target; a compliance watch path; and a source whose profile targets a
/// file under the home. Returns the workspace (owning the source's bare repo),
/// the config dir and a root for the state and cache directories.
fn homeless_population_fixture() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let (workspace, config_dir, root) = cfgd_test_fixtures::local_source_setup("", |_| {
        (
            "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: acme\n  version: \"1.0.0\"\nspec:\n  provides:\n    profiles:\n      - default\n".to_string(),
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  files:\n    managed:\n      - source: acmerc\n        target: ~/.acmerc\n".to_string(),
        )
    });
    let dir = config_dir.path();
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("notes.txt"), "notes\n").unwrap();
    std::fs::create_dir_all(dir.join("files")).unwrap();
    std::fs::write(dir.join("files/gitconfig"), "[user]\n").unwrap();
    std::fs::write(dir.join("files/token"), "token\n").unwrap();
    std::fs::write(
        dir.join("profiles/default.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules: [tools]\n  env:\n    - name: EDITOR\n      value: vim\n  aliases:\n    - name: gs\n      command: git status\n  files:\n    managed:\n      - source: files/gitconfig\n        target: ~/.gitconfig\n  secrets:\n    - source: files/token\n      target: ~/.token\n  backups:\n    - name: notes\n      source: {}\n      destination: ~/bk\n    - name: dotfiles\n      source: ~/dotfiles\n      destination: {}\n",
            cfgd_core::to_posix_string(&data),
            cfgd_core::to_posix_string(dir.join("bk"))
        ),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("modules/tools")).unwrap();
    std::fs::write(
        dir.join("modules/tools/module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: tools\nspec:\n  env:\n    - name: PAGER\n      value: less\n  aliases:\n    - name: ll\n      command: ls -l\n",
    )
    .unwrap();
    // Kept out of the profile: resolving a module expands its file targets,
    // so a profile naming it would be refused at resolution and never reach
    // the home check that follows it.
    std::fs::create_dir_all(dir.join("modules/dots")).unwrap();
    std::fs::write(
        dir.join("modules/dots/module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: dots\nspec:\n  files:\n    - source: dotsrc\n      target: ~/.dotsrc\n",
    )
    .unwrap();
    std::fs::write(dir.join("modules/dots/dotsrc"), "dots\n").unwrap();
    let mut config = std::fs::read_to_string(dir.join("cfgd.yaml")).unwrap();
    config.push_str(
        "  compliance:\n    enabled: true\n    scope:\n      watchPaths:\n        - ~/watched\n",
    );
    std::fs::write(dir.join("cfgd.yaml"), config).unwrap();
    (workspace, config_dir, root)
}

/// Stands in a row's arguments for the fixture's config directory, which each
/// run creates afresh.
const HOMELESS_CONFIG_DIR: &str = "{config-dir}";

/// The fixture value an apply flag that takes one is given, by the noun its
/// long name ends in: the module and the profile the fixture declares.
const HOMELESS_APPLY_VALUES: &[(&str, &str)] = &[("module", "tools"), ("profile", "default")];

/// What one homeless run of a row left: whether it was refused for the home,
/// the state store before and after it, and the bytes of the fixture's
/// managed file ([`LONE_MANAGED_TARGET`]) before and after it.
struct HomelessRun {
    refused: bool,
    before: [Option<Vec<u8>>; 3],
    after: [Option<Vec<u8>>; 3],
    managed: [Option<Vec<u8>>; 2],
    output: String,
}

/// The file every [`lone_slot_fixture`] manages at an absolute target under
/// its config dir, holding [`LONE_MANAGED_BYTES`] until an action rewrites it.
const LONE_MANAGED_TARGET: &str = "managed/.rc";

/// What [`LONE_MANAGED_TARGET`] holds before a run, which differs from the
/// source the profile deploys there.
const LONE_MANAGED_BYTES: &str = "precious\n";

/// A fixture for [`run_homeless_row`]: the workspace it keeps alive, the
/// config dir and a root for the state and cache directories.
type HomelessFixture = fn() -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir);

/// A config whose only `~/` work is `slot`, written into the profile's spec
/// or, for a top-level `spec` key, into `cfgd.yaml`, beside a module `plain`
/// that declares no home work at all. The profile also manages
/// [`LONE_MANAGED_TARGET`] at an absolute path, so an action that ran shows
/// in its bytes; a `profile_slot` opening with a `- source:` list item joins
/// that `files.managed` list.
fn lone_slot_fixture(
    profile_slot: &str,
    config_slot: &str,
) -> (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir) {
    let workspace = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    let dir = config_dir.path();
    std::fs::create_dir_all(dir.join("profiles")).unwrap();
    std::fs::create_dir_all(dir.join("files")).unwrap();
    std::fs::write(dir.join("files/token"), "token\n").unwrap();
    std::fs::write(dir.join("files/rc"), "deployed\n").unwrap();
    let managed = dir.join(LONE_MANAGED_TARGET);
    std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
    std::fs::write(&managed, LONE_MANAGED_BYTES).unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: lone\nspec:\n  profile: default\n{config_slot}"
        ),
    )
    .unwrap();
    let posix_dir = cfgd_core::to_posix_string(dir);
    let slot = profile_slot.replace("{dir}", &posix_dir);
    std::fs::write(
        dir.join("profiles/default.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  modules: [plain]\n  files:\n    managed:\n      - source: files/rc\n        target: {posix_dir}/{LONE_MANAGED_TARGET}\n{slot}"
        ),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("modules/plain")).unwrap();
    std::fs::write(
        dir.join("modules/plain/module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: plain\nspec: {}\n",
    )
    .unwrap();
    (workspace, config_dir, root)
}

/// The rows of the lone-slot population pin: a fixture whose only `~/` work
/// is one slot of [`HOME_REFUSAL_SLOTS`], named by that slot, with the verb
/// that must refuse it. A slot asked under more than one hook names the hook
/// in parentheses after it. `no slot` declares nothing under `~/`.
///
/// [`HOME_REFUSAL_SLOTS`]: cfgd_core::reconciler::HOME_REFUSAL_SLOTS
const LONE_SLOTS: &[(&str, &str, HomelessFixture)] = &[
    ("file target", "apply", || {
        lone_slot_fixture("      - source: files/rc\n        target: ~/.rc\n", "")
    }),
    ("secret target", "apply", || {
        lone_slot_fixture(
            "  secrets:\n    - source: files/token\n      target: ~/.token\n",
            "",
        )
    }),
    ("backup source", "apply", || {
        lone_slot_fixture(
            "  backups:\n    - name: dotfiles\n      source: ~/dotfiles\n      destination: {dir}/bk\n",
            "",
        )
    }),
    ("backup destination", "apply", || {
        lone_slot_fixture(
            "  backups:\n    - name: dotfiles\n      source: {dir}/files\n      destination: ~/bk\n",
            "",
        )
    }),
    ("script workdir (postApply)", "apply", || {
        lone_slot_fixture(
            "  scripts:\n    postApply:\n      - run: \"true\"\n        workdir: ~/x\n",
            "",
        )
    }),
    ("script workdir (preApply)", "apply", || {
        lone_slot_fixture(
            "  scripts:\n    preApply:\n      - run: \"true\"\n        workdir: ~/x\n",
            "",
        )
    }),
    ("creates guard", "apply", || {
        lone_slot_fixture(
            "  scripts:\n    postApply:\n      - run: \"true\"\n        creates: ~/x\n",
            "",
        )
    }),
    ("env var", "apply", || {
        lone_slot_fixture("  env:\n    - name: TOOLS\n      value: ~/bin\n", "")
    }),
    ("age key", "apply", || {
        lone_slot_fixture(
            "  secrets:\n    - source: files/token\n      target: {dir}/out/token\n",
            "  secrets:\n    backend: sops\n    sops:\n      ageKey: ~/age-key.txt\n",
        )
    }),
    ("SSH key", "apply", || {
        lone_slot_fixture("  system:\n    sshKeys:\n      - name: k\n", "")
    }),
    ("systemd unit file", "apply", || {
        lone_slot_fixture(
            "  system:\n    systemdUnits:\n      - name: x.service\n        unitFile: ~/x.service\n",
            "",
        )
    }),
    ("KDE config file", "apply", || {
        lone_slot_fixture(
            "  system:\n    kdeConfig:\n      ~/kdeglobals:\n        General:\n          Name: v\n",
            "",
        )
    }),
    ("launch agent", "apply", || {
        lone_slot_fixture(
            "  system:\n    launchAgents:\n      - name: com.example.agent\n        program: /bin/true\n",
            "",
        )
    }),
    ("compliance watch path", "compliance", || {
        lone_slot_fixture(
            "",
            "  compliance:\n    enabled: true\n    scope:\n      watchPaths:\n        - ~/watched\n",
        )
    }),
    ("compliance export directory", "compliance export", || {
        lone_slot_fixture(
            "",
            "  compliance:\n    enabled: true\n    export:\n      path: ~/out\n",
        )
    }),
    ("no slot", "", || lone_slot_fixture("", "")),
];

/// Whether this host runs the configurator a [`LONE_SLOTS`] row declares, so
/// its refusal is asked: `systemdUnits` needs `systemctl`, and `launchAgents`
/// runs on macOS alone.
fn host_asks(slot: &str) -> bool {
    match slot {
        "systemd unit file" => cfgd_core::systemctl_available(),
        "launch agent" => cfg!(target_os = "macos"),
        _ => true,
    }
}

/// Run `args` with no home over a fresh `fixture`, with a closed store
/// already in the state dir when `with_store` is set.
fn run_homeless_row(fixture: HomelessFixture, args: &[String], with_store: bool) -> HomelessRun {
    let (_workspace, config_dir, root) = fixture();
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    if with_store {
        drop(cfgd_core::state::StateStore::open(&state.join("state.db")).unwrap());
    }
    let before = store_snapshot(&state).0;
    let managed = config_dir.path().join(LONE_MANAGED_TARGET);
    let managed_before = std::fs::read(&managed).ok();
    let mut cmd = cfgd_bin().unwrap();
    cmd.current_dir(config_dir.path())
        .env_remove("HOME")
        .env_remove("USERPROFILE")
        .env(cfgd_core::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1")
        .env(cfgd_core::CFGD_STATE_DIR_ENV, &state)
        .env(cfgd_core::CFGD_CACHE_DIR_ENV, root.path().join("cache"))
        .env("EDITOR", "true")
        .env("VISUAL", "true")
        .stdin(std::process::Stdio::null())
        .arg("--config")
        .arg(config_dir.path().join("cfgd.yaml"))
        .args(["--yes", "-o", "json"])
        .args(args.iter().map(|arg| {
            arg.replace(
                HOMELESS_CONFIG_DIR,
                &cfgd_core::to_posix_string(config_dir.path()),
            )
        }));
    let out = assert_cmd::Command::from_std(cmd)
        .timeout(std::time::Duration::from_secs(120))
        .output()
        .unwrap();
    let output = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The run's own verdict names the home: the `-o json` error envelope on
    // stdout, or a failure line on stderr. A payload that reports a home
    // failure as one leg of a run that did its other work (`sync`'s
    // `configResolutionError`) is no refusal.
    let names_home = |text: &str| {
        text.contains("no home directory found")
            || text.contains("cannot resolve home directory")
            || text.contains("home_unresolved")
    };
    let envelope = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .filter(|v| v.get("error").is_some())
        .map(|v| v.to_string());
    let refused = out.status.code().is_some_and(|code| code != 0)
        && (envelope.as_deref().is_some_and(names_home)
            || names_home(&String::from_utf8_lossy(&out.stderr)));
    HomelessRun {
        refused,
        before,
        after: store_snapshot(&state).0,
        managed: [managed_before, std::fs::read(&managed).ok()],
        output,
    }
}

/// A run refused for want of a home leaves the state store as it found it:
/// no `state.db` where there was none, and the bytes of one that existed
/// unchanged. The rows are every command path clap can run, plus the flag
/// variants that reach a store by another road (`--module`, `--scan`, a named
/// backup listing), each run once over no store and once over an existing
/// one, under one fixture declaring `~/` work in every slot a verb reads. A
/// new verb joins the population by being compiled; one the fixture cannot
/// drive is named in [`HOMELESS_STORE_SKIPS`] with its reason.
#[test]
fn every_verb_refused_for_no_home_leaves_the_state_store_as_it_found_it() {
    use clap::CommandFactory;
    let root = cfgd::Cli::command();
    let mut paths = Vec::new();
    runnable_paths(&root, &[], &mut paths);
    let names: Vec<String> = paths.iter().map(|p| p.join(" ")).collect();

    for (name, reason) in HOMELESS_STORE_SKIPS {
        assert!(
            !reason.trim().is_empty(),
            "{name}: a skip carries its reason"
        );
        assert!(
            names.contains(&name.to_string()),
            "{name}: the skip names a path the clap tree has no longer"
        );
    }
    for (name, _) in HOMELESS_STORE_ARGS {
        assert!(
            names.contains(&name.to_string()),
            "{name}: arguments for a path not in the clap tree"
        );
    }
    let skipped = |name: &str| HOMELESS_STORE_SKIPS.iter().any(|(n, _)| *n == name);
    let args_of = |name: &str| {
        HOMELESS_STORE_ARGS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, args)| *args)
    };

    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut unrunnable = Vec::new();
    for (path, name) in paths.iter().zip(&names) {
        if skipped(name) {
            continue;
        }
        let needs_args = command_at(&root, path)
            .get_arguments()
            .any(|arg| arg.is_required_set());
        let extra = args_of(name);
        if needs_args && extra.is_none() {
            unrunnable.push(name.clone());
            continue;
        }
        let mut row = path.clone();
        row.extend(extra.unwrap_or_default().iter().map(|a| a.to_string()));
        rows.push(row);
    }
    assert!(
        unrunnable.is_empty(),
        "give these rows their required arguments in HOMELESS_STORE_ARGS, or a reason in \
         HOMELESS_STORE_SKIPS:\n{}",
        unrunnable.join("\n")
    );
    // A path scoped to one module reaches its store by another road, so every
    // path clap gives a `--module` runs scoped to each fixture module too, with
    // `--scan` where it takes one.
    let mut scoped = 0;
    for (path, name) in paths.iter().zip(&names) {
        let cmd = command_at(&root, path);
        let takes = |long: &str| cmd.get_arguments().any(|arg| arg.get_long() == Some(long));
        if skipped(name) || !takes("module") {
            continue;
        }
        for module in ["tools", "dots"] {
            let mut row = path.clone();
            row.extend(["--module".to_string(), module.to_string()]);
            if takes("scan") {
                let mut scan = row.clone();
                scan.push("--scan".to_string());
                rows.push(scan);
            }
            rows.push(row);
        }
        scoped += 1;
    }
    assert!(scoped >= 3, "only {scoped} paths take `--module`");
    // A flag that switches a verb into applying what it built (`--apply`,
    // `--apply-module`, `--apply-profile`) opens the store by the road the
    // plan takes, so every such flag clap gives runs as a row of its own. A
    // path that also takes `--from` reads the fixture's config through it.
    let mut apply_rows: Vec<String> = Vec::new();
    for (path, name) in paths.iter().zip(&names) {
        let cmd = command_at(&root, path);
        if skipped(name) {
            continue;
        }
        let mut base = path.clone();
        base.extend(
            args_of(name)
                .unwrap_or_default()
                .iter()
                .map(|a| a.to_string()),
        );
        if cmd
            .get_arguments()
            .any(|arg| arg.get_long() == Some("from"))
        {
            base.extend(["--from".to_string(), HOMELESS_CONFIG_DIR.to_string()]);
        }
        for arg in cmd.get_arguments() {
            let Some(long) = arg.get_long().filter(|long| long.starts_with("apply")) else {
                continue;
            };
            let mut row = base.clone();
            row.push(format!("--{long}"));
            if arg.get_action().takes_values() {
                let value = HOMELESS_APPLY_VALUES
                    .iter()
                    .find(|(noun, _)| long.ends_with(noun))
                    .map(|(_, value)| *value)
                    .unwrap_or_else(|| {
                        panic!("{name} --{long}: give it a fixture value in HOMELESS_APPLY_VALUES")
                    });
                row.push(value.to_string());
            }
            apply_rows.push(row.join(" "));
            rows.push(row);
        }
    }
    assert!(
        apply_rows.len() >= 4,
        "only {} apply rows: {apply_rows:?}",
        apply_rows.len()
    );
    for variant in [
        &["backup", "list", "notes"][..],
        &["backup", "list", "notes", "--snapshots"],
        &["backup", "run"],
        &["backup", "gc"],
        &["backup", "rollback"],
    ] {
        let path: Vec<String> = variant
            .iter()
            .take_while(|a| !a.starts_with('-') && *a != &"tools" && *a != &"notes")
            .map(|a| a.to_string())
            .collect();
        assert!(
            names.contains(&path.join(" ")),
            "{variant:?}: not in the clap tree"
        );
        rows.push(variant.iter().map(|a| a.to_string()).collect());
    }

    for bare in ["compliance", "compliance export"] {
        assert!(
            rows.iter().any(|row| row.join(" ") == bare),
            "`{bare}` is not a row"
        );
    }

    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let chunk = rows.len().div_ceil(threads);
    let results: Vec<(String, bool, HomelessRun)> = std::thread::scope(|scope| {
        rows.chunks(chunk)
            .map(|rows| {
                scope.spawn(move || {
                    let mut done = Vec::new();
                    for row in rows {
                        for with_store in [false, true] {
                            done.push((
                                row.join(" "),
                                with_store,
                                run_homeless_row(homeless_population_fixture, row, with_store),
                            ));
                        }
                    }
                    done
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect()
    });

    let refusals = results.iter().filter(|(_, _, run)| run.refused).count();
    // Every apply row plans over `~/` work, so one that is not refused never
    // reached its apply path and proves nothing about it.
    let unreached: Vec<String> = results
        .iter()
        .filter(|(row, _, run)| apply_rows.contains(row) && !run.refused)
        .map(|(row, with_store, run)| {
            let tail: String = run
                .output
                .lines()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .join(" | ");
            format!("{row} (store: {with_store}): {tail}")
        })
        .collect();
    assert!(
        unreached.is_empty(),
        "these apply rows were not refused for the home:\n{}",
        unreached.join("\n")
    );
    assert!(
        refusals >= 20,
        "only {refusals} runs were refused for the home, so the fixture reaches no home work"
    );
    let changed: Vec<String> = results
        .iter()
        .filter(|(_, _, run)| run.refused && run.before != run.after)
        .map(|(row, with_store, run)| {
            let store = if *with_store {
                "existing store"
            } else {
                "no store"
            };
            let tail: String = run
                .output
                .lines()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .join(" | ");
            format!("{row} ({store}): {tail}")
        })
        .collect();
    assert!(
        changed.is_empty(),
        "a run refused for the home changed the state store:\n{}",
        changed.join("\n")
    );
}

/// A run refused for want of a home leaves the state store as it found it
/// and runs no action when the refusal comes from ONE `~/` slot, so each
/// slot's own road to the store is exercised where no other slot refuses
/// first. Each fixture in [`LONE_SLOTS`] runs every verb that reads it, over
/// no store and over an existing one, and the verb each row names is asserted
/// refused, so the pin cannot pass by never reaching the slot. The rows are
/// the slots of [`HOME_REFUSAL_SLOTS`], which the `expand_tilde` walk derives
/// from every production expansion, so a new slot fails here until it has a
/// row.
///
/// [`HOME_REFUSAL_SLOTS`]: cfgd_core::reconciler::HOME_REFUSAL_SLOTS
#[test]
fn every_lone_home_slot_refusal_leaves_the_state_store_as_it_found_it() {
    use cfgd_core::reconciler::HomeQuestioner;
    const VERBS: &[&[&str]] = &[
        &["apply"],
        &["plan"],
        &["diff"],
        &["status", "--scan"],
        &["verify"],
        &["compliance"],
        &["compliance", "export"],
        &["backup", "run"],
        &["diff", "--module", "plain"],
        &["status", "--module", "plain", "--scan"],
    ];
    // The module verbs fold the env file's `PATH` line against the home on
    // every chain, so every row refuses them.
    const MODULE_VERBS: &[&str] = &["diff --module plain", "status --module plain --scan"];
    let slot_of = |name: &str| name.split(" (").next().unwrap_or(name).to_string();
    let mut questioner_of = std::collections::BTreeMap::new();
    for (role, slot, questioner) in cfgd_core::reconciler::HOME_REFUSAL_SLOTS {
        let first = *questioner_of.entry(slot.to_string()).or_insert(*questioner);
        assert_eq!(
            first, *questioner,
            "every role filling the slot `{slot}` is asked by one questioner (`{role}`)"
        );
    }
    let rows: std::collections::BTreeSet<String> = LONE_SLOTS
        .iter()
        .map(|(name, _, _)| slot_of(name))
        .filter(|slot| slot != "no slot")
        .collect();
    assert_eq!(
        rows,
        questioner_of.keys().cloned().collect(),
        "LONE_SLOTS holds a row for exactly the slots HOME_REFUSAL_SLOTS names"
    );
    // Each row's verbs are its questioner's: `unresolved_home_refusal` is
    // asked by `apply` and `plan` alike, the compliance check by the
    // `compliance` verb that reads the slot.
    let mut must_refuse = Vec::new();
    for (name, verb, _) in LONE_SLOTS {
        let Some(questioner) = questioner_of.get(&slot_of(name)) else {
            continue;
        };
        let own: Vec<&str> = match questioner {
            HomeQuestioner::UnresolvedHomeRefusal => vec!["apply", "plan"],
            HomeQuestioner::Compliance => vec![*verb],
        };
        assert!(
            own.contains(verb)
                && (*questioner != HomeQuestioner::Compliance || verb.starts_with("compliance")),
            "the `{name}` row names `{verb}`, a verb its questioner {questioner:?} does not ask"
        );
        if host_asks(&slot_of(name)) {
            must_refuse.extend(
                own.into_iter()
                    .chain(MODULE_VERBS.iter().copied())
                    .map(|verb| format!("{name}: {verb}")),
            );
        }
    }
    let jobs: Vec<(&str, HomelessFixture, Vec<String>)> = LONE_SLOTS
        .iter()
        .flat_map(|(slot, _, fixture)| {
            VERBS.iter().map(move |verb| {
                (
                    *slot,
                    *fixture,
                    verb.iter().map(|a| a.to_string()).collect(),
                )
            })
        })
        .collect();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let chunk = jobs.len().div_ceil(threads);
    let results: Vec<(String, bool, HomelessRun)> = std::thread::scope(|scope| {
        jobs.chunks(chunk)
            .map(|jobs| {
                scope.spawn(move || {
                    let mut done = Vec::new();
                    for (slot, fixture, row) in jobs {
                        for with_store in [false, true] {
                            done.push((
                                format!("{slot}: {}", row.join(" ")),
                                with_store,
                                run_homeless_row(*fixture, row, with_store),
                            ));
                        }
                    }
                    done
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect()
    });
    let describe = |row: &str, with_store: bool, run: &HomelessRun| {
        let tail: String = run
            .output
            .lines()
            .rev()
            .take(4)
            .collect::<Vec<_>>()
            .join(" | ");
        format!("{row} (existing store: {with_store}): {tail}")
    };
    // A row that must refuse is judged on all three facts whether or not its
    // output names the home: an action that failed at run time for the home
    // names it too, and only an untouched store and managed file tell that
    // run apart from a refusal before any action ran.
    let mut failures = Vec::new();
    for (row, with_store, run) in &results {
        let must = must_refuse.contains(row);
        if must && !run.refused {
            failures.push(format!(
                "never reached the slot it exists for: {}",
                describe(row, *with_store, run)
            ));
        }
        if !(must || run.refused) {
            continue;
        }
        if run.before != run.after {
            failures.push(format!(
                "changed the state store: {}",
                describe(row, *with_store, run)
            ));
        }
        if run.managed[0].as_deref() != Some(LONE_MANAGED_BYTES.as_bytes())
            || run.managed[0] != run.managed[1]
        {
            failures.push(format!(
                "ran an action, rewriting {LONE_MANAGED_TARGET}: {}",
                describe(row, *with_store, run)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "a homeless run over one slot did not refuse before any action ran:\n{}",
        failures.join("\n")
    );
}
