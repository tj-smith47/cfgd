//! The alias pass and clap settle on one startup document under every
//! spelling of the config location, and its reload count says whether clap
//! moved it.
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

/// The config home `cfgd_bin` points this test's runs at.
fn config_home() -> std::path::PathBuf {
    cfgd_bin()
        .expect("the cfgd binary builds")
        .get_envs()
        .find(|(var, _)| *var == "XDG_CONFIG_HOME")
        .and_then(|(_, value)| value)
        .map(std::path::PathBuf::from)
        .expect("cfgd_bin isolates XDG_CONFIG_HOME")
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
    let config_home = config_home();
    write_fixture(&config_home.join("cfgd"));
    assert_reads(&["status"], &stderr_of(Config::Default, &["status"]), 1);
}

/// A document named in the environment is the one the alias pass reads, so
/// clap settles on it with no reload.
#[test]
fn a_config_named_only_in_the_environment_is_read_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture(dir.path());
    let config = dir.path().join("cfgd.yaml");
    let stderr = stderr_of(Config::Env(&config), &["status"]);
    assert_reads(&["status"], &stderr, 1);
    assert!(
        stderr.contains(&config.display().to_string()),
        "the line names the document clap settled on:\n{stderr}"
    );
}

/// A config document declaring `profile` and a `who` alias that prints it.
fn write_alias_fixture(dir: &std::path::Path, profile: &str) {
    std::fs::create_dir_all(dir).expect("mkdir");
    std::fs::write(
        dir.join("cfgd.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: aliases\nspec:\n  profile: {profile}\n  aliases:\n    who: config get profile\n"
        ),
    )
    .expect("write config");
}

/// Every spelling of the config location expands the aliases of the document
/// it names: `who` is declared in that document alone, and prints its profile.
/// The default document carries no aliases, so a pass that read it instead
/// refuses `who` as an unknown command.
#[test]
fn every_spelling_of_the_config_location_expands_that_documents_aliases() {
    let config_home = config_home();
    write_fixture(&config_home.join("cfgd"));

    let dir = tempfile::tempdir().expect("tempdir");
    write_alias_fixture(dir.path(), "named");
    let x = dir.path().join("cfgd.yaml");
    let x_flag = format!("--config={}", x.display());
    let d = dir.path().as_os_str();

    type Row<'a> = (
        &'a str,
        Vec<&'a std::ffi::OsStr>,
        Option<(&'a str, &'a std::ffi::OsStr)>,
    );
    let rows: Vec<Row<'_>> = vec![
        ("--config X", vec!["--config".as_ref(), x.as_os_str()], None),
        ("--config=X", vec![x_flag.as_ref()], None),
        (
            "CFGD_CONFIG=X",
            vec![],
            Some((cfgd_core::CFGD_CONFIG_ENV, x.as_os_str())),
        ),
        ("--config-dir D", vec!["--config-dir".as_ref(), d], None),
        (
            "CFGD_CONFIG_DIR=D",
            vec![],
            Some((cfgd_core::CFGD_CONFIG_DIR_ENV, d)),
        ),
        (
            "--scope system",
            vec!["--scope".as_ref(), "system".as_ref()],
            Some(("CONFIGURATION_DIRECTORY", d)),
        ),
    ];
    let mut wrong = Vec::new();
    for (shape, args, env) in rows {
        let mut cmd = cfgd_bin().expect("the cfgd binary builds");
        cmd.env_remove("RUST_LOG").arg("-v").args(&args).arg("who");
        if let Some((var, value)) = env {
            cmd.env(var, value);
        }
        let output = cmd.output().expect("cfgd runs");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let summary = stderr
            .lines()
            .filter(|l| l.contains(SUMMARY_LINE))
            .collect::<Vec<_>>();
        if !output.status.success()
            || stdout.trim() != "named"
            || summary.len() != 1
            || !summary[0].contains("reads=1 ")
        {
            wrong.push(format!("{shape}: stdout {stdout:?}\n{stderr}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "the alias did not run from the named document once:\n{}",
        wrong.join("\n")
    );
}

/// With no location spelled, the aliases come from the default document.
#[test]
fn a_run_without_config_expands_the_default_documents_aliases() {
    let config_home = config_home();
    write_alias_fixture(&config_home.join("cfgd"), "fromdefault");
    let output = cfgd_bin()
        .expect("the cfgd binary builds")
        .arg("who")
        .output()
        .expect("cfgd runs");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "fromdefault",
        "{}",
        String::from_utf8_lossy(&output.stderr)
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
