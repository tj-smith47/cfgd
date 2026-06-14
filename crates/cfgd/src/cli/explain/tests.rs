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
    let output = buf.lock().unwrap();
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
    let output = buf.lock().unwrap();
    assert!(
        output.contains("Module"),
        "expected Module name in output, got: {output}"
    );
    assert!(
        output.contains("packages"),
        "expected packages field in module output, got: {output}"
    );
    assert!(
        output.contains("FIELDS"),
        "expected FIELDS section header, got: {output}"
    );
}

#[test]
fn explain_cmd_field_path() {
    let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer, Some("module.packages"), false).unwrap();
    printer.flush();
    let output = buf.lock().unwrap();
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
    let output_a = buf_a.lock().unwrap().clone();

    let (printer_b, buf_b) = Printer::for_test_at(Verbosity::Normal);
    cmd_explain(&printer_b, Some("module.spec.packages"), false).unwrap();
    printer_b.flush();
    let output_b = buf_b.lock().unwrap().clone();

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
    let output = buf.lock().unwrap();
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
    // ThemeOverrides has 19 fields (12 styles + 7 icons) — verify schema matches
    let config = find_schema("Config").unwrap();
    let fields = resolve_field_path(&config.fields, &["theme", "overrides"]);
    let children = fields.unwrap();
    assert_eq!(
        children.len(),
        19,
        "ThemeOverrides schema should have 19 fields, got {}",
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
