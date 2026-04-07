use crate::ai::client::{ContentBlock, Message};

pub struct Conversation {
    system_prompt: String,
    messages: Vec<Message>,
    input_tokens: u64,
    output_tokens: u64,
}

impl Conversation {
    /// Create a new conversation with the given system prompt.
    pub fn new(system_prompt: String) -> Self {
        Self {
            system_prompt,
            messages: vec![],
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    /// Add a user message with text content.
    pub fn add_user_message(&mut self, text: &str) {
        self.messages.push(Message {
            role: "user".into(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        });
    }

    /// Add an assistant response (from API).
    pub fn add_assistant_message(&mut self, blocks: Vec<ContentBlock>) {
        self.messages.push(Message {
            role: "assistant".into(),
            content: blocks,
        });
    }

    /// Add tool results as a user message (Anthropic API requires tool_result in user role).
    pub fn add_tool_results(&mut self, results: Vec<ContentBlock>) {
        self.messages.push(Message {
            role: "user".into(),
            content: results,
        });
    }

    /// Get the system prompt.
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Get all messages for the API call.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// Track token usage from an API response.
    pub fn track_usage(&mut self, input: u64, output: u64) {
        self.input_tokens += input;
        self.output_tokens += output;
    }

    /// Get total token usage (input, output).
    pub fn total_tokens(&self) -> (u64, u64) {
        (self.input_tokens, self.output_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_conversation_flow() {
        let mut conv = Conversation::new("system".into());
        // User asks
        conv.add_user_message("Generate config");
        // Assistant responds with tool call
        conv.add_assistant_message(vec![
            ContentBlock::Text {
                text: "Scanning...".into(),
            },
            ContentBlock::ToolUse {
                id: "t1".into(),
                name: "scan_dotfiles".into(),
                input: serde_json::json!({}),
            },
        ]);
        // Tool result
        conv.add_tool_results(vec![ContentBlock::ToolResult {
            tool_use_id: "t1".into(),
            content: "[]".into(),
            is_error: None,
        }]);
        assert_eq!(conv.messages().len(), 3);
    }
}
