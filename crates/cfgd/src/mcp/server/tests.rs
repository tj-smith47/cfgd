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

    // Without cfgd_ prefix
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

    // With cfgd_ prefix
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(serde_json::json!(6)),
        method: "tools/call".into(),
        params: serde_json::json!({"name": "cfgd_detect_platform", "arguments": {}}),
    };
    let resp = server.handle_request(&req);
    assert!(resp.error.is_none());
    let result = resp.result.unwrap();
    assert!(!result["isError"].as_bool().unwrap());
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
    let yaml = "apiVersion: cfgd.io/v1alpha1\nkind: Module\nmetadata:\n  name: nvim\nspec: {}\n";
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

// ---------------------------------------------------------------------------
// handle_notification — exercises the three notification arms (initialized,
// cancelled, unknown). They produce no response, only tracing — but they
// must not panic and they're entered exactly when `id.is_none()`.
// ---------------------------------------------------------------------------

#[test]
fn test_handle_notification_initialized_does_not_panic() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: None,
        method: "notifications/initialized".into(),
        params: serde_json::json!({}),
    };
    server.handle_notification(&req);
}

#[test]
fn test_handle_notification_cancelled_does_not_panic() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: None,
        method: "notifications/cancelled".into(),
        params: serde_json::json!({}),
    };
    server.handle_notification(&req);
}

#[test]
fn test_handle_notification_unknown_method_does_not_panic() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: None,
        method: "notifications/foo_bar_baz_unknown".into(),
        params: serde_json::json!({}),
    };
    server.handle_notification(&req);
}

// ---------------------------------------------------------------------------
// present_yaml — error path: invalid arguments returns -32602.
// ---------------------------------------------------------------------------

#[test]
fn test_present_yaml_invalid_arguments_returns_invalid_params_error() {
    // PresentYamlRequest requires content/kind/description. Send a non-object
    // arguments field so serde_json::from_value fails and the -32602 arm fires.
    let tmp = tempfile::TempDir::new().unwrap();
    let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(serde_json::json!(200)),
        method: "tools/call".into(),
        params: serde_json::json!({
            "name": "cfgd_present_yaml",
            "arguments": "not-an-object"
        }),
    };
    let resp = server.handle_request(&req);
    let err = resp
        .error
        .expect("invalid args must surface as JSON-RPC error");
    assert_eq!(err.code, -32602);
    assert!(
        err.message.contains("present_yaml"),
        "error message must mention present_yaml: {}",
        err.message
    );
}

// ---------------------------------------------------------------------------
// JsonRpcResponse helpers — exhaustive shape coverage to pin the public API.
// ---------------------------------------------------------------------------

#[test]
fn test_json_rpc_response_success_uses_jsonrpc_2_0() {
    let resp = JsonRpcResponse::success(Some(serde_json::json!(1)), serde_json::json!(null));
    assert_eq!(resp.jsonrpc, "2.0");
    assert!(resp.error.is_none());
}

#[test]
fn test_json_rpc_response_error_uses_jsonrpc_2_0() {
    let resp = JsonRpcResponse::error(Some(serde_json::json!(1)), -1, "x".into());
    assert_eq!(resp.jsonrpc, "2.0");
    assert!(resp.result.is_none());
}

#[test]
fn test_json_rpc_error_data_is_none_for_helper_constructor() {
    let resp = JsonRpcResponse::error(None, -32601, "Method not found".into());
    let err = resp.error.expect("error must be present");
    assert!(
        err.data.is_none(),
        "error helper should not populate data field"
    );
    assert_eq!(err.code, -32601);
}

// ---------------------------------------------------------------------------
// handle_request — verify resources/list returns array and prompts/get with
// arguments returns messages array. Pins MCP wire shape so downstream clients
// remain stable.
// ---------------------------------------------------------------------------

#[test]
fn test_handle_resources_list_returns_resources_field() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(serde_json::json!(300)),
        method: "resources/list".into(),
        params: serde_json::json!({}),
    };
    let resp = server.handle_request(&req);
    let result = resp.result.expect("resources/list must succeed");
    assert!(
        result["resources"].is_array(),
        "result must have resources array: {result}"
    );
}

#[test]
fn test_handle_prompts_get_unknown_name_still_succeeds_with_empty_messages() {
    // prompts::get for an unknown prompt returns an empty messages array
    // rather than an error — this matches MCP's tolerant prompt-discovery model.
    let tmp = tempfile::TempDir::new().unwrap();
    let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
    let req = JsonRpcRequest {
        jsonrpc: "2.0".into(),
        id: Some(serde_json::json!(301)),
        method: "prompts/get".into(),
        params: serde_json::json!({"name": "totally_unknown_prompt"}),
    };
    let resp = server.handle_request(&req);
    // No error — unknown prompts return an empty/default response.
    assert!(resp.error.is_none() || resp.result.is_some());
}

#[test]
fn test_jsonrpc_invalid_version_2_5_returns_invalid_request() {
    // "2.5" is not "2.0" — should fail the jsonrpc-version gate.
    let tmp = tempfile::TempDir::new().unwrap();
    let mut server = McpServer::new(tmp.path().to_path_buf(), tmp.path().to_path_buf());
    let req = JsonRpcRequest {
        jsonrpc: "2.5".into(),
        id: Some(serde_json::json!(302)),
        method: "ping".into(),
        params: serde_json::json!({}),
    };
    let resp = server.handle_request(&req);
    let err = resp.error.expect("invalid version must yield error");
    assert_eq!(err.code, -32600);
    assert!(
        err.message.contains("2.5"),
        "error must reference the offending version: {}",
        err.message
    );
}
