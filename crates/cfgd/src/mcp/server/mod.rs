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

        // Cap individual JSON-RPC messages at 10 MB to prevent DoS via memory exhaustion
        const MAX_LINE_BYTES: usize = 10 * 1024 * 1024;
        let reader = stdin.lock();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if line.len() > MAX_LINE_BYTES {
                let resp = JsonRpcResponse::error(None, -32600, "request too large".to_string());
                serde_json::to_writer(&mut stdout, &resp)?;
                writeln!(stdout)?;
                stdout.flush()?;
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
mod tests;
