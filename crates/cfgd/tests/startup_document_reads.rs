//! The alias pass and clap settle on one startup document, and its reload
//! count says whether clap moved it.
//!
//! The alias pass reads the document before the tracing subscriber exists, so
//! `main` reports the startup document in one `loaded config document` debug
//! line, after the last place the path can move. `reads=1` means clap settled
//! on the file the alias pass read; each move clap or the macOS config move
//! makes adds one. The line counts the startup document alone: a loader call
//! elsewhere before dispatch never reaches it, and
//! `the_pre_dispatch_path_loads_the_document_once` (`src/cli/tests.rs`) is the
//! walk that fails on one.

mod cfgd_binary;
use cfgd_binary::cfgd_bin;

const SUMMARY_LINE: &str = "loaded config document";

/// How `stderr_of` names the config document to the run.
enum Config<'a> {
    Default,
    Flag(&'a std::path::Path),
    Env(&'a std::path::Path),
}

fn stderr_of(config: Config<'_>, verb: &[&str]) -> String {
    let mut cmd = cfgd_bin().expect("the cfgd binary builds");
    cmd.env_remove("RUST_LOG").arg("-v");
    match config {
        Config::Default => {}
        Config::Flag(path) => {
            cmd.arg("--config").arg(path);
        }
        Config::Env(path) => {
            cmd.env(cfgd_core::CFGD_CONFIG_ENV, path);
        }
    }
    let output = cmd.args(verb).output().expect("cfgd runs");
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn write_fixture(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("profiles")).expect("mkdir profiles");
    std::fs::write(
        dir.join("cfgd.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: once\nspec:\n  profile: base\n",
    )
    .expect("write config");
    std::fs::write(
        dir.join("profiles/base.yaml"),
        "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec: {}\n",
    )
    .expect("write profile");
}

fn assert_reads(verb: &[&str], stderr: &str, reads: u32) {
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains(SUMMARY_LINE))
        .collect();
    assert_eq!(lines.len(), 1, "{verb:?}: one summary line:\n{stderr}");
    assert!(
        lines[0].contains(&format!("reads={reads} ")) && lines[0].contains("found=true"),
        "{verb:?}: {reads} reads, and the document found:\n{stderr}"
    );
}

#[test]
fn the_alias_pass_and_clap_settle_on_one_document_for_every_verb() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture(dir.path());
    let config = dir.path().join("cfgd.yaml");

    for verb in [
        &["status"][..],
        &["profile", "show"][..],
        &["config", "get", "theme.name"][..],
    ] {
        assert_reads(verb, &stderr_of(Config::Flag(&config), verb), 1);
    }
}

/// With no `--config`, the alias pass and clap both land on the default
/// document under the isolated config home.
#[test]
fn a_run_without_config_settles_on_the_default_document_with_no_reload() {
    let probe = cfgd_bin().expect("the cfgd binary builds");
    let config_home = probe
        .get_envs()
        .find(|(var, _)| *var == "XDG_CONFIG_HOME")
        .and_then(|(_, value)| value)
        .map(std::path::PathBuf::from)
        .expect("cfgd_bin isolates XDG_CONFIG_HOME");
    write_fixture(&config_home.join("cfgd"));
    assert_reads(&["status"], &stderr_of(Config::Default, &["status"]), 1);
}

/// `CFGD_CONFIG` is clap's alone: the alias pass reads the default document,
/// and the summary line counts the second read that lands on the named one.
#[test]
fn a_config_named_only_in_the_environment_counts_as_one_reload() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture(dir.path());
    let config = dir.path().join("cfgd.yaml");
    let stderr = stderr_of(Config::Env(&config), &["status"]);
    assert_reads(&["status"], &stderr, 2);
    assert!(
        stderr.contains(&config.display().to_string()),
        "the line names the document clap settled on:\n{stderr}"
    );
}

/// `config get` of a key the document leaves out answers from the startup
/// document's parsed config: the verb parses nothing itself. The alias pass
/// parses the startup document before the tracing subscriber exists, so any
/// `parsing config document` line is a second parse.
#[test]
fn config_get_of_an_undeclared_key_parses_the_document_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture(dir.path());
    let config = dir.path().join("cfgd.yaml");
    let verb = &["config", "get", "daemon.reconcile.interval"][..];
    let stderr = stderr_of(Config::Flag(&config), verb);
    assert_reads(verb, &stderr, 1);
    let parses: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains("parsing config document"))
        .collect();
    assert!(parses.is_empty(), "{verb:?}: a second parse:\n{stderr}");
}
