// Placeholder — real implementation comes in Task 5 (YAML validation).

use crate::generate::{SchemaKind, ValidationResult};

pub fn validate_yaml(_content: &str, _kind: SchemaKind) -> ValidationResult {
    ValidationResult {
        valid: true,
        errors: vec![],
    }
}
