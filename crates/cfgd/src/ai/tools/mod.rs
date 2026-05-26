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

const ERR_NAME_REQUIRED: &str = "Error: 'name' parameter is required";
const ERR_CONTENT_REQUIRED: &str = "Error: 'content' parameter is required";
const DOC_KIND_DESC: &str = "Document kind: 'Module', 'Profile', or 'Config'";

fn serialize_tool_content<T: serde::Serialize>(value: &T) -> ToolCallResult {
    match serde_json::to_string(value) {
        Ok(s) => ToolCallResult {
            content: s,
            is_error: false,
        },
        Err(e) => {
            tracing::error!(error = %e, "tool result serialization failed");
            ToolCallResult {
                content: format!("Error: tool result serialization failed: {e}"),
                is_error: true,
            }
        }
    }
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
            description: "Scan platform-specific system settings: macOS defaults domains, systemd user units, LaunchAgents, gsettings schemas, Windows registry values, and Windows services.".into(),
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
                        "description": DOC_KIND_DESC
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
                        "description": DOC_KIND_DESC
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
                        "description": DOC_KIND_DESC
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
        Ok(entries) => serialize_tool_content(&entries),
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
        Ok(entries) => serialize_tool_content(&entries),
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
        Ok(result) => serialize_tool_content(&result),
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_scan_system_settings() -> ToolCallResult {
    match scan::scan_system_settings() {
        Ok(result) => serialize_tool_content(&result),
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
    serialize_tool_content(&value)
}

fn dispatch_inspect_tool(input: &Value, home: &Path) -> ToolCallResult {
    let name = match input.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return ToolCallResult {
                content: ERR_NAME_REQUIRED.to_string(),
                is_error: true,
            };
        }
    };
    match inspect::inspect_tool(name, home) {
        Ok(result) => serialize_tool_content(&result),
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
        Ok(result) => serialize_tool_content(&result),
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
        Ok(result) => serialize_tool_content(&result),
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
        Ok(entries) => serialize_tool_content(&entries),
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
            serialize_tool_content(&paths)
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
                content: ERR_CONTENT_REQUIRED.to_string(),
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
    serialize_tool_content(&result)
}

fn dispatch_write_module_yaml(input: &Value, session: &mut GenerateSession) -> ToolCallResult {
    let name = match input.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return ToolCallResult {
                content: ERR_NAME_REQUIRED.to_string(),
                is_error: true,
            };
        }
    };
    let content = match input.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ToolCallResult {
                content: ERR_CONTENT_REQUIRED.to_string(),
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
                content: ERR_NAME_REQUIRED.to_string(),
                is_error: true,
            };
        }
    };
    let content = match input.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ToolCallResult {
                content: ERR_CONTENT_REQUIRED.to_string(),
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
    serialize_tool_content(&entries)
}

fn dispatch_get_existing_modules(session: &GenerateSession) -> ToolCallResult {
    match session.get_existing_modules() {
        Ok(names) => serialize_tool_content(&names),
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

fn dispatch_get_existing_profiles(session: &GenerateSession) -> ToolCallResult {
    match session.get_existing_profiles() {
        Ok(names) => serialize_tool_content(&names),
        Err(e) => ToolCallResult {
            content: format!("Error: {}", e),
            is_error: true,
        },
    }
}

#[cfg(test)]
mod tests;
