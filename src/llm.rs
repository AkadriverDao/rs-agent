use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use pin_project::pin_project;
use reqwest::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::types::{
    ContentPart, FinishReason, LlmEvent, Message,
};

#[derive(Debug, Clone)]
pub struct LlmConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: String,
    pub chat_path: String,
    pub auth_header_name: String,
    pub auth_header_value: String,
    pub max_tokens: u64,
    pub temperature: f64,
    pub top_p: f64,
}

impl LlmConfig {
    pub fn deepseek(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: "deepseek-chat".to_string(),
            base_url: "https://api.deepseek.com".to_string(),
            chat_path: "/v1/chat/completions".to_string(),
            auth_header_name: "Authorization".to_string(),
            auth_header_value: "Bearer {}".to_string(),
            max_tokens: 4096,
            temperature: 0.0,
            top_p: 0.95,
        }
    }

    pub fn openai_compatible(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            base_url: base_url.into(),
            chat_path: "/v1/chat/completions".to_string(),
            auth_header_name: "Authorization".to_string(),
            auth_header_value: "Bearer {}".to_string(),
            max_tokens: 4096,
            temperature: 0.0,
            top_p: 0.95,
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self::deepseek("")
    }
}

#[derive(Debug, Deserialize)]
struct StreamUsage {
    #[serde(rename = "prompt_tokens")]
    input_tokens: u64,
    #[serde(rename = "completion_tokens")]
    output_tokens: u64,
    #[serde(rename = "prompt_cache_hit_tokens")]
    cache_read_input_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct DeepSeekChunk {
    choices: Vec<Choice>,
    usage: Option<StreamUsage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    delta: Delta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Delta {
    content: Option<String>,
    reasoning_content: Option<String>,
    tool_calls: Option<Vec<DeltaToolCall>>,
}

#[derive(Debug, Deserialize)]
struct DeltaToolCall {
    index: u64,
    id: Option<String>,
    #[serde(rename = "type")]
    type_: Option<String>,
    function: Option<DeltaFunction>,
}

#[derive(Debug, Deserialize)]
struct DeltaFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Value>,
    stream: bool,
    max_tokens: u64,
    temperature: f64,
    top_p: f64,
    tools: Option<Vec<Value>>,
    tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<Value>,
}

#[pin_project]
pub struct LlmStream {
    #[pin]
    inner: mpsc::Receiver<LlmEvent>,
}

impl Stream for LlmStream {
    type Item = LlmEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().inner.poll_recv(cx)
    }
}

pub struct LlmClient {
    config: LlmConfig,
    client: Client,
}

impl LlmClient {
    pub fn new(config: LlmConfig) -> Self {
        Self {
            client: Client::new(),
            config,
        }
    }

    pub async fn stream_chat(
        &self,
        messages: Vec<Message>,
        tools: Vec<Value>,
        system_prompt: &str,
    ) -> Result<LlmStream, anyhow::Error> {
        let (tx, rx) = mpsc::channel(256);

        let mut llm_messages: Vec<Value> = Vec::new();

        if !system_prompt.is_empty() {
            llm_messages.push(serde_json::json!({
                "role": "system",
                "content": system_prompt
            }));
        }

        for msg in &messages {
            let m = match msg {
                Message::System { content, .. } => {
                    serde_json::json!({"role": "system", "content": content})
                }
                Message::User { content, .. } => {
                    let parts: Vec<Value> = content
                        .iter()
                        .map(|p| match p {
                            ContentPart::Text { text } => {
                                serde_json::json!({"type": "text", "text": text})
                            }
                            ContentPart::File { uri, mime } => {
                                serde_json::json!({"type": "file", "uri": uri, "mime": mime})
                            }
                            _ => serde_json::json!({}),
                        })
                        .collect();
                    serde_json::json!({"role": "user", "content": parts})
                }
                Message::Assistant {
                    content, tool_calls, ..
                } => {
                    let text: String = content
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::Text { text } => Some(text.clone()),
                            _ => None,
                        })
                        .collect();
                    let mut assistant_msg = serde_json::json!({
                        "role": "assistant",
                        "content": text,
                    });
                    if !tool_calls.is_empty() {
                        let calls: Vec<Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": tc.input.to_string()
                                    }
                                })
                            })
                            .collect();
                        assistant_msg["tool_calls"] = serde_json::Value::Array(calls);
                    }
                    assistant_msg
                }
                Message::Tool {
                    tool_call_id,
                    tool_name: _,
                    result,
                    ..
                } => {
                    let content = match result {
                        crate::types::ToolResultValue::Text { value } => value.clone(),
                        crate::types::ToolResultValue::Json { value } => value.to_string(),
                        crate::types::ToolResultValue::Error { value } => format!("Error: {}", value),
                    };
                    serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": content
                    })
                }
            };
            llm_messages.push(m);
        }

        let request_body = ChatRequest {
            model: self.config.model.clone(),
            messages: llm_messages,
            stream: true,
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            top_p: self.config.top_p,
            tools: if tools.is_empty() { None } else { Some(tools) },
            tool_choice: None,
            stream_options: Some(serde_json::json!({"include_usage": true})),
        };

        let client = self.client.clone();
        let url = format!("{}{}", self.config.base_url.trim_end_matches('/'), self.config.chat_path);
        let api_key = self.config.api_key.clone();
        let auth_header_name = self.config.auth_header_name.clone();
        let auth_header_value_template = self.config.auth_header_value.clone();

        tokio::spawn(async move {
            let max_retries = 3;
            let mut attempt = 0u32;

            let response = loop {
                attempt += 1;
                match Self::send_request(&client, &url, &api_key, &auth_header_name, &auth_header_value_template, &request_body).await {
                    Ok(resp) => break resp,
                    Err((msg, retryable)) => {
                        if retryable && attempt <= max_retries {
                            let delay_ms = 1000u64 * 2u64.pow(attempt - 1);
                            warn!(
                                "LLM request failed (attempt {}/{}): {} — retrying in {}ms",
                                attempt, max_retries, msg, delay_ms
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                            continue;
                        }
                        let _ = tx
                            .send(LlmEvent::ProviderError {
                                message: format!(
                                    "Request failed after {} attempt(s): {}",
                                    attempt, msg
                                ),
                                retryable: false,
                            })
                            .await;
                        return;
                    }
                }
            };

            #[derive(Default)]
            struct AccToolCall {
                id: String,
                name: String,
                args: String,
            }

            let step_index = 0u32;
            let mut current_text_id = String::new();
            let mut current_reasoning_id = String::new();
            let mut acc_tool_calls: HashMap<u64, AccToolCall> = HashMap::new();
            let mut last_usage: Option<crate::types::Usage> = None;

            let _ = tx.send(LlmEvent::StepStart { index: step_index }).await;

            let mut stream = response.bytes_stream();
            let mut buffer = String::new();

            use futures::StreamExt;

            let finalize_tool_calls = |tx: &mpsc::Sender<LlmEvent>, acc: &mut HashMap<u64, AccToolCall>| {
                let mut indices: Vec<u64> = acc.keys().copied().collect();
                indices.sort();
                for idx in indices {
                    if let Some(tc) = acc.remove(&idx) {
                        if tc.id.is_empty() || tc.name.is_empty() {
                            continue;
                        }
                        let _ = tx.try_send(LlmEvent::ToolCallEnd {
                            id: tc.id.clone(),
                            name: tc.name.clone(),
                        });
                        let input: Value =
                            serde_json::from_str(&tc.args).unwrap_or(Value::String(tc.args.clone()));
                        let _ = tx.try_send(LlmEvent::ToolCallReceived {
                            id: tc.id,
                            name: tc.name,
                            input,
                        });
                    }
                }
            };

            while let Some(chunk_result) = stream.next().await {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = tx
                            .send(LlmEvent::ProviderError {
                                message: format!("Stream error: {}", e),
                                retryable: true,
                            })
                            .await;
                        return;
                    }
                };

                buffer.push_str(&String::from_utf8_lossy(&chunk));

                while let Some(line_end) = buffer.find('\n') {
                    let line = buffer[..line_end].trim().to_string();
                    buffer = buffer[line_end + 1..].to_string();

                    if line.is_empty() {
                        continue;
                    }
                    if !line.starts_with("data: ") {
                        continue;
                    }
                    let data = &line[6..];
                    if data == "[DONE]" {
                        if !current_text_id.is_empty() {
                            let _ = tx.send(LlmEvent::TextEnd { id: current_text_id.clone() }).await;
                        }
                        if !current_reasoning_id.is_empty() {
                            let _ = tx.send(LlmEvent::ReasoningEnd { id: current_reasoning_id.clone() }).await;
                        }
                        finalize_tool_calls(&tx, &mut acc_tool_calls);
                        let _ = tx
                            .send(LlmEvent::StepFinish {
                                index: step_index,
                                reason: FinishReason::Stop,
                                usage: last_usage.clone(),
                            })
                            .await;
                        let _ = tx
                            .send(LlmEvent::Finish {
                                reason: FinishReason::Stop,
                                usage: last_usage.clone(),
                            })
                            .await;
                        return;
                    }

                    match serde_json::from_str::<DeepSeekChunk>(data) {
                        Ok(parsed) => {
                            if let Some(stream_usage) = &parsed.usage {
                                last_usage = Some(crate::types::Usage {
                                    input_tokens: stream_usage.input_tokens,
                                    output_tokens: stream_usage.output_tokens,
                                    cache_read_input_tokens: stream_usage.cache_read_input_tokens,
                                    cache_write_input_tokens: None,
                                });
                            }
                            for choice in parsed.choices {
                                if let Some(reason) = choice.finish_reason {
                                    if !current_text_id.is_empty() {
                                        let _ = tx.send(LlmEvent::TextEnd { id: current_text_id.clone() }).await;
                                    }
                                    if !current_reasoning_id.is_empty() {
                                        let _ = tx.send(LlmEvent::ReasoningEnd { id: current_reasoning_id.clone() }).await;
                                    }
                                    if !acc_tool_calls.is_empty() {
                                        finalize_tool_calls(&tx, &mut acc_tool_calls);
                                    }
                                    let reason = match reason.as_str() {
                                        "stop" => FinishReason::Stop,
                                        "length" => FinishReason::Length,
                                        "content_filter" => FinishReason::ContentFiltered,
                                        "tool_calls" => FinishReason::ToolCalls,
                                        other => FinishReason::Other(other.to_string()),
                                    };
                                    let _ = tx
                                        .send(LlmEvent::StepFinish {
                                            index: step_index,
                                            reason: reason.clone(),
                                            usage: last_usage.clone(),
                                        })
                                        .await;
                                    let _ = tx
                                        .send(LlmEvent::Finish {
                                            reason,
                                            usage: last_usage.clone(),
                                        })
                                        .await;
                                    return;
                                }

                                // Accumulate tool call deltas by index
                                if let Some(ref tool_calls) = choice.delta.tool_calls {
                                    for tc in tool_calls {
                                        let is_new = !acc_tool_calls.contains_key(&tc.index);
                                        let entry =
                                            acc_tool_calls.entry(tc.index).or_default();
                                        if let Some(ref id) = tc.id {
                                            entry.id = id.clone();
                                        }
                                        if let Some(ref name) =
                                            tc.function.as_ref().and_then(|f| f.name.clone())
                                        {
                                            entry.name = name.to_owned();
                                        }
                                        if let Some(ref args) = tc.function
                                            .as_ref()
                                            .and_then(|f| f.arguments.clone())
                                        {
                                            if is_new {
                                                let _ = tx.try_send(LlmEvent::ToolCallStart {
                                                    id: entry.id.clone(),
                                                    name: entry.name.clone(),
                                                });
                                            }
                                            let _ = tx.try_send(LlmEvent::ToolCallDelta {
                                                id: entry.id.clone(),
                                                name: entry.name.clone(),
                                                text: args.clone(),
                                            });
                                            entry.args.push_str(args);
                                        }
                                    }
                                }

                                if let Some(content) = choice.delta.content {
                                    if current_text_id.is_empty() {
                                        current_text_id = uuid::Uuid::new_v4().to_string();
                                        let _ = tx
                                            .send(LlmEvent::TextStart {
                                                id: current_text_id.clone(),
                                            })
                                            .await;
                                    }
                                    let _ = tx
                                        .send(LlmEvent::TextDelta {
                                            id: current_text_id.clone(),
                                            text: content,
                                        })
                                        .await;
                                }

                                if let Some(reasoning) = choice.delta.reasoning_content {
                                    if current_reasoning_id.is_empty() {
                                        current_reasoning_id = uuid::Uuid::new_v4().to_string();
                                        let _ = tx
                                            .send(LlmEvent::ReasoningStart {
                                                id: current_reasoning_id.clone(),
                                            })
                                            .await;
                                    }
                                    let _ = tx
                                        .send(LlmEvent::ReasoningDelta {
                                            id: current_reasoning_id.clone(),
                                            text: reasoning,
                                        })
                                        .await;
                                }
                            }
                        }
                        Err(e) => {
                            debug!("Failed to parse SSE chunk: {} | data: {}", e, data);
                        }
                    }
                }
            }

            // Stream ended without finish reason
            if !current_text_id.is_empty() {
                let _ = tx.send(LlmEvent::TextEnd { id: current_text_id.clone() }).await;
            }
            if !current_reasoning_id.is_empty() {
                let _ = tx.send(LlmEvent::ReasoningEnd { id: current_reasoning_id.clone() }).await;
            }
            finalize_tool_calls(&tx, &mut acc_tool_calls);
            let _ = tx
                .send(LlmEvent::StepFinish {
                    index: step_index,
                    reason: FinishReason::Stop,
                    usage: last_usage.clone(),
                })
                .await;
            let _ = tx
                .send(LlmEvent::Finish {
                    reason: FinishReason::Stop,
                    usage: last_usage.clone(),
                })
                .await;
        });

        Ok(LlmStream { inner: rx })
    }

    async fn send_request(
        client: &Client,
        url: &str,
        api_key: &str,
        auth_header_name: &str,
        auth_header_value_template: &str,
        body: &ChatRequest,
    ) -> Result<Response, (String, bool)> {
        let auth_value = auth_header_value_template.replace("{}", api_key);
        let response = client
            .post(url)
            .header(auth_header_name, auth_value)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|e| (format!("HTTP request failed: {}", e), true))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err((
                format!("API error {}: {}", status, body),
                status.is_server_error(),
            ));
        }

        Ok(response)
    }
}
