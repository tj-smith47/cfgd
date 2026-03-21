use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use cfgd_core::generate::PresentYamlRequest;
use cfgd_core::generate::session::GenerateSession;
use cfgd_core::providers::PackageManager;

use crate::mcp::{prompts, resources, tools};
use crate::packages;

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcResponse {
    pub fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Option<Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
        }
    }
}

pub struct McpServer {
    session: GenerateSession,
    home: PathBuf,
    managers: Vec<Box<dyn PackageManager>>,
}

impl McpServer {
    pub fn new(repo_root: PathBuf, home: PathBuf) -> Self {
        Self {
            session: GenerateSession::new(repo_root),
            home,
            managers: packages::all_package_managers(),
        }
    }

    /// Run the MCP server, reading JSON-RPC messages from stdin and writing responses to stdout.
    pub fn run(&mut self) -> anyhow::Result<()> {
        let stdin = io::stdin();
        let stdout = io::stdout();
        let mut stdout = stdout.lock();

        for line in stdin.lock().lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }

            let request: JsonRpcRequest = match serde_json::from_str(&line) {
                Ok(req) => req,
                Err(e) => {
                    let resp = JsonRpcResponse::error(None, -32700, format!("Parse error: {}", e));
                    serde_json::to_writer(&mut stdout, &resp)?;
                    writeln!(stdout)?;
                    stdout.flush()?;
                    continue;
                }
            };

            // Notifications (no id) don't require a response per JSON-RPC 2.0,
            // but MCP expects we handle them silently.
            if request.id.is_none() {
                self.handle_notification(&request);
                continue;
            }

            let response = self.handle_request(&request);
            serde_json::to_writer(&mut stdout, &response)?;
            writeln!(stdout)?;
            stdout.flush()?;
        }

        Ok(())
    }

    fn handle_notification(&mut self, request: &JsonRpcRequest) {
        match request.method.as_str() {
            "notifications/initialized" => {
                tracing::debug!("MCP client initialized");
            }
            "notifications/cancelled" => {
                tracing::debug!("MCP client cancelled request");
            }
            other => {
                tracing::debug!(method = other, "unknown MCP notification");
            }
        }
    }

    pub fn handle_request(&mut self, request: &JsonRpcRequest) -> JsonRpcResponse {
        if request.jsonrpc != "2.0" {
            return JsonRpcResponse::error(
                request.id.clone(),
                -32600,
                format!(
                    "Invalid Request: jsonrpc must be \"2.0\", got \"{}\"",
                    request.jsonrpc
                ),
            );
        }
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request),
            "ping" => JsonRpcResponse::success(request.id.clone(), serde_json::json!({})),
            "tools/list" => self.handle_tools_list(request),
            "tools/call" => self.handle_tools_call(request),
            "resources/list" => self.handle_resources_list(request),
            "resources/read" => self.handle_resources_read(request),
            "prompts/list" => self.handle_prompts_list(request),
            "prompts/get" => self.handle_prompts_get(request),
            _ => JsonRpcResponse::error(
                request.id.clone(),
                -32601,
                format!("Method not found: {}", request.method),
            ),
        }
    }

    fn handle_initialize(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse::success(
            request.id.clone(),
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {},
                    "resources": {},
                    "prompts": {}
                },
                "serverInfo": {
                    "name": "cfgd",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )
    }

    fn handle_tools_list(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse::success(request.id.clone(), tools::list())
    }

    fn handle_tools_call(&mut self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let name = request.params.get("name").and_then(|v| v.as_str());
        let arguments = request
            .params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        match name {
            Some(tool_name) => {
                let dispatch_name = tools::strip_prefix(tool_name).unwrap_or(tool_name);

                // present_yaml is handled here in MCP mode: return the YAML content
                // formatted for the client to display. The MCP client handles presentation.
                if dispatch_name == "present_yaml" {
                    let req = match serde_json::from_value::<PresentYamlRequest>(arguments.clone())
                    {
                        Ok(r) => r,
                        Err(e) => {
                            return JsonRpcResponse::error(
                                request.id.clone(),
                                -32602,
                                format!("Invalid present_yaml arguments: {}", e),
                            );
                        }
                    };
                    let text = format!(
                        "## {} — {}\n\n```yaml\n{}\n```\n\nPlease review and respond with your choice: accept, reject, feedback (with message), or stepThrough.",
                        req.kind, req.description, req.content
                    );
                    return JsonRpcResponse::success(
                        request.id.clone(),
                        serde_json::json!({
                            "content": [{"type": "text", "text": text}],
                            "isError": false
                        }),
                    );
                }

                let result = crate::ai::tools::dispatch_tool_call(
                    dispatch_name,
                    &arguments,
                    &mut self.session,
                    &self.home,
                    &self.managers,
                );
                let mcp_result = serde_json::json!({
                    "content": [{
                        "type": "text",
                        "text": result.content
                    }],
                    "isError": result.is_error
                });
                JsonRpcResponse::success(request.id.clone(), mcp_result)
            }
            None => JsonRpcResponse::error(
                request.id.clone(),
                -32602,
                "Missing required parameter: name".into(),
            ),
        }
    }

    fn handle_resources_list(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse::success(request.id.clone(), resources::list())
    }

    fn handle_resources_read(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let uri = request.params.get("uri").and_then(|v| v.as_str());
        match uri {
            Some(resource_uri) => match resources::read(resource_uri) {
                Ok(result) => JsonRpcResponse::success(request.id.clone(), result),
                Err(msg) => JsonRpcResponse::error(request.id.clone(), -32002, msg),
            },
            None => JsonRpcResponse::error(
                request.id.clone(),
                -32602,
                "Missing required parameter: uri".into(),
            ),
        }
    }

    fn handle_prompts_list(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse::success(request.id.clone(), prompts::list())
    }

    fn handle_prompts_get(&self, request: &JsonRpcRequest) -> JsonRpcResponse {
        let name = request.params.get("name").and_then(|v| v.as_str());
        let arguments = request
            .params
            .get("arguments")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        match name {
            Some(prompt_name) => {
                JsonRpcResponse::success(request.id.clone(), prompts::get(prompt_name, &arguments))
            }
            None => JsonRpcResponse::error(
                request.id.clone(),
                -32602,
                "Missing required parameter: name".into(),
            ),
        }
    }
}

/// Entry point for `cfgd mcp-server` command.
pub fn run_mcp_server(config_path: &std::path::Path) -> anyhow::Result<()> {
    let repo_root = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));

    let mut server = McpServer::new(repo_root, home);
    server.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_rpc_request_parsing() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, Some(serde_json::json!(1)));
    }

    #[test]
    fn test_json_rpc_request_without_params() {
        let json = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.method, "ping");
        assert_eq!(req.params, Value::Null);
    }

    #[test]
    fn test_json_rpc_request_string_id() {
        let json = r#"{"jsonrpc":"2.0","id":"abc-123","method":"tools/list","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.id, Some(serde_json::json!("abc-123")));
    }

    #[test]
    fn test_json_rpc_request_null_id_notification() {
        let json = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
        assert!(req.id.is_none());
    }

    #[test]
    fn test_json_rpc_response_success() {
        let resp =
            JsonRpcResponse::success(Some(serde_json::json!(1)), serde_json::json!({"ok": true}));
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["jsonrpc"], "2.0");
        assert_eq!(json["id"], 1);
        assert!(json["result"]["ok"].as_bool().unwrap());
        assert!(json.get("error").is_none());
    }

    #[test]
    fn test_json_rpc_response_success_null_result() {
        let resp = JsonRpcResponse::success(Some(serde_json::json!(1)), Value::Null);
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["jsonrpc"], "2.0");
        assert!(json["result"].is_null());
    }

    #[test]
    fn test_json_rpc_response_error() {
        let resp = JsonRpcResponse::error(Some(serde_json::json!(1)), -32601, "Not found".into());
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["error"]["code"], -32601);
        assert_eq!(json["error"]["message"], "Not found");
        assert!(json.get("result").is_none());
    }

    #[test]
    fn test_json_rpc_response_error_without_id() {
        let resp = JsonRpcResponse::error(None, -32700, "Parse error".into());
        let json = serde_json::to_value(&resp).unwrap();
        assert!(json.get("id").is_none());
        assert_eq!(json["error"]["code"], -32700);
    }

    #[test]
    fn test_handle_initialize() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(1)),
            method: "initialize".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(result["capabilities"]["tools"].is_object());
        assert!(result["capabilities"]["resources"].is_object());
        assert!(result["capabilities"]["prompts"].is_object());
        assert_eq!(result["serverInfo"]["name"], "cfgd");
        assert!(!result["serverInfo"]["version"].as_str().unwrap().is_empty());
    }

    #[test]
    fn test_handle_ping() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(42)),
            method: "ping".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_none());
        assert_eq!(resp.id, Some(serde_json::json!(42)));
        assert_eq!(resp.result.unwrap(), serde_json::json!({}));
    }

    #[test]
    fn test_handle_tools_list() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(2)),
            method: "tools/list".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert!(result["tools"].is_array());
    }

    #[test]
    fn test_handle_tools_call_missing_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(3)),
            method: "tools/call".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[test]
    fn test_handle_tools_call_unknown_tool() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(4)),
            method: "tools/call".into(),
            params: serde_json::json!({"name": "nonexistent_tool", "arguments": {}}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert!(result["isError"].as_bool().unwrap());
    }

    #[test]
    fn test_handle_tools_call_detect_platform() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(5)),
            method: "tools/call".into(),
            params: serde_json::json!({"name": "detect_platform", "arguments": {}}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert!(!result["isError"].as_bool().unwrap());
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("os")
        );
    }

    #[test]
    fn test_handle_resources_list() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(6)),
            method: "resources/list".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert!(result["resources"].is_array());
    }

    #[test]
    fn test_handle_resources_read_missing_uri() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(7)),
            method: "resources/read".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[test]
    fn test_handle_resources_read_unknown_uri_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(77)),
            method: "resources/read".into(),
            params: serde_json::json!({"uri": "cfgd://unknown/resource"}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32002);
    }

    #[test]
    fn test_handle_prompts_list() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(8)),
            method: "prompts/list".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert!(result["prompts"].is_array());
    }

    #[test]
    fn test_handle_prompts_get_missing_name() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(9)),
            method: "prompts/get".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32602);
    }

    #[test]
    fn test_unknown_method_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(10)),
            method: "unknown/method".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_some());
        let err = resp.error.unwrap();
        assert_eq!(err.code, -32601);
        assert!(err.message.contains("unknown/method"));
    }

    #[test]
    fn test_response_serialization_skips_none_fields() {
        let resp = JsonRpcResponse::success(Some(serde_json::json!(1)), serde_json::json!("ok"));
        let serialized = serde_json::to_string(&resp).unwrap();
        assert!(!serialized.contains("error"));

        let resp = JsonRpcResponse::error(Some(serde_json::json!(1)), -1, "fail".into());
        let serialized = serde_json::to_string(&resp).unwrap();
        assert!(!serialized.contains("result"));
    }

    #[test]
    fn test_id_preserved_across_request_response() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());

        // Integer id
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(999)),
            method: "ping".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        assert_eq!(resp.id, Some(serde_json::json!(999)));

        // String id
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!("request-abc")),
            method: "ping".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        assert_eq!(resp.id, Some(serde_json::json!("request-abc")));
    }

    #[test]
    fn test_mcp_full_initialize_handshake() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(1)),
            method: "initialize".into(),
            params: serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {}}),
        };
        let resp = server.handle_request(&req);
        let result = resp.result.unwrap();
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert!(result["capabilities"]["tools"].is_object());
        assert!(result["capabilities"]["resources"].is_object());
        assert!(result["capabilities"]["prompts"].is_object());
    }

    #[test]
    fn test_mcp_tools_list_returns_all_tools() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(2)),
            method: "tools/list".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        let result = resp.result.unwrap();
        let tools = result["tools"].as_array().unwrap();
        assert!(tools.len() >= 18);
        for tool in tools {
            assert!(
                tool["name"].as_str().unwrap().starts_with("cfgd_"),
                "tool '{}' does not have cfgd_ prefix",
                tool["name"]
            );
        }
    }

    #[test]
    fn test_mcp_tools_call_detect_platform() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(3)),
            method: "tools/call".into(),
            params: serde_json::json!({"name": "cfgd_detect_platform", "arguments": {}}),
        };
        let resp = server.handle_request(&req);
        let result = resp.result.unwrap();
        assert!(result["content"][0]["text"].as_str().is_some());
    }

    #[test]
    fn test_mcp_tools_call_get_schema() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(4)),
            method: "tools/call".into(),
            params: serde_json::json!({"name": "cfgd_get_schema", "arguments": {"kind": "Module"}}),
        };
        let resp = server.handle_request(&req);
        let result = resp.result.unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("apiVersion"));
    }

    #[test]
    fn test_mcp_resources_read_skill() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(5)),
            method: "resources/read".into(),
            params: serde_json::json!({"uri": "cfgd://skill/generate"}),
        };
        let resp = server.handle_request(&req);
        let result = resp.result.unwrap();
        let text = result["contents"][0]["text"].as_str().unwrap();
        assert!(text.contains("configuration generator"));
    }

    #[test]
    fn test_mcp_prompts_get_generate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());

        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(6)),
            method: "prompts/get".into(),
            params: serde_json::json!({"name": "cfgd_generate", "arguments": {"mode": "module", "name": "nvim"}}),
        };
        let resp = server.handle_request(&req);
        let result = resp.result.unwrap();
        let messages = result["messages"].as_array().unwrap();
        assert!(!messages.is_empty());
    }

    #[test]
    fn test_mcp_full_pipeline_write_module() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());

        let module_yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: test\nspec:\n  packages:\n    - name: git\n";

        // Validate
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(7)),
            method: "tools/call".into(),
            params: serde_json::json!({"name": "cfgd_validate_yaml", "arguments": {"content": module_yaml, "kind": "Module"}}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_none());

        // Write
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(8)),
            method: "tools/call".into(),
            params: serde_json::json!({"name": "cfgd_write_module_yaml", "arguments": {"name": "test", "content": module_yaml}}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_none());

        // Verify file exists
        assert!(tmp.path().join("modules/test/module.yaml").exists());
    }

    #[test]
    fn test_full_roundtrip_initialize_and_tools_list() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());

        // Initialize
        let init_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(1)),
            method: "initialize".into(),
            params: serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0.1.0"}
            }),
        };
        let init_resp = server.handle_request(&init_req);
        assert!(init_resp.error.is_none());

        // The initialized notification (no id) would be handled by handle_notification in run()

        // tools/list
        let list_req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(2)),
            method: "tools/list".into(),
            params: serde_json::json!({}),
        };
        let list_resp = server.handle_request(&list_req);
        assert!(list_resp.error.is_none());
        assert!(list_resp.result.unwrap()["tools"].is_array());
    }

    #[test]
    fn test_invalid_jsonrpc_version_returns_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "1.0".into(),
            id: Some(serde_json::json!(1)),
            method: "ping".into(),
            params: serde_json::json!({}),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32600);
    }

    #[test]
    fn test_present_yaml_mcp_returns_formatted_content() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let yaml =
            "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: nvim\nspec: {}\n";
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(100)),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "cfgd_present_yaml",
                "arguments": {
                    "content": yaml,
                    "kind": "Module",
                    "description": "Neovim configuration module"
                }
            }),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert!(!result["isError"].as_bool().unwrap());
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Module"));
        assert!(text.contains("Neovim configuration module"));
        assert!(text.contains("```yaml"));
        assert!(text.contains("nvim"));
        assert!(text.contains("accept, reject, feedback"));
    }

    #[test]
    fn test_present_yaml_without_cfgd_prefix_also_works() {
        // When strip_prefix returns "present_yaml" (no prefix in name)
        let tmp = tempfile::TempDir::new().unwrap();
        let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
        let req = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(101)),
            method: "tools/call".into(),
            params: serde_json::json!({
                "name": "present_yaml",
                "arguments": {
                    "content": "key: value\n",
                    "kind": "Config",
                    "description": "test"
                }
            }),
        };
        let resp = server.handle_request(&req);
        assert!(resp.error.is_none());
        let result = resp.result.unwrap();
        assert!(!result["isError"].as_bool().unwrap());
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("Config"));
    }
}
