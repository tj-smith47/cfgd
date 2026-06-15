use crate::generate::{SchemaKind, ValidationResult};
use crate::schema::{KIND_REGISTRY, KindEntry};

/// Look up the registry entry for a `kind` string, preferring the local
/// document entry when a CRD entry shares the same `kind` (both a local and a
/// CRD `Module` exist; local documents are what these validators receive).
fn entry_for_kind(kind: &str) -> Option<&'static KindEntry> {
    KIND_REGISTRY
        .iter()
        .find(|e| e.kind == kind && !e.crd)
        .or_else(|| KIND_REGISTRY.iter().find(|e| e.kind == kind))
}

/// Validate a YAML document, reading its `kind` and dispatching to the matching
/// [`KIND_REGISTRY`] entry's validator. An unknown or missing `kind` is an
/// error; the per-kind validator covers unknown fields and `apiVersion`.
pub fn validate_document(yaml: &str) -> ValidationResult {
    let value: serde_yaml::Value = match serde_yaml::from_str(yaml) {
        Ok(v) => v,
        Err(e) => {
            return ValidationResult {
                valid: false,
                errors: vec![format!("YAML syntax error: {e}")],
            };
        }
    };

    let Some(kind) = value.get("kind").and_then(|v| v.as_str()) else {
        return ValidationResult {
            valid: false,
            errors: vec!["document is missing a 'kind' field".to_string()],
        };
    };

    let Some(entry) = entry_for_kind(kind) else {
        return ValidationResult {
            valid: false,
            errors: vec![format!("unknown kind '{kind}'")],
        };
    };

    match (entry.validate_fn)(yaml) {
        Ok(()) => ValidationResult {
            valid: true,
            errors: vec![],
        },
        Err(errors) => ValidationResult {
            valid: false,
            errors,
        },
    }
}

/// Validate YAML against an expected [`SchemaKind`], preserving the
/// expected-vs-found kind diagnostic for callers that already know which
/// kind they want. Deserialization is delegated to the unified
/// [`KIND_REGISTRY`], so there is one validation implementation across the
/// validate paths.
pub fn validate_yaml(content: &str, kind: SchemaKind) -> ValidationResult {
    let value: serde_yaml::Value = match serde_yaml::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            return ValidationResult {
                valid: false,
                errors: vec![format!("YAML syntax error: {e}")],
            };
        }
    };

    if let Some(doc_kind) = value.get("kind").and_then(|v| v.as_str())
        && doc_kind != kind.as_str()
    {
        return ValidationResult {
            valid: false,
            errors: vec![format!(
                "Expected kind '{}', found '{}'",
                kind.as_str(),
                doc_kind
            )],
        };
    }

    match entry_for_kind(kind.as_str()) {
        Some(entry) => match (entry.validate_fn)(content) {
            Ok(()) => ValidationResult {
                valid: true,
                errors: vec![],
            },
            Err(errors) => ValidationResult {
                valid: false,
                errors,
            },
        },
        None => ValidationResult {
            valid: false,
            errors: vec![format!("unknown kind '{}'", kind.as_str())],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_unknown_field_for_configsource() {
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: ConfigSource\nmetadata:\n  name: x\nspec:\n  bogusField: 1\n";
        let r = validate_document(yaml);
        assert!(!r.valid);
        assert!(
            r.errors
                .iter()
                .any(|e| e.to_lowercase().contains("bogusfield")),
            "error must name the unknown field, got: {:?}",
            r.errors
        );
    }

    #[test]
    fn validate_accepts_minimal_module() {
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: m\nspec: {}\n";
        assert!(validate_document(yaml).valid);
    }

    #[test]
    fn validate_document_rejects_unknown_api_version() {
        let yaml = "apiVersion: cfgd.io/v1alpha2\nkind: Module\nmetadata:\n  name: m\nspec: {}\n";
        let r = validate_document(yaml);
        assert!(!r.valid);
        assert!(
            r.errors.iter().any(|e| e.contains("apiVersion")),
            "error must mention apiVersion, got: {:?}",
            r.errors
        );
        assert!(
            r.errors.iter().any(|e| e.contains("cfgd.io/v1alpha1")),
            "error must name the supported version, got: {:?}",
            r.errors
        );
    }

    #[test]
    fn test_validate_valid_module() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Module
metadata:
  name: nvim
spec:
  packages:
    - name: neovim
"#;
        let result = validate_yaml(yaml, SchemaKind::Module);
        assert!(
            result.valid,
            "Expected valid, got errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn test_validate_valid_profile() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  modules: [nvim, tmux]
"#;
        let result = validate_yaml(yaml, SchemaKind::Profile);
        assert!(
            result.valid,
            "Expected valid, got errors: {:?}",
            result.errors
        );
    }

    #[test]
    fn test_validate_invalid_yaml_syntax() {
        let yaml = "not: [valid: yaml: {{";
        let result = validate_yaml(yaml, SchemaKind::Module);
        assert!(!result.valid);
        assert!(!result.errors.is_empty());
    }

    #[test]
    fn test_validate_wrong_kind() {
        let yaml = r#"
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: test
spec: {}
"#;
        let result = validate_yaml(yaml, SchemaKind::Module);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("kind")));
    }

    #[test]
    fn test_validate_missing_api_version() {
        let yaml = r#"
kind: Module
metadata:
  name: test
spec: {}
"#;
        let result = validate_yaml(yaml, SchemaKind::Module);
        assert!(!result.valid);
        assert!(result.errors.iter().any(|e| e.contains("apiVersion")));
    }

    #[test]
    fn validate_document_missing_kind_names_the_missing_field() {
        let yaml = "apiVersion: cfgd.io/v1alpha1\nmetadata:\n  name: x\nspec: {}\n";
        let r = validate_document(yaml);
        assert!(!r.valid, "a document with no kind must be invalid");
        assert!(
            r.errors.iter().any(|e| e.to_lowercase().contains("kind")),
            "error must name the missing 'kind' field, got: {:?}",
            r.errors
        );
    }

    #[test]
    fn validate_document_empty_input_is_clear_error_no_panic() {
        let r = validate_document("");
        assert!(!r.valid, "empty input must be invalid");
        assert!(
            !r.errors.is_empty(),
            "empty input must carry at least one error, got: {:?}",
            r.errors
        );
        // An empty document parses to YAML null, which has no `kind`.
        assert!(
            r.errors.iter().any(|e| e.to_lowercase().contains("kind")),
            "empty-input error must name the missing 'kind' field, got: {:?}",
            r.errors
        );
    }

    #[test]
    fn validate_document_multi_document_stream_is_yaml_syntax_error() {
        // serde_yaml rejects a stream of more than one document; the registry
        // surfaces that as a YAML syntax error rather than panicking.
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: a\nspec: {}\n---\napiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: b\nspec: {}\n";
        let r = validate_document(yaml);
        assert!(!r.valid, "a multi-document stream must be invalid");
        assert!(
            r.errors.iter().any(|e| e.contains("YAML syntax error")),
            "multi-doc stream must report a YAML syntax error, got: {:?}",
            r.errors
        );
    }
}
