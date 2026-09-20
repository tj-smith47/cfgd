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

    /// `cfgd init` scaffolds the theme as the union's scalar arm
    /// (`theme: dracula`), which IS `theme: {name: dracula}`. A document
    /// written that way declares the name beneath it, so nothing under
    /// `spec.output.theme` is behind the schema, and nothing is reported that
    /// `cfgd config set` would then refuse to write.
    #[test]
    fn a_theme_written_as_the_unions_scalar_arm_declares_the_name_beneath_it() {
        let doc = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  output:\n    theme: dracula\n  fileStrategy: Symlink\n";
        let cfg = cfgd_core::config::parse_config(doc, std::path::Path::new("cfgd.yaml")).unwrap();
        let pending = pending_alignment(&cfg, doc);
        assert!(
            !pending
                .keys
                .iter()
                .any(|key| key.starts_with("spec.output.theme")),
            "the scalar arm declares the theme: {:?}",
            pending.keys
        );
        assert!(
            pending.keys.contains(&"spec.migrationPolicy".to_string()),
            "a key the document really is missing is still reported: {:?}",
            pending.keys
        );
    }

    /// An element of a declared list is data, and neither key walker
    /// addresses a sequence, so no reported key steps through an index.
    #[test]
    fn no_reported_key_names_an_element_of_a_declared_list() {
        let doc = "apiVersion: cfgd.io/v1alpha1\nkind: CfgdConfig\nmetadata:\n  name: t\nspec:\n  origin:\n    - type: Git\n      url: git@github.com:me/dotfiles.git\n";
        let cfg = cfgd_core::config::parse_config(doc, std::path::Path::new("cfgd.yaml")).unwrap();
        let pending = pending_alignment(&cfg, doc);
        assert!(
            !pending.keys.iter().any(|key| key
                .split('.')
                .any(|segment| segment.parse::<usize>().is_ok())),
            "no reported key steps through an index: {:?}",
            pending.keys
        );
        assert!(
            !pending
                .keys
                .iter()
                .any(|key| key.starts_with("spec.origin.")),
            "what a declared element carries is data, not a pending key: {:?}",
            pending.keys
        );
        assert!(
            pending.keys.contains(&"spec.migrationPolicy".to_string()),
            "the walk still reports the keys it should: {:?}",
            pending.keys
        );
    }

    /// Every key reported is one `cfgd config migrate` can materialize: each
    /// is written into the document through the same walker that verb uses,
    /// and the re-parsed document is behind the schema by nothing. A key the
    /// writer refuses, or one whose write the reader cannot see afterwards,
    /// leaves the second call non-empty. The fixture is what `cfgd init`
    /// scaffolds, so the union's scalar arm is on the path the writer has to
    /// descend.
    #[test]
    fn every_key_pending_alignment_reports_is_one_the_config_writer_can_set() {
        let doc = "apiVersion: cfgd.io/v1alpha1\nkind: Config\nmetadata:\n  name: t\nspec:\n  output:\n    theme: dracula\n  fileStrategy: Symlink\n  profile: work\n";
        let cfg = cfgd_core::config::parse_config(doc, std::path::Path::new("cfgd.yaml")).unwrap();
        let pending = pending_alignment(&cfg, doc);
        assert!(
            pending.keys.contains(&"spec.migrationPolicy".to_string()),
            "the fixture reports something to write: {:?}",
            pending.keys
        );

        let serialized = serde_yaml::to_value(&cfg).unwrap();
        let mut written: serde_yaml::Value = serde_yaml::from_str(doc).unwrap();
        for key in &pending.keys {
            let value = crate::cli::config_cmd::walk_yaml_path(&serialized, key)
                .unwrap_or_else(|e| panic!("the read walker resolves '{key}': {e}"))
                .clone();
            let (parent, leaf) = crate::cli::config_cmd::walk_yaml_path_mut(&mut written, key)
                .unwrap_or_else(|e| panic!("the write walker sets '{key}': {e}"));
            parent.insert(serde_yaml::Value::String(leaf), value);
        }

        let written = serde_yaml::to_string(&written).unwrap();
        let cfg = cfgd_core::config::parse_config(&written, std::path::Path::new("cfgd.yaml"))
            .unwrap_or_else(|e| panic!("the written document parses: {e}"));
        let pending = pending_alignment(&cfg, &written);
        assert!(
            pending.keys.is_empty(),
            "a document carrying every reported key is behind by nothing: {:?}",
            pending.keys
        );
    }
}
