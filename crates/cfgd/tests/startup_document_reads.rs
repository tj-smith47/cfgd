//! The real binary reads `cfgd.yaml` once before dispatch, whichever verb runs.
//!
//! The read happens before the tracing subscriber exists, so `main` reports
//! it in one `loaded config document` debug line carrying how many reads the
//! startup document took. A second pre-dispatch read shows as `reads=2`, and
//! a second startup document as a second line.

mod cfgd_binary;
use cfgd_binary::cfgd_bin;

const SUMMARY_LINE: &str = "loaded config document";

fn stderr_of(config: Option<&std::path::Path>, verb: &[&str]) -> String {
    let mut cmd = cfgd_bin().expect("the cfgd binary builds");
    cmd.env_remove("RUST_LOG").arg("-v");
    if let Some(config) = config {
        cmd.arg("--config").arg(config);
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

fn assert_read_once(verb: &[&str], stderr: &str) {
    let lines: Vec<&str> = stderr
        .lines()
        .filter(|l| l.contains(SUMMARY_LINE))
        .collect();
    assert_eq!(lines.len(), 1, "{verb:?}: one startup document:\n{stderr}");
    assert!(
        lines[0].contains("reads=1") && lines[0].contains("found=true"),
        "{verb:?}: read once and found:\n{stderr}"
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
        assert_read_once(verb, &stderr_of(Some(&config), verb));
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
    assert_read_once(&["status"], &stderr_of(None, &["status"]));
}
