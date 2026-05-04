use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConfig {
    #[serde(default = "default_ai_provider")]
    pub provider: String,
    #[serde(default = "default_ai_model")]
    pub model: String,
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
    "claude-sonnet-4-6".into()
}
fn default_api_key_env() -> String {
    "ANTHROPIC_API_KEY".into()
}
