#![allow(deprecated)] // assert_cmd 2.x cargo_bin deprecation; upgrade path is assert_cmd 3.x

//! `spec.output.theme.name` is a plain string in the document, so the preset
//! vocabulary is held at the two points a word enters: the setter that writes
//! one, and the load that reads one somebody else wrote.
//!
//! Both halves need the real binary. The setter's refusal ends in
//! `std::process::exit`, and the load-time warning is raised in `main.rs`
//! where the printer is built, which no in-process call reaches.

use std::path::Path;
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;

/// A document whose theme block holds `name`, laid out at the default config
/// directory of a throwaway home.
fn write_config(home: &Path, theme_name: &str) {
    let dir = home.join(".config").join("cfgd");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: pin\nspec:\n  \
             output:\n    theme:\n      name: {theme_name}\n"
        ),
    )
    .unwrap();
}

/// The binary under that home, with every seam resolving the default config
/// directory pointed into it.
fn run(home: &Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("cfgd")
        .unwrap()
        .args(args)
        .env("HOME", home)
        // Windows resolves `~` from USERPROFILE first.
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local").join("share"))
        .env("XDG_STATE_HOME", home.join(".local").join("state"))
        .env("CFGD_CACHE_DIR", home.join("cache"))
        .env("NO_COLOR", "1")
        .output()
        .unwrap()
}

#[test]
fn the_setter_refuses_a_theme_name_no_preset_answers_to() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), "default");

    let out = run(home.path(), &["config", "set", "theme.name", "Bogus"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "refused: {stderr}");
    assert!(
        stderr.contains("`Bogus` is not a theme preset"),
        "the refusal names the word the caller wrote: {stderr}"
    );
    for name in cfgd_core::output::Theme::PRESET_NAMES {
        assert!(
            stderr.contains(name),
            "the refusal names the whole accepted vocabulary, and not {name}: {stderr}"
        );
    }

    let after = std::fs::read_to_string(home.path().join(".config/cfgd/cfgd.yaml")).unwrap();
    assert!(
        after.contains("name: default") && !after.contains("Bogus"),
        "a refused word never reaches the document: {after}"
    );
}

#[test]
fn the_setter_refuses_the_pascal_case_spelling_of_a_real_preset() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), "default");

    // `Theme::preset` matches the lowercase spelling alone, so the setter
    // accepts exactly what resolves rather than folding a case the renderer
    // would then fail to look up.
    let out = run(home.path(), &["config", "set", "theme.name", "Dracula"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("`Dracula` is not a theme preset"));
}

#[test]
fn a_refused_theme_name_carries_its_kind_and_the_accepted_list_on_the_wire() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), "default");

    let out = run(
        home.path(),
        &["-o", "json", "config", "set", "theme.name", "Bogus"],
    );
    assert_eq!(out.status.code(), Some(1));
    let payload: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("the refusal is a JSON document");
    assert_eq!(payload["error"], "invalid_value");
    assert_eq!(payload["name"], "theme.name");
    assert_eq!(payload["value"], "Bogus");
    assert_eq!(
        payload["accepted"],
        serde_json::json!(cfgd_core::output::Theme::PRESET_NAMES),
    );
}

#[test]
fn the_setter_accepts_every_name_the_theme_vocabulary_holds() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), "default");

    for name in cfgd_core::output::Theme::PRESET_NAMES {
        let out = run(home.path(), &["config", "set", "theme.name", name]);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{name} is a preset: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn a_stored_theme_name_no_preset_answers_to_warns_and_renders_the_default_palette() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), "Bogus");

    // A refusal here would lock a reader out of a config they may not own, so
    // the load says what the render will do and carries on.
    let out = run(home.path(), &["config", "get", "theme.name"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        out.status.code(),
        Some(0),
        "the command still runs: {stderr}"
    );
    assert!(
        stderr.contains("`Bogus` is not a theme preset")
            && stderr.contains("rendering the default palette"),
        "the load names the fallback: {stderr}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Bogus"),
        "the stored word is still what `config get` reads back"
    );
}

#[test]
fn the_load_time_warning_stays_off_the_structured_channel() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), "Bogus");

    let out = run(home.path(), &["-o", "json", "config", "get", "theme.name"]);
    assert_eq!(out.status.code(), Some(0));
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .expect("stdout carries the payload alone");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("is not a theme preset"),
        "the advisory is still raised, on stderr"
    );
}

#[test]
fn a_stored_preset_raises_no_warning() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), "nord");

    let out = run(home.path(), &["config", "get", "theme.name"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("theme preset"),
        "a name the palette answers to says nothing"
    );
}

#[test]
fn the_setter_refuses_a_value_of_every_shape_no_preset_answers_to() {
    let home = tempfile::tempdir().unwrap();
    write_config(home.path(), "default");

    // The setter parses its value into a YAML scalar before writing it, so a
    // word judged in the string arm alone lets `123`, `true`, `null` and
    // `3.14` through; a mapping written inline names no preset either.
    for value in ["123", "true", "null", "3.14", "{name: nord}"] {
        let out = run(home.path(), &["config", "set", "theme.name", value]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{value} is no preset: {stderr}");
        assert!(
            stderr.contains(&format!("`{value}` is not a theme preset")),
            "the refusal names the word the caller wrote: {stderr}"
        );
    }

    let after = std::fs::read_to_string(home.path().join(".config/cfgd/cfgd.yaml")).unwrap();
    assert!(
        after.contains("name: default") && !after.contains("123"),
        "no refused value reaches the document: {after}"
    );
}
