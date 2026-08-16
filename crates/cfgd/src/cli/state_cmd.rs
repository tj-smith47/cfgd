use super::*;

use cfgd_core::output::{Doc, Printer, Role};
use cfgd_core::state::PackageManagerPrefixRecord;

/// Sole place the `forget-prefix` final Role/subject/payload is decided, so
/// every caller (the CLI entry point and tests) sees identical shape whether
/// or not a row existed to forget.
pub fn build_forget_prefix_doc(
    manager: &str,
    forgotten: Option<&PackageManagerPrefixRecord>,
) -> Doc {
    match forgotten {
        Some(record) => Doc::new()
            .status(
                Role::Ok,
                format!("Forgot persisted global-install prefix for '{manager}'"),
            )
            .with_data(&ForgetPrefixOutput {
                manager: manager.to_string(),
                forgotten: true,
                prefix: Some(record.prefix.clone()),
                is_fallback: Some(record.is_fallback),
                resolved_at: Some(record.resolved_at.clone()),
            }),
        None => Doc::new()
            .status(
                Role::Info,
                format!("No persisted global-install prefix decision for '{manager}'"),
            )
            .with_data(&ForgetPrefixOutput {
                manager: manager.to_string(),
                forgotten: false,
                prefix: None,
                is_fallback: None,
                resolved_at: None,
            }),
    }
}

pub fn cmd_state_forget_prefix(
    printer: &Printer,
    manager: &str,
    state_dir: Option<&Path>,
    scope: cfgd_core::Scope,
) -> anyhow::Result<()> {
    let state = open_state_store(state_dir, scope)?;
    let forgotten = state.forget_package_manager_prefix(manager)?;
    printer.emit(build_forget_prefix_doc(manager, forgotten.as_ref()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use cfgd_core::output::Verbosity;
    use cfgd_core::state::StateStore;

    fn test_state_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn cmd_state_forget_prefix_reports_no_row_when_nothing_persisted() {
        let dir = test_state_dir();
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        cmd_state_forget_prefix(&printer, "npm", Some(dir.path()), cfgd_core::Scope::User).unwrap();
        drop(printer);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("No persisted global-install prefix decision for 'npm'"),
            "got: {output}"
        );
    }

    #[test]
    fn cmd_state_forget_prefix_deletes_the_persisted_row() {
        let dir = test_state_dir();
        let state = StateStore::open_in_dir(dir.path()).unwrap();
        state
            .record_package_manager_prefix("npm", "/home/u/.npm-global", true)
            .unwrap();
        drop(state);

        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        cmd_state_forget_prefix(&printer, "npm", Some(dir.path()), cfgd_core::Scope::User).unwrap();
        drop(printer);

        let output = buf.lock().unwrap();
        assert!(
            output.contains("Forgot persisted global-install prefix for 'npm'"),
            "got: {output}"
        );

        let state = StateStore::open_in_dir(dir.path()).unwrap();
        assert_eq!(
            state.package_manager_prefix("npm").unwrap(),
            None,
            "row must actually be gone"
        );
    }

    #[test]
    fn build_forget_prefix_doc_json_payload_present_when_forgotten() {
        use cfgd_core::output::OutputFormat;

        let record = PackageManagerPrefixRecord {
            manager: "npm".to_string(),
            prefix: "/home/u/.npm-global".to_string(),
            is_fallback: true,
            resolved_at: "2026-01-01T00:00:00Z".to_string(),
        };
        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Json);
        printer.emit(build_forget_prefix_doc("npm", Some(&record)));
        drop(printer);

        let output = buf.lock().unwrap();
        let value: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(value["manager"], "npm");
        assert_eq!(value["forgotten"], true);
        assert_eq!(value["prefix"], "/home/u/.npm-global");
        assert_eq!(value["isFallback"], true);
        assert_eq!(value["resolvedAt"], "2026-01-01T00:00:00Z");
    }

    #[test]
    fn build_forget_prefix_doc_omits_row_fields_when_nothing_forgotten() {
        use cfgd_core::output::OutputFormat;

        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Json);
        printer.emit(build_forget_prefix_doc("npm", None));
        drop(printer);

        let output = buf.lock().unwrap();
        let value: serde_json::Value = serde_json::from_str(output.trim()).unwrap();
        assert_eq!(value["manager"], "npm");
        assert_eq!(value["forgotten"], false);
        assert!(value.get("prefix").is_none());
        assert!(value.get("isFallback").is_none());
        assert!(value.get("resolvedAt").is_none());
    }
}
