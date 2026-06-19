

use crate::types::*;

#[derive(Debug, Clone)]
pub struct ContextManager {
    pub system_prompt: String,
    pub history: MessageHistory,
    pub max_tool_output_chars: u64,
}

impl ContextManager {
    pub fn new(system_prompt: String, max_tokens: u64) -> Self {
        Self {
            system_prompt,
            history: MessageHistory::new(max_tokens),
            max_tool_output_chars: 50_000,
        }
    }

    pub fn add_user_message(&mut self, parts: Vec<ContentPart>) -> MessageId {
        let id = uuid::Uuid::new_v4().to_string();
        let msg = Message::User {
            id: id.clone(),
            content: parts,
        };
        self.history.push(msg);
        id
    }

    pub fn add_assistant_message(
        &mut self,
        text: String,
        reasoning: Option<String>,
        tool_calls: Vec<ToolCall>,
    ) -> MessageId {
        let id = uuid::Uuid::new_v4().to_string();
        let mut content = Vec::new();
        if let Some(r) = reasoning {
            content.push(ContentPart::Reasoning { text: r });
        }
        if !text.is_empty() {
            content.push(ContentPart::Text { text });
        }
        let msg = Message::Assistant {
            id: id.clone(),
            content,
            tool_calls,
        };
        self.history.push(msg);
        id
    }

    pub fn add_tool_result(
        &mut self,
        tool_call_id: String,
        tool_name: String,
        result: ToolResultValue,
    ) -> MessageId {
        let id = uuid::Uuid::new_v4().to_string();

        let truncated_result = match &result {
            ToolResultValue::Text { value } => {
                let truncated = if value.len() as u64 > self.max_tool_output_chars {
                    format!(
                        "{}...[output truncated at {} chars]",
                        &value[..self.max_tool_output_chars as usize],
                        self.max_tool_output_chars
                    )
                } else {
                    value.clone()
                };
                ToolResultValue::Text { value: truncated }
            }
            ToolResultValue::Json { value } => {
                let s = value.to_string();
                let truncated = if s.len() as u64 > self.max_tool_output_chars {
                    ToolResultValue::Text {
                        value: format!(
                            "{}...[output truncated at {} chars]",
                            &s[..self.max_tool_output_chars as usize],
                            self.max_tool_output_chars
                        ),
                    }
                } else {
                    ToolResultValue::Json {
                        value: value.clone(),
                    }
                };
                truncated
            }
            _ => result.clone(),
        };

        let msg = Message::Tool {
            id: id.clone(),
            tool_call_id,
            tool_name,
            result: truncated_result,
        };
        self.history.push(msg);
        id
    }

    pub fn build_request(&self) -> (String, Vec<Message>, Vec<serde_json::Value>) {
        let system = self.system_prompt.clone();
        let messages = self.history.messages.clone();
        (system, messages, Vec::new())
    }

    pub fn needs_compaction(&self) -> bool {
        self.history.is_overflow()
    }

    pub fn estimated_tokens(&self) -> u64 {
        self.history.current_tokens
    }
}
