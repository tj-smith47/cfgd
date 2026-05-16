use super::structured::{
    apply_jsonpath, format_jsonpath_result, name_from_value, split_jsonpath_segment,
};
use super::theme::{ansi256_from_rgb, parse_hex_color};
use super::*;
use crate::config::ThemeConfig;

#[test]
fn printer_respects_quiet_verbosity() {
    let printer = Printer::new(Verbosity::Quiet);
    assert_eq!(printer.verbosity(), Verbosity::Quiet);
    printer.header("test");
    printer.success("test");
    printer.warning("test");
    printer.info("test");
    printer.key_value("key", "value");
}

#[test]
fn printer_error_always_prints() {
    // Errors go to stderr which can't easily be captured in unit tests,
    // so we verify it doesn't panic when called at Quiet verbosity.
    let printer = Printer::new(Verbosity::Quiet);
    printer.error("this is an error");
}

#[test]
fn parse_hex_valid() {
    let color = parse_hex_color("#ff5555");
    assert!(color.is_some());
}

#[test]
fn parse_hex_no_hash() {
    let color = parse_hex_color("50fa7b");
    assert!(color.is_some());
}

#[test]
fn parse_hex_invalid() {
    assert!(parse_hex_color("xyz").is_none());
    assert!(parse_hex_color("#gg0000").is_none());
    assert!(parse_hex_color("#ff").is_none());
}

#[test]
fn named_presets_exist() {
    let default = Theme::from_preset("default");
    let dracula = Theme::from_preset("dracula");
    let solarized_dark = Theme::from_preset("solarized-dark");
    let solarized_light = Theme::from_preset("solarized-light");
    let minimal = Theme::from_preset("minimal");
    // Presets should produce distinct themes
    assert_ne!(default.icon_success, minimal.icon_success);
    assert_ne!(dracula.icon_success, minimal.icon_success);
    assert_ne!(solarized_dark.icon_success, minimal.icon_success);
    assert_ne!(solarized_light.icon_success, minimal.icon_success);
}

#[test]
fn from_config_with_overrides() {
    let config = ThemeConfig {
        name: "default".into(),
        overrides: crate::config::ThemeOverrides {
            icon_ok: Some("OK".into()),
            success: Some("#50fa7b".into()),
            ..Default::default()
        },
    };
    let theme = Theme::from_config(Some(&config));
    assert_eq!(theme.icon_success, "OK");
}

#[test]
fn run_with_output_captures_stdout() {
    let printer = Printer::new(Verbosity::Quiet);
    let output = printer
        .run_with_output(std::process::Command::new("echo").arg("hello"), "test echo")
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.contains("hello"));
}

#[test]
#[cfg(unix)]
fn run_with_output_captures_stderr() {
    let printer = Printer::new(Verbosity::Quiet);
    let output = printer
        .run_with_output(
            std::process::Command::new("sh")
                .arg("-c")
                .arg("echo error >&2"),
            "test stderr",
        )
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.contains("error"));
}

#[test]
#[cfg(unix)]
fn run_with_output_reports_failure() {
    let printer = Printer::new(Verbosity::Quiet);
    let output = printer
        .run_with_output(&mut std::process::Command::new("false"), "test failure")
        .unwrap();
    assert!(!output.status.success());
}

#[test]
#[cfg(unix)]
fn run_with_output_tracks_duration() {
    let printer = Printer::new(Verbosity::Quiet);
    let output = printer
        .run_with_output(&mut std::process::Command::new("true"), "test duration")
        .unwrap();
    assert!(output.duration.as_secs() < 5);
}

#[test]
fn run_with_output_spawn_error() {
    let printer = Printer::new(Verbosity::Quiet);
    let result = printer.run_with_output(
        &mut std::process::Command::new("/nonexistent/binary"),
        "test spawn error",
    );
    match result {
        Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::NotFound),
        Ok(_) => panic!("expected spawn error for nonexistent binary"),
    }
}

// --- OutputFormat and write_structured tests ---

#[test]
fn output_format_table_returns_false() {
    let printer = Printer::new(Verbosity::Normal);
    assert!(!printer.is_structured());
    let val = serde_json::json!({"key": "value"});
    assert!(!printer.write_structured(&val));
}

#[test]
fn output_format_json_returns_true() {
    let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Json);
    assert!(printer.is_structured());
    let val = serde_json::json!({"key": "value"});
    assert!(printer.write_structured(&val));
}

#[test]
fn output_format_yaml_returns_true() {
    let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Yaml);
    assert!(printer.is_structured());
    assert!(printer.write_structured(&"hello"));
}

// --- jsonpath tests ---

#[test]
fn apply_jsonpath_cases() {
    let obj = serde_json::json!({"name": "cfgd", "version": "1.0"});
    let nested = serde_json::json!({"status": {"phase": "running", "ready": true}});
    let arr = serde_json::json!({"items": ["a", "b", "c"]});
    let arr_obj = serde_json::json!({"items": [{"name": "a"}, {"name": "b"}]});
    let arr_num = serde_json::json!({"items": [1, 2, 3, 4, 5]});
    let arr_short = serde_json::json!({"items": [1, 2]});
    let arr3 = serde_json::json!({"items": [1, 2, 3]});
    let null_val = serde_json::json!({"key": null});
    let small = serde_json::json!({"a": 1});

    // Simple scalar lookups
    let cases: &[(&serde_json::Value, &str, &str)] = &[
        (&obj, "{.name}", "cfgd"),
        (&obj, "{.version}", "1.0"),
        (&nested, "{.status.phase}", "running"),
        (&nested, "{.status.ready}", "true"),
        (&arr, "{.items[0]}", "a"),
        (&arr, "{.items[2]}", "c"),
        (&arr_obj, "{.items[*].name}", "a\nb"),
        (&arr_num, "{.items[1:3]}", "2\n3"),
        (&obj, "{.missing}", ""),
        (&obj, ".name", "cfgd"),
        (&arr_short, "{.items[5]}", ""),
        (&arr3, "{.items[10:20]}", ""),
        (&arr3, "{.items[5:2]}", ""),
        (&null_val, "{.key}", ""),
    ];
    for (val, expr, expected) in cases {
        assert_eq!(
            apply_jsonpath(val, expr),
            *expected,
            "failed for expr {expr:?}"
        );
    }

    // Object/full-value results (need JSON parsing)
    let status_json = apply_jsonpath(&nested, "{.status}");
    let parsed: serde_json::Value = serde_json::from_str(&status_json).unwrap();
    assert_eq!(parsed["phase"], "running");

    let full = apply_jsonpath(&small, "{}");
    let parsed: serde_json::Value = serde_json::from_str(&full).unwrap();
    assert_eq!(parsed["a"], 1);
}

#[test]
fn split_jsonpath_segment_simple() {
    assert_eq!(split_jsonpath_segment("foo.bar"), ("foo", "bar"));
    assert_eq!(split_jsonpath_segment("foo"), ("foo", ""));
}

#[test]
fn split_jsonpath_segment_with_bracket() {
    assert_eq!(
        split_jsonpath_segment("items[0].name"),
        ("items[0]", "name")
    );
    assert_eq!(
        split_jsonpath_segment("items[*].name"),
        ("items[*]", "name")
    );
}

// Smoke test: verify all printer methods execute without panic
#[test]
fn printer_methods_smoke_test() {
    let printer = Printer::new(Verbosity::Quiet);
    printer.subheader("Test Section");
    printer.newline();
    printer.stdout_line("output line");

    // Table with data and empty
    let rows = vec![
        vec!["a".to_string(), "b".to_string()],
        vec!["c".to_string(), "d".to_string()],
    ];
    printer.table(&["Col1", "Col2"], &rows);
    let empty_rows: Vec<Vec<String>> = vec![];
    printer.table(&["Col1"], &empty_rows);

    // Plan phase with items and empty
    printer.plan_phase("Packages", &["install brew: curl".to_string()]);
    printer.plan_phase("Files", &[]);

    // Diff with changes and identical
    printer.diff("old content\nline2", "new content\nline2");
    printer.diff("same", "same");

    // Syntax highlighting with known and unknown language
    printer.syntax_highlight("fn main() {}", "rs");
    printer.syntax_highlight("some text", "unknown_lang_xyz");

    // write_structured in non-structured mode
    let data = serde_json::json!({"key": "value"});
    printer.write_structured(&data);
}

// --- write_structured format variants ---

#[test]
fn write_structured_name_single_object() {
    let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Name);
    assert!(printer.is_structured());
    let val = serde_json::json!({"name": "my-profile"});
    assert!(printer.write_structured(&val));
}

#[test]
fn write_structured_name_array() {
    let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Name);
    let val = serde_json::json!([
        {"name": "profile-a"},
        {"name": "profile-b"}
    ]);
    assert!(printer.write_structured(&val));
}

#[test]
fn write_structured_name_fallback_fields() {
    // name_from_value tries "name", then "context", "phase", "resourceType", "url", "applyId"
    let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Name);

    let context_val = serde_json::json!({"context": "production"});
    assert!(printer.write_structured(&context_val));

    let phase_val = serde_json::json!({"phase": "Packages"});
    assert!(printer.write_structured(&phase_val));

    let apply_id_val = serde_json::json!({"applyId": 42});
    assert!(printer.write_structured(&apply_id_val));
}

#[test]
fn write_structured_jsonpath() {
    let printer = Printer::with_format(
        Verbosity::Normal,
        None,
        OutputFormat::Jsonpath("{.status.phase}".to_string()),
    );
    assert!(printer.is_structured());
    let val = serde_json::json!({"status": {"phase": "ready"}});
    assert!(printer.write_structured(&val));
}

#[test]
fn write_structured_template() {
    let printer = Printer::with_format(
        Verbosity::Normal,
        None,
        OutputFormat::Template("Name: {{ name }}".to_string()),
    );
    assert!(printer.is_structured());
    let val = serde_json::json!({"name": "my-config"});
    assert!(printer.write_structured(&val));
}

#[test]
fn write_structured_template_array() {
    let printer = Printer::with_format(
        Verbosity::Normal,
        None,
        OutputFormat::Template("- {{ name }}".to_string()),
    );
    let val = serde_json::json!([
        {"name": "a"},
        {"name": "b"}
    ]);
    assert!(printer.write_structured(&val));
}

#[test]
fn write_structured_template_file() {
    let dir = tempfile::tempdir().unwrap();
    let tmpl_path = dir.path().join("output.tmpl");
    std::fs::write(&tmpl_path, "Name={{ name }} Version={{ version }}").unwrap();

    let printer = Printer::with_format(
        Verbosity::Normal,
        None,
        OutputFormat::TemplateFile(tmpl_path),
    );
    let val = serde_json::json!({"name": "cfgd", "version": "1.0"});
    assert!(printer.write_structured(&val));
}

#[test]
fn write_structured_template_file_missing() {
    let printer = Printer::with_format(
        Verbosity::Normal,
        None,
        OutputFormat::TemplateFile("/nonexistent/template.tmpl".into()),
    );
    let val = serde_json::json!({"key": "value"});
    // Should return true (structured output mode) but print error
    assert!(printer.write_structured(&val));
}

#[test]
fn write_structured_wide_returns_false() {
    let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Wide);
    assert!(!printer.is_structured());
    assert!(printer.is_wide());
    let val = serde_json::json!({"key": "value"});
    assert!(!printer.write_structured(&val));
}

// --- name_from_value edge cases ---

#[test]
fn name_from_value_no_match_returns_none() {
    let val = serde_json::json!({"unknown_field": "value"});
    assert!(name_from_value(&val).is_none());
}

#[test]
fn name_from_value_prefers_name_over_others() {
    let val = serde_json::json!({"name": "primary", "context": "secondary"});
    assert_eq!(name_from_value(&val), Some("primary".to_string()));
}

#[test]
fn name_from_value_numeric_apply_id() {
    let val = serde_json::json!({"applyId": 123});
    assert_eq!(name_from_value(&val), Some("123".to_string()));
}

#[test]
fn name_from_value_null_returns_none() {
    assert!(name_from_value(&serde_json::Value::Null).is_none());
}

// --- Theme construction edge cases ---

#[test]
fn theme_from_config_all_icon_overrides() {
    let config = ThemeConfig {
        name: "default".into(),
        overrides: crate::config::ThemeOverrides {
            icon_ok: Some("OK".into()),
            icon_warn: Some("!!".into()),
            icon_fail: Some("ERR".into()),
            icon_pending: Some("..".into()),
            icon_arrow: Some(">>".into()),
            ..Default::default()
        },
    };
    let theme = Theme::from_config(Some(&config));
    assert_eq!(theme.icon_success, "OK");
    assert_eq!(theme.icon_warning, "!!");
    assert_eq!(theme.icon_error, "ERR");
    assert_eq!(theme.icon_pending, "..");
    assert_eq!(theme.icon_arrow, ">>");
}

#[test]
fn theme_from_config_all_color_overrides() {
    let config = ThemeConfig {
        name: "default".into(),
        overrides: crate::config::ThemeOverrides {
            success: Some("#00ff00".into()),
            warning: Some("#ffff00".into()),
            error: Some("#ff0000".into()),
            info: Some("#00ffff".into()),
            muted: Some("#888888".into()),
            header: Some("#0000ff".into()),
            running: Some("#aabbcc".into()),
            diff_add: Some("#50fa7b".into()),
            diff_remove: Some("#ff5555".into()),
            diff_context: Some("#6272a4".into()),
            ..Default::default()
        },
    };
    // Should not panic — all colors are applied
    let _theme = Theme::from_config(Some(&config));
}

#[test]
fn theme_from_config_invalid_color_ignored() {
    let config = ThemeConfig {
        name: "default".into(),
        overrides: crate::config::ThemeOverrides {
            success: Some("not-a-color".into()),
            ..Default::default()
        },
    };
    // Should not panic — invalid color is silently ignored
    let _theme = Theme::from_config(Some(&config));
}

// --- format_jsonpath_result ---

#[test]
fn format_jsonpath_result_bool() {
    assert_eq!(
        format_jsonpath_result(&serde_json::Value::Bool(true)),
        "true"
    );
    assert_eq!(
        format_jsonpath_result(&serde_json::Value::Bool(false)),
        "false"
    );
}

#[test]
fn format_jsonpath_result_number() {
    let num = serde_json::json!(42);
    assert_eq!(format_jsonpath_result(&num), "42");
    let float = serde_json::json!(1.234);
    assert_eq!(format_jsonpath_result(&float), "1.234");
}

#[test]
fn format_jsonpath_result_null_empty() {
    assert_eq!(format_jsonpath_result(&serde_json::Value::Null), "");
}

// --- Printer method behavior ---

#[test]
fn printer_normal_verbosity_methods_do_not_panic() {
    // Unlike Quiet mode (which skips), Normal mode exercises the full rendering path
    let printer = Printer::new(Verbosity::Normal);

    printer.header("Test Header");
    printer.subheader("Test Subheader");
    printer.success("Operation succeeded");
    printer.warning("Something might be wrong");
    printer.error("Something failed");
    printer.info("Some information");
    printer.key_value("Profile", "developer");
    printer.key_value("Config Dir", "~/.config/cfgd");
    printer.newline();

    // Table with real data
    printer.table(
        &["NAME", "STATUS", "AGE"],
        &[
            vec!["nvim".into(), "installed".into(), "2d".into()],
            vec!["tmux".into(), "missing".into(), "-".into()],
        ],
    );

    // Plan phase
    printer.plan_phase(
        "Packages",
        &[
            "install brew: neovim".to_string(),
            "install apt: ripgrep".to_string(),
        ],
    );
    printer.plan_phase("Files", &[]);

    // Diff
    printer.diff("line1\nline2\nline3", "line1\nmodified\nline3");
    printer.diff("identical", "identical");

    // Syntax highlight
    printer.syntax_highlight("fn main() { println!(\"hello\"); }", "rs");
}

#[test]
fn spinner_hidden_in_quiet_mode() {
    let printer = Printer::new(Verbosity::Quiet);
    let spinner = printer.spinner("loading...");
    // ProgressBar::hidden() is returned in quiet mode — verify it doesn't panic
    spinner.finish_and_clear();
}

#[test]
fn progress_bar_creates_valid_bar() {
    let printer = Printer::new(Verbosity::Normal);
    let pb = printer.progress_bar(100, "processing");
    pb.inc(50);
    assert_eq!(pb.position(), 50);
    pb.finish();
}

#[test]
fn disable_colors_does_not_panic() {
    Printer::disable_colors();
}

// --- jsonpath walk edge cases ---

#[test]
fn jsonpath_non_array_bracket_access_returns_empty() {
    let val = serde_json::json!({"items": "not-an-array"});
    assert_eq!(apply_jsonpath(&val, "{.items[0]}"), "");
}

#[test]
fn jsonpath_non_numeric_bracket_returns_empty() {
    let val = serde_json::json!({"items": [1, 2, 3]});
    assert_eq!(apply_jsonpath(&val, "{.items[abc]}"), "");
}

#[test]
fn jsonpath_nested_array_wildcard() {
    let val = serde_json::json!({
        "groups": [
            {"members": [{"name": "alice"}, {"name": "bob"}]},
            {"members": [{"name": "charlie"}]}
        ]
    });
    let result = apply_jsonpath(&val, "{.groups[*].members[*].name}");
    assert!(result.contains("alice"), "should contain alice: {result}");
    assert!(result.contains("bob"), "should contain bob: {result}");
    assert!(
        result.contains("charlie"),
        "should contain charlie: {result}"
    );
}

#[test]
fn jsonpath_slice_from_start() {
    let val = serde_json::json!({"items": ["a", "b", "c", "d"]});
    // [0:2] should return first two
    assert_eq!(apply_jsonpath(&val, "{.items[0:2]}"), "a\nb");
}

#[test]
fn jsonpath_empty_array() {
    let val = serde_json::json!({"items": []});
    assert_eq!(apply_jsonpath(&val, "{.items[0]}"), "");
    assert_eq!(apply_jsonpath(&val, "{.items[*]}"), "");
    assert_eq!(apply_jsonpath(&val, "{.items[0:5]}"), "");
}

// --- Printer::for_test capture behavior ---

#[test]
fn for_test_captures_header_text() {
    let (printer, buf) = Printer::for_test();
    printer.header("Test Header");
    let output = buf.lock().unwrap();
    assert!(output.contains("Test Header"), "should capture header text");
}

#[test]
fn for_test_captures_success_text() {
    let (printer, buf) = Printer::for_test();
    printer.success("Operation passed");
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Operation passed"),
        "should capture success text"
    );
}

#[test]
fn for_test_captures_warning_text() {
    let (printer, buf) = Printer::for_test();
    printer.warning("Careful now");
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Careful now"),
        "should capture warning text"
    );
}

#[test]
fn for_test_captures_error_text() {
    let (printer, buf) = Printer::for_test();
    printer.error("Something broke");
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Something broke"),
        "should capture error text"
    );
}

#[test]
fn for_test_captures_info_text() {
    let (printer, buf) = Printer::for_test();
    printer.info("FYI message");
    let output = buf.lock().unwrap();
    assert!(output.contains("FYI message"), "should capture info text");
}

#[test]
fn for_test_captures_key_value() {
    let (printer, buf) = Printer::for_test();
    printer.key_value("Profile", "developer");
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Profile") && output.contains("developer"),
        "should capture key and value"
    );
}

#[test]
fn for_test_captures_subheader() {
    let (printer, buf) = Printer::for_test();
    printer.subheader("Subsection");
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Subsection"),
        "should capture subheader text"
    );
}

#[test]
fn for_test_captures_plan_phase() {
    let (printer, buf) = Printer::for_test();
    printer.plan_phase("Install", &["neovim".to_string(), "tmux".to_string()]);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Phase: Install"),
        "should capture phase name"
    );
    assert!(
        output.contains("neovim") && output.contains("tmux"),
        "should capture phase items"
    );
}

#[test]
fn for_test_captures_plan_phase_empty() {
    let (printer, buf) = Printer::for_test();
    printer.plan_phase("Cleanup", &[]);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Phase: Cleanup"),
        "should capture empty phase name"
    );
}

#[test]
fn for_test_captures_table() {
    let (printer, buf) = Printer::for_test();
    let rows = vec![
        vec!["nvim".to_string(), "installed".to_string()],
        vec!["tmux".to_string(), "missing".to_string()],
    ];
    printer.table(&["NAME", "STATUS"], &rows);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("NAME") && output.contains("STATUS"),
        "should capture table headers"
    );
    assert!(
        output.contains("nvim") && output.contains("installed"),
        "should capture table rows"
    );
    assert!(
        output.contains("tmux") && output.contains("missing"),
        "should capture second row"
    );
}

#[test]
fn for_test_captures_diff() {
    let (printer, buf) = Printer::for_test();
    printer.diff("line1\nold\nline3\n", "line1\nnew\nline3\n");
    let output = buf.lock().unwrap();
    assert!(output.contains("-old"), "should capture removed line");
    assert!(output.contains("+new"), "should capture added line");
    assert!(
        output.contains(" line1"),
        "should capture unchanged context"
    );
}

#[test]
fn for_test_captures_diff_identical() {
    let (printer, buf) = Printer::for_test();
    printer.diff("same\n", "same\n");
    let output = buf.lock().unwrap();
    assert!(
        output.contains(" same"),
        "identical diff should show context line"
    );
    assert!(
        !output.contains("-same") && !output.contains("+same"),
        "identical diff should have no add/remove markers"
    );
}

#[test]
fn for_test_captures_stdout_line() {
    let (printer, buf) = Printer::for_test();
    printer.stdout_line("data output");
    let output = buf.lock().unwrap();
    assert!(
        output.contains("data output"),
        "should capture stdout_line text"
    );
}

// --- write_structured with for_test ---

#[test]
fn for_test_write_structured_json() {
    let (printer, buf) = Printer::for_test_with_format(OutputFormat::Json);
    let val = serde_json::json!({"key": "value"});
    let wrote = printer.write_structured(&val);
    assert!(wrote, "should return true for JSON format");
    let output = buf.lock().unwrap();
    assert!(
        output.contains("key") && output.contains("value"),
        "should capture JSON output"
    );
}

#[test]
fn for_test_write_structured_yaml() {
    let (printer, buf) = Printer::for_test_with_format(OutputFormat::Yaml);
    let val = serde_json::json!({"name": "test"});
    let wrote = printer.write_structured(&val);
    assert!(wrote, "should return true for YAML format");
    let output = buf.lock().unwrap();
    assert!(
        output.contains("name") && output.contains("test"),
        "should capture YAML output"
    );
}

#[test]
fn for_test_write_structured_name() {
    let (printer, buf) = Printer::for_test_with_format(OutputFormat::Name);
    let val = serde_json::json!({"name": "my-profile"});
    let wrote = printer.write_structured(&val);
    assert!(wrote, "should return true for Name format");
    let output = buf.lock().unwrap();
    assert!(output.contains("my-profile"), "should capture name output");
}

#[test]
fn for_test_write_structured_name_array_items() {
    let (printer, buf) = Printer::for_test_with_format(OutputFormat::Name);
    let val = serde_json::json!([
        {"name": "alpha"},
        {"name": "beta"}
    ]);
    let wrote = printer.write_structured(&val);
    assert!(wrote);
    let output = buf.lock().unwrap();
    assert!(output.contains("alpha"), "should capture first name");
    assert!(output.contains("beta"), "should capture second name");
}

#[test]
fn for_test_write_structured_jsonpath_captures() {
    let (printer, buf) =
        Printer::for_test_with_format(OutputFormat::Jsonpath("{.name}".to_string()));
    let val = serde_json::json!({"name": "cfgd", "version": "2.0"});
    let wrote = printer.write_structured(&val);
    assert!(wrote);
    let output = buf.lock().unwrap();
    assert!(output.contains("cfgd"), "should capture jsonpath result");
}

#[test]
fn for_test_write_structured_template_captures() {
    let (printer, buf) =
        Printer::for_test_with_format(OutputFormat::Template("Hello {{ name }}!".to_string()));
    let val = serde_json::json!({"name": "world"});
    let wrote = printer.write_structured(&val);
    assert!(wrote);
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Hello world!"),
        "should capture rendered template"
    );
}

// --- ansi256_from_rgb edge cases ---

#[test]
fn ansi256_from_rgb_pure_black() {
    // r==g==b==0, r < 8 -> should return 16
    assert_eq!(ansi256_from_rgb(0, 0, 0), 16);
}

#[test]
fn ansi256_from_rgb_near_white() {
    // r==g==b==255, r > 248 -> should return 231
    assert_eq!(ansi256_from_rgb(255, 255, 255), 231);
}

#[test]
fn ansi256_from_rgb_grayscale_midrange() {
    // r==g==b, 8 <= r <= 248 -> grayscale ramp 232-255
    let idx = ansi256_from_rgb(128, 128, 128);
    assert!(
        idx >= 232,
        "midrange gray should be in grayscale ramp: {idx}"
    );
}

#[test]
fn ansi256_from_rgb_color_cube() {
    // r!=g or g!=b -> 6x6x6 color cube (16-231)
    let idx = ansi256_from_rgb(255, 0, 0); // pure red
    assert!(
        (16..=231).contains(&idx),
        "pure red should map to color cube: {idx}"
    );
}

#[test]
fn ansi256_from_rgb_various_colors() {
    // Verify no panics and range validity for several colors
    let colors: &[(u8, u8, u8)] = &[
        (0, 255, 0),    // green
        (0, 0, 255),    // blue
        (128, 64, 0),   // brown
        (200, 200, 50), // yellow-ish (not all equal -> cube)
    ];
    for (r, g, b) in colors {
        let idx = ansi256_from_rgb(*r, *g, *b);
        assert!(
            (16..=231).contains(&idx),
            "color ({r},{g},{b}) should map to cube or grayscale: {idx}"
        );
    }
}

// --- Template rendering error paths ---

#[test]
fn write_structured_invalid_template_syntax() {
    let (printer, buf) =
        Printer::for_test_with_format(OutputFormat::Template("{{ invalid {% endfor }".to_string()));
    let val = serde_json::json!({"name": "test"});
    let wrote = printer.write_structured(&val);
    assert!(wrote, "should still return true");
    let output = buf.lock().unwrap();
    assert!(
        output.contains("invalid template"),
        "should capture template error message, got: {output}"
    );
}

// --- OutputFormat variant coverage ---

#[test]
fn output_format_auto_quiets_structured() {
    // When structured output is active, verbosity should be set to Quiet
    let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Json);
    assert_eq!(printer.verbosity(), Verbosity::Quiet);
}

#[test]
fn output_format_table_preserves_verbosity() {
    let printer = Printer::with_format(Verbosity::Normal, None, OutputFormat::Table);
    assert_eq!(printer.verbosity(), Verbosity::Normal);
}

#[test]
fn output_format_wide_preserves_verbosity() {
    let printer = Printer::with_format(Verbosity::Verbose, None, OutputFormat::Wide);
    assert_eq!(printer.verbosity(), Verbosity::Verbose);
}

// --- name_from_value additional fields ---

#[test]
fn name_from_value_url_field() {
    let val = serde_json::json!({"url": "https://example.com"});
    assert_eq!(
        name_from_value(&val),
        Some("https://example.com".to_string())
    );
}

#[test]
fn name_from_value_resource_type_field() {
    let val = serde_json::json!({"resourceType": "MachineConfig"});
    assert_eq!(name_from_value(&val), Some("MachineConfig".to_string()));
}

// ---------------------------------------------------------------------------
// prompt-response mock (Printer::for_test_with_prompt_responses)
//
// 35+ production call-sites of prompt_confirm currently fall back to
// `unwrap_or(false)` in tests because Printer::for_test() returns a printer
// whose prompt_* methods would block on inquire. The queue lets tests drive
// the "user said yes" / "user typed X" branches of those flows.
// ---------------------------------------------------------------------------

#[test]
fn prompt_confirm_consumes_canned_true() {
    let (printer, _buf) =
        Printer::for_test_with_prompt_responses(vec![PromptAnswer::Confirm(true)]);
    assert!(printer.prompt_confirm("first").unwrap());
}

#[test]
fn prompt_confirm_consumes_canned_false() {
    let (printer, _buf) =
        Printer::for_test_with_prompt_responses(vec![PromptAnswer::Confirm(false)]);
    assert!(!printer.prompt_confirm("confirm?").unwrap());
}

#[test]
fn prompt_select_consumes_canned_choice_matching_options() {
    let (printer, _buf) =
        Printer::for_test_with_prompt_responses(vec![PromptAnswer::Select("beta".to_string())]);
    let options = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
    let chosen = printer.prompt_select("pick one", &options).unwrap();
    assert_eq!(chosen, "beta");
}

#[test]
fn prompt_select_with_option_not_in_list_returns_err() {
    // Caller queued "ghost" but it isn't among the options the production
    // code supplied — return a Custom Err that explains the mismatch
    // rather than silently mapping to the first option.
    let (printer, _buf) =
        Printer::for_test_with_prompt_responses(vec![PromptAnswer::Select("ghost".to_string())]);
    let options = vec!["alpha".to_string(), "beta".to_string()];
    let result = printer.prompt_select("pick", &options);
    assert!(result.is_err(), "ghost option should not match");
    let msg = format!("{:?}", result.unwrap_err());
    assert!(
        msg.contains("not in option list"),
        "error should explain mismatch: {msg}"
    );
}

#[test]
fn prompt_text_consumes_canned_string() {
    let (printer, _buf) = Printer::for_test_with_prompt_responses(vec![PromptAnswer::Text(
        "hello world".to_string(),
    )]);
    assert_eq!(
        printer.prompt_text("name?", "default").unwrap(),
        "hello world"
    );
}

#[test]
fn prompt_queue_drains_in_order_across_mixed_calls() {
    let (printer, _buf) = Printer::for_test_with_prompt_responses(vec![
        PromptAnswer::Confirm(true),
        PromptAnswer::Text("typed".to_string()),
        PromptAnswer::Confirm(false),
    ]);
    assert!(printer.prompt_confirm("a").unwrap());
    assert_eq!(printer.prompt_text("b", "").unwrap(), "typed");
    assert!(!printer.prompt_confirm("c").unwrap());
}

#[test]
fn for_test_with_prompt_responses_captures_output_to_shared_buffer() {
    // The new constructor also wires the test_buf, so `info`/`warning`/`success`
    // calls during the prompted flow remain inspectable by tests.
    let (printer, buf) = Printer::for_test_with_prompt_responses(vec![PromptAnswer::Confirm(true)]);
    printer.info("inspectable");
    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("inspectable"),
        "test_buf must capture: {captured}"
    );
}
