use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub type MessageId = String;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub output_schema: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

impl ToolCall {
    pub fn from_json_value(v: &serde_json::Value) -> Option<Self> {
        Some(Self {
            id: v["id"].as_str()?.to_string(),
            name: v["name"].as_str()?.to_string(),
            input: v["input"].clone(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub id: String,
    pub name: String,
    pub result: ToolResultValue,
    pub output: Option<ToolOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolResultValue {
    Text { value: String },
    Json { value: serde_json::Value },
    Error { value: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub content: Vec<ToolContent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolContent {
    Text { text: String },
    File { uri: String, mime: String, name: Option<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum Message {
    System {
        id: MessageId,
        content: String,
    },
    User {
        id: MessageId,
        content: Vec<ContentPart>,
    },
    Assistant {
        id: MessageId,
        content: Vec<ContentPart>,
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        id: MessageId,
        tool_call_id: String,
        tool_name: String,
        result: ToolResultValue,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    Text { text: String },
    Reasoning { text: String },
    File { uri: String, mime: String },
}

#[derive(Debug, Clone)]
pub enum LlmEvent {
    StepStart {
        index: u32,
    },
    TextStart {
        id: String,
    },
    TextDelta {
        id: String,
        text: String,
    },
    TextEnd {
        id: String,
    },
    ReasoningStart {
        id: String,
    },
    ReasoningDelta {
        id: String,
        text: String,
    },
    ReasoningEnd {
        id: String,
    },
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallDelta {
        id: String,
        name: String,
        text: String,
    },
    ToolCallEnd {
        id: String,
        name: String,
    },
    ToolCallReceived {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResultReceived {
        id: String,
        name: String,
        result: ToolResultValue,
        output: Option<ToolOutput>,
    },
    ToolError {
        id: String,
        name: String,
        message: String,
    },
    StepFinish {
        index: u32,
        reason: FinishReason,
        usage: Option<Usage>,
    },
    Finish {
        reason: FinishReason,
        usage: Option<Usage>,
    },
    ProviderError {
        message: String,
        retryable: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FinishReason {
    Stop,
    Length,
    ContentFiltered,
    ToolCalls,
    Error,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
}

impl Usage {
    pub fn merge(&mut self, other: &Usage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_input_tokens = Some(
            self.cache_read_input_tokens.unwrap_or(0)
                + other.cache_read_input_tokens.unwrap_or(0),
        );
        self.cache_write_input_tokens = Some(
            self.cache_write_input_tokens.unwrap_or(0)
                + other.cache_write_input_tokens.unwrap_or(0),
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageHistory {
    pub messages: Vec<Message>,
    pub max_tokens: u64,
    pub current_tokens: u64,
}

impl Message {
    pub fn id(&self) -> &str {
        match self {
            Message::System { id, .. } => id,
            Message::User { id, .. } => id,
            Message::Assistant { id, .. } => id,
            Message::Tool { id, .. } => id,
        }
    }

    pub fn role_str(&self) -> &'static str {
        match self {
            Message::System { .. } => "system",
            Message::User { .. } => "user",
            Message::Assistant { .. } => "assistant",
            Message::Tool { .. } => "tool",
        }
    }
}

impl MessageHistory {
    pub fn new(max_tokens: u64) -> Self {
        Self {
            messages: Vec::new(),
            max_tokens,
            current_tokens: 0,
        }
    }

    pub fn push(&mut self, message: Message) {
        self.current_tokens += estimate_tokens(&message);
        self.messages.push(message);
    }

    pub fn is_overflow(&self) -> bool {
        self.current_tokens > self.max_tokens
    }

    pub fn as_llm_messages(&self) -> Vec<HashMap<String, serde_json::Value>> {
        let mut result = Vec::new();
        for msg in &self.messages {
            match msg {
                Message::System { content, .. } => {
                    let mut map = HashMap::new();
                    map.insert("role".into(), serde_json::Value::String("system".into()));
                    map.insert("content".into(), serde_json::Value::String(content.clone()));
                    result.push(map);
                }
                Message::User { content, .. } => {
                    let mut map = HashMap::new();
                    map.insert("role".into(), serde_json::Value::String("user".into()));
                    let parts: Vec<serde_json::Value> = content
                        .iter()
                        .map(|p| match p {
                            ContentPart::Text { text } => serde_json::json!({
                                "type": "text",
                                "text": text
                            }),
                            ContentPart::File { uri, mime } => serde_json::json!({
                                "type": "file",
                                "uri": uri,
                                "mime": mime
                            }),
                            ContentPart::Reasoning { .. } => serde_json::json!({}),
                        })
                        .collect();
                    map.insert("content".into(), serde_json::Value::Array(parts));
                    result.push(map);
                }
                Message::Assistant {
                    content, tool_calls, ..
                } => {
                    let mut map = HashMap::new();
                    map.insert("role".into(), serde_json::Value::String("assistant".into()));
                    let text_parts: Vec<String> = content
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect();
                    map.insert("content".into(), serde_json::Value::String(text_parts.join("")));
                    if !tool_calls.is_empty() {
                        let calls: Vec<serde_json::Value> = tool_calls
                            .iter()
                            .map(|tc| serde_json::json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": tc.input.to_string()
                                }
                            }))
                            .collect();
                        map.insert("tool_calls".into(), serde_json::Value::Array(calls));
                    }
                    result.push(map);
                }
                Message::Tool {
                    tool_call_id,
                    tool_name: _,
                    result: tool_result_value,
                    ..
                } => {
                    let mut map = HashMap::new();
                    map.insert("role".into(), serde_json::Value::String("tool".into()));
                    map.insert("tool_call_id".into(), serde_json::Value::String(tool_call_id.clone()));
                    let content = match tool_result_value {
                        ToolResultValue::Text { value } => value.clone(),
                        ToolResultValue::Json { value } => value.to_string(),
                        ToolResultValue::Error { value } => format!("Error: {}", value),
                    };
                    map.insert("content".into(), serde_json::Value::String(content));
                    result.push(map);
                }
            }
        }
        result
    }
}

pub fn estimate_tokens(msg: &Message) -> u64 {
    let text = match msg {
        Message::System { content, .. } => content.len() as u64,
        Message::User { content, .. } => content
            .iter()
            .map(|p| match p {
                ContentPart::Text { text } => text.len() as u64,
                ContentPart::Reasoning { text } => text.len() as u64,
                ContentPart::File { uri, .. } => uri.len() as u64,
            })
            .sum(),
        Message::Assistant { content, tool_calls, .. } => {
            let text_len: u64 = content
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => text.len() as u64,
                    _ => 0,
                })
                .sum();
            let tools_len: u64 = tool_calls
                .iter()
                .map(|tc| tc.input.to_string().len() as u64)
                .sum();
            text_len + tools_len
        }
        Message::Tool { result, .. } => match result {
            ToolResultValue::Text { value } => value.len() as u64,
            ToolResultValue::Json { value } => value.to_string().len() as u64,
            ToolResultValue::Error { value } => value.len() as u64,
        },
    };
    text / 4
}
