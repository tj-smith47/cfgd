use serde_json::{Value, json};

pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Return MCP tool definitions for tools/list response.
pub fn list() -> Value {
    let tools: Vec<Value> = tool_entries()
        .into_iter()
        .map(|t| json!({
            "name": t.name,
            "description": t.description,
            "inputSchema": t.input_schema,
        }))
        .collect();
    json!({ "tools": tools })
}

/// Strip the `cfgd_` prefix from a tool name for dispatch to ai::tools.
pub fn strip_prefix(name: &str) -> Option<&str> {
    name.strip_prefix("cfgd_")
}

fn tool_entries() -> Vec<McpTool> {
    vec![
        McpTool {
            name: "cfgd_scan_installed_packages".into(),
            description: "List installed packages across all available package managers. Returns package name, version, and which manager owns it.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "manager": {
                        "type": "string",
                        "description": "Optional: filter to a specific package manager name (e.g. 'brew', 'apt', 'cargo')"
                    }
                }
            }),
        },
        McpTool {
            name: "cfgd_scan_dotfiles".into(),
            description: "Scan the home directory for dotfiles and XDG config entries. Returns file paths, sizes, types, and guessed tool names.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "home": {
                        "type": "string",
                        "description": "Optional: override home directory path"
                    }
                }
            }),
        },
        McpTool {
            name: "cfgd_scan_shell_config".into(),
            description: "Parse shell RC files to extract aliases, exports, PATH additions, sourced files, and plugin manager detection.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "shell": {
                        "type": "string",
                        "description": "Shell name: 'zsh', 'bash', 'fish', or 'sh'"
                    }
                },
                "required": ["shell"]
            }),
        },
        McpTool {
            name: "cfgd_scan_system_settings".into(),
            description: "Scan platform-specific system settings: macOS defaults domains, systemd user units, and LaunchAgents.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        McpTool {
            name: "cfgd_detect_platform".into(),
            description: "Detect the current platform: OS (linux/macos/freebsd), distro, version, and CPU architecture.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        McpTool {
            name: "cfgd_inspect_tool".into(),
            description: "Inspect an installed tool: detect its version, locate config files, and identify plugin systems.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Tool name to inspect (e.g. 'nvim', 'tmux', 'zsh')"
                    }
                },
                "required": ["name"]
            }),
        },
        McpTool {
            name: "cfgd_query_package_manager".into(),
            description: "Query a specific package manager for information about a package: availability, version, and aliases.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "manager": {
                        "type": "string",
                        "description": "Package manager name (e.g. 'brew', 'apt', 'cargo')"
                    },
                    "package": {
                        "type": "string",
                        "description": "Package name to query"
                    }
                },
                "required": ["manager", "package"]
            }),
        },
        McpTool {
            name: "cfgd_read_file".into(),
            description: "Read a file's contents. Restricted to files within the home directory or config repo. Sensitive files (SSH keys, secrets) are blocked. Large files are truncated.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file to read"
                    }
                },
                "required": ["path"]
            }),
        },
        McpTool {
            name: "cfgd_list_directory".into(),
            description: "List entries in a directory. Restricted to directories within the home directory or config repo.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the directory to list"
                    }
                },
                "required": ["path"]
            }),
        },
        McpTool {
            name: "cfgd_adopt_files".into(),
            description: "Copy config files into the config repo for management. Takes source/destination pairs.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "description": "Array of {source, dest} objects. source is absolute path, dest is relative path within repo.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "source": {
                                    "type": "string",
                                    "description": "Absolute source file path"
                                },
                                "dest": {
                                    "type": "string",
                                    "description": "Relative destination path within the config repo"
                                }
                            },
                            "required": ["source", "dest"]
                        }
                    }
                },
                "required": ["files"]
            }),
        },
        McpTool {
            name: "cfgd_get_schema".into(),
            description: "Get the annotated YAML schema for a cfgd document kind. Use this to understand the structure before writing YAML.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "description": "Document kind: 'Module', 'Profile', or 'Config'"
                    }
                },
                "required": ["kind"]
            }),
        },
        McpTool {
            name: "cfgd_validate_yaml".into(),
            description: "Validate YAML content against the cfgd schema for a given kind. Returns whether the YAML is valid and any errors.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "YAML content to validate"
                    },
                    "kind": {
                        "type": "string",
                        "description": "Document kind: 'Module', 'Profile', or 'Config'"
                    }
                },
                "required": ["content", "kind"]
            }),
        },
        McpTool {
            name: "cfgd_write_module_yaml".into(),
            description: "Write a Module YAML file to the config repo. Validates the YAML before writing. Creates modules/<name>/module.yaml.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Module name (used as directory name)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Full YAML content for the module"
                    }
                },
                "required": ["name", "content"]
            }),
        },
        McpTool {
            name: "cfgd_write_profile_yaml".into(),
            description: "Write a Profile YAML file to the config repo. Validates the YAML before writing. Creates profiles/<name>.yaml.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Profile name (used as filename without .yaml extension)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Full YAML content for the profile"
                    }
                },
                "required": ["name", "content"]
            }),
        },
        McpTool {
            name: "cfgd_list_generated".into(),
            description: "List all modules and profiles generated so far in this session.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        McpTool {
            name: "cfgd_get_existing_modules".into(),
            description: "List module names that already exist in the config repo (modules/<name>/module.yaml).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        McpTool {
            name: "cfgd_get_existing_profiles".into(),
            description: "List profile names that already exist in the config repo (profiles/<name>.yaml).".into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        },
        McpTool {
            name: "cfgd_present_yaml".into(),
            description: "Present generated YAML to the user for review. The user can accept, reject, or provide feedback. Use this before writing any YAML file.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "content": {
                        "type": "string",
                        "description": "YAML content to present"
                    },
                    "kind": {
                        "type": "string",
                        "description": "Document kind: 'Module', 'Profile', or 'Config'"
                    },
                    "description": {
                        "type": "string",
                        "description": "Human-readable description of what this YAML configures"
                    }
                },
                "required": ["content", "kind", "description"]
            }),
        },
    ]
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
    fn test_tools_count() {
        let result = list();
        let tools = result["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 18, "expected 18 MCP tool definitions");
    }

    #[test]
    fn test_strip_prefix() {
        assert_eq!(strip_prefix("cfgd_detect_platform"), Some("detect_platform"));
        assert_eq!(strip_prefix("cfgd_scan_installed_packages"), Some("scan_installed_packages"));
        assert_eq!(strip_prefix("detect_platform"), None);
        assert_eq!(strip_prefix(""), None);
        assert_eq!(strip_prefix("cfgd_"), Some(""));
    }
}
