//! End-to-end coverage for `strategy: Patch` through the real `cfgd plan` /
//! `cfgd apply` entry points.
//!
//! `Patch` is the one strategy whose desired content is a function of the
//! target's *current* content, so the properties that matter cannot be observed
//! from a single call: a second apply must be a no-op, keys the spec never
//! mentions must survive every reconcile, and an out-of-band edit must be folded
//! back in rather than blindly overwritten. Each test drives the command
//! functions, not the file manager, so the plan → apply hand-off (which carries
//! the `patch` block through `FileAction`) is exercised too.

mod common;

use std::path::Path;

use cfgd::cli::apply::cmd_apply;
use cfgd::cli::plan::cmd_plan;
use cfgd_core::output::{OutputFormat, Printer};
use cfgd_core::test_helpers::test_printer;

use common::{apply_args, cli_for, plan_args};

/// Config + state + home tempdirs for one test. The home tempdir keeps script
/// execution (whose default working directory is the user's home) off the real
/// `$HOME`.
struct Fixture {
    config_dir: tempfile::TempDir,
    state_dir: tempfile::TempDir,
    home: tempfile::TempDir,
    target: std::path::PathBuf,
}

impl Fixture {
    /// Profile with a single `Patch` managed file over `settings.json`, whose
    /// `patch:` block is the caller-supplied YAML (indented four spaces to sit
    /// under the managed-file entry).
    fn new(patch_block: &str) -> Self {
        let config_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let target = config_dir.path().join("out").join("settings.json");

        let profile = format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  inherits: []\n  modules: []\n  files:\n    managed:\n      - target: {}\n        strategy: Patch\n        patch:\n{}",
            cfgd_core::to_posix_string(&target),
            patch_block
        );
        let profiles_dir = config_dir.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("tiny.yaml"), &profile).unwrap();

        let config = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n";
        std::fs::write(config_dir.path().join("cfgd.yaml"), config).unwrap();

        Self {
            config_dir,
            state_dir,
            home,
            target,
        }
    }

    fn seed_target(&self, content: &str) {
        std::fs::create_dir_all(self.target.parent().unwrap()).unwrap();
        std::fs::write(&self.target, content).unwrap();
    }

    fn apply(&self) {
        let cli = cli_for(self.config_dir.path(), self.state_dir.path());
        let printer = test_printer();
        cfgd_core::with_test_home(self.home.path(), || {
            cmd_apply(&cli, &printer, &apply_args()).unwrap()
        });
    }

    /// Number of actions the next apply would take.
    fn planned_action_count(&self) -> u64 {
        let cli = cli_for(self.config_dir.path(), self.state_dir.path());
        let (printer, cap) = Printer::for_test_doc_with_format(OutputFormat::Json);
        cfgd_core::with_test_home(self.home.path(), || {
            cmd_plan(&cli, &printer, &plan_args()).unwrap()
        });
        drop(printer);
        cap.json().expect("plan doc carries a payload")["totalActions"]
            .as_u64()
            .expect("totalActions is a number")
    }

    fn target_json(&self) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(&self.target).unwrap()).unwrap()
    }
}

#[test]
fn apply_merges_ensured_keys_and_preserves_everything_else() {
    let fixture = Fixture::new(
        "          ensure:\n            editor:\n              tabSize: 4\n            telemetry: false\n",
    );
    fixture.seed_target(
        "{\n  \"runtimeToken\": \"issued-at-runtime\",\n  \"editor\": {\n    \"fontSize\": 12\n  }\n}\n",
    );

    fixture.apply();

    let written = fixture.target_json();
    assert_eq!(
        written["editor"]["tabSize"], 4,
        "the ensured key is applied"
    );
    assert_eq!(
        written["editor"]["fontSize"], 12,
        "a sibling key inside an ensured mapping must survive"
    );
    assert_eq!(
        written["runtimeToken"], "issued-at-runtime",
        "a top-level key the spec never mentions must survive"
    );
    assert_eq!(written["telemetry"], false);
}

#[test]
fn second_apply_plans_nothing_and_rewrites_nothing() {
    let fixture =
        Fixture::new("          ensure:\n            editor:\n              tabSize: 4\n");
    fixture.seed_target("{\n  \"runtimeToken\": \"issued-at-runtime\"\n}\n");

    fixture.apply();
    let after_first = std::fs::read_to_string(&fixture.target).unwrap();

    assert_eq!(
        fixture.planned_action_count(),
        0,
        "a converged Patch target must plan no work"
    );

    fixture.apply();
    assert_eq!(
        std::fs::read_to_string(&fixture.target).unwrap(),
        after_first,
        "a second apply must not rewrite a converged target"
    );
}

#[test]
fn apply_reconverges_after_out_of_band_drift() {
    let fixture = Fixture::new("          ensure:\n            telemetry: false\n");
    fixture.seed_target("{\n  \"runtimeToken\": \"issued-at-runtime\"\n}\n");
    fixture.apply();

    // Another tool rewrites the file: it flips the key cfgd owns and adds one
    // cfgd has never seen.
    std::fs::write(
        &fixture.target,
        "{\n  \"runtimeToken\": \"rotated\",\n  \"telemetry\": true,\n  \"addedByAnotherTool\": 1\n}\n",
    )
    .unwrap();

    assert_eq!(
        fixture.planned_action_count(),
        1,
        "drift on a Patch target must be planned as one action"
    );
    fixture.apply();

    let written = fixture.target_json();
    assert_eq!(written["telemetry"], false, "the ensured key is restored");
    assert_eq!(
        written["addedByAnotherTool"], 1,
        "the other tool's key must survive re-convergence"
    );
    assert_eq!(written["runtimeToken"], "rotated");
}

#[test]
fn plan_never_writes_the_target() {
    let fixture = Fixture::new("          ensure:\n            telemetry: false\n");

    assert_eq!(fixture.planned_action_count(), 1);
    assert!(
        !fixture.target.exists(),
        "planning a Patch file must not create its target"
    );
}

#[test]
fn an_unparseable_target_fails_planning_and_writes_nothing() {
    // Planning a `Patch` file has to parse the target, so a malformed one
    // fails there — the same place a missing source or an unreadable file
    // fails for every other strategy, and before anything is written.
    let fixture = Fixture::new("          ensure:\n            telemetry: false\n");
    fixture.seed_target("this is not json\n");

    let cli = cli_for(fixture.config_dir.path(), fixture.state_dir.path());
    let printer = test_printer();
    let err = cfgd_core::with_test_home(fixture.home.path(), || {
        cmd_apply(&cli, &printer, &apply_args()).unwrap_err()
    });
    assert!(
        err.to_string().contains("is not valid json"),
        "expected a typed patch-parse error, got: {err}"
    );

    assert_eq!(
        std::fs::read_to_string(&fixture.target).unwrap(),
        "this is not json\n",
        "an unparseable target must be left exactly as it was"
    );
}

/// Write `body` as an executable `/bin/sh` script at `path`.
#[cfg(unix)]
fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
#[cfg(unix)]
fn script_mode_resolves_relative_to_the_config_dir_and_sees_cfgd_env() {
    let fixture = Fixture::new("          script: scripts/ensure-line.sh\n");
    write_script(
        &fixture.config_dir.path().join("scripts/ensure-line.sh"),
        // Read stdin once: a second `cat` would see EOF and append forever.
        "#!/bin/sh\ncontent=$(cat)\nprintf '%s\\n' \"$content\"\nprintf '%s\\n' \"$content\" | grep -q \"^profile=\" || echo \"profile=$CFGD_PROFILE\"\n",
    );
    fixture.seed_target("keep=me\n");

    fixture.apply();

    let written = std::fs::read_to_string(&fixture.target).unwrap();
    assert_eq!(
        written, "keep=me\nprofile=tiny\n",
        "a relative script path must resolve under the config dir and see CFGD_PROFILE"
    );

    assert_eq!(
        fixture.planned_action_count(),
        0,
        "an idempotent filter must converge after one apply"
    );
    fixture.apply();
    assert_eq!(
        std::fs::read_to_string(&fixture.target).unwrap(),
        written,
        "re-applying an idempotent filter must not change the target"
    );
}

#[test]
#[cfg(unix)]
fn script_mode_accepts_an_inline_command() {
    let fixture = Fixture::new("          script: \"tr '[:lower:]' '[:upper:]'\"\n");
    fixture.seed_target("shout\n");

    fixture.apply();

    assert_eq!(
        std::fs::read_to_string(&fixture.target).unwrap(),
        "SHOUT\n",
        "an inline command must run when no script path resolves"
    );
}

#[test]
#[cfg(unix)]
fn a_failing_filter_leaves_the_target_untouched() {
    let fixture = Fixture::new("          script: \"echo nope >&2; exit 3\"\n");
    fixture.seed_target("original\n");

    let cli = cli_for(fixture.config_dir.path(), fixture.state_dir.path());
    let printer = test_printer();
    let err = cfgd_core::with_test_home(fixture.home.path(), || {
        cmd_apply(&cli, &printer, &apply_args()).unwrap_err()
    });
    assert!(
        err.to_string().contains("nope"),
        "the filter's stderr must reach the operator, got: {err}"
    );

    assert_eq!(
        std::fs::read_to_string(&fixture.target).unwrap(),
        "original\n",
        "a non-zero filter exit must not write the target"
    );
}

#[test]
#[cfg(unix)]
fn module_script_resolves_relative_to_the_module_dir() {
    // The binding a module-deployed `Patch` file gets is the module's own
    // directory: the same relative `script:` path that resolves for a profile
    // file must resolve against `modules/<name>/` here, and the filter must see
    // the module's metadata and declared env.
    let config_dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let target = config_dir.path().join("out").join("hosts");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "127.0.0.1 localhost\n").unwrap();

    // No `spec.env` on the module: applying one would push the value into the
    // developer's live systemd/launchd user session. `PatchBinding::module`'s
    // env injection is pinned by the cfgd-core unit tests instead.
    let module_dir = config_dir.path().join("modules/hosts-mod");
    let module = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: hosts-mod\nspec:\n  files:\n    - target: {}\n      strategy: Patch\n      patch:\n        script: ensure-host.sh\n",
        cfgd_core::to_posix_string(&target)
    );
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(module_dir.join("module.yaml"), module).unwrap();
    write_script(
        &module_dir.join("ensure-host.sh"),
        "#!/bin/sh\ncontent=$(cat)\nprintf '%s\\n' \"$content\"\nprintf '%s\\n' \"$content\" | grep -q build.internal || echo \"10.0.0.5 build.internal ($CFGD_MODULE_NAME)\"\n",
    );

    let profile = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: tiny\nspec:\n  modules:\n    - hosts-mod\n";
    let profiles_dir = config_dir.path().join("profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    std::fs::write(profiles_dir.join("tiny.yaml"), profile).unwrap();
    std::fs::write(
        config_dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: tiny\n",
    )
    .unwrap();

    let cli = cli_for(config_dir.path(), state_dir.path());
    let printer = test_printer();
    cfgd_core::with_test_home(home.path(), || {
        cmd_apply(&cli, &printer, &apply_args()).unwrap()
    });

    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "127.0.0.1 localhost\n10.0.0.5 build.internal (hosts-mod)\n",
        "the module's script must resolve under the module dir and see its metadata"
    );
}

#[test]
#[cfg(unix)]
#[serial_test::serial]
fn a_source_barred_from_scripts_cannot_run_its_patch_filter_from_any_surface() {
    // A `patch.script` is the one script surface a read-only command executes:
    // `diff` and `verify` evaluate every `Patch` file, and evaluating one runs
    // the filter. Report mode keeps composing so the read can still render, so
    // the block has to live in the spec itself rather than in the abort.
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (_workspace, config_dir, state_dir, target, marker) =
        common::barred_patch_script_source_setup();
    let home = tempfile::tempdir().unwrap();
    let cli = cli_for(config_dir.path(), state_dir.path());
    let original = std::fs::read_to_string(&target).unwrap();

    let (sync_printer, _sync_cap) = Printer::for_test_doc();
    cfgd::cli::sync::cmd_sync(&cli, &sync_printer).expect("the source must sync into the cache");
    drop(sync_printer);

    let assert_untouched = |surface: &str| {
        assert!(
            !marker.exists(),
            "{surface} must not run a filter the source is barred from running"
        );
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            original,
            "{surface} must not rewrite the target through a blocked filter"
        );
    };

    let (diff_printer, diff_cap) = Printer::for_test_doc();
    cfgd_core::with_test_home(home.path(), || {
        cfgd::cli::diff::cmd_diff(&cli, &diff_printer, None, false).expect("diff still renders")
    });
    drop(diff_printer);
    let diff_out = diff_cap.human();
    assert_untouched("diff");
    assert!(
        diff_out.contains("is blocked") && diff_out.contains("acme"),
        "diff must degrade the file with a status naming the source: {diff_out}"
    );
    assert!(
        diff_out.contains("violates its constraints"),
        "diff must warn that the source breaks its own constraints: {diff_out}"
    );

    let (json_printer, json_cap) = Printer::for_test_doc_with_format(OutputFormat::Json);
    cfgd_core::with_test_home(home.path(), || {
        cfgd::cli::diff::cmd_diff(&cli, &json_printer, None, false).expect("diff renders")
    });
    drop(json_printer);
    let payload = json_cap.json().expect("diff doc carries a payload");
    assert_untouched("diff -o json");
    let files = payload["files"].as_array().expect("files is an array");
    let blocked = files
        .iter()
        .find(|f| {
            f["target"]
                .as_str()
                .is_some_and(|t| t.ends_with("settings.json"))
        })
        .expect("the degraded file must appear in the structured payload");
    assert_eq!(blocked["matches"], serde_json::json!(false));
    assert!(
        blocked["actual"]
            .as_str()
            .is_some_and(|a| a.contains("is blocked") && a.contains("acme")),
        "the block reason must survive to the structured path: {blocked}"
    );

    let (verify_printer, verify_cap) = Printer::for_test_doc();
    cfgd_core::with_test_home(home.path(), || {
        cfgd::cli::verify::cmd_verify(&cli, &verify_printer, None, false)
            .expect("verify still renders")
    });
    drop(verify_printer);
    let verify_out = verify_cap.human();
    assert_untouched("verify");
    assert!(
        verify_out.contains("is blocked") && verify_out.contains("acme"),
        "verify must report the file as failing with the block as its reason: {verify_out}"
    );

    for (surface, result) in [
        ("plan", {
            let (printer, _cap) = Printer::for_test_doc();
            cfgd_core::with_test_home(home.path(), || {
                cmd_plan(&cli, &printer, &plan_args())
                    .err()
                    .map(|e| format!("{e:#}"))
            })
        }),
        ("apply", {
            let (printer, _cap) = Printer::for_test_doc();
            cfgd_core::with_test_home(home.path(), || {
                cmd_apply(&cli, &printer, &apply_args())
                    .err()
                    .map(|e| format!("{e:#}"))
            })
        }),
    ] {
        let msg = result.unwrap_or_else(|| panic!("{surface} must abort on a barred source"));
        assert!(
            msg.contains("acme") && msg.contains("not allowed to run scripts"),
            "{surface}'s abort must name the source and the constraint: {msg}"
        );
        assert_untouched(surface);
    }
}

#[test]
#[serial_test::serial]
fn the_allow_scripts_disclosure_reaches_the_operator_at_default_verbosity() {
    // The one line telling an operator that third-party code will run on their
    // machine cannot live behind `-v`.
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (_workspace, config_dir, state_dir, _target) = common::opted_in_script_source_setup(true);
    let home = tempfile::tempdir().unwrap();
    let cli = cli_for(config_dir.path(), state_dir.path());

    let (sync_printer, _sync_cap) = Printer::for_test_doc();
    cfgd::cli::sync::cmd_sync(&cli, &sync_printer).expect("the source must sync into the cache");
    drop(sync_printer);

    let (printer, cap) = Printer::for_test_doc();
    cfgd_core::with_test_home(home.path(), || {
        cfgd::cli::diff::cmd_diff(&cli, &printer, None, false).expect("diff renders")
    });
    drop(printer);
    let out = cap.human();

    assert!(
        out.contains("source 'acme' scripts will run because allowScripts is set"),
        "the disclosure must print without -v: {out}"
    );
    assert!(
        out.contains("a postApply script") && out.contains("a patch script for"),
        "the disclosure must name every surface it found: {out}"
    );
}

#[test]
#[serial_test::serial]
fn no_disclosure_is_printed_for_an_opted_in_source_that_ships_no_scripts() {
    let _allow = cfgd_core::test_helpers::EnvVarGuard::set("CFGD_ALLOW_LOCAL_SOURCES", "1");
    let (_workspace, config_dir, state_dir, _target) = common::opted_in_script_source_setup(false);
    let home = tempfile::tempdir().unwrap();
    let cli = cli_for(config_dir.path(), state_dir.path());

    let (sync_printer, _sync_cap) = Printer::for_test_doc();
    cfgd::cli::sync::cmd_sync(&cli, &sync_printer).expect("the source must sync into the cache");
    drop(sync_printer);

    let (printer, cap) = Printer::for_test_doc();
    cfgd_core::with_test_home(home.path(), || {
        cfgd::cli::diff::cmd_diff(&cli, &printer, None, false).expect("diff renders")
    });
    drop(printer);
    let out = cap.human();

    assert!(
        !out.contains("scripts will run"),
        "a source with no script surface has nothing to disclose: {out}"
    );
}
