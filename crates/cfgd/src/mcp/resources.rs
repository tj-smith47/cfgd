use serde_json::{Value, json};

/// Return MCP resource definitions for resources/list response.
pub fn list() -> Value {
    json!({
        "resources": [
            {
                "uri": "cfgd://skill/generate",
                "name": "Generate Orchestration Skill",
                "description": "System prompt for AI-guided configuration generation",
                "mimeType": "text/markdown"
            },
            {
                "uri": "cfgd://schema/module",
                "name": "Module Schema",
                "description": "Annotated YAML schema for cfgd Module resources",
                "mimeType": "text/yaml"
            },
            {
                "uri": "cfgd://schema/profile",
                "name": "Profile Schema",
                "description": "Annotated YAML schema for cfgd Profile resources",
                "mimeType": "text/yaml"
            },
            {
                "uri": "cfgd://schema/config",
                "name": "Config Schema",
                "description": "Annotated YAML schema for cfgd Config resources",
                "mimeType": "text/yaml"
            },
        ]
    })
}

/// Read an MCP resource by URI, returning a resources/read response.
/// Returns `Err` for unknown URIs so the server can emit a JSON-RPC error.
pub fn read(uri: &str) -> Result<Value, String> {
    let contents: Vec<Value> = match uri {
        "cfgd://skill/generate" => vec![json!({
            "uri": uri,
            "mimeType": "text/markdown",
            "text": crate::generate::GENERATE_SKILL
        })],
        "cfgd://schema/module" => vec![json!({
            "uri": uri,
            "mimeType": "text/yaml",
            "text": cfgd_core::generate::schema::get_schema(cfgd_core::generate::SchemaKind::Module)
        })],
        "cfgd://schema/profile" => vec![json!({
            "uri": uri,
            "mimeType": "text/yaml",
            "text": cfgd_core::generate::schema::get_schema(cfgd_core::generate::SchemaKind::Profile)
        })],
        "cfgd://schema/config" => vec![json!({
            "uri": uri,
            "mimeType": "text/yaml",
            "text": cfgd_core::generate::schema::get_schema(cfgd_core::generate::SchemaKind::Config)
        })],
        _ => return Err(format!("Resource not found: {}", uri)),
    };
    Ok(json!({ "contents": contents }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resources_list_has_4_entries() {
        let result = list();
        let resources = result["resources"].as_array().unwrap();
        assert_eq!(resources.len(), 4);
    }

    #[test]
    fn test_read_skill_resource() {
        let result = read("cfgd://skill/generate").unwrap();
        let contents = result["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        let text = contents[0]["text"].as_str().unwrap();
        assert!(!text.is_empty());
        assert_eq!(contents[0]["mimeType"].as_str().unwrap(), "text/markdown");
    }

    #[test]
    fn test_read_module_schema() {
        let result = read("cfgd://schema/module").unwrap();
        let contents = result["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        let text = contents[0]["text"].as_str().unwrap();
        assert!(text.contains("apiVersion"));
        assert_eq!(contents[0]["mimeType"].as_str().unwrap(), "text/yaml");
    }

    #[test]
    fn test_read_profile_schema() {
        let result = read("cfgd://schema/profile").unwrap();
        let contents = result["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        let text = contents[0]["text"].as_str().unwrap();
        assert!(text.contains("Profile"));
    }

    #[test]
    fn test_read_config_schema() {
        let result = read("cfgd://schema/config").unwrap();
        let contents = result["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        let text = contents[0]["text"].as_str().unwrap();
        assert!(text.contains("Config"));
    }

    #[test]
    fn test_read_unknown_resource_returns_error() {
        let result = read("cfgd://unknown/resource");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Resource not found"));
    }
}
