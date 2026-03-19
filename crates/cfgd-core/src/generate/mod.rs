pub mod schema;
pub mod session;
pub mod validate;

use serde::{Deserialize, Serialize};

/// Kinds of cfgd documents that can be generated/validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaKind {
    Module,
    Profile,
    Config,
}

impl std::str::FromStr for SchemaKind {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "module" => Ok(Self::Module),
            "profile" => Ok(Self::Profile),
            "config" => Ok(Self::Config),
            _ => Err(format!("unknown schema kind: {}", s)),
        }
    }
}

impl SchemaKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Module => "Module",
            Self::Profile => "Profile",
            Self::Config => "Config",
        }
    }
}

/// Request to present YAML for user review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresentYamlRequest {
    pub content: String,
    pub kind: String,
    pub description: String,
}

/// User's response to a YAML presentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action")]
pub enum PresentYamlResponse {
    #[serde(rename = "accept")]
    Accept,
    #[serde(rename = "reject")]
    Reject,
    #[serde(rename = "feedback")]
    Feedback { message: String },
    #[serde(rename = "stepThrough")]
    StepThrough,
}

/// Result of YAML validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationResult {
    pub valid: bool,
    pub errors: Vec<String>,
}
