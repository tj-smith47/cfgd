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
    let fields = resolve_field_path(&profile.fields, &["packages", "brew"]);
    assert!(fields.is_some());
    let children = fields.unwrap();
    // Brew has file, taps, formulae, casks
    assert_eq!(children.len(), 4);
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
    // ThemeOverrides has 21 fields (13 styles + 8 icons) — verify schema matches
    let config = find_schema("Config").unwrap();
    let fields = resolve_field_path(&config.fields, &["theme", "overrides"]);
    let children = fields.unwrap();
    assert_eq!(
        children.len(),
        21,
        "ThemeOverrides schema should have 21 fields, got {}",
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
        output.contains("object"),
        "expected the object variant, got: {output}"
    );
    // Non-recursive: the object variant's own fields (`run`, …) stay behind
    // the `[+]` marker, matching every other expandable field.
    assert!(
        output.contains("[+]"),
        "expected the object variant to carry an unexpanded [+] marker, got: {output}"
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
