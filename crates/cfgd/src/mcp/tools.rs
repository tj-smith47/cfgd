use serde_json::{Value, json};

/// Return MCP tool definitions for tools/list response.
pub fn list() -> Value {
    let tools: Vec<Value> = crate::ai::tools::tool_definitions()
        .into_iter()
        .map(|t| {
            json!({
                "name": format!("cfgd_{}", t.name),
                "description": t.description,
                "inputSchema": t.input_schema,
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// Strip the `cfgd_` prefix from a tool name for dispatch to ai::tools.
pub fn strip_prefix(name: &str) -> Option<&str> {
    name.strip_prefix("cfgd_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tools_list_not_empty() {
        let result = list();
        let tools = result["tools"].as_array().unwrap();
        assert!(!tools.is_empty());
    }

    #[test]
    fn test_tools_have_cfgd_prefix() {
        let result = list();
        let tools = result["tools"].as_array().unwrap();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            assert!(
                name.starts_with("cfgd_"),
                "tool '{}' does not have cfgd_ prefix",
                name
            );
        }
    }

    #[test]
    fn test_tools_derived_from_ai_definitions() {
        // Verify names match ai::tools with cfgd_ prefix applied
        let result = list();
        let mcp_tools = result["tools"].as_array().unwrap();
        let ai_defs = crate::ai::tools::tool_definitions();
        assert_eq!(mcp_tools.len(), ai_defs.len());
        for (mcp, ai) in mcp_tools.iter().zip(ai_defs.iter()) {
            let expected_name = format!("cfgd_{}", ai.name);
            assert_eq!(mcp["name"].as_str().unwrap(), expected_name);
            assert_eq!(mcp["description"].as_str().unwrap(), ai.description);
        }
    }

    #[test]
    fn test_strip_prefix() {
        assert_eq!(
            strip_prefix("cfgd_detect_platform"),
            Some("detect_platform")
        );
        assert_eq!(
            strip_prefix("cfgd_scan_installed_packages"),
            Some("scan_installed_packages")
        );
        assert_eq!(strip_prefix("detect_platform"), None);
        assert_eq!(strip_prefix(""), None);
        assert_eq!(strip_prefix("cfgd_"), Some(""));
    }
}
