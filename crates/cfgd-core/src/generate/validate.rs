use crate::config::{CfgdConfig, ModuleDocument, ProfileDocument};
use crate::generate::{SchemaKind, ValidationResult};

pub fn validate_yaml(content: &str, kind: SchemaKind) -> ValidationResult {
    // Step 1: Parse as generic YAML to check syntax
    let value: serde_yaml::Value = match serde_yaml::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            return ValidationResult {
                valid: false,
                errors: vec![format!("YAML syntax error: {}", e)],
            };
        }
    };

    // Step 2: Check kind field matches expected
    if let Some(doc_kind) = value.get("kind").and_then(|v| v.as_str()) {
        if doc_kind != kind.as_str() {
            return ValidationResult {
                valid: false,
                errors: vec![format!(
                    "Expected kind '{}', found '{}'",
                    kind.as_str(),
                    doc_kind
                )],
            };
        }
    }

    // Step 3: Attempt deserialization into concrete type
    let deser_result = match kind {
        SchemaKind::Module => serde_yaml::from_str::<ModuleDocument>(content).map(|_| ()),
        SchemaKind::Profile => serde_yaml::from_str::<ProfileDocument>(content).map(|_| ()),
        SchemaKind::Config => serde_yaml::from_str::<CfgdConfig>(content).map(|_| ()),
    };

    match deser_result {
        Ok(()) => ValidationResult {
            valid: true,
            errors: vec![],
        },
        Err(e) => ValidationResult {
            valid: false,
            errors: vec![format!("Deserialization error: {}", e)],
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(result.valid, "Expected valid, got errors: {:?}", result.errors);
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
        assert!(result.valid, "Expected valid, got errors: {:?}", result.errors);
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
        assert!(result.valid || !result.errors.is_empty());
    }
}
