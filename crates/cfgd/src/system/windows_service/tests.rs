use super::*;

#[test]
fn service_parse_sc_state_running() {
    let output = "SERVICE_NAME: MyService\n\
                      \tTYPE               : 10  WIN32_OWN_PROCESS\n\
                      \tSTATE              : 4  RUNNING\n\
                      \t\t\t\t\t(STOPPABLE, NOT_PAUSABLE)\n";
    assert_eq!(parse_sc_state(output), Some("running".to_string()));
}

#[test]
fn service_parse_sc_state_stopped() {
    let output = "SERVICE_NAME: MyService\n\
                      \tSTATE              : 1  STOPPED\n";
    assert_eq!(parse_sc_state(output), Some("stopped".to_string()));
}

#[test]
fn service_parse_sc_state_paused() {
    let output = "\tSTATE              : 7  PAUSED\n";
    assert_eq!(parse_sc_state(output), Some("paused".to_string()));
}

#[test]
fn service_parse_sc_state_no_state_line() {
    let output = "SERVICE_NAME: MyService\n\
                      \tTYPE               : 10  WIN32_OWN_PROCESS\n";
    assert_eq!(parse_sc_state(output), None);
}

#[test]
fn service_parse_sc_state_empty_input() {
    assert_eq!(parse_sc_state(""), None);
}

#[test]
fn service_parse_sc_start_type_auto() {
    let output = "SERVICE_NAME: MyService\n\
                      \tTYPE               : 10  WIN32_OWN_PROCESS\n\
                      \tSTART_TYPE         : 2   AUTO_START\n\
                      \tERROR_CONTROL      : 1   NORMAL\n";
    assert_eq!(parse_sc_start_type(output), Some("auto".to_string()));
}

#[test]
fn service_parse_sc_start_type_manual() {
    let output = "\tSTART_TYPE         : 3   DEMAND_START\n";
    assert_eq!(parse_sc_start_type(output), Some("manual".to_string()));
}

#[test]
fn service_parse_sc_start_type_disabled() {
    let output = "\tSTART_TYPE         : 4   DISABLED\n";
    assert_eq!(parse_sc_start_type(output), Some("disabled".to_string()));
}

#[test]
fn service_parse_sc_start_type_no_start_type_line() {
    let output = "SERVICE_NAME: MyService\n\
                      \tTYPE               : 10  WIN32_OWN_PROCESS\n";
    assert_eq!(parse_sc_start_type(output), None);
}

#[test]
fn service_parse_sc_start_type_empty_input() {
    assert_eq!(parse_sc_start_type(""), None);
}

#[test]
fn service_parse_sc_config_value_extracts_binary_path() {
    let output = "SERVICE_NAME: MyService\n\
                      \tTYPE               : 10  WIN32_OWN_PROCESS\n\
                      \tBINARY_PATH_NAME   : C:\\path\\to\\service.exe\n\
                      \tDISPLAY_NAME       : My Custom Service\n";
    assert_eq!(
        parse_sc_config_value(output, "BINARY_PATH_NAME"),
        Some("C:\\path\\to\\service.exe".to_string())
    );
    assert_eq!(
        parse_sc_config_value(output, "DISPLAY_NAME"),
        Some("My Custom Service".to_string())
    );
}

#[test]
fn service_parse_sc_config_value_missing_key() {
    let output = "SERVICE_NAME: MyService\n";
    assert_eq!(parse_sc_config_value(output, "BINARY_PATH_NAME"), None);
}

#[test]
fn service_parse_entries() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: MyService
  displayName: My Custom Service
  binaryPath: C:\path\to\service.exe
  startType: auto
  state: running
- name: AnotherService
  startType: disabled
  state: stopped
"#,
    )
    .unwrap();
    let entries = WindowsServiceConfigurator::parse_services(&yaml);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].name, "MyService");
    assert_eq!(
        entries[0].display_name,
        Some("My Custom Service".to_string())
    );
    assert_eq!(
        entries[0].binary_path,
        Some(r"C:\path\to\service.exe".to_string())
    );
    assert_eq!(entries[0].start_type, Some("auto".to_string()));
    assert_eq!(entries[0].state, Some("running".to_string()));
    assert_eq!(entries[1].name, "AnotherService");
    assert_eq!(entries[1].binary_path, None);
    assert_eq!(entries[1].display_name, None);
    assert_eq!(entries[1].start_type, Some("disabled".to_string()));
    assert_eq!(entries[1].state, Some("stopped".to_string()));
}

#[test]
fn service_parse_entries_empty_sequence() {
    let yaml = serde_yaml::Value::Sequence(Vec::new());
    let entries = WindowsServiceConfigurator::parse_services(&yaml);
    assert!(entries.is_empty());
}

#[test]
fn service_parse_entries_non_sequence() {
    let yaml = serde_yaml::Value::String("not a sequence".to_string());
    let entries = WindowsServiceConfigurator::parse_services(&yaml);
    assert!(entries.is_empty());
}

#[test]
fn service_parse_entries_skips_missing_name() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- displayName: No Name Field
  startType: auto
- name: ValidService
  state: running
"#,
    )
    .unwrap();
    let entries = WindowsServiceConfigurator::parse_services(&yaml);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "ValidService");
}

#[test]
fn service_diff_empty_desired() {
    let wsc = WindowsServiceConfigurator;
    let yaml = serde_yaml::Value::Sequence(Vec::new());
    let drifts = wsc.diff(&yaml).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn service_diff_non_sequence_desired() {
    let wsc = WindowsServiceConfigurator;
    let yaml = serde_yaml::Value::String("not a sequence".into());
    let drifts = wsc.diff(&yaml).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn service_current_state_returns_empty_sequence() {
    let wsc = WindowsServiceConfigurator;
    let state = wsc.current_state().unwrap();
    assert!(state.is_sequence());
    assert!(state.as_sequence().unwrap().is_empty());
}

#[test]
fn windows_services_not_available_on_linux() {
    let wsc = WindowsServiceConfigurator;
    assert!(!wsc.is_available());
}

#[test]
fn sc_start_type_auto() {
    assert_eq!(sc_start_type("auto"), Some("auto"));
}

#[test]
fn sc_start_type_manual() {
    assert_eq!(sc_start_type("manual"), Some("demand"));
}

#[test]
fn sc_start_type_disabled() {
    assert_eq!(sc_start_type("disabled"), Some("disabled"));
}

#[test]
fn sc_start_type_unrecognized() {
    assert_eq!(sc_start_type("boot"), None);
    assert_eq!(sc_start_type(""), None);
    assert_eq!(sc_start_type("Auto"), None);
}

#[test]
fn parse_sc_state_start_pending() {
    let output = "\tSTATE              : 2  START_PENDING\n";
    assert_eq!(parse_sc_state(output), Some("start-pending".to_string()));
}

#[test]
fn parse_sc_state_stop_pending() {
    let output = "\tSTATE              : 3  STOP_PENDING\n";
    assert_eq!(parse_sc_state(output), Some("stop-pending".to_string()));
}

#[test]
fn parse_sc_config_value_empty_input() {
    assert_eq!(parse_sc_config_value("", "ANYTHING"), None);
}

#[test]
fn parse_sc_config_value_value_with_spaces() {
    let output = "\tDISPLAY_NAME       : My Long Service Name\n";
    assert_eq!(
        parse_sc_config_value(output, "DISPLAY_NAME"),
        Some("My Long Service Name".to_string())
    );
}

#[test]
fn service_parse_entries_minimal() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: MinimalService
"#,
    )
    .unwrap();
    let entries = WindowsServiceConfigurator::parse_services(&yaml);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "MinimalService");
    assert!(entries[0].display_name.is_none());
    assert!(entries[0].binary_path.is_none());
    assert!(entries[0].start_type.is_none());
    assert!(entries[0].state.is_none());
}

#[test]
fn service_diff_missing_service_without_binary_path_no_drift() {
    let wsc = WindowsServiceConfigurator;
    // A service entry with only name and state but no binary_path
    // On non-Windows, query_service returns None
    // Without binary_path, no "exists" drift should be produced
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: NonExistentService12345
  state: running
"#,
    )
    .unwrap();
    let drifts = wsc.diff(&yaml).unwrap();
    // On non-Windows, query_service returns None.
    // The code only reports "exists" drift when binary_path is Some,
    // so there should be no drift for this entry.
    assert!(
        !drifts.iter().any(|d| d.key.contains("exists")),
        "should not report exists drift without binary_path"
    );
}

#[test]
fn parse_services_single_full_entry() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: MyService
  displayName: My Service Display
  binaryPath: C:\Program Files\svc.exe
  startType: auto
  state: running
"#,
    )
    .unwrap();
    let entries = WindowsServiceConfigurator::parse_services(&yaml);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "MyService");
    assert_eq!(
        entries[0].display_name.as_deref(),
        Some("My Service Display")
    );
    assert_eq!(
        entries[0].binary_path.as_deref(),
        Some("C:\\Program Files\\svc.exe")
    );
    assert_eq!(entries[0].start_type.as_deref(), Some("auto"));
    assert_eq!(entries[0].state.as_deref(), Some("running"));
}

#[test]
fn parse_services_multiple_entries() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: svc1
  binaryPath: /bin/svc1
- name: svc2
  startType: manual
- name: svc3
"#,
    )
    .unwrap();
    let entries = WindowsServiceConfigurator::parse_services(&yaml);
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].name, "svc1");
    assert!(entries[0].binary_path.is_some());
    assert_eq!(entries[1].name, "svc2");
    assert_eq!(entries[1].start_type.as_deref(), Some("manual"));
    assert_eq!(entries[2].name, "svc3");
    assert!(entries[2].binary_path.is_none());
}

#[test]
fn parse_services_non_sequence_returns_empty() {
    let yaml = serde_yaml::Value::String("not a sequence".into());
    let entries = WindowsServiceConfigurator::parse_services(&yaml);
    assert!(entries.is_empty());
}

#[test]
fn parse_services_entries_without_name_skipped() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- binaryPath: /bin/noname
- name: valid
- displayName: orphan
"#,
    )
    .unwrap();
    let entries = WindowsServiceConfigurator::parse_services(&yaml);
    assert_eq!(
        entries.len(),
        1,
        "only the entry with 'name' should be included"
    );
    assert_eq!(entries[0].name, "valid");
}

#[test]
fn parse_services_null_input() {
    let yaml = serde_yaml::Value::Null;
    let entries = WindowsServiceConfigurator::parse_services(&yaml);
    assert!(entries.is_empty());
}

#[test]
fn parse_services_optional_fields_all_none() {
    let yaml: serde_yaml::Value = serde_yaml::from_str("- name: bare\n").unwrap();
    let entries = WindowsServiceConfigurator::parse_services(&yaml);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "bare");
    assert!(entries[0].display_name.is_none());
    assert!(entries[0].binary_path.is_none());
    assert!(entries[0].start_type.is_none());
    assert!(entries[0].state.is_none());
}

#[test]
fn parse_sc_state_continue_pending() {
    let output = "\tSTATE              : 5  CONTINUE_PENDING\n";
    assert_eq!(parse_sc_state(output), Some("continue-pending".to_string()));
}

#[test]
fn parse_sc_state_pause_pending() {
    let output = "\tSTATE              : 6  PAUSE_PENDING\n";
    assert_eq!(parse_sc_state(output), Some("pause-pending".to_string()));
}

#[test]
fn parse_sc_state_underscores_become_hyphens() {
    // The function replaces '_' with '-' via .replace('_', "-")
    let output = "\tSTATE              : 2  START_PENDING\n";
    let state = parse_sc_state(output).unwrap();
    assert!(
        !state.contains('_'),
        "underscores should be converted to hyphens"
    );
    assert_eq!(state, "start-pending");
}

#[test]
fn parse_sc_state_full_output_with_extra_lines() {
    let output = "\
SERVICE_NAME: W32Time\n\
\tTYPE               : 30  WIN32\n\
\tSTATE              : 4  RUNNING\n\
\t\t\t\t\t(STOPPABLE, NOT_PAUSABLE, ACCEPTS_SHUTDOWN)\n\
\tWIN32_EXIT_CODE    : 0  (0x0)\n\
\tSERVICE_EXIT_CODE  : 0  (0x0)\n\
\tCHECKPOINT         : 0x0\n\
\tWAIT_HINT          : 0x0\n";
    assert_eq!(parse_sc_state(output), Some("running".to_string()));
}

#[test]
fn parse_sc_start_type_lowercase_input_still_works() {
    // The function uppercases the rest before checking .contains()
    let output = "\tSTART_TYPE         : 2   auto_start\n";
    assert_eq!(parse_sc_start_type(output), Some("auto".to_string()));
}

#[test]
fn parse_sc_start_type_full_qc_output() {
    let output = "\
[SC] QueryServiceConfig SUCCESS\n\
\n\
SERVICE_NAME: Spooler\n\
\tTYPE               : 110  WIN32_OWN_PROCESS  (interactive)\n\
\tSTART_TYPE         : 2   AUTO_START\n\
\tERROR_CONTROL      : 1   NORMAL\n\
\tBINARY_PATH_NAME   : C:\\Windows\\System32\\spoolsv.exe\n\
\tLOAD_ORDER_GROUP   : SpoolerGroup\n\
\tTAG                : 0\n\
\tDISPLAY_NAME       : Print Spooler\n\
\tDEPENDENCIES       : RPCSS\n\
\t                   : http\n\
\tSERVICE_START_NAME : LocalSystem\n";
    assert_eq!(parse_sc_start_type(output), Some("auto".to_string()));
}

#[test]
fn sc_start_type_case_sensitive() {
    // Function is case-sensitive — "Auto" is not "auto"
    assert_eq!(sc_start_type("Auto"), None);
    assert_eq!(sc_start_type("Manual"), None);
    assert_eq!(sc_start_type("Disabled"), None);
}

#[test]
fn parse_sc_state_only_number_no_word() {
    // STATE line with only the number, no state word after it
    let output = "\tSTATE              : 4\n";
    // split_whitespace on "4" yields only one element, nth(1) returns None
    assert_eq!(parse_sc_state(output), None);
}

#[test]
fn parse_sc_state_state_line_with_extra_whitespace() {
    let output = "\t  STATE              :   4   RUNNING   \n";
    assert_eq!(parse_sc_state(output), Some("running".to_string()));
}

#[test]
fn service_apply_empty_sequence_is_noop() {
    let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
    let wsc = WindowsServiceConfigurator;
    let yaml = serde_yaml::Value::Sequence(Vec::new());
    wsc.apply(&yaml, &printer).unwrap();
}

#[test]
fn service_apply_non_sequence_is_noop() {
    let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
    let wsc = WindowsServiceConfigurator;
    let yaml = serde_yaml::Value::String("not a sequence".into());
    wsc.apply(&yaml, &printer).unwrap();
}

#[test]
fn service_query_returns_none_on_non_windows() {
    assert!(WindowsServiceConfigurator::query_service("AnyService").is_none());
}

#[test]
fn parse_sc_config_value_value_with_colon() {
    let output = "\tBINARY_PATH_NAME   : C:\\path:with:colons\\svc.exe\n";
    assert_eq!(
        parse_sc_config_value(output, "BINARY_PATH_NAME"),
        Some("C:\\path:with:colons\\svc.exe".to_string())
    );
}

// --- sc_create_args ---

#[test]
fn sc_create_args_returns_none_when_binary_path_missing() {
    // sc.exe create without binPath= is invalid; apply() skips create when
    // entry.binary_path is None. Pin that the helper signals "skip" via None
    // rather than returning a malformed args vector.
    assert!(super::sc_create_args("MySvc", None, None, None).is_none());
    assert!(super::sc_create_args("MySvc", None, Some("Display"), Some("auto")).is_none());
}

#[test]
fn sc_create_args_minimal_entry_only_binary_path() {
    let (args, unknown) = super::sc_create_args("MySvc", Some(r"C:\svc.exe"), None, None).unwrap();
    assert_eq!(
        args,
        vec!["create", "MySvc", "binPath=", r"C:\svc.exe"],
        "minimal create must be exactly 4 args"
    );
    assert!(unknown.is_none());
}

#[test]
fn sc_create_args_separates_key_and_value_for_sc_quirk() {
    // sc.exe's quirk: `key= value` is NOT one argument — the trailing
    // `=` is glued to the key and the value is the next argv slot. Pin
    // that the helper produces this shape (regression guard).
    let (args, _) = super::sc_create_args(
        "MySvc",
        Some(r"C:\svc.exe"),
        Some("My Service"),
        Some("auto"),
    )
    .unwrap();
    assert!(
        args.iter().any(|a| a == "binPath="),
        "binPath= must be a standalone arg: {args:?}"
    );
    assert!(
        args.iter().any(|a| a == "DisplayName="),
        "DisplayName= must be a standalone arg: {args:?}"
    );
    assert!(
        args.iter().any(|a| a == "start="),
        "start= must be a standalone arg: {args:?}"
    );
    // Each `key=` must be followed immediately by its value.
    let bin_idx = args.iter().position(|a| a == "binPath=").unwrap();
    assert_eq!(args[bin_idx + 1], r"C:\svc.exe");
    let dn_idx = args.iter().position(|a| a == "DisplayName=").unwrap();
    assert_eq!(args[dn_idx + 1], "My Service");
    let st_idx = args.iter().position(|a| a == "start=").unwrap();
    assert_eq!(args[st_idx + 1], "auto");
}

#[test]
fn sc_create_args_maps_manual_to_sc_demand() {
    // The user-facing `manual` start-type maps to sc.exe's `demand`.
    let (args, unknown) =
        super::sc_create_args("MySvc", Some(r"C:\svc.exe"), None, Some("manual")).unwrap();
    let st_idx = args.iter().position(|a| a == "start=").unwrap();
    assert_eq!(args[st_idx + 1], "demand");
    assert!(unknown.is_none(), "manual is a recognized value");
}

#[test]
fn sc_create_args_unknown_start_type_falls_back_to_demand_and_returns_warning() {
    // Unknown start types must NOT cause the create to fail outright —
    // apply() warns and uses `demand`. Pin both: the warning surface (raw
    // value returned in the second tuple slot) and the fallback default.
    let (args, unknown) =
        super::sc_create_args("MySvc", Some(r"C:\svc.exe"), None, Some("on-demand")).unwrap();
    let st_idx = args.iter().position(|a| a == "start=").unwrap();
    assert_eq!(args[st_idx + 1], "demand");
    assert_eq!(unknown.as_deref(), Some("on-demand"));
}

#[test]
fn sc_create_args_omits_optional_fields_when_none() {
    // No DisplayName, no start_type → those keys must NOT appear at all.
    let (args, _) = super::sc_create_args("MySvc", Some(r"C:\svc.exe"), None, None).unwrap();
    assert!(
        !args.iter().any(|a| a == "DisplayName="),
        "DisplayName= must be absent when display_name is None"
    );
    assert!(
        !args.iter().any(|a| a == "start="),
        "start= must be absent when start_type is None"
    );
}

#[test]
fn sc_create_args_preserves_field_order_create_name_binpath_displayname_start() {
    let (args, _) = super::sc_create_args(
        "MySvc",
        Some(r"C:\svc.exe"),
        Some("My Service"),
        Some("auto"),
    )
    .unwrap();
    assert_eq!(args[0], "create");
    assert_eq!(args[1], "MySvc");
    assert_eq!(args[2], "binPath=");
    let dn_idx = args.iter().position(|a| a == "DisplayName=").unwrap();
    let st_idx = args.iter().position(|a| a == "start=").unwrap();
    assert!(
        dn_idx < st_idx,
        "DisplayName= must appear before start= for sc.exe argument-order safety: {args:?}"
    );
}

// --- sc_config_args ---

#[test]
fn sc_config_args_returns_none_when_no_field_changed() {
    // Empty entry (no fields to update) must return None so apply() skips
    // the no-op sc invocation.
    assert!(super::sc_config_args("MySvc", None, None, None).is_none());
}

#[test]
fn sc_config_args_returns_none_when_only_unknown_start_type() {
    // Unknown start_type is silently dropped on reconfigure (unlike create).
    // If it's the ONLY field provided, the helper has nothing to emit.
    assert!(super::sc_config_args("MySvc", None, None, Some("on-demand")).is_none());
}

#[test]
fn sc_config_args_minimal_with_only_binary_path() {
    let args = super::sc_config_args("MySvc", Some(r"C:\new.exe"), None, None).unwrap();
    assert_eq!(args, vec!["config", "MySvc", "binPath=", r"C:\new.exe"]);
}

#[test]
fn sc_config_args_emits_recognized_start_but_drops_unknown() {
    // Recognized start_type emits start= start. Provide an unknown value
    // alongside binary_path: the result includes binPath= but NOT start=.
    let args =
        super::sc_config_args("MySvc", Some(r"C:\new.exe"), None, Some("on-demand")).unwrap();
    assert!(
        !args.iter().any(|a| a == "start="),
        "unknown start_type must be silently dropped on reconfigure: {args:?}"
    );
    assert!(args.iter().any(|a| a == "binPath="));
}

#[test]
fn sc_config_args_recognized_start_type_threads_through() {
    let args = super::sc_config_args("MySvc", None, None, Some("disabled")).unwrap();
    let st_idx = args.iter().position(|a| a == "start=").unwrap();
    assert_eq!(args[st_idx + 1], "disabled");
}

#[test]
fn sc_config_args_full_entry_emits_all_three_keys() {
    let args = super::sc_config_args(
        "MySvc",
        Some(r"C:\new.exe"),
        Some("My Service v2"),
        Some("auto"),
    )
    .unwrap();
    assert_eq!(args[0], "config");
    assert_eq!(args[1], "MySvc");
    assert!(args.iter().any(|a| a == "start="));
    assert!(args.iter().any(|a| a == "binPath="));
    assert!(args.iter().any(|a| a == "DisplayName="));
}

#[test]
fn sc_config_args_separates_key_and_value_for_sc_quirk() {
    // Same sc.exe quirk as create: `key= value` is two args.
    let args =
        super::sc_config_args("MySvc", Some(r"C:\new.exe"), Some("DN"), Some("manual")).unwrap();
    let bin_idx = args.iter().position(|a| a == "binPath=").unwrap();
    assert_eq!(args[bin_idx + 1], r"C:\new.exe");
    let dn_idx = args.iter().position(|a| a == "DisplayName=").unwrap();
    assert_eq!(args[dn_idx + 1], "DN");
    let st_idx = args.iter().position(|a| a == "start=").unwrap();
    assert_eq!(args[st_idx + 1], "demand"); // manual → demand
}

// === diff() "absent" drift branch ===
//
// On non-Windows, query_service() always returns None — so any entry that
// has a binaryPath must surface a `{name}.exists` drift with
// expected="present", actual="absent". The inverse (no binaryPath → no
// drift) is covered above.

#[test]
fn service_diff_missing_service_with_binary_path_emits_absent_drift() {
    let wsc = WindowsServiceConfigurator;
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: GhostService
  binaryPath: C:\Program Files\ghost.exe
"#,
    )
    .unwrap();
    let drifts = wsc.diff(&yaml).unwrap();
    assert_eq!(drifts.len(), 1, "exactly one drift expected");
    let d = &drifts[0];
    assert_eq!(d.key, "GhostService.exists");
    assert_eq!(d.expected, "present");
    assert_eq!(d.actual, "absent");
}

#[test]
fn service_diff_absent_drift_uses_dot_exists_key_format() {
    // Pin the exact key shape: `{name}.exists`. Consumers (drift renderers,
    // operator alerts) grep on this suffix to identify presence drift.
    let wsc = WindowsServiceConfigurator;
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: svc.with.dots
  binaryPath: C:\bin.exe
"#,
    )
    .unwrap();
    let drifts = wsc.diff(&yaml).unwrap();
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "svc.with.dots.exists");
}

#[test]
fn service_diff_multiple_absent_services_with_binary_path_emit_drift_each() {
    let wsc = WindowsServiceConfigurator;
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: AlphaSvc
  binaryPath: C:\alpha.exe
- name: BetaSvc
  binaryPath: C:\beta.exe
"#,
    )
    .unwrap();
    let drifts = wsc.diff(&yaml).unwrap();
    assert_eq!(drifts.len(), 2);
    let keys: Vec<&str> = drifts.iter().map(|d| d.key.as_str()).collect();
    assert!(keys.contains(&"AlphaSvc.exists"));
    assert!(keys.contains(&"BetaSvc.exists"));
    for d in &drifts {
        assert_eq!(d.expected, "present");
        assert_eq!(d.actual, "absent");
    }
}

#[test]
fn service_diff_mixed_entries_only_those_with_binary_path_drift() {
    // Pinned in tests: `binaryPath` is the gate for the absent drift, not
    // `state` or `startType`. An entry with only `state: running` and no
    // `binaryPath` must NOT produce an `.exists` drift even though the
    // service is missing.
    let wsc = WindowsServiceConfigurator;
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: HasBinPath
  binaryPath: C:\has.exe
- name: NoBinPath
  state: running
"#,
    )
    .unwrap();
    let drifts = wsc.diff(&yaml).unwrap();
    assert_eq!(drifts.len(), 1, "only the entry with binaryPath drifts");
    assert_eq!(drifts[0].key, "HasBinPath.exists");
}
