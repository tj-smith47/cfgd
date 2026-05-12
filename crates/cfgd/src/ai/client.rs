use serde::{Deserialize, Serialize};

use cfgd_core::errors::GenerateError;

/// A message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

/// A content block within a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// Tool definition sent in the API request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// API request body.
#[derive(Debug, Serialize)]
struct ApiRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "<[ToolDefinition]>::is_empty")]
    tools: &'a [ToolDefinition],
}

/// API response body.
#[derive(Debug, Deserialize)]
pub struct ApiResponse {
    pub id: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    pub usage: Usage,
}

#[derive(Debug, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Production base URL for the Anthropic Messages API. Test seam:
/// `CFGD_ANTHROPIC_URL` overrides this at `AnthropicClient::new` time so
/// `mockito::Server` can drive `cmd_generate` end-to-end against canned
/// responses (mirrors the `CFGD_<NAME>_BIN` tool-shim pattern documented
/// in `.claude/rules/shared-utils.md`).
const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

/// Thin Anthropic Messages API client.
pub struct AnthropicClient {
    api_key: String,
    model: String,
    base_url: String,
}

impl AnthropicClient {
    pub fn new(api_key: String, model: String) -> Self {
        let base_url = std::env::var("CFGD_ANTHROPIC_URL")
            .unwrap_or_else(|_| DEFAULT_ANTHROPIC_BASE_URL.to_string());
        Self {
            api_key,
            model,
            base_url,
        }
    }

    /// Send a message to the Anthropic Messages API.
    pub fn send_message(
        &self,
        messages: &[Message],
        system: &str,
        tools: &[ToolDefinition],
        max_tokens: u32,
    ) -> Result<ApiResponse, GenerateError> {
        let request = ApiRequest {
            model: &self.model,
            max_tokens,
            system,
            messages,
            tools,
        };

        // Must use a bounded timeout — the previous `ureq::post(...)` had
        // none and could hang the CLI indefinitely on a slow / unreachable
        // api.anthropic.com.
        let response = cfgd_core::http::http_agent(cfgd_core::http::HTTP_AI_TIMEOUT)
            .post(&format!("{}/v1/messages", self.base_url))
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", "2023-06-01")
            .set("content-type", "application/json")
            .send_json(&request)
            .map_err(|e| GenerateError::ProviderError {
                message: format!("API request failed: {e}"),
            })?;

        let api_response: ApiResponse =
            response
                .into_json()
                .map_err(|e| GenerateError::ProviderError {
                    message: format!("Failed to parse API response: {e}"),
                })?;

        Ok(api_response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cfgd_core::test_helpers::EnvVarGuard;

    #[test]
    fn test_tool_use_deserialization() {
        let json = r#"{
            "type": "tool_use",
            "id": "toolu_123",
            "name": "scan_dotfiles",
            "input": {"home": "/home/user"}
        }"#;
        let block: ContentBlock = serde_json::from_str(json).unwrap();
        match block {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_123");
                assert_eq!(name, "scan_dotfiles");
                assert_eq!(input["home"], "/home/user");
            }
            _ => panic!("Expected ToolUse"),
        }
    }

    #[test]
    fn test_api_response_deserialization() {
        let json = r#"{
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "I'll scan your system."},
                {"type": "tool_use", "id": "toolu_456", "name": "scan_dotfiles", "input": {"home": "/home/user"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 100, "output_tokens": 50}
        }"#;
        let resp: ApiResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.id, "msg_123");
        assert_eq!(resp.content.len(), 2);
        assert_eq!(resp.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(resp.usage.input_tokens, 100);
    }

    #[test]
    fn test_message_with_multiple_content_blocks() {
        let msg = Message {
            role: "assistant".into(),
            content: vec![
                ContentBlock::Text {
                    text: "Let me scan.".into(),
                },
                ContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "scan_dotfiles".into(),
                    input: serde_json::json!({"home": "/home/user"}),
                },
            ],
        };
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["content"].as_array().unwrap().len(), 2);
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][1]["type"], "tool_use");
    }

    #[test]
    fn test_api_request_omits_empty_tools() {
        let request = ApiRequest {
            model: "claude-sonnet-4-6",
            max_tokens: 4096,
            system: "You are helpful.",
            messages: &[],
            tools: &[],
        };
        let json = serde_json::to_value(&request).unwrap();
        assert!(json.get("tools").is_none());
    }

    #[test]
    fn test_api_request_includes_tools_when_present() {
        let tools = vec![ToolDefinition {
            name: "test_tool".into(),
            description: "A test tool".into(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let request = ApiRequest {
            model: "claude-sonnet-4-6",
            max_tokens: 4096,
            system: "You are helpful.",
            messages: &[],
            tools: &tools,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert!(json.get("tools").is_some());
        assert_eq!(json["tools"].as_array().unwrap().len(), 1);
    }

    // ─── send_message HTTP round-trip via mockito ────────────────────────
    //
    // The `CFGD_ANTHROPIC_URL` env-shim redirects the production
    // `POST {base_url}/v1/messages` call at a `mockito::Server` so we can
    // drive `AnthropicClient::send_message` end-to-end against canned
    // responses — same pattern as upgrade/server_client/oci tests.

    #[test]
    #[serial_test::serial]
    fn send_message_round_trips_via_mockito_base_url() {
        let mut server = mockito::Server::new();
        let _env = EnvVarGuard::set("CFGD_ANTHROPIC_URL", &server.url());

        let mock = server
            .mock("POST", "/v1/messages")
            .match_header("x-api-key", "test-key-abc")
            .match_header("anthropic-version", "2023-06-01")
            .match_header("content-type", "application/json")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{
                    "id": "msg_mocked_001",
                    "content": [{"type": "text", "text": "Hello from mock."}],
                    "stop_reason": "end_turn",
                    "usage": {"input_tokens": 12, "output_tokens": 7}
                }"#,
            )
            .create();

        let client = AnthropicClient::new("test-key-abc".to_string(), "claude-sonnet-4-6".into());
        let response = client
            .send_message(&[], "You are a test.", &[], 1024)
            .expect("send_message should succeed against the mock");

        mock.assert();
        assert_eq!(response.id, "msg_mocked_001");
        assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(response.usage.input_tokens, 12);
        assert_eq!(response.usage.output_tokens, 7);
        match &response.content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello from mock."),
            other => panic!("expected text block, got {other:?}"),
        }
    }

    #[test]
    #[serial_test::serial]
    fn send_message_surfaces_api_error_status_as_provider_error() {
        let mut server = mockito::Server::new();
        let _env = EnvVarGuard::set("CFGD_ANTHROPIC_URL", &server.url());

        let _mock = server
            .mock("POST", "/v1/messages")
            .with_status(429)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
            )
            .create();

        let client = AnthropicClient::new("test-key".into(), "claude-sonnet-4-6".into());
        let err = client
            .send_message(&[], "system", &[], 1024)
            .expect_err("non-2xx should surface as ProviderError");
        let msg = format!("{err}");
        assert!(
            msg.contains("API request failed"),
            "error should be wrapped as ProviderError: {msg}"
        );
    }
}
