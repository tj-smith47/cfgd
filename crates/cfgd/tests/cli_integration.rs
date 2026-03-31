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

// --- completions command ---

#[test]
fn completions_bash() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("cfgd"));
}

#[test]
fn completions_zsh() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["completions", "zsh"])
        .assert()
        .success();
}

#[test]
fn completions_fish() {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(["completions", "fish"])
        .assert()
        .success();
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
