use std::path::{Path, PathBuf};

use serde_json::Value;

use cfgd_core::generate::SchemaKind;
use cfgd_core::generate::session::GenerateSession;
use cfgd_core::platform::Platform;
use cfgd_core::providers::PackageManager;

use crate::ai::client::ToolDefinition;
use crate::generate::{files, inspect, scan};

/// Result of dispatching a tool call.
pub struct ToolCallResult {
    pub content: String,
    pub is_error: bool,
}

/// Execute a tool call by name, returning the result as a JSON string.
pub fn dispatch_tool_call(
    name: &str,
    input: &Value,
    session: &mut GenerateSession,
    home: &Path,
    managers: &[Box<dyn PackageManager>],
) -> ToolCallResult {
    match name {
        "scan_installed_packages" => dispatch_scan_installed_packages(input, managers),
        "scan_dotfiles" => dispatch_scan_dotfiles(input, home),
        "scan_shell_config" => dispatch_scan_shell_config(input, home),
        "scan_system_settings" => dispatch_scan_system_settings(),
        "detect_platform" => dispatch_detect_platform(),
        "inspect_tool" => dispatch_inspect_tool(input, home),
        "query_package_manager" => dispatch_query_package_manager(input, managers),
        "read_file" => dispatch_read_file(input, home, session.repo_root()),
        "list_directory" => dispatch_list_directory(input, home, session.repo_root()),
        "adopt_files" => dispatch_adopt_files(input, session.repo_root()),
        "get_schema" => dispatch_get_schema(input),
        "validate_yaml" => dispatch_validate_yaml(input),
        "write_module_yaml" => dispatch_write_module_yaml(input, session),
        "write_profile_yaml" => dispatch_write_profile_yaml(input, session),
        "list_generated" => dispatch_list_generated(session),
        "get_existing_modules" => dispatch_get_existing_modules(session),
        "get_existing_profiles" => dispatch_get_existing_profiles(session),
        // present_yaml is handled specially by the conversation loop, not here
        _ => ToolCallResult {
            content: format!("Unknown tool: {}", name),
            is_error: true,
        },
    }
}

/// Return all tool definitions for the Anthropic API.
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "scan_installed_packages".into(),
            description: "List installed packages across all available package managers. Returns package name, version, and which manager owns it.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "manager": {
                        "type": "string",
                        "description": "Optional: filter to a specific package manager name (e.g. 'brew', 'apt', 'cargo')"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "scan_dotfiles".into(),
            description: "Scan the home directory for dotfiles and XDG config entries. Returns file paths, sizes, types, and guessed tool names.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "home": {
                        "type": "string",
                        "description": "Optional: override home directory path"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "scan_shell_config".into(),
            description: "Parse shell RC files to extract aliases, exports, PATH additions, sourced files, and plugin manager detection.".into(),
            input_schema: serde_json::json!({
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
        ToolDefinition {
            name: "scan_system_settings".into(),
            description: "Scan platform-specific system settings: macOS defaults domains, systemd user units, and LaunchAgents.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "detect_platform".into(),
            description: "Detect the current platform: OS (linux/macos/freebsd), distro, version, and CPU architecture.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "inspect_tool".into(),
            description: "Inspect an installed tool: detect its version, locate config files, and identify plugin systems.".into(),
            input_schema: serde_json::json!({
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
        ToolDefinition {
            name: "query_package_manager".into(),
            description: "Query a specific package manager for information about a package: availability, version, and aliases.".into(),
            input_schema: serde_json::json!({
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
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a file's contents. Restricted to files within the home directory or config repo. Sensitive files (SSH keys, secrets) are blocked. Large files are truncated.".into(),
            input_schema: serde_json::json!({
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
        ToolDefinition {
            name: "list_directory".into(),
            description: "List entries in a directory. Restricted to directories within the home directory or config repo.".into(),
            input_schema: serde_json::json!({
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
        ToolDefinition {
            name: "adopt_files".into(),
            description: "Copy config files into the config repo for management. Takes source/destination pairs.".into(),
            input_schema: serde_json::json!({
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
        ToolDefinition {
            name: "get_schema".into(),
            description: "Get the annotated YAML schema for a cfgd document kind. Use this to understand the structure before writing YAML.".into(),
            input_schema: serde_json::json!({
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
        ToolDefinition {
            name: "validate_yaml".into(),
            description: "Validate YAML content against the cfgd schema for a given kind. Returns whether the YAML is valid and any errors.".into(),
            input_schema: serde_json::json!({
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
        ToolDefinition {
            name: "write_module_yaml".into(),
            description: "Write a Module YAML file to the config repo. Validates the YAML before writing. Creates modules/<name>/module.yaml.".into(),
            input_schema: serde_json::json!({
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
        ToolDefinition {
            name: "write_profile_yaml".into(),
            description: "Write a Profile YAML file to the config repo. Validates the YAML before writing. Creates profiles/<name>.yaml.".into(),
            input_schema: serde_json::json!({
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
        ToolDefinition {
            name: "list_generated".into(),
            description: "List all modules and profiles generated so far in this session.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "get_existing_modules".into(),
            description: "List module names that already exist in the config repo (modules/<name>/module.yaml).".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "get_existing_profiles".into(),
            description: "List profile names that already exist in the config repo (profiles/<name>.yaml).".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
        ToolDefinition {
            name: "present_yaml".into(),
            description: "Present generated YAML to the user for review. The user can accept, reject, or provide feedback. Use this before writing any YAML file.".into(),
            input_schema: serde_json::json!({
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

// ---------------------------------------------------------------------------
// Dispatch functions
// ---------------------------------------------------------------------------

fn dispatch_scan_installed_packages(
    input: &Value,
    managers: &[Box<dyn PackageManager>],
) -> ToolCallResult {
    let filter_manager = input.get("manager").and_then(|v| v.as_str());
    let refs: Vec<&dyn PackageManager> = managers.iter().map(|m| m.as_ref()).collect();
    match scan::scan_installed_packages(&refs, filter_manager) {
        Ok(entries) => ToolCallResult {
            content: serde_json::to_string(&entries).unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_scan_dotfiles(input: &Value, home: &Path) -> ToolCallResult {
    let home_override = input
        .get("home")
        .and_then(|v| v.as_str())
        .map(PathBuf::from);
    let home_path = home_override.as_deref().unwrap_or(home);
    match scan::scan_dotfiles(home_path) {
        Ok(entries) => ToolCallResult {
            content: serde_json::to_string(&entries).unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_scan_shell_config(input: &Value, home: &Path) -> ToolCallResult {
    let shell = match input.get("shell").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ToolCallResult {
                content: "Error: 'shell' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    match scan::scan_shell_config(shell, home) {
        Ok(result) => ToolCallResult {
            content: serde_json::to_string(&result).unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_scan_system_settings() -> ToolCallResult {
    match scan::scan_system_settings() {
        Ok(result) => ToolCallResult {
            content: serde_json::to_string(&result).unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_detect_platform() -> ToolCallResult {
    let platform = Platform::detect();
    let value = serde_json::json!({
        "os": format!("{:?}", platform.os),
        "distro": format!("{:?}", platform.distro),
        "version": platform.version,
        "arch": format!("{:?}", platform.arch),
    });
    ToolCallResult {
        content: serde_json::to_string(&value).unwrap_or_default(),
        is_error: false,
    }
}

fn dispatch_inspect_tool(input: &Value, home: &Path) -> ToolCallResult {
    let name = match input.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return ToolCallResult {
                content: "Error: 'name' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    match inspect::inspect_tool(name, home) {
        Ok(result) => ToolCallResult {
            content: serde_json::to_string(&result).unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_query_package_manager(
    input: &Value,
    managers: &[Box<dyn PackageManager>],
) -> ToolCallResult {
    let manager_name = match input.get("manager").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return ToolCallResult {
                content: "Error: 'manager' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    let package = match input.get("package").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ToolCallResult {
                content: "Error: 'package' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    let manager = match managers.iter().find(|m| m.name() == manager_name) {
        Some(m) => m,
        None => {
            return ToolCallResult {
                content: format!("Error: package manager '{}' not found", manager_name),
                is_error: true,
            };
        }
    };
    match inspect::query_package_manager(manager.as_ref(), package) {
        Ok(result) => ToolCallResult {
            content: serde_json::to_string(&result).unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_read_file(input: &Value, home: &Path, repo_root: &Path) -> ToolCallResult {
    let path = match input.get("path").and_then(|v| v.as_str()) {
        Some(p) => PathBuf::from(p),
        None => {
            return ToolCallResult {
                content: "Error: 'path' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    match files::read_file(&path, home, repo_root) {
        Ok(result) => ToolCallResult {
            content: serde_json::to_string(&result).unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_list_directory(input: &Value, home: &Path, repo_root: &Path) -> ToolCallResult {
    let path = match input.get("path").and_then(|v| v.as_str()) {
        Some(p) => PathBuf::from(p),
        None => {
            return ToolCallResult {
                content: "Error: 'path' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    match files::list_directory(&path, home, repo_root) {
        Ok(entries) => ToolCallResult {
            content: serde_json::to_string(&entries).unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_adopt_files(input: &Value, repo_root: &Path) -> ToolCallResult {
    let files_arr = match input.get("files").and_then(|v| v.as_array()) {
        Some(arr) => arr,
        None => {
            return ToolCallResult {
                content: "Error: 'files' parameter is required and must be an array".to_string(),
                is_error: true,
            };
        }
    };

    let mut pairs: Vec<(PathBuf, PathBuf)> = Vec::new();
    for item in files_arr {
        let source = match item.get("source").and_then(|v| v.as_str()) {
            Some(s) => PathBuf::from(s),
            None => {
                return ToolCallResult {
                    content: "Error: each file entry requires a 'source' string".to_string(),
                    is_error: true,
                };
            }
        };
        let dest = match item.get("dest").and_then(|v| v.as_str()) {
            Some(d) => PathBuf::from(d),
            None => {
                return ToolCallResult {
                    content: "Error: each file entry requires a 'dest' string".to_string(),
                    is_error: true,
                };
            }
        };
        pairs.push((source, dest));
    }

    match files::adopt_files(&pairs, repo_root) {
        Ok(written) => {
            let paths: Vec<String> = written.iter().map(|p| p.display().to_string()).collect();
            ToolCallResult {
                content: serde_json::to_string(&paths).unwrap_or_default(),
                is_error: false,
            }
        }
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_get_schema(input: &Value) -> ToolCallResult {
    let kind_str = match input.get("kind").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => {
            return ToolCallResult {
                content: "Error: 'kind' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    let kind: SchemaKind = match kind_str.parse() {
        Ok(k) => k,
        Err(e) => {
            return ToolCallResult {
                content: format!("Error: {}", e),
                is_error: true,
            };
        }
    };
    let schema = cfgd_core::generate::schema::get_schema(kind);
    ToolCallResult {
        content: schema.to_string(),
        is_error: false,
    }
}

fn dispatch_validate_yaml(input: &Value) -> ToolCallResult {
    let content = match input.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ToolCallResult {
                content: "Error: 'content' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    let kind_str = match input.get("kind").and_then(|v| v.as_str()) {
        Some(k) => k,
        None => {
            return ToolCallResult {
                content: "Error: 'kind' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    let kind: SchemaKind = match kind_str.parse() {
        Ok(k) => k,
        Err(e) => {
            return ToolCallResult {
                content: format!("Error: {}", e),
                is_error: true,
            };
        }
    };
    let result = cfgd_core::generate::validate::validate_yaml(content, kind);
    ToolCallResult {
        content: serde_json::to_string(&result).unwrap_or_default(),
        is_error: false,
    }
}

fn dispatch_write_module_yaml(input: &Value, session: &mut GenerateSession) -> ToolCallResult {
    let name = match input.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return ToolCallResult {
                content: "Error: 'name' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    let content = match input.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ToolCallResult {
                content: "Error: 'content' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    match session.write_module_yaml(name, content) {
        Ok(path) => ToolCallResult {
            content: serde_json::json!({"path": path.display().to_string()}).to_string(),
            is_error: false,
        },
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_write_profile_yaml(input: &Value, session: &mut GenerateSession) -> ToolCallResult {
    let name = match input.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return ToolCallResult {
                content: "Error: 'name' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    let content = match input.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ToolCallResult {
                content: "Error: 'content' parameter is required".to_string(),
                is_error: true,
            };
        }
    };
    match session.write_profile_yaml(name, content) {
        Ok(path) => ToolCallResult {
            content: serde_json::json!({"path": path.display().to_string()}).to_string(),
            is_error: false,
        },
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_list_generated(session: &GenerateSession) -> ToolCallResult {
    let items = session.list_generated();
    let entries: Vec<serde_json::Value> = items
        .iter()
        .map(|item| {
            serde_json::json!({
                "kind": item.kind.as_str(),
                "name": item.name,
                "path": item.path.display().to_string(),
            })
        })
        .collect();
    ToolCallResult {
        content: serde_json::to_string(&entries).unwrap_or_default(),
        is_error: false,
    }
}

fn dispatch_get_existing_modules(session: &GenerateSession) -> ToolCallResult {
    match session.get_existing_modules() {
        Ok(names) => ToolCallResult {
            content: serde_json::to_string(&names).unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_get_existing_profiles(session: &GenerateSession) -> ToolCallResult {
    match session.get_existing_profiles() {
        Ok(names) => ToolCallResult {
            content: serde_json::to_string(&names).unwrap_or_default(),
            is_error: false,
        },
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dispatch_unknown_tool() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let result = dispatch_tool_call(
            "nonexistent",
            &Value::Null,
            &mut session,
            Path::new("/"),
            &[],
        );
        assert!(result.is_error);
        assert!(result.content.contains("Unknown tool"));
    }

    #[test]
    fn test_dispatch_get_schema() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"kind": "Module"});
        let result = dispatch_tool_call("get_schema", &input, &mut session, Path::new("/"), &[]);
        assert!(!result.is_error);
        assert!(result.content.contains("apiVersion"));
    }

    #[test]
    fn test_dispatch_get_schema_profile() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"kind": "Profile"});
        let result = dispatch_tool_call("get_schema", &input, &mut session, Path::new("/"), &[]);
        assert!(!result.is_error);
        assert!(result.content.contains("kind: Profile"));
    }

    #[test]
    fn test_dispatch_get_schema_config() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"kind": "Config"});
        let result = dispatch_tool_call("get_schema", &input, &mut session, Path::new("/"), &[]);
        assert!(!result.is_error);
        assert!(result.content.contains("kind: Config"));
    }

    #[test]
    fn test_dispatch_get_schema_invalid_kind() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"kind": "InvalidKind"});
        let result = dispatch_tool_call("get_schema", &input, &mut session, Path::new("/"), &[]);
        assert!(result.is_error);
        assert!(result.content.contains("unknown schema kind"));
    }

    #[test]
    fn test_dispatch_get_schema_missing_kind() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({});
        let result = dispatch_tool_call("get_schema", &input, &mut session, Path::new("/"), &[]);
        assert!(result.is_error);
        assert!(result.content.contains("'kind' parameter is required"));
    }

    #[test]
    fn test_dispatch_validate_yaml_valid() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let yaml =
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test\nspec: {}\n";
        let input = serde_json::json!({"content": yaml, "kind": "Module"});
        let result = dispatch_tool_call("validate_yaml", &input, &mut session, Path::new("/"), &[]);
        assert!(!result.is_error);
        assert!(result.content.contains("true"));
    }

    #[test]
    fn test_dispatch_validate_yaml_invalid() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"content": "not valid yaml {{", "kind": "Module"});
        let result = dispatch_tool_call("validate_yaml", &input, &mut session, Path::new("/"), &[]);
        assert!(!result.is_error); // validate_yaml itself returns a result struct, not an error
        assert!(result.content.contains("false"));
    }

    #[test]
    fn test_dispatch_validate_yaml_missing_content() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"kind": "Module"});
        let result = dispatch_tool_call("validate_yaml", &input, &mut session, Path::new("/"), &[]);
        assert!(result.is_error);
        assert!(result.content.contains("'content' parameter is required"));
    }

    #[test]
    fn test_dispatch_scan_dotfiles() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".zshrc"), "# config").unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let input = serde_json::json!({});
        let result = dispatch_tool_call("scan_dotfiles", &input, &mut session, tmp.path(), &[]);
        assert!(!result.is_error);
        assert!(result.content.contains(".zshrc"));
    }

    #[test]
    fn test_dispatch_scan_dotfiles_with_home_override() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".bashrc"), "# bash").unwrap();
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"home": tmp.path().to_str().unwrap()});
        let result = dispatch_tool_call(
            "scan_dotfiles",
            &input,
            &mut session,
            Path::new("/nonexistent"),
            &[],
        );
        assert!(!result.is_error);
        assert!(result.content.contains(".bashrc"));
    }

    #[test]
    fn test_dispatch_scan_shell_config() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            tmp.path().join(".zshrc"),
            "alias ll='ls -la'\nexport EDITOR=nvim\n",
        )
        .unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let input = serde_json::json!({"shell": "zsh"});
        let result = dispatch_tool_call("scan_shell_config", &input, &mut session, tmp.path(), &[]);
        assert!(!result.is_error);
        assert!(result.content.contains("ll"));
        assert!(result.content.contains("EDITOR"));
    }

    #[test]
    fn test_dispatch_scan_shell_config_missing_shell() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({});
        let result = dispatch_tool_call(
            "scan_shell_config",
            &input,
            &mut session,
            Path::new("/"),
            &[],
        );
        assert!(result.is_error);
        assert!(result.content.contains("'shell' parameter is required"));
    }

    #[test]
    fn test_dispatch_scan_system_settings() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({});
        let result = dispatch_tool_call(
            "scan_system_settings",
            &input,
            &mut session,
            Path::new("/"),
            &[],
        );
        assert!(!result.is_error);
    }

    #[test]
    fn test_dispatch_detect_platform() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({});
        let result =
            dispatch_tool_call("detect_platform", &input, &mut session, Path::new("/"), &[]);
        assert!(!result.is_error);
        assert!(result.content.contains("os"));
        assert!(result.content.contains("arch"));
    }

    #[test]
    fn test_dispatch_inspect_tool_missing_name() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({});
        let result = dispatch_tool_call("inspect_tool", &input, &mut session, Path::new("/"), &[]);
        assert!(result.is_error);
        assert!(result.content.contains("'name' parameter is required"));
    }

    #[test]
    fn test_dispatch_inspect_tool() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".zshrc"), "# zsh config").unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let input = serde_json::json!({"name": "zsh"});
        let result = dispatch_tool_call("inspect_tool", &input, &mut session, tmp.path(), &[]);
        assert!(!result.is_error);
        assert!(result.content.contains("zsh"));
    }

    #[test]
    fn test_dispatch_query_package_manager_missing_manager() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"package": "neovim"});
        let result = dispatch_tool_call(
            "query_package_manager",
            &input,
            &mut session,
            Path::new("/"),
            &[],
        );
        assert!(result.is_error);
        assert!(result.content.contains("'manager' parameter is required"));
    }

    #[test]
    fn test_dispatch_query_package_manager_missing_package() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"manager": "brew"});
        let result = dispatch_tool_call(
            "query_package_manager",
            &input,
            &mut session,
            Path::new("/"),
            &[],
        );
        assert!(result.is_error);
        assert!(result.content.contains("'package' parameter is required"));
    }

    #[test]
    fn test_dispatch_query_package_manager_not_found() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"manager": "nonexistent", "package": "vim"});
        let result = dispatch_tool_call(
            "query_package_manager",
            &input,
            &mut session,
            Path::new("/"),
            &[],
        );
        assert!(result.is_error);
        assert!(result.content.contains("not found"));
    }

    #[test]
    fn test_dispatch_read_file_missing_path() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({});
        let result = dispatch_tool_call("read_file", &input, &mut session, Path::new("/"), &[]);
        assert!(result.is_error);
        assert!(result.content.contains("'path' parameter is required"));
    }

    #[test]
    fn test_dispatch_list_directory_missing_path() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({});
        let result =
            dispatch_tool_call("list_directory", &input, &mut session, Path::new("/"), &[]);
        assert!(result.is_error);
        assert!(result.content.contains("'path' parameter is required"));
    }

    #[test]
    fn test_dispatch_read_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("test.txt");
        std::fs::write(&file, "hello world").unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let input = serde_json::json!({"path": file.to_str().unwrap()});
        let result = dispatch_tool_call("read_file", &input, &mut session, tmp.path(), &[]);
        assert!(!result.is_error);
        assert!(result.content.contains("hello world"));
    }

    #[test]
    fn test_dispatch_list_directory() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "").unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let input = serde_json::json!({"path": tmp.path().to_str().unwrap()});
        let result = dispatch_tool_call("list_directory", &input, &mut session, tmp.path(), &[]);
        assert!(!result.is_error);
        assert!(result.content.contains("a.txt"));
        assert!(result.content.contains("b.txt"));
    }

    #[test]
    fn test_dispatch_adopt_files() {
        let src_dir = tempfile::TempDir::new().unwrap();
        let repo_dir = tempfile::TempDir::new().unwrap();
        let src_file = src_dir.path().join("config.toml");
        std::fs::write(&src_file, "key = 'val'").unwrap();

        let mut session = GenerateSession::new(repo_dir.path().to_path_buf());
        let input = serde_json::json!({
            "files": [
                {"source": src_file.to_str().unwrap(), "dest": "tool/config.toml"}
            ]
        });
        let result = dispatch_tool_call("adopt_files", &input, &mut session, src_dir.path(), &[]);
        assert!(!result.is_error);
        assert!(repo_dir.path().join("tool/config.toml").exists());
    }

    #[test]
    fn test_dispatch_adopt_files_missing_files() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({});
        let result = dispatch_tool_call("adopt_files", &input, &mut session, Path::new("/"), &[]);
        assert!(result.is_error);
        assert!(result.content.contains("'files' parameter is required"));
    }

    #[test]
    fn test_dispatch_adopt_files_missing_source() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"files": [{"dest": "out.txt"}]});
        let result = dispatch_tool_call("adopt_files", &input, &mut session, Path::new("/"), &[]);
        assert!(result.is_error);
        assert!(result.content.contains("'source'"));
    }

    #[test]
    fn test_dispatch_adopt_files_missing_dest() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"files": [{"source": "/tmp/x"}]});
        let result = dispatch_tool_call("adopt_files", &input, &mut session, Path::new("/"), &[]);
        assert!(result.is_error);
        assert!(result.content.contains("'dest'"));
    }

    #[test]
    fn test_dispatch_write_module_yaml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test\nspec:\n  packages:\n    - name: test-pkg\n";
        let input = serde_json::json!({"name": "test", "content": yaml});
        let result = dispatch_tool_call("write_module_yaml", &input, &mut session, tmp.path(), &[]);
        assert!(!result.is_error, "Error: {}", result.content);
        assert!(result.content.contains("path"));
        assert!(tmp.path().join("modules/test/module.yaml").exists());
    }

    #[test]
    fn test_dispatch_write_module_yaml_invalid() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let input = serde_json::json!({"name": "bad", "content": "invalid yaml {{"});
        let result = dispatch_tool_call("write_module_yaml", &input, &mut session, tmp.path(), &[]);
        assert!(result.is_error);
    }

    #[test]
    fn test_dispatch_write_module_yaml_missing_name() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"content": "test"});
        let result = dispatch_tool_call(
            "write_module_yaml",
            &input,
            &mut session,
            Path::new("/"),
            &[],
        );
        assert!(result.is_error);
        assert!(result.content.contains("'name' parameter is required"));
    }

    #[test]
    fn test_dispatch_write_profile_yaml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  modules:\n    - test\n";
        let input = serde_json::json!({"name": "base", "content": yaml});
        let result =
            dispatch_tool_call("write_profile_yaml", &input, &mut session, tmp.path(), &[]);
        assert!(!result.is_error, "Error: {}", result.content);
        assert!(result.content.contains("path"));
        assert!(tmp.path().join("profiles/base.yaml").exists());
    }

    #[test]
    fn test_dispatch_write_profile_yaml_missing_content() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({"name": "test"});
        let result = dispatch_tool_call(
            "write_profile_yaml",
            &input,
            &mut session,
            Path::new("/"),
            &[],
        );
        assert!(result.is_error);
        assert!(result.content.contains("'content' parameter is required"));
    }

    #[test]
    fn test_dispatch_list_generated_empty() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let result = dispatch_tool_call(
            "list_generated",
            &Value::Null,
            &mut session,
            Path::new("/"),
            &[],
        );
        assert!(!result.is_error);
        assert_eq!(result.content, "[]");
    }

    #[test]
    fn test_dispatch_list_generated_after_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: nvim\nspec:\n  packages:\n    - name: neovim\n";
        session.write_module_yaml("nvim", yaml).unwrap();
        let result = dispatch_tool_call(
            "list_generated",
            &Value::Null,
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert!(result.content.contains("nvim"));
        assert!(result.content.contains("Module"));
    }

    #[test]
    fn test_dispatch_get_existing_modules() {
        let tmp = tempfile::TempDir::new().unwrap();
        let nvim_dir = tmp.path().join("modules").join("nvim");
        std::fs::create_dir_all(&nvim_dir).unwrap();
        std::fs::write(nvim_dir.join("module.yaml"), "test").unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let result = dispatch_tool_call(
            "get_existing_modules",
            &Value::Null,
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert!(result.content.contains("nvim"));
    }

    #[test]
    fn test_dispatch_get_existing_modules_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let result = dispatch_tool_call(
            "get_existing_modules",
            &Value::Null,
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert_eq!(result.content, "[]");
    }

    #[test]
    fn test_dispatch_get_existing_profiles() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profiles_dir = tmp.path().join("profiles");
        std::fs::create_dir_all(&profiles_dir).unwrap();
        std::fs::write(profiles_dir.join("base.yaml"), "test").unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let result = dispatch_tool_call(
            "get_existing_profiles",
            &Value::Null,
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert!(result.content.contains("base"));
    }

    #[test]
    fn test_dispatch_get_existing_profiles_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());
        let result = dispatch_tool_call(
            "get_existing_profiles",
            &Value::Null,
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert_eq!(result.content, "[]");
    }

    #[test]
    fn test_tool_definitions_not_empty() {
        let defs = tool_definitions();
        assert!(!defs.is_empty());
        for def in &defs {
            assert!(!def.name.is_empty());
            assert!(!def.description.is_empty());
        }
    }

    #[test]
    fn test_tool_definitions_all_have_object_schema() {
        let defs = tool_definitions();
        for def in &defs {
            assert_eq!(
                def.input_schema["type"], "object",
                "tool '{}' input_schema must have type: object",
                def.name
            );
        }
    }

    #[test]
    fn test_tool_definitions_present_yaml_included() {
        let defs = tool_definitions();
        assert!(
            defs.iter().any(|d| d.name == "present_yaml"),
            "present_yaml should be in tool definitions even though it's handled specially"
        );
    }

    #[test]
    fn test_tool_definitions_count() {
        let defs = tool_definitions();
        // 17 dispatch tools + present_yaml = 18
        assert_eq!(defs.len(), 18, "expected 18 tool definitions");
    }

    #[test]
    fn test_dispatch_scan_installed_packages_empty() {
        let mut session = GenerateSession::new(PathBuf::from("/tmp/test"));
        let input = serde_json::json!({});
        let result = dispatch_tool_call(
            "scan_installed_packages",
            &input,
            &mut session,
            Path::new("/"),
            &[],
        );
        assert!(!result.is_error);
        assert_eq!(result.content, "[]");
    }

    // ---------------------------------------------------------------------------
    // Pipeline integration tests — sequential tool call flows
    // ---------------------------------------------------------------------------

    #[test]
    fn test_generate_tool_pipeline_writes_module() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());

        // Step 1: AI calls get_schema to learn Module format
        let result = dispatch_tool_call(
            "get_schema",
            &serde_json::json!({"kind": "Module"}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert!(result.content.contains("apiVersion"));

        // Step 2: AI calls validate_yaml to check its generated YAML
        let module_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: git\nspec:\n  packages:\n    - name: git\n";
        let result = dispatch_tool_call(
            "validate_yaml",
            &serde_json::json!({"content": module_yaml, "kind": "Module"}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert!(result.content.contains("true"));

        // Step 3: AI calls write_module_yaml
        let result = dispatch_tool_call(
            "write_module_yaml",
            &serde_json::json!({"name": "git", "content": module_yaml}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);

        // Step 4: Verify file was written
        let module_path = tmp.path().join("modules/git/module.yaml");
        assert!(module_path.exists());
        assert_eq!(
            std::fs::read_to_string(&module_path).unwrap(),
            module_yaml
        );

        // Step 5: AI calls list_generated to see what it wrote
        let result = dispatch_tool_call(
            "list_generated",
            &serde_json::json!({}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert!(result.content.contains("git"));
    }

    #[test]
    fn test_generate_tool_pipeline_writes_profile() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());

        let profile_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  modules:\n    - git\n";
        let result = dispatch_tool_call(
            "write_profile_yaml",
            &serde_json::json!({"name": "base", "content": profile_yaml}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);

        let profile_path = tmp.path().join("profiles/base.yaml");
        assert!(profile_path.exists());
    }

    #[test]
    fn test_generate_scan_dotfiles_via_dispatch() {
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join(".gitconfig"), "[user]\nname = Test").unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());

        let result = dispatch_tool_call(
            "scan_dotfiles",
            &serde_json::json!({}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert!(result.content.contains(".gitconfig"));
    }

    #[test]
    fn test_generate_unknown_tool_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());

        let result = dispatch_tool_call(
            "nonexistent_tool",
            &serde_json::json!({}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(result.is_error);
        assert!(result.content.contains("Unknown tool"));
    }

    #[test]
    fn test_generate_pipeline_module_then_profile() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());

        // Write a module
        let module_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: git\nspec:\n  packages:\n    - name: git\n";
        let result = dispatch_tool_call(
            "write_module_yaml",
            &serde_json::json!({"name": "git", "content": module_yaml}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);

        // Write a profile that references the module
        let profile_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Profile\nmetadata:\n  name: base\nspec:\n  modules:\n    - git\n";
        let result = dispatch_tool_call(
            "write_profile_yaml",
            &serde_json::json!({"name": "base", "content": profile_yaml}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);

        // list_generated shows both
        let result = dispatch_tool_call(
            "list_generated",
            &serde_json::json!({}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert!(result.content.contains("git"));
        assert!(result.content.contains("base"));
        assert!(result.content.contains("Module"));
        assert!(result.content.contains("Profile"));

        // get_existing_modules picks up the written module
        let result = dispatch_tool_call(
            "get_existing_modules",
            &serde_json::json!({}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert!(result.content.contains("git"));

        // get_existing_profiles picks up the written profile
        let result = dispatch_tool_call(
            "get_existing_profiles",
            &serde_json::json!({}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert!(result.content.contains("base"));
    }

    #[test]
    fn test_generate_pipeline_invalid_yaml_does_not_write() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut session = GenerateSession::new(tmp.path().to_path_buf());

        // validate_yaml reports invalid
        let bad_yaml = "not: valid: yaml: {{";
        let result = dispatch_tool_call(
            "validate_yaml",
            &serde_json::json!({"content": bad_yaml, "kind": "Module"}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error); // validate_yaml never errors; returns a result struct
        assert!(result.content.contains("false"));

        // write_module_yaml rejects the same invalid YAML
        let result = dispatch_tool_call(
            "write_module_yaml",
            &serde_json::json!({"name": "bad", "content": bad_yaml}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(result.is_error);

        // Nothing was written
        assert!(!tmp.path().join("modules/bad/module.yaml").exists());

        // list_generated is empty
        let result = dispatch_tool_call(
            "list_generated",
            &serde_json::json!({}),
            &mut session,
            tmp.path(),
            &[],
        );
        assert!(!result.is_error);
        assert_eq!(result.content, "[]");
    }
}
