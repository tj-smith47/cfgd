//! `spec.output.theme.name` is a plain string in the document, so the preset
//! vocabulary is held at the two points a word enters: the setter that writes
//! one, and the load that reads one somebody else wrote.
//!
//! Both halves need the real binary. The setter's refusal ends in
//! `std::process::exit`, and the load-time warning is raised in `main.rs`
//! where the printer is built, which no in-process call reaches.

use std::path::Path;

mod cfgd_binary;
use cfgd_binary::cfgd_bin;

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

/// A document whose theme block is the SCALAR arm of the union — the shape
/// `cfgd init` and `cfgd config set theme <name>` write.
fn write_scalar_config(home: &Path, theme_name: &str) {
    let dir = home.join(".config").join("cfgd");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: pin\nspec:\n  \
             output:\n    theme: {theme_name}\n"
        ),
    )
    .unwrap();
}

/// The document text at the throwaway home's default config path.
fn stored_config(home: &Path) -> String {
    std::fs::read_to_string(home.join(".config/cfgd/cfgd.yaml")).unwrap()
}

/// The binary under that home, with every seam resolving the default config
/// directory pointed into it.
fn run(home: &Path, args: &[&str]) -> std::process::Output {
    run_at(home, home, args)
}

/// The same, from a chosen working directory, for `cfgd init` — which writes
/// its document where it is run.
fn run_at(home: &Path, cwd: &Path, args: &[&str]) -> std::process::Output {
    cfgd_bin()
        .unwrap()
        .current_dir(cwd)
        .args(args)
        .env("HOME", home)
        // Windows resolves `~` from USERPROFILE first.
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("XDG_DATA_HOME", home.join(".local").join("share"))
        .env("XDG_STATE_HOME", home.join(".local").join("state"))
        .env(cfgd_core::CFGD_CACHE_DIR_ENV, home.join("cache"))
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
            "the refusal names the whole accepted vocabulary, {name} included: {stderr}"
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
    // accepts exactly what resolves. A folded case would be one the renderer
    // then fails to look up.
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

#[test]
fn the_documented_theme_name_setter_runs_against_the_document_init_writes() {
    let home = tempfile::tempdir().unwrap();
    let workspace = home.path().join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let init = run_at(home.path(), &workspace, &["init", "--name", "pin"]);
    assert_eq!(
        init.status.code(),
        Some(0),
        "init: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let written = workspace.join("cfgd.yaml");
    let doc = std::fs::read_to_string(&written).unwrap();
    assert!(
        doc.contains("theme: default"),
        "init writes the union's scalar arm, which is what this pin drives: {doc}"
    );

    let config = written.to_string_lossy().to_string();
    let read = run(
        home.path(),
        &["--config", &config, "config", "get", "theme.name"],
    );
    assert_eq!(
        String::from_utf8_lossy(&read.stdout).trim(),
        "default",
        "the scalar IS the name: {}",
        String::from_utf8_lossy(&read.stderr)
    );

    let set = run(
        home.path(),
        &[
            "--config",
            &config,
            "config",
            "set",
            "theme.name",
            "minimal",
        ],
    );
    assert_eq!(
        set.status.code(),
        Some(0),
        "the documented setter: {}",
        String::from_utf8_lossy(&set.stderr)
    );

    let read = run(
        home.path(),
        &["--config", &config, "config", "get", "theme.name"],
    );
    assert_eq!(
        String::from_utf8_lossy(&read.stdout).trim(),
        "minimal",
        "the write reads back: {}",
        String::from_utf8_lossy(&read.stderr)
    );
}

#[test]
fn setting_the_name_over_the_scalar_arm_invents_no_overrides() {
    let home = tempfile::tempdir().unwrap();
    write_scalar_config(home.path(), "dracula");

    let out = run(home.path(), &["config", "set", "theme.name", "minimal"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = stored_config(home.path());
    assert!(
        after.contains("name: minimal"),
        "the scalar was promoted to the mapping arm and the name written: {after}"
    );
    assert!(
        !after.contains("overrides"),
        "the promotion carries only the field the scalar stood for: {after}"
    );
}

#[test]
fn setting_an_override_over_the_scalar_arm_keeps_the_stored_name() {
    let home = tempfile::tempdir().unwrap();
    write_scalar_config(home.path(), "dracula");

    let out = run(
        home.path(),
        &["config", "set", "theme.overrides.header", "#ff0000"],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let after = stored_config(home.path());
    assert!(
        after.contains("name: dracula"),
        "the scalar the document held is the promoted mapping's name: {after}"
    );
    assert!(
        after.contains("header:"),
        "the override reached the block: {after}"
    );
}

#[test]
fn a_field_the_scalar_arm_does_not_carry_reads_as_a_missing_key() {
    let home = tempfile::tempdir().unwrap();
    write_scalar_config(home.path(), "dracula");

    let out = run(home.path(), &["config", "get", "theme.overrides"]);
    assert_eq!(
        out.status.code(),
        Some(6),
        "absent (no shape error): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not found"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn a_key_under_a_genuine_leaf_is_refused_as_a_missing_key_by_both_verbs() {
    let home = tempfile::tempdir().unwrap();
    write_scalar_config(home.path(), "dracula");
    // `fileStrategy` holds a scalar in the document, so the path names a child
    // of a leaf, which the setter cannot create.
    let dir = home.path().join(".config").join("cfgd");
    let doc = std::fs::read_to_string(dir.join("cfgd.yaml")).unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        format!("{doc}  fileStrategy: Symlink\n"),
    )
    .unwrap();

    for args in [
        vec!["config", "get", "fileStrategy.nope"],
        vec!["config", "set", "fileStrategy.nope", "x"],
        vec!["config", "unset", "fileStrategy.nope"],
    ] {
        let out = run(home.path(), &args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(6),
            "a missing key exits 6 on every verb: {stderr}"
        );
        assert!(
            stderr.contains("key 'fileStrategy.nope' not found"),
            "the refusal names the path it walked: {stderr}"
        );

        let json: Vec<&str> = std::iter::once("-o")
            .chain(std::iter::once("json"))
            .chain(args.iter().copied())
            .collect();
        let out = run(home.path(), &json);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("\"key_not_found\""),
            "a child of a leaf is a missing key: {stdout}"
        );
        assert_eq!(
            out.status.code(),
            Some(6),
            "one kind, one exit code, on both channels: {stdout}"
        );
    }
}

#[test]
fn a_key_named_under_a_declared_list_is_a_missing_key_on_every_verb() {
    // The other half of the shape question: these two documents hold exactly
    // what the schema declares — `spec.sources` and `spec.origin` are both
    // lists — so nothing about them is wrong. The key walkers address no list
    // element, which makes `sources.name` a key that is not there. The
    // document does not contradict its schema.
    for (body, key) in [
        (
            "  sources:\n    - name: team\n      origin:\n        type: Git\n        \
             url: https://example.invalid/cfg.git\n",
            "sources.name",
        ),
        (
            "  origin:\n    - type: Git\n      url: https://example.invalid/cfg.git\n",
            "origin.url",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        write_spec(home.path(), body);

        let shown = run(home.path(), &["config", "show"]);
        assert_eq!(
            shown.status.code(),
            Some(0),
            "the document loads: {}",
            String::from_utf8_lossy(&shown.stderr)
        );

        for verb in ["get", "set", "unset"] {
            let mut args = vec!["config", verb, key];
            if verb == "set" {
                args.push("x");
            }
            let out = run(home.path(), &args);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(
                out.status.code(),
                Some(6),
                "a key under a list exits 6 on every verb: {stderr}"
            );
            assert!(
                stderr.contains(&format!("key '{key}' not found")),
                "the refusal names the path it walked: {stderr}"
            );

            let json: Vec<&str> = std::iter::once("-o")
                .chain(std::iter::once("json"))
                .chain(args.iter().copied())
                .collect();
            let out = run(home.path(), &json);
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                stdout.contains("\"key_not_found\""),
                "a list the schema declares is no shape failure: {stdout}"
            );
            assert_eq!(
                out.status.code(),
                Some(6),
                "one kind, one exit code, on both channels: {stdout}"
            );
        }
    }
}

/// A document whose spec is `body`, laid out at the default config directory
/// of a throwaway home.
fn write_spec(home: &Path, body: &str) {
    let dir = home.join(".config").join("cfgd");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("cfgd.yaml"),
        format!(
            "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: pin\nspec:\n{body}"
        ),
    )
    .unwrap();
}

#[test]
fn a_value_standing_where_a_mapping_belongs_is_a_shape_failure_not_a_missing_key() {
    // Five documents whose shape contradicts the schema, each named by the
    // path it goes wrong at: a sequence where the theme union accepts a scalar
    // or a mapping, a spec that is no mapping at all, a scalar where the
    // schema declares a section, a scalar where it declares a free-form map,
    // and a scalar where it declares a list. None of them is a key a setter
    // could create. A list standing where the schema declares one is the other
    // half of this, and is a missing key — see
    // `a_key_named_under_a_declared_list_is_a_missing_key_on_every_verb`.
    for (body, args, refusal) in [
        (
            "  output:\n    theme:\n      - dracula\n",
            vec!["config", "set", "theme.name", "minimal"],
            "'output.theme' holds a sequence where a mapping belongs",
        ),
        (
            "  - a\n",
            vec!["config", "set", "theme.name", "minimal"],
            "'spec' holds a sequence where a mapping belongs",
        ),
        (
            "  daemon: yes\n",
            vec!["config", "set", "daemon.interval", "5m"],
            "'daemon' holds a scalar where a mapping belongs",
        ),
        (
            "  aliases: yes\n",
            vec!["config", "set", "aliases.ll", "ls -la"],
            "'aliases' holds a scalar where a mapping belongs",
        ),
        (
            "  sources: team\n",
            vec!["config", "set", "sources.name", "team"],
            "'sources' holds a scalar where a mapping belongs",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        write_spec(home.path(), body);
        let before = stored_config(home.path());

        let out = run(home.path(), &args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(refusal),
            "the refusal names the path and the shape it found: {stderr}"
        );
        assert_eq!(out.status.code(), Some(1), "refused: {stderr}");

        let json: Vec<&str> = std::iter::once("-o")
            .chain(std::iter::once("json"))
            .chain(args.iter().copied())
            .collect();
        let out = run(home.path(), &json);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("\"parse_failed\""),
            "a shape the document got wrong is not a key that is merely unset: {stdout}"
        );
        assert_eq!(
            out.status.code(),
            Some(1),
            "the kind a script reads and the exit code agree: {stdout}"
        );
        assert_eq!(
            stored_config(home.path()),
            before,
            "a refused write leaves the document alone"
        );
    }
}

#[test]
fn the_documented_getter_answers_from_a_document_nothing_has_migrated() {
    // `spec.theme` is the spelling before the presentation knobs moved under
    // `spec.output`; both arms of the union still name a theme, and
    // `config get theme.name` is what the docs print for either.
    for body in ["  theme: dracula\n", "  theme:\n    name: dracula\n"] {
        let home = tempfile::tempdir().unwrap();
        write_spec(home.path(), body);

        for key in ["theme.name", "output.theme.name"] {
            let out = run(home.path(), &["config", "get", key]);
            assert_eq!(
                String::from_utf8_lossy(&out.stdout).trim(),
                "dracula",
                "{key} on {body:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
