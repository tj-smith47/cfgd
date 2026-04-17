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

/// Thin Anthropic Messages API client.
pub struct AnthropicClient {
    api_key: String,
    model: String,
}

impl AnthropicClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self { api_key, model }
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
            .post("https://api.anthropic.com/v1/messages")
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
}
