use serde_json::{Value, json};

/// Return MCP prompt definitions for prompts/list response.
pub fn list() -> Value {
    json!({
        "prompts": [
            {
                "name": "cfgd_generate",
                "description": "AI-guided configuration generation",
                "arguments": [
                    { "name": "mode", "description": "full, module, or profile", "required": false },
                    { "name": "name", "description": "Target name (for module/profile modes)", "required": false }
                ]
            },
            {
                "name": "cfgd_generate_module",
                "description": "Generate a cfgd module for a specific tool",
                "arguments": [
                    { "name": "name", "description": "Tool name to generate module for", "required": true }
                ]
            },
            {
                "name": "cfgd_generate_profile",
                "description": "Generate a cfgd profile",
                "arguments": [
                    { "name": "name", "description": "Profile name to generate", "required": true }
                ]
            },
        ]
    })
}

/// Get an MCP prompt by name, returning a prompts/get response.
pub fn get(name: &str, arguments: &Value) -> Value {
    let skill = crate::generate::GENERATE_SKILL;

    let mode_context = match name {
        "cfgd_generate" => {
            let mode = arguments.get("mode").and_then(|v| v.as_str()).unwrap_or("full");
            let target_name = arguments.get("name").and_then(|v| v.as_str()).unwrap_or("");
            match mode {
                "module" => format!("Mode: module — generate module for '{}'.", target_name),
                "profile" => format!("Mode: profile — generate profile '{}'.", target_name),
                _ => "Mode: full — scan system, propose structure, generate all.".into(),
            }
        }
        "cfgd_generate_module" => {
            let n = arguments.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
            format!("Mode: module — generate module for '{}'.", n)
        }
        "cfgd_generate_profile" => {
            let n = arguments.get("name").and_then(|v| v.as_str()).unwrap_or("unknown");
            format!("Mode: profile — generate profile '{}'.", n)
        }
        _ => {
            return json!({ "messages": [] });
        }
    };

    json!({
        "messages": [
            {
                "role": "user",
                "content": {
                    "type": "text",
                    "text": format!("{}\n\n## Current Session\n\n{}", skill, mode_context)
                }
            }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompts_list_has_3_entries() {
        let result = list();
        let prompts = result["prompts"].as_array().unwrap();
        assert_eq!(prompts.len(), 3);
    }

    #[test]
    fn test_get_generate_full() {
        let result = get("cfgd_generate", &json!({}));
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        let text = messages[0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("full"));
        assert_eq!(messages[0]["role"].as_str().unwrap(), "user");
    }

    #[test]
    fn test_get_generate_module_mode() {
        let result = get("cfgd_generate", &json!({"mode": "module", "name": "nvim"}));
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        let text = messages[0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("nvim"));
        assert!(text.contains("module"));
    }

    #[test]
    fn test_get_generate_module() {
        let result = get("cfgd_generate_module", &json!({"name": "tmux"}));
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        let text = messages[0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("tmux"));
    }

    #[test]
    fn test_get_generate_profile() {
        let result = get("cfgd_generate_profile", &json!({"name": "workstation"}));
        let messages = result["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        let text = messages[0]["content"]["text"].as_str().unwrap();
        assert!(text.contains("workstation"));
        assert!(text.contains("profile"));
    }

    #[test]
    fn test_get_unknown_prompt() {
        let result = get("cfgd_nonexistent", &json!({}));
        let messages = result["messages"].as_array().unwrap();
        assert!(messages.is_empty());
    }
}
