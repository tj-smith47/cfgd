//! What a config document does not declare that the running binary's schema
//! carries.

use cfgd_core::config::CfgdConfig;

/// Dotted key paths the typed value carries that the document on disk does
/// not name.
pub struct PendingAlignment {
    /// Sorted, e.g. `["spec.fileStrategy", "spec.migrationPolicy"]`.
    pub keys: Vec<String>,
}

/// What the document at `on_disk` does not declare that the typed value `cfg`
/// carries. `cfg` is the parse of those same bytes, so the two sides differ by
/// exactly what the deserializer materialized.
///
/// Nothing here reports a VERSION. A document at another `apiVersion` cannot
/// reach this function: `validate_api_version` refuses every version the
/// conversion table does not name, and the shipped table names one, so a `cfg`
/// that parsed spells `API_VERSION`. The release that adds a second row to
/// that table is the one that can report a version here, with a document that
/// can populate it.
pub fn pending_alignment(cfg: &CfgdConfig, on_disk: &str) -> PendingAlignment {
    let declared = serde_yaml::from_str(on_disk).unwrap_or(serde_yaml::Value::Null);
    PendingAlignment {
        keys: crate::cli::helpers::undeclared_scalar_keys(cfg, &declared),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document that names no `migrationPolicy` is behind the schema by
    /// exactly that key; one that names it is behind by nothing. The detection
    /// reads the SERIALIZED typed value against the declared tree, so a field
    /// added to `ConfigSpec` later appears here with no edit to this function.
    #[test]
    fn a_document_missing_a_defaulted_field_is_reported_as_behind_by_that_key() {
        let doc = "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  profile: work\n";
        let cfg = cfgd_core::config::parse_config(doc, std::path::Path::new("cfgd.yaml")).unwrap();
        let pending = pending_alignment(&cfg, doc);
        assert!(
            pending.keys.contains(&"spec.migrationPolicy".to_string()),
            "the document names no migrationPolicy: {:?}",
            pending.keys
        );
        assert!(
            !pending.keys.contains(&"spec.profile".to_string()),
            "a key the document declares is never pending: {:?}",
            pending.keys
        );

        let aligned = format!("{doc}  migrationPolicy: Prompt\n");
        let cfg =
            cfgd_core::config::parse_config(&aligned, std::path::Path::new("cfgd.yaml")).unwrap();
        assert!(
            !pending_alignment(&cfg, &aligned)
                .keys
                .contains(&"spec.migrationPolicy".to_string()),
            "a declared key is not pending"
        );
    }
}
