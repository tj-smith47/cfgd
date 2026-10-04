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

const HOME_UNRESOLVED: &str = "cannot resolve home directory (HOME unset) to locate config at";

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
        stderr.contains(&format!("{HOME_UNRESOLVED} ~/cfgd.yaml")),
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
            .is_some_and(|m| m.contains(&format!("{HOME_UNRESOLVED} ~/cfgd.yaml"))),
        "payload: {v}"
    );
}

/// The default config location with no home set. Linux and macOS spell it
/// under `~`, so the run reports the unset home; Windows finds its config
/// root through a known-folder lookup that needs no home variable, so the
/// run reports the document that is not there.
#[test]
fn the_default_config_with_no_home_names_what_is_missing() {
    let expected = if cfg!(windows) {
        "config file not found: "
    } else {
        HOME_UNRESOLVED
    };
    for format in ["table", "json"] {
        let (stdout, stderr, code) = run_homeless(&["status", "-o", format]);
        assert_eq!(code, Some(3), "{format}: exits NoConfig(3): {stderr}");
        let reported = if format == "json" {
            let v = parse_single_json(&stdout);
            v["message"].as_str().unwrap_or_default().to_string()
        } else {
            stderr
        };
        assert!(reported.contains(expected), "{format}: {reported:?}");
        assert!(
            !reported.contains("/~/") && !reported.contains("\\~\\"),
            "{format}: a `~` joined under another directory: {reported:?}"
        );
    }
}
