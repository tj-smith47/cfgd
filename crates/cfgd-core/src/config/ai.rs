use serde::{Deserialize, Serialize};

/// `spec.ai`: settings for `cfgd generate`'s AI-guided session.
///
/// ```yaml
/// ai:
///   provider: claude
///   model: claude-sonnet-5
///   apiKeyEnv: ANTHROPIC_API_KEY
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AiConfig {
    /// The AI provider name. Default: `claude`.
    #[serde(default = "default_ai_provider")]
    pub provider: String,
    /// The model identifier to request. Default: `claude-sonnet-5`.
    #[serde(default = "default_ai_model")]
    pub model: String,
    /// Name of the environment variable holding the API key. Default: `ANTHROPIC_API_KEY`.
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: default_ai_provider(),
            model: default_ai_model(),
            api_key_env: default_api_key_env(),
        }
    }
}

fn default_ai_provider() -> String {
    "claude".into()
}
fn default_ai_model() -> String {
    "claude-sonnet-5".into()
}
fn default_api_key_env() -> String {
    "ANTHROPIC_API_KEY".into()
}
