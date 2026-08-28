use super::*;
use cfgd_core::output::{Printer, Verbosity};

// --- explain tests ---

#[test]
fn explain_covers_every_kind_incl_clusterpolicy_and_module_crd() {
    for k in [
        "Module",
        "Profile",
        "ConfigSource",
        "Config",
        "TeamConfig",
        "MachineConfig",
        "ConfigPolicy",
        "ClusterConfigPolicy",
        "DriftAlert",
    ] {
        assert!(find_schema(k).is_some(), "explain missing {k}");
    }
    // CRD Module variant disambiguated
    assert!(find_schema("Module").is_some());
}

#[test]
fn explain_module_fields_match_schemars() {
    let live = find_schema("Module").unwrap().field_tree();
    let from_schema = cfgd_core::schema::KIND_REGISTRY
        .iter()
        .find(|e| e.kind == "Module" && !e.crd)
        .unwrap()
        .field_tree();
    assert_eq!(live.len(), from_schema.len());
}

#[test]
fn explain_find_schema_by_kind() {
    assert!(find_schema("Module").is_some());
    assert!(find_schema("Profile").is_some());
    assert!(find_schema("Config").is_some());
    assert!(find_schema("MachineConfig").is_some());
    assert!(find_schema("ConfigPolicy").is_some());
    assert!(find_schema("DriftAlert").is_some());
    assert!(find_schema("TeamConfig").is_some());
    assert!(find_schema("ConfigSource").is_some());
}

#[test]
fn explain_find_schema_case_insensitive() {
    assert!(find_schema("module").is_some());
    assert!(find_schema("PROFILE").is_some());
    assert!(find_schema("cfgdconfig").is_some());
    assert!(find_schema("configsource").is_some());
    assert!(find_schema("cfgd-source").is_some());
}

#[test]
fn explain_find_schema_unknown_returns_none() {
    assert!(find_schema("nonexistent").is_none());
    assert!(find_schema("").is_none());
}

#[test]
fn explain_resolve_field_path_top_level() {
    let module = find_schema("Module").unwrap();
    let fields = resolve_field_path(&module.fields, &[]);
    assert!(fields.is_some());
    let fields = fields.unwrap();
    // Module has depends, packages, files, scripts
    assert!(fields.len() >= 3);
}

#[test]
fn explain_resolve_field_path_nested() {
    let module = find_schema("Module").unwrap();
    let fields = resolve_field_path(&module.fields, &["packages"]);
    assert!(fields.is_some());
    let children = fields.unwrap();
    // Module packages entries have name, minVersion, prefer, aliases, script, platforms
    assert!(children.len() >= 4);
}

#[test]
fn explain_resolve_field_path_deep() {
    let profile = find_schema("Profile").unwrap();
    // `brew` is a two-shape union (a bare list, or a `BrewSpec` map), so the
    // path answers with the field itself and its shapes live in `variants`.
    let fields = resolve_field_path(&profile.fields, &["packages", "brew"]).unwrap();
    assert_eq!(fields.len(), 1);
    let brew = &fields[0];
    assert!(brew.children.is_empty());
    let object = brew
        .variants
        .iter()
        .find(|v| v.name == "BrewSpec")
        .expect("the map shape is a variant, named by its $defs type");
    // Brew has file, taps, formulae, casks
    assert_eq!(object.children.len(), 4);
    let list = brew.variants.iter().find(|v| v.name == "[]string");
    assert!(list.is_some(), "the bare-list shape is a variant too");
}

#[test]
fn explain_resolve_field_path_leaf() {
    let profile = find_schema("Profile").unwrap();
    let fields = resolve_field_path(&profile.fields, &["packages", "brew", "taps"]);
    assert!(fields.is_some());
    let children = fields.unwrap();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].name, "taps");
}

#[test]
fn explain_resolve_field_path_unknown() {
    let module = find_schema("Module").unwrap();
    let fields = resolve_field_path(&module.fields, &["nonexistent"]);
    assert!(fields.is_none());
}

#[test]
fn explain_all_schemas_have_fields() {
    for schema in all_schemas() {
        assert!(
            !schema.fields.is_empty(),
            "Schema {} has no fields",
            schema.name
        );
        assert!(!schema.name.is_empty());
        assert!(!schema.api_version.is_empty());
        assert!(!schema.kind.is_empty());
        assert!(!schema.location.is_empty());
        assert!(!schema.description.is_empty());
    }
}

#[test]
fn explain_cmd_no_args_lists_types() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, None, false).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        output.contains("Available resource types"),
        "expected header listing resource types, got: {output}"
    );
    assert!(
        output.contains("Module"),
        "expected Module in resource list, got: {output}"
    );
    assert!(
        output.contains("Profile"),
        "expected Profile in resource list, got: {output}"
    );
    assert!(
        output.contains("Config"),
        "expected Config in resource list, got: {output}"
    );
}

#[test]
fn explain_cmd_known_resource() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("module"), false).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        output.contains("Module"),
        "expected Module name in output, got: {output}"
    );
    assert!(
        output.contains("packages"),
        "expected packages field in module output, got: {output}"
    );
    assert!(
        output.contains("Fields (under spec)"),
        "expected Fields section header, got: {output}"
    );
}

#[test]
fn explain_cmd_field_path() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("module.packages"), false).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        output.contains("module.spec.packages"),
        "expected field path header, got: {output}"
    );
    // packages[] entries drill into their object fields (name, minVersion, …).
    assert!(
        output.contains("name") && output.contains("minVersion"),
        "expected package-entry children in output, got: {output}"
    );
}

#[test]
fn explain_cmd_field_path_single_object_auto_expands_without_recursive() {
    // `config.security` resolves to exactly one object (`SecurityConfig`, one
    // field). Drilling in without --recursive must show the object's OWN
    // description alongside its one field, not silently replace it with the
    // field's description as if the object itself were never named.
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("Config.security"), false).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        output.contains("Security settings for source signature verification."),
        "expected the queried object's own description, got: {output}"
    );
    assert!(
        output.contains("allowUnsigned"),
        "expected the object's field auto-expanded without --recursive, got: {output}"
    );
}

#[test]
fn explain_cmd_field_path_multi_child_object_shows_own_header_and_tree_stays_collapsed() {
    // `config.modules` resolves to exactly one object with two children
    // (`registries`, `security`). Its own description must render, its two
    // children must auto-expand one level without --recursive, and a
    // GRANDCHILD field (`registries[].name`) must stay behind the `[+]`
    // marker: --recursive still governs the deeper tree, auto-expand is one
    // level only.
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("Config.modules"), false).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        output.contains("Module configuration: registries and security."),
        "expected the queried object's own description, got: {output}"
    );
    assert!(
        output.contains("registries") && output.contains("security"),
        "expected both children auto-expanded one level, got: {output}"
    );
    assert!(
        output.contains("[+]"),
        "expected registries' own fields to stay collapsed without --recursive, got: {output}"
    );
    assert!(
        !output.contains("requireSignatures"),
        "grandchild field must not appear without --recursive, got: {output}"
    );

    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("Config.modules"), true).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        !output.contains("[+]"),
        "recursive expansion must leave no unexpanded markers, got: {output}"
    );
    assert!(
        output.contains("\n    requireSignatures  <boolean>"),
        "expected grandchild field expanded under --recursive, got: {output}"
    );
}

/// A field list that MARKS expandable fields explains the mark exactly once,
/// and a list carrying no mark says nothing about it.
///
/// The legend was missing entirely: `[+]` appeared beside a field with no
/// statement anywhere of what it meant or how to act on it, so the reader had
/// to guess that a second `cfgd explain` call was on offer.
#[test]
fn every_field_list_carrying_the_mark_explains_it_once() {
    let render = |resource: &str, recursive: bool| {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        cmd_explain(&printer, Some(resource), recursive).unwrap();
        printer.flush();
        cfgd_core::test_helpers::captured_text(&buf)
    };
    let legend = "expands a field marked [+]";

    // Every schema's own field list, and the drill-down beneath it: whichever
    // of the two carries a mark carries the legend, and never twice.
    for schema in super::all_schemas() {
        for resource in [schema.name.clone(), format!("{}.spec", schema.name)] {
            let out = render(&resource, false);
            let marks = out.matches("[+]").count();
            let legends = out.matches(legend).count();
            if marks > usize::from(legends > 0) {
                assert_eq!(
                    legends, 1,
                    "`cfgd explain {resource}` marks a field and never says what the mark means:\n{out}"
                );
            } else {
                assert_eq!(
                    legends, 0,
                    "`cfgd explain {resource}` explains a mark it never printed:\n{out}"
                );
            }
        }
    }

    // `--recursive` has already expanded everything, so it mints no mark and
    // earns no legend.
    let recursive = render("Config", true);
    assert!(
        !recursive.contains(legend),
        "an expanded tree explains nothing about expanding: {recursive}"
    );
}

#[test]
fn explain_cmd_field_descriptions_render_documented_ai_fields() {
    // Non-recursive on purpose: descriptions belong to the field-list view.
    // `--recursive` renders the structure alone.
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("Config.ai"), false).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        output.contains("The AI provider name. Default: `claude`."),
        "expected AiConfig.provider rustdoc in output, got: {output}"
    );
    assert!(
        output.contains("Name of the environment variable holding the API key."),
        "expected AiConfig.api_key_env rustdoc in output, got: {output}"
    );
    // No leaked rustdoc intra-doc link markdown anywhere in a field description:
    // schemars copies `///` text verbatim and cfgd explain prints it with no
    // markdown renderer, so a `[`Type`]` link would show as literal brackets.
    assert!(
        !output.contains("[`"),
        "field description leaked rustdoc link syntax, got: {output}"
    );
}

#[test]
fn explain_cmd_spec_prefix_stripped() {
    // "module.spec.packages" should produce identical output to "module.packages"
    let (printer_a, buf_a) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer_a, Some("module.packages"), false).unwrap();
    printer_a.flush();
    let output_a = cfgd_core::test_helpers::captured_text(&buf_a);

    let (printer_b, buf_b) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer_b, Some("module.spec.packages"), false).unwrap();
    printer_b.flush();
    let output_b = cfgd_core::test_helpers::captured_text(&buf_b);

    assert_eq!(
        output_a, output_b,
        "spec prefix should be stripped transparently"
    );
    assert!(
        output_a.contains("module.spec.packages"),
        "expected field path header, got: {output_a}"
    );
}

#[test]
fn explain_cmd_recursive() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("profile"), true).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        output.contains("Profile"),
        "expected Profile resource name, got: {output}"
    );
    // Recursive output should expand nested children (no [+] markers)
    assert!(
        !output.contains("[+]"),
        "recursive output should not have unexpanded [+] markers, got: {output}"
    );
    // Profile has nested fields like packages.brew etc. that should be expanded
    assert!(
        output.contains("inherits"),
        "expected inherits field in profile output, got: {output}"
    );
}

#[test]
fn explain_cmd_unknown_resource() {
    let (printer, _buf) = Printer::for_test_at(Verbosity::Normal);
    let err = cmd_explain(&printer, Some("nonexistent"), false).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Unknown resource type") && msg.contains("nonexistent"),
        "expected unknown resource error mentioning 'nonexistent', got: {msg}"
    );
}

#[test]
fn explain_cmd_unknown_field_path() {
    let (printer, _buf) = Printer::for_test_at(Verbosity::Normal);
    let err = cmd_explain(&printer, Some("module.nonexistent"), false).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("Unknown field path") && msg.contains("nonexistent"),
        "expected unknown field path error mentioning 'nonexistent', got: {msg}"
    );
}

#[test]
fn explain_theme_overrides_complete() {
    // ThemeOverrides has 22 fields (14 styles + 8 icons) — verify schema matches
    let config = find_schema("Config").unwrap();
    let fields = resolve_field_path(&config.fields, &["theme", "overrides"]);
    let children = fields.unwrap();
    assert_eq!(
        children.len(),
        22,
        "ThemeOverrides schema should have 22 fields, got {}",
        children.len()
    );
}

#[test]
fn explain_source_alias() {
    assert!(find_schema("source").is_some());
    assert!(find_schema("cfgd-source").is_some());
    assert_eq!(find_schema("source").unwrap().kind, "ConfigSource");
}

#[test]
fn explain_sources_origin_has_children() {
    // sources[].origin should have drillable children
    let config = find_schema("Config").unwrap();
    let fields = resolve_field_path(&config.fields, &["sources", "origin"]);
    let children = fields.unwrap();
    assert!(
        children.len() >= 3,
        "sources.origin should have type/url/branch/auth children"
    );
}

// --- Task 18: array-of-object + oneOf (untagged-enum) expansion ---

#[test]
fn explain_resolve_field_path_array_of_object_lists_element_fields() {
    // `profile.backups[]` is a `Vec<BackupSpec>` (a plain-object array
    // element); the field path resolves straight to BackupSpec's own
    // properties, same as `explain_cmd_field_path`'s `module.packages`.
    let profile = find_schema("Profile").unwrap();
    let fields = resolve_field_path(&profile.fields, &["backups"]).expect("backups resolves");
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    assert!(
        names.contains(&"name"),
        "expected BackupSpec's name field, got {names:?}"
    );
    assert!(
        names.contains(&"schedule"),
        "expected BackupSpec's schedule field, got {names:?}"
    );
    // Drilldown continues through the element's own (leaf) fields.
    let name_field = fields.iter().find(|f| f.name == "name").unwrap();
    assert!(name_field.children.is_empty());
    assert!(name_field.variants.is_empty());
}

#[test]
fn explain_resolve_field_path_oneof_field_carries_both_variants() {
    // `profile.scripts.preApply` is a `Vec<ScriptEntry>` — each entry is
    // either a bare string or a `{ run, timeout, … }` object. The field's
    // own `children` stay empty (it is not itself an object), so
    // `resolve_field_path` returns it as a single-element slice; the two
    // accepted shapes live in `variants` instead.
    let profile = find_schema("Profile").unwrap();
    let fields = resolve_field_path(&profile.fields, &["scripts", "preApply"])
        .expect("scripts.preApply resolves");
    assert_eq!(fields.len(), 1);
    let f = &fields[0];
    assert!(f.children.is_empty());
    let variant_types: Vec<&str> = f.variants.iter().map(|v| v.type_desc.as_str()).collect();
    assert_eq!(variant_types, vec!["string", "object"]);
    let object_variant = f
        .variants
        .iter()
        .find(|v| v.type_desc == "object")
        .expect("object variant present");
    assert!(
        object_variant.children.iter().any(|c| c.name == "run"),
        "expected the object variant's `run` field, got {:?}",
        object_variant.children
    );
}

#[test]
fn explain_cmd_field_path_oneof_shows_both_variants() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("profile.scripts.preApply"), false).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        output.contains("Variants"),
        "expected a Variants section, got: {output}"
    );
    assert!(
        output.contains("string"),
        "expected the string variant, got: {output}"
    );
    assert!(
        output.contains("ScriptCommand"),
        "expected the object variant by name, got: {output}"
    );
    // Non-recursive: the one object shape's own fields (`run`, …) are the
    // page's `Fields`, listed by name where a plain object would list its
    // own; the shape row itself is never marked, its name being no path.
    assert!(
        output.contains("Fields") && output.lines().any(|l| l.trim_start().starts_with("run ")),
        "expected the object shape's fields listed by name, got: {output}"
    );
    assert!(
        !output
            .lines()
            .any(|l| l.trim_start().starts_with("object") && l.contains("[+]")),
        "a shape row never carries the [+] a field earns, got: {output}"
    );
}

#[test]
fn explain_cmd_field_path_oneof_recursive_expands_object_variant() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("profile.scripts.preApply"), true).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        !output.contains("[+]"),
        "recursive drilldown should have no unexpanded markers, got: {output}"
    );
    assert!(
        output.contains("continueOnError"),
        "expected the object variant's own fields expanded, got: {output}"
    );
}

#[test]
fn explain_field_path_oneof_json_shape_is_additive() {
    let (printer, cap) = Printer::for_test_doc();
    cmd_explain(&printer, Some("profile.scripts.preApply"), false).unwrap();
    drop(printer);
    let json = cap.json().expect("drilldown doc carries with_data");
    let fields = json["fields"].as_array().expect("fields array");
    assert_eq!(fields.len(), 1);
    let field = &fields[0];
    assert_eq!(field["name"], "preApply");
    let variants = field["variants"].as_array().expect("variants array");
    assert_eq!(variants.len(), 2);
    let variant_types: Vec<&str> = variants
        .iter()
        .map(|v| v["type"].as_str().unwrap())
        .collect();
    assert_eq!(variant_types, vec!["string", "object"]);
    let object_variant = variants
        .iter()
        .find(|v| v["type"] == "object")
        .expect("object variant present");
    let children = object_variant["children"]
        .as_array()
        .expect("object variant carries children");
    assert!(
        children.iter().any(|c| c["name"] == "run"),
        "expected the object variant's `run` child in json, got: {object_variant:?}"
    );
    // The string variant has nothing to drill into — `variants` and
    // `children` both stay absent (skip_serializing_if empty), the additive
    // contract: an existing consumer reading only `children` sees no change.
    let string_variant = variants.iter().find(|v| v["type"] == "string").unwrap();
    assert!(string_variant.get("children").is_none());
    assert!(string_variant.get("variants").is_none());
}

// --- Fix round 1: resolve_field_path must continue PAST a variant boundary ---

#[test]
fn explain_resolve_field_path_traverses_past_a_variant_into_its_object_shape() {
    // `scripts.preApply` has no children of its own (it's a union field); the
    // path must keep walking into the object variant's own children to find
    // `run`, without the caller ever naming the variant ("object") in the
    // path.
    let profile = find_schema("Profile").unwrap();
    let fields = resolve_field_path(&profile.fields, &["scripts", "preApply", "run"])
        .expect("scripts.preApply.run must resolve past the variant boundary");
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "run");
    assert_eq!(fields[0].type_desc, "string");
}

#[test]
fn explain_resolve_field_path_traverses_past_a_variant_for_array_element_unions() {
    // `backups[].preBackup` reaches its variants through
    // `array_element_variants` rather than `union_variants` directly — same
    // ScriptEntry shape, different path through field_node's 4-way branch.
    let profile = find_schema("Profile").unwrap();
    let fields = resolve_field_path(&profile.fields, &["backups", "preBackup", "run"])
        .expect("backups.preBackup.run must resolve past the variant boundary");
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0].name, "run");
    assert_eq!(fields[0].type_desc, "string");
}

#[test]
fn explain_resolve_field_path_unknown_field_inside_a_variant_still_errors() {
    // A field that exists on neither variant (string has no fields at all;
    // object has no `bogus`) must still report unknown, not silently resolve
    // to something else or panic.
    let profile = find_schema("Profile").unwrap();
    assert!(resolve_field_path(&profile.fields, &["scripts", "preApply", "bogus"]).is_none());
}

#[test]
fn explain_cmd_field_path_past_variant_boundary_human_and_error() {
    // Live end-to-end: the CLI command itself resolves past the variant
    // boundary, and a nonexistent field inside a variant still reports the
    // same "Unknown field path" error as any other unknown field.
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("profile.scripts.preApply.run"), false).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        output.contains("run"),
        "expected the run field past the variant boundary, got: {output}"
    );
    drop(output);
    drop(printer);

    let (printer, _buf) = Printer::for_test_at(Verbosity::Normal);
    let err = cmd_explain(&printer, Some("profile.scripts.preApply.bogus"), false).unwrap_err();
    assert!(
        err.to_string().contains("Unknown field path"),
        "expected the standard unknown-field-path error, got: {err}"
    );
}

#[test]
fn every_schema_is_reflected_once_per_process_including_the_bad_name_path() {
    use std::sync::atomic::Ordering;

    // Warm the memo, whichever test in this binary got there first.
    let _ = all_schemas();
    let reflections = SCHEMA_REFLECTIONS.load(Ordering::Relaxed);
    assert_eq!(reflections, 1, "the memo must reflect exactly once");

    // A mistyped name reads the set twice — once for the lookup that misses,
    // once to list the available names in the error — and a successful lookup
    // reads it again. None of the three is a second reflection.
    let (printer, _buf) = Printer::for_test_at(Verbosity::Normal);
    assert!(cmd_explain(&printer, Some("Modle"), false).is_err());
    cmd_explain(&printer, Some("module"), false).unwrap();

    assert_eq!(SCHEMA_REFLECTIONS.load(Ordering::Relaxed), reflections);
}

#[test]
fn explain_drilldown_renders_the_documented_shape() {
    // The whole drill-in view, pinned byte-for-byte: the heading is the
    // `Explain: <path>` TitleLabel every sibling report noun uses and carries
    // the queried field's own type, the description is body text under it, and
    // the shapes under `Variants` each state their type once (as their name),
    // and the one object shape's fields are the two-column
    // `name <type> — description` list whose name and type columns each
    // align beneath themselves.
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("profile.spec.packages.brew"), false).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    let expected = "\
Explain: profile.spec.packages.brew <([]string | BrewSpec)>
  Homebrew packages (macOS/Linux).

Variants
  []string — Package names, as a bare list.
  BrewSpec — The object form of `brew`: taps, formulae and casks, or a Brewfile. A bare list of names folds into `formulae`.

Fields
  casks     <[]string> — Homebrew casks (GUI applications) to install.
  file      <string>   — Path to a Brewfile to apply instead of (or alongside) `taps`, `formulae` and `casks`.
  formulae  <[]string> — Homebrew formulae (CLI packages) to install.
  taps      <[]string> — Third-party taps to add before installing formulae/casks.
";
    pretty_assertions::assert_eq!(output, expected);
}

// --- named types, enum values, docs pointer ---

#[test]
fn explain_renders_the_named_defs_type_and_keeps_the_wire_shape_word() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("module"), false).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        output.contains("files      <[]ModuleFileEntry>"),
        "expected the named element type in the human view, got: {output}"
    );

    let (printer, cap) = Printer::for_test_doc();
    cmd_explain(&printer, Some("module"), false).unwrap();
    drop(printer);
    let json = cap.json().expect("explain doc carries with_data");
    let files = json["fields"]
        .as_array()
        .expect("fields array")
        .iter()
        .find(|f| f["name"] == "files")
        .expect("files field");
    // The shape word is the wire value and never moves; the named type is an
    // additive field beside it.
    assert_eq!(files["type"], "[]object");
    assert_eq!(files["typeName"], "ModuleFileEntry");
    let depends = json["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "depends")
        .expect("depends field");
    assert!(
        depends.get("typeName").is_none(),
        "a field resolving through no named definition carries no typeName, got: {depends:?}"
    );
}

#[test]
fn explain_renders_enum_values_in_both_views_and_in_json() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("profile.files.managed.strategy"), false).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        output.contains("enum: Symlink, Copy, Template, Hardlink, Patch"),
        "expected the accepted values after the description, got: {output}"
    );

    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("profile.files.managed"), true).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    assert!(
        output.contains("\n  strategy")
            && output.contains("\n    enum: Symlink, Copy, Template, Hardlink, Patch"),
        "expected the enum line indented under its own field row, got: {output}"
    );

    let (printer, cap) = Printer::for_test_doc();
    cmd_explain(&printer, Some("profile.files.managed.strategy"), false).unwrap();
    drop(printer);
    let json = cap.json().expect("drilldown doc carries with_data");
    let field = &json["fields"].as_array().expect("fields array")[0];
    let values: Vec<&str> = field["enum"]
        .as_array()
        .expect("enum array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        values,
        vec!["Symlink", "Copy", "Template", "Hardlink", "Patch"]
    );
    // Additive: a field with no accepted-value list carries no `enum` key.
    let (printer, cap) = Printer::for_test_doc();
    cmd_explain(&printer, Some("module"), false).unwrap();
    drop(printer);
    let json = cap.json().expect("explain doc carries with_data");
    let depends = json["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "depends")
        .unwrap();
    assert!(depends.get("enum").is_none());
}

#[test]
fn explain_points_every_kind_at_its_docs_page() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("module"), false).unwrap();
    printer.flush();
    let output = cfgd_core::test_helpers::captured_text(&buf);
    // A capture opens no hyperlink, so the row states the URL a reader can
    // copy — release-pinned, never `master`.
    let expected = format!(
        "Docs        https://github.com/tj-smith47/cfgd/blob/v{}/docs/spec/module.md#fields",
        env!("CARGO_PKG_VERSION")
    );
    assert!(
        output.contains(&expected),
        "expected the docs row beneath location to carry {expected}, got: {output}"
    );

    let (printer, cap) = Printer::for_test_doc();
    cmd_explain(&printer, None, false).unwrap();
    drop(printer);
    let json = cap.json().expect("index doc carries with_data");
    for schema in json.as_array().expect("index array") {
        let docs = schema["docs"].as_str().unwrap_or_default();
        assert!(
            docs.starts_with("docs/") && docs.contains('#'),
            "every kind points at a docs anchor, {} does not: {docs:?}",
            schema["kind"]
        );
        let url = schema["docsUrl"].as_str().unwrap_or_default();
        assert_eq!(
            url,
            format!(
                "https://github.com/tj-smith47/cfgd/blob/v{}/{docs}",
                env!("CARGO_PKG_VERSION")
            ),
            "docsUrl must be the release-pinned form of docs for {}",
            schema["kind"]
        );
    }
}

/// Every docs pointer `explain` prints names a real file and a heading that
/// really exists in it. cfgd-core pins the registry-derived kinds; TeamConfig
/// is hand-authored here and reaches no registry entry, so its pointer would
/// otherwise be the one nobody checks.
#[test]
fn every_explain_docs_pointer_names_a_real_heading() {
    fn slug(heading: &str) -> String {
        heading
            .to_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == ' ' || *c == '-')
            .map(|c| if c == ' ' { '-' } else { c })
            .collect()
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for schema in all_schemas() {
        let (rel, anchor) = schema
            .docs
            .split_once('#')
            .unwrap_or_else(|| panic!("{} docs pointer carries no anchor", schema.name));
        let path = root.join(rel);
        let body = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("{} points at {rel}, which cannot be read: {e}", schema.name)
        });
        // A `#` inside a fenced block is a shell comment or a YAML key, not a
        // heading, so an anchor could otherwise pass against text no renderer
        // ever turns into a link target.
        let mut in_fence = false;
        let found = body.lines().any(|line| {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                return false;
            }
            if in_fence {
                return false;
            }
            line.strip_prefix('#')
                .is_some_and(|h| slug(h.trim_start_matches('#').trim()) == anchor)
        });
        assert!(
            found,
            "{} points at {rel}#{anchor}, which is no heading in that file",
            schema.name
        );
    }
}

/// TeamConfig's explain tree is hand-authored (a Crossplane XR has no Rust
/// spec type to reflect), so nothing but this test keeps it honest against the
/// XRD that actually admits documents. Every property the XRD declares, at
/// every depth, is in the tree with the XRD's own required-ness, and the tree
/// invents no property the XRD lacks. Descriptions are deliberately NOT
/// compared: the tree's are written for a reader and are richer than the
/// XRD's one-liners.
#[test]
fn hand_authored_teamconfig_tree_matches_the_crossplane_xrd() {
    let xrd: serde_yaml::Value = serde_yaml::from_str(include_str!(
        "../../../../../manifests/crossplane/xrd-teamconfig.yaml"
    ))
    .expect("xrd-teamconfig.yaml parses");
    let spec = &xrd["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"];
    assert!(
        spec.is_mapping(),
        "XRD layout moved; update the path in this test"
    );

    fn compare(path: &str, schema: &serde_yaml::Value, fields: &[FieldNode]) {
        let props = schema["properties"].as_mapping();
        let required: Vec<&str> = schema["required"]
            .as_sequence()
            .map(|r| r.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let Some(props) = props else {
            assert!(
                fields.is_empty(),
                "{path}: the tree lists children the XRD does not declare"
            );
            return;
        };
        for (name, prop) in props {
            let name = name.as_str().unwrap();
            let field = fields.iter().find(|f| f.name == name).unwrap_or_else(|| {
                panic!("{path}.{name}: declared by the XRD, missing from the tree")
            });
            assert_eq!(
                field.required,
                required.contains(&name),
                "{path}.{name}: required-ness disagrees with the XRD"
            );
            // Descend through an object, or through an array of objects.
            let inner = if prop["type"].as_str() == Some("array") {
                &prop["items"]
            } else {
                prop
            };
            if inner["type"].as_str() == Some("object") && inner["properties"].is_mapping() {
                compare(&format!("{path}.{name}"), inner, &field.children);
            }
        }
        for field in fields {
            assert!(
                props.contains_key(serde_yaml::Value::String(field.name.clone())),
                "{path}.{}: in the tree, but the XRD declares no such property",
                field.name
            );
        }
    }
    let schema = find_schema("teamconfig").expect("TeamConfig is explainable");
    compare("spec", spec, &schema.fields);
}

/// No description `explain` renders points at a POSITION on the screen.
///
/// The renderer sorts a level alphabetically (`sorted_by_name`), so a doc
/// comment written against declaration order describes a layout the reader is
/// not looking at: `file … instead of (or alongside) the lists below` sat
/// UNDER the `casks` row it claimed to precede. Three sibling references were
/// correct only by luck of the alphabet, which is not a property a rustdoc
/// edit can preserve.
///
/// The walk is every field of every kind `explain` can render, at every depth,
/// so a new positional word is caught wherever it is authored — a local config
/// struct, a CRD spec, or the hand-authored TeamConfig tree. A field names its
/// siblings instead; the ordering is then the renderer's business alone.
#[test]
fn no_explain_description_names_a_position_on_the_screen() {
    /// The words that place a claim on the SCREEN. Flagged only where the
    /// word closes its phrase (`the lists below.`, `no override below`) —
    /// `a version below this` compares two values and names no row.
    const POSITIONAL: &[&str] = &["below", "above", "preceding", "succeeding"];

    /// Whether `text` uses one of those words as a position rather than as a
    /// comparison: the word ends the clause it sits in.
    fn names_a_position(text: &str) -> bool {
        let lower = text.to_lowercase();
        POSITIONAL.iter().any(|word| {
            lower.match_indices(word).any(|(at, _)| {
                let before_is_boundary = at == 0
                    || !lower[..at]
                        .chars()
                        .next_back()
                        .is_some_and(|c| c.is_alphanumeric());
                let after = lower[at + word.len()..].trim_start_matches([' ', '`']);
                before_is_boundary && after.chars().next().is_none_or(|c| ".,;:)".contains(c))
            })
        })
    }

    fn walk(path: &str, fields: &[FieldNode], found: &mut Vec<String>) {
        for f in fields {
            let here = format!("{path}.{}", f.name);
            if names_a_position(&f.description) {
                found.push(format!("{here}: {}", f.description));
            }
            walk(&here, &f.children, found);
            walk(&here, &f.variants, found);
        }
    }

    let mut found = Vec::new();
    for schema in all_schemas() {
        walk(&schema.name, &schema.fields, &mut found);
    }
    assert!(
        found.is_empty(),
        "a description points at a rendered position, which the alphabetical \
         sort does not guarantee — name the sibling fields instead: {found:#?}"
    );
}

/// No description `explain` renders — or the published schemas carry —
/// addresses a MAINTAINER instead of a user.
///
/// `schemars` takes a type's WHOLE doc comment as its `description`: every
/// paragraph, on `cfgd explain`, in `-o json`, in the SchemaStore-published
/// schema and in every generated agent skill. A WHY paragraph written for
/// the next maintainer therefore ships to a reader who has no
/// `profile_spec.rs`: `PackageListSpec`'s named two private functions in
/// rustdoc link syntax on all twelve bare-list drilldowns, and
/// `ScriptCommand`'s explained a rendering decision to whoever hovered
/// `postApply:`. The WHY goes in a `//` comment or on the private function.
///
/// Walked twice, over the in-process render AND the committed
/// `tests/golden/schema/*.json`, so the file SchemaStore serves is judged
/// rather than only the tree `explain` built from it.
#[test]
fn no_schema_description_addresses_a_maintainer_instead_of_a_user() {
    /// Words with no meaning in the config grammar: a schema-derivation
    /// vocabulary, and the first-person plural of a note between authors.
    /// `cfgd renders with` is a user sentence about the theme; only a
    /// rendering DECISION (`cfgd renders \`…\``) is refused.
    const MAINTAINER_VOCABULARY: &[&str] = &[
        "deserializer",
        "schema_with",
        "serde",
        "inline variant",
        "enum variant",
        " we ",
        "cfgd renders `",
    ];

    /// Why `text` reads as a note to a maintainer, if it does: rustdoc
    /// intra-doc link syntax (`` [`ident`] ``), a private schema/deserializer
    /// helper's name, or the vocabulary above.
    fn addresses_a_maintainer(text: &str) -> Option<&'static str> {
        if text.contains("[`") {
            return Some("rustdoc link syntax");
        }
        let names_a_private_fn = text
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|w| {
                w.contains('_')
                    && (w.ends_with("_schema") || w.ends_with("_vec") || w.contains("_or_"))
            });
        if names_a_private_fn {
            return Some("a private function name");
        }
        let lower = format!(" {} ", text.to_lowercase());
        MAINTAINER_VOCABULARY
            .iter()
            .find(|word| lower.contains(*word))
            .copied()
    }

    fn walk(path: &str, fields: &[FieldNode], found: &mut Vec<String>) {
        for f in fields {
            let here = format!("{path}.{}", f.name);
            if let Some(why) = addresses_a_maintainer(&f.description) {
                found.push(format!("{here} ({why}): {}", f.description));
            }
            walk(&here, &f.children, found);
            walk(&here, &f.variants, found);
        }
    }

    fn walk_json(path: &str, value: &serde_json::Value, found: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(d)) = map.get("description")
                    && let Some(why) = addresses_a_maintainer(d)
                {
                    found.push(format!("{path} ({why}): {d}"));
                }
                for (k, v) in map {
                    walk_json(&format!("{path}/{k}"), v, found);
                }
            }
            serde_json::Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    walk_json(&format!("{path}/{i}"), v, found);
                }
            }
            _ => {}
        }
    }

    let mut found = Vec::new();
    for schema in all_schemas() {
        walk(&schema.name, &schema.fields, &mut found);
    }
    let goldens =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../cfgd-core/tests/golden/schema");
    let mut judged = 0;
    for entry in std::fs::read_dir(&goldens).expect("golden schema dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_none_or(|e| e != "json") {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("golden readable");
        let value: serde_json::Value = serde_json::from_str(&body).expect("golden is JSON");
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        walk_json(&name, &value, &mut found);
        judged += 1;
    }
    assert!(
        judged > 0,
        "no golden schemas judged under {}",
        goldens.display()
    );
    assert!(
        found.is_empty(),
        "a schema description is written for a maintainer, and schemars ships every \
         paragraph of a doc comment to the user — move the WHY to a `//` comment: {found:#?}"
    );
}

/// Every mark a field row carries lands in a COLUMN.
///
/// The legend tells the reader to scan for `[+]`, and only a column can be
/// scanned: concatenated onto a variable-width type span the mark landed at
/// six different x positions down eight rows, and ` (required)` — which sits
/// between the type and the mark — moved every mark after it again.
///
/// Both slots are walked, on both surfaces that render a field row: the flat
/// field list and the `--recursive` tree, whose rows are indented per level
/// and so are measured per level.
#[test]
fn every_field_row_mark_lands_in_a_column() {
    /// Split rendered rows into LEVELS — the unit each column is measured
    /// over. In the flat list that is the whole list; in the `--recursive`
    /// tree a level is one parent's children, so two sibling groups at the
    /// same indent are two levels and legitimately pad to different widths.
    fn levels(rendered: &str) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = Vec::new();
        // (indent, index into `out`) for each open level, outermost first.
        let mut open: Vec<(usize, usize)> = Vec::new();
        for line in rendered.lines().filter(|l| !l.trim().is_empty()) {
            let indent = line.len() - line.trim_start().len();
            while open.last().is_some_and(|(at, _)| *at > indent) {
                open.pop();
            }
            let slot = match open.last() {
                Some((at, slot)) if *at == indent => *slot,
                _ => {
                    out.push(Vec::new());
                    open.push((indent, out.len() - 1));
                    out.len() - 1
                }
            };
            out[slot].push(line.to_string());
        }
        out
    }

    for (query, recursive) in [("module", false), ("profile", true)] {
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        cmd_explain(&printer, Some(query), recursive).unwrap();
        printer.flush();
        let rendered = cfgd_core::test_helpers::captured_text(&buf);

        for needle in ["[+]", "(required)"] {
            for level in levels(&rendered) {
                let marked: Vec<(usize, &String)> = level
                    .iter()
                    .filter_map(|line| {
                        line.find(needle)
                            .map(|at| (line[..at].chars().count(), line))
                    })
                    .collect();
                let Some((first, _)) = marked.first() else {
                    continue;
                };
                assert!(
                    marked.iter().all(|(at, _)| at == first),
                    "`{needle}` lands at more than one column within one level of \
                     `cfgd explain {query}`: {marked:#?}"
                );
            }
        }
        assert!(
            !rendered.lines().any(|l| l.ends_with(' ')),
            "a padded column left trailing whitespace: {rendered:?}"
        );
    }
}

/// Every union-typed field renders its shapes ONCE and its fields BY NAME.
///
/// Walked over every field of every registered schema whose `variants` are
/// non-empty, rendering the non-recursive drill-down each of them heads:
///
/// - a `$ref` member of the union names its `$defs` type, in the heading's
///   type span and on its own Variants row (`<([]string | BrewSpec)>`), and
///   `object` is reserved for a member that genuinely has none;
/// - a Variants row carries one fact: its name IS its shape, so no `<type>`
///   span restates it;
/// - a union whose ONE object arm is the only shape with fields lists that
///   arm's fields under `Fields`, where a plain object would have listed its
///   own — a `Variants` section discloses shapes, it never replaces the list;
/// - every row marked `[+]` names a path segment `resolve_field_path` accepts
///   under the page's own path, so the legend's placeholder substitutes to a
///   command that runs.
///
/// - every object arm is a NAMED type, so the type span reads
///   `<([]string | PackageListSpec)>` and not `<([]string | object)>`;
/// - a field's description never restates a type its own span already
///   shows: `Homebrew packages. Accepts a bare list or a \`BrewSpec\`
///   mapping.` under `<([]string | BrewSpec)>` above a `Variants` section
///   listing both was one fact stated three times.
///
/// One commit shipped the first four wrong on the one screen the author demo
/// exists to show: `brew`'s `taps`/`formulae`/`casks`/`file` vanished behind a
/// row named `object`, that row wore the `[+]` the legend promised a field
/// for, and eighteen of nineteen type spans read `object`.
#[test]
fn every_union_typed_field_renders_its_shapes_once_and_its_fields_by_name() {
    fn walk<'a>(
        schema: &ResourceSchema,
        fields: &'a [cfgd_core::schema::FieldNode],
        path: &mut Vec<&'a str>,
        seen: &mut usize,
    ) {
        for f in fields {
            path.push(&f.name);
            if !f.variants.is_empty() {
                *seen += 1;
                let resolved = resolve_field_path(&schema.fields, path)
                    .unwrap_or_else(|| panic!("{} resolves", path.join(".")));
                let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
                printer.emit(build_explain_drilldown_doc(schema, path, resolved, false));
                printer.flush();
                let rendered = cfgd_core::test_helpers::captured_text(&buf);
                let page = format!("{}.spec.{}", schema.name.to_lowercase(), path.join("."));
                let heading = rendered.lines().next().unwrap_or_default().to_string();

                for v in &f.variants {
                    assert!(
                        !v.type_name.is_empty() || v.type_desc != "object",
                        "{page}: an object arm is a named type a reader can look up, never \
                         an anonymous `object`"
                    );
                    // A field whose own type is a named union (`[]ScriptEntry`)
                    // heads with that name and discloses the arms under
                    // `Variants`; an inline union heads with the join.
                    if !v.type_name.is_empty() {
                        assert!(
                            (heading.contains(&v.displayed_type()) || !f.type_name.is_empty())
                                && !heading.contains("object"),
                            "{page}: a $ref member names its type in the heading: {heading}"
                        );
                    }
                    assert!(
                        !f.description.contains(&v.displayed_type()),
                        "{page}: the description restates its own type span `{}`: {}",
                        v.displayed_type(),
                        f.description
                    );
                    let row = rendered
                        .lines()
                        .find(|l| l.trim_start().starts_with(&v.name))
                        .unwrap_or_else(|| {
                            panic!("{page}: a Variants row for {}:\n{rendered}", v.name)
                        });
                    assert!(
                        !row.contains(&format!("<{}>", v.displayed_type())),
                        "{page}: a Variants row states its shape once: {row}"
                    );
                    assert!(
                        !row.contains("[+]"),
                        "{page}: a shape is not a field and never earns the mark: {row}"
                    );
                }

                let mut with_fields = f.variants.iter().filter(|v| !v.children.is_empty());
                if let (Some(only), None) = (with_fields.next(), with_fields.next()) {
                    assert!(
                        rendered.contains("Fields"),
                        "{page}: the one object arm's fields are listed:\n{rendered}"
                    );
                    for child in &only.children {
                        assert!(
                            rendered
                                .lines()
                                .any(|l| l.trim_start().starts_with(&child.name)),
                            "{page}: `{}` is a row a reader can scan:\n{rendered}",
                            child.name
                        );
                    }
                }

                for line in rendered.lines().filter(|l| l.contains("[+]")) {
                    let name = line.split_whitespace().next().unwrap_or_default();
                    let mut deeper = path.clone();
                    deeper.push(name);
                    assert!(
                        resolve_field_path(&schema.fields, &deeper).is_some(),
                        "{page}: `[+]` promises `cfgd explain {page}.{name}` runs: {line}"
                    );
                }
            }
            walk(schema, &f.children, path, seen);
            for v in &f.variants {
                walk(schema, &v.children, path, seen);
            }
            path.pop();
        }
    }
    let mut seen = 0;
    for schema in all_schemas() {
        walk(schema, &schema.fields, &mut Vec::new(), &mut seen);
    }
    assert!(
        seen > 0,
        "the registry carries at least one union-typed field"
    );
}
