#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

use assert_cmd::Command;
use predicates::prelude::*;

/// Helper: create a minimal valid config directory with a profile.
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

#[test]
fn help_flag_shows_usage() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("cfgd"));
}

#[test]
fn version_flag_shows_version() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("cfgd"));
}

#[test]
fn status_without_config_shows_error() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("nonexistent").join("cfgd.yaml");

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("status")
        .arg("--config")
        .arg(&nonexistent)
        .assert()
        .failure();
}

#[test]
fn plan_without_config_shows_error() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("nonexistent").join("cfgd.yaml");

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("plan")
        .arg("--config")
        .arg(&nonexistent)
        .assert()
        .failure();
}

#[test]
fn unknown_subcommand_shows_error() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("nonexistent-command")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}

#[test]
fn apply_dry_run_with_empty_config() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    create_valid_config(config_dir);

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--dry-run"])
        .arg("--config")
        .arg(config_dir.join("cfgd.yaml"))
        .assert()
        .success();
}

#[test]
fn config_dir_arg_infers_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    create_valid_config(config_dir);

    // Pass the directory, not the file — cfgd should infer cfgd.yaml inside it.
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--dry-run"])
        .arg("--config")
        .arg(config_dir)
        .assert()
        .success();
}

#[test]
fn config_env_var_dir_infers_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path();

    create_valid_config(config_dir);

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--dry-run"])
        .env("CFGD_CONFIG", config_dir)
        .assert()
        .success();
}

#[test]
fn help_subcommand_shows_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Usage"));
}

#[test]
fn status_subcommand_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("status"));
}

#[test]
fn plan_subcommand_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["plan", "--help"])
        .assert()
        .success();
}

#[test]
fn apply_subcommand_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--help"])
        .assert()
        .success();
}

#[test]
fn config_env_var_is_respected() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("via-env").join("cfgd.yaml");

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("status")
        .env("CFGD_CONFIG", &nonexistent)
        .assert()
        .failure();
}

// --- daemon subcommand help tests ---

#[test]
fn daemon_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["daemon", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("daemon"));
}

#[test]
fn daemon_run_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["daemon", "run", "--help"])
        .assert()
        .success();
}

#[test]
fn daemon_install_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["daemon", "install", "--help"])
        .assert()
        .success();
}

#[test]
fn daemon_status_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["daemon", "status", "--help"])
        .assert()
        .success();
}

// --- module subcommand help tests ---

#[test]
fn module_list_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["module", "list", "--help"])
        .assert()
        .success();
}

#[test]
fn module_create_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["module", "create", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("module"));
}

#[test]
fn module_show_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["module", "show", "--help"])
        .assert()
        .success();
}

#[test]
fn module_update_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["module", "update", "--help"])
        .assert()
        .success();
}

// --- upgrade help ---

#[test]
fn upgrade_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["upgrade", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("upgrade"));
}

// --- generate help ---

#[test]
fn generate_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["generate", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("generate"));
}

// --- profile subcommand help tests ---

#[test]
fn profile_list_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["profile", "list", "--help"])
        .assert()
        .success();
}

#[test]
fn profile_show_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["profile", "show", "--help"])
        .assert()
        .success();
}

#[test]
fn profile_create_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["profile", "create", "--help"])
        .assert()
        .success();
}

// --- source subcommand help tests ---

#[test]
fn source_list_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["source", "list", "--help"])
        .assert()
        .success();
}

#[test]
fn source_add_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["source", "add", "--help"])
        .assert()
        .success();
}

#[test]
fn source_show_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["source", "show", "--help"])
        .assert()
        .success();
}

// --- secret subcommand help tests ---

#[test]
fn secret_encrypt_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["secret", "encrypt", "--help"])
        .assert()
        .success();
}

#[test]
fn secret_init_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["secret", "init", "--help"])
        .assert()
        .success();
}

// --- config subcommand help tests ---

#[test]
fn config_show_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["config", "show", "--help"])
        .assert()
        .success();
}

#[test]
fn config_get_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["config", "get", "--help"])
        .assert()
        .success();
}

#[test]
fn config_set_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["config", "set", "--help"])
        .assert()
        .success();
}

// --- explain command ---

#[test]
fn explain_without_args_shows_overview() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("explain")
        .assert()
        .success();
}

#[test]
fn explain_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["explain", "--help"])
        .assert()
        .success();
}

// --- compliance subcommand help ---

#[test]
fn compliance_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["compliance", "--help"])
        .assert()
        .success();
}

#[test]
fn compliance_export_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["compliance", "export", "--help"])
        .assert()
        .success();
}

#[test]
fn compliance_history_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["compliance", "history", "--help"])
        .assert()
        .success();
}

// --- workflow subcommand help ---

#[test]
fn workflow_generate_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["workflow", "generate", "--help"])
        .assert()
        .success();
}

// --- completion command ---

#[test]
fn completion_bash() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["completion", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cfgd"));
}

#[test]
fn completion_zsh() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["completion", "zsh"])
        .assert()
        .success();
}

#[test]
fn completion_fish() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["completion", "fish"])
        .assert()
        .success();
}

// The `completions` (plural) alias is retained for back-compat; this guards
// against accidental removal of `alias = "completions"` in cli/mod.rs.
#[test]
fn completions_alias_still_works() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cfgd"));
}

// --- status with valid config ---

#[test]
fn status_with_valid_config_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("status")
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- plan with valid config ---

#[test]
fn plan_with_valid_config_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("plan")
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- verify with valid config ---

#[test]
fn verify_with_valid_config_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("verify")
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

/// Full `cfgd verify` on a modules-only profile must resolve the profile's
/// modules — not report "No managed resources to verify". A content-drifted
/// module file drives a failure (exit 5 with --exit-code). Regression guard for
/// the full path that previously passed `Vec::new()` modules.
#[test]
fn verify_full_path_resolves_modules_and_catches_module_file_drift() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();

    // Module with one file (source relative to the module dir); deploy a
    // tampered target so the failure is driven by genuine CONTENT drift, not a
    // missing source.
    let module_dir = dir.path().join("modules").join("accmod");
    std::fs::create_dir_all(&module_dir).unwrap();
    std::fs::write(module_dir.join("conf"), "desired\n").unwrap();
    let module_target = dir.path().join("mod-out").join("conf");
    std::fs::create_dir_all(module_target.parent().unwrap()).unwrap();
    std::fs::write(&module_target, "tampered\n").unwrap();

    let module_yaml = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: accmod\nspec:\n  packages: []\n  files:\n    - source: conf\n      target: {}\n",
        module_target.display()
    );
    std::fs::write(module_dir.join("module.yaml"), module_yaml).unwrap();

    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: base\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("profiles/base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  modules:\n    - accmod\n",
    )
    .unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .arg("verify")
        .arg("--exit-code")
        .arg("--no-color")
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(5);
    // The human Doc renders to stderr; stdout is reserved for structured `-o`.
    let out = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        !out.contains("No managed resources to verify"),
        "modules-only profile must be verified, got:\n{out}"
    );
    assert!(
        out.contains("conf") && out.contains("differs"),
        "drifted module file must surface as content drift, got:\n{out}"
    );
}

// --- diff with valid config ---

#[test]
fn diff_with_valid_config_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("diff")
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- doctor with valid config ---

#[test]
fn doctor_with_valid_config_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("doctor")
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- doctor without config still succeeds (reports problems) ---

#[test]
fn doctor_without_config_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("gone").join("cfgd.yaml");

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("doctor")
        .env("CFGD_CONFIG", &nonexistent)
        .assert()
        .success();
}

// --- log with valid config (empty state) ---

#[test]
fn log_with_valid_config_shows_empty() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("log")
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .env("CFGD_STATE_DIR", &state_dir)
        .assert()
        .success();
}

// --- module list with valid config (no modules) ---

#[test]
fn module_list_with_valid_config() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["module", "list"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- profile list with valid config ---

#[test]
fn profile_list_with_valid_config() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["profile", "list"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- profile show with valid config ---

#[test]
fn profile_show_with_valid_config() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["profile", "show"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- source list with valid config (no sources) ---

#[test]
fn source_list_with_valid_config() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["source", "list"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- config show with valid config ---

#[test]
fn config_show_with_valid_config() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["config", "show"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- JSON output format ---

#[test]
fn status_json_output() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["status", "-o", "json"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

#[test]
fn plan_json_output() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["plan", "-o", "json"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- YAML output format ---

#[test]
fn status_yaml_output() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["status", "-o", "yaml"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- verbose and quiet flags ---

#[test]
fn verbose_flag_accepted() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["--verbose", "status"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

#[test]
fn quiet_flag_accepted() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["--quiet", "status"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

#[test]
fn verbose_and_quiet_conflict() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["--verbose", "--quiet", "status"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("cannot be used with"));
}

// --- no-color flag ---

#[test]
fn no_color_flag_accepted() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["--no-color", "status"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- plan with phase filter ---

#[test]
fn plan_with_phase_filter() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["plan", "--phase", "packages"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- apply dry-run with skip and only flags ---

#[test]
fn apply_dry_run_with_skip_flag() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--dry-run", "--skip", "packages.brew.ripgrep"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

#[test]
fn apply_dry_run_with_only_flag() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--dry-run", "--only", "packages"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- checkin without server_url argument fails (missing required arg) ---

#[test]
fn checkin_missing_server_url_fails() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("checkin")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--server-url"));
}

// --- enroll without server_url argument fails ---

#[test]
fn enroll_missing_server_url_fails() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("enroll")
        .assert()
        .failure()
        .stderr(predicate::str::contains("--server-url"));
}

// --- explain with known resource type ---

#[test]
fn explain_module_resource() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["explain", "module"])
        .assert()
        .success();
}

#[test]
fn explain_profile_resource() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["explain", "profile"])
        .assert()
        .success();
}

// --- sync and pull without valid origin ---

#[test]
fn sync_without_config_shows_error() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("gone").join("cfgd.yaml");

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("sync")
        .env("CFGD_CONFIG", &nonexistent)
        .assert()
        .failure();
}

#[test]
fn pull_without_config_shows_error() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("gone").join("cfgd.yaml");

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("pull")
        .env("CFGD_CONFIG", &nonexistent)
        .assert()
        .failure();
}

// --- decide subcommand ---

#[test]
fn decide_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["decide", "--help"])
        .assert()
        .success();
}

// --- rollback subcommand ---

#[test]
fn rollback_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["rollback", "--help"])
        .assert()
        .success();
}

// --- mcp-server help ---

#[test]
fn mcp_server_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["mcp-server", "--help"])
        .assert()
        .success();
}

// --- checkin help ---

#[test]
fn checkin_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["checkin", "--help"])
        .assert()
        .success();
}

// --- enroll help ---

#[test]
fn enroll_help() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["enroll", "--help"])
        .assert()
        .success();
}

// --- log with --limit flag ---

#[test]
fn log_with_limit_flag() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = dir.path().join("state");
    std::fs::create_dir_all(&state_dir).unwrap();

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["log", "-n", "5"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .env("CFGD_STATE_DIR", &state_dir)
        .assert()
        .success();
}

// --- diff without config shows error ---

#[test]
fn diff_without_config_shows_error() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("gone").join("cfgd.yaml");

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("diff")
        .env("CFGD_CONFIG", &nonexistent)
        .assert()
        .failure();
}

// --- verify without config shows error ---

#[test]
fn verify_without_config_shows_error() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("gone").join("cfgd.yaml");

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("verify")
        .env("CFGD_CONFIG", &nonexistent)
        .assert()
        .failure();
}

// --- module list without config still succeeds (shows warning) ---

#[test]
fn module_list_without_config_shows_warning() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("gone").join("cfgd.yaml");

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["module", "list"])
        .env("CFGD_CONFIG", &nonexistent)
        .assert()
        .success();
}

// --- profile list without config still succeeds (shows warning) ---

#[test]
fn profile_list_without_config_shows_warning() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("gone").join("cfgd.yaml");

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["profile", "list"])
        .env("CFGD_CONFIG", &nonexistent)
        .assert()
        .success();
}

// --- apply dry-run with --skip-scripts flag ---

#[test]
fn apply_dry_run_with_skip_scripts() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["apply", "--dry-run", "--skip-scripts"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- plan with --context flag ---

#[test]
fn plan_with_context_reconcile() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["plan", "--context", "reconcile"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- profile override via --profile flag ---

#[test]
fn profile_override_flag() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());

    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["--profile", "base", "status"])
        .env("CFGD_CONFIG", dir.path().join("cfgd.yaml"))
        .assert()
        .success();
}

// --- invalid output format ---

#[test]
fn invalid_output_format_shows_error() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "invalid-format", "status"])
        .assert()
        .failure();
}

// ─── Exit-code taxonomy (cfgd_core::exit::ExitCode) ─────────────────────
//
// These tests lock the wire-level exit codes scripted consumers depend on.
// Any change to a code number below is a breaking change and must bump
// the CLI major version + land in release notes.

/// ExitCode::NoConfig = 3 — `cfgd status` with a config path that does
/// not exist.
#[test]
fn exit_code_no_config_is_3() {
    let dir = tempfile::tempdir().unwrap();
    let nonexistent = dir.path().join("nonexistent").join("cfgd.yaml");

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("status")
        .arg("--config")
        .arg(&nonexistent)
        .assert()
        .code(3);
}

/// ExitCode::ConfigInvalid = 4 — `cfgd status` with malformed YAML.
#[test]
fn exit_code_config_invalid_is_4() {
    let dir = tempfile::tempdir().unwrap();
    let bad_config = dir.path().join("cfgd.yaml");
    // Valid YAML that fails schema validation (missing required apiVersion/kind).
    std::fs::write(&bad_config, "spec: { this is not valid cfgd: true\n").unwrap();

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("status")
        .arg("--config")
        .arg(&bad_config)
        .assert()
        .code(4);
}

/// ExitCode::Success = 0 — valid config, no drift, default flags.
#[test]
fn exit_code_success_is_0() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("status")
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(0);
}

/// `cfgd status --exit-code` with no drift is still 0 — the flag only
/// escalates when drift is actually present.
#[test]
fn exit_code_flag_with_no_drift_is_0() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    Command::cargo_bin("cfgd")
        .unwrap()
        .arg("status")
        .arg("--exit-code")
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(0);
}

/// `cfgd status --exit-code` with an out-of-band content-drifted managed file
/// must exit 5 AND render the file in the Drift section — the human verdict can
/// never contradict the exit code. Regression guard for the live-scan display.
#[test]
fn status_exit_code_renders_live_file_drift_not_no_drift() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: base\n",
    )
    .unwrap();

    // Managed source + a deployed target whose bytes were tampered out-of-band.
    std::fs::write(dir.path().join("dotfile"), "desired\n").unwrap();
    let target = dir.path().join("deployed.conf");
    std::fs::write(&target, "tampered\n").unwrap();

    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  files:\n    managed:\n      - source: dotfile\n        target: {}\n",
        target.display()
    );
    std::fs::write(dir.path().join("profiles/base.yaml"), profile).unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .arg("status")
        .arg("--exit-code")
        .arg("--no-color")
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(5);
    // The human Doc renders to stderr; stdout is reserved for structured `-o`.
    let out = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        out.contains("deployed.conf"),
        "drift section must name the drifted file, got:\n{out}"
    );
    assert!(
        !out.contains("No drift detected"),
        "verdict must not contradict exit 5, got:\n{out}"
    );
}

/// `cfgd source update` exits 1 (ExitCode::Error) when every configured source
/// fails to update. A scripted consumer must detect the failure from `$?`
/// alone, and the per-source failure line must still render — the exit code can
/// never silently contradict the output. Regression guard for the wiring that
/// turns `run_source_update`'s error count into a nonzero exit.
#[test]
fn source_update_all_failed_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: default\n  sources:\n    - name: my-source\n      origin:\n        url: file:///nonexistent/repo.git\n        branch: main\n        type: Git\n      subscription:\n        priority: 300\n",
    )
    .unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .arg("source")
        .arg("update")
        .arg("--no-color")
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(1);
    let out = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        out.contains("Failed to update source 'my-source'"),
        "stderr must name the failed source, got:\n{out}"
    );
}

/// ExitCode::NotFound = 6 — `cfgd module show <missing>` against a valid config.
/// The dedicated not-found code must reach the process, and the structured
/// payload must keep the stable `not_found` kind for acceptance oracles.
#[test]
fn exit_code_module_show_missing_is_6() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "module", "show", "nosuchmod"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "not_found");
    assert_eq!(v["name"], "nosuchmod");
}

/// ExitCode::NotFound = 6 — `cfgd profile show <missing>`.
#[test]
fn exit_code_profile_show_missing_is_6() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "profile", "show", "nosuchprof"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "not_found");
}

/// ExitCode::NotFound = 6 — `cfgd profile switch <missing>`.
#[test]
fn exit_code_profile_switch_missing_is_6() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "profile", "switch", "nosuchprof"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "not_found");
}

/// ExitCode::NotFound = 6 — `cfgd source show <missing>`.
#[test]
fn exit_code_source_show_missing_is_6() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "source", "show", "nosuchsrc"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "not_found");
}

/// `cfgd source update <missing>` against a ZERO-source config must NOT report a
/// false success (exit 0, `{"sources":[]}`) — a named-but-absent source is a
/// NotFound: exit 6 + `not_found` payload. Regression guard for S15-D.
#[test]
fn source_update_missing_name_zero_sources_is_6_not_found() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "source", "update", "nosuchsrc"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "not_found");
    assert_eq!(v["name"], "nosuchsrc");
}

/// `cfgd source update <missing>` against a config that HAS other sources is also
/// a NotFound (exit 6) for the named-but-absent source.
#[test]
fn source_update_missing_name_with_other_sources_is_6_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: base\n  sources:\n    - name: other\n      origin:\n        url: https://example.com/foo.git\n        branch: main\n        type: Git\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("profiles/base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
    )
    .unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "source", "update", "nosuchsrc"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "not_found");
    assert_eq!(v["name"], "nosuchsrc");
}

/// ExitCode::NotFound = 6 — `cfgd source remove <missing>`. Mutating commands
/// must share the not-found exit code with the read-only `show` commands.
#[test]
fn exit_code_source_remove_missing_is_6() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "source", "remove", "nosuchsrc"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "not_found");
}

/// ExitCode::NotFound = 6 — `cfgd profile delete <missing>`.
#[test]
fn exit_code_profile_delete_missing_is_6() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "profile", "delete", "--yes", "nosuchprof"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "not_found");
}

/// GUARD must NOT move: `cfgd profile delete <ACTIVE>` is a precondition failure
/// (`active_profile`), not a not-found — it stays ExitCode::Error (1). `base` is
/// the active profile in `create_valid_config`. Regression guard for S8.
#[test]
fn exit_code_profile_delete_active_stays_1() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "profile", "delete", "--yes", "base"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(1);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(
        v["error"], "active_profile",
        "guard kind must not become not_found"
    );
}

/// ExitCode::NotFound = 6 — `cfgd module delete <missing>`.
#[test]
fn exit_code_module_delete_missing_is_6() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "module", "delete", "--yes", "nosuchmod"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "not_found");
}

/// ExitCode::NotFound = 6 — `cfgd module registry remove <missing>`. Removing an
/// absent registry is a strict not-found (json kind `registry_not_found`), NOT an
/// idempotent exit-0 no-op — uniform with every other named-resource miss.
#[test]
fn exit_code_module_registry_remove_missing_is_6() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: base\n  modules:\n    registries:\n      - name: community\n        url: https://github.com/cfgd-community/modules.git\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("profiles/base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
    )
    .unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "module", "registry", "remove", "nosuchreg"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "registry_not_found");
}

/// ExitCode::NotFound = 6 — `cfgd module registry rename <missing> <new>`.
#[test]
fn exit_code_module_registry_rename_missing_is_6() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: base\n  modules:\n    registries:\n      - name: community\n        url: https://github.com/cfgd-community/modules.git\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("profiles/base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
    )
    .unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args([
            "-o",
            "json",
            "module",
            "registry",
            "rename",
            "nosuchreg",
            "fresh",
        ])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "registry_not_found");
}

// --- `--ignore-not-found` (kubectl-style idempotent delete opt-in) ---
//
// For each of the four destructive verbs, an absent named resource + the flag
// is an exit-0 no-op carrying `{"removed":false,"reason":"not_found"}`; without
// the flag the strict exit-6 not-found behavior is unchanged.

/// Config WITH a registry so `module registry remove` exercises the
/// NotFound (not NoRegistries) branch.
fn config_with_registry(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("profiles")).unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: base\n  modules:\n    registries:\n      - name: community\n        url: https://github.com/cfgd-community/modules.git\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("profiles/base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
    )
    .unwrap();
}

#[test]
fn module_delete_missing_ignore_not_found_is_0() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    // Human: exit 0, success line on stderr.
    let human = Command::cargo_bin("cfgd")
        .unwrap()
        .args([
            "module",
            "delete",
            "--yes",
            "--ignore-not-found",
            "nosuchmod",
        ])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(0);
    let err = String::from_utf8_lossy(&human.get_output().stderr).to_string();
    assert!(
        err.contains("module 'nosuchmod' not found; nothing to remove (--ignore-not-found)"),
        "human no-op line, got:\n{err}"
    );

    // JSON: exit 0, structured no-op payload on stdout.
    let json = Command::cargo_bin("cfgd")
        .unwrap()
        .args([
            "-o",
            "json",
            "module",
            "delete",
            "--yes",
            "--ignore-not-found",
            "nosuchmod",
        ])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(0);
    let out = String::from_utf8_lossy(&json.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["removed"], false);
    assert_eq!(v["reason"], "not_found");
    assert_eq!(v["kind"], "module");
    assert_eq!(v["name"], "nosuchmod");
}

#[test]
fn module_delete_missing_without_flag_still_6() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "module", "delete", "--yes", "nosuchmod"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "not_found");
}

#[test]
fn module_registry_remove_missing_ignore_not_found_is_0() {
    let dir = tempfile::tempdir().unwrap();
    config_with_registry(dir.path());

    let human = Command::cargo_bin("cfgd")
        .unwrap()
        .args([
            "module",
            "registry",
            "remove",
            "--ignore-not-found",
            "nosuchreg",
        ])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .assert()
        .code(0);
    let err = String::from_utf8_lossy(&human.get_output().stderr).to_string();
    assert!(
        err.contains("registry 'nosuchreg' not found; nothing to remove (--ignore-not-found)"),
        "human no-op line, got:\n{err}"
    );

    let json = Command::cargo_bin("cfgd")
        .unwrap()
        .args([
            "-o",
            "json",
            "module",
            "registry",
            "remove",
            "--ignore-not-found",
            "nosuchreg",
        ])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .assert()
        .code(0);
    let out = String::from_utf8_lossy(&json.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["removed"], false);
    assert_eq!(v["reason"], "not_found");
    assert_eq!(v["kind"], "registry");
    assert_eq!(v["name"], "nosuchreg");
}

#[test]
fn module_registry_remove_missing_without_flag_still_6() {
    let dir = tempfile::tempdir().unwrap();
    config_with_registry(dir.path());

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "module", "registry", "remove", "nosuchreg"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "registry_not_found");
}

#[test]
fn source_remove_missing_ignore_not_found_is_0() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let human = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["source", "remove", "--ignore-not-found", "nosuchsrc"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(0);
    let err = String::from_utf8_lossy(&human.get_output().stderr).to_string();
    assert!(
        err.contains("source 'nosuchsrc' not found; nothing to remove (--ignore-not-found)"),
        "human no-op line, got:\n{err}"
    );

    let json = Command::cargo_bin("cfgd")
        .unwrap()
        .args([
            "-o",
            "json",
            "source",
            "remove",
            "--ignore-not-found",
            "nosuchsrc",
        ])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(0);
    let out = String::from_utf8_lossy(&json.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["removed"], false);
    assert_eq!(v["reason"], "not_found");
    assert_eq!(v["kind"], "source");
    assert_eq!(v["name"], "nosuchsrc");
}

#[test]
fn source_remove_missing_without_flag_still_6() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "source", "remove", "nosuchsrc"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "not_found");
}

#[test]
fn profile_delete_missing_ignore_not_found_is_0() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let human = Command::cargo_bin("cfgd")
        .unwrap()
        .args([
            "profile",
            "delete",
            "--yes",
            "--ignore-not-found",
            "nosuchprof",
        ])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(0);
    let err = String::from_utf8_lossy(&human.get_output().stderr).to_string();
    assert!(
        err.contains("profile 'nosuchprof' not found; nothing to remove (--ignore-not-found)"),
        "human no-op line, got:\n{err}"
    );

    let json = Command::cargo_bin("cfgd")
        .unwrap()
        .args([
            "-o",
            "json",
            "profile",
            "delete",
            "--yes",
            "--ignore-not-found",
            "nosuchprof",
        ])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(0);
    let out = String::from_utf8_lossy(&json.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["removed"], false);
    assert_eq!(v["reason"], "not_found");
    assert_eq!(v["kind"], "profile");
    assert_eq!(v["name"], "nosuchprof");
}

#[test]
fn profile_delete_missing_without_flag_still_6() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args(["-o", "json", "profile", "delete", "--yes", "nosuchprof"])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(6);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(v["error"], "not_found");
}

/// CRITICAL invariant: `--ignore-not-found` must ONLY silence the not-found
/// branch. Deleting the ACTIVE profile is a precondition failure that MUST still
/// exit 1 even with the flag set — `base` is active in `create_valid_config`.
#[test]
fn profile_delete_active_with_ignore_not_found_still_1() {
    let dir = tempfile::tempdir().unwrap();
    create_valid_config(dir.path());
    let state_dir = tempfile::tempdir().unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args([
            "-o",
            "json",
            "profile",
            "delete",
            "--yes",
            "--ignore-not-found",
            "base",
        ])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(1);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(
        v["error"], "active_profile",
        "active-profile guard must not be silenced by --ignore-not-found"
    );
}

/// CRITICAL invariant (symmetric with the active-profile case): the module
/// in-use guard MUST still fire with `--ignore-not-found` set — the flag only
/// silences the pure not-found branch, never a precondition failure. A profile
/// references the module, so deletion is refused (exit 1, `in_use`), NOT the
/// idempotent no-op.
#[test]
fn module_delete_in_use_with_ignore_not_found_still_errors() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("modules/used")).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: base\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("modules/used/module.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: used\nspec:\n  packages: []\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
    // The active profile references the module, tripping the in-use guard.
    std::fs::write(
        dir.path().join("profiles/base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  modules:\n    - used\n",
    )
    .unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args([
            "-o",
            "json",
            "module",
            "delete",
            "--yes",
            "--ignore-not-found",
            "used",
        ])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(1);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(
        v["error"], "in_use",
        "in-use guard must not be silenced by --ignore-not-found"
    );
    assert_ne!(
        v["reason"], "not_found",
        "must NOT emit the not-found no-op payload"
    );
}

/// CRITICAL invariant (symmetric with the active-profile case): the profile
/// inherited-by-others guard MUST still fire with `--ignore-not-found` set. A
/// child profile inherits the target, so deletion is refused (exit 1,
/// `inherited`), NOT the idempotent no-op. `shared` is deletable-by-existence
/// (not the active profile) so only the inherited guard is exercised.
#[test]
fn profile_delete_inherited_with_ignore_not_found_still_errors() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
    // `base` is active so the active-profile guard doesn't shadow this case.
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  profile: base\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("profiles/base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("profiles/shared.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: shared\nspec: {}\n",
    )
    .unwrap();
    // `child` inherits `shared`, tripping the inherited guard on `shared` delete.
    std::fs::write(
        dir.path().join("profiles/child.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: child\nspec:\n  inherits:\n    - shared\n",
    )
    .unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .args([
            "-o",
            "json",
            "profile",
            "delete",
            "--yes",
            "--ignore-not-found",
            "shared",
        ])
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(1);
    let out = String::from_utf8_lossy(&assert.get_output().stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("one json payload");
    assert_eq!(
        v["error"], "inherited",
        "inherited guard must not be silenced by --ignore-not-found"
    );
    assert_ne!(
        v["reason"], "not_found",
        "must NOT emit the not-found no-op payload"
    );
}

/// Plain `cfgd status` (no --exit-code) keeps the fast RECORDED-drift dashboard:
/// with no recorded events it shows "No drift detected" and exits 0 even when a
/// managed file is live-drifted. The live scan is `-e`-only by design.
#[test]
fn status_plain_keeps_recorded_dashboard_despite_live_drift() {
    let dir = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("profiles")).unwrap();
    std::fs::write(
        dir.path().join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: test\nspec:\n  profile: base\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("dotfile"), "desired\n").unwrap();
    let target = dir.path().join("deployed.conf");
    std::fs::write(&target, "tampered\n").unwrap();
    let profile = format!(
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  files:\n    managed:\n      - source: dotfile\n        target: {}\n",
        target.display()
    );
    std::fs::write(dir.path().join("profiles/base.yaml"), profile).unwrap();

    let assert = Command::cargo_bin("cfgd")
        .unwrap()
        .arg("status")
        .arg("--no-color")
        .arg("--config")
        .arg(dir.path().join("cfgd.yaml"))
        .arg("--state-dir")
        .arg(state_dir.path())
        .assert()
        .code(0);
    // The human Doc renders to stderr; stdout is reserved for structured `-o`.
    let out = String::from_utf8_lossy(&assert.get_output().stderr).to_string();
    assert!(
        out.contains("No drift detected"),
        "plain status shows recorded dashboard (no live scan), got:\n{out}"
    );
}

/// `cfgd upgrade --help` surfaces the exit-code taxonomy in the long_about
/// block — catches regressions where someone removes the documentation.
#[test]
fn upgrade_help_documents_exit_codes() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["upgrade", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("update available"))
        .stdout(predicate::str::contains("exit code"));
}

/// `cfgd status --help` advertises the --exit-code flag.
#[test]
fn status_help_documents_exit_code_flag() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["status", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--exit-code"))
        .stdout(predicate::str::contains("drift"));
}
