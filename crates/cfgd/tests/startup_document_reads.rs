//! The real binary reads `cfgd.yaml` once before dispatch, whichever verb runs.
//!
//! The read happens before the tracing subscriber exists, so `main` reports
//! it in one `loaded config document` debug line carrying how many reads the
//! startup document took. A second pre-dispatch read shows as `reads=2`, and
//! a second startup document as a second line.

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
fn every_verb_reads_the_config_document_once_before_dispatch() {
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
fn a_run_without_config_reads_the_default_document_once() {
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
fn a_config_named_only_in_the_environment_is_read_a_second_time() {
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
