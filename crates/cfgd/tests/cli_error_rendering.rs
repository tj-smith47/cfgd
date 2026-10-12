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

    // Exit 3 = NoConfig — proves cli_error_ctx's exit-code preservation survives the
    // real `std::process::exit`, end to end, not just at the unit level.
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
    let before = files_under_isolated_root(&probe);

    for args in [&["doctor"][..], &["doctor", "-o", "json"][..]] {
        let mut cmd = cfgd_bin().unwrap();
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
        let (_workspace, config_dir, _state) = cfgd_test_fixtures::local_source_setup("", |_| {
            (
                "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: acme\n  version: \"1.0.0\"\nspec:\n  provides:\n    profiles:\n      - default\n".to_string(),
                "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: default\nspec:\n  files:\n    managed:\n      - source: acmerc\n        target: ~/.acmerc\n".to_string(),
            )
        });
        let mut cmd = cfgd_bin().unwrap();
        cmd.env_remove("HOME")
            .env_remove("USERPROFILE")
            .env(cfgd_core::CFGD_ALLOW_LOCAL_SOURCES_ENV, "1")
            .arg("--config")
            .arg(config_dir.path().join("cfgd.yaml"))
            .args(verb)
            .args(["-o", "json"]);
        let state_dir = env_path(&cmd, cfgd_core::CFGD_STATE_DIR_ENV);
        let cache_dir = env_path(&cmd, cfgd_core::CFGD_CACHE_DIR_ENV);
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

/// A store a run with a home left behind records a manager `PATH` entry, which
/// lives in the managed env file. With no home, `apply` and `plan` read that
/// record without opening the store for writing and refuse naming the env
/// file, leaving the store's bytes, its `-wal` and `-shm` sidecars and its
/// schema version as they were, whether those sidecars are open or closed.
#[test]
fn a_homeless_run_over_a_recorded_path_entry_refuses_and_leaves_the_store_as_it_was() {
    for sidecars_open in [false, true] {
        for verb in [&["apply", "--yes"][..], &["plan"]] {
            let case = format!("{verb:?} with sidecars open: {sidecars_open}");
            let mut cmd = cfgd_bin().unwrap();
            let config_dir = home_of(&cmd).join(".config").join("cfgd");
            create_valid_config(&config_dir);
            let state_dir = env_path(&cmd, cfgd_core::CFGD_STATE_DIR_ENV);
            std::fs::create_dir_all(&state_dir).unwrap();
            let store = cfgd_core::state::StateStore::open(&state_dir.join("state.db")).unwrap();
            store
                .record_bootstrapped_path_dirs("brew", &["/opt/brew/bin".to_string()])
                .unwrap();
            let schema_version = || {
                rusqlite::Connection::open_with_flags(
                    state_dir.join("state.db"),
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )
                .unwrap()
                .query_row("SELECT version FROM schema_version", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
            };
            let version = schema_version();
            let held = sidecars_open.then_some(store);
            let files = ["state.db", "state.db-wal", "state.db-shm"];
            // The directory's mtime moves with any file created or removed in it,
            // so a `-wal` a writable open makes and a clean close removes again
            // still shows.
            let snapshot = || {
                (
                    files.map(|f| std::fs::read(state_dir.join(f)).ok()),
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
            assert_eq!(schema_version(), version, "{case}");
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
/// for writing. Over the store the plan was derived against, which records a
/// manager `PATH` entry, the replay refuses naming the managed env file and
/// leaves the store's files as they were; with no store on disk the plan is
/// stale and no store is created.
#[test]
fn a_homeless_replay_reads_its_store_without_opening_it_for_writing() {
    for store_on_disk in [true, false] {
        let mut with_home = cfgd_bin().unwrap();
        let config = home_of(&with_home).join(".config").join("cfgd");
        create_valid_config(&config);
        let config = config.join("cfgd.yaml");
        let state_dir = env_path(&with_home, cfgd_core::CFGD_STATE_DIR_ENV);
        std::fs::create_dir_all(&state_dir).unwrap();
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
        assert_eq!(out.status.code(), Some(0), "{stderr}");
        let plan_file = state_dir.parent().unwrap().join("plan.json");
        std::fs::write(&plan_file, &out.stdout).unwrap();
        let files = ["state.db", "state.db-wal", "state.db-shm"];
        if !store_on_disk {
            for file in files {
                let _ = std::fs::remove_file(state_dir.join(file));
            }
        }
        // The directory's mtime moves with any file created or removed in it.
        let snapshot = || {
            (
                files.map(|f| std::fs::read(state_dir.join(f)).ok()),
                std::fs::metadata(&state_dir).unwrap().modified().unwrap(),
            )
        };
        let before = snapshot();

        let mut replay = cfgd_bin().unwrap();
        let out = replay
            .env_remove("HOME")
            .env_remove("USERPROFILE")
            .env(cfgd_core::CFGD_STATE_DIR_ENV, &state_dir)
            .arg("--config")
            .arg(&config)
            .args(["apply", "--yes", "-o", "json", "--plan"])
            .arg(&plan_file)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        let v = parse_single_json(&String::from_utf8_lossy(&out.stdout));
        assert_ne!(out.status.code(), Some(0), "{store_on_disk}: {stderr}");
        if store_on_disk {
            assert!(
                v["message"]
                    .as_str()
                    .is_some_and(|m| m.contains("cannot write the managed env file")),
                "{v}"
            );
        } else {
            assert_eq!(v["error"], "stale", "{v}");
        }
        let after = snapshot();
        for ((file, before), after) in files.iter().zip(&before.0).zip(&after.0) {
            assert!(
                before == after,
                "{store_on_disk}: the replay changed {file}"
            );
        }
        assert_eq!(
            before.1, after.1,
            "{store_on_disk}: the replay created or removed a file beside the store"
        );
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
